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
use crate::ids::{FireId, MessageId, TaskId};
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
    #[error("loop task record not read: {0}")]
    Read(#[from] super::task_record::TaskRecordErr),
    #[error("loop task record update failed: {0}")]
    Record(#[from] super::task_record_store::TaskStoreErr),
    #[error("the run's room not read: {0:#}")]
    Room(anyhow::Error),
}

/// Attempt the history append before updating the task's strike count and arming. Both are
/// best-effort: a row with no fire has no retry to hand a failure to. A workflow run's report
/// takes `report_run`'s fallible path instead.
///
/// A workflow run's launch appends no row, since the run's report is its fire's one row, and the
/// runner noted the launch on the fire's mark.
pub(super) fn record_transition(task: &LoadedTask, record: &LoopRunRecord) -> RunTransition {
    if record.result == LoopRunResult::Launched && record.run_id.is_some() {
        return RunTransition::Recorded;
    }
    let binding = task.binding(&record.task);
    let mut record = record.clone();
    // The row's first state write binds a hand-written row, so its history has an id.
    if record.task_id.is_none() {
        record.task_id = super::task_record_store::mutate(|records| records.bind(&binding).0)
            .inspect_err(
                |err| tracing::warn!(task = record.task, error = %err, "loop task not bound"),
            )
            .ok();
    }
    let record = &record;
    append_for_root(&task.entry().resolved_root(), record);
    end_quiet_streak(&binding, record);
    note_strike(task, &binding, record).unwrap_or_else(|err| {
        tracing::warn!(task = record.task, error = %err, "loop strike state update failed");
        RunTransition::Recorded
    })
}

/// Note the record's strike signal, then disable a task that struck out; the disable is
/// best-effort, since the task's next strike attempts it again.
fn note_strike(
    task: &LoadedTask,
    binding: &super::task_record::Binding,
    record: &LoopRunRecord,
) -> Result<RunTransition, strikes::StrikesError> {
    let signal = strikes::classify(record);
    let count = strikes::note(binding, record.task_id.as_ref(), signal)?;
    let Some(max) = strikes::threshold(task.entry()) else {
        return Ok(RunTransition::Recorded);
    };
    if signal != strikes::Signal::Strike || count < max {
        return Ok(RunTransition::Recorded);
    }
    let transition = match arming::disable_if_live(
        binding,
        task.source(),
        Some(count),
        Timestamp::now(),
    ) {
        Ok(true) => RunTransition::AutoDisabled { strikes: count },
        Ok(false) => RunTransition::Recorded,
        Err(err) => {
            tracing::warn!(task = record.task, error = %err, "loop auto-disable state update failed");
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
    /// The task's id; views select a task's rows by it. A row an older RimZ wrote has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<TaskId>,
    /// The fire this row reports; a gate refusal or overlap that started no run has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire: Option<FireId>,
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

/// A task's quiet fires: fires whose command exited 0, which leave no home and no history row.
/// `streak` counts those since the task's last failure row; `last` is the newest one's
/// `completed` row, with its output.
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
            task_id: None,
            fire: None,
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

/// [`collect_run`], returning why a report did not land instead of warning. A finished run this
/// RimZ does not decode is reported from its header, without moving a strike.
pub fn try_collect_run(
    state: &crate::StatePaths,
    run: &crate::ids::WorkflowRunId,
) -> anyhow::Result<()> {
    let ended = collect_under_lock(state, run)?;
    // An ended run never runs again, so its fire's verdict cannot go stale while the lock is
    // released; the sweep takes each supervised run's own lock and skips one already ended.
    if ended {
        stop_fire_launches(state, run);
    }
    Ok(())
}

/// Report the run under the workspace lock, answering whether it has ended.
fn collect_under_lock(
    state: &crate::StatePaths,
    run: &crate::ids::WorkflowRunId,
) -> anyhow::Result<bool> {
    use crate::store::workflow;
    let paths = state.workflow(run);
    let guard = crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock)?;
    // A run that left the live directory owes nothing.
    let mut record = match workflow::classify(&paths)? {
        workflow::RunClass::Current(record) => record,
        workflow::RunClass::Ended(header) => {
            if header.owes_loop_report() {
                report_header(state, run, &header)?;
                workflow::mark_header_report_sent(&paths, &guard)?;
            }
            return Ok(true);
        }
        workflow::RunClass::Recordless => return Ok(true),
        _ => return Ok(false),
    };
    if !record.owes_loop_report() {
        return Ok(record.outcome.is_some());
    }
    let outcome = record
        .outcome
        .clone()
        .ok_or_else(|| anyhow::anyhow!("the run's record carries no outcome"))?;
    // The exit is detail on the report; an unreadable journal leaves it out.
    let command = workflow::journal(&paths)
        .ok()
        .as_deref()
        .and_then(command_exit)
        .map(|mut check| {
            // The report releases the home to the finished directory, where the row's log then lies.
            let finished = state.finished_workflow(run);
            check.output_path = check.output_path.map(|log| finished.relocate(&log));
            check
        });
    match report_run(&record, &outcome, command)? {
        Collected::Reported(_) => {
            record.loop_report = workflow::LoopReport::Sent;
            workflow::write(&paths, &record, &guard)?;
        }
        Collected::Forgotten => {
            std::fs::remove_file(&paths.record)?;
            std::fs::remove_dir_all(&paths.home)?;
        }
    }
    Ok(true)
}

/// Stop each open supervised run that the fire `run`'s command launched blocking and that its
/// ended command left behind: a stamped run outlives a command its fire's timeout or cancel
/// killed. Only runs stamped with this fire's id are stopped, never a successor fire's.
fn stop_fire_launches(state: &crate::StatePaths, run: &crate::ids::WorkflowRunId) {
    let launches = match crate::harness::run::list(state) {
        Ok(launches) => launches,
        Err(error) => {
            tracing::warn!(%run, error = %format!("{error:#}"), "fire's launches not listed");
            return;
        }
    };
    let owned = launches
        .into_iter()
        .filter(|launch| !launch.status.is_terminal() && launch.loop_fire.as_ref() == Some(run));
    let mut owned = owned.peekable();
    if owned.peek().is_none() {
        return;
    }
    let runtime = match crate::RuntimePaths::for_state(state) {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::warn!(%run, error = %format!("{error:#}"), "fire's launches not stopped");
            return;
        }
    };
    for launch in owned {
        if let Err(error) =
            crate::harness::run::cancel_and_wake_paths(state, &runtime, &launch.run_id, None)
        {
            let launch = &launch.run_id;
            tracing::warn!(%run, %launch, error = %format!("{error:#}"), "fire's launch not stopped");
        }
    }
}

/// Report a finished run this RimZ does not decode from its record's header: the report row,
/// without moving a strike, settling its fire's tree or consuming an `--until` row, since the
/// body that would say what the run did is unread.
fn report_header(
    state: &crate::StatePaths,
    run: &crate::ids::WorkflowRunId,
    header: &crate::store::workflow::RunHeader,
) -> Result<(), ReportError> {
    use crate::store::workflow::{RunEnding, RunOutcome, RunState};
    let (Some(task), Some(ended)) = (header.task.as_deref(), header.state) else {
        return Ok(());
    };
    let root = match &header.project_root {
        Some(root) => root.clone(),
        None => {
            crate::workspace::record::read(&state.workspace_record)
                .map_err(|error| ReportFailure::Room(error.into()))?
                .project_root
        }
    };
    let ending = header.ending.clone().unwrap_or(match ended {
        RunState::Done => RunEnding::Done,
        RunState::Canceled => RunEnding::Canceled,
        _ => RunEnding::Failed,
    });
    let outcome = RunOutcome {
        ending,
        cost_usd: None,
        acted: header.acted.unwrap_or(true),
        reason: header.reason.clone(),
        quiet: false,
    };
    let checkout = if header.fire_tree {
        Some(&root)
    } else {
        header.scope_path.as_ref()
    };
    let facts = ReportFacts {
        task,
        fire: header.loop_fire.as_ref(),
        run,
        manual: header.manual,
        created_at: header
            .created_at
            .or(header.terminal_at)
            .unwrap_or_else(Timestamp::now),
        terminal_at: header.terminal_at,
        checkout: checkout.unwrap_or(&root),
        fire_facts: header.fire_facts.as_ref(),
    };
    report_neutral(&facts, &outcome, &root)
}

/// Report a run without moving a strike or the quiet streak: append its row, close its fire's
/// mark, so the checkout it held is free, and drain the task's waiting triggers. A run with no
/// fire names no task id and closes no mark.
fn report_neutral(
    facts: &ReportFacts<'_>,
    outcome: &crate::store::workflow::RunOutcome,
    project_root: &Path,
) -> Result<(), ReportError> {
    let mut record = report_row(facts, outcome);
    let catalog = super::catalog::TaskCatalog::load_lenient(Some(project_root));
    let root = match catalog.for_run(facts.task) {
        Some(loaded) => {
            record.kind = Some(loaded.kind());
            loaded.entry().resolved_root()
        }
        None => project_root.to_path_buf(),
    };
    try_append_for_root(&root, &record).map_err(ReportFailure::from)?;
    if let Some(fire) = facts.fire {
        super::fires::settle(super::fires::Outcome {
            fire,
            result: record.result,
            signal: strikes::Signal::Neutral,
            quiet: None,
            ends_streak: false,
            strike_out: None,
            now: Timestamp::now(),
        })
        .map_err(ReportFailure::from)?;
    }
    drain_waiting(facts.task, facts.fire, &root);
    Ok(())
}

/// Collect every finished run of the task named `task` in `root` that still owes its report, and
/// say whether one was owed, so a fire decides on the strikes, rows and marks those runs leave.
pub fn collect_task_runs(root: &Path, task: &str) -> bool {
    let owed = (|| -> anyhow::Result<_> {
        let state = crate::StatePaths::for_project_root(root)?;
        if !state.root.exists() {
            return Ok(None);
        }
        let ids = super::task_record::load()?.ids_named(task);
        let runs: Vec<_> = crate::store::workflow::live_runs(&state)?
            .into_iter()
            .filter(|record| ids.iter().any(|id| record.ran_for(id)) && record.owes_loop_report())
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
    /// The fire the run reports, which names its task; a run an older RimZ started has none.
    pub(in crate::harness) fire: Option<&'a FireId>,
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
            fire: run.loop_fire.as_ref(),
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
    record.task_id = facts.fire.map(FireId::task);
    record.fire = facts.fire.cloned();
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
/// settles that tree first, so whichever collection lands first settles it.
///
/// The fire the run carries names the task: its record gives the task's current name, so a
/// renamed task still gets its run's report. One write of that record settles the fire: it
/// closes the fire's mark with the run's result (only a run whose command exited 0 keeps holding
/// its checkout for `once-per-checkout`), moves the strike count once per fire id, adds a quiet
/// fire to the task's quiet summary or ends the quiet streak on a failure, and disables a task
/// that struck out. A quiet run whose task row exists writes no row; any other appends one
/// first, so a failed append returns before the settle and a retry does both, and history readers
/// fold a repeated row by its run id. When a scheduled fire's command exited 0, an `--until` row
/// is consumed, quiet or not. Either way a waiting trigger is drained once the report landed.
/// `command` is how the run's command ended, which a row carries as its check.
pub(in crate::harness) fn report_run(
    run: &crate::store::workflow::RunRecord,
    outcome: &crate::store::workflow::RunOutcome,
    command: Option<CheckRecord>,
) -> Result<Collected, ReportError> {
    let Some(task) = run.task.as_deref() else {
        return Ok(Collected::Reported(RunTransition::Recorded));
    };
    if !run.state.is_terminal() {
        return Ok(Collected::Reported(RunTransition::Recorded));
    }
    let Some(fire) = run.loop_fire.as_ref().filter(|_| !run.stranded) else {
        report_neutral(&ReportFacts::of(run, task), outcome, &run.project_root)?;
        return Ok(Collected::Reported(RunTransition::Recorded));
    };
    super::runner::settle_fire_tree(run);
    let records = super::task_record::load().map_err(ReportFailure::from)?;
    let bound = records.get(&fire.task());
    // The task's current name: a rename since the fire moved the binding, not the run.
    let name = bound.map_or(task, |record| record.binding.name.as_str());
    let mut record = report_row(&ReportFacts::of(run, name), outcome);
    record.check = command;
    let catalog = super::catalog::TaskCatalog::load_lenient(Some(&run.project_root));
    let loaded = catalog.for_run(name).filter(|loaded| {
        bound.is_some_and(|bound| bound.is_live() && loaded.binding(name) == bound.binding)
    });
    let root = loaded.map_or_else(
        || run.project_root.clone(),
        |loaded| loaded.entry().resolved_root(),
    );
    if let Some(loaded) = loaded {
        record.kind = Some(loaded.kind());
    }
    // A consumed one-shot's row is gone, so its quiet run still writes its row.
    let quiet = outcome.quiet && loaded.is_some();
    let signal = if quiet {
        record.check = quiet_check(run);
        strikes::Signal::Reset
    } else {
        // The facts a consumed one-shot's fire sealed still name the kind its row had.
        try_append_for_root(&root, &record).map_err(ReportFailure::from)?;
        strikes::classify(&record)
    };
    let settled = super::fires::settle(super::fires::Outcome {
        fire,
        result: record.result,
        signal,
        quiet: quiet.then(|| capped_record(&scoped_to(&root, &record))),
        ends_streak: record.polarity() == Some(RunPolarity::Failure),
        strike_out: loaded.and_then(|loaded| {
            strikes::threshold(loaded.entry()).map(|max| (max, loaded.source()))
        }),
        now: Timestamp::now(),
    })
    .map_err(ReportFailure::from)?;
    let transition = match settled {
        Some(settled) if settled.disabled => RunTransition::AutoDisabled {
            strikes: settled.count,
        },
        _ => RunTransition::Recorded,
    };
    if let RunTransition::AutoDisabled { strikes } = transition
        && let Some(notify) = LOOP_DISABLED.get()
    {
        notify(name, &root, strikes);
    }
    consume_until(&catalog, run, &record);
    drain_waiting(name, Some(fire), &root);
    Ok(if quiet {
        Collected::Forgotten
    } else {
        Collected::Reported(transition)
    })
}

/// Consume a scheduled fire's `--until` row once its command exited 0.
fn consume_until(
    catalog: &super::catalog::TaskCatalog,
    run: &crate::store::workflow::RunRecord,
    record: &LoopRunRecord,
) {
    let Some(loaded) = catalog.for_run(&record.task) else {
        return;
    };
    if record.result == LoopRunResult::Completed
        && !run.manual
        && loaded.is_ephemeral()
        && loaded.entry().deadline.is_some()
        && let Err(error) = catalog.consume_scheduled(&record.task)
    {
        tracing::warn!(task = record.task, %error, "--until row not consumed after its command exited 0");
    }
}

/// How the run's command ended, from the newest evaluation of its command check in the
/// run's journal: the exit code its test recorded, or the engine's timeout. The run's
/// outcome alone says only done or failed.
fn command_exit(journal: &[crate::store::workflow::JournalEntry]) -> Option<CheckRecord> {
    use crate::store::workflow::JournalEvent;
    journal.iter().rev().find_map(|entry| {
        let JournalEvent::Evaluation {
            outcome,
            object,
            started_at,
            ended_at,
            ..
        } = &entry.event
        else {
            return None;
        };
        let timed_out = outcome == "timeout";
        if entry.node != super::runner::COMMAND_NODE || !(timed_out || object.get("code").is_some())
        {
            return None;
        }
        let output_path = object.get("log").and_then(Value::as_str).map(PathBuf::from);
        Some(CheckRecord {
            code: object
                .get("code")
                .and_then(Value::as_i64)
                .and_then(|code| i32::try_from(code).ok()),
            timed_out,
            output: output_path.as_deref().map(log_tail).unwrap_or_default(),
            output_path,
            duration_ms: u64::try_from(ended_at.duration_since(*started_at).as_millis()).ok(),
            agent: None,
        })
    })
}

/// The last `CHECK_OUTPUT_CAP` bytes of a command's log, which the row keeps as its output;
/// an unreadable log keeps none.
fn log_tail(path: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path).and_then(|mut file| {
        let len = file.metadata()?.len();
        file.seek(SeekFrom::Start(len.saturating_sub(CHECK_OUTPUT_CAP as u64)))?;
        file.read_to_end(&mut bytes)
    });
    if read.is_err() {
        return String::new();
    }
    tail_string(&String::from_utf8_lossy(&bytes), CHECK_OUTPUT_CAP)
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
    let read = |log: &std::path::Path| std::fs::read_to_string(log).unwrap_or_default();
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

/// A failure row ends the task's quiet streak; a failed reset leaves the streak to the next one.
fn end_quiet_streak(binding: &super::task_record::Binding, record: &LoopRunRecord) {
    if record.polarity() == Some(RunPolarity::Failure)
        && let Err(error) = super::fires::reset_streak(binding)
    {
        tracing::warn!(task = record.task, %error, "loop quiet streak not reset");
    }
}

/// Start the next waiting trigger now that the run holding the task is over.
fn drain_waiting(task: &str, fire: Option<&FireId>, root: &Path) {
    let result = (|| -> anyhow::Result<()> {
        let paths = crate::StatePaths::for_project_root(root)?;
        let backlog = super::backlog::load(&paths)?;
        let id = fire.map(|fire| fire.task().to_string());
        let waiting = id.as_deref().into_iter().chain([task]).any(|key| {
            backlog
                .tasks
                .get(key)
                .is_some_and(|task| !task.triggers.is_empty())
        });
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

/// Whether an acting run's outcome reads as good or as a failure.
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
    stats: BTreeMap<TaskId, LoopRunStats>,
    reported: Reported,
}

/// The runs a report row named, by root and run id.
type Reported = std::collections::BTreeSet<(Option<PathBuf>, String)>;

/// Whether a logged row is history: a workflow run's logged launch is not, since the run's report
/// is its fire's row, and nor is a report of a run whose first report was already read.
fn is_history(record: &LoopRunRecord, reported: &mut Reported) -> bool {
    if record.result == LoopRunResult::Launched && record.run_id.is_some() {
        return false;
    }
    match record.run_id.as_ref().filter(|_| record.is_report()) {
        Some(run) => reported.insert((record.root.clone(), run.clone())),
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
        // The record names each task's rows; an unreadable one shows none.
        let records = super::task_record::load().unwrap_or_default();
        let shown = shown_tasks(&records, root);
        if let Some(root) = root {
            for row in in_flight(root, &records, &shown, &merged.reported) {
                fold_record(row, now, &mut merged);
            }
        }
        let mut stats = BTreeMap::new();
        for (id, name) in &shown {
            let mut entry = merged.stats.remove(id).unwrap_or_else(LoopRunStats::empty);
            let quiet = records.get(id).and_then(|record| record.quiet.clone());
            if let Some(quiet) = quiet.filter(|quiet| matches_root(&quiet.last, root)) {
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
            if entry.runs > 0 || entry.heard.is_some() || entry.last.is_some() {
                stats.insert(name.clone(), entry);
            }
        }
        self.merged = stats;
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

/// The task each name in a view of `root` shows: of the records bound in the scopes `root` sees
/// (every scope with no root), a live record before a retired one, a room's before the
/// machine's, and among retired ones the newest, which is a consumed one-off's.
fn shown_tasks(
    records: &super::task_record::TaskRecords,
    root: Option<&Path>,
) -> BTreeMap<TaskId, String> {
    use super::task_record::Scope;
    let known = root.map(|root| Scope::known(Some(root)));
    let mut best: BTreeMap<&str, (_, &TaskId)> = BTreeMap::new();
    for (id, record) in &records.tasks {
        let scope = &record.binding.scope;
        if known.as_ref().is_some_and(|known| !known.contains(scope)) {
            continue;
        }
        let rank = (
            record.is_live(),
            matches!(scope, Scope::Workspace(_)),
            record.retired_at,
        );
        let name = record.binding.name.as_str();
        if best.get(name).is_none_or(|(held, _)| rank > *held) {
            best.insert(name, (rank, id));
        }
    }
    best.into_iter()
        .map(|(name, (_, id))| (id.clone(), name.to_owned()))
        .collect()
}

/// One neutral `launched` row for each run of a shown task in `root`'s live directory that has
/// not been reported yet, under the task's current name: from the facts its fire sealed, else
/// from its record. A run its report row already names has none, nor has a quiet run its task's
/// summary already took, nor a run this RimZ does not run.
fn in_flight(
    root: &Path,
    records: &super::task_record::TaskRecords,
    shown: &BTreeMap<TaskId, String>,
    reported: &Reported,
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
            let fire = run.loop_fire.clone().filter(|_| run.task.is_some())?;
            let task = fire.task();
            let name = shown.get(&task)?.clone();
            let id = run.id.to_string();
            let collected = records
                .get(&task)
                .and_then(|record| record.quiet.as_ref())
                .is_some_and(|quiet| quiet.last.run_id.as_deref() == Some(id.as_str()));
            (!collected && !reported.contains(&(Some(root.to_path_buf()), id)))
                .then(|| launch_row(&run, &fire, name, root))
        })
        .collect();
    rows.sort_by_key(|row| row.at);
    rows
}

fn launch_row(
    run: &crate::store::workflow::RunRecord,
    fire: &FireId,
    task: String,
    root: &Path,
) -> LoopRunRecord {
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
    row.task_id = Some(fire.task());
    row.fire = Some(fire.clone());
    row.result = LoopRunResult::Launched;
    row.mode = Some(mode);
    row.at = run.created_at;
    row.root = Some(root.to_path_buf());
    row.run_id = Some(run.id.to_string());
    row.acted = None;
    row
}

/// A task's logged history rows in log order, as `is_history` keeps them. A row with no task
/// id, which an older RimZ wrote, belongs to no task.
pub fn task_records(state_root: &Path, task: &TaskId, root: Option<&Path>) -> Vec<LoopRunRecord> {
    logged_history(state_root, task, root).0
}

/// A task's logged history rows in log order, with the runs the log reports.
fn logged_history(
    state_root: &Path,
    task: &TaskId,
    root: Option<&Path>,
) -> (Vec<LoopRunRecord>, Reported) {
    let mut records = Vec::new();
    let mut reported = Reported::new();
    crate::disk::rotating::visit_records(&log_path(state_root), |record: LoopRunRecord| {
        if record.task_id.as_ref() == Some(task)
            && matches_root(&record, root)
            && is_history(&record, &mut reported)
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
/// room's live directory, and its quiet summary. A rowless root reads the log alone; a task with
/// no record (`None`) has none.
pub fn task_history(
    state_root: &Path,
    task: Option<&TaskId>,
    root: Option<&Path>,
    armed_at: Option<Timestamp>,
) -> TaskHistory {
    let Some(task) = task else {
        return TaskHistory::default();
    };
    let (mut records, reported) = logged_history(state_root, task, root);
    let task_records = super::task_record::load().unwrap_or_default();
    let bound = task_records.get(task);
    if let (Some(root), Some(bound)) = (root, bound) {
        let shown = BTreeMap::from([(task.clone(), bound.binding.name.clone())]);
        records = merge_by_at(records, in_flight(root, &task_records, &shown, &reported));
    }
    let mut history = TaskHistory::split(records, armed_at);
    history.quiet = bound
        .and_then(|bound| bound.quiet.clone())
        .filter(|quiet| matches_root(&quiet.last, root));
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
/// or a scheduled workflow launch, which logs no row and stamps the task's record instead.
pub(super) fn has_scheduled_row_since(
    state_root: &Path,
    key: &str,
    root: &Path,
    checkout: Option<&Path>,
    since: Timestamp,
) -> bool {
    super::fires::scheduled_since(key, checkout, since).unwrap_or(false)
        || has_scheduled_row_where(state_root, key, root, checkout, since, |_| true)
}

/// Whether a runner recorded a scheduled attempt since `since`: a scheduled launch, or any
/// scheduled row but `start failed`, which the pass writes for a runner that never ran.
pub(super) fn has_recorded_attempt_since(
    state_root: &Path,
    key: &str,
    root: &Path,
    since: Timestamp,
) -> bool {
    super::fires::scheduled_since(key, None, since).unwrap_or(false)
        || has_scheduled_row_where(state_root, key, root, None, since, |result| {
            result != LoopRunResult::StartFailed
        })
}

/// The rows of the task bound to `key`, by its id, retired since the fire or not: a task that
/// was never bound has none.
fn has_scheduled_row_where(
    state_root: &Path,
    key: &str,
    root: &Path,
    checkout: Option<&Path>,
    since: Timestamp,
    counts: impl Fn(LoopRunResult) -> bool,
) -> bool {
    let Some(task) = super::task_record::load()
        .ok()
        .and_then(|records| records.named(key).map(|(id, _)| id.clone()))
    else {
        return false;
    };
    let mut found = false;
    crate::disk::rotating::visit_records(&log_path(state_root), |record: LoopRunRecord| {
        if record.task_id.as_ref() == Some(&task)
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
    let Some(task) = record.task_id.clone() else {
        return;
    };
    let entry = state.stats.entry(task).or_insert_with(LoopRunStats::empty);
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
