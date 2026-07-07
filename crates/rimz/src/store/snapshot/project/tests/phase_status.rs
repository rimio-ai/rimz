use super::*;

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
