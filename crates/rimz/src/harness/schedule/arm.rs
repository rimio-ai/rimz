//! Shared construction and lifecycle of session-pinned delivery tasks.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::time::Duration;

use jiff::Timestamp;

use super::catalog::{LoadedTask, TaskSource};
use super::signal::SignalSelector;
use super::{ParsedSchedule, Schedule, ScheduleErr};
use crate::agents::AgentState;
use crate::config::{CheckOn, TaskEntry, TaskTarget, WaitMeta, WatchSpec};
use crate::ids::TeamInstanceId;
use crate::workspace::ResolvedWorkspace;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskName(String);

impl FromStr for TaskName {
    type Err = ScheduleErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        super::validate_name(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl std::fmt::Display for TaskName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub enum DeliveryName {
    MintWait,
    Named(TaskName),
}

pub enum SubscriptionLifetime {
    Once,
    Standing,
}

pub enum DeliveryTrigger {
    Condition {
        expr: super::when::WhenExpr,
        hold: Option<Duration>,
        lifetime: SubscriptionLifetime,
    },
    Delay(Duration),
    /// `on` is the row's delivery polarity: the user's `--on` for a command,
    /// `any` for a polled spec, whose watcher emits only once its condition holds.
    Watch {
        spec: WatchSpec,
        on: CheckOn,
        timeout: Option<Duration>,
    },
    Signal {
        selector: SignalSelector,
        matches: BTreeMap<String, String>,
        lifetime: SubscriptionLifetime,
    },
    Clock(ParsedSchedule),
}

pub enum DeliveryPrompt {
    None,
    Inline(String),
    File(PathBuf),
}

pub enum DeliveryProvenance {
    /// `reader` is the arming agent's handle, whose `out/` directory receives
    /// a watched command's output.
    SelfWait {
        reader: Option<String>,
    },
    Loop,
    Resident(String),
    Team(TeamInstanceId),
}

pub struct DeliveryCheck {
    pub command: String,
    pub on: Option<CheckOn>,
    pub timeout: Option<String>,
}

pub struct DeliverySurplus {
    pub ratio: Option<String>,
    pub after: Option<String>,
}

pub struct DeliverySpec {
    pub name: DeliveryName,
    pub label: Option<String>,
    pub target: TaskTarget,
    pub trigger: DeliveryTrigger,
    pub prompt: DeliveryPrompt,
    pub provenance: DeliveryProvenance,
    pub check: Option<DeliveryCheck>,
    pub deadline: Option<Timestamp>,
    pub max_strikes: Option<u32>,
    pub surplus: Option<DeliverySurplus>,
}

pub enum ArmOutcome {
    Armed { name: String, task: Box<LoadedTask> },
    AlreadySubscribed { name: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ArmFailure {
    #[error(transparent)]
    Schedule(#[from] ScheduleErr),
    #[error("{0}")]
    InvalidProvenance(&'static str),
    #[error("updating delivery task state: {0}")]
    State(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("starting watched command: {0}")]
    Watcher(#[source] std::io::Error),
    #[error("{0}")]
    Scope(#[from] DeliveryScopeFailure),
    #[error("loop task `{0}` is configuration-owned; rename it or choose another delivery name")]
    ConfigOwned(String),
}

pub fn arm_delivery(
    workspace: &ResolvedWorkspace,
    spec: DeliverySpec,
) -> Result<ArmOutcome, ArmFailure> {
    let (name, task) = build_entry(workspace, spec, Timestamp::now())?;
    let entry = task.entry();
    let catalog = super::catalog::TaskCatalog::load(Some(&workspace.project_root))
        .map_err(|err| ArmFailure::State(err.into()))?;
    let name = match &name {
        DeliveryName::MintWait => None,
        DeliveryName::Named(name) => Some(name.0.as_str()),
    };
    if let Some(name) = name
        && catalog.visible().get(name).is_some_and(|task| {
            matches!(task.source(), super::catalog::TaskSource::Project { .. })
                || ((entry.team.is_some() || entry.loop_task.is_some())
                    && task.source() == super::catalog::TaskSource::Config)
        })
    {
        return Err(ArmFailure::ConfigOwned(name.to_owned()));
    }
    let paths = crate::disk::paths::StatePaths::for_project_root(&workspace.project_root)
        .map_err(|err| ArmFailure::State(Box::new(err)))?;
    let taken = catalog
        .visible()
        .iter()
        .filter(|(_, task)| task.source() != super::catalog::TaskSource::Instance)
        .map(|(name, _)| name.clone())
        .collect();
    let (name, duplicate) = super::instances::insert_delivery(&paths, name, entry, &taken)
        .map_err(|err| ArmFailure::State(Box::new(err)))?;
    if duplicate {
        return Ok(ArmOutcome::AlreadySubscribed { name });
    }
    if entry.team.is_none() && entry.loop_task.is_none() {
        super::config_edit::remove(super::config_edit::TaskStore::Machine, &name)
            .map_err(|err| ArmFailure::State(err.into()))?;
    }
    if entry.watch.is_some() {
        let spawn = || -> std::io::Result<()> {
            let path = super::signal::wait_output_path(&paths, &name, entry);
            for dir in [
                paths.out_dir.as_path(),
                path.parent().unwrap_or(&paths.out_dir),
            ] {
                crate::disk::paths::ensure_private_runtime_dir(dir)
                    .map_err(std::io::Error::other)?;
            }
            let output = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)?;
            let mut command = Command::new(crate::proc::rimz_exe());
            command
                .arg("--root")
                .arg(&workspace.project_root)
                .args(["wait", "watch", &name])
                .current_dir(&workspace.project_root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::from(output))
                .process_group(0);
            // The watcher runs the agent's own command, which writes where the
            // agent was told its temp files live.
            crate::child_process::spawn_detached_reaped_as_agent(&mut command, "wait-watch")?;
            Ok(())
        };
        if let Err(error) = spawn() {
            super::instances::remove(&paths, &name, Some(entry))
                .map_err(|err| ArmFailure::State(Box::new(err)))?;
            return Err(ArmFailure::Watcher(error));
        }
    }
    Ok(ArmOutcome::Armed {
        name,
        task: Box::new(task),
    })
}

#[derive(Debug, thiserror::Error)]
#[error("retiring session deliveries: {message}")]
pub struct RetireFailure {
    message: String,
    dropped: usize,
}

impl RetireFailure {
    fn before_removal(err: impl std::fmt::Display) -> Self {
        Self {
            message: err.to_string(),
            dropped: 0,
        }
    }

    /// A failure after `dropped` rows went, for a consumer's test.
    #[cfg(any(test, feature = "testkit"))]
    pub fn after_dropping(dropped: usize) -> Self {
        Self {
            message: "withdrawal failed".to_owned(),
            dropped,
        }
    }

    /// How many rows nothing arms again were removed before the failure: the
    /// count `Ok` would have carried.
    pub fn dropped(&self) -> usize {
        self.dropped
    }
}

/// Which of a session's rows a retirement takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetireScope {
    /// Every row: the session is over and its identity will not come back.
    Session,
    /// Only the rows no later registration re-arms. A same-session
    /// `agents restart` is a continuation, so team and resident-loop bindings
    /// carry over rather than depending on an order nothing enforces.
    UnrestorableOnly,
}

/// Retire the instance rows pinned to `(kind, session)` that `scope` selects,
/// paused and disabled rows included, and clear each one's watcher, arming and
/// strike overlays.
///
/// Returns how many of the removed rows nothing arms again: self waits and
/// `loop add --wait` rows. Team and resident-loop bindings, restored at the
/// member's next registration, stay out of that count.
/// Overlay cleanup is best-effort and warns instead of failing, so the count is
/// reported whatever the overlays do; `Err` means the durable map could not be
/// rewritten and nothing was removed, or the session's idle-stop request could
/// not be withdrawn after its rows went, and carries the count either way
/// ([`RetireFailure::dropped`]).
pub fn retire_session(
    project_root: &Path,
    kind: &crate::ids::AgentKind,
    session: &crate::ids::AgentSessionId,
    scope: RetireScope,
) -> Result<usize, RetireFailure> {
    let paths = crate::disk::paths::StatePaths::for_project_root(project_root)
        .map_err(RetireFailure::before_removal)?;
    let runtime = crate::disk::paths::RuntimePaths::for_project_root(project_root)
        .map_err(RetireFailure::before_removal)?;
    retire_session_in(&paths, &runtime, project_root, kind, session, scope)
}

fn retire_session_in(
    paths: &crate::disk::paths::StatePaths,
    runtime: &crate::disk::paths::RuntimePaths,
    project_root: &Path,
    kind: &crate::ids::AgentKind,
    session: &crate::ids::AgentSessionId,
    scope: RetireScope,
) -> Result<usize, RetireFailure> {
    let retired = super::instances::retire_session(paths, kind, session, scope)
        .map_err(RetireFailure::before_removal)?;
    let mut dropped = 0;
    for (name, entry) in &retired {
        if entry.team.is_none() && entry.loop_task.is_none() {
            dropped += 1;
        }
        let key = super::arming::TaskKey::for_task(
            name,
            super::catalog::TaskSource::Instance,
            project_root,
        );
        if let Err(err) = super::signal::stop_watcher(runtime, name) {
            tracing::warn!(task = name, error = %err, "retire: stopping the watcher");
        }
        if let Err(err) = super::arming::remove(&key) {
            tracing::warn!(task = name, error = %err, "retire: clearing the arming overlay");
        }
        if let Err(err) = super::strikes::clear(&key) {
            tracing::warn!(task = name, error = %err, "retire: clearing the strike overlay");
        }
    }
    // A pending idle stop belongs to the pane it would close, so it goes under
    // either scope: a resumed restart must not inherit it.
    crate::store::idle_stop::withdraw(paths, kind, session).map_err(|err| RetireFailure {
        message: err.to_string(),
        dropped,
    })?;
    Ok(dropped)
}

/// Retire every instance row in this workspace pinned to a durably ended
/// session, whoever ended it.
///
/// This is the one place the retirement predicate is spelled: an agent row that
/// is not a provider subagent, matches the pinned `(kind, session)`, and carries
/// an `ended_at`. A session whose latest lifecycle event revived it has no
/// `ended_at` and stays; a target with no agent row at all is gc's business.
///
/// `agents` is read only once this workspace is known to hold a pinned row or
/// an idle-stop request, so a caller with neither pays two file reads.
pub fn retire_ended_sessions<'a>(
    project_root: &Path,
    agents: impl FnOnce() -> std::borrow::Cow<'a, [AgentState]>,
) -> Result<usize, RetireFailure> {
    let paths = crate::disk::paths::StatePaths::for_project_root(project_root)
        .map_err(RetireFailure::before_removal)?;
    let runtime = crate::disk::paths::RuntimePaths::for_project_root(project_root)
        .map_err(RetireFailure::before_removal)?;
    retire_ended_sessions_in(&paths, &runtime, project_root, agents)
}

fn retire_ended_sessions_in<'a>(
    paths: &crate::disk::paths::StatePaths,
    runtime: &crate::disk::paths::RuntimePaths,
    project_root: &Path,
    agents: impl FnOnce() -> std::borrow::Cow<'a, [AgentState]>,
) -> Result<usize, RetireFailure> {
    let mut pinned = super::instances::pinned_sessions(&paths.root);
    pinned.extend(
        crate::store::idle_stop::read(paths)
            .into_iter()
            .map(|request| (request.kind, request.agent_id)),
    );
    if pinned.is_empty() {
        return Ok(0);
    }
    let mut dropped = 0;
    let mut failures = Vec::new();
    for (kind, session) in ended_pinned_sessions(&agents(), &pinned) {
        match retire_session_in(
            paths,
            runtime,
            project_root,
            kind,
            session,
            RetireScope::Session,
        ) {
            Ok(count) => dropped += count,
            Err(err) => {
                dropped += err.dropped;
                failures.push(err.message);
            }
        }
    }
    if !failures.is_empty() {
        return Err(RetireFailure {
            message: failures.join("; "),
            dropped,
        });
    }
    Ok(dropped)
}

/// The retirement predicate: which of the `pinned` sessions `agents` proves
/// durably ended. Collecting the ended identities first keeps the scan over an
/// audit list, which holds every session the room ever saw, allocation-free.
fn ended_pinned_sessions<'a>(
    agents: &[AgentState],
    pinned: &'a std::collections::BTreeSet<(crate::ids::AgentKind, crate::ids::AgentSessionId)>,
) -> Vec<&'a (crate::ids::AgentKind, crate::ids::AgentSessionId)> {
    let ended = agents
        .iter()
        .filter(|agent| agent.ended_at.is_some() && !agent.is_provider_subagent())
        .map(|agent| (&agent.kind, &agent.agent_id))
        .collect::<std::collections::BTreeSet<_>>();
    pinned
        .iter()
        .filter(|(kind, session)| ended.contains(&(kind, session)))
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryScopeFailure {
    #[error(
        "A ci.* or pr.* binding from the root checkout needs an explicit scope. Pass --match branch=<name>, --match path=<worktree-path>, or --match branch='*' for every checkout RimZ watches, or watch it with: rimz wait --run 'gh run watch --exit-status'"
    )]
    RootCheckout,
    #[error("team.* waits need a team member; pass --match instance=<team#channel>")]
    NoTeam,
    #[error(
        "--wait on an agent.* signal requires --match handle=<other> or --match session=<other> to avoid waking the target from its own lifecycle signal"
    )]
    SelfSignal,
}

pub fn default_signal_match_key(
    selector: &SignalSelector,
    matches: &BTreeMap<String, String>,
) -> Option<&'static str> {
    match selector.family() {
        "ci" | "pr" if !matches.contains_key("path") && !matches.contains_key("branch") => {
            Some("path")
        }
        "team" if !matches.contains_key("team") && !matches.contains_key("instance") => {
            Some("instance")
        }
        _ => None,
    }
}

pub fn default_signal_matches(
    workspace: &ResolvedWorkspace,
    agents: &[AgentState],
    scope: &AgentState,
    selector: &SignalSelector,
    matches: &mut BTreeMap<String, String>,
) -> Result<(), DeliveryScopeFailure> {
    match default_signal_match_key(selector, matches) {
        Some("path") => {
            let path = scope
                .worktree_path
                .as_deref()
                .map(std::path::Path::new)
                .unwrap_or(&workspace.worktree_root);
            if path == workspace.project_root {
                return Err(DeliveryScopeFailure::RootCheckout);
            }
            matches.insert("path".to_owned(), path.display().to_string());
        }
        Some("instance") => {
            let cohort = crate::address::team_cohorts(agents)
                .into_iter()
                .find(|cohort| cohort.contains(scope))
                .ok_or(DeliveryScopeFailure::NoTeam)?;
            matches.insert(
                "instance".to_owned(),
                format!("{}#{}", cohort.team, cohort.channel),
            );
        }
        _ => {}
    }
    Ok(())
}

pub fn validate_self_signal(
    selector: &SignalSelector,
    matches: &BTreeMap<String, String>,
    target: &TaskTarget,
) -> Result<(), DeliveryScopeFailure> {
    if selector.family() != "agent" {
        return Ok(());
    }
    fn handle_name(handle: &str) -> &str {
        handle.split_once('#').map_or(handle, |(name, _)| name)
    }
    let other = matches.get("handle").is_some_and(|handle| {
        !handle.is_empty()
            && !super::signal::is_match_wildcard(handle)
            && handle_name(handle) != handle_name(&target.handle)
    }) || matches.get("session").is_some_and(|session| {
        !session.is_empty()
            && !super::signal::is_match_wildcard(session)
            && session != &target.session
    });
    if other {
        return Ok(());
    }
    Err(DeliveryScopeFailure::SelfSignal)
}

fn build_entry(
    workspace: &ResolvedWorkspace,
    spec: DeliverySpec,
    now: Timestamp,
) -> Result<(DeliveryName, LoadedTask), ArmFailure> {
    let self_wait = matches!(spec.provenance, DeliveryProvenance::SelfWait { .. });
    if self_wait
        && (!matches!(
            spec.trigger,
            DeliveryTrigger::Delay(_) | DeliveryTrigger::Watch { .. }
        ) || !matches!(spec.prompt, DeliveryPrompt::None)
            || spec.check.is_some()
            || spec.surplus.is_some()
            || spec.deadline.is_some())
    {
        return Err(ArmFailure::InvalidProvenance(
            "self waits require a timer or watched command without a prompt or guard",
        ));
    }
    if matches!(
        spec.provenance,
        DeliveryProvenance::Team(_) | DeliveryProvenance::Resident(_)
    ) && !matches!(
        spec.trigger,
        DeliveryTrigger::Signal {
            lifetime: SubscriptionLifetime::Standing,
            ..
        }
    ) {
        return Err(ArmFailure::InvalidProvenance(
            "declared bindings require a standing signal subscription",
        ));
    }
    let mut entry = TaskEntry {
        label: spec.label,
        wait: Some(spec.target),
        root: workspace.project_root.clone(),
        dir: (workspace.worktree_root != workspace.project_root)
            .then(|| workspace.worktree_root.clone()),
        deadline: spec.deadline,
        max_strikes: spec.max_strikes,
        ..TaskEntry::default()
    };
    let reader = match spec.provenance {
        DeliveryProvenance::Team(instance) => {
            entry.team = Some(instance);
            None
        }
        DeliveryProvenance::Resident(task) => {
            entry.loop_task = Some(task);
            None
        }
        DeliveryProvenance::SelfWait { reader } => reader,
        DeliveryProvenance::Loop => None,
    };
    match spec.prompt {
        DeliveryPrompt::None => {}
        DeliveryPrompt::Inline(prompt) => entry.prompt = Some(prompt),
        DeliveryPrompt::File(path) => entry.prompt_file = Some(path),
    }
    if let Some(check) = spec.check {
        entry.check = Some(check.command);
        entry.on = check.on;
        entry.timeout = check.timeout;
    }
    if let Some(surplus) = spec.surplus {
        entry.surplus = surplus.ratio;
        entry.surplus_after = surplus.after;
    }
    let delay = match spec.trigger {
        DeliveryTrigger::Condition {
            expr,
            hold,
            lifetime,
        } => {
            if expr.window_spans().next().is_some() {
                entry.provider = entry.wait.as_ref().map(|target| target.kind.clone());
            }
            entry.when = Some(vec![expr.to_string()]);
            entry.hold = hold.map(duration_label);
            entry.once = matches!(lifetime, SubscriptionLifetime::Once).then_some(true);
            None
        }
        DeliveryTrigger::Delay(delay) => {
            entry.at = Some(super::delayed_at(delay).map_err(|err| ArmFailure::State(err.into()))?);
            Some(duration_label(delay))
        }
        DeliveryTrigger::Watch { spec, on, timeout } => {
            entry.watch = Some(spec);
            entry.on = Some(on);
            entry.timeout = timeout.map(duration_label);
            None
        }
        DeliveryTrigger::Signal {
            selector,
            matches,
            lifetime,
        } => {
            validate_self_signal(
                &selector,
                &matches,
                entry.wait.as_ref().expect("delivery target was set above"),
            )?;
            entry.signal = Some(selector.to_string());
            entry.matches = (!matches.is_empty()).then_some(matches);
            entry.once = matches!(lifetime, SubscriptionLifetime::Once).then_some(true);
            None
        }
        DeliveryTrigger::Clock(parsed) => {
            match parsed.schedule {
                Schedule::Instant(at) => entry.fire_at = Some(at),
                Schedule::RawCron(cron) => entry.cron = Some(cron),
                Schedule::Interval(interval) => {
                    entry.every = Some(format!("{}m", interval.minutes))
                }
                Schedule::Calendar(calendar) => {
                    entry.at = Some(format!("{:02}:{:02}", calendar.hour, calendar.minute));
                    if !parsed.once {
                        entry.every = Some(if calendar.weekdays.is_empty() {
                            "day".to_owned()
                        } else {
                            calendar
                                .weekdays
                                .iter()
                                .map(|day| super::short_day_name(*day))
                                .collect::<Vec<_>>()
                                .join(",")
                        });
                    }
                }
            }
            None
        }
    };
    if self_wait {
        entry.wait_meta = Some(WaitMeta {
            armed_at: now,
            delay,
            reader,
        });
    }
    let name = match &spec.name {
        DeliveryName::MintWait => "wait",
        DeliveryName::Named(name) => &name.0,
    };
    let task = LoadedTask::new(name, entry, TaskSource::Instance);
    task.trigger().as_ref().map_err(Clone::clone)?;
    Ok((spec.name, task))
}

pub fn duration_label(duration: Duration) -> String {
    let seconds = duration.as_secs();
    for (unit, suffix) in [(86_400, "d"), (3_600, "h"), (60, "m")] {
        if seconds >= unit && seconds.is_multiple_of(unit) {
            return format!("{}{suffix}", seconds / unit);
        }
    }
    format!("{seconds}s")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    #[test]
    fn self_signal_requires_a_concrete_other_agent() {
        let target = TaskTarget {
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            session: "self".into(),
            handle: "@self".to_owned(),
        };
        for key in ["handle", "session"] {
            let matches = BTreeMap::from([(key.to_owned(), "*".to_owned())]);
            assert!(matches!(
                validate_self_signal(&"agent.ended".parse().unwrap(), &matches, &target),
                Err(DeliveryScopeFailure::SelfSignal)
            ));
        }
    }

    fn idle_stop_fixture() -> (
        tempfile::TempDir,
        crate::disk::paths::StatePaths,
        crate::disk::paths::RuntimePaths,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = crate::ids::WorkspaceId::from_project_root(dir.path());
        let paths = crate::disk::paths::StatePaths::under(id.clone(), &dir.path().join("state"))
            .expect("state paths");
        let workspace = crate::workspace::WorkspaceResolver::resolve(dir.path(), None).unwrap();
        crate::workspace::record::write(
            &paths,
            &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
        )
        .unwrap();
        let runtime = crate::disk::paths::RuntimePaths::under(id, &dir.path().join("runtime"))
            .expect("runtime paths");
        for session in ["stopping", "resting"] {
            crate::store::idle_stop::arm(
                &paths,
                crate::store::idle_stop::IdleStopRequest {
                    kind: crate::ids::AgentKind::new_unchecked("claude"),
                    agent_id: session.into(),
                    stop: crate::agents::state::IdleStop {
                        after_secs: 180,
                        requested_at: Timestamp::UNIX_EPOCH,
                        requested_by: None,
                    },
                },
            )
            .expect("arm idle stop");
        }
        (dir, paths, runtime)
    }

    fn idle_stop_sessions(paths: &crate::disk::paths::StatePaths) -> Vec<String> {
        crate::store::idle_stop::read(paths)
            .into_iter()
            .map(|request| request.agent_id.to_string())
            .collect()
    }

    /// Hard stop, a fresh restart, and a durable end retire under `Session`; a
    /// resumed restart under `UnrestorableOnly`. Each takes the idle-stop request.
    #[test]
    fn retiring_a_session_removes_its_idle_stop_request_under_either_scope() {
        for scope in [RetireScope::Session, RetireScope::UnrestorableOnly] {
            let (dir, paths, runtime) = idle_stop_fixture();
            let dropped = retire_session_in(
                &paths,
                &runtime,
                dir.path(),
                &crate::ids::AgentKind::new_unchecked("claude"),
                &"stopping".into(),
                scope,
            )
            .expect("retire");
            assert_eq!(dropped, 0, "a request is not a wait row");
            assert_eq!(idle_stop_sessions(&paths), ["resting"], "{scope:?}");
        }
    }

    /// The rows go before the idle-stop request, so a withdrawal that fails
    /// still reports the waits already dropped.
    #[test]
    fn a_failed_withdrawal_carries_the_waits_already_dropped() {
        let (dir, mut paths, runtime) = idle_stop_fixture();
        let entry = TaskEntry {
            wait: Some(TaskTarget {
                kind: crate::ids::AgentKind::new_unchecked("claude"),
                session: "stopping".into(),
                handle: "@stopping".to_owned(),
            }),
            root: dir.path().to_path_buf(),
            ..TaskEntry::default()
        };
        super::super::instances::insert_delivery(&paths, None, &entry, &Default::default())
            .expect("wait row");
        // A directory cannot be opened as the withdrawal's lock file.
        paths.workspace_lock = dir.path().to_path_buf();
        let now = Timestamp::UNIX_EPOCH;
        let mut ended = crate::testkit::agent_state("claude", "stopping", now);
        ended.ended_at = Some(now);

        let failure = retire_ended_sessions_in(&paths, &runtime, dir.path(), || {
            std::borrow::Cow::Owned(vec![ended])
        })
        .expect_err("the withdrawal fails");

        assert_eq!(failure.dropped(), 1, "{failure}");
        assert!(super::super::instances::pinned_sessions(&paths.root).is_empty());
        assert_eq!(idle_stop_sessions(&paths), ["stopping", "resting"]);
    }

    /// A session holding only an idle-stop request, no instance row, is still
    /// reconciled once it durably ends.
    #[test]
    fn ended_session_reconcile_covers_a_request_only_session() {
        let (dir, paths, runtime) = idle_stop_fixture();
        let now = Timestamp::UNIX_EPOCH;
        let mut ended = crate::testkit::agent_state("claude", "stopping", now);
        ended.ended_at = Some(now);
        let agents = vec![ended, crate::testkit::agent_state("claude", "resting", now)];
        retire_ended_sessions_in(&paths, &runtime, dir.path(), || {
            std::borrow::Cow::Borrowed(&agents)
        })
        .expect("reconcile");
        assert_eq!(idle_stop_sessions(&paths), ["resting"]);
    }

    /// The retirement predicate: a pinned session is retired on positive
    /// evidence of its own durable end, and on nothing else.
    #[test]
    fn ended_pinned_sessions_selects_only_durably_ended_pinned_targets() {
        let now = Timestamp::UNIX_EPOCH;
        let session = crate::ids::AgentSessionId::from;
        let kind = crate::ids::AgentKind::new_unchecked;
        let agent = |agent_kind: &str, id: &str, ended: bool| {
            let mut agent = crate::testkit::agent_state(agent_kind, id, now);
            agent.ended_at = ended.then_some(now);
            agent
        };
        let pinned = ["ended", "revived", "subagent", "other-kind"]
            .map(|id| (kind("claude"), session(id)))
            .into();
        let mut subagent = agent("claude", "subagent", true);
        subagent.parent_agent_id = Some(session("parent"));
        subagent.launch_depth = None;
        let agents = vec![
            agent("claude", "ended", true),
            // A later lifecycle event for the same session clears `ended_at`.
            agent("claude", "revived", false),
            subagent,
            // The pin names `claude`; this end is another provider's.
            agent("codex", "other-kind", true),
            // Ended, but nothing is pinned to it.
            agent("claude", "unpinned", true),
        ];

        let selected = ended_pinned_sessions(&agents, &pinned);

        assert_eq!(
            selected
                .iter()
                .map(|(kind, session)| (kind.as_str(), session.as_str()))
                .collect::<Vec<_>>(),
            vec![("claude", "ended")]
        );
    }
}
