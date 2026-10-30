use super::*;

use std::io::Write as _;

use crate::agents::spending::{ResponseTiming, ToolCounts};
use tempfile::TempDir;

fn timing_line(ms: u64, kind: &str, payload: serde_json::Value) -> String {
    serde_json::json!({
        "timestamp": format!("2026-01-01T10:00:{:02}.{:03}Z", ms / 1_000, ms % 1_000),
        "type": kind,
        "payload": payload,
    })
    .to_string()
}

fn timed_response_lines() -> Vec<String> {
    use serde_json::json;
    vec![
        timing_line(0, "turn_context", json!({"model":"gpt-5"})),
        timing_line(0, "event_msg", json!({"type":"task_started"})),
        timing_line(
            1_000,
            "response_item",
            json!({"type":"message","role":"user"}),
        ),
        timing_line(
            2_000,
            "response_item",
            json!({"type":"function_call_output"}),
        ),
        timing_line(3_000, "response_item", json!({"type":"reasoning"})),
        timing_line(
            4_000,
            "response_item",
            json!({"type":"message","role":"developer"}),
        ),
        timing_line(
            5_500,
            "token_usage_record",
            json!({"usage":{"output_tokens":200}}),
        ),
        timing_line(
            20_000,
            "event_msg",
            json!({"type":"token_count","info":{
                "last_token_usage":{"input_tokens":100,"output_tokens":200},
                "total_token_usage":{"input_tokens":100,"output_tokens":200}
            }}),
        ),
    ]
}

#[test]
fn response_records_time_inputs_to_completion_without_changing_usage_or_cost() {
    let lines = timed_response_lines();
    let (_dir, path) = write_session(
        "timed.jsonl",
        &lines.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let timed = parse_codex_spend(&path, None, &gpt5_book());
    assert_eq!(timed.entries.len(), 1);
    assert_eq!(
        timed.entries[0].timing,
        Some(ResponseTiming {
            ms: 3_500,
            output: 200
        })
    );
    let stripped = lines
        .iter()
        .filter(|line| !line.contains("token_usage_record"))
        .map(String::as_str)
        .collect::<Vec<_>>();
    let (_dir, path) = write_session("untimed.jsonl", &stripped);
    let untimed = parse_codex_spend(&path, None, &gpt5_book());
    let mut entries = timed.entries.clone();
    for entry in &mut entries {
        entry.timing = None;
    }
    assert_eq!(entries, untimed.entries);
    assert_eq!(
        timed
            .cursor
            .state_as::<Option<serde_json::Value>>()
            .unwrap()["previous_totals"],
        untimed
            .cursor
            .state_as::<Option<serde_json::Value>>()
            .unwrap()["previous_totals"]
    );
    assert!(untimed.entries.iter().all(|entry| entry.timing.is_none()));
}

#[test]
fn response_input_and_model_item_kinds_each_define_the_boundary() {
    use serde_json::json;
    for (kind, input) in [
        ("event_msg", json!({"type":"task_started"})),
        ("response_item", json!({"type":"message","role":"user"})),
        (
            "response_item",
            json!({"type":"message","role":"developer"}),
        ),
        ("response_item", json!({"type":"function_call_output"})),
        ("response_item", json!({"type":"future_input"})),
    ] {
        for model in [
            json!({"type":"reasoning"}),
            json!({"type":"message","role":"assistant"}),
            json!({"type":"function_call"}),
            json!({"type":"custom_tool_call"}),
            json!({"type":"local_shell_call"}),
            json!({"type":"web_search_call"}),
        ] {
            let mut lines = timed_response_lines();
            lines.splice(
                1..6,
                [
                    timing_line(1_000, kind, input.clone()),
                    timing_line(2_000, "response_item", model),
                ],
            );
            let (_dir, path) = write_session(
                "kinds.jsonl",
                &lines.iter().map(String::as_str).collect::<Vec<_>>(),
            );
            assert_eq!(
                parse_codex_spend(&path, None, &gpt5_book()).entries[0].timing,
                Some(ResponseTiming {
                    ms: 4_500,
                    output: 200
                })
            );
        }
    }
}

#[test]
fn response_timing_cursor_round_trips_before_and_after_the_record() {
    let lines = timed_response_lines();
    for split in [4, 7] {
        let (_dir, path) = write_session(
            "resume.jsonl",
            &lines[..split]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        );
        let first = parse_codex_spend(&path, None, &gpt5_book());
        let cursor: SpendCursor =
            serde_json::from_value(serde_json::to_value(&first.cursor).unwrap()).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        for line in &lines[split..] {
            writeln!(file, "{line}").unwrap();
        }
        let resumed = parse_codex_spend(&path, Some(&cursor), &gpt5_book());
        assert_eq!(
            resumed.entries[0].timing,
            Some(ResponseTiming {
                ms: 3_500,
                output: 200
            })
        );
        let cold = parse_codex_spend(&path, None, &gpt5_book());
        assert_eq!(resumed.entries, cold.entries);
        assert_eq!(resumed.cursor, cold.cursor);
    }
}

#[test]
fn response_record_timing_is_consumed_only_by_matching_counts() {
    let lines = timed_response_lines();
    let (_dir, path) = write_session(
        "matched.jsonl",
        &lines.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    assert!(
        parse_codex_spend(&path, None, &gpt5_book()).entries[0]
            .timing
            .is_some()
    );
    for replacement in [
        timing_line(
            5_500,
            "token_usage_record",
            serde_json::json!({"usage":{"output_tokens":201}}),
        ),
        timing_line(
            5_500,
            "token_usage_record",
            serde_json::json!({"usage":{"output_tokens":99}}),
        ),
    ] {
        let mut changed = lines.clone();
        changed[6] = replacement;
        changed.push(lines[7].clone());
        let (_dir, path) = write_session(
            "mismatch.jsonl",
            &changed.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert!(
            parse_codex_spend(&path, None, &gpt5_book())
                .entries
                .iter()
                .all(|entry| entry.timing.is_none())
        );
    }
    let mut replaced = lines.clone();
    replaced.insert(
        7,
        timing_line(
            6_000,
            "token_usage_record",
            serde_json::json!({"usage":{"output_tokens":200}}),
        ),
    );
    let (_dir, path) = write_session(
        "replaced.jsonl",
        &replaced.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    assert_eq!(
        parse_codex_spend(&path, None, &gpt5_book()).entries[0].timing,
        None
    );
}

#[test]
fn copied_head_does_not_leak_response_timing_into_real_usage() {
    let mut lines = timed_response_lines();
    lines.insert(
        0,
        r#"{"type":"session_meta","payload":{"forked_from_id":"parent"}}"#.to_owned(),
    );
    for line in &mut lines[1..] {
        *line = line
            .replace("10:00:01", "10:00:00")
            .replace("10:00:02", "10:00:00")
            .replace("10:00:03", "10:00:00")
            .replace("10:00:04", "10:00:00")
            .replace("10:00:05", "10:00:00")
            .replace("10:00:20", "10:00:00");
    }
    lines.push(lines.last().unwrap().clone());
    lines.extend(timed_response_lines().into_iter().skip(1));
    let (_dir, path) = write_session(
        "replay.jsonl",
        &lines.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let parsed = parse_codex_spend(&path, None, &gpt5_book());
    assert_eq!(parsed.entries.len(), 1);
    assert_eq!(
        parsed.entries[0].timing,
        Some(ResponseTiming {
            ms: 3_500,
            output: 200
        })
    );
}

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
        br#"{"type":"response_item","payload":{"type":"function_call","name":"shell"}}"#,
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
fn headless_usage_rejects_non_object_field() {
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

    let events = parse_codex_session(&path, 0, &mut CodexSpendState::default())
        .unwrap()
        .0;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].input_tokens, 100);
    assert_eq!(events[0].output_tokens, 50);
    assert_eq!(events[0].model, "gpt-5");

    let (_dir, path) = write_session(
        "session-b.jsonl",
        &[
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:00.000Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cache_write_input_tokens":40,"output_tokens":50}}}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:01:00.000Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"cache_write_input_tokens":90,"output_tokens":120}}}}"#,
        ],
    );

    let events = parse_codex_session(&path, 0, &mut CodexSpendState::default())
        .unwrap()
        .0;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].input_tokens, 100);
    assert_eq!(events[0].cache_write_input_tokens, 40);
    assert_eq!(events[1].input_tokens, 200);
    assert_eq!(events[1].cache_write_input_tokens, 50);
    assert_eq!(events[1].output_tokens, 70);

    let (_dir, path) = write_session(
        "exec.jsonl",
        &[
            r#"{"model":"gpt-5","timestamp":"2026-01-01T10:00:00.000Z","usage":{"input_tokens":200,"output_tokens":80}}"#,
        ],
    );

    let events = parse_codex_session(&path, 0, &mut CodexSpendState::default())
        .unwrap()
        .0;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].input_tokens, 200);
    assert_eq!(events[0].output_tokens, 80);
    assert_eq!(events[0].model, "gpt-5");
}

#[test]
fn response_item_tools_attach_to_the_next_usage_entry() {
    let (_dir, path) = write_session(
        "tools.jsonl",
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#,
            r#"{"type":"response_item","timestamp":"2026-01-01T10:00:00.000Z","payload":{"type":"function_call","name":"shell"}}"#,
            r#"{"type":"response_item","timestamp":"2026-01-01T10:00:01.000Z","payload":{"type":"custom_tool_call","name":"apply_patch"}}"#,
            r#"{"type":"response_item","timestamp":"2026-01-01T10:00:02.000Z","payload":{"type":"web_search_call"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:03.000Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":50}}}}"#,
        ],
    );

    let entries = parse_codex_spend(&path, None, &gpt5_book()).entries;

    assert_eq!(
        entries[0].tool_calls,
        ToolCounts::from(BTreeMap::from([
            ("apply_patch".to_owned(), 1),
            ("shell".to_owned(), 1),
            ("web_search".to_owned(), 1),
        ]))
    );
}

#[test]
fn forked_rollout_skips_copied_history_and_keeps_its_cumulative_baseline() {
    let (_dir, path) = write_session(
        "fork.jsonl",
        &[
            r#"{"type":"session_meta","payload":{"id":"fork","forked_from_id":"parent"}}"#,
            r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#,
            r#"{"type":"response_item","timestamp":"2026-01-01T09:59:59.000Z","payload":{"type":"message","role":"assistant"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:00.100Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":50}}}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:00.200Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"output_tokens":120}}}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:01:00.000Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":450,"output_tokens":170}}}}"#,
        ],
    );

    let events = parse_codex_session(&path, 0, &mut CodexSpendState::default())
        .unwrap()
        .0;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].input_tokens, 150);
    assert_eq!(events[0].output_tokens, 50);
    assert_eq!(events[0].model, "gpt-5");
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

    let events = parse_codex_session(&path, 0, &mut CodexSpendState::default())
        .unwrap()
        .0;

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
fn codex_cache_write_is_priced_as_a_subset_of_input() {
    let line = r#"{"model":"gpt-5.6-sol","timestamp":"2026-01-01T10:00:00.000Z","usage":{"input_tokens":100,"cached_input_tokens":20,"cache_write_input_tokens":30,"output_tokens":10}}"#;
    let (_dir, path) = write_session("cache-write-rate.jsonl", &[line]);
    let prices = PriceBook::fixture();

    let entries = parse_codex_spend(&path, None, &prices).entries;

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].input, 50);
    assert_eq!(entries[0].cache_write, 30);
    assert_eq!(entries[0].cache_read, 20);
    assert_eq!(entries[0].output, 10);
    let price = prices.price("gpt-5.6-sol").unwrap();
    let expected = 50.0 * price.input
        + 30.0 * price.cache_create
        + 20.0 * price.cache_read
        + 10.0 * price.output;
    assert!(
        (entries[0].cost_usd - expected).abs() < 1e-15,
        "cache-write cost was {}",
        entries[0].cost_usd
    );
}

#[test]
fn implicit_cache_read_billing_preserves_cache_write() {
    let price = Pricing::from_base_rates(1.0, 2.0, Some(1.25), None);
    let split = TokenSplit::new(50, 10).cached(30, 20);

    assert_eq!(
        codex_billed_split(price, split),
        TokenSplit::new(70, 10).cached(30, 0)
    );
}

#[test]
fn dedup_key_separates_events_differing_only_in_reasoning_or_total() {
    let base = parse::CodexTokenEvent {
        timestamp: "2026-01-01T10:00:00.000Z".to_string(),
        model: "gpt-5".to_string(),
        input_tokens: 100,
        cached_input_tokens: 10,
        cache_write_input_tokens: 0,
        output_tokens: 50,
        reasoning_output_tokens: 5,
        total_tokens: 155,
        tool_calls: BTreeMap::new(),
        timing: None,
        fast: false,
        basis: CodexPricingBasis::List,
    };
    let key = |event: &parse::CodexTokenEvent| {
        codex_event_dedup_key(&event.timestamp, &event.model, event)
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

fn write_live_rollout(path: &Path, totals: &[(u64, u64, u64)]) {
    let mut file = std::fs::File::create(path).unwrap();
    writeln!(
        file,
        r#"{{"type":"turn_context","payload":{{"model":"gpt-5.6-sol"}}}}"#
    )
    .unwrap();
    for (index, (input, cached, output)) in totals.iter().enumerate() {
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "type": "event_msg",
                "timestamp": format!("2026-01-01T10:00:0{index}.000Z"),
                "payload": {
                    "type": "token_count",
                    "info": {"total_token_usage": {
                        "input_tokens": input,
                        "cached_input_tokens": cached,
                        "output_tokens": output,
                    }},
                },
            })
        )
        .unwrap();
    }
}

fn append_live_total(path: &Path, index: usize, input: u64, cached: u64, output: u64) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "type": "event_msg",
            "timestamp": format!("2026-01-01T10:00:0{index}.000Z"),
            "payload": {
                "type": "token_count",
                "info": {"total_token_usage": {
                    "input_tokens": input,
                    "cached_input_tokens": cached,
                    "output_tokens": output,
                }},
            },
        })
    )
    .unwrap();
}

#[test]
fn live_fold_resume_matches_a_cold_full_fold() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rollout.jsonl");
    let prices = PriceBook::fixture();
    write_live_rollout(&path, &[(200_000, 150_000, 1_000)]);
    let first = resume_live_fold(
        &path,
        None,
        std::fs::metadata(&path).unwrap().len(),
        &prices,
    );

    append_live_total(&path, 1, 400_000, 300_000, 2_000);
    let resumed = resume_live_fold(
        &path,
        Some(&first),
        std::fs::metadata(&path).unwrap().len(),
        &prices,
    );
    let cold = resume_live_fold(
        &path,
        None,
        std::fs::metadata(&path).unwrap().len(),
        &prices,
    );

    assert_eq!(resumed, cold);
    assert!(resumed.total_usd > first.total_usd);
    assert_eq!(
        (
            first.input,
            first.output,
            first.cache_write,
            first.cache_read
        ),
        (50_000, 1_000, 0, 150_000)
    );
    assert_eq!(
        (
            resumed.input,
            resumed.output,
            resumed.cache_write,
            resumed.cache_read
        ),
        (100_000, 2_000, 0, 300_000)
    );
}

#[test]
fn live_fold_resets_when_the_rollout_is_truncated() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rollout.jsonl");
    let prices = PriceBook::fixture();
    write_live_rollout(
        &path,
        &[(200_000, 150_000, 1_000), (400_000, 300_000, 2_000)],
    );
    let full = resume_live_fold(
        &path,
        None,
        std::fs::metadata(&path).unwrap().len(),
        &prices,
    );

    write_live_rollout(&path, &[(100_000, 75_000, 500)]);
    let truncated_len = std::fs::metadata(&path).unwrap().len();
    assert!(truncated_len < full.cursor.offset);
    let reset = resume_live_fold(&path, Some(&full), truncated_len, &prices);
    let cold = resume_live_fold(&path, None, truncated_len, &prices);

    assert_eq!(reset, cold);
    assert!(reset.total_usd < full.total_usd);
}

#[test]
fn live_fold_heals_a_pre_token_counter_sidecar() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("rollout.jsonl");
    let prices = PriceBook::fixture();
    write_live_rollout(&path, &[(200_000, 150_000, 1_000)]);
    let complete = resume_live_fold(
        &path,
        None,
        std::fs::metadata(&path).unwrap().len(),
        &prices,
    );
    let legacy = LocalSpendFold {
        cursor: complete.cursor.clone(),
        total_usd: complete.total_usd,
        ..LocalSpendFold::default()
    };

    assert_eq!(
        resume_live_fold(
            &path,
            Some(&legacy),
            std::fs::metadata(&path).unwrap().len(),
            &prices,
        ),
        complete
    );
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

#[test]
fn hintless_session_prices_as_gpt5_across_a_resume() {
    let (_dir, path) = write_session("hintless.jsonl", &[TOKEN_COUNT_LINE]);
    let first = parse_codex_spend(&path, None, &gpt5_book());
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.entries[0].model.as_deref(), Some("gpt-5"));

    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(
        f,
        r#"{{"type":"event_msg","timestamp":"2026-01-01T10:05:00.000Z","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":10,"output_tokens":5}}}}}}}}"#
    )
    .unwrap();
    drop(f);
    let second = parse_codex_spend(&path, Some(&first.cursor), &gpt5_book());
    assert_eq!(second.entries.len(), 1);
    assert_eq!(second.entries[0].model.as_deref(), Some("gpt-5"));
}

#[test]
fn cache_tokens_clamp_to_input_and_zero_usage_emits_nothing_in_both_formats() {
    let (_dir, path) = write_session(
        "session.jsonl",
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:00.000Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":0,"output_tokens":0}}}}"#,
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:01.000Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":150,"cache_write_input_tokens":120,"output_tokens":5}}}}"#,
        ],
    );
    let (_dir, headless) = write_session(
        "exec.jsonl",
        &[
            r#"{"model":"gpt-5","timestamp":"2026-01-01T10:00:00.000Z","usage":{"input_tokens":0,"output_tokens":0}}"#,
            r#"{"model":"gpt-5","timestamp":"2026-01-01T10:00:01.000Z","usage":{"input_tokens":100,"cached_tokens":150,"cache_write_tokens":120,"output_tokens":5}}"#,
        ],
    );
    for path in [path, headless] {
        let events = parse_codex_session(&path, 0, &mut CodexSpendState::default())
            .unwrap()
            .0;
        assert_eq!(events.len(), 1, "{}", path.display());
        assert_eq!(events[0].input_tokens, 100);
        assert_eq!(events[0].cached_input_tokens, 100);
        assert_eq!(events[0].cache_write_input_tokens, 100);
    }
}

fn fast_book() -> PriceBook {
    PriceBook::from_litellm_json(
        r#"{"gpt-5": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
                      "cache_read_input_token_cost": 1e-7,
                      "provider_specific_entry": {"fast": 2.5}}}"#,
    )
}

fn tier_line(second: u64, tier: Option<&str>) -> String {
    let mut settings = serde_json::json!({"model": "gpt-5", "reasoning_effort": "high"});
    if let Some(tier) = tier {
        settings["service_tier"] = tier.into();
    }
    serde_json::json!({
        "timestamp": format!("2026-01-01T10:00:{second:02}.000Z"),
        "type": "event_msg",
        "payload": {"type": "thread_settings_applied", "thread_settings": settings},
    })
    .to_string()
}

fn count_line(second: u64) -> String {
    serde_json::json!({
        "timestamp": format!("2026-01-01T10:00:{second:02}.000Z"),
        "type": "event_msg",
        "payload": {"type": "token_count", "info": {
            "last_token_usage": {"input_tokens": 1000, "output_tokens": 100}
        }},
    })
    .to_string()
}

fn plan_count_line(second: u64, rate_limits: serde_json::Value, input: u64) -> String {
    serde_json::json!({
        "timestamp": format!("2026-01-01T10:00:{second:02}.000Z"),
        "type": "event_msg",
        "payload": {"type": "token_count", "rate_limits": rate_limits, "info": {
            "last_token_usage": {
                "input_tokens": input, "cached_input_tokens": 200,
                "cache_write_input_tokens": 100, "output_tokens": 100
            }
        }},
    })
    .to_string()
}

#[test]
fn plan_burn_uses_each_requests_snapshot_without_losing_malformed_usage() {
    use serde_json::json;
    let cases = [
        (
            "pro included",
            json!({"plan_type":"pro", "primary":{"used_percent":99.9},
                "credits":{"has_credits":true}}),
            1_000,
            true,
            0.0180625,
        ),
        (
            "primary credits",
            json!({"plan_type":"pro", "primary":{"used_percent":100.0},
                "credits":{"has_credits":true}}),
            1_000,
            true,
            0.01445,
        ),
        (
            "secondary credits",
            json!({"plan_type":"pro", "secondary":{"used_percent":100.1},
                "credits":{"has_credits":true}}),
            1_000,
            true,
            0.01445,
        ),
        (
            "team included",
            json!({"plan_type":"team", "primary":{"used_percent":100.0}, "credits":null}),
            1_000,
            true,
            0.0180625,
        ),
        (
            "no credits",
            json!({"plan_type":"pro", "primary":{"used_percent":100.0},
                "credits":{"has_credits":false}}),
            1_000,
            true,
            0.0180625,
        ),
        (
            "unknown credits",
            json!({"plan_type":"pro", "primary":{"used_percent":100.0}, "credits":{}}),
            1_000,
            true,
            0.0180625,
        ),
        (
            "reached type alone",
            json!({"plan_type":"pro", "rate_limit_reached_type":"usage_limit",
                "credits":{"has_credits":true}}),
            1_000,
            true,
            0.0180625,
        ),
        (
            "business plan",
            json!({"plan_type":"self_serve_business_usage_based"}),
            1_000,
            true,
            0.0180625,
        ),
        (
            "standard included",
            json!({"plan_type":"pro"}),
            1_000,
            false,
            0.007225,
        ),
        (
            "standard credits",
            json!({"plan_type":"pro", "primary":{"used_percent":100},
                "credits":{"has_credits":true}}),
            1_000,
            false,
            0.007225,
        ),
        (
            "long included",
            json!({"plan_type":"pro"}),
            300_000,
            true,
            3.7555625,
        ),
        (
            "long credits",
            json!({"plan_type":"pro", "primary":{"used_percent":100},
                "credits":{"has_credits":true}}),
            300_000,
            true,
            3.00445,
        ),
        ("null after plan", json!(null), 1_000, true, 0.007225),
        ("long list", json!(null), 300_000, true, 3.00295),
        (
            "http default",
            json!({"limit_id":"codex", "plan_type":null, "primary":null,
                "secondary":null, "credits":null}),
            1_000,
            true,
            0.007225,
        ),
        (
            "http only plan miss",
            json!({"primary":{"used_percent":100.0}, "credits":{"has_credits":true}}),
            1_000,
            true,
            0.007225,
        ),
        ("empty plan", json!({"plan_type":""}), 1_000, true, 0.007225),
        (
            "blank plan",
            json!({"plan_type":"  "}),
            1_000,
            true,
            0.007225,
        ),
        (
            "wrong percent type",
            json!({"plan_type":"pro", "primary":{"used_percent":"100"}}),
            1_000,
            true,
            0.007225,
        ),
    ];
    let mut lines = vec![r#"{"type":"turn_context","payload":{"model":"gpt-5.6-sol"}}"#.to_owned()];
    for (index, (_, snapshot, input, fast, _)) in cases.iter().enumerate() {
        lines.push(tier_line(
            index as u64,
            Some(if *fast { "priority" } else { "default" }),
        ));
        lines.push(plan_count_line(index as u64, snapshot.clone(), *input));
    }
    let (_dir, path) = write_session(
        "plan-bases.jsonl",
        &lines.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let parsed = parse_codex_spend(&path, None, &PriceBook::fixture());
    let actual = parsed
        .entries
        .iter()
        .map(|entry| {
            (
                entry.input,
                entry.cache_write,
                entry.cache_read,
                entry.output,
                (entry.cost_usd * 1e9).round() as u64,
            )
        })
        .collect::<Vec<_>>();
    let expected = cases
        .iter()
        .map(|(_, _, input, _, cost)| (input - 300, 100, 200, 100, (cost * 1e9_f64).round() as u64))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "cases: {:?}", cases.map(|case| case.0));
}

#[test]
fn plan_burn_mixed_basis_resume_matches_cold_entries_and_live_fold() {
    use serde_json::json;
    let included = json!({"plan_type":"pro"});
    let credits =
        json!({"plan_type":"pro", "primary":{"used_percent":100}, "credits":{"has_credits":true}});
    let lines = [
        r#"{"type":"turn_context","payload":{"model":"gpt-5.6-sol"}}"#.to_owned(),
        tier_line(0, Some("priority")),
        plan_count_line(1, included.clone(), 1_000),
        plan_count_line(2, credits, 1_000),
        plan_count_line(3, included.clone(), 300_000),
        plan_count_line(4, json!(null), 1_000),
        plan_count_line(5, included, 1_000),
        json!({"timestamp":"2026-01-01T10:00:06Z", "usage":{
            "input_tokens":1_000, "cached_input_tokens":200,
            "cache_write_input_tokens":100, "output_tokens":100
        }, "rate_limits":{"plan_type":"pro"}})
        .to_string(),
        count_line(7),
    ];
    let (_dir, path) = write_session(
        "mixed-plan.jsonl",
        &lines[..3].iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let prices = PriceBook::fixture();
    let first = parse_codex_spend(&path, None, &prices);
    let cursor: SpendCursor =
        serde_json::from_value(serde_json::to_value(&first.cursor).unwrap()).unwrap();
    let first_fold = resume_live_fold(
        &path,
        None,
        std::fs::metadata(&path).unwrap().len(),
        &prices,
    );
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    for line in &lines[3..] {
        writeln!(file, "{line}").unwrap();
    }
    let resumed = parse_codex_spend(&path, Some(&cursor), &prices);
    let cold = parse_codex_spend(&path, None, &prices);
    let mut entries = first.entries;
    entries.extend(resumed.entries);
    assert_eq!(entries, cold.entries);
    assert_eq!(resumed.cursor, cold.cursor);
    let actual = cold
        .entries
        .iter()
        .map(|entry| (entry.cost_usd * 1e9).round() as u64)
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        [
            18_062_500,
            14_450_000,
            3_755_562_500,
            7_225_000,
            18_062_500,
            7_225_000,
            8_000_000
        ]
    );
    let file_len = std::fs::metadata(&path).unwrap().len();
    let live = resume_live_fold(&path, Some(&first_fold), file_len, &prices);
    assert_eq!(live, resume_live_fold(&path, None, file_len, &prices));
    assert_eq!(
        live.total_usd,
        cold.entries.iter().map(|entry| entry.cost_usd).sum::<f64>()
    );
}

fn costs(path: &Path, resume: Option<&SpendCursor>) -> (Vec<f64>, SpendCursor) {
    let parsed = parse_codex_spend(path, resume, &fast_book());
    (
        parsed.entries.iter().map(|entry| entry.cost_usd).collect(),
        parsed.cursor,
    )
}

#[test]
fn fast_cursor_distinguishes_unobserved_standard_and_legacy_priority() {
    let empty = serde_json::to_value(CodexSpendState::default()).unwrap();
    assert!(empty.get("fast").is_none());
    let legacy: CodexSpendState =
        serde_json::from_value(serde_json::json!({"fast": true})).unwrap();
    assert_eq!(serde_json::to_value(legacy).unwrap()["fast"], true);
    let (_dir, path) = write_session("standard-tier.jsonl", &[&tier_line(1, Some("default"))]);
    let mut state = CodexSpendState::default();
    parse_codex_session(&path, 0, &mut state).unwrap();
    assert_eq!(serde_json::to_value(state).unwrap()["fast"], false);
}

#[test]
fn priority_thread_settings_price_following_turns_fast_until_the_tier_reverts() {
    let standard = 1000.0 * 1e-6 + 100.0 * 2e-6;
    let lines = [
        r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#.to_owned(),
        count_line(1),
        tier_line(2, Some("priority")),
        count_line(3),
        tier_line(4, Some("default")),
        count_line(5),
        tier_line(6, Some("fast")),
        count_line(7),
        tier_line(8, None),
        count_line(9),
    ];
    let (_dir, path) = write_session(
        "tier.jsonl",
        &lines.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let (costs, _) = costs(&path, None);
    let expected = [standard, standard * 2.5, standard, standard * 2.5, standard];
    assert_eq!(costs.len(), expected.len(), "{costs:?}");
    for (cost, expected) in costs.iter().zip(expected) {
        assert!((cost - expected).abs() < 1e-12, "{costs:?}");
    }
}

#[test]
fn priced_entries_keep_the_observed_fast_tier() {
    let (_dir, path) = write_session(
        "fast-entries.jsonl",
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#,
            &count_line(1),
            &tier_line(2, Some("priority")),
            &count_line(3),
            &tier_line(4, Some("default")),
            &count_line(5),
        ],
    );
    let parsed = parse_codex_spend(&path, None, &fast_book());
    assert_eq!(
        parsed
            .entries
            .iter()
            .map(|entry| entry.fast)
            .collect::<Vec<_>>(),
        [false, true, false]
    );
}

#[test]
fn a_resumed_spend_cursor_keeps_the_fast_tier() {
    let (_dir, path) = write_session(
        "tier-resume.jsonl",
        &[
            r#"{"type":"turn_context","payload":{"model":"gpt-5"}}"#,
            &tier_line(1, Some("priority")),
            &count_line(2),
        ],
    );
    let (_, cursor) = costs(&path, None);
    let cursor: SpendCursor =
        serde_json::from_value(serde_json::to_value(&cursor).unwrap()).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(file, "{}", count_line(3)).unwrap();
    let (resumed, _) = costs(&path, Some(&cursor));
    let (cold, _) = costs(&path, None);
    assert_eq!(resumed.len(), 1);
    assert!((resumed[0] - (1000.0 * 1e-6 + 100.0 * 2e-6) * 2.5).abs() < 1e-12);
    assert_eq!(resumed[0], cold[1]);
}
