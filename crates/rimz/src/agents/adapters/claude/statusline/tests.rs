//! Unit tests for the Claude statusline parser and transcript scans.

use super::super::account;
use super::*;
use serde_json::json;

fn parse(value: serde_json::Value) -> AgentContext {
    let payload: StatuslinePayload = serde_json::from_value(value).unwrap();
    payload.into_context("claude", Timestamp::from_second(1_700_000_000).unwrap())
}

/// The window stamped with `mins` minutes — Claude's two named wire windows
/// map to fixed durations.
fn window_by_mins(rate: &AgentRateLimits, mins: u32) -> &RateLimitWindow {
    rate.windows
        .iter()
        .find(|window| window.duration_mins == Some(mins))
        .expect("window present for duration")
}

#[test]
fn full_payload_projects_every_field() {
    let ctx = parse(json!({
        "session_id": "abc123",
        "session_name": "store-refactor",
        "model": { "id": "claude-opus-4-8", "display_name": "Opus" },
        "cost": {
            "total_cost_usd": 0.01234,
            "total_duration_ms": 45000,
            "total_api_duration_ms": 2300,
            "total_lines_added": 156,
            "total_lines_removed": 23
        },
        "context_window": {
            "total_input_tokens": 15500,
            "total_output_tokens": 1200,
            "context_window_size": 200000,
            "used_percentage": 8,
            "remaining_percentage": 92,
            "current_usage": {
                "input_tokens": 8500,
                "output_tokens": 1200,
                "cache_creation_input_tokens": 5000,
                "cache_read_input_tokens": 2000
            }
        },
        "exceeds_200k_tokens": false,
        "effort": { "level": "high" },
        "thinking": { "enabled": true },
        "rate_limits": {
            "five_hour": { "used_percentage": 23.5, "resets_at": 1738425600i64 },
            "seven_day": { "used_percentage": 41.2, "resets_at": 1738857600i64 }
        },
        "vim": { "mode": "NORMAL" },
        "version": "2.1.90",
        "output_style": { "name": "default" },
        "pr": { "number": 1234, "url": "https://example/pr/1234", "review_state": "pending" }
    }));

    assert_eq!(ctx.source, "claude");
    assert_eq!(ctx.session_name.as_deref(), Some("store-refactor"));
    assert_eq!(ctx.model_id.as_deref(), Some("claude-opus-4-8"));
    assert_eq!(ctx.model_display_name.as_deref(), Some("Opus"));
    assert_eq!(ctx.effort.as_deref(), Some("high"));
    assert_eq!(ctx.thinking_enabled, Some(true));
    assert_eq!(ctx.output_style.as_deref(), Some("default"));
    assert_eq!(ctx.vim_mode.as_deref(), Some("NORMAL"));
    assert_eq!(ctx.agent_version.as_deref(), Some("2.1.90"));
    assert_eq!(ctx.exceeds_200k_tokens, Some(false));

    let cost = ctx.cost.unwrap();
    assert_eq!(cost.total_cost_usd, Some(0.01234));
    assert_eq!(cost.total_lines_added, Some(156));

    // `total_input_tokens` / `total_output_tokens` ride the wire above but
    // are not captured — `current_usage` carries the same window split.
    let tokens = ctx.tokens.unwrap();
    assert_eq!(tokens.context_window_size, Some(200000));
    assert_eq!(tokens.used_percentage, Some(8));
    assert_eq!(tokens.remaining_percentage, Some(92));
    let usage = tokens.current_usage.unwrap();
    assert_eq!(usage.input_tokens, Some(8500));
    assert_eq!(usage.output_tokens, Some(1200));
    assert_eq!(usage.cache_creation_input_tokens, Some(5000));
    assert_eq!(usage.cache_read_input_tokens, Some(2000));

    let rate = ctx.rate_limits.unwrap();
    let five = window_by_mins(&rate, account::FIVE_HOUR_MINS);
    // 23.5 rounds to 24.
    assert_eq!(five.used_percentage, Some(24));
    assert_eq!(five.resets_at, Timestamp::from_second(1738425600).ok());
    assert!(
        five.source.is_best_effort(),
        "a statusline reading is best-effort — a drop is confirmed before it lowers the bar"
    );
    assert_eq!(
        five.observed_at,
        Some(ctx.observed_at),
        "into_context stamps the capture instant onto each window"
    );
    assert_eq!(
        window_by_mins(&rate, account::SEVEN_DAY_MINS).used_percentage,
        Some(41)
    );

    let pr = ctx.pr.unwrap();
    assert_eq!(pr.number, Some(1234));
    assert_eq!(pr.review_state.as_deref(), Some("pending"));
}

#[test]
fn payload_tolerates_sparse_null_unknown_and_clamped_fields() {
    let ctx = parse(json!({ "session_id": "s", "model": {} }));
    assert_eq!(ctx.source, "claude");
    assert!(ctx.model_id.is_none());
    assert!(ctx.cost.is_none());
    assert!(ctx.tokens.is_none());
    assert!(ctx.rate_limits.is_none());
    assert!(ctx.pr.is_none());

    // `current_usage` is null before the first API call in older Claude;
    // the rest of the context window still projects.
    let ctx = parse(json!({
        "context_window": { "used_percentage": 12, "current_usage": null }
    }));
    let tokens = ctx.tokens.unwrap();
    assert_eq!(tokens.used_percentage, Some(12));
    assert!(tokens.current_usage.is_none());

    // An all-null usage object carries nothing, so it collapses rather than
    // serializing as `{}`.
    let ctx = parse(json!({
        "context_window": { "used_percentage": 5, "current_usage": {} }
    }));
    assert!(ctx.tokens.unwrap().current_usage.is_none());

    // Newer Claude reports the same state as explicit zeros.
    let ctx = parse(json!({
        "context_window": { "used_percentage": 5, "current_usage": { "input_tokens": 0 } }
    }));
    assert!(ctx.tokens.unwrap().current_usage.is_none());

    // A newer Claude that adds keys must still parse.
    let ctx = parse(json!({ "model": { "id": "m" }, "brand_new_field": { "x": 1 } }));
    assert_eq!(ctx.model_id.as_deref(), Some("m"));

    let ctx = parse(json!({ "context_window": { "used_percentage": 250 } }));
    assert_eq!(ctx.tokens.unwrap().used_percentage, Some(100));

    // i64::MIN is out of Timestamp's range; the reset drops, the pct stays.
    let ctx = parse(json!({
        "rate_limits": { "five_hour": { "used_percentage": 10, "resets_at": i64::MIN } }
    }));
    let rate = ctx.rate_limits.unwrap();
    let five = window_by_mins(&rate, account::FIVE_HOUR_MINS);
    assert_eq!(five.used_percentage, Some(10));
    assert!(five.resets_at.is_none());

    let ctx = parse(json!({
        "rate_limits": {
            "five_hour": { "used_percentage": 99.5, "resets_at": 1738425600i64 },
            "seven_day": { "used_percentage": 100.0, "resets_at": 1738857600i64 }
        }
    }));
    let rate = ctx.rate_limits.unwrap();
    assert_eq!(
        window_by_mins(&rate, account::FIVE_HOUR_MINS).used_percentage,
        Some(99),
        "99.5% used still leaves visible remaining budget"
    );
    assert_eq!(
        window_by_mins(&rate, account::SEVEN_DAY_MINS).used_percentage,
        Some(100),
        "exactly 100% used remains exhausted"
    );
}

/// The verbatim shape an API-error abort writes (observed live 2026-06-04):
/// the flagged assistant entry, then a `system`/`turn_duration` record 4ms
/// later, then nothing — and no `Stop` hook.
const API_ERROR_ENTRY: &str = r#"{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-06-04T02:56:32.919Z","message":{"role":"assistant","content":[{"type":"text","text":"API Error: Overloaded"}]}}"#;
const TURN_DURATION_ENTRY: &str =
    r#"{"type":"system","subtype":"turn_duration","timestamp":"2026-06-04T02:56:32.923Z"}"#;
const NORMAL_ASSISTANT_ENTRY: &str = r#"{"type":"assistant","timestamp":"2026-06-04T03:00:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#;
const INTERRUPTED_ENTRY: &str = r#"{"type":"user","timestamp":"2026-06-04T03:01:00.000Z","message":{"role":"user","content":"[Request interrupted by user]"}}"#;

#[test]
fn verified_incident_shape_marks_turn_error() {
    // The newer `turn_duration` record is a non-conversation entry: passed
    // over, never decisive, so the flagged assistant entry decides.
    let tail = format!("{API_ERROR_ENTRY}\n{TURN_DURATION_ENTRY}\n");
    let error = detect_turn_error(&tail).expect("the dead turn is detected");
    assert_eq!(
        error.at,
        "2026-06-04T02:56:32.919Z".parse::<Timestamp>().unwrap(),
        "the marker carries the error entry's own wall-clock instant"
    );
    assert_eq!(error.class, TurnErrorClass::PausedOverloaded);
    assert_eq!(error.label.as_deref(), Some("API Error: Overloaded"));
}

#[test]
fn interruption_sentinels_mark_the_turn_at_rest() {
    assert_eq!(
        detect_turn_interrupted(INTERRUPTED_ENTRY),
        Some("2026-06-04T03:01:00Z".parse::<Timestamp>().unwrap())
    );
    // Verbatim content-block shape observed in Claude's transcript JSONL;
    // this is a text block, distinct from the Messages API wire shape.
    let tool_use = r#"{"type":"user","timestamp":"2026-06-04T03:02:00.000Z","message":{"content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#;
    assert_eq!(
        detect_turn_interrupted(tool_use),
        Some("2026-06-04T03:02:00Z".parse::<Timestamp>().unwrap())
    );
}

#[test]
fn resting_turn_scan_lets_the_newest_conversation_entry_decide() {
    assert!(detect_turn_interrupted(API_ERROR_ENTRY).is_none());
    assert!(detect_turn_error(API_ERROR_ENTRY).is_some());

    assert!(detect_turn_interrupted(NORMAL_ASSISTANT_ENTRY).is_none());
    assert!(detect_turn_error(INTERRUPTED_ENTRY).is_none());

    let ordinary_user = r#"{"type":"user","timestamp":"2026-06-04T03:03:00.000Z","message":{"content":"keep going"}}"#;
    let tail = format!("{INTERRUPTED_ENTRY}\n{ordinary_user}\n");
    assert!(detect_turn_interrupted(&tail).is_none());
}

#[test]
fn resting_turn_scan_skips_sidechain_and_nonconversation_records() {
    let sidechain = r#"{"type":"user","isSidechain":true,"timestamp":"2026-06-04T03:02:00.000Z","message":{"content":"[Request interrupted by user]"}}"#;
    let tail = format!(
        "{INTERRUPTED_ENTRY}\n{{\"type\":\"system\",\"timestamp\":\"2026-06-04T03:01:01.000Z\"}}\n{sidechain}\n"
    );
    assert!(detect_turn_interrupted(&tail).is_some());

    let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{sidechain}\nnot-json\n");
    assert!(detect_turn_interrupted(&tail).is_none());
}

#[test]
fn subagent_interruption_scan_accepts_only_the_named_sidechain() {
    let child = r#"{"type":"user","isSidechain":true,"agentId":"child","timestamp":"2026-06-04T03:02:00.000Z","message":{"content":"[Request interrupted by user for tool use]"}}"#;
    assert!(detect_subagent_interrupted(child, "child"));
    assert!(detect_turn_interrupted(child).is_none());

    let nested = r#"{"type":"assistant","isSidechain":true,"agentId":"nested","timestamp":"2026-06-04T03:03:00.000Z","message":{"content":[{"type":"text","text":"still running"}]}}"#;
    let tail = format!("{child}\n{nested}\n");
    assert!(detect_subagent_interrupted(&tail, "child"));
    assert!(!detect_subagent_interrupted(&tail, "nested"));
}

#[test]
fn tool_result_scan_accepts_only_the_root_result_for_the_named_call() {
    let result = |id: &str, sidechain: &str| {
        format!(
            r#"{{"type":"user"{sidechain},"timestamp":"2026-06-04T03:02:00.000Z","message":{{"content":[{{"type":"tool_result","tool_use_id":"{id}","is_error":true}}]}}}}"#
        )
    };
    let answered = result("toolu_ask", "");
    assert!(tool_result_recorded(&answered, "toolu_ask"));

    // A sibling's result, and a child's replay of this call, both leave the
    // root call open.
    assert!(!tool_result_recorded(
        &result("toolu_read", ""),
        "toolu_ask"
    ));
    assert!(!tool_result_recorded(
        &result("toolu_ask", r#","isSidechain":true"#),
        "toolu_ask"
    ));

    // The tail's torn leading record proves nothing, and the assistant entry
    // that requested the call is not its result.
    let torn = format!("ser\":\"toolu_ask\"}}]}}}}\n{NORMAL_ASSISTANT_ENTRY}\n");
    assert!(!tool_result_recorded(&torn, "toolu_ask"));
    assert!(tool_result_recorded(
        &format!("{torn}{answered}\n"),
        "toolu_ask"
    ));
}

#[test]
fn turn_error_label_classifies_paused_and_failed_errors() {
    let entry = |text: &str| {
        format!(
            r#"{{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-06-04T02:56:32.919Z","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    };
    let temporary_500 = concat!(
        "API Error: 500 Internal server error. ",
        "This is a server-side issue, usually temporary — try again in a moment."
    );

    assert_eq!(
        detect_turn_error(&entry("You've hit your usage limit"))
            .unwrap()
            .class,
        TurnErrorClass::PausedRateLimit
    );
    assert_eq!(
        detect_turn_error(&entry("You've hit your monthly spend limit."))
            .unwrap()
            .class,
        TurnErrorClass::PausedSpendLimit
    );
    assert_eq!(
        detect_turn_error(&entry(
            "You've hit your session limit · resets 10:50am (UTC)"
        ))
        .unwrap()
        .class,
        TurnErrorClass::PausedRateLimit
    );
    assert_eq!(
        detect_turn_error(&entry("API Error: rate limit exceeded"))
            .unwrap()
            .class,
        TurnErrorClass::PausedRateLimit
    );
    assert_eq!(
        detect_turn_error(&entry("API Error: Server Error"))
            .unwrap()
            .class,
        TurnErrorClass::PausedOverloaded
    );
    assert_eq!(
        detect_turn_error(&entry(
            "API Error: Response stalled mid-stream. The response above may be incomplete."
        ))
        .unwrap()
        .class,
        TurnErrorClass::PausedOverloaded
    );
    assert_eq!(
        detect_turn_error(&entry(
            "API Error: Connection closed mid-response. The response above may be incomplete."
        ))
        .unwrap()
        .class,
        TurnErrorClass::PausedOverloaded
    );
    assert_eq!(
        detect_turn_error(&entry(temporary_500)).unwrap().class,
        TurnErrorClass::PausedOverloaded
    );
    assert_eq!(
        detect_turn_error(&entry("API Error: Bad Request"))
            .unwrap()
            .class,
        TurnErrorClass::Failed
    );
}

#[test]
fn api_error_structured_fields_classify_ahead_of_the_label() {
    let entry = |fields: &str, text: &str| {
        format!(
            r#"{{"type":"assistant","isApiErrorMessage":true,{fields}"timestamp":"2026-09-13T08:00:00.000Z","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    };
    let fable = "You've reached your Fable limit. Run /usage-credits to continue or switch models with /model.";
    let rate_limit = r#""error":"rate_limit","apiErrorStatus":429,"#;
    let error = detect_turn_error(&entry(rate_limit, fable)).expect("marker");
    assert_eq!(error.label, cap_turn_error_label(fable));

    for (fields, text, class) in [
        (rate_limit, fable, TurnErrorClass::PausedRateLimit),
        (
            r#""apiErrorStatus":429,"#,
            "Opus limit",
            TurnErrorClass::PausedRateLimit,
        ),
        (
            rate_limit,
            "monthly spend limit",
            TurnErrorClass::PausedSpendLimit,
        ),
        (
            r#""error":"overloaded","#,
            "Bad Request",
            TurnErrorClass::PausedOverloaded,
        ),
    ] {
        assert_eq!(
            detect_turn_error(&entry(fields, text)).unwrap().class,
            class,
            "{text}"
        );
    }
}

#[test]
fn turn_error_scan_skips_recovered_sidechain_and_nonconversation_records() {
    let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n");
    assert!(detect_turn_error(&tail).is_none());

    // A normal conversation entry newer than the error means the session
    // moved on (a resume, a rewind, a fresh prompt): alive, not dead.
    let tail = format!("{API_ERROR_ENTRY}\n{TURN_DURATION_ENTRY}\n{NORMAL_ASSISTANT_ENTRY}\n");
    assert!(detect_turn_error(&tail).is_none());

    // Rewind/fork artifacts (`file-history-snapshot`, no timestamp) and
    // `summary` records ride the tail; the scan passes over them to the
    // newest conversation entry.
    let tail = format!(
        "{API_ERROR_ENTRY}\n{TURN_DURATION_ENTRY}\n{{\"type\":\"file-history-snapshot\"}}\n{{\"type\":\"summary\",\"summary\":\"t\"}}\n"
    );
    assert!(detect_turn_error(&tail).is_some());

    // A subagent replay's API error is the child's problem; the parent's
    // newest own entry (older, normal) decides.
    let sidechain = r#"{"type":"assistant","isSidechain":true,"isApiErrorMessage":true,"timestamp":"2026-06-04T03:01:00.000Z","message":{"content":[{"type":"text","text":"API Error: Overloaded"}]}}"#;
    let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{sidechain}\n");
    assert!(detect_turn_error(&tail).is_none());
}

#[test]
fn assistant_message_readers_keep_main_thread_signal() {
    let earlier = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"old"}]}}"#;
    let latest = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hello"},{"type":"text","text":"world"}]}}"#;
    let tail = format!("{earlier}\n{latest}\n");

    assert_eq!(
        last_assistant_message(&tail).as_deref(),
        Some("hello\nworld")
    );

    let sidechain = r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"child answer"}]}}"#;
    let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{sidechain}\n");
    assert_eq!(last_assistant_message(&tail).as_deref(), Some("done"));

    let tool_only = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[]}}]}}"#;
    let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{tool_only}\n");
    assert_eq!(last_assistant_message(&tail).as_deref(), Some("done"));

    let tool_call = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"pwd"}}]}}"#;
    let tool_result =
        r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#;
    let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{tool_call}\n{tool_result}\n{tool_only}\n");
    assert_eq!(last_assistant_message(&tail).as_deref(), Some("done"));

    let meta = r#"{"type":"user","isMeta":true,"message":{"content":"generated context"}}"#;
    let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{meta}\n{tool_only}\n");
    assert_eq!(last_assistant_message(&tail).as_deref(), Some("done"));

    let prior_turn =
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"previous turn"}]}}"#;
    let user = r#"{"type":"user","message":{"content":[{"type":"text","text":"current prompt"}]}}"#;
    let tail = format!("{prior_turn}\n{user}\n{tool_only}\n");
    assert!(last_assistant_message(&tail).is_none());

    let tail = format!("{NORMAL_ASSISTANT_ENTRY}\n{API_ERROR_ENTRY}\n");
    assert!(last_assistant_message(&tail).is_none());

    let tail = format!("{prior_turn}\n{user}\n{API_ERROR_ENTRY}\n{TURN_DURATION_ENTRY}\n");

    assert!(last_assistant_message(&tail).is_none());

    let messages = parse_messages(include_str!("../tests/fixtures/stream-transcript.jsonl"))
        .into_iter()
        .filter(|message| message.role == TranscriptRole::Assistant)
        .map(|message| message.text)
        .collect::<Vec<_>>();
    assert_eq!(messages, vec!["first update", "second\nline"]);
}

#[test]
fn parse_messages_reads_user_assistant_and_timestamps() {
    let lines = concat!(
        r#"{"type":"user","timestamp":"2026-06-04T03:00:00.000Z","message":{"content":[{"type":"text","text":"fix auth"}]}}"#,
        "\n",
        r#"{"type":"user","isMeta":true,"timestamp":"2026-06-04T03:00:00.500Z","message":{"content":"Caveat: generated context"}}"#,
        "\n",
        r#"{"type":"user","timestamp":"2026-06-04T03:00:00.750Z","message":{"content":"<local-command-stdout>pwd</local-command-stdout>"}}"#,
        "\n",
        r#"{"type":"assistant","timestamp":"2026-06-04T03:00:01.000Z","message":{"content":[{"type":"text","text":"done"}]}}"#,
        "\n",
        r#"{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-06-04T03:00:02.000Z","message":{"content":[{"type":"text","text":"API Error: Overloaded"}]}}"#,
        "\n",
        r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-06-04T03:00:03.000Z","message":{"content":[{"type":"text","text":"child"}]}}"#,
        "\n",
    );
    let messages = parse_messages(lines);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, TranscriptRole::User);
    assert_eq!(messages[0].text, "fix auth");
    assert_eq!(
        messages[0].at,
        Some("2026-06-04T03:00:00Z".parse::<Timestamp>().unwrap())
    );
    assert_eq!(messages[1].role, TranscriptRole::Assistant);
    assert_eq!(messages[1].text, "done");
}

#[test]
fn turn_error_scan_tolerates_truncated_unclocked_and_empty_tail() {
    // The 64KB tail seek can split the first line mid-JSON; it fails to
    // parse and is passed over.
    let tail = format!("age\":{{\"truncated\":true}}}}\n{API_ERROR_ENTRY}\n");
    assert!(detect_turn_error(&tail).is_some());

    // No clock, no self-clear guard: the scan passes over it rather than
    // emitting a marker the projection could never expire.
    let unclocked = r#"{"type":"assistant","isApiErrorMessage":true,"message":{"content":[{"type":"text","text":"API Error: Overloaded"}]}}"#;
    assert!(detect_turn_error(&format!("{unclocked}\n")).is_none());

    assert!(detect_turn_error("").is_none());
    assert!(detect_turn_error("\n\n").is_none());
}

#[test]
fn turn_error_labels_are_capped_and_accept_flat_content() {
    let long = "x".repeat(500);
    let entry = format!(
        r#"{{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-06-04T02:56:32.919Z","message":{{"content":[{{"type":"text","text":"{long}"}}]}}}}"#
    );
    let error = detect_turn_error(&entry).expect("detected");
    assert_eq!(error.label.unwrap().chars().count(), TURN_ERROR_LABEL_MAX);

    // Tolerate a flat-string `message.content` alongside the block array.
    let entry = r#"{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-06-04T02:56:32.919Z","message":{"content":"API Error: Overloaded"}}"#;
    let error = detect_turn_error(entry).expect("detected");
    assert_eq!(error.label.as_deref(), Some("API Error: Overloaded"));
}
