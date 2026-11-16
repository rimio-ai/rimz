//! Loop-fire policy from ordered gates through one durable history transition.
//!
//! [`TaskFire`] owns checks, deadlines, task consumption, run locks, prompt and
//! launch preparation, and terminal record mapping. Every other fire starts a workflow
//! run here; the CLI executes a prepared message delivery and returns its typed result.

mod prompt;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};

use super::{
    backlog::{self, StopCause, StopMark, WaitingTrigger},
    backlog_store::{self, LockedBacklog},
    fire::deadline_expired_at,
    run_lock::{file_name as run_lock_file_name, is_run_lock_of},
};
use crate::agents::PermissionMode;
use crate::agents::{
    ManagedLaunchState, ProviderCapacity, ProviderLogin, RateLimitWindow, WindowSpan,
    find_definition,
};
use crate::config::{CheckOn, MachineConfig, OverlapPolicy, TaskEntry, TaskTarget, WatchSpec};
use crate::disk::lease::{Lease, LeaseAttempt, LeaseState, claim, probe};
use crate::disk::paths::{RuntimePaths, StatePaths, logs_dir};
use crate::harness::check_run::{CheckOutcome, CheckSignalGuard};
use crate::harness::loop_task::{LOOP_BOOKED_BY_ENV, LOOP_TASK_ENV, read_fire_launch};
use crate::harness::run::SupervisedRunRequest;
use crate::harness::schedule::catalog::{self, LoadedTask, TaskCatalog};
use crate::harness::schedule::run_log::{
    self, CheckRecord, LoopRunMode, LoopRunPresentation, LoopRunRecord, LoopRunResult,
    RunTransition, SignalRecord,
};
use crate::harness::schedule::{TaskAction, TaskActionKind, TaskKind, Trigger};
use crate::harness::signal::{Signal as TriggerSignal, WatchOutcome};
use crate::harness::task_login::task_login;
use crate::ids::AgentKind;
use crate::ids::{MessageId, RunId, WorkspaceId};
use crate::store::run::RunRecord;
use crate::store::workflow::RunFire;
use crate::store::writer::LaunchLogin;
use crate::utils::time::{DurationUnit, parse_duration_units};
use crate::workspace::{ResolvedWorkspace, WorkspaceResolver};

pub const CHECK_DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
pub(super) const SCHEDULED_RUN_DEFAULT_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
pub const SCHEDULED_RUN_DEFAULT_TIMEOUT_LABEL: &str = "2h";
const RUN_LOCK_RELEASE_POLL_INTERVAL: Duration = Duration::from_millis(200);
const HOLDER_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
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

/// What fired the run whose command is running, for a launch under it to append to its
/// prompt; outside a fire's command, nothing.
pub fn loop_trigger() -> Option<String> {
    loop_check_task()?;
    std::env::var(crate::harness::loop_task::LOOP_TRIGGER_ENV)
        .ok()
        .filter(|text| !text.trim().is_empty())
}

fn check_task_from_identity(task: Option<String>, has_agent: bool) -> Option<String> {
    task.filter(|task| !has_agent && !task.is_empty())
}

/// The booking chain of the fire whose command is running, for a launch under it to carry;
/// outside a fire's command, nothing.
pub fn loop_check_booked_by() -> Vec<crate::agents::LaunchedBy> {
    loop_check_task()
        .map(|_| check_booked_by(std::env::var(LOOP_BOOKED_BY_ENV).ok()))
        .unwrap_or_default()
}

/// The firing task's booking chain from a check's environment; an absent or unreadable
/// value is no chain.
fn check_booked_by(raw: Option<String>) -> Vec<crate::agents::LaunchedBy> {
    raw.and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Apply loop-owned deadline, cleanup, and scheduled-tab placement policy, and stamp the
/// firing task's booking chain and the fire's workflow run from the check's environment.
pub fn shape_loop_owned(
    request: &mut SupervisedRunRequest,
    task: &str,
    config: &MachineConfig,
    mode: LoopRunMode,
) -> Result<()> {
    request.timeout = effective_spawn_timeout(mode, request.timeout, configured_timeout(config)?);
    request.force_new_tab |= mode == LoopRunMode::Scheduled;
    request.loop_task = Some(task.to_owned());
    request.loop_booked_by = loop_check_booked_by();
    request.loop_fire = loop_check_task()
        .and_then(|_| std::env::var("RIMZ_WORKFLOW_RUN").ok())
        .and_then(|raw| crate::ids::WorkflowRunId::parse(&raw).ok());
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
    /// The room the task was armed in, where [`Self::holder`] reads its rows.
    task_root: PathBuf,
    /// The agent the wake is for, as the task's target names it: a swap can
    /// move it to another session before the wake is sent.
    pub target: TaskTarget,
    pub prompt: String,
    pub supersedes: Vec<MessageId>,
    pub intent: DeliveryIntent,
}

impl PreparedDelivery {
    /// The session the wake goes to now: the holder of the agent the target
    /// names, read at the call, or `None` once the agent has ended. Its handle
    /// is the holder's own name while the holder is still starting.
    pub fn holder(&self) -> Result<Option<TaskTarget>> {
        catalog::delivery_holder(&self.task_root, &self.target)
    }
}

#[derive(Clone, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "one plan per fire, moved once to its caller; boxing the finished record buys nothing"
)]
pub enum TaskFirePlan {
    Done(TaskFireFinished),
    /// The checkout's memory says a run was already started there; nothing is recorded.
    AlreadyLaunched,
    /// A drained trigger went back to wait for the run still holding the scope.
    Waiting,
    Deliver(PreparedDelivery),
}

#[derive(Debug)]
pub enum TaskFireEffect {
    Delivered(crate::ids::MessageId),
    TargetGone,
}

struct FireContext {
    root: PathBuf,
    scope: Option<FireScope>,
}

struct FireScope {
    scope_runtime: RuntimePaths,
    scope_state: StatePaths,
    managed_launch: ManagedLaunchState,
    /// The room's [`task_login`] for `kind`.
    login: Option<ProviderLogin>,
}

impl FireScope {
    fn new(
        scope_runtime: RuntimePaths,
        scope_state: StatePaths,
        login: Option<ProviderLogin>,
    ) -> Self {
        Self {
            login,
            scope_runtime,
            scope_state,
            managed_launch: ManagedLaunchState::Unsupported,
        }
    }

    fn login_key(&self) -> Option<crate::ids::LoginKey> {
        self.login.as_ref().map(ProviderLogin::key)
    }
}

impl FireContext {
    fn resolve(entry: &TaskEntry, action: TaskAction) -> Result<Self> {
        let root = entry.resolved_root();
        let TaskAction::Deliver(target) = action else {
            return Ok(Self { root, scope: None });
        };
        let project_root = WorkspaceResolver::persisted_project_root(&root)?;
        let runtime = RuntimePaths::for_project_root(&project_root)?;
        let login = task_login(&LaunchLogin::RoomDefault, target.kind.as_str(), &runtime)?;
        let mut scope =
            FireScope::new(runtime, StatePaths::for_project_root(&project_root)?, login);
        scope.managed_launch = unresolved_managed_state(&root, &target.kind);
        Ok(Self {
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

/// The lateness a `fire-at` fire records: an on-time fire lands within one 60 s clock tick plus
/// spawn time, so two minutes or more means no clock saw the instant.
const LATE_FIRE_MS: u64 = 120_000;

/// The room a fire tree's signals go to, when its store exists.
fn fire_tree_room(root: &Path) -> Option<(crate::Store, crate::ResolvedWorkspace)> {
    let paths = StatePaths::for_project_root(root).ok()?;
    let runtime = RuntimePaths::for_state(&paths).ok()?;
    let store = crate::Store::open_existing(paths, runtime)?;
    let workspace = crate::workspace::WorkspaceResolver::resolve(root, None).ok()?;
    Some((store, workspace))
}

/// The worktree one fire of a `fresh-worktree` task cut as its run's scope.
struct FireTree {
    name: String,
    path: PathBuf,
    base_fallback: Option<String>,
    /// A run took it over, so the fire never force-removes it; the run's report settles it.
    handed_over: bool,
}

/// Force-remove the fresh tree a loop fire cut, then retire its sessions and archive its channel.
fn remove_fire_tree(
    task: &str,
    root: &Path,
    config: &crate::config::WorktreeConfig,
    name: &str,
    path: &Path,
) {
    let memo = crate::worktree::LandedMemo::load(root);
    match crate::worktree::remove(
        root,
        config,
        name,
        path,
        true,
        &crate::worktree::ProtectionSet::default(),
        &memo,
    ) {
        Ok(removed) => {
            if let Some((store, workspace)) = fire_tree_room(root) {
                let retirement = crate::worktree::retire_removal(
                    &store,
                    &removed,
                    crate::worktree::WORKTREE_REMOVED_ARCHIVE_REASON,
                    &workspace.session_name,
                );
                for (step, result) in [
                    ("retire its sessions", retirement.session_retirement),
                    ("archive its channel", retirement.message_archival),
                ] {
                    if let Err(error) = result {
                        tracing::warn!(task, worktree = %path.display(), %error, "failed to {step} after removing the fire's unused worktree");
                    }
                }
            }
        }
        Err(error) => {
            tracing::warn!(task, worktree = %path.display(), error = %format!("{error:#}"), "failed to remove the fire's unused worktree");
        }
    }
    memo.save();
}

/// Settle the fresh tree a reported run worked in, through a detached `rimz`: collection reports
/// under the workspace lock, which the removal's store writes take. Whatever the command did,
/// `worktree cleanup` keeps a tree that holds work or an agent its command launched there, and
/// removes it otherwise.
pub(super) fn settle_fire_tree(run: &crate::store::workflow::RunRecord) {
    let Some(task) = run.task.as_deref().filter(|_| run.fire_tree) else {
        return;
    };
    let path = &run.scope.path;
    match crate::worktree::read_marker_for_worktree(path) {
        Ok(Some(_)) => {}
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(task, worktree = %path.display(), %error, "fire tree marker unreadable; tree kept");
            return;
        }
    }
    let args: Vec<std::ffi::OsString> = vec![
        "--root".into(),
        run.project_root.clone().into_os_string(),
        "worktree".into(),
        "cleanup".into(),
        path.clone().into_os_string(),
        "--non-interactive".into(),
    ];
    let spawned = StatePaths::for_project_root(&run.project_root)
        .and_then(|paths| RuntimePaths::for_state(&paths))
        .map_err(anyhow::Error::from)
        .and_then(|runtime| {
            crate::child_process::spawn_detached_rimz(&runtime, &args, "loop-fire-tree")
                .map_err(anyhow::Error::from)
        });
    if let Err(error) = spawned {
        tracing::warn!(task, worktree = %path.display(), error = %format!("{error:#}"), "fire tree cleanup not started; gc reclaims a clean tree");
    }
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
    now: Timestamp,
    config: Arc<MachineConfig>,
    signal: Option<TriggerSignal>,
    arrived_at: Option<Timestamp>,
    from_arrived_at: Option<Timestamp>,
    condition: Option<crate::harness::when::ConditionEvidence>,
    checkout: Option<PathBuf>,
    /// The fresh tree of a `fresh-worktree` fire. It is never `checkout`, which
    /// keys the run lock, capacity and declines at the project root.
    fire_tree: Option<FireTree>,
    started: Instant,
    run_lock: Option<RunLockGuard>,
    record_hold: Option<CheckSignalGuard>,
    backlog_paths: Option<StatePaths>,
    absorbed: u64,
    run_lock_path: fn(&str, &TaskEntry) -> Result<PathBuf>,
    /// How far past its `fire-at` instant a scheduled one-shot started, taken at construction.
    late_ms: Option<u64>,
    /// A watch's verdict, recorded as the fire's check.
    watch_check: Option<CheckRecord>,
    pending: Option<TaskTarget>,
    finished: bool,
    /// This fire took its trigger from the backlog.
    draining: bool,
    /// A fresh queued arrival that served the queue's head under the lease.
    queued_arrival: Option<WaitingTrigger>,
    /// This fire started a run or found one live, whose report drains the backlog.
    saw_run: bool,
    /// The detached advance of the run this fire started, which its receipt waits on.
    advance: Option<crate::harness::workflow::Advance>,
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
        now: Timestamp,
        config: Arc<MachineConfig>,
        signal: Option<TriggerSignal>,
        started: Instant,
    ) -> Result<Self> {
        let name = name.into();
        let action = task.action().cloned().map_err(Clone::clone)?;
        let entry = task.entry().clone();
        let ephemeral = task.is_ephemeral();
        let late_ms = entry
            .fire_at
            .filter(|_| mode == LoopRunMode::Scheduled)
            .and_then(|at| u64::try_from(now.duration_since(at).as_millis()).ok())
            .filter(|late| *late >= LATE_FIRE_MS);
        Ok(Self {
            name,
            task,
            entry,
            catalog,
            action: Some(action),
            ephemeral,
            context: None,
            mode,
            now,
            config,
            signal,
            arrived_at: None,
            from_arrived_at: None,
            condition: None,
            checkout: None,
            fire_tree: None,
            started,
            run_lock: None,
            record_hold: None,
            backlog_paths: None,
            absorbed: 0,
            run_lock_path,
            late_ms,
            watch_check: None,
            pending: None,
            finished: false,
            draining: false,
            queued_arrival: None,
            saw_run: false,
            advance: None,
        })
    }

    /// The detached advance of the run this fire started, if it started one.
    pub fn advance(&self) -> Option<&crate::harness::workflow::Advance> {
        self.advance.as_ref()
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
        let path = self.fire_run_lock_path()?;
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
        self.draining = true;
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
        let Some(paths) = self.backlog_paths.as_ref().filter(|_| !self.saw_run) else {
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

    pub fn with_condition(
        mut self,
        condition: Option<crate::harness::when::ConditionEvidence>,
    ) -> Self {
        self.condition = condition;
        self
    }

    pub fn with_checkout(mut self, checkout: Option<PathBuf>) -> Self {
        self.checkout = checkout;
        self
    }

    fn launch_checkout(&self) -> PathBuf {
        self.checkout
            .clone()
            .unwrap_or_else(|| self.entry.run_dir())
    }

    /// The run's scope: the fire's fresh tree, else the launch checkout.
    fn fire_dir(&self) -> PathBuf {
        self.fire_tree
            .as_ref()
            .map_or_else(|| self.launch_checkout(), |tree| tree.path.clone())
    }

    /// Cut a `fresh-worktree` fire's tree from trunk and announce it when the room's
    /// store exists.
    fn prepare_fire_tree(&mut self) -> Result<()> {
        let Some(prefix) = self.entry.fresh_worktree.as_deref() else {
            return Ok(());
        };
        if self.fire_tree.is_some() {
            return Ok(());
        }
        let root = self.entry.resolved_root();
        let created = crate::worktree::create_for_loop_fire(
            &root,
            &self.config.agents.worktree,
            prefix,
            &self.name,
            self.now,
        )
        .with_context(|| format!("creating the fresh worktree for loop task `{}`", self.name))?;
        self.fire_tree = Some(FireTree {
            name: created.marker.name.clone(),
            path: created.marker.worktree_path.clone(),
            base_fallback: created.trunk_fetch.as_ref().map(ToString::to_string),
            handed_over: false,
        });
        match fire_tree_room(&root) {
            Some((store, workspace)) => {
                crate::worktree::emit_worktree_created(&store, &workspace, &created.marker);
            }
            None => {
                tracing::debug!(task = %self.name, "skipping worktree signal: store unavailable")
            }
        }
        Ok(())
    }

    /// From `create` on, the run owns the tree and its report settles it.
    fn hand_over_fire_tree(&mut self) {
        if let Some(tree) = self.fire_tree.as_mut() {
            tree.handed_over = true;
        }
    }

    /// Remove a fire's tree that no run took over, forced: nothing ran in it.
    fn remove_unlaunched_fire_tree(&mut self) {
        let Some(tree) = self.fire_tree.take_if(|tree| !tree.handed_over) else {
            return;
        };
        remove_fire_tree(
            &self.name,
            &self.entry.resolved_root(),
            &self.config.agents.worktree,
            &tree.name,
            &tree.path,
        );
    }

    pub fn prepare(
        &mut self,
        before_launch: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<TaskFirePlan> {
        match self.task.action().map_err(Clone::clone)?.clone() {
            TaskAction::Command(argv) => self.prepare_command(&argv, before_launch),
            TaskAction::Deliver(target) => self.prepare_wait(target),
        }
    }

    /// An unguarded wait: scope gates, the lease, the deadline and a watch's verdict, then the
    /// delivery to its living session.
    fn prepare_wait(&mut self, target: TaskTarget) -> Result<TaskFirePlan> {
        if let Some(done) = self.prepare_scope_gates()? {
            return Ok(TaskFirePlan::Done(done));
        }
        if let Some(done) = self.prepare_run_lock()? {
            return Ok(TaskFirePlan::Done(done));
        }
        if let Some(done) = self.prepare_deadline()? {
            return Ok(TaskFirePlan::Done(done));
        }
        if let Some(done) = self.prepare_watch()? {
            return Ok(TaskFirePlan::Done(done));
        }
        self.prepare_delivery(target)
    }

    /// A watch's verdict arrives with its fire: a check-in passes on, and a terminal verdict
    /// passes only on the polarity `on` names.
    fn prepare_watch(&mut self) -> Result<Option<TaskFireFinished>> {
        let Some(watch) = self.watch_spec().and(self.watch_outcome()) else {
            return Ok(None);
        };
        let outcome = watch.to_check_outcome();
        let terminal = watch.verdict.is_terminal();
        let duration_ms = watch.verdict.elapsed_ms();
        let mut record = check_record(&outcome);
        record.duration_ms = Some(duration_ms);
        record.output_path.clone_from(&watch.output_path);
        self.watch_check = Some(record);
        if !terminal {
            return Ok(None);
        }
        let interrupted = outcome.interrupted();
        if !interrupted && polarity_fires(self.entry.on, &outcome) {
            return Ok(None);
        }
        if self.mode == LoopRunMode::Scheduled && !interrupted {
            self.remove_schedule()?;
        }
        let result = if interrupted {
            LoopRunResult::Canceled
        } else {
            LoopRunResult::CheckSkipped
        };
        Ok(Some(self.record_terminal_with(
            result,
            LoopRunPresentation {
                check_duration_ms: Some(duration_ms),
                exit_code: interrupted.then_some(130),
                ..LoopRunPresentation::default()
            },
            TaskFireNotice::None,
            None,
            |run| run.error = interrupted.then(|| "check interrupted".to_owned()),
        )))
    }

    /// A command fire starts the workflow run of its one command check, behind the ladder:
    /// deadline and memory, the admission lease, then overlap against the live run, which
    /// `create` refuses.
    fn prepare_command(
        &mut self,
        argv: &[String],
        before_launch: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<TaskFirePlan> {
        if let Some(done) = self.prepare_watch()? {
            return Ok(TaskFirePlan::Done(done));
        }
        if let Some(done) = self.prepare_deadline()? {
            return Ok(TaskFirePlan::Done(done));
        }
        if self.workflow_already_launched()? {
            self.finished = true;
            return Ok(TaskFirePlan::AlreadyLaunched);
        }
        let definition = self.command_definition(argv)?;
        if let Some(done) = self.prepare_run_lock()? {
            return Ok(TaskFirePlan::Done(done));
        }
        let root = self.entry.resolved_root();
        before_launch(&root)?;
        // Another fire may have started a run while this one waited for the room or lease.
        if self.workflow_already_launched()? {
            self.finished = true;
            return Ok(TaskFirePlan::AlreadyLaunched);
        }
        let state = StatePaths::for_project_root(&root)?;
        if let Some(done) = self.reconcile_backlog()? {
            return Ok(TaskFirePlan::Done(done));
        }
        let fresh = self.entry.fresh_worktree.is_some();
        if fresh && let Some(blocker) = self.live_task_run(&state)? {
            return self.prepare_overlap(&blocker.id, None);
        }
        // The last rung: a fresh task's run works in a tree cut for this fire alone.
        self.prepare_fire_tree()?;
        // The run lock and the reconcile may have switched the trigger this fire serves; the
        // run's prompt, which its command reads as `RIMZ_LOOP_TRIGGER`, names the one served.
        let trigger = self.effect_prompt(String::new());
        let prompt = || Ok((!trigger.is_empty()).then(|| trigger.clone()));
        let definition = crate::harness::workflow::DefinitionSource::Inline {
            task: self.name.clone(),
            definition,
            fire_tree: fresh,
            surplus: None,
            surplus_after: None,
            fire: Some(self.run_fire()),
            fire_facts: Some(self.fire_facts()),
        };
        let checkout = self.fire_dir();
        std::fs::metadata(&checkout)
            .with_context(|| format!("task checkout {}", checkout.display()))?;
        let created = crate::harness::workflow::create(
            &state,
            definition,
            crate::harness::workflow::ScopeRoots {
                checkout,
                project: root,
            },
            prompt,
            self.mode == LoopRunMode::Manual,
            crate::harness::workflow::StartArgs::default(),
        );
        let run = match created {
            Ok((run, advance)) => {
                self.advance = Some(advance);
                run
            }
            Err(crate::harness::workflow::WorkflowErr::Exists {
                run, task, kind, ..
            }) => {
                let repair = (task.as_deref() == Some(self.name.as_str()))
                    .then(|| self.record_workflow_launch(&state, &run, kind, self.now, false))
                    .flatten();
                return self.prepare_overlap(&run, repair);
            }
            Err(crate::harness::workflow::WorkflowErr::NoRoom(root)) => {
                return Ok(TaskFirePlan::Done(self.record_gate(
                    LoopRunResult::StartFailed,
                    format!(
                        "no room at {}; run `rimz start` there first",
                        root.display()
                    ),
                )));
            }
            Err(error) => return Err(error.into()),
        };
        self.saw_run = true;
        // The run owns its tree now; its report removes it, or keeps it while it holds work.
        self.hand_over_fire_tree();
        let worktree = self.fire_tree.as_ref().map(|tree| run_log::FireWorktree {
            path: tree.path.clone(),
            base_fallback: tree.base_fallback.clone(),
        });
        let error = self.record_workflow_launch(
            &state,
            &run.id,
            run.scope.kind,
            run.created_at,
            self.mode == LoopRunMode::Manual,
        );
        // A poll-until row lives until a run acts, which the run's report decides.
        if self.entry.deadline.is_none() {
            self.consume_ephemeral()?;
        }
        // The run's report is the fire's one history row; this launch only stamps its proof.
        Ok(TaskFirePlan::Done(self.record_terminal_with(
            LoopRunResult::Launched,
            LoopRunPresentation::default(),
            TaskFireNotice::None,
            None,
            |record| {
                record.run_id = Some(run.id.to_string());
                record.worktree = worktree;
                record.error = error;
            },
        )))
    }

    /// What this fire would log at its launch, sealed into its run for the run's one report row:
    /// its trigger, waits, lock tenure, fresh tree and kind.
    fn fire_facts(&self) -> serde_json::Value {
        let mut record = self.terminal_record(LoopRunResult::Launched);
        record.run_lock = self.run_lock.as_ref().map(|guard| guard.info);
        record.worktree = self.fire_tree.as_ref().map(|tree| run_log::FireWorktree {
            path: tree.path.clone(),
            base_fallback: tree.base_fallback.clone(),
        });
        run_log::fire_facts(&record)
    }

    /// What this fire tells its run's briefs, frozen into the run at its creation: the task, and
    /// for a scheduled fire the trigger; a manual fire's brief names the command instead. The
    /// task's booking chain rides along for the agents the run launches.
    fn run_fire(&self) -> RunFire {
        let task = self.name.clone();
        let booked_by = self.task.entry().booked_by.clone();
        if self.mode == LoopRunMode::Manual {
            return RunFire::Manual { task, booked_by };
        }
        RunFire::Scheduled {
            task,
            trigger: super::trigger_clause(&self.task),
            booked_by,
        }
    }

    /// The task's live run keyed on its firing checkout, read before a fresh tree is cut so
    /// a fire that would overlap cuts none; `create` still decides under the workspace lock.
    fn live_task_run(
        &self,
        state: &StatePaths,
    ) -> Result<Option<crate::store::workflow::RunRecord>> {
        let checkout = self.launch_checkout().canonicalize()?;
        Ok(crate::store::workflow::live_runs(state)?
            .into_iter()
            .find(|run| run.started_by(&self.name) && run.firing_checkout() == checkout))
    }

    /// The live run `create` refused on: `skip` records an overlap naming it; `coalesce`,
    /// `queue` and `replace` keep the trigger waiting for its report, and `replace` cancels it.
    /// A drained trigger whose blocker still runs goes back to the head of its queue.
    fn prepare_overlap(
        &mut self,
        run: &crate::ids::WorkflowRunId,
        repair: Option<String>,
    ) -> Result<TaskFirePlan> {
        self.saw_run = true;
        let policy = self.entry.overlap.unwrap_or_default();
        let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
        if self.draining {
            let mut backlog = LockedBacklog::acquire(&paths)?;
            let waiting = backlog.record.tasks.entry(self.name.clone()).or_default();
            waiting.triggers.push_front(self.waiting_trigger());
            backlog.save()?;
            self.finished = true;
            return Ok(TaskFirePlan::Waiting);
        }
        let repair = repair
            .map(|repair| format!("; {repair}"))
            .unwrap_or_default();
        if self.mode != LoopRunMode::Scheduled || policy == OverlapPolicy::Skip {
            let detail = format!("run {run} still active — skipped");
            return Ok(TaskFirePlan::Done(self.record_terminal_with(
                LoopRunResult::Overlapped,
                LoopRunPresentation::default(),
                TaskFireNotice::Overlap {
                    detail: Some(detail.clone()),
                },
                None,
                |record| {
                    record.blocking_run = Some(run.clone());
                    record.error = Some(format!("{detail}{repair}"));
                },
            )));
        }
        let limit = self.entry.overlap_limit.unwrap_or(10);
        let mut backlog = LockedBacklog::acquire(&paths)?;
        let waiting = backlog.record.tasks.entry(self.name.clone()).or_default();
        // The head this arrival served waits again first, and the arrival queues behind it.
        if let Some(arrival) = self.queued_arrival.take() {
            waiting.triggers.retain(|trigger| *trigger != arrival);
            waiting.triggers.push_front(self.waiting_trigger());
            self.serve_trigger(arrival);
        }
        let enqueued = waiting.enqueue(policy, limit, self.waiting_trigger());
        let count = waiting.triggers.len();
        backlog.save()?;
        drop(backlog);
        let suffix = if !enqueued {
            format!("queue full ({count}/{limit}), skipped")
        } else if policy == OverlapPolicy::Queue {
            format!("waiting (queue {count}/{limit})")
        } else if policy == OverlapPolicy::Replace {
            "canceling it for this trigger".to_owned()
        } else {
            "waiting".to_owned()
        };
        let detail = format!("run {run} still active — {suffix}");
        let finished = self.record_terminal_with(
            if enqueued {
                LoopRunResult::Deferred
            } else {
                LoopRunResult::Overlapped
            },
            LoopRunPresentation::default(),
            TaskFireNotice::Overlap {
                detail: Some(detail.clone()),
            },
            None,
            |record| {
                record.blocking_run = Some(run.clone());
                record.error = Some(format!("{detail}{repair}"));
            },
        );
        // The deferred row and this fire's lock release come first: the canceled run's report
        // drains the waiting trigger, and a drainer that finds this fire's lock held starts none.
        if enqueued && policy == OverlapPolicy::Replace {
            self.cancel_blocker(&paths, run);
        }
        Ok(TaskFirePlan::Done(finished))
    }

    /// Cancel a replaced run in a detached helper; its report drains the waiting trigger once
    /// the run is terminal, so a cancel that returns early never starts the replacement.
    fn cancel_blocker(&self, paths: &StatePaths, run: &crate::ids::WorkflowRunId) {
        let mut args: Vec<std::ffi::OsString> =
            vec!["--root".into(), self.entry.resolved_root().into_os_string()];
        args.extend(crate::harness::workflow::cancel_args(
            run,
            "replaced by a newer trigger",
        ));
        let spawned = RuntimePaths::for_state(paths)
            .map_err(anyhow::Error::from)
            .and_then(|runtime| {
                crate::child_process::spawn_detached_rimz(&runtime, &args, "loop-replace")
                    .map_err(anyhow::Error::from)
            });
        if let Err(error) = spawned {
            tracing::warn!(task = %self.name, %run, error = %format!("{error:#}"), "replacement could not cancel the active run; trigger remains waiting");
        }
    }

    /// The definition a fire starts: a deployed directory, or the task's compiled graph with
    /// its assets read now and the evidence of the trigger the fire serves now in its prompt.
    /// The one-check workflow a fire of `argv` runs: the command exits 0 and the run is done,
    /// else it fails. `sh` only execs the argv, every word single-quoted, so each reaches the
    /// process as written; a `command` step would confine every word holding `/` to the
    /// definition's directory.
    pub(in crate::harness) fn command_definition(
        &self,
        argv: &[String],
    ) -> Result<crate::harness::workflow::InlineDefinition> {
        command_definition(&self.name, &self.entry, argv)
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
            &workflows,
        )
        .is_some())
    }

    /// A booked one-off agent or team launch, whose consumed sidebar entry the bookings lane
    /// rebuilds from this launch memory once its row is gone.
    fn is_booked_launch(&self) -> bool {
        self.mode == LoopRunMode::Scheduled
            && self.task.kind() == TaskKind::OneOff
            && self.entry.wait.is_none()
            && !self.entry.booked_by.is_empty()
            && read_fire_launch(&self.entry.command).is_some()
    }

    fn record_workflow_launch(
        &self,
        paths: &StatePaths,
        run: &crate::ids::WorkflowRunId,
        scope: crate::store::workflow::ScopeKind,
        at: Timestamp,
        replace_project: bool,
    ) -> Option<String> {
        if !self.entry.dedupes_checkout() && !self.is_booked_launch() {
            return None;
        }
        let checkout = match scope {
            crate::store::workflow::ScopeKind::Project => self.entry.resolved_root(),
            crate::store::workflow::ScopeKind::Worktree => self.launch_checkout(),
        };
        let record = super::launch_ledger::WorkflowLaunchRecord {
            at,
            run: Some(run.clone()),
            scope,
            leader: None,
            destination: None,
        };
        #[cfg(feature = "testkit")]
        crate::testkit::rendezvous("RIMZ_TEST_LOOP_BEFORE_LAUNCH_MEMORY");
        let Err(error) = super::launch_ledger_store::record_workflow(
            paths,
            &self.name,
            &checkout,
            record,
            replace_project,
        ) else {
            // The run advances before this write, so its collection may already have forgotten it.
            if let Err(error) = run_log::forget_collected_launch(paths, &self.name, run) {
                tracing::warn!(task = %self.name, %run, %error, "launch memory kept");
            }
            return None;
        };
        tracing::warn!(
            task = %self.name, %run, %error, "recording workflow loop launch failed"
        );
        Some(error.to_string())
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

    fn prepare_scope_gates(&mut self) -> Result<Option<TaskFireFinished>> {
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

    /// The first scope gate that refuses a delivery, in ladder order; every one reads the
    /// room's login.
    fn scope_refusal(&self, scope: &FireScope) -> Option<(LoopRunResult, String)> {
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
        None
    }

    /// The admission lease: it serializes fires of one task, while the live run is what a
    /// later fire overlaps. A fire held off by another admission waits under the policy.
    fn prepare_run_lock(&mut self) -> Result<Option<TaskFireFinished>> {
        if self.run_lock.is_some() {
            return Ok(None);
        }
        let path = self.fire_run_lock_path()?;
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
                        let arrival = self.waiting_trigger();
                        waiting.enqueue(policy, usize::MAX, arrival.clone());
                        // The fresh arrival was just enqueued while the queue was locked.
                        let trigger = waiting.triggers.pop_front().expect("arrival enqueued");
                        backlog.save()?;
                        if policy == OverlapPolicy::Queue && trigger != arrival {
                            self.queued_arrival = Some(arrival);
                        }
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
                    backlog.save()?;
                    let suffix = if !enqueued {
                        format!("queue full ({count}/{limit}), skipped")
                    } else if policy == OverlapPolicy::Queue {
                        format!("waiting (queue {count}/{limit})")
                    } else {
                        "waiting".to_owned()
                    };
                    return Ok(Some(self.record_terminal_with(
                        if enqueued {
                            LoopRunResult::Deferred
                        } else {
                            LoopRunResult::Overlapped
                        },
                        LoopRunPresentation::default(),
                        TaskFireNotice::Overlap {
                            detail: Some(overlap_detail(info, &suffix)),
                        },
                        None,
                        |record| {
                            record.holder = info;
                            record.error = Some(format!("previous run still active — {suffix}"));
                        },
                    )));
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

    /// The lease file, taken before the fire births the room it runs in.
    fn fire_run_lock_path(&mut self) -> Result<PathBuf> {
        let checkout = self
            .entry
            .each_worktree
            .then(|| WorkspaceId::from_project_root(&self.launch_checkout()));
        let file = run_lock_file_name(&self.name, checkout.as_ref());
        match (self.run_lock_path)(&file, &self.entry) {
            Ok(path) => Ok(path),
            Err(err)
                if matches!(
                    err.downcast_ref::<crate::workspace::WorkspaceErr>(),
                    Some(crate::workspace::WorkspaceErr::NoRoom { .. })
                ) =>
            {
                let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
                Ok(RuntimePaths::for_state(&paths)?.lock_path(&file))
            }
            Err(err) => Err(err),
        }
    }

    fn record_overlap(&mut self, info: Option<RunLockInfo>) -> TaskFireFinished {
        self.record_terminal_with(
            LoopRunResult::Overlapped,
            LoopRunPresentation::default(),
            TaskFireNotice::Overlap {
                detail: info.map(|info| overlap_detail(Some(info), "skipped")),
            },
            None,
            |record| {
                record.holder = info;
                record.error = Some("previous run still active — skipped".to_owned());
            },
        )
    }

    pub fn finish(&mut self, effect: TaskFireEffect) -> Result<TaskFireFinished> {
        let target = self
            .pending
            .take()
            .context("loop task has no prepared effect to finish")?;
        match effect {
            TaskFireEffect::Delivered(message_id) => {
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
            TaskFireEffect::TargetGone => {
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
        }
    }

    pub fn finish_error(&mut self, err: &anyhow::Error) -> TaskFireFinished {
        if self.pending.take().is_some()
            && let Err(consume) = self.consume_ephemeral()
        {
            tracing::warn!(task = %self.name, error = %format!("{consume:#}"), "failed to consume errored one-shot delivery");
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

    fn prepare_delivery(&mut self, target: TaskTarget) -> Result<TaskFirePlan> {
        if catalog::delivery_holder(&self.entry.resolved_root(), &target)?.is_none() {
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
        self.pending = Some(target.clone());
        Ok(TaskFirePlan::Deliver(PreparedDelivery {
            root: self.context_root()?,
            task_root: self.entry.resolved_root(),
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
        Ok(self.effect_prompt(self.entry.prompt.clone().unwrap_or_default()))
    }

    /// `body` with the evidence of what fired this run.
    fn effect_prompt(&self, mut body: String) -> String {
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
        if !self.entry.command.is_empty() && self.entry.watch.is_none() {
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
        record.check.clone_from(&self.watch_check);
        record.late_ms = self.late_ms;
        if self.entry.each_worktree {
            record.checkout = Some(self.launch_checkout());
        }
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
        // A recorded scheduled attempt ends a `fire-at` row, whatever its
        // result; a dispatch that never records is retried by the clock pass.
        if self.mode == LoopRunMode::Scheduled
            && self.entry.fire_at.is_some()
            && let Err(err) = self.remove_schedule()
        {
            tracing::warn!(task = %self.name, error = %format!("{err:#}"), "failed to remove a fired fire-at task");
        }
        let result = record.result;
        let transition = self.record_and_release(&mut record);
        if record.result != result {
            presentation.exit_code = record.result.spawn_exit_code();
        }
        self.finished = true;
        TaskFireFinished {
            record,
            presentation,
            transition,
            notice,
        }
    }

    fn record_and_release(&mut self, record: &mut LoopRunRecord) -> RunTransition {
        let Some(holder) = self.run_lock.as_ref().map(|guard| guard.info) else {
            let transition = run_log::record_transition(&self.task, record);
            self.record_hold = None;
            return transition;
        };
        record.run_lock = Some(holder);
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
            // A run this fire saw drains the backlog from its own report.
            let release = if self.saw_run {
                Ok(())
            } else {
                spawn_backlog_drain(&paths, &root, &self.name, &backlog.record)
            };
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
        self.remove_unlaunched_fire_tree();
        if let Err(error) = self.release_run_lock() {
            tracing::warn!(task = %self.name, error = %format!("{error:#}"), "failed to release loop run and start its backlog drain");
        }
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn overlap_detail(info: Option<RunLockInfo>, suffix: &str) -> String {
    let holder = info.map_or_else(
        || "previous run still active".to_owned(),
        |info| {
            format!(
                "previous run still active (pid {}, started {})",
                info.pid,
                crate::theme::fmt::rel_age(info.started_at, Timestamp::now())
            )
        },
    );
    format!("{holder} — {suffix}")
}

/// Why `rimz loop add` refuses a window trigger; each display carries its fix.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WindowRefusal {
    #[error("a window trigger reads a provider's windows; name it with --provider <kind>")]
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
    let Ok(expr) = crate::harness::when::WhenExpr::parse(when) else {
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
    })
}

/// Resolve `--after-reset <span>`: refused as a window term is, then for a
/// lifted window and a window without a reset.
pub fn after_reset(
    span: WindowSpan,
    kind: Option<&str>,
    login: Option<&crate::ids::LoginKey>,
    root: &Path,
    runtime: &RuntimePaths,
    now: Timestamp,
) -> Result<AfterReset, WindowRefusal> {
    let WindowAtAdd { kind, span, window } = window_at_add(kind, span, login, root, runtime, now)?;
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
    let started = !window.not_started(now);
    Ok(AfterReset {
        left: crate::harness::when::percent_left(&window),
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
}

/// The run of task `name` in flight under `root`. The run lock decides; the run
/// record is read only while the lock is held, so a stale record is no run.
pub fn in_flight_run(name: &str, root: &Path) -> Result<Option<InFlightRun>> {
    let Some((_, holder)) = held_run_lock(name, root)? else {
        return Ok(None);
    };
    let (_, project_root) = stop_workspace(root)?;
    let run = newest_active_run(&StatePaths::for_project_root(&project_root)?, name)?;
    Ok(Some(InFlightRun { holder, run }))
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
            cause: StopCause::Stop,
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

    if wait_for_run_lock_release_path(&lock, holder, STOP_GRACE)? {
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
        // A run stopped while it holds its admitted ticket dies with it. The
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
        crate::harness::throttle::reap();
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
    record.run_lock = Some(info);
    set_stopped_result(&mut record, mark.cause);
    record.run_id = run.map(|record| record.run_id.to_string());
    backlog.save()?;
    run_log::record_stopped(root, &record, kind);
    if let Err(error) = spawn_backlog_drain(&paths, root, name, &backlog.record) {
        tracing::warn!(task = name, %error, "failed to start loop backlog drain");
    }
    Ok(RunTransition::Recorded)
}

/// A stopped holder's row is a strike-neutral `canceled`; a `replace` mark an older RimZ left
/// keeps its reason.
fn set_stopped_result(record: &mut LoopRunRecord, cause: StopCause) {
    record.result = LoopRunResult::Canceled;
    record.error = Some(
        match cause {
            StopCause::Replace => "replaced by a newer trigger",
            StopCause::Stop => "stopped by rimz loop stop",
        }
        .to_owned(),
    );
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

/// The task name a bare run lock file carries, `run_lock_file_name`'s inverse.
fn run_lock_stem(file: &str) -> Option<&str> {
    let frame = run_lock_file_name("\0", None);
    let (prefix, suffix) = frame.split_once('\0')?;
    file.strip_prefix(prefix)?
        .strip_suffix(suffix)
        .filter(|stem| !stem.is_empty())
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

/// The node a command fire's run starts at and ends on.
pub const COMMAND_NODE: &str = "command";

/// The inline definition one fire of task `name` runs: a `test` check whose script execs `argv`
/// under the row's `timeout`, routing a zero exit to `@done` and anything else to `@failed`.
pub fn command_definition(
    name: &str,
    entry: &TaskEntry,
    argv: &[String],
) -> Result<crate::harness::workflow::InlineDefinition> {
    let timeout = entry
        .timeout
        .as_deref()
        .with_context(|| format!("loop task `{name}` has a command without `timeout`"))?;
    let on = toml::Table::from_iter(
        [
            ("pass", "@done"),
            ("fail", "@failed"),
            ("timeout", "@failed"),
            ("died", "@failed"),
        ]
        .map(|(outcome, target)| (outcome.to_owned(), toml::Value::from(target))),
    );
    let check = toml::Table::from_iter([
        ("summary".to_owned(), "the task's command".into()),
        (
            "shell".to_owned(),
            format!("exec {}", super::command_line(argv)).into(),
        ),
        ("timeout".to_owned(), timeout.into()),
        ("on".to_owned(), on.into()),
    ]);
    let workflow = toml::Table::from_iter([
        ("summary".to_owned(), format!("loop task {name}").into()),
        ("scope".to_owned(), "worktree".into()),
        ("start".to_owned(), COMMAND_NODE.into()),
    ]);
    let spec = toml::Table::from_iter([
        ("workflow".to_owned(), workflow.into()),
        (
            "test".to_owned(),
            toml::Table::from_iter([(COMMAND_NODE.to_owned(), check.into())]).into(),
        ),
    ]);
    Ok(crate::harness::workflow::InlineDefinition {
        text: toml::to_string(&spec)?,
        assets: BTreeMap::new(),
    })
}

pub fn check_record(outcome: &CheckOutcome) -> CheckRecord {
    CheckRecord {
        code: outcome.code(),
        timed_out: outcome.timed_out(),
        output: outcome.output().to_owned(),
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

fn polarity_fires(on: Option<CheckOn>, outcome: &CheckOutcome) -> bool {
    match on.unwrap_or_default() {
        CheckOn::Fail => !outcome.passed(),
        CheckOn::Success => outcome.passed(),
        CheckOn::Any => true,
    }
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

#[cfg(test)]
mod tests;
