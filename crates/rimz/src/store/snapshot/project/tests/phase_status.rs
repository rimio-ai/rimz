use super::*;

#[test]
fn reaper_ends_rest_active_sessions_without_erasing_failed_verdicts() {
    for event_name in [
        "ReapedSuperseded",
        "ReapedDead",
        "ReapedStale",
        "WorktreeRemoved",
    ] {
        for (signal, ended_status, recovered_status) in [
            (
                json!({ "signal": "turn_started" }),
                AgentStatus::Idle,
                AgentStatus::Running,
            ),
            (
                json!({ "signal": "awaiting_input", "kind": "permission" }),
                AgentStatus::Idle,
                AgentStatus::Running,
            ),
            (
                json!({ "signal": "turn_ended", "errored": true, "parked_on_background": false }),
                AgentStatus::Failed,
                AgentStatus::Failed,
            ),
        ] {
            let mut events = vec![
                raw_lifecycle_at(
                    "codex",
                    0,
                    json!({ "agent_id": "s1", "signal": { "signal": "registered" } }),
                ),
                raw_lifecycle_at("codex", 10, json!({ "agent_id": "s1", "signal": signal })),
                raw_lifecycle_at(
                    "codex",
                    20,
                    json!({ "event_name": event_name, "agent_id": "s1", "signal": { "signal": "ended" } }),
                ),
            ];
            let ended = reduce_agent_states(&events);
            assert_eq!(ended[0].status, ended_status, "{event_name}");
            assert_eq!(ended[0].phase, TurnPhase::Idle);
            assert_eq!(ended[0].ended_at, Some(events[2].timestamp));
            assert!(ended[0].waiting_since.is_none());

            events.push(raw_lifecycle_at("codex", 30, json!({ "agent_id": "s1", "signal": { "signal": "tool_used", "mutates": false, "edits": false } })));
            let recovered = reduce_agent_states(&events);
            assert_eq!(recovered[0].status, recovered_status, "{event_name}");
            assert_eq!(recovered[0].ended_at, None);
        }
    }
}

#[test]
fn authoritative_ends_stay_failed_through_trailing_tool_hooks() {
    for event_name in [
        Some("rimz.agent-ended"),
        Some("SessionEnd"),
        Some("unknown"),
        None,
    ] {
        let mut events = vec![
            raw_lifecycle_at(
                "codex",
                0,
                json!({ "agent_id": "s1", "signal": { "signal": "turn_started" } }),
            ),
            raw_lifecycle_at(
                "codex",
                10,
                json!({ "event_name": event_name, "agent_id": "s1", "signal": { "signal": "ended" } }),
            ),
        ];
        let ended = reduce_agent_states(&events);
        assert_eq!(ended[0].status, AgentStatus::Failed, "{event_name:?}");
        assert_eq!(ended[0].ended_at, Some(events[1].timestamp));
        events.push(raw_lifecycle_at("codex", 20, json!({ "agent_id": "s1", "signal": { "signal": "tool_used", "mutates": false, "edits": false } })));
        let trailing = reduce_agent_states(&events);
        assert_eq!(trailing[0].status, AgentStatus::Failed, "{event_name:?}");
        assert_eq!(trailing[0].ended_at, None);
    }
}

#[test]
fn thinking_phase_follows_the_turn_through_the_reducer() {
    // A legacy `permission_posture` param rides along unread — replay of an
    // old log never errors on it.
    let start = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
            "permission_posture": "plan",
        }),
    );
    let prompt = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }),
    );
    let running = reduce_agent_states(&[start.clone(), prompt.clone()]);
    assert_eq!(running[0].status, AgentStatus::Running);
    assert_eq!(
        running[0].phase,
        TurnPhase::Reasoning,
        "a fresh turn opens reasoning"
    );

    // A mutating-but-not-editing tool (a shell command) keeps the head.
    let shell = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "PostToolUse",
            "agent_id": "sess-1",
            "signal": { "signal": "tool_used", "mutates": true, "edits": false },
        }),
    );
    let still = reduce_agent_states(&[start.clone(), prompt.clone(), shell.clone()]);
    assert_eq!(
        still[0].phase,
        TurnPhase::Reasoning,
        "a shell command is not a file edit"
    );

    // The turn's first file edit flips it to working.
    let edit = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "PostToolUse",
            "agent_id": "sess-1",
            "signal": { "signal": "tool_used", "mutates": true, "edits": true },
        }),
    );
    let working = reduce_agent_states(&[start.clone(), prompt.clone(), shell, edit]);
    assert_eq!(working[0].status, AgentStatus::Running);
    assert_eq!(working[0].phase, TurnPhase::Acting);

    // The turn end clears the head regardless.
    let stop = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "Stop",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false },
        }),
    );
    let stopped = reduce_agent_states(&[start, prompt, stop]);
    assert_eq!(stopped[0].status, AgentStatus::Success);
    assert_eq!(stopped[0].phase, TurnPhase::Idle);
}

#[test]
fn open_ask_tracks_the_waiting_lifecycle_edge() {
    let start = raw_lifecycle_at(
        "claude",
        1,
        serde_json::json!({
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
        }),
    );
    let waiting = raw_lifecycle_at(
        "claude",
        2,
        serde_json::json!({
            "agent_id": "sess-1",
            "signal": {
                "signal": "awaiting_input",
                "kind": "question",
                "ask_id": "ask_0123456789abcdef",
                "detail": "Choose a rollout"
            },
        }),
    );
    let answered = raw_lifecycle_at(
        "claude",
        3,
        serde_json::json!({
            "agent_id": "sess-1",
            "signal": { "signal": "tool_used", "mutates": false, "edits": false },
        }),
    );

    let open = reduce_agent_states(&[start.clone(), waiting.clone()]);
    let ask = open[0].open_ask.as_ref().expect("open ask");
    assert_eq!(ask.id.as_str(), "ask_0123456789abcdef");
    assert_eq!(ask.kind, crate::agents::AskKind::Question);
    assert_eq!(ask.detail.as_deref(), Some("Choose a rollout"));

    let closed = reduce_agent_states(&[start.clone(), waiting, answered]);
    assert!(closed[0].open_ask.is_none());

    let legacy = raw_lifecycle_at(
        "claude",
        2,
        serde_json::json!({
            "agent_id": "sess-1",
            "signal": { "signal": "awaiting_input", "kind": "question" },
        }),
    );
    let replayed = reduce_agent_states(&[start, legacy]);
    assert_eq!(replayed[0].status, AgentStatus::Waiting);
    assert!(replayed[0].open_ask.is_none());
}

#[test]
fn keyed_open_ask_survives_a_parallel_sibling_completion() {
    let start = raw_lifecycle_at(
        "pi",
        1,
        serde_json::json!({
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
        }),
    );
    let waiting = raw_lifecycle_at(
        "pi",
        2,
        serde_json::json!({
            "agent_id": "sess-1",
            "signal": {
                "signal": "awaiting_input",
                "kind": "question",
                "ask_id": "ask_0123456789abcdef",
                "native_key": "ask-call"
            },
        }),
    );
    let sibling = raw_lifecycle_at(
        "pi",
        3,
        serde_json::json!({
            "agent_id": "sess-1",
            "signal": {
                "signal": "tool_used",
                "mutates": true,
                "edits": false,
                "native_key": "sibling-call"
            },
        }),
    );
    let matching = raw_lifecycle_at(
        "pi",
        4,
        serde_json::json!({
            "agent_id": "sess-1",
            "signal": {
                "signal": "tool_used",
                "mutates": false,
                "edits": false,
                "native_key": "ask-call"
            },
        }),
    );

    let open = reduce_agent_states(&[start.clone(), waiting.clone(), sibling]);
    assert_eq!(open[0].status, AgentStatus::Waiting);
    assert_eq!(open[0].waiting_since, Some(waiting.timestamp));
    assert_eq!(
        open[0]
            .open_ask
            .as_ref()
            .and_then(|ask| ask.native_key.as_deref()),
        Some("ask-call")
    );

    let closed = reduce_agent_states(&[start, waiting, matching]);
    assert_eq!(closed[0].status, AgentStatus::Running);
    assert!(closed[0].waiting_since.is_none());
    assert!(closed[0].open_ask.is_none());
}
