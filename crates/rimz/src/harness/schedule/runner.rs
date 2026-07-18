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
    HookPreflightErr, LongestWindowSignal, ProviderAccountBinding, ProviderCapacity,
    TurnLifecycleNeed, WindowSurplus, find_adapter, preflight_hooks,
};
use crate::config::{CheckOn, MachineConfig, TaskEntry, TaskTarget};
use crate::harness::assist_log::AssistWindowReset;
use crate::harness::run::{PermissionMode, RunRecord, SupervisedRunOutcome, SupervisedRunRequest};
use crate::harness::schedule::catalog::{self, TaskCatalog};
use crate::harness::schedule::run_log::{
    self, CheckRecord, LoopRunMode, LoopRunPresentation, LoopRunRecord, LoopRunResult,
    PingWindowOutcome, RunTransition,
};
use crate::harness::schedule::{ResetSignal, TaskAction};
use crate::harness::spec::{self as agents_spec, Cell, LayoutSpec};
use crate::ids::WorkspaceId;
use crate::store::paths::{RuntimePaths, StatePaths, config_home, runtime_home, state_home};
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
    PingWindow { kind: String },
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
    workspace_id: WorkspaceId,
    scope_runtime: RuntimePaths,
    resolved: Option<ResolvedTaskSpec>,
    provider_account_binding: Option<ProviderAccountBinding>,
    capacity: OnceCell<Option<ProviderCapacity>>,
    #[cfg(test)]
    capacity_reads: std::cell::Cell<usize>,
    #[cfg(test)]
    capacity_runtime: Option<RuntimePaths>,
}

impl FireScope {
    fn new(
        kind: crate::ids::AgentKind,
        workspace_id: WorkspaceId,
        scope_runtime: RuntimePaths,
        resolved: Option<ResolvedTaskSpec>,
    ) -> Self {
        Self {
            kind,
            workspace_id,
            scope_runtime,
            resolved,
            provider_account_binding: None,
            capacity: OnceCell::new(),
            #[cfg(test)]
            capacity_reads: std::cell::Cell::new(0),
            #[cfg(test)]
            capacity_runtime: None,
        }
    }

    fn capacity(&self) -> Result<Option<&ProviderCapacity>> {
        if self.capacity.get().is_none() {
            #[cfg(test)]
            self.capacity_reads.set(self.capacity_reads.get() + 1);
            let runtime = self.capacity_runtime()?;
            let capacity = match self.provider_account_binding.as_ref() {
                Some(binding) => {
                    ProviderCapacity::read_bound(&runtime, self.kind.as_str(), binding)
                }
                None if self.kind.as_str() == "qwen" => None,
                None => ProviderCapacity::read(&runtime, self.kind.as_str()),
            };
            let _ = self.capacity.set(capacity);
        }
        Ok(self.capacity.get().and_then(Option::as_ref))
    }

    fn capacity_runtime(&self) -> Result<RuntimePaths> {
        #[cfg(test)]
        if let Some(runtime) = self.capacity_runtime.clone() {
            return Ok(runtime);
        }
        RuntimePaths::for_workspace(self.workspace_id.clone()).map_err(Into::into)
    }

    fn surplus_gate(&self, entry: &TaskEntry, now: Timestamp) -> Result<Option<String>> {
        if entry.surplus.is_none() && entry.surplus_after.is_none() {
            return Ok(None);
        }
        Ok(surplus_gate_in(
            entry,
            self.kind.as_str(),
            self.capacity()?
                .and_then(|capacity| capacity.longest_window_surplus(now)),
        ))
    }

    #[cfg(test)]
    fn window_running(&self, longest: bool, now: Timestamp) -> Result<bool> {
        Ok(self.window_running_state(longest, now)? == Some(true))
    }

    fn window_running_state(&self, longest: bool, now: Timestamp) -> Result<Option<bool>> {
        Ok(self.capacity()?.and_then(|capacity| {
            if longest {
                capacity.longest_window_running(now)
            } else {
                capacity.shortest_window_running(now)
            }
        }))
    }

    fn ping_gate_reason(&self, entry: &TaskEntry, now: Timestamp) -> Result<Option<String>> {
        let kind = self.kind.as_str();
        let binding_state = if kind == "qwen" {
            let Some(binding) = self.provider_account_binding.as_ref() else {
                return Ok(qwen_ping_cache_reason(false, None, None));
            };
            let runtime = self.capacity_runtime()?;
            let state = ProviderCapacity::binding_cache_matches(&runtime, kind, binding);
            if state == Some(false) {
                return Ok(qwen_ping_cache_reason(true, state, None));
            }
            state
        } else {
            None
        };
        let running = self.window_running_state(entry.every.as_deref() == Some("reset"), now)?;
        if kind == "qwen" {
            return Ok(qwen_ping_cache_reason(true, binding_state, running));
        }
        if entry.every.as_deref() == Some("reset")
            && self
                .capacity()?
                .is_some_and(ProviderCapacity::longest_window_lifted)
        {
            return Ok(Some(format!("{kind} budget window is not enforced")));
        }
        Ok((running == Some(true)).then(|| format!("{kind} budget window already counting down")))
    }

    fn refreshed_ping_window(&self) -> Option<PingWindowOutcome> {
        let prior = self.capacity().ok().flatten().and_then(ping_window_outcome);
        let runtime = self.capacity_runtime().ok()?;
        let fresh = capacity_for(
            &runtime,
            self.kind.as_str(),
            self.provider_account_binding.as_ref(),
        )
        .as_ref()
        .and_then(ping_window_outcome)?;
        (prior.as_ref() != Some(&fresh)).then_some(fresh)
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
        let workspace_id = workspace.workspace_id.clone();
        let runtime = RuntimePaths::for_workspace(workspace_id.clone())?;
        let scope = match &action {
            TaskAction::Spawn(spec) => {
                let resolved = resolve_task_spec(spec, &workspace)?;
                let provider_account_binding =
                    resolve_managed_spawn_binding(entry, &workspace, &resolved, config)?;
                let mut scope = FireScope::new(
                    crate::ids::AgentKind::new_unchecked(resolved.kind.clone()),
                    workspace_id,
                    runtime,
                    Some(resolved),
                );
                scope.provider_account_binding = provider_account_binding;
                scope
            }
            TaskAction::Deliver(target) => FireScope::new(
                crate::ids::AgentKind::new_unchecked(target.kind.clone()),
                workspace_id,
                runtime,
                None,
            ),
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
    entry: TaskEntry,
    catalog: &'a TaskCatalog,
    action: Option<TaskAction>,
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
        entry: TaskEntry,
        catalog: &'a TaskCatalog,
        mode: LoopRunMode,
        keep: bool,
        now: Timestamp,
        config: Arc<MachineConfig>,
        check_echo: CheckEcho,
        started: Instant,
    ) -> Result<Self> {
        let name = name.into();
        let action = TaskAction::from_entry(&name, &entry)?;
        Ok(Self {
            name,
            entry,
            catalog,
            action: Some(action),
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
            && let Some(binding) = scope.provider_account_binding.as_ref()
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
            && let Some(reason) = scope.surplus_gate(&self.entry, self.now)?
        {
            return Ok(Some(
                self.record_gate(LoopRunResult::SurplusSkipped, reason),
            ));
        }
        if let Some(scope) = &context.scope
            && scope
                .resolved
                .as_ref()
                .is_some_and(|resolved| resolved.is_ping)
            && let Some(reason) = scope.ping_gate_reason(&self.entry, self.now)?
        {
            let kind = scope.kind.as_str().to_owned();
            return Ok(Some(self.record_terminal_with(
                LoopRunResult::SkippedWindow,
                LoopRunPresentation {
                    skip_reason: Some(reason),
                    ..LoopRunPresentation::default()
                },
                TaskFireNotice::PingWindow { kind },
                Some(self.now),
                |_| {},
            )));
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
                if record.result == LoopRunResult::Completed
                    && let Some(scope) = self
                        .context
                        .as_ref()
                        .and_then(|context| context.scope.as_ref())
                    && scope
                        .resolved
                        .as_ref()
                        .is_some_and(|resolved| resolved.is_ping)
                {
                    record.window = scope.refreshed_ping_window();
                }
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
            if self.mode == LoopRunMode::Scheduled && catalog::is_ephemeral(&self.entry) {
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
        let (resolved_kind, is_ping, provider_account_binding) = {
            let scope = self
                .context
                .as_ref()
                .and_then(|context| context.scope.as_ref())
                .context("loop spawn context missing provider scope")?;
            let resolved = scope
                .resolved
                .as_ref()
                .context("loop spawn context missing resolved task spec")?;
            preflight_resolved_task(&spec, resolved)?;
            (
                resolved.kind.clone(),
                resolved.is_ping,
                scope.provider_account_binding.clone(),
            )
        };
        let prompt = self.resolve_effect_prompt(fired_check.as_ref())?;
        let request = self.compile_spawn_request(
            spec,
            prompt,
            is_ping,
            &resolved_kind,
            provider_account_binding.as_ref(),
        )?;
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
        is_ping: bool,
        resolved_kind: &str,
        provider_account_binding: Option<&ProviderAccountBinding>,
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
        let effort = self.entry.effort.clone().or_else(|| {
            (is_ping
                && find_adapter(resolved_kind)
                    .is_some_and(|adapter| adapter.descriptor().launch.presets.effort.is_some()))
            .then(|| "low".to_owned())
        });
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
            permission_mode,
            model: None,
            system_prompt_file,
            append_system_prompt_file: None,
            effort,
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
            managed_provider_binding: provider_account_binding.cloned(),
            managed_provider_binding_resolved: resolved_kind == "qwen",
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
        if self.mode == LoopRunMode::Scheduled && catalog::is_ephemeral(&self.entry) {
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
        let transition = run_log::record_transition(&self.name, &self.entry, &record);
        self.finished = true;
        TaskFireFinished {
            record,
            presentation,
            transition,
            notice,
        }
    }
}

fn ping_window_outcome(capacity: &ProviderCapacity) -> Option<PingWindowOutcome> {
    let shortest = capacity
        .duration_windows()
        .min_by_key(|window| window.duration_mins)
        .map(assist_window_reset);
    let longest = capacity
        .duration_windows()
        .max_by_key(|window| window.duration_mins)
        .map(assist_window_reset);
    (shortest.is_some() || longest.is_some()).then_some(PingWindowOutcome { shortest, longest })
}

fn assist_window_reset(window: &crate::agents::RateLimitWindow) -> AssistWindowReset {
    AssistWindowReset {
        duration_mins: window.duration_mins.map(u64::from),
        resets_at: window.resets_at,
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
    is_ping: bool,
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
        is_ping: agents_spec::virtual_ping_shape(spec),
    })
}

pub fn ping_kind_supported(kind: &str) -> Result<()> {
    let adapter =
        find_adapter(kind).ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
    if adapter.ping_args().is_none() {
        anyhow::bail!(
            "agent kind `{kind}` does not support a ping turn; use `claude`, `codex`, or `qwen`"
        );
    }
    Ok(())
}

pub fn preflight_entry(
    name: &str,
    entry: &TaskEntry,
    resolved: Option<&ResolvedTaskSpec>,
) -> Result<()> {
    match TaskAction::from_entry(name, entry)? {
        TaskAction::Spawn(spec) => {
            let resolved = resolved
                .with_context(|| format!("missing resolved loop task spec for `{spec}`"))?;
            preflight_resolved_task(&spec, resolved)?;
        }
        TaskAction::Deliver(target) => preflight_kind(&target.kind)?,
        TaskAction::CheckOnly => {}
    }
    Ok(())
}

fn preflight_resolved_task(spec: &str, resolved: &ResolvedTaskSpec) -> Result<()> {
    if agents_spec::virtual_ping_shape(spec) {
        ping_kind_supported(&resolved.kind)?;
    }
    preflight_kind(&resolved.kind)
}

fn preflight_kind(kind: &str) -> Result<()> {
    let adapter =
        find_adapter(kind).ok_or_else(|| anyhow::anyhow!("unknown agent kind `{kind}`"))?;
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

fn resolve_managed_spawn_binding(
    entry: &TaskEntry,
    workspace: &crate::workspace::ResolvedWorkspace,
    resolved: &ResolvedTaskSpec,
    config: &MachineConfig,
) -> Result<Option<ProviderAccountBinding>> {
    if resolved.kind != "qwen" || entry.worktree.is_some() {
        return Ok(None);
    }
    let adapter = find_adapter(&resolved.kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", resolved.kind))?;
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
    let process = crate::harness::launch::compile_agent_process(
        &workspace.project_root,
        config.harness.rtk,
        &invocation,
        &workspace.worktree_root,
    )?;
    Ok(adapter.resolve_managed_launch(
        &workspace.worktree_root,
        &crate::harness::launch::effective_launch_env(&process.env),
        resolved.model.as_deref(),
        &process.provider_argv,
    ))
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

pub fn deadline_expired(entry: &TaskEntry) -> bool {
    deadline_expired_at(entry, Timestamp::now())
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
    let output = tail_output(&output, CHECK_OUTPUT_CAP);
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

pub fn tail_output(bytes: &[u8], cap: usize) -> String {
    let start = bytes.len().saturating_sub(cap);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// Whether `entry`'s provider already has a budget window counting down, read
/// from the shared account-scoped cache. The window state is account-scoped, so
/// the entry's workspace is resolved only to reach this user's runtime root.
pub fn window_already_running(
    entry: &TaskEntry,
    kind: &str,
    binding: Option<&ProviderAccountBinding>,
) -> Result<Option<bool>> {
    let runtime = entry_runtime(entry)?;
    Ok(capacity_for(&runtime, kind, binding)
        .and_then(|capacity| capacity.shortest_window_running(Timestamp::now())))
}

/// Whether `entry`'s provider already has its longest budget window counting
/// down, read from the shared account-scoped cache.
pub fn reset_window_already_running(
    entry: &TaskEntry,
    kind: &str,
    binding: Option<&ProviderAccountBinding>,
) -> Result<Option<bool>> {
    let runtime = entry_runtime(entry)?;
    Ok(capacity_for(&runtime, kind, binding)
        .and_then(|capacity| capacity.longest_window_running(Timestamp::now())))
}

pub fn binding_cache_matches(
    entry: &TaskEntry,
    kind: &str,
    binding: &ProviderAccountBinding,
) -> Result<Option<bool>> {
    let runtime = entry_runtime(entry)?;
    Ok(ProviderCapacity::binding_cache_matches(
        &runtime, kind, binding,
    ))
}

/// Resolve the exact provider account a fresh ping task would launch with.
pub fn managed_ping_binding(entry: &TaskEntry, kind: &str) -> Option<ProviderAccountBinding> {
    if kind != "qwen" || entry.worktree.is_some() {
        return None;
    }
    let config = MachineConfig::load_lenient();
    let result = (|| -> Result<Option<ProviderAccountBinding>> {
        let workspace = WorkspaceResolver::resolve(entry.resolved_root(), None)?;
        let resolved = resolve_task_spec("qwen-ping", &workspace)?;
        if resolved.kind() != kind {
            return Ok(None);
        }
        resolve_managed_spawn_binding(entry, &workspace, &resolved, &config)
    })();
    result
        .inspect_err(|err| {
            tracing::debug!(error = %err, "Qwen ping account binding skipped: launch resolution failed");
        })
        .ok()
        .flatten()
}

fn qwen_ping_cache_reason(
    binding_resolved: bool,
    binding_state: Option<bool>,
    running: Option<bool>,
) -> Option<String> {
    if !binding_resolved {
        return Some("Qwen launch has no exact Alibaba account binding".to_owned());
    }
    match (binding_state, running) {
        (Some(false), _) => {
            Some("Qwen cached quota belongs to a different Alibaba account".to_owned())
        }
        (Some(true), None) => Some("matching Qwen budget-window reading is incomplete".to_owned()),
        (_, Some(true)) => Some("qwen budget window already counting down".to_owned()),
        _ => None,
    }
}

/// Decide whether a task's provider-window surplus gate keeps this fire closed.
pub fn surplus_gate(
    entry: &TaskEntry,
    kind: &str,
    binding: Option<&ProviderAccountBinding>,
    now: Timestamp,
) -> Result<Option<String>> {
    if entry.surplus.is_none() && entry.surplus_after.is_none() {
        return Ok(None);
    }
    let runtime = entry_runtime(entry)?;
    Ok(surplus_gate_in(
        entry,
        kind,
        capacity_for(&runtime, kind, binding)
            .and_then(|capacity| capacity.longest_window_surplus(now)),
    ))
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

pub(crate) fn reset_signal(capacity: Option<&ProviderCapacity>, now: Timestamp) -> ResetSignal {
    match capacity.map(|capacity| capacity.longest_window_signal(now)) {
        Some(LongestWindowSignal::At(reset_at)) => ResetSignal::At(reset_at),
        Some(LongestWindowSignal::ConfirmedDown) => ResetSignal::ConfirmedDown,
        Some(LongestWindowSignal::Unknown) | None => ResetSignal::Unknown,
    }
}

pub(crate) fn reset_signal_for(
    runtime: &RuntimePaths,
    kind: &str,
    binding: Option<&ProviderAccountBinding>,
    now: Timestamp,
) -> ResetSignal {
    let capacity = capacity_for(runtime, kind, binding);
    reset_signal(capacity.as_ref(), now)
}

/// Reset signal for `entry`'s provider longest budget window.
pub fn window_reset_signal(
    entry: &TaskEntry,
    kind: &str,
    binding: Option<&ProviderAccountBinding>,
    now: Timestamp,
) -> Result<ResetSignal> {
    let runtime = entry_runtime(entry)?;
    Ok(reset_signal_for(&runtime, kind, binding, now))
}

fn capacity_for(
    runtime: &RuntimePaths,
    kind: &str,
    binding: Option<&ProviderAccountBinding>,
) -> Option<ProviderCapacity> {
    match binding {
        Some(binding) => ProviderCapacity::read_bound(runtime, kind, binding),
        None if kind == "qwen" => None,
        None => ProviderCapacity::read(runtime, kind),
    }
}

fn entry_runtime(entry: &TaskEntry) -> Result<RuntimePaths> {
    let root = entry.resolved_root();
    let workspace = WorkspaceResolver::resolve(&root, None)
        .with_context(|| format!("resolving project root at {}", root.display()))?;
    RuntimePaths::under(workspace.workspace_id, &runtime_home()).context("locating runtime")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduled_spawn_timeout_prefers_task_then_config_then_builtin() {
        let task = Duration::from_secs(30);
        let configured = Duration::from_secs(60);
        assert_eq!(
            effective_spawn_timeout(LoopRunMode::Scheduled, Some(task), Some(configured)),
            Some(task)
        );
        assert_eq!(
            effective_spawn_timeout(LoopRunMode::Scheduled, None, Some(configured)),
            Some(configured)
        );
        assert_eq!(
            effective_spawn_timeout(LoopRunMode::Scheduled, None, None),
            Some(SCHEDULED_RUN_DEFAULT_TIMEOUT)
        );
        assert_eq!(
            effective_spawn_timeout(LoopRunMode::Manual, None, Some(configured)),
            None
        );
    }

    #[test]
    fn supervised_budget_refusal_finishes_as_a_recorded_gate() {
        let check = CheckRecord {
            code: Some(1),
            timed_out: false,
            output: "guard failed".to_owned(),
        };

        let mut record = LoopRunRecord::new(
            "nightly",
            LoopRunResult::Completed,
            LoopRunMode::Scheduled,
            12,
        );
        let (presentation, notice) = finish_spawn_effect(
            &mut record,
            SupervisedRunOutcome::BudgetExceeded {
                reason: "room budget reached".to_owned(),
            },
            Some(check.clone()),
            true,
        );

        assert_eq!(record.result, LoopRunResult::BudgetSkipped);
        assert_eq!(record.check.as_ref(), Some(&check));
        assert_eq!(record.error.as_deref(), Some("room budget reached"));
        assert_eq!(presentation, LoopRunPresentation::default());
        assert!(matches!(
            notice,
            TaskFireNotice::Gate { reason } if reason == "room budget reached"
        ));
    }

    #[test]
    fn deadline_and_lock_age_use_injected_time() {
        let now = Timestamp::from_second(200_000).expect("now");
        let entry = TaskEntry {
            deadline: Some(Timestamp::from_second(199_999).expect("deadline")),
            ..TaskEntry::default()
        };
        assert!(deadline_expired_at(&entry, now));
        assert_eq!(
            relative_age(Timestamp::from_second(198_500).expect("started"), now),
            "25m ago"
        );
    }

    #[test]
    fn ping_window_gate_distinguishes_cold_and_active_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime paths");
        runtime.ensure_dirs().expect("runtime dirs");
        let now = Timestamp::from_second(1_000_000).expect("now");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let mut cold = FireScope::new(
            crate::ids::AgentKind::new_unchecked("claude"),
            workspace_id.clone(),
            runtime.clone(),
            None,
        );
        cold.capacity_runtime = Some(runtime.clone());
        assert!(!cold.window_running(false, now).expect("cold window"));

        let cache = crate::agents::account::RateLimitsCache {
            entries: std::collections::BTreeMap::from([(
                "claude".to_owned(),
                crate::agents::account::RateLimitCacheEntry {
                    limits: crate::agents::AgentRateLimits {
                        windows: vec![crate::agents::RateLimitWindow {
                            used_percentage: Some(1),
                            resets_at: Some(now + jiff::SignedDuration::from_secs(300)),
                            duration_mins: Some(300),
                            ..Default::default()
                        }],
                    },
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        crate::store::atomic::write_temp_then_rename_cache(
            &runtime.shared_rate_limits_path(),
            &cache,
        )
        .expect("rate-limit cache");
        let mut active = FireScope::new(
            crate::ids::AgentKind::new_unchecked("claude"),
            workspace_id.clone(),
            runtime.clone(),
            None,
        );
        active.capacity_runtime = Some(runtime.clone());
        let entry = TaskEntry {
            surplus: Some("0.5".to_owned()),
            ..TaskEntry::default()
        };
        assert_eq!(
            active.surplus_gate(&entry, now).expect("surplus gate"),
            None
        );
        assert!(active.window_running(false, now).expect("active window"));
        assert_eq!(active.capacity_reads.get(), 1);

        let lifted_cache = crate::agents::account::RateLimitsCache {
            entries: std::collections::BTreeMap::from([(
                "claude".to_owned(),
                crate::agents::account::RateLimitCacheEntry {
                    limits: crate::agents::AgentRateLimits {
                        windows: vec![crate::agents::RateLimitWindow {
                            duration_mins: Some(7 * 24 * 60),
                            source: crate::agents::context::WindowSource::Authoritative,
                            lifted: true,
                            ..Default::default()
                        }],
                    },
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        crate::store::atomic::write_temp_then_rename_cache(
            &runtime.shared_rate_limits_path(),
            &lifted_cache,
        )
        .expect("lifted rate-limit cache");
        let mut lifted = FireScope::new(
            crate::ids::AgentKind::new_unchecked("claude"),
            workspace_id,
            runtime.clone(),
            None,
        );
        lifted.capacity_runtime = Some(runtime);
        let reset_ping = TaskEntry {
            every: Some("reset".to_owned()),
            ..TaskEntry::default()
        };
        assert_eq!(
            lifted
                .ping_gate_reason(&reset_ping, now)
                .expect("lifted reset window needs no primer"),
            Some("claude budget window is not enforced".to_owned())
        );
    }

    #[test]
    fn ping_window_outcome_selects_shortest_and_longest_duration() {
        let capacity = ProviderCapacity::from_windows(vec![
            crate::agents::RateLimitWindow {
                duration_mins: Some(10_080),
                resets_at: Some(Timestamp::from_second(20_000).unwrap()),
                ..Default::default()
            },
            crate::agents::RateLimitWindow {
                duration_mins: Some(300),
                resets_at: Some(Timestamp::from_second(10_000).unwrap()),
                ..Default::default()
            },
        ]);

        let outcome = ping_window_outcome(&capacity).expect("window outcome");
        assert_eq!(outcome.shortest.unwrap().duration_mins, Some(300));
        assert_eq!(outcome.longest.unwrap().duration_mins, Some(10_080));
    }

    #[test]
    fn run_lock_reports_holder_metadata_and_accepts_empty_legacy_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing_path = dir.path().join("missing.lock");
        assert!(matches!(
            probe_run_lock_path(&missing_path).expect("probe missing lock"),
            RunLockState::Available
        ));
        assert!(
            !missing_path.exists(),
            "probing should not create a lock file"
        );
        let path = dir.path().join("task.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .expect("open lock");

        let guard = match acquire_run_lock_file(file, &path).expect("acquire lock") {
            RunLockAttempt::Acquired(guard) => guard,
            RunLockAttempt::Held(_) => panic!("fresh lock should be acquired"),
        };
        let written: RunLockInfo =
            serde_json::from_slice(&std::fs::read(&path).expect("read lock"))
                .expect("parse lock info");
        assert_eq!(written.pid, std::process::id());

        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open contender");
        match acquire_run_lock_file(contender, &path).expect("contend for lock") {
            RunLockAttempt::Held(Some(info)) => assert_eq!(info, written),
            RunLockAttempt::Held(None) => panic!("holder metadata should be readable"),
            RunLockAttempt::Acquired(_) => panic!("held lock should reject contender"),
        }
        drop(guard);
        let before_probe = std::fs::read(&path).expect("read lock before probe");
        let probe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open probe");
        assert!(matches!(
            probe_run_lock_file(probe, &path).expect("probe available lock"),
            RunLockState::Available
        ));
        assert_eq!(
            std::fs::read(&path).expect("read lock after probe"),
            before_probe,
            "probing an available lock should not rewrite its metadata"
        );

        let empty_path = dir.path().join("legacy.lock");
        let empty = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&empty_path)
            .expect("open empty lock");
        empty.try_lock().expect("hold empty lock");
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&empty_path)
            .expect("open empty contender");
        assert!(matches!(
            probe_run_lock_file(contender, &empty_path).expect("probe empty lock"),
            RunLockState::Held(None)
        ));
    }

    #[test]
    fn stop_ladder_cancels_then_signals_then_reports_manual_recovery() {
        let info = RunLockInfo {
            pid: 42,
            started_at: Timestamp::from_second(1).expect("timestamp"),
        };
        assert_eq!(
            next_stop_action(&RunLockState::Available, true, false, false),
            StopAction::Done
        );
        assert_eq!(
            next_stop_action(&RunLockState::Held(Some(info)), true, false, false),
            StopAction::CancelRun
        );
        assert_eq!(
            next_stop_action(&RunLockState::Held(Some(info)), true, true, false),
            StopAction::Signal(info)
        );
        assert_eq!(
            next_stop_action(&RunLockState::Held(Some(info)), true, true, true),
            StopAction::Manual
        );
        assert_eq!(
            next_stop_action(&RunLockState::Held(None), false, false, false),
            StopAction::Manual
        );
    }

    #[test]
    fn wait_for_run_lock_release_observes_guard_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("task.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .expect("open lock");
        let guard = match acquire_run_lock_file(file, &path).expect("acquire lock") {
            RunLockAttempt::Acquired(guard) => guard,
            RunLockAttempt::Held(_) => panic!("fresh lock should be acquired"),
        };
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            drop(guard);
        });

        assert!(
            wait_for_run_lock_release_path(&path, Duration::from_secs(1))
                .expect("wait for release")
        );
        releaser.join().expect("release thread");
    }

    fn surplus_entry(surplus: Option<&str>, surplus_after: Option<&str>) -> TaskEntry {
        TaskEntry {
            surplus: surplus.map(ToOwned::to_owned),
            surplus_after: surplus_after.map(ToOwned::to_owned),
            ..TaskEntry::default()
        }
    }

    fn reading(elapsed_days: i64, headroom: f64) -> WindowSurplus {
        WindowSurplus {
            duration_mins: 7 * 24 * 60,
            elapsed: jiff::SignedDuration::from_secs(elapsed_days * 86_400),
            headroom,
        }
    }

    #[test]
    fn surplus_gate_covers_closed_elapsed_headroom_and_open_branches() {
        assert_eq!(surplus_gate_in(&TaskEntry::default(), "claude", None), None);
        assert_eq!(
            surplus_gate_in(&surplus_entry(Some("1.5x"), None), "claude", None).as_deref(),
            Some("no claude budget-window reading; surplus gate stays closed")
        );
        assert_eq!(
            surplus_gate_in(
                &surplus_entry(Some("1.5x"), Some("3d")),
                "claude",
                Some(reading(2, 2.0)),
            )
            .as_deref(),
            Some("claude 7d window 2d elapsed; fires after 3d")
        );
        assert_eq!(
            surplus_gate_in(
                &surplus_entry(Some("1.5x"), Some("3d")),
                "claude",
                Some(reading(4, 1.4)),
            )
            .as_deref(),
            Some("claude 7d window surplus 1.4x below 1.5x")
        );
        assert_eq!(
            surplus_gate_in(
                &surplus_entry(Some("1.5x"), Some("3d")),
                "claude",
                Some(reading(4, 1.5)),
            ),
            None
        );
    }

    #[test]
    fn surplus_after_alone_implies_sustainable_headroom() {
        assert_eq!(
            surplus_gate_in(
                &surplus_entry(None, Some("3d")),
                "codex",
                Some(reading(4, 0.9)),
            )
            .as_deref(),
            Some("codex 7d window surplus 0.9x below 1.0x")
        );
    }

    #[test]
    fn check_polarity_truth_table() {
        let passed = CheckOutcome {
            passed: true,
            timed_out: false,
            output: String::new(),
            code: Some(0),
        };
        let failed = CheckOutcome {
            passed: false,
            timed_out: false,
            output: String::new(),
            code: Some(1),
        };
        let timed_out = CheckOutcome {
            passed: false,
            timed_out: true,
            output: String::new(),
            code: None,
        };

        assert!(!polarity_fires(Some(CheckOn::Fail), &passed));
        assert!(polarity_fires(Some(CheckOn::Fail), &failed));
        assert!(polarity_fires(Some(CheckOn::Fail), &timed_out));
        assert!(polarity_fires(Some(CheckOn::Success), &passed));
        assert!(!polarity_fires(Some(CheckOn::Success), &failed));
        assert!(!polarity_fires(Some(CheckOn::Success), &timed_out));
    }

    #[test]
    fn run_check_captures_output_and_status() {
        let dir = tempfile::tempdir().expect("tempdir");

        let passed = run_check(
            dir.path(),
            "printf out; printf err >&2",
            Duration::from_secs(1),
            CheckEcho::Capture,
        )
        .expect("passed check");
        assert!(passed.passed);
        assert_eq!(passed.code, Some(0));
        assert!(passed.output.contains("out"));
        assert!(passed.output.contains("err"));

        let failed = run_check(
            dir.path(),
            "printf nope; exit 1",
            Duration::from_secs(1),
            CheckEcho::Capture,
        )
        .expect("failed check");
        assert!(!failed.passed);
        assert!(!failed.timed_out);
        assert_eq!(failed.code, Some(1));
        assert!(failed.output.contains("nope"));
    }

    #[test]
    fn run_check_honours_timeout() {
        let dir = tempfile::tempdir().expect("tempdir");

        let outcome = run_check(
            dir.path(),
            "sleep 1",
            Duration::from_millis(50),
            CheckEcho::Capture,
        )
        .expect("timed-out check");

        assert!(!outcome.passed);
        assert!(outcome.timed_out);
    }

    #[test]
    fn pipe_forward_buffers_partial_lines_and_terminates_the_tail() {
        let mut pending = b"first".to_vec();
        assert_eq!(take_complete_line(&mut pending), None);

        pending.extend_from_slice(b" line\nsecond");
        assert_eq!(
            take_complete_line(&mut pending),
            Some(b"first line\n".to_vec())
        );
        assert_eq!(pending, b"second");
        assert_eq!(take_trailing_line(&mut pending), Some(b"second\n".to_vec()));
        assert!(pending.is_empty());
    }
}
