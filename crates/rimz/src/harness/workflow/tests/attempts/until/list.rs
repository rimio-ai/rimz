use super::*;
use std::time::Duration;

fn definition() -> String {
    UNTIL
        .replace("then='pr'", "then=['pr','review']")
        .replace("max-visits=2", "max-visits=3")
        .replace("on.done='@done'", "on.done='review'")
        + "[checks.review]\nrun='cat review.json'\nwake=['review.ready']\ntimeout='1h'\non={pending='build',waiting='@wait',done={to='@done',delay='30m'}}\n"
}

fn launched() -> (tempfile::TempDir, StatePaths, RunRecord, FakeHost) {
    let text = definition();
    let (dir, state, record) = setup(&text.replace("then=['pr','review']", "then='pr'"));
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    fs::write(&state.workflow(&record.id).spec, text).unwrap();
    check(
        dir.path(),
        json!({"outcome":"done","key":"must not replace input"}),
    );
    host.observed.borrow_mut().turn = Some(turn());
    (dir, state, live, host)
}

#[test]
fn sealed_then_accepts_a_name_or_a_list_with_the_same_entry() {
    let (_dir, state, record) = setup(UNTIL);
    let paths = state.workflow(&record.id);
    let scalar = crate::harness::workflow::load_run(&paths.home, &record).unwrap();
    assert_eq!(scalar.engine_target("build", "then"), "pr");
    fs::write(&paths.spec, definition()).unwrap();
    let listed = crate::harness::workflow::load_run(&paths.home, &record).unwrap();
    assert_eq!(listed.engine_target("build", "then"), "pr");
}

#[test]
fn listed_checks_keep_the_session_and_input_then_push_and_restart_at_the_first() {
    let (dir, state, live, host) = launched();
    fs::write(dir.path().join("review.json"), r#"{"outcome":"pending"}"#).unwrap();
    let pushed = advance_ok(&state, &live.id, &host);
    assert_eq!(pushed.node, "build");
    assert_eq!(pushed.visits["build"].total, 2);
    assert_eq!(
        pushed.attempt.as_ref().unwrap().run,
        live.attempt.as_ref().unwrap().run
    );
    let context: Value = serde_json::from_slice(
        &fs::read(state.workflow(&live.id).work.join("check-view.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(context["input"], Value::Null);
    let rows = decisions(&state, &pushed);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (&rows[0]["action"], &rows[0]["check"], &rows[0]["target"]),
        (&json!("kept"), &json!("pr"), &json!("review"))
    );
    assert_eq!(
        (&rows[1]["action"], &rows[1]["check"]),
        (&json!("pushed"), &json!("review"))
    );
    let spec = load(&state.workflow(&live.id).home).unwrap();
    let journal = records::journal(&state.workflow(&live.id)).unwrap();
    assert!(
        hops(&spec, &journal, None)
            .iter()
            .any(|hop| hop.node == "pr" && hop.target.as_deref() == Some("review"))
    );
    let assists = assist_log::recent(&crate::disk::paths::logs_dir(), None);
    assert_eq!(
        assists
            .iter()
            .filter(|row| matches!(&row.assist,
        Assist::WorkflowRoute { run, node, to, .. }
        if run == &live.id && node == "pr" && to == "review"))
            .count(),
        1
    );
    check(dir.path(), json!({"outcome":"pending"}));
    host.observed
        .borrow_mut()
        .turn
        .as_mut()
        .unwrap()
        .end
        .ended_at += Duration::from_secs(1);
    advance_ok(&state, &live.id, &host);
    assert_eq!(decisions(&state, &pushed).last().unwrap()["check"], "pr");
}

#[test]
fn listed_second_check_waits_resamples_and_uses_its_own_close_delay() {
    for signal in [false, true] {
        let (dir, state, live, host) = launched();
        fs::write(dir.path().join("review.json"), r#"{"outcome":"waiting"}"#).unwrap();
        let mut waiting = advance_ok(&state, &live.id, &host);
        assert_eq!(waiting.node, "review");
        assert!(waiting.held.is_some());
        assert_eq!(waiting.input, live.input);
        assert_eq!(waiting.key, live.key);
        assert_eq!(waiting.wait.as_ref().unwrap().node, "review");
        if signal {
            wake(
                &state,
                &crate::harness::schedule::signal::Signal {
                    name: "review.ready".parse().unwrap(),
                    payload: Default::default(),
                    source: crate::store::event::SignalSource::Cli,
                    watch: None,
                },
                &host,
            )
            .unwrap();
            assert!(
                records::load(&state.workflow(&live.id))
                    .unwrap()
                    .held
                    .unwrap()
                    .attempt
                    .woken
            );
        } else {
            waiting.recheck_at = Some(Timestamp::UNIX_EPOCH);
            update(&state, &waiting);
        }
        fs::write(dir.path().join("review.json"), r#"{"outcome":"done"}"#).unwrap();
        let mut delayed = advance_ok(&state, &live.id, &host);
        let spec = load(&state.workflow(&live.id).home).unwrap();
        assert_eq!(close_after(&spec, &delayed).unwrap().1, "@done");
        assert_eq!(host.stops.get(), 0);
        delayed.held.as_mut().unwrap().verdict.as_mut().unwrap().at =
            Some(Timestamp::now() - Duration::from_secs(1801));
        update(&state, &delayed);
        assert_eq!(advance_ok(&state, &live.id, &host).state, RunState::Done);
        assert_eq!(decisions(&state, &live).last().unwrap()["check"], "review");
    }
}

#[test]
fn a_listed_move_waits_for_the_judged_turn_to_rest_again() {
    let (dir, state, live, host) = launched();
    fs::write(dir.path().join("review.json"), r#"{"outcome":"waiting"}"#).unwrap();
    let observed = host.observed.clone();
    let paths = state.workflow(&live.id);
    *host.on_observe.borrow_mut() = Some(Box::new(move || {
        if records::load(&paths)
            .unwrap()
            .held
            .as_ref()
            .is_some_and(|held| {
                held.verdict
                    .as_ref()
                    .is_some_and(|verdict| verdict.node == "pr")
            })
        {
            observed.borrow_mut().turn.as_mut().unwrap().at_rest = false;
        }
    }));
    let pending = advance_ok(&state, &live.id, &host);
    assert_eq!(pending.node, "pr");
    assert!(decisions(&state, &pending).is_empty());
    *host.on_observe.borrow_mut() = None;
    host.observed.borrow_mut().turn.as_mut().unwrap().at_rest = true;
    assert_eq!(advance_ok(&state, &live.id, &host).node, "review");
}

#[test]
fn listed_second_check_interruptions_and_moved_turns_name_that_check() {
    for ending in ["moved", "cancel", "restart", "deadline", "exit"] {
        let (dir, state, live, host) = launched();
        fs::write(dir.path().join("review.json"), r#"{"outcome":"waiting"}"#).unwrap();
        let mut waiting = advance_ok(&state, &live.id, &host);
        match ending {
            "moved" => {
                host.observed.borrow_mut().turn = None;
            }
            "cancel" => {
                cancel(&state, &live.id, &host, None).unwrap();
            }
            "restart" => {
                restart(&state, &live.id, None, &host).unwrap();
            }
            "deadline" => {
                waiting.deadline = Some(Timestamp::UNIX_EPOCH);
                update(&state, &waiting);
            }
            _ => {
                host.observed.borrow_mut().status = RunStatus::Completed;
            }
        }
        if !matches!(ending, "cancel" | "restart") {
            advance_ok(&state, &live.id, &host);
        }
        let rows = decisions(&state, &live);
        let last = rows.last().unwrap();
        assert_eq!(last["check"], "review", "{ending}: {rows:?}");
        assert_eq!(
            last["action"],
            if ending == "moved" {
                "discarded"
            } else {
                "interrupted"
            }
        );
    }
}

#[test]
fn a_listed_check_reached_without_a_held_session_routes_normally() {
    let text = definition().replace("start='build'", "start='command'")
        + "[steps.command]\nshell='true'\ntimeout='1m'\nthen='review'\n";
    let (dir, state, record) = setup(&text.replace("then=['pr','review']", "then='pr'"));
    fs::write(&state.workflow(&record.id).spec, text).unwrap();
    fs::write(dir.path().join("review.json"), r#"{"outcome":"done"}"#).unwrap();
    let host = FakeHost::new();
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Done);
    assert!(decisions(&state, &record).is_empty());
}
