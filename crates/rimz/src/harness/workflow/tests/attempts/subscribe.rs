use super::rest::{SESSION, check, turn};
use super::*;
use crate::harness::signal::Signal;

const SUBSCRIBED: &str = "[workflow]\nscope='worktree'\nstart='build'\n[agent.build]\nagent='claude'\nprompt-file='prompt.md'\nlifecycle='session'\nsubscribe=['ci.failed', {signal='deploy.done', match={env='prod'}, prompt='redeploy'}]\nthen='@done'\n";

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
fn a_session_forwards_only_delivering_signals_to_its_leader() {
    let (dir, state, record) = setup(SUBSCRIBED);
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
        text.contains(&format!(
            "rimz {}",
            crate::store::workflow::workflow_command(
                "show",
                &crate::store::workflow::RunNames::new(&state).shown(&record.id),
                &[]
            )
        )),
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
    let (dir, state, _record) = setup(SUBSCRIBED);
    let host = FakeHost::new();
    wake(&state, &signal("ci.failed", here(&dir)), &host).unwrap();

    let (dir, state, record) =
        setup(&SUBSCRIBED.replace("lifecycle='session'", "lifecycle='turn'"));
    // Admission refuses a subscribed turn, so the run waits with no attempt to forward to.
    assert!(advance(&state, &record.id, &host).is_err());
    wake(&state, &signal("ci.failed", here(&dir)), &host).unwrap();

    let (dir, state, record) = setup(&SUBSCRIBED.replace("subscribe=", "notes=false\n#"));
    advance_ok(&state, &record.id, &host);
    wake(&state, &signal("ci.failed", here(&dir)), &host).unwrap();

    let (dir, state, record) = setup(SUBSCRIBED);
    advance_ok(&state, &record.id, &host);
    cancel(&state, &record.id, &host, None).unwrap();
    wake(&state, &signal("ci.failed", here(&dir)), &host).unwrap();
    assert!(host.forwards.borrow().is_empty());
}

#[test]
fn a_held_session_and_its_rest_check_both_take_the_signal() {
    let definition = SESSION
        .replace(
            "lifecycle='session'",
            "lifecycle='session'\nsubscribe=['review.ready']",
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
    let text = SUBSCRIBED.replace(
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
    let (_dir, state, record) = setup(&SUBSCRIBED.replace("subscribe=", "notes=false\n#"));
    advance_ok(&state, &record.id, &host);
    let note = host.launched.borrow().last().unwrap().workflow_note.clone();
    assert!(!note.contains("You subscribe"), "{note}");
}

#[test]
fn a_sessions_defaults_follow_the_checkout_its_leader_launched_in() {
    let (dir, state, record) = setup(&SUBSCRIBED.replace(
        "lifecycle='session'\nsubscribe=['ci.failed',",
        "lifecycle='session'\nworktree='review'\nsubscribe=['ci.failed', 'team.stage',",
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
    let (dir, state, record) = setup(&SUBSCRIBED.replace(
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
fn a_session_never_hears_its_own_leaders_lifecycle_signals() {
    let (_dir, state, record) = setup(&SUBSCRIBED.replace(
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
fn a_session_never_hears_lifecycle_signals_from_its_leaders_session() {
    let (_dir, state, record) = setup(&SUBSCRIBED.replace(
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
fn a_session_hears_its_cohorts_and_other_agents_lifecycle_signals() {
    let (dir, state, record) = setup(&SUBSCRIBED.replace(
        "subscribe=['ci.failed',",
        "subscribe=['team.idle', {signal='agent.done', match={handle='@other'}}, ",
    ));
    let lifecycle = |name: &str, payload: Value| Signal {
        source: crate::store::event::SignalSource::Lifecycle,
        ..signal(name, payload)
    };
    let cohort = |channel: &str| json!({"team": "forge", "instance": format!("forge#{channel}")});
    let idle = lifecycle("team.idle", cohort("forge"));
    assert!(
        !hears_lifecycle(&state, &idle, &|_| None).unwrap(),
        "nothing is live before the leader launches"
    );
    let host = FakeHost::new();
    let live = advance_ok(&state, &record.id, &host);
    // A live session run whose definition no longer reads hears nothing and hides no other.
    let deployed = dir.path().join("broken");
    fs::create_dir_all(&deployed).unwrap();
    fs::write(deployed.join("workflow.toml"), "[workflow").unwrap();
    let broken = RunRecord {
        id: WorkflowRunId::new(),
        spec_dir: Some(deployed),
        ..live
    };
    let guard = crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    records::initialize(&state, &broken, &guard).unwrap();
    drop(guard);
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
        "the checkout's cohort and the named agent deliver"
    );
    cancel(&state, &record.id, &host, None).unwrap();
    assert!(!hears_lifecycle(&state, &idle, &|_| None).unwrap());
}

/// A check run that waits for `review.ready`, and the same check routing `done` to a node that
/// does not exist, which parses and admission refuses.
const WAITING: &str = "[workflow]\nscope='worktree'\nstart='pr'\n[script.pr]\nrun='sh check.sh'\ntimeout='1h'\nwake=['review.ready']\non.pending='@wait'\non.done='@done'\n";

/// The signal wake admits each matching run before it touches it: a refused definition keeps
/// its backoff and gets no journal row or forward, and the wake goes on to the next run.
#[test]
fn a_wake_skips_a_refused_definition_and_wakes_the_next_run() {
    let refused = WAITING.replace("on.done='@done'", "on.done='nowhere'");
    let (dir, state, first) = setup(WAITING);
    check(
        dir.path(),
        json!({"outcome":"pending", "recheck_at":"2099-01-01T00:00:00Z"}),
    );
    let other = dir.path().join("another");
    fs::create_dir_all(&other).unwrap();
    let mut second = first.clone();
    second.id = crate::ids::WorkflowRunId::new();
    second.spec_dir = Some(other.clone());
    let guard = crate::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
    records::initialize(&state, &second, &guard).unwrap();
    drop(guard);
    fs::write(definition_file(&second), WAITING).unwrap();
    let host = FakeHost::new();
    for record in [&first, &second] {
        let waiting = advance_ok(&state, &record.id, &host);
        assert!(waiting.wait.is_some(), "{waiting:#?}");
    }
    fs::write(definition_file(&first), &refused).unwrap();
    let paths = state.workflow(&first.id);
    let before = (
        fs::read_to_string(&paths.record).unwrap(),
        fs::read_to_string(&paths.journal).ok(),
    );
    wake(&state, &signal("review.ready", json!({})), &host).unwrap();
    assert_eq!(
        (
            fs::read_to_string(&paths.record).unwrap(),
            fs::read_to_string(&paths.journal).ok()
        ),
        before,
        "the refused run keeps its backoff and journal"
    );
    let journal = records::journal(&state.workflow(&second.id)).unwrap();
    assert!(
        journal
            .iter()
            .any(|row| matches!(row.event, JournalEvent::Wake { .. })),
        "{journal:#?}"
    );
}

/// A session's subscription on a refused definition forwards nothing and journals no signal.
#[test]
fn a_session_on_a_refused_definition_hears_nothing() {
    let (dir, state, record) = setup(SUBSCRIBED);
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    fs::write(
        definition_file(&record),
        SUBSCRIBED.replace("then='@done'", "then='build'"),
    )
    .unwrap();
    wake(
        &state,
        &signal("ci.failed", json!({"path": dir.path().to_string_lossy()})),
        &host,
    )
    .unwrap();
    assert!(host.forwards.borrow().is_empty());
    assert!(forwarded(&state, &record).is_empty());
    cancel(&state, &record.id, &host, None).unwrap();
}

fn run_signal(name: &str, run: &WorkflowRunId) -> Signal {
    Signal {
        source: crate::store::event::SignalSource::Lifecycle,
        ..signal(
            name,
            json!({"run": run.as_str(), "workflow": "test", "node": "pr"}),
        )
    }
}

/// A run's own `workflow.*` signal never reaches it; another run's reaches its subscribed step.
#[test]
fn a_session_hears_other_runs_workflow_signals_never_its_own() {
    let (_dir, state, record) =
        setup(&SUBSCRIBED.replace("subscribe=['ci.failed',", "subscribe=['workflow.*',"));
    let host = FakeHost::new();
    advance_ok(&state, &record.id, &host);
    let other = WorkflowRunId::new();
    let own = run_signal("workflow.moved", &record.id);
    assert!(!hears_lifecycle(&state, &own, &|_| None).unwrap());
    assert!(hears_lifecycle(&state, &run_signal("workflow.moved", &other), &|_| None).unwrap());
    for run in [&record.id, &other] {
        wake(&state, &run_signal("workflow.moved", run), &host).unwrap();
    }
    assert_eq!(
        forwarded(&state, &record)
            .into_iter()
            .map(|(_, signal, _)| signal)
            .collect::<Vec<_>>(),
        ["workflow.moved"],
        "only the other run's signal forwards"
    );
    cancel(&state, &record.id, &host, None).unwrap();
}

#[test]
fn a_check_wakes_on_another_runs_workflow_signal_never_its_own() {
    let (dir, state, record) = setup(&WAITING.replace("'review.ready'", "'workflow.ended'"));
    check(
        dir.path(),
        json!({"outcome":"pending", "recheck_at":"2099-01-01T00:00:00Z"}),
    );
    let host = FakeHost::new();
    let waiting = advance_ok(&state, &record.id, &host);
    assert!(waiting.wait.is_some(), "{waiting:#?}");
    let wakes = || {
        records::journal(&state.workflow(&record.id))
            .unwrap()
            .iter()
            .filter(|row| matches!(row.event, JournalEvent::Wake { .. }))
            .count()
    };
    let own = run_signal("workflow.ended", &record.id);
    assert!(!hears_lifecycle(&state, &own, &|_| None).unwrap());
    wake(&state, &own, &host).unwrap();
    assert_eq!(wakes(), 0, "a run's own ending never wakes it");
    let other = run_signal("workflow.ended", &WorkflowRunId::new());
    assert!(hears_lifecycle(&state, &other, &|_| None).unwrap());
    wake(&state, &other, &host).unwrap();
    assert_eq!(wakes(), 1);
}
