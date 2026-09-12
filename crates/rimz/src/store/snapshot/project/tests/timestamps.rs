use super::*;

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
