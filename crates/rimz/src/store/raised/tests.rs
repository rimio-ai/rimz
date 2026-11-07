use super::*;
use crate::agents::AgentStatus;
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
            options: vec!["A".into(), "B".into()],
        },
    }
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
