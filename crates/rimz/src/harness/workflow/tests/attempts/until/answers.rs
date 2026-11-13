use super::*;

#[test]
fn run_pushbacks_point_at_complete_stderr_or_say_no_output() {
    for stderr in [String::new(), "private stderr evidence\n".repeat(500)] {
        let (dir, state, record) = setup(UNTIL);
        let host = FakeHost::new();
        advance_ok(&state, &record.id, &host);
        fs::write(dir.path().join("check.sh"), format!(
            "printf '%s' {} >&2\nprintf '%s' '{{\"outcome\":\"pending\",\"reason\":\"repair tests\"}}'\n",
            shlex::try_quote(&stderr).unwrap(),
        )).unwrap();
        host.observed.borrow_mut().turn = Some(turn());
        advance_ok(&state, &record.id, &host);
        let paths = state.workflow(&record.id);
        let journal = records::journal(&paths).unwrap();
        let seq = journal
            .iter()
            .find(|row| matches!(row.event, JournalEvent::Evaluation { .. }))
            .unwrap()
            .seq
            .unwrap();
        let log = paths.check_log(seq);
        assert!(
            log.is_file(),
            "run checks must save stderr even when empty: {}",
            log.display()
        );
        assert_eq!(fs::read_to_string(&log).unwrap(), stderr);
        let text = host.pushes.borrow().values().next().unwrap().clone();
        assert!(text.contains("repair tests"), "{text}");
        assert!(!text.contains("private stderr evidence"), "{text}");
        if stderr.is_empty() {
            assert!(text.contains("\n\nno output\n\n"), "{text}");
        } else {
            let summary = crate::disk::summary::FileSummary::measure(&log).unwrap();
            assert!(
                text.contains(&format!("output: {} ({})", log.display(), summary.label())),
                "{text}"
            );
        }
    }
}

#[test]
fn shell_and_judge_pushbacks_name_the_answering_check_and_its_output() {
    let text = UNTIL
        .replace("then='pr'", "then=['pr','review']")
        .replace("on.done='@done'", "on.done='review'")
        + "[test.review]\nshell='echo private-shell-evidence; exit 1'\non={pass='@done',fail='build'}\n";
    let (dir, state, record) = setup(&text);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    check(dir.path(), json!({"outcome":"done"}));
    host.observed.borrow_mut().turn = Some(turn());
    let pushed = advance_ok(&state, &record.id, &host);
    let paths = state.workflow(&record.id);
    let journal = records::journal(&paths).unwrap();
    let seq = journal
        .iter()
        .find(|row| row.node == "review" && matches!(row.event, JournalEvent::Evaluation { .. }))
        .unwrap()
        .seq
        .unwrap();
    let text = host.pushes.borrow().values().next().unwrap().clone();
    assert!(text.contains("check `review` answered `fail`"), "{text}");
    assert!(
        text.contains(&format!("output: {} (", paths.check_log(seq).display())),
        "{text}"
    );
    assert!(!text.contains("private-shell-evidence"), "{text}");
    let mut spec = load(&paths.home).unwrap();
    spec.edit(|spec| {
        let check = spec.checks.get_mut("review").unwrap();
        check.table = crate::harness::workflow::spec::CheckTable::Judge;
        check.shell = None;
        check.agent = Some("claude".into());
        check.prompt_file = Some("prompt.md".into());
    });
    fs::write(
        paths.check_output(seq),
        r#"{"outcome":"fail","reason":"exited 1"}"#,
    )
    .unwrap();
    let text = crate::harness::workflow::brief::push(&paths, &spec, &pushed).unwrap();
    assert!(text.find("exited 1") < text.find("output:"), "{text}");
    assert!(
        text.contains(&format!("output: {} (", paths.check_output(seq).display())),
        "{text}"
    );
    fs::remove_file(paths.check_output(seq)).unwrap();
    let text = crate::harness::workflow::brief::push(&paths, &spec, &pushed).unwrap();
    assert!(text.contains("\n\nno output\n\n"), "{text}");
    fs::remove_file(&paths.journal).unwrap();
    let text = crate::harness::workflow::brief::push(&paths, &spec, &pushed).unwrap();
    assert!(text.contains("check `pr` answered `fail`"), "{text}");
    assert!(
        !text.contains("\n\noutput:") && !text.contains("\n\nno output"),
        "{text}"
    );
}

#[test]
fn condition_pushback_explains_false_or_leaves_and_unread_terms() {
    let definition = UNTIL.split("[script.pr]").next().unwrap().to_owned()
        + "[condition.pr]\nelse='pending'\non={done='@done',pending='build',unknown='build'}\n[[condition.pr.cases]]\nwhen=['team.stage=Done || team.stage=Review']\noutcome='done'\n[[condition.pr.cases]]\nwhen=['team.stage=Done && pr=merged']\noutcome='done'\n";
    let (dir, state, record) = setup(&definition);
    fs::create_dir_all(dir.path().join(".rimz/team")).unwrap();
    fs::write(
        dir.path().join(".rimz/team/blackboard.md"),
        "Stage: Implement\n",
    )
    .unwrap();
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().turn = Some(turn());
    advance_ok(&state, &record.id, &host);
    let text = host
        .pushes
        .borrow()
        .values()
        .next()
        .cloned()
        .unwrap_or_default();
    assert!(
        text.contains("team.stage=Done: false")
            && text.contains("team.stage=Review: false")
            && text.contains("team.stage=Implement")
            && text.contains("pr=merged: unread"),
        "{text}"
    );
}

#[test]
fn a_push_interrupted_before_enqueue_reuses_its_id_without_recounting() {
    let (dir, state, record) = setup(UNTIL);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    check(dir.path(), json!({"outcome":"pending"}));
    host.observed.borrow_mut().turn = Some(turn());
    host.push_error.set(true);
    assert!(
        advance(&state, &record.id, &host).is_err(),
        "the host must be called after the push is saved"
    );
    let saved = records::load(&state.workflow(&record.id)).unwrap();
    let push = saved.attempt.as_ref().unwrap().push.as_ref().unwrap();
    assert!(!push.queued);
    let retried = advance_ok(&state, &record.id, &host);
    assert_eq!(retried.visits, saved.visits);
    assert_eq!(decisions(&state, &retried).len(), 1);
    assert_eq!(
        &*host.push_calls.borrow(),
        &[push.message.clone(), push.message.clone()]
    );
    assert_eq!(host.pushes.borrow().len(), 1);
}

#[test]
fn another_advance_between_persist_and_push_enqueues_once() {
    let (dir, state, record) = setup(UNTIL);
    let host = Rc::new(FakeHost::new());
    advance_ok(&state, &record.id, &host);
    check(dir.path(), json!({"outcome":"pending"}));
    host.observed.borrow_mut().turn = Some(turn());
    let concurrent = host.clone();
    let concurrent_state = state.clone();
    let id = record.id.clone();
    *host.on_push.borrow_mut() = Some(Box::new(move || {
        advance_ok(&concurrent_state, &id, &concurrent);
    }));
    let pushed = advance_ok(&state, &record.id, &host);
    assert_eq!(host.push_calls.borrow().len(), 2);
    assert_eq!(host.push_calls.borrow()[0], host.push_calls.borrow()[1]);
    assert_eq!(host.pushes.borrow().len(), 1);
    assert_eq!(pushed.visits["build"], 2);
    assert_eq!(decisions(&state, &pushed).len(), 1);
}

#[test]
fn a_push_over_the_visit_cap_closes_through_the_cap_route() {
    let (dir, state, mut record) = setup(&UNTIL.replace("max-visits=2", "max-visits=1"));
    record.input = Some(json!({"outcome":"pending"}));
    update(&state, &record);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    check(dir.path(), json!({"outcome":"pending"}));
    host.observed.borrow_mut().turn = Some(turn());
    let capped = advance_ok(&state, &record.id, &host);
    assert_eq!(
        (
            capped.state,
            capped.node,
            capped.input.unwrap()["outcome"].clone(),
            host.stops.get(),
            host.pushes.borrow().len(),
        ),
        (RunState::Failed, "build".into(), json!("cap"), 1, 0)
    );
}

#[test]
fn a_held_wait_rechecks_on_a_signal_or_due_tick_and_can_push() {
    for signal in [false, true] {
        let definition = UNTIL.replace(
            "on.pending='build'",
            "wake=['review.ready']\non.pending='@wait'\non.fix='build'",
        );
        let (dir, state, record) = setup(&definition);
        let host = FakeHost::new();
        advance_ok(&state, &record.id, &host);
        check(
            dir.path(),
            json!({"outcome":"pending", "recheck_at":"2099-01-01T00:00:00Z"}),
        );
        host.observed.borrow_mut().turn = Some(turn());
        let mut waiting = advance_ok(&state, &record.id, &host);
        assert_eq!(waiting.node, "pr");
        assert!(waiting.held.is_some());
        check(
            dir.path(),
            json!({"outcome":"fix", "reason":"please repair"}),
        );
        if signal {
            wake(
                &state,
                &crate::harness::signal::Signal {
                    name: "review.ready".parse().unwrap(),
                    payload: Default::default(),
                    source: crate::store::event::SignalSource::Cli,
                    watch: None,
                },
                &host,
            )
            .unwrap();
        } else {
            waiting.recheck_at = Some(Timestamp::UNIX_EPOCH);
            update(&state, &waiting);
        }
        let pushed = advance_ok(&state, &record.id, &host);
        assert!(pushed.held.is_none(), "signal={signal}: {pushed:?}");
        assert_eq!(pushed.visits["build"], 2);
        assert_eq!(host.pushes.borrow().len(), 1);
    }
}

#[test]
fn a_signal_after_a_held_answer_does_not_get_lost_in_its_wait() {
    let definition = UNTIL.replace(
        "on.pending='build'",
        "wake=['review.ready']\non.pending='@wait'",
    );
    let (dir, state, record) = setup(&definition);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    let mut pending = held(&live, true);
    let verdict = pending.held.as_mut().unwrap().verdict.as_mut().unwrap();
    verdict.target = "@wait".into();
    verdict.object = json!({"outcome":"pending", "recheck_at":"2099-01-01T00:00:00Z"});
    update(&state, &pending);
    check(dir.path(), json!({"outcome":"done"}));
    host.observed.borrow_mut().turn = Some(turn());
    wake(
        &state,
        &crate::harness::signal::Signal {
            name: "review.ready".parse().unwrap(),
            payload: Default::default(),
            source: crate::store::event::SignalSource::Cli,
            watch: None,
        },
        &host,
    )
    .unwrap();
    let ended = advance_ok(&state, &record.id, &host);
    assert_eq!(ended.state, RunState::Done);
}

#[test]
fn a_new_leader_turn_discards_a_held_wait_before_the_next_clean_rest() {
    let (dir, state, record, host) = keeps("@wait");
    host.observed.borrow_mut().turn.as_mut().unwrap().at_rest = false;
    let restored = advance_ok(&state, &record.id, &host);
    assert!(restored.held.is_none());
    assert!(restored.wait.is_none());
    let rows = decisions(&state, &restored);
    assert_eq!(rows.last().unwrap()["action"], "discarded");
    assert_eq!(rows.last().unwrap()["reason"], "leader turn moved on");
    check(dir.path(), json!({"outcome":"done"}));
    let mut next = turn();
    next.end.ended_at += std::time::Duration::from_secs(1);
    host.observed.borrow_mut().turn = Some(next);
    assert_eq!(advance_ok(&state, &record.id, &host).state, RunState::Done);
}
