//! Machine-local consecutive failure counts for loop tasks.
//!
//! Counts survive process exits and reset independently of run-log rotation.
//! An advisory lock serializes updates from different task runners sharing the
//! user-global JSON file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::TaskEntry;
use crate::harness::schedule::run_log::{LoopRunRecord, LoopRunResult};
use crate::store::atomic::{AtomicErr, write_temp_then_rename_cache};
use crate::store::lock::{LockErr, WorkspaceLock};
use crate::store::paths::state_home;

const NAME: &str = "loop-strikes.json";
const LOCK_NAME: &str = "loop-strikes.lock";
pub const DEFAULT_MAX_STRIKES: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Signal {
    Strike,
    Reset,
    Neutral,
}

#[derive(Debug, thiserror::Error)]
pub enum StrikesError {
    #[error(transparent)]
    Lock(#[from] LockErr),
    #[error(transparent)]
    Write(#[from] AtomicErr),
}

type Result<T> = std::result::Result<T, StrikesError>;

pub fn classify(record: &LoopRunRecord) -> Signal {
    match record.result {
        LoopRunResult::Failed
        | LoopRunResult::VerifyFailed
        | LoopRunResult::TimedOut
        | LoopRunResult::Errored
        | LoopRunResult::BudgetExceeded => Signal::Strike,
        LoopRunResult::Completed | LoopRunResult::Delivered => {
            if record
                .check
                .as_ref()
                .is_some_and(|check| !check_passed(check))
            {
                Signal::Strike
            } else {
                Signal::Reset
            }
        }
        LoopRunResult::CheckSkipped => match record.check.as_ref().map(check_passed) {
            Some(true) => Signal::Reset,
            Some(false) | None => Signal::Neutral,
        },
        LoopRunResult::BudgetSkipped
        | LoopRunResult::SurplusSkipped
        | LoopRunResult::SkippedWindow
        | LoopRunResult::Overlapped
        | LoopRunResult::Canceled
        | LoopRunResult::Expired
        | LoopRunResult::TargetGone => Signal::Neutral,
    }
}

fn check_passed(check: &crate::harness::schedule::run_log::CheckRecord) -> bool {
    check.code == Some(0) && !check.timed_out
}

pub fn threshold(entry: &TaskEntry) -> Option<u32> {
    match entry.max_strikes.unwrap_or(DEFAULT_MAX_STRIKES) {
        0 => None,
        max => Some(max),
    }
}

pub fn path(state_root: &Path) -> PathBuf {
    state_root.join("rimz").join(NAME)
}

fn lock_path(state_root: &Path) -> PathBuf {
    state_root.join("rimz").join(LOCK_NAME)
}

pub fn load() -> BTreeMap<String, u32> {
    load_from(&state_home())
}

pub fn note(name: &str, signal: Signal) -> Result<u32> {
    note_in(&state_home(), name, signal)
}

pub fn clear(name: &str) -> Result<bool> {
    clear_from(&state_home(), name)
}

pub fn rename(old: &str, new: &str) -> Result<bool> {
    rename_in(&state_home(), old, new)
}

pub fn prune_orphans(known: &BTreeSet<String>) -> Result<usize> {
    prune_orphans_in(&state_home(), known)
}

fn load_from(state_root: &Path) -> BTreeMap<String, u32> {
    let Ok(bytes) = std::fs::read(path(state_root)) else {
        return BTreeMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn note_in(state_root: &Path, name: &str, signal: Signal) -> Result<u32> {
    if signal == Signal::Neutral {
        return Ok(load_from(state_root).get(name).copied().unwrap_or(0));
    }
    let _guard = WorkspaceLock::acquire(&lock_path(state_root))?;
    let mut strikes = load_from(state_root);
    let (count, changed) = match signal {
        Signal::Strike => {
            let count = strikes.get(name).copied().unwrap_or(0).saturating_add(1);
            strikes.insert(name.to_owned(), count);
            (count, true)
        }
        Signal::Reset => (0, strikes.remove(name).is_some()),
        Signal::Neutral => return Ok(strikes.get(name).copied().unwrap_or(0)),
    };
    if changed {
        write_temp_then_rename_cache(&path(state_root), &strikes)?;
    }
    Ok(count)
}

fn clear_from(state_root: &Path, name: &str) -> Result<bool> {
    let _guard = WorkspaceLock::acquire(&lock_path(state_root))?;
    let mut strikes = load_from(state_root);
    let removed = strikes.remove(name).is_some();
    if removed {
        write_temp_then_rename_cache(&path(state_root), &strikes)?;
    }
    Ok(removed)
}

fn rename_in(state_root: &Path, old: &str, new: &str) -> Result<bool> {
    let _guard = WorkspaceLock::acquire(&lock_path(state_root))?;
    let mut strikes = load_from(state_root);
    let Some(count) = strikes.remove(old) else {
        return Ok(false);
    };
    strikes.insert(new.to_owned(), count);
    write_temp_then_rename_cache(&path(state_root), &strikes)?;
    Ok(true)
}

fn prune_orphans_in(state_root: &Path, known: &BTreeSet<String>) -> Result<usize> {
    let _guard = WorkspaceLock::acquire(&lock_path(state_root))?;
    let mut strikes = load_from(state_root);
    let before = strikes.len();
    strikes.retain(|name, _| known.contains(name));
    let removed = before - strikes.len();
    if removed > 0 {
        write_temp_then_rename_cache(&path(state_root), &strikes)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::harness::schedule::run_log::{CheckRecord, LoopRunMode};

    fn record(result: LoopRunResult, check: Option<CheckRecord>) -> LoopRunRecord {
        LoopRunRecord {
            task: "nightly".to_owned(),
            at: Timestamp::from_second(1).expect("timestamp"),
            result,
            mode: Some(LoopRunMode::Scheduled),
            duration_ms: Some(1),
            error: None,
            check,
            run_id: None,
            transcript_path: None,
            last_message: None,
            target: None,
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
        }
    }

    fn check(code: Option<i32>, timed_out: bool) -> Option<CheckRecord> {
        Some(CheckRecord {
            code,
            timed_out,
            output: String::new(),
        })
    }

    #[test]
    fn classifies_failure_progress_and_poll_signals() {
        assert_eq!(
            classify(&record(LoopRunResult::Completed, check(Some(1), false))),
            Signal::Strike
        );
        assert_eq!(
            classify(&record(LoopRunResult::Delivered, check(Some(0), true))),
            Signal::Strike
        );
        assert_eq!(
            classify(&record(LoopRunResult::CheckSkipped, check(Some(0), false))),
            Signal::Reset
        );
        assert_eq!(
            classify(&record(LoopRunResult::CheckSkipped, check(Some(1), false))),
            Signal::Neutral
        );
        assert_eq!(
            classify(&record(LoopRunResult::Completed, None)),
            Signal::Reset
        );
        assert_eq!(
            classify(&record(LoopRunResult::BudgetSkipped, None)),
            Signal::Neutral
        );
        assert_eq!(
            classify(&record(LoopRunResult::SurplusSkipped, None)),
            Signal::Neutral
        );
        for result in [
            LoopRunResult::Failed,
            LoopRunResult::VerifyFailed,
            LoopRunResult::TimedOut,
            LoopRunResult::Errored,
            LoopRunResult::BudgetExceeded,
        ] {
            assert_eq!(classify(&record(result, None)), Signal::Strike);
        }
    }

    #[test]
    fn counter_round_trips_resets_and_tolerates_corruption() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            note_in(dir.path(), "nightly", Signal::Strike).expect("strike"),
            1
        );
        assert_eq!(
            note_in(dir.path(), "nightly", Signal::Strike).expect("strike"),
            2
        );
        assert_eq!(load_from(dir.path()).get("nightly"), Some(&2));
        assert_eq!(
            note_in(dir.path(), "nightly", Signal::Reset).expect("reset"),
            0
        );
        assert!(!load_from(dir.path()).contains_key("nightly"));

        std::fs::write(path(dir.path()), b"not json").expect("corrupt state");
        assert!(load_from(dir.path()).is_empty());
    }

    #[test]
    fn rename_and_prune_preserve_known_counts() {
        let dir = tempfile::tempdir().expect("tempdir");
        note_in(dir.path(), "old", Signal::Strike).expect("old strike");
        note_in(dir.path(), "gone", Signal::Strike).expect("gone strike");
        assert!(rename_in(dir.path(), "old", "new").expect("rename"));
        assert!(!rename_in(dir.path(), "missing", "other").expect("missing"));

        let removed = prune_orphans_in(
            dir.path(),
            &BTreeSet::from(["new".to_owned(), "other".to_owned()]),
        )
        .expect("prune");
        assert_eq!(removed, 1);
        assert_eq!(
            load_from(dir.path()),
            BTreeMap::from([("new".to_owned(), 1)])
        );
    }

    #[test]
    fn threshold_defaults_overrides_and_disables() {
        assert_eq!(threshold(&TaskEntry::default()), Some(DEFAULT_MAX_STRIKES));
        assert_eq!(
            threshold(&TaskEntry {
                max_strikes: Some(7),
                ..TaskEntry::default()
            }),
            Some(7)
        );
        assert_eq!(
            threshold(&TaskEntry {
                max_strikes: Some(0),
                ..TaskEntry::default()
            }),
            None
        );
    }
}
