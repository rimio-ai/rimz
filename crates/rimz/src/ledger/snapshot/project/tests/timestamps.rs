use super::*;

#[test]
fn registered_at_stamps_first_event_then_carries_forward() {
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

    let agents = reduce_agent_states(&[start, prompt, stop]);

    // Identity, never activity: the spawn key is the first event's instant and
    // no later event re-stamps it — the sidebar's calm order stands on that.
    assert_eq!(agents[0].registered_at, Some(born));
}

#[test]
fn registered_at_survives_seeded_resume() {
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

    // Fold the prefix, then resume onto the suffix — the rotation-carryover
    // shape. The set-once spawn key must equal the from-scratch fold's.
    let seed = reduce_agent_states_seeded(BTreeMap::new(), std::slice::from_ref(&start));
    let resumed = reduce_agent_states_seeded(seed, std::slice::from_ref(&prompt));
    let scratch = reduce_agent_states(&[start.clone(), prompt]);

    let key = (AgentKind::new_unchecked("claude"), "s1".into());
    assert_eq!(
        resumed.get(&key).and_then(|agent| agent.registered_at),
        Some(start.timestamp)
    );
    assert_eq!(
        resumed.get(&key).and_then(|agent| agent.registered_at),
        scratch[0].registered_at
    );
}

#[test]
fn registered_at_restamps_after_a_session_tombstone() {
    let start = raw_lifecycle_at(
        "claude",
        0,
        serde_json::json!({ "event_name": "SessionStart", "agent_id": "s1", "signal": { "signal": "registered" } }),
    );
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
