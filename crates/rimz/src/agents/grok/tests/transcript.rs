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
