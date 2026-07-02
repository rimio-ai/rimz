//! User-global loop task run history.
//!
//! Loop config is per-machine, so task outcomes append to one user-global JSONL
//! log. Each loaded `rimz loop run`/`fire` appends one best-effort record with
//! the terminal result, mode, duration, and capped forensics. `rimz loop list`
//! folds the current and rotated files for summary columns; `rimz loop show`
//! reads the same records for per-task inspection.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::harness::run::RunStatus;
use crate::ledger::paths::state_home;

const NAME: &str = "loop-runs.log.jsonl";
const MAX_BYTES: u64 = 4 * 1_048_576;
const CHECK_OUTPUT_CAP: usize = 4 * 1024;
const ERROR_CAP: usize = 2 * 1024;
const LAST_MESSAGE_CAP: usize = 2 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    pub last_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
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
            RunStatus::Canceled => Self::Canceled,
            RunStatus::Pending | RunStatus::Running => Self::Failed,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopRunStats {
    pub runs: usize,
    pub last: LoopRunRecord,
}

pub fn log_path(state_root: &Path) -> PathBuf {
    state_root.join("rimz").join(NAME)
}

pub fn append(record: &LoopRunRecord) {
    let state_root = state_home();
    let path = log_path(&state_root);
    if let Err(err) = append_to(&state_root, record) {
        tracing::debug!(path = %path.display(), error = %err, "loop run log append failed");
    }
}

fn append_to(state_root: &Path, record: &LoopRunRecord) -> std::io::Result<()> {
    let capped = capped_record(record);
    crate::diag::rotating::append_rotating_jsonl(&log_path(state_root), MAX_BYTES, &capped)
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
                    entry.last = record.clone();
                }
            })
            .or_insert(LoopRunStats {
                runs: 1,
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
            last_message: None,
            target: None,
        }
    }

    #[test]
    fn append_then_stats_round_trips_records() {
        let dir = tempfile::tempdir().expect("tempdir");

        append_to(dir.path(), &record("wake", 10, LoopRunResult::Delivered)).expect("append 1");
        append_to(dir.path(), &record("wake", 12, LoopRunResult::TargetGone)).expect("append 2");

        let stats = stats(dir.path());
        let wake = stats.get("wake").expect("wake stats");
        assert_eq!(wake.runs, 2);
        assert_eq!(wake.last.result, LoopRunResult::TargetGone);
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
        assert_eq!(wake.last.result, LoopRunResult::Completed);
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
        with_detail.last_message = Some("last words".to_owned());
        with_detail.target = Some("@coder".to_owned());
        append_to(dir.path(), &with_detail).expect("append detail");
        append_to(dir.path(), &record("other", 11, LoopRunResult::Completed))
            .expect("append other");

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

        append_to(dir.path(), &record).expect("append capped");
        let stored = task_records(dir.path(), "wake")
            .pop()
            .expect("stored record");
        assert_eq!(stored.error.expect("error").len(), ERROR_CAP);
        assert_eq!(stored.last_message.expect("last").len(), LAST_MESSAGE_CAP);
        assert_eq!(stored.check.expect("check").output.len(), CHECK_OUTPUT_CAP);
    }
}
