use super::*;

const REASON: &str = "fleet budget $5.00/day is spent";

fn budget_wait(record: &RunRecord) -> Option<(&str, Option<&str>)> {
    record
        .wait
        .as_ref()
        .map(|wait| (wait.outcome.as_str(), wait.detail.as_deref()))
}

fn budget_waits(state: &StatePaths, record: &RunRecord) -> usize {
    records::journal(&state.workflow(&record.id))
        .unwrap()
        .iter()
        .filter(|entry| {
            matches!(&entry.event, JournalEvent::Wait { outcome, .. } if outcome == "budget")
        })
        .count()
}

/// A step the budget gate refuses waits before it counts a visit or reads and stops its
/// checkout's occupants, and launches at the advance after the gate admits it.
#[test]
fn a_refused_step_waits_on_its_budget_before_any_visit_or_occupancy_read() {
    let (_dir, state, record) = setup(&format!("{STEP}admission.occupancy='takeover'\n"));
    let host = FakeHost::new();
    *host.occupancy.borrow_mut() = Occupancy::Occupied(vec!["sweeper".into()]);
    *host.budget.borrow_mut() = Some(REASON.into());
    let waiting = advance_ok(&state, &record.id, &host);
    assert_eq!(waiting.state, RunState::Active, "{waiting:#?}");
    assert_eq!(budget_wait(&waiting), Some(("budget", Some(REASON))));
    assert!(
        waiting.attempt.is_none() && waiting.prev_attempt.is_none() && waiting.visits.is_empty(),
        "{waiting:#?}"
    );
    assert_eq!(
        (
            host.launched.borrow().len(),
            host.occupancies.get(),
            host.evictions.get()
        ),
        (0, 0, 0)
    );
    host.budget.borrow_mut().take();
    let live = advance_ok(&state, &record.id, &host);
    assert_eq!(host.launched.borrow().len(), 1);
    assert!(live.wait.is_none(), "the budget wait closes at the launch");
    assert_eq!(live.visits["build"], 1);
    assert_eq!(budget_waits(&state, &record), 1);
}

/// The launcher's own gate refusing after the first read admitted the launch withdraws the
/// attempt and its visit and waits, every time it refuses, with no failure route taken.
#[test]
fn a_launch_the_launcher_refuses_waits_each_time_with_its_visit_refunded() {
    let (_dir, state, mut record) = setup(STEP);
    record.key = Some(Key::Integer(7));
    update(&state, &record);
    let host = FakeHost::new();
    *host.launch_budget.borrow_mut() = Some(REASON.into());
    // Past `max-visits=2`, so a visit that stayed counted would trip the step's bound.
    for count in 1..=3 {
        let waiting = advance_ok(&state, &record.id, &host);
        assert_eq!(
            (waiting.state, waiting.node.as_str()),
            (RunState::Active, "build"),
            "{waiting:#?}"
        );
        assert_eq!(budget_wait(&waiting), Some(("budget", Some(REASON))));
        // The launch closed the wait it passed, so each refusal opens one again.
        assert_eq!(budget_waits(&state, &record), count - 1);
        assert!(
            waiting.attempt.is_none()
                && waiting.prev_attempt.is_none()
                && waiting.visits.is_empty()
                && waiting.key_visits.is_empty(),
            "{waiting:#?}"
        );
    }
    host.launch_budget.borrow_mut().take();
    let live = advance_ok(&state, &record.id, &host);
    assert_eq!(host.launched.borrow().len(), 1);
    assert!(live.wait.is_none() && live.attempt.is_some(), "{live:#?}");
    assert_eq!(live.visits["build"], 1);
}

/// A restart that continues the conversation keeps it across a launcher refusal, and a kept
/// step takes the same wait, so the launch after the wait is the restart's.
#[test]
fn a_refused_restart_keeps_its_conversation_for_the_launch_after_the_wait() {
    for keep in [false, true] {
        let (_dir, state, record) =
            setup(&STEP.replace("max-visits=2", &format!("max-visits=2\nkeep={keep}")));
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
        *host.launch_budget.borrow_mut() = Some(REASON.into());
        let waiting = advance_ok(&state, &record.id, &host);
        assert_eq!(
            budget_wait(&waiting),
            Some(("budget", Some(REASON))),
            "keep={keep}"
        );
        assert!(
            waiting
                .restart
                .as_ref()
                .is_some_and(|restart| restart.resume.is_some()),
            "keep={keep}: {waiting:#?}"
        );
        host.launch_budget.borrow_mut().take();
        advance_ok(&state, &record.id, &host);
        let launches = host.launched.borrow();
        assert_eq!(launches.len(), 2, "keep={keep}");
        assert_eq!(
            launches[1].resume.as_ref().map(ToString::to_string),
            Some("conversation".into()),
            "keep={keep}"
        );
    }
}

/// Every way into a step's launch waits at a refusal: a stored skip passes capacity
/// admission only.
#[test]
fn every_origin_and_the_stored_skip_wait_at_a_budget_refusal() {
    let definition = STEP.to_owned()
        + "[agent.other]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompt.md'\nthen='@done'\n";
    for origin in ["hand", "skip", "restart", "resume"] {
        let (_dir, state, mut record) = setup(&definition);
        if origin == "skip" {
            record.capacity = records::StartCapacity::Skip;
            update(&state, &record);
        }
        let host = FakeHost::new();
        if origin != "hand" && origin != "skip" {
            advance_ok(&state, &record.id, &host);
            if origin == "restart" {
                restart(&state, &record.id, None, &host).unwrap();
            } else {
                resume(&state, &record.id, &host, Some("other"), &[]).unwrap();
            }
        }
        let launches = host.launched.borrow().len();
        *host.budget.borrow_mut() = Some(REASON.into());
        let waiting = advance_ok(&state, &record.id, &host);
        assert_eq!(
            budget_wait(&waiting),
            Some(("budget", Some(REASON))),
            "{origin}: {waiting:#?}"
        );
        assert_eq!(host.launched.borrow().len(), launches, "{origin}");
    }
}

/// The run's deadline ends a budget wait, and a capacity give-up never does.
#[test]
fn the_deadline_ends_a_budget_wait_and_the_capacity_give_up_does_not() {
    let (_dir, state, record) = setup(&format!("{STEP}admission.give-up='1m'\n"));
    let host = FakeHost::new();
    *host.budget.borrow_mut() = Some(REASON.into());
    let mut waiting = advance_ok(&state, &record.id, &host);
    let wait = waiting.wait.as_mut().unwrap();
    wait.first -= jiff::SignedDuration::from_hours(1);
    update(&state, &waiting);
    let still = advance_ok(&state, &record.id, &host);
    assert_eq!(still.state, RunState::Active, "{still:#?}");
    assert_eq!(budget_wait(&still), Some(("budget", Some(REASON))));
    let mut late = still;
    late.deadline = Some(Timestamp::now() - std::time::Duration::from_secs(1));
    update(&state, &late);
    let ended = advance_ok(&state, &record.id, &host);
    assert!(ended.deadline_fired, "{ended:#?}");
    assert_eq!(ended.state, RunState::Failed);
    assert!(host.launched.borrow().is_empty());
}

/// A cancel or a misfit staged while the launch runs off the lock wins over the launcher's
/// refusal, budget or taken checkout alike, and settles as staged. (`resume --at` refuses an
/// attempt still launching, so it cannot stage one.)
#[test]
fn an_ending_staged_during_a_refused_launch_settles_instead_of_waiting() {
    let definition = STEP.to_owned()
        + "[agent.other]\nagent='claude'\nlifecycle='turn'\nprompt-file='prompt.md'\nthen='@done'\n";
    let edited = definition
        .replace("[agent.build]", "[agent.renamed]")
        .replace("start='build'", "start='renamed'");
    for (refusal, staged) in [
        ("budget", "cancel"),
        ("budget", "misfit"),
        ("taken", "cancel"),
    ] {
        let (_dir, state, record) = setup(&definition);
        let host = FakeHost::new();
        if refusal == "budget" {
            *host.launch_budget.borrow_mut() = Some(REASON.into());
        } else {
            *host.arrives_at_launch.borrow_mut() =
                Some(Occupancy::Occupied(vec!["sweeper".into()]));
        }
        let fired = Rc::new(std::cell::Cell::new(false));
        let (callback_state, id, once) = (state.clone(), record.id.clone(), fired.clone());
        let (spec_file, edited) = (definition_file(&record), edited.clone());
        *host.on_launch.borrow_mut() = Some(Box::new(move || {
            if once.replace(true) {
                return;
            }
            if staged == "cancel" {
                cancel(&callback_state, &id, &FakeHost::new(), None).unwrap();
            } else {
                fs::write(&spec_file, &edited).unwrap();
                advance(&callback_state, &id, &FakeHost::new()).unwrap();
            }
        }));
        let settled = advance_ok(&state, &record.id, &host);
        let case = format!("{refusal} {staged}: {settled:#?}");
        assert!(fired.get(), "{case}");
        if staged == "cancel" {
            assert_eq!(settled.state, RunState::Canceled, "{case}");
            assert!(settled.wait.is_none(), "{case}");
        } else {
            assert_eq!(settled.state, RunState::Failed, "{case}");
        }
        let journal = records::journal(&state.workflow(&record.id)).unwrap();
        assert!(
            !journal
                .iter()
                .any(|entry| entry.node == "build"
                    && matches!(entry.event, JournalEvent::Wait { .. }))
                && settled
                    .wait
                    .as_ref()
                    .is_none_or(|wait| wait.node != "build"),
            "{case}"
        );
    }
}

/// A script's own `budget` answer routed to `@wait` is an ordinary wait: a second answer
/// merges into it rather than being closed as a judge's budget wait would be.
#[test]
fn a_scripts_budget_answer_waits_like_any_other_answer() {
    let (_dir, state, record) = setup(&OUTCOME_STEP.replace(
        "on={red='build',blue='build',queued='@wait'}",
        "on={red='build',blue='build',queued='@wait',budget='@wait'}",
    ));
    let host = FakeHost::new();
    let first = answer(&state, &record, &host, json!({"outcome":"budget"}));
    let opened = first.wait.as_ref().unwrap().first;
    let again = answer(&state, &record, &host, json!({"outcome":"budget"}));
    let wait = again.wait.as_ref().unwrap();
    assert_eq!(
        (wait.outcome.as_str(), wait.count, wait.first),
        ("budget", 2, opened),
        "{again:#?}"
    );
}
