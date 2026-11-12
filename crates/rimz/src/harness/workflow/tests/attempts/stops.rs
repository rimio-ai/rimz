use super::*;
use crate::harness::workflow::{StopKind, Stopped};
use jiff::Timestamp;
use std::time::Duration;

#[test]
fn step_timeout_holds_for_limits_errors_and_budgets() {
    for kind in [StopKind::Limit, StopKind::Error, StopKind::Budget] {
        for seconds in [5400, 10800, 0] {
            let (_dir, state, record) = setup(&format!("{STEP}timeout='1h'\n"));
            let host = FakeHost::new();
            let mut live = advance_ok(&state, &record.id, &host);
            let now = Timestamp::now();
            let start = now - Duration::from_secs(7200);
            let since = if seconds == 0 {
                now + Duration::from_secs(60)
            } else {
                now - Duration::from_secs(seconds)
            };
            live.attempt.as_mut().unwrap().started_at = start;
            update(&state, &live);
            host.observed.borrow_mut().stopped = Some(Stopped { since, kind });
            let held = advance_ok(&state, &record.id, &host);
            assert_eq!(held.state, RunState::Active, "{kind:?}");
            let attempt = held.attempt.as_ref().unwrap();
            let opened = attempt.held_since.unwrap();
            if seconds == 0 {
                assert!(now <= opened && opened <= Timestamp::now());
            } else {
                assert_eq!(opened, since.max(start));
            }
            assert_eq!(attempt.asked_ms, 0);
            assert!(
                timeout_at(&load(&state.workflow(&record.id).home).unwrap(), &held)
                    .unwrap()
                    .is_none()
            );
            host.observed.borrow_mut().stopped = None;
            let earliest = Timestamp::now();
            let resumed = advance_ok(&state, &record.id, &host);
            if seconds == 0 {
                assert_eq!(
                    resumed.prev_attempt.unwrap().outcome,
                    AttemptOutcome::Timeout
                );
            } else {
                let credit = i128::from(resumed.attempt.as_ref().unwrap().asked_ms);
                assert!(earliest.duration_since(opened).as_millis() <= credit);
                assert!(credit <= Timestamp::now().duration_since(opened).as_millis());
                let again = advance_ok(&state, &record.id, &host);
                assert_eq!(again.attempt.unwrap().asked_ms, credit as u64);
                assert_eq!(host.stops.get(), 0);
            }
        }
    }
}

#[test]
fn overlapping_ask_and_limit_credit_one_hold_from_the_earliest_evidence() {
    let (_dir, state, record) = setup(&format!("{STEP}timeout='1h'\n"));
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    let now = Timestamp::now();
    live.attempt.as_mut().unwrap().started_at = now - Duration::from_secs(7200);
    update(&state, &live);
    let since = now - Duration::from_secs(6000);
    {
        let mut observed = host.observed.borrow_mut();
        observed.ask = Some(records::Asking {
            since: now - Duration::from_secs(5400),
            ask: records::Asked {
                text: "Approve?".into(),
                kind: "permission".into(),
                handle: "@child".into(),
            },
        });
        observed.stopped = Some(Stopped {
            since,
            kind: StopKind::Limit,
        });
    }
    let held = advance_ok(&state, &record.id, &host);
    assert_eq!(held.attempt.unwrap().held_since, Some(since));
    host.observed.borrow_mut().ask = None;
    let held = advance_ok(&state, &record.id, &host);
    assert_eq!(held.attempt.as_ref().unwrap().held_since, Some(since));
    assert_eq!(held.attempt.unwrap().asked_ms, 0);
    host.observed.borrow_mut().stopped = None;
    let earliest = Timestamp::now();
    let resumed = advance_ok(&state, &record.id, &host);
    let attempt = resumed.attempt.unwrap();
    assert!(attempt.held_since.is_none());
    assert!(i128::from(attempt.asked_ms) >= earliest.duration_since(since).as_millis());
    assert!(i128::from(attempt.asked_ms) <= Timestamp::now().duration_since(since).as_millis());
}

#[test]
fn legacy_open_ask_opens_the_shared_hold_without_losing_its_start() {
    let (_dir, state, record) = setup(&format!("{STEP}timeout='1h'\n"));
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    let now = Timestamp::now();
    let since = now - Duration::from_secs(5400);
    let asking = records::Asking {
        since,
        ask: records::Asked {
            text: "Approve?".into(),
            kind: "permission".into(),
            handle: "@child".into(),
        },
    };
    let mut value = serde_json::to_value(live.attempt.take().unwrap()).unwrap();
    value["started_at"] = json!(now - Duration::from_secs(7200));
    value["asking"] = json!(asking);
    value.as_object_mut().unwrap().remove("held_since");
    live.attempt = Some(serde_json::from_value(value).unwrap());
    update(&state, &live);
    let spec = load(&state.workflow(&record.id).home).unwrap();
    assert!(timeout_at(&spec, &live).unwrap().is_none());
    host.observed.borrow_mut().ask = Some(asking);
    let held = advance_ok(&state, &record.id, &host);
    assert_eq!(held.attempt.unwrap().held_since, Some(since));
    host.observed.borrow_mut().ask = None;
    let resumed = advance_ok(&state, &record.id, &host);
    assert_eq!(resumed.state, RunState::Active);
    assert!(resumed.attempt.unwrap().asked_ms >= 5_400_000);
}

#[test]
fn interrupt_and_silence_do_not_hold_the_step_timeout() {
    for kind in [None, Some(StopKind::Interrupt)] {
        let (_dir, state, record) = setup(&format!("{STEP}timeout='1h'\n"));
        let host = FakeHost::new();
        let mut live = advance_ok(&state, &record.id, &host);
        live.attempt.as_mut().unwrap().started_at = Timestamp::now() - Duration::from_secs(7200);
        update(&state, &live);
        host.observed.borrow_mut().stopped = kind.map(|kind| Stopped {
            since: Timestamp::now() - Duration::from_secs(5400),
            kind,
        });
        let ended = advance_ok(&state, &record.id, &host);
        assert_eq!(ended.prev_attempt.unwrap().outcome, AttemptOutcome::Timeout);
        assert_eq!(host.stops.get(), 1);
    }
}
