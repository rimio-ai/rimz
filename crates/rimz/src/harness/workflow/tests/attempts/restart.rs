use super::*;
use crate::store::workflow::{Park, ParkReason};

#[test]
fn continued_restart_failure_uses_the_sealed_failure_policy() {
    for explicit in [false, true] {
        let text = STEP.replace("start='build'", "start='build'\non-break={agent='claude'}")
            + if explicit {
                "on.invalid='@failed'\n"
            } else {
                ""
            };
        let (_dir, state, record) = setup(&text);
        let host = FakeHost::new();
        let live = advance_ok(&state, &record.id, &host);
        let seq = live.attempt.as_ref().unwrap().seq;
        restart(
            &state,
            &record.id,
            Some((seq, "conversation".into())),
            &host,
        )
        .unwrap();
        *host.failure.borrow_mut() = Some(LaunchFailure {
            outcome: AttemptOutcome::Died,
            reason: "history pool changed".into(),
        });
        let failed = advance_ok(&state, &record.id, &host);
        assert_eq!(failed.visits["build"], 1);
        if explicit {
            assert_eq!(failed.state, RunState::Failed);
        } else {
            assert_eq!(failed.node, "@break");
            assert_eq!(failed.input.unwrap()["edge"], "invalid");
        }
    }
}

#[test]
fn restart_during_launch_keeps_manual_intent_until_the_replacement_launches() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    let callback_state = state.clone();
    let id = record.id.clone();
    let occupancy = host.occupancy.clone();
    *host.on_launch.borrow_mut() = Some(Box::new(move || {
        restart(&callback_state, &id, None, &FakeHost::new()).unwrap();
        *occupancy.borrow_mut() = Occupancy::Blocked(vec![]);
    }));
    let waiting = advance_ok(&state, &record.id, &host);
    assert!(
        waiting.restart.is_some(),
        "the old launch must not consume its replacement's manual intent: {waiting:#?}"
    );
    assert_eq!(waiting.wait.as_ref().unwrap().outcome, "takeover");
    host.on_launch.borrow_mut().take();
    *host.occupancy.borrow_mut() = Occupancy::Free;
    let live = advance_ok(&state, &record.id, &host);
    assert!(live.restart.is_none());
    assert_eq!(live.visits["build"], 1);
    assert_eq!(host.launched.borrow().len(), 2);
}

#[test]
fn restart_history_is_a_manual_reentry_not_a_resumed_conversation() {
    let definition = STEP.replace("then='@done'", "then='verify'")
        + "[script.verify]\nrun='true'\ntimeout='1m'\non.done='@done'\n";
    let (_dir, state, record) = setup(&definition);
    let spec = super::super::super::spec::parse(&definition).unwrap();
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    for _ in 0..2 {
        restart(&state, &record.id, None, &host).unwrap();
        advance_ok(&state, &record.id, &host);
    }
    let entries = records::journal(&state.workflow(&record.id)).unwrap();
    let trail = hops(&spec, &entries, None);
    assert!(
        trail
            .iter()
            .all(|hop| hop.target.as_ref().map(|target| target.as_str()) != Some("verify")),
        "{trail:#?}"
    );
    assert!(!reached(&trail, &entries).contains_key("verify"));
    let sender = routing_hop(&spec, &entries, "build", None);
    assert_eq!(
        (
            format!("{:?}", trail.last().unwrap().kind),
            sender.map(|hop| format!("{:?}", hop.kind)),
            reached(&trail, &entries)["build"]
        ),
        ("Restart".into(), Some("Restart".into()), 2)
    );
}

#[test]
fn restart_that_breaks_on_a_step_bound_can_be_restarted_again() {
    let (_dir, state, record) = setup(&on_break(STEP));
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    restart(&state, &record.id, None, &host).unwrap();
    let mut pending = records::load(&state.workflow(&record.id)).unwrap();
    pending.deadline = Some(jiff::Timestamp::now() - std::time::Duration::from_secs(2));
    update(&state, &pending);
    let broken = advance_ok(&state, &record.id, &host);
    assert_eq!(broken.node, "@break");
    assert_eq!(broken.input.as_ref().unwrap()["edge"], "deadline");
    assert!(broken.restart.is_none(), "{broken:#?}");
    restart(&state, &record.id, None, &host).unwrap();
    assert_eq!(
        advance_ok(&state, &record.id, &host).state,
        RunState::Active
    );
}

#[test]
fn restart_replaces_active_and_held_agents_and_resets_all_visits() {
    let mut actual = Vec::new();
    for held in [false, true] {
        let definition = until::UNTIL.replace("on.pending='build'", "on.pending='@wait'");
        let (dir, state, record) = setup(&definition);
        until::check(dir.path(), json!({"outcome":"pending"}));
        let host = FakeHost::new();
        let mut live = advance_ok(&state, &record.id, &host);
        if held {
            host.observed.borrow_mut().turn = Some(until::turn());
            live = advance_ok(&state, &record.id, &host);
            assert!(live.held.is_some());
        }
        live.input = Some(json!({"outcome":"pending", "key":"lint"}));
        live.key = Some(Key::String("lint".into()));
        *live.visits.get_mut("build").unwrap() = 20;
        update(&state, &live);
        let session = "continued-session".into();
        let seq = live.session_attempt().unwrap().seq;
        restart(&state, &record.id, Some((seq, session)), &host).unwrap();
        host.observed.borrow_mut().turn = None;
        let current = advance_ok(&state, &record.id, &host);
        let launches = host.launched.borrow();
        let journal = records::journal(&state.workflow(&record.id)).unwrap();
        actual.push((
            current.node.clone(),
            current.visits["build"],
            current.attempt.as_ref().and_then(|attempt| attempt.visit),
            current.input == live.input,
            current.key == live.key,
            launches.len(),
            launches
                .last()
                .unwrap()
                .resume
                .as_ref()
                .map(ToString::to_string),
            host.stops.get(),
            host.detaches.get(),
            journal
                .iter()
                .any(|entry| serde_json::to_value(entry).unwrap()["event"] == "restart"),
        ));
    }
    assert_eq!(
        actual,
        vec![
            (
                "build".into(),
                1,
                Some(1),
                true,
                true,
                2,
                Some("continued-session".into()),
                1,
                0,
                true
            );
            2
        ]
    );
}

#[test]
fn restart_unparks_cap_and_asked_steps_without_extending_bounds() {
    let mut actual = Vec::new();
    for reason in [ParkReason::Cap, ParkReason::Asked] {
        let (_dir, state, mut record) = setup(STEP);
        record.state = RunState::Parked;
        record.park = Some(Park {
            reason,
            at: record.entered_at,
            text: None,
        });
        record.visits.insert("build".into(), 7);
        update(&state, &record);
        let host = FakeHost::new();
        restart(&state, &record.id, None, &host).unwrap();
        let live = advance_ok(&state, &record.id, &host);
        actual.push((
            live.state,
            live.node,
            live.visits["build"],
            live.visit_grant,
            host.launched.borrow().len(),
            host.stops.get(),
        ));
    }
    assert_eq!(
        actual,
        vec![(RunState::Active, "build".into(), 1, 0, 1, 0); 2]
    );
}

#[test]
fn restart_refusals_and_stale_continue_stop_nothing() {
    let mut actual = Vec::new();
    for definition in [
        "[workflow]\nscope='worktree'\nstart='check'\n[script.check]\nrun='true'\ntimeout='1m'\non.done='@done'\n",
        "[workflow]\nscope='worktree'\nstart='build'\n[command.build]\nrun='true'\nthen='@done'\n",
        STEP,
    ] {
        let (_dir, state, mut record) = setup(definition);
        let host = FakeHost::new();
        if definition == STEP {
            record.state = RunState::Done;
            update(&state, &record);
        }
        let refused = restart(&state, &record.id, None, &host).is_err();
        actual.push((refused, host.stops.get(), host.launched.borrow().len()));
    }
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    actual.push((
        restart(&state, &record.id, Some((999, "old-session".into())), &host).is_err(),
        host.stops.get(),
        host.launched.borrow().len() - 1,
    ));
    assert_eq!(actual, vec![(true, 0, 0); 4]);
}

#[test]
fn cancel_during_restart_stop_wins_and_an_advance_cannot_relaunch_early() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    let stop_state = state.clone();
    let id = record.id.clone();
    *host.on_stop.borrow_mut() = Some(Box::new(move || {
        let other = FakeHost::new();
        advance(&stop_state, &id, &other).unwrap();
        assert!(other.launched.borrow().is_empty());
        cancel(&stop_state, &id, &other, None).unwrap();
    }));
    restart(&state, &record.id, None, &host).unwrap();
    let final_record = advance_ok(&state, &record.id, &host);
    assert_eq!(final_record.state, RunState::Canceled);
    assert_eq!(host.launched.borrow().len(), 1);
}

#[test]
fn restart_during_cancel_stop_cannot_replace_the_cancel() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    let stop_state = state.clone();
    let id = record.id.clone();
    *host.on_stop.borrow_mut() = Some(Box::new(move || {
        assert!(restart(&stop_state, &id, None, &FakeHost::new()).is_err());
    }));
    cancel(&state, &record.id, &host, None).unwrap();
    let final_record = advance_ok(&state, &record.id, &host);
    assert_eq!(final_record.state, RunState::Canceled);
    assert_eq!(host.launched.borrow().len(), 1);
}
