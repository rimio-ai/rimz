//! User-global loop task run history.
//!
//! Loop config is per-machine, so task outcomes append to one user-global JSONL
//! log. The runner writes best-effort records; `rimz loop list` folds the
//! current and rotated files for its run count, last-run age, and result column.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ledger::paths::state_home;
use crate::run::RunStatus;

const NAME: &str = "loop-runs.log.jsonl";
const MAX_BYTES: u64 = 1_048_576;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRunRecord {
    pub task: String,
    pub at: Timestamp,
    pub result: LoopRunResult,
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
    crate::rotating_log::append_rotating_jsonl(&log_path(state_root), MAX_BYTES, record)
}

pub fn stats(state_root: &Path) -> BTreeMap<String, LoopRunStats> {
    let path = log_path(state_root);
    let mut stats = BTreeMap::new();
    fold_file(&rotated_path(&path), &mut stats);
    fold_file(&path, &mut stats);
    stats
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
}
