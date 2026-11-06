use super::*;
use crate::store::workflow::{EndCause, Ending, ParkReason, Verdict};

#[test]
fn keep_launch_note_explains_that_the_pane_will_remain_outside_the_run() {
    let mut notes = Vec::new();
    for lifecycle in ["turn", "until"] {
        let definition = until::UNTIL.replace(
            "lifecycle='until'",
            &format!("lifecycle='{lifecycle}'\nkeep=true"),
        );
        let (_dir, state, record) = setup(&definition);
        let host = FakeHost::new();
        advance_ok(&state, &record.id, &host);
        notes.push(host.launched.borrow()[0].workflow_note.clone());
    }
    assert!(
        notes.iter().all(
            |note| note.contains("leaves your pane open outside this run")
                && !note.contains("closes your pane")
        ),
        "{notes:#?}"
    );
}

#[test]
fn keep_launches_without_cleanup_and_detaches_only_non_forced_endings() {
    let mut actual = Vec::new();
    for keep in [false, true] {
        for (cause, cap_park) in [
            (EndCause::Lifecycle, None),
            (EndCause::Lifecycle, Some(false)),
            (EndCause::Lifecycle, Some(true)),
            (EndCause::Timeout, None),
            (EndCause::Deadline, None),
            (EndCause::Cancel, None),
        ] {
            let definition = STEP.replace("max-visits=2", &format!("max-visits=2\nkeep={keep}"));
            let (_dir, state, record) = setup(&definition);
            let host = FakeHost::new();
            let mut live = advance_ok(&state, &record.id, &host);
            live.attempt.as_mut().unwrap().ending = Some(Ending {
                cause,
                outcome: AttemptOutcome::Ended,
                route: cap_park.map(|park| Verdict {
                    node: "build".into(),
                    entered_at: live.entered_at,
                    at: None,
                    target: if park { "@user" } else { "@done" }.into(),
                    object: json!({"outcome":"cap", "step":"build"}),
                    reason: ParkReason::Cap,
                }),
                reason: None,
            });
            live.attempt.as_mut().unwrap().host.pid = u32::MAX;
            update(&state, &live);
            advance_ok(&state, &record.id, &host);
            actual.push((
                host.launched.borrow()[0].keep,
                host.stops.get(),
                host.detaches.get(),
            ));
        }
    }
    assert_eq!(
        actual,
        [
            (false, 1, 0),
            (false, 1, 0),
            (false, 1, 0),
            (false, 1, 0),
            (false, 1, 0),
            (false, 1, 0),
            (true, 0, 1),
            (true, 0, 1),
            (true, 1, 0),
            (true, 1, 0),
            (true, 1, 0),
            (true, 1, 0),
        ]
    );
}

#[test]
fn kept_until_close_detaches_and_cap_parks_still_stop() {
    let mut actual = Vec::new();
    for (outcome, cap, edge) in [
        ("done", 2, ""),
        ("pending", 1, "on.cap='@done'\n"),
        ("pending", 1, ""),
    ] {
        let definition = until::UNTIL.replace(
            "max-visits=2",
            &format!("max-visits={cap}\nkeep=true\n{edge}"),
        );
        let (dir, state, record) = setup(&definition);
        let host = FakeHost::new();
        advance_ok(&state, &record.id, &host);
        until::check(dir.path(), json!({"outcome":outcome}));
        host.observed.borrow_mut().turn = Some(until::turn());
        let ended = advance_ok(&state, &record.id, &host);
        actual.push((ended.state, host.stops.get(), host.detaches.get()));
    }
    assert_eq!(
        actual,
        [
            (RunState::Done, 0, 1),
            (RunState::Done, 0, 1),
            (RunState::Parked, 1, 0),
        ]
    );
}

#[test]
fn cancel_during_detach_still_stops_the_kept_agent() {
    let (_dir, state, record) = setup(&STEP.replace("max-visits=2", "max-visits=2\nkeep=true"));
    let host = Rc::new(FakeHost::new());
    advance_ok(&state, &record.id, &host);
    let concurrent = host.clone();
    let concurrent_state = state.clone();
    let id = record.id.clone();
    *host.on_detach.borrow_mut() = Some(Box::new(move || {
        cancel(&concurrent_state, &id, concurrent.as_ref(), None).unwrap();
    }));
    host.observed.borrow_mut().status = RunStatus::Completed;
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(
        (ended.state, host.stops.get(), host.detaches.get()),
        (RunState::Canceled, 1, 1)
    );
}
