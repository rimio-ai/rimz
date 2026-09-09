//! Machine-global user prompt ledger for spend-session boundaries.
//!
//! Lifecycle hooks append one record when a human prompt starts a turn. Spend
//! aggregation reads the current and rotated files so only user input opens or
//! bridges the five-hour session window; every priced entry inside that window
//! still contributes to the tally.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::disk::paths::state_home;
use crate::ids::AgentKind;

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
        let origin = crate::utils::path::normalize_path_lexical(origin);
        origin.is_absolute().then_some(origin)
    });
    crate::disk::rotating::append(&log_path(state_root), MAX_BYTES, &record);
}

pub(super) fn load() -> Vec<UserInputRecord> {
    load_in(&state_home())
}

pub fn load_in(state_root: &Path) -> Vec<UserInputRecord> {
    let path = log_path(state_root);
    let mut records = Vec::new();
    crate::disk::rotating::visit_records(&path, |record: UserInputRecord| {
        records.push(record);
    });
    records
}

fn log_path(state_root: &Path) -> PathBuf {
    state_root.join("rimz").join(NAME)
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
            crate::disk::rotating::rotated_path(&path),
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
}
