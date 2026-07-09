//! User-global loop task run history.
//!
//! Loop config is per-machine, so task outcomes append to one user-global JSONL
//! log. Each loaded `rimz loop run`/`fire` appends one best-effort record with
//! the terminal result, mode, duration, and capped forensics. `rimz loop list`
//! folds the current and rotated files for summary columns; `rimz loop show`
//! reads the same records for per-task inspection.

use std::collections::{BTreeMap, HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::io::BufRead;
use std::path::{Path, PathBuf};

use jiff::{Timestamp, Zoned};
use serde::{Deserialize, Serialize};

use crate::harness::run::RunStatus;
use crate::store::parse_cache::FileStamp;
use crate::store::paths::state_home;

const NAME: &str = "loop-runs.log.jsonl";
const MAX_BYTES: u64 = 4 * 1_048_576;
const CHECK_OUTPUT_CAP: usize = 4 * 1024;
const ERROR_CAP: usize = 2 * 1024;
const LAST_MESSAGE_CAP: usize = 2 * 1024;

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
    TimedOut,
    BudgetExceeded,
    BudgetSkipped,
    Canceled,
    Delivered,
    TargetGone,
    SkippedWindow,
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
            Self::TimedOut => "timed out",
            Self::BudgetExceeded => "budget exceeded",
            Self::BudgetSkipped => "budget skipped",
            Self::Canceled => "canceled",
            Self::Delivered => "delivered",
            Self::TargetGone => "target gone",
            Self::SkippedWindow => "skipped",
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
    crate::diag::rotating::JsonlLog::new(log_path(state_root), MAX_BYTES).append(&capped);
}

pub fn stats(state_root: &Path) -> BTreeMap<String, LoopRunStats> {
    let path = log_path(state_root);
    let mut stats = BTreeMap::new();
    fold_file(&rotated_path(&path), &mut stats);
    fold_file(&path, &mut stats);
    stats
}

pub fn task_records(state_root: &Path, task: &str) -> Vec<LoopRunRecord> {
    let path = log_path(state_root);
    let mut records = Vec::new();
    append_task_records(&rotated_path(&path), task, &mut records);
    append_task_records(&path, task, &mut records);
    records
}

pub fn task_spend_today(state_root: &Path, task: &str, now: &Zoned) -> f64 {
    spend_on_local_day(&task_records(state_root, task), now)
}

pub fn spend_on_local_day(records: &[LoopRunRecord], now: &Zoned) -> f64 {
    let date = now.date();
    records
        .iter()
        .filter(|record| record.at.to_zoned(now.time_zone().clone()).date() == date)
        .filter_map(|record| record.cost_usd)
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
        .sum()
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

pub fn automation_transcripts() -> HashSet<PathBuf> {
    automation_transcripts_in(&state_home())
}

pub(crate) fn automation_transcripts_in(state_root: &Path) -> HashSet<PathBuf> {
    let path = log_path(state_root);
    let mut transcripts = HashSet::new();
    append_automation_transcripts(&rotated_path(&path), &mut transcripts);
    append_automation_transcripts(&path, &mut transcripts);
    transcripts
}

pub fn automation_signature() -> u64 {
    automation_signature_in(&state_home())
}

pub(crate) fn automation_signature_in(state_root: &Path) -> u64 {
    let path = log_path(state_root);
    let mut hasher = DefaultHasher::new();
    FileStamp::of(&rotated_path(&path)).hash(&mut hasher);
    FileStamp::of(&path).hash(&mut hasher);
    hasher.finish()
}

fn fold_file(path: &Path, stats: &mut BTreeMap<String, LoopRunStats>) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let lines = std::io::BufReader::new(file).lines();
    for line in lines.map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<LoopRunRecord>(&line) else {
            continue;
        };
        stats
            .entry(record.task.clone())
            .and_modify(|entry| {
                entry.runs += 1;
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
            });
    }
}

fn append_task_records(path: &Path, task: &str, records: &mut Vec<LoopRunRecord>) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let lines = std::io::BufReader::new(file).lines();
    for line in lines.map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<LoopRunRecord>(&line) else {
            continue;
        };
        if record.task == task {
            records.push(record);
        }
    }
}

fn append_automation_transcripts(path: &Path, transcripts: &mut HashSet<PathBuf>) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let lines = std::io::BufReader::new(file).lines();
    for line in lines.map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<LoopRunRecord>(&line) else {
            continue;
        };
        let Some(transcript_path) = record.transcript_path.as_deref() else {
            continue;
        };
        let transcript_path = crate::worktree::normalize_path_lexical(Path::new(transcript_path));
        if transcript_path.is_absolute() {
            transcripts.insert(transcript_path);
        }
    }
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

fn rotated_path(path: &Path) -> PathBuf {
    path.with_file_name("loop-runs.log.1.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }

    #[test]
    fn append_then_stats_round_trips_records() {
        let dir = tempfile::tempdir().expect("tempdir");

        append_to(dir.path(), &record("wake", 10, LoopRunResult::Delivered));
        append_to(dir.path(), &record("wake", 12, LoopRunResult::TargetGone));

        let stats = stats(dir.path());
        let wake = stats.get("wake").expect("wake stats");
        assert_eq!(wake.runs, 2);
        assert_eq!(wake.streak, 1);
        assert_eq!(wake.last.result, LoopRunResult::TargetGone);
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
        let path = log_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("log parent")).expect("mkdir log parent");
        std::fs::write(
            rotated_path(&path),
            serde_json::to_string(&record("wake", 20, LoopRunResult::Completed)).expect("json")
                + "\n",
        )
        .expect("write rotated");
        std::fs::write(
            path,
            serde_json::to_string(&record("wake", 10, LoopRunResult::Failed)).expect("json")
                + "\n"
                + "not json\n",
        )
        .expect("write current");

        let stats = stats(dir.path());
        let wake = stats.get("wake").expect("wake stats");
        assert_eq!(wake.runs, 2);
        assert_eq!(wake.streak, 1);
        assert_eq!(wake.last.result, LoopRunResult::Completed);
    }

    #[test]
    fn stats_tracks_matching_result_streak_across_rotated_and_current_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = log_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("log parent")).expect("mkdir log parent");
        std::fs::write(
            rotated_path(&path),
            serde_json::to_string(&record("wake", 10, LoopRunResult::Failed)).expect("json") + "\n",
        )
        .expect("write rotated");
        std::fs::write(
            path,
            serde_json::to_string(&record("wake", 20, LoopRunResult::Failed)).expect("json") + "\n",
        )
        .expect("write current");

        let stats = stats(dir.path());
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
    fn automation_transcripts_fold_current_and_rotated_logs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = log_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("log parent")).expect("mkdir log parent");
        let mut rotated = record("codex-ping", 10, LoopRunResult::Completed);
        rotated.transcript_path = Some("/tmp/rimz/../rimz/codex.jsonl".to_owned());
        let mut current = record("claude-ping", 20, LoopRunResult::Completed);
        current.transcript_path = Some("/tmp/rimz/claude.jsonl".to_owned());
        let mut relative = record("relative", 30, LoopRunResult::Completed);
        relative.transcript_path = Some("relative.jsonl".to_owned());
        std::fs::write(
            rotated_path(&path),
            serde_json::to_string(&rotated).expect("json") + "\n",
        )
        .expect("write rotated");
        std::fs::write(
            path,
            serde_json::to_string(&current).expect("json")
                + "\n"
                + &serde_json::to_string(&relative).expect("json")
                + "\nnot json\n",
        )
        .expect("write current");

        let transcripts = automation_transcripts_in(dir.path());

        assert_eq!(
            transcripts,
            HashSet::from([
                PathBuf::from("/tmp/rimz/codex.jsonl"),
                PathBuf::from("/tmp/rimz/claude.jsonl")
            ])
        );
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
