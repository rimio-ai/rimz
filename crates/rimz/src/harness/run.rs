//! Supervised-run requests, transitions, and cancellation.

pub mod report;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use serde::Serialize;

use crate::agents::lifecycle::TerminalDisposition;
use crate::agents::{AgentLifecycleObservation, LifecycleSignal, PermissionMode, TurnPhase};
use crate::agents::{AgentState, AgentStatus};
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::StatePaths;
use crate::harness::owed::OwedWake;
use crate::ids::{AgentSessionId, PaneId, RunId};
use crate::store::run::{RunRecord, RunStatus, RunStoreErr, RunVerify};
use crate::store::{Store, snapshot::SidebarSnapshot};

const FAILURE_TAIL_CAP: usize = 4 * 1024;
const STRANDED_PARK_REASON: &str =
    "parked on a wake that never arrived: no armed wait, no wake in flight, no live subagents";

type Result<T> = std::result::Result<T, RunStoreErr>;

#[derive(Debug, thiserror::Error)]
pub enum ResponsePublishErr {
    #[error(transparent)]
    Paths(#[from] crate::disk::paths::PathErr),
    #[error(transparent)]
    Atomic(#[from] crate::disk::atomic::AtomicErr),
    #[error("accessing subagent response {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Host path promised by the launch receipt for this run's response.
pub fn response_path(paths: &StatePaths, agent_name: &str) -> PathBuf {
    paths.subagents_dir.join(format!("{agent_name}.output"))
}

/// Ensure the captured response, leaving identical files untouched and removing stale empty answers.
pub fn publish_response(
    paths: &StatePaths,
    record: &RunRecord,
) -> std::result::Result<Option<PathBuf>, ResponsePublishErr> {
    paths.ensure_tmp_dir()?;
    let Some(name) = record.agent_name.as_deref() else {
        return Ok(None);
    };
    let path = response_path(paths, name);
    let io_error = |source| ResponsePublishErr::Io {
        path: path.clone(),
        source,
    };
    let Some(message) = record
        .last_message
        .as_deref()
        .filter(|message| !message.is_empty())
    else {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(io_error(err)),
        }
        return Ok(None);
    };
    let mut bytes = message.as_bytes().to_vec();
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    match std::fs::read(&path) {
        Ok(existing) if existing == bytes => return Ok(Some(path)),
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(io_error(err)),
    }
    crate::disk::atomic::write_bytes_atomically(&path, &bytes)?;
    Ok(Some(path))
}

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
    pub force_new_tab: bool,
    pub permission_mode: Option<PermissionMode>,
    /// The launch's `--isolation` override; a subagent without one inherits
    /// its parent's.
    pub isolation: Option<crate::config::Isolation>,
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
        permission_mode: Option<PermissionMode>,
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
            force_new_tab: false,
            permission_mode,
            isolation: None,
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
    Background {
        agent_name: String,
        run_id: RunId,
        /// Where the launching agent's fleet report will write the captured
        /// final response, as that agent sees the path. `None` when no agent
        /// launched the run, so nothing reports back.
        response_path: Option<std::path::PathBuf>,
    },
    BudgetExceeded {
        reason: String,
    },
}

impl RunCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(any(test, feature = "testkit"))]
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

/// Durably cancel a run and wake its waiter only for the newly-written
/// terminal transition.
pub fn cancel_and_wake(store: &Store, run_id: &RunId) -> Result<RunRecord> {
    let (record, wrote) = cancel(store.paths(), run_id)?;
    if wrote {
        crate::store::run::wake_run(store.runtime_paths(), &record);
    }
    Ok(record)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunLiveStatus {
    pub agent_status: AgentStatus,
    pub phase: TurnPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_pct: Option<u8>,
    /// Set while the run is open only because its session is owed a wake.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parked_at: Option<Timestamp>,
}

pub fn create(paths: &StatePaths, record: &RunRecord) -> Result<()> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    crate::store::run::write(&paths.runs_dir, record)
}

pub fn load(paths: &StatePaths, run_id: &RunId) -> Result<RunRecord> {
    crate::store::run::load(&paths.runs_dir, run_id)
}

pub fn list(paths: &StatePaths) -> Result<Vec<RunRecord>> {
    crate::store::run::list(&paths.runs_dir)
}

enum RecordMutation<T> {
    Keep(T),
    Write(T),
}

/// One strand check of a parked run, as the in-pane wrapper sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParkCheck {
    /// Not parked, or still owed a wake.
    Live,
    /// Nothing owed under this `parked_at`; an identical second check settles.
    Stranded(Timestamp),
    /// This check failed the run.
    Settled,
}

/// Settle only after two nothing-owed checks under the same park timestamp.
///
/// The CAS protects a wake turn starting between the read and lock: the
/// lifecycle hook folds `TurnStarted` (clearing `parked_at`) before settling
/// the wake message to `Delivered`, so a queue read showing the wake gone
/// implies the clear already happened.
///
/// The second look covers producer sequences shorter than the check cadence:
/// the digest reporter stamps child `report_message_id` fields and queues the
/// digest in two separate lock holds.
pub fn settle_stranded_park(
    store: &Store,
    record: &RunRecord,
    previous: Option<Timestamp>,
) -> Result<ParkCheck> {
    let Some(parked_at) = record.parked_at.filter(|_| !record.status.is_terminal()) else {
        return Ok(ParkCheck::Live);
    };
    let Some(agent_id) = record.agent_id.as_ref() else {
        return Ok(ParkCheck::Live);
    };
    match crate::harness::owed::owed_wake(store, &record.kind, agent_id) {
        Ok(Some(_)) => return Ok(ParkCheck::Live),
        Err(error) => {
            tracing::debug!(run_id = %record.run_id, %error, "could not check parked run wake");
            return Ok(ParkCheck::Live);
        }
        Ok(None) => {}
    }
    if previous != Some(parked_at) {
        return Ok(ParkCheck::Stranded(parked_at));
    }
    let (record, check) = update_record(store.paths(), &record.run_id, |record, now| {
        if record.parked_at != Some(parked_at) || record.status.is_terminal() {
            return Ok(RecordMutation::Keep(ParkCheck::Live));
        }
        if record.failure_tail.is_none() {
            record.failure_tail = Some(STRANDED_PARK_REASON.to_owned());
        }
        record.mark_terminal(RunStatus::Failed, now);
        Ok(RecordMutation::Write(ParkCheck::Settled))
    })?;
    if check == ParkCheck::Settled {
        crate::store::run::wake_run(store.runtime_paths(), &record);
    }
    Ok(check)
}

fn update_record<T>(
    paths: &StatePaths,
    run_id: &RunId,
    update: impl FnOnce(&mut RunRecord, Timestamp) -> Result<RecordMutation<T>>,
) -> Result<(RunRecord, T)> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut record = load(paths, run_id)?;
    let was_terminal = record.status.is_terminal();
    let now = Timestamp::now();
    match update(&mut record, now)? {
        RecordMutation::Keep(outcome) => Ok((record, outcome)),
        RecordMutation::Write(outcome) => {
            record.updated_at = now;
            // Waiters read the record unlocked, so a terminal record must imply its file.
            if !was_terminal
                && record.status.is_terminal()
                && record.subagent
                && let Err(err) = publish_response(paths, &record)
            {
                tracing::warn!(run_id = %record.run_id, error = %err, "publishing subagent response failed");
            }
            crate::store::run::write(&paths.runs_dir, &record)?;
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

pub(super) fn timeout(paths: &StatePaths, run_id: &RunId) -> Result<RunRecord> {
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
        if !record.deadline_at.is_some_and(|deadline| deadline <= now)
            || !record.mark_terminal(RunStatus::TimedOut, now)
        {
            return Ok(RecordMutation::Keep(false));
        }
        Ok(RecordMutation::Write(true))
    })
}

pub fn budget_exceeded(
    paths: &StatePaths,
    run_id: &RunId,
    cost_usd: Option<f64>,
) -> Result<(RunRecord, bool)> {
    update_record(paths, run_id, |record, now| {
        if !record.mark_terminal(RunStatus::BudgetExceeded, now) {
            return Ok(RecordMutation::Keep(false));
        }
        record.cost_usd = cost_usd.filter(|cost| cost.is_finite() && *cost >= 0.0);
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
        Ok(if record.mark_terminal(status, now) {
            RecordMutation::Write(true)
        } else {
            RecordMutation::Keep(false)
        })
    })
}

/// Fold one lifecycle observation into an optional run record update.
///
/// Returns `Some(record)` only when this observation newly makes the run
/// terminal, so callers can send exactly one wakeup datagram.
///
/// The owed callback runs only for a root observation classified Completed,
/// before `update_record` takes the workspace lock; its lock-free reads must
/// never run inside that lock.
pub fn record_lifecycle(
    paths: &StatePaths,
    run_id: &RunId,
    kind: &str,
    observation: &AgentLifecycleObservation,
    last_message: Option<String>,
    owed: impl FnOnce() -> Option<OwedWake>,
) -> Result<Option<RunRecord>> {
    if observation.parent_agent_id.is_some()
        || matches!(
            observation.signal,
            LifecycleSignal::SubagentStarted | LifecycleSignal::SubagentStopped { .. }
        )
    {
        return Ok(None);
    }
    let owed = if observation.signal.terminal_disposition() == Some(TerminalDisposition::Completed)
    {
        owed()
    } else {
        None
    };
    let (record, transition) = update_record(paths, run_id, |record, now| {
        Ok(
            match fold_lifecycle(record, kind, observation, last_message, now, owed) {
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
    owed: Option<OwedWake>,
) -> LifecycleFold {
    let reopen = record.status.is_terminal()
        && record.subagent
        && matches!(observation.signal, LifecycleSignal::TurnStarted { .. });
    if record.kind.as_str() != kind || (record.status.is_terminal() && !reopen) {
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
    if reopen {
        record.deadline_at = record
            .deadline_at
            .map(|deadline| now + (deadline - record.started_at));
        record.status = RunStatus::Running;
        record.completed_at = None;
        record.parked_at = None;
        record.joined_at = None;
        record.report_message_id = None;
        record.last_message = None;
        record.failure_tail = None;
    }
    if let Some(disposition) = observation.signal.terminal_disposition() {
        if let Some(path) = observation.transcript_path.as_ref() {
            record.transcript_path = Some(path.clone());
        }
        record.last_message = last_message.or(record.last_message.take());
        if disposition == TerminalDisposition::Completed
            && let Some(owed) = owed
        {
            record.status = RunStatus::Running;
            record.parked_at = Some(now);
            tracing::info!(
                run_id = %record.run_id,
                owed = owed.as_str(),
                "supervised run parked on an owed wake",
            );
            return LifecycleFold::Updated;
        }
        record.status = match disposition {
            TerminalDisposition::Completed => RunStatus::Completed,
            TerminalDisposition::Failed => RunStatus::Failed,
            TerminalDisposition::Canceled => RunStatus::Canceled,
        };
        record.completed_at = Some(now);
        record.parked_at = None;
        return LifecycleFold::NewlyTerminal;
    }
    // The wake turn both clears the park and may carry the run's first
    // transcript path, so the clear cannot return ahead of that fold.
    let unparked = matches!(observation.signal, LifecycleSignal::TurnStarted { .. })
        && record.parked_at.is_some();
    if unparked {
        record.parked_at = None;
    }
    let first_transcript_path =
        record.transcript_path.is_none() && observation.transcript_path.is_some();
    if !reopen && !unparked && record.status != RunStatus::Pending && !first_transcript_path {
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
        parked_at: record.parked_at,
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

#[cfg(test)]
mod tests;
