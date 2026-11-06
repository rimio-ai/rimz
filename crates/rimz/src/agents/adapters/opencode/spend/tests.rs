use super::*;
use tempfile::TempDir;

#[test]
fn compaction_rows_contribute_usage_with_own_or_latest_assistant_model() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("opencode.db");
    let db = crate::testkit::opencode::OpenCodeDb::new(&path, false, false);
    db.session("ses", dir.path());
    db.message(
        "a",
        "ses",
        "assistant",
        1000,
        r#"{"model":{"id":"gpt-priced","providerID":"openai"}}"#,
    );
    db.message("c", "ses", "compaction", 2000, r#"{"status":"completed","cost":0.5,"model":{"id":"gpt-provider","providerID":"openai"},"tokens":{"input":10,"output":2,"reasoning":3}}"#);
    db.message(
        "f",
        "ses",
        "compaction",
        3000,
        r#"{"status":"failed","cost":0,"tokens":{"input":20,"output":4}}"#,
    );
    db.message(
        "cost",
        "ses",
        "compaction",
        4000,
        r#"{"status":"failed","cost":0.25}"#,
    );
    db.message(
        "running",
        "ses",
        "compaction",
        5000,
        r#"{"status":"running"}"#,
    );
    let parsed = parse(&path, None, &prices());
    assert_eq!(parsed.entries.len(), 3);
    let entry = &parsed.entries[0];
    assert_eq!((entry.input, entry.output, entry.ts_secs), (10, 5, 2));
    assert_eq!(entry.cost_usd, 0.5);
    assert_eq!(entry.model.as_deref(), Some("gpt-provider"));
    assert_eq!(entry.thread_id.as_deref(), Some("ses"));
    assert_eq!(parsed.entries[1].model.as_deref(), Some("gpt-priced"));
    assert!((parsed.entries[1].cost_usd - 0.000028).abs() < 1e-12);
    assert_eq!(parsed.entries[2].cost_usd, 0.25);
    assert!(
        super::super::transcript::read_messages(
            &path,
            Some(&crate::ids::AgentSessionId::from("ses"))
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn v2_rows_price_reasoning_and_count_inline_tools() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("opencode.db");
    let db = crate::testkit::opencode::OpenCodeDb::new(&path, false, false);
    db.session("ses", dir.path());
    let data = r#"{"model":{"id":"gpt-priced","providerID":"openai"},"cost":0,"time":{"created":2000},"tokens":{"input":10,"output":20,"reasoning":5,"cache":{"read":30,"write":40}},"content":[{"type":"tool","name":"read"},{"type":"tool","name":"read"},{"type":"text","text":"answer"}]}"#;
    db.message("assistant", "ses", "assistant", 2000, data);
    db.message("user", "ses", "user", 3000, data);
    let parsed = parse(&path, None, &prices());
    assert_eq!(parsed.entries.len(), 1);
    let entry = &parsed.entries[0];
    assert_eq!(
        (
            entry.input,
            entry.output,
            entry.cache_read,
            entry.cache_write
        ),
        (10, 25, 30, 40)
    );
    assert!(
        (entry.cost_usd
            - (10.0 * 0.000001 + 25.0 * 0.000002 + 30.0 * 0.0000001 + 40.0 * 0.0000005))
            .abs()
            < 1e-12
    );
    assert_eq!(entry.tool_calls.get("read"), Some(&2));
    assert_eq!(entry.ts_secs, 2);
    assert_eq!(parsed.origin.as_deref(), Some(dir.path()));
}

fn create_db(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    crate::testkit::opencode::OpenCodeDb::new(&path, false, false);
    path
}

fn insert_message(path: &Path, data: &str) {
    insert_message_for_session(path, "ses", data);
}

fn insert_message_for_session(path: &Path, session_id: &str, data: &str) {
    let db = crate::testkit::opencode::OpenCodeDb::new(path, false, false);
    let index: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM session_message", [], |row| row.get(0))
        .unwrap();
    db.message(
        &format!("msg-{index}"),
        session_id,
        "assistant",
        index,
        data,
    );
}

/// Rewrite the newest row's `data` in place, as OpenCode does when a
/// streaming assistant message completes.
fn update_last_message(path: &Path, data: &str) {
    let db = crate::testkit::opencode::OpenCodeDb::new(path, false, false);
    let id: String = db
        .connection()
        .query_row(
            "SELECT id FROM session_message ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    db.update_message(&id, 0, data);
}

fn prices() -> PriceBook {
    PriceBook::from_litellm_json(
        r#"{
            "gpt-priced": {
                "input_cost_per_token": 0.000001,
                "output_cost_per_token": 0.000002,
                "cache_read_input_token_cost": 0.0000001,
                "cache_creation_input_token_cost": 0.0000005
            },
            "openai/gpt-provider": {
                "input_cost_per_token": 0.000003,
                "output_cost_per_token": 0.000004
            },
            "claude-sonnet-4-5": {
                "input_cost_per_token": 0.000005,
                "output_cost_per_token": 0.000006
            }
        }"#,
    )
}

#[test]
fn parses_stored_cost_tokens_timestamp_and_origin() {
    let dir = TempDir::new().unwrap();
    let cwd = dir.path().join("repo");
    let path = create_db(dir.path(), "opencode.db");
    crate::testkit::opencode::OpenCodeDb::new(&path, false, false).session("ses", &cwd);
    insert_message(
        &path,
        r#"{
                "cost": 0.42,
                "model":{"id":"gpt-priced","providerID":"openai"},
                "time": { "created": 1780590149011 },
                "tokens": {
                    "input": 100,
                    "output": 20,
                    "cache": { "read": 30, "write": 40 }
                }
            }"#,
    );

    let parsed = parse(&path, None, &prices());
    assert_eq!(parsed.entries.len(), 1);
    let entry = &parsed.entries[0];
    assert!((entry.cost_usd - 0.42).abs() < 1e-9);
    assert_eq!(entry.input, 100);
    assert_eq!(entry.output, 20);
    assert_eq!(entry.cache_read, 30);
    assert_eq!(entry.cache_write, 40);
    assert_eq!(entry.ts_secs, 1_780_590_149);
    assert_eq!(entry.thread_id.as_deref(), Some("ses"));
    assert_eq!(parsed.origin.as_deref(), Some(cwd.as_path()));
    assert_eq!(parsed.cursor, SpendCursor::default());
}

#[test]
fn attaches_tool_content_to_its_message_entry() {
    let dir = TempDir::new().unwrap();
    let path = create_db(dir.path(), "opencode.db");
    insert_message(
        &path,
        r#"{
            "cost": 0.42,
            "model":{"id":"gpt-priced","providerID":"openai"},
            "time": { "created": 1780590149011 },
            "tokens": { "input": 100, "output": 20 },
            "content": [{"type":"tool","name":"bash"},{"type":"tool","name":"read"},{"type":"tool","name":"read"}]
        }"#,
    );

    let parsed = parse(&path, None, &prices());

    assert_eq!(
        parsed.entries[0].tool_calls,
        ToolCounts::from(BTreeMap::from([
            ("bash".to_owned(), 1),
            ("read".to_owned(), 2)
        ]))
    );
}

#[test]
fn prices_zero_cost_rows_and_records_unknown_models() {
    let dir = TempDir::new().unwrap();
    let path = create_db(dir.path(), "opencode.db");
    insert_message(
        &path,
        r#"{
            "cost": 0,
            "model":{"id":"gpt-priced","providerID":"openai"},
            "time": { "created": 2000 },
            "tokens": {
                "input": 10,
                "output": 20,
                "cache": { "read": 30, "write": 40 }
            }
        }"#,
    );
    insert_message(
        &path,
        r#"{
            "cost": 0,
            "model":{"id":"unknown-future","providerID":"openai"},
            "time": { "created": 3000 },
            "tokens": { "input": 10, "output": 20 }
        }"#,
    );

    let parsed = parse(&path, None, &prices());
    assert_eq!(parsed.entries.len(), 2);
    let expected = 10.0 * 0.000001 + 30.0 * 0.0000001 + 40.0 * 0.0000005 + 20.0 * 0.000002;
    assert!((parsed.entries[0].cost_usd - expected).abs() < 1e-12);
    assert_eq!(parsed.entries[1].cost_usd, 0.0);
    assert_eq!(parsed.entries[1].input, 10);
    assert_eq!(parsed.entries[1].output, 20);
    assert_eq!(parsed.unknown_models.get("unknown-future"), Some(&3));
    assert_eq!(parsed.unknown_models.get("openai/unknown-future"), Some(&3));
}

#[test]
fn provider_rows_win_before_bare_rows_and_aliases() {
    let prices = PriceBook::from_litellm_json(
        r#"{
        "opencode_go/hy4": {"input_cost_per_token": 2, "output_cost_per_token": 4},
        "hy4": {"input_cost_per_token": 9, "output_cost_per_token": 9},
        "opencode/glm-5-free": {"input_cost_per_token": 0, "output_cost_per_token": 0},
        "glm-5": {"input_cost_per_token": 1, "output_cost_per_token": 3.2},
        "gateway/vendor/model": {"input_cost_per_token": 3, "output_cost_per_token": 5},
        "vendor/model": {"input_cost_per_token": 8, "output_cost_per_token": 8},
        "openai/gpt-provider": {"input_cost_per_token": 4, "output_cost_per_token": 6},
        "openai/gpt-4o": {"input_cost_per_token": 2.5, "output_cost_per_token": 10},
        "openai/gpt-4o-mini": {"input_cost_per_token": 0.15, "output_cost_per_token": 0.6},
        "gpt-4o-mini-2024-07-18": {"input_cost_per_token": 0.15, "output_cost_per_token": 0.6},
        "gpt-4o": {"input_cost_per_token": 30, "output_cost_per_token": 40},
        "gpt-priced": {"input_cost_per_token": 5, "output_cost_per_token": 7},
        "anthropic/claude-sonnet-4-5": {"input_cost_per_token": 6, "output_cost_per_token": 8},
        "claude-sonnet-4.5": {"input_cost_per_token": 9, "output_cost_per_token": 9}
    }"#,
    );
    let cases = [
        ("hy4", "opencode-go", 6.0),
        ("glm-5-free", "opencode", 0.0),
        ("vendor/model", "gateway", 8.0),
        ("gpt-provider", "openai", 10.0),
        ("gpt-4o-mini-2024-07-18", "openai", 0.75),
        ("gpt-4o-via-gateway", "openai", 12.5),
        ("gpt-priced", "custom", 12.0),
        ("hy4", "unknown", 18.0),
        ("claude-sonnet-4.5", "anthropic", 14.0),
    ];
    let mut actual = Vec::new();
    for (model, provider, _) in cases {
        let mut unknowns = BTreeMap::new();
        let message = serde_json::from_value(serde_json::json!({
            "cost": 0,
            "model": {"id": model, "providerID": provider},
            "tokens": {"input": 1, "output": 1}
        }))
        .unwrap();
        let entry = parse_message_entry(message, None, false, &prices, &mut unknowns).unwrap();
        actual.push((entry.cost_usd, unknowns.is_empty()));
    }
    assert_eq!(actual, cases.map(|(_, _, cost)| (cost, true)));
}

#[test]
fn unknown_aliases_record_every_scoped_then_bare_candidate() {
    let mut unknowns = BTreeMap::new();
    let message = serde_json::from_str(
        r#"{
        "model": {"id": "claude-sonnet-4.5", "providerID": "custom-provider"},
        "time": {"created": 3000}, "tokens": {"input": 1}
    }"#,
    )
    .unwrap();
    parse_message_entry(
        message,
        None,
        false,
        &PriceBook::from_litellm_json("{}"),
        &mut unknowns,
    )
    .unwrap();
    let expected = [
        "custom_provider/claude-sonnet-4.5",
        "custom_provider/claude-sonnet-4-5",
        "claude-sonnet-4.5",
        "claude-sonnet-4-5",
    ];
    assert_eq!(
        (
            model_candidates("claude-sonnet-4.5", "custom-provider"),
            unknowns
        ),
        (
            expected.map(str::to_owned).to_vec(),
            expected
                .into_iter()
                .map(|key| (key.to_owned(), 3))
                .collect()
        )
    );
}

#[test]
fn total_fallback_and_candidate_resolution_work() {
    let dir = TempDir::new().unwrap();
    let path = create_db(dir.path(), "opencode.db");
    insert_message(
        &path,
        r#"{
            "model":{"id":"gpt-provider","providerID":"openai"},
            "tokens": { "total": 50 }
        }"#,
    );
    insert_message(
        &path,
        r#"{
            "model":{"id":"claude-sonnet-4.5","providerID":"anthropic"},
            "tokens": { "input": 10, "output": 10 }
        }"#,
    );

    let parsed = parse(&path, None, &prices());
    assert_eq!(parsed.entries.len(), 2);
    assert_eq!(parsed.entries[0].output, 50);
    assert!((parsed.entries[0].cost_usd - 50.0 * 0.000004).abs() < 1e-12);
    assert!((parsed.entries[1].cost_usd - (10.0 * 0.000005 + 10.0 * 0.000006)).abs() < 1e-12);
}

#[test]
fn parses_numeric_strings_and_ignores_malformed_nested_values() {
    let dir = TempDir::new().unwrap();
    let path = create_db(dir.path(), "opencode.db");
    insert_message(
        &path,
        r#"{
            "cost": "0.42",
            "model":{"id":"gpt-priced","providerID":"openai"},
            "time": { "created": "1780590149011" },
            "tokens": {
                "input": "100",
                "output": "20",
                "cache": { "read": "30", "write": "40" }
            }
        }"#,
    );
    insert_message(
        &path,
        r#"{
            "cost": 0.21,
            "model":{"id":"gpt-priced","providerID":"openai"},
            "time": "unknown",
            "tokens": {
                "input": true,
                "output": 5,
                "cache": 0
            }
        }"#,
    );

    let parsed = parse(&path, None, &prices());
    assert_eq!(parsed.entries.len(), 2);
    assert_eq!(
        (
            parsed.entries[0].input,
            parsed.entries[0].output,
            parsed.entries[0].cache_read,
            parsed.entries[0].cache_write,
        ),
        (100, 20, 30, 40)
    );
    assert_eq!(parsed.entries[0].ts_secs, 1_780_590_149);
    assert!((parsed.entries[0].cost_usd - 0.42).abs() < 1e-9);
    assert_eq!((parsed.entries[1].input, parsed.entries[1].output), (0, 5));
    assert_eq!(parsed.entries[1].ts_secs, 0);
    assert!((parsed.entries[1].cost_usd - 0.21).abs() < 1e-9);
}

#[test]
fn drop_rules_hold_and_each_refresh_replaces_the_cache_set() {
    let dir = TempDir::new().unwrap();
    let path = create_db(dir.path(), "opencode.db");
    for data in [
        r#"{"model":{"id":"gpt-priced","providerID":"openai"}}"#,
        r#"{"model":{"id":"gpt-priced","providerID":"openai"},"tokens":{"input":0}}"#,
        r#"{"model":{"providerID":"openai"},"tokens":{"input":1}}"#,
        r#"{"model":{"id":"gpt-priced"},"tokens":{"input":1}}"#,
    ] {
        insert_message(&path, data);
    }
    let first = parse(&path, None, &prices());
    assert!(first.entries.is_empty());
    // A mutable store is cold-folded whole: no resume cursor, and the fold is
    // authoritative for the file so it replaces the cache set.
    assert_eq!(first.update, SpendUpdate::Replace);
    assert_eq!(first.cursor, SpendCursor::default());

    insert_message(
        &path,
        r#"{"model":{"id":"gpt-priced","providerID":"openai"},"tokens":{"input":1,"output":1}}"#,
    );
    let second = parse(&path, Some(&first.cursor), &prices());
    assert_eq!(second.entries.len(), 1);
    assert_eq!(second.update, SpendUpdate::Replace);
}

#[test]
fn in_place_completion_is_counted_after_the_row_updates() {
    // Guards the resumed-spend data loss: an assistant row is inserted while
    // streaming (no tokens yet), then updated in place with tokens and cost
    // when the turn completes. A rowid resume cursor would have advanced past
    // the incomplete row and skipped its finalized cost forever; the full
    // reparse counts it.
    let dir = TempDir::new().unwrap();
    let path = create_db(dir.path(), "opencode.db");
    insert_message(
        &path,
        r#"{"model":{"id":"gpt-priced","providerID":"openai"}}"#,
    );
    let mid_stream = parse(&path, None, &prices());
    assert!(mid_stream.entries.is_empty());

    // The turn completes: the same row is rewritten with tokens+cost, and a
    // later row lands so the database grows — the condition that used to pick
    // the lossy resume path.
    update_last_message(
        &path,
        r#"{"model":{"id":"gpt-priced","providerID":"openai"},"cost":0.5,"tokens":{"input":10,"output":5}}"#,
    );
    insert_message(
        &path,
        r#"{"model":{"id":"gpt-priced","providerID":"openai"},"tokens":{"input":1,"output":1}}"#,
    );
    let completed = parse(&path, Some(&mid_stream.cursor), &prices());
    assert_eq!(completed.entries.len(), 2);
    assert!((completed.entries[0].cost_usd - 0.5).abs() < 1e-9);
}

#[test]
fn total_token_gap_folds_into_output_when_parts_are_nonzero() {
    // `total` exceeds the itemized parts while output is already nonzero: the
    // excess is counted and priced as output rather than silently dropped.
    let dir = TempDir::new().unwrap();
    let path = create_db(dir.path(), "opencode.db");
    insert_message(
        &path,
        r#"{"model":{"id":"gpt-priced","providerID":"openai"},"tokens":{"input":100,"output":20,"cache":{"read":30},"total":200}}"#,
    );
    let parsed = parse(&path, None, &prices());
    assert_eq!(parsed.entries.len(), 1);
    let entry = &parsed.entries[0];
    // missing = 200 - (100 + 20 + 30) = 50 → output = 20 + 50 = 70.
    assert_eq!((entry.input, entry.output, entry.cache_read), (100, 70, 30));
    let expected = 100.0 * 0.000001 + 70.0 * 0.000002 + 30.0 * 0.0000001;
    assert!((entry.cost_usd - expected).abs() < 1e-12);
}
