//! User-global loop task run history.
//!
//! Loop config is per-machine, so task outcomes append to one user-global JSONL
//! log. Each admitted `rimz loop run`/`fire` that decides without a workflow run
//! appends one best-effort record with the terminal result, mode, duration, and
//! capped forensics. A fire that starts a workflow run appends nothing itself: the
//! run's collection appends its one report row, built from the facts the fire
//! sealed into the run, or, for a quiet run, writes the task's quiet summary in
//! `fires` instead and removes the run's home. History readers add a `launched`
//! row for each run still in flight and merge the quiet summary. `rimz loop list`
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
    #[error("loop fire record update failed: {0}")]
    Fires(#[from] super::overlay_store::OverlayError),
    #[error("launch memory not forgotten: {0:#}")]
    Forget(anyhow::Error),
}

/// Attempt the history append before updating strike and arming overlays.
/// Both the append and overlay updates are best-effort: a fire's own row has no retry to hand
/// a failure to. A workflow run's report takes `report_run`'s fallible path instead.
///
/// A workflow run's launch appends no row, since the run's report is its fire's one row: a
/// scheduled launch stamps the task's launch proof instead, which a failure only warns about.
pub(super) fn record_transition(task: &LoadedTask, record: &LoopRunRecord) -> RunTransition {
    let key = task.key(&record.task);
    if record.result == LoopRunResult::Launched && record.run_id.is_some() {
        if record.mode == Some(LoopRunMode::Scheduled)
            && let Err(error) =
                super::fires::stamp_scheduled(&key, record.checkout.as_deref(), record.at)
        {
            tracing::warn!(task = record.task, %error, "loop launch proof not stamped");
        }
        return RunTransition::Recorded;
    }
    append_for_root(&task.entry().resolved_root(), record);
    end_quiet_streak(&key, record);
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
    /// Readers keep a run's first report and drop any later one of the same run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acted: Option<bool>,
}

/// The launch facts a workflow fire seals into its run for the run's report row, capped as a
/// logged row is.
pub(super) fn fire_facts(record: &LoopRunRecord) -> Value {
    serde_json::to_value(capped_record(record)).unwrap_or(Value::Null)
}

/// A task's quiet fires: runs that ended done without starting an agent, a judge or a message,
/// which leave no home and no history row. `streak` counts those since the task's last failure
/// row; `last` is the newest one's `completed` row, with its output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuietFires {
    pub count: u64,
    pub streak: u64,
    pub last: LoopRunRecord,
}

impl QuietFires {
    /// The stored streak as the task's rows leave it: zero once a failure row is newer than the
    /// last quiet fire.
    pub fn streak_beside(&self, acting: Option<&ActingRun>) -> u64 {
        let failed_since = acting.is_some_and(|acting| match acting.polarity {
            RunPolarity::Failure => acting.record.at > self.last.at,
            RunPolarity::Good => acting.since.is_some_and(|since| since > self.last.at),
        });
        if failed_since { 0 } else { self.streak }
    }
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

    /// Merge what the fire recorded at launch and a report cannot know: the trigger, the lock
    /// tenure that attributes refused fires, the fire's own waits, its tree and its kind.
    fn with_fire_facts(&mut self, fire: Self) {
        if self.absorbed == 0 {
            self.absorbed = fire.absorbed;
        }
        self.throttle_wait_ms = self.throttle_wait_ms.or(fire.throttle_wait_ms);
        self.late_ms = self.late_ms.or(fire.late_ms);
        self.holder = self.holder.or(fire.holder);
        self.run_lock = self.run_lock.or(fire.run_lock);
        self.signal = self.signal.take().or(fire.signal);
        self.condition = self.condition.take().or(fire.condition);
        self.watch = self.watch.take().or(fire.watch);
        self.message_id = self.message_id.take().or(fire.message_id);
        self.target = self.target.take().or(fire.target);
        self.worktree = self.worktree.take().or(fire.worktree);
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

/// Collect a finished task-started workflow run: report its outcome to its loop task, under the
/// workspace lock, once. A run that owes nothing is left alone. A quiet run whose task row exists
/// is forgotten: its home is deleted, record first, so a crash leaves either a run still owed or
/// a home without a record, which GC removes. Any other run gets its report row and is marked
/// collected. A report that did not land leaves the run owed, and the scheduler's backstop
/// advance of the run, whose command collects after it, retries it.
pub fn collect_run(state: &crate::StatePaths, run: &crate::ids::WorkflowRunId) {
    if let Err(error) = try_collect_run(state, run) {
        tracing::warn!(%run, error = %format!("{error:#}"), "workflow run not collected");
    }
}

fn try_collect_run(
    state: &crate::StatePaths,
    run: &crate::ids::WorkflowRunId,
) -> anyhow::Result<()> {
    use crate::store::workflow;
    let paths = state.workflow(run);
    let guard = crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock)?;
    // A run that left the live directory owes nothing, and one this RimZ does not run is the
    // workflow pass's to report.
    let workflow::RunClass::Current(mut record) = workflow::classify(&paths)? else {
        return Ok(());
    };
    if !record.owes_loop_report() {
        return Ok(());
    }
    let outcome = record
        .outcome
        .clone()
        .ok_or_else(|| anyhow::anyhow!("the run's record carries no outcome"))?;
    match report_run(&record, &outcome)? {
        Collected::Reported(_) => {
            record.loop_report = workflow::LoopReport::Sent;
            workflow::write(&paths, &record, &guard)?;
        }
        Collected::Forgotten => {
            std::fs::remove_file(&paths.record)?;
            std::fs::remove_dir_all(&paths.home)?;
        }
    }
    Ok(())
}

/// Collect every finished run of `task` in `root` that still owes its report, and say whether
/// one was owed, so a fire decides on the strikes, rows and launch memory those runs leave.
pub fn collect_task_runs(root: &Path, task: &str) -> bool {
    let owed = (|| -> anyhow::Result<_> {
        let state = crate::StatePaths::for_project_root(root)?;
        if !state.root.exists() {
            return Ok(None);
        }
        let runs: Vec<_> = crate::store::workflow::live_runs(&state)?
            .into_iter()
            .filter(|record| record.ran_for(task) && record.owes_loop_report())
            .map(|record| record.id)
            .collect();
        Ok(Some((state, runs)))
    })();
    match owed {
        Ok(Some((state, runs))) => {
            for run in &runs {
                collect_run(&state, run);
            }
            !runs.is_empty()
        }
        Ok(None) => false,
        Err(error) => {
            tracing::warn!(task, error = %format!("{error:#}"), "owed workflow runs not read");
            false
        }
    }
}

/// Apply a collected run's launch-memory rule again, for a launcher that remembered the run only
/// after its collection forgot it: a fast run can end before its launcher writes the memory. The
/// workspace lock orders this read after a collection in flight, which forgets under that lock.
/// A run with no home was forgotten as quiet, which leaves nothing to remember.
pub(super) fn forget_collected_launch(
    state: &crate::StatePaths,
    task: &str,
    run: &crate::ids::WorkflowRunId,
) -> anyhow::Result<()> {
    use crate::store::workflow;
    let _guard = crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock)?;
    let Some(found) = workflow::find(state, run)? else {
        return Ok(super::launch_ledger_store::forget_runs(
            state,
            task,
            std::slice::from_ref(run),
        )?);
    };
    let (ended, acted, project) = match &found.class {
        workflow::RunClass::Current(record)
            if found.finished || record.loop_report == workflow::LoopReport::Sent =>
        {
            let Some(outcome) = &record.outcome else {
                return Ok(());
            };
            (
                record.state,
                outcome.acted,
                Some(record.project_root.clone()),
            )
        }
        workflow::RunClass::Current(_) => return Ok(()),
        class if found.finished => {
            let Some(header) = class.header() else {
                return Ok(());
            };
            let Some(ended) = header.state.filter(|state| state.is_terminal()) else {
                return Ok(());
            };
            // A header that does not say whether its run acted keeps its memory.
            let acted = header.acted.unwrap_or(true);
            (ended, acted, header.project_root.clone())
        }
        _ => return Ok(()),
    };
    // The collection forgot in the run's project store; this forgets there too.
    match project {
        Some(root) => forget_unkept_launch(
            &crate::StatePaths::for_project_root(&root)?,
            ended,
            task,
            run,
            acted,
        ),
        None => forget_unkept_launch(state, ended, task, run, acted),
    }
}

/// What a run report does when its task strikes out: notify the user. The notice's sender,
/// `sidebar::notify`, sits in a later atlas layer than `harness`, and runs are collected from
/// the CLI commands that end them or fire their task, so the binary registers the CLI's
/// notifier at dispatch, in every process.
/// `loop_repeated_failures_auto_disable_notify_once_and_enable` in the integration suite
/// fails without that registration.
static LOOP_DISABLED: std::sync::OnceLock<fn(&str, &Path, u32)> = std::sync::OnceLock::new();

/// Register the strike-out notice a run report sends; later registrations are ignored.
pub fn on_loop_disabled(notify: fn(&str, &Path, u32)) {
    let _ = LOOP_DISABLED.set(notify);
}

/// What a collection did with a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::harness) enum Collected {
    /// One report row landed; the caller marks the run collected.
    Reported(RunTransition),
    /// The run was quiet: its task's quiet summary took it, and the caller deletes its home.
    Forgotten,
}

/// What one report row says about a finished run, apart from its outcome: enough for a run
/// this RimZ cannot decode, read from its header.
pub(in crate::harness) struct ReportFacts<'a> {
    pub(in crate::harness) task: &'a str,
    pub(in crate::harness) run: &'a crate::ids::WorkflowRunId,
    pub(in crate::harness) manual: bool,
    pub(in crate::harness) created_at: Timestamp,
    pub(in crate::harness) terminal_at: Option<Timestamp>,
    pub(in crate::harness) checkout: &'a Path,
    /// The launch facts the fire sealed into the run, merged into the row.
    pub(in crate::harness) fire_facts: Option<&'a Value>,
}

impl<'a> ReportFacts<'a> {
    fn of(run: &'a crate::store::workflow::RunRecord, task: &'a str) -> Self {
        Self {
            task,
            run: &run.id,
            manual: run.manual,
            created_at: run.created_at,
            terminal_at: run.terminal_at,
            checkout: run.firing_checkout(),
            fire_facts: run.fire_facts.as_ref(),
        }
    }
}

/// The report row of a finished run: its outcome as a loop result, the run id, its summed attempt
/// cost, why a failed run ended, and the facts its fire sealed. The row names no kind when the
/// facts do not; the caller sets the task row's.
pub(in crate::harness) fn report_row(
    facts: &ReportFacts<'_>,
    outcome: &crate::store::workflow::RunOutcome,
) -> LoopRunRecord {
    use crate::store::workflow::RunEnding;
    let result = match outcome.ending {
        RunEnding::Done => LoopRunResult::Completed,
        RunEnding::Failed => LoopRunResult::Failed,
        RunEnding::Canceled => LoopRunResult::Canceled,
        RunEnding::CapacityGaveUp { .. } => LoopRunResult::ThrottleSkipped,
        RunEnding::Budget { turned: true } => LoopRunResult::BudgetExceeded,
        RunEnding::Budget { turned: false } => LoopRunResult::BudgetSkipped,
    };
    let ended = facts.terminal_at.unwrap_or_else(Timestamp::now);
    let mode = if facts.manual {
        LoopRunMode::Manual
    } else {
        LoopRunMode::Scheduled
    };
    let duration_ms =
        u64::try_from(ended.duration_since(facts.created_at).as_millis()).unwrap_or(0);
    let mut record = LoopRunRecord::new(facts.task, result, mode, duration_ms);
    if let Some(fire) = facts
        .fire_facts
        .and_then(|facts| LoopRunRecord::deserialize(facts).ok())
    {
        record.with_fire_facts(fire);
    }
    record.at = ended;
    record.checkout = Some(facts.checkout.to_path_buf());
    record.run_id = Some(facts.run.to_string());
    record.cost_usd = outcome.cost_usd;
    record.acted = Some(outcome.acted);
    // A failed run names why its last attempt ended, as a fire's own refusal would.
    record.error = match &outcome.ending {
        RunEnding::Failed | RunEnding::Budget { .. } => outcome.reason.clone(),
        RunEnding::CapacityGaveUp { waited_ms, reason } => Some(format!(
            "gave up waiting for capacity after {}; {}",
            crate::utils::time::format_duration_compact(std::time::Duration::from_millis(
                *waited_ms
            )),
            reason.as_deref().unwrap_or("no capacity")
        )),
        RunEnding::Done | RunEnding::Canceled => None,
    };
    record
}

/// Report a finished task-started workflow run to its loop task. A run in its fire's own tree
/// settles that tree first, so whichever collection lands first settles it, and the launch memory
/// of a run that failed or ended unacted is forgotten; a failed forget fails the report.
///
/// A quiet run whose task row exists writes no row: its strike reset is noted through the strike
/// store, which counts a run id once, and only a run id it counts for the first time adds to the
/// task's quiet summary and replaces its last output.
///
/// Any other run appends one row and, while the task's row exists, notes its strike. The report
/// has landed only once both are durable; an error says it has not, and a failed append returns
/// before the strike so a retry notes the two together, and history readers fold a repeated row
/// by its run id. When the run entered an agent or message step, a scheduled `--until` row is
/// consumed. Either way a waiting trigger is drained once the report landed.
pub(in crate::harness) fn report_run(
    run: &crate::store::workflow::RunRecord,
    outcome: &crate::store::workflow::RunOutcome,
) -> Result<Collected, ReportError> {
    let Some(task) = run.task.as_deref() else {
        return Ok(Collected::Reported(RunTransition::Recorded));
    };
    if !run.state.is_terminal() {
        return Ok(Collected::Reported(RunTransition::Recorded));
    }
    let acted = outcome.acted;
    super::runner::settle_fire_tree(run, acted);
    crate::StatePaths::for_project_root(&run.project_root)
        .map_err(anyhow::Error::from)
        .and_then(|state| forget_unkept_launch(&state, run.state, task, &run.id, acted))
        .map_err(ReportFailure::Forget)?;
    let mut record = report_row(&ReportFacts::of(run, task), outcome);
    let catalog = super::catalog::TaskCatalog::load_lenient(Some(&run.project_root));
    let Some(loaded) = catalog.for_run(task) else {
        // A consumed one-shot's row is gone; the facts its fire sealed still name the kind.
        try_append_for_root(&run.project_root, &record).map_err(ReportFailure::from)?;
        return Ok(Collected::Reported(RunTransition::Recorded));
    };
    record.kind = Some(loaded.kind());
    let root = loaded.entry().resolved_root();
    let key = loaded.key(task);
    if outcome.quiet {
        let (_, counted) = strikes::note_run(&key, &run.id.to_string(), strikes::Signal::Reset)
            .map_err(ReportFailure::from)?;
        if counted {
            record.check = quiet_check(run);
            super::fires::note_quiet(&key, capped_record(&scoped_to(&root, &record)))
                .map_err(ReportFailure::from)?;
        }
        drain_waiting(task, &root);
        return Ok(Collected::Forgotten);
    }
    try_append_for_root(&root, &record).map_err(ReportFailure::from)?;
    end_quiet_streak(&key, &record);
    let transition = note_strike(loaded, &record).map_err(ReportFailure::from)?;
    if let RunTransition::AutoDisabled { strikes } = transition
        && let Some(notify) = LOOP_DISABLED.get()
    {
        notify(task, &root, strikes);
    }
    if acted
        && !run.manual
        && loaded.is_ephemeral()
        && loaded.entry().deadline.is_some()
        && let Err(error) = catalog.consume_scheduled(task)
    {
        tracing::warn!(task, %error, "acted --until row not consumed");
    }
    drain_waiting(task, &root);
    Ok(Collected::Reported(transition))
}

/// What a quiet run's newest command step or check that printed anything printed, for its
/// task's quiet summary: the exit code, whether it timed out, how long it took, and the tail of
/// its log. A structured script check that answered exited 0, and printed its stderr log, or,
/// when that is empty, its answer. A run whose every log is empty reports its newest.
fn quiet_check(run: &crate::store::workflow::RunRecord) -> Option<CheckRecord> {
    use crate::store::workflow::{AttemptOutcome, EvaluationOrigin, JournalEvent};
    let state = crate::StatePaths::for_project_root(&run.project_root).ok()?;
    let paths = state.workflow(&run.id);
    let journal = crate::store::workflow::journal(&paths).ok()?;
    let read =
        |log: &std::path::Path| std::fs::read_to_string(paths.relocate(log)).unwrap_or_default();
    let mut printed = journal
        .iter()
        .rev()
        .filter_map(|entry| match &entry.event {
            JournalEvent::Attempt {
                outcome,
                code,
                log,
                started_at,
                ended_at,
                ..
            } if log.is_some() || code.is_some() => Some((
                *code,
                *outcome == AttemptOutcome::Timeout,
                log.as_deref().map(read).unwrap_or_default(),
                *started_at,
                *ended_at,
            )),
            JournalEvent::Evaluation {
                outcome,
                object,
                started_at,
                ended_at,
                ..
            } if object.get("log").is_some() || object.get("code").is_some() => Some((
                object
                    .get("code")
                    .and_then(Value::as_i64)
                    .and_then(|code| i32::try_from(code).ok()),
                outcome == "timeout",
                object
                    .get("log")
                    .and_then(Value::as_str)
                    .map(|log| read(log.as_ref()))
                    .unwrap_or_default(),
                *started_at,
                *ended_at,
            )),
            JournalEvent::Evaluation {
                origin: EvaluationOrigin::Check,
                object,
                started_at,
                ended_at,
                ..
            } => {
                let log = paths.check_log(entry.seq?);
                if !log.exists() {
                    return None;
                }
                let stderr = read(&log);
                let output = if stderr.trim().is_empty() {
                    object.to_string()
                } else {
                    stderr
                };
                Some((Some(0), false, output, *started_at, *ended_at))
            }
            _ => None,
        })
        .peekable();
    let newest = printed.peek().cloned()?;
    let (code, timed_out, output, started, ended) = printed
        .find(|(_, _, output, _, _)| !output.trim().is_empty())
        .unwrap_or(newest);
    Some(CheckRecord {
        code,
        timed_out,
        output: tail_string(&output, CHECK_OUTPUT_CAP),
        output_path: None,
        duration_ms: u64::try_from(ended.duration_since(started).as_millis()).ok(),
        agent: None,
    })
}

/// A run that ended without acting, or failed, leaves its checkout free to launch again: the
/// launch memory keeps a checkout for a run whose act came to an end of its own, and for a
/// canceled run, which only a manual fire replaces.
fn forget_unkept_launch(
    state: &crate::StatePaths,
    ended: crate::store::workflow::RunState,
    task: &str,
    run: &crate::ids::WorkflowRunId,
    acted: bool,
) -> anyhow::Result<()> {
    use crate::store::workflow::RunState;
    let kept = match ended {
        RunState::Canceled => true,
        RunState::Failed => false,
        _ => acted,
    };
    if kept {
        return Ok(());
    }
    Ok(super::launch_ledger_store::forget_runs(
        state,
        task,
        std::slice::from_ref(run),
    )?)
}

/// A failure row ends the task's quiet streak; a failed reset leaves the streak to the next one.
fn end_quiet_streak(key: &str, record: &LoopRunRecord) {
    if record.polarity() == Some(RunPolarity::Failure)
        && let Err(error) = super::fires::reset_streak(key)
    {
        tracing::warn!(task = record.task, %error, "loop quiet streak not reset");
    }
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

/// How a fire's process ended: its first check's exit, else a spawned run's mapped code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessExit {
    Code(i32),
    Timeout,
    Signal,
}

impl ProcessExit {
    pub fn of(record: &LoopRunRecord) -> Option<Self> {
        if let Some(RunStage::Check(check)) = record.stages().next() {
            return Some(if check.timed_out {
                Self::Timeout
            } else {
                check.code.map_or(Self::Signal, Self::Code)
            });
        }
        record
            .run_id
            .as_ref()
            .and_then(|_| record.result.spawn_exit_code())
            .map(Self::Code)
    }
}

impl LoopRunRecord {
    /// The fire's exit in words: a watch's outcome, else its process exit.
    pub fn exit_label(&self) -> Option<String> {
        if let Some(verdict) = &self.watch {
            return Some(verdict.label());
        }
        ProcessExit::of(self).map(|exit| match exit {
            ProcessExit::Code(code) => code.to_string(),
            ProcessExit::Timeout => "timeout".to_owned(),
            ProcessExit::Signal => "signal".to_owned(),
        })
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
    /// When each record of a failure streak ran, in log order; empty for a good streak. A quiet
    /// fire among them ends the streak, and the log holds no row for it.
    pub failed_at: Vec<Timestamp>,
}

impl ActingRun {
    fn after(previous: Option<Self>, record: &LoopRunRecord) -> Option<Self> {
        let Some(polarity) = record.polarity() else {
            return previous;
        };
        let (streak, since, mut failed_at) = match previous {
            Some(previous) if previous.polarity == polarity => {
                (previous.streak + 1, previous.since, previous.failed_at)
            }
            Some(previous) => (1, Some(previous.record.at), Vec::new()),
            None => (1, None, Vec::new()),
        };
        if polarity == RunPolarity::Failure {
            failed_at.push(record.at);
        }
        Some(Self {
            record: record.clone(),
            polarity,
            streak,
            since,
            failed_at,
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

/// The newest acting run once a task's quiet fires join its rows: quiet fires are good runs, so
/// their streak extends a good streak and outranks an older failure. A failure row newer than the
/// newest quiet fire ends the quiet streak whatever the summary stored, and a row wins a tie.
pub fn with_quiet(acting: Option<ActingRun>, quiet: Option<&QuietFires>) -> Option<ActingRun> {
    let Some(quiet) = quiet else {
        return acting;
    };
    let streak = usize::try_from(quiet.streak_beside(acting.as_ref())).unwrap_or(usize::MAX);
    match acting {
        Some(mut acting) if acting.polarity == RunPolarity::Good => {
            acting.streak = acting.streak.saturating_add(streak);
            if quiet.last.at > acting.record.at {
                acting.record = quiet.last.clone();
            }
            Some(acting)
        }
        // A failure newer than the last quiet fire streaks only from it.
        Some(mut acting) if quiet.last.at <= acting.record.at => {
            let newer = acting
                .failed_at
                .iter()
                .filter(|at| **at >= quiet.last.at)
                .count();
            if newer < acting.streak {
                acting.streak = newer;
                acting.since = Some(quiet.last.at);
            }
            Some(acting)
        }
        Some(acting) if streak == 0 => Some(acting),
        Some(acting) => Some(ActingRun {
            record: quiet.last.clone(),
            polarity: RunPolarity::Good,
            streak,
            since: Some(acting.record.at),
            failed_at: Vec::new(),
        }),
        None => (streak > 0).then(|| ActingRun {
            record: quiet.last.clone(),
            polarity: RunPolarity::Good,
            streak,
            since: None,
            failed_at: Vec::new(),
        }),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoopRunStats {
    pub runs: usize,
    pub last: Option<LoopRunRecord>,
    pub last_kind: Option<(Timestamp, super::TaskKind)>,
    pub spend_today_usd: f64,
    pub acting: Option<ActingRun>,
    pub heard: Option<HeardSignal>,
    /// The task's quiet fires, which `runs`, `last` and `acting` already count.
    pub quiet: Option<QuietFires>,
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

/// The per-task summaries folded from the log, and the rows' reported runs by root, task and id.
#[derive(Clone, Default)]
struct FoldState {
    stats: BTreeMap<String, LoopRunStats>,
    reported: Reported,
}

/// The runs a report row named, by root, task and run id.
type Reported = std::collections::BTreeSet<(Option<PathBuf>, String, String)>;

/// Whether a logged row is history: a workflow run's logged launch is not, since the run's report
/// is its fire's row, and nor is a report of a run whose first report was already read.
fn is_history(record: &LoopRunRecord, reported: &mut Reported) -> bool {
    if record.result == LoopRunResult::Launched && record.run_id.is_some() {
        return false;
    }
    match record.run_id.as_ref().filter(|_| record.is_report()) {
        Some(run) => reported.insert((record.root.clone(), record.task.clone(), run.clone())),
        None => true,
    }
}

/// The log fold is cached; the quiet summaries and the live directory are read on every refresh,
/// since a quiet fire or a run in flight appends nothing to the log.
struct StatsFold {
    reader: crate::disk::rotating::IncrementalFold<FoldState>,
    merged: BTreeMap<String, LoopRunStats>,
    date: jiff::civil::Date,
    zone: jiff::tz::TimeZone,
}

impl StatsFold {
    fn new(state_root: &Path, now: &Zoned) -> Self {
        Self {
            reader: crate::disk::rotating::IncrementalFold::new(log_path(state_root)),
            merged: BTreeMap::new(),
            date: now.date(),
            zone: now.time_zone().clone(),
        }
    }

    fn refresh(&mut self, root: Option<&Path>, now: &Zoned) -> &BTreeMap<String, LoopRunStats> {
        let folded = self.reader.refresh(|state, record: LoopRunRecord| {
            if matches_root(&record, root) && is_history(&record, &mut state.reported) {
                fold_record(record, now, state);
            }
        });
        let mut merged = folded.clone();
        let quiet = quiet_fires(root);
        if let Some(root) = root {
            for row in in_flight(root, None, &merged.reported, &quiet) {
                fold_record(row, now, &mut merged);
            }
        }
        for (task, quiet) in quiet {
            let entry = merged.stats.entry(task).or_insert_with(LoopRunStats::empty);
            entry.runs = entry
                .runs
                .saturating_add(usize::try_from(quiet.count).unwrap_or(usize::MAX));
            entry.last_kind = last_kind_after(entry.last_kind, &quiet.last);
            if entry
                .last
                .as_ref()
                .is_none_or(|last| quiet.last.at > last.at)
            {
                entry.last = Some(quiet.last.clone());
            }
            entry.acting = with_quiet(entry.acting.take(), Some(&quiet));
            entry.quiet = Some(quiet);
        }
        self.merged = merged.stats;
        &self.merged
    }
}

impl LoopRunStats {
    const fn empty() -> Self {
        Self {
            runs: 0,
            last: None,
            last_kind: None,
            spend_today_usd: 0.0,
            acting: None,
            heard: None,
            quiet: None,
        }
    }
}

/// The quiet summary of each task with one in `root`, by task name: a room task's under the
/// room's key, a machine task's when its last quiet fire ran in `root`, the room task's winning a
/// name both hold. With no root, every summary, by the task its last fire names.
fn quiet_fires(root: Option<&Path>) -> BTreeMap<String, QuietFires> {
    use super::arming::TaskKey;
    use super::catalog::TaskSource;
    let fires = super::fires::load();
    let mut quiet = BTreeMap::new();
    let Some(root) = root else {
        for summary in fires.into_values().filter_map(|fires| fires.quiet) {
            quiet.insert(summary.last.task.clone(), summary);
        }
        return quiet;
    };
    let machine = TaskKey::for_task("", TaskSource::Config, root);
    let room = TaskKey::for_task("", TaskSource::Instance, root);
    for (scope, own) in [(machine, false), (room, true)] {
        for (key, summary) in &fires {
            let (Some(name), Some(summary)) = (key.strip_prefix(&scope), &summary.quiet) else {
                continue;
            };
            if own || summary.last.root.as_deref() == Some(root) {
                quiet.insert(name.to_owned(), summary.clone());
            }
        }
    }
    quiet
}

/// One neutral `launched` row for each run of a task in `root`'s live directory that has not been
/// reported yet: from the facts its fire sealed, else from its record. A run its report row or its
/// task's quiet summary already names has none, nor has a run this RimZ does not run.
fn in_flight(
    root: &Path,
    task: Option<&str>,
    reported: &Reported,
    quiet: &BTreeMap<String, QuietFires>,
) -> Vec<LoopRunRecord> {
    let runs = crate::StatePaths::for_project_root(root)
        .ok()
        .filter(|state| state.root.exists())
        .and_then(|state| crate::store::workflow::live_runs(&state).ok())
        .unwrap_or_default();
    let mut rows: Vec<_> = runs
        .into_iter()
        .filter(|run| run.loop_report != crate::store::workflow::LoopReport::Sent)
        .filter_map(|run| {
            let name = run.task.clone()?;
            let id = run.id.to_string();
            let key = (Some(root.to_path_buf()), name.clone(), id.clone());
            let collected = quiet
                .get(&name)
                .is_some_and(|quiet| quiet.last.run_id.as_deref() == Some(id.as_str()));
            (task.is_none_or(|task| task == name) && !reported.contains(&key) && !collected)
                .then(|| launch_row(&run, name, root))
        })
        .collect();
    rows.sort_by_key(|row| row.at);
    rows
}

fn launch_row(run: &crate::store::workflow::RunRecord, task: String, root: &Path) -> LoopRunRecord {
    let mode = if run.manual {
        LoopRunMode::Manual
    } else {
        LoopRunMode::Scheduled
    };
    let mut row = run
        .fire_facts
        .as_ref()
        .and_then(|facts| LoopRunRecord::deserialize(facts).ok())
        .unwrap_or_else(|| {
            let mut row = LoopRunRecord::new(&task, LoopRunResult::Launched, mode, 0);
            row.checkout = Some(run.firing_checkout().to_path_buf());
            row
        });
    row.task = task;
    row.result = LoopRunResult::Launched;
    row.mode = Some(mode);
    row.at = run.created_at;
    row.root = Some(root.to_path_buf());
    row.run_id = Some(run.id.to_string());
    row.acted = None;
    row
}

/// A task's logged history rows in log order, as `is_history` keeps them.
pub fn task_records(state_root: &Path, task: &str, root: Option<&Path>) -> Vec<LoopRunRecord> {
    logged_history(state_root, task, root).0
}

/// A task's logged history rows in log order, with the runs the log reports.
fn logged_history(
    state_root: &Path,
    task: &str,
    root: Option<&Path>,
) -> (Vec<LoopRunRecord>, Reported) {
    let mut records = Vec::new();
    let mut reported = Reported::new();
    crate::disk::rotating::visit_records(&log_path(state_root), |record: LoopRunRecord| {
        if record.task == task && matches_root(&record, root) && is_history(&record, &mut reported)
        {
            records.push(record);
        }
    });
    (records, reported)
}

#[derive(Clone, Debug, Default)]
pub struct TaskHistory {
    pub fires: Vec<LoopRunRecord>,
    pub heard: Option<HeardSummary>,
    pub records: Vec<LoopRunRecord>,
    /// The task's quiet fires, which have no row in `fires` or `records`.
    pub quiet: Option<QuietFires>,
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
            quiet: None,
        }
    }

    /// The newest acting run, the task's quiet fires included.
    pub fn acting(&self) -> Option<ActingRun> {
        with_quiet(acting_run(&self.fires), self.quiet.as_ref())
    }
}

/// A task's history: its logged rows, a `launched` row for each of its runs in flight in the
/// room's live directory, and its quiet summary. A rowless root reads the log alone.
pub fn task_history(
    state_root: &Path,
    task: &str,
    root: Option<&Path>,
    armed_at: Option<Timestamp>,
) -> TaskHistory {
    let (mut records, reported) = logged_history(state_root, task, root);
    let mut quiet = quiet_fires(root);
    if let Some(root) = root {
        records = merge_by_at(records, in_flight(root, Some(task), &reported, &quiet));
    }
    let mut history = TaskHistory::split(records, armed_at);
    history.quiet = quiet.remove(task);
    history
}

/// Logged rows, in log order, with each in-flight row, oldest first, before the first logged row
/// newer than it: a logged row wins a tie.
fn merge_by_at(logged: Vec<LoopRunRecord>, in_flight: Vec<LoopRunRecord>) -> Vec<LoopRunRecord> {
    let mut merged = Vec::with_capacity(logged.len() + in_flight.len());
    let mut in_flight = in_flight.into_iter().peekable();
    for row in logged {
        while let Some(older) = in_flight.next_if(|next| next.at < row.at) {
            merged.push(older);
        }
        merged.push(row);
    }
    merged.extend(in_flight);
    merged
}

/// Whether a scheduled fire of the task, keyed `key`, left proof since `since`: a scheduled row,
/// or a scheduled workflow launch, which logs no row and stamps the task's fire record instead.
pub(super) fn has_scheduled_row_since(
    state_root: &Path,
    key: &str,
    task: &str,
    root: &Path,
    checkout: Option<&Path>,
    since: Timestamp,
) -> bool {
    super::fires::scheduled_since(key, checkout, since)
        || has_scheduled_row_where(state_root, task, root, checkout, since, |_| true)
}

/// Whether a runner recorded a scheduled attempt since `since`: a scheduled launch, or any
/// scheduled row but `start failed`, which the pass writes for a runner that never ran.
pub(super) fn has_recorded_attempt_since(
    state_root: &Path,
    key: &str,
    task: &str,
    root: &Path,
    since: Timestamp,
) -> bool {
    super::fires::scheduled_since(key, None, since)
        || has_scheduled_row_where(state_root, task, root, None, since, |result| {
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
    let entry = state
        .stats
        .entry(record.task.clone())
        .or_insert_with(LoopRunStats::empty);
    entry.last_kind = last_kind_after(entry.last_kind, &record);
    if record.result == LoopRunResult::SignalSkipped {
        if let Some(heard) = HeardSignal::of(&record) {
            entry.heard = Some(heard);
        }
        return;
    }
    entry.runs += 1;
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
