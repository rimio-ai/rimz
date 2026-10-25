use super::super::ClaudeAdapter;
use super::*;
use crate::agents::capabilities::SpendingCapability;
use crate::agents::spending::{ResponseTiming, ToolCounts, session_entries};
use std::io::Write as _;
use tempfile::TempDir;

#[test]
fn streamed_response_timing_survives_interleaved_inputs_and_resume() {
    let dir = TempDir::new().unwrap();
    let lines = [
        r#"{"parentUuid":"parent","type":"user","timestamp":"2026-01-01T10:00:02.100Z","message":{"content":"prompt"}}"#,
        r#"{"type":"user","timestamp":"2026-01-01T10:00:01.100Z","message":{"content":"older input"}}"#,
        r#"{"timestamp":"2026-01-01T10:00:04.600Z","costUSD":1,"requestId":"req","message":{"id":"msg","usage":{"output_tokens":300}}}"#,
        r#"{"parentUuid":"parent","type":"user","timestamp":"2026-01-01T10:00:04.800Z","message":{"content":[{"type":"tool_result","content":"result"}]}}"#,
        r#"{"timestamp":"2026-01-01T10:00:06.600Z","costUSD":1,"requestId":"req","message":{"id":"msg","usage":{"output_tokens":300}}}"#,
    ];
    let path = write_jsonl(dir.path(), "stream.jsonl", &lines[..3]);
    let first = ClaudeAdapter.parse_spend(&path, None, &no_prices());
    assert_eq!(
        first.entries[0].timing,
        Some(ResponseTiming {
            ms: 2_500,
            output: 300
        })
    );
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    for line in &lines[3..] {
        writeln!(file, "{line}").unwrap();
    }
    let resumed = ClaudeAdapter.parse_spend(&path, Some(&first.cursor), &no_prices());
    let cold = ClaudeAdapter.parse_spend(&path, None, &no_prices());
    assert_eq!(resumed.entries, cold.entries[1..]);
    assert_eq!(resumed.cursor, cold.cursor);
    let kept = session_entries(&cold.entries, "session");
    assert_eq!(kept.len(), 1);
    assert_eq!(
        kept[0].timing,
        Some(ResponseTiming {
            ms: 4_500,
            output: 300
        })
    );
    let mut chunk = cold.entries.clone();
    crate::agents::spending::dedup_chunk(&mut chunk);
    assert_eq!(chunk, vec![kept[0].clone()]);
}

#[test]
fn sidechain_user_lines_leave_the_response_start_unmoved() {
    let dir = TempDir::new().unwrap();
    let path = write_jsonl(
        dir.path(),
        "sidechain.jsonl",
        &[
            r#"{"type":"user","timestamp":"2026-01-01T10:00:00.000Z","message":{"content":"prompt"}}"#,
            r#"{"type":"user","isSidechain":true,"timestamp":"2026-01-01T10:00:01.000Z","message":{"content":"child prompt"}}"#,
            r#"{"timestamp":"2026-01-01T10:00:03.000Z","costUSD":1,"requestId":"req","message":{"id":"msg","usage":{"output_tokens":300}}}"#,
        ],
    );
    assert_eq!(
        ClaudeAdapter.parse_spend(&path, None, &no_prices()).entries[0].timing,
        Some(ResponseTiming {
            ms: 3_000,
            output: 300
        })
    );
}

#[test]
fn pricing_fingerprint_shares_an_object_cursor_with_response_timing() {
    let dir = TempDir::new().unwrap();
    let path = write_jsonl(dir.path(), "cursor.jsonl", &[]);
    let parsed = ClaudeAdapter.parse_spend(&path, None, &no_prices());
    assert!(
        parsed
            .cursor
            .state
            .as_ref()
            .is_some_and(serde_json::Value::is_object)
    );
    assert!(ClaudeAdapter.spend_pricing_is_current(&parsed.cursor));
    let mut stale = parsed.cursor;
    stale.state.as_mut().unwrap()["pricing_fingerprint"] = serde_json::json!("old-pricing");
    assert!(!ClaudeAdapter.spend_pricing_is_current(&stale));
}

#[test]
fn timing_filters_short_responses_and_leaves_advisor_calls_untimed() {
    let dir = TempDir::new().unwrap();
    let prices = PriceBook::from_litellm_json(r#"{"advisor":{"output_cost_per_token":0.00001}}"#);
    let path = write_jsonl(
        dir.path(),
        "advisor.jsonl",
        &[
            r#"{"type":"user","timestamp":"2026-01-01T10:00:00.000Z"}"#,
            r#"{"timestamp":"2026-01-01T10:00:02.000Z","costUSD":1,"message":{"id":"msg","usage":{"output_tokens":200,"iterations":[{"type":"advisor_message","model":"advisor","output_tokens":200}]}}}"#,
        ],
    );
    let parsed = ClaudeAdapter.parse_spend(&path, None, &prices);
    assert_eq!(parsed.entries.len(), 2);
    assert_eq!(
        parsed.entries[0].timing,
        Some(ResponseTiming {
            ms: 2_000,
            output: 200
        })
    );
    assert_eq!(parsed.entries[1].timing, None);
    for (end, output) in [
        ("00.200", 100),
        ("00.100", 100),
        ("01.000", 99),
        ("05:01.000", 100),
    ] {
        let end = if end.contains(':') {
            format!("10:{end}")
        } else {
            format!("10:00:{end}")
        };
        let line = format!(
            r#"{{"timestamp":"2026-01-01T{end}Z","costUSD":1,"message":{{"id":"msg","usage":{{"output_tokens":{output}}}}}}}"#
        );
        let path = write_jsonl(
            dir.path(),
            "filtered.jsonl",
            &[
                r#"{"type":"user","timestamp":"2026-01-01T10:00:00.000Z"}"#,
                &line,
            ],
        );
        assert_eq!(
            ClaudeAdapter.parse_spend(&path, None, &no_prices()).entries[0].timing,
            None
        );
    }
}

/// An empty price book: the `costUSD`-bearing fixtures below never consult it,
/// so spend is driven entirely by their logged cost.
fn no_prices() -> PriceBook {
    PriceBook::from_litellm_json("{}")
}

fn write_jsonl(dir: &Path, filename: &str, lines: &[&str]) -> PathBuf {
    let path = dir.join(filename);
    let mut f = std::fs::File::create(&path).unwrap();
    for line in lines {
        writeln!(f, "{line}").unwrap();
    }
    path
}

#[test]
fn config_dirs_fall_back_to_the_default_roots_when_no_env_root_has_projects() {
    let dir = TempDir::new().unwrap();
    let native = dir.path().join(".claude");
    std::fs::create_dir_all(native.join("projects")).unwrap();
    let env = BTreeMap::from([
        ("HOME".to_owned(), dir.path().display().to_string()),
        (
            "CLAUDE_CONFIG_DIR".to_owned(),
            dir.path().join("empty").display().to_string(),
        ),
    ]);
    assert_eq!(claude_config_dirs_from(&env), vec![native]);
    assert_eq!(claude_config_roots(&env), vec![dir.path().join("empty")]);
}

fn claude_line(date: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
    format!(
        r#"{{"timestamp":"{date}T10:00:00.000Z","costUSD":{cost},"requestId":"{req_id}","message":{{"id":"{msg_id}","usage":{{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":3,"cache_read_input_tokens":7}}}}}}"#
    )
}

fn sidechain_line(date: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
    format!(
        r#"{{"timestamp":"{date}T10:00:00.000Z","costUSD":{cost},"requestId":"{req_id}","isSidechain":true,"message":{{"id":"{msg_id}","usage":{{"input_tokens":50000}}}}}}"#
    )
}

#[test]
fn skips_lines_without_usage_object_and_captures_token_components() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().join("repo");
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":99.0}"#,
            &format!(
                r#"{{"timestamp":"2026-01-01T10:00:00.000Z","cwd":"{}","costUSD":0.5,"requestId":"req-1","message":{{"id":"msg-1","usage":{{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":3,"cache_read_input_tokens":7}}}}}}"#,
                cwd.display()
            ),
        ],
    );
    let parsed = parse_claude_spend(&file, None, &no_prices());
    let entries = parsed.entries;
    assert_eq!(entries.len(), 1);
    assert!((entries[0].cost_usd - 0.5).abs() < 1e-9);
    // The components are kept apart here; window aggregation folds
    // cache-write into input/total downstream.
    assert_eq!(entries[0].input, 10);
    assert_eq!(entries[0].output, 5);
    assert_eq!(entries[0].cache_write, 3);
    assert_eq!(entries[0].cache_read, 7);
    assert_eq!(parsed.origin.as_deref(), Some(cwd.as_path()));
}

#[test]
fn captures_named_tool_use_blocks_on_the_priced_response() {
    let dir = TempDir::new().unwrap();
    let file = write_jsonl(
        dir.path(),
        "tools.jsonl",
        &[
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.5,"requestId":"req-1","message":{"id":"msg-1","content":[{"type":"tool_use","name":"Read"},{"type":"text","text":"working"},{"type":"tool_use","name":"Read"},{"type":"tool_use","name":"Bash"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
        ],
    );

    let entries = parse_claude_spend(&file, None, &no_prices()).entries;

    assert_eq!(
        entries[0].tool_calls,
        ToolCounts::from(BTreeMap::from([
            ("Bash".to_owned(), 1),
            ("Read".to_owned(), 2)
        ]))
    );
}

#[test]
fn cwd_null_keeps_global_spend_with_unknown_origin() {
    let dir = TempDir::new().unwrap();
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","cwd":null,"costUSD":0.5,"requestId":"req-1","message":{"id":"msg-1","usage":{"input_tokens":10,"output_tokens":5}}}"#,
        ],
    );

    let parsed = parse_claude_spend(&file, None, &no_prices());
    let entries = parsed.entries;

    assert_eq!(entries.len(), 1);
    assert!((entries[0].cost_usd - 0.5).abs() < 1e-9);
    assert_eq!(parsed.origin, None);
}

#[test]
fn prices_from_tokens_when_cost_usd_absent() {
    // Current Claude transcripts carry no `costUSD`; cost is reconstructed
    // from the token usage through the model table.
    let dir = TempDir::new().unwrap();
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","requestId":"req-1","message":{"id":"msg-1","model":"claude-test","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":200,"cache_read_input_tokens":800}}}"#,
        ],
    );
    let prices = PriceBook::from_litellm_json(
        r#"{"claude-test": {"input_cost_per_token": 3e-6, "output_cost_per_token": 15e-6,
                            "cache_read_input_token_cost": 3e-7,
                            "cache_creation_input_token_cost": 3.75e-6}}"#,
    );
    let entries = parse_claude_spend(&file, None, &prices).entries;
    assert_eq!(entries.len(), 1);
    // 100*3e-6 + 50*15e-6 + 200*3.75e-6 + 800*3e-7
    //   = 3e-4 + 7.5e-4 + 7.5e-4 + 2.4e-4 = 2.04e-3
    assert!(
        (entries[0].cost_usd - 2.04e-3).abs() < 1e-12,
        "got {}",
        entries[0].cost_usd
    );
    assert_eq!(entries[0].input, 100);
    assert_eq!(entries[0].output, 50);
    assert_eq!(entries[0].cache_write, 200);
    assert_eq!(entries[0].cache_read, 800);
}

#[test]
fn counts_advisor_iterations_as_separate_model_usage() {
    let dir = TempDir::new().unwrap();
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            r#"{"timestamp":"2026-05-22T02:34:40.000Z","version":"1.2.3","sessionId":"session-1","requestId":"req-1","message":{"id":"msg-1","model":"main-model","usage":{"input_tokens":2,"output_tokens":491,"cache_creation_input_tokens":7853,"cache_read_input_tokens":226584,"iterations":[{"type":"message","input_tokens":1,"output_tokens":45},{"type":"advisor_message","model":"advisor-model","input_tokens":159419,"output_tokens":7805,"cache_creation_input_tokens":3,"cache_read_input_tokens":4}]}}}"#,
        ],
    );
    let prices = PriceBook::from_litellm_json(
        r#"{
            "main-model": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6},
            "advisor-model": {"input_cost_per_token": 3e-6, "output_cost_per_token": 4e-6,
                              "cache_creation_input_token_cost": 5e-6,
                              "cache_read_input_token_cost": 6e-6}
        }"#,
    );

    let entries = parse_claude_spend(&file, None, &prices).entries;

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].model.as_deref(), Some("main-model"));
    assert_eq!(entries[0].input, 2);
    assert_eq!(entries[1].model.as_deref(), Some("advisor-model"));
    assert_eq!(entries[1].input, 159_419);
    assert_eq!(entries[1].output, 7_805);
    assert_eq!(entries[1].cache_write, 3);
    assert_eq!(entries[1].cache_read, 4);
    assert_eq!(entries[1].message_id.as_deref(), Some("msg-1:advisor:1"));
    assert_eq!(entries[1].request_id.as_deref(), Some("req-1"));
    let expected = 159_419.0 * 3e-6 + 7_805.0 * 4e-6 + 3.0 * 5e-6 + 4.0 * 6e-6;
    assert!((entries[1].cost_usd - expected).abs() < 1e-12);
}

#[test]
fn one_hour_cache_creation_uses_twice_input_rate() {
    let dir = TempDir::new().unwrap();
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","requestId":"req-1","message":{"id":"msg-1","model":"test-model","usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":20,"cache_creation":{"ephemeral_5m_input_tokens":0,"ephemeral_1h_input_tokens":20},"cache_read_input_tokens":0}}}"#,
        ],
    );
    let prices = PriceBook::from_litellm_json(
        r#"{"test-model": {"input_cost_per_token": 1.0, "output_cost_per_token": 10.0,
                            "cache_creation_input_token_cost": 1.25,
                            "cache_read_input_token_cost": 0.1}}"#,
    );

    let entries = parse_claude_spend(&file, None, &prices).entries;

    assert_eq!(entries.len(), 1);
    assert!((entries[0].cost_usd - 40.0).abs() < f64::EPSILON);
    assert_eq!(entries[0].cache_write, 20);
}

#[test]
fn mixed_cache_creation_breakdown_matches_ccusage_formula() {
    let dir = TempDir::new().unwrap();
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","requestId":"req-1","message":{"id":"msg-1","model":"test-model","usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":999,"cache_creation":{"ephemeral_5m_input_tokens":10,"ephemeral_1h_input_tokens":20},"cache_read_input_tokens":30}}}"#,
        ],
    );
    let prices = PriceBook::from_litellm_json(
        r#"{"test-model": {"input_cost_per_token": 1.0, "output_cost_per_token": 10.0,
                            "cache_creation_input_token_cost": 1.25,
                            "cache_read_input_token_cost": 0.1}}"#,
    );

    let entries = parse_claude_spend(&file, None, &prices).entries;

    assert_eq!(entries.len(), 1);
    assert!((entries[0].cost_usd - 55.5).abs() < f64::EPSILON);
    assert_eq!(entries[0].cache_write, 30);
}

#[test]
fn pricing_applies_200k_tiers() {
    let dir = TempDir::new().unwrap();
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","requestId":"req-1","message":{"id":"msg-1","model":"test-model","usage":{"input_tokens":200001,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#,
        ],
    );
    let prices = PriceBook::from_litellm_json(
        r#"{"test-model": {"input_cost_per_token": 1.0, "output_cost_per_token": 10.0,
                            "input_cost_per_token_above_200k_tokens": 2.0}}"#,
    );

    let entries = parse_claude_spend(&file, None, &prices).entries;

    assert_eq!(entries.len(), 1);
    assert!((entries[0].cost_usd - 200_002.0).abs() < f64::EPSILON);
}

#[test]
fn pricing_applies_fast_multiplier() {
    let dir = TempDir::new().unwrap();
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","requestId":"req-1","message":{"id":"msg-1","model":"test-model","usage":{"input_tokens":10,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"speed":"fast"}}}"#,
        ],
    );
    let prices = PriceBook::from_litellm_json(
        r#"{"test-model": {"input_cost_per_token": 1.0, "output_cost_per_token": 10.0,
                            "provider_specific_entry": {"fast": 2.5}}}"#,
    );

    let entries = parse_claude_spend(&file, None, &prices).entries;

    assert_eq!(entries.len(), 1);
    assert!((entries[0].cost_usd - 25.0).abs() < f64::EPSILON);
    assert!(entries[0].fast);
}

#[test]
fn unknown_model_tracking_records_priceable_names_only() {
    let dir = TempDir::new().unwrap();
    let timestamp = "2026-01-01T10:00:00.000Z";
    let unknown = write_jsonl(
        dir.path(),
        "unknown.jsonl",
        &[&format!(
            r#"{{"timestamp":"{timestamp}","requestId":"req-1","message":{{"id":"msg-1","model":"claude-new-release","usage":{{"input_tokens":100,"output_tokens":50}}}}}}"#
        )],
    );

    let parsed = parse_claude_spend(&unknown, None, &no_prices());

    assert_eq!(parsed.entries.len(), 1);
    assert_eq!(parsed.entries[0].cost_usd, 0.0);
    assert_eq!(parsed.entries[0].input, 100);
    assert_eq!(parsed.entries[0].output, 50);
    assert_eq!(
        parsed.unknown_models,
        BTreeMap::from([(
            "claude-new-release".to_owned(),
            crate::agents::spending::iso_to_unix_secs(timestamp).unwrap()
        )])
    );

    let synthetic = write_jsonl(
        dir.path(),
        "synthetic.jsonl",
        &[
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","requestId":"req-1","message":{"id":"msg-1","model":"<synthetic>","usage":{"input_tokens":100,"output_tokens":50}}}"#,
        ],
    );
    let parsed = parse_claude_spend(&synthetic, None, &no_prices());
    assert!(parsed.entries.is_empty());
    assert!(parsed.unknown_models.is_empty());

    let logged_cost = write_jsonl(
        dir.path(),
        "logged-cost.jsonl",
        &[
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.25,"requestId":"req-1","message":{"id":"msg-1","model":"claude-new-release","usage":{"input_tokens":100,"output_tokens":50}}}"#,
        ],
    );
    let parsed = parse_claude_spend(&logged_cost, None, &no_prices());
    assert_eq!(parsed.entries.len(), 1);
    assert!(parsed.unknown_models.is_empty());
}

#[test]
fn parse_is_raw_and_dedup_lives_downstream() {
    // Downstream batch consumers and live folds own dedup, so an
    // incremental suffix parse keeps every copy without earlier lines.
    let dir = TempDir::new().unwrap();

    // Byte-identical exact-key duplicate: both kept.
    let line = claude_line("2026-01-01", 1.0, "msg-a", "req-a");
    let dup = write_jsonl(dir.path(), "dup.jsonl", &[&line, &line]);
    assert_eq!(
        parse_claude_spend(&dup, None, &no_prices()).entries.len(),
        2
    );

    // Sidechain replay sharing a msg_id but a distinct req_id is a separate
    // exact-key, so both ride through; cross-file suppression is downstream.
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            &sidechain_line("2026-01-01", 5.0, "msg-x", "req-sc"),
            &claude_line("2026-01-01", 0.05, "msg-x", "req-main"),
        ],
    );
    let entries = parse_claude_spend(&file, None, &no_prices()).entries;
    assert_eq!(entries.len(), 2);
    let sc = entries.iter().find(|e| e.is_sidechain).unwrap();
    let main = entries.iter().find(|e| !e.is_sidechain).unwrap();
    assert!((sc.cost_usd - 5.0).abs() < 1e-9);
    assert!((main.cost_usd - 0.05).abs() < 1e-9);
}

#[test]
fn invalid_or_unsupported_cost_lines_are_skipped() {
    let dir = TempDir::new().unwrap();
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.0,"message":{"usage":{"input_tokens":1}}}"#,
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":-1.0,"message":{"usage":{"input_tokens":1}}}"#,
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":null,"message":{"usage":{"input_tokens":1}}}"#,
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.1,"message":{"id":"msg-1","model":null,"usage":{"input_tokens":1}}}"#,
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.1,"message":{"id":"msg-5","usage":{"input_tokens":1,"cache_read_input_tokens":null}}}"#,
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.1,"isApiErrorMessage":null,"message":{"id":"msg-6","usage":{"input_tokens":1}}}"#,
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.1,"requestId":"","message":{"id":"msg-2","usage":{"input_tokens":1}}}"#,
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.1,"version":"dev","message":{"id":"msg-3","usage":{"input_tokens":1}}}"#,
            &claude_line("2026-01-01", 0.5, "msg-4", "req-4"),
        ],
    );
    let entries = parse_claude_spend(&file, None, &no_prices()).entries;
    assert_eq!(entries.len(), 1);
    assert!((entries[0].cost_usd - 0.5).abs() < 1e-9);
}

#[test]
fn nested_null_iteration_model_keeps_main_model_usage() {
    // Current Claude Code logs `usage.iterations[].model: null` on the main
    // response; only the schema's top-level `message.model` may reject it.
    let dir = TempDir::new().unwrap();
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            r#"{"timestamp":"2026-09-12T16:44:35.983Z","message":{"id":"msg-repro","model":"claude-fable-5-1","usage":{"input_tokens":1,"iterations":[{"type":"message","model":null}]}}}"#,
        ],
    );
    let parsed = parse_claude_spend(&file, None, &no_prices());
    assert_eq!(parsed.entries.len(), 1);
    assert_eq!(parsed.entries[0].model.as_deref(), Some("claude-fable-5-1"));
    assert_eq!(parsed.entries[0].input, 1);
    assert!(parsed.unknown_models.contains_key("claude-fable-5-1"));
}

#[test]
fn lenient_token_fields_keep_entry_when_a_count_is_mistyped() {
    // A token count logged as a string or float must not drop the whole
    // entry: the mistyped field coerces to absent (0) and the rest prices.
    let dir = TempDir::new().unwrap();
    let file = write_jsonl(
        dir.path(),
        "chat.jsonl",
        &[
            r#"{"timestamp":"2026-01-01T10:00:00.000Z","requestId":"req-1","message":{"id":"msg-1","model":"test-model","usage":{"input_tokens":100,"output_tokens":"oops","cache_read_input_tokens":50.5}}}"#,
        ],
    );
    let prices = PriceBook::from_litellm_json(
        r#"{"test-model": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
                            "cache_read_input_token_cost": 1e-7}}"#,
    );
    let entries = parse_claude_spend(&file, None, &prices).entries;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input, 100);
    assert_eq!(entries[0].output, 0, "mistyped string coerces to absent");
    assert_eq!(entries[0].cache_read, 0, "mistyped float coerces to absent");
    assert!((entries[0].cost_usd - 100.0 * 1e-6).abs() < 1e-15);
}

#[test]
fn is_semver_prefix_accepts_valid_versions() {
    assert!(is_semver_prefix("1.0.0"));
    assert!(is_semver_prefix("12.34.56"));
    assert!(is_semver_prefix("1.0.0-beta"));
    assert!(!is_semver_prefix("dev"));
    assert!(!is_semver_prefix("1.0"));
    assert!(!is_semver_prefix("v1.0.0"));
    assert!(!is_semver_prefix(""));
}
