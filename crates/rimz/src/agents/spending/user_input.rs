//! Machine-global user prompt ledger for spend-session boundaries.
//!
//! Lifecycle hooks append one record when a human prompt starts a turn. Spend
//! aggregation reads the current and rotated files so only user input opens or
//! bridges the five-hour session window; every priced entry inside that window
//! still contributes to the tally.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::BufRead;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ids::AgentKind;
use crate::store::parse_cache::FileStamp;
use crate::store::paths::state_home;

const NAME: &str = "user-inputs.log.jsonl";
const MAX_BYTES: u64 = 1_048_576;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserInputRecord {
    pub at: Timestamp,
    pub kind: AgentKind,
    /// The agent's worktree/project root at prompt time, normalized lexical
    /// absolute — matched against [`super::SpendScope`] for the cockpit tally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<PathBuf>,
}

pub fn append(record: &UserInputRecord) {
    append_in(&state_home(), record);
}

pub fn append_in(state_root: &Path, record: &UserInputRecord) {
    let mut record = record.clone();
    record.origin = record.origin.as_deref().and_then(|origin| {
        let origin = crate::worktree::normalize_path_lexical(origin);
        origin.is_absolute().then_some(origin)
    });
    crate::diag::rotating::JsonlLog::new(log_path(state_root), MAX_BYTES).append(&record);
}

pub fn load() -> Vec<UserInputRecord> {
    load_in(&state_home())
}

pub fn load_in(state_root: &Path) -> Vec<UserInputRecord> {
    let path = log_path(state_root);
    let mut records = Vec::new();
    append_records(&rotated_path(&path), &mut records);
    append_records(&path, &mut records);
    records
}

pub fn signature() -> u64 {
    signature_in(&state_home())
}

pub fn signature_in(state_root: &Path) -> u64 {
    let path = log_path(state_root);
    let mut hasher = DefaultHasher::new();
    FileStamp::of(&rotated_path(&path)).hash(&mut hasher);
    FileStamp::of(&path).hash(&mut hasher);
    hasher.finish()
}

fn log_path(state_root: &Path) -> PathBuf {
    state_root.join("rimz").join(NAME)
}

fn rotated_path(path: &Path) -> PathBuf {
    path.with_file_name("user-inputs.log.1.jsonl")
}

fn append_records(path: &Path, records: &mut Vec<UserInputRecord>) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        if let Ok(record) = serde_json::from_str(&line) {
            records.push(record);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(at: i64, origin: Option<&str>) -> UserInputRecord {
        UserInputRecord {
            at: Timestamp::from_second(at).expect("timestamp"),
            kind: AgentKind::new_unchecked("codex"),
            origin: origin.map(PathBuf::from),
        }
    }

    #[test]
    fn append_load_round_trip_normalizes_absolute_origin() {
        let dir = tempfile::tempdir().expect("tempdir");

        append_in(dir.path(), &record(10, Some("/tmp/repo/../repo/worktree")));
        append_in(dir.path(), &record(20, Some("relative/worktree")));

        assert_eq!(
            load_in(dir.path()),
            vec![record(10, Some("/tmp/repo/worktree")), record(20, None),]
        );
    }

    #[test]
    fn load_folds_rotated_then_current_and_skips_bad_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = log_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("log parent")).expect("mkdir log parent");
        std::fs::write(
            rotated_path(&path),
            serde_json::to_string(&record(10, Some("/tmp/one"))).expect("json") + "\n",
        )
        .expect("write rotated");
        std::fs::write(
            path,
            "not json\n".to_owned()
                + &serde_json::to_string(&record(20, Some("/tmp/two"))).expect("json")
                + "\n",
        )
        .expect("write current");

        assert_eq!(
            load_in(dir.path()),
            vec![record(10, Some("/tmp/one")), record(20, Some("/tmp/two")),]
        );
    }

    #[test]
    fn signature_changes_on_append() {
        let dir = tempfile::tempdir().expect("tempdir");
        let before = signature_in(dir.path());

        append_in(dir.path(), &record(10, Some("/tmp/repo")));

        assert_ne!(signature_in(dir.path()), before);
    }
}
