use super::*;

fn chunk(tag: &str, text: &str, prompt_index: Option<u64>) -> String {
    let mut update = serde_json::json!({
        "sessionUpdate": tag,
        "content": {"type": "text", "text": text}
    });
    if let Some(index) = prompt_index {
        update["_meta"] = serde_json::json!({"promptIndex": index});
    }
    serde_json::json!({
        "timestamp": 1_700_000_000_u64,
        "method": "session/update",
        "params": {"sessionId": "s1", "update": update}
    })
    .to_string()
}

#[test]
fn rewind_replaces_the_abandoned_prompt_branch() {
    let lines = [
        chunk("user_message_chunk", "one", Some(0)),
        chunk("agent_message_chunk", "first", None),
        chunk("user_message_chunk", "two", Some(1)),
        chunk("agent_message_chunk", "abandoned", None),
        serde_json::json!({
            "timestamp": 1_700_000_001_u64,
            "method": "_x.ai/session/update",
            "params": {"update": {
                "sessionUpdate": "rewind_marker",
                "target_prompt_index": 1
            }}
        })
        .to_string(),
        chunk("user_message_chunk", "replacement", Some(1)),
        chunk("agent_message_chunk", "kept", None),
    ]
    .join("\n");
    let messages = parse_messages(&lines);
    assert_eq!(
        messages
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["one", "first", "replacement", "kept"]
    );
}

#[test]
fn indexed_history_rejects_later_unmarked_phantom_prompts() {
    let lines = [
        chunk("user_message_chunk", "real", Some(0)),
        chunk("agent_message_chunk", "answer", None),
        chunk("user_message_chunk", "system echo", None),
        chunk("agent_message_chunk", "not main history", None),
    ]
    .join("\n");
    assert_eq!(
        parse_messages(&lines)
            .iter()
            .map(|message| message.text.as_str())
            .collect::<Vec<_>>(),
        ["real", "answer"]
    );
}

#[test]
fn assistant_suffix_does_not_require_the_earlier_user_chunk() {
    let suffix = [
        chunk("agent_message_chunk", "stream", None),
        chunk("agent_thought_chunk", "hidden reasoning", None),
        chunk("agent_message_chunk", "continues", None),
    ]
    .join("\n");
    assert_eq!(parse_assistant_suffix(&suffix), ["stream continues"]);
    assert_eq!(
        parse_messages(&[chunk("user_message_chunk", "prompt", Some(0)), suffix,].join("\n"))[1]
            .text,
        "stream continues"
    );

    let rewound = format!(
        "{}\n{}",
        serde_json::json!({"method":"_x.ai/session/update","params":{"update":{"sessionUpdate":"rewind_marker","target_prompt_index":0}}}),
        chunk("agent_message_chunk", "new branch", None)
    );
    assert_eq!(parse_assistant_suffix(&rewound), ["new branch"]);
}

#[test]
fn context_measurement_uses_samples_not_completed_usage() {
    let completion = serde_json::json!({
        "timestamp": 1_700_000_001_u64,
        "method": "_x.ai/session/update",
        "params": {
            "sessionId": "s1",
            "update": {
                "sessionUpdate": "turn_completed",
                "prompt_id": "p1",
                "stop_reason": "end_turn",
                "usage": {"inputTokens": 100, "outputTokens": 5}
            }
        }
    })
    .to_string();
    let sample = serde_json::json!({
        "method":"session/update",
        "params":{"sessionId":"s1", "_meta":{"totalTokens":9},
            "update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking"}}}
    }).to_string();
    let completed = [
        chunk("user_message_chunk", "one", Some(0)),
        sample,
        completion,
    ]
    .join("\n");
    assert_eq!(fold(&completed).latest_context_tokens(), Some(9));

    let in_progress = serde_json::json!({
        "timestamp": 1_700_000_002_u64,
        "method": "session/update",
        "params": {
            "sessionId": "s1",
            "_meta": {"totalTokens": 175},
            "update": {
                "sessionUpdate": "agent_thought_chunk",
                "content": {"type": "text", "text": "thinking"}
            }
        }
    })
    .to_string();
    let second_turn = [
        completed,
        chunk("user_message_chunk", "two", Some(1)),
        in_progress,
    ]
    .join("\n");
    assert_eq!(fold(&second_turn).latest_context_tokens(), Some(175));
}
