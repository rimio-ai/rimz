//! Machine-local consecutive failure counts for loop tasks.
//!
//! Counts survive process exits and reset independently of run-log rotation.
//! An advisory lock serializes updates from different task runners sharing the
//! user-global JSON file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::overlay_store::{OverlayError, OverlayStore};
use crate::config::TaskEntry;
use crate::harness::schedule::run_log::{LoopRunRecord, LoopRunResult};
use crate::store::paths::state_home;

const STORE: OverlayStore = OverlayStore::new("loop-strikes.json", "loop-strikes.lock");
pub(super) const DEFAULT_MAX_STRIKES: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Signal {
    Strike,
    Reset,
    Neutral,
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct StrikesError(#[from] OverlayError);

type Result<T> = std::result::Result<T, StrikesError>;

pub(super) fn classify(record: &LoopRunRecord) -> Signal {
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
    STORE.path(state_root)
}

pub fn load() -> BTreeMap<String, u32> {
    load_from(&state_home())
}

pub(super) fn note(key: &str, signal: Signal) -> Result<u32> {
    note_in(&state_home(), key, signal)
}

pub fn clear(key: &str) -> Result<bool> {
    clear_from(&state_home(), key)
}

pub(super) fn rename(old: &str, new: &str) -> Result<bool> {
    rename_in(&state_home(), old, new)
}

pub(super) fn prune_orphans(known: &BTreeSet<String>, scopes: &BTreeSet<String>) -> Result<usize> {
    prune_orphans_in(&state_home(), known, scopes)
}

fn load_from(state_root: &Path) -> BTreeMap<String, u32> {
    STORE.load(state_root)
}

fn note_in(state_root: &Path, key: &str, signal: Signal) -> Result<u32> {
    if signal == Signal::Neutral {
        return Ok(load_from(state_root).get(key).copied().unwrap_or(0));
    }
    STORE
        .mutate::<u32, _>(state_root, |strikes| match signal {
            Signal::Strike => {
                let previous = strikes.get(key).copied().unwrap_or(0);
                let count = previous.saturating_add(1);
                strikes.insert(key.to_owned(), count);
                (count, count != previous)
            }
            Signal::Reset => (0, strikes.remove(key).is_some()),
            Signal::Neutral => (strikes.get(key).copied().unwrap_or(0), false),
        })
        .map_err(Into::into)
}

fn clear_from(state_root: &Path, key: &str) -> Result<bool> {
    STORE.remove::<u32>(state_root, key).map_err(Into::into)
}

fn rename_in(state_root: &Path, old: &str, new: &str) -> Result<bool> {
    STORE
        .rename::<u32>(state_root, old, new)
        .map_err(Into::into)
}

fn prune_orphans_in(
    state_root: &Path,
    known: &BTreeSet<String>,
    scopes: &BTreeSet<String>,
) -> Result<usize> {
    STORE
        .prune_orphans_in_scopes::<u32>(state_root, known, scopes)
        .map_err(Into::into)
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
    fn counter_round_trips_and_resets() {
        const KEY: &str = "machine::nightly";
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(note_in(dir.path(), KEY, Signal::Strike).expect("strike"), 1);
        let encoded: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path(dir.path())).expect("serialized strikes"))
                .expect("strikes json");
        assert_eq!(encoded, serde_json::json!({"machine::nightly": 1}));
        assert_eq!(note_in(dir.path(), KEY, Signal::Strike).expect("strike"), 2);
        assert_eq!(load_from(dir.path()).get(KEY), Some(&2));
        assert_eq!(note_in(dir.path(), KEY, Signal::Reset).expect("reset"), 0);
        assert!(!load_from(dir.path()).contains_key(KEY));
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
