//! Machine-local consecutive failure counts for loop tasks.
//!
//! Counts survive process exits and reset independently of run-log rotation.
//! An advisory lock serializes updates from different task runners sharing the
//! user-global JSON file.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::overlay_store::{OverlayError, OverlayStore};
use crate::config::TaskEntry;
use crate::disk::paths::loops_dir;
use crate::harness::schedule::run_log::{LoopRunRecord, LoopRunResult};

const STORE: OverlayStore = OverlayStore::new("loop-strikes.json", "loop-strikes.lock");
/// The workflow run ids each count has taken a signal from, newest last.
const COUNTED: OverlayStore = OverlayStore::new("loop-strike-runs.json", "loop-strike-runs.lock");
const COUNTED_RUNS: usize = 64;
pub(super) const DEFAULT_MAX_STRIKES: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Signal {
    Strike,
    Reset,
    Neutral,
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(super) struct StrikesError(#[from] OverlayError);

type Result<T> = std::result::Result<T, StrikesError>;

pub(super) fn classify(record: &LoopRunRecord) -> Signal {
    if record
        .watch
        .as_ref()
        .is_some_and(|watch| !watch.is_terminal())
    {
        return Signal::Neutral;
    }
    match record.result {
        LoopRunResult::Failed
        | LoopRunResult::VerifyFailed
        | LoopRunResult::TimedOut
        | LoopRunResult::Errored
        | LoopRunResult::BudgetExceeded => Signal::Strike,
        // A workflow run's launch waits for its report, which decides.
        LoopRunResult::Launched if record.run_id.is_some() => Signal::Neutral,
        LoopRunResult::Completed | LoopRunResult::Delivered | LoopRunResult::Launched => {
            Signal::Reset
        }
        LoopRunResult::CheckSkipped
        | LoopRunResult::BudgetSkipped
        | LoopRunResult::SurplusSkipped
        | LoopRunResult::AccountSkipped
        | LoopRunResult::ThrottleSkipped
        | LoopRunResult::CapacityHeld
        | LoopRunResult::Overlapped
        | LoopRunResult::Deferred
        | LoopRunResult::TakeoverBlocked
        | LoopRunResult::StartFailed
        | LoopRunResult::Canceled
        | LoopRunResult::Replaced
        | LoopRunResult::SignalSkipped
        | LoopRunResult::WorkflowSkipped
        | LoopRunResult::Expired
        | LoopRunResult::TargetGone => Signal::Neutral,
    }
}

pub fn threshold(entry: &TaskEntry) -> Option<u32> {
    match entry.max_strikes.unwrap_or(DEFAULT_MAX_STRIKES) {
        0 => None,
        max => Some(max),
    }
}

#[cfg(test)]
fn path(state_root: &Path) -> std::path::PathBuf {
    STORE.path(state_root)
}

pub fn load() -> BTreeMap<String, u32> {
    load_from(&loops_dir())
}

pub(super) fn note(key: &str, signal: Signal) -> Result<u32> {
    note_in(&loops_dir(), key, signal)
}

/// Note a workflow run's signal at most once per run id; the flag says whether it counted.
pub(super) fn note_run(key: &str, run: &str, signal: Signal) -> Result<(u32, bool)> {
    note_run_in(&loops_dir(), key, run, signal)
}

pub(super) fn clear(key: &str) -> Result<bool> {
    clear_from(&loops_dir(), key)
}

pub(super) fn rename(old: &str, new: &str) -> Result<bool> {
    rename_in(&loops_dir(), old, new)
}

pub(super) fn prune_orphans(known: &BTreeSet<String>, scopes: &BTreeSet<String>) -> Result<usize> {
    prune_orphans_in(&loops_dir(), known, scopes)
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

/// The run id is recorded before the count moves, so a crash between the two loses the
/// signal rather than counting it twice.
fn note_run_in(state_root: &Path, key: &str, run: &str, signal: Signal) -> Result<(u32, bool)> {
    let fresh = COUNTED.mutate::<Vec<String>, _>(state_root, |counted| {
        let runs = counted.entry(key.to_owned()).or_default();
        if runs.iter().any(|seen| seen == run) {
            return (false, false);
        }
        runs.push(run.to_owned());
        let excess = runs.len().saturating_sub(COUNTED_RUNS);
        runs.drain(..excess);
        (true, true)
    })?;
    let signal = if fresh { signal } else { Signal::Neutral };
    Ok((note_in(state_root, key, signal)?, fresh))
}

fn clear_from(state_root: &Path, key: &str) -> Result<bool> {
    COUNTED.remove::<Vec<String>>(state_root, key)?;
    STORE.remove::<u32>(state_root, key).map_err(Into::into)
}

fn rename_in(state_root: &Path, old: &str, new: &str) -> Result<bool> {
    COUNTED.rename::<Vec<String>>(state_root, old, new)?;
    STORE
        .rename::<u32>(state_root, old, new)
        .map_err(Into::into)
}

fn prune_orphans_in(
    state_root: &Path,
    known: &BTreeSet<String>,
    scopes: &BTreeSet<String>,
) -> Result<usize> {
    COUNTED.prune_orphans_in_scopes::<Vec<String>>(state_root, known, scopes)?;
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
            kind: None,
            judge: Vec::new(),
            checkout: None,
            worktree: None,
            task: "nightly".to_owned(),
            root: None,
            at: Timestamp::from_second(1).expect("timestamp"),
            result,
            mode: Some(LoopRunMode::Scheduled),
            duration_ms: Some(1),
            throttle_wait_ms: None,
            late_ms: None,
            holder: None,
            run_lock: None,
            absorbed: 0,
            error: None,
            check,
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

    fn check(code: Option<i32>, timed_out: bool) -> Option<CheckRecord> {
        Some(CheckRecord {
            duration_ms: None,
            agent: None,
            output_path: None,
            code,
            timed_out,
            output: String::new(),
        })
    }

    #[test]
    fn overlap_results_are_neutral() {
        for name in ["deferred", "replaced", "overlapped"] {
            let result = serde_json::from_value::<LoopRunResult>(serde_json::json!(name));
            assert!(result.is_ok(), "{name} must be a history result");
            assert_eq!(classify(&record(result.unwrap(), None)), Signal::Neutral);
        }
    }

    #[test]
    fn classifies_outcomes_without_reading_the_guard() {
        for (result, check) in [
            (LoopRunResult::Completed, check(Some(1), false)),
            (LoopRunResult::Delivered, check(Some(0), true)),
            (LoopRunResult::Completed, None),
            (LoopRunResult::Launched, None),
        ] {
            assert_eq!(
                classify(&record(result, check)),
                Signal::Reset,
                "{result:?}"
            );
        }
        let mut judged =
            serde_json::to_value(record(LoopRunResult::CheckSkipped, check(Some(0), false)))
                .unwrap();
        judged["judge"] = serde_json::json!([
            {"profile": "haiku", "kind": "claude", "verdict": true},
        ]);
        let judged: LoopRunRecord = serde_json::from_value(judged).unwrap();
        assert_eq!(
            classify(&judged),
            Signal::Neutral,
            "a pass that skips is no reset"
        );
        for result in [
            LoopRunResult::CheckSkipped,
            LoopRunResult::BudgetSkipped,
            LoopRunResult::SurplusSkipped,
            LoopRunResult::AccountSkipped,
            LoopRunResult::ThrottleSkipped,
            LoopRunResult::CapacityHeld,
            LoopRunResult::TakeoverBlocked,
            LoopRunResult::WorkflowSkipped,
            LoopRunResult::Canceled,
            LoopRunResult::Expired,
            LoopRunResult::TargetGone,
            LoopRunResult::StartFailed,
        ] {
            assert_eq!(
                classify(&record(result, check(Some(1), false))),
                Signal::Neutral
            );
        }
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
    fn a_workflow_launch_waits_for_its_report() {
        let mut launch = record(LoopRunResult::Launched, None);
        launch.run_id = Some("wf_1".to_owned());
        assert_eq!(classify(&launch), Signal::Neutral);
    }

    #[test]
    fn a_run_reported_twice_counts_once() {
        let dir = tempfile::tempdir().unwrap();
        let key = "machine::nightly";
        assert_eq!(
            note_run_in(dir.path(), key, "wf_1", Signal::Strike).unwrap(),
            (1, true)
        );
        assert_eq!(
            note_run_in(dir.path(), key, "wf_1", Signal::Strike).unwrap(),
            (1, false)
        );
        assert_eq!(
            note_run_in(dir.path(), key, "wf_2", Signal::Strike).unwrap(),
            (2, true)
        );
        assert_eq!(
            note_run_in(dir.path(), key, "wf_2", Signal::Reset).unwrap(),
            (2, false)
        );
        assert!(clear_from(dir.path(), key).unwrap());
        assert_eq!(
            note_run_in(dir.path(), key, "wf_1", Signal::Strike).unwrap(),
            (1, true),
            "clearing a count forgets the runs it took"
        );
    }

    #[test]
    fn running_watch_notice_is_strike_neutral() {
        let dir = tempfile::tempdir().unwrap();
        let key = "instance::watch";
        note_in(dir.path(), key, Signal::Strike).unwrap();
        for result in [
            LoopRunResult::Delivered,
            LoopRunResult::Failed,
            LoopRunResult::CheckSkipped,
        ] {
            let mut notice = record(result, check(None, false));
            notice.watch =
                Some(crate::harness::schedule::signal::WatchVerdict::Running { elapsed_ms: 1_000 });
            assert_eq!(classify(&notice), Signal::Neutral);
            assert_eq!(note_in(dir.path(), key, classify(&notice)).unwrap(), 1);
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
        let signal = classify(&record(LoopRunResult::StartFailed, None));
        assert_eq!(signal, Signal::Neutral);
        let before = std::fs::read(path(dir.path())).expect("strikes before failed start");
        assert_eq!(note_in(dir.path(), KEY, signal).expect("neutral"), 2);
        assert_eq!(
            std::fs::read(path(dir.path())).expect("unchanged strikes"),
            before
        );
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
