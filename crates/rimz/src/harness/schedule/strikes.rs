//! Machine-local consecutive failure counts for loop tasks.
//!
//! Counts survive process exits and reset independently of run-log rotation.
//! They live on the task's record, so `task_record_store`'s lock serializes
//! updates from different task runners.

use std::collections::BTreeMap;

use super::task_record::{self, Binding, TaskRecord, TaskRecordErr};
use super::task_record_store::{self, TaskStoreErr};
use crate::config::TaskEntry;
use crate::harness::schedule::run_log::{LoopRunRecord, LoopRunResult};
use crate::ids::{FireId, TaskId};

/// How many fire ids a count remembers having taken a signal from.
const COUNTED_FIRES: usize = 64;
pub(super) const DEFAULT_MAX_STRIKES: u32 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Signal {
    Strike,
    Reset,
    Neutral,
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(super) struct StrikesError(#[from] TaskStoreErr);

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

/// Every bound task's nonzero count, by binding key.
pub fn load() -> std::result::Result<BTreeMap<String, u32>, TaskRecordErr> {
    Ok(task_record::load()?
        .by_key()
        .into_iter()
        .filter(|(_, (_, record))| record.strikes > 0)
        .map(|(key, (_, record))| (key, record.strikes))
        .collect())
}

/// Move the count of a row's signal that no fire id names (a skip, a start failure), on the
/// record of the row's task id, retired or not, so a row written after its task retired
/// binds no new record; a row with no id moves the record bound to `binding`.
pub(super) fn note(binding: &Binding, task: Option<&TaskId>, signal: Signal) -> Result<u32> {
    if signal == Signal::Neutral {
        let records = task_record::load().map_err(TaskStoreErr::from)?;
        let record = match task {
            Some(task) => records.get(task),
            None => records.live(&binding.key()).map(|(_, record)| record),
        };
        return Ok(record.map_or(0, |record| record.strikes));
    }
    Ok(task_record_store::mutate(|records| {
        let record = match task {
            Some(task) => records.get_mut(task),
            None => Some(records.bind(binding).1),
        };
        record.map_or(0, |record| apply(record, None, signal).0)
    })?)
}

/// Clear the count, as `loop enable` does; the task's fire record stays.
pub(super) fn clear(binding: &Binding) -> Result<bool> {
    Ok(task_record_store::mutate(|records| {
        let Some(record) = records.live_mut(&binding.key()) else {
            return false;
        };
        let cleared = record.strikes != 0 || !record.counted.is_empty();
        record.strikes = 0;
        record.counted.clear();
        cleared
    })?)
}

/// Move `record`'s count by `signal`, at most once per fire id; the flag says whether it
/// counted. The fire id is kept even for a neutral signal, so a second report of the fire
/// changes nothing.
pub(super) fn apply(record: &mut TaskRecord, fire: Option<&FireId>, signal: Signal) -> (u32, bool) {
    if let Some(fire) = fire {
        if record.counted.contains(fire) {
            return (record.strikes, false);
        }
        record.counted.push(fire.clone());
        let excess = record.counted.len().saturating_sub(COUNTED_FIRES);
        record.counted.drain(..excess);
    }
    match signal {
        Signal::Strike => record.strikes = record.strikes.saturating_add(1),
        Signal::Reset => record.strikes = 0,
        Signal::Neutral => {}
    }
    (record.strikes, true)
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::harness::schedule::run_log::{CheckRecord, LoopRunMode};

    fn record(result: LoopRunResult, check: Option<CheckRecord>) -> LoopRunRecord {
        LoopRunRecord {
            task_id: None,
            fire: None,
            kind: None,
            judge: Vec::new(),
            checkout: None,
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

    fn task() -> TaskRecord {
        TaskRecord::new(Binding {
            scope: super::super::task_record::Scope::Machine,
            name: "nightly".to_owned(),
        })
    }

    #[test]
    fn a_fire_reported_twice_counts_once() {
        let id = crate::ids::TaskId::new();
        let (first, second) = (FireId::new(&id), FireId::new(&id));
        let mut record = task();
        assert_eq!(apply(&mut record, Some(&first), Signal::Strike), (1, true));
        assert_eq!(apply(&mut record, Some(&first), Signal::Strike), (1, false));
        assert_eq!(apply(&mut record, Some(&second), Signal::Strike), (2, true));
        assert_eq!(apply(&mut record, Some(&second), Signal::Reset), (2, false));
    }

    #[test]
    fn running_watch_notice_is_strike_neutral() {
        let mut record = task();
        apply(&mut record, None, Signal::Strike);
        for result in [
            LoopRunResult::Delivered,
            LoopRunResult::Failed,
            LoopRunResult::CheckSkipped,
        ] {
            let mut notice = self::record(result, check(None, false));
            notice.watch =
                Some(crate::harness::signal::WatchVerdict::Running { elapsed_ms: 1_000 });
            assert_eq!(classify(&notice), Signal::Neutral);
            assert_eq!(apply(&mut record, None, classify(&notice)).0, 1);
        }
    }

    #[test]
    fn counter_counts_and_resets() {
        let mut record = task();
        assert_eq!(apply(&mut record, None, Signal::Strike).0, 1);
        assert_eq!(apply(&mut record, None, Signal::Strike).0, 2);
        let signal = classify(&self::record(LoopRunResult::StartFailed, None));
        assert_eq!(signal, Signal::Neutral);
        assert_eq!(apply(&mut record, None, signal).0, 2);
        assert_eq!(apply(&mut record, None, Signal::Reset).0, 0);
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
