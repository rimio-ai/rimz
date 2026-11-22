//! The loop-fire transaction, from ordered gates through one durable history transition.
//!
//! A [`TaskFire`] is a shared core plus one effect per task action, holding only its kind's
//! state: `shell` starts a workflow run, `launch` spawns a launch child, and `deliver` prepares
//! a delivery the CLI executes. The core owns the shared gates (watch verdict, deadline, launch
//! memory, run lock, backlog reconcile, overlap), the fire mark, task consumption and the one
//! terminal record. The run lock's holder side and the stop ladder are `run_control`'s.

mod deliver;
mod launch;
mod prompt;
mod shell;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;

use self::deliver::Delivery;
use self::launch::Launch;
use self::shell::Shell;

use super::{
    backlog::{self, WaitingTrigger},
    backlog_store::{self, LockedBacklog},
    fire::deadline_expired_at,
    fires,
    launches::room_agents,
    run_control::{
        RunLockAttempt, RunLockGuard, RunLockInfo, acquire_run_lock_file, run_lock_path,
        set_stopped_result, spawn_backlog_drain,
    },
    run_lock::file_name as run_lock_file_name,
    run_log::check_record,
    shell_task::command_definition,
    task_record::{self, FireMark},
    task_record_store,
};
use crate::agents::ProviderLogin;
use crate::config::LaunchKind;
use crate::config::{CheckOn, MachineConfig, OverlapPolicy, TaskEntry, TaskTarget, WatchSpec};
use crate::disk::paths::{RuntimePaths, StatePaths, logs_dir};
use crate::harness::check_run::{CheckOutcome, CheckSignalGuard};
use crate::harness::schedule::catalog::{self, LoadedTask, TaskCatalog};
use crate::harness::schedule::run_log::{
    self, CheckRecord, LoopRunMode, LoopRunPresentation, LoopRunRecord, LoopRunResult,
    RunTransition, SignalRecord,
};
use crate::harness::schedule::{TaskAction, TaskLaunch, Trigger};
use crate::harness::signal::{Signal as TriggerSignal, WatchOutcome};
use crate::harness::task_login::task_login;
use crate::ids::{FireId, TaskId};
use crate::ids::{MessageId, RunId, WorkspaceId};
use crate::store::workflow::RunFire;
use crate::store::writer::LaunchLogin;
use crate::workspace::WorkspaceResolver;

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
    /// The room the task was armed in, where [`Self::holder`] reads its rows.
    pub root: PathBuf,
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
        catalog::delivery_holder(&self.root, &self.target)
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

/// The lateness a `fire-at` fire records: an on-time fire lands within one 60 s clock tick plus
/// spawn time, so two minutes or more means no clock saw the instant.
const LATE_FIRE_MS: u64 = 120_000;

/// What a fire overlaps: the task's live workflow run, or the alive launch of an earlier fire.
enum Blocker {
    Run(crate::ids::WorkflowRunId),
    /// The alive launch of `fire`, as `holder` names it.
    Launch {
        fire: FireId,
        holder: String,
    },
}

impl Blocker {
    fn run(&self) -> Option<&crate::ids::WorkflowRunId> {
        match self {
            Self::Run(run) => Some(run),
            Self::Launch { .. } => None,
        }
    }
}

impl std::fmt::Display for Blocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run(run) => write!(f, "run {run}"),
            Self::Launch { holder, .. } => f.write_str(holder),
        }
    }
}

/// The state every kind of fire shares: the task, the trigger it serves, and the lease.
struct Fire<'a> {
    name: String,
    /// The task's id, bound at construction: the key of its backlog entry and run lock.
    task_id: TaskId,
    task: LoadedTask,
    entry: TaskEntry,
    catalog: &'a TaskCatalog,
    ephemeral: bool,
    mode: LoopRunMode,
    now: Timestamp,
    config: Arc<MachineConfig>,
    signal: Option<TriggerSignal>,
    arrived_at: Option<Timestamp>,
    from_arrived_at: Option<Timestamp>,
    condition: Option<crate::harness::when::ConditionEvidence>,
    checkout: Option<PathBuf>,
    /// A shell row's `worktree`, once this fire created or found it: where its command runs.
    row_tree: Option<PathBuf>,
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
    finished: bool,
    /// This fire took its trigger from the backlog.
    draining: bool,
    /// A fresh queued arrival that served the queue's head under the lease.
    queued_arrival: Option<WaitingTrigger>,
    /// This fire started a run or found one live, whose report drains the backlog.
    saw_run: bool,
}

/// One loop fire from ordered gates through one history transition, unless a drain finds no work.
pub struct TaskFire<'a> {
    core: Fire<'a>,
    effect: Effect,
}

/// What a fire does once admitted, one per task action, with the state only that kind keeps.
enum Effect {
    Shell(Shell),
    Launch(Launch),
    Deliver(Delivery),
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
        let effect = match task.action().map_err(Clone::clone)?.clone() {
            TaskAction::Shell(argv) => Effect::Shell(Shell { argv }),
            TaskAction::Launch(launch) => Effect::Launch(Launch { launch }),
            TaskAction::Deliver(target) => Effect::Deliver(Delivery {
                target,
                pending: false,
            }),
        };
        let binding = task.binding(&name);
        let task_id = task_record_store::mutate(|records| records.bind(&binding).0)?;
        let entry = task.entry().clone();
        let ephemeral = task.is_ephemeral();
        let late_ms = entry
            .fire_at
            .filter(|_| mode == LoopRunMode::Scheduled)
            .and_then(|at| u64::try_from(now.duration_since(at).as_millis()).ok())
            .filter(|late| *late >= LATE_FIRE_MS);
        let core = Fire {
            name,
            task_id,
            task,
            entry,
            catalog,
            ephemeral,
            mode,
            now,
            config,
            signal,
            arrived_at: None,
            from_arrived_at: None,
            condition: None,
            checkout: None,
            row_tree: None,
            started,
            run_lock: None,
            record_hold: None,
            backlog_paths: None,
            absorbed: 0,
            run_lock_path,
            late_ms,
            watch_check: None,
            finished: false,
            draining: false,
            queued_arrival: None,
            saw_run: false,
        };
        Ok(Self { core, effect })
    }

    /// Claim one waiting trigger before the CLI rechecks arming and trust.
    pub fn take_backlog(&mut self) -> Result<bool> {
        self.core.take_backlog()
    }

    /// Spawn the next drain before releasing a protocol-managed run lock.
    pub fn release_run_lock(&mut self) -> Result<()> {
        self.core.release_run_lock()
    }

    pub fn with_arrived_at(mut self, arrived_at: Option<Timestamp>) -> Self {
        self.core.arrived_at = arrived_at;
        self
    }

    pub fn with_condition(
        mut self,
        condition: Option<crate::harness::when::ConditionEvidence>,
    ) -> Self {
        self.core.condition = condition;
        self
    }

    pub fn with_checkout(mut self, checkout: Option<PathBuf>) -> Self {
        self.core.checkout = checkout;
        self
    }

    pub fn prepare(
        &mut self,
        before_launch: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<TaskFirePlan> {
        match &mut self.effect {
            Effect::Shell(shell) => self.core.prepare_shell(shell, before_launch),
            Effect::Launch(launch) => self.core.prepare_launch(launch, before_launch),
            Effect::Deliver(delivery) => self.core.prepare_delivery(delivery),
        }
    }

    pub fn finish(&mut self, effect: TaskFireEffect) -> Result<TaskFireFinished> {
        let delivery = match &mut self.effect {
            Effect::Deliver(delivery) if delivery.pending => delivery,
            _ => bail!("loop task has no prepared effect to finish"),
        };
        delivery.pending = false;
        self.core.finish_delivery(&delivery.target, effect)
    }

    pub fn finish_error(&mut self, err: &anyhow::Error) -> TaskFireFinished {
        let prepared = match &mut self.effect {
            Effect::Deliver(delivery) => std::mem::take(&mut delivery.pending),
            Effect::Shell(_) | Effect::Launch(_) => false,
        };
        self.core.finish_error(err, prepared)
    }
}

impl<'a> Fire<'a> {
    fn take_backlog(&mut self) -> Result<bool> {
        let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
        let mut backlog = LockedBacklog::acquire(&paths)?;
        if let Some(legacy) = backlog.record.tasks.remove(&self.name) {
            // An older RimZ keyed waiting triggers by name; they move to the task's id.
            backlog
                .record
                .tasks
                .entry(self.task_id.to_string())
                .or_default()
                .triggers
                .extend(legacy.triggers);
            backlog.save()?;
        }
        let Some(waiting) = backlog.record.tasks.get_mut(self.task_id.as_str()) else {
            return Ok(false);
        };
        if waiting.triggers.is_empty() {
            return Ok(false);
        }
        let path = self.fire_run_lock_path()?;
        let RunLockAttempt::Acquired(guard) = acquire_run_lock_file(&path, Some(self.task.kind()))?
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
    fn release_run_lock(&mut self) -> Result<()> {
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
                .get(self.task_id.as_str())
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
                    || !self.entry.overlap.unwrap_or_default().retains_triggers() =>
            {
                tracing::warn!(task = %self.name, %error, "failed to read loop stop mark; continuing without it");
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let Some(waiting) = backlog.record.tasks.get_mut(self.task_id.as_str()) else {
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

    fn launch_checkout(&self) -> PathBuf {
        self.checkout
            .clone()
            .unwrap_or_else(|| self.entry.run_dir())
    }

    /// The run's scope: a shell row's worktree, else the launch checkout.
    fn fire_dir(&self) -> PathBuf {
        self.row_tree
            .clone()
            .unwrap_or_else(|| self.launch_checkout())
    }

    /// Create a shell row's `worktree` from the configured base when it is missing, or reuse
    /// it, and announce a created one when the room's store exists. The creation takes the
    /// admission an agent launch's `-w` takes (a readable core config, the name's own checks),
    /// and a refusal fails the fire with nothing made. A directory there that RimZ does not
    /// own fails the fire too, which never asks. Returns whether the fire created the tree.
    fn prepare_row_tree(&mut self) -> Result<bool> {
        let Some(name) = self.entry.worktree.as_deref() else {
            return Ok(false);
        };

        let root = self.entry.resolved_root();
        let placing = || format!("placing loop task `{}` in worktree `{name}`", self.name);
        self.config
            .require_readable_core("create a worktree")
            .with_context(placing)?;
        let workspace = crate::workspace::WorkspaceResolver::resolve(&root, None)?;
        let room = fire_tree_room(&root);
        let agents = match &room {
            Some((store, _)) => store.runtime_projection(crate::RuntimeScope::Audit)?.agents,
            None => Vec::new(),
        };
        crate::harness::lanes::admit_launch_worktree_name(
            &workspace,
            &self.config.agents.worktree,
            &agents,
            Some(name),
        )
        .with_context(placing)?;
        let checkout = crate::worktree::resolve_launch_checkout(
            &workspace,
            &self.config.agents.worktree,
            Some(name),
            None,
            None,
            None,
            crate::worktree::LaunchBase::Configured,
        )
        .with_context(placing)?;
        let created = checkout.created.is_some();
        if let Some(marker) = &checkout.created {
            match &room {
                Some((store, workspace)) => {
                    crate::worktree::emit_worktree_created(store, workspace, marker);
                }
                None => {
                    tracing::debug!(task = %self.name, "skipping worktree signal: store unavailable")
                }
            }
        }
        self.row_tree = Some(checkout.cwd);
        Ok(created)
    }

    /// Stamp the tree this fire created with the fire, so the task's record outlives its
    /// retirement while the tree stands, as a launch's own tree does.
    fn stamp_row_tree(&self, fire: &FireId) {
        let Some(tree) = &self.row_tree else {
            return;
        };
        if let Err(error) = crate::worktree::stamp_loop_fire(tree, fire) {
            tracing::warn!(
                %fire,
                %error,
                path = %tree.display(),
                "the fire's tree carries no fire stamp"
            );
        }
    }

    /// The gates in front of the lease: a watch's verdict, the deadline, then the checkout's
    /// memory of a run already started.
    fn before_lease(&mut self) -> Result<Option<TaskFirePlan>> {
        if let Some(done) = self.prepare_watch()? {
            return Ok(Some(TaskFirePlan::Done(done)));
        }
        if let Some(done) = self.prepare_deadline()? {
            return Ok(Some(TaskFirePlan::Done(done)));
        }
        if self.workflow_already_launched()? {
            self.finished = true;
            return Ok(Some(TaskFirePlan::AlreadyLaunched));
        }
        Ok(None)
    }

    /// Admission: the run lock, the room `before_launch` readies, the memory again, since
    /// another fire may have started a run while this one waited for the room or lease, then
    /// the backlog's reconcile.
    fn admit(
        &mut self,
        before_launch: &mut dyn FnMut(&Path) -> Result<()>,
    ) -> Result<Option<TaskFirePlan>> {
        if let Some(done) = self.prepare_run_lock()? {
            return Ok(Some(TaskFirePlan::Done(done)));
        }
        before_launch(&self.entry.resolved_root())?;
        if self.workflow_already_launched()? {
            self.finished = true;
            return Ok(Some(TaskFirePlan::AlreadyLaunched));
        }
        Ok(self.reconcile_backlog()?.map(TaskFirePlan::Done))
    }

    /// A fire that published `run` at `at`. The run's report is the fire's one history row:
    /// this launch only stamps its proof.
    fn launched(
        &mut self,
        fire: FireId,
        run: Option<crate::ids::WorkflowRunId>,
        at: Timestamp,
    ) -> Result<TaskFirePlan> {
        self.saw_run = true;
        let error = self.note_launched(&fire, run.as_ref(), at);
        // A poll-until row lives until a run acts, which the run's report decides.
        if self.entry.deadline.is_none() {
            self.consume_ephemeral()?;
        }
        Ok(TaskFirePlan::Done(self.record_terminal_with(
            LoopRunResult::Launched,
            LoopRunPresentation::default(),
            TaskFireNotice::None,
            None,
            |record| {
                record.run_id = run.map(|run| run.to_string());
                record.fire = Some(fire);
                record.error = error;
            },
        )))
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
        if !terminal || polarity_fires(self.entry.on, &outcome) {
            return Ok(None);
        }
        if self.mode == LoopRunMode::Scheduled {
            self.remove_schedule()?;
        }
        Ok(Some(self.record_terminal_with(
            LoopRunResult::CheckSkipped,
            LoopRunPresentation {
                check_duration_ms: Some(duration_ms),
                ..LoopRunPresentation::default()
            },
            TaskFireNotice::None,
            None,
            |_| {},
        )))
    }

    /// `replace` on a launch: SIGTERM its launch child (a foreground `-p` run turns that into a
    /// durable cancel), then forward each alive run or agent the fire launched to its own stop
    /// in a detached helper; a `-p` run whose launch child is gone is stopped the same way. The
    /// held trigger drains once nothing of it is alive.
    fn stop_launch(&self, paths: &StatePaths, fire: &FireId) {
        let kind = self.entry.launch_kind().unwrap_or_default();
        let stops = (|| -> Result<()> {
            let runtime = RuntimePaths::for_state(paths)?;
            let rows = room_agents(paths)?;
            let open = rows
                .iter()
                .filter(|row| !row.session_over())
                .collect::<Vec<_>>();
            let alive: Vec<_> = super::launches::alive(&self.task_id, paths, rows.iter())?
                .into_iter()
                .filter(|alive| &alive.fire == fire && alive.launch.launched_by(kind))
                .collect();
            let child_runs = alive
                .iter()
                .any(|alive| matches!(alive.launch, super::launches::Launch::Child { .. }));
            for alive in alive {
                let mut args: Vec<std::ffi::OsString> =
                    vec!["--root".into(), self.entry.resolved_root().into_os_string()];
                match &alive.launch {
                    super::launches::Launch::Child { child, .. } => {
                        crate::child_process::signal_process_term(
                            child.pid,
                            child.start.as_deref(),
                        );
                        continue;
                    }
                    super::launches::Launch::Workflow(run) => {
                        args.extend(crate::harness::workflow::cancel_args(
                            &run.id,
                            "replaced by a newer trigger",
                        ))
                    }
                    // The `-p` child the SIGTERM reached cancels its own run.
                    super::launches::Launch::Supervised(_) if child_runs => continue,
                    super::launches::Launch::Supervised(run) => args.extend([
                        "agents".into(),
                        "stop".into(),
                        run.run_id.to_string().into(),
                    ]),
                    super::launches::Launch::Agent(agent) => args.extend([
                        "agents".into(),
                        "stop".into(),
                        crate::address::agent_handle(agent, &open, true).into(),
                    ]),
                }
                crate::child_process::spawn_detached_rimz(&runtime, &args, "loop-replace")?;
            }
            Ok(())
        })();
        if let Err(error) = stops {
            tracing::warn!(task = %self.name, %fire, error = %format!("{error:#}"), "replacement could not stop the alive launch; trigger remains waiting");
        }
    }

    /// What this fire would log at its launch, sealed into its run for the run's one report row:
    /// its trigger, waits, lock tenure and kind. A run placed in the row's tree also carries the
    /// fire's checkout, which its rows key on rather than the tree.
    fn fire_facts(&self) -> serde_json::Value {
        let mut record = self.terminal_record(LoopRunResult::Launched);
        record.run_lock = self.run_lock.as_ref().map(|guard| guard.info);
        if self.row_tree.is_some() {
            record.checkout = Some(self.launch_checkout());
        }
        run_log::fire_facts(&record)
    }

    /// What this fire tells its run's briefs, frozen into the run at its creation: the task, the
    /// served trigger's `evidence` when it has any, and for a scheduled fire the trigger; a
    /// manual fire's brief names the command instead. The task's booking chain rides along for
    /// the agents the run launches.
    fn run_fire(&self, evidence: &str) -> RunFire {
        let task = self.name.clone();
        let evidence = (!evidence.is_empty()).then(|| evidence.to_owned());
        let booked_by = self.task.entry().booked_by.clone();
        if self.mode == LoopRunMode::Manual {
            return RunFire::Manual {
                task,
                evidence,
                booked_by,
            };
        }
        RunFire::Scheduled {
            task,
            trigger: super::trigger_clause(&self.task),
            evidence,
            booked_by,
        }
    }

    /// The live run or launch this fire overlaps: `skip` records an overlap naming it;
    /// `coalesce`, `queue` and `replace` keep the trigger waiting for it, and `replace` stops it.
    /// An `allow` fire never reaches it: it launches beside what is alive.
    /// A drained trigger whose blocker still runs goes back to the head of its queue.
    fn prepare_overlap(&mut self, blocker: &Blocker) -> Result<TaskFirePlan> {
        self.saw_run = true;
        let policy = self.entry.overlap.unwrap_or_default();
        let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
        if self.draining {
            let mut backlog = LockedBacklog::acquire(&paths)?;
            let waiting = backlog
                .record
                .tasks
                .entry(self.task_id.to_string())
                .or_default();
            waiting.triggers.push_front(self.waiting_trigger());
            backlog.save()?;
            self.finished = true;
            return Ok(TaskFirePlan::Waiting);
        }
        if self.mode != LoopRunMode::Scheduled || policy == OverlapPolicy::Skip {
            let detail = format!("{blocker} still active — skipped");
            return Ok(TaskFirePlan::Done(self.record_terminal_with(
                LoopRunResult::Overlapped,
                LoopRunPresentation::default(),
                TaskFireNotice::Overlap {
                    detail: Some(detail.clone()),
                },
                None,
                |record| {
                    record.blocking_run = blocker.run().cloned();
                    record.error = Some(detail);
                },
            )));
        }
        let limit = self.entry.overlap_limit.unwrap_or(10);
        let mut backlog = LockedBacklog::acquire(&paths)?;
        let waiting = backlog
            .record
            .tasks
            .entry(self.task_id.to_string())
            .or_default();
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
        let detail = format!("{blocker} still active — {suffix}");
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
                record.blocking_run = blocker.run().cloned();
                record.error = Some(detail);
            },
        );
        // The deferred row and this fire's lock release come first: the canceled run's report
        // drains the waiting trigger, and a drainer that finds this fire's lock held starts none.
        if enqueued && policy == OverlapPolicy::Replace {
            match blocker {
                Blocker::Run(run) => self.cancel_blocker(&paths, run),
                Blocker::Launch { fire, .. } => self.stop_launch(&paths, fire),
            }
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

    fn workflow_already_launched(&self) -> Result<bool> {
        if self.mode != LoopRunMode::Scheduled || !self.entry.dedupes_checkout() {
            return Ok(false);
        }
        Ok(task_record::load()?
            .launched(
                &self.entry,
                &self.task.key(&self.name),
                &self.launch_checkout(),
            )
            .is_some())
    }

    /// Mint this fire's id and write its open mark for the launch checkout, before the run
    /// exists, with the mark it displaced; a displaced mark whose fire died is settled first.
    fn open_fire(&self) -> Result<(FireId, Option<FireMark>)> {
        let binding = self.task.binding(&self.name);
        let checkout = self.launch_checkout();
        if let Some(record) = task_record::load()?.get(&self.task_id)
            && let Some(mark) = record.marks.get(&checkout)
        {
            run_log::settle_displaced(&self.entry.resolved_root(), &record.binding, mark);
        }
        Ok(fires::open(
            &self.task_id,
            &binding,
            fires::Opening {
                checkout: &checkout,
                booked_by: self.entry.booked_by.clone(),
                once: self.entry.dedupes_checkout(),
                scope: crate::store::workflow::ScopeKind::Worktree,
                at: self.now,
                kind: self.entry.launch_kind().unwrap_or_default(),
                target: self.entry.target.clone(),
                manual: self.mode == LoopRunMode::Manual,
            },
        )?)
    }

    /// Note the run this fire published, when it names one, and, for a scheduled fire, stamp
    /// the task's launch proof at `at`. A failed write is the launch row's error: the launch
    /// stands, and its report or the settle pass closes the mark.
    fn note_launched(
        &self,
        fire: &FireId,
        run: Option<&crate::ids::WorkflowRunId>,
        at: Timestamp,
    ) -> Option<String> {
        #[cfg(any(test, feature = "testkit"))]
        crate::testkit::rendezvous("RIMZ_TEST_LOOP_BEFORE_LAUNCH_MEMORY");
        let scheduled =
            (self.mode == LoopRunMode::Scheduled).then_some((self.checkout.as_deref(), at));
        let error = fires::launched(fire, run, scheduled).err()?;
        tracing::warn!(task = %self.name, %fire, %error, "recording the fire's launch failed");
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

    /// The admission lease: it serializes fires of one task, while the live run is what a
    /// later fire overlaps. A fire held off by another admission waits under the policy.
    fn prepare_run_lock(&mut self) -> Result<Option<TaskFireFinished>> {
        if self.run_lock.is_some() {
            return Ok(None);
        }
        let path = self.fire_run_lock_path()?;
        let policy = self.entry.overlap.unwrap_or_default();
        if policy.retains_triggers() {
            let paths = StatePaths::for_project_root(&self.entry.resolved_root())?;
            let mut backlog = LockedBacklog::acquire(&paths)?;
            match acquire_run_lock_file(&path, Some(self.task.kind()))? {
                RunLockAttempt::Acquired(guard) => {
                    if self.mode == LoopRunMode::Scheduled {
                        let waiting = backlog
                            .record
                            .tasks
                            .entry(self.task_id.to_string())
                            .or_default();
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
                    let waiting = backlog
                        .record
                        .tasks
                        .entry(self.task_id.to_string())
                        .or_default();
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
        match acquire_run_lock_file(&path, Some(self.task.kind()))? {
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
        let file = run_lock_file_name(self.task_id.as_str(), checkout.as_ref());
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

    fn finish_error(&mut self, err: &anyhow::Error, prepared: bool) -> TaskFireFinished {
        if prepared && let Err(consume) = self.consume_ephemeral() {
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
        if self.entry.launch_kind().is_some() && self.entry.watch.is_none() {
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
        record.task_id = Some(self.task_id.clone());
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
        presentation: LoopRunPresentation,
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
        let transition = self.record_and_release(&mut record);
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
                .get_mut(self.task_id.as_str())
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
                spawn_backlog_drain(
                    &paths,
                    &root,
                    self.task_id.as_str(),
                    &self.name,
                    &backlog.record,
                )
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
        if let Err(error) = self.core.release_run_lock() {
            tracing::warn!(task = %self.core.name, error = %format!("{error:#}"), "failed to release loop run and start its backlog drain");
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

/// The room a fire's created worktree is announced in, when its store exists.
fn fire_tree_room(root: &Path) -> Option<(crate::Store, crate::ResolvedWorkspace)> {
    let paths = StatePaths::for_project_root(root).ok()?;
    let runtime = RuntimePaths::for_state(&paths).ok()?;
    let store = crate::Store::open_existing(paths, runtime)?;
    let workspace = crate::workspace::WorkspaceResolver::resolve(root, None).ok()?;
    Some((store, workspace))
}

fn polarity_fires(on: Option<CheckOn>, outcome: &CheckOutcome) -> bool {
    match on.unwrap_or_default() {
        CheckOn::Fail => !outcome.passed(),
        CheckOn::Success => outcome.passed(),
        CheckOn::Any => true,
    }
}

#[cfg(test)]
mod tests;
