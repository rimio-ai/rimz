//! Supervised, launcher-opened peer, and team-long leader run transitions, responses, and cancellation.

mod peer;
pub mod report;
mod team;
pub use peer::{
    create_peer_prompt, fail_peer_run, open_peer_run, peer_can_report, record_run_delivery,
};
pub use team::{open_team_run, open_team_run_for, reopen_team_run, settle_team_run};

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use serde::Serialize;

use crate::agents::lifecycle::TerminalDisposition;
use crate::agents::{
    AgentDefinition, AgentLifecycleObservation, LifecycleSignal, PermissionMode, TurnPhase,
};
use crate::agents::{AgentState, AgentStatus};
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::StatePaths;
use crate::harness::owed::OwedWake;
use crate::ids::{AgentSessionId, PaneId, RunId};
use crate::store::run::{
    EarlierAnswer, FollowUpTurn, ReportTo, RunRecord, RunStatus, RunStoreErr, RunVerify,
};
use crate::store::{
    Store,
    snapshot::{SidebarSnapshot, find_agent},
};

const FAILURE_TAIL_CAP: usize = 4 * 1024;
const STRANDED_PARK_REASON: &str =
    "parked on a wake that never arrived: no armed wait, no wake in flight, no live subagents";

type Result<T> = std::result::Result<T, RunStoreErr>;

#[derive(Debug, thiserror::Error)]
pub enum ResponsePublishErr {
    #[error(transparent)]
    Atomic(#[from] crate::disk::atomic::AtomicErr),
    #[error("accessing subagent response {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Host path of this run's current-turn response, in its reader's `out/`
/// directory: the launcher's, or the run's own agent's when none launched it.
pub fn response_path(paths: &StatePaths, record: &RunRecord) -> Option<PathBuf> {
    let name = record.agent_name.as_deref()?;
    if record.peer.is_some() || record.team.is_some() {
        return Some(response_file(
            paths,
            record,
            name,
            &format!("{name}.{}", record.run_id),
        ));
    }
    earlier_response_path(paths, record, record.follow_ups + 1)
}

/// Host path of this subagent run's response to turn `ordinal` (1 is the first).
pub fn earlier_response_path(
    paths: &StatePaths,
    record: &RunRecord,
    ordinal: u32,
) -> Option<PathBuf> {
    let name = record.agent_name.as_deref()?;
    let stem = if ordinal == 1 {
        name.to_owned()
    } else {
        format!("{name}.{ordinal}")
    };
    Some(response_file(paths, record, name, &stem))
}

fn response_file(paths: &StatePaths, record: &RunRecord, name: &str, stem: &str) -> PathBuf {
    paths
        .out_reader_dir(Some(record.reader.as_deref().unwrap_or(name)))
        .join(format!("{stem}.output"))
}

/// Ensure the captured response, leaving identical files untouched and removing stale empty answers.
pub fn publish_response(
    paths: &StatePaths,
    record: &RunRecord,
) -> std::result::Result<Option<PathBuf>, ResponsePublishErr> {
    let Some(path) = response_path(paths, record) else {
        return Ok(None);
    };
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
    for dir in [
        paths.out_dir.as_path(),
        path.parent().unwrap_or(&paths.out_dir),
    ] {
        crate::disk::paths::ensure_private_runtime_dir(dir)
            .map_err(|err| io_error(std::io::Error::other(err)))?;
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

/// The turns a verified run gets when its request sets no `max_attempts`.
pub const VERIFY_MAX_ATTEMPTS_DEFAULT: u32 = 3;

/// Command-neutral input for one supervised turn.
#[derive(Clone, Debug)]
pub struct SupervisedRunRequest {
    pub spec: String,
    pub prompt: String,
    pub description: Option<String>,
    pub worktree: Option<String>,
    pub cwd: Option<PathBuf>,
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
    pub tier: Option<crate::config::tiers::ModelTier>,
    pub model: Option<String>,
    pub system_prompt_file: Option<PathBuf>,
    pub append_system_prompt_files: Vec<PathBuf>,
    pub effort: Option<String>,
    pub budget: Option<crate::harness::budget::BudgetSpec>,
    pub max_turns: Option<u32>,
    pub timeout: Option<std::time::Duration>,
    pub warn: Vec<std::time::Duration>,
    pub grace: Option<std::time::Duration>,
    pub keep: bool,
    pub report_to: ReportTo,
    pub retries: u32,
    pub verify: Option<String>,
    pub max_attempts: Option<u32>,
    pub loop_task: Option<String>,
    /// The `### Loop` reminder body the loop fire composed; every attempt's
    /// pane request carries it.
    pub loop_reminder: Option<String>,
    pub passthrough: Vec<String>,
    /// Managed-account state: pending inputs, unsupported selection, unresolved
    /// exact selection, or one proven binding.
    pub managed_launch: crate::agents::ManagedLaunchState,
    /// The account the launch runs under; a subagent on the room default
    /// inherits its same-kind parent's.
    pub login: crate::store::writer::LaunchLogin,
    /// The loop throttle turn this launch holds; the launch commit reports it.
    pub throttle_turn: Option<crate::harness::schedule::throttle::Turn>,
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
            cwd: None,
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
            tier: None,
            model: None,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            effort: None,
            budget: None,
            max_turns: None,
            timeout: None,
            warn: Vec::new(),
            grace: None,
            keep: false,
            report_to: ReportTo::Launcher,
            retries: 0,
            verify: None,
            max_attempts: None,
            loop_task: None,
            loop_reminder: None,
            passthrough: Vec::new(),
            managed_launch,
            login: crate::store::writer::LaunchLogin::RoomDefault,
            throttle_turn: None,
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
        /// final response, as a host path. `None` when no agent
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
            // A detached plain run is outside the fleet, so this is its only publisher.
            if !was_terminal
                && record.status.is_terminal()
                && (record.subagent
                    || record.peer.is_some()
                    || record.team.is_some()
                    || record.report_to == ReportTo::Nobody)
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

/// What the exec wrapper saw when one provider process exited.
#[derive(Clone, Copy, Debug)]
pub struct ProviderExit {
    /// A fresh launch, not a resume or a fork.
    pub fresh_launch: bool,
    pub success: bool,
    /// The wrapper ended the provider itself, or the launching parent ended.
    pub abrupt: bool,
    /// A stop or interrupt signal reached the wrapper.
    pub signaled: bool,
    /// How many times this wrapper already relaunched its provider.
    pub relaunches: u8,
    /// Spawn to exit of this process.
    pub startup: std::time::Duration,
}

/// What durably says whether a launch's session opened.
#[derive(Clone, Copy, Debug)]
pub enum StartupEvidence {
    /// The launch's run record; `Pending` means RimZ accepted no lifecycle
    /// observation for it.
    Run(RunStatus),
    /// A launch without a run: whether its launch card is still provisional.
    Card { provisional: bool },
}

/// How soon after its spawn a provider must exit for a provisional card to
/// count as a startup death. A lazily registering provider keeps that card
/// until its first prompt, so past this window the card says nothing.
const CARD_EVIDENCE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);

/// The wrapper's answer to one provider exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupRelaunch {
    /// Not a startup death, or the relaunch is disabled: the exit settles.
    No,
    /// A startup death below the cap: spawn the same command again.
    Due,
    /// A startup death with every allowed relaunch already made.
    Spent,
}

/// Whether a provider exit is a startup death, and whether the wrapper still
/// answers it by spawning the same command again: at most `cap` times per
/// launch, and never at `cap` 0. A `Pending` run is evidence at any startup
/// time: a prompt was handed over and never observed. Neither source proves
/// the provider did nothing: one that acts before its first hook and then
/// exits nonzero may repeat that work.
pub fn startup_relaunch(exit: ProviderExit, evidence: StartupEvidence, cap: u8) -> StartupRelaunch {
    let unopened = match evidence {
        StartupEvidence::Run(status) => status == RunStatus::Pending,
        StartupEvidence::Card { provisional } => {
            provisional && exit.startup <= CARD_EVIDENCE_WINDOW
        }
    };
    let died_at_startup =
        unopened && exit.fresh_launch && !exit.success && !exit.abrupt && !exit.signaled;
    if cap == 0 || !died_at_startup {
        StartupRelaunch::No
    } else if exit.relaunches < cap {
        StartupRelaunch::Due
    } else {
        StartupRelaunch::Spent
    }
}

/// Whether a provider exit warrants startup diagnostics, including resumes and forks:
/// an unsuccessful exit within the window that neither the wrapper nor a signal to it caused.
pub fn provider_startup_exit(exit: ProviderExit) -> bool {
    !exit.success && !exit.abrupt && !exit.signaled && exit.startup <= CARD_EVIDENCE_WINDOW
}

pub fn record_failure_tail(paths: &StatePaths, run_id: &RunId, tail: &str) -> Result<RunRecord> {
    update_record(paths, run_id, |record, _| {
        if record.failure_tail.is_some() {
            return Ok(RecordMutation::Keep(()));
        }
        let Some(tail) = capped_failure_tail(tail) else {
            return Ok(RecordMutation::Keep(()));
        };
        record.failure_tail = Some(tail);
        Ok(RecordMutation::Write(()))
    })
    .map(|(record, ())| record)
}

/// The last `FAILURE_TAIL_CAP` bytes of a failure reason, or nothing for a blank one.
fn capped_failure_tail(tail: &str) -> Option<String> {
    let tail = tail.trim_end();
    (!tail.trim().is_empty()).then(|| {
        crate::proc::tail_output(tail.as_bytes(), FAILURE_TAIL_CAP)
            .trim_end()
            .to_owned()
    })
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
    harvest: Option<String>,
) -> Result<(RunRecord, bool)> {
    update_record(paths, run_id, |record, _| {
        if !super::deadline::kill_due(record, now)
            || !record.mark_terminal(RunStatus::TimedOut, now)
        {
            return Ok(RecordMutation::Keep(false));
        }
        if record.last_message.is_none() {
            record.last_message = harvest;
        }
        Ok(RecordMutation::Write(true))
    })
}

pub fn claim_rung(
    paths: &StatePaths,
    run_id: &RunId,
    now: Timestamp,
    attach: impl FnOnce(&super::deadline::Rung) -> bool,
) -> Result<Option<super::deadline::Rung>> {
    // Records are published atomically, so an unlocked read can rule out the
    // common case (no rung due) without taking the workspace lock on every tool
    // hook; a due rung is re-checked under the lock.
    if super::deadline::due_rung(&load(paths, run_id)?, now).is_none() {
        return Ok(None);
    }
    update_record(paths, run_id, |record, _| {
        let Some(rung) = super::deadline::due_rung(record, now) else {
            return Ok(RecordMutation::Keep(None));
        };
        if !attach(&rung) {
            return Ok(RecordMutation::Keep(None));
        }
        record.deadline_notice_at = Some(rung.at());
        Ok(RecordMutation::Write(Some(rung)))
    })
    .map(|(_, rung)| rung)
}

/// Claim one provider-limit park of a child run for its parent's notice. The
/// park is named by the child's `last_activity`; a settled run and a park
/// already claimed write nothing. Returns whether this caller owns the notice.
pub fn claim_park_notice(paths: &StatePaths, run_id: &RunId, activity: Timestamp) -> Result<bool> {
    update_record(paths, run_id, |record, _| {
        if record.status.is_terminal()
            || record
                .park_noticed_activity
                .is_some_and(|noticed| noticed >= activity)
        {
            return Ok(RecordMutation::Keep(false));
        }
        record.park_noticed_activity = Some(activity);
        Ok(RecordMutation::Write(true))
    })
    .map(|(_, claimed)| claimed)
}

/// Give a claimed park back when its notice could not be queued.
pub fn release_park_notice(paths: &StatePaths, run_id: &RunId, activity: Timestamp) -> Result<()> {
    update_record(paths, run_id, |record, _| {
        if record.park_noticed_activity != Some(activity) {
            return Ok(RecordMutation::Keep(()));
        }
        record.park_noticed_activity = None;
        Ok(RecordMutation::Write(()))
    })
    .map(|_| ())
}

/// Claim this run's one silent-child notice. A settled or already-noticed run writes nothing.
pub fn claim_stall_notice(paths: &StatePaths, run_id: &RunId, now: Timestamp) -> Result<bool> {
    update_record(paths, run_id, |record, _| {
        if record.status.is_terminal() || record.stall_noticed_at.is_some() {
            return Ok(RecordMutation::Keep(false));
        }
        record.stall_noticed_at = Some(now);
        Ok(RecordMutation::Write(true))
    })
    .map(|(_, claimed)| claimed)
}

/// Give back only this caller's claim when its notice could not be queued.
pub fn release_stall_notice(paths: &StatePaths, run_id: &RunId, at: Timestamp) -> Result<()> {
    update_record(paths, run_id, |record, _| {
        if record.stall_noticed_at != Some(at) {
            return Ok(RecordMutation::Keep(()));
        }
        record.stall_noticed_at = None;
        Ok(RecordMutation::Write(()))
    })
    .map(|_| ())
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

fn record_spend(
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

/// Fail a non-terminal run and record why in the same locked write, so a
/// waiter released by the terminal record reads the reason with it.
///
/// The reason follows `record_failure_tail`: an existing tail wins and a blank
/// one records nothing. A terminal run is left untouched, reason included.
/// Returns the record only when this call wrote it, for the caller to wake the
/// waiter once the lock has dropped.
pub fn fail_if_nonterminal(
    paths: &StatePaths,
    run_id: &RunId,
    reason: &str,
) -> Result<Option<RunRecord>> {
    let (record, wrote) = update_record(paths, run_id, |record, now| {
        if !record.mark_terminal(RunStatus::Failed, now) {
            return Ok(RecordMutation::Keep(false));
        }
        if record.failure_tail.is_none() {
            record.failure_tail = capped_failure_tail(reason);
        }
        Ok(RecordMutation::Write(true))
    })?;
    Ok(wrote.then_some(record))
}

/// Where a completed supervised run stands once one verify command finished.
pub enum VerifyStep {
    /// The run is terminal: canceled, verified, or out of verify attempts.
    Settled(RunRecord),
    /// The run reopened as `Running`, awaiting the re-prompted turn.
    Reprompt(RunRecord),
}

/// Store one finished verify on a completed run and settle it: a requested
/// cancellation wins, then a pass, then the attempt cap; otherwise the run
/// reopens for a re-prompt.
pub fn settle_verify(
    paths: &StatePaths,
    run_id: &RunId,
    verify: RunVerify,
    cancel_requested: bool,
    max_attempts: u32,
) -> Result<VerifyStep> {
    if cancel_requested {
        reopen_for_verify(paths, run_id, verify)?;
        return cancel(paths, run_id).map(|(record, _wrote)| VerifyStep::Settled(record));
    }
    if verify.passed {
        return verify_passed(paths, run_id, verify).map(VerifyStep::Settled);
    }
    if verify.attempts == max_attempts {
        return verify_failed(paths, run_id, verify).map(VerifyStep::Settled);
    }
    reopen_for_verify(paths, run_id, verify).map(VerifyStep::Reprompt)
}

fn reopen_for_verify(paths: &StatePaths, run_id: &RunId, verify: RunVerify) -> Result<RunRecord> {
    update_record(paths, run_id, |record, _| {
        require_completed(record)?;
        record.status = RunStatus::Running;
        record.verify = Some(verify);
        record.completed_at = None;
        Ok(RecordMutation::Write(()))
    })
    .map(|(record, ())| record)
}

fn verify_failed(paths: &StatePaths, run_id: &RunId, verify: RunVerify) -> Result<RunRecord> {
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

fn verify_passed(paths: &StatePaths, run_id: &RunId, verify: RunVerify) -> Result<RunRecord> {
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

/// Fold a hook lifecycle and settle a newly terminal non-peer run's session spend.
pub fn settle_lifecycle(
    store: &Store,
    run_id: &RunId,
    agent: &AgentDefinition,
    observation: &AgentLifecycleObservation,
    last_message: Option<String>,
) -> Result<Option<RunRecord>> {
    let kind = agent.spec().kind;
    let record = record_lifecycle(
        store.paths(),
        run_id,
        kind,
        observation,
        last_message,
        || {
            let agent_id = observation.agent_id.as_ref()?;
            match super::owed::owed_wake(store, &agent.spec().kind_id(), agent_id) {
                Ok(owed) => owed,
                Err(err) => {
                    tracing::warn!(error = %err, "could not read wakes owed to supervised run");
                    None
                }
            }
        },
    )?;
    Ok(record.map(|record| {
        // Spend is session-cumulative, so it would overstate a later peer turn.
        if record.peer.is_some() {
            return record;
        }
        let cost_usd = record
            .agent_id
            .as_deref()
            .and_then(|id| crate::store::agent_context::read_one(store.runtime_paths(), kind, id))
            .and_then(|context| context.context.cost)
            .and_then(|cost| cost.total_cost_usd);
        let token_totals = record
            .agent_id
            .as_deref()
            .zip(record.transcript_path.as_deref())
            .and_then(|(agent_id, transcript_path)| {
                let prices = crate::agents::pricing::cached_book(
                    &store.runtime_paths().shared_pricing_cache_path(),
                );
                crate::agents::spending::session_token_totals(
                    agent,
                    agent_id,
                    std::path::Path::new(transcript_path),
                    &prices,
                )
            });
        record_spend(
            store.paths(),
            &record.run_id,
            cost_usd,
            token_totals.map(|totals| totals.input),
            token_totals.map(|totals| totals.output),
        )
        .unwrap_or_else(|err| {
            tracing::warn!(run_id = %record.run_id, error = %err, "could not record supervised run spend");
            record
        })
    }))
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
    if record.team.is_some() {
        return fold_team_lifecycle(record, observation, last_message);
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
        let started_at = record.answer_started_at();
        record
            .earlier_answers
            .retain(|answer| answer.joined_at.is_none());
        // A detached answer was owed to nobody; the follow-up is the launcher's.
        if record.joined_at.is_none() && record.report_to == ReportTo::Launcher {
            record.earlier_answers.push(EarlierAnswer {
                ordinal: record.follow_ups + 1,
                status: record.status,
                started_at,
                completed_at: record.completed_at,
                prompt: record
                    .follow_up
                    .as_ref()
                    .and_then(|turn| turn.prompt.clone()),
                failure_tail: record.failure_tail.take(),
                opened_by: std::mem::take(&mut record.opened_by),
                report_message_id: record.report_message_id.take(),
                joined_at: None,
            });
        }
        record.deadline_at = record.timeout.map(|timeout| now + timeout).or_else(|| {
            record
                .deadline_at
                .map(|deadline| now + (deadline - started_at))
        });
        record.report_to = ReportTo::Launcher;
        record.follow_ups += 1;
        record.follow_up = Some(FollowUpTurn {
            started_at: now,
            prompt: None,
        });
        record.opened_by.clear();
        record.deadline_notice_at = None;
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

/// A team run spans every turn of its leader until the board flips to Done:
/// a turn end keeps the leader's final message and leaves the run open, and
/// the run follows the leader onto a new conversation row of the same launch,
/// since `session_run_id` routed the observation here by launch id.
fn fold_team_lifecycle(
    record: &mut RunRecord,
    observation: &AgentLifecycleObservation,
    last_message: Option<String>,
) -> LifecycleFold {
    let Some(observed) = observation.agent_id.as_ref() else {
        return LifecycleFold::Ignored;
    };
    let mut changed = record.agent_id.as_ref() != Some(observed);
    record.agent_id = Some(observed.clone());
    if let Some(name) = observation.agent_name.clone() {
        record.agent_name = Some(name);
    }
    if let Some(path) = observation.transcript_path.as_ref()
        && record.transcript_path.as_ref() != Some(path)
    {
        record.transcript_path = Some(path.clone());
        changed = true;
    }
    if observation.signal.terminal_disposition().is_some()
        && let Some(message) = last_message.filter(|message| !message.is_empty())
    {
        record.last_message = Some(message);
        changed = true;
    }
    if record.status == RunStatus::Pending {
        record.status = RunStatus::Running;
        changed = true;
    }
    if changed {
        LifecycleFold::Updated
    } else {
        LifecycleFold::Ignored
    }
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
        // A team run follows its leader across conversation rows of one launch.
        if record.team.is_some() {
            record.agent_id = Some(agent_id.clone());
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
    let agent = find_agent(&snapshot.agents, &record.kind, agent_id)?;
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
