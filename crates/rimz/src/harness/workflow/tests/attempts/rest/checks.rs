use super::*;

#[test]
fn kept_waits_merge_the_saved_answers_instant_instead_of_the_later_advance() {
    let (_dir, state, record) = setup(SESSION);
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().turn = Some(turn());
    let saved_verdict = held(&live, true).held.unwrap().verdict;
    let first = Timestamp::UNIX_EPOCH;
    for (index, at) in [first, first + std::time::Duration::from_secs(10)]
        .into_iter()
        .enumerate()
    {
        let mut pending = if live.held.is_some() {
            live.clone()
        } else {
            held(&live, true)
        };
        pending.held.as_mut().unwrap().verdict = saved_verdict.clone();
        let verdict = pending.held.as_mut().unwrap().verdict.as_mut().unwrap();
        verdict.at = at;
        verdict.target = "@wait".into();
        verdict.object = json!({"outcome":"pending"});
        update(&state, &pending);
        live = advance_ok(&state, &record.id, &host);
        let wait = live.wait.as_ref().unwrap();
        assert_eq!(
            (wait.first, wait.last, wait.count),
            (first, at, index as u64 + 1)
        );
    }
}

#[test]
fn forge_probe_during_the_session_can_close_a_later_held_check() {
    let (_dir, state, host, ended) = forge_check("pr=merged", 60, json!({"state":"merged"}));
    assert_eq!(ended.state, RunState::Done);
    assert_eq!(host.stops.get(), 1);
    assert_eq!(ended.input.as_ref().unwrap()["readings"]["pr"], "merged");
    assert_eq!(decisions(&state, &ended)[0]["action"], "closed");
}

#[test]
fn forge_probe_before_the_session_keeps_with_a_session_start_reason() {
    let (_dir, state, host, kept) = forge_check("pr=merged", -1, json!({"state":"merged"}));
    assert_eq!(
        kept.wait.as_ref().unwrap().detail.as_deref(),
        Some("pr (probed before this session started)")
    );
    assert_eq!(kept.state, RunState::Active);
    assert_eq!(host.stops.get(), 0);
    assert_eq!(decisions(&state, &kept)[0]["action"], "kept");
}

#[test]
fn a_held_forge_check_still_requires_ci_for_the_pr_head() {
    let (_dir, state, host, kept) = forge_check(
        "ci=passed",
        60,
        json!({"state":"open", "ci":"passing", "ci_sha":"aaaaaaa1", "open":{"head":"bbbbbbb2"}}),
    );
    assert_eq!(
        kept.wait.as_ref().unwrap().detail.as_deref(),
        Some("ci (read at aaaaaaa, PR head bbbbbbb)")
    );
    assert_eq!(host.stops.get(), 0);
    assert_eq!(decisions(&state, &kept)[0]["outcome"], "unknown");
}

fn forge_check(
    when: &str,
    seconds_after_start: i64,
    link: Value,
) -> (tempfile::TempDir, StatePaths, FakeHost, RunRecord) {
    let text = SESSION.split("[script.pr]").next().unwrap().to_owned()
        + &format!(
            "[condition.pr]\nelse='pending'\ntimeout='1h'\non={{done='@done',pending='@wait',unknown='@wait'}}\n[[condition.pr.cases]]\nwhen=['{when}']\noutcome='done'\n"
        );
    let (dir, state, record) = setup(&text);
    let host = FakeHost::new();
    let mut live = advance_ok(&state, &record.id, &host);
    live.entered_at -= std::time::Duration::from_secs(120);
    update(&state, &live);
    let at = live.entered_at + jiff::SignedDuration::from_secs(seconds_after_start);
    let runtime = crate::RuntimePaths::for_state(&state).unwrap();
    let cache = runtime.pr_state_path();
    fs::create_dir_all(cache.parent().unwrap()).unwrap();
    fs::write(
        cache,
        serde_json::to_vec(&json!({
            "branch_ci":{},
            "path_repos":{(live.scope.path.to_string_lossy().as_ref()):"repo"},
            "repos":{"repo":{"refreshed_at_ms":at.as_millisecond(), "ok":true}},
            "states":{(live.scope.path.to_string_lossy().as_ref()):link}
        }))
        .unwrap(),
    )
    .unwrap();
    host.observed.borrow_mut().turn = Some(turn());
    let resolved = advance_ok(&state, &record.id, &host);
    (dir, state, host, resolved)
}

#[test]
fn condition_hold_matures_while_the_leader_rests_on_the_judged_turn() {
    let text = SESSION.split("[script.pr]").next().unwrap().to_owned()
        + "[condition.pr]\nelse='pending'\ntimeout='1h'\non={done='@done',pending='build',unknown='build'}\n[[condition.pr.cases]]\nwhen=['team.stage=Done']\noutcome='done'\nfor='1h'\n";
    let (dir, state, record) = setup(&text);
    fs::create_dir_all(dir.path().join(".rimz/team")).unwrap();
    fs::write(dir.path().join(".rimz/team/blackboard.md"), "Stage: Done\n").unwrap();
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().turn = Some(turn());
    let mut kept = advance_ok(&state, &record.id, &host);
    assert_eq!(
        kept.wait.as_ref().map(|wait| wait.outcome.as_str()),
        Some("hold")
    );
    assert_eq!(host.stops.get(), 0);
    kept.wait.as_mut().unwrap().first -= std::time::Duration::from_secs(7200);
    update(&state, &kept);
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Done);
    assert_eq!(host.stops.get(), 1);
    assert_eq!(
        decisions(&state, &ended)
            .iter()
            .map(|row| row["action"].clone())
            .collect::<Vec<_>>(),
        vec![json!("kept"), json!("closed")]
    );
}

#[test]
fn a_held_checks_timeout_closes_through_the_same_verdict_gate() {
    let (_dir, state, record) = setup(SESSION);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    let mut pending = held(&live, false);
    pending.entered_at = Timestamp::UNIX_EPOCH;
    update(&state, &pending);
    host.observed.borrow_mut().turn = Some(turn());
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(host.stops.get(), 1);
    assert_eq!(ended.state, RunState::Failed);
    let rows = decisions(&state, &ended);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["action"], "closed");
    assert_eq!(rows[0]["outcome"], "timeout");
}

/// A session held at its rest check `pr`, which the definition `text` makes a judge, with the
/// judge the check launched still running.
fn judging_rest(text: &str) -> (tempfile::TempDir, StatePaths, FakeHost, RunRecord) {
    let text = text.replace(
        "[script.pr]\nrun='sh check.sh'",
        "[judge.pr]\nagent='claude'\nsummary='Review the work'\nprompt-file='prompt.md'",
    );
    let (dir, state, record) = setup(&text);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().turn = Some(turn());
    update(&state, &held(&live, false));
    let judging = advance_ok(&state, &record.id, &host);
    let judge = judging
        .attempt
        .as_ref()
        .expect("the rest check launches its judge");
    assert_eq!(judge.kind, records::AttemptKind::Judge);
    assert!(
        judging.held.is_some(),
        "the session is held while its judge runs"
    );
    assert_eq!(host.launched.borrow().len(), 2);
    (dir, state, host, judging)
}

/// The supervised runs of a judging rest's session and of its judge.
fn rest_runs(judging: &RunRecord) -> (crate::ids::RunId, crate::ids::RunId) {
    let run = |attempt: &records::LiveAttempt| attempt.run.as_ref().unwrap().run_id.clone();
    (
        run(&judging.held.as_ref().unwrap().attempt),
        run(judging.attempt.as_ref().unwrap()),
    )
}

#[test]
fn a_held_session_stays_held_while_its_rest_check_judge_is_held_on_a_limit() {
    let (_dir, state, host, judging) = judging_rest(SESSION);
    let judge = judging.attempt.as_ref().unwrap();
    // Only the judge's own run shows the limit; the session still rests on its judged turn.
    let mut observed = host.observed.borrow().clone();
    observed.status = RunStatus::Pending;
    observed.stopped = Some(crate::harness::workflow::Stopped {
        since: Timestamp::now(),
        kind: crate::harness::workflow::StopKind::Limit,
        handle: Some("@judge".into()),
        class: None,
        text: Some("usage limit".into()),
    });
    let judge_run = judge.run.clone().unwrap().run_id;
    host.observed_runs.borrow_mut().insert(judge_run, observed);
    let limited = advance_ok(&state, &judging.id, &host);
    assert_eq!(limited.state, RunState::Active, "{limited:#?}");
    assert!(
        limited.attempt.as_ref().unwrap().held_since.is_some(),
        "{limited:#?}"
    );
    assert!(
        limited.held.is_some(),
        "the session stays held behind its judge"
    );
    assert_eq!(host.stops.get(), 0);
}

/// The judge's own errored turn fails the check through the failure chain, which no route
/// names here: the run fails and closes the session it judged, as a script check's `died` does.
#[test]
fn a_rest_check_judges_unrouted_errored_turn_fails_the_run_and_closes_its_session() {
    let (_dir, state, host, judging) = judging_rest(SESSION);
    let (session, judge) = rest_runs(&judging);
    let error = crate::harness::workflow::Stopped {
        since: Timestamp::now(),
        kind: crate::harness::workflow::StopKind::Error,
        handle: Some("@judge".into()),
        class: Some(crate::agents::TurnErrorClass::Failed),
        text: Some("API Error: 500".into()),
    };
    let mut observed = host.observed.borrow().clone();
    observed.status = RunStatus::Pending;
    observed.stopped = Some(error.clone());
    observed.errored = Some(error);
    host.observed_runs
        .borrow_mut()
        .insert(judge.clone(), observed);
    let failed = advance_ok(&state, &judging.id, &host);
    assert_eq!(failed.state, RunState::Failed, "{failed:#?}");
    assert!(failed.held.is_none(), "{failed:#?}");
    assert_eq!(*host.stopped.borrow(), [judge, session]);
    assert!(
        records::journal(&state.workflow(&judging.id))
            .unwrap()
            .iter()
            .any(|entry| matches!(
                &entry.event,
                JournalEvent::Route { outcome, .. } if outcome == "errored"
            ))
    );
}

#[test]
fn a_session_that_opens_a_turn_while_its_rest_check_judge_runs_stops_only_the_judge() {
    // A stop that fails is dropped from the debt as well, so it never holds the run.
    for failing in [false, true] {
        let (_dir, state, host, judging) = judging_rest(SESSION);
        let (session, judge) = rest_runs(&judging);
        host.observed.borrow_mut().turn.as_mut().unwrap().at_rest = false;
        host.stop_error.set(failing);
        let after = advance_ok(&state, &judging.id, &host);
        let stopped = if failing { vec![] } else { vec![judge] };
        assert_eq!(*host.stopped.borrow(), stopped, "failing stop: {failing}");
        let attempt = after.attempt.as_ref().unwrap();
        assert_eq!(attempt.kind, records::AttemptKind::Agent);
        assert_eq!(attempt.run.as_ref().unwrap().run_id, session);
        assert!(after.held.is_none(), "{after:#?}");
        assert!(after.abandoned.is_empty(), "{after:#?}");
    }
}

#[test]
fn a_pass_that_dies_before_an_abandoned_judges_stop_leaves_the_stop_to_the_next_pass() {
    let (_dir, state, host, judging) = judging_rest(SESSION);
    let (session, judge) = rest_runs(&judging);
    host.observed.borrow_mut().turn.as_mut().unwrap().at_rest = false;
    *host.on_stop.borrow_mut() = Some(Box::new(|| panic!("the pass dies before its stop")));
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        advance(&state, &judging.id, &host)
    }));
    assert!(died.is_err());
    let stored = records::load(&state.workflow(&judging.id)).unwrap();
    assert!(stored.held.is_none(), "{stored:#?}");
    let owed = stored.abandoned.iter().map(|run| &run.run_id);
    assert_eq!(owed.collect::<Vec<_>>(), vec![&judge]);
    *host.on_stop.borrow_mut() = None;
    let after = advance_ok(&state, &judging.id, &host);
    assert_eq!(*host.stopped.borrow(), vec![judge]);
    assert_eq!(after.attempt.unwrap().run.unwrap().run_id, session);
    assert!(after.abandoned.is_empty());
}

#[test]
fn a_sessions_timeout_passing_at_its_running_judge_rest_check_stops_both_agents() {
    let text = SESSION.replace("rest-check=['pr']\n", "rest-check=['pr']\ntimeout='1h'\n");
    let (_dir, state, host, mut judging) = judging_rest(&text);
    let (session, judge) = rest_runs(&judging);
    judging.held.as_mut().unwrap().attempt.started_at =
        Timestamp::now() - std::time::Duration::from_secs(7200);
    update(&state, &judging);
    let ended = advance_ok(&state, &judging.id, &host);
    assert_eq!(ended.state, RunState::Failed, "{ended:#?}");
    assert_eq!(*host.stopped.borrow(), vec![session, judge]);
    assert!(ended.abandoned.is_empty());
}

#[test]
fn cancel_and_restart_of_a_session_held_at_a_running_judge_stop_the_judge() {
    for restarting in [false, true] {
        let (_dir, state, host, judging) = judging_rest(SESSION);
        let (session, judge) = rest_runs(&judging);
        // Every stop, the abandoned judge's included, lands before a write ends the run.
        let (stop_state, id) = (state.clone(), judging.id.clone());
        *host.on_stop.borrow_mut() = Some(Box::new(move || {
            let stored = records::load(&stop_state.workflow(&id)).unwrap();
            assert_eq!(stored.state, RunState::Active, "{stored:#?}");
        }));
        let after = if restarting {
            restart(&state, &judging.id, None, &host).unwrap();
            advance_ok(&state, &judging.id, &host)
        } else {
            cancel(&state, &judging.id, &host, None).unwrap();
            records::load(&state.workflow(&judging.id)).unwrap()
        };
        let stopped = host.stopped.borrow();
        assert!(
            stopped.contains(&judge),
            "restart {restarting}: {stopped:?}"
        );
        assert!(
            stopped.contains(&session),
            "restart {restarting}: {stopped:?}"
        );
        assert!(after.abandoned.is_empty(), "restart {restarting}");
        if !restarting {
            assert_eq!(after.state, RunState::Canceled);
        }
    }
}

#[test]
fn a_dead_pass_that_staged_an_ending_owing_a_judges_stop_is_repaid_and_finished_by_one_advance() {
    let (_dir, state, host, judging) = judging_rest(SESSION);
    let (session, judge) = rest_runs(&judging);
    *host.on_stop.borrow_mut() = Some(Box::new(|| panic!("the cancel dies at its first stop")));
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cancel(&state, &judging.id, &host, None)
    }));
    assert!(died.is_err());
    let mut staged = records::load(&state.workflow(&judging.id)).unwrap();
    assert_eq!(staged.state, RunState::Active);
    let attempt = staged.attempt.as_mut().unwrap();
    assert!(attempt.ending.is_some(), "{staged:#?}");
    attempt.host = Host {
        pid: u32::MAX,
        start: None,
    };
    let owed = staged.abandoned.iter().map(|run| &run.run_id);
    assert_eq!(owed.collect::<Vec<_>>(), vec![&judge]);
    update(&state, &staged);
    *host.on_stop.borrow_mut() = None;
    let ended = advance_ok(&state, &judging.id, &host);
    let stopped = host.stopped.borrow();
    assert!(stopped.contains(&judge), "{stopped:?}");
    assert!(stopped.contains(&session), "{stopped:?}");
    assert_eq!(ended.state, RunState::Canceled, "{ended:#?}");
    assert!(ended.abandoned.is_empty());
}

/// A session with a one-hour clock held at its rest check `pr`, whose check runs under this
/// process: the session started `session` ago and the check entered `check` ago.
fn timed_rest(
    routes: &str,
    session: std::time::Duration,
    check: std::time::Duration,
    ask: bool,
) -> (
    tempfile::TempDir,
    StatePaths,
    FakeHost,
    RunRecord,
    Timestamp,
) {
    let text = SESSION.replace(
        "rest-check=['pr']\n",
        &format!("rest-check=['pr']\ntimeout='1h'\n{routes}"),
    );
    let (dir, state, record) = setup(&text);
    super::check(dir.path(), json!({"outcome":"pending"}));
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().turn = Some(turn());
    let mut pending = held(&live, false);
    let now = Timestamp::now();
    let start = now - session;
    pending.held.as_mut().unwrap().attempt.started_at = start;
    pending.entered_at = now - check;
    let mut running = live.attempt.unwrap();
    running.node = "pr".into();
    running.kind = records::AttemptKind::Script;
    running.lifecycle = StepLifecycle::Turn;
    running.judged = None;
    running.run = None;
    running.visit = None;
    running.seq = pending.next_seq;
    running.host = Host {
        pid: std::process::id(),
        start: crate::proc::process_start_token(std::process::id()),
    };
    pending.next_seq += 1;
    pending.attempt = Some(running);
    update(&state, &pending);
    if ask {
        host.observed.borrow_mut().ask = Some(records::Asking {
            since: now - std::time::Duration::from_secs(5400),
            ask: records::Asked {
                text: "Approve?".into(),
                kind: "permission".into(),
                handle: "@child".into(),
            },
        });
    }
    let after = advance_ok(&state, &record.id, &host);
    (dir, state, host, after, start)
}

#[test]
fn a_sessions_clock_passing_at_its_running_rest_check_fails_the_session() {
    let hour = std::time::Duration::from_secs(3600);
    let minute = std::time::Duration::from_secs(60);
    for (routes, check, ending) in [
        ("keep=true\n", minute, RunState::Failed),
        ("keep=true\non.timeout='@done'\n", minute, RunState::Done),
        ("keep=true\n", 2 * hour, RunState::Failed),
    ] {
        let (_dir, state, host, ended, _) = timed_rest(routes, 2 * hour, check, false);
        let case = format!("{routes:?} check entered {check:?} ago");
        assert_eq!(ended.state, ending, "{case}");
        assert_eq!(
            host.stops.get(),
            1,
            "keep does not hold a timed-out session: {case}"
        );
        let previous = ended.prev_attempt.unwrap();
        assert_eq!(
            (previous.step.as_str(), previous.outcome),
            ("build", AttemptOutcome::Timeout),
            "{case}"
        );
        let journal = records::journal(&state.workflow(&ended.id)).unwrap();
        assert!(
            journal.iter().any(|entry| entry.node == "pr"
                && matches!(&entry.event, JournalEvent::Evaluation { outcome, .. } if outcome == "canceled")),
            "the running check is abandoned: {case}"
        );
        assert!(
            journal.iter().any(|entry| entry.node == "build"
                && matches!(&entry.event, JournalEvent::Route { outcome, .. } if outcome == "timeout")),
            "the session's timeout routes: {case}"
        );
    }
}

#[test]
fn a_rest_checks_own_timeout_passing_first_fails_the_check_and_leaves_the_session_clock() {
    let hour = std::time::Duration::from_secs(3600);
    let (_dir, state, host, ended, _) =
        timed_rest("", std::time::Duration::from_secs(60), 2 * hour, false);
    assert_eq!(ended.state, RunState::Failed);
    assert_eq!(host.stops.get(), 1);
    assert_eq!(
        ended.prev_attempt.as_ref().unwrap().outcome,
        AttemptOutcome::Ended
    );
    let rows = decisions(&state, &ended);
    assert_eq!(
        (rows.len(), &rows[0]["action"], &rows[0]["outcome"]),
        (1, &json!("closed"), &json!("timeout"))
    );
}

#[test]
fn a_paused_sessions_clock_does_not_pass_while_its_rest_check_runs() {
    let hour = std::time::Duration::from_secs(3600);
    let minute = std::time::Duration::from_secs(60);
    // An open question pauses the session's clock.
    let (_dir, _state, host, after, start) = timed_rest("", 2 * hour, minute, true);
    assert_eq!(after.state, RunState::Active);
    assert_eq!(host.stops.get(), 0);
    assert!(after.prev_attempt.is_none());
    // The session keeps its clock from its admission when it comes back from the rest.
    let session = after.session_attempt().unwrap();
    assert_eq!(session.started_at, start);
    assert!(session.held_since.is_some());
}
