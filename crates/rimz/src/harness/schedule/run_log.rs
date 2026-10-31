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
use crate::harness::schedule::signal::WatchVerdict;
use crate::harness::schedule::strikes;
use crate::ids::MessageId;
use crate::store::event::SignalName;
use crate::store::run::RunStatus;
use serde_json::{Map, Value};

pub type ConditionRecord = super::when::ConditionEvidence;

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

/// Attempt the history append before updating strike and arming overlays.
/// Both the append and overlay updates are best-effort.
pub(super) fn record_transition(task: &LoadedTask, record: &LoopRunRecord) -> RunTransition {
    append_for_root(&task.entry().resolved_root(), record);
    let name = &record.task;
    let key = task.key(name);
    let signal = strikes::classify(record);
    let count = match strikes::note(&key, signal) {
        Ok(count) => count,
        Err(err) => {
            tracing::warn!(task = name, error = %err, "loop strike state update failed");
            return RunTransition::Recorded;
        }
    };
    let Some(max) = strikes::threshold(task.entry()) else {
        return RunTransition::Recorded;
    };
    if signal != strikes::Signal::Strike || count < max {
        return RunTransition::Recorded;
    }
    match arming::disable_if_live(&key, task.source(), Some(count), Timestamp::now()) {
        Ok(true) => RunTransition::AutoDisabled { strikes: count },
        Ok(false) => RunTransition::Recorded,
        Err(err) => {
            tracing::warn!(task = name, error = %err, "loop auto-disable state update failed");
            RunTransition::Recorded
        }
    }
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
    let mut scoped_record = record.clone();
    scoped_record.root = Some(root.to_path_buf());
    append_to(&logs_dir(), &scoped_record);
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopRunRecord {
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<super::TaskKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<PathBuf>,
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
}

impl LoopRunRecord {
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
            root: None,
            at: Timestamp::now(),
            result,
            absorbed: 0,
            mode: Some(mode),
            duration_ms: Some(duration_ms),
            throttle_wait_ms: None,
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
            transcript_path: None,
            last_message: None,
            target: None,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
        }
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

struct StatsFold {
    reader: crate::disk::rotating::IncrementalFold<BTreeMap<String, LoopRunStats>>,
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
        self.reader.refresh(|stats, record: LoopRunRecord| {
            if matches_root(&record, root) {
                fold_record(record, now, stats);
            }
        })
    }
}

pub fn task_records(state_root: &Path, task: &str, root: Option<&Path>) -> Vec<LoopRunRecord> {
    let mut records = Vec::new();
    crate::disk::rotating::visit_records(&log_path(state_root), |record: LoopRunRecord| {
        if record.task == task && matches_root(&record, root) {
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
    let mut found = false;
    crate::disk::rotating::visit_records(&log_path(state_root), |record: LoopRunRecord| {
        if record.task == task
            && matches_root(&record, Some(root))
            && checkout.is_none_or(|checkout| record.checkout.as_deref() == Some(checkout))
            && record.mode == Some(LoopRunMode::Scheduled)
            && record.at >= since
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
}

impl DailyBudgetGate {
    pub(super) fn reason(self) -> String {
        if self.reserved_usd > 0.0 {
            format!(
                "daily budget ${:.2} cannot fund the next ${:.2} run (${:.2} spent)",
                self.cap_usd, self.reserved_usd, self.spend_usd
            )
        } else {
            format!(
                "daily budget ${:.2} exhausted (${:.2} spent)",
                self.cap_usd, self.spend_usd
            )
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
    let cap = raw_cap
        .parse::<crate::harness::budget::BudgetSpec>()
        .map_err(|err| format!("task `{task}` has invalid budget-per-day: {err}"))?
        .cap_usd;
    let raw_reserved = entry
        .budget
        .as_deref()
        .ok_or_else(|| format!("task `{task}` uses budget-per-day without a per-run budget"))?;
    let reserved = raw_reserved
        .parse::<crate::harness::budget::BudgetSpec>()
        .map_err(|err| format!("task `{task}` has invalid budget: {err}"))?
        .cap_usd;
    let spend = spend_on_local_day(
        &task_records(state_root, task, Some(&entry.resolved_root())),
        now,
    );
    Ok(
        ((spend >= cap) || (reserved > 0.0 && spend + reserved > cap)).then_some(DailyBudgetGate {
            spend_usd: spend,
            cap_usd: cap,
            reserved_usd: reserved,
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

fn fold_record(record: LoopRunRecord, now: &Zoned, stats: &mut BTreeMap<String, LoopRunStats>) {
    let entry = stats.entry(record.task.clone()).or_insert(LoopRunStats {
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
