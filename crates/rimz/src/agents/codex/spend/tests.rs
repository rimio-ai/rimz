use super::*;

/// The line classifier feeding `parse_codex_session` — exercised here
/// through the kind probe so each accepted/skipped shape stays pinned.
fn token_line(line: &[u8]) -> bool {
    codex_line_kind(line).is_some()
}

#[test]
fn token_line_accepts_each_known_shape() {
    assert!(token_line(
        br#"{"type":"event_msg","payload":{"type":"token_count","info":{}}}"#
    ));
    assert!(token_line(br#"{"type":"turn_context","payload":{}}"#));
    assert!(token_line(
        br#"{"usage":{"input_tokens":100,"output_tokens":50},"model":"gpt-5"}"#
    ));
    assert!(token_line(
        br#"{"prompt_tokens":100,"completion_tokens":50,"model":"gpt-5"}"#
    ));
}

#[test]
fn token_line_skips_non_usage_shapes() {
    assert!(!token_line(
        br#"{"type":"event_msg","payload":{"type":"tool_call"}}"#
    ));
    assert!(!token_line(br#"{"type":"other","foo":"bar"}"#));
    assert!(!token_line(b"{}"));
}

#[test]
fn millis_to_rfc3339_known_values() {
    // 2026-01-01 00:00:00.000 UTC = 1767225600000 ms
    assert_eq!(
        millis_to_rfc3339(1_767_225_600_000),
        "2026-01-01T00:00:00.000Z"
    );
    // 1970-01-01 00:00:01.000 UTC
    assert_eq!(millis_to_rfc3339(1_000), "1970-01-01T00:00:01.000Z");
    // fractional seconds
    assert_eq!(millis_to_rfc3339(1_000 + 42), "1970-01-01T00:00:01.042Z");
}

#[test]
fn codex_raw_usage_field_aliases() {
    // OpenAI alias names
    let s = r#"{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}"#;
    let u: CodexRawUsage = serde_json::from_str(s).unwrap();
    assert_eq!(u.input_tokens, 100);
    assert_eq!(u.output_tokens, 50);
    assert_eq!(u.total_tokens, 150);
}

#[test]
fn codex_raw_usage_cached_aliases() {
    let s = r#"{"input_tokens":200,"cached_tokens":80,"output_tokens":30}"#;
    let u: CodexRawUsage = serde_json::from_str(s).unwrap();
    assert_eq!(u.input_tokens, 200);
    assert_eq!(u.cached_input_tokens, 80);
    assert_eq!(u.output_tokens, 30);
    assert_eq!(u.total_tokens, 230);
}

#[test]
fn codex_raw_usage_string_token_count() {
    // Some Codex log variants write counts as strings.
    let s = r#"{"input_tokens":"100","output_tokens":"50"}"#;
    let u: CodexRawUsage = serde_json::from_str(s).unwrap();
    assert_eq!(u.input_tokens, 100);
    assert_eq!(u.output_tokens, 50);
}

#[test]
fn codex_raw_usage_non_object_field_is_none() {
    // CodexLogEntry.usage may be a boolean in malformed logs — skip gracefully.
    let s = r#"{"timestamp":"2026-01-01T00:00:00Z","usage":true}"#;
    let e: CodexLogEntry<'_> = serde_json::from_str(s).unwrap();
    assert!(e.usage.is_none());
}

#[test]
fn subtract_raw_usage_computes_delta() {
    let prev = CodexRawUsage {
        input_tokens: 100,
        output_tokens: 50,
        ..Default::default()
    };
    let current = CodexRawUsage {
        input_tokens: 300,
        output_tokens: 120,
        ..Default::default()
    };
    let delta = subtract_raw_usage(&current, Some(&prev));
    assert_eq!(delta.input_tokens, 200);
    assert_eq!(delta.output_tokens, 70);
}

#[test]
fn parse_codex_session_event_msg() {
    use std::io::Write as _;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let sessions_dir = dir.path();
    let path = sessions_dir.join("session-a.jsonl");

    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        r#"{{"type":"turn_context","payload":{{"model":"gpt-5"}}}}"#
    )
    .unwrap();
    writeln!(
        f,
        r#"{{"type":"event_msg","timestamp":"2026-01-01T10:00:00.000Z","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":100,"output_tokens":50}}}}}}}}"#
    ).unwrap();

    let events = parse_codex_session(&path, 0, &mut CodexSpendState::default()).0;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].input_tokens, 100);
    assert_eq!(events[0].output_tokens, 50);
    assert_eq!(events[0].model.as_deref(), Some("gpt-5"));
}

#[test]
fn parse_codex_session_cumulative_total_subtracted() {
    use std::io::Write as _;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let sessions_dir = dir.path();
    let path = sessions_dir.join("session-b.jsonl");

    let mut f = std::fs::File::create(&path).unwrap();
    // First event: total = 100/50
    writeln!(
        f,
        r#"{{"type":"event_msg","timestamp":"2026-01-01T10:00:00.000Z","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":100,"output_tokens":50}}}}}}}}"#
    ).unwrap();
    // Second event: total = 300/120 → delta = 200/70
    writeln!(
        f,
        r#"{{"type":"event_msg","timestamp":"2026-01-01T10:01:00.000Z","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":300,"output_tokens":120}}}}}}}}"#
    ).unwrap();

    let events = parse_codex_session(&path, 0, &mut CodexSpendState::default()).0;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].input_tokens, 100);
    assert_eq!(events[1].input_tokens, 200);
    assert_eq!(events[1].output_tokens, 70);
}

#[test]
fn parse_codex_session_headless() {
    use std::io::Write as _;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let sessions_dir = dir.path();
    let path = sessions_dir.join("exec.jsonl");

    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        r#"{{"model":"gpt-5","timestamp":"2026-01-01T10:00:00.000Z","usage":{{"input_tokens":200,"output_tokens":80}}}}"#
    ).unwrap();

    let events = parse_codex_session(&path, 0, &mut CodexSpendState::default()).0;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].input_tokens, 200);
    assert_eq!(events[0].output_tokens, 80);
    assert_eq!(events[0].model.as_deref(), Some("gpt-5"));
}
