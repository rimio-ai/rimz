use super::until::{UNTIL, check, turn};
use super::*;
use crate::harness::schedule::signal::Signal;

const STAY: &str = "[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\nagent='claude'\nprompt-file='prompt.md'\nlifecycle='stay'\nsubscribe=['ci.failed', {signal='deploy.done', match={env='prod'}}]\nthen='@done'\n";

fn signal(name: &str, payload: Value) -> Signal {
    Signal {
        name: name.parse().unwrap(),
        payload: payload.as_object().unwrap().clone(),
        source: crate::store::event::SignalSource::Cli,
        watch: None,
    }
}

fn forwarded(
    state: &StatePaths,
    record: &RunRecord,
) -> Vec<(String, String, crate::ids::MessageId)> {
    records::journal(&state.workflow(&record.id))
        .unwrap()
        .into_iter()
        .filter_map(|row| match row.event {
            JournalEvent::Signal { signal, message } => Some((row.node, signal, message)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_staying_step_forwards_only_delivering_signals_to_its_leader() {
    let (dir, state, record) = setup(STAY);
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    let paths = state.workflow(&record.id);
    let before = fs::read(&paths.record).unwrap();
    let here = dir.path().to_string_lossy().into_owned();
    for (name, payload) in [
        ("ci.passed", json!({"path": here})),
        ("ci.failed", json!({"path": "/elsewhere"})),
        ("deploy.done", json!({"env": "staging"})),
        ("review.ready", json!({})),
    ] {
        wake(&state, &signal(name, payload), &host).unwrap();
    }
    assert!(
        host.forwards.borrow().is_empty(),
        "skip and ignore forward nothing"
    );
    wake(
        &state,
        &signal("ci.failed", json!({"path": here, "branch": "feat"})),
        &host,
    )
    .unwrap();
    wake(
        &state,
        &signal("deploy.done", json!({"env": "prod"})),
        &host,
    )
    .unwrap();
    let forwards = host.forwards.borrow();
    assert_eq!(forwards.len(), 2);
    let (message, text) = &forwards[0];
    assert!(text.starts_with("ci.failed on feat\n"), "{text}");
    assert!(
        text.contains(&format!("rimz workflow show {}", record.id)),
        "{text}"
    );
    assert!(
        forwards[1].1.starts_with("deploy.done\nenv: prod\n"),
        "{}",
        forwards[1].1
    );
    assert_eq!(
        forwarded(&state, &record),
        vec![
            ("build".into(), "ci.failed".into(), message.clone()),
            ("build".into(), "deploy.done".into(), forwards[1].0.clone()),
        ]
    );
    assert_eq!(
        records::journal(&paths)
            .unwrap()
            .iter()
            .filter(|row| matches!(row.event, JournalEvent::Signal { .. }))
            .map(|row| row.seq)
            .collect::<Vec<_>>(),
        vec![Some(live.attempt.as_ref().unwrap().seq); 2]
    );
    assert_eq!(
        fs::read(&paths.record).unwrap(),
        before,
        "a step-only match changes no run state"
    );
}

#[test]
fn unsubscribed_turn_ended_and_unlaunched_steps_receive_nothing() {
    let here = |dir: &tempfile::TempDir| json!({"path": dir.path().to_string_lossy()});
    let (dir, state, _record) = setup(STAY);
    let host = FakeHost::new();
    wake(&state, &signal("ci.failed", here(&dir)), &host).unwrap();

    let (dir, state, record) = setup(&STAY.replace("lifecycle='stay'", "lifecycle='turn'"));
    advance_ok(&state, &record.id, &host);
    wake(&state, &signal("ci.failed", here(&dir)), &host).unwrap();

    let (dir, state, record) = setup(&STAY.replace("subscribe=", "notes=false\n#"));
    advance_ok(&state, &record.id, &host);
    wake(&state, &signal("ci.failed", here(&dir)), &host).unwrap();

    let (dir, state, record) = setup(STAY);
    advance_ok(&state, &record.id, &host);
    cancel(&state, &record.id, &host, None).unwrap();
    wake(&state, &signal("ci.failed", here(&dir)), &host).unwrap();
    assert!(host.forwards.borrow().is_empty());
}

#[test]
fn a_held_until_session_and_its_check_both_take_the_signal() {
    let definition = UNTIL
        .replace(
            "lifecycle='until'",
            "lifecycle='until'\nsubscribe=['review.ready']",
        )
        .replace(
            "on.pending='build'",
            "wake=['review.ready']\non.pending='@wait'",
        );
    let (dir, state, record) = setup(&definition);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    check(
        dir.path(),
        json!({"outcome":"pending", "recheck_at":"2099-01-01T00:00:00Z"}),
    );
    host.observed.borrow_mut().turn = Some(turn());
    let waiting = advance_ok(&state, &record.id, &host);
    assert_eq!(waiting.node, "pr");
    let seq = waiting.held.as_ref().unwrap().attempt.seq;
    host.forward_error.set(true);
    wake(&state, &signal("review.ready", json!({})), &host).unwrap();
    let woken = records::load(&state.workflow(&record.id)).unwrap();
    assert!(woken.held.as_ref().unwrap().attempt.woken);
    let journal = records::journal(&state.workflow(&record.id)).unwrap();
    assert!(
        journal
            .iter()
            .any(|row| row.node == "pr" && matches!(row.event, JournalEvent::Wake { .. }))
    );
    let signals = journal
        .iter()
        .filter(|row| matches!(row.event, JournalEvent::Signal { .. }))
        .map(|row| (row.node.as_str(), row.seq))
        .collect::<Vec<_>>();
    assert_eq!(
        signals,
        vec![("build", Some(seq))],
        "a failed send keeps its journal row"
    );
    assert_eq!(host.forward_calls.get(), 1);
}

#[test]
fn a_subscribed_step_brief_lists_each_subscription_with_its_scope() {
    let text = STAY.replace(
        "subscribe=[",
        "subscribe=['team.stage', {signal='team.stage', match={board='/b'}}, 'review.ready', {signal='pr.opened', match={repo='o/r'}}, ",
    );
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    let note = host.launched.borrow().last().unwrap().workflow_note.clone();
    assert!(
        note.contains(
            "You subscribe to `team.stage` for this checkout's team board, `team.stage` where board=/b, `review.ready` anywhere in the room, `pr.opened` for this checkout where repo=o/r, `ci.failed` for this checkout, and `deploy.done` where env=prod. Each arrives as a SIGNAL message at one of your turn boundaries while this step lasts."
        ),
        "{note}"
    );
    let (_dir, state, record) = setup(&STAY.replace("subscribe=", "notes=false\n#"));
    advance_ok(&state, &record.id, &host);
    let note = host.launched.borrow().last().unwrap().workflow_note.clone();
    assert!(!note.contains("You subscribe"), "{note}");
}
