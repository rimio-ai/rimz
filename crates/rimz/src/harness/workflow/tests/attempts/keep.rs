use super::*;
use crate::store::workflow::{EndCause, Ending, ParkReason, Verdict};

#[test]
fn keep_launch_note_explains_that_the_pane_will_remain_outside_the_run() {
    let mut notes = Vec::new();
    for lifecycle in [
        "lifecycle='turn'",
        "lifecycle='session'",
        "lifecycle='session'\nrest-check=['pr']",
    ] {
        let definition = rest::SESSION.replace(
            "lifecycle='session'\nrest-check=['pr']",
            &format!("{lifecycle}\nkeep=true"),
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
    for (lifecycle, keep) in [
        ("turn", false),
        ("turn", true),
        ("session", false),
        ("session", true),
    ] {
        for (cause, capped) in [
            (EndCause::Lifecycle, false),
            (EndCause::Lifecycle, true),
            (EndCause::Timeout, false),
            (EndCause::Deadline, false),
            (EndCause::Cancel, false),
        ] {
            let definition = STEP
                .replace("lifecycle='turn'", &format!("lifecycle='{lifecycle}'"))
                .replace("max-visits=2", &format!("max-visits=2\nkeep={keep}"));
            let (_dir, state, record) = setup(&definition);
            let host = FakeHost::new();
            let mut live = advance_ok(&state, &record.id, &host);
            live.attempt.as_mut().unwrap().ending = Some(Ending {
                cause,
                outcome: AttemptOutcome::Ended,
                route: capped.then(|| Verdict {
                    node: "build".into(),
                    entered_at: live.entered_at,
                    at: live.entered_at,
                    target: "@done".into(),
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
    let unkept = [(false, 1, 0); 5];
    let kept = [
        (true, 0, 1),
        (true, 0, 1),
        (true, 1, 0),
        (true, 1, 0),
        (true, 1, 0),
    ];
    assert_eq!(actual, [unkept, kept, unkept, kept].concat());
}

#[test]
fn kept_rest_check_close_and_cap_endings_detach() {
    let mut actual = Vec::new();
    for (outcome, cap, edge) in [
        ("done", 2, ""),
        ("pending", 1, "on.visits='@done'\n"),
        ("pending", 1, ""),
    ] {
        let definition = rest::SESSION.replace(
            "max-visits=2",
            &format!("max-visits={cap}\nkeep=true\n{edge}"),
        );
        let (dir, state, record) = setup(&definition);
        let host = FakeHost::new();
        advance_ok(&state, &record.id, &host);
        rest::check(dir.path(), json!({"outcome":outcome}));
        host.observed.borrow_mut().turn = Some(rest::turn());
        let ended = advance_ok(&state, &record.id, &host);
        actual.push((ended.state, host.stops.get(), host.detaches.get()));
    }
    assert_eq!(
        actual,
        [
            (RunState::Done, 0, 1),
            (RunState::Done, 0, 1),
            (RunState::Failed, 0, 1),
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

#[test]
fn a_session_question_takes_on_asked_and_ends_the_node_as_a_turns_does() {
    use crate::store::workflow::{Asked, Asking};
    let mut actual = Vec::new();
    for (lifecycle, keep) in [
        ("lifecycle='turn'", false),
        ("lifecycle='session'", false),
        ("lifecycle='session'", true),
        ("lifecycle='session'\nrest-check=['pr']", false),
    ] {
        let definition = rest::SESSION.replace(
            "lifecycle='session'\nrest-check=['pr']",
            &format!("{lifecycle}\nkeep={keep}\non.asked='@done'"),
        );
        let (_dir, state, record) = setup(&definition);
        let host = FakeHost::new();
        advance_ok(&state, &record.id, &host);
        host.observed.borrow_mut().ask = Some(Asking {
            since: jiff::Timestamp::now(),
            ask: Asked {
                text: "Which approach?".into(),
                kind: "question".into(),
                handle: "@builder".into(),
            },
        });
        let ended = advance_ok(&state, &record.id, &host);
        actual.push((
            ended.state,
            ended.prev_attempt.map(|attempt| attempt.outcome),
            host.stops.get(),
            host.detaches.get(),
        ));
    }
    let asked = Some(AttemptOutcome::Asked);
    assert_eq!(
        actual,
        [
            (RunState::Done, asked, 1, 0),
            (RunState::Done, asked, 1, 0),
            (RunState::Done, asked, 0, 1),
            (RunState::Done, asked, 1, 0),
        ]
    );
}

#[test]
fn a_kept_agent_that_died_hit_a_limit_or_was_canceled_is_stopped() {
    let mut actual = Vec::new();
    for (lifecycle, held) in [
        ("lifecycle='turn'", false),
        ("lifecycle='session'", false),
        ("lifecycle='session'\nrest-check=['pr']", false),
        ("lifecycle='session'\nrest-check=['pr']", true),
    ] {
        for status in [
            RunStatus::Failed,
            RunStatus::BudgetExceeded,
            RunStatus::Canceled,
            RunStatus::Completed,
        ] {
            let definition = rest::SESSION.replace(
                "lifecycle='session'\nrest-check=['pr']",
                &format!("{lifecycle}\nkeep=true"),
            );
            let (_dir, state, record) = setup(&definition);
            let host = FakeHost::new();
            let live = advance_ok(&state, &record.id, &host);
            if held {
                update(&state, &rest::held(&live, false));
            }
            host.observed.borrow_mut().status = status;
            advance_ok(&state, &record.id, &host);
            actual.push((host.stops.get(), host.detaches.get()));
        }
    }
    let per_shape = [(1, 0), (1, 0), (1, 0), (0, 1)];
    assert_eq!(actual, [per_shape; 4].concat());
}
