use super::*;

use std::io::Write as _;

use tempfile::TempDir;

/// The line classifier feeding `parse_codex_session` — exercised here
/// through the kind probe so each accepted/skipped shape stays pinned.
fn token_line(line: &[u8]) -> bool {
    codex_line_kind(line).is_some()
}

fn write_session(filename: &str, lines: &[&str]) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(filename);
    let mut f = std::fs::File::create(&path).unwrap();
    for line in lines {
        writeln!(f, "{line}").unwrap();
    }
    (dir, path)
}

#[test]
fn token_line_classifies_known_usage_shapes_only() {
    for line in [
        br#"{"type":"event_msg","payload":{"type":"token_count","info":{}}}"#.as_slice(),
        br#"{"type":"turn_context","payload":{}}"#,
        br#"{"usage":{"input_tokens":100,"output_tokens":50},"model":"gpt-5"}"#,
        br#"{"prompt_tokens":100,"completion_tokens":50,"model":"gpt-5"}"#,
    ] {
        assert!(
            token_line(line),
            "accepted {}",
            String::from_utf8_lossy(line)
        );
    }
    for line in [
        br#"{"type":"event_msg","payload":{"type":"tool_call"}}"#.as_slice(),
        br#"{"type":"other","foo":"bar"}"#,
        b"{}",
    ] {
        assert!(
            !token_line(line),
            "skipped {}",
            String::from_utf8_lossy(line)
        );
    }
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
fn codex_raw_usage_accepts_aliases_strings_and_rejects_non_object_field() {
    // OpenAI alias names
    let s = r#"{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}"#;
    let u: CodexRawUsage = serde_json::from_str(s).unwrap();
    assert_eq!(u.input_tokens, 100);
    assert_eq!(u.output_tokens, 50);
    assert_eq!(u.total_tokens, 150);

    let s = r#"{"input_tokens":200,"cached_tokens":80,"output_tokens":30}"#;
    let u: CodexRawUsage = serde_json::from_str(s).unwrap();
    assert_eq!(u.input_tokens, 200);
    assert_eq!(u.cached_input_tokens, 80);
    assert_eq!(u.output_tokens, 30);
    assert_eq!(u.total_tokens, 230);

    // Some Codex log variants write counts as strings.
    let s = r#"{"input_tokens":"100","output_tokens":"50"}"#;
    let u: CodexRawUsage = serde_json::from_str(s).unwrap();
    assert_eq!(u.input_tokens, 100);
    assert_eq!(u.output_tokens, 50);

    // CodexLogEntry.usage may be a boolean in malformed logs — skip gracefully.
    let s = r#"{"timestamp":"2026-01-01T00:00:00Z","usage":true}"#;
    let e: CodexLogEntry<'_> = serde_json::from_str(s).unwrap();
    assert!(e.usage.is_none());
}

#[test]
fn parse_codex_session_usage_shapes_and_cumulative_deltas() {
    let (_dir, path) = write_session(
        "session-a.jsonl",
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:00.000Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":50}}}}"#,
        ],
    );

    let events = parse_codex_session(&path, 0, &mut CodexSpendState::default()).0;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].input_tokens, 100);
    assert_eq!(events[0].output_tokens, 50);
    assert_eq!(events[0].model.as_deref(), Some("gpt-5"));

    let (_dir, path) = write_session(
        "session-b.jsonl",
        &[
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:00.000Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":50}}}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:01:00.000Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"output_tokens":120}}}}"#,
        ],
    );

    let events = parse_codex_session(&path, 0, &mut CodexSpendState::default()).0;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].input_tokens, 100);
    assert_eq!(events[1].input_tokens, 200);
    assert_eq!(events[1].output_tokens, 70);

    let (_dir, path) = write_session(
        "exec.jsonl",
        &[
            r#"{"model":"gpt-5","timestamp":"2026-01-01T10:00:00.000Z","usage":{"input_tokens":200,"output_tokens":80}}"#,
        ],
    );

    let events = parse_codex_session(&path, 0, &mut CodexSpendState::default()).0;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].input_tokens, 200);
    assert_eq!(events[0].output_tokens, 80);
    assert_eq!(events[0].model.as_deref(), Some("gpt-5"));
}

#[test]
fn unpriced_model_is_recorded_as_unknown() {
    use std::collections::BTreeMap;

    let timestamp = "2026-01-01T10:00:00.000Z";
    let (_dir, path) = write_session(
        "session.jsonl",
        &[&format!(
            r#"{{"model":"new-codex-release","timestamp":"{timestamp}","usage":{{"input_tokens":200,"output_tokens":80}}}}"#
        )],
    );

    let parsed = parse_codex_spend(&path, None, &PriceBook::from_litellm_json("{}"));

    assert_eq!(parsed.entries.len(), 1);
    assert_eq!(parsed.entries[0].cost_usd, 0.0);
    assert_eq!(parsed.entries[0].input, 200);
    assert_eq!(parsed.entries[0].output, 80);
    assert_eq!(
        parsed.unknown_models,
        BTreeMap::from([(
            "new-codex-release".to_owned(),
            iso_to_unix_secs(timestamp).unwrap()
        )])
    );
}

fn gpt5_book() -> PriceBook {
    PriceBook::from_litellm_json(
        r#"{"gpt-5": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
                      "cache_read_input_token_cost": 1e-7}}"#,
    )
}

const TOKEN_COUNT_LINE: &str = r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:00.000Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":50}}}}"#;

#[test]
fn session_meta_cwd_stamps_origin_survives_resume_and_is_none_when_absent() {
    let cwd = "/home/user/code/rimz-worktrees/budget-reset";
    let meta = format!(r#"{{"type":"session_meta","payload":{{"id":"s","cwd":"{cwd}"}}}}"#);

    // The rollout's session_meta cwd stamps each entry's durable origin.
    let (_dir, path) = write_session(
        "rollout.jsonl",
        &[
            meta.as_str(),
            r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#,
            TOKEN_COUNT_LINE,
        ],
    );
    let parsed = parse_codex_spend(&path, None, &gpt5_book());
    assert_eq!(parsed.entries.len(), 1);
    assert_eq!(
        parsed.entries[0].origin_path.as_deref(),
        Some(Path::new(cwd))
    );

    // The cwd rides the resume cursor's state: a first parse over the header-only
    // prefix prices nothing yet, but a turn appended afterwards is still stamped
    // without re-reading the header.
    let (_dir, path) = write_session(
        "resume.jsonl",
        &[
            meta.as_str(),
            r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#,
        ],
    );
    let first = parse_codex_spend(&path, None, &gpt5_book());
    assert!(first.entries.is_empty());
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(f, "{TOKEN_COUNT_LINE}").unwrap();
    drop(f);
    let second = parse_codex_spend(&path, Some(&first.cursor), &gpt5_book());
    assert_eq!(second.entries.len(), 1);
    assert_eq!(
        second.entries[0].origin_path.as_deref(),
        Some(Path::new(cwd)),
        "the resume cursor carries the session cwd to entries appended after the header"
    );

    // A headless rollout without session_meta leaves origin for the snapshot
    // override to fill.
    let (_dir, path) = write_session(
        "headless.jsonl",
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#,
            TOKEN_COUNT_LINE,
        ],
    );
    let parsed = parse_codex_spend(&path, None, &gpt5_book());
    assert_eq!(parsed.entries.len(), 1);
    assert_eq!(parsed.entries[0].origin_path, None);
}
