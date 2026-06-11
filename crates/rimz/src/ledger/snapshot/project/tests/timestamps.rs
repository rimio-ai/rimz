use super::*;

#[test]
fn registered_at_stamps_first_event_and_restamps_after_tombstone() {
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
    let reborn = raw_lifecycle_at(
        "claude",
        20,
        serde_json::json!({ "event_name": "SessionStart", "agent_id": "s1", "signal": { "signal": "registered" } }),
    );
    let reborn_ts = reborn.timestamp;

    let agents = reduce_agent_states(&[start, end, reborn]);

    // The one exception to set-once: `Ended` tombstones the key, so a later
    // event under the same id is a genuinely new session and stamps fresh —
    // the spawn key names the session, not the id's whole history.
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].registered_at, Some(reborn_ts));
}

#[test]
fn turn_started_at_survives_parked_wake_then_restamps_on_next_turn() {
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
