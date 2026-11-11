//! User-global loop task run history.
//!
//! Loop config is per-machine, so task outcomes append to one user-global JSONL
//! log. Each admitted `rimz loop run`/`fire` appends one best-effort record with
//! the terminal result, mode, duration, and capped forensics. `rimz loop list`
//! folds the current and rotated files for summary columns; `rimz loop watch`
//! retains that fold across repaints, rebuilding on rotation or a local date or
//! time-zone change. `rimz loop show` reads records for per-task inspection.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};

use crate::disk::paths::logs_dir;
use crate::harness::schedule::arming;
use crate::harness::schedule::catalog::LoadedTask;
use crate::harness::schedule::runner::RunLockInfo;
use crate::harness::schedule::strikes;
use crate::harness::signal::WatchVerdict;
use crate::ids::MessageId;
use crate::store::event::SignalName;
use crate::store::run::RunStatus;
use serde_json::{Map, Value};

pub type ConditionRecord = crate::harness::when::ConditionEvidence;

const NAME: &str = "loop-runs.log.jsonl";
const MAX_BYTES: u64 = 4 * 1_048_576;
const CHECK_OUTPUT_CAP: usize = 4 * 1024;
const ERROR_CAP: usize = 2 * 1024;
const LAST_MESSAGE_CAP: usize = 2 * 1024;
const COST_WINDOW: usize = 10;

/// Output facts that are useful only while presenting one terminal fire.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoopRunPresentation {
    pub check_duration_ms: Option<u64>,
    pub failure_tail: Option<String>,
    pub skip_reason: Option<String>,
    pub streamed: bool,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunTransition {
    Recorded,
    AutoDisabled { strikes: u32 },
}

/// Why a run report did not land: the run keeps owing it, and the scheduler retries.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(in crate::harness) struct ReportError(#[from] ReportFailure);

#[derive(Debug, thiserror::Error)]
enum ReportFailure {
    #[error("loop history append failed: {0}")]
    Append(#[from] std::io::Error),
    #[error("loop strike state update failed: {0}")]
    Strike(#[from] strikes::StrikesError),
}

/// Attempt the history append before updating strike and arming overlays.
/// Both the append and overlay updates are best-effort: a fire's own row has no retry to hand
/// a failure to. A workflow run's report takes `report_run`'s fallible path instead.
pub(super) fn record_transition(task: &LoadedTask, record: &LoopRunRecord) -> RunTransition {
    append_for_root(&task.entry().resolved_root(), record);
    note_strike(task, record).unwrap_or_else(|err| {
        tracing::warn!(task = record.task, error = %err, "loop strike state update failed");
        RunTransition::Recorded
    })
}

/// Note the record's strike signal, then disable a task that struck out; the disable is
/// best-effort, since the task's next strike attempts it again.
fn note_strike(
    task: &LoadedTask,
    record: &LoopRunRecord,
) -> Result<RunTransition, strikes::StrikesError> {
    let name = &record.task;
    let key = task.key(name);
    let mut signal = strikes::classify(record);
    let noted = match record.run_id.as_deref().filter(|_| record.is_report()) {
        Some(run) => strikes::note_run(&key, run, signal).map(|(count, counted)| {
            if !counted {
                signal = strikes::Signal::Neutral;
            }
            count
        }),
        None => strikes::note(&key, signal),
    };
    let count = noted?;
    let Some(max) = strikes::threshold(task.entry()) else {
        return Ok(RunTransition::Recorded);
    };
    if signal != strikes::Signal::Strike || count < max {
        return Ok(RunTransition::Recorded);
    }
    let transition =
        match arming::disable_if_live(&key, task.source(), Some(count), Timestamp::now()) {
            Ok(true) => RunTransition::AutoDisabled { strikes: count },
            Ok(false) => RunTransition::Recorded,
            Err(err) => {
                tracing::warn!(task = name, error = %err, "loop auto-disable state update failed");
                RunTransition::Recorded
            }
        };
    Ok(transition)
}

/// Append a `rimz loop stop` cancellation, which may outlive its task's row. A
/// cancellation is strike-neutral, so no overlay moves and no row is needed.
pub(super) fn record_stopped(root: &Path, record: &LoopRunRecord, kind: Option<super::TaskKind>) {
    debug_assert_eq!(strikes::classify(record), strikes::Signal::Neutral);
    let mut record = record.clone();
    record.kind = kind;
    append_for_root(root, &record);
}

fn append_for_root(root: &Path, record: &LoopRunRecord) {
    append_to(&logs_dir(), &scoped_to(root, record));
}

fn try_append_for_root(root: &Path, record: &LoopRunRecord) -> std::io::Result<()> {
    let capped = capped_record(&scoped_to(root, record));
    crate::disk::rotating::append_rotating_jsonl(&log_path(&logs_dir()), MAX_BYTES, &capped)
}

fn scoped_to(root: &Path, record: &LoopRunRecord) -> LoopRunRecord {
    let mut scoped_record = record.clone();
    scoped_record.root = Some(root.to_path_buf());
    scoped_record
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopRunRecord {
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<super::TaskKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<PathBuf>,
    /// The fresh worktree this fire created, apart from `checkout`, which keys lock attribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<FireWorktree>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    /// When the fire started for a gate refusal (`TaskFire::record_gate`), and when the record
    /// was written for runs and heard records. It is not append order: order records by their
    /// position in the log.
    pub at: Timestamp,
    pub result: LoopRunResult,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub absorbed: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<LoopRunMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// How long the start throttle held the run before admitting it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throttle_wait_ms: Option<u64>,
    /// How far past its `fire-at` instant a scheduled one-shot started, present only when no
    /// clock saw the instant (two minutes or more).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub late_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<RunLockInfo>,
    /// The lock tenure held by this fire, including when the stop ladder writes its record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_lock: Option<RunLockInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub judge: Vec<JudgeRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<WatchVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<SignalRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<ConditionRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// The task's workflow run still active when an `overlapped` or `deferred` fire found it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_run: Option<crate::ids::WorkflowRunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Set only on a workflow run's terminal report: whether the run started an act step.
    /// A report supersedes the run's launch row and any earlier report of the same run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acted: Option<bool>,
}

/// The fresh worktree one fire of a `fresh-worktree` task created.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FireWorktree {
    pub path: PathBuf,
    /// Why the trunk fetch fell back to the last fetched tip, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_fallback: Option<String>,
}

impl LoopRunRecord {
    /// Whether this row is a workflow run's terminal report rather than a fire.
    fn is_report(&self) -> bool {
        self.acted.is_some()
    }

    /// Whether `self`, a report, replaces `earlier`: the run's launch row or an earlier report.
    fn supersedes(&self, earlier: &Self) -> bool {
        self.is_report()
            && self.run_id.is_some()
            && earlier.run_id == self.run_id
            && (earlier.is_report() || earlier.result == LoopRunResult::Launched)
    }

    /// Carry what the superseded row recorded at fire time and a report cannot know: the
    /// trigger, the lock tenure that attributes refused fires, the fire's own waits, its tree
    /// and its kind.
    fn keep_fire_facts(&mut self, fire: &Self) {
        if self.absorbed == 0 {
            self.absorbed = fire.absorbed;
        }
        self.throttle_wait_ms = self.throttle_wait_ms.or(fire.throttle_wait_ms);
        self.late_ms = self.late_ms.or(fire.late_ms);
        self.holder = self.holder.or(fire.holder);
        self.run_lock = self.run_lock.or(fire.run_lock);
        self.signal = self.signal.take().or_else(|| fire.signal.clone());
        self.condition = self.condition.take().or_else(|| fire.condition.clone());
        self.watch = self.watch.take().or_else(|| fire.watch.clone());
        self.message_id = self.message_id.take().or_else(|| fire.message_id.clone());
        self.target = self.target.take().or_else(|| fire.target.clone());
        self.worktree = self.worktree.take().or_else(|| fire.worktree.clone());
        self.kind = self.kind.or(fire.kind);
    }

    pub fn stages(&self) -> impl DoubleEndedIterator<Item = RunStage<'_>> {
        self.check
            .iter()
            .map(|check| match &check.agent {
                Some(agent) => RunStage::Judge(std::borrow::Cow::Owned(JudgeRecord {
                    profile: agent.profile.clone(),
                    kind: agent.kind.clone(),
                    model: agent.model.clone(),
                    verdict: agent.verdict.as_ref().map(|verdict| verdict.pass),
                    reason: agent.verdict.as_ref().map(|verdict| verdict.reason.clone()),
                    error: agent.error.clone(),
                    cost_usd: agent.cost_usd,
                    input_tokens: agent.input_tokens,
                    output_tokens: agent.output_tokens,
                    duration_ms: check.duration_ms,
                    reused: false,
                    warnings: Vec::new(),
                    retry: None,
                })),
                None => RunStage::Check(check),
            })
            .chain(
                self.judge
                    .iter()
                    .map(|judge| RunStage::Judge(std::borrow::Cow::Borrowed(judge))),
            )
    }

    pub fn last_verdict(&self) -> Option<bool> {
        self.stages().rev().find_map(|stage| stage.verdict())
    }

    pub fn new(
        task: impl Into<String>,
        result: LoopRunResult,
        mode: LoopRunMode,
        duration_ms: u64,
    ) -> Self {
        Self {
            task: task.into(),
            kind: None,
            checkout: None,
            worktree: None,
            root: None,
            at: Timestamp::now(),
            result,
            absorbed: 0,
            mode: Some(mode),
            duration_ms: Some(duration_ms),
            throttle_wait_ms: None,
            late_ms: None,
            holder: None,
            run_lock: None,
            error: None,
            check: None,
            judge: Vec::new(),
            watch: None,
            signal: None,
            condition: None,
            message_id: None,
            run_id: None,
            blocking_run: None,
            transcript_path: None,
            last_message: None,
            target: None,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            acted: None,
        }
    }
}

/// What a run report does when its task strikes out: notify the user. The notice's sender,
/// `sidebar::notify`, sits in a later atlas layer than `harness`, and the engine reports in
/// whichever process makes the run's terminal transition, so the binary registers the CLI's
/// notifier at dispatch, in every process.
/// `loop_repeated_failures_auto_disable_notify_once_and_enable` in the integration suite
/// fails without that registration.
static LOOP_DISABLED: std::sync::OnceLock<fn(&str, &Path, u32)> = std::sync::OnceLock::new();

/// Register the strike-out notice a run report sends; later registrations are ignored.
pub fn on_loop_disabled(notify: fn(&str, &Path, u32)) {
    let _ = LOOP_DISABLED.set(notify);
}

/// Report a finished task-started workflow run to its loop task: one history row carrying the
/// run id, its outcome and its summed attempt cost, and the strike it notes. The report has
/// landed only once both the row and, while the task's row exists, the strike are durable; an
/// error says it has not, and a failed append returns before the strike so a retry notes the
/// two together. The strike store counts a run id once, so a report repeated after a crash or
/// a failed attempt moves no count, and history readers fold a repeated row by its run id.
/// `acted` says the run entered an agent or message step: a scheduled `--until` row is consumed
/// then, and a waiting trigger is drained once the run is over, both only after the report
/// landed. A run in its fire's own tree settles that tree here, so whichever report lands
/// first, the engine's or the backstop's, settles it.
pub(in crate::harness) fn report_run(
    run: &crate::store::workflow::RunRecord,
    journal: &[crate::store::workflow::JournalEntry],
    acted: bool,
) -> Result<RunTransition, ReportError> {
    use crate::store::workflow::{JournalEvent, RunState};
    let Some(task) = run.task.as_deref() else {
        return Ok(RunTransition::Recorded);
    };
    let gave_up = capacity_give_up(run, journal);
    let result = match run.state {
        RunState::Failed if gave_up.is_some() => LoopRunResult::ThrottleSkipped,
        RunState::Failed if let Some(turned) = budget_ended(journal) => {
            if turned {
                LoopRunResult::BudgetExceeded
            } else {
                LoopRunResult::BudgetSkipped
            }
        }
        RunState::Done => LoopRunResult::Completed,
        RunState::Failed => LoopRunResult::Failed,
        RunState::Canceled => LoopRunResult::Canceled,
        RunState::Active | RunState::Parked => return Ok(RunTransition::Recorded),
    };
    let ended = run.terminal_at.unwrap_or_else(Timestamp::now);
    let mode = if run.manual {
        LoopRunMode::Manual
    } else {
        LoopRunMode::Scheduled
    };
    let duration_ms = u64::try_from(ended.duration_since(run.created_at).as_millis()).unwrap_or(0);
    let mut record = LoopRunRecord::new(task, result, mode, duration_ms);
    record.at = ended;
    record.checkout = Some(run.firing_checkout().to_path_buf());
    record.run_id = Some(run.id.to_string());
    let mut costs = journal
        .iter()
        .filter_map(|entry| match entry.event {
            JournalEvent::Attempt { cost_usd, .. } | JournalEvent::Evaluation { cost_usd, .. } => {
                cost_usd
            }
            _ => None,
        })
        .peekable();
    record.cost_usd = costs
        .peek()
        .is_some()
        .then(|| costs.fold(0.0, |sum, cost| sum + cost));
    record.acted = Some(acted);
    // A failed run names why its last attempt ended, as a fire's own refusal would.
    record.error = matches!(
        result,
        LoopRunResult::Failed | LoopRunResult::BudgetSkipped | LoopRunResult::BudgetExceeded
    )
    .then(|| {
        journal.iter().rev().find_map(|entry| match &entry.event {
            JournalEvent::Attempt { reason, .. } => reason.clone(),
            JournalEvent::Route { outcome, input } if outcome == "budget" => {
                input["reason"].as_str().map(str::to_owned)
            }
            _ => None,
        })
    })
    .flatten()
    .or(gave_up);
    if run.fire_tree {
        // The report supersedes the launch row, so it carries the tree that row named.
        record.worktree = task_records(&logs_dir(), task, Some(&run.project_root))
            .into_iter()
            .rev()
            .find(|row| row.run_id == record.run_id)
            .and_then(|row| row.worktree);
    }
    super::runner::settle_fire_tree(run, acted);
    forget_unkept_launch(run, task, acted);
    let catalog = super::catalog::TaskCatalog::load_lenient(Some(&run.project_root));
    let Some(loaded) = catalog.for_run(task) else {
        // A consumed one-shot's row is gone; the row its fire wrote still names the kind.
        record.kind = task_records(&logs_dir(), task, Some(&run.project_root))
            .into_iter()
            .rev()
            .find(|row| row.run_id == record.run_id)
            .and_then(|row| row.kind);
        try_append_for_root(&run.project_root, &record).map_err(ReportFailure::from)?;
        return Ok(RunTransition::Recorded);
    };
    record.kind = Some(loaded.kind());
    try_append_for_root(&loaded.entry().resolved_root(), &record).map_err(ReportFailure::from)?;
    let transition = note_strike(loaded, &record).map_err(ReportFailure::from)?;
    if let RunTransition::AutoDisabled { strikes } = transition
        && let Some(notify) = LOOP_DISABLED.get()
    {
        notify(task, &loaded.entry().resolved_root(), strikes);
    }
    if acted
        && !run.manual
        && loaded.is_ephemeral()
        && loaded.entry().deadline.is_some()
        && let Err(error) = catalog.consume_scheduled(task)
    {
        tracing::warn!(task, %error, "acted --until row not consumed");
    }
    drain_waiting(task, &loaded.entry().resolved_root());
    Ok(transition)
}

/// A run that ended without acting, or failed, leaves its checkout free to launch again: the
/// launch memory keeps a checkout for a run whose act came to an end of its own, and for a
/// canceled run, which only a manual fire replaces.
pub(in crate::harness) fn forget_unkept_launch(
    run: &crate::store::workflow::RunRecord,
    task: &str,
    acted: bool,
) {
    use crate::store::workflow::RunState;
    let kept = match run.state {
        RunState::Canceled => true,
        RunState::Failed => false,
        _ => acted,
    };
    if kept {
        return;
    }
    let forgotten = crate::StatePaths::for_project_root(&run.project_root)
        .map_err(anyhow::Error::from)
        .and_then(|paths| {
            super::launch_ledger_store::forget_runs(&paths, task, std::slice::from_ref(&run.id))
                .map_err(anyhow::Error::from)
        });
    if let Err(error) = forgotten {
        tracing::warn!(task, run = %run.id, error = %format!("{error:#}"), "launch memory kept");
    }
}

/// Whether the run's last attempt ended at its budget, and if so whether a turn ran: a launch
/// or a judge the budget refused has no agent, as a fire whose launch it refused did.
fn budget_ended(journal: &[crate::store::workflow::JournalEntry]) -> Option<bool> {
    use crate::store::workflow::{AttemptOutcome, JournalEvent};
    journal.iter().rev().find_map(|entry| match &entry.event {
        JournalEvent::Attempt {
            outcome, handle, ..
        } => Some((*outcome == AttemptOutcome::Limited).then_some(handle.is_some())),
        JournalEvent::Route { outcome, .. } if outcome == "budget" => Some(Some(false)),
        _ => None,
    })?
}

/// Why a failed run ended by giving up on capacity, worded as a fire's own give-up was: the
/// engine's `capacity` route out of the agent step is the run's last decision.
fn capacity_give_up(
    run: &crate::store::workflow::RunRecord,
    journal: &[crate::store::workflow::JournalEntry],
) -> Option<String> {
    use crate::store::workflow::{JournalEvent, RunState};
    if run.state != RunState::Failed {
        return None;
    }
    let input = journal.iter().rev().find_map(|entry| match &entry.event {
        JournalEvent::Route { outcome, input } => Some((outcome == "capacity").then_some(input)),
        JournalEvent::Attempt { .. } => Some(None),
        _ => None,
    })??;
    let waited = std::time::Duration::from_millis(input["waited_ms"].as_u64().unwrap_or(0));
    Some(format!(
        "gave up waiting for capacity after {}; {}",
        crate::utils::time::format_duration_compact(waited),
        input["reason"].as_str().unwrap_or("no capacity")
    ))
}

/// Start the next waiting trigger now that the run holding the task is over.
fn drain_waiting(task: &str, root: &Path) {
    let result = (|| -> anyhow::Result<()> {
        let paths = crate::StatePaths::for_project_root(root)?;
        let waiting = super::backlog::load(&paths)?
            .tasks
            .get(task)
            .is_some_and(|task| !task.triggers.is_empty());
        if waiting {
            let runtime = crate::RuntimePaths::for_state(&paths)?;
            super::fire::spawn_loop_drain(
                &runtime,
                root,
                task,
                super::fire::LoopRunHost::Detached,
            )?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        tracing::warn!(task, %error, "loop backlog drain not started; the tick retries it");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopRunMode {
    Scheduled,
    Manual,
}

impl LoopRunMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Manual => "manual",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckRecord {
    pub code: Option<i32>,
    pub timed_out: bool,
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Legacy model-check rows only; new fires write the `judge` array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentCheckRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentCheckRecord {
    pub profile: String,
    pub kind: crate::ids::AgentKind,
    pub model: Option<String>,
    pub verdict: Option<crate::agents::HeadlessVerdict>,
    pub error: Option<String>,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JudgeRecord {
    pub profile: String,
    pub kind: crate::ids::AgentKind,
    pub model: Option<String>,
    pub verdict: Option<bool>,
    pub reason: Option<String>,
    pub error: Option<String>,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub reused: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// The first attempt's error when the judge ran a second time; usage and
    /// duration then sum both attempts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "a short-lived projection; only legacy rows own their judge, and boxing it would allocate for every read"
)]
pub enum RunStage<'a> {
    Check(&'a CheckRecord),
    Judge(std::borrow::Cow<'a, JudgeRecord>),
}

impl RunStage<'_> {
    pub fn verdict(&self) -> Option<bool> {
        match self {
            Self::Check(check) => Some(check.code == Some(0) && !check.timed_out),
            Self::Judge(judge) => judge.verdict,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalRecord {
    pub name: SignalName,
    pub payload: Map<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopRunResult {
    Launched,
    Completed,
    Failed,
    VerifyFailed,
    TimedOut,
    BudgetExceeded,
    BudgetSkipped,
    SurplusSkipped,
    AccountSkipped,
    ThrottleSkipped,
    CapacityHeld,
    Canceled,
    Delivered,
    TargetGone,
    CheckSkipped,
    SignalSkipped,
    WorkflowSkipped,
    Expired,
    Errored,
    StartFailed,
    Overlapped,
    Deferred,
    Replaced,
    TakeoverBlocked,
}

impl LoopRunResult {
    pub const fn spawn_exit_code(self) -> Option<i32> {
        match self {
            Self::Completed => Some(RunStatus::Completed.exit_code()),
            Self::Failed => Some(RunStatus::Failed.exit_code()),
            Self::VerifyFailed => Some(RunStatus::VerifyFailed.exit_code()),
            Self::TimedOut => Some(RunStatus::TimedOut.exit_code()),
            Self::BudgetExceeded => Some(RunStatus::BudgetExceeded.exit_code()),
            Self::Canceled | Self::Replaced => Some(RunStatus::Canceled.exit_code()),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Launched => "launched",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::VerifyFailed => "verify failed",
            Self::TimedOut => "timed out",
            Self::BudgetExceeded => "budget exceeded",
            Self::BudgetSkipped => "budget skipped",
            Self::SurplusSkipped => "surplus skipped",
            Self::AccountSkipped => "account skipped",
            Self::ThrottleSkipped => "throttle skipped",
            Self::CapacityHeld => "capacity held",
            Self::Canceled => "canceled",
            Self::Deferred => "deferred",
            Self::Replaced => "replaced",
            Self::Delivered => "delivered",
            Self::TargetGone => "target gone",
            Self::CheckSkipped | Self::SignalSkipped | Self::WorkflowSkipped => "skipped",
            Self::Expired => "expired",
            Self::Errored => "error",
            Self::StartFailed => "start failed",
            Self::Overlapped => "overlapped",
            Self::TakeoverBlocked => "takeover blocked",
        }
    }
}

impl From<RunStatus> for LoopRunResult {
    fn from(status: RunStatus) -> Self {
        match status {
            RunStatus::Completed => Self::Completed,
            RunStatus::Failed => Self::Failed,
            RunStatus::VerifyFailed => Self::VerifyFailed,
            RunStatus::TimedOut => Self::TimedOut,
            RunStatus::BudgetExceeded => Self::BudgetExceeded,
            RunStatus::Canceled => Self::Canceled,
            RunStatus::Pending | RunStatus::Running => Self::Failed,
        }
    }
}

/// The outcome of an acting run, including a successful check-only fire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPolarity {
    Good,
    Failure,
}

impl LoopRunRecord {
    pub fn polarity(&self) -> Option<RunPolarity> {
        match self.result {
            // A workflow run's launch is not an outcome; its report is.
            LoopRunResult::Launched if self.run_id.is_some() => None,
            LoopRunResult::Completed | LoopRunResult::Delivered | LoopRunResult::Launched => {
                Some(RunPolarity::Good)
            }
            LoopRunResult::CheckSkipped if self.last_verdict() == Some(true) => {
                Some(RunPolarity::Good)
            }
            LoopRunResult::Errored
            | LoopRunResult::StartFailed
            | LoopRunResult::Failed
            | LoopRunResult::VerifyFailed
            | LoopRunResult::TimedOut
            | LoopRunResult::Replaced
            | LoopRunResult::BudgetExceeded => Some(RunPolarity::Failure),
            _ => None,
        }
    }
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// The newest acting record of a task, in log order, with the streak of acting records of its polarity that ends at it.
#[derive(Clone, Debug, PartialEq)]
pub struct ActingRun {
    pub record: LoopRunRecord,
    pub polarity: RunPolarity,
    pub streak: usize,
    /// When the acting record of the other polarity just before the streak ran.
    pub since: Option<Timestamp>,
}

impl ActingRun {
    fn after(previous: Option<Self>, record: &LoopRunRecord) -> Option<Self> {
        let Some(polarity) = record.polarity() else {
            return previous;
        };
        let (streak, since) = match previous {
            Some(previous) if previous.polarity == polarity => {
                (previous.streak + 1, previous.since)
            }
            Some(previous) => (1, Some(previous.record.at)),
            None => (1, None),
        };
        Some(Self {
            record: record.clone(),
            polarity,
            streak,
            since,
        })
    }
}

/// The newest signal a subscription heard without acting on it, in log order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HeardSignal {
    pub signal: SignalName,
    pub at: Timestamp,
}

impl HeardSignal {
    fn of(record: &LoopRunRecord) -> Option<Self> {
        Some(Self {
            signal: record.signal.as_ref()?.name.clone(),
            at: record.at,
        })
    }
}

/// Fold one task's records, in log order, to its newest acting run.
pub fn acting_run(records: &[LoopRunRecord]) -> Option<ActingRun> {
    records.iter().fold(None, ActingRun::after)
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoopRunStats {
    pub runs: usize,
    pub last: Option<LoopRunRecord>,
    pub last_kind: Option<(Timestamp, super::TaskKind)>,
    pub spend_today_usd: f64,
    pub acting: Option<ActingRun>,
    pub heard: Option<HeardSignal>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TaskCostSummary {
    pub last_usd: Option<f64>,
    pub avg_usd: Option<f64>,
    pub costed_runs: usize,
}

fn log_path(state_root: &Path) -> PathBuf {
    state_root.join(NAME)
}

fn append_to(state_root: &Path, record: &LoopRunRecord) {
    let capped = capped_record(record);
    crate::disk::rotating::append(&log_path(state_root), MAX_BYTES, &capped);
}

pub fn stats(
    state_root: &Path,
    now: &Zoned,
    root: Option<&Path>,
) -> BTreeMap<String, LoopRunStats> {
    StatsFold::new(state_root, now).refresh(root, now).clone()
}

/// Per-room run summaries retained across dashboard repaints.
pub struct HeldStats {
    state_root: PathBuf,
    roots: BTreeMap<PathBuf, StatsFold>,
}

impl HeldStats {
    pub fn new(state_root: &Path) -> Self {
        Self {
            state_root: state_root.into(),
            roots: BTreeMap::new(),
        }
    }

    pub fn stats(&mut self, root: &Path, now: &Zoned) -> &BTreeMap<String, LoopRunStats> {
        let fold = self
            .roots
            .entry(root.into())
            .or_insert_with(|| StatsFold::new(&self.state_root, now));
        if fold.date != now.date() || fold.zone != *now.time_zone() {
            *fold = StatsFold::new(&self.state_root, now);
        }
        fold.refresh(Some(root), now)
    }
}

/// The per-task summaries, and per task and workflow run whether its launch or report was folded.
#[derive(Default)]
struct FoldState {
    stats: BTreeMap<String, LoopRunStats>,
    runs: BTreeMap<(String, String), bool>,
}

struct StatsFold {
    reader: crate::disk::rotating::IncrementalFold<FoldState>,
    date: jiff::civil::Date,
    zone: jiff::tz::TimeZone,
}

impl StatsFold {
    fn new(state_root: &Path, now: &Zoned) -> Self {
        Self {
            reader: crate::disk::rotating::IncrementalFold::new(log_path(state_root)),
            date: now.date(),
            zone: now.time_zone().clone(),
        }
    }

    fn refresh(&mut self, root: Option<&Path>, now: &Zoned) -> &BTreeMap<String, LoopRunStats> {
        &self
            .reader
            .refresh(|state, record: LoopRunRecord| {
                if matches_root(&record, root) {
                    fold_record(record, now, state);
                }
            })
            .stats
    }
}

pub fn task_records(state_root: &Path, task: &str, root: Option<&Path>) -> Vec<LoopRunRecord> {
    let mut records: Vec<LoopRunRecord> = Vec::new();
    crate::disk::rotating::visit_records(&log_path(state_root), |mut record: LoopRunRecord| {
        if record.task == task && matches_root(&record, root) {
            // A fast run can report before its launcher appends the launch row.
            if record.result == LoopRunResult::Launched
                && let Some(report) = records.iter_mut().find(|later| later.supersedes(&record))
            {
                report.keep_fire_facts(&record);
                return;
            }
            records.retain(|earlier| {
                let superseded = record.supersedes(earlier);
                if superseded {
                    record.keep_fire_facts(earlier);
                }
                !superseded
            });
            records.push(record);
        }
    });
    records
}

#[derive(Clone, Debug, Default)]
pub struct TaskHistory {
    pub fires: Vec<LoopRunRecord>,
    pub heard: Option<HeardSummary>,
    pub records: Vec<LoopRunRecord>,
}

#[derive(Clone, Debug, Serialize)]
pub struct HeardSummary {
    pub count: usize,
    pub since: Option<Timestamp>,
    pub latest: SignalName,
    pub latest_at: Timestamp,
}

impl TaskHistory {
    pub fn is_retired_one_off(&self) -> bool {
        !self.fires.is_empty()
            && self
                .records
                .iter()
                .fold(None, last_kind_after)
                .map(|(_, kind)| kind)
                == Some(super::TaskKind::OneOff)
    }

    fn split(mut records: Vec<LoopRunRecord>, armed_at: Option<Timestamp>) -> Self {
        records.retain(|record| armed_at.is_none_or(|at| record.at >= at));
        let mut fires = Vec::new();
        let mut heard = None;
        let mut count = 0;
        let mut since = None;
        for record in &records {
            if record.result == LoopRunResult::SignalSkipped {
                count += 1;
                if let Some(signal) = HeardSignal::of(record) {
                    heard = Some(HeardSummary {
                        count,
                        since,
                        latest: signal.signal,
                        latest_at: signal.at,
                    });
                } else if let Some(summary) = &mut heard {
                    summary.count = count;
                }
                continue;
            }
            if record.polarity().is_some() {
                since = Some(record.at);
                count = 0;
                if let Some(summary) = &mut heard {
                    summary.count = 0;
                    summary.since = since;
                }
            }
            fires.push(record.clone());
        }
        Self {
            fires,
            heard,
            records,
        }
    }
}

pub fn task_history(
    state_root: &Path,
    task: &str,
    root: Option<&Path>,
    armed_at: Option<Timestamp>,
) -> TaskHistory {
    TaskHistory::split(task_records(state_root, task, root), armed_at)
}

pub(super) fn has_scheduled_row_since(
    state_root: &Path,
    task: &str,
    root: &Path,
    checkout: Option<&Path>,
    since: Timestamp,
) -> bool {
    has_scheduled_row_where(state_root, task, root, checkout, since, |_| true)
}

/// Whether a runner recorded a scheduled attempt since `since`: any scheduled
/// row but `start failed`, which the pass writes for a runner that never ran.
pub(super) fn has_recorded_attempt_since(
    state_root: &Path,
    task: &str,
    root: &Path,
    since: Timestamp,
) -> bool {
    has_scheduled_row_where(state_root, task, root, None, since, |result| {
        result != LoopRunResult::StartFailed
    })
}

fn has_scheduled_row_where(
    state_root: &Path,
    task: &str,
    root: &Path,
    checkout: Option<&Path>,
    since: Timestamp,
    counts: impl Fn(LoopRunResult) -> bool,
) -> bool {
    let mut found = false;
    crate::disk::rotating::visit_records(&log_path(state_root), |record: LoopRunRecord| {
        if record.task == task
            && matches_root(&record, Some(root))
            && checkout.is_none_or(|checkout| record.checkout.as_deref() == Some(checkout))
            && record.mode == Some(LoopRunMode::Scheduled)
            && record.at >= since
            && counts(record.result)
        {
            found = true;
        }
    });
    found
}

fn matches_root(record: &LoopRunRecord, root: Option<&Path>) -> bool {
    root.zip(record.root.as_deref())
        .is_none_or(|(root, recorded)| root == recorded)
}

pub fn spend_on_local_day(records: &[LoopRunRecord], now: &Zoned) -> f64 {
    records
        .iter()
        .filter_map(|record| cost_on_local_day(record, now))
        // `Sum` for floats starts at -0.0, which a no-spend day would print as `$-0.00`.
        .fold(0.0, |total, cost| total + cost)
}

pub fn has_cost_on_local_day(records: &[LoopRunRecord], now: &Zoned) -> bool {
    records
        .iter()
        .any(|record| cost_on_local_day(record, now).is_some())
}

pub fn cost_summary(records: &[LoopRunRecord]) -> TaskCostSummary {
    let costs = records
        .iter()
        .rev()
        .filter_map(|record| record.cost_usd)
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
        .take(COST_WINDOW)
        .collect::<Vec<_>>();
    let costed_runs = costs.len();
    let last_usd = costs.first().copied();
    let avg_usd = (costed_runs > 0).then(|| {
        costs
            .iter()
            .map(|cost| cost / costed_runs as f64)
            .sum::<f64>()
    });
    TaskCostSummary {
        last_usd,
        avg_usd,
        costed_runs,
    }
}

fn cost_on_local_day(record: &LoopRunRecord, now: &Zoned) -> Option<f64> {
    (record.at.to_zoned(now.time_zone().clone()).date() == now.date())
        .then_some(record.cost_usd)
        .flatten()
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DailyBudgetGate {
    pub(super) spend_usd: f64,
    pub(super) cap_usd: f64,
    pub(super) reserved_usd: f64,
    /// What the task's live runs hold back until they report their cost.
    pub(super) live_usd: f64,
}

impl DailyBudgetGate {
    pub(super) fn reason(self) -> String {
        let spent = if self.live_usd > 0.0 {
            format!(
                "${:.2} spent, ${:.2} held by live runs",
                self.spend_usd, self.live_usd
            )
        } else {
            format!("${:.2} spent", self.spend_usd)
        };
        if self.reserved_usd > 0.0 {
            format!(
                "daily budget ${:.2} cannot fund the next ${:.2} run ({spent})",
                self.cap_usd, self.reserved_usd
            )
        } else {
            format!("daily budget ${:.2} exhausted ({spent})", self.cap_usd)
        }
    }
}

pub(super) fn daily_budget_gate(
    state_root: &Path,
    task: &str,
    entry: &crate::config::TaskEntry,
    now: &Zoned,
) -> std::result::Result<Option<DailyBudgetGate>, String> {
    let Some(raw_cap) = entry.budget_per_day.as_deref() else {
        return Ok(None);
    };
    raw_cap
        .parse::<crate::harness::budget::BudgetSpec>()
        .map_err(|err| format!("task `{task}` has invalid budget-per-day: {err}"))?;
    let raw_reserved = entry
        .budget
        .as_deref()
        .ok_or_else(|| format!("task `{task}` uses budget-per-day without a per-run budget"))?;
    let reserved = raw_reserved
        .parse::<crate::harness::budget::BudgetSpec>()
        .map_err(|err| format!("task `{task}` has invalid budget: {err}"))?
        .cap_usd;
    reserved_budget_gate(state_root, task, entry, now, reserved, 0.0)
}

/// The daily gate for a task whose runs are workflow runs: the next run reserves its graph's
/// agent-step budgets, and each live run holds back its own until it reports.
pub(super) fn reserved_budget_gate(
    state_root: &Path,
    task: &str,
    entry: &crate::config::TaskEntry,
    now: &Zoned,
    reserved: f64,
    live: f64,
) -> std::result::Result<Option<DailyBudgetGate>, String> {
    let Some(raw_cap) = entry.budget_per_day.as_deref() else {
        return Ok(None);
    };
    let cap = raw_cap
        .parse::<crate::harness::budget::BudgetSpec>()
        .map_err(|err| format!("task `{task}` has invalid budget-per-day: {err}"))?
        .cap_usd;
    let spend = spend_on_local_day(
        &task_records(state_root, task, Some(&entry.resolved_root())),
        now,
    );
    let held = spend + live;
    Ok(
        ((held >= cap) || (reserved > 0.0 && held + reserved > cap)).then_some(DailyBudgetGate {
            spend_usd: spend,
            cap_usd: cap,
            reserved_usd: reserved,
            live_usd: live,
        }),
    )
}

fn last_kind_after(
    previous: Option<(Timestamp, super::TaskKind)>,
    record: &LoopRunRecord,
) -> Option<(Timestamp, super::TaskKind)> {
    match record.kind {
        Some(kind) if previous.is_none_or(|(at, _)| record.at >= at) => Some((record.at, kind)),
        _ => previous,
    }
}

fn fold_record(record: LoopRunRecord, now: &Zoned, state: &mut FoldState) {
    let mut repeat = false;
    if let Some(run) = record
        .run_id
        .clone()
        .filter(|_| record.is_report() || record.result == LoopRunResult::Launched)
    {
        let key = (record.task.clone(), run);
        match state.runs.get(&key) {
            // The run's report is counted, whether it came before or after its launch row.
            Some(true) => return,
            Some(_) => repeat = true,
            None => {}
        }
        *state.runs.entry(key).or_default() |= record.is_report();
    }
    let entry = state
        .stats
        .entry(record.task.clone())
        .or_insert(LoopRunStats {
            runs: 0,
            last: None,
            last_kind: None,
            spend_today_usd: 0.0,
            acting: None,
            heard: None,
        });
    entry.last_kind = last_kind_after(entry.last_kind, &record);
    if record.result == LoopRunResult::SignalSkipped {
        if let Some(heard) = HeardSignal::of(&record) {
            entry.heard = Some(heard);
        }
        return;
    }
    if !repeat {
        entry.runs += 1;
    }
    entry.spend_today_usd += cost_on_local_day(&record, now).unwrap_or(0.0);
    entry.acting = ActingRun::after(entry.acting.take(), &record);
    if entry.last.as_ref().is_none_or(|last| record.at >= last.at) {
        entry.last = Some(record);
    }
}

fn capped_record(record: &LoopRunRecord) -> LoopRunRecord {
    let mut capped = record.clone();
    if let Some(error) = &mut capped.error {
        *error = tail_string(error, ERROR_CAP);
    }
    if let Some(check) = &mut capped.check {
        check.output = tail_string(&check.output, CHECK_OUTPUT_CAP);
        if let Some(agent) = &mut check.agent {
            if let Some(verdict) = &mut agent.verdict {
                verdict.reason = tail_string(&verdict.reason, CHECK_OUTPUT_CAP);
            }
            if let Some(error) = &mut agent.error {
                *error = tail_string(error, CHECK_OUTPUT_CAP);
            }
        }
    }
    for judge in &mut capped.judge {
        if let Some(reason) = &mut judge.reason {
            *reason = tail_string(reason, CHECK_OUTPUT_CAP);
        }
        if let Some(error) = &mut judge.error {
            *error = tail_string(error, CHECK_OUTPUT_CAP);
        }
        if let Some(retry) = &mut judge.retry {
            *retry = tail_string(retry, CHECK_OUTPUT_CAP);
        }
    }
    if let Some(signal) = &mut capped.signal
        && serde_json::to_vec(&signal.payload).is_ok_and(|bytes| bytes.len() > CHECK_OUTPUT_CAP)
    {
        let rendered = serde_json::to_string(&signal.payload).unwrap_or_default();
        signal.payload = Map::from_iter([(
            "_truncated".to_owned(),
            Value::String(tail_string(&rendered, CHECK_OUTPUT_CAP)),
        )]);
    }
    if let Some(last_message) = &mut capped.last_message {
        *last_message = tail_string(last_message, LAST_MESSAGE_CAP);
    }
    capped
}

fn tail_string(value: &str, cap: usize) -> String {
    if value.len() <= cap {
        return value.to_owned();
    }
    let mut start = value.len() - cap;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].to_owned()
}

#[cfg(test)]
mod tests;
