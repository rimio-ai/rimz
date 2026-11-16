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
        "fast_mode": true,
        "thinking": { "enabled": true },
        "rate_limits": {
            "five_hour": { "used_percentage": 23.5, "resets_at": 1738425600i64 },
            "seven_day": { "used_percentage": 41.2, "resets_at": 1738857600i64 },
            "spend_limit": { "used_percentage": 65.2, "resets_at": 1738857600i64 }
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
    assert_eq!(ctx.fast, Some(true));
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
    assert_eq!(rate.windows.len(), 3);
    let spend = rate
        .windows
        .iter()
        .find(|window| {
            window
                .scope
                .as_ref()
                .is_some_and(|scope| scope.id == "spend")
        })
        .expect("gateway spend quota");
    assert_eq!(spend.scope.as_ref().unwrap().label, "$");
    assert_eq!(spend.duration_mins, None);
    assert_eq!(spend.used_percentage, Some(65));
    assert_eq!(spend.resets_at, Timestamp::from_second(1738857600).ok());
    assert!(spend.source.is_best_effort());
    assert_eq!(spend.observed_at, Some(ctx.observed_at));
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
fn fast_mode_projects_enabled_disabled_and_unobserved() {
    for (payload, expected) in [
        (json!({"fast_mode": true}), Some(true)),
        (json!({"fast_mode": false}), Some(false)),
        (json!({}), None),
    ] {
        assert_eq!(parse(payload.clone()).fast, expected, "{payload}");
    }
}

#[test]
fn gateway_only_spend_limit_normalizes_without_subscription_windows() {
    for (used, expected) in [(112.4, 100), (99.6, 99)] {
        let ctx = parse(
            json!({"rate_limits": {"spend_limit": {"used_percentage": used, "resets_at": 1738857600i64}}}),
        );
        assert!(ctx.rate_limits.is_some(), "gateway quota must be projected");
        let windows = ctx.rate_limits.unwrap().windows;
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percentage, Some(expected));
        assert_eq!(windows[0].is_spent(), expected == 100);
        assert_eq!(windows[0].duration_mins, None);
        assert_eq!(windows[0].scope.as_ref().unwrap().id, "spend");
        let capacity = crate::agents::ProviderCapacity::from_windows(windows);
        assert_eq!(capacity.latest_spent_window_reset(ctx.observed_at), None);
        assert!(!capacity.subscription_exhausted_for_model(ctx.observed_at, Some("opus")));
    }
    let ctx = parse(json!({"rate_limits": {"spend_limit": {"resets_at": 1738857600i64}}}));
    assert!(
        ctx.rate_limits.is_some(),
        "reset-only quota must be projected"
    );
    let windows = ctx.rate_limits.unwrap().windows;
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].used_percentage, None);
    assert_eq!(
        windows[0].resets_at,
        Timestamp::from_second(1738857600).ok()
    );
}

#[test]
fn empty_or_dollar_only_spend_limit_is_omitted() {
    for field in [
        json!({}),
        json!({"used_usd": 10, "limit_usd": 20, "period": "monthly"}),
    ] {
        assert!(
            parse(json!({"rate_limits": {"spend_limit": field}}))
                .rate_limits
                .is_none()
        );
    }
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
    assert_eq!(error.label.unwrap().chars().count(), 80);

    // Tolerate a flat-string `message.content` alongside the block array.
    let entry = r#"{"type":"assistant","isApiErrorMessage":true,"timestamp":"2026-06-04T02:56:32.919Z","message":{"content":"API Error: Overloaded"}}"#;
    let error = detect_turn_error(entry).expect("detected");
    assert_eq!(error.label.as_deref(), Some("API Error: Overloaded"));
}

#[test]
fn resting_turn_scan_continues_past_blank_lines() {
    let at = "2026-06-04T03:01:00Z".parse::<Timestamp>().unwrap();
    let tail = format!("{INTERRUPTED_ENTRY}\n \t\u{2003}\n");
    assert_eq!(detect_turn_interrupted(&tail), Some(at));
    assert!(detect_turn_error(&tail).is_none());
    let child = r#"{"type":"user","isSidechain":true,"agentId":"child","timestamp":"2026-06-04T03:01:00.000Z","message":{"role":"user","content":"[Request interrupted by user]"}}"#;
    let tail = format!("{child}\n \t\u{2003}\n");
    assert!(detect_subagent_interrupted(&tail, "child"));
    assert_eq!(detect_turn_interrupted(&tail), None);
}

#[test]
fn resting_turn_scan_trims_unicode_space_around_an_entry() {
    let at = "2026-06-04T03:01:00Z".parse::<Timestamp>().unwrap();
    let tail = format!("\u{2003}{INTERRUPTED_ENTRY}\u{2003}\n");
    assert_eq!(detect_turn_interrupted(&tail), Some(at));
}

/// Claude Code 2.1.325's auto-mode no-verdict stop, as one session wrote it:
/// the refused call's `tool_use`, a sibling Edit result that finished first,
/// the refusal flagged `toolDenialEndsTurn`, then two `system` records and no
/// `Stop` hook.
const DENIED_TOOL_USE_ENTRY: &str = r#"{"parentUuid":"87362d7d-7ea5-4e6d-a440-3b7cdc3ee14a","isSidechain":false,"message":{"model":"claude-opus-5-5","id":"msg_011Ch6EHYGEZk9h1coW571f1","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_01D98sqyHPSkpV95NdnPxSP4","name":"Bash","input":{"command":"cargo xtask test","description":"Run the carried misfit cost unit test","timeout":600000},"caller":{"type":"direct"}}],"stop_reason":"tool_use"},"type":"assistant","uuid":"97e66d3d-18df-4988-b305-a65292fe2dec","timestamp":"2026-11-16T07:41:36.485Z","version":"2.1.325"}"#;
const SIBLING_RESULT_ENTRY: &str = r#"{"parentUuid":"87362d7d-7ea5-4e6d-a440-3b7cdc3ee14a","isSidechain":false,"promptId":"508d46c3-d1a3-4900-b5b6-293ce96cbf00","type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_015H6TwEhN52TxNSapNzGGwD","type":"tool_result","content":"The file docs/guide/configuration.md has been updated successfully."}]},"uuid":"f2de0bf0-5d65-4af3-85aa-f8197fc6c916","timestamp":"2026-11-16T07:41:37.457Z","sourceToolAssistantUUID":"87362d7d-7ea5-4e6d-a440-3b7cdc3ee14a","permissionDecision":{"decision":"accept","source":"config","reasonType":"mode"},"version":"2.1.325"}"#;
const NO_VERDICT_DENIAL_ENTRY: &str = r#"{"parentUuid":"97e66d3d-18df-4988-b305-a65292fe2dec","isSidechain":false,"promptId":"508d46c3-d1a3-4900-b5b6-293ce96cbf00","type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"Auto mode is unavailable: the server returned no safety verdict for 10 responses in a row, so Bash was not run and this turn has ended. Wait for the user's next message before doing anything further.","is_error":true,"tool_use_id":"toolu_01D98sqyHPSkpV95NdnPxSP4"}]},"uuid":"9dd14655-4773-4fe3-8258-81bb79f22987","timestamp":"2026-11-16T07:41:37.485Z","toolUseResult":"Error: Auto mode is unavailable: the server returned no safety verdict for 10 responses in a row, so Bash was not run and this turn has ended. Wait for the user's next message before doing anything further.","toolDenialKind":"automode-unavailable","toolDenialEndsTurn":true,"sourceToolAssistantUUID":"97e66d3d-18df-4988-b305-a65292fe2dec","permissionDecision":{"decision":"reject","source":"config","reasonType":"classifier","reasonCode":"classifier:auto-mode:no-verdict"},"version":"2.1.325"}"#;
const AUTO_MODE_WARNING_ENTRY: &str = r#"{"parentUuid":"9dd14655-4773-4fe3-8258-81bb79f22987","isSidechain":false,"type":"system","subtype":"informational","content":"Auto mode is unavailable — the server returned no safety verdict for the last 10 responses, so Claude stopped. Send a message to try again, or switch out of auto mode.","isMeta":false,"timestamp":"2026-11-16T07:41:37.489Z","uuid":"b1128a68-efb9-471e-adb4-806ec85d682d","level":"warning","version":"2.1.325"}"#;
const DENIAL_TURN_DURATION_ENTRY: &str = r#"{"parentUuid":"b1128a68-efb9-471e-adb4-806ec85d682d","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":417487,"messageCount":1293,"timestamp":"2026-11-16T07:41:37.495Z","uuid":"d260ae6b-b7cd-4adc-a806-48199e6c6b90","isMeta":false,"version":"2.1.325"}"#;

/// The recorded stop's tail with the refusal record reshaped by `edit`.
fn denial_tail(edit: impl FnOnce(&mut serde_json::Value)) -> String {
    let mut denial: serde_json::Value = serde_json::from_str(NO_VERDICT_DENIAL_ENTRY).unwrap();
    edit(&mut denial);
    format!(
        "{DENIED_TOOL_USE_ENTRY}\n{SIBLING_RESULT_ENTRY}\n{denial}\n{AUTO_MODE_WARNING_ENTRY}\n{DENIAL_TURN_DURATION_ENTRY}\n"
    )
}

/// A flagged result Claude writes for a sibling call after the abort, with
/// no `permissionDecision` of its own.
fn flagged_sibling(at: &str) -> String {
    flagged_sibling_decided(at, None)
}

/// The same result stamped with its own call's decision, as Claude stamps
/// every result of a call it decided.
fn flagged_sibling_decided(at: &str, decision: Option<serde_json::Value>) -> String {
    let mut sibling = json!({
        "type": "user", "isSidechain": false, "timestamp": at,
        "toolDenialKind": "interrupted", "toolDenialEndsTurn": true,
        "message": {"role": "user", "content": [{
            "type": "tool_result", "tool_use_id": "toolu_sibling", "is_error": true,
            "content": [{"type": "text", "text": "Interrupted by a permission stop"}],
        }]},
    });
    if let Some(decision) = decision {
        sibling["permissionDecision"] = decision;
    }
    sibling.to_string()
}

fn no_verdict_label() -> Option<String> {
    cap_turn_error_label(
        "Auto mode is unavailable: the server returned no safety verdict for 10 responses in a row, so Bash was not run and this turn has ended. Wait for the user's next message before doing anything further.",
    )
}

#[test]
fn turn_ending_denial_pauses_on_no_verdict_and_fails_otherwise() {
    let error = detect_turn_error(&denial_tail(|_| {})).expect("the stopped turn is detected");
    assert_eq!(error.class, TurnErrorClass::PausedOverloaded);
    assert_eq!(
        error.at,
        "2026-11-16T07:41:37.485Z".parse::<Timestamp>().unwrap(),
        "the marker carries the refusal's own instant"
    );
    assert_eq!(error.label, no_verdict_label());
    assert!(!error.ended_turn, "the row stays raw running");
    assert!(detect_turn_interrupted(&denial_tail(|_| {})).is_none());

    let other = denial_tail(|denial| {
        denial["permissionDecision"]["reasonCode"] = json!("classifier:auto-mode:denied");
    });
    let error = detect_turn_error(&other).expect("another turn-ending refusal is detected");
    assert_eq!(error.class, TurnErrorClass::Failed);
    assert_eq!(error.label, no_verdict_label());
}

#[test]
fn only_the_turn_ending_flag_makes_a_denial_a_stop() {
    assert!(detect_turn_error(&denial_tail(|_| {})).is_some());
    let unflagged = denial_tail(|denial| {
        denial.as_object_mut().unwrap().remove("toolDenialEndsTurn");
    });
    assert!(detect_turn_error(&unflagged).is_none());
    let cleared = denial_tail(|denial| denial["toolDenialEndsTurn"] = json!(false));
    assert!(detect_turn_error(&cleared).is_none());
    // The census's ordinary no-verdict denial: same kind and text, a turn
    // that goes on.
    let continuing = denial_tail(|denial| {
        denial.as_object_mut().unwrap().remove("toolDenialEndsTurn");
        denial["permissionDecision"]["reasonCode"] = json!("classifier:auto-mode:unavailable");
    });
    assert!(detect_turn_error(&continuing).is_none());

    // Neither the kind nor the text decides the class: a flagged refusal of
    // another kind with the outage code pauses.
    let other_kind = denial_tail(|denial| {
        denial["toolDenialKind"] = json!("user-rejected");
        denial["message"]["content"][0]["content"] = json!("Rejected");
    });
    let error = detect_turn_error(&other_kind).expect("flagged");
    assert_eq!(error.class, TurnErrorClass::PausedOverloaded);
    assert_eq!(error.label.as_deref(), Some("Rejected"));
}

#[test]
fn turn_ending_denial_is_found_inside_the_trailing_flagged_run() {
    let sibling = flagged_sibling("2026-11-16T07:41:37.490Z");
    let tail = format!("{}{sibling}\n", denial_tail(|_| {}));
    let error = detect_turn_error(&tail).expect("the stopped turn is detected");
    assert_eq!(error.class, TurnErrorClass::PausedOverloaded);
    assert_eq!(
        error.at,
        "2026-11-16T07:41:37.490Z".parse::<Timestamp>().unwrap(),
        "the newest flagged record stamps the marker"
    );
    assert_eq!(error.label, no_verdict_label(), "the refusal labels it");

    // A sibling's own decision, allowed or aborted, never decides the class.
    for decision in [
        json!({"decision": "accept", "source": "config", "reasonType": "mode"}),
        json!({"decision": "reject", "source": {"type": "user_abort"}}),
    ] {
        let sibling = flagged_sibling_decided("2026-11-16T07:41:37.490Z", Some(decision));
        let tail = format!("{}{sibling}\n", denial_tail(|_| {}));
        let error = detect_turn_error(&tail).expect("the stopped turn is detected");
        assert_eq!(error.class, TurnErrorClass::PausedOverloaded);
        assert_eq!(error.label, no_verdict_label(), "the refusal labels it");

        // Behind another refusal it fails, labelled by that refusal.
        let denied = denial_tail(|denial| {
            denial["permissionDecision"]["reasonCode"] = json!("classifier:auto-mode:denied");
        });
        let error = detect_turn_error(&format!("{denied}{sibling}\n")).expect("flagged");
        assert_eq!(error.class, TurnErrorClass::Failed);
        assert_eq!(error.label, no_verdict_label(), "the refusal labels it");
    }

    // A flagged run with no refusal in it fails, even when an outage refusal
    // sits behind an unflagged record.
    let tail = format!(
        "{NO_VERDICT_DENIAL_ENTRY}\n{SIBLING_RESULT_ENTRY}\n{}\n",
        flagged_sibling("2026-11-16T07:41:38.000Z")
    );
    let error = detect_turn_error(&tail).expect("flagged");
    assert_eq!(error.class, TurnErrorClass::Failed);
    assert_eq!(
        error.label.as_deref(),
        Some("Interrupted by a permission stop")
    );
}

#[test]
fn a_later_prompt_clears_a_turn_ending_denial() {
    assert!(detect_turn_error(&denial_tail(|_| {})).is_some());
    let prompt = r#"{"type":"user","isSidechain":false,"timestamp":"2026-11-16T08:11:54.000Z","message":{"role":"user","content":"continue"}}"#;
    let tail = format!("{}{prompt}\n", denial_tail(|_| {}));
    assert!(detect_turn_error(&tail).is_none());
    assert!(detect_turn_interrupted(&tail).is_none());
}
