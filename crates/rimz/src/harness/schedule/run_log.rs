//! User-global loop task run history.
//!
//! Loop config is per-machine, so task outcomes append to one user-global JSONL
//! log. Each loaded `rimz loop run`/`fire` appends one best-effort record with
//! the terminal result, mode, duration, and capped forensics. `rimz loop list`
//! folds the current and rotated files for summary columns; `rimz loop show`
//! reads the same records for per-task inspection.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};

use crate::config::TaskEntry;
use crate::harness::run::RunStatus;
use crate::harness::schedule::{pauses, strikes};
use crate::store::paths::state_home;

const NAME: &str = "loop-runs.log.jsonl";
const MAX_BYTES: u64 = 4 * 1_048_576;
const CHECK_OUTPUT_CAP: usize = 4 * 1024;
const ERROR_CAP: usize = 2 * 1024;
const LAST_MESSAGE_CAP: usize = 2 * 1024;
pub const COST_WINDOW: usize = 10;

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
    AutoPaused { strikes: u32 },
}

/// Append first, then update strike and pause overlays. Overlay failures stay
/// best-effort because the terminal history row is durable truth.
pub fn record_transition(name: &str, entry: &TaskEntry, record: &LoopRunRecord) -> RunTransition {
    append(record);
    let signal = strikes::classify(record);
    let count = match strikes::note(name, signal) {
        Ok(count) => count,
        Err(err) => {
            tracing::warn!(task = name, error = %err, "loop strike state update failed");
            return RunTransition::Recorded;
        }
    };
    let Some(max) = strikes::threshold(entry) else {
        return RunTransition::Recorded;
    };
    if signal != strikes::Signal::Strike || count < max {
        return RunTransition::Recorded;
    }
    match pauses::set_if_inactive(
        name,
        pauses::PauseEntry {
            until: None,
            strikes: Some(count),
        },
        Timestamp::now(),
    ) {
        Ok(true) => RunTransition::AutoPaused { strikes: count },
        Ok(false) => RunTransition::Recorded,
        Err(err) => {
            tracing::warn!(task = name, error = %err, "loop auto-pause state update failed");
            RunTransition::Recorded
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoopRunRecord {
    pub task: String,
    pub at: Timestamp,
    pub result: LoopRunResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<LoopRunMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckRecord>,
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
    pub fn new(
        task: impl Into<String>,
        result: LoopRunResult,
        mode: LoopRunMode,
        duration_ms: u64,
    ) -> Self {
        Self {
            task: task.into(),
            at: Timestamp::now(),
            result,
            mode: Some(mode),
            duration_ms: Some(duration_ms),
            error: None,
            check: None,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRecord {
    pub code: Option<i32>,
    pub timed_out: bool,
    pub output: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopRunResult {
    Completed,
    Failed,
    VerifyFailed,
    TimedOut,
    BudgetExceeded,
    BudgetSkipped,
    SurplusSkipped,
    Canceled,
    Delivered,
    TargetGone,
    CheckSkipped,
    Expired,
    Errored,
    Overlapped,
}

impl LoopRunResult {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::VerifyFailed => "verify failed",
            Self::TimedOut => "timed out",
            Self::BudgetExceeded => "budget exceeded",
            Self::BudgetSkipped => "budget skipped",
            Self::SurplusSkipped => "surplus skipped",
            Self::Canceled => "canceled",
            Self::Delivered => "delivered",
            Self::TargetGone => "target gone",
            Self::CheckSkipped => "skipped",
            Self::Expired => "expired",
            Self::Errored => "error",
            Self::Overlapped => "overlapped",
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

#[derive(Clone, Debug, PartialEq)]
pub struct LoopRunStats {
    pub runs: usize,
    pub streak: usize,
    pub last: LoopRunRecord,
    pub spend_today_usd: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TaskCostSummary {
    pub last_usd: Option<f64>,
    pub avg_usd: Option<f64>,
    pub costed_runs: usize,
}

pub fn log_path(state_root: &Path) -> PathBuf {
    state_root.join("rimz").join(NAME)
}

pub fn append(record: &LoopRunRecord) {
    let state_root = state_home();
    append_to(&state_root, record);
}

fn append_to(state_root: &Path, record: &LoopRunRecord) {
    let capped = capped_record(record);
    log(state_root, MAX_BYTES).append(&capped);
}

pub fn stats(state_root: &Path, now: &Zoned) -> BTreeMap<String, LoopRunStats> {
    let mut stats = BTreeMap::new();
    log(state_root, MAX_BYTES).visit_records(|record: LoopRunRecord| {
        fold_record(record, now, &mut stats);
    });
    stats
}

pub fn task_records(state_root: &Path, task: &str) -> Vec<LoopRunRecord> {
    let mut records = Vec::new();
    log(state_root, MAX_BYTES).visit_records(|record: LoopRunRecord| {
        if record.task == task {
            records.push(record);
        }
    });
    records
}

pub fn task_spend_today(state_root: &Path, task: &str, now: &Zoned) -> f64 {
    spend_on_local_day(&task_records(state_root, task), now)
}

pub fn spend_on_local_day(records: &[LoopRunRecord], now: &Zoned) -> f64 {
    records
        .iter()
        .filter_map(|record| cost_on_local_day(record, now))
        .sum()
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
pub struct DailyBudgetGate {
    pub spend_usd: f64,
    pub cap_usd: f64,
    pub reserved_usd: f64,
}

impl DailyBudgetGate {
    pub fn reason(self) -> String {
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

pub fn daily_budget_gate(
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
    let spend = task_spend_today(state_root, task, now);
    Ok(
        ((spend >= cap) || (reserved > 0.0 && spend + reserved > cap)).then_some(DailyBudgetGate {
            spend_usd: spend,
            cap_usd: cap,
            reserved_usd: reserved,
        }),
    )
}

fn fold_record(record: LoopRunRecord, now: &Zoned, stats: &mut BTreeMap<String, LoopRunStats>) {
    let spend_today_usd = cost_on_local_day(&record, now).unwrap_or(0.0);
    stats
        .entry(record.task.clone())
        .and_modify(|entry| {
            entry.runs += 1;
            entry.spend_today_usd += spend_today_usd;
            if record.at > entry.last.at {
                entry.streak = if record.result == entry.last.result {
                    entry.streak + 1
                } else {
                    1
                };
                entry.last = record.clone();
            }
        })
        .or_insert(LoopRunStats {
            runs: 1,
            streak: 1,
            last: record,
            spend_today_usd,
        });
}

fn capped_record(record: &LoopRunRecord) -> LoopRunRecord {
    let mut capped = record.clone();
    if let Some(error) = &mut capped.error {
        *error = tail_string(error, ERROR_CAP);
    }
    if let Some(check) = &mut capped.check {
        check.output = tail_string(&check.output, CHECK_OUTPUT_CAP);
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

fn log(state_root: &Path, max_bytes: u64) -> crate::diag::rotating::JsonlLog {
    crate::diag::rotating::JsonlLog::new(log_path(state_root), max_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn record(task: &str, second: i64, result: LoopRunResult) -> LoopRunRecord {
        LoopRunRecord {
            task: task.to_owned(),
            at: Timestamp::from_second(second).expect("timestamp"),
            result,
            mode: None,
            duration_ms: None,
            error: None,
            check: None,
            run_id: None,
            transcript_path: None,
            last_message: None,
            target: None,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
        }
    }

    #[test]
    fn append_then_stats_round_trips_records() {
        let dir = tempfile::tempdir().expect("tempdir");

        append_to(dir.path(), &record("wake", 10, LoopRunResult::Delivered));
        append_to(dir.path(), &record("wake", 12, LoopRunResult::TargetGone));

        let now = Timestamp::from_second(20)
            .expect("timestamp")
            .to_zoned(jiff::tz::TimeZone::UTC);
        let stats = stats(dir.path(), &now);
        let wake = stats.get("wake").expect("wake stats");
        assert_eq!(wake.runs, 2);
        assert_eq!(wake.streak, 1);
        assert_eq!(wake.last.result, LoopRunResult::TargetGone);
    }

    #[test]
    fn terminal_records_keep_durable_and_presentation_fields_separate() {
        for result in [
            LoopRunResult::Completed,
            LoopRunResult::Failed,
            LoopRunResult::VerifyFailed,
            LoopRunResult::TimedOut,
            LoopRunResult::BudgetSkipped,
            LoopRunResult::Overlapped,
            LoopRunResult::CheckSkipped,
            LoopRunResult::TargetGone,
            LoopRunResult::Expired,
            LoopRunResult::Errored,
            LoopRunResult::Canceled,
        ] {
            let record = LoopRunRecord::new("matrix", result, LoopRunMode::Scheduled, 17);
            assert_eq!(record.task, "matrix");
            assert_eq!(record.result, result);
            assert_eq!(record.mode, Some(LoopRunMode::Scheduled));
            assert_eq!(record.duration_ms, Some(17));
            assert_eq!(
                (
                    record.error,
                    record.check,
                    record.run_id,
                    record.transcript_path,
                    record.last_message,
                    record.target,
                    record.cost_usd,
                    record.input_tokens,
                    record.output_tokens,
                ),
                (None, None, None, None, None, None, None, None, None)
            );
        }

        let check = CheckRecord {
            code: Some(7),
            timed_out: false,
            output: "check output".to_owned(),
        };
        let mut record =
            LoopRunRecord::new("matrix", LoopRunResult::Failed, LoopRunMode::Manual, 19);
        record.error = Some("durable error".to_owned());
        record.check = Some(check.clone());
        record.run_id = Some("run_1".to_owned());
        record.transcript_path = Some("/tmp/transcript".to_owned());
        record.last_message = Some("last message".to_owned());
        record.target = Some("@coder".to_owned());
        record.cost_usd = Some(1.25);
        record.input_tokens = Some(10);
        record.output_tokens = Some(20);
        let presentation = LoopRunPresentation {
            failure_tail: Some("presentation tail".to_owned()),
            skip_reason: Some("presentation skip".to_owned()),
            streamed: true,
            exit_code: Some(7),
            ..LoopRunPresentation::default()
        };

        assert_eq!(record.error.as_deref(), Some("durable error"));
        assert_eq!(record.check, Some(check));
        assert_eq!(record.run_id.as_deref(), Some("run_1"));
        assert_eq!(record.transcript_path.as_deref(), Some("/tmp/transcript"));
        assert_eq!(record.last_message.as_deref(), Some("last message"));
        assert_eq!(record.target.as_deref(), Some("@coder"));
        assert_eq!(
            (record.cost_usd, record.input_tokens, record.output_tokens),
            (Some(1.25), Some(10), Some(20))
        );
        assert_eq!(
            presentation.failure_tail.as_deref(),
            Some("presentation tail")
        );
        assert_eq!(presentation.exit_code, Some(7));
        assert_eq!(
            presentation.skip_reason.as_deref(),
            Some("presentation skip")
        );
        assert!(presentation.streamed);
    }

    #[test]
    fn verify_failed_run_status_keeps_its_distinct_loop_result() {
        assert_eq!(
            LoopRunResult::from(RunStatus::VerifyFailed),
            LoopRunResult::VerifyFailed
        );
        assert_eq!(LoopRunResult::VerifyFailed.label(), "verify failed");
    }

    #[test]
    fn task_records_folds_log_generations() {
        let dir = tempfile::tempdir().expect("tempdir");
        let old = record("morning", 10, LoopRunResult::Completed);
        let new = record("other", 30, LoopRunResult::Completed);
        let log = log(dir.path(), 1);
        log.append(&old);
        log.append(&new);

        assert_eq!(task_records(dir.path(), "morning"), vec![old]);
        assert_eq!(task_records(dir.path(), "other"), vec![new]);
    }

    #[test]
    fn daily_spend_uses_the_configured_local_day() {
        let now = "2026-06-02T00:30:00-04:00[America/New_York]"
            .parse::<Zoned>()
            .expect("zoned");
        let mut prior = record("wake", 0, LoopRunResult::Completed);
        prior.at = "2026-06-01T23:30:00Z".parse().expect("timestamp");
        prior.cost_usd = Some(3.0);
        let mut today = record("wake", 0, LoopRunResult::Completed);
        today.at = "2026-06-02T04:10:00Z".parse().expect("timestamp");
        today.cost_usd = Some(4.0);
        assert_eq!(spend_on_local_day(&[prior, today], &now), 4.0);
    }

    #[test]
    fn cost_summary_uses_the_last_ten_costed_runs() {
        assert_eq!(cost_summary(&[]), TaskCostSummary::default());

        let mut records = vec![record("wake", 0, LoopRunResult::BudgetSkipped)];
        records[0].cost_usd = Some(f64::NAN);
        let mut first = record("wake", 1, LoopRunResult::Completed);
        first.cost_usd = Some(1.0);
        records.push(first);
        assert_eq!(cost_summary(&records).last_usd, Some(1.0));
        assert_eq!(cost_summary(&records).costed_runs, 1);

        for cost in 2..=12 {
            let mut costed = record("wake", cost, LoopRunResult::Completed);
            costed.cost_usd = Some(cost as f64);
            records.push(costed);
        }
        let summary = cost_summary(&records);
        assert_eq!(summary.last_usd, Some(12.0));
        assert_eq!(summary.avg_usd, Some(7.5));
        assert_eq!(summary.costed_runs, COST_WINDOW);
    }

    #[test]
    fn stats_accumulates_only_same_local_day_spend() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = "2026-06-02T12:00:00-04:00[America/New_York]"
            .parse::<Zoned>()
            .expect("zoned");
        let path = log_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("log parent")).expect("log dir");
        let mut prior = record("wake", 0, LoopRunResult::Completed);
        prior.at = "2026-06-02T03:00:00Z".parse().expect("timestamp");
        prior.cost_usd = Some(3.0);
        let mut today = record("wake", 0, LoopRunResult::Completed);
        today.at = "2026-06-02T04:00:00Z".parse().expect("timestamp");
        today.cost_usd = Some(4.0);
        std::fs::write(
            path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&prior).expect("prior json"),
                serde_json::to_string(&today).expect("today json")
            ),
        )
        .expect("write run log");

        assert_eq!(stats(dir.path(), &now)["wake"].spend_today_usd, 4.0);
    }

    #[test]
    fn daily_gate_reserves_the_next_runs_full_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = "2026-06-02T12:00:00Z[UTC]".parse::<Zoned>().expect("zoned");
        let mut spent = record("bounded", 0, LoopRunResult::Completed);
        spent.at = now.timestamp();
        spent.cost_usd = Some(6.0);
        let path = log_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("log parent")).expect("log dir");
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string(&spent).expect("record json")),
        )
        .expect("write run log");
        let entry = crate::config::TaskEntry {
            budget: Some("$5.00".to_owned()),
            budget_per_day: Some("$10.00".to_owned()),
            ..crate::config::TaskEntry::default()
        };

        let gate = daily_budget_gate(dir.path(), "bounded", &entry, &now)
            .expect("valid gate")
            .expect("next run does not fit");
        assert_eq!(gate.spend_usd, 6.0);
        assert_eq!(gate.reserved_usd, 5.0);
        assert_eq!(gate.cap_usd, 10.0);
    }

    #[test]
    fn stats_folds_rotated_sibling_and_keeps_newest_last() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = log(dir.path(), 1);
        log.append(&record("wake", 20, LoopRunResult::Completed));
        log.append(&record("wake", 10, LoopRunResult::Failed));
        std::fs::OpenOptions::new()
            .append(true)
            .open(log_path(dir.path()))
            .expect("open active")
            .write_all(b"not json\n")
            .expect("append malformed line");

        let now = Timestamp::from_second(30)
            .expect("timestamp")
            .to_zoned(jiff::tz::TimeZone::UTC);
        let stats = stats(dir.path(), &now);
        let wake = stats.get("wake").expect("wake stats");
        assert_eq!(wake.runs, 2);
        assert_eq!(wake.streak, 1);
        assert_eq!(wake.last.result, LoopRunResult::Completed);
    }

    #[test]
    fn stats_tracks_matching_result_streak_across_rotated_and_current_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = log(dir.path(), 1);
        log.append(&record("wake", 10, LoopRunResult::Failed));
        log.append(&record("wake", 20, LoopRunResult::Failed));

        let now = Timestamp::from_second(30)
            .expect("timestamp")
            .to_zoned(jiff::tz::TimeZone::UTC);
        let stats = stats(dir.path(), &now);
        let wake = stats.get("wake").expect("wake stats");
        assert_eq!(wake.runs, 2);
        assert_eq!(wake.streak, 2);
        assert_eq!(wake.last.result, LoopRunResult::Failed);
    }

    #[test]
    fn new_fields_round_trip_and_task_records_filter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut with_detail = record("wake", 10, LoopRunResult::Failed);
        with_detail.mode = Some(LoopRunMode::Manual);
        with_detail.duration_ms = Some(123);
        with_detail.check = Some(CheckRecord {
            code: Some(127),
            timed_out: false,
            output: "missing command".to_owned(),
        });
        with_detail.run_id = Some("run_0123456789abcdef0123456789abcdef".to_owned());
        with_detail.transcript_path = Some("/tmp/rimz/sessions/wake.jsonl".to_owned());
        with_detail.last_message = Some("last words".to_owned());
        with_detail.target = Some("@coder".to_owned());
        append_to(dir.path(), &with_detail);
        append_to(dir.path(), &record("other", 11, LoopRunResult::Completed));

        assert_eq!(task_records(dir.path(), "wake"), vec![with_detail]);
    }

    #[test]
    fn old_minimal_records_still_parse() {
        let line = r#"{"task":"wake","at":"1970-01-01T00:00:10Z","result":"completed"}"#;
        let record: LoopRunRecord = serde_json::from_str(line).expect("legacy record");
        assert_eq!(record.task, "wake");
        assert_eq!(record.result, LoopRunResult::Completed);
        assert_eq!(record.mode, None);
        assert_eq!(record.check, None);
        assert_eq!(record.transcript_path, None);
    }

    #[test]
    fn append_caps_forensic_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut record = record("wake", 10, LoopRunResult::Errored);
        record.error = Some("e".repeat(ERROR_CAP + 20));
        record.last_message = Some("m".repeat(LAST_MESSAGE_CAP + 20));
        record.check = Some(CheckRecord {
            code: Some(1),
            timed_out: false,
            output: "o".repeat(CHECK_OUTPUT_CAP + 20),
        });

        append_to(dir.path(), &record);
        let stored = task_records(dir.path(), "wake")
            .pop()
            .expect("stored record");
        assert_eq!(stored.error.expect("error").len(), ERROR_CAP);
        assert_eq!(stored.last_message.expect("last").len(), LAST_MESSAGE_CAP);
        assert_eq!(stored.check.expect("check").output.len(), CHECK_OUTPUT_CAP);
    }
}
