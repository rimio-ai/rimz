use super::*;
use crate::disk::paths::WorkflowPaths;
use crate::ids::MessageId;
use crate::store::workflow::{self as records, AttemptOutcome, JournalEvent, RunState};

const TELL: &str = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='tell'\n[steps.tell]\nmessage='@planner'\nprompt-file='prompt.md'\nthen='@done'\n";

fn attempt_rows(paths: &WorkflowPaths) -> Vec<(AttemptOutcome, Option<String>, Option<MessageId>)> {
    records::journal(paths)
        .unwrap()
        .into_iter()
        .filter_map(|row| match row.event {
            JournalEvent::Attempt {
                outcome,
                handle,
                message,
                ..
            } => Some((outcome, handle, message)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_message_step_queues_one_notice_and_follows_then_without_agent_host_operations() {
    let (_dir, state, record) = super::attempts::on_break_setup(TELL);
    let host = FakeHost::new();
    *host.occupancy.borrow_mut() = Occupancy::Blocked(vec![]);
    let paths = state.workflow(&record.id);
    fs::write(paths.signs(Some("tell")), "Merged as #42").unwrap();
    advance(&state, &record.id, &host).unwrap();
    let ended = records::load(&paths).unwrap();
    assert_eq!(ended.state, RunState::Done, "{ended:?}");
    let previous = ended.prev_attempt.as_ref().unwrap();
    assert_eq!(
        (
            previous.outcome,
            previous.handle.as_deref(),
            previous.run_id.as_ref()
        ),
        (AttemptOutcome::Ended, Some("@planner"), None)
    );
    let messages = host.messages.borrow();
    let [(target, id, text)] = messages.as_slice() else {
        panic!("one notice per visit: {messages:?}");
    };
    assert_eq!(target, "@planner");
    assert_eq!(
        text,
        &format!(
            "Workflow `test` step `tell` sends you this message (run {id}; `rimz workflow show {id}`).\n\nBuild it\n\nMerged as #42\n\nRun request",
            id = record.id
        )
    );
    assert_eq!(
        attempt_rows(&paths),
        vec![(
            AttemptOutcome::Ended,
            Some("@planner".into()),
            Some(id.clone())
        )]
    );
    let calls = (
        host.occupancies.get(),
        host.evictions.get(),
        host.launched.borrow().len(),
        host.observations.get(),
        host.push_calls.borrow().len(),
        host.stops.get(),
        host.detaches.get(),
    );
    assert_eq!(calls, (0, 0, 0, 0, 0, 0, 0));
}

#[test]
fn a_message_step_without_prompt_text_sends_its_header_line_alone() {
    let text = TELL.replace("prompt-file='prompt.md'\n", "");
    let (_dir, state, mut record) = super::attempts::on_break_setup(&text);
    record.prompt = None;
    super::attempts::update(&state, &record);
    let host = FakeHost::new();
    advance(&state, &record.id, &host).unwrap();
    let messages = host.messages.borrow();
    assert_eq!(
        messages
            .iter()
            .map(|(_, _, text)| text.as_str())
            .collect::<Vec<_>>(),
        vec![format!(
            "Workflow `test` step `tell` sends you this message (run {id}; `rimz workflow show {id}`).",
            id = record.id
        )]
    );
}

#[test]
fn an_undelivered_message_follows_on_undelivered_never_then() {
    for (routes, entered) in [("", None), ("on.undelivered='retry'\n", Some("retry"))] {
        let text = format!(
            "{TELL}{routes}[checks.retry]\nshell='true'\non={{pass='@done',fail='@failed'}}\n"
        );
        let (_dir, state, record) = super::attempts::on_break_setup(&text);
        let paths = state.workflow(&record.id);
        let spec = spec::load_run(&paths.home, &record).unwrap();
        assert_eq!(
            (spec.target("tell", "ended"), spec.target("tell", "died")),
            (
                Some("@done".into()),
                Some(entered.unwrap_or("@failed").into())
            )
        );
        let host = FakeHost::new();
        *host.message_answer.borrow_mut() = Ok(Err(
            "no agent matches target `@planner`; run `rimz agents list` to see live agents".into(),
        ));
        advance(&state, &record.id, &host).unwrap();
        let journal = records::journal(&paths).unwrap();
        assert!(
            journal.iter().any(|row| matches!(&row.event,
                JournalEvent::Route { outcome, input }
                if outcome == "undelivered"
                    && input["reason"].as_str().is_some_and(|reason| reason.contains("no agent matches")))),
            "{journal:#?}"
        );
        assert_eq!(attempt_rows(&paths)[0].0, AttemptOutcome::Died, "{routes}");
        let departures = hops(&spec, &journal, None)
            .into_iter()
            .filter(|hop| hop.node == "tell" && hop.kind == HopKind::Route)
            .map(|hop| (hop.outcome, hop.target))
            .collect::<Vec<_>>();
        assert_eq!(
            departures,
            vec![(
                Some("undelivered".into()),
                Some(entered.unwrap_or("@failed").into())
            )],
            "{journal:#?}"
        );
        let ended = records::load(&paths).unwrap();
        match entered {
            None => assert_eq!(ended.state, RunState::Failed, "{ended:?}"),
            Some(check) => assert!(
                journal.iter().any(|row| row.node == check),
                "undelivered must enter {check}: {journal:#?}"
            ),
        }
    }
}

#[test]
fn a_lost_message_host_retries_the_same_notice_id() {
    let (_dir, state, record) = super::attempts::on_break_setup(TELL);
    let paths = state.workflow(&record.id);
    let host = FakeHost::new();
    *host.message_answer.borrow_mut() = Err("store unavailable".into());
    assert!(advance(&state, &record.id, &host).is_err());
    let published = records::load(&paths).unwrap();
    assert_eq!(published.state, RunState::Active);
    let id = published
        .attempt
        .as_ref()
        .and_then(|attempt| attempt.message.clone())
        .expect("the notice id is minted before the queue");
    let refusal = restart(&state, &record.id, None, &host)
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("node tell is a message step;"),
        "{refusal}"
    );
    let mut lost = published.clone();
    lost.attempt.as_mut().unwrap().host.pid = u32::MAX;
    super::attempts::update(&state, &lost);
    fs::write(record.spec_dir.join("prompt.md"), "Build it again").unwrap();
    *host.message_answer.borrow_mut() = Ok(Ok("@planner".into()));
    let retry = std::rc::Rc::new(FakeHost::new());
    *host.on_message.borrow_mut() = Some(Box::new({
        let (state, retry, run) = (state.clone(), retry.clone(), record.id.clone());
        move || {
            let mut stale = records::load(&state.workflow(&run)).unwrap();
            stale.attempt.as_mut().unwrap().host.pid = u32::MAX;
            super::attempts::update(&state, &stale);
            advance(&state, &run, &*retry).unwrap();
        }
    }));
    advance(&state, &record.id, &host).unwrap();
    let ended = records::load(&paths).unwrap();
    assert_eq!(ended.state, RunState::Done, "{ended:?}");
    let sent = |host: &FakeHost| {
        host.messages
            .borrow()
            .iter()
            .map(|(_, message, _)| message.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        (sent(&host), sent(&retry)),
        (vec![id.clone(), id.clone()], vec![id.clone()])
    );
    assert_eq!(
        (host.retired.borrow().len(), retry.retired.borrow().len()),
        (0, 0),
        "a stale host keeps the notice its retry delivered"
    );
    let assists = crate::harness::assist_log::recent(&crate::disk::paths::logs_dir(), None);
    assert_eq!(
        assists
            .iter()
            .filter(|row| matches!(&row.assist,
                crate::harness::assist_log::Assist::WorkflowStep { run, .. } if run == &record.id))
            .count(),
        1,
        "only the host that settled the attempt counts it"
    );
    assert_eq!(
        attempt_rows(&paths),
        vec![(AttemptOutcome::Ended, Some("@planner".into()), Some(id))]
    );
    assert!(
        retry.messages.borrow()[0]
            .2
            .contains("\n\nBuild it again\n\n")
    );
    let digests = records::journal(&paths)
        .unwrap()
        .into_iter()
        .find_map(|row| match row.event {
            JournalEvent::Attempt { digests, .. } => Some(digests),
            _ => None,
        });
    assert_eq!(
        digests.unwrap()["prompt.md"],
        super::super::script::digest(b"Build it again"),
        "the retry's digests describe the text it sent"
    );
    assert_eq!(ended.visits["tell"].total, 1, "a retry is the same visit");
}

#[test]
fn a_cancel_while_the_notice_is_queued_retires_it() {
    let (_dir, state, record) = super::attempts::on_break_setup(TELL);
    let host = FakeHost::new();
    *host.on_message.borrow_mut() = Some(Box::new({
        let (state, run) = (state.clone(), record.id.clone());
        move || assert!(cancel(&state, &run, &FakeHost::new(), None).unwrap())
    }));
    advance(&state, &record.id, &host).unwrap();
    let paths = state.workflow(&record.id);
    assert_eq!(records::load(&paths).unwrap().state, RunState::Canceled);
    let [(_, id, _)] = host.messages.borrow().clone().try_into().unwrap();
    assert_eq!(*host.retired.borrow(), vec![id]);
    assert_eq!(attempt_rows(&paths)[0].0, AttemptOutcome::Canceled);
}

#[test]
fn a_cancel_or_deadline_after_the_host_died_retires_its_queued_notice() {
    for deadline in [false, true] {
        let (_dir, state, record) = super::attempts::on_break_setup(TELL);
        let paths = state.workflow(&record.id);
        let host = FakeHost::new();
        *host.message_answer.borrow_mut() = Err("store unavailable".into());
        assert!(advance(&state, &record.id, &host).is_err());
        let mut lost = records::load(&paths).unwrap();
        let id = lost.attempt.as_ref().unwrap().message.clone().unwrap();
        lost.attempt.as_mut().unwrap().host.pid = u32::MAX;
        if deadline {
            lost.deadline = Some(jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1));
        }
        super::attempts::update(&state, &lost);
        let ender = FakeHost::new();
        if deadline {
            advance(&state, &record.id, &ender).unwrap();
        } else {
            assert!(cancel(&state, &record.id, &ender, None).unwrap());
        }
        assert!(ender.messages.borrow().is_empty(), "deadline={deadline}");
        assert_eq!(*ender.retired.borrow(), vec![id], "deadline={deadline}");
        assert_eq!(attempt_rows(&paths)[0].0, AttemptOutcome::Canceled);
    }
}

#[test]
fn message_step_validation_refuses_non_agent_targets_and_open_then_lists() {
    let (_home, dir) = fixture();
    let base = "[workflow]\non-break='@failed'\nscope='worktree'\nstart='tell'\n[steps.tell]\nmessage='@planner#feat-x'\nthen='@done'\n[checks.done]\nrun='true'\non.ok='@done'\n";
    fs::write(dir.join("workflow.toml"), base).unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    for (label, text, path) in [
        (
            "bare",
            base.replace("'@planner#feat-x'", "'planner'"),
            "steps.tell.message",
        ),
        (
            "pane",
            base.replace("'@planner#feat-x'", "'tmux:%1'"),
            "steps.tell.message",
        ),
        (
            "user",
            base.replace("'@planner#feat-x'", "'@user'"),
            "steps.tell.message",
        ),
        (
            "me",
            base.replace("'@planner#feat-x'", "'@me'"),
            "steps.tell.message",
        ),
        (
            "empty then",
            base.replace("then='@done'", "then=[]"),
            "steps.tell.then",
        ),
        (
            "two then",
            base.replace("then='@done'", "then=['done', '@done']"),
            "steps.tell.then",
        ),
        (
            "wait",
            base.replace("then='@done'", "then='@wait'"),
            "steps.tell.then",
        ),
        (
            "unknown",
            base.replace("then='@done'", "then='nowhere'"),
            "steps.tell.then",
        ),
        (
            "two kinds",
            base.replace("message=", "shell='true'\nmessage="),
            "steps.tell",
        ),
    ] {
        fs::write(dir.join("workflow.toml"), &text).unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        assert!(
            report.errors.iter().any(|error| error.path == path),
            "{label}: {:?}",
            report.errors
        );
    }
    fs::write(
        dir.join("workflow.toml"),
        base.replace("then='@done'", "then=['@done', 'done']"),
    )
    .unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert_eq!(report.errors.len(), 1, "one refusal: {:?}", report.errors);
    for then in ["'done'", "'@failed'", "'tell'"] {
        fs::write(
            dir.join("workflow.toml"),
            base.replace("then='@done'", &format!("then={then}\nmax-visits=2")),
        )
        .unwrap();
        let report = validate(&dir, &LoadedDefinitions::default(), None);
        assert!(report.errors.is_empty(), "{then}: {:?}", report.errors);
    }
    fs::write(
        dir.join("workflow.toml"),
        base.replace("then='@done'", "then='@done'\nprompt-file='../escape.md'"),
    )
    .unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.path == "steps.tell.prompt-file"),
        "{:?}",
        report.errors
    );
}

#[test]
fn a_message_step_row_lists_then_undelivered_cap_and_deadline() {
    let (_home, dir) = fixture();
    let text = "[workflow]\nscope='worktree'\nstart='tell'\n[steps.tell]\nmessage='@planner'\nthen='@done'\non={asked='@failed', capacity='@failed', invalid='@failed'}\n";
    fs::write(dir.join("workflow.toml"), text).unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.path == "steps.tell.on.undelivered"),
        "an unfilled undelivered edge refuses: {:?}",
        report.errors
    );
    fs::write(
        dir.join("workflow.toml"),
        text.replace("scope='worktree'", "on-break='@failed'\nscope='worktree'"),
    )
    .unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let row = &report.rows[0];
    assert_eq!(
        (
            row.runner.as_str(),
            row.message.as_deref(),
            row.agent.as_deref()
        ),
        ("message", Some("@planner"), None)
    );
    assert_eq!(
        row.edges
            .iter()
            .map(|edge| (edge.outcome.as_str(), edge.target.as_str(), edge.source))
            .collect::<Vec<_>>(),
        vec![
            ("then", "@done", EdgeSource::Explicit),
            ("undelivered", "@failed", EdgeSource::OnBreak),
            ("cap", "@failed", EdgeSource::OnBreak),
            ("deadline", "@failed", EdgeSource::OnBreak),
        ]
    );
}

#[test]
fn a_message_step_cycle_warns_that_it_never_waits() {
    let (_home, dir) = fixture();
    fs::write(
        dir.join("workflow.toml"),
        "[workflow]\non-break='@failed'\nscope='worktree'\nstart='tell'\n[steps.tell]\nmessage='@planner'\nthen='tell'\nmax-visits=3\n",
    )
    .unwrap();
    let report = validate(&dir, &LoadedDefinitions::default(), None);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let warning = report
        .warnings
        .iter()
        .find(|issue| issue.path == "steps.tell")
        .unwrap_or_else(|| panic!("{:?}", report.warnings));
    assert!(
        warning.message.contains("message steps") && warning.message.contains("never waits"),
        "{}",
        warning.message
    );
}
