//! Supervised-run requests, records, transitions, and cancellation.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::{AgentLifecycleObservation, LifecycleSignal, PermissionMode, TurnPhase};
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, AgentSessionId, PaneId, RunId, WorkspaceId};
use crate::store::lock::WorkspaceLock;
use crate::store::run_store;
use crate::store::{StatePaths, Store, snapshot::SidebarSnapshot};

const FAILURE_TAIL_CAP: usize = 4 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum RunStoreErr {
    #[error("run {0} not found")]
    NotFound(RunId),
    #[error(transparent)]
    Atomic(#[from] crate::store::atomic::AtomicErr),
    #[error(transparent)]
    Lock(#[from] crate::store::lock::LockErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("json parse error on {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("run {run_id} is {actual}; expected {expected}")]
    InvalidStatus {
        run_id: RunId,
        actual: &'static str,
        expected: &'static str,
    },
}

pub type Result<T> = std::result::Result<T, RunStoreErr>;

/// Typed cancellation signal shared between CLI signal handlers and the
/// supervised-run waiter.
#[derive(Clone, Debug, Default)]
pub struct RunCancellation {
    requested: Arc<AtomicBool>,
}

/// Command-neutral input for one supervised turn.
#[derive(Clone, Debug)]
pub struct SupervisedRunRequest {
    pub spec: String,
    pub prompt: String,
    pub description: Option<String>,
    pub worktree: Option<String>,
    pub from_pr: Option<crate::forge::PrTarget>,
    pub channel: Option<String>,
    pub name: Option<String>,
    pub background: bool,
    /// Let the in-pane wrapper reclaim the provider and pane from the durable
    /// run outcome, independently of the launching process.
    pub self_cleanup_on_completion: bool,
    /// Apply provider-native delegation restrictions to a `rimz subagents`
    /// child.
    pub subagent: bool,
    /// Queue the settled outcome to the launching agent as a `SUBAGENT_REPORT`;
    /// set by `rimz subagents` when the caller does not join.
    pub report_to_parent: bool,
    pub force_new_tab: bool,
    pub permission_mode: PermissionMode,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub system_prompt_file: Option<PathBuf>,
    pub append_system_prompt_files: Vec<PathBuf>,
    pub effort: Option<String>,
    pub budget: Option<crate::harness::budget::BudgetSpec>,
    pub max_turns: Option<u32>,
    pub timeout: Option<std::time::Duration>,
    pub keep: bool,
    pub retries: u32,
    pub verify: Option<String>,
    pub max_attempts: Option<u32>,
    pub loop_zone: bool,
    pub loop_task: Option<String>,
    pub passthrough: Vec<String>,
    /// Managed-account state: pending inputs, unsupported selection, unresolved
    /// exact selection, or one proven binding.
    pub managed_launch: crate::agents::ManagedLaunchState,
}

impl SupervisedRunRequest {
    pub fn new(
        spec: String,
        prompt: String,
        permission_mode: PermissionMode,
        managed_launch: crate::agents::ManagedLaunchState,
    ) -> Self {
        Self {
            spec,
            prompt,
            description: None,
            worktree: None,
            from_pr: None,
            channel: None,
            name: None,
            background: false,
            self_cleanup_on_completion: false,
            subagent: false,
            report_to_parent: false,
            force_new_tab: false,
            permission_mode,
            agent: None,
            model: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            effort: None,
            budget: None,
            max_turns: None,
            timeout: None,
            keep: false,
            retries: 0,
            verify: None,
            max_attempts: None,
            loop_zone: false,
            loop_task: None,
            passthrough: Vec::new(),
            managed_launch,
        }
    }
}

/// Command-neutral result of attempting one supervised turn.
#[derive(Debug)]
pub enum SupervisedRunOutcome {
    Record(Box<RunRecord>),
    Background { agent_name: String, run_id: RunId },
    BudgetExceeded { reason: String },
}

impl RunCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
    }

    pub fn reset(&self) {
        self.requested.store(false, Ordering::SeqCst);
    }

    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// Shared flag registered by the CLI's OS signal effect.
    pub fn signal_flag(&self) -> Arc<AtomicBool> {
        self.requested.clone()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CancelRunErr {
    #[error(transparent)]
    Store(#[from] RunStoreErr),
    #[error(transparent)]
    Wake(#[from] crate::harness::run_wake::RunWakeErr),
}

/// Durably cancel a run and wake its waiter only for the newly-written
/// terminal transition.
pub fn cancel_and_wake(
    store: &Store,
    run_id: &RunId,
) -> std::result::Result<RunRecord, CancelRunErr> {
    let (record, wrote) = cancel(store.paths(), run_id)?;
    if wrote {
        crate::harness::run_wake::wake_run(store.runtime_paths(), &record)?;
    }
    Ok(record)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    VerifyFailed,
    TimedOut,
    BudgetExceeded,
    Canceled,
}

impl RunStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::VerifyFailed => "verify_failed",
            Self::TimedOut => "timed_out",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Canceled => "canceled",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::VerifyFailed
                | Self::TimedOut
                | Self::BudgetExceeded
                | Self::Canceled
        )
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::VerifyFailed => "verify failed",
            Self::TimedOut => "timed out",
            Self::BudgetExceeded => "budget exceeded",
            Self::Canceled => "canceled",
        }
    }

    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Completed => 0,
            Self::Failed => 1,
            Self::VerifyFailed => 123,
            Self::Canceled => 130,
            Self::BudgetExceeded => 125,
            Self::TimedOut | Self::Pending | Self::Running => 124,
        }
    }

    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Failed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunVerify {
    pub cmd: String,
    pub attempts: u32,
    pub passed: bool,
    pub code: Option<i32>,
    pub timed_out: bool,
    pub output: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: RunId,
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentSessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    /// Spawned provider process owned by the in-pane wrapper.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_pid: Option<u32>,
    /// Process-start token paired with `provider_pid` to reject PID reuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_process_start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_tail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<RunVerify>,
    pub status: RunStatus,
    pub permission_mode: PermissionMode,
    /// Never reclaim this run's pane automatically, including when its parent
    /// agent exits.
    #[serde(default, skip_serializing_if = "is_false")]
    pub keep: bool,
    /// Pane-backed child launched through `rimz subagents`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub subagent: bool,
    /// Queue the settled outcome to the launching agent.
    #[serde(default, skip_serializing_if = "is_false")]
    pub report_to_parent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    pub prompt: String,
    pub worktree_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    pub started_at: Timestamp,
    /// Producer-enforced wall-clock deadline for this supervised attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_at: Option<Timestamp>,
    pub updated_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Timestamp>,
}

impl RunRecord {
    pub fn new(
        workspace_id: WorkspaceId,
        kind: AgentKind,
        permission_mode: PermissionMode,
        prompt: String,
        worktree_path: PathBuf,
    ) -> Self {
        let now = Timestamp::now();
        Self {
            run_id: RunId::new(),
            workspace_id,
            kind,
            agent_id: None,
            agent_name: None,
            pane_id: None,
            provider_pid: None,
            provider_process_start: None,
            transcript_path: None,
            failure_tail: None,
            retry_of: None,
            loop_task: None,
            verify: None,
            status: RunStatus::Pending,
            permission_mode,
            keep: false,
            subagent: false,
            report_to_parent: false,
            budget: None,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            prompt,
            worktree_path,
            last_message: None,
            started_at: now,
            deadline_at: None,
            updated_at: now,
            completed_at: None,
        }
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunLiveStatus {
    pub agent_status: AgentStatus,
    pub phase: TurnPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_pct: Option<u8>,
}

pub fn create(paths: &StatePaths, record: &RunRecord) -> Result<()> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    run_store::write(&paths.runs_dir, record)
}

pub fn load(paths: &StatePaths, run_id: &RunId) -> Result<RunRecord> {
    run_store::load(&paths.runs_dir, run_id)
}

pub fn list(paths: &StatePaths) -> Result<Vec<RunRecord>> {
    run_store::list(&paths.runs_dir)
}

pub fn newest_run_for_agent<'a>(
    runs: &'a [RunRecord],
    agent: &crate::agents::AgentState,
) -> Option<&'a RunRecord> {
    runs.iter()
        .filter(|run| {
            run.agent_id.as_ref() == Some(&agent.agent_id)
                || run.agent_name.as_deref() == agent.name.as_deref()
        })
        .max_by_key(|run| run.started_at)
}

enum RecordMutation<T> {
    Keep(T),
    Write(T),
}

fn update_record<T>(
    paths: &StatePaths,
    run_id: &RunId,
    update: impl FnOnce(&mut RunRecord, Timestamp) -> Result<RecordMutation<T>>,
) -> Result<(RunRecord, T)> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = load(paths, run_id)?;
    let now = Timestamp::now();
    match update(&mut record, now)? {
        RecordMutation::Keep(outcome) => Ok((record, outcome)),
        RecordMutation::Write(outcome) => {
            record.updated_at = now;
            run_store::write(&paths.runs_dir, &record)?;
            Ok((record, outcome))
        }
    }
}

pub fn record_pane(paths: &StatePaths, run_id: &RunId, pane_id: PaneId) -> Result<RunRecord> {
    update_record(paths, run_id, |record, _| {
        if record.pane_id.as_ref() == Some(&pane_id) {
            return Ok(RecordMutation::Keep(()));
        }
        record.pane_id = Some(pane_id);
        Ok(RecordMutation::Write(()))
    })
    .map(|(record, ())| record)
}

pub fn record_provider_process(
    paths: &StatePaths,
    run_id: &RunId,
    pid: u32,
    process_start: Option<String>,
) -> Result<RunRecord> {
    update_record(paths, run_id, |record, _| {
        if record.provider_pid == Some(pid)
            && record.provider_process_start.as_ref() == process_start.as_ref()
        {
            return Ok(RecordMutation::Keep(()));
        }
        record.provider_pid = Some(pid);
        record.provider_process_start = process_start;
        Ok(RecordMutation::Write(()))
    })
    .map(|(record, ())| record)
}

pub fn record_failure_tail(paths: &StatePaths, run_id: &RunId, tail: &str) -> Result<RunRecord> {
    let tail = tail.trim_end();
    update_record(paths, run_id, |record, _| {
        if record.failure_tail.is_some() || tail.trim().is_empty() {
            return Ok(RecordMutation::Keep(()));
        }
        record.failure_tail = Some(
            crate::proc::tail_output(tail.as_bytes(), FAILURE_TAIL_CAP)
                .trim_end()
                .to_owned(),
        );
        Ok(RecordMutation::Write(()))
    })
    .map(|(record, ())| record)
}

pub fn timeout(paths: &StatePaths, run_id: &RunId) -> Result<RunRecord> {
    mark_terminal(paths, run_id, RunStatus::TimedOut).map(|(record, _wrote)| record)
}

/// Mark a run timed out only when its durable producer deadline is due.
///
/// The hidden timeout helper rechecks this under the workspace lock so a
/// detached enforcement decision cannot overwrite a run that completed while
/// the helper was starting.
pub fn timeout_if_due(
    paths: &StatePaths,
    run_id: &RunId,
    now: Timestamp,
) -> Result<(RunRecord, bool)> {
    update_record(paths, run_id, |record, _| {
        if record.status.is_terminal()
            || !record.deadline_at.is_some_and(|deadline| deadline <= now)
        {
            return Ok(RecordMutation::Keep(false));
        }
        record.status = RunStatus::TimedOut;
        record.completed_at = Some(now);
        Ok(RecordMutation::Write(true))
    })
}

pub fn budget_exceeded(
    paths: &StatePaths,
    run_id: &RunId,
    cost_usd: Option<f64>,
) -> Result<(RunRecord, bool)> {
    update_record(paths, run_id, |record, now| {
        if record.status.is_terminal() {
            return Ok(RecordMutation::Keep(false));
        }
        record.status = RunStatus::BudgetExceeded;
        record.cost_usd = cost_usd.filter(|cost| cost.is_finite() && *cost >= 0.0);
        record.completed_at = Some(now);
        Ok(RecordMutation::Write(true))
    })
}

pub fn record_spend(
    paths: &StatePaths,
    run_id: &RunId,
    cost_usd: Option<f64>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Result<RunRecord> {
    let cost_usd = cost_usd.filter(|cost| cost.is_finite() && *cost >= 0.0);
    if cost_usd.is_none() && input_tokens.is_none() && output_tokens.is_none() {
        return load(paths, run_id);
    }
    update_record(paths, run_id, |record, _| {
        if let Some(cost_usd) = cost_usd {
            record.cost_usd = Some(cost_usd);
        }
        if let Some(input_tokens) = input_tokens {
            record.input_tokens = Some(input_tokens);
        }
        if let Some(output_tokens) = output_tokens {
            record.output_tokens = Some(output_tokens);
        }
        Ok(RecordMutation::Write(()))
    })
    .map(|(record, ())| record)
}

pub fn cancel(paths: &StatePaths, run_id: &RunId) -> Result<(RunRecord, bool)> {
    mark_terminal(paths, run_id, RunStatus::Canceled)
}

pub fn fail(paths: &StatePaths, run_id: &RunId) -> Result<RunRecord> {
    mark_terminal(paths, run_id, RunStatus::Failed).map(|(record, _wrote)| record)
}

pub fn fail_if_nonterminal(paths: &StatePaths, run_id: &RunId) -> Result<Option<RunRecord>> {
    let (record, wrote) = mark_terminal(paths, run_id, RunStatus::Failed)?;
    Ok(wrote.then_some(record))
}

pub fn reopen_for_verify(
    paths: &StatePaths,
    run_id: &RunId,
    verify: RunVerify,
) -> Result<RunRecord> {
    update_record(paths, run_id, |record, _| {
        require_completed(record)?;
        record.status = RunStatus::Running;
        record.verify = Some(verify);
        record.completed_at = None;
        Ok(RecordMutation::Write(()))
    })
    .map(|(record, ())| record)
}

pub fn verify_failed(paths: &StatePaths, run_id: &RunId, verify: RunVerify) -> Result<RunRecord> {
    update_record(paths, run_id, |record, now| {
        if record.status == RunStatus::VerifyFailed {
            return Ok(RecordMutation::Keep(()));
        }
        require_completed(record)?;
        record.status = RunStatus::VerifyFailed;
        record.verify = Some(verify);
        record.completed_at = Some(now);
        Ok(RecordMutation::Write(()))
    })
    .map(|(record, ())| record)
}

pub fn verify_passed(paths: &StatePaths, run_id: &RunId, verify: RunVerify) -> Result<RunRecord> {
    update_record(paths, run_id, |record, _| {
        require_completed(record)?;
        record.verify = Some(verify);
        Ok(RecordMutation::Write(()))
    })
    .map(|(record, ())| record)
}

fn require_completed(record: &RunRecord) -> Result<()> {
    if record.status != RunStatus::Completed {
        return Err(RunStoreErr::InvalidStatus {
            run_id: record.run_id.clone(),
            actual: record.status.as_str(),
            expected: "completed",
        });
    }
    Ok(())
}

fn mark_terminal(
    paths: &StatePaths,
    run_id: &RunId,
    status: RunStatus,
) -> Result<(RunRecord, bool)> {
    update_record(paths, run_id, |record, now| {
        if record.status.is_terminal() {
            return Ok(RecordMutation::Keep(false));
        }
        record.status = status;
        record.completed_at = Some(now);
        Ok(RecordMutation::Write(true))
    })
}

/// Fold one lifecycle observation into an optional run record update.
///
/// Returns `Some(record)` only when this observation newly makes the run
/// terminal, so callers can send exactly one wakeup datagram.
pub fn record_lifecycle(
    paths: &StatePaths,
    run_id: &RunId,
    kind: &str,
    observation: &AgentLifecycleObservation,
    last_message: Option<String>,
) -> Result<Option<RunRecord>> {
    if observation.parent_agent_id.is_some()
        || matches!(
            observation.signal,
            LifecycleSignal::SubagentStarted | LifecycleSignal::SubagentStopped { .. }
        )
    {
        return Ok(None);
    }
    let (record, transition) = update_record(paths, run_id, |record, now| {
        Ok(
            match fold_lifecycle(record, kind, observation, last_message, now) {
                LifecycleFold::Ignored => RecordMutation::Keep(LifecycleFold::Ignored),
                LifecycleFold::Updated => RecordMutation::Write(LifecycleFold::Updated),
                LifecycleFold::NewlyTerminal => RecordMutation::Write(LifecycleFold::NewlyTerminal),
            },
        )
    })?;
    Ok(matches!(transition, LifecycleFold::NewlyTerminal).then_some(record))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LifecycleFold {
    Ignored,
    Updated,
    NewlyTerminal,
}

fn fold_lifecycle(
    record: &mut RunRecord,
    kind: &str,
    observation: &AgentLifecycleObservation,
    last_message: Option<String>,
    now: Timestamp,
) -> LifecycleFold {
    if record.kind.as_str() != kind || record.status.is_terminal() {
        return LifecycleFold::Ignored;
    }
    match (&record.agent_id, &observation.agent_id) {
        (Some(bound), Some(observed)) if observed != bound => return LifecycleFold::Ignored,
        (Some(_), None) => return LifecycleFold::Ignored,
        (None, Some(observed)) => {
            record.agent_id = Some(observed.clone());
            record.agent_name = observation.agent_name.clone().or(record.agent_name.take());
        }
        (None, None) | (Some(_), Some(_)) => {}
    }
    if let Some(status) = terminal_status_for_signal(&observation.signal) {
        record.status = status;
        if let Some(path) = observation.transcript_path.as_ref() {
            record.transcript_path = Some(path.clone());
        }
        record.last_message = last_message.or(record.last_message.take());
        record.completed_at = Some(now);
        return LifecycleFold::NewlyTerminal;
    }
    let first_transcript_path =
        record.transcript_path.is_none() && observation.transcript_path.is_some();
    if record.status != RunStatus::Pending && !first_transcript_path {
        return LifecycleFold::Ignored;
    }
    record.status = RunStatus::Running;
    if record.transcript_path.is_none() {
        record
            .transcript_path
            .clone_from(&observation.transcript_path);
    }
    LifecycleFold::Updated
}

/// Store provider-declared final visible output without ending the run.
pub fn record_assistant_message(
    paths: &StatePaths,
    run_id: &RunId,
    kind: &str,
    agent_id: &AgentSessionId,
    message: String,
) -> Result<()> {
    update_record(paths, run_id, |record, _| {
        if record.kind.as_str() != kind || record.status.is_terminal() {
            return Ok(RecordMutation::Keep(()));
        }
        match &record.agent_id {
            Some(bound) if bound != agent_id => return Ok(RecordMutation::Keep(())),
            None => record.agent_id = Some(agent_id.clone()),
            Some(_) => {}
        }
        record.last_message = Some(message);
        Ok(RecordMutation::Write(()))
    })
    .map(|_| ())
}

pub fn live_status(record: &RunRecord, snapshot: &SidebarSnapshot) -> Option<RunLiveStatus> {
    if record.status.is_terminal() {
        return None;
    }
    let agent_id = record.agent_id.as_ref()?;
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind == record.kind && &agent.agent_id == agent_id)?;
    Some(RunLiveStatus {
        agent_status: agent.status,
        phase: agent.phase,
        pane_id: agent
            .pane
            .as_ref()
            .map(|pane| pane.pane_id.clone())
            .or_else(|| record.pane_id.clone()),
        context_pct: agent_context_pct(agent),
    })
}

fn agent_context_pct(agent: &AgentState) -> Option<u8> {
    agent
        .context
        .as_ref()
        .and_then(|context| context.tokens.as_ref())
        // Trust a statusline percentage only alongside its own window; without
        // one, the fold-derived scalar (tied to the resolved window) is the
        // consistent reading.
        .filter(|tokens| tokens.context_window_size.is_some())
        .and_then(|tokens| tokens.used_percentage)
        .or(agent.usage.context_pct)
}

/// Terminal run status produced by one agent lifecycle signal.
///
/// Hook ingestion uses this same predicate before extracting the final
/// assistant message, so the deliverable and status transition stay in lockstep.
pub fn terminal_status_for_signal(signal: &LifecycleSignal) -> Option<RunStatus> {
    match signal {
        LifecycleSignal::TurnEnded {
            errored,
            parked_on_background,
        } if !parked_on_background => Some(if *errored {
            RunStatus::Failed
        } else {
            RunStatus::Completed
        }),
        LifecycleSignal::TurnInterrupted { .. } => Some(RunStatus::Canceled),
        LifecycleSignal::Ended => Some(RunStatus::Failed),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
