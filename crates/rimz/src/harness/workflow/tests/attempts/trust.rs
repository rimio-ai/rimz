use super::*;

const PROBLEMS: &str =
    "claude hooks are missing for account `default`; run `rimz hooks install claude`";

const JUDGED: &str = "[workflow]\nscope='worktree'\nstart='review'\n\
    [judge.review]\nagent='claude'\nsummary='Reviews the change'\nprompt-file='prompt.md'\n\
    timeout='1h'\non={pass='@done',fail='@failed'}\n";

fn trust_wait(record: &RunRecord) -> Option<(&str, Option<&str>, u64)> {
    record
        .wait
        .as_ref()
        .map(|wait| (wait.outcome.as_str(), wait.detail.as_deref(), wait.count))
}

/// The closed `trust` waits the journal holds, each with its count.
fn closed_trust_waits(state: &StatePaths, record: &RunRecord) -> Vec<u64> {
    records::journal(&state.workflow(&record.id))
        .unwrap()
        .iter()
        .filter_map(|entry| match &entry.event {
            JournalEvent::Wait { outcome, count, .. } if outcome == "trust" => Some(*count),
            _ => None,
        })
        .collect()
}

/// A launch the launcher's trust read refuses waits on `trust` with its problems, counting no
/// attempt and no visit, keeps the wait's first stamp across rechecks, needs the user while it
/// waits, and launches once the problem clears, closing the one wait.
#[test]
fn a_trust_refusal_waits_with_its_first_stamp_kept_and_launches_once_cleared() {
    for text in [STEP, JUDGED] {
        let (_dir, state, mut record) = setup(text);
        record.key = Some(Key::Integer(7));
        update(&state, &record);
        let host = FakeHost::new();
        *host.launch_trust.borrow_mut() = Some(PROBLEMS.into());
        let mut first = None;
        // Past `max-visits=2`, so a visit that stayed counted would trip the step's bound.
        for count in 1..=3 {
            let waiting = advance_ok(&state, &record.id, &host);
            let case = format!("{text}: {waiting:#?}");
            assert_eq!(waiting.state, RunState::Active, "{case}");
            assert_eq!(
                trust_wait(&waiting),
                Some(("trust", Some(PROBLEMS), count)),
                "{case}"
            );
            let stamp = waiting.wait.as_ref().unwrap().first;
            assert_eq!(*first.get_or_insert(stamp), stamp, "{case}");
            assert!(
                waiting.attempt.is_none()
                    && waiting.prev_attempt.is_none()
                    && waiting.visits.is_empty()
                    && waiting.key_visits.is_empty()
                    && !waiting.launch_retry,
                "{case}"
            );
            assert!(waiting.needs_you(), "{case}");
            assert!(closed_trust_waits(&state, &record).is_empty(), "{case}");
        }
        host.launch_trust.borrow_mut().take();
        let live = advance_ok(&state, &record.id, &host);
        assert_eq!(host.launched.borrow().len(), 1, "{text}");
        assert!(live.wait.is_none() && live.attempt.is_some(), "{live:#?}");
        assert!(!live.needs_you(), "{live:#?}");
        assert_eq!(closed_trust_waits(&state, &record), [3], "{text}");
    }
}

/// The run's deadline alone ends a trust wait.
#[test]
fn the_deadline_ends_a_trust_wait() {
    let (_dir, state, record) = setup(STEP);
    let host = FakeHost::new();
    *host.launch_trust.borrow_mut() = Some(PROBLEMS.into());
    let mut waiting = advance_ok(&state, &record.id, &host);
    waiting.wait.as_mut().unwrap().first -= jiff::SignedDuration::from_hours(24);
    update(&state, &waiting);
    let still = advance_ok(&state, &record.id, &host);
    assert_eq!(
        trust_wait(&still).map(|wait| wait.0),
        Some("trust"),
        "{still:#?}"
    );
    let mut late = still;
    late.deadline = Some(Timestamp::now() - std::time::Duration::from_secs(1));
    update(&state, &late);
    let ended = advance_ok(&state, &record.id, &host);
    assert!(ended.deadline_fired, "{ended:#?}");
    assert_eq!(ended.state, RunState::Failed);
    assert!(host.launched.borrow().is_empty());
}

/// A node that stops launching an agent drops the trust wait it held: a definition edit that
/// retypes a waiting step or judge into a command or a script check, and `resume --at` onto
/// such a node, leave no wait that reads as needing the user while the new node runs.
#[test]
fn a_trust_wait_closes_when_its_node_no_longer_launches_an_agent() {
    const COMMAND: &str = "[command.build]\nshell='sleep 60'\ntimeout='1h'\nthen='after'\n[test.after]\nshell='true'\ntimeout='1h'\non.pass='@done'\non.fail='@failed'\n";
    const CHECK: &str =
        "[test.review]\nshell='sleep 60'\ntimeout='1h'\non.pass='@done'\non.fail='@failed'\n";
    let head = |start: &str| format!("[workflow]\nscope='worktree'\nstart='{start}'\n");
    let moved = format!(
        "{STEP}[command.other]\nshell='sleep 60'\ntimeout='1h'\nthen='after'\n[test.after]\nshell='true'\ntimeout='1h'\non.pass='@done'\non.fail='@failed'\n"
    );
    let cases = [
        (
            "edited step",
            STEP.to_owned(),
            Some(format!("{}{COMMAND}", head("build"))),
            None,
        ),
        (
            "edited judge",
            JUDGED.to_owned(),
            Some(format!("{}{CHECK}", head("review"))),
            None,
        ),
        ("resume --at", moved, None, Some("other")),
    ];
    for (case, text, edited, at) in cases {
        let (_dir, state, record) = setup(&text);
        let paths = state.workflow(&record.id);
        let host = FakeHost::new();
        *host.launch_trust.borrow_mut() = Some(PROBLEMS.into());
        let waiting = advance_ok(&state, &record.id, &host);
        assert_eq!(
            trust_wait(&waiting).map(|wait| wait.0),
            Some("trust"),
            "{case}"
        );
        if let Some(edited) = &edited {
            fs::write(definition_file(&waiting), edited).unwrap();
        }
        if let Some(at) = at {
            let resumed = resume(&state, &record.id, &host, Some(at)).unwrap();
            assert!(
                resumed.wait.is_none() && !resumed.needs_you(),
                "{case}: {resumed:#?}"
            );
        }
        std::thread::scope(|threads| {
            let running = threads.spawn(|| advance_ok(&state, &record.id, &FakeHost::new()));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let live = loop {
                let live = records::load(&paths).unwrap();
                if live
                    .attempt
                    .as_ref()
                    .is_some_and(|attempt| attempt.child.is_some())
                {
                    break live;
                }
                assert!(std::time::Instant::now() < deadline, "{case}: {live:#?}");
                std::thread::sleep(std::time::Duration::from_millis(10));
            };
            assert!(cancel(&state, &record.id, &FakeHost::new(), None).unwrap());
            running.join().unwrap();
            assert!(
                live.wait.is_none() && !live.needs_you(),
                "{case}: {live:#?}"
            );
        });
        assert_eq!(closed_trust_waits(&state, &record), [1], "{case}");
    }
}

/// A rest check's judge waiting on trust ends with its held session when the run's deadline
/// passes: the record the pass publishes before it stops the session holds no wait, so nothing
/// reads the ending run as needing the user.
#[test]
fn an_interrupted_rest_check_publishes_no_trust_wait() {
    let text = super::rest::SESSION.replace(
        "[script.pr]\nrun='sh check.sh'",
        "[judge.pr]\nagent='claude'\nsummary='Review the work'\nprompt-file='prompt.md'",
    );
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    host.observed.borrow_mut().turn = Some(super::rest::turn());
    update(&state, &super::rest::held(&live, false));
    *host.launch_trust.borrow_mut() = Some(PROBLEMS.into());
    let mut waiting = advance_ok(&state, &record.id, &host);
    assert_eq!(
        trust_wait(&waiting).map(|wait| wait.0),
        Some("trust"),
        "{waiting:#?}"
    );
    assert!(waiting.held.is_some(), "{waiting:#?}");
    waiting.deadline = Some(Timestamp::now() - std::time::Duration::from_secs(1));
    update(&state, &waiting);
    let published = std::rc::Rc::new(RefCell::new(Vec::new()));
    let seen = published.clone();
    let paths = state.workflow(&record.id);
    *host.on_stop.borrow_mut() = Some(Box::new(move || {
        let record = records::load(&paths).unwrap();
        seen.borrow_mut()
            .push((record.wait.is_some(), record.needs_you()));
    }));
    let ended = advance_ok(&state, &record.id, &host);
    assert!(ended.deadline_fired, "{ended:#?}");
    assert_eq!(ended.state, RunState::Failed, "{ended:#?}");
    assert!(!published.borrow().is_empty(), "{ended:#?}");
    assert!(
        published
            .borrow()
            .iter()
            .all(|seen| *seen == (false, false)),
        "{:?}",
        published.borrow()
    );
}
