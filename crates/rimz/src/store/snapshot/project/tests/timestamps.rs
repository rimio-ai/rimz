use super::*;

#[test]
fn finished_clock_survives_reap_reconciliation_and_ping_but_restarts_with_a_turn() {
    let mut payload = launch_payload("child", "worker");
    payload.launch.parent_agent_id = Some("parent".into());
    payload.launch.launch_depth = Some(1);
    let mut launch = launch_event("claude", payload);
    launch.timestamp = epoch();
    let mut events = vec![launch];
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (offset, signal, event_name, prompt, finish) in [
        (
            1,
            json!({"signal": "turn_ended", "errored": false,
            "parked_on_background": false}),
            "Stop",
            None,
            Some(1),
        ),
        (2, json!({"signal": "ended"}), "ReapedDead", None, Some(1)),
        (
            3,
            json!({"signal": "turn_started"}),
            "Prompt",
            Some("next"),
            None,
        ),
        (
            4,
            json!({"signal": "turn_ended", "errored": false,
            "parked_on_background": false}),
            "Stop",
            None,
            Some(4),
        ),
        (
            5,
            json!({"signal": "turn_started"}),
            "Prompt",
            Some("Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nKeep warm."),
            Some(4),
        ),
        (
            6,
            json!({"signal": "turn_ended", "errored": false,
            "parked_on_background": false}),
            "Stop",
            None,
            Some(4),
        ),
    ] {
        events.push(raw_lifecycle_at(
            "claude",
            offset,
            json!({
                "agent_id": "child", "event_name": event_name, "prompt": prompt,
                "signal": signal,
            }),
        ));
        let row = reduce_agent_states(&events).remove(0);
        actual.push(serde_json::to_value(row).unwrap()["finished_at"].clone());
        expected.push(finish.map(|secs| epoch() + std::time::Duration::from_secs(secs)));
    }
    let mut native = Vec::new();
    for (offset, signal, finish) in [
        (1, json!({"signal": "subagent_started"}), None),
        (2, json!({"signal": "subagent_stopped"}), Some(2)),
        (
            3,
            json!({"signal": "subagent_stopped", "errored": true}),
            Some(2),
        ),
    ] {
        native.push(raw_lifecycle_at(
            "claude",
            offset,
            json!({
                "agent_id": "child", "parent_agent_id": "parent", "task": "Explore",
                "event_name": "SubagentReconciled", "total_tokens": offset * 100,
                "signal": signal,
            }),
        ));
        let row = reduce_agent_states(&native).remove(0);
        actual.push(serde_json::to_value(row).unwrap()["finished_at"].clone());
        expected.push(finish.map(|secs| epoch() + std::time::Duration::from_secs(secs)));
    }
    assert_eq!(
        actual,
        expected
            .into_iter()
            .map(|stamp| json!(stamp))
            .collect::<Vec<_>>()
    );
}

#[test]
fn finished_clock_uses_the_final_status_on_ends_launches_and_state_snapshots() {
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for event_name in ["SessionEnd", "ReapedDead"] {
        let events = [
            raw_lifecycle_at(
                "claude",
                1,
                json!({
                    "agent_id": "child", "signal": {"signal": "turn_started"},
                }),
            ),
            raw_lifecycle_at(
                "claude",
                2,
                json!({
                    "agent_id": "child", "event_name": event_name,
                    "signal": {"signal": "ended"},
                }),
            ),
        ];
        let row = reduce_agent_states(&events).remove(0);
        assert_eq!(
            row.status,
            if event_name == "SessionEnd" {
                AgentStatus::Failed
            } else {
                AgentStatus::Idle
            }
        );
        actual.push(serde_json::to_value(row).unwrap()["finished_at"].clone());
        expected.push((event_name == "SessionEnd").then_some(events[1].timestamp));
    }
    let mut payload = launch_payload("child", "worker");
    let mut starting = launch_event("claude", payload.clone());
    starting.timestamp = epoch();
    payload.state = AgentLaunchState::Failed;
    let mut launch = launch_event("claude", payload);
    launch.timestamp = epoch() + std::time::Duration::from_secs(1);
    let row = reduce_agent_states(&[starting, launch]).remove(0);
    actual.push(serde_json::to_value(row).unwrap()["finished_at"].clone());
    expected.push(Some(epoch() + std::time::Duration::from_secs(1)));

    let mut events = vec![raw_lifecycle_at(
        "opencode",
        1,
        json!({
            "agent_id": "child", "signal": {"signal": "turn_ended", "errored": false,
                "parked_on_background": false},
        }),
    )];
    for (offset, signal, finish) in [(2, "registered", Some(1)), (3, "turn_started", None)] {
        events.push(raw_lifecycle_at(
            "opencode",
            offset,
            json!({
                "agent_id": "child", "state_snapshot": true, "signal": {"signal": signal},
            }),
        ));
        let row = reduce_agent_states(&events).remove(0);
        actual.push(serde_json::to_value(row).unwrap()["finished_at"].clone());
        expected.push(finish.map(|secs| epoch() + std::time::Duration::from_secs(secs)));
    }
    assert_eq!(
        actual,
        expected
            .into_iter()
            .map(|stamp| json!(stamp))
            .collect::<Vec<_>>()
    );
}

#[test]
fn closed_turn_id_reads_the_legacy_key_and_writes_the_new_key() {
    let row = crate::testkit::agent_state("codex", "session", epoch());
    let mut legacy = serde_json::to_value(row).unwrap();
    legacy["interrupted_turn_id"] = json!("closed");
    let restored: AgentState = serde_json::from_value(legacy).unwrap();
    let serialized = serde_json::to_value(restored).unwrap();
    assert_eq!(serialized["closed_turn_id"], json!("closed"));
    assert!(serialized.get("interrupted_turn_id").is_none());
}

#[test]
fn a_closed_turns_trailing_tool_leaves_a_ping_turn_open() {
    let events = [
        raw_lifecycle_at(
            "codex",
            1,
            json!({
                "agent_id": "session", "signal": {"signal": "turn_started"}
            }),
        ),
        raw_lifecycle_at(
            "codex",
            2,
            json!({
                "agent_id": "session", "signal": {
                    "signal": "turn_ended", "errored": false,
                    "parked_on_background": false, "turn_id": "closed"
                }
            }),
        ),
        raw_lifecycle_at(
            "codex",
            3,
            json!({
                "agent_id": "session",
                "prompt": "Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nKeep warm.",
                "signal": {"signal": "turn_started"}
            }),
        ),
        raw_lifecycle_at(
            "codex",
            4,
            json!({
                "agent_id": "session", "signal": {
                    "signal": "tool_used", "mutates": true, "name": "Bash", "turn_id": "closed"
                }
            }),
        ),
    ];
    let rows = reduce_agent_states(&events);
    let row = &rows[0];
    assert!(row.ping_turn);
    assert_eq!(
        (row.status, row.phase),
        (AgentStatus::Success, TurnPhase::Idle)
    );
    assert_eq!(row.turn_started_at, Some(events[0].timestamp));
}

#[test]
fn client_snapshots_fold_status_without_resetting_turn_evidence() {
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    let evidence = |row: &AgentState| {
        let value = serde_json::to_value(row).unwrap();
        [
            "status",
            "phase",
            "turn_started_at",
            "user_turn_started_at",
            "turn_ended_at",
            "last_tool_at",
            "keepalive_since",
            "started_turn_id",
            "superseded_turn_id",
            "closed_turn_id",
            "compacting_since",
            "compaction_count",
            "compaction_retired",
            "tool_calls",
            "background_shells",
            "registered_at",
            "waiting_since",
            "open_ask",
            "ended_at",
            "last_activity",
            "on_screen_at",
            "total_tokens",
        ]
        .map(|key| value[key].clone())
    };
    for status in [
        None,
        Some(AgentStatus::Idle),
        Some(AgentStatus::Success),
        Some(AgentStatus::Failed),
        Some(AgentStatus::Running),
        Some(AgentStatus::Waiting),
    ] {
        for running in [false, true] {
            let prior = status.map(|status| {
                let mut row = AgentState::seed(
                    AgentKind::new_unchecked("opencode"),
                    "session".into(),
                    status,
                    epoch(),
                );
                row.turn_started_at = Some(ago(20));
                row.user_turn_started_at = Some(ago(30));
                row.turn_ended_at = Some(ago(25));
                row.last_tool_at = Some(ago(5));
                row.keepalive_since = Some(ago(40));
                row.started_turn_id = Some("current".into());
                row.superseded_turn_id = Some("previous".into());
                row.closed_turn_id = Some("interrupted".into());
                row.compacting_since = Some(ago(2));
                row.compaction_count = 3;
                row.compaction_retired = true;
                row.waiting_since = Some(ago(4));
                row.open_ask = Some(crate::agents::OpenAsk {
                    id: crate::ids::AskId::new(),
                    kind: lifecycle::AskKind::Permission,
                    detail: None,
                    native_key: None,
                    since: ago(4),
                });
                row.tool_calls.insert("bash".into(), 2);
                row.background_shells.push(crate::agents::BackgroundShell {
                    id: "shell".into(),
                    command: Some("build".into()),
                    description: None,
                    started_at: ago(10),
                });
                row.ended_at = Some(ago(1));
                row
            });
            let event = raw_lifecycle_at(
                "opencode",
                1,
                json!({
                    "agent_id": "session", "state_snapshot": true, "on_screen": true,
                    "signal": {"signal": if running { "turn_started" } else { "registered" }},
                    "total_tokens": 1234
                }),
            );
            let states = reduce_agent_states_seeded(
                prior
                    .iter()
                    .map(|row| ((row.kind.clone(), row.agent_id.clone()), row.clone()))
                    .collect(),
                std::slice::from_ref(&event),
            );
            let row = states.values().next().unwrap();
            let opens =
                running && !matches!(status, Some(AgentStatus::Running | AgentStatus::Waiting));
            let next = if running {
                AgentStatus::Running
            } else {
                match status {
                    Some(AgentStatus::Success | AgentStatus::Failed) => status.unwrap(),
                    _ => AgentStatus::Idle,
                }
            };
            let mut wanted = prior.clone().unwrap_or_else(|| {
                AgentState::seed(
                    AgentKind::new_unchecked("opencode"),
                    "session".into(),
                    AgentStatus::Idle,
                    event.timestamp,
                )
            });
            if wanted.status != next {
                wanted.phase = if running {
                    TurnPhase::Reasoning
                } else {
                    TurnPhase::Idle
                };
            }
            wanted.status = next;
            if opens {
                wanted.turn_started_at = Some(event.timestamp);
            }
            wanted.waiting_since = None;
            wanted.open_ask = None;
            wanted.ended_at = None;
            wanted.last_activity = event.timestamp;
            wanted.on_screen_at = Some(event.timestamp);
            wanted.usage.total_tokens = Some(1234);
            actual.push((status, running, evidence(row)));
            expected.push((status, running, evidence(&wanted)));
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn on_screen_stamp_survives_unstamped_events_and_never_follows_identity() {
    let mut payload = launch_payload("session", "screen");
    payload.launch_id = Some("launch".into());
    payload.pane_id = Some(PaneId::parse("tmux:%1").unwrap());
    payload.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "client",
        42,
        None,
    ));
    let mut launch = launch_event("opencode", payload);
    launch.timestamp = epoch();
    let mut events = vec![
        launch,
        raw_lifecycle_at(
            "opencode",
            0,
            json!({
                "agent_id": "session", "signal": {"signal": "turn_started"}, "on_screen": true,
                "pane_id": "tmux:%1", "agent_pid": 42
            }),
        ),
    ];
    let stamp = events[0].timestamp;
    let stamped = events.clone();
    let mut stamps = Vec::new();
    for (offset, signal, on_screen) in [
        (1, "registered", json!(false)),
        (2, "ended", json!(null)),
        (3, "turn_started", json!(null)),
    ] {
        events.push(raw_lifecycle_at(
            "opencode",
            offset,
            json!({
                "agent_id": "session", "event_name": "ReapedSuperseded",
                "signal": {"signal": signal}, "on_screen": on_screen
            }),
        ));
        let rows = reduce_agent_states(&events);
        let cached: AgentState =
            serde_json::from_value(serde_json::to_value(&rows[0]).unwrap()).unwrap();
        stamps.push(cached.on_screen_at);
    }
    for id in ["successor", "continuation"] {
        events.push(raw_lifecycle_at(
            "opencode",
            4,
            json!({
                "agent_id": id, "signal": {"signal": "registered"},
                "pane_id": "tmux:%1", "agent_pid": 42,
                "compacted_from": if id == "continuation" { Some("session") } else { None }
            }),
        ));
    }
    let rows = reduce_agent_states(&events);
    let successors: Vec<_> = rows
        .iter()
        .filter(|row| row.agent_id != "session")
        .map(|row| (row.launch_id.as_deref(), row.on_screen_at))
        .collect();
    let mut old = serde_json::to_value(&rows[0]).unwrap();
    old.as_object_mut().unwrap().remove("on_screen_at");
    let old: AgentState = serde_json::from_value(old).unwrap();
    assert_eq!(
        (stamps, successors, old.on_screen_at),
        (vec![Some(stamp); 3], vec![(Some("launch"), None); 2], None)
    );

    let mut payload = launch_payload("session", "screen");
    payload.pane_id = Some(PaneId::parse("tmux:%2").unwrap());
    payload.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "session",
        43,
        None,
    ));
    let moves = [
        raw_lifecycle_at(
            "opencode",
            1,
            json!({"agent_id": "session", "signal": {"signal": "registered"},
                "pane_id": "tmux:%2", "agent_pid": 42}),
        ),
        raw_lifecycle_at(
            "opencode",
            1,
            json!({"agent_id": "session", "signal": {"signal": "registered"},
                "pane_id": "tmux:%1", "agent_pid": 43}),
        ),
        launch_event("opencode", payload),
        attach_event("opencode", "session", None, "tmux:%2", None, 43),
        EventEnvelope::session_rebirth(workspace(), "session"),
    ];
    let moved_stamps = moves.map(|event| {
        let mut events = stamped.clone();
        events.push(event);
        reduce_agent_states(&events)[0].on_screen_at
    });
    assert_eq!(
        moved_stamps, [None; 5],
        "unstamped placement changes retire the old instance's on-screen claim"
    );
}

#[test]
fn stamped_real_edges_still_open_and_finish_a_turn() {
    let events = [
        raw_lifecycle_at(
            "opencode",
            0,
            json!({"agent_id": "session", "on_screen": true,
            "signal": {"signal": "turn_started"}, "prompt": "work"}),
        ),
        raw_lifecycle_at(
            "opencode",
            1,
            json!({"agent_id": "session", "on_screen": true,
            "signal": {"signal": "turn_ended", "errored": false, "parked_on_background": false}}),
        ),
    ];
    let rows = reduce_agent_states(&events);
    assert_eq!(
        (
            rows[0].status,
            rows[0].turn_started_at,
            rows[0].turn_ended_at,
            rows[0].on_screen_at
        ),
        (
            AgentStatus::Success,
            Some(events[0].timestamp),
            Some(events[1].timestamp),
            Some(events[1].timestamp)
        )
    );
}

#[test]
fn registered_prompted_child_is_silent_even_after_registration_clears_its_task() {
    for prompted in [false, true] {
        let mut payload = launch_payload("child", "still-silver");
        payload.launch.parent_agent_id = Some("parent".into());
        payload.launch.launch_depth = Some(1);
        payload.prompt = prompted.then(|| "gate the release".to_owned());
        let mut launch = launch_event("codex", payload);
        launch.timestamp = epoch();
        let registered = raw_lifecycle_at(
            "codex",
            1,
            json!({
                "agent_id": "child", "signal": {"signal": "registered"}
            }),
        );
        let states = reduce_agent_states(&[launch, registered.clone()]);
        let child = &states[0];
        assert_eq!(child.status, AgentStatus::Idle);
        assert_eq!(
            child.task, None,
            "task is activity-bound, not a launch marker"
        );
        assert_eq!(child.prompt.is_some(), prompted);
        assert_eq!(
            child.turn_started_at,
            prompted.then_some(registered.timestamp)
        );
        assert_eq!(child.turn_ended_at, None);
        assert_eq!(
            child.silent_child_for(epoch() + std::time::Duration::from_secs(3_601), 1_800),
            prompted.then_some((3_600, crate::agents::state::ChildStall::NeverStarted))
        );
    }
}

#[test]
fn a_launched_child_that_completes_its_first_turn_is_not_silent() {
    let mut payload = launch_payload("child", "still-silver");
    payload.launch.parent_agent_id = Some("parent".into());
    payload.launch.launch_depth = Some(1);
    let mut events = vec![launch_event("codex", payload)];
    events[0].timestamp = epoch();
    for (offset, signal) in [
        (1, json!({"signal": "registered"})),
        (2, json!({"signal": "turn_started"})),
        (
            3,
            json!({"signal": "turn_ended", "errored": false, "parked_on_background": false}),
        ),
    ] {
        events.push(raw_lifecycle_at(
            "codex",
            offset,
            json!({"agent_id": "child", "signal": signal}),
        ));
    }
    let states = reduce_agent_states(&events);
    let child = &states[0];
    assert_eq!(child.status, AgentStatus::Success);
    assert_eq!(child.turn_ended_at, Some(events[3].timestamp));
    assert_eq!(
        child.silent_child_for(epoch() + std::time::Duration::from_secs(3_603), 1_800),
        None
    );
}

#[test]
fn resumed_stamp_clears_on_the_next_lifecycle_and_survives_cache_roundtrip() {
    let resumed = raw_lifecycle_at(
        "claude",
        1,
        json!({
            "agent_id": "session-a", "event_name": "rimz.agent-resumed",
            "signal": {"signal": "registered"}
        }),
    );
    let states = reduce_agent_states(std::slice::from_ref(&resumed));
    assert_eq!(states[0].resumed_at, Some(resumed.timestamp));
    let cached: AgentState =
        serde_json::from_value(serde_json::to_value(&states[0]).unwrap()).unwrap();
    assert_eq!(cached.resumed_at, Some(resumed.timestamp));
    for signal in ["registered", "ended", "turn_started"] {
        let next = raw_lifecycle_at(
            "claude",
            2,
            json!({
                "agent_id": "session-a", "event_name": "provider-hook",
                "signal": {"signal": signal}
            }),
        );
        let states = reduce_agent_states_seeded(
            BTreeMap::from([(
                (cached.kind.clone(), cached.agent_id.clone()),
                cached.clone(),
            )]),
            &[next],
        );
        assert_eq!(states.values().next().unwrap().resumed_at, None, "{signal}");
    }
}

#[test]
fn turn_ids_carry_supersede_and_clear_through_the_fold() {
    let mut events = Vec::new();
    for (signal, expected) in [
        (
            json!({"signal": "turn_started", "turn_id": "a"}),
            (Some("a"), None, None),
        ),
        (
            json!({"signal": "turn_started", "turn_id": "a"}),
            (Some("a"), None, None),
        ),
        (
            json!({"signal": "turn_started", "turn_id": "b"}),
            (Some("b"), Some("a"), None),
        ),
        (
            json!({"signal": "turn_interrupted", "turn_id": "b"}),
            (Some("b"), Some("a"), Some("b")),
        ),
        (
            json!({"signal": "tool_used", "mutates": false, "turn_id": "b"}),
            (Some("b"), Some("a"), Some("b")),
        ),
        (
            json!({"signal": "turn_started", "turn_id": "c"}),
            (Some("c"), Some("b"), None),
        ),
        (
            json!({"signal": "turn_interrupted", "turn_id": "c"}),
            (Some("c"), Some("b"), Some("c")),
        ),
        (
            json!({"signal": "registered"}),
            (Some("c"), Some("b"), None),
        ),
    ] {
        events.push(raw_lifecycle_at(
            "codex",
            events.len() as i64,
            json!({
                "agent_id": "session-a", "signal": signal,
            }),
        ));
        let agents = reduce_agent_states(&events);
        let state = &agents[0];
        assert_eq!(
            (
                state.started_turn_id.as_deref(),
                state.superseded_turn_id.as_deref(),
                state.closed_turn_id.as_deref()
            ),
            expected,
            "event {}",
            events.len()
        );
    }
}

#[test]
fn launch_carries_or_restamps_start_clocks_but_drops_end_clock() {
    let mut prior = crate::testkit::agent_state("codex", "launch-a", epoch());
    prior.turn_started_at = Some(epoch());
    prior.user_turn_started_at = Some(epoch() + jiff::SignedDuration::from_secs(1));
    prior.turn_ended_at = Some(epoch() + jiff::SignedDuration::from_secs(2));
    for prompted in [false, true] {
        let mut payload = launch_payload("launch-a", "lucid-atlas");
        payload.prompt = prompted.then(|| "boot".to_owned());
        let mut event = launch_event("codex", payload);
        event.timestamp = epoch() + jiff::SignedDuration::from_secs(3);
        let states = reduce_agent_states_seeded(
            BTreeMap::from([((prior.kind.clone(), prior.agent_id.clone()), prior.clone())]),
            &[event.clone()],
        );
        let state = states.values().next().unwrap();
        assert_eq!(
            state.turn_started_at,
            if prompted {
                Some(event.timestamp)
            } else {
                prior.turn_started_at
            }
        );
        assert_eq!(
            state.user_turn_started_at,
            if prompted {
                Some(event.timestamp)
            } else {
                prior.user_turn_started_at
            }
        );
        // Reported launch asymmetry, not endorsed: lifecycle registration carries this clock.
        assert_eq!(state.turn_ended_at, None);
    }
}

#[test]
fn turn_end_clock_tracks_completions_not_resume_or_tools() {
    let mut events = Vec::new();
    let mut expected = None;
    for (offset, signal, completes) in [
        (0, json!({"signal": "registered"}), false),
        (1, json!({"signal": "turn_started"}), false),
        (
            2,
            json!({"signal": "turn_ended", "errored": false, "parked_on_background": false}),
            true,
        ),
        (3, json!({"signal": "registered"}), false),
        (
            4,
            json!({"signal": "tool_used", "name": "Read", "mutates": false}),
            false,
        ),
        (5, json!({"signal": "turn_interrupted"}), true),
        (6, json!({"signal": "compacting"}), false),
        (
            7,
            json!({"signal": "compaction_ended", "failed": false, "auto": false}),
            true,
        ),
        (8, json!({"signal": "compacting"}), false),
        (
            9,
            json!({"signal": "compaction_ended", "failed": true, "auto": false}),
            false,
        ),
        (
            10,
            json!({"signal": "turn_started", "turn_id": "old"}),
            false,
        ),
        (
            11,
            json!({"signal": "turn_started", "turn_id": "new"}),
            false,
        ),
        (
            12,
            json!({"signal": "turn_ended", "turn_id": "old", "errored": false, "parked_on_background": false}),
            false,
        ),
        (
            13,
            json!({"signal": "turn_interrupted", "turn_id": "old"}),
            false,
        ),
    ] {
        let event = raw_lifecycle_at(
            "claude",
            offset,
            json!({
                "event_name": "test", "agent_id": "s1", "signal": signal,
            }),
        );
        if completes {
            expected = Some(event.timestamp);
        }
        events.push(event);
        assert_eq!(
            reduce_agent_states(&events)[0].turn_ended_at,
            expected,
            "offset {offset}"
        );
    }
}

#[test]
fn promptless_launch_never_opens_a_turn() {
    let mut payload = launch_payload("launch_a", "lucid-atlas");
    payload.prompt = None;
    payload.state = AgentLaunchState::Starting;
    let mut launch = launch_event("codex", payload.clone());
    launch.timestamp = epoch();
    payload.state = AgentLaunchState::Bound;
    payload.pane_id = Some(PaneId::parse("tmux:%1").unwrap());
    let mut bound = launch_event("codex", payload.clone());
    bound.timestamp = epoch() + jiff::SignedDuration::from_secs(1);
    let mut events = vec![launch, bound];
    for (offset, event_name, signal) in [
        (2, "SessionStart", json!({"signal": "registered"})),
        (3, "SessionStart", json!({"signal": "registered"})),
        (
            4,
            "SessionStart",
            json!({"signal": "compaction_ended", "failed": false, "auto": false}),
        ),
        (5, "ReapedDead", json!({"signal": "ended"})),
    ] {
        events.push(raw_lifecycle_at(
            "codex",
            offset,
            json!({
                "event_name": event_name,
                "agent_id": "native-a",
                "agent_name": "lucid-atlas",
                "pane_id": "tmux:%1",
                "signal": signal,
            }),
        ));
    }
    for len in 1..=events.len() {
        let agents = reduce_agent_states(&events[..len]);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].turn_started_at, None, "prefix {len}");
        assert_eq!(agents[0].user_turn_started_at, None, "prefix {len}");
    }
    let prompt = raw_lifecycle_at(
        "codex",
        6,
        json!({
            "event_name": "UserPromptSubmit", "agent_id": "native-a",
            "signal": {"signal": "turn_started"},
        }),
    );
    events.push(prompt.clone());
    assert_eq!(
        reduce_agent_states(&events)[0].turn_started_at,
        Some(prompt.timestamp)
    );

    payload.state = AgentLaunchState::Failed;
    let failed = launch_event("codex", payload);
    assert_eq!(
        reduce_agent_states(&[events[0].clone(), failed])[0].turn_started_at,
        None
    );
}

#[test]
fn prompted_launch_opens_a_turn_and_sparse_updates_preserve_it() {
    let mut payload = launch_payload("launch-a", "lucid-atlas");
    payload.state = AgentLaunchState::Starting;
    let mut launch = launch_event("codex", payload.clone());
    launch.timestamp = epoch();
    let mut events = vec![launch.clone()];
    for (offset, state) in [(1, AgentLaunchState::Bound), (2, AgentLaunchState::Failed)] {
        payload.state = state;
        payload.prompt = None;
        let mut event = launch_event("codex", payload.clone());
        event.timestamp = epoch() + jiff::SignedDuration::from_secs(offset);
        events.push(event);
    }
    for len in 1..=events.len() {
        assert_eq!(
            reduce_agent_states(&events[..len])[0].turn_started_at,
            Some(launch.timestamp)
        );
        assert_eq!(
            reduce_agent_states(&events[..len])[0].user_turn_started_at,
            Some(launch.timestamp)
        );
    }
}

#[test]
fn registered_at_stamps_first_event_and_survives_end_and_restart() {
    let start = raw_lifecycle_at(
        "claude",
        0,
        serde_json::json!({ "event_name": "SessionStart", "agent_id": "s1", "signal": { "signal": "registered" } }),
    );
    let born = start.timestamp;
    let prompt = raw_lifecycle_at(
        "claude",
        10,
        serde_json::json!({ "event_name": "UserPromptSubmit", "agent_id": "s1", "signal": { "signal": "turn_started" } }),
    );
    let stop = raw_lifecycle_at(
        "claude",
        20,
        serde_json::json!({ "event_name": "Stop", "agent_id": "s1", "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false } }),
    );

    let agents = reduce_agent_states(&[start.clone(), prompt, stop]);

    // Identity, never activity: the spawn key is the first event's instant and
    // no later event re-stamps it — the sidebar's calm order stands on that.
    assert_eq!(agents[0].registered_at, Some(born));

    let end = raw_lifecycle_at(
        "claude",
        10,
        serde_json::json!({ "event_name": "SessionEnd", "agent_id": "s1", "signal": { "signal": "ended" } }),
    );
    let ended = reduce_agent_states(&[start.clone(), end.clone()]);
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0].registered_at, Some(born));
    assert_eq!(ended[0].last_seen, end.timestamp);
    assert_eq!(ended[0].ended_at, Some(end.timestamp));

    let reborn = raw_lifecycle_at(
        "claude",
        20,
        serde_json::json!({ "event_name": "SessionStart", "agent_id": "s1", "signal": { "signal": "registered" } }),
    );
    let reborn_ts = reborn.timestamp;

    let agents = reduce_agent_states(&[start, end, reborn]);

    // Ending retains the durable session row, so a later lifecycle event under
    // the same provider id clears the end stamp without replacing its identity.
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].registered_at, Some(born));
    assert_eq!(agents[0].last_seen, reborn_ts);
    assert_eq!(agents[0].ended_at, None);
}

#[test]
fn account_key_carries_through_turns_and_rebinds_on_registration() {
    let first = raw_lifecycle_at(
        "claude",
        0,
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "s1",
            "account_key": "account-one",
            "signal": { "signal": "registered" }
        }),
    );
    let prompt = raw_lifecycle_at(
        "claude",
        10,
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "s1",
            "signal": { "signal": "turn_started" }
        }),
    );
    let stop = raw_lifecycle_at(
        "claude",
        20,
        serde_json::json!({
            "event_name": "Stop",
            "agent_id": "s1",
            "signal": {
                "signal": "turn_ended",
                "errored": false,
                "parked_on_background": false
            }
        }),
    );
    let during_turn = reduce_agent_states(&[first.clone(), prompt, stop]);
    assert_eq!(during_turn[0].account_key.as_deref(), Some("account-one"));

    let resumed = raw_lifecycle_at(
        "claude",
        30,
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "s1",
            "account_key": "account-two",
            "signal": { "signal": "registered" }
        }),
    );
    let rebound = reduce_agent_states(&[first, resumed]);
    assert_eq!(rebound[0].account_key.as_deref(), Some("account-two"));
}

#[test]
fn launch_registered_at_stamps_first_launch_event() {
    let mut launch = raw_launch(
        AgentLaunchState::Bound,
        "launch-a",
        "lucid-atlas",
        Some("tmux:%1"),
    );
    launch.timestamp = Timestamp::from_second(epoch().as_second() + 5).unwrap();

    let agents = reduce_agent_states(&[launch.clone()]);

    assert_eq!(agents[0].registered_at, Some(launch.timestamp));
}

#[test]
fn turn_started_at_survives_parked_wait_then_restamps_on_next_turn() {
    let start = raw_lifecycle_at(
        "claude",
        0,
        serde_json::json!({ "event_name": "SessionStart", "agent_id": "s1", "signal": { "signal": "registered" } }),
    );
    let prompt = raw_lifecycle_at(
        "claude",
        10,
        serde_json::json!({ "event_name": "UserPromptSubmit", "agent_id": "s1", "signal": { "signal": "turn_started" } }),
    );
    let park = raw_lifecycle_at(
        "claude",
        20,
        serde_json::json!({ "event_name": "Stop", "agent_id": "s1", "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": true } }),
    );
    let wake = raw_lifecycle_at(
        "claude",
        30,
        serde_json::json!({ "event_name": "UserPromptSubmit", "agent_id": "s1", "signal": { "signal": "turn_started" } }),
    );
    let agents = reduce_agent_states(&[start.clone(), prompt.clone(), park.clone(), wake.clone()]);
    assert_eq!(agents[0].status, AgentStatus::Running);
    assert_eq!(agents[0].phase, TurnPhase::Reasoning);
    assert_eq!(agents[0].turn_started_at, Some(prompt.timestamp));

    let stop = raw_lifecycle_at(
        "claude",
        40,
        serde_json::json!({ "event_name": "Stop", "agent_id": "s1", "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false } }),
    );
    let next_prompt = raw_lifecycle_at(
        "claude",
        50,
        serde_json::json!({ "event_name": "UserPromptSubmit", "agent_id": "s1", "signal": { "signal": "turn_started" } }),
    );
    let next_turn = next_prompt.timestamp;

    let agents = reduce_agent_states(&[start, prompt, park, wake, stop, next_prompt]);

    assert_eq!(agents[0].status, AgentStatus::Running);
    assert_eq!(agents[0].phase, TurnPhase::Reasoning);
    assert_eq!(agents[0].turn_started_at, Some(next_turn));
}

#[test]
fn turn_started_at_stamps_first_turn_and_existing_session_reset() {
    let start = raw_lifecycle_at(
        "codex",
        0,
        serde_json::json!({ "event_name": "SessionStart", "agent_id": "s1", "signal": { "signal": "registered" } }),
    );
    let prompt = raw_lifecycle_at(
        "codex",
        10,
        serde_json::json!({ "event_name": "UserPromptSubmit", "agent_id": "s1", "signal": { "signal": "turn_started" } }),
    );
    let stop = raw_lifecycle_at(
        "codex",
        20,
        serde_json::json!({ "event_name": "Stop", "agent_id": "s1", "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false } }),
    );
    let reset = raw_lifecycle_at(
        "codex",
        30,
        serde_json::json!({ "event_name": "SessionStart", "agent_id": "s1", "signal": { "signal": "registered" } }),
    );

    let registered = reduce_agent_states(std::slice::from_ref(&start));
    assert_eq!(registered[0].turn_started_at, None);

    let first_turn = reduce_agent_states(&[start.clone(), prompt.clone()]);
    assert_eq!(first_turn[0].turn_started_at, Some(prompt.timestamp));

    let reset_session = reduce_agent_states(&[start, prompt, stop, reset.clone()]);
    assert_eq!(reset_session[0].status, AgentStatus::Idle);
    assert_eq!(reset_session[0].turn_started_at, Some(reset.timestamp));
}

#[test]
fn keepalive_since_holds_the_last_real_request_across_a_run_of_pings() {
    let keepalive =
        "Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nCache keepalive, no action needed.";
    let event = |at: i64, prompt: Option<&str>, signal: serde_json::Value| {
        raw_lifecycle_at(
            "claude",
            at,
            json!({"agent_id": "s1", "prompt": prompt, "signal": signal}),
        )
    };
    let turn = || json!({"signal": "turn_started"});
    let ended = || json!({"signal": "turn_ended", "errored": false, "parked_on_background": false});
    let since = |events: &[EventEnvelope]| reduce_agent_states(events)[0].keepalive_since;
    let mut events = vec![
        event(0, None, json!({"signal": "registered"})),
        event(1, Some("do X"), turn()),
        event(2, None, ended()),
    ];
    assert_eq!(since(&events), None, "a real turn opens no run");
    events.push(event(3540, Some(keepalive), turn()));
    events.push(event(3545, None, ended()));
    assert_eq!(
        since(&events),
        Some(events[1].timestamp),
        "the first ping stamps the prior last request"
    );
    events.push(event(7085, Some(keepalive), turn()));
    events.push(event(7090, None, ended()));
    assert_eq!(
        since(&events),
        Some(events[1].timestamp),
        "a second ping keeps it"
    );

    let prior: AgentState =
        serde_json::from_value(serde_json::to_value(&reduce_agent_states(&events)[0]).unwrap())
            .unwrap();
    let carried = reduce_agent_states_seeded(
        BTreeMap::from([((prior.kind.clone(), prior.agent_id.clone()), prior)]),
        &[event(7100, None, ended())],
    );
    assert_eq!(
        carried.values().next().unwrap().keepalive_since,
        Some(events[1].timestamp),
        "the carried baseline keeps the clock across rotation"
    );

    let mixed = format!("{keepalive}\n\nType: WAIT\nFrom: @rimz\nContent:\ncheck back");
    for (label, tail) in [
        (
            "stage",
            vec![event(
                7200,
                Some("Type: STAGE\nFrom: @rimz\nContent:\nImplement is yours."),
                turn(),
            )],
        ),
        (
            "agent message",
            vec![event(
                7200,
                Some("Type: AGENT_MESSAGE\nFrom: @planner\nContent:\nhi"),
                turn(),
            )],
        ),
        ("batched", vec![event(7200, Some(&mixed), turn())]),
        ("human", vec![event(7200, Some("next task"), turn())]),
        (
            "clear",
            vec![event(7200, None, json!({"signal": "registered"}))],
        ),
        (
            "compact",
            vec![
                event(7200, None, json!({"signal": "compacting"})),
                event(
                    7210,
                    None,
                    json!({"signal": "compaction_ended", "failed": false, "auto": false}),
                ),
            ],
        ),
    ] {
        events.truncate(7);
        events.extend(tail);
        assert_eq!(since(&events), None, "{label} clears the run");
    }

    let parked = || json!({"signal": "turn_ended", "errored": false, "parked_on_background": true});
    events.truncate(4);
    events.push(event(3545, None, parked()));
    assert_eq!(since(&events), Some(events[1].timestamp));
    events.push(event(
        3600,
        Some("Type: WAIT\nFrom: @rimz\nContent:\ncheck back"),
        turn(),
    ));
    assert_eq!(
        since(&events),
        None,
        "a real wake resuming a parked ping turn clears the run"
    );
    events.push(event(3610, None, parked()));
    events.push(event(7000, Some(keepalive), turn()));
    assert_eq!(
        since(&events),
        Some(events[5].timestamp),
        "a ping to a parked row opens a run from the wake it parked"
    );
}

#[test]
fn a_ping_only_turn_moves_only_the_cache_clock() {
    let ping = "Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nCache keepalive, no action needed.";
    let lifecycle = |at: i64, prompt: Option<&str>, signal: serde_json::Value| {
        raw_lifecycle_at(
            "claude",
            at,
            json!({ "agent_id": "a", "prompt": prompt, "signal": signal }),
        )
    };
    let started = |at, prompt, id: &str| {
        lifecycle(at, prompt, json!({"signal": "turn_started", "turn_id": id}))
    };
    let ended = |at, id: &str| {
        lifecycle(
            at,
            None,
            json!({"signal": "turn_ended", "errored": false, "parked_on_background": false, "turn_id": id}),
        )
    };
    let interrupted = |at, id: &str| {
        lifecycle(
            at,
            None,
            json!({"signal": "turn_interrupted", "turn_id": id}),
        )
    };
    let fold = |events: &[EventEnvelope]| reduce_agent_states(events).remove(0);
    let mixed = format!("{ping}\n\nType: WAIT\nFrom: @rimz\nContent:\nwoke");
    let observable = |state: &AgentState| {
        (
            state.status,
            state.phase,
            state.last_activity,
            state.turn_started_at,
            state.turn_ended_at,
            state.user_turn_started_at,
            state.started_turn_id.clone(),
            state.superseded_turn_id.clone(),
            state.closed_turn_id.clone(),
        )
    };
    for (opener, rest) in [
        (Some("do X"), AgentStatus::Success),
        (
            Some("Type: WAIT\nFrom: @rimz\nContent:\nwoke"),
            AgentStatus::Success,
        ),
        (
            Some("Type: AGENT_MESSAGE\nFrom: @coder\nContent:\nhi"),
            AgentStatus::Success,
        ),
        (Some(mixed.as_str()), AgentStatus::Success),
        (None, AgentStatus::Success),
        (Some("do X"), AgentStatus::Idle),
    ] {
        let mut events = vec![started(1, opener, "r1")];
        events.push(if rest == AgentStatus::Idle {
            interrupted(2, "r1")
        } else {
            ended(2, "r1")
        });
        let before = fold(&events);
        assert_eq!(before.status, rest, "{opener:?}");
        assert_eq!(
            before.turn_ended_at,
            Some(events[1].timestamp),
            "{opener:?}"
        );
        assert_eq!(before.pinged_at, None);

        events.push(started(3, Some(ping), "p1"));
        let open = fold(&events);
        assert!(open.ping_turn, "{opener:?}");
        assert_eq!(observable(&open), observable(&before), "{opener:?}");
        assert_eq!(
            open.keepalive_since,
            Some(events[0].timestamp),
            "{opener:?}"
        );

        events.push(ended(4, "p1"));
        let closed = fold(&events);
        assert!(!closed.ping_turn);
        assert_eq!(observable(&closed), observable(&before), "{opener:?}");
        assert_eq!(closed.pinged_at, Some(events[3].timestamp));
        assert_eq!(closed.last_request_at(), Some(events[3].timestamp));
        assert_eq!(closed.keepalive_since, open.keepalive_since);
        let decoded: AgentState =
            serde_json::from_value(serde_json::to_value(&closed).unwrap()).unwrap();
        assert_eq!(decoded.pinged_at, closed.pinged_at);

        events.push(lifecycle(5, None, json!({"signal": "registered"})));
        let cleared = fold(&events);
        assert_eq!(cleared.pinged_at, Some(events[3].timestamp));
        assert_eq!(cleared.keepalive_since, None, "a reset ends the run");

        events.extend([started(6, Some("next"), "r2"), ended(7, "r2")]);
        let real = fold(&events);
        assert_eq!(real.turn_ended_at, Some(events[6].timestamp));
        assert_eq!(real.last_request_at(), Some(events[5].timestamp));
    }

    // A ping that grows into work the user must see folds as that work.
    let base = vec![
        started(1, Some("do X"), "r1"),
        ended(2, "r1"),
        started(3, Some(ping), "p1"),
    ];
    let mut tool = base.clone();
    tool.push(lifecycle(
        4,
        None,
        json!({"signal": "tool_used", "mutates": false, "edits": false}),
    ));
    let state = fold(&tool);
    assert!(!state.ping_turn);
    assert_eq!(state.status, AgentStatus::Running);
    assert_eq!(state.last_activity, tool[3].timestamp);
    let mut asking = base.clone();
    asking.push(lifecycle(
        4,
        None,
        json!({"signal": "awaiting_input", "kind": "permission"}),
    ));
    let state = fold(&asking);
    assert!(!state.ping_turn);
    assert_eq!(state.status, AgentStatus::Waiting);

    // A real turn's late report stays ignored while a ping is open.
    let late = vec![
        started(1, Some("do X"), "r1"),
        started(2, Some("again"), "r2"),
        ended(3, "r2"),
        started(4, Some(ping), "p1"),
        ended(5, "r1"),
    ];
    let state = fold(&late);
    assert!(state.ping_turn, "the stale report is not the ping's close");
    assert_eq!(state.turn_ended_at, Some(late[2].timestamp));
    assert_eq!(state.status, AgentStatus::Success);

    // So does a duplicate verdict for the real turn the ping followed: only the
    // ping's own close moves the cache clock, and the next real turn folds.
    let mut dup = late[..4].to_vec();
    let rested = fold(&dup[..3]);
    dup.extend([ended(5, "r2"), interrupted(6, "r2")]);
    let state = fold(&dup);
    assert!(
        state.ping_turn,
        "a duplicate real verdict is not the ping's close"
    );
    assert_eq!(observable(&state), observable(&rested));
    assert_eq!(state.pinged_at, None);
    dup.push(ended(7, "p1"));
    let state = fold(&dup);
    assert!(!state.ping_turn);
    assert_eq!(observable(&state), observable(&rested));
    assert_eq!(state.pinged_at, Some(dup[6].timestamp));
    dup.extend([started(8, Some("next"), "r3"), ended(9, "r3")]);
    let state = fold(&dup);
    assert_eq!(state.status, AgentStatus::Success);
    assert_eq!(state.started_turn_id.as_deref(), Some("r3"));
    assert_eq!(state.turn_started_at, Some(dup[7].timestamp));
    assert_eq!(state.turn_ended_at, Some(dup[8].timestamp));

    let fresh = fold(&[lifecycle(1, None, json!({"signal": "registered"}))]);
    let encoded = serde_json::to_value(&fresh).unwrap();
    assert!(encoded.get("ping_turn").is_none() && encoded.get("pinged_at").is_none());
}

#[test]
fn a_ping_on_a_row_parked_on_background_work_moves_only_the_cache_clock() {
    let ping = "Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nCache keepalive, no action needed.";
    let lifecycle = |at: i64, prompt: Option<&str>, signal: serde_json::Value| {
        raw_lifecycle_at(
            "claude",
            at,
            json!({ "agent_id": "a", "prompt": prompt, "signal": signal }),
        )
    };
    let started = |at, prompt| lifecycle(at, prompt, json!({"signal": "turn_started"}));
    let parked_end = |at| {
        lifecycle(
            at,
            None,
            json!({"signal": "turn_ended", "errored": false, "parked_on_background": true}),
        )
    };
    let fold = |events: &[EventEnvelope]| reduce_agent_states(events).remove(0);
    let mut events = vec![started(1, Some("start the dev server")), parked_end(2)];
    let rested = fold(&events);
    assert_eq!(
        (rested.status, rested.phase),
        (AgentStatus::Running, TurnPhase::Parked)
    );
    assert_eq!(rested.effective_status(), AgentStatus::Success);
    let observable = |state: &AgentState| {
        (
            state.status,
            state.phase,
            state.last_activity,
            state.turn_started_at,
            state.turn_ended_at,
            state.user_turn_started_at,
        )
    };

    events.push(started(3, Some(ping)));
    let open = fold(&events);
    assert!(open.ping_turn, "a parked row rests, so the ping is no work");
    assert_eq!(observable(&open), observable(&rested));
    assert_eq!(open.keepalive_since, Some(events[0].timestamp));
    events.push(parked_end(4));
    let closed = fold(&events);
    assert!(!closed.ping_turn);
    assert_eq!(
        observable(&closed),
        observable(&rested),
        "the ping's close never renews the horizon anchor"
    );
    assert_eq!(closed.pinged_at, Some(events[3].timestamp));

    // A turn still in flight is no rest: the same prompt opens real work.
    let working = [started(1, Some("build it")), started(2, Some(ping))];
    let active = fold(&working);
    assert!(!active.ping_turn);
    assert_eq!(active.status, AgentStatus::Running);
    assert_eq!(active.turn_started_at, Some(working[1].timestamp));
}
