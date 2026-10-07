//! Elder-owned loop task firing.
//!
//! The elected sidebar elder keeps time while a room is open; the opt-in OS
//! timer runs the same scheduler for roots without one. Durable state arms tasks
//! on first sight and records each fire before spawning the detached
//! `rimz loop run <name>` helper, so a hot tick does not spawn the same
//! occurrence twice.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use jiff::{Timestamp, Zoned};

use super::{
    Trigger,
    arming::{self, ArmState, Arming},
    catalog::{LoadedTask, TaskCatalog},
    run_log::{self, LoopRunMode, LoopRunRecord, LoopRunResult},
    signal::Signal,
    when::{self, CiSource, ConditionEvidence, Verdict, WhenState},
};
use crate::RuntimePaths;
use crate::disk::atomic::write_temp_then_rename_cache;
use crate::disk::paths::{StatePaths, logs_dir};
use crate::ids::WorkspaceId;
use crate::workspace::record;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Arm,
    Fire,
    WatchLost,
}

const WATCH_LOST_GRACE_SECS: i64 = 30;
const RESIDENT_RETRY_SECS: i64 = 5 * 60;

struct ScopedTask {
    name: String,
    checkout: Option<PathBuf>,
    task: LoadedTask,
}

pub(super) fn scope_key(name: &str, checkout: &Path) -> String {
    // A tuple of strings is always JSON-serializable; unlike concatenation, it cannot collide with another pair.
    serde_json::to_string(&(name, checkout.to_string_lossy())).expect("string tuple serializes")
}

fn scoped_tasks(
    tasks: &BTreeMap<String, LoadedTask>,
    ledger: &super::launch_ledger::Ledger,
    owned: &[PathBuf],
) -> BTreeMap<String, ScopedTask> {
    let mut scoped = BTreeMap::new();
    for (name, task) in tasks {
        let entry = task.entry();
        let launches = ledger.get(name);
        if !entry.each_worktree {
            if entry.stay
                && launches.is_some_and(|launches| launches.contains_key(&entry.run_dir()))
            {
                continue;
            }
            scoped.insert(
                name.clone(),
                ScopedTask {
                    name: name.clone(),
                    checkout: None,
                    task: task.clone(),
                },
            );
            continue;
        }
        for checkout in owned {
            let checkout = crate::utils::path::normalize_path_lexical(checkout);
            if launches.is_some_and(|launches| launches.contains_key(&checkout)) {
                continue;
            }
            let key = scope_key(name, &checkout);
            scoped.insert(
                key,
                ScopedTask {
                    name: name.clone(),
                    checkout: Some(checkout),
                    task: task.clone(),
                },
            );
        }
    }
    scoped
}

/// Whether a launch actually put a runner on the host. A `NotStarted` launch
/// has already recorded its own `start failed` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub(super) enum LaunchOutcome {
    Started,
    NotStarted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopRunHost {
    /// Ordinary detached child, used by the elder and signal emitters.
    Detached,
    /// External non-systemd tick child that must outlive the timer's process group.
    IsolatedProcessGroup,
    /// External systemd tick child that must leave the service cgroup.
    TransientScope,
}

#[doc(hidden)]
pub fn fire_due_tasks(
    runtime: &RuntimePaths,
    project_root: Option<&Path>,
    now: &Zoned,
    host: LoopRunHost,
    ci_source: Option<&CiSource>,
) {
    let project_root = project_root
        .map(Path::to_path_buf)
        .or_else(|| workspace_project_root(runtime));
    let tasks = runnable_tasks_for(runtime, project_root.as_deref());
    fire_tasks(
        runtime,
        project_root.as_deref(),
        tasks,
        now,
        host,
        ci_source,
    );
}

fn fire_tasks(
    runtime: &RuntimePaths,
    project_root: Option<&Path>,
    mut tasks: BTreeMap<String, LoadedTask>,
    now: &Zoned,
    host: LoopRunHost,
    ci_source: Option<&CiSource>,
) -> Vec<String> {
    let path = state_path(runtime);
    let state = read_state(&path);
    let mut arming = arming::load();
    let ledger = if tasks.values().any(|task| task.entry().stay) {
        match super::launch_ledger::load_room(runtime, project_root) {
            Ok(ledger) => ledger,
            Err(error) => {
                tracing::warn!(%error, "resident loop ledger unavailable");
                tasks.retain(|_, task| !task.entry().stay);
                BTreeMap::new()
            }
        }
    } else {
        BTreeMap::new()
    };
    let owned = tasks
        .values()
        .find(|task| task.entry().each_worktree)
        .map(|task| {
            crate::worktree::discover_owned(project_root.unwrap_or(&task.entry().resolved_root()))
        })
        .transpose()
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "enumerating loop worktrees");
            None
        })
        .unwrap_or_default()
        .into_iter()
        .map(|worktree| worktree.marker.worktree_path)
        .collect::<Vec<_>>();
    let scoped = scoped_tasks(&tasks, &ledger, &owned);
    for (key, scope) in &scoped {
        if let Some(record) = arming.get(&scope.task.key(&scope.name)).copied() {
            arming.insert(scope.task.key(key), record);
        }
    }
    let tasks: BTreeMap<_, _> = scoped
        .iter()
        .map(|(key, scope)| (key.clone(), scope.task.clone()))
        .collect();
    let when_states = last_when_states(runtime);
    let windows = when::WindowReadings::new(Some(runtime), now.timestamp());
    let verdicts = tasks
        .iter()
        .filter_map(|(name, task)| {
            if !state.contains_key(name)
                || ArmState::resolve(arming.get(&task.key(name)), task.source(), now.timestamp())
                    != ArmState::Live
            {
                return None;
            }
            match &task.trigger().as_ref().ok()?.trigger {
                Trigger::Condition { expr, .. } => Some((
                    name.clone(),
                    when::evaluate(
                        expr,
                        &scoped[name]
                            .checkout
                            .clone()
                            .unwrap_or_else(|| task.entry().run_dir()),
                        ci_source,
                        task.entry().provider.as_ref(),
                        task.entry().account.as_ref(),
                        &windows,
                    ),
                )),
                Trigger::Schedule(_) | Trigger::Signal { .. } | Trigger::Watch(_) => None,
            }
        })
        .collect();
    let (mut actions, next_state, next_when) =
        plan(&tasks, &state, &arming, now, &when_states, &verdicts);
    if next_state != state
        && let Err(err) = write_temp_then_rename_cache(&path, &next_state)
    {
        tracing::warn!(
            workspace = %runtime.workspace_id,
            tags.operation = "loop_fire.write_state",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to record loop task fire state",
        );
        return Vec::new();
    }
    if next_when != when_states
        && let Err(err) = write_temp_then_rename_cache(&when_state_path(runtime), &next_when)
    {
        tracing::warn!(workspace = %runtime.workspace_id, error = %err, "sidebar: failed to record loop condition state");
        actions.retain(|(name, action)| {
            *action != Action::Fire
                || !tasks[name]
                    .trigger()
                    .as_ref()
                    .is_ok_and(|parsed| matches!(parsed.trigger, Trigger::Condition { .. }))
        });
    }
    let mut fired = Vec::new();
    for (name, action) in actions {
        match action {
            Action::Arm => {}
            Action::Fire => {
                // plan omits invalid triggers before producing any action.
                let condition = match &tasks[&name]
                    .trigger()
                    .as_ref()
                    .expect("the planner fires only valid triggers")
                    .trigger
                {
                    Trigger::Condition { expr, .. } => Some(ConditionEvidence {
                        when: expr.to_string(),
                        hold: tasks[&name].entry().hold.clone(),
                        held_ms: u64::try_from(
                            now.timestamp()
                                .duration_since(next_when[&name].since)
                                .as_millis(),
                        )
                        .unwrap_or(0),
                        readings: verdicts[&name].readings.clone(),
                    }),
                    Trigger::Schedule(_) | Trigger::Signal { .. } | Trigger::Watch(_) => None,
                };
                if spawn_loop_run(
                    runtime,
                    &tasks[&name],
                    project_root,
                    &scoped[&name].name,
                    None,
                    condition.as_ref(),
                    scoped[&name].checkout.as_deref(),
                    host,
                ) == LaunchOutcome::Started
                {
                    fired.push(scoped[&name].name.clone());
                }
            }
            Action::WatchLost => {
                let signal_name = match format!("wait.{name}").parse() {
                    Ok(signal_name) => signal_name,
                    Err(err) => {
                        tracing::debug!(
                            task = %name,
                            error = %err,
                            "loop watcher with invalid derived signal skipped by elder fire"
                        );
                        continue;
                    }
                };
                let watch = match lost_watch_outcome(
                    &tasks[&name],
                    &name,
                    state[&name],
                    now.timestamp(),
                ) {
                    Ok(outcome) => outcome,
                    Err(err) => {
                        tracing::warn!(task = %name, error = %err, "resolving lost watcher output");
                        continue;
                    }
                };
                let signal = Signal {
                    name: signal_name,
                    payload: serde_json::Map::new(),
                    source: crate::store::event::SignalSource::Watch,
                    watch: Some(watch),
                };
                if spawn_loop_run(
                    runtime,
                    &tasks[&name],
                    project_root,
                    &name,
                    Some(&signal),
                    None,
                    None,
                    host,
                ) == LaunchOutcome::Started
                {
                    fired.push(name);
                }
            }
        }
    }
    fired
}

fn lost_watch_outcome(
    task: &LoadedTask,
    name: &str,
    arm_stamp: Timestamp,
    now: Timestamp,
) -> anyhow::Result<super::signal::WatchOutcome> {
    let paths = StatePaths::for_project_root(&task.entry().resolved_root())?;
    let path = super::signal::wait_output_path(&paths, name, task.entry());
    let armed_at = task
        .entry()
        .wait_meta
        .as_ref()
        .map_or(arm_stamp, |meta| meta.armed_at);
    let elapsed_ms = u64::try_from(now.duration_since(armed_at).as_millis()).unwrap_or(0);
    let output = match super::signal::read_wait_tail(&path) {
        Ok(output) => output,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(task = name, error = %err, "reading lost watcher output");
            }
            String::new()
        }
    };
    Ok(super::signal::WatchOutcome::measured(
        super::signal::WatchVerdict::Lost {
            detail: "watcher process exited without reporting".to_owned(),
            elapsed_ms,
        },
        output,
        &path,
    ))
}

pub(super) fn workspace_project_root(runtime: &RuntimePaths) -> Option<PathBuf> {
    let paths = StatePaths::for_workspace(runtime.workspace_id.clone()).ok()?;
    match record::read(&paths.workspace_record) {
        Ok(record) => Some(record.project_root),
        Err(err) => {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                error = &err as &dyn std::error::Error,
                "loop elder skipped project tasks without workspace record"
            );
            None
        }
    }
}

pub(super) fn deadline_expired_at(entry: &crate::config::TaskEntry, now: Timestamp) -> bool {
    entry.deadline.is_some_and(|deadline| now >= deadline)
}

type PlannedTasks = (
    Vec<(String, Action)>,
    BTreeMap<String, Timestamp>,
    BTreeMap<String, WhenState>,
);

fn plan(
    tasks: &BTreeMap<String, LoadedTask>,
    state: &BTreeMap<String, Timestamp>,
    arming_entries: &BTreeMap<String, Arming>,
    now: &Zoned,
    when_states: &BTreeMap<String, WhenState>,
    verdicts: &BTreeMap<String, Verdict>,
) -> PlannedTasks {
    let mut actions = Vec::new();
    let mut next_state = BTreeMap::new();
    let mut next_when = BTreeMap::new();
    for (name, task) in tasks {
        let parsed = match task.trigger() {
            Ok(parsed) => parsed,
            Err(err) => {
                tracing::debug!(
                    task = %name,
                    error = %err,
                    "invalid loop task skipped by elder fire"
                );
                continue;
            }
        };
        let key = task.key(name);
        let arming = arming_entries.get(&key);
        let arm_state = ArmState::resolve(arming, task.source(), now.timestamp());
        let instant = match &parsed.trigger {
            Trigger::Schedule(schedule) => schedule.schedule.instant(),
            _ => None,
        };
        match state.get(name).copied() {
            // An absolute one-shot has no later occurrence, so arming it past
            // due would strand it: it fires on its first live sight instead.
            None if instant.is_some_and(|at| at <= now.timestamp()) => {
                if arm_state == ArmState::Live {
                    actions.push((name.clone(), Action::Fire));
                    next_state.insert(name.clone(), now.timestamp());
                }
            }
            None => {
                actions.push((name.clone(), Action::Arm));
                next_state.insert(name.clone(), now.timestamp());
            }
            Some(last_fire) if arm_state != ArmState::Live => {
                next_state.insert(name.clone(), last_fire);
                if matches!(parsed.trigger, Trigger::Condition { .. })
                    && let Some(state) = when_states.get(name)
                {
                    next_when.insert(name.clone(), state.clone());
                }
            }
            Some(last_fire) if matches!(parsed.trigger, Trigger::Condition { .. }) => {
                next_state.insert(name.clone(), last_fire);
                if !verdicts.get(name).is_some_and(|verdict| verdict.ok) {
                    continue;
                }
                let Trigger::Condition { expr, hold } = &parsed.trigger else {
                    // The match guard admits only condition triggers.
                    unreachable!("condition arm")
                };
                let fingerprint = Some((expr.to_string(), *hold, task.entry().run_dir()));
                let mut state = when_states
                    .get(name)
                    .filter(|state| state.matches_condition(expr, *hold, &task.entry().run_dir()))
                    .cloned()
                    .unwrap_or(WhenState {
                        fingerprint,
                        since: now.timestamp(),
                        fired: false,
                    });
                let elapsed =
                    u128::try_from(now.timestamp().duration_since(state.since).as_millis())
                        .unwrap_or(0);
                let retry_due = task.entry().stay
                    && now.timestamp().duration_since(last_fire).as_secs() >= RESIDENT_RETRY_SECS;
                if (!state.fired || retry_due)
                    && elapsed >= hold.map_or(0, |duration| duration.as_millis())
                {
                    actions.push((name.clone(), Action::Fire));
                    next_state.insert(name.clone(), now.timestamp());
                    state.fired = true;
                }
                next_when.insert(name.clone(), state);
            }
            Some(last_fire)
                if matches!(&parsed.trigger, Trigger::Schedule(schedule) if schedule.schedule.due(
                    if instant.is_some() {
                        last_fire
                    } else {
                        arming::effective_last_fire(last_fire, arming, now.timestamp())
                    },
                    now,
                )) =>
            {
                actions.push((name.clone(), Action::Fire));
                next_state.insert(name.clone(), now.timestamp());
            }
            Some(last_fire)
                if matches!(&parsed.trigger, Trigger::Watch(_))
                    && now.timestamp().duration_since(last_fire).as_secs()
                        > WATCH_LOST_GRACE_SECS
                    && watcher_missing(task, name) =>
            {
                actions.push((name.clone(), Action::WatchLost));
                next_state.insert(name.clone(), now.timestamp());
            }
            Some(last_fire) => {
                next_state.insert(name.clone(), last_fire);
            }
        }
    }
    (actions, next_state, next_when)
}

fn watcher_missing(task: &LoadedTask, name: &str) -> bool {
    let Ok(state) = StatePaths::for_project_root(&task.entry().resolved_root()) else {
        return false;
    };
    let Ok(runtime) = RuntimePaths::for_state(&state) else {
        return false;
    };
    super::signal::watcher_info(&runtime, name).is_ok_and(|info| info.is_none())
}

pub(super) fn runnable_tasks_for(
    runtime: &RuntimePaths,
    project_root: Option<&Path>,
) -> BTreeMap<String, LoadedTask> {
    workspace_tasks(
        TaskCatalog::load_lenient(project_root)
            .runnable()
            .iter()
            .filter(|(_, task)| {
                !matches!(
                    task.source(),
                    super::catalog::TaskSource::Project { state }
                        if state != crate::trust::TrustState::Trusted
                )
            })
            .map(|(name, task)| (name.clone(), task.clone()))
            .collect(),
        &runtime.workspace_id,
    )
}

fn workspace_tasks(
    tasks: BTreeMap<String, LoadedTask>,
    workspace_id: &WorkspaceId,
) -> BTreeMap<String, LoadedTask> {
    tasks
        .into_iter()
        .filter(|(_, task)| {
            WorkspaceId::from_project_root(&task.entry().resolved_root()) == *workspace_id
        })
        .collect()
}

fn read_state(path: &Path) -> BTreeMap<String, Timestamp> {
    let Ok(bytes) = std::fs::read(path) else {
        return BTreeMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn last_stamps(runtime: &RuntimePaths) -> BTreeMap<String, Timestamp> {
    read_state(&state_path(runtime))
}

pub fn last_when_states(runtime: &RuntimePaths) -> BTreeMap<String, WhenState> {
    crate::disk::atomic::read_json_cache(&when_state_path(runtime))
}

fn state_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.lane_path("loop-fire.json")
}

fn when_state_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.lane_path("loop-when.json")
}

#[expect(
    clippy::too_many_arguments,
    reason = "explicit task, trigger and checkout at the helper boundary"
)]
pub(super) fn spawn_loop_run(
    runtime: &RuntimePaths,
    task: &LoadedTask,
    project_root: Option<&Path>,
    name: &str,
    signal: Option<&Signal>,
    condition: Option<&ConditionEvidence>,
    checkout: Option<&Path>,
    host: LoopRunHost,
) -> LaunchOutcome {
    let encoded = match encode_signal(signal) {
        Ok(encoded) => encoded,
        Err(reason) => return record_start_failed(task, name, signal, condition, checkout, reason),
    };
    let encoded_condition = match condition.map(serde_json::to_string).transpose() {
        Ok(encoded) => encoded,
        Err(err) => {
            return record_start_failed(task, name, signal, condition, checkout, err.to_string());
        }
    };
    let args = loop_run_args(
        project_root,
        name,
        encoded.as_deref(),
        encoded_condition.as_deref(),
        checkout,
    );
    let (program, args) = loop_run_command(host, &crate::proc::rimz_exe(), &args, name);
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        task = name,
        "loop scheduler firing task",
    );
    let before = Timestamp::now();
    let spawned = if host == LoopRunHost::Detached {
        crate::child_process::spawn_detached_rimz(runtime, &args, "loop-run").map(|()| None)
    } else {
        crate::child_process::spawn_detached_program(&program, &args, runtime, "loop-run")
    };
    match spawned {
        Ok(Some(pid)) if host == LoopRunHost::TransientScope => {
            if wait_for_scope(pid, name) == ScopeHandoff::ChildGone
                && !run_log::has_scheduled_row_since(
                    &logs_dir(),
                    name,
                    &task.entry().resolved_root(),
                    checkout,
                    before,
                )
                && !(task.entry().stay
                    && super::launch_ledger::load_room(runtime, project_root).is_ok_and(|ledger| {
                        let scope = checkout
                            .map(Path::to_path_buf)
                            .unwrap_or_else(|| task.entry().run_dir());
                        ledger
                            .get(name)
                            .is_some_and(|launches| launches.contains_key(&scope))
                    }))
            {
                return record_start_failed(
                    task,
                    name,
                    signal,
                    condition,
                    checkout,
                    "runner exited before the scope hand-off and left no history row".to_owned(),
                );
            }
        }
        Ok(_) => {}
        Err(err) => {
            tracing::warn!(
                task = name,
                tags.operation = "loop_fire.spawn",
                error = &err as &dyn std::error::Error,
                "sidebar: failed to spawn loop task",
            );
            return record_start_failed(task, name, signal, condition, checkout, err.to_string());
        }
    }
    LaunchOutcome::Started
}

fn record_start_failed(
    task: &LoadedTask,
    name: &str,
    signal: Option<&Signal>,
    condition: Option<&ConditionEvidence>,
    checkout: Option<&Path>,
    reason: String,
) -> LaunchOutcome {
    let mut record =
        LoopRunRecord::new(name, LoopRunResult::StartFailed, LoopRunMode::Scheduled, 0);
    record.duration_ms = None;
    record.error = Some(reason);
    record.signal = signal.map(|signal| run_log::SignalRecord {
        name: signal.name.clone(),
        payload: signal.payload.clone(),
    });
    record.condition = condition.cloned();
    if task.entry().stay {
        record.checkout = Some(
            checkout
                .map(Path::to_path_buf)
                .unwrap_or_else(|| task.entry().run_dir()),
        );
    }
    run_log::record_transition(task, &record);
    LaunchOutcome::NotStarted
}

/// Encode the signal the runner is handed. Each launch encodes its own, so a
/// launch holds the `Signal` itself and a failed start can record it.
fn encode_signal(signal: Option<&Signal>) -> Result<Option<String>, String> {
    signal
        .map(|signal| serde_json::to_string(signal).map_err(|err| err.to_string()))
        .transpose()
}

fn loop_run_command(
    host: LoopRunHost,
    exe: &Path,
    args: &[OsString],
    name: &str,
) -> (OsString, Vec<OsString>) {
    if host != LoopRunHost::TransientScope {
        return (exe.as_os_str().to_owned(), args.to_vec());
    }
    let mut scoped = Vec::from([
        "--user".into(),
        "--scope".into(),
        "--quiet".into(),
        "--collect".into(),
        "--description".into(),
        format!("RimZ loop run {name}").into(),
        "--".into(),
        exe.as_os_str().to_owned(),
    ]);
    scoped.extend_from_slice(args);
    ("systemd-run".into(), scoped)
}

fn cgroup_changed(parent: Option<&[u8]>, child: Option<&[u8]>) -> bool {
    matches!((parent, child), (Some(parent), Some(child)) if !parent.is_empty() && !child.is_empty() && parent != child)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScopeHandoff {
    HandedOff,
    ChildGone,
    Pending,
}

fn scope_handoff(parent: Option<&[u8]>, child: Option<&[u8]>, live: bool) -> ScopeHandoff {
    if !live {
        return ScopeHandoff::ChildGone;
    }
    if cgroup_changed(parent, child) {
        return ScopeHandoff::HandedOff;
    }
    ScopeHandoff::Pending
}

fn wait_for_scope(pid: u32, name: &str) -> ScopeHandoff {
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(5);
    let parent = std::fs::read("/proc/self/cgroup").ok();
    let child_path = format!("/proc/{pid}/cgroup");
    loop {
        let child = std::fs::read(&child_path).ok();
        match scope_handoff(
            parent.as_deref(),
            child.as_deref(),
            crate::proc::process_is_live(pid, None),
        ) {
            ScopeHandoff::HandedOff => return ScopeHandoff::HandedOff,
            ScopeHandoff::ChildGone => {
                tracing::warn!(
                    task = name,
                    pid,
                    "loop run exited before the timer scope hand-off was confirmed"
                );
                return ScopeHandoff::ChildGone;
            }
            ScopeHandoff::Pending => {}
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            tracing::warn!(
                task = name,
                pid,
                "loop run did not leave the timer cgroup within 5 seconds"
            );
            return ScopeHandoff::Pending;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(25)));
    }
}

pub(super) fn wait_loop_run(
    runtime: &RuntimePaths,
    task: &LoadedTask,
    project_root: Option<&Path>,
    name: &str,
    signal: &Signal,
) -> LaunchOutcome {
    let encoded = match encode_signal(Some(signal)) {
        Ok(encoded) => encoded,
        Err(reason) => return record_start_failed(task, name, Some(signal), None, None, reason),
    };
    let mut command = crate::child_process::detached_rimz_command(crate::proc::rimz_exe(), runtime);
    command.args(loop_run_args(
        project_root,
        name,
        encoded.as_deref(),
        None,
        None,
    ));
    // A loop run is not the arming agent's work, as on the detached path.
    crate::child_process::restore_user_temp_env(&mut command);
    match command.status() {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::warn!(task = name, %status, "watched wait delivery failed"),
        Err(err) => {
            tracing::warn!(task = name, error = %err, "running watched wait delivery");
            return record_start_failed(task, name, Some(signal), None, None, err.to_string());
        }
    }
    LaunchOutcome::Started
}

fn loop_run_args(
    project_root: Option<&Path>,
    name: &str,
    signal_json: Option<&str>,
    condition_json: Option<&str>,
    checkout: Option<&Path>,
) -> Vec<OsString> {
    let mut args = Vec::<OsString>::new();
    if let Some(project_root) = project_root {
        args.extend([
            OsString::from("--root"),
            project_root.as_os_str().to_owned(),
        ]);
    }
    args.extend([OsString::from("loop"), OsString::from("run"), name.into()]);
    if let Some(checkout) = checkout {
        args.extend([OsString::from("--cwd"), checkout.as_os_str().to_owned()]);
    }
    if let Some(signal_json) = signal_json {
        args.extend([OsString::from("--signal-json"), signal_json.into()]);
    }
    if let Some(condition_json) = condition_json {
        args.extend([OsString::from("--condition-json"), condition_json.into()]);
    }
    args
}

#[cfg(test)]
mod tests;
