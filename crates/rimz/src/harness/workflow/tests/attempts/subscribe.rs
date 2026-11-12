use super::until::{UNTIL, check, turn};
use super::*;
use crate::harness::signal::Signal;

const STAY: &str = "[workflow]\nscope='worktree'\nstart='build'\n[steps.build]\nagent='claude'\nprompt-file='prompt.md'\nlifecycle='stay'\nsubscribe=['ci.failed', {signal='deploy.done', match={env='prod'}, prompt='redeploy'}]\nthen='@done'\n";

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
    assert!(
        forwards[1].1.ends_with(").\n\nredeploy"),
        "the binding's prompt follows its signal: {}",
        forwards[1].1
    );
    assert!(
        text.ends_with(")."),
        "a binding without a prompt adds none: {text}"
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
        "subscribe=['team.stage', {signal='team.stage', match={board='/b'}}, 'team.*', 'review.ready', {signal='pr.opened', match={repo='o/r'}}, ",
    );
    let (_dir, state, record) = setup(&text);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    let note = host.launched.borrow().last().unwrap().workflow_note.clone();
    assert!(
        note.contains(
            "You subscribe to `team.stage` for this checkout's team board, `team.stage` where board=/b, `team.*` for this checkout's team, `review.ready` anywhere in the room, `pr.opened` for this checkout where repo=o/r, `ci.failed` for this checkout, and `deploy.done` where env=prod. Each arrives as a SIGNAL message at one of your turn boundaries while this step lasts."
        ),
        "{note}"
    );
    let (_dir, state, record) = setup(&STAY.replace("subscribe=", "notes=false\n#"));
    advance_ok(&state, &record.id, &host);
    let note = host.launched.borrow().last().unwrap().workflow_note.clone();
    assert!(!note.contains("You subscribe"), "{note}");
}

#[test]
fn a_staying_steps_defaults_follow_the_checkout_its_leader_launched_in() {
    let (dir, state, record) = setup(&STAY.replace(
        "lifecycle='stay'\nsubscribe=['ci.failed',",
        "lifecycle='stay'\nworktree='review'\nsubscribe=['ci.failed', 'team.stage',",
    ));
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    let review = crate::worktree::worktree_path(
        &record.project_root,
        &crate::config::MachineConfig::load_lenient().agents.worktree,
        "review",
    )
    .unwrap();
    let board = crate::harness::scratch::durable_board_path(&review);
    for (name, payload) in [
        ("ci.failed", json!({"path": dir.path().to_string_lossy()})),
        ("ci.failed", json!({"path": review.to_string_lossy()})),
        ("team.stage", json!({"board": board.to_string_lossy()})),
    ] {
        wake(&state, &signal(name, payload), &host).unwrap();
    }
    let signals = forwarded(&state, &record)
        .into_iter()
        .map(|(_, signal, _)| signal)
        .collect::<Vec<_>>();
    assert_eq!(
        signals,
        vec!["ci.failed".to_owned(), "team.stage".into()],
        "the launch checkout's CI and board deliver, the firing checkout's do not"
    );
}

#[test]
fn every_distinct_delivering_binding_forwards_its_own_prompt() {
    let (dir, state, record) = setup(&STAY.replace(
        "subscribe=['ci.failed',",
        "subscribe=[{signal='ci.failed', match={branch='feat'}, prompt='one'}, {signal='ci.failed', prompt='two'}, {signal='ci.failed', match={branch='feat'}, prompt='dup'}, ",
    ));
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    wake(
        &state,
        &signal(
            "ci.failed",
            json!({"path": dir.path().to_string_lossy(), "branch": "feat"}),
        ),
        &host,
    )
    .unwrap();
    let prompts = host
        .forwards
        .borrow()
        .iter()
        .map(|(_, text)| text.rsplit('\n').next().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        prompts,
        vec!["one".to_owned(), "two".into()],
        "identical bindings coalesce into the first"
    );
    assert_eq!(forwarded(&state, &record).len(), 2);
}

#[test]
fn a_staying_step_never_hears_its_own_leaders_lifecycle_signals() {
    let (_dir, state, record) = setup(&STAY.replace(
        "subscribe=['ci.failed',",
        concat!(
            "subscribe=[{signal='agent.idle', match={handle='@builder'}}, ",
            "{signal='agent.idle', match={handle='@other'}}, "
        ),
    ));
    let lifecycle = |payload: Value| Signal {
        source: crate::store::event::SignalSource::Lifecycle,
        ..signal("agent.idle", payload)
    };
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    assert_eq!(live.attempt.unwrap().run.unwrap().handle, "@builder");
    for payload in [
        json!({"handle": "@builder"}),
        json!({"handle": "@builder#main"}),
        json!({"handle": "@other"}),
    ] {
        wake(&state, &lifecycle(payload), &host).unwrap();
    }
    assert_eq!(
        forwarded(&state, &record).len(),
        1,
        "only another agent's idle forwards; the leader's own never wakes it"
    );
    cancel(&state, &record.id, &host, None).unwrap();
}

#[test]
fn a_staying_step_never_hears_lifecycle_signals_from_its_leaders_session() {
    let (_dir, state, record) = setup(&STAY.replace(
        "subscribe=['ci.failed',",
        "subscribe=[{signal='agent.idle', match={handle='@other'}}, ",
    ));
    let idle = |session: &str| Signal {
        source: crate::store::event::SignalSource::Lifecycle,
        ..signal(
            "agent.idle",
            json!({"handle": "@other", "session": session}),
        )
    };
    let host = FakeHost::new();
    *host.leader_session.borrow_mut() = Some("leader-session".into());
    advance_ok(&state, &record.id, &host);
    let leader = |_: &AttemptRun| Some(crate::ids::AgentSessionId::from("leader-session"));
    assert!(!hears_lifecycle(&state, &idle("leader-session"), &leader).unwrap());
    assert!(hears_lifecycle(&state, &idle("other-session"), &leader).unwrap());
    for session in ["leader-session", "other-session"] {
        wake(&state, &idle(session), &host).unwrap();
    }
    assert_eq!(
        forwarded(&state, &record).len(),
        1,
        "the leader's own session never wakes it, whatever handle it reports"
    );
    cancel(&state, &record.id, &host, None).unwrap();
}

#[test]
fn a_staying_step_hears_its_cohorts_and_other_agents_lifecycle_signals() {
    let (dir, state, record) = setup(&STAY.replace(
        "subscribe=['ci.failed',",
        "subscribe=['team.idle', {signal='agent.done', match={handle='@other'}}, 'agent.idle', ",
    ));
    let lifecycle = |name: &str, payload: Value| Signal {
        source: crate::store::event::SignalSource::Lifecycle,
        ..signal(name, payload)
    };
    let cohort = |channel: &str| json!({"team": "forge", "instance": format!("forge#{channel}")});
    let here = crate::harness::lanes::in_place_team_name(dir.path(), "forge").to_string();
    let idle = lifecycle("team.idle", cohort(&here));
    assert!(
        !hears_lifecycle(&state, &idle, &|_| None).unwrap(),
        "nothing is live before the leader launches"
    );
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    assert!(hears_lifecycle(&state, &idle, &|_| None).unwrap());
    assert!(
        !hears_lifecycle(&state, &lifecycle("worktree.created", json!({})), &|_| None).unwrap()
    );
    for signal in [
        lifecycle("team.idle", cohort("elsewhere")),
        lifecycle("agent.done", json!({"handle": "@leader"})),
        lifecycle("agent.idle", json!({"handle": "@other"})),
        idle.clone(),
        lifecycle("agent.done", json!({"handle": "@other"})),
    ] {
        wake(&state, &signal, &host).unwrap();
    }
    let signals = forwarded(&state, &record)
        .into_iter()
        .map(|(_, signal, _)| signal)
        .collect::<Vec<_>>();
    assert_eq!(
        signals,
        vec!["team.idle".to_owned(), "agent.done".into()],
        "the checkout's cohort and the named agent deliver; a self-scoped agent selector is dropped"
    );
    cancel(&state, &record.id, &host, None).unwrap();
    assert!(!hears_lifecycle(&state, &idle, &|_| None).unwrap());
}
