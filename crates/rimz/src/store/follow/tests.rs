use super::*;
use crate::disk::paths::RuntimePaths;
use crate::ids::WorkspaceId;
use crate::store::{Store, writer::AgentLifecycleIntent};

fn fixture() -> (tempfile::TempDir, Store, StatePaths) {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(workspace_id, dir.path()).unwrap();
    let store = Store::open(paths.clone(), runtime).unwrap();
    (dir, store, paths)
}

fn append(store: &Store, signal: LifecycleSignal) {
    append_for(store, "session-1", signal);
}

fn append_for(store: &Store, id: &str, signal: LifecycleSignal) {
    let observation =
        crate::agents::AgentLifecycleObservation::new(Some(AgentSessionId::from(id)), signal);
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "room",
            agent_kind: AgentKind::new_unchecked("claude"),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
}

#[test]
fn side_registration_emits_no_lifecycle_or_follow_state() {
    let (_dir, _store, paths) = fixture();
    let mut follower = EventFollower::open(paths.clone(), true).unwrap();
    let mut observation = crate::agents::AgentLifecycleObservation::new(
        Some(AgentSessionId::from("side")),
        LifecycleSignal::Registered,
    );
    observation.origin = Some(SessionOrigin::SideConversation);
    let event = crate::store::event::EventEnvelope::agent_lifecycle(
        paths.workspace_id.clone(),
        "room",
        "codex",
        "SessionStart",
        &observation,
    );
    assert!(follower.fold(vec![event]).is_empty());
    assert!(follower.states.is_empty());
}

fn observe(follower: &mut EventFollower, id: &str, signal: LifecycleSignal) -> LifecycleEvent {
    let observation =
        crate::agents::AgentLifecycleObservation::new(Some(AgentSessionId::from(id)), signal);
    let envelope = crate::store::event::EventEnvelope::agent_lifecycle(
        follower.paths.workspace_id.clone(),
        "room",
        "claude",
        "test",
        &observation,
    );
    let FollowEvent::Lifecycle(event) = follower.fold(vec![envelope]).remove(0) else {
        panic!("lifecycle")
    };
    event
}

#[test]
fn replay_retains_only_the_latest_256_ended_states_and_all_live_states() {
    let (_dir, _store, paths) = fixture();
    let mut follower = EventFollower::open(paths, true).unwrap();
    for index in 0..300 {
        let id = format!("ended-{index}");
        observe(&mut follower, &id, LifecycleSignal::Registered);
        observe(&mut follower, &id, LifecycleSignal::Ended);
    }
    assert_eq!(follower.states.len(), 256);
    observe(&mut follower, "live", LifecycleSignal::Registered);
    for index in 300..600 {
        let id = format!("ended-{index}");
        observe(&mut follower, &id, LifecycleSignal::Registered);
        observe(&mut follower, &id, LifecycleSignal::Ended);
    }
    assert_eq!(follower.states.len(), 257);
    assert!(
        observe(&mut follower, "live", LifecycleSignal::Registered)
            .prior_status
            .is_some()
    );
}

#[test]
fn a_recently_ended_agent_keeps_its_prior_status_on_return() {
    use crate::agents::AgentStatus;

    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (event_name, active_status) in [
        ("ReapedSuperseded", AgentStatus::Idle),
        ("ReapedDead", AgentStatus::Idle),
        ("WorktreeRemoved", AgentStatus::Idle),
        ("SessionEnd", AgentStatus::Failed),
    ] {
        for (signal, prior_status, status) in [
            (
                LifecycleSignal::Registered,
                AgentStatus::Idle,
                AgentStatus::Idle,
            ),
            (
                LifecycleSignal::TurnStarted { turn_id: None },
                AgentStatus::Running,
                active_status,
            ),
            (
                LifecycleSignal::AwaitingInput {
                    kind: crate::agents::AskKind::Permission,
                    ask_id: None,
                    detail: None,
                    native_key: None,
                },
                AgentStatus::Waiting,
                active_status,
            ),
        ] {
            let (_dir, store, paths) = fixture();
            append(&store, signal);
            let live = EventFollower::open(paths.clone(), false).unwrap();
            let observation = crate::agents::AgentLifecycleObservation::new(
                Some("session-1".into()),
                LifecycleSignal::Ended,
            );
            store
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "room",
                    agent_kind: AgentKind::new_unchecked("claude"),
                    event_name,
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .unwrap();
            let replay = EventFollower::open(paths, true).unwrap();
            for (replay, mut follower) in [(false, live), (true, replay)] {
                let FollowEvent::Lifecycle(ended) = follower.poll().unwrap().events.pop().unwrap()
                else {
                    panic!("lifecycle")
                };
                let retained = follower.ended.len();
                let resumed = observe(&mut follower, "session-1", LifecycleSignal::Registered);
                actual.push((
                    event_name,
                    replay,
                    ended.prior_status,
                    ended.status,
                    retained,
                    resumed.prior_status,
                ));
                expected.push((
                    event_name,
                    replay,
                    Some(prior_status),
                    status,
                    1,
                    Some(status),
                ));
            }
        }
    }
    assert_eq!(
        actual, expected,
        "reaper ends rest at idle; only observed active ends fail"
    );
}

#[test]
fn an_agent_evicted_after_256_later_endings_returns_without_prior_status() {
    let (_dir, _store, paths) = fixture();
    let mut follower = EventFollower::open(paths, true).unwrap();
    observe(&mut follower, "returning", LifecycleSignal::Registered);
    observe(&mut follower, "returning", LifecycleSignal::Ended);
    for index in 0..256 {
        let id = format!("ended-{index}");
        observe(&mut follower, &id, LifecycleSignal::Registered);
        observe(&mut follower, &id, LifecycleSignal::Ended);
    }
    let resumed = observe(&mut follower, "returning", LifecycleSignal::Registered);
    assert_eq!(resumed.prior_status, None);
}

#[test]
fn live_seed_evicts_the_earliest_endings_not_the_lowest_keys() {
    let (_dir, store, paths) = fixture();
    for index in 0..ENDED_STATES_RETAINED {
        let id = format!("b-{index:03}");
        append_for(&store, &id, LifecycleSignal::Registered);
        append_for(&store, &id, LifecycleSignal::Ended);
    }
    std::thread::sleep(std::time::Duration::from_millis(5));
    append_for(&store, "a-latest", LifecycleSignal::Registered);
    append_for(&store, "a-latest", LifecycleSignal::Ended);
    let follower = EventFollower::open(paths, false).unwrap();
    let claude = AgentKind::new_unchecked("claude");
    assert_eq!(follower.states.len(), ENDED_STATES_RETAINED);
    assert!(
        follower
            .states
            .contains_key(&(claude.clone(), AgentSessionId::from("a-latest")))
    );
    assert!(
        !follower
            .states
            .contains_key(&(claude, AgentSessionId::from("b-000")))
    );
}

#[test]
fn replay_folds_the_active_generation_and_live_mode_starts_at_the_edge() {
    let (_dir, store, paths) = fixture();
    append(&store, LifecycleSignal::Registered);
    let mut live = EventFollower::open(paths.clone(), false).unwrap();
    assert!(live.poll().unwrap().events.is_empty());

    let mut replay = EventFollower::open(paths, true).unwrap();
    let batch = replay.poll().unwrap();
    assert_eq!(batch.events.len(), 1);
    let FollowEvent::Lifecycle(event) = &batch.events[0] else {
        panic!("lifecycle")
    };
    assert_eq!(event.status, crate::agents::AgentStatus::Idle);
}

#[test]
fn snapshots_update_live_and_replayed_state_without_emitting_edges_or_changing_turn_ids() {
    use crate::agents::AgentStatus;

    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for replay in [false, true] {
        for (status, idle_status) in [
            (None, AgentStatus::Idle),
            (Some(AgentStatus::Idle), AgentStatus::Idle),
            (Some(AgentStatus::Success), AgentStatus::Success),
            (Some(AgentStatus::Failed), AgentStatus::Failed),
            (Some(AgentStatus::Running), AgentStatus::Idle),
            (Some(AgentStatus::Waiting), AgentStatus::Idle),
        ] {
            for (running, next_status) in [(false, idle_status), (true, AgentStatus::Running)] {
                let (_dir, store, paths) = fixture();
                if let Some(status) = status {
                    for id in ["previous", "current"] {
                        append(
                            &store,
                            LifecycleSignal::TurnStarted {
                                turn_id: Some(id.into()),
                            },
                        );
                    }
                    let signal = match status {
                        AgentStatus::Idle => LifecycleSignal::Registered,
                        AgentStatus::Success | AgentStatus::Failed => LifecycleSignal::TurnEnded {
                            errored: status == AgentStatus::Failed,
                            parked_on_background: false,
                            turn_id: Some("current".into()),
                        },
                        AgentStatus::Running => LifecycleSignal::Compacting,
                        AgentStatus::Waiting => LifecycleSignal::AwaitingInput {
                            kind: crate::agents::AskKind::Permission,
                            ask_id: Some(crate::ids::AskId::new()),
                            detail: None,
                            native_key: Some("call".into()),
                        },
                        AgentStatus::Paused | AgentStatus::Sleeping => {
                            unreachable!("fixture uses durable lifecycle statuses")
                        }
                    };
                    append(&store, signal);
                }
                let mut follower = EventFollower::open(paths, replay).unwrap();
                follower.poll().unwrap();
                let key = (AgentKind::new_unchecked("claude"), "session-1".into());
                let prior = follower.states.get(&key).cloned();
                let ids = prior
                    .as_ref()
                    .map(|state| {
                        (
                            state.started_turn_id.clone(),
                            state.superseded_turn_id.clone(),
                            state.interrupted_turn_id.clone(),
                        )
                    })
                    .unwrap_or_default();
                let mut observation = crate::agents::AgentLifecycleObservation::new(
                    Some("session-1".into()),
                    if running {
                        LifecycleSignal::TurnStarted { turn_id: None }
                    } else {
                        LifecycleSignal::Registered
                    },
                );
                observation.state_snapshot = Some(true);
                store
                    .append_agent_lifecycle(AgentLifecycleIntent {
                        session_name: "room",
                        agent_kind: key.0.clone(),
                        event_name: "snapshot",
                        observation: &observation,
                        spawned_subagents: &[],
                    })
                    .unwrap();
                let batch = follower.poll().unwrap();
                let state = follower.states[&key].clone();
                append(
                    &store,
                    LifecycleSignal::ToolUsed {
                        mutates: true,
                        edits: false,
                        name: Some("bash".into()),
                        native_key: None,
                        turn_id: Some("current".into()),
                    },
                );
                let edges = follower.poll().unwrap().events;
                let FollowEvent::Lifecycle(edge) = &edges[0] else {
                    panic!("lifecycle")
                };
                actual.push((
                    replay,
                    status,
                    running,
                    batch.events.len(),
                    state.lifecycle.status,
                    state.lifecycle.compacting,
                    (
                        state.started_turn_id,
                        state.superseded_turn_id,
                        state.interrupted_turn_id,
                    ),
                    state.open_ask_key,
                    edge.prior_status,
                ));
                expected.push((
                    replay,
                    status,
                    running,
                    0,
                    next_status,
                    prior.is_some_and(|state| state.lifecycle.compacting),
                    ids,
                    None,
                    Some(next_status),
                ));
            }
        }
    }
    assert_eq!(
        actual, expected,
        "snapshots update private state, never lifecycle edges"
    );
}

#[test]
fn follower_drains_the_rotated_tail_before_the_new_active_log() {
    let (_dir, store, paths) = fixture();
    append(&store, LifecycleSignal::Registered);
    let mut follower = EventFollower::open(paths, false).unwrap();
    append(&store, LifecycleSignal::TurnStarted { turn_id: None });
    assert_eq!(follower.poll().unwrap().events.len(), 1);

    store.rotate_event_log(1, None).unwrap();
    append(
        &store,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    let batch = follower.poll().unwrap();
    assert!(batch.warnings.is_empty());
    assert_eq!(batch.events.len(), 1);
    let FollowEvent::Lifecycle(event) = &batch.events[0] else {
        panic!("lifecycle")
    };
    assert_eq!(
        event.prior_status,
        Some(crate::agents::AgentStatus::Running)
    );
    assert_eq!(event.status, crate::agents::AgentStatus::Success);
}

#[test]
fn launch_tail_preserves_seed_through_rotation_before_first_poll() {
    let (_dir, store, paths) = fixture();
    append(&store, LifecycleSignal::Registered);
    let cursor = event_log::LogExtent {
        generation: snapshot::lifecycle_log_generation(&paths),
        offset: fs::metadata(&paths.events_log).unwrap().len(),
    };
    append(&store, LifecycleSignal::Ended);
    store.rotate_event_log(1, None).unwrap();
    append(&store, LifecycleSignal::Registered);
    let before = snapshot::fold_testkit::carryover_bytes_parsed();
    let mut tail = LaunchTail::from_cursor(paths, cursor);
    let mut signals = Vec::new();
    let mut visit = |event: crate::store::event::EventEnvelope| {
        if let EventKind::AgentLifecycle(payload) = event.kind() {
            signals.push(payload.observation.signal);
        }
    };
    assert!(tail.poll(&mut visit).unwrap().is_empty());
    tail.poll(&mut visit).unwrap();
    assert_eq!(
        signals,
        vec![LifecycleSignal::Ended, LifecycleSignal::Registered]
    );
    assert_eq!(snapshot::fold_testkit::carryover_bytes_parsed(), before);
}

#[test]
fn poll_before_the_generation_bump_waits_then_drains_the_tail_once() {
    let (_dir, store, paths) = fixture();
    append(&store, LifecycleSignal::Registered);
    let mut follower = EventFollower::open(paths.clone(), false).unwrap();
    append(&store, LifecycleSignal::TurnStarted { turn_id: None });
    let cursor = follower.cursor.offset;

    // Rotation renames the active log before the commit boundary bumps
    // the generation: replay that window by restoring the old marker. The
    // short-active-log guard answers it; the rename racing the open
    // itself is pinned in `read_from_offset_resume_cases`.
    let old_rollup = fs::read(&paths.rollup_cache).unwrap();
    store.rotate_event_log(1, None).unwrap();
    let new_rollup = fs::read(&paths.rollup_cache).unwrap();
    fs::write(&paths.rollup_cache, &old_rollup).unwrap();
    assert!(!paths.events_log.exists());
    assert!(follower.poll().unwrap().events.is_empty());
    assert_eq!(follower.cursor.offset, cursor);

    fs::write(&paths.rollup_cache, &new_rollup).unwrap();
    let batch = follower.poll().unwrap();
    assert!(batch.warnings.is_empty());
    assert_eq!(batch.events.len(), 1);
    let FollowEvent::Lifecycle(event) = &batch.events[0] else {
        panic!("lifecycle")
    };
    assert_eq!(event.status, crate::agents::AgentStatus::Running);
}

#[test]
fn late_report_for_a_superseded_turn_is_ignored_from_the_seed_and_the_log() {
    let (_dir, store, paths) = fixture();
    let start = |id: &str| LifecycleSignal::TurnStarted {
        turn_id: Some(id.to_owned()),
    };
    append(&store, start("prompt-1"));
    append(&store, start("prompt-2"));
    let mut seeded = EventFollower::open(paths.clone(), false).unwrap();
    let late = LifecycleSignal::TurnInterrupted {
        turn_id: Some("prompt-1".to_owned()),
    };
    append(&store, late);

    let seeded_events = seeded.poll().unwrap().events;
    let replayed_events = EventFollower::open(paths, true)
        .unwrap()
        .poll()
        .unwrap()
        .events;
    for events in [seeded_events, replayed_events] {
        let FollowEvent::Lifecycle(late) = events.last().unwrap() else {
            panic!("lifecycle")
        };
        assert!(
            matches!(
                late.transition,
                crate::agents::LifecycleTransition::Ignored { .. }
            ),
            "{late:?}"
        );
        assert_eq!(late.status, crate::agents::AgentStatus::Running);
    }
}

#[test]
fn adapter_ask_without_id_does_not_hold_a_sibling_tool_waiting() {
    let (_dir, store, paths) = fixture();
    append(&store, LifecycleSignal::Registered);
    append(&store, LifecycleSignal::TurnStarted { turn_id: None });
    append(
        &store,
        LifecycleSignal::AwaitingInput {
            kind: crate::agents::AskKind::Permission,
            ask_id: None,
            detail: None,
            native_key: Some("ask-call".to_owned()),
        },
    );
    append(
        &store,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            name: None,
            native_key: Some("sibling-call".to_owned()),
            turn_id: None,
        },
    );

    let mut follower = EventFollower::open(paths, true).unwrap();
    let events = follower.poll().unwrap().events;
    let FollowEvent::Lifecycle(tool) = events.last().unwrap() else {
        panic!("lifecycle")
    };
    assert_eq!(tool.prior_status, Some(crate::agents::AgentStatus::Waiting));
    assert_eq!(tool.status, crate::agents::AgentStatus::Running);
    assert_eq!(tool.transition, crate::agents::LifecycleTransition::Normal);
    assert!(tool.waiting_cleared);
}
