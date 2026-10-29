//! Loop-fire policy from ordered gates through one durable history transition.
//!
//! [`TaskFire`] owns checks, deadlines, task consumption, run locks, prompt and
//! launch preparation, and terminal record mapping. CLI executes the prepared
//! supervised-run or message effect and returns its typed result.

mod agent_check;
mod prompt;
pub use prompt::signal_subject;
mod reminder;

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};

use super::super::assist_log::{self, Assist, AssistRecord};
use super::{
    LOOP_TASK_ENV,
    backlog::{self, StopCause, StopMark, WaitingTrigger},
    backlog_store::{self, LockedBacklog},
    fire::deadline_expired_at,
    throttle,
};
use crate::agents::PermissionMode;
use crate::agents::account::{AccountsCache, ProviderStatus, read_accounts_cache};
use crate::agents::login::LoginErr;
use crate::agents::{
    BirthLoginErr, HookPreflightErr, ManagedLaunchState, ProviderCapacity, ProviderLogin,
    RateLimitWindow, RoomLoginErr, TurnLifecycleNeed, WindowSpan, WindowSurplus, ambient_env,
    find_definition, preflight_hooks,
};
#[cfg(test)]
use crate::config::TaskCheck;
use crate::config::{
    CheckOn, MachineConfig, OverlapPolicy, TaskEntry, TaskTarget, ThrottleSwitch, WatchSpec,
};
use crate::disk::lease::{Lease, LeaseAttempt, LeaseState, claim, probe};
use crate::disk::paths::{RuntimePaths, StatePaths, logs_dir};
use crate::harness::plan::ResolvedSingleAgentLaunch;
use crate::harness::run::{RunCancellation, SupervisedRunOutcome, SupervisedRunRequest};
use crate::harness::schedule::catalog::{self, LoadedTask, TaskCatalog};
use crate::harness::schedule::run_log::{
    self, CheckRecord, JudgeRecord, LoopRunMode, LoopRunPresentation, LoopRunRecord, LoopRunResult,
    RunStage, RunTransition, SignalRecord,
};
use crate::harness::schedule::signal::{
    Signal as TriggerSignal, WAIT_TAIL_CAP, WatchOutcome, WatchVerdict,
};
use crate::harness::schedule::{TaskAction, TaskActionKind, TaskKind, Trigger};
use crate::ids::AgentKind;
use crate::ids::{MessageId, RunId, WorkspaceId};
use crate::store::run::RunRecord;
use crate::store::writer::LaunchLogin;
use crate::utils::time::{DurationUnit, parse_duration_units};
use crate::workspace::{ResolvedWorkspace, WorkspaceResolver};

pub const CHECK_DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
pub(super) const SCHEDULED_RUN_DEFAULT_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
pub const SCHEDULED_RUN_DEFAULT_TIMEOUT_LABEL: &str = "2h";
const CHECK_POLL_INTERVAL: Duration = Duration::from_millis(20);
const CHECK_DRAIN_GRACE: Duration = Duration::from_millis(200);
const CHECK_INTERRUPT_GRACE: Duration = Duration::from_secs(1);
const RUN_LOCK_RELEASE_POLL_INTERVAL: Duration = Duration::from_millis(200);
const HOLDER_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const CHECK_OUTPUT_CAP: usize = 16 * 1024;
const TASK_TIMEOUT_UNITS: &[DurationUnit] = &[
    DurationUnit::Second,
    DurationUnit::Minute,
    DurationUnit::Hour,
    DurationUnit::Day,
];

/// Check-owned launches must not capture launches made by an agent in its pane.
pub fn loop_check_task() -> Option<String> {
    check_task_from_identity(
        std::env::var(LOOP_TASK_ENV).ok(),
        std::env::var_os(crate::harness::launch::ENV_AGENT_ID).is_some(),
    )
}

fn check_task_from_identity(task: Option<String>, has_agent: bool) -> Option<String> {
    task.filter(|task| !has_agent && !task.is_empty())
}

/// Apply loop-owned deadline, cleanup, and scheduled-tab placement policy.
pub fn shape_loop_owned(
    request: &mut SupervisedRunRequest,
    task: &str,
    config: &MachineConfig,
    mode: LoopRunMode,
) -> Result<()> {
    request.timeout = effective_spawn_timeout(mode, request.timeout, configured_timeout(config)?);
    request.force_new_tab |= mode == LoopRunMode::Scheduled;
    request.loop_task = Some(task.to_owned());
    request.self_cleanup_on_completion = !request.keep;
    Ok(())
}

#[derive(Clone, Debug)]
pub enum TaskFireNotice {
    None,
    Gate { reason: String },
    Overlap { detail: Option<String> },
    TargetGone { handle: String },
}

#[derive(Clone, Debug)]
pub struct TaskFireFinished {
    pub record: LoopRunRecord,
    pub presentation: LoopRunPresentation,
    pub transition: RunTransition,
    pub notice: TaskFireNotice,
    pub replace_holder: Option<Box<RunLockInfo>>,
}

/// One fired guard check awaiting CLI presentation before its effect.
#[derive(Clone, Debug)]
pub struct CheckTrip {
    pub record: LoopRunRecord,
    pub watch: Option<WatchVerdict>,
    pub duration_ms: u64,
}

#[derive(Clone, Debug)]
pub struct PreparedSpawn {
    pub root: PathBuf,
    pub request: SupervisedRunRequest,
    pub stream: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryIntent {
    SelfWait,
    Wait,
    Signal,
}

#[derive(Clone, Debug)]
pub struct PreparedDelivery {
    pub root: PathBuf,
    pub target: TaskTarget,
    pub prompt: String,
    pub supersedes: Vec<MessageId>,
    pub intent: DeliveryIntent,
}

#[derive(Clone, Debug)]
pub enum TaskFirePlan {
    Done(TaskFireFinished),
    AlreadyLaunched,
    DeclineHeld,
    CapacityHeld,
    Resident {
        root: PathBuf,
        cwd: PathBuf,
        worktree: Option<String>,
        spec: String,
        prompt: String,
        /// The `### Loop` reminder body for the prompt leader.
        loop_reminder: String,
    },
    Spawn(PreparedSpawn),
    Deliver(PreparedDelivery),
}

#[derive(Debug)]
pub enum TaskFireEffect {
    Resident {
        leader: String,
        checkout: PathBuf,
        handles: Vec<String>,
        /// Handles of the checkout occupants a takeover stopped first.
        stopped: Vec<String>,
    },
    /// A takeover found an open turn in the checkout; nothing was stopped or
    /// launched.
    TakeoverBlocked {
        blockers: Vec<(String, super::takeover::OpenTurn)>,
    },
    Spawn(SupervisedRunOutcome),
    Delivered(crate::ids::MessageId),
    TargetGone,
}

#[derive(Clone, Debug)]
enum PendingEffect {
    Resident,
    Spawn { stream: bool },
    Deliver { target: TaskTarget },
}

enum FiredCheck {
    Shell {
        command: String,
        record: CheckRecord,
    },
    Judge {
        tier: usize,
        record: JudgeRecord,
    },
}

enum GuardStop {
    Interrupted,
    Declined,
    Gate(LoopRunResult, String),
    CheckOnly(LoopRunResult),
}

impl FiredCheck {
    fn heading(&self) -> String {
        match self {
            Self::Judge { record, .. } => {
                let status = record
                    .verdict
                    .map_or("no verdict", |pass| if pass { "pass" } else { "fail" });
                format!(
                    "judge `{}` ({} {}): {status}",
                    record.profile,
                    record.kind,
                    record.model.as_deref().unwrap_or("default")
                )
            }
            Self::Shell { command, record } => {
                let status = if record.timed_out {
                    "timeout".to_owned()
                } else {
                    record
                        .code
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "signal".to_owned())
                };
                format!("check `{command}` exited {status}")
            }
        }
    }

    fn output(&self) -> String {
        match self {
            Self::Shell { record, .. } => record.output.clone(),
            Self::Judge { record, .. } => std::iter::once(
                record
                    .reason
                    .as_deref()
                    .or(record.error.as_deref())
                    .unwrap_or_default(),
            )
            .chain(record.warnings.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        }
    }

    fn duration_ms(&self) -> u64 {
        match self {
            Self::Shell { record, .. } => record.duration_ms.unwrap_or_default(),
            Self::Judge { record, .. } => record.duration_ms.unwrap_or_default(),
        }
    }
}

struct FireContext {
    action: TaskAction,
    root: PathBuf,
    scope: Option<FireScope>,
}

struct FireScope {
    kind: crate::ids::AgentKind,
    scope_runtime: RuntimePaths,
    scope_state: StatePaths,
    resolved: Option<ResolvedSingleAgentLaunch>,
    managed_launch: ManagedLaunchState,
    /// The task's [`task_login`] for `kind`.
    login: Option<ProviderLogin>,
    /// Why the task's pinned account cannot run this fire.
    account_skip: Option<String>,
    capacity: OnceCell<Option<ProviderCapacity>>,
}

impl FireScope {
    fn new(
        kind: crate::ids::AgentKind,
        scope_runtime: RuntimePaths,
        scope_state: StatePaths,
        resolved: Option<ResolvedSingleAgentLaunch>,
        login: Option<ProviderLogin>,
    ) -> Self {
        Self {
            login,
            account_skip: None,
            kind,
            scope_runtime,
            scope_state,
            resolved,
            managed_launch: ManagedLaunchState::Unsupported,
            capacity: OnceCell::new(),
        }
    }

    fn login_key(&self) -> Option<crate::ids::LoginKey> {
        self.login.as_ref().map(ProviderLogin::key)
    }

    fn capacity(&self) -> Option<&ProviderCapacity> {
        self.capacity
            .get_or_init(|| {
                self.managed_launch
                    .capacity(&self.scope_runtime, &self.login_key()?)
            })
            .as_ref()
    }

    fn surplus_gate(&self, entry: &TaskEntry, now: Timestamp) -> Option<String> {
        if entry.surplus.is_none() && entry.surplus_after.is_none() {
            return None;
        }
        surplus_gate_in(
            entry,
            self.kind.as_str(),
            self.capacity()
                .and_then(|capacity| capacity.longest_window_surplus(now)),
        )
    }
}

impl FireContext {
    fn resolve(entry: &TaskEntry, action: TaskAction) -> Result<Self> {
        let root = entry.resolved_root();
        if matches!(action, TaskAction::CheckOnly | TaskAction::Workflow(_)) {
            return Ok(Self {
                action,
                root,
                scope: None,
            });
        }
        let launch = LaunchLogin::from(entry.account.clone());
        let scope = match &action {
            TaskAction::Spawn(spec) => {
                let workspace = WorkspaceResolver::resolve(&root, None)?;
                let runtime = RuntimePaths::for_project_root(&workspace.project_root)?;
                let resolved =
                    crate::harness::plan::resolve_single_agent_launch(spec, &workspace, &launch)?;
                let managed_launch = resolve_managed_spawn_state(entry, &workspace, &resolved)?;
                let kind = AgentKind::new_unchecked(resolved.kind.clone());
                let (login, account_skip) = match &entry.account {
                    None => (task_login(&launch, &resolved.kind, &runtime)?, None),
                    Some(name) => match pinned_login(
                        &kind,
                        name,
                        &MachineConfig::load_lenient().accounts,
                        &ambient_env(),
                        &read_accounts_cache(&runtime.shared_accounts_path()),
                    )? {
                        Ok(login) => (Some(login), None),
                        Err(reason) => (None, Some(reason)),
                    },
                };
                let mut scope = FireScope::new(
                    kind,
                    runtime,
                    StatePaths::for_project_root(&workspace.project_root)?,
                    Some(resolved),
                    login,
                );
                scope.account_skip = account_skip;
                scope.managed_launch = managed_launch;
                scope
            }
            TaskAction::Deliver(target) => {
                let project_root = WorkspaceResolver::persisted_project_root(&root)?;
                let runtime = RuntimePaths::for_project_root(&project_root)?;
                let login = task_login(&launch, target.kind.as_str(), &runtime)?;
                let mut scope = FireScope::new(
                    target.kind.clone(),
                    runtime,
                    StatePaths::for_project_root(&project_root)?,
                    None,
                    login,
                );
                scope.managed_launch =
                    unresolved_managed_state(&entry.resolved_root(), &target.kind);
                scope
            }
            TaskAction::CheckOnly | TaskAction::Workflow(_) => {
                unreachable!("action without provider scope returned above")
            }
        };
        Ok(Self {
            action,
            root,
            scope: Some(scope),
        })
    }
}

/// Discard waiting triggers and stop marks when a drain cannot compile its task.
pub fn discard_backlog(name: &str, root: &Path) -> Result<()> {
    super::backlog_store::remove(&StatePaths::for_project_root(root)?, name)?;
    Ok(())
}

/// One loop fire from ordered gates through one history transition, unless a drain finds no work.
pub struct TaskFire<'a> {
    name: String,
    task: LoadedTask,
    entry: TaskEntry,
    catalog: &'a TaskCatalog,
    action: Option<TaskAction>,
    ephemeral: bool,
    context: Option<FireContext>,
    mode: LoopRunMode,
    keep: bool,
    now: Timestamp,
    config: Arc<MachineConfig>,
    check_echo: Option<CheckEcho>,
    check_trip: Option<CheckTrip>,
    signal: Option<TriggerSignal>,
    arrived_at: Option<Timestamp>,
    from_arrived_at: Option<Timestamp>,
    condition: Option<super::when::ConditionEvidence>,
    checkout: Option<PathBuf>,
    started: Instant,
    run_lock: Option<RunLockGuard>,
    record_hold: Option<CheckSignalGuard>,
    backlog_paths: Option<StatePaths>,
    absorbed: u64,
    needs_room: bool,
    run_lock_path: fn(&str, &TaskEntry) -> Result<PathBuf>,
    throttle_host: Arc<dyn throttle::Host>,
    /// Ctrl-C heard from before the check on, for the hold to inherit.
    check_interrupts: Option<throttle::Interrupts>,
    throttle_turn: Option<throttle::Turn>,
    throttle_wait: Option<Duration>,
    resident_groups: Vec<throttle::StartGroup>,
    fired_guard: Vec<FiredCheck>,
    pending: Option<PendingEffect>,
    finished: bool,
    check_process: agent_check::ProcessRunner,
}

impl<'a> TaskFire<'a> {
    #[expect(
        clippy::too_many_arguments,
        reason = "one explicit loop-fire policy boundary"
    )]
    pub fn new(
        name: impl Into<String>,
        task: LoadedTask,
        catalog: &'a TaskCatalog,
        mode: LoopRunMode,
        keep: bool,
        now: Timestamp,
        config: Arc<MachineConfig>,
        signal: Option<TriggerSignal>,
        check_echo: CheckEcho,
        started: Instant,
    ) -> Result<Self> {
        let name = name.into();
        let action = task.action().cloned().map_err(Clone::clone)?;
        let entry = task.entry().clone();
        let ephemeral = task.is_ephemeral();
        Ok(Self {
            name,
            task,
            entry,
            catalog,
            action: Some(action),
            ephemeral,
            context: None,
            mode,
            keep,
            now,
            config,
            check_echo: Some(check_echo),
            check_trip: None,
            signal,
            arrived_at: None,
            from_arrived_at: None,
            condition: None,
            checkout: None,
            started,
            run_lock: None,
            record_hold: None,
            backlog_paths: None,
            absorbed: 0,
            needs_room: false,
            run_lock_path,
            throttle_host: Arc::new(throttle::SystemHost),
            resident_groups: Vec::new(),
            check_interrupts: None,
            throttle_turn: None,
            throttle_wait: None,
            fired_guard: Vec::new(),
            pending: None,
            finished: false,
            check_process: agent_check::execute,
        })
    }

    pub fn take_check_trip(&mut self) -> Option<CheckTrip> {
        self.check_trip.take()
    }

    /// Hand an action's SIGTERM guard through terminal recording and run-lock release.
    pub fn record_signal_guard(&mut self) -> &mut Option<CheckSignalGuard> {
        &mut self.record_hold
    }

    /// Claim one waiting trigger before the CLI rechecks arming and trust.
    pub fn take_backlog(&mut self) -> Result<bool> {
        let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
        let mut backlog = LockedBacklog::acquire(&paths)?;
        let Some(waiting) = backlog.record.tasks.get_mut(&self.name) else {
            return Ok(false);
        };
        if waiting.triggers.is_empty() {
            return Ok(false);
        }
        let path = self
            .fire_run_lock_path(true)?
            .context("waiting trigger needs a run lock")?;
        let RunLockAttempt::Acquired(guard) = acquire_run_lock_file(
            &path,
            Some(self.task.kind()),
            self.task.action().ok().map(TaskAction::kind),
        )?
        else {
            return Ok(false);
        };
        // The queue was checked under this same lock; its head cannot disappear.
        let trigger = waiting
            .triggers
            .pop_front()
            .expect("nonempty locked backlog");
        backlog.save()?;
        self.run_lock = Some(guard);
        self.backlog_paths = Some(paths.clone());
        self.serve_trigger(trigger);
        self.now = Timestamp::now();
        Ok(true)
    }

    /// Spawn the next drain before releasing a protocol-managed run lock.
    pub fn release_run_lock(&mut self) -> Result<()> {
        self.release_run_lock_with(|runtime, root, name| {
            super::fire::spawn_loop_drain(runtime, root, name, super::fire::LoopRunHost::Detached)
        })
    }

    fn release_run_lock_with(
        &mut self,
        spawn: impl FnOnce(&RuntimePaths, &Path, &str) -> std::io::Result<()>,
    ) -> Result<()> {
        if self.run_lock.is_none() {
            self.record_hold = None;
            return Ok(());
        }
        let Some(paths) = self.backlog_paths.as_ref() else {
            self.run_lock = None;
            self.record_hold = None;
            return Ok(());
        };
        let _guard = backlog_store::hold(paths)?;
        let result = (|| -> Result<()> {
            let backlog = backlog::load(paths)?;
            if backlog
                .tasks
                .get(&self.name)
                .is_some_and(|task| !task.triggers.is_empty())
            {
                spawn(
                    &RuntimePaths::for_state(paths)?,
                    &self.entry.resolved_root(),
                    &self.name,
                )?;
            }
            Ok(())
        })();
        // Even an unreadable backlog or a failed spawn releases under the protocol lock.
        self.run_lock = None;
        self.record_hold = None;
        result
    }

    fn waiting_trigger(&self) -> WaitingTrigger {
        WaitingTrigger {
            arrived_at: self.arrived_at.unwrap_or(self.now),
            signal: self.signal.as_ref().map(|signal| SignalRecord {
                name: signal.name.clone(),
                payload: signal.payload.clone(),
            }),
            absorbed: self.absorbed,
            from_arrived_at: self.from_arrived_at,
        }
    }

    fn serve_trigger(&mut self, trigger: WaitingTrigger) {
        self.absorbed = trigger.absorbed;
        self.arrived_at = Some(trigger.arrived_at);
        self.from_arrived_at = trigger.from_arrived_at;
        self.signal = trigger.signal.map(|signal| TriggerSignal {
            name: signal.name,
            payload: signal.payload,
            source: crate::store::event::SignalSource::Cli,
            watch: None,
        });
    }

    fn reconcile_backlog(&mut self) -> Result<Option<TaskFireFinished>> {
        let Some(holder) = self.run_lock.as_ref().map(|guard| guard.info) else {
            return Ok(None);
        };
        let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
        let mut backlog = match LockedBacklog::acquire(&paths) {
            Ok(backlog) => backlog,
            Err(error)
                if self.mode != LoopRunMode::Scheduled
                    || self.entry.overlap.unwrap_or_default() == OverlapPolicy::Skip =>
            {
                tracing::warn!(task = %self.name, %error, "failed to read loop stop mark; continuing without it");
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let Some(waiting) = backlog.record.tasks.get_mut(&self.name) else {
            return Ok(None);
        };
        if waiting
            .stopping
            .as_ref()
            .is_some_and(|mark| holder.matches_stop(mark))
        {
            drop(backlog);
            return Ok(Some(self.record_terminal_with(
                LoopRunResult::Canceled,
                LoopRunPresentation {
                    exit_code: LoopRunResult::Canceled.spawn_exit_code(),
                    ..LoopRunPresentation::default()
                },
                TaskFireNotice::None,
                None,
                |_| {},
            )));
        }
        if self.mode != LoopRunMode::Scheduled
            || !matches!(
                self.entry.overlap,
                Some(OverlapPolicy::Coalesce | OverlapPolicy::Replace)
            )
        {
            return Ok(None);
        }
        let Some(mut newest) = waiting.triggers.pop_back() else {
            return Ok(None);
        };
        for older in waiting.triggers.drain(..).rev() {
            newest.absorb(older);
        }
        newest.absorb(self.waiting_trigger());
        backlog.save()?;
        self.serve_trigger(newest);
        Ok(None)
    }

    pub fn with_arrived_at(mut self, arrived_at: Option<Timestamp>) -> Self {
        self.arrived_at = arrived_at;
        self
    }

    pub fn with_condition(mut self, condition: Option<super::when::ConditionEvidence>) -> Self {
        self.condition = condition;
        self
    }

    pub fn with_checkout(mut self, checkout: Option<PathBuf>) -> Self {
        self.checkout = checkout;
        self
    }

    /// Report the committed launch of a resident plan, so the throttle turn
    /// this fire holds passes on once the provider reports the agent.
    pub fn report_launch(&self, workspace: &WorkspaceId, agent: &crate::ids::AgentSessionId) {
        if let Some(turn) = &self.throttle_turn {
            turn.report_launch(workspace, agent);
        }
    }

    fn capacity_condition(&self) -> bool {
        self.mode == LoopRunMode::Scheduled
            && self.entry.agent.is_some()
            && self.entry.when.is_some()
            && self.entry.throttle != Some(ThrottleSwitch::Off)
    }

    fn capacity_now(&self) -> Result<Timestamp> {
        Ok(Timestamp::from_millisecond(i64::try_from(
            self.throttle_host.now_ms(),
        )?)?)
    }

    fn capacity_gives_up_at(&self, first: Timestamp) -> Result<Option<Timestamp>> {
        use crate::config::CapacityWait;
        let until = match self.entry.capacity_wait {
            Some(CapacityWait::Forever) => None,
            Some(CapacityWait::Duration(duration)) => first.checked_add(duration).ok(),
            None => match self
                .task
                .trigger()
                .as_ref()
                .ok()
                .map(|parsed| &parsed.trigger)
            {
                Some(Trigger::Schedule(schedule)) if !schedule.once => {
                    let runtime = RuntimePaths::for_project_root(&self.entry.resolved_root())?;
                    let key = self
                        .checkout
                        .as_ref()
                        .filter(|_| self.entry.each_worktree)
                        .map_or_else(
                            || self.name.clone(),
                            |checkout| super::fire::scope_key(&self.name, checkout),
                        );
                    let stamp = super::fire::last_stamps(&runtime)
                        .get(&key)
                        .copied()
                        .unwrap_or(self.now);
                    schedule
                        .schedule
                        .next_after(stamp, &self.now.to_zoned(self.config.time_zone()))
                        .and_then(|at| at.checked_sub(Duration::from_secs(1)).ok())
                }
                _ => None,
            },
        };
        Ok(match (until, self.entry.deadline) {
            (Some(until), Some(deadline)) => Some(until.min(deadline)),
            (until, deadline) => until.or(deadline),
        })
    }

    fn clear_capacity(&self) -> Result<()> {
        let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
        if let Err(error) = super::launch_ledger_store::remove_checkout_capacity(
            &paths,
            &self.name,
            &self.launch_checkout(),
        ) {
            tracing::warn!(
                task = %self.name, path = %super::launch_ledger::capacity_path(&paths).display(),
                %error, "failed to clear loop capacity wait"
            );
        }
        Ok(())
    }

    fn prepare_capacity_end(&mut self) -> Result<Option<TaskFirePlan>> {
        if !self.capacity_condition() {
            return Ok(None);
        }
        if self.entry.deadline.is_some()
            && let Some(done) = self.prepare_deadline_at(self.capacity_now()?)?
        {
            return Ok(Some(TaskFirePlan::Done(done)));
        }
        let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
        let waits = super::launch_ledger::load_capacity(&paths)?;
        let Some(wait) = waits
            .get(&self.name)
            .and_then(|checkouts| checkouts.get(&self.launch_checkout()))
            .filter(|wait| {
                wait.since
                    == self
                        .condition
                        .as_ref()
                        .and_then(|condition| condition.since)
            })
        else {
            return Ok(None);
        };
        if wait.gave_up {
            self.finished = true;
            return Ok(Some(TaskFirePlan::CapacityHeld));
        }
        let now = self.capacity_now()?;
        if wait.gives_up_at.is_some_and(|at| now >= at) {
            return self
                .record_capacity_hold(wait.reason.clone(), wait.kinds.clone())
                .map(Some);
        }
        Ok(None)
    }

    fn record_capacity_hold(
        &mut self,
        reason: String,
        kinds: Vec<AgentKind>,
    ) -> Result<TaskFirePlan> {
        let now = self.capacity_now()?;
        let since = self
            .condition
            .as_ref()
            .and_then(|condition| condition.since);
        let gives_up_at = self.capacity_gives_up_at(now)?;
        let wait = super::launch_ledger_store::record_capacity(
            &StatePaths::for_project_root(&self.entry.resolved_root())?,
            &self.name,
            &self.launch_checkout(),
            |previous| {
                let previous = previous.filter(|wait| wait.since == since);
                let first = previous.as_ref().map_or(now, |wait| wait.first);
                let gives_up_at = previous
                    .as_ref()
                    .map_or(gives_up_at, |wait| wait.gives_up_at);
                super::launch_ledger::CapacityRecord {
                    since,
                    first,
                    last: now,
                    count: previous.map_or(1, |wait| wait.count.saturating_add(1)),
                    kinds,
                    reason,
                    gives_up_at,
                    gave_up: gives_up_at.is_some_and(|at| now >= at),
                }
            },
        )?;
        if wait.gave_up {
            let waited = Duration::from_millis(
                u64::try_from(now.duration_since(wait.first).as_millis()).unwrap_or(0),
            );
            return Ok(TaskFirePlan::Done(self.record_gate(
                LoopRunResult::ThrottleSkipped,
                format!(
                    "gave up waiting for capacity after {}; {}",
                    crate::utils::time::format_duration_compact(waited),
                    wait.reason
                ),
            )));
        }
        if wait.count == 1 {
            self.record_terminal_with(
                LoopRunResult::CapacityHeld,
                LoopRunPresentation::default(),
                TaskFireNotice::None,
                Some(self.now),
                |record| record.error = Some(wait.reason),
            );
        } else {
            self.finished = true;
        }
        Ok(TaskFirePlan::CapacityHeld)
    }

    fn prepare_capacity_look(&mut self) -> Result<Option<TaskFirePlan>> {
        if let Some(plan) = self.prepare_capacity_end()? {
            return Ok(Some(plan));
        }
        if !self.capacity_condition()
            || (self.entry.guard().shell.is_none() && self.entry.guard().judges.is_empty())
        {
            return Ok(None);
        }
        let run = self.throttle_run()?;
        let held = throttle::look_in(
            self.throttle_host.as_ref(),
            &self.config.r#loop.throttle,
            &run,
        )?;
        if let Some(plan) = self.prepare_capacity_end()? {
            return Ok(Some(plan));
        }
        held.map(|hold| self.record_capacity_hold(hold.text, hold.kinds))
            .transpose()
    }

    fn throttle_run(&self) -> Result<throttle::Run> {
        let root = self.entry.resolved_root();
        let groups = match self
            .context
            .as_ref()
            .and_then(|context| context.scope.as_ref())
        {
            Some(scope) => vec![throttle::StartGroup {
                kind: scope.kind.clone(),
                account: scope.login.as_ref().map(|login| login.name().clone()),
                agents: 1,
            }],
            None => self.resident_groups.clone(),
        };
        Ok(throttle::Run {
            groups,
            gives_up_ms: self
                .capacity_gives_up_at(self.capacity_now()?)?
                .and_then(|at| u64::try_from(at.as_millisecond()).ok()),
            task: self.name.clone(),
            workspace: StatePaths::for_project_root(&root)?.workspace_id,
            root,
            checkout: self.launch_checkout(),
            disk: throttle::disk_at(
                &self.entry,
                &self.config.agents.worktree,
                self.launch_checkout(),
            ),
        })
    }

    /// Only a spawning fire reaches admission; condition fires never keep a ticket while held.
    fn prepare_throttle(&mut self) -> Result<Option<TaskFirePlan>> {
        if self.entry.deadline.is_some()
            && let Some(done) = self.prepare_deadline_at(self.capacity_now()?)?
        {
            return Ok(Some(TaskFirePlan::Done(done)));
        }
        if self.throttle_turn.is_some() {
            return Ok(None);
        }
        if self.mode == LoopRunMode::Manual || self.entry.throttle == Some(ThrottleSwitch::Off) {
            self.clear_capacity()?;
            return Ok(None);
        }
        if let Some(plan) = self.prepare_capacity_end()? {
            return Ok(Some(plan));
        }
        let run = self.throttle_run()?;
        if self.capacity_condition() {
            let attempt =
                throttle::try_admit_in(&self.throttle_host, &self.config.r#loop.throttle, &run)?;
            if let Some(plan) = self.prepare_capacity_end()? {
                return Ok(Some(plan));
            }
            match attempt {
                throttle::Attempt::Held(hold) => {
                    return self.record_capacity_hold(hold.text, hold.kinds).map(Some);
                }
                throttle::Attempt::Turn(turn) => self.throttle_turn = Some(turn),
                throttle::Attempt::Open => {}
            }
            self.clear_capacity()?;
            return Ok(None);
        }
        let listening = self.check_interrupts.take();
        let admission = throttle::admit(
            &self.throttle_host,
            &self.config.r#loop.throttle,
            &run,
            listening,
        )?;
        self.clear_capacity()?;
        match admission {
            throttle::Admission::Open => Ok(None),
            throttle::Admission::Turn { turn, waited } => {
                self.throttle_turn = Some(turn);
                self.throttle_wait = waited;
                Ok(None)
            }
            throttle::Admission::Skipped { reason } => {
                if let Some(done) = self.prepare_deadline_at(self.capacity_now()?)? {
                    return Ok(Some(TaskFirePlan::Done(done)));
                }
                Ok(Some(TaskFirePlan::Done(
                    self.record_gate(LoopRunResult::ThrottleSkipped, reason),
                )))
            }
            throttle::Admission::Interrupted => {
                Ok(Some(TaskFirePlan::Done(self.record_terminal_with(
                    LoopRunResult::Canceled,
                    LoopRunPresentation {
                        exit_code: Some(130),
                        ..LoopRunPresentation::default()
                    },
                    TaskFireNotice::None,
                    None,
                    |record| record.error = Some("interrupted while held".to_owned()),
                ))))
            }
        }
    }

    fn launch_checkout(&self) -> PathBuf {
        self.checkout
            .clone()
            .unwrap_or_else(|| self.entry.run_dir())
    }

    pub fn prepare(
        &mut self,
        before_check: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<TaskFirePlan> {
        if let Some(TaskAction::Workflow(name)) = self.action.as_ref() {
            return self.prepare_workflow(name.clone(), before_check);
        }
        if self.entry.stay {
            return self.prepare_resident(before_check);
        }
        if let Some(done) = self.prepare_scope_gates()? {
            return Ok(TaskFirePlan::Done(done));
        }
        let may_birth = self.context.as_ref().is_some_and(|context| {
            matches!(context.action, TaskAction::Spawn(_))
                || (self.mode == LoopRunMode::Scheduled && context.action.is_check_only())
        });
        if let Some(done) = self.prepare_run_lock(may_birth)? {
            return Ok(TaskFirePlan::Done(done));
        }

        if let Some(done) = self.prepare_deadline()? {
            return Ok(TaskFirePlan::Done(done));
        }

        if let Some(plan) = self.prepare_capacity_look()? {
            return Ok(plan);
        }

        let check = if self.run_lock.is_none()
            && self
                .context
                .as_ref()
                .is_some_and(|context| context.action.is_check_only())
        {
            before_check(&self.context_root()?)?;
            if let Some(done) = self.prepare_run_lock(false)? {
                return Ok(TaskFirePlan::Done(done));
            }
            self.prepare_check(&mut |_| Ok(()))?
        } else {
            self.prepare_check(before_check)?
        };
        if let ControlFlow::Break(done) = check {
            return Ok(TaskFirePlan::Done(done));
        }
        if self.run_lock.is_none() || self.needs_room {
            if let Some(done) = self.prepare_throttle()? {
                return Ok(done);
            }
            before_check(&self.context_root()?)?;
            self.needs_room = false;
            if let Some(done) = self.prepare_run_lock(false)? {
                return Ok(TaskFirePlan::Done(done));
            }
        }
        self.prepare_effect()
    }

    /// The effect of a one-shot fire whose gates passed. Only a spawn takes a
    /// turn in the start throttle, before its ephemeral row is consumed.
    fn prepare_effect(&mut self) -> Result<TaskFirePlan> {
        match self
            .context
            .as_ref()
            .context("loop task context missing after gates")?
            .action
            .clone()
        {
            TaskAction::Spawn(spec) => {
                if let Some(done) = self.prepare_throttle()? {
                    return Ok(done);
                }
                if let Some(done) = self.reconcile_backlog()? {
                    return Ok(TaskFirePlan::Done(done));
                }
                self.prepare_spawn(spec)
            }
            TaskAction::Deliver(target) => self.prepare_delivery(target),
            TaskAction::CheckOnly => {
                unreachable!("check-only action is completed by prepare_check")
            }
            TaskAction::Workflow(_) => {
                unreachable!("workflow action is completed by prepare_workflow")
            }
        }
    }

    fn prepare_workflow(
        &mut self,
        name: String,
        before_launch: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<TaskFirePlan> {
        super::validate_name(&name)?;
        if let Some(done) = self.prepare_run_lock(true)? {
            return Ok(TaskFirePlan::Done(done));
        }
        if self.run_lock.is_some() && self.workflow_already_launched()? {
            self.finished = true;
            return Ok(TaskFirePlan::AlreadyLaunched);
        }
        if let Some(done) = self.prepare_deadline()? {
            return Ok(TaskFirePlan::Done(done));
        }
        let root = self.entry.resolved_root();
        before_launch(&root)?;
        if self.run_lock.is_none() {
            if let Some(done) = self.prepare_run_lock(false)? {
                return Ok(TaskFirePlan::Done(done));
            }
            if self.workflow_already_launched()? {
                self.finished = true;
                return Ok(TaskFirePlan::AlreadyLaunched);
            }
        }
        let state = StatePaths::for_project_root(&root)?;
        let home = crate::disk::paths::workflows_dir().canonicalize()?;
        let dir = home.join(&name).canonicalize()?;
        anyhow::ensure!(
            dir.starts_with(&home),
            "workflow must exist inside {}",
            home.display()
        );
        let prompt = || {
            if self.entry.prompt.is_none() && self.entry.prompt_file.is_none() {
                return Ok(None);
            }
            self.resolve_effect_prompt()
                .map(Some)
                .map_err(|error| format!("{error:#}"))
        };
        let result = crate::harness::workflow::create(
            &state,
            &dir,
            crate::harness::workflow::ScopeRoots {
                checkout: self.launch_checkout(),
                project: root,
            },
            prompt,
            Some(self.name.clone()),
            self.mode == LoopRunMode::Manual,
        );
        let (result, id, error) = match result {
            Ok(run) => {
                assist_log::append(&AssistRecord {
                    at: Timestamp::now(),
                    assist: Assist::WorkflowStart {
                        run: run.id.clone(),
                        workflow: run.workflow,
                        task: run.task,
                        checkout: run.scope.path,
                        prompt: run.prompt.is_some(),
                    },
                });
                let error = self.record_workflow_launch(
                    &state,
                    &run.id,
                    run.scope.kind,
                    run.created_at,
                    self.mode == LoopRunMode::Manual,
                );
                (LoopRunResult::Launched, run.id, error)
            }
            Err(error) => match &error {
                crate::harness::workflow::WorkflowErr::Exists {
                    run, task, kind, ..
                } => {
                    let repair_error = if task.as_deref() == Some(self.name.as_str()) {
                        self.record_workflow_launch(&state, run, *kind, self.now, false)
                    } else {
                        None
                    };
                    let reason = match repair_error {
                        Some(repair) => format!("{error}; {repair}"),
                        None => error.to_string(),
                    };
                    (LoopRunResult::WorkflowSkipped, run.clone(), Some(reason))
                }
                _ => return Err(error.into()),
            },
        };
        self.consume_ephemeral()?;
        Ok(TaskFirePlan::Done(self.record_terminal_with(
            result,
            LoopRunPresentation::default(),
            TaskFireNotice::None,
            None,
            |record| {
                record.run_id = Some(id.to_string());
                record.error = error;
            },
        )))
    }

    fn workflow_already_launched(&self) -> Result<bool> {
        if self.mode != LoopRunMode::Scheduled || !self.entry.dedupes_checkout() {
            return Ok(false);
        }
        let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
        let workflows = super::launch_ledger::load_workflows(&paths)?;
        Ok(super::launch_ledger::launched(
            &self.entry,
            &self.name,
            &self.launch_checkout(),
            &Default::default(),
            &workflows,
        )
        .is_some())
    }

    fn record_workflow_launch(
        &self,
        paths: &StatePaths,
        run: &crate::ids::WorkflowRunId,
        scope: crate::store::workflow::ScopeKind,
        at: Timestamp,
        replace_project: bool,
    ) -> Option<String> {
        if !self.entry.dedupes_checkout() {
            return None;
        }
        let checkout = match scope {
            crate::store::workflow::ScopeKind::Project => self.entry.resolved_root(),
            crate::store::workflow::ScopeKind::Worktree => self.launch_checkout(),
        };
        let record = super::launch_ledger::WorkflowLaunchRecord {
            at,
            run: run.clone(),
            scope,
        };
        let Err(error) = super::launch_ledger_store::record_workflow(
            paths,
            &self.name,
            &checkout,
            record,
            replace_project,
        ) else {
            return None;
        };
        tracing::warn!(
            task = %self.name, %run, %error, "recording workflow loop launch failed"
        );
        Some(error.to_string())
    }

    fn prepare_resident(
        &mut self,
        before_launch: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<TaskFirePlan> {
        if let Some(done) = self.prepare_run_lock(true)? {
            return Ok(TaskFirePlan::Done(done));
        }
        let root = self.entry.resolved_root();
        let paths = StatePaths::for_project_root(&root)?;
        let cwd = self.launch_checkout();
        if self.mode == LoopRunMode::Scheduled
            && super::launch_ledger::load(&paths)?
                .get(&self.name)
                .is_some_and(|launches| launches.contains_key(&cwd))
        {
            self.clear_capacity()?;
            self.finished = true;
            return Ok(TaskFirePlan::AlreadyLaunched);
        }
        if self.mode == LoopRunMode::Scheduled
            && self.entry.guard().shell.is_none()
            && super::launch_ledger::holding_decline(
                &super::launch_ledger::load_declines(&paths)?,
                &self.name,
                &self.entry,
                &cwd,
                self.condition
                    .as_ref()
                    .and_then(|condition| condition.since),
                self.now,
            )
            .is_some()
        {
            self.clear_capacity()?;
            self.finished = true;
            return Ok(TaskFirePlan::DeclineHeld);
        }
        if let Some(done) = self.prepare_resident_scope_gates(&cwd)? {
            return Ok(TaskFirePlan::Done(done));
        }
        if let Some(done) = self.prepare_deadline()? {
            return Ok(TaskFirePlan::Done(done));
        }
        if let Some(plan) = self.prepare_capacity_look()? {
            return Ok(plan);
        }
        if let ControlFlow::Break(done) = self.prepare_check(before_launch)? {
            return Ok(TaskFirePlan::Done(done));
        }
        if let Some(done) = self.prepare_throttle()? {
            return Ok(done);
        }
        let TaskAction::Spawn(spec) = self
            .action
            .take()
            .context("resident task already prepared")?
        else {
            bail!("--stay requires an agent layout");
        };
        let prompt = self.resolve_effect_prompt()?;
        before_launch(&root)?;
        if self.run_lock.is_none() {
            if let Some(done) = self.prepare_run_lock(false)? {
                return Ok(TaskFirePlan::Done(done));
            }
            if self.mode == LoopRunMode::Scheduled
                && super::launch_ledger::load(&paths)?
                    .get(&self.name)
                    .is_some_and(|launches| launches.contains_key(&cwd))
            {
                let detail = "resident launch completed during room birth — skipped".to_owned();
                return Ok(TaskFirePlan::Done(self.record_terminal_with(
                    LoopRunResult::Overlapped,
                    LoopRunPresentation::default(),
                    TaskFireNotice::Overlap {
                        detail: Some(detail.clone()),
                    },
                    None,
                    |record| record.error = Some(detail),
                )));
            }
        }
        self.pending = Some(PendingEffect::Resident);
        Ok(TaskFirePlan::Resident {
            root,
            cwd,
            worktree: self.entry.worktree.clone(),
            spec,
            prompt,
            loop_reminder: self.loop_reminder(None),
        })
    }

    fn prepare_deadline(&mut self) -> Result<Option<TaskFireFinished>> {
        self.prepare_deadline_at(self.now)
    }

    fn prepare_deadline_at(&mut self, now: Timestamp) -> Result<Option<TaskFireFinished>> {
        if !deadline_expired_at(&self.entry, now) {
            return Ok(None);
        }
        if self.mode == LoopRunMode::Scheduled {
            self.remove_schedule()?;
        }
        Ok(Some(self.record_terminal_with(
            LoopRunResult::Expired,
            LoopRunPresentation::default(),
            TaskFireNotice::None,
            None,
            |_| {},
        )))
    }

    fn prepare_resident_scope_gates(&mut self, cwd: &Path) -> Result<Option<TaskFireFinished>> {
        let workspace = WorkspaceResolver::resolve(cwd, Some(self.entry.resolved_root()))?;
        let state = StatePaths::for_project_root(&workspace.project_root)?;
        let runtime = RuntimePaths::for_state(&state)?;
        let mut effective = crate::config::effective::load(&self.config, &workspace.project_root)?;
        let availability = crate::harness::plan::LaunchAvailability::read(
            &runtime,
            &state,
            &self.config,
            self.now,
        );
        effective.route(
            &self.config.tiers,
            crate::config::effective::ProfileScope::Agents,
            self.entry.agent.as_deref(),
            None,
            None,
            None,
            |kind, model| availability.unavailable(kind, model),
        )?;
        let layout = crate::harness::plan::resolve_launch(
            &effective,
            crate::config::effective::ProfileScope::Agents,
            &self.config.agents.commands,
            self.entry.agent.as_deref(),
            None,
        )?;
        let mut groups: Vec<throttle::StartGroup> = Vec::new();
        for cell in layout.layout.agent_cells() {
            let resolved = ResolvedSingleAgentLaunch {
                kind: cell.kind.as_str().to_owned(),
                args: cell.args.clone(),
                model: cell.launch.model.clone(),
            };
            let login = task_login(&LaunchLogin::RoomDefault, &cell.kind, &runtime)?;
            let account = login.as_ref().map(|login| login.name().clone());
            match groups
                .iter_mut()
                .find(|group| group.kind == cell.kind && group.account == account)
            {
                Some(group) => group.agents += 1,
                None => groups.push(throttle::StartGroup {
                    kind: cell.kind.clone(),
                    account,
                    agents: 1,
                }),
            }
            let mut scope = FireScope::new(
                cell.kind.clone(),
                runtime.clone(),
                state.clone(),
                None,
                login,
            );
            scope.managed_launch = resolve_managed_spawn_state(&self.entry, &workspace, &resolved)?;
            if let Some((result, reason)) = self.scope_refusal(&scope) {
                return Ok(Some(self.record_gate(result, reason)));
            }
        }
        self.resident_groups = groups;
        Ok(None)
    }

    fn prepare_scope_gates(&mut self) -> Result<Option<TaskFireFinished>> {
        let zoned_now = self.now.to_zoned(self.config.time_zone());
        if let Some(gate) =
            run_log::daily_budget_gate(&logs_dir(), &self.name, &self.entry, &zoned_now)
                .map_err(anyhow::Error::msg)?
        {
            return Ok(Some(
                self.record_gate(LoopRunResult::BudgetSkipped, gate.reason()),
            ));
        }
        let action = self
            .action
            .take()
            .context("loop task action already prepared")?;
        let context = FireContext::resolve(&self.entry, action)?;
        if let Some((result, reason)) = context
            .scope
            .as_ref()
            .and_then(|scope| self.scope_refusal(scope))
        {
            return Ok(Some(self.record_gate(result, reason)));
        }
        self.context = Some(context);
        Ok(None)
    }

    /// The first scope gate that refuses this fire, in ladder order; every
    /// one reads the task's login.
    fn scope_refusal(&self, scope: &FireScope) -> Option<(LoopRunResult, String)> {
        if let Some(reason) = scope.account_skip.clone() {
            return Some((LoopRunResult::AccountSkipped, reason));
        }
        let key = scope.login_key();
        if let Some(reason) = crate::harness::budget::scope_gate(
            &scope.scope_runtime,
            &scope.scope_state,
            key.as_ref(),
            &self.config,
            self.now,
        ) {
            return Some((LoopRunResult::BudgetSkipped, reason));
        }
        if let Some(binding) = scope.managed_launch.binding()
            && let Some(key) = &key
            && let Some(reason) =
                crate::agents::provider_budget_gate(&scope.scope_runtime, key, binding, self.now)
        {
            return Some((LoopRunResult::BudgetSkipped, reason));
        }
        scope
            .surplus_gate(&self.entry, self.now)
            .map(|reason| (LoopRunResult::SurplusSkipped, reason))
    }

    fn prepare_run_lock(&mut self, may_birth: bool) -> Result<Option<TaskFireFinished>> {
        if self.run_lock.is_some() {
            return Ok(None);
        }
        let Some(path) = self.fire_run_lock_path(may_birth)? else {
            return Ok(None);
        };
        let policy = self.entry.overlap.unwrap_or_default();
        if policy != OverlapPolicy::Skip {
            let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
            let mut backlog = LockedBacklog::acquire(&paths)?;
            match acquire_run_lock_file(
                &path,
                Some(self.task.kind()),
                self.task.action().ok().map(TaskAction::kind),
            )? {
                RunLockAttempt::Acquired(guard) => {
                    if self.mode == LoopRunMode::Scheduled {
                        let waiting = backlog.record.tasks.entry(self.name.clone()).or_default();
                        waiting.enqueue(policy, usize::MAX, self.waiting_trigger());
                        // The fresh arrival was just enqueued while the queue was locked.
                        let trigger = waiting.triggers.pop_front().expect("arrival enqueued");
                        backlog.save()?;
                        self.serve_trigger(trigger);
                    }
                    self.run_lock = Some(guard);
                    self.backlog_paths = Some(paths.clone());
                    return Ok(None);
                }
                RunLockAttempt::Held(info) if self.mode == LoopRunMode::Scheduled => {
                    let limit = self.entry.overlap_limit.unwrap_or(10);
                    let waiting = backlog.record.tasks.entry(self.name.clone()).or_default();
                    let enqueued = waiting.enqueue(policy, limit, self.waiting_trigger());
                    let count = waiting.triggers.len();
                    let mut replace_holder = None;
                    let stopping = if policy == OverlapPolicy::Replace
                        && let Some(info) = info
                        && !throttle::held(&self.name, &self.entry.resolved_root())
                            .iter()
                            .any(|held| held.pid == info.pid)
                    {
                        if !waiting
                            .stopping
                            .as_ref()
                            .is_some_and(|mark| info.matches_stop(mark))
                        {
                            waiting.stopping = Some(StopMark {
                                pid: info.pid,
                                started_at: info.started_at,
                                cause: StopCause::Replace,
                            });
                            replace_holder = Some(Box::new(info));
                        }
                        true
                    } else {
                        false
                    };
                    backlog.save()?;
                    let suffix = if !enqueued {
                        format!("queue full ({count}/{limit}), skipped")
                    } else if policy == OverlapPolicy::Queue {
                        format!("waiting (queue {count}/{limit})")
                    } else if stopping {
                        "stopping it for this trigger".to_owned()
                    } else if policy == OverlapPolicy::Replace && info.is_none() {
                        "waiting (holder identity unavailable)".to_owned()
                    } else {
                        "waiting".to_owned()
                    };
                    let holder = info.map_or_else(
                        || "previous run still active".to_owned(),
                        |info| {
                            format!(
                                "previous run still active (pid {}, started {})",
                                info.pid,
                                relative_age(info.started_at, Timestamp::now())
                            )
                        },
                    );
                    let detail = Some(format!("{holder} — {suffix}"));
                    let mut finished = self.record_terminal_with(
                        if enqueued {
                            LoopRunResult::Deferred
                        } else {
                            LoopRunResult::Overlapped
                        },
                        LoopRunPresentation::default(),
                        TaskFireNotice::Overlap {
                            detail: detail.clone(),
                        },
                        None,
                        |record| record.error = detail,
                    );
                    finished.replace_holder = replace_holder;
                    return Ok(Some(finished));
                }
                RunLockAttempt::Held(info) => return Ok(Some(self.record_overlap(info))),
            }
        }
        match acquire_run_lock_file(
            &path,
            Some(self.task.kind()),
            self.task.action().ok().map(TaskAction::kind),
        )? {
            RunLockAttempt::Acquired(guard) => {
                self.run_lock = Some(guard);
                Ok(None)
            }
            RunLockAttempt::Held(info) => Ok(Some(self.record_overlap(info))),
        }
    }

    fn fire_run_lock_path(&mut self, may_birth: bool) -> Result<Option<PathBuf>> {
        let checkout = self
            .entry
            .each_worktree
            .then(|| WorkspaceId::from_project_root(&self.launch_checkout()));
        let file = run_lock_file_name(&self.name, checkout.as_ref());
        let path = match (self.run_lock_path)(&file, &self.entry) {
            Ok(path) => path,
            Err(err)
                if may_birth
                    && matches!(
                        err.downcast_ref::<crate::workspace::WorkspaceErr>(),
                        Some(crate::workspace::WorkspaceErr::NoRoom { .. })
                    ) =>
            {
                let guard = self.entry.guard();
                if (self.entry.stay || self.entry.workflow.is_some())
                    && guard.shell.is_none()
                    && guard.judges.is_empty()
                {
                    return Ok(None);
                }
                self.needs_room = true;
                let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
                RuntimePaths::for_state(&paths)?.lock_path(&file)
            }
            Err(err) => return Err(err),
        };
        Ok(Some(path))
    }

    fn record_overlap(&mut self, info: Option<RunLockInfo>) -> TaskFireFinished {
        let detail = info.map(|info| {
            format!(
                "previous run still active (pid {}, started {}) — skipped",
                info.pid,
                relative_age(info.started_at, Timestamp::now())
            )
        });
        self.record_terminal_with(
            LoopRunResult::Overlapped,
            LoopRunPresentation::default(),
            TaskFireNotice::Overlap {
                detail: detail.clone(),
            },
            None,
            |record| record.error = detail.clone(),
        )
    }

    pub fn finish(&mut self, effect: TaskFireEffect) -> Result<TaskFireFinished> {
        let pending = self
            .pending
            .take()
            .context("loop task has no prepared effect to finish")?;
        match (pending, effect) {
            (
                PendingEffect::Resident,
                TaskFireEffect::Resident {
                    leader,
                    checkout,
                    handles,
                    stopped,
                },
            ) => {
                let cwd = self.launch_checkout();
                let destination = self.entry.worktree.as_ref().map(|_| checkout);
                let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
                let assist = AssistRecord {
                    at: Timestamp::now(),
                    assist: Assist::ResidentLaunch {
                        task: self.name.clone(),
                        checkout: destination.clone().unwrap_or_else(|| cwd.clone()),
                        condition: self.condition.clone(),
                        stopped,
                        handles,
                    },
                };
                super::launch_ledger_store::record(
                    &paths,
                    &self.name,
                    &cwd,
                    super::launch_ledger::LaunchRecord {
                        at: assist.at,
                        leader: leader.clone(),
                        destination,
                    },
                    || assist_log::try_append(&assist),
                )?;
                Ok(self.record_terminal_with(
                    LoopRunResult::Launched,
                    LoopRunPresentation::default(),
                    TaskFireNotice::None,
                    None,
                    |record| {
                        record.target = Some(format!("@{leader}"));
                    },
                ))
            }
            (PendingEffect::Resident, TaskFireEffect::TakeoverBlocked { blockers }) => Ok(self
                .record_gate(
                    LoopRunResult::TakeoverBlocked,
                    super::takeover::blocked_reason(&blockers),
                )),
            (PendingEffect::Spawn { stream }, TaskFireEffect::Spawn(outcome)) => {
                let mut record = self.terminal_record(LoopRunResult::Completed);
                let (presentation, notice) = finish_spawn_effect(&mut record, outcome, stream);
                Ok(self.finish_record(record, presentation, notice, None))
            }
            (PendingEffect::Deliver { target }, TaskFireEffect::Delivered(message_id)) => {
                self.consume_ephemeral()?;
                let handle = target.handle;
                Ok(self.record_terminal_with(
                    LoopRunResult::Delivered,
                    LoopRunPresentation::default(),
                    TaskFireNotice::None,
                    None,
                    |record| {
                        record.target = Some(handle);
                        record.message_id = Some(message_id);
                    },
                ))
            }
            (PendingEffect::Deliver { target }, TaskFireEffect::TargetGone) => {
                if self.mode == LoopRunMode::Scheduled {
                    self.remove_schedule()?;
                }
                let handle = target.handle;
                Ok(self.record_terminal_with(
                    LoopRunResult::TargetGone,
                    LoopRunPresentation::default(),
                    TaskFireNotice::TargetGone {
                        handle: handle.clone(),
                    },
                    None,
                    |record| {
                        record.target = Some(handle.clone());
                    },
                ))
            }
            _ => anyhow::bail!("loop task effect does not match its prepared plan"),
        }
    }

    pub fn finish_error(&mut self, err: &anyhow::Error) -> TaskFireFinished {
        if matches!(self.pending.take(), Some(PendingEffect::Deliver { .. }))
            && let Err(consume) = self.consume_ephemeral()
        {
            tracing::warn!(task = %self.name, error = %format!("{consume:#}"), "failed to consume errored one-shot delivery");
        }
        // The launch resolves the pin again for the declared provider.
        let unresolved =
            self.entry
                .account
                .as_ref()
                .and_then(|name| match err.downcast_ref::<RoomLoginErr>() {
                    Some(RoomLoginErr::Login(login)) => Some(unresolved_pin(login, name)),
                    _ => None,
                });
        if let Some(reason) = unresolved {
            return self.record_gate(LoopRunResult::AccountSkipped, reason);
        }
        let error = format!("{err:#}");
        self.record_terminal_with(
            LoopRunResult::Errored,
            LoopRunPresentation::default(),
            TaskFireNotice::None,
            None,
            |record| record.error = Some(error),
        )
    }

    fn prepare_check(
        &mut self,
        before_check: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<ControlFlow<TaskFireFinished, ()>> {
        let stop = self.run_guard(before_check)?;
        let duration_ms = self.fired_guard.iter().map(FiredCheck::duration_ms).sum();
        if let ControlFlow::Break(stop) = stop {
            let (result, interrupted, consume) = match stop {
                GuardStop::Gate(result, reason) => {
                    return Ok(ControlFlow::Break(self.record_gate(result, reason)));
                }
                GuardStop::Interrupted => (LoopRunResult::Canceled, true, false),
                GuardStop::Declined => (
                    LoopRunResult::CheckSkipped,
                    false,
                    self.watch_spec().is_some(),
                ),
                GuardStop::CheckOnly(result) => (result, false, self.ephemeral),
            };
            if result == LoopRunResult::CheckSkipped
                && let Some(FiredCheck::Judge { tier, record }) = self.fired_guard.last()
                && !record.reused
                && record.verdict.is_some()
            {
                let checkout = self.launch_checkout();
                let assist = AssistRecord {
                    at: Timestamp::now(),
                    assist: Assist::JudgeDecline {
                        task: self.name.clone(),
                        checkout: checkout.clone(),
                        profile: record.profile.clone(),
                        kind: record.kind.clone(),
                        reason: record.reason.clone().unwrap_or_default(),
                        cost_usd: record.cost_usd,
                    },
                };
                if self.entry.stay {
                    super::launch_ledger_store::record_decline(
                        &StatePaths::for_project_root(&self.entry.resolved_root())?,
                        &self.name,
                        &checkout,
                        super::launch_ledger::DeclineRecord {
                            tier: *tier,
                            at: assist.at,
                            since: self
                                .condition
                                .as_ref()
                                .and_then(|condition| condition.since),
                            reason: record.reason.clone().unwrap_or_default(),
                            profile: record.profile.clone(),
                            kind: Some(record.kind.clone()),
                            model: record.model.clone(),
                            fingerprint: super::launch_ledger::check_fingerprint(&self.entry),
                        },
                        || assist_log::try_append(&assist),
                    )?;
                } else {
                    assist_log::try_append(&assist)?;
                }
            }
            if self.mode == LoopRunMode::Scheduled && consume {
                self.remove_schedule()?;
            }
            return Ok(ControlFlow::Break(self.record_terminal_with(
                result,
                LoopRunPresentation {
                    check_duration_ms: Some(duration_ms),
                    exit_code: interrupted.then_some(130),
                    ..LoopRunPresentation::default()
                },
                TaskFireNotice::None,
                None,
                |run| run.error = interrupted.then(|| "check interrupted".to_owned()),
            )));
        }
        if self.mode == LoopRunMode::Manual
            && self.entry.stay
            && self.fired_guard.iter().any(|stage| matches!(stage, FiredCheck::Judge { record, .. } if record.verdict.is_some()))
        {
            super::launch_ledger_store::remove_checkout_decline(
                &StatePaths::for_project_root(&self.entry.resolved_root())?,
                &self.name,
                &self.launch_checkout(),
            )?;
        }
        let supplied_watch = self
            .watch_spec()
            .and(self.watch_outcome())
            .map(|watch| watch.verdict.clone());
        if self.mode == LoopRunMode::Manual
            && !self.fired_guard.is_empty()
            && supplied_watch
                .as_ref()
                .is_none_or(WatchVerdict::is_terminal)
        {
            self.check_trip = Some(CheckTrip {
                record: self.terminal_record(LoopRunResult::Completed),
                watch: supplied_watch,
                duration_ms,
            });
        }
        Ok(ControlFlow::Continue(()))
    }

    fn run_guard(
        &mut self,
        before_check: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<ControlFlow<GuardStop, ()>> {
        if let Some((command, watch)) = self
            .watch_spec()
            .map(WatchSpec::describe)
            .zip(self.watch_outcome())
        {
            let outcome = watch.to_check_outcome();
            let terminal = watch.verdict.is_terminal();
            let mut record = check_record(&outcome);
            record.duration_ms = Some(watch.verdict.elapsed_ms());
            record.output_path.clone_from(&watch.output_path);
            self.fired_guard.push(FiredCheck::Shell { command, record });
            if !terminal {
                return Ok(ControlFlow::Continue(()));
            }
            if outcome.interrupted {
                return Ok(ControlFlow::Break(GuardStop::Interrupted));
            }
            if !polarity_fires(self.entry.on, &outcome) {
                return Ok(ControlFlow::Break(GuardStop::Declined));
            }
            return Ok(ControlFlow::Continue(()));
        }
        let guard = self.entry.guard();
        let shell = guard.shell.map(str::to_owned);
        let shell_timeout = guard
            .shell_timeout
            .map(parse_task_timeout)
            .transpose()
            .map_err(anyhow::Error::msg)?
            .unwrap_or(CHECK_DEFAULT_TIMEOUT);
        let judges: Vec<_> = guard.judges.into_iter().cloned().collect();
        if shell.is_none() && judges.is_empty() {
            return Ok(ControlFlow::Continue(()));
        }
        let root = if self.entry.stay {
            self.entry.resolved_root()
        } else {
            self.context_root()?
        };
        if !self.entry.stay
            && ((self.run_lock.is_some() && !self.needs_room)
                || !matches!(
                    self.context.as_ref().map(|context| &context.action),
                    Some(TaskAction::Spawn(_))
                ))
        {
            before_check(&root)?;
        }
        // The shell check leaves SIGINT ignored once it returns, so listen first
        // and let a hold this fire reaches inherit the listener.
        self.check_interrupts = Some(
            self.throttle_host
                .interrupts()
                .map_err(throttle::ThrottleError::Interrupts)?,
        );
        let dir = self.launch_checkout();
        if let Some(command) = shell {
            if self.check_interrupted() {
                return Ok(ControlFlow::Break(GuardStop::Interrupted));
            }
            let mut env = crate::workspace::pin_env(&WorkspaceId::from_project_root(&root), &root);
            env.insert(LOOP_TASK_ENV.to_owned(), self.name.clone());
            env.insert(
                crate::workspace::ENV_WORKTREE_PATH.to_owned(),
                dir.to_string_lossy().into_owned(),
            );
            let started = Instant::now();
            let mut outcome = run_command(
                &dir,
                &command,
                WatchDeadline::KillAfter(shell_timeout),
                self.check_echo.take().unwrap_or(CheckEcho::Capture),
                &env,
                |_, _| {},
                &mut self.record_hold,
            )?;
            outcome.passed &= !outcome.timed_out;
            let mut record = check_record(&outcome);
            record.duration_ms = Some(elapsed_millis(started));
            self.fired_guard.push(FiredCheck::Shell { command, record });
            if outcome.interrupted || self.check_interrupted() {
                return Ok(ControlFlow::Break(GuardStop::Interrupted));
            }
            if self
                .context
                .as_ref()
                .is_some_and(|context| context.action.is_check_only())
            {
                return Ok(ControlFlow::Break(GuardStop::CheckOnly(check_only_result(
                    &outcome,
                ))));
            }
            if !polarity_fires(self.entry.on, &outcome) {
                if self.entry.stay {
                    super::launch_ledger_store::remove_checkout_decline(
                        &StatePaths::for_project_root(&self.entry.resolved_root())?,
                        &self.name,
                        &self.launch_checkout(),
                    )?;
                }
                return Ok(ControlFlow::Break(GuardStop::Declined));
            }
            self.record_hold = None;
        }
        if self.mode == LoopRunMode::Scheduled && self.entry.stay && !judges.is_empty() {
            let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
            let declines = super::launch_ledger::load_declines(&paths)?;
            if let Some(decline) = super::launch_ledger::holding_decline(
                &declines,
                &self.name,
                &self.entry,
                &dir,
                self.condition
                    .as_ref()
                    .and_then(|condition| condition.since),
                Timestamp::now(),
            ) && let Some(kind) = &decline.kind
            {
                if self.check_interrupted() {
                    return Ok(ControlFlow::Break(GuardStop::Interrupted));
                }
                let record = agent_check::replay(self, decline, kind);
                self.fired_guard.push(FiredCheck::Judge {
                    tier: decline.tier,
                    record,
                });
                return Ok(ControlFlow::Break(GuardStop::Declined));
            }
        }
        for (tier, judge) in judges.iter().enumerate() {
            if self.check_interrupted() {
                return Ok(ControlFlow::Break(GuardStop::Interrupted));
            }
            let started = Instant::now();
            let (outcome, mut record) = match agent_check::run(self, judge, &dir, &root)? {
                ControlFlow::Continue(stage) => stage,
                ControlFlow::Break((result, reason)) => {
                    return Ok(ControlFlow::Break(GuardStop::Gate(result, reason)));
                }
            };
            record.duration_ms = Some(elapsed_millis(started));
            let has_verdict = record.verdict.is_some();
            self.fired_guard.push(FiredCheck::Judge { tier, record });
            if outcome.interrupted || self.check_interrupted() {
                return Ok(ControlFlow::Break(GuardStop::Interrupted));
            }
            if has_verdict && !polarity_fires(self.entry.on, &outcome) {
                return Ok(ControlFlow::Break(GuardStop::Declined));
            }
            self.record_hold = None;
        }
        Ok(ControlFlow::Continue(()))
    }

    fn check_interrupted(&self) -> bool {
        self.check_interrupts
            .as_ref()
            .is_some_and(throttle::Interrupts::raised)
    }

    fn prepare_spawn(&mut self, spec: String) -> Result<TaskFirePlan> {
        let managed_launch = {
            let scope = self
                .context
                .as_ref()
                .and_then(|context| context.scope.as_ref())
                .context("loop spawn context missing provider scope")?;
            let resolved = scope
                .resolved
                .as_ref()
                .context("loop spawn context missing resolved task spec")?;
            preflight_kind(&resolved.kind, scope.login.as_ref())?;
            scope.managed_launch.clone()
        };
        let prompt = self.resolve_effect_prompt()?;
        let mut request = self.compile_spawn_request(spec, prompt, managed_launch)?;
        request.throttle_turn.clone_from(&self.throttle_turn);
        self.consume_ephemeral()?;
        let stream = self.mode == LoopRunMode::Manual;
        self.pending = Some(PendingEffect::Spawn { stream });
        Ok(TaskFirePlan::Spawn(PreparedSpawn {
            root: self.context_root()?,
            request,
            stream,
        }))
    }

    fn compile_spawn_request(
        &self,
        spec: String,
        prompt: String,
        managed_launch: ManagedLaunchState,
    ) -> Result<SupervisedRunRequest> {
        let system_prompt_file = self
            .entry
            .system_prompt_file
            .as_deref()
            .map(resolve_config_path)
            .transpose()?;
        let permission_mode = self
            .entry
            .mode
            .as_deref()
            .filter(|mode| !mode.trim().is_empty())
            .map(parse_mode_value)
            .transpose()?;
        let budget = self
            .entry
            .budget
            .as_deref()
            .map(str::parse::<crate::harness::budget::BudgetSpec>)
            .transpose()?;
        let mut request = SupervisedRunRequest::new(spec, prompt, permission_mode, managed_launch);
        request.login = LaunchLogin::from(self.entry.account.clone());
        request.worktree.clone_from(&self.entry.worktree);
        request.system_prompt_file = system_prompt_file;
        request.effort.clone_from(&self.entry.effort);
        request.fast = self.entry.fast;
        request.budget = budget;
        request.timeout = task_timeout(&self.entry)?;
        request.keep = self.keep;
        request.verify.clone_from(&self.entry.verify);
        request.max_attempts = self.entry.max_attempts;
        shape_loop_owned(&mut request, &self.name, &self.config, self.mode)?;
        request.agent_shell_persistence = match self.mode {
            LoopRunMode::Scheduled => crate::harness::launch::AgentShellPersistence::PerLaunch,
            LoopRunMode::Manual => crate::harness::launch::AgentShellPersistence::Room,
        };
        request.loop_reminder = Some(self.loop_reminder(request.timeout));
        Ok(request)
    }

    fn loop_reminder(&self, timeout: Option<Duration>) -> String {
        reminder::compose(&reminder::LoopFire {
            name: &self.name,
            task: &self.task,
            mode: self.mode,
            keep: self.keep,
            timeout,
        })
    }

    fn prepare_delivery(&mut self, target: TaskTarget) -> Result<TaskFirePlan> {
        if !catalog::delivery_target_alive(&self.entry, &target)? {
            if self.mode == LoopRunMode::Scheduled {
                self.remove_schedule()?;
            }
            let handle = target.handle;
            return Ok(TaskFirePlan::Done(self.record_terminal_with(
                LoopRunResult::TargetGone,
                LoopRunPresentation::default(),
                TaskFireNotice::TargetGone {
                    handle: handle.clone(),
                },
                None,
                |record| {
                    record.target = Some(handle);
                },
            )));
        }
        let prompt = self.resolve_effect_prompt()?;
        // The one-shot row is consumed in `finish`, once the wake's message
        // record exists: turn-completion waits read the row, then the queue,
        // so the agent never looks rested between the two.
        self.pending = Some(PendingEffect::Deliver {
            target: target.clone(),
        });
        Ok(TaskFirePlan::Deliver(PreparedDelivery {
            root: self.context_root()?,
            target,
            prompt,
            supersedes: self.superseded_check_ins(),
            intent: if self
                .task
                .trigger()
                .as_ref()
                .is_ok_and(|parsed| matches!(parsed.trigger, Trigger::Signal { .. }))
            {
                DeliveryIntent::Signal
            } else if self.entry.wait_meta.is_some() {
                DeliveryIntent::SelfWait
            } else {
                DeliveryIntent::Wait
            },
        }))
    }

    fn remove_schedule(&self) -> Result<()> {
        self.catalog.consume_scheduled(&self.name)?;
        Ok(())
    }

    fn consume_ephemeral(&self) -> Result<()> {
        if self.watch_spec().is_some()
            && self
                .watch_outcome()
                .is_some_and(|watch| !watch.verdict.is_terminal())
        {
            return Ok(());
        }
        if self.mode == LoopRunMode::Scheduled && self.ephemeral {
            self.remove_schedule()?;
        }
        Ok(())
    }

    fn watch_spec(&self) -> Option<&WatchSpec> {
        match &self.task.trigger().as_ref().ok()?.trigger {
            Trigger::Watch(spec) => Some(spec),
            Trigger::Schedule(_) | Trigger::Signal { .. } | Trigger::Condition { .. } => None,
        }
    }

    fn watch_outcome(&self) -> Option<&WatchOutcome> {
        self.signal
            .as_ref()
            .and_then(|signal| signal.watch.as_ref())
    }

    /// Message ids of this arming's earlier check-ins, which a watch's
    /// terminal fire makes stale; empty for every other delivery.
    fn superseded_check_ins(&self) -> Vec<MessageId> {
        let terminal = self
            .watch_outcome()
            .is_some_and(|outcome| outcome.verdict.is_terminal());
        if self.watch_spec().is_none() || !terminal {
            return Vec::new();
        }
        let armed_at = self.entry.wait_meta.as_ref().map(|meta| meta.armed_at);
        run_log::task_records(&logs_dir(), &self.name, Some(&self.entry.resolved_root()))
            .into_iter()
            .filter(|record| {
                record.result == LoopRunResult::Delivered
                    && record
                        .watch
                        .as_ref()
                        .is_some_and(|watch| !watch.is_terminal())
                    && armed_at.is_none_or(|armed_at| record.at >= armed_at)
            })
            .filter_map(|record| record.message_id)
            .collect()
    }

    fn context_root(&self) -> Result<PathBuf> {
        self.context
            .as_ref()
            .map(|context| context.root.clone())
            .context("loop task context missing after gates")
    }

    fn resolve_effect_prompt(&self) -> Result<String> {
        let mut body = resolve_task_prompt(&self.name, &self.entry)?;
        let signal_trigger =
            self.entry.signal.is_some() || self.entry.watch.is_some() || self.entry.when.is_some();
        let evidence = if let Some(condition) = &self.condition {
            prompt::Evidence::Condition(condition)
        } else if let Some(signal) = &self.signal {
            prompt::Evidence::Signal(signal)
        } else if signal_trigger {
            prompt::Evidence::Manual
        } else {
            prompt::Evidence::Scheduled
        };
        if self.entry.agent.is_some() && self.entry.watch.is_none() {
            body = prompt::compose_launch(&self.name, &evidence, &body);
        } else if self.entry.wait.is_some()
            || signal_trigger
            || self.signal.is_some()
            || self.condition.is_some()
        {
            body = prompt::compose_wait(
                &self.name,
                &self.entry,
                self.entry.wait_meta.as_ref(),
                evidence,
                &body,
                self.now,
            );
        }
        if self.entry.watch.is_none() {
            body = self.augment_guard_prompt(body);
        }
        Ok(body)
    }

    fn augment_guard_prompt(&self, mut body: String) -> String {
        for stage in &self.fired_guard {
            body = augment_prompt(body, &stage.heading(), &stage.output());
        }
        body
    }

    fn terminal_record(&self, result: LoopRunResult) -> LoopRunRecord {
        let mut record =
            LoopRunRecord::new(&self.name, result, self.mode, elapsed_millis(self.started));
        record.kind = Some(self.task.kind());
        record.watch = self.watch_outcome().map(|watch| watch.verdict.clone());
        record.signal = self.signal.as_ref().map(|signal| SignalRecord {
            name: signal.name.clone(),
            payload: signal.payload.clone(),
        });
        record.condition = self.condition.clone();
        record.absorbed = self.absorbed;
        for stage in &self.fired_guard {
            match stage {
                FiredCheck::Shell { record: shell, .. } => record.check = Some(shell.clone()),
                FiredCheck::Judge { record: judge, .. } => record.judge.push(judge.clone()),
            }
        }
        record.throttle_wait_ms = self
            .throttle_wait
            .map(|waited| u64::try_from(waited.as_millis()).unwrap_or(u64::MAX));
        if self.entry.stay || self.entry.each_worktree {
            record.checkout = Some(self.launch_checkout());
        }
        add_check_usage(&mut record);
        record
    }

    fn record_gate(&mut self, result: LoopRunResult, reason: String) -> TaskFireFinished {
        self.record_terminal_with(
            result,
            LoopRunPresentation::default(),
            TaskFireNotice::Gate {
                reason: reason.clone(),
            },
            Some(self.now),
            |record| record.error = Some(reason),
        )
    }

    fn record_terminal_with(
        &mut self,
        result: LoopRunResult,
        presentation: LoopRunPresentation,
        notice: TaskFireNotice,
        at: Option<Timestamp>,
        update: impl FnOnce(&mut LoopRunRecord),
    ) -> TaskFireFinished {
        let mut record = self.terminal_record(result);
        update(&mut record);
        self.finish_record(record, presentation, notice, at)
    }

    fn finish_record(
        &mut self,
        mut record: LoopRunRecord,
        mut presentation: LoopRunPresentation,
        notice: TaskFireNotice,
        at: Option<Timestamp>,
    ) -> TaskFireFinished {
        // Construction and effect completion are linear; reaching this twice
        // is an internal state-machine violation, not a recoverable input.
        assert!(!self.finished, "loop task history transition written once");
        if self.record_hold.is_none() {
            match CheckSignalGuard::register(|| Ok(())) {
                Ok(((), guard)) => self.record_hold = Some(guard),
                Err(error) => {
                    tracing::warn!(task = %self.name, %error, "failed to hold SIGTERM during loop recording");
                }
            }
        }
        if let Some(at) = at {
            record.at = at;
        }
        // A `fire-at` row gets one scheduled attempt, whatever its result.
        if self.mode == LoopRunMode::Scheduled
            && self.entry.fire_at.is_some()
            && let Err(err) = self.remove_schedule()
        {
            tracing::warn!(task = %self.name, error = %format!("{err:#}"), "failed to remove a fired fire-at task");
        }
        if record.result != LoopRunResult::CapacityHeld
            && !(record.result == LoopRunResult::ThrottleSkipped && self.capacity_condition())
            && let Err(error) = self.clear_capacity()
        {
            tracing::warn!(task = %self.name, %error, "failed to clear loop capacity wait");
        }
        let result = record.result;
        let transition = self.record_and_release(&mut record);
        if record.result != result {
            presentation.exit_code = record.result.spawn_exit_code();
        }
        self.finished = true;
        // A turn this fire never reported is released with its row.
        self.throttle_turn = None;
        TaskFireFinished {
            record,
            presentation,
            transition,
            notice,
            replace_holder: None,
        }
    }

    fn record_and_release(&mut self, record: &mut LoopRunRecord) -> RunTransition {
        let Some(holder) = self.run_lock.as_ref().map(|guard| guard.info) else {
            let transition = run_log::record_transition(&self.task, record);
            self.record_hold = None;
            return transition;
        };
        let result = (|| -> Result<RunTransition> {
            let root = self.entry.resolved_root();
            let paths = StatePaths::for_project_root(&root)?;
            let mut backlog = LockedBacklog::acquire(&paths)?;
            let stopping = backlog
                .record
                .tasks
                .get_mut(&self.name)
                .and_then(|task| task.stopping.take_if(|mark| holder.matches_stop(mark)));
            if let Some(mark) = &stopping {
                if record.result == LoopRunResult::Canceled {
                    set_stopped_result(record, mark.cause);
                }
                if let Err(error) = backlog.save() {
                    // The surviving mark leaves the row to the stop ladder;
                    // writing it here would let both sides record this run.
                    tracing::warn!(task = %self.name, %error, "failed to retire loop stop mark; leaving terminal history to the stop ladder");
                    self.run_lock = None;
                    self.record_hold = None;
                    return Ok(RunTransition::Recorded);
                }
            }
            let transition = run_log::record_transition(&self.task, record);
            let release = spawn_backlog_drain(&paths, &root, &self.name, &backlog.record);
            // Keep the terminal row and release in one critical section: a stop
            // must not install a new mark after this row has already been written.
            self.run_lock = None;
            self.record_hold = None;
            if let Err(error) = release {
                tracing::warn!(task = %self.name, %error, "failed to start loop backlog drain");
            }
            Ok(transition)
        })();
        match result {
            Ok(transition) => transition,
            Err(error) => {
                tracing::warn!(task = %self.name, %error, "failed to lock loop backlog for terminal history");
                run_log::record_transition(&self.task, record)
            }
        }
    }
}

impl Drop for TaskFire<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.release_run_lock() {
            tracing::warn!(task = %self.name, error = %format!("{error:#}"), "failed to release loop run and start its backlog drain");
        }
    }
}

fn finish_spawn_effect(
    record: &mut LoopRunRecord,
    effect: SupervisedRunOutcome,
    stream: bool,
) -> (LoopRunPresentation, TaskFireNotice) {
    match effect {
        SupervisedRunOutcome::Record(run) => {
            let run = *run;
            let status = run.status;
            record.result = status.into();
            record.run_id = Some(run.run_id.to_string());
            record.transcript_path = run.transcript_path;
            record.last_message = run.last_message;
            record.cost_usd = run.cost_usd;
            record.input_tokens = run.input_tokens;
            record.output_tokens = run.output_tokens;
            add_check_usage(record);
            (
                LoopRunPresentation {
                    failure_tail: run.failure_tail,
                    streamed: stream,
                    exit_code: Some(status.exit_code()),
                    ..LoopRunPresentation::default()
                },
                TaskFireNotice::None,
            )
        }
        SupervisedRunOutcome::Background { .. } => {
            (LoopRunPresentation::default(), TaskFireNotice::None)
        }
        SupervisedRunOutcome::BudgetExceeded { reason } => {
            record.result = LoopRunResult::BudgetSkipped;
            record.error = Some(reason.clone());
            (
                LoopRunPresentation::default(),
                TaskFireNotice::Gate { reason },
            )
        }
    }
}

fn add_check_usage(record: &mut LoopRunRecord) {
    let (mut cost_usd, mut input_tokens, mut output_tokens) =
        (record.cost_usd, record.input_tokens, record.output_tokens);
    for stage in record.stages() {
        let RunStage::Judge(judge) = stage else {
            continue;
        };
        cost_usd = match (cost_usd, judge.cost_usd) {
            (None, None) => None,
            (run, check) => Some(run.unwrap_or_default() + check.unwrap_or_default()),
        };
        input_tokens = match (input_tokens, judge.input_tokens) {
            (None, None) => None,
            (run, check) => Some(
                run.unwrap_or_default()
                    .saturating_add(check.unwrap_or_default()),
            ),
        };
        output_tokens = match (output_tokens, judge.output_tokens) {
            (None, None) => None,
            (run, check) => Some(
                run.unwrap_or_default()
                    .saturating_add(check.unwrap_or_default()),
            ),
        };
    }
    (record.cost_usd, record.input_tokens, record.output_tokens) =
        (cost_usd, input_tokens, output_tokens);
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn relative_age(ts: Timestamp, now: Timestamp) -> String {
    let age = now.duration_since(ts);
    if age.is_negative() {
        return "now".to_owned();
    }
    let secs = age.as_secs().max(0) as u64;
    let label = if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}d", secs / 86_400)
    };
    format!("{label} ago")
}

/// The account a task's gates, hooks preflight, window reads, and launch use
/// for `kind`: the row's pin, else the room's current account, which is `None`
/// when the room's record resolves none.
pub fn task_login(
    launch: &LaunchLogin,
    kind: &str,
    runtime: &RuntimePaths,
) -> Result<Option<ProviderLogin>, RoomLoginErr> {
    match launch {
        LaunchLogin::RoomDefault => {
            Ok(crate::agents::RoomLoginSet::for_runtime(runtime).default_login(kind))
        }
        LaunchLogin::Pinned(name) => crate::agents::session_login(
            &AgentKind::new_unchecked(kind),
            Some(name),
            &MachineConfig::load_lenient().accounts,
        )
        .map(Some),
    }
}

/// The account gate for a task pinned to `name`: the login its fire runs on,
/// or the reason the fire is skipped, ending in the fix. A fire never falls
/// back to the room's account. A home without trusted hooks passes here and
/// fails the hooks preflight as an error.
fn pinned_login(
    kind: &AgentKind,
    name: &crate::ids::LoginName,
    accounts: &crate::config::AccountsConfig,
    ambient: &BTreeMap<String, String>,
    statuses: &AccountsCache,
) -> Result<Result<ProviderLogin, String>, RoomLoginErr> {
    let login = match crate::agents::session_login(kind, Some(name), accounts) {
        Ok(login) => login,
        Err(RoomLoginErr::Login(err)) => return Ok(Err(unresolved_pin(&err, name))),
        Err(err) => return Err(err),
    };
    if let Err(err @ BirthLoginErr::MissingHome { .. }) = login.preflight(ambient) {
        return Ok(Err(err.to_string()));
    }
    if ProviderStatus::from_record(statuses.logins.get(&login.key())) == ProviderStatus::LoggedOut {
        let start: String = login
            .overrides(ambient)
            .into_iter()
            .map(|(key, value)| {
                let value = shlex::try_quote(&value).unwrap_or(value.as_str().into());
                format!("{key}={value} ")
            })
            .collect();
        return Ok(Err(format!(
            "{kind} account `{name}` is logged out; log in with `{start}{kind}`"
        )));
    }
    Ok(Ok(login))
}

/// Why a pin that does not resolve skips the fire, ending in the fix.
fn unresolved_pin(err: &LoginErr, name: &crate::ids::LoginName) -> String {
    match err {
        LoginErr::Unknown { .. } => err.to_string(),
        LoginErr::Unsupported { .. } => {
            format!("{err}; remove `account = \"{name}\"` from the task")
        }
    }
}

/// Hooks preflight for the action's kind under `login`, its [`task_login`].
pub fn preflight_entry(
    action: &TaskAction,
    resolved: Option<&ResolvedSingleAgentLaunch>,
    login: Option<&ProviderLogin>,
) -> Result<()> {
    match action {
        TaskAction::Spawn(spec) => {
            let resolved = resolved
                .with_context(|| format!("missing resolved loop task spec for `{spec}`"))?;
            preflight_kind(&resolved.kind, login)?;
        }
        TaskAction::Deliver(target) => preflight_kind(&target.kind, login)?,
        TaskAction::CheckOnly | TaskAction::Workflow(_) => {}
    }
    Ok(())
}

fn preflight_kind(kind: &str, login: Option<&ProviderLogin>) -> Result<()> {
    let adapter =
        find_definition(kind).ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
    let login = login.with_context(|| {
        format!("cannot resolve the room's {kind} account; run `rimz accounts list`")
    })?;
    match preflight_hooks(
        adapter,
        &login.env(&ambient_env()),
        TurnLifecycleNeed::NotUnsupported,
    ) {
        Ok(()) => Ok(()),
        Err(HookPreflightErr::TurnLifecycleUnsupported { reason }) => anyhow::bail!(
            "{kind} cannot run as a scheduled turn: a verified executable turn-lifecycle signal is required; {reason}"
        ),
        Err(HookPreflightErr::HooksMissing) => anyhow::bail!(
            "{kind} hooks are not installed, so a scheduled turn cannot report completion\ninstall them with `rimz hooks install {kind}`"
        ),
        Err(HookPreflightErr::HooksUntrusted { hooks, fix }) => anyhow::bail!(
            "{kind} hooks are installed but not trusted ({}), so a scheduled turn cannot report completion\n{}",
            hooks,
            fix
        ),
        Err(HookPreflightErr::HooksDisabled { reason, fix }) => {
            anyhow::bail!("{kind} cannot run as a scheduled turn: {reason}\n{fix}")
        }
    }
}

/// Why `rimz loop add` refuses a window trigger; each display carries its fix.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WindowRefusal {
    #[error(
        "a window trigger reads the task's provider, and a --check-only task has none; add --agent or --wait"
    )]
    NoProvider,
    #[error(
        "{kind} selects an exact managed account at launch, so its windows cannot be read before the run; window triggers do not support {kind}"
    )]
    ManagedAccount { kind: String },
    #[error("cannot resolve the room's {kind} account; run `rimz accounts list`")]
    NoAccount { kind: String },
    #[error(
        "no current {kind} {span} window reading; open the room's sidebar or run `rimz accounts --refresh`"
    )]
    NoReading { kind: String, span: WindowSpan },
    #[error("{kind} has no {span} window")]
    NoWindow { kind: String, span: WindowSpan },
    #[error(
        "{kind} is not enforcing its {span} window, so it has no reset to wait for; use --after-reset {other}"
    )]
    Lifted {
        kind: String,
        span: WindowSpan,
        other: WindowSpan,
    },
    #[error(
        "the {span} window is {kind}'s longest, so at its reset the window has not started and --surplus would always skip; drop --surplus and --surplus-after, or use --after-reset {other}"
    )]
    SurplusAtLongestReset {
        kind: String,
        span: WindowSpan,
        other: WindowSpan,
    },
}

/// When an `--after-reset` task fires: its provider's window as add read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AfterReset {
    pub kind: AgentKind,
    pub span: WindowSpan,
    pub left: Option<u8>,
    pub resets_at: Timestamp,
    /// A window that has not started fires now rather than a full span away.
    pub started: bool,
    pub fire_at: Timestamp,
}

/// A provider's window of one span as `rimz loop add` reads it.
#[derive(Clone, Debug, PartialEq)]
struct WindowAtAdd {
    pub kind: AgentKind,
    pub span: WindowSpan,
    pub window: RateLimitWindow,
    /// The span is the provider's longest window, the one `--surplus` reads.
    pub longest: bool,
}

/// The provider a task's `window.*` terms read, once add can read each window
/// they name; `None` when the clauses read no window or do not parse.
pub fn window_condition_provider(
    when: &[String],
    kind: Option<&str>,
    login: Option<&crate::ids::LoginKey>,
    root: &Path,
    runtime: &RuntimePaths,
    now: Timestamp,
) -> Result<Option<AgentKind>, WindowRefusal> {
    let Ok(expr) = super::when::WhenExpr::parse(when) else {
        return Ok(None);
    };
    let spans = expr
        .window_spans()
        .collect::<std::collections::BTreeSet<_>>();
    let mut provider = None;
    for span in spans {
        provider = Some(window_at_add(kind, span, login, root, runtime, now)?.kind);
    }
    Ok(provider)
}

/// The stored reading of `kind`'s `span` window on `login`, the key of the
/// task's [`task_login`], refused in the order the fixes apply: provider,
/// account selection, account, reading, window.
fn window_at_add(
    kind: Option<&str>,
    span: WindowSpan,
    login: Option<&crate::ids::LoginKey>,
    root: &Path,
    runtime: &RuntimePaths,
    now: Timestamp,
) -> Result<WindowAtAdd, WindowRefusal> {
    let kind = kind.ok_or(WindowRefusal::NoProvider)?;
    let owned = || kind.to_owned();
    if !matches!(
        unresolved_managed_state(root, kind),
        ManagedLaunchState::Unsupported
    ) {
        return Err(WindowRefusal::ManagedAccount { kind: owned() });
    }
    let key = login.ok_or_else(|| WindowRefusal::NoAccount { kind: owned() })?;
    let capacity =
        ProviderCapacity::read(runtime, key).ok_or_else(|| WindowRefusal::NoReading {
            kind: owned(),
            span,
        })?;
    // Projecting to the earliest instant leaves the stored reading as cached:
    // add refuses an expired window instead of reading it as refilled.
    let window = capacity
        .window_of_span(span, Timestamp::MIN)
        .ok_or_else(|| WindowRefusal::NoWindow {
            kind: owned(),
            span,
        })?;
    if !window.lifted && window.resets_at.is_some_and(|reset| reset <= now) {
        return Err(WindowRefusal::NoReading {
            kind: owned(),
            span,
        });
    }
    Ok(WindowAtAdd {
        kind: AgentKind::new_unchecked(kind),
        span,
        window,
        longest: capacity
            .longest_window_observation(now)
            .is_some_and(|longest| longest.duration_mins == Some(span.minutes())),
    })
}

/// Resolve `--after-reset <span>`: refused as a window term is, then for a
/// lifted window, a window without a reset, and a surplus gate that the reset
/// would always close.
pub fn after_reset(
    span: WindowSpan,
    kind: Option<&str>,
    login: Option<&crate::ids::LoginKey>,
    surplus: bool,
    root: &Path,
    runtime: &RuntimePaths,
    now: Timestamp,
) -> Result<AfterReset, WindowRefusal> {
    let WindowAtAdd {
        kind,
        span,
        window,
        longest,
    } = window_at_add(kind, span, login, root, runtime, now)?;
    let other = match span {
        WindowSpan::FiveHour => WindowSpan::SevenDay,
        WindowSpan::SevenDay => WindowSpan::FiveHour,
    };
    let refused_kind = || kind.as_str().to_owned();
    if window.lifted {
        return Err(WindowRefusal::Lifted {
            kind: refused_kind(),
            span,
            other,
        });
    }
    let resets_at = window.resets_at.ok_or_else(|| WindowRefusal::NoReading {
        kind: refused_kind(),
        span,
    })?;
    if surplus && longest {
        return Err(WindowRefusal::SurplusAtLongestReset {
            kind: refused_kind(),
            span,
            other,
        });
    }
    let started = !window.not_started(now);
    Ok(AfterReset {
        left: super::when::percent_left(&window),
        resets_at,
        started,
        fire_at: if started { resets_at } else { now },
        kind,
        span,
    })
}

pub fn parse_mode(raw: &str) -> Result<String> {
    Ok(mode_name(parse_mode_value(raw)?).to_owned())
}

fn parse_mode_value(raw: &str) -> Result<PermissionMode> {
    let trimmed = raw.trim();
    match PermissionMode::from_str(trimmed) {
        Ok(PermissionMode::Plan) | Err(_) => {
            anyhow::bail!("unknown loop mode `{trimmed}`; use auto, ask, or yolo")
        }
        Ok(mode) => Ok(mode),
    }
}

fn mode_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Auto => "auto",
        PermissionMode::Ask => "ask",
        PermissionMode::Yolo => "yolo",
        PermissionMode::Plan => unreachable!("loop mode parser rejects plan"),
    }
}

pub fn parse_task_timeout(raw: &str) -> std::result::Result<Duration, String> {
    parse_duration_units(raw, TASK_TIMEOUT_UNITS).map_err(|err| err.to_string())
}

fn resolve_task_prompt(name: &str, entry: &TaskEntry) -> Result<String> {
    if let Some(prompt) = entry
        .prompt
        .as_deref()
        .filter(|prompt| entry.wait.is_some() || !prompt.trim().is_empty())
    {
        return Ok(prompt.to_owned());
    }
    let Some(path) = entry.prompt_file.as_deref() else {
        if entry.wait.is_some() {
            return Ok(String::new());
        }
        anyhow::bail!("loop task `{name}` has no prompt; set `prompt` or `prompt-file`");
    };
    let path = resolve_config_path(path)?;
    let prompt = std::fs::read_to_string(&path)
        .with_context(|| format!("reading prompt-file `{}`", path.display()))?;
    if prompt.trim().is_empty() {
        anyhow::bail!("prompt-file `{}` is empty", path.display());
    }
    Ok(prompt)
}

fn resolve_config_path(path: &Path) -> Result<PathBuf> {
    let expanded = expand_tilde(path);
    if expanded.is_absolute() {
        return Ok(expanded);
    }
    let loop_path = MachineConfig::loop_path();
    let config_dir = loop_path.parent().unwrap_or_else(|| Path::new("."));
    Ok(config_dir.join(expanded))
}

fn expand_tilde(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    path.to_path_buf()
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn resolve_managed_spawn_state(
    entry: &TaskEntry,
    workspace: &crate::workspace::ResolvedWorkspace,
    resolved: &ResolvedSingleAgentLaunch,
) -> Result<ManagedLaunchState> {
    let adapter = find_definition(&resolved.kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", resolved.kind))?;
    if entry.worktree.is_some() {
        let applicability = adapter.resolve_managed_launch(
            &workspace.worktree_root,
            &std::collections::BTreeMap::new(),
            resolved.model.as_deref(),
            &resolved.args,
        );
        return Ok(
            if matches!(applicability, ManagedLaunchState::Unsupported) {
                ManagedLaunchState::Unsupported
            } else {
                ManagedLaunchState::Unresolved
            },
        );
    }
    let launch = crate::agents::LaunchParams {
        model: resolved.model.clone(),
        ..crate::agents::LaunchParams::default()
    };
    let mut invocation = crate::harness::launch::ExecRequest::bare_launch(
        crate::ids::AgentKind::new_unchecked(resolved.kind.clone()),
        resolved.args.clone(),
    );
    invocation.identity.params = launch;
    let (_, managed_launch) = crate::harness::launch::compile_managed_agent_process(
        &workspace.project_root,
        &invocation,
        &workspace.worktree_root,
        &ManagedLaunchState::PendingResolution,
        None,
    )?;
    Ok(managed_launch)
}

/// Newest durable supervised run still active for one loop task.
fn newest_active_run(paths: &StatePaths, name: &str) -> Result<Option<RunRecord>> {
    let mut records = crate::harness::run::list(paths)?;
    records
        .retain(|record| !record.status.is_terminal() && record.loop_task.as_deref() == Some(name));
    records.sort_by_key(|record| std::cmp::Reverse(record.started_at));
    Ok(records.into_iter().next())
}

/// A loop run in flight: the held run lock's holder, and its run record once written.
pub struct InFlightRun {
    pub holder: Option<RunLockInfo>,
    pub run: Option<RunRecord>,
    /// The task's runs waiting for their turn in the start throttle; the
    /// holder is one of them when its pid matches.
    pub held: Vec<throttle::Held>,
}

/// The run of task `name` in flight under `root`. The run lock decides; the run
/// record is read only while the lock is held, so a stale record is no run, and
/// never for a holder still waiting in the start throttle, which has none.
pub fn in_flight_run(name: &str, root: &Path) -> Result<Option<InFlightRun>> {
    let Some((_, holder)) = held_run_lock(name, root)? else {
        return Ok(None);
    };
    let (_, project_root) = stop_workspace(root)?;
    let held = throttle::held(name, root);
    // A holder still held for its turn has started no run of its own.
    let holder_held = holder.is_some_and(|info| held.iter().any(|held| held.pid == info.pid));
    let run = if holder_held {
        None
    } else {
        newest_active_run(&StatePaths::for_project_root(&project_root)?, name)?
    };
    Ok(Some(InFlightRun { holder, run, held }))
}

pub(super) fn effective_spawn_timeout(
    mode: crate::harness::schedule::run_log::LoopRunMode,
    task_timeout: Option<Duration>,
    configured_timeout: Option<Duration>,
) -> Option<Duration> {
    task_timeout.or_else(|| {
        (mode == crate::harness::schedule::run_log::LoopRunMode::Scheduled)
            .then_some(configured_timeout.unwrap_or(SCHEDULED_RUN_DEFAULT_TIMEOUT))
    })
}

const STOP_GRACE: Duration = Duration::from_secs(5);

pub enum StopOutcome {
    NoActiveRun,
    Stopped {
        run_id: Option<RunId>,
        signaled: bool,
        waiting: usize,
    },
}

/// Stop the run of task `name` under `root`; the task's row may already be gone.
pub fn stop_task(
    name: &str,
    root: &Path,
    cause: StopCause,
    expected: Option<RunLockInfo>,
    cancel: impl FnOnce(Option<&ResolvedWorkspace>, StatePaths, Option<&RunRecord>) -> Result<()>,
) -> Result<(StopOutcome, RunTransition)> {
    if expected.is_none() && held_run_lock(name, root)?.is_none() {
        return Ok((StopOutcome::NoActiveRun, RunTransition::Recorded));
    }
    let (workspace, project_root) = stop_workspace(root)?;
    let paths = StatePaths::for_project_root(&project_root)?;
    let backlog_paths = StatePaths::for_project_root(root)?;
    let mut backlog = LockedBacklog::acquire(&backlog_paths)?;
    let Some((lock, holder)) = held_run_lock(name, root)? else {
        let transition = match expected {
            Some(info) => append_stopped_record(name, root, info, None, None, &mut backlog)?,
            None => RunTransition::Recorded,
        };
        return Ok((StopOutcome::NoActiveRun, transition));
    };
    if let Some(expected) = expected
        && Some(expected) != holder
    {
        // The original holder released and a drainer may already own this file.
        let transition = append_stopped_record(name, root, expected, None, None, &mut backlog)?;
        return Ok((StopOutcome::NoActiveRun, transition));
    }
    let info = holder
        .context("loop run lock holder identity unavailable; stop the holder manually and retry")?;
    let metadata = std::fs::read(&lock)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<RunLockMetadata>(&bytes).ok())
        .filter(|metadata| Some(metadata.holder) == holder);
    let kind = metadata.as_ref().and_then(|metadata| metadata.kind);
    let check_only = metadata
        .as_ref()
        .and_then(|metadata| metadata.action_kind)
        .is_some_and(TaskActionKind::is_check_only);

    let waiting = backlog.record.tasks.entry(name.to_owned()).or_default();
    if expected.is_none()
        || !waiting
            .stopping
            .as_ref()
            .is_some_and(|mark| info.matches_stop(mark))
    {
        waiting.stopping = Some(StopMark {
            pid: info.pid,
            started_at: info.started_at,
            cause,
        });
    }
    let waiting = waiting.triggers.len();
    backlog.save()?;
    let run = newest_active_run(&paths, name);
    drop(backlog);
    cancel(
        workspace.as_ref(),
        paths.clone(),
        run.as_ref().ok().and_then(Option::as_ref),
    )?;
    let run = run?;

    if !check_only && wait_for_run_lock_release_path(&lock, holder, STOP_GRACE)? {
        let mut backlog = LockedBacklog::acquire(&backlog_paths)?;
        let transition = append_stopped_record(name, root, info, run.as_ref(), kind, &mut backlog)?;
        return Ok((
            StopOutcome::Stopped {
                run_id: run.map(|record| record.run_id),
                signaled: false,
                waiting,
            },
            transition,
        ));
    }

    // Cancellation and lock I/O can outlast a holder. Never signal its successor.
    let (signaled, signal_error) = match probe_run_lock_path(&lock)? {
        RunLockState::Held(current) if current == holder => {
            (true, signal_run_lock_holder(&info).err())
        }
        _ => (false, None),
    };
    let signal_deadline = Instant::now() + STOP_GRACE;

    if signal_error.is_none() && wait_for_run_lock_release_path(&lock, holder, STOP_GRACE)? {
        let mut backlog = LockedBacklog::acquire(&backlog_paths)?;
        let transition = append_stopped_record(name, root, info, run.as_ref(), kind, &mut backlog)?;
        drop(backlog);
        // A run stopped while held for its turn dies holding a ticket. The
        // kernel frees the lock before `/proc` reports the holder dead, and the
        // sweep removes a ticket only once its owner reads dead, so wait for
        // that within the same grace. A holder still live past it keeps its
        // ticket for the next admit's sweep.
        if !wait_for_process_exit(
            info.pid,
            signal_deadline.saturating_duration_since(Instant::now()),
        ) {
            tracing::debug!(pid = info.pid, "stopped loop run holder still reads live");
        }
        throttle::reap();
        return Ok((
            StopOutcome::Stopped {
                run_id: run.map(|record| record.run_id),
                signaled,
                waiting,
            },
            transition,
        ));
    }

    let holder = holder
        .map(|info| format!(" (pid {})", info.pid))
        .unwrap_or_default();
    let signal = signal_error
        .map(|err| format!("; SIGTERM failed: {err:#}"))
        .unwrap_or_default();
    bail!(
        "loop `{name}` is still active{holder}; lock {}; stop the holder manually and retry{signal}",
        lock.display()
    )
}

fn stop_workspace(root: &Path) -> Result<(Option<ResolvedWorkspace>, PathBuf)> {
    let workspace = root
        .exists()
        .then(|| WorkspaceResolver::resolve(root, None))
        .transpose()
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    let project_root = match &workspace {
        Some(workspace) => workspace.project_root.clone(),
        None => WorkspaceResolver::persisted_project_root(root)?,
    };
    Ok((workspace, project_root))
}

fn append_stopped_record(
    name: &str,
    root: &Path,
    info: RunLockInfo,
    run: Option<&RunRecord>,
    kind: Option<TaskKind>,
    backlog: &mut LockedBacklog<'_>,
) -> Result<RunTransition> {
    let Some(mark) = backlog
        .record
        .tasks
        .get_mut(name)
        .and_then(|task| task.stopping.take_if(|mark| info.matches_stop(mark)))
    else {
        return Ok(RunTransition::Recorded);
    };
    let paths = StatePaths::for_project_root(root)?;
    let elapsed = Timestamp::now()
        .as_millisecond()
        .saturating_sub(info.started_at.as_millisecond());
    let duration_ms = u64::try_from(elapsed).unwrap_or(0);
    let mut record = LoopRunRecord::new(
        name,
        LoopRunResult::Canceled,
        LoopRunMode::Scheduled,
        duration_ms,
    );
    record.mode = None;
    set_stopped_result(&mut record, mark.cause);
    record.run_id = run.map(|record| record.run_id.to_string());
    let transition = if mark.cause == StopCause::Replace {
        let catalog = TaskCatalog::load(Some(root))?;
        let task = catalog
            .for_run(name)
            .context("replaced loop task was removed")?;
        record.kind = Some(task.kind());
        backlog.save()?;
        run_log::record_transition(task, &record)
    } else {
        backlog.save()?;
        run_log::record_stopped(root, &record, kind);
        RunTransition::Recorded
    };
    if let Err(error) = spawn_backlog_drain(&paths, root, name, &backlog.record) {
        tracing::warn!(task = name, %error, "failed to start loop backlog drain");
    }
    Ok(transition)
}

fn set_stopped_result(record: &mut LoopRunRecord, cause: StopCause) {
    let (result, detail) = match cause {
        StopCause::Replace => (LoopRunResult::Replaced, "replaced by a newer trigger"),
        StopCause::Stop => (LoopRunResult::Canceled, "stopped by rimz loop stop"),
    };
    record.result = result;
    record.error = Some(detail.to_owned());
}

fn spawn_backlog_drain(
    paths: &StatePaths,
    root: &Path,
    name: &str,
    backlog: &backlog::Backlog,
) -> Result<()> {
    if backlog
        .tasks
        .get(name)
        .is_some_and(|task| !task.triggers.is_empty())
    {
        super::fire::spawn_loop_drain(
            &RuntimePaths::for_state(paths)?,
            root,
            name,
            super::fire::LoopRunHost::Detached,
        )?;
    }
    Ok(())
}

struct RunLockGuard {
    _lease: Lease,
    info: RunLockInfo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLockInfo {
    pub pid: u32,
    pub started_at: Timestamp,
}

impl RunLockInfo {
    fn matches_stop(self, mark: &StopMark) -> bool {
        self.pid == mark.pid && self.started_at == mark.started_at
    }
}

#[derive(Serialize, Deserialize)]
struct RunLockMetadata {
    #[serde(flatten)]
    holder: RunLockInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<TaskKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action_kind: Option<TaskActionKind>,
}

enum RunLockAttempt {
    Acquired(RunLockGuard),
    Held(Option<RunLockInfo>),
}

pub enum RunLockState {
    Available,
    Held(Option<RunLockInfo>),
}

/// One listing of a root's `locks/`, with every run lock in it probed once.
pub struct RunLocks {
    files: Vec<RunLockFile>,
}

struct RunLockFile {
    name: String,
    path: PathBuf,
    state: Result<RunLockState>,
}

impl RunLocks {
    /// List and probe run locks, including a cold check's runtime-only lock.
    pub fn list(root: &Path) -> Result<Self> {
        let runtime = RuntimePaths::for_state(&StatePaths::for_project_root(root)?)?;
        let locks = runtime.locks_dir;
        let listing = || format!("listing loop run locks `{}`", locks.display());
        let entries = match std::fs::read_dir(&locks) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self { files: Vec::new() });
            }
            Err(err) => return Err(err).with_context(listing),
        };
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.with_context(listing)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if run_lock_stem(&name).is_none() {
                continue;
            }
            let path = entry.path();
            let state = probe_run_lock_path(&path);
            files.push(RunLockFile { name, path, state });
        }
        Ok(Self { files })
    }

    /// Whether task `name` is running: held when any lock file it claims is.
    pub fn state(&self, name: &str) -> Result<RunLockState> {
        Ok(match self.held(name)? {
            Some((_, holder)) => RunLockState::Held(holder),
            None => RunLockState::Available,
        })
    }

    /// The held locks none of `rows` claims, as task name and holder, by name.
    /// The name is the file's whole stem, the one `in_flight_run` and
    /// `stop_task` resolve back to that file.
    pub fn rowless(&self, rows: &[&str]) -> Vec<(String, Option<RunLockInfo>)> {
        let mut rowless = Vec::new();
        for file in &self.files {
            let Ok(RunLockState::Held(holder)) = &file.state else {
                continue;
            };
            if rows.iter().any(|row| is_run_lock_of(&file.name, row)) {
                continue;
            }
            if let Some(stem) = run_lock_stem(&file.name) {
                rowless.push((stem.to_owned(), *holder));
            }
        }
        rowless.sort_by(|(left, _), (right, _)| left.cmp(right));
        rowless
    }

    /// The held run lock of task `name`, with its holder. A fan-out task holds
    /// one lock per checkout; the earliest-started holder is the one reported,
    /// a holderless lock last.
    fn held(&self, name: &str) -> Result<Option<(&Path, Option<RunLockInfo>)>> {
        let mut held = Vec::new();
        for file in &self.files {
            if !is_run_lock_of(&file.name, name) {
                continue;
            }
            match &file.state {
                Ok(RunLockState::Held(holder)) => held.push((file.path.as_path(), *holder)),
                Ok(RunLockState::Available) => {}
                Err(error) => bail!("{error:#}"),
            }
        }
        Ok(held
            .into_iter()
            .min_by_key(|(_, holder)| (holder.is_none(), holder.map(|info| info.started_at))))
    }
}

fn held_run_lock(name: &str, root: &Path) -> Result<Option<(PathBuf, Option<RunLockInfo>)>> {
    Ok(RunLocks::list(root)?
        .held(name)?
        .map(|(path, holder)| (path.to_owned(), holder)))
}

fn probe_run_lock_path(path: &Path) -> Result<RunLockState> {
    match probe(path).with_context(|| format!("probing loop run lock `{}`", path.display()))? {
        LeaseState::Free => Ok(RunLockState::Available),
        LeaseState::Held(body) => Ok(RunLockState::Held(read_run_lock_info(&body))),
    }
}

fn run_lock_path(file: &str, entry: &TaskEntry) -> Result<PathBuf> {
    let root = entry.resolved_root();
    let runtime =
        run_lock_runtime(&root)?.ok_or_else(|| crate::workspace::WorkspaceErr::NoRoom {
            location: format!("at {}", root.display()),
        })?;
    Ok(runtime.lock_path(file))
}

fn run_lock_runtime(root: &Path) -> Result<Option<RuntimePaths>> {
    let state = StatePaths::for_project_root(root).context("locating loop task state")?;
    if crate::workspace::record::read(&state.workspace_record).is_err() {
        return Ok(None);
    }
    let runtime = RuntimePaths::for_state(&state).context("locating loop task runtime")?;
    Ok(crate::store::Store::open_existing(state, runtime.clone()).map(|_| runtime))
}

/// The one spelling of a run lock's file name. `checkout` names the launch
/// checkout of a fan-out fire, which locks per checkout.
fn run_lock_file_name(name: &str, checkout: Option<&WorkspaceId>) -> String {
    match checkout {
        Some(checkout) => format!("loop-run-{name}-{checkout}.lock"),
        None => format!("loop-run-{name}.lock"),
    }
}

/// The task name a bare run lock file carries, `run_lock_file_name`'s inverse.
fn run_lock_stem(file: &str) -> Option<&str> {
    let frame = run_lock_file_name("\0", None);
    let (prefix, suffix) = frame.split_once('\0')?;
    file.strip_prefix(prefix)?
        .strip_suffix(suffix)
        .filter(|stem| !stem.is_empty())
}

/// Whether `file` is task `name`'s run lock, bare or per-checkout.
fn is_run_lock_of(file: &str, name: &str) -> bool {
    let checkout = file
        .strip_suffix(".lock")
        .and_then(|stem| stem.rsplit_once('-'))
        .and_then(|(_, id)| WorkspaceId::parse(id).ok());
    file == run_lock_file_name(name, None)
        || checkout.is_some_and(|checkout| file == run_lock_file_name(name, Some(&checkout)))
}

fn signal_run_lock_holder(info: &RunLockInfo) -> Result<()> {
    let pid = i32::try_from(info.pid).context("loop run lock holder pid is out of range")?;
    if pid == 0 {
        anyhow::bail!("loop run lock holder pid must be positive");
    }
    if info.pid == std::process::id() {
        anyhow::bail!("refusing to signal the current process as a loop run lock holder");
    }
    match kill(Pid::from_raw(pid), Signal::SIGTERM) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(err) => Err(err).with_context(|| format!("signaling loop run lock holder pid {pid}")),
    }
}

fn wait_for_run_lock_release_path(
    path: &Path,
    holder: Option<RunLockInfo>,
    grace: Duration,
) -> Result<bool> {
    let deadline = Instant::now() + grace;
    loop {
        if match probe_run_lock_path(path)? {
            RunLockState::Available => true,
            RunLockState::Held(current) => holder.is_some() && current != holder,
        } {
            return Ok(true);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        std::thread::sleep(RUN_LOCK_RELEASE_POLL_INTERVAL.min(remaining));
    }
}

fn wait_for_process_exit(pid: u32, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while crate::proc::process_is_live(pid, None) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        std::thread::sleep(HOLDER_EXIT_POLL_INTERVAL.min(remaining));
    }
    true
}

fn acquire_run_lock_file(
    path: &Path,
    kind: Option<TaskKind>,
    action_kind: Option<TaskActionKind>,
) -> Result<RunLockAttempt> {
    let mut holder = None;
    match claim(path, |file| {
        let info = RunLockInfo {
            pid: std::process::id(),
            started_at: Timestamp::now(),
        };
        holder = Some(info);
        serde_json::to_writer(
            file,
            &RunLockMetadata {
                holder: info,
                kind,
                action_kind,
            },
        )
        .map_err(std::io::Error::other)
    })
    .with_context(|| format!("locking loop run lock `{}`", path.display()))?
    {
        LeaseAttempt::Acquired(lease) => Ok(RunLockAttempt::Acquired(RunLockGuard {
            _lease: lease,
            // `claim` runs the body writer before it reports an acquired lease.
            info: holder.expect("acquired lease wrote its holder"),
        })),
        LeaseAttempt::Held(body) => Ok(RunLockAttempt::Held(read_run_lock_info(&body))),
    }
}

fn read_run_lock_info(body: &[u8]) -> Option<RunLockInfo> {
    serde_json::from_slice(body).ok()
}

pub struct CheckOutcome {
    passed: bool,
    timed_out: bool,
    interrupted: bool,
    terminated: bool,
    output: String,
    code: Option<i32>,
    stderr_line: String,
}

impl CheckOutcome {
    pub(super) fn stderr_line(&self) -> &str {
        &self.stderr_line
    }

    pub(super) fn timed_out(&self) -> bool {
        self.timed_out
    }

    pub(super) fn new(passed: bool, timed_out: bool, output: String, code: Option<i32>) -> Self {
        Self {
            passed,
            timed_out,
            interrupted: false,
            terminated: false,
            output,
            code,
            stderr_line: String::new(),
        }
    }

    pub fn passed(&self) -> bool {
        self.passed
    }

    pub fn terminated(&self) -> bool {
        self.terminated
    }

    pub(crate) fn code(&self) -> Option<i32> {
        self.code
    }

    pub(crate) fn interrupted(&self) -> bool {
        self.interrupted
    }
}

pub enum CheckEcho {
    Capture,
    Tee {
        file: File,
    },
    Stream {
        announcement: Option<String>,
        prefix: String,
    },
}

pub fn check_record(outcome: &CheckOutcome) -> CheckRecord {
    CheckRecord {
        code: outcome.code,
        timed_out: outcome.timed_out,
        output: outcome.output.clone(),
        output_path: None,
        duration_ms: None,
        agent: None,
    }
}

pub(super) fn configured_timeout(config: &MachineConfig) -> Result<Option<Duration>> {
    config
        .r#loop
        .default_timeout
        .as_deref()
        .map(parse_task_timeout)
        .transpose()
        .map_err(anyhow::Error::msg)
}

pub(super) fn task_timeout(entry: &TaskEntry) -> Result<Option<Duration>> {
    entry
        .timeout
        .as_deref()
        .map(|raw| parse_duration_units(raw, TASK_TIMEOUT_UNITS))
        .transpose()
        .map_err(|err| anyhow::anyhow!("{err}"))
}

fn check_only_result(outcome: &CheckOutcome) -> LoopRunResult {
    if outcome.timed_out {
        LoopRunResult::TimedOut
    } else if outcome.passed {
        LoopRunResult::Completed
    } else {
        LoopRunResult::Failed
    }
}

fn polarity_fires(on: Option<CheckOn>, outcome: &CheckOutcome) -> bool {
    match on.unwrap_or_default() {
        CheckOn::Fail => !outcome.passed,
        CheckOn::Success => outcome.passed,
        CheckOn::Any => true,
    }
}

fn augment_prompt(base: String, heading: &str, output: &str) -> String {
    format!("{base}\n\n--- {heading} ---\n{output}")
}

pub fn run_check(
    dir: &Path,
    cmd: &str,
    timeout: Duration,
    echo: CheckEcho,
    env: &BTreeMap<String, String>,
) -> Result<CheckOutcome> {
    run_command(
        dir,
        cmd,
        WatchDeadline::KillAfter(timeout),
        echo,
        env,
        |_, _| {},
        &mut None,
    )
}

pub(crate) enum WatchDeadline<'a> {
    KillAfter(Duration),
    CapacityCheck(Duration),
    /// Run to exit, with an optional one-time check-in.
    Watch(Option<Duration>),
    Hosted {
        argv: Option<&'a [std::ffi::OsString]>,
        interrupted: &'a dyn Fn() -> bool,
        spawned: &'a mut dyn FnMut(u32) -> std::io::Result<()>,
    },
}

#[cfg(not(test))]
fn check_command(command: &str) -> Command {
    let mut child = Command::new(crate::proc::rimz_exe());
    child.args(["loop", "check-exec", "--command", command]);
    child
}

#[cfg(test)]
fn check_command(command: &str) -> Command {
    use std::os::unix::process::CommandExt;

    // Unit tests cover the driver; CLI integration covers the session trampoline.
    let mut child = Command::new("sh");
    child.args(["-c", command]).process_group(0);
    child
}

struct CheckSignalState {
    unhandled: Option<Arc<AtomicBool>>,
    active: usize,
}

static CHECK_SIGNALS: Mutex<CheckSignalState> = Mutex::new(CheckSignalState {
    unhandled: None,
    active: 0,
});

/// Keep SIGTERM handled until an action's terminal record releases its run lock.
pub struct CheckSignalGuard {
    cancellation: Option<signal_hook::SigId>,
}

impl CheckSignalGuard {
    fn register<T>(register: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<(T, Self)> {
        let mut state = CHECK_SIGNALS
            .lock()
            .map_err(|_| std::io::Error::other("loop check signal lock poisoned"))?;
        let unhandled = match &state.unhandled {
            Some(flag) => Arc::clone(flag),
            None => {
                let flag = Arc::new(AtomicBool::new(true));
                // Unregistering signal-hook's iterator leaves SIGTERM ignored, so retain its
                // default action whenever no action or terminal record owns a guard.
                signal_hook::flag::register_conditional_default(
                    signal_hook::consts::SIGTERM,
                    Arc::clone(&flag),
                )?;
                state.unhandled = Some(Arc::clone(&flag));
                flag
            }
        };
        let handler = register()?;
        state.active += 1;
        unhandled.store(false, Ordering::SeqCst);
        Ok((handler, Self { cancellation: None }))
    }
}

impl Drop for CheckSignalGuard {
    fn drop(&mut self) {
        if let Some(handler) = self.cancellation.take() {
            signal_hook::low_level::unregister(handler);
        }
        let Ok(mut state) = CHECK_SIGNALS.lock() else {
            return;
        };
        state.active -= 1;
        if state.active == 0
            && let Some(flag) = &state.unhandled
        {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

struct CheckSignals {
    signals: signal_hook::iterator::Signals,
}

impl CheckSignals {
    fn new(signals: &[i32], record_hold: &mut Option<CheckSignalGuard>) -> std::io::Result<Self> {
        let (signals, guard) =
            CheckSignalGuard::register(|| signal_hook::iterator::Signals::new(signals))?;
        *record_hold = Some(guard);
        Ok(Self { signals })
    }

    fn pending(&mut self) -> impl Iterator<Item = i32> + '_ {
        #[cfg(feature = "testkit")]
        crate::testkit::rendezvous("RIMZ_TEST_CHECK_BEFORE_SIGNAL_READ");
        self.signals.pending()
    }
}

/// Cancel a loop-owned run on SIGTERM until the returned guard is dropped.
pub fn cancel_run_on_sigterm(cancellation: RunCancellation) -> std::io::Result<CheckSignalGuard> {
    let (handler, mut guard) = CheckSignalGuard::register(|| {
        signal_hook::flag::register(signal_hook::consts::SIGTERM, cancellation.signal_flag())
    })?;
    guard.cancellation = Some(handler);
    Ok(guard)
}

pub(crate) fn run_command(
    dir: &Path,
    cmd: &str,
    mut deadline: WatchDeadline<'_>,
    echo: CheckEcho,
    env: &BTreeMap<String, String>,
    mut check_in: impl FnMut(u64, String),
    record_hold: &mut Option<CheckSignalGuard>,
) -> Result<CheckOutcome> {
    let (prefix, file, cap) = match echo {
        CheckEcho::Capture => (None, None, CHECK_OUTPUT_CAP),
        CheckEcho::Tee { file } => (None, Some(file), WAIT_TAIL_CAP),
        CheckEcho::Stream {
            announcement,
            prefix,
        } => {
            if let Some(announcement) = announcement {
                let mut out = anstream::AutoStream::auto(std::io::stdout().lock());
                out.write_all(announcement.as_bytes())?;
                out.flush()?;
            }
            (Some(prefix), None, CHECK_OUTPUT_CAP)
        }
    };
    let capture = Arc::new(Mutex::new(CheckCapture {
        file,
        tail: Vec::with_capacity(cap),
        cap,
    }));
    let captured_output = || {
        capture
            .lock()
            .map(|capture| capture.output())
            .map_err(|_| anyhow::anyhow!("loop check output lock poisoned"))
    };
    let (mut command, mut interrupts, kill_after, check_in_after) = match &deadline {
        WatchDeadline::KillAfter(timeout) | WatchDeadline::CapacityCheck(timeout) => (
            check_command(cmd),
            Some(CheckSignals::new(
                &[signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM],
                record_hold,
            )?),
            Some(*timeout),
            None,
        ),
        WatchDeadline::Watch(check_in) => {
            let mut command = Command::new("sh");
            command.args(["-c", cmd]);
            (command, None, None, *check_in)
        }
        WatchDeadline::Hosted { argv, .. } => {
            use std::os::unix::process::CommandExt;

            let mut command = if let Some(argv) = argv {
                // Hosted argv comes from the validated workflow resolver.
                let mut command = Command::new(&argv[0]);
                command.args(&argv[1..]);
                command
            } else {
                let mut command = Command::new("sh");
                command.args(["-c", cmd]);
                command
            };
            command.process_group(0);
            (command, None, None, None)
        }
    };
    if env.contains_key(LOOP_TASK_ENV) {
        command.env_remove(crate::workspace::ENV_CHANNEL);
        for (key, _) in std::env::vars_os()
            .filter(|(key, _)| key.as_encoded_bytes().starts_with(b"RIMZ_AGENT_"))
        {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(dir)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running loop check `{cmd}` in {}", dir.display()))?;
    let stdout = drain_pipe(
        child.stdout.take(),
        Arc::clone(&capture),
        prefix
            .clone()
            .map(|prefix| PipeForward::new(PipeDestination::Stdout, prefix)),
    );
    let stderr = drain_pipe(
        child.stderr.take(),
        Arc::clone(&capture),
        prefix.map(|prefix| PipeForward::new(PipeDestination::Stderr, prefix)),
    );
    let started = Instant::now();
    let mut kill_at = kill_after.map(|timeout| started + timeout);
    let mut check_in_at = check_in_after.map(|timeout| started + timeout);
    let publication_error = match &mut deadline {
        WatchDeadline::Hosted { spawned, .. } => spawned(child.id()).err(),
        _ => None,
    };
    let host_interrupted =
        || matches!(&deadline, WatchDeadline::Hosted { interrupted, .. } if interrupted());
    let hosted = matches!(&deadline, WatchDeadline::Hosted { .. });
    let mut interrupted = false;
    let mut terminated = false;
    let mut forwarded = false;
    let mut pending_signals = Vec::new();
    let (status, timed_out) = loop {
        if publication_error.is_some() || host_interrupted() {
            if publication_error.is_none()
                && stdout.is_finished()
                && stderr.is_finished()
                && let Some(status) = child
                    .try_wait()
                    .with_context(|| format!("waiting for hosted command `{cmd}`"))?
            {
                break (status, false);
            }
            interrupted = true;
            let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
            let _ = child.kill();
            let status = child
                .wait()
                .with_context(|| format!("reaping interrupted hosted check `{cmd}`"))?;
            break (status, false);
        }
        // Read stops before exit so a completed check is not judged against
        // liveness sampled before the stop read.
        if let Some(interrupts) = &mut interrupts {
            for signal in interrupts.pending() {
                interrupted = true;
                let signal = if signal == signal_hook::consts::SIGTERM {
                    terminated = true;
                    Signal::SIGTERM
                } else {
                    let interrupt_deadline = Instant::now() + CHECK_INTERRUPT_GRACE;
                    kill_at = kill_at.map(|at| at.min(interrupt_deadline));
                    Signal::SIGINT
                };
                if !pending_signals.contains(&signal) {
                    pending_signals.push(signal);
                }
            }
        }
        if !forwarded
            && (!hosted || (stdout.is_finished() && stderr.is_finished()))
            && let Some(status) = child
                .try_wait()
                .with_context(|| format!("waiting for loop check `{cmd}`"))?
        {
            interrupted = false;
            terminated = false;
            break (status, false);
        }
        pending_signals.retain(
            |signal| match killpg(Pid::from_raw(child.id() as i32), *signal) {
                Ok(()) => {
                    forwarded = true;
                    false
                }
                Err(Errno::ESRCH) => true,
                Err(_) => false,
            },
        );
        // A shell can exit before its group's TERM handlers, and a hosted leader's start
        // token and PGID are its kill handle. Keep the lock, the original timeout, and the
        // leader unreaped until those members close their output pipes.
        if (!(terminated || hosted) || (stdout.is_finished() && stderr.is_finished()))
            && let Some(status) = child
                .try_wait()
                .with_context(|| format!("waiting for loop check `{cmd}`"))?
        {
            break (status, false);
        }
        if kill_at.is_some_and(|at| Instant::now() >= at) {
            let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
            let _ = child.kill();
            let status = child
                .wait()
                .with_context(|| format!("reaping timed-out loop check `{cmd}`"))?;
            break (status, !interrupted);
        }
        if check_in_at.take_if(|at| Instant::now() >= *at).is_some() {
            let output = captured_output()?;
            check_in(elapsed_millis(started), output);
        }
        std::thread::sleep(CHECK_POLL_INTERVAL);
    };
    let capacity_drain = matches!(deadline, WatchDeadline::CapacityCheck(_));
    let drain_deadline = if capacity_drain && !(timed_out || interrupted) {
        kill_at
    } else {
        (timed_out || interrupted).then(|| Instant::now() + CHECK_DRAIN_GRACE)
    };
    let mut stderr_line = String::new();
    for (is_stderr, drain) in [(false, stdout), (true, stderr)] {
        while !drain.is_finished() && drain_deadline.is_some_and(|at| Instant::now() < at) {
            std::thread::sleep(CHECK_POLL_INTERVAL);
        }
        if drain_deadline.is_some() && !drain.is_finished() {
            if capacity_drain {
                let _ = killpg(Pid::from_raw(child.id() as i32), Signal::SIGKILL);
            }
            tracing::debug!("stopped check output truncated while a survivor holds its pipe");
            continue;
        }
        let first_line = drain
            .join()
            .map_err(|_| anyhow::anyhow!("loop check output reader panicked"))??;
        if is_stderr {
            stderr_line = first_line;
        }
    }
    if let Some(error) = publication_error {
        return Err(error).context("publishing hosted check child");
    }
    Ok(CheckOutcome {
        passed: status.success() && !timed_out && !interrupted,
        timed_out,
        interrupted,
        terminated,
        output: captured_output()?,
        code: status.code(),
        stderr_line,
    })
}

#[derive(Clone, Copy)]
enum PipeDestination {
    Stdout,
    Stderr,
}

struct PipeForward {
    destination: PipeDestination,
    prefix: Vec<u8>,
    pending: Vec<u8>,
}

impl PipeForward {
    fn new(destination: PipeDestination, prefix: String) -> Self {
        Self {
            destination,
            prefix: prefix.into_bytes(),
            pending: Vec::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        while let Some(line) = take_complete_line(&mut self.pending) {
            let _ = self.write_line(&line);
        }
    }

    fn finish(mut self) {
        if let Some(line) = take_trailing_line(&mut self.pending) {
            let _ = self.write_line(&line);
        }
    }

    fn write_line(&self, line: &[u8]) -> std::io::Result<()> {
        let mut painted = Vec::with_capacity(self.prefix.len() + line.len());
        painted.extend_from_slice(&self.prefix);
        painted.extend_from_slice(line);
        match self.destination {
            PipeDestination::Stdout => {
                let mut out = anstream::AutoStream::auto(std::io::stdout().lock());
                out.write_all(&painted)?;
                out.flush()
            }
            PipeDestination::Stderr => {
                let mut err = anstream::AutoStream::auto(std::io::stderr().lock());
                err.write_all(&painted)?;
                err.flush()
            }
        }
    }
}

fn take_complete_line(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    let end = pending.iter().position(|byte| *byte == b'\n')?;
    Some(pending.drain(..=end).collect())
}

fn take_trailing_line(pending: &mut Vec<u8>) -> Option<Vec<u8>> {
    if pending.is_empty() {
        return None;
    }
    let mut line = std::mem::take(pending);
    line.push(b'\n');
    Some(line)
}

struct CheckCapture {
    file: Option<File>,
    tail: Vec<u8>,
    cap: usize,
}

impl CheckCapture {
    fn output(&self) -> String {
        let output = String::from_utf8_lossy(&self.tail);
        let mut start = output.len().saturating_sub(self.cap);
        while !output.is_char_boundary(start) {
            start += 1;
        }
        output[start..].to_owned()
    }

    fn push(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if let Some(file) = &mut self.file {
            file.write_all(bytes)?;
        }
        if bytes.len() >= self.cap {
            self.tail.clear();
            self.tail
                .extend_from_slice(&bytes[bytes.len() - self.cap..]);
            return Ok(());
        }
        let discard = (self.tail.len() + bytes.len()).saturating_sub(self.cap);
        self.tail.drain(..discard);
        self.tail.extend_from_slice(bytes);
        Ok(())
    }
}

fn drain_pipe(
    pipe: Option<impl Read + Send + 'static>,
    capture: Arc<Mutex<CheckCapture>>,
    mut forward: Option<PipeForward>,
) -> std::thread::JoinHandle<std::io::Result<String>> {
    std::thread::spawn(move || {
        let mut first_line = Vec::new();
        let mut complete = false;
        if let Some(mut pipe) = pipe {
            let mut chunk = [0; 8 * 1024];
            loop {
                let read = pipe.read(&mut chunk)?;
                if read == 0 {
                    break;
                }
                let bytes = &chunk[..read];
                if !complete {
                    let end = bytes.iter().position(|byte| *byte == b'\n');
                    let line = &bytes[..end.unwrap_or(bytes.len())];
                    let remaining = 1024_usize.saturating_sub(first_line.len());
                    first_line.extend_from_slice(&line[..line.len().min(remaining)]);
                    complete = end.is_some() || first_line.len() == 1024;
                }
                capture
                    .lock()
                    .map_err(|_| std::io::Error::other("loop check output lock poisoned"))?
                    .push(bytes)?;
                if let Some(forward) = &mut forward {
                    forward.push(bytes);
                }
            }
        }
        if let Some(forward) = forward {
            forward.finish();
        }
        if first_line.len() == 1024
            && let Err(error) = std::str::from_utf8(&first_line)
            && error.error_len().is_none()
        {
            first_line.truncate(error.valid_up_to());
        }
        Ok(String::from_utf8_lossy(&first_line).trim_end().to_owned())
    })
}

fn unresolved_managed_state(root: &Path, kind: &str) -> ManagedLaunchState {
    let Some(adapter) = find_definition(kind) else {
        return ManagedLaunchState::Unsupported;
    };
    let state = adapter.resolve_managed_launch(root, &std::collections::BTreeMap::new(), None, &[]);
    if matches!(state, ManagedLaunchState::Unsupported) {
        state
    } else {
        ManagedLaunchState::Unresolved
    }
}

fn surplus_gate_in(
    entry: &TaskEntry,
    kind: &str,
    reading: Option<WindowSurplus>,
) -> Option<String> {
    if entry.surplus.is_none() && entry.surplus_after.is_none() {
        return None;
    }
    let Some(reading) = reading else {
        return Some(format!(
            "no {kind} budget-window reading; surplus gate stays closed"
        ));
    };
    let after = match entry
        .surplus_after
        .as_deref()
        .map(super::parse_surplus_after)
    {
        Some(Ok(after)) => Some(after),
        Some(Err(_)) => {
            return Some("invalid surplus-after gate; surplus gate stays closed".to_owned());
        }
        None => None,
    };
    if let Some(after) = after
        && (reading.elapsed.as_secs().max(0) as u64) < after.as_secs()
    {
        return Some(format!(
            "{kind} {} window {} elapsed; fires after {}",
            crate::theme::fmt::window_duration_label(reading.duration_mins),
            elapsed_label(reading.elapsed),
            entry.surplus_after.as_deref().unwrap_or_default().trim(),
        ));
    }
    let threshold = match entry.surplus.as_deref().map(super::parse_surplus) {
        Some(Ok(threshold)) => threshold,
        Some(Err(_)) => return Some("invalid surplus gate; surplus gate stays closed".to_owned()),
        None => 1.0,
    };
    (reading.headroom < threshold).then(|| {
        format!(
            "{kind} {} window surplus {:.1}x below {threshold:.1}x",
            crate::theme::fmt::window_duration_label(reading.duration_mins),
            reading.headroom,
        )
    })
}

fn elapsed_label(elapsed: jiff::SignedDuration) -> String {
    let total_mins = elapsed.as_secs().max(0) / 60;
    let days = total_mins / (24 * 60);
    let hours = total_mins % (24 * 60) / 60;
    let mins = total_mins % 60;
    if days > 0 {
        if hours > 0 {
            format!("{days}d{hours}h")
        } else {
            format!("{days}d")
        }
    } else if hours > 0 {
        if mins > 0 {
            format!("{hours}h{mins}m")
        } else {
            format!("{hours}h")
        }
    } else {
        format!("{mins}m")
    }
}

#[cfg(test)]
mod tests;
