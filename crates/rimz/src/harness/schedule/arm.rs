//! Shared construction and lifecycle of session-pinned delivery tasks.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::time::Duration;

use jiff::Timestamp;

use super::ScheduleErr;
use super::catalog::{LoadedTask, TaskSource};
use crate::agents::AgentState;
use crate::config::{CheckOn, TaskEntry, TaskTarget, WaitMeta, WatchSpec};
use crate::harness::signal::{SignalSelector, default_signal_match_key};
use crate::ids::TeamInstanceId;
use crate::workspace::ResolvedWorkspace;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskName(String);

impl FromStr for TaskName {
    type Err = ScheduleErr;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        crate::harness::name::validate_name(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl std::fmt::Display for TaskName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub enum DeliveryName {
    /// A self wait's name, minted from its kind's prefix (`shell-`, `watch-`) and a petname.
    Mint,
    Named(TaskName),
}

pub enum SubscriptionLifetime {
    Once,
    Standing,
}

pub enum DeliveryTrigger {
    /// `check_in` is a self watch's one still-running notice, due that long after it arms.
    Condition {
        expr: crate::harness::when::WhenExpr,
        hold: Option<Duration>,
        lifetime: SubscriptionLifetime,
        check_in: Option<Duration>,
    },
    /// `on` is the row's delivery polarity, the shell's `--on`; `input` is written to the
    /// command's `context.json` before its watcher starts.
    Watch {
        spec: WatchSpec,
        on: CheckOn,
        timeout: Option<Duration>,
        input: Option<serde_json::Map<String, serde_json::Value>>,
    },
    Signal {
        selector: SignalSelector,
        matches: BTreeMap<String, String>,
        lifetime: SubscriptionLifetime,
    },
}

pub enum DeliveryPrompt {
    None,
    Inline(String),
}

pub enum DeliveryProvenance {
    /// `reader` is the arming agent's name (a launched child's petname), whose
    /// `out/` directory receives a watched command's output.
    SelfWait {
        reader: Option<String>,
    },
    Team(TeamInstanceId),
}

pub struct DeliverySpec {
    pub name: DeliveryName,
    pub label: Option<String>,
    pub target: TaskTarget,
    pub trigger: DeliveryTrigger,
    pub prompt: DeliveryPrompt,
    pub provenance: DeliveryProvenance,
    pub deadline: Option<Timestamp>,
    /// When a self wait ends unmet (the row's `expires`).
    pub expires: Option<Timestamp>,
    pub max_strikes: Option<u32>,
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
    mut spec: DeliverySpec,
) -> Result<ArmOutcome, ArmFailure> {
    let input = match &mut spec.trigger {
        DeliveryTrigger::Watch { input, .. } => input.take(),
        DeliveryTrigger::Condition { .. } | DeliveryTrigger::Signal { .. } => None,
    };
    let (name, task) = build_entry(workspace, spec, Timestamp::now())?;
    let entry = task.entry();
    let catalog = super::catalog::TaskCatalog::load(Some(&workspace.project_root))
        .map_err(|err| ArmFailure::State(err.into()))?;
    let name = match &name {
        DeliveryName::Mint => None,
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
    let finish = || -> Result<(), ArmFailure> {
        if entry.team.is_none() && entry.loop_task.is_none() {
            super::config_edit::remove_machine_at_root(&name, &entry.resolved_root())
                .map_err(|err| ArmFailure::State(err.into()))?;
        }
        if entry.watch.is_none() {
            return Ok(());
        }
        let spawn = || -> std::io::Result<()> {
            let path = super::signal::wait_output_path(&paths, &name, entry);
            for dir in [
                paths.out_dir.as_path(),
                path.parent().unwrap_or(&paths.out_dir),
            ] {
                crate::disk::paths::ensure_private_runtime_dir(dir)
                    .map_err(std::io::Error::other)?;
            }
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)?;
            super::signal::publish_wait_context(&paths, &name, entry, input.as_ref())?;
            // The output file holds only what the watch writes; the watcher's
            // own diagnostics go to the room's shared watcher log.
            let log = crate::disk::capped_log::open(&paths, &paths.wait_watch_log())?;
            let mut command = Command::new(crate::proc::rimz_exe());
            command
                .arg("--root")
                .arg(&workspace.project_root)
                .args(["shell", "watcher", &name])
                .current_dir(&workspace.project_root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::from(log))
                .process_group(0);
            // The watcher runs the agent's own command, which writes where the
            // agent was told its temp files live.
            crate::child_process::spawn_detached_reaped_as_agent(&mut command, "shell-watcher")?;
            Ok(())
        };
        spawn().map_err(ArmFailure::Watcher)
    };
    if let Err(error) = finish() {
        super::instances::remove(&paths, &name, Some(entry))
            .map_err(|err| ArmFailure::State(Box::new(err)))?;
        return Err(error);
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

/// Retire every instance row pinned to `(kind, session)`, paused and disabled
/// rows included, and clear each one's watcher, arming and strike overlays.
///
/// Returns how many of the removed rows nothing arms again: self waits and
/// other delivery rows. Team and resident-loop bindings, restored at the
/// member's next registration, stay out of that count.
/// Overlay cleanup is best-effort and warns instead of failing, so the count is
/// reported whatever the overlays do; `Err` means the durable map could not be
/// rewritten and nothing was removed, or the session's idle-stop request could
/// not be withdrawn, or its raised attention could not be retired after its rows
/// went, and carries the count either way
/// ([`RetireFailure::dropped`]).
pub fn retire_session(
    project_root: &Path,
    kind: &crate::ids::AgentKind,
    session: &crate::ids::AgentSessionId,
) -> Result<usize, RetireFailure> {
    let paths = crate::disk::paths::StatePaths::for_project_root(project_root)
        .map_err(RetireFailure::before_removal)?;
    let runtime = crate::disk::paths::RuntimePaths::for_project_root(project_root)
        .map_err(RetireFailure::before_removal)?;
    retire_session_in(
        &paths,
        &runtime,
        project_root,
        kind,
        session,
        Path::new("/sys/fs/cgroup"),
    )
}

/// Release what belonged to a session a swap replaced, leaving its agent's
/// waits armed: thaw its hibernation and withdraw its pane's idle stop, and,
/// when `conversation_over` (a fresh restart or molt), retire the
/// conversation's raised attention. A resumed restart continues the
/// conversation, so its raised attention stays.
pub fn release_replaced_session(
    project_root: &Path,
    kind: &crate::ids::AgentKind,
    session: &crate::ids::AgentSessionId,
    conversation_over: bool,
) -> Result<(), String> {
    let paths = crate::disk::paths::StatePaths::for_project_root(project_root)
        .map_err(|err| err.to_string())?;
    let runtime = crate::disk::paths::RuntimePaths::for_project_root(project_root)
        .map_err(|err| err.to_string())?;
    release_replaced_session_in(
        &paths,
        &runtime,
        kind,
        session,
        conversation_over,
        Path::new("/sys/fs/cgroup"),
    )
}

fn release_replaced_session_in(
    paths: &crate::disk::paths::StatePaths,
    runtime: &crate::disk::paths::RuntimePaths,
    kind: &crate::ids::AgentKind,
    session: &crate::ids::AgentSessionId,
    conversation_over: bool,
    cgroup_root: &Path,
) -> Result<(), String> {
    thaw_session(paths, kind, session, cgroup_root);
    release_session(paths, runtime, kind, session, conversation_over)
}

fn retire_session_in(
    paths: &crate::disk::paths::StatePaths,
    runtime: &crate::disk::paths::RuntimePaths,
    project_root: &Path,
    kind: &crate::ids::AgentKind,
    session: &crate::ids::AgentSessionId,
    cgroup_root: &Path,
) -> Result<usize, RetireFailure> {
    thaw_session(paths, kind, session, cgroup_root);
    let retired = super::instances::retire_session(paths, kind, session)
        .map_err(RetireFailure::before_removal)?;
    let mut dropped = 0;
    for (name, entry) in &retired {
        if entry.team.is_none() && entry.loop_task.is_none() {
            dropped += 1;
        }
        let binding = super::task_record::Binding::for_task(
            name,
            super::catalog::TaskSource::Instance,
            project_root,
        );
        if let Err(err) = super::signal::stop_watcher(runtime, name) {
            tracing::warn!(task = name, error = %err, "retire: stopping the watcher");
        }
        if let Err(err) = super::catalog::retire(&binding, false) {
            tracing::warn!(task = name, error = %format!("{err:#}"), "retire: retiring the task record");
        }
    }
    release_session(paths, runtime, kind, session, true)
        .map_err(|message| RetireFailure { message, dropped })?;
    Ok(dropped)
}

/// Thaw every hibernation record of one session; a failure only warns.
fn thaw_session(
    paths: &crate::disk::paths::StatePaths,
    kind: &crate::ids::AgentKind,
    session: &crate::ids::AgentSessionId,
    cgroup_root: &Path,
) {
    for entry in crate::store::hibernate::read(paths)
        .into_iter()
        .filter(|entry| &entry.kind == kind && &entry.agent_id == session)
    {
        if let Err(error) =
            crate::harness::hibernate::wake_pane_in(paths, &entry.pane_id, cgroup_root)
        {
            tracing::warn!(%error, "retire: waking hibernating session");
        }
    }
}

/// Withdraw what belongs to one session rather than to its agent: the idle
/// stop of the pane it would close, which a resumed restart must not inherit
/// either, and, when `retire_raised`, the conversation's raised attention.
fn release_session(
    paths: &crate::disk::paths::StatePaths,
    runtime: &crate::disk::paths::RuntimePaths,
    kind: &crate::ids::AgentKind,
    session: &crate::ids::AgentSessionId,
    retire_raised: bool,
) -> Result<(), String> {
    crate::store::idle_stop::withdraw(paths, kind, session).map_err(|err| err.to_string())?;
    if !retire_raised {
        return Ok(());
    }
    let Some(ask) =
        crate::store::raised::retire(paths, kind, session).map_err(|err| err.to_string())?
    else {
        return Ok(());
    };
    let record =
        crate::workspace::record::read(&paths.workspace_record).map_err(|err| err.to_string())?;
    let store = crate::Store::open_existing(paths.clone(), runtime.clone())
        .ok_or_else(|| "retiring raised attention requires a readable room record".to_owned())?;
    let mut params = serde_json::json!({"ask_id": ask.ask_id, "cause": "retired"});
    if let Some(caller) = crate::harness::ancestry::CallerIdentity::from_env() {
        let actor = crate::store::message::MessageSender::Agent {
            kind: caller.kind,
            agent_id: caller.launch_id,
            name: caller.name,
            profile: caller.profile,
            role: caller.role,
            channel: None,
        };
        params["actor"] = serde_json::json!(actor.render());
    }
    crate::store::raised::audit(
        &store,
        &record.session_name,
        "ask.closed",
        kind,
        session,
        params,
    )
    .map_err(|err| err.to_string())
}

/// Why a stored pid, check, or file wait is retired: this RimZ no longer runs one.
pub(super) const LEGACY_WAIT_RETIRED: &str = "retired: RimZ no longer watches a pid, a check, or \
     a file; run the wait as a command with `rimz shell`";

/// Retire each stored pid, check, or file wait with a canceled history row saying why, so the
/// rest of the room's rows load and fire; a failure only warns. Each row's record goes first,
/// under the row's task record (bound now for a row that never fired, and retired), so
/// `rimz loop logs` finds it by name and a stop between the two leaves the record.
fn retire_legacy_waits(
    paths: &crate::disk::paths::StatePaths,
    runtime: &crate::disk::paths::RuntimePaths,
    project_root: &Path,
) {
    for (name, entry) in super::instances::legacy_waits(paths) {
        let binding = super::task_record::Binding::for_task(
            &name,
            super::catalog::TaskSource::Instance,
            project_root,
        );
        let task_id = super::task_record_store::mutate(|records| {
            let (id, _) = records.bind(&binding);
            records.retire(&binding.key(), Timestamp::now());
            id
        })
        .inspect_err(|err| {
            tracing::warn!(task = name, error = %err, "retire: retiring the task record");
        })
        .ok();
        let mut record = super::run_log::LoopRunRecord::new(
            &name,
            super::run_log::LoopRunResult::Canceled,
            super::run_log::LoopRunMode::Scheduled,
            0,
        );
        record.task_id = task_id;
        record.error = Some(LEGACY_WAIT_RETIRED.to_owned());
        super::run_log::record_stopped(project_root, &record, Some(super::TaskKind::WakeUp));
        if let Err(err) = super::instances::remove(paths, &name, Some(&entry)) {
            tracing::warn!(task = name, error = %err, "retire: removing a legacy wait");
            continue;
        }
        if let Err(err) = super::signal::stop_watcher(runtime, &name) {
            tracing::warn!(task = name, error = %err, "retire: stopping the watcher");
        }
    }
}

/// Retire every instance row in this workspace pinned to a durably ended
/// session, whoever ended it.
///
/// This is the one place the retirement predicate is spelled: an agent row that
/// is not a provider subagent, matches the pinned `(kind, session)`, and carries
/// an `ended_at`. A session whose latest lifecycle event revived it has no
/// `ended_at` and stays; a target with no agent row at all is gc's business.
///
/// `agents` is read only once this workspace holds a pinned row, an idle-stop
/// request, raised attention, or a hibernation record.
pub fn retire_ended_sessions(
    project_root: &Path,
    agents: impl FnOnce() -> crate::store::runtime::AuditRows,
) -> Result<usize, RetireFailure> {
    let paths = crate::disk::paths::StatePaths::for_project_root(project_root)
        .map_err(RetireFailure::before_removal)?;
    let runtime = crate::disk::paths::RuntimePaths::for_project_root(project_root)
        .map_err(RetireFailure::before_removal)?;
    retire_ended_sessions_in(&paths, &runtime, project_root, agents)
}

fn retire_ended_sessions_in(
    paths: &crate::disk::paths::StatePaths,
    runtime: &crate::disk::paths::RuntimePaths,
    project_root: &Path,
    agents: impl FnOnce() -> crate::store::runtime::AuditRows,
) -> Result<usize, RetireFailure> {
    retire_legacy_waits(paths, runtime, project_root);
    let mut pinned = super::instances::pinned_sessions(&paths.root);
    let raised = crate::store::raised::read(paths);
    pinned.extend(
        raised
            .asks
            .into_iter()
            .map(|entry| (entry.kind, entry.agent_id)),
    );
    pinned.extend(
        raised
            .alerts
            .into_iter()
            .map(|entry| (entry.kind, entry.agent_id)),
    );
    pinned.extend(
        raised
            .human_turns
            .into_iter()
            .map(|entry| (entry.kind, entry.agent_id)),
    );
    pinned.extend(
        crate::store::idle_stop::read(paths)
            .into_iter()
            .map(|request| (request.kind, request.agent_id)),
    );
    pinned.extend(
        crate::store::hibernate::read(paths)
            .into_iter()
            .map(|entry| (entry.kind, entry.agent_id)),
    );
    if pinned.is_empty() {
        return Ok(0);
    }
    let mut dropped = 0;
    let mut failures = Vec::new();
    let agents = agents();
    for ((kind, session), agent_ended) in ended_pinned_sessions(agents.iter(), &pinned) {
        // A replaced session's end leaves its agent's waits armed; only what
        // belonged to the session itself goes.
        if !agent_ended {
            if let Err(message) = release_replaced_session_in(
                paths,
                runtime,
                kind,
                session,
                true,
                Path::new("/sys/fs/cgroup"),
            ) {
                failures.push(message);
            }
            continue;
        }
        match retire_session_in(
            paths,
            runtime,
            project_root,
            kind,
            session,
            Path::new("/sys/fs/cgroup"),
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

/// The retirement predicate: which of the `pinned` targets `agents` proves
/// durably ended, each with whether its agent ended too
/// ([`crate::agents::state::agent_end`]). A session another swap replaced can
/// end while its agent lives on in the holder; a launch id ends with its agent.
/// One pass indexes the launch rule, so the cost is rows plus pins, not their
/// product.
fn ended_pinned_sessions<'a, 'agents>(
    agents: impl IntoIterator<Item = &'agents AgentState>,
    pinned: &'a std::collections::BTreeSet<(crate::ids::AgentKind, crate::ids::AgentSessionId)>,
) -> Vec<(
    &'a (crate::ids::AgentKind, crate::ids::AgentSessionId),
    bool,
)> {
    let agents = agents.into_iter().collect::<Vec<_>>();
    let launch_rows = crate::harness::fleet::LaunchRows::new(agents.iter().copied());
    let ended = agents
        .iter()
        .filter(|agent| agent.ended_at.is_some() && !agent.is_provider_subagent())
        .map(|agent| (&agent.kind, &agent.agent_id))
        .collect::<std::collections::BTreeSet<_>>();
    pinned
        .iter()
        .filter_map(|key| {
            let members = launch_rows
                .members(&key.0, &key.1)
                .into_iter()
                .map(|index| agents[index])
                .collect::<Vec<_>>();
            // The rule over its own members selects them all again, so this
            // reads what it would over every row.
            let end = crate::agents::state::agent_end(members, &key.0, &key.1);
            let session_ended = ended.contains(&(&key.0, &key.1));
            // A launch pin goes with its agent, even while a session its
            // failed replacement replaced is still open.
            let agent_over = end == crate::agents::state::AgentEnd::Ended;
            (session_ended || agent_over)
                .then_some((key, end != crate::agents::state::AgentEnd::Live))
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryScopeFailure {
    #[error(
        "A ci.* or pr.* binding from the root checkout needs an explicit scope. Pass --match branch=<name>, --match path=<worktree-path>, or --match branch='*' for every checkout RimZ watches, or watch it with: rimz shell -- gh run watch --exit-status"
    )]
    RootCheckout,
    #[error("team.* waits need a team member; pass --match instance=<team#channel>")]
    NoTeam,
    #[error(
        "--wait on an agent.* signal requires --match handle=<other> or --match session=<other> to avoid waking the target from its own lifecycle signal"
    )]
    SelfSignal,
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
                .lane_path()
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
            matches.insert("instance".to_owned(), cohort.instance.to_string());
        }
        _ => {}
    }
    Ok(())
}

/// Refuse an `agent.*` subscription that its own target's lifecycle would
/// fire. `agent` is the target's row where the caller holds it: a session
/// match names the target when it names any session of that agent.
pub fn validate_self_signal(
    selector: &SignalSelector,
    matches: &BTreeMap<String, String>,
    target: &TaskTarget,
    agent: Option<&AgentState>,
) -> Result<(), DeliveryScopeFailure> {
    if selector.family() != "agent" {
        return Ok(());
    }
    fn handle_name(handle: &str) -> &str {
        handle.split_once('#').map_or(handle, |(name, _)| name)
    }
    let other = matches.get("handle").is_some_and(|handle| {
        !handle.is_empty()
            && !crate::harness::signal::is_match_wildcard(handle)
            && handle_name(handle) != handle_name(&target.handle)
    }) || matches.get("session").is_some_and(|session| {
        !session.is_empty()
            && !crate::harness::signal::is_match_wildcard(session)
            && session != &target.session
            && !agent.is_some_and(|agent| agent.answers_to(&session.as_str().into()))
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
    let one_shot = match &spec.trigger {
        DeliveryTrigger::Watch { .. } => true,
        DeliveryTrigger::Condition { hold, lifetime, .. } => {
            hold.is_none() && matches!(lifetime, SubscriptionLifetime::Once)
        }
        DeliveryTrigger::Signal { .. } => false,
    };
    if self_wait
        && (!one_shot || !matches!(spec.prompt, DeliveryPrompt::None) || spec.deadline.is_some())
    {
        return Err(ArmFailure::InvalidProvenance(
            "self waits require a once condition without a hold, or a watched command, without \
             a prompt",
        ));
    }
    if matches!(spec.provenance, DeliveryProvenance::Team(_))
        && !matches!(
            spec.trigger,
            DeliveryTrigger::Signal {
                lifetime: SubscriptionLifetime::Standing,
                ..
            }
        )
    {
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
        expires: spec.expires,
        max_strikes: spec.max_strikes,
        ..TaskEntry::default()
    };
    let reader = match spec.provenance {
        DeliveryProvenance::Team(instance) => {
            entry.team = Some(instance);
            None
        }
        DeliveryProvenance::SelfWait { reader } => reader,
    };
    match spec.prompt {
        DeliveryPrompt::None => {}
        DeliveryPrompt::Inline(prompt) => entry.prompt = Some(prompt),
    }
    match spec.trigger {
        DeliveryTrigger::Condition {
            expr,
            hold,
            lifetime,
            check_in,
        } => {
            if expr.window_spans().next().is_some() {
                entry.provider = entry.wait.as_ref().map(|target| target.kind.clone());
            }
            entry.when = Some(vec![expr.to_string()]);
            entry.hold = hold.map(duration_label);
            entry.repeat = matches!(lifetime, SubscriptionLifetime::Standing);
            entry.timeout = check_in.map(duration_label);
        }
        DeliveryTrigger::Watch {
            spec, on, timeout, ..
        } => {
            entry.watch = Some(spec);
            entry.on = Some(on);
            entry.timeout = timeout.map(duration_label);
        }
        // Only a team binding arms a signal, and its `team` makes the row standing.
        DeliveryTrigger::Signal {
            selector, matches, ..
        } => {
            validate_self_signal(
                &selector,
                &matches,
                entry.wait.as_ref().expect("delivery target was set above"),
                None,
            )?;
            entry.signal = Some(selector.to_string());
            entry.matches = (!matches.is_empty()).then_some(matches);
        }
    }
    if self_wait {
        entry.wait_meta = Some(WaitMeta {
            armed_at: now,
            delay: None,
            reader,
            joined_until: None,
        });
    }
    let name = match &spec.name {
        DeliveryName::Mint => super::pending::WaitKind::of(&entry).as_str(),
        DeliveryName::Named(name) => &name.0,
    };
    let task = LoadedTask::new(name, entry, TaskSource::Instance);
    task.trigger().as_ref().map_err(Clone::clone)?;
    Ok((spec.name, task))
}

/// A task's label as `--label` takes it: one line of 1 to 60 characters, trimmed.
pub fn task_label(value: &str) -> Result<String, &'static str> {
    let label = value.trim();
    if value.contains(['\n', '\r']) || !(1..=60).contains(&label.chars().count()) {
        return Err("label must be one line of 1 to 60 characters");
    }
    Ok(label.to_owned())
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
mod tests;
