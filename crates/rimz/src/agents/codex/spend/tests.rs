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
fn discovery_includes_archives_and_prefers_the_active_copy() {
    let dir = TempDir::new().unwrap();
    let active = dir.path().join("sessions/2026/01/01");
    let archived = dir.path().join("archived_sessions/2026/01/01");
    std::fs::create_dir_all(&active).unwrap();
    std::fs::create_dir_all(&archived).unwrap();
    std::fs::write(active.join("duplicate.jsonl"), "active\n").unwrap();
    std::fs::write(archived.join("duplicate.jsonl"), "archived\n").unwrap();
    std::fs::write(archived.join("archived-only.jsonl"), "archived-only\n").unwrap();

    let files = codex_session_files_from_homes(&[dir.path().to_path_buf()]);

    assert_eq!(
        files,
        vec![
            archived.join("archived-only.jsonl"),
            active.join("duplicate.jsonl"),
        ]
    );
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
fn forked_rollout_skips_copied_history_and_keeps_its_cumulative_baseline() {
    let (_dir, path) = write_session(
        "fork.jsonl",
        &[
            r#"{"type":"session_meta","payload":{"id":"fork","forked_from_id":"parent"}}"#,
            r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:00.100Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":50}}}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:00.200Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"output_tokens":120}}}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:01:00.000Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":450,"output_tokens":170}}}}"#,
        ],
    );

    let events = parse_codex_session(&path, 0, &mut CodexSpendState::default()).0;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].input_tokens, 150);
    assert_eq!(events[0].output_tokens, 50);
    assert_eq!(events[0].model.as_deref(), Some("gpt-5"));
    assert_eq!(events[0].timestamp, "2026-01-01T10:01:00.000Z");
}

#[test]
fn fork_with_one_usage_record_keeps_that_usage() {
    let (_dir, path) = write_session(
        "fork-short.jsonl",
        &[
            r#"{"type":"session_meta","payload":{"id":"fork","forked_from_id":"parent"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:00.100Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":50}}}}"#,
        ],
    );

    let events = parse_codex_session(&path, 0, &mut CodexSpendState::default()).0;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].input_tokens, 100);
    assert_eq!(events[0].output_tokens, 50);
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

#[test]
fn codex_cached_input_bills_at_input_rate_without_explicit_cache_read_cost() {
    // input_tokens counts the whole prompt; cached_tokens is the hit slice.
    let line = r#"{"model":"codex-test","timestamp":"2026-01-01T10:00:00.000Z","usage":{"input_tokens":200,"cached_tokens":80,"output_tokens":50}}"#;
    let (_dir, path) = write_session("cache-rate.jsonl", &[line]);

    // No explicit cache-read rate: the cached slice bills at the full input
    // rate, matching ccusage's Codex cost path (a model without a discounted
    // cache-read rate does not discount cached tokens).
    let implicit = PriceBook::from_litellm_json(
        r#"{"codex-test": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6}}"#,
    );
    let entries = parse_codex_spend(&path, None, &implicit).entries;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input, 120, "input is the uncached slice");
    assert_eq!(entries[0].cache_read, 80);
    // (120 uncached + 80 cached) * input_rate + 50 * output_rate.
    let expected = 200.0 * 1e-6 + 50.0 * 2e-6;
    assert!(
        (entries[0].cost_usd - expected).abs() < 1e-15,
        "implicit cache-read cost was {}",
        entries[0].cost_usd
    );

    // An explicit cache-read rate discounts the cached slice at that rate.
    let explicit = PriceBook::from_litellm_json(
        r#"{"codex-test": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
                            "cache_read_input_token_cost": 1e-7}}"#,
    );
    let entries = parse_codex_spend(&path, None, &explicit).entries;
    let expected = 120.0 * 1e-6 + 80.0 * 1e-7 + 50.0 * 2e-6;
    assert!(
        (entries[0].cost_usd - expected).abs() < 1e-15,
        "explicit cache-read cost was {}",
        entries[0].cost_usd
    );
}

#[test]
fn dedup_key_separates_events_differing_only_in_reasoning_or_total() {
    let base = wire::CodexTokenEvent {
        timestamp: "2026-01-01T10:00:00.000Z".to_string(),
        model: Some("gpt-5".to_string()),
        input_tokens: 100,
        cached_input_tokens: 10,
        output_tokens: 50,
        reasoning_output_tokens: 5,
        total_tokens: 155,
    };
    let key = |event: &wire::CodexTokenEvent| {
        codex_event_dedup_key(&event.timestamp, event.model.as_deref().unwrap(), event)
    };

    let mut diff_reasoning = base.clone();
    diff_reasoning.reasoning_output_tokens = 9;
    let mut diff_total = base.clone();
    diff_total.total_tokens = 999;

    assert_ne!(key(&base), key(&diff_reasoning));
    assert_ne!(key(&base), key(&diff_total));
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
    assert_eq!(parsed.origin.as_deref(), Some(Path::new(cwd)));

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
        second.origin.as_deref(),
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
    assert_eq!(parsed.origin, None);
}
