use super::*;

fn lifecycle(secs: i64, params: serde_json::Value) -> EventEnvelope {
    let mut params = params;
    params["agent_id"] = json!("sess-1");
    raw_lifecycle_at("claude", secs, params)
}

fn ids(state: &AgentState) -> Vec<(&str, i64)> {
    state
        .background_shells
        .iter()
        .map(|shell| (shell.id.as_str(), shell.started_at.as_second()))
        .collect()
}

#[test]
fn background_shell_reports_fold_carry_and_clear_at_session_end() {
    let started = |id: &str, at: i64| json!({"id": id, "command": "sleep 30", "started_at": Timestamp::from_second(at).unwrap()});
    let mut events = vec![
        lifecycle(0, json!({"signal": {"signal": "registered"}})),
        lifecycle(
            1,
            json!({
                "signal": {"signal": "tool_used", "mutates": true, "edits": false, "name": "Bash"},
                "background_shells": {"kind": "started", "shell": started("a", 100)},
            }),
        ),
        lifecycle(
            2,
            json!({
                "signal": {"signal": "turn_ended", "errored": false, "parked_on_background": true},
                "background_shells": {"kind": "snapshot", "shells": [started("a", 200), started("b", 200)]},
            }),
        ),
        lifecycle(3, json!({"signal": {"signal": "turn_started"}})),
    ];
    let agents = reduce_agent_states(&events);
    assert_eq!(ids(&agents[0]), vec![("a", 100), ("b", 200)]);

    events.push(lifecycle(
        4,
        json!({
            "signal": {"signal": "turn_started"},
            "background_shells": {"kind": "finished", "ids": ["a"]},
        }),
    ));
    let agents = reduce_agent_states(&events);
    assert_eq!(ids(&agents[0]), vec![("b", 200)]);

    events.push(lifecycle(5, json!({"signal": {"signal": "ended"}})));
    let agents = reduce_agent_states(&events);
    assert!(agents[0].background_shells.is_empty());
}
