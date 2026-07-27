//! Loop-fire policy from ordered gates through one durable history transition.
//!
//! [`TaskFire`] owns checks, deadlines, task consumption, run locks, prompt and
//! launch preparation, and terminal record mapping. CLI executes the prepared
//! supervised-run or message effect and returns its typed result.

use std::cell::OnceCell;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use jiff::Timestamp;
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};

use crate::agents::{
    HookPreflightErr, ManagedLaunchState, ProviderCapacity, TurnLifecycleNeed, WindowSurplus,
    find_definition, preflight_hooks,
};
use crate::config::{CheckOn, MachineConfig, TaskEntry, TaskTarget};
use crate::harness::run::{PermissionMode, RunRecord, SupervisedRunOutcome, SupervisedRunRequest};
use crate::harness::schedule::TaskAction;
use crate::harness::schedule::catalog::{self, LoadedTask, TaskCatalog};
use crate::harness::schedule::run_log::{
    self, CheckRecord, LoopRunMode, LoopRunPresentation, LoopRunRecord, LoopRunResult,
    RunTransition,
};
use crate::harness::spec::{self as agents_spec, Cell, LayoutSpec};
use crate::ids::WorkspaceId;
use crate::store::paths::{RuntimePaths, StatePaths, config_home, state_home};
use crate::workspace::WorkspaceResolver;

pub const CHECK_DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
pub const SCHEDULED_RUN_DEFAULT_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
pub const SCHEDULED_RUN_DEFAULT_TIMEOUT_LABEL: &str = "2h";
const CHECK_POLL_INTERVAL: Duration = Duration::from_millis(20);
const RUN_LOCK_RELEASE_POLL_INTERVAL: Duration = Duration::from_millis(200);
const CHECK_OUTPUT_CAP: usize = 16 * 1024;
const TASK_TIMEOUT_UNITS: &[(&str, u64)] = &[("s", 1), ("m", 60), ("h", 3600), ("d", 86_400)];

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

/// One fired guard check awaiting CLI presentation before its effect.
#[derive(Clone, Debug)]
pub struct CheckTrip {
    pub record: CheckRecord,
    pub duration_ms: u64,
}

#[derive(Clone, Debug)]
pub struct PreparedSpawn {
    pub root: PathBuf,
    pub request: SupervisedRunRequest,
    pub stream: bool,
}

#[derive(Clone, Debug)]
pub struct PreparedDelivery {
    pub root: PathBuf,
    pub target: TaskTarget,
    pub prompt: String,
}

#[derive(Clone, Debug)]
pub enum TaskFirePlan {
    Done(TaskFireFinished),
    Spawn(PreparedSpawn),
    Deliver(PreparedDelivery),
}

#[derive(Debug)]
pub enum TaskFireEffect {
    Spawn(SupervisedRunOutcome),
    Delivered,
    TargetGone,
}

#[derive(Clone, Debug)]
enum PendingEffect {
    Spawn {
        check: Option<CheckRecord>,
        stream: bool,
    },
    Deliver {
        target: TaskTarget,
        check: Option<CheckRecord>,
    },
}

struct FiredCheck {
    command: String,
    outcome: CheckOutcome,
    record: CheckRecord,
}

struct FireContext {
    action: TaskAction,
    root: PathBuf,
    scope: Option<FireScope>,
}

struct FireScope {
    kind: crate::ids::AgentKind,
    scope_runtime: RuntimePaths,
    resolved: Option<ResolvedTaskSpec>,
    managed_launch: ManagedLaunchState,
    capacity: OnceCell<Option<ProviderCapacity>>,
}

impl FireScope {
    fn new(
        kind: crate::ids::AgentKind,
        scope_runtime: RuntimePaths,
        resolved: Option<ResolvedTaskSpec>,
    ) -> Self {
        Self {
            kind,
            scope_runtime,
            resolved,
            managed_launch: ManagedLaunchState::Unsupported,
            capacity: OnceCell::new(),
        }
    }

    fn capacity(&self) -> Option<&ProviderCapacity> {
        self.capacity
            .get_or_init(|| {
                self.managed_launch
                    .capacity(&self.scope_runtime, self.kind.as_str())
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
    fn resolve(entry: &TaskEntry, action: TaskAction, config: &MachineConfig) -> Result<Self> {
        let root = entry.resolved_root();
        if action.is_check_only() {
            return Ok(Self {
                action,
                root,
                scope: None,
            });
        }
        let workspace = WorkspaceResolver::resolve(&root, None)?;
        let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
        let scope = match &action {
            TaskAction::Spawn(spec) => {
                let resolved = resolve_task_spec(spec, &workspace)?;
                let managed_launch =
                    resolve_managed_spawn_state(entry, &workspace, &resolved, config)?;
                let mut scope = FireScope::new(
                    crate::ids::AgentKind::new_unchecked(resolved.kind.clone()),
                    runtime,
                    Some(resolved),
                );
                scope.managed_launch = managed_launch;
                scope
            }
            TaskAction::Deliver(target) => {
                let mut scope = FireScope::new(
                    crate::ids::AgentKind::new_unchecked(target.kind.clone()),
                    runtime,
                    None,
                );
                scope.managed_launch = unresolved_managed_state(entry, &target.kind);
                scope
            }
            TaskAction::CheckOnly => unreachable!("check-only context returned above"),
        };
        Ok(Self {
            action,
            root,
            scope: Some(scope),
        })
    }
}

/// One loop fire from ordered gates through exactly one history transition.
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
    started: Instant,
    run_lock: Option<RunLockGuard>,
    pending: Option<PendingEffect>,
    finished: bool,
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
            started,
            run_lock: None,
            pending: None,
            finished: false,
        })
    }

    pub fn entry(&self) -> &TaskEntry {
        &self.entry
    }

    pub fn keep(&self) -> bool {
        self.keep
    }

    pub fn mode(&self) -> LoopRunMode {
        self.mode
    }

    pub fn take_check_trip(&mut self) -> Option<CheckTrip> {
        self.check_trip.take()
    }

    pub fn prepare(&mut self) -> Result<TaskFirePlan> {
        if let Some(done) = self.prepare_scope_gates()? {
            return Ok(TaskFirePlan::Done(done));
        }
        if let Some(done) = self.prepare_run_lock()? {
            return Ok(TaskFirePlan::Done(done));
        }

        if deadline_expired_at(&self.entry, self.now) {
            if self.mode == LoopRunMode::Scheduled {
                self.catalog.consume_scheduled(&self.name)?;
            }
            return Ok(TaskFirePlan::Done(self.record_terminal(
                LoopRunResult::Expired,
                LoopRunPresentation::default(),
                TaskFireNotice::None,
                None,
            )));
        }

        let fired_check = self.prepare_check()?;
        if let Some(done) = fired_check.done {
            return Ok(TaskFirePlan::Done(done));
        }
        let fired_check = fired_check.fire;
        match self
            .context
            .as_ref()
            .context("loop task context missing after gates")?
            .action
            .clone()
        {
            TaskAction::Spawn(spec) => self.prepare_spawn(spec, fired_check),
            TaskAction::Deliver(target) => self.prepare_delivery(target, fired_check),
            TaskAction::CheckOnly => {
                unreachable!("check-only action is completed by prepare_check")
            }
        }
    }

    fn prepare_scope_gates(&mut self) -> Result<Option<TaskFireFinished>> {
        let zoned_now = self.now.to_zoned(self.config.time_zone());
        if let Some(gate) =
            run_log::daily_budget_gate(&state_home(), &self.name, &self.entry, &zoned_now)
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
        let context = FireContext::resolve(&self.entry, action, &self.config)?;
        if let Some(scope) = &context.scope
            && let Some(reason) = crate::harness::budget::scope_gate(
                &scope.scope_runtime,
                &scope.kind,
                &self.config,
                self.now,
            )
        {
            return Ok(Some(self.record_gate(LoopRunResult::BudgetSkipped, reason)));
        }
        if let Some(scope) = &context.scope
            && let Some(binding) = scope.managed_launch.binding()
            && let Some(reason) = crate::agents::provider_budget_gate(
                &scope.scope_runtime,
                scope.kind.as_str(),
                binding,
                self.now,
            )
        {
            return Ok(Some(self.record_gate(LoopRunResult::BudgetSkipped, reason)));
        }
        if let Some(scope) = &context.scope
            && let Some(reason) = scope.surplus_gate(&self.entry, self.now)
        {
            return Ok(Some(
                self.record_gate(LoopRunResult::SurplusSkipped, reason),
            ));
        }
        self.context = Some(context);
        Ok(None)
    }

    fn prepare_run_lock(&mut self) -> Result<Option<TaskFireFinished>> {
        match acquire_run_lock(&self.name, &self.entry)? {
            RunLockAttempt::Acquired(guard) => {
                self.run_lock = Some(guard);
                Ok(None)
            }
            RunLockAttempt::Held(info) => {
                let detail = info.map(|info| {
                    format!(
                        "previous run still active (pid {}, started {}) — skipped",
                        info.pid,
                        relative_age(info.started_at, Timestamp::now())
                    )
                });
                Ok(Some(self.record_terminal_with(
                    LoopRunResult::Overlapped,
                    LoopRunPresentation::default(),
                    TaskFireNotice::Overlap {
                        detail: detail.clone(),
                    },
                    None,
                    |record| record.error = detail.clone(),
                )))
            }
        }
    }

    pub fn finish(&mut self, effect: TaskFireEffect) -> Result<TaskFireFinished> {
        let pending = self
            .pending
            .take()
            .context("loop task has no prepared effect to finish")?;
        match (pending, effect) {
            (PendingEffect::Spawn { check, stream }, TaskFireEffect::Spawn(outcome)) => {
                let mut record = self.terminal_record(LoopRunResult::Completed);
                let (presentation, notice) =
                    finish_spawn_effect(&mut record, outcome, check, stream);
                Ok(self.finish_record(record, presentation, notice, None))
            }
            (PendingEffect::Deliver { target, check }, TaskFireEffect::Delivered) => {
                let handle = target.handle;
                Ok(self.record_terminal_with(
                    LoopRunResult::Delivered,
                    LoopRunPresentation::default(),
                    TaskFireNotice::None,
                    None,
                    |record| {
                        record.target = Some(handle);
                        record.check = check;
                    },
                ))
            }
            (PendingEffect::Deliver { target, check }, TaskFireEffect::TargetGone) => {
                if self.mode == LoopRunMode::Scheduled {
                    self.catalog.consume_scheduled(&self.name)?;
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
                        record.check = check;
                    },
                ))
            }
            _ => anyhow::bail!("loop task effect does not match its prepared plan"),
        }
    }

    pub fn finish_error(&mut self, err: &anyhow::Error) -> TaskFireFinished {
        self.pending = None;
        let error = format!("{err:#}");
        self.record_terminal_with(
            LoopRunResult::Errored,
            LoopRunPresentation::default(),
            TaskFireNotice::None,
            None,
            |record| record.error = Some(error),
        )
    }

    fn prepare_check(&mut self) -> Result<PreparedCheck> {
        let Some(command) = self.entry.check.clone() else {
            return Ok(PreparedCheck::fire(None));
        };
        let check_started = Instant::now();
        let outcome = run_check(
            &self.entry.resolved_root(),
            &command,
            check_timeout(&self.entry)?.unwrap_or(CHECK_DEFAULT_TIMEOUT),
            self.check_echo.take().unwrap_or(CheckEcho::Capture),
        )?;
        let duration_ms = elapsed_millis(check_started);
        let record = check_record(&outcome);
        if self
            .context
            .as_ref()
            .is_some_and(|context| context.action.is_check_only())
        {
            if self.mode == LoopRunMode::Scheduled && self.ephemeral {
                self.catalog.consume_scheduled(&self.name)?;
            }
            let result = check_only_result(&outcome);
            let finished = self.record_terminal_with(
                result,
                LoopRunPresentation {
                    check_duration_ms: Some(duration_ms),
                    ..LoopRunPresentation::default()
                },
                TaskFireNotice::None,
                None,
                |run| run.check = Some(record),
            );
            return Ok(PreparedCheck::done(finished));
        }
        if !polarity_fires(self.entry.on, &outcome) {
            let finished = self.record_terminal_with(
                LoopRunResult::CheckSkipped,
                LoopRunPresentation {
                    check_duration_ms: Some(duration_ms),
                    ..LoopRunPresentation::default()
                },
                TaskFireNotice::None,
                None,
                |run| run.check = Some(record),
            );
            return Ok(PreparedCheck::done(finished));
        }
        if self.mode == LoopRunMode::Manual {
            self.check_trip = Some(CheckTrip {
                record: record.clone(),
                duration_ms,
            });
        }
        Ok(PreparedCheck::fire(Some(FiredCheck {
            command,
            outcome,
            record,
        })))
    }

    fn prepare_spawn(
        &mut self,
        spec: String,
        fired_check: Option<FiredCheck>,
    ) -> Result<TaskFirePlan> {
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
            preflight_resolved_task(resolved)?;
            scope.managed_launch.clone()
        };
        let prompt = self.resolve_effect_prompt(fired_check.as_ref())?;
        let request = self.compile_spawn_request(spec, prompt, managed_launch)?;
        self.consume_ephemeral()?;
        let check = fired_check.as_ref().map(|check| check.record.clone());
        let stream = self.mode == LoopRunMode::Manual;
        self.pending = Some(PendingEffect::Spawn {
            check: check.clone(),
            stream,
        });
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
            .transpose()?
            .unwrap_or(PermissionMode::Auto);
        let task_timeout = self
            .entry
            .timeout
            .as_deref()
            .map(parse_task_timeout)
            .transpose()
            .map_err(anyhow::Error::msg)?;
        let configured_timeout = self
            .config
            .r#loop
            .default_timeout
            .as_deref()
            .map(parse_task_timeout)
            .transpose()
            .map_err(anyhow::Error::msg)?;
        let timeout = effective_spawn_timeout(self.mode, task_timeout, configured_timeout);
        let budget = self
            .entry
            .budget
            .as_deref()
            .map(str::parse::<crate::harness::budget::BudgetSpec>)
            .transpose()?;
        Ok(SupervisedRunRequest {
            spec,
            prompt,
            description: None,
            worktree: self.entry.worktree.clone(),
            from_pr: None,
            channel: None,
            name: None,
            background: false,
            force_new_tab: false,
            top_level: false,
            permission_mode,
            agent: None,
            model: None,
            system_prompt_file,
            effort: self.entry.effort.clone(),
            budget,
            max_turns: None,
            timeout,
            keep: self.keep,
            retries: 0,
            verify: self.entry.verify.clone(),
            max_attempts: self.entry.max_attempts,
            loop_zone: self.mode == LoopRunMode::Scheduled,
            loop_task: Some(self.name.clone()),
            passthrough: Vec::new(),
            managed_launch,
        })
    }

    fn prepare_delivery(
        &mut self,
        target: TaskTarget,
        fired_check: Option<FiredCheck>,
    ) -> Result<TaskFirePlan> {
        let check = fired_check.as_ref().map(|check| check.record.clone());
        if !catalog::delivery_target_alive(&self.entry, &target)? {
            if self.mode == LoopRunMode::Scheduled {
                self.catalog.consume_scheduled(&self.name)?;
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
                    record.check = check;
                },
            )));
        }
        let prompt = self.resolve_effect_prompt(fired_check.as_ref())?;
        self.consume_ephemeral()?;
        self.pending = Some(PendingEffect::Deliver {
            target: target.clone(),
            check: check.clone(),
        });
        Ok(TaskFirePlan::Deliver(PreparedDelivery {
            root: self.context_root()?,
            target,
            prompt,
        }))
    }

    fn consume_ephemeral(&self) -> Result<()> {
        if self.mode == LoopRunMode::Scheduled && self.ephemeral {
            self.catalog.consume_scheduled(&self.name)?;
        }
        Ok(())
    }

    fn context_root(&self) -> Result<PathBuf> {
        self.context
            .as_ref()
            .map(|context| context.root.clone())
            .context("loop task context missing after gates")
    }

    fn resolve_effect_prompt(&self, fired_check: Option<&FiredCheck>) -> Result<String> {
        let prompt = resolve_task_prompt(&self.name, &self.entry)?;
        Ok(match fired_check {
            Some(check) => augment_prompt(prompt, &check.command, &check.outcome),
            None => prompt,
        })
    }

    fn terminal_record(&self, result: LoopRunResult) -> LoopRunRecord {
        LoopRunRecord::new(&self.name, result, self.mode, elapsed_millis(self.started))
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

    fn record_terminal(
        &mut self,
        result: LoopRunResult,
        presentation: LoopRunPresentation,
        notice: TaskFireNotice,
        at: Option<Timestamp>,
    ) -> TaskFireFinished {
        let record = self.terminal_record(result);
        self.finish_record(record, presentation, notice, at)
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
        if let Some(at) = at {
            record.at = at;
        }
        let transition = run_log::record_transition(&self.task, &record);
        self.finished = true;
        TaskFireFinished {
            record,
            presentation,
            transition,
            notice,
        }
    }
}

fn finish_spawn_effect(
    record: &mut LoopRunRecord,
    effect: SupervisedRunOutcome,
    check: Option<CheckRecord>,
    stream: bool,
) -> (LoopRunPresentation, TaskFireNotice) {
    record.check = check;
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
        SupervisedRunOutcome::Background => (LoopRunPresentation::default(), TaskFireNotice::None),
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

struct PreparedCheck {
    done: Option<TaskFireFinished>,
    fire: Option<FiredCheck>,
}

impl PreparedCheck {
    fn done(finished: TaskFireFinished) -> Self {
        Self {
            done: Some(finished),
            fire: None,
        }
    }

    fn fire(check: Option<FiredCheck>) -> Self {
        Self {
            done: None,
            fire: check,
        }
    }
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTaskSpec {
    kind: String,
    args: Vec<String>,
    model: Option<String>,
}

impl ResolvedTaskSpec {
    pub fn kind(&self) -> &str {
        &self.kind
    }
}

pub fn resolve_task_spec(
    spec: &str,
    workspace: &crate::workspace::ResolvedWorkspace,
) -> Result<ResolvedTaskSpec> {
    let machine_config = MachineConfig::load_lenient();
    let launch = crate::config::effective::load(
        &machine_config.agents,
        &workspace.project_root,
        &config_home(),
    )?;
    let layout = match agents_spec::resolve_spec(
        Some(spec),
        &launch.profiles,
        &machine_config.agents.commands,
        &launch.teams,
    ) {
        Ok(layout) => layout,
        Err(err @ agents_spec::LayoutErr::UnknownTeam { .. })
        | Err(err @ agents_spec::LayoutErr::UnknownCell { .. }) => {
            launch.block_untrusted_reference(Some(spec), &machine_config.agents.commands)?;
            return Err(err.into());
        }
        Err(err) => return Err(err.into()),
    };
    single_agent_cell(spec, &layout)
}

fn single_agent_cell(spec: &str, layout: &LayoutSpec) -> Result<ResolvedTaskSpec> {
    let cell_count: usize = layout.columns.iter().map(|column| column.rows.len()).sum();
    if cell_count != 1 {
        anyhow::bail!(
            "loop task `{spec}` must resolve to one agent; use a kind, profile, or virtual cell"
        );
    }
    let cell = &layout.columns[0].rows[0];
    let Cell::Agent(cell) = cell else {
        anyhow::bail!(
            "loop task `{spec}` must resolve to one agent; command cells are not supported"
        );
    };
    Ok(ResolvedTaskSpec {
        kind: cell.kind.as_str().to_owned(),
        args: cell.args.clone(),
        model: cell.launch.model.clone(),
    })
}

pub fn preflight_entry(action: &TaskAction, resolved: Option<&ResolvedTaskSpec>) -> Result<()> {
    match action {
        TaskAction::Spawn(spec) => {
            let resolved = resolved
                .with_context(|| format!("missing resolved loop task spec for `{spec}`"))?;
            preflight_resolved_task(resolved)?;
        }
        TaskAction::Deliver(target) => preflight_kind(&target.kind)?,
        TaskAction::CheckOnly => {}
    }
    Ok(())
}

fn preflight_resolved_task(resolved: &ResolvedTaskSpec) -> Result<()> {
    preflight_kind(&resolved.kind)
}

fn preflight_kind(kind: &str) -> Result<()> {
    let adapter =
        find_definition(kind).ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
    match preflight_hooks(adapter, TurnLifecycleNeed::NotUnsupported) {
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
    }
}

pub fn parse_mode(raw: &str) -> Result<String> {
    Ok(mode_name(parse_mode_value(raw)?).to_owned())
}

pub fn parse_mode_value(raw: &str) -> Result<PermissionMode> {
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
    super::parse_duration_units(raw, TASK_TIMEOUT_UNITS)
}

pub fn resolve_task_prompt(name: &str, entry: &TaskEntry) -> Result<String> {
    if let Some(prompt) = entry
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty())
    {
        return Ok(prompt.to_owned());
    }
    let Some(path) = entry.prompt_file.as_deref() else {
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

pub fn resolve_config_path(path: &Path) -> Result<PathBuf> {
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
    resolved: &ResolvedTaskSpec,
    config: &MachineConfig,
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
    let invocation = crate::harness::launch::ExecRequest {
        kind: crate::ids::AgentKind::new_unchecked(resolved.kind.clone()),
        action: crate::harness::launch::ExecAction::Launch {
            prompt: None,
            extra_args: resolved.args.clone(),
        },
        system_prompt_file: None,
        provider_account: crate::harness::launch::ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        identity: crate::harness::launch::ExecIdentity {
            params: launch,
            ..Default::default()
        },
    };
    let (_, managed_launch) = crate::harness::launch::compile_managed_agent_process(
        &workspace.project_root,
        config.harness.rtk,
        &invocation,
        &workspace.worktree_root,
        &ManagedLaunchState::PendingResolution,
    )?;
    Ok(managed_launch)
}

/// Newest durable supervised run still active for one loop task.
pub fn newest_active_run(paths: &StatePaths, name: &str) -> Result<Option<RunRecord>> {
    let mut records = crate::harness::run::list(paths)?;
    records
        .retain(|record| !record.status.is_terminal() && record.loop_task.as_deref() == Some(name));
    records.sort_by_key(|record| std::cmp::Reverse(record.started_at));
    Ok(records.into_iter().next())
}

/// Resolve the task workspace before selecting its newest active run.
pub fn newest_active_run_for_entry(name: &str, entry: &TaskEntry) -> Result<Option<RunRecord>> {
    let root = entry.resolved_root();
    let workspace = WorkspaceResolver::resolve(&root, None)
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    let paths = StatePaths::for_workspace(workspace.workspace_id)?;
    newest_active_run(&paths, name)
}

pub fn effective_spawn_timeout(
    mode: crate::harness::schedule::run_log::LoopRunMode,
    task_timeout: Option<Duration>,
    configured_timeout: Option<Duration>,
) -> Option<Duration> {
    task_timeout.or_else(|| {
        (mode == crate::harness::schedule::run_log::LoopRunMode::Scheduled)
            .then_some(configured_timeout.unwrap_or(SCHEDULED_RUN_DEFAULT_TIMEOUT))
    })
}

pub struct RunLockGuard {
    file: File,
}

impl Drop for RunLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLockInfo {
    pub pid: u32,
    pub started_at: Timestamp,
}

pub enum RunLockAttempt {
    Acquired(RunLockGuard),
    Held(Option<RunLockInfo>),
}

pub enum RunLockState {
    Available,
    Held(Option<RunLockInfo>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopAction {
    Done,
    CancelRun,
    Signal(RunLockInfo),
    Manual,
}

pub fn acquire_run_lock(name: &str, entry: &TaskEntry) -> Result<RunLockAttempt> {
    let path = run_lock_path(name, entry)?;
    let parent = path
        .parent()
        .context("loop run lock path has no runtime parent")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating loop task runtime for `{}`", path.display()))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening loop run lock `{}`", path.display()))?;
    acquire_run_lock_file(file, &path)
}

pub fn probe_run_lock(name: &str, entry: &TaskEntry) -> Result<RunLockState> {
    let path = run_lock_path(name, entry)?;
    probe_run_lock_path(&path)
}

fn probe_run_lock_path(path: &Path) -> Result<RunLockState> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RunLockState::Available);
        }
        Err(err) => {
            return Err(err).with_context(|| format!("opening loop run lock `{}`", path.display()));
        }
    };
    probe_run_lock_file(file, path)
}

pub fn run_lock_path(name: &str, entry: &TaskEntry) -> Result<PathBuf> {
    let runtime =
        RuntimePaths::for_workspace(WorkspaceId::from_project_root(&entry.resolved_root()))
            .context("locating loop task runtime")?;
    Ok(runtime.root.join(format!("loop-run-{name}.lock")))
}

pub fn next_stop_action(
    state: &RunLockState,
    run_found: bool,
    cancel_attempted: bool,
    signal_attempted: bool,
) -> StopAction {
    match state {
        RunLockState::Available => StopAction::Done,
        RunLockState::Held(_) if run_found && !cancel_attempted => StopAction::CancelRun,
        RunLockState::Held(Some(info)) if !signal_attempted => StopAction::Signal(*info),
        RunLockState::Held(_) => StopAction::Manual,
    }
}

pub fn signal_run_lock_holder(info: &RunLockInfo) -> Result<()> {
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

pub fn wait_for_run_lock_release(name: &str, entry: &TaskEntry, grace: Duration) -> Result<bool> {
    wait_for_run_lock_release_path(&run_lock_path(name, entry)?, grace)
}

fn wait_for_run_lock_release_path(path: &Path, grace: Duration) -> Result<bool> {
    let deadline = Instant::now() + grace;
    loop {
        if matches!(probe_run_lock_path(path)?, RunLockState::Available) {
            return Ok(true);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        std::thread::sleep(RUN_LOCK_RELEASE_POLL_INTERVAL.min(remaining));
    }
}

fn acquire_run_lock_file(mut file: File, path: &Path) -> Result<RunLockAttempt> {
    match file.try_lock() {
        Ok(()) => {
            let info = RunLockInfo {
                pid: std::process::id(),
                started_at: Timestamp::now(),
            };
            // Runtime scratch needs no fsync and stays on the locked fd:
            // renaming an atomic replacement would detach the advisory lock.
            file.set_len(0)
                .with_context(|| format!("truncating loop run lock `{}`", path.display()))?;
            file.rewind()
                .with_context(|| format!("rewinding loop run lock `{}`", path.display()))?;
            serde_json::to_writer(&mut file, &info)
                .with_context(|| format!("writing loop run lock `{}`", path.display()))?;
            file.flush()
                .with_context(|| format!("flushing loop run lock `{}`", path.display()))?;
            Ok(RunLockAttempt::Acquired(RunLockGuard { file }))
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            Ok(RunLockAttempt::Held(read_run_lock_info(&mut file)))
        }
        Err(err) => Err(std::io::Error::from(err))
            .with_context(|| format!("locking loop run lock `{}`", path.display())),
    }
}

fn probe_run_lock_file(mut file: File, path: &Path) -> Result<RunLockState> {
    match file.try_lock() {
        Ok(()) => Ok(RunLockState::Available),
        Err(std::fs::TryLockError::WouldBlock) => {
            Ok(RunLockState::Held(read_run_lock_info(&mut file)))
        }
        Err(err) => Err(std::io::Error::from(err))
            .with_context(|| format!("probing loop run lock `{}`", path.display())),
    }
}

fn read_run_lock_info(file: &mut File) -> Option<RunLockInfo> {
    let mut payload = Vec::new();
    file.read_to_end(&mut payload)
        .ok()
        .and_then(|_| serde_json::from_slice(&payload).ok())
}

pub struct CheckOutcome {
    passed: bool,
    timed_out: bool,
    output: String,
    code: Option<i32>,
}

impl CheckOutcome {
    pub fn passed(&self) -> bool {
        self.passed
    }
}

pub enum CheckEcho {
    Capture,
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
    }
}

fn deadline_expired_at(entry: &TaskEntry, now: Timestamp) -> bool {
    entry.deadline.is_some_and(|deadline| now >= deadline)
}

pub fn check_timeout(entry: &TaskEntry) -> Result<Option<Duration>> {
    entry
        .timeout
        .as_deref()
        .map(|raw| super::parse_duration_units(raw, TASK_TIMEOUT_UNITS))
        .transpose()
        .map_err(|err| anyhow::anyhow!("{err}"))
}

pub fn check_only_result(outcome: &CheckOutcome) -> LoopRunResult {
    if outcome.timed_out {
        LoopRunResult::TimedOut
    } else if outcome.passed {
        LoopRunResult::Completed
    } else {
        LoopRunResult::Failed
    }
}

pub fn polarity_fires(on: Option<CheckOn>, outcome: &CheckOutcome) -> bool {
    match on.unwrap_or_default() {
        CheckOn::Fail => !outcome.passed,
        CheckOn::Success => outcome.passed,
    }
}

pub fn augment_prompt(base: String, cmd: &str, outcome: &CheckOutcome) -> String {
    let status = if outcome.timed_out {
        "timeout".to_owned()
    } else {
        outcome
            .code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "signal".to_owned())
    };
    format!(
        "{base}\n\n--- check `{cmd}` exited {status} ---\n{}",
        outcome.output
    )
}

pub fn run_check(
    dir: &Path,
    cmd: &str,
    timeout: Duration,
    echo: CheckEcho,
) -> Result<CheckOutcome> {
    let prefix = match echo {
        CheckEcho::Capture => None,
        CheckEcho::Stream {
            announcement,
            prefix,
        } => {
            if let Some(announcement) = announcement {
                let mut out = anstream::AutoStream::auto(std::io::stdout().lock());
                out.write_all(announcement.as_bytes())?;
                out.flush()?;
            }
            Some(prefix)
        }
    };
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running loop check `{cmd}` in {}", dir.display()))?;
    let stdout = drain_pipe(
        child.stdout.take(),
        prefix
            .clone()
            .map(|prefix| PipeForward::new(PipeDestination::Stdout, prefix)),
    );
    let stderr = drain_pipe(
        child.stderr.take(),
        prefix.map(|prefix| PipeForward::new(PipeDestination::Stderr, prefix)),
    );
    let deadline = Instant::now() + timeout;
    let (status, timed_out) = loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("waiting for loop check `{cmd}`"))?
        {
            break (status, false);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child
                .wait()
                .with_context(|| format!("reaping timed-out loop check `{cmd}`"))?;
            break (status, true);
        }
        std::thread::sleep(CHECK_POLL_INTERVAL);
    };
    let mut output = stdout.join().unwrap_or_default();
    output.extend(stderr.join().unwrap_or_default());
    let output = crate::proc::tail_output(&output, CHECK_OUTPUT_CAP);
    Ok(CheckOutcome {
        passed: status.success() && !timed_out,
        timed_out,
        output,
        code: status.code(),
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

fn drain_pipe(
    pipe: Option<impl Read + Send + 'static>,
    mut forward: Option<PipeForward>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let mut chunk = [0; 8 * 1024];
            while let Ok(read) = pipe.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                let bytes = &chunk[..read];
                buf.extend_from_slice(bytes);
                if let Some(forward) = &mut forward {
                    forward.push(bytes);
                }
            }
        }
        if let Some(forward) = forward {
            forward.finish();
        }
        buf
    })
}

fn unresolved_managed_state(entry: &TaskEntry, kind: &str) -> ManagedLaunchState {
    let Some(adapter) = find_definition(kind) else {
        return ManagedLaunchState::Unsupported;
    };
    let state = adapter.resolve_managed_launch(
        &entry.resolved_root(),
        &std::collections::BTreeMap::new(),
        None,
        &[],
    );
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
            window_label(reading.duration_mins),
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
            window_label(reading.duration_mins),
            reading.headroom,
        )
    })
}

fn window_label(duration_mins: u32) -> String {
    if duration_mins.is_multiple_of(24 * 60) {
        format!("{}d", duration_mins / (24 * 60))
    } else if duration_mins.is_multiple_of(60) {
        format!("{}h", duration_mins / 60)
    } else {
        format!("{duration_mins}m")
    }
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
