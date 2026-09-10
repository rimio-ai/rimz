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
};
use crate::RuntimePaths;
use crate::disk::atomic::write_temp_then_rename_cache;
use crate::disk::paths::StatePaths;
use crate::ids::WorkspaceId;
use crate::workspace::record;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Arm,
    Fire,
    WatchLost,
}

const WATCH_LOST_GRACE_SECS: i64 = 30;

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
) {
    let project_root = project_root
        .map(Path::to_path_buf)
        .or_else(|| workspace_project_root(runtime));
    let tasks = runnable_tasks_for(runtime, project_root.as_deref());
    fire_tasks(runtime, project_root.as_deref(), tasks, now, host);
}

fn fire_tasks(
    runtime: &RuntimePaths,
    project_root: Option<&Path>,
    tasks: BTreeMap<String, LoadedTask>,
    now: &Zoned,
    host: LoopRunHost,
) -> Vec<String> {
    let path = state_path(runtime);
    let state = read_state(&path);
    let arming = arming::load();
    let (actions, next_state) = plan(&tasks, &state, &arming, now);
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
    let mut fired = Vec::new();
    for (name, action) in actions {
        match action {
            Action::Arm => {}
            Action::Fire => {
                spawn_loop_run(runtime, project_root, &name, None, host);
                fired.push(name);
            }
            Action::WatchLost => {
                let signal_name = match format!("wake.{name}").parse() {
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
                let signal = super::signal::Signal {
                    name: signal_name,
                    payload: serde_json::Map::new(),
                    source: crate::store::event::SignalSource::Watch,
                    watch: Some(watch),
                };
                if let Ok(encoded) = serde_json::to_string(&signal) {
                    spawn_loop_run(runtime, project_root, &name, Some(&encoded), host);
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
    let paths = StatePaths::for_workspace(WorkspaceId::from_project_root(
        &task.entry().resolved_root(),
    ))?;
    let path = super::signal::wake_output_path(&paths, name);
    let armed_at = task
        .entry()
        .wake_meta
        .as_ref()
        .map_or(arm_stamp, |meta| meta.armed_at);
    let elapsed_ms = u64::try_from(now.duration_since(armed_at).as_millis()).unwrap_or(0);
    let output = match super::signal::read_wake_tail(&path) {
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
        &crate::sandbox::TmpView::current(&paths),
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

fn plan(
    tasks: &BTreeMap<String, LoadedTask>,
    state: &BTreeMap<String, Timestamp>,
    arming_entries: &BTreeMap<String, Arming>,
    now: &Zoned,
) -> (Vec<(String, Action)>, BTreeMap<String, Timestamp>) {
    let mut actions = Vec::new();
    let mut next_state = BTreeMap::new();
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
        match state.get(name).copied() {
            None => {
                actions.push((name.clone(), Action::Arm));
                next_state.insert(name.clone(), now.timestamp());
            }
            Some(last_fire) if arm_state != ArmState::Live => {
                next_state.insert(name.clone(), last_fire);
            }
            Some(last_fire)
                if matches!(&parsed.trigger, Trigger::Schedule(schedule) if schedule.schedule.due(
                    arming::effective_last_fire(last_fire, arming, now.timestamp()),
                    now,
                )) =>
            {
                actions.push((name.clone(), Action::Fire));
                next_state.insert(name.clone(), now.timestamp());
            }
            Some(last_fire)
                if matches!(&parsed.trigger, Trigger::Watch { .. })
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
    (actions, next_state)
}

fn watcher_missing(task: &LoadedTask, name: &str) -> bool {
    let Ok(runtime) = RuntimePaths::for_workspace(WorkspaceId::from_project_root(
        &task.entry().resolved_root(),
    )) else {
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

fn state_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("loop-fire.json")
}

pub(super) fn spawn_loop_run(
    runtime: &RuntimePaths,
    project_root: Option<&Path>,
    name: &str,
    signal_json: Option<&str>,
    host: LoopRunHost,
) {
    let args = loop_run_args(project_root, name, signal_json);
    let (program, args) = loop_run_command(host, &crate::proc::rimz_exe(), &args, name);
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        task = name,
        "loop scheduler firing task",
    );
    let spawned = if host == LoopRunHost::Detached {
        crate::child_process::spawn_detached_rimz(runtime, &args, "loop-run").map(|()| None)
    } else {
        crate::child_process::spawn_detached_program(&program, &args, runtime, "loop-run")
    };
    match spawned {
        Ok(Some(pid)) if host == LoopRunHost::TransientScope => wait_for_scope(pid, name),
        Ok(_) => {}
        Err(err) => tracing::warn!(
            task = name,
            tags.operation = "loop_fire.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn loop task",
        ),
    }
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

fn wait_for_scope(pid: u32, name: &str) {
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
            ScopeHandoff::HandedOff => return,
            ScopeHandoff::ChildGone => {
                tracing::warn!(
                    task = name,
                    pid,
                    "loop run exited before the timer scope hand-off was confirmed"
                );
                return;
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
            return;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(25)));
    }
}

pub(super) fn wait_loop_run(
    runtime: &RuntimePaths,
    project_root: Option<&Path>,
    name: &str,
    signal_json: &str,
) {
    let mut command = crate::child_process::detached_rimz_command(crate::proc::rimz_exe(), runtime);
    command.args(loop_run_args(project_root, name, Some(signal_json)));
    match command.status() {
        Ok(status) if status.success() => {}
        Ok(status) => tracing::warn!(task = name, %status, "watched wake delivery failed"),
        Err(err) => tracing::warn!(task = name, error = %err, "running watched wake delivery"),
    }
}

fn loop_run_args(
    project_root: Option<&Path>,
    name: &str,
    signal_json: Option<&str>,
) -> Vec<OsString> {
    let mut args = Vec::<OsString>::new();
    if let Some(project_root) = project_root {
        args.extend([
            OsString::from("--root"),
            project_root.as_os_str().to_owned(),
        ]);
    }
    args.extend([OsString::from("loop"), OsString::from("run"), name.into()]);
    if let Some(signal_json) = signal_json {
        args.extend([OsString::from("--signal-json"), signal_json.into()]);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::super::catalog::TaskSource;
    use super::super::tests::{seconds_before, zdt};
    use super::*;
    use crate::config::TaskEntry;

    const NAME: &str = "task";

    #[test]
    fn loop_run_hosts_render_precise_argv() {
        let exe = Path::new("/opt/RimZ build/rimz");
        let args = loop_run_args(
            Some(Path::new("/workspace with spaces")),
            NAME,
            Some(r#"{"name":"deploy.done"}"#),
        );
        assert_eq!(
            loop_run_command(LoopRunHost::Detached, exe, &args, NAME),
            (exe.as_os_str().to_owned(), args.clone()),
        );
        assert_eq!(
            loop_run_command(LoopRunHost::IsolatedProcessGroup, exe, &args, NAME),
            (exe.as_os_str().to_owned(), args.clone()),
        );
        assert_eq!(
            loop_run_command(LoopRunHost::TransientScope, exe, &args, NAME),
            (
                OsString::from("systemd-run"),
                [
                    "--user",
                    "--scope",
                    "--quiet",
                    "--collect",
                    "--description",
                    "RimZ loop run task",
                    "--",
                    "/opt/RimZ build/rimz",
                    "--root",
                    "/workspace with spaces",
                    "loop",
                    "run",
                    "task",
                    "--signal-json",
                    r#"{"name":"deploy.done"}"#,
                ]
                .map(OsString::from)
                .to_vec(),
            ),
        );
    }

    #[test]
    fn scope_handoff_requires_observed_distinct_cgroups() {
        for (parent, child) in [
            (
                b"0::/user.slice/loop.service\n".as_slice(),
                b"0::/user.slice/run-test.scope\n".as_slice(),
            ),
            (
                b"1:name=systemd:/user.slice/loop.service\n".as_slice(),
                b"1:name=systemd:/user.slice/run-test.scope\n".as_slice(),
            ),
        ] {
            assert!(cgroup_changed(Some(parent), Some(child)));
            assert!(!cgroup_changed(Some(parent), Some(parent)));
            assert!(!cgroup_changed(Some(parent), None));
            assert!(!cgroup_changed(None, Some(child)));
            assert!(!cgroup_changed(Some(parent), Some(b"")));
            assert!(!cgroup_changed(Some(b""), Some(child)));
        }
        assert!(!cgroup_changed(None, None));
    }

    #[test]
    fn scope_handoff_distinguishes_exit_from_pending_migration() {
        let parent = Some(b"0::/user.slice/loop.service\n".as_slice());
        let scoped = Some(b"0::/user.slice/run-test.scope\n".as_slice());
        for child in [parent, scoped, None, Some(b"".as_slice())] {
            assert_eq!(scope_handoff(parent, child, false), ScopeHandoff::ChildGone);
        }
        for child in [parent, None, Some(b"".as_slice())] {
            assert_eq!(scope_handoff(parent, child, true), ScopeHandoff::Pending);
        }
        assert_eq!(scope_handoff(parent, scoped, true), ScopeHandoff::HandedOff);
        assert_eq!(scope_handoff(None, scoped, true), ScopeHandoff::Pending);
        assert_eq!(scope_handoff(None, None, false), ScopeHandoff::ChildGone);
    }

    #[test]
    fn all_loop_run_hosts_suppress_subprocesses_in_unit_tests() {
        let root = Path::new("/missing-loop-test-root");
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(root), root).unwrap();
        for host in [
            LoopRunHost::Detached,
            LoopRunHost::IsolatedProcessGroup,
            LoopRunHost::TransientScope,
        ] {
            spawn_loop_run(&runtime, Some(root), NAME, None, host);
        }
    }

    fn one<T>(value: T) -> BTreeMap<String, T> {
        BTreeMap::from([(NAME.to_owned(), value)])
    }

    fn loaded(entry: TaskEntry) -> LoadedTask {
        LoadedTask::new(NAME, entry, TaskSource::Config)
    }

    fn loaded_from(entry: TaskEntry, source: TaskSource) -> LoadedTask {
        LoadedTask::new(NAME, entry, source)
    }

    fn task(root: &str, every: &str) -> LoadedTask {
        loaded(TaskEntry {
            agent: Some("claude".to_owned()),
            prompt: Some("do it".to_owned()),
            root: PathBuf::from(root),
            every: Some(every.to_owned()),
            ..TaskEntry::default()
        })
    }

    fn until(stamp: Timestamp) -> Arming {
        Arming {
            enabled: true,
            at: Timestamp::from_second(0).ok(),
            pause_until: Some(stamp),
            strikes: None,
        }
    }

    /// Durable inputs for one elder tick over a single task. The default is the
    /// never-seen, live case.
    #[derive(Default)]
    struct Tick {
        state: Option<Timestamp>,
        arming: Option<Arming>,
    }

    impl Tick {
        /// A task the elder has already stamped once.
        fn armed(state: Timestamp) -> Self {
            Self {
                state: Some(state),
                ..Self::default()
            }
        }

        fn held(self, arming: Arming) -> Self {
            Self {
                arming: Some(arming),
                ..self
            }
        }

        /// The task's action this tick and the stamp carried into the next one.
        fn run(self, task: &LoadedTask, now: &Zoned) -> (Option<Action>, Option<Timestamp>) {
            let (actions, next) = plan(
                &one(task.clone()),
                &self.state.map(one).unwrap_or_default(),
                &self
                    .arming
                    .map(|arming| BTreeMap::from([(task.key(NAME), arming)]))
                    .unwrap_or_default(),
                now,
            );
            (
                actions.first().map(|(_, action)| *action),
                next.get(NAME).copied(),
            )
        }
    }

    fn arm(stamp: Timestamp) -> (Option<Action>, Option<Timestamp>) {
        (Some(Action::Arm), Some(stamp))
    }

    fn fire(stamp: Timestamp) -> (Option<Action>, Option<Timestamp>) {
        (Some(Action::Fire), Some(stamp))
    }

    fn watch_lost(stamp: Timestamp) -> (Option<Action>, Option<Timestamp>) {
        (Some(Action::WatchLost), Some(stamp))
    }

    fn carry(stamp: Timestamp) -> (Option<Action>, Option<Timestamp>) {
        (None, Some(stamp))
    }

    /// Arm on first sight, fire once due, otherwise carry the prior stamp. A row
    /// the elder cannot schedule leaves no stamp at all.
    #[test]
    fn plan_arms_fires_and_carries_each_task_state() {
        let now = zdt(2026, 6, 24, 8, 5, 0);
        let stamp = now.timestamp();
        let due = seconds_before(stamp, 300);
        let early = seconds_before(stamp, 240);
        let prior = seconds_before(stamp, 600);
        let manual = Arming {
            enabled: false,
            at: Some(stamp),
            pause_until: None,
            strikes: Some(3),
        };
        let every_5m = &task("/repo", "5m");
        // `every` and `at` together are a time conflict, so this row never parses.
        let malformed = &loaded(TaskEntry {
            agent: Some("claude".to_owned()),
            every: Some("5m".to_owned()),
            at: Some("07:00".to_owned()),
            ..TaskEntry::default()
        });

        let run = |tick: Tick, task| tick.run(task, &now);
        assert_eq!(run(Tick::default(), every_5m), arm(stamp), "first sight");
        assert_eq!(run(Tick::armed(due), every_5m), fire(stamp), "due");
        assert_eq!(
            run(Tick::armed(early), every_5m),
            carry(early),
            "not yet due"
        );
        assert_eq!(
            run(Tick::default().held(manual), every_5m),
            arm(stamp),
            "armed while paused"
        );
        assert_eq!(
            run(Tick::armed(prior).held(manual), every_5m),
            carry(prior),
            "held by a pause"
        );
        assert_eq!(
            run(Tick::armed(due), malformed),
            (None, None),
            "malformed is skipped"
        );

        // State for a task the catalog no longer holds is dropped, not carried.
        let (actions, next) = plan(&BTreeMap::new(), &one(due), &BTreeMap::new(), &now);
        assert!(actions.is_empty());
        assert!(next.is_empty());
    }

    #[test]
    fn signal_tasks_only_arm_while_missing_watchers_fire_after_the_grace() {
        let now = zdt(2026, 6, 24, 8, 5, 0);
        let signal = loaded(TaskEntry {
            agent: Some("claude".to_owned()),
            signal: Some("ci.failed".to_owned()),
            ..TaskEntry::default()
        });
        assert_eq!(Tick::default().run(&signal, &now), arm(now.timestamp()));
        let prior = seconds_before(now.timestamp(), 300);
        assert_eq!(Tick::armed(prior).run(&signal, &now), carry(prior));

        let root = tempfile::tempdir().unwrap();
        let watch = loaded(TaskEntry {
            agent: Some("claude".to_owned()),
            root: root.path().to_path_buf(),
            watch: Some("cargo test".to_owned()),
            ..TaskEntry::default()
        });
        assert_eq!(Tick::default().run(&watch, &now), arm(now.timestamp()));
        let recent = seconds_before(now.timestamp(), WATCH_LOST_GRACE_SECS);
        assert_eq!(Tick::armed(recent).run(&watch, &now), carry(recent));

        let stale = seconds_before(now.timestamp(), WATCH_LOST_GRACE_SECS + 1);
        let runtime = RuntimePaths::for_workspace(WorkspaceId::from_project_root(root.path()))
            .expect("watch runtime");
        std::fs::create_dir_all(&runtime.root).expect("runtime root");
        let guard = super::super::signal::acquire_watch_lock(&runtime, NAME)
            .unwrap()
            .expect("watch lock");
        assert_eq!(Tick::armed(stale).run(&watch, &now), carry(stale));
        drop(guard);
        assert_eq!(
            Tick::armed(stale).run(&watch, &now),
            watch_lost(now.timestamp())
        );
        let outcome = lost_watch_outcome(&watch, NAME, stale, now.timestamp()).unwrap();
        assert!(outcome.verdict.elapsed_ms() >= WATCH_LOST_GRACE_SECS as u64 * 1_000);
        assert!(outcome.output.is_empty());
        assert_eq!(
            outcome.summary,
            crate::disk::summary::FileSummary::default()
        );
        let paths = StatePaths::for_workspace(WorkspaceId::from_project_root(root.path())).unwrap();
        let path = super::super::signal::wake_output_path(&paths, NAME);
        assert_eq!(outcome.output_path, Some(path.clone()));
        paths.ensure_tmp_dir().unwrap();
        std::fs::write(&path, "watcher failed before launching command").unwrap();
        let watch = loaded(TaskEntry {
            wake_meta: Some(crate::config::WakeMeta {
                armed_at: prior,
                delay: None,
            }),
            ..watch.entry().clone()
        });
        let outcome = lost_watch_outcome(&watch, NAME, stale, now.timestamp()).unwrap();
        assert_eq!(outcome.verdict.elapsed_ms(), 300_000);
        assert_eq!(outcome.output, "watcher failed before launching command");
        assert_eq!(outcome.summary.bytes, 39);
        assert_eq!(outcome.summary.lines, 1);
        let sandboxed = super::super::signal::WatchOutcome::measured(
            outcome.verdict,
            outcome.output,
            &path,
            &crate::sandbox::TmpView::new(crate::config::Isolation::Sandbox, &paths),
        );
        assert_eq!(
            sandboxed.output_path,
            Some(PathBuf::from(format!("/tmp/rimz-wakes/{NAME}.output")))
        );
        assert_eq!(sandboxed.summary, outcome.summary);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn invalid_renamed_watcher_signal_does_not_stop_elder_fire() {
        let root = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::for_workspace(WorkspaceId::from_project_root(root.path()))
            .expect("watch runtime");
        std::fs::create_dir_all(&runtime.root).expect("runtime root");
        let now = zdt(2026, 6, 24, 8, 5, 0);
        let stale = seconds_before(now.timestamp(), WATCH_LOST_GRACE_SECS + 1);
        write_temp_then_rename_cache(
            &state_path(&runtime),
            &BTreeMap::from([("Renamed".to_owned(), stale), ("valid".to_owned(), stale)]),
        )
        .expect("fire state");
        let watch = |name| {
            LoadedTask::new(
                name,
                TaskEntry {
                    agent: Some("claude".to_owned()),
                    root: root.path().to_path_buf(),
                    watch: Some("cargo test".to_owned()),
                    ..TaskEntry::default()
                },
                TaskSource::Config,
            )
        };
        let tasks = BTreeMap::from([
            ("Renamed".to_owned(), watch("Renamed")),
            ("valid".to_owned(), watch("valid")),
        ]);

        assert_eq!(
            fire_tasks(
                &runtime,
                Some(root.path()),
                tasks,
                &now,
                LoopRunHost::Detached
            ),
            vec!["valid"]
        );
    }

    /// An ended pause becomes the effective last-fire edge, so a lifted task
    /// waits out a full interval and never replays an occurrence it slept through.
    #[test]
    fn ended_pause_sets_the_effective_fire_edge() {
        let now = zdt(2026, 6, 24, 8, 0, 0);
        let interval_task = &task("/repo", "5m");
        let pause_end = seconds_before(now.timestamp(), 240);
        let stale_stamp = seconds_before(pause_end, 600);
        let resumed = || Tick::armed(stale_stamp).held(until(pause_end));

        assert_eq!(
            resumed().run(interval_task, &now),
            carry(stale_stamp),
            "the interval restarts from the pause end, not the stale stamp"
        );
        let interval_due = zdt(2026, 6, 24, 8, 1, 0);
        assert_eq!(
            resumed().run(interval_task, &interval_due),
            fire(interval_due.timestamp()),
            "a full interval past the pause end fires"
        );

        let daily = &loaded(TaskEntry {
            agent: Some("claude".to_owned()),
            prompt: Some("do it".to_owned()),
            root: PathBuf::from("/repo"),
            every: Some("day".to_owned()),
            at: Some("07:00".to_owned()),
            ..TaskEntry::default()
        });
        let slept_through = zdt(2026, 6, 23, 6, 0, 0).timestamp();
        let woke_at_0730 =
            || Tick::armed(slept_through).held(until(zdt(2026, 6, 24, 7, 30, 0).timestamp()));
        assert_eq!(
            woke_at_0730().run(daily, &zdt(2026, 6, 24, 8, 0, 0)),
            carry(slept_through),
            "the 07:00 occurrence crossed while paused is not replayed"
        );
        assert_eq!(
            woke_at_0730().run(daily, &zdt(2026, 6, 25, 7, 0, 0)).0,
            Some(Action::Fire),
            "the next day's occurrence fires normally"
        );
    }

    #[test]
    fn project_task_stays_held_until_enable_and_does_not_replay() {
        let now = zdt(2026, 6, 24, 8, 5, 0);
        let prior = seconds_before(now.timestamp(), 600);
        let task = loaded_from(
            TaskEntry {
                agent: Some("claude".to_owned()),
                prompt: Some("do it".to_owned()),
                root: PathBuf::from("/repo"),
                every: Some("5m".to_owned()),
                ..TaskEntry::default()
            },
            TaskSource::Project {
                state: crate::trust::TrustState::Trusted,
            },
        );

        assert_eq!(
            Tick::default().run(&task, &now),
            arm(now.timestamp()),
            "an unstamped disabled task still gets its first-sight stamp"
        );
        assert_eq!(
            Tick::armed(prior).run(&task, &now),
            carry(prior),
            "a project task without a local enable stays held"
        );
        let enabled = Arming {
            enabled: true,
            at: Some(now.timestamp()),
            pause_until: None,
            strikes: None,
        };
        assert_eq!(
            Tick::armed(prior).held(enabled).run(&task, &now),
            carry(prior),
            "enabling establishes a fresh replay edge"
        );
        let due = zdt(2026, 6, 24, 8, 10, 0);
        assert_eq!(
            Tick::armed(prior).held(enabled).run(&task, &due),
            fire(due.timestamp()),
            "the next occurrence fires normally"
        );
    }

    /// Tasks are filtered by resolved root, so a `~` root reaches its own room.
    #[test]
    fn workspace_filter_keeps_only_this_rooms_roots() {
        let owned = WorkspaceId::from_project_root(Path::new("/repo/owned"));
        let filtered = workspace_tasks(
            BTreeMap::from([
                ("owned".to_owned(), task("/repo/owned", "5m")),
                ("foreign".to_owned(), task("/repo/foreign", "5m")),
            ]),
            &owned,
        );
        assert_eq!(filtered.keys().cloned().collect::<Vec<_>>(), vec!["owned"]);

        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        let home = home.canonicalize().unwrap_or(home);
        let filtered = workspace_tasks(
            BTreeMap::from([("home".to_owned(), task("~", "5m"))]),
            &WorkspaceId::from_project_root(&home),
        );
        assert_eq!(filtered.keys().cloned().collect::<Vec<_>>(), vec!["home"]);
    }
}
