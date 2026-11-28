use super::*;
use crate::harness::schedule::task_record::Scope;
use crate::ids::TaskId;

fn outcome(fire: &FireId, result: LoopRunResult, signal: Signal) -> Outcome<'_> {
    Outcome {
        fire,
        result,
        signal,
        quiet: None,
        ends_streak: false,
        strike_out: Some((2, TaskSource::Config)),
        now: Timestamp::UNIX_EPOCH,
    }
}

fn booker() -> crate::agents::LaunchedBy {
    crate::agents::LaunchedBy {
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        agent_id: "booker".into(),
    }
}

fn opened(task: &TaskId) -> (TaskRecord, FireId) {
    let fire = FireId::new(task);
    let mut record = TaskRecord::new(Binding {
        scope: Scope::Machine,
        name: "nightly".to_owned(),
    });
    record.marks.insert(
        "/checkout".into(),
        FireMark {
            fire: fire.clone(),
            at: Timestamp::UNIX_EPOCH,
            booked_by: vec![booker()],
            on: Some(crate::config::CheckOn::Fail),
            report: None,
            once: true,
            run: None,
            scope: crate::store::workflow::ScopeKind::Worktree,
            result: None,
            kind: None,
            target: None,
            manual: false,
            opener: None,
            child: None,
        },
    );
    (record, fire)
}

#[test]
fn a_report_closes_the_mark_and_moves_strikes_once_per_fire() {
    let task = TaskId::new();
    let (mut record, fire) = opened(&task);
    let settled = settle_record(
        &mut record,
        outcome(&fire, LoopRunResult::Failed, Signal::Strike),
    );
    assert_eq!(
        settled,
        Settled {
            count: 1,
            counted: true,
            disabled: false,
            booker: super::super::run_log::Booker::of(
                &[booker()],
                Some(crate::config::CheckOn::Fail),
                record.marks[Path::new("/checkout")]
                    .report
                    .clone()
                    .unwrap_or_default(),
            ),
        }
    );
    assert_eq!(
        record.marks[Path::new("/checkout")].result,
        Some(LoopRunResult::Failed)
    );
    let after = record.clone();
    let again = settle_record(
        &mut record,
        outcome(&fire, LoopRunResult::Failed, Signal::Strike),
    );
    assert!(!again.counted);
    assert_eq!(
        again.booker, settled.booker,
        "a second report names the booker under the same message, which the queue takes once"
    );
    assert_eq!(record, after, "a second report changes nothing");

    let second = FireId::new(&task);
    let settled = settle_record(
        &mut record,
        outcome(&second, LoopRunResult::Failed, Signal::Strike),
    );
    assert!(settled.disabled, "the second strike reaches the threshold");
    assert_eq!(
        record.arming.map(|arming| (arming.enabled, arming.strikes)),
        Some((false, Some(2)))
    );
}

#[test]
fn a_late_quiet_collection_counts_but_keeps_the_newer_last_fire() {
    let mut quiet = None;
    for second in [20, 10] {
        let mut last = LoopRunRecord::new(
            "task",
            LoopRunResult::Completed,
            super::super::run_log::LoopRunMode::Scheduled,
            0,
        );
        last.at = Timestamp::from_second(second).unwrap();
        note_quiet(&mut quiet, last);
    }
    let quiet = quiet.unwrap();
    assert_eq!(
        (quiet.count, quiet.streak, quiet.last.at.as_second()),
        (2, 2, 20)
    );
}
