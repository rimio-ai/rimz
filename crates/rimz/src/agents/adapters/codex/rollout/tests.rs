use super::*;

#[test]
fn raw_usage_normalizes_aliases_and_string_counts() {
    let usage: CodexRawUsage =
        serde_json::from_str(r#"{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}"#)
            .unwrap();
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 50);
    assert_eq!(usage.total_tokens, 150);
    assert!(usage.input_reported());
    assert!(usage.output_reported());
    assert!(usage.total_reported());

    let usage: CodexRawUsage =
        serde_json::from_str(r#"{"input_tokens":200,"cached_tokens":80,"output_tokens":30}"#)
            .unwrap();
    assert_eq!(usage.input_tokens, 200);
    assert_eq!(usage.cached_input_tokens, 80);
    assert_eq!(usage.cache_write_input_tokens, 0);
    assert_eq!(usage.output_tokens, 30);
    assert_eq!(usage.total_tokens, 230);
    assert!(!usage.cache_write_reported());
    assert!(!usage.total_reported());

    let usage: CodexRawUsage =
        serde_json::from_str(r#"{"input_tokens":"100","output_tokens":"50"}"#).unwrap();
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 50);

    let usage: CodexRawUsage = serde_json::from_str(
        r#"{"input_tokens":100,"cache_write_input_tokens":60,"output_tokens":50}"#,
    )
    .unwrap();
    assert_eq!(usage.cache_write_input_tokens, 60);
    assert!(usage.cache_write_reported());

    let usage: CodexRawUsage =
        serde_json::from_str(r#"{"input_tokens":100,"cache_write_tokens":60}"#).unwrap();
    assert_eq!(usage.cache_write_input_tokens, 60);
    assert!(usage.cache_write_reported());
}

#[test]
fn decoder_returns_other_for_unknown_valid_shapes() {
    let record = decode_line(br#"{"type":"future_entry","payload":{}}"#).unwrap();
    assert!(matches!(record.kind, RolloutKind::Other));

    let record = decode_line(br#"{"type":"event_msg","payload":{"type":"future_event"}}"#).unwrap();
    assert!(matches!(record.kind, RolloutKind::Other));
    assert!(decode_line(b"{torn").is_none());
}

#[test]
fn decoder_normalizes_visible_context_usage_and_terminal_facts() {
    let visible = decode_line(
        br#"{"timestamp":"2026-01-01T00:00:00Z","type":"event_msg","payload":{"type":"agent_message","message":"done"}}"#,
    )
    .unwrap();
    assert!(matches!(visible.kind, RolloutKind::AgentMessage));
    assert_eq!(visible.message.as_deref(), Some("done"));
    assert!(matches!(visible.message, Some(Cow::Borrowed("done"))));
    assert!(visible.event_timestamp().is_some());

    let escaped = decode_line(
        br#"{"type":"event_msg","payload":{"type":"agent_message","message":"done\nnow"}}"#,
    )
    .unwrap();
    assert_eq!(escaped.message.as_deref(), Some("done\nnow"));
    assert!(matches!(escaped.message, Some(Cow::Owned(_))));

    let context = decode_line(
        br#"{"type":"turn_context","payload":{"model_name":"gpt-5","reasoning_effort":"high"}}"#,
    )
    .unwrap();
    let RolloutKind::TurnContext(context) = context.kind else {
        panic!("turn context");
    };
    assert_eq!(context.model(), Some("gpt-5"));
    assert_eq!(context.effort(), Some("high"));

    let usage = decode_line(
        br#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10},"model_context_window":100}}}"#,
    )
    .unwrap();
    let RolloutKind::TokenCount(usage) = usage.kind else {
        panic!("token count");
    };
    assert_eq!(
        usage.info().unwrap().last_token_usage.unwrap().input_tokens,
        10
    );
    assert_eq!(usage.info().unwrap().model_context_window, Some(100));

    let complete = decode_line(
        br#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","last_agent_message":"done"}}"#,
    )
    .unwrap();
    let RolloutKind::TaskComplete(complete) = complete.kind else {
        panic!("task complete");
    };
    assert_eq!(complete.turn_id.as_deref(), Some("turn-1"));
    assert!(!complete.error_field_present);
}

#[test]
fn decoder_normalizes_paginated_visible_message_content() {
    let user = decode_line(
        br#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"one"},{"type":"image","image_url":"ignored"},{"type":"text","text":"two"}]}}}"#,
    )
    .unwrap();
    assert!(matches!(user.kind, RolloutKind::UserMessage));
    assert_eq!(user.message.as_deref(), Some("one\ntwo"));

    let assistant = decode_line(
        br#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"done"}]}}}"#,
    )
    .unwrap();
    assert!(matches!(assistant.kind, RolloutKind::AgentMessage));
    assert_eq!(assistant.message.as_deref(), Some("done"));

    let hook = decode_line(
        br#"{"type":"event_msg","payload":{"type":"item_completed","item":{"type":"HookPrompt","content":[{"type":"text","text":"hidden"}]}}}"#,
    )
    .unwrap();
    assert!(matches!(hook.kind, RolloutKind::ItemCompleted(_)));
    assert!(hook.message.is_none());
}

#[test]
fn timestamp_normalization_keeps_seconds_and_milliseconds_exact() {
    assert_eq!(
        millis_to_rfc3339(1_767_225_600_000),
        "2026-01-01T00:00:00.000Z"
    );
    assert_eq!(millis_to_rfc3339(1_000), "1970-01-01T00:00:01.000Z");
    assert_eq!(millis_to_rfc3339(1_042), "1970-01-01T00:00:01.042Z");
}
