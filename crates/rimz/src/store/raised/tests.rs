use super::*;
use crate::agents::AgentStatus;
use crate::agents::state::TransitionMode;
use crate::ids::WorkspaceId;

fn paths(dir: &tempfile::TempDir) -> StatePaths {
    StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap()
}

#[test]
fn cached_snapshot_reads_raised_records_even_without_a_new_log_event() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let runtime =
        crate::disk::paths::RuntimePaths::under(paths.workspace_id.clone(), dir.path()).unwrap();
    let store = crate::Store::open(paths.clone(), runtime).unwrap();
    let observation = crate::agents::AgentLifecycleObservation::new(
        Some("a".into()),
        crate::agents::LifecycleSignal::Registered,
    );
    store
        .append_event(&crate::store::event::EventEnvelope::agent_lifecycle(
            paths.workspace_id.clone(),
            "test",
            "claude",
            "SessionStart",
            &observation,
        ))
        .unwrap();
    assert!(
        store.snapshot_cached().unwrap().agents[0]
            .raised_ask
            .is_none()
    );
    let ask = ask("a", RaisedAskState::Open);
    raise(&paths, ask.clone()).unwrap();
    assert_eq!(
        store.snapshot_cached().unwrap().agents[0].raised_ask,
        Some(ask.value)
    );
}

fn ask(session: &str, state: RaisedAskState) -> Entry<RaisedAsk> {
    Entry {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: session.into(),
        value: RaisedAsk {
            ask_id: AskId::new(),
            state,
            raised_at: Timestamp::UNIX_EPOCH,
            basis_at: Timestamp::UNIX_EPOCH,
            opened_at: None,
            raised_by: None,
            text: Some("Which one?".into()),
            brief: None,
        },
    }
}

#[test]
fn retirement_does_not_report_an_already_human_closed_ask_again() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let ask = ask("a", RaisedAskState::Open);
    raise(&paths, ask.clone()).unwrap();
    let mut observation = crate::agents::AgentLifecycleObservation::new(
        Some(ask.agent_id.clone()),
        crate::agents::LifecycleSignal::TurnStarted { turn_id: None },
    );
    observation.prompt = crate::agents::SanitizedPrompt::new(Some("yes"));
    assert!(
        observe(
            &paths,
            &ask.kind,
            &observation,
            Some(AgentState::transition(
                None,
                &observation.signal,
                TransitionMode::Edge
            )),
            Timestamp::from_second(1).unwrap(),
            true,
            None,
        )
        .unwrap()
        .is_some()
    );
    assert!(
        retire(&paths, &ask.kind, &ask.agent_id).unwrap().is_none(),
        "retirement must not audit a second close after user_turn"
    );
    assert!(read(&paths).human_turns.is_empty());
}

#[test]
fn an_earlier_read_cannot_remove_a_later_alert_and_a_tied_read_can() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let alert = Entry {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "a".into(),
        value: CardAlert {
            text: "Check the result".into(),
            raised_at: Timestamp::from_second(2).unwrap(),
            raised_by: None,
        },
    };
    set_alert(&paths, alert.clone()).unwrap();
    clear_alert(&paths, &alert.kind, &alert.agent_id, Timestamp::UNIX_EPOCH).unwrap();
    assert_eq!(
        read(&paths).alerts,
        vec![alert.clone()],
        "a stale read must not clear later attention"
    );
    clear_alert(&paths, &alert.kind, &alert.agent_id, alert.value.raised_at).unwrap();
    assert!(
        read(&paths).alerts.is_empty(),
        "a receipt reaches an equal alert stamp"
    );
}

#[test]
fn open_predicate_keeps_ties_and_rejects_later_human_turns() {
    let mut record = RaisedRecord::default();
    let ask = ask("a", RaisedAskState::Open);
    assert!(record.is_open(&ask));
    record.human_turns.push(Entry {
        kind: ask.kind.clone(),
        agent_id: ask.agent_id.clone(),
        value: HumanTurn {
            at: ask.value.basis_at,
        },
    });
    assert!(
        record.is_open(&ask),
        "a turn the raiser saw must not close its ask"
    );
    record.human_turns[0].value.at = Timestamp::from_nanosecond(1).unwrap();
    assert!(!record.is_open(&ask));
    record.human_turns.clear();
    let mut armed = ask;
    armed.value.state = RaisedAskState::Armed;
    assert!(!record.is_open(&armed));
}

#[test]
fn record_replaces_withdraws_and_attaches_only_open_live_asks() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let first = ask("a", RaisedAskState::Open);
    raise(&paths, first.clone()).unwrap();
    assert_eq!(read(&paths).asks.as_slice(), std::slice::from_ref(&first));
    let replacement = ask("a", RaisedAskState::Open);
    raise(&paths, replacement.clone()).unwrap();
    let armed = ask("b", RaisedAskState::Armed);
    raise(&paths, armed).unwrap();
    assert_eq!(read(&paths).asks.len(), 2);
    let mut agents = Arc::new(vec![
        AgentState::stub("claude", "a", AgentStatus::Idle),
        AgentState::stub("claude", "b", AgentStatus::Running),
    ]);
    attach(&mut agents, &paths);
    assert_eq!(agents[0].raised_ask, Some(replacement.value.clone()));
    assert!(agents[1].raised_ask.is_none());
    Arc::make_mut(&mut agents)[0].ended_at = Some(Timestamp::UNIX_EPOCH);
    let shared = Arc::clone(&agents);
    attach(&mut agents, &paths);
    assert!(
        !Arc::ptr_eq(&shared, &agents),
        "a changed overlay copies the rows"
    );
    let shared = Arc::clone(&agents);
    attach(&mut agents, &paths);
    assert!(
        Arc::ptr_eq(&shared, &agents),
        "unchanged overlays keep the rows shared"
    );
    assert!(agents[0].raised_ask.is_none());
    assert!(
        withdraw(
            &paths,
            &first.kind,
            &first.agent_id,
            Some(&first.value.ask_id)
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        withdraw(&paths, &first.kind, &first.agent_id, None).unwrap(),
        Some(replacement.value)
    );
    assert!(
        withdraw(&paths, &first.kind, &first.agent_id, None)
            .unwrap()
            .is_none()
    );
}

#[test]
fn alerts_replace_and_retirement_removes_all_session_records() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let first = ask("a", RaisedAskState::Open);
    raise(&paths, first.clone()).unwrap();
    for text in ["first", "look here"] {
        set_alert(
            &paths,
            Entry {
                kind: first.kind.clone(),
                agent_id: first.agent_id.clone(),
                value: CardAlert {
                    text: text.into(),
                    raised_at: Timestamp::UNIX_EPOCH,
                    raised_by: Some("@lead".into()),
                },
            },
        )
        .unwrap();
    }
    assert_eq!(read(&paths).alerts.len(), 1);
    assert_eq!(read(&paths).alerts[0].value.text, "look here");
    retire(&paths, &first.kind, &first.agent_id).unwrap();
    let record = read(&paths);
    assert!(record.asks.is_empty() && record.alerts.is_empty() && record.human_turns.is_empty());
}

#[test]
fn stale_outside_raise_is_refused_and_invalid_records_read_empty() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    paths.ensure_dirs().unwrap();
    let ask = ask("a", RaisedAskState::Open);
    let mut record = RaisedRecord::default();
    record.human_turns.push(Entry {
        kind: ask.kind.clone(),
        agent_id: ask.agent_id.clone(),
        value: HumanTurn {
            at: Timestamp::from_second(1).unwrap(),
        },
    });
    atomic::write_temp_then_rename(&paths.raised, &record).unwrap();
    assert!(matches!(
        raise(&paths, ask),
        Err(RaisedErr::AlreadyAnswered)
    ));
    std::fs::write(&paths.raised, b"bad json").unwrap();
    assert!(read(&paths).asks.is_empty());
    std::fs::write(
        &paths.raised,
        br#"{"version":999,"asks":[],"alerts":[],"human_turns":[]}"#,
    )
    .unwrap();
    assert!(read(&paths).human_turns.is_empty());
}

#[test]
fn prepare_raise_keeps_the_brief_and_an_outside_raise_opens_with_it_as_context() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let now = Timestamp::from_second(100).unwrap();
    let mut agent = AgentState::stub("claude", "raised", AgentStatus::Idle);
    agent.registered_at = Some(Timestamp::from_second(10).unwrap());
    let brief = Some("Ship it? Or wait for review?".to_owned());
    let own = prepare_raise(&paths, &agent, true, brief.clone(), None, now)
        .unwrap()
        .value;
    assert_eq!(
        (own.state, own.brief.as_deref(), own.text),
        (RaisedAskState::Armed, brief.as_deref(), None)
    );
    let outside = prepare_raise(&paths, &agent, false, brief.clone(), None, now)
        .unwrap()
        .value;
    assert_eq!(
        (outside.state, outside.brief, outside.text),
        (RaisedAskState::Open, brief.clone(), brief)
    );
}

#[test]
fn a_record_written_with_options_still_reads() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    std::fs::create_dir_all(paths.raised.parent().unwrap()).unwrap();
    std::fs::write(
        &paths.raised,
        br#"{"version":1,"asks":[{"kind":"claude","agent_id":"a","ask_id":"ask_0123456789abcdef","state":"open","raised_at":"1970-01-01T00:00:00Z","basis_at":"1970-01-01T00:00:00Z","text":"Which one?","options":["safe","fast"]}],"alerts":[],"human_turns":[]}"#,
    )
    .unwrap();
    let record = read(&paths);
    assert_eq!(record.asks.len(), 1);
    assert_eq!(record.asks[0].value.text.as_deref(), Some("Which one?"));
}

#[test]
fn prepare_raise_without_text_refuses_a_parked_agent_as_not_resting() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let now = Timestamp::from_second(100).unwrap();
    let mut agent = AgentState::stub("claude", "parked", AgentStatus::Running);
    agent.phase = crate::agents::TurnPhase::Parked;
    agent.registered_at = Some(Timestamp::from_second(10).unwrap());
    agent.last_prompt_at = Some(Timestamp::from_second(20).unwrap());
    agent.last_activity = Timestamp::from_second(50).unwrap();
    let result = prepare_raise(&paths, &agent, false, None, None, now);
    assert!(
        matches!(result, Err(RaisedErr::NotResting)),
        "expected NotResting for a parked agent, got {result:?}"
    );
}

#[test]
fn prepare_raise_refuses_a_subagents_self_raise_and_allows_an_outside_raise() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let now = Timestamp::from_second(100).unwrap();
    let mut agent = AgentState::stub("claude", "child", AgentStatus::Idle);
    agent.registered_at = Some(Timestamp::from_second(10).unwrap());
    agent.parent_agent_id = Some("parent".into());
    agent.launch_depth = Some(1);
    let err = prepare_raise(&paths, &agent, true, Some("Ship?".into()), None, now).unwrap_err();
    assert!(matches!(err, RaisedErr::SubagentSelfRaise), "{err:?}");
    assert_eq!(
        err.to_string(),
        "a subagent cannot ask the user: make the question your result and end your turn, and \
         the agent that launched you answers it"
    );
    let outside = prepare_raise(&paths, &agent, false, Some("Review?".into()), None, now).unwrap();
    assert_eq!(outside.value.state, RaisedAskState::Open);
    assert_eq!(outside.value.brief.as_deref(), Some("Review?"));
}
