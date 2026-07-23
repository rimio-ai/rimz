use super::*;

fn lifecycle(signal: serde_json::Value) -> EventEnvelope {
    raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "PostToolUse",
            "agent_id": "sess-1",
            "signal": signal,
        }),
    )
}

#[test]
fn named_tool_calls_accumulate_and_legacy_unnamed_calls_do_not() {
    let events = [
        lifecycle(serde_json::json!({
            "signal": "registered",
        })),
        lifecycle(serde_json::json!({
            "signal": "tool_used",
            "mutates": false,
            "edits": false,
            "name": "Read",
        })),
        lifecycle(serde_json::json!({
            "signal": "tool_used",
            "mutates": true,
            "edits": false,
            "name": "Bash",
        })),
        lifecycle(serde_json::json!({
            "signal": "tool_used",
            "mutates": false,
            "edits": false,
            "name": "Read",
        })),
        lifecycle(serde_json::json!({
            "signal": "tool_used",
            "mutates": false,
            "edits": false,
        })),
    ];

    let agents = reduce_agent_states(&events);

    assert_eq!(
        agents[0].tool_calls,
        BTreeMap::from([("Bash".to_owned(), 1), ("Read".to_owned(), 2)])
    );
}
