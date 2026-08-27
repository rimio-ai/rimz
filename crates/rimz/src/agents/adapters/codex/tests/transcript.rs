use super::*;
use crate::agents::adapters::codex::spend::live_fold_needs_token_counter_backfill;
use crate::agents::{LocalSpendFold, PriceBook, SessionOrigin, TokenSplit, TranscriptStat};

#[test]
fn usage_from_transcript_reads_split_totals_and_separates_zero_from_unknown() {
    let dir = tempfile::tempdir().unwrap();

    // Codex reports token usage only in the rollout JSONL. A full `token_count`
    // event carries the model, latest-call split, and model context window.
    let full = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &full,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"sess-1\"}}\n\
             {\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.5\",\"effort\":\"xhigh\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":\
             {\"last_token_usage\":{\"input_tokens\":129200,\"cached_input_tokens\":120000,\
             \"cache_write_input_tokens\":8000,\
             \"output_tokens\":800,\"total_tokens\":130000},\
             \"total_token_usage\":{\"input_tokens\":1000,\"output_tokens\":200,\
             \"cached_input_tokens\":400},\
             \"model_context_window\":258400}}}\n",
    )
    .unwrap();
    let usage = usage_from_transcript(&full);
    assert_eq!(usage.reported_context_window(), Some(258_400));
    assert_eq!(usage.total_tokens, Some(130_000));
    assert_eq!(usage.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(usage.effort.as_deref(), Some("xhigh"));
    assert_eq!(usage.last_input_tokens, Some(129_200));
    assert_eq!(usage.last_cached_input_tokens, Some(120_000));
    assert_eq!(usage.last_cache_write_tokens, Some(8_000));
    assert_eq!(usage.last_output_tokens, Some(800));

    // A brand-new session (rollout opened, no `token_count` yet) reads as an
    // explicit zero with the provider-default window, so the gauge draws an
    // empty bar rather than vanishing.
    let fresh = dir.path().join("fresh.jsonl");
    std::fs::write(
        &fresh,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"sess-1\"}}\n",
    )
    .unwrap();
    let usage = usage_from_transcript(&fresh);
    assert_eq!(usage.context_window, Some(272_000));
    assert_eq!(usage.reported_context_window(), None);
    assert_eq!(usage.total_tokens, Some(0));
    assert_eq!(usage.effort, None);
    assert_eq!(usage.last_input_tokens, Some(0));
    assert_eq!(usage.last_cached_input_tokens, Some(0));
    assert_eq!(usage.last_cache_write_tokens, Some(0));
    assert_eq!(usage.last_output_tokens, Some(0));

    // An unreadable rollout is unknown, not zero — the gauge stays hidden.
    let usage = usage_from_transcript(Path::new("/nonexistent/path/rollout.jsonl"));
    assert_eq!(usage.context_window, None);
    assert_eq!(usage.total_tokens, None);

    // An older `last_token_usage` carrying only input + total leaves the
    // cached/output sides unknown rather than zero.
    let older = dir.path().join("older.jsonl");
    std::fs::write(
        &older,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
             \"last_token_usage\":{\"input_tokens\":500,\"total_tokens\":600},\
             \"model_context_window\":100000}}}\n",
    )
    .unwrap();
    let usage = usage_from_transcript(&older);
    assert_eq!(usage.last_input_tokens, Some(500));
    assert_eq!(usage.last_cached_input_tokens, None);
    assert_eq!(usage.last_cache_write_tokens, None);
    assert_eq!(usage.last_output_tokens, None);
}

#[test]
fn shared_scan_keeps_latest_usage_and_same_turn_plan_past_older_recovery() {
    let tail = concat!(
        r#"{"timestamp":"2026-07-13T09:00:00Z","type":"event_msg","payload":{"type":"stream_error","message":"older failure"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-13T09:01:00Z","type":"event_msg","payload":{"type":"task_started"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-13T10:00:00Z","type":"turn_context","payload":{"turn_id":"turn-plan","model":"gpt-5.5","effort":"high"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-13T10:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":120,"cached_input_tokens":20,"output_tokens":8,"total_tokens":128},"total_token_usage":{"input_tokens":1000,"cached_input_tokens":200,"output_tokens":80},"model_context_window":258400}}}"#,
        "\n",
        r#"{"timestamp":"2026-07-13T10:00:02Z","type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-plan","item":{"type":"Plan","text":"Ship shared scan"}}}"#,
        "\n",
        r#"{"timestamp":"2026-07-13T10:00:03Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-plan","last_agent_message":"Codex says:"}}"#,
    );

    let (usage, outcome, raw_error) =
        scan_transcript_tail(tail, TranscriptScanNeed::UsageAndOutcome).into_parts();
    assert_eq!(usage.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(usage.effort.as_deref(), Some("high"));
    assert_eq!(usage.last_input_tokens, Some(120));
    let Some(RestingTurnOutcome::PlanProposed(plan)) = outcome else {
        panic!("latest clean turn should rest on its plan")
    };
    assert_eq!(plan.text, "Ship shared scan");
    assert_eq!(plan.at, "2026-07-13T10:00:03Z".parse().unwrap());
    assert_eq!(raw_error, None);
}

#[test]
fn raw_error_scan_looks_past_newer_abort() {
    let tail = concat!(
        r#"{"timestamp":"2026-07-13T10:00:01Z","type":"event_msg","payload":{"type":"stream_error","message":"provider failed"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-13T10:00:02Z","type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn-failed"}}"#,
    );

    let error = detect_turn_error(tail).expect("abort does not hide raw provider error");
    assert_eq!(error.label.as_deref(), Some("provider failed"));
    assert_eq!(
        detect_turn_interrupted(tail),
        Some("2026-07-13T10:00:02Z".parse().unwrap())
    );
}

#[test]
fn stream_assistant_messages_reads_rollout_agent_messages_only() {
    let messages =
        CodexAdapter.stream_assistant_messages(include_str!("fixtures/stream-rollout.jsonl"));
    assert_eq!(messages, vec!["first update", "second update"]);
}

#[test]
fn parse_transcript_messages_reads_user_assistant_and_timestamps() {
    let messages =
        CodexAdapter.parse_transcript_messages(include_str!("fixtures/stream-rollout.jsonl"));
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].role, TranscriptRole::User);
    assert_eq!(messages[0].text, "prompt");
    assert_eq!(
        messages[0].at,
        Some(
            "2026-06-07T16:36:00.539Z"
                .parse::<jiff::Timestamp>()
                .unwrap()
        )
    );
    assert_eq!(messages[1].role, TranscriptRole::Assistant);
    assert_eq!(messages[1].text, "first update");
    assert_eq!(messages[2].role, TranscriptRole::Assistant);
    assert_eq!(messages[2].text, "second update");
}

#[test]
fn parse_transcript_messages_reads_paginated_visible_messages_only() {
    let rollout = include_str!("fixtures/paginated-rollout.jsonl");
    let messages = CodexAdapter.parse_transcript_messages(rollout);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, TranscriptRole::User);
    assert_eq!(messages[0].text, "first line\nsecond line");
    assert_eq!(
        messages[0].at,
        Some(
            "2026-07-18T09:00:01.000Z"
                .parse::<jiff::Timestamp>()
                .unwrap()
        )
    );
    assert_eq!(messages[1].role, TranscriptRole::Assistant);
    assert_eq!(messages[1].text, "assistant update");
    assert_eq!(
        CodexAdapter.stream_assistant_messages(rollout),
        vec!["assistant update"]
    );
}

#[test]
fn turn_error_detector_maps_known_error_shapes() {
    let rate_limit = json!({
        "timestamp": "2026-06-11T07:18:00.000Z",
        "type": "event_msg",
        "payload": {
            "type": "turn_error",
            "message": "You've hit your usage limit",
            "codexErrorInfo": "usageLimitExceeded"
        }
    })
    .to_string();
    let error = detect_turn_error(&rate_limit).expect("turn error detected");
    assert_eq!(error.class, crate::agents::TurnErrorClass::PausedRateLimit);
    assert_eq!(
        error.at,
        "2026-06-11T07:18:00.000Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
    );
    assert_eq!(error.label.as_deref(), Some("You've hit your usage limit"));

    // Generated from `codex app-server generate-json-schema --out …`:
    // ErrorNotification carries `error.message` plus `error.codexErrorInfo`.
    // The rollout wrapper still supplies the timestamp RimZ needs for
    // self-clear projection.
    let schema_notification = json!({
        "timestamp": "2026-06-11T07:18:00.000Z",
        "error": {
            "message": "You've hit your usage limit",
            "codexErrorInfo": "usageLimitExceeded"
        },
        "threadId": "thread-1",
        "turnId": "turn-1",
        "willRetry": false
    })
    .to_string();

    let error = detect_turn_error(&schema_notification).expect("schema-shaped error detected");
    assert_eq!(error.class, crate::agents::TurnErrorClass::PausedRateLimit);
    assert_eq!(error.label.as_deref(), Some("You've hit your usage limit"));

    let session_limit = json!({
        "timestamp": "2026-06-11T07:18:00.000Z",
        "type": "event_msg",
        "payload": {
            "type": "turn_error",
            "message": "You've hit your session limit · resets 10:50am (UTC)"
        }
    })
    .to_string();
    let error = detect_turn_error(&session_limit).expect("session-limit error detected");
    assert_eq!(error.class, crate::agents::TurnErrorClass::PausedRateLimit);
    assert_eq!(
        error.label.as_deref(),
        Some("You've hit your session limit · resets 10:50am (UTC)")
    );

    let spend_limit = json!({
        "timestamp": "2026-06-11T07:18:00.000Z",
        "type": "event_msg",
        "payload": {
            "type": "turn_error",
            "message": "You've hit your monthly spend limit."
        }
    })
    .to_string();
    let error = detect_turn_error(&spend_limit).expect("spend-limit error detected");
    assert_eq!(error.class, crate::agents::TurnErrorClass::PausedSpendLimit);
    assert_eq!(
        error.label.as_deref(),
        Some("You've hit your monthly spend limit.")
    );

    let overloaded = json!({
        "timestamp": "2026-06-11T07:18:00.000Z",
        "type": "event_msg",
        "payload": {
            "type": "stream_error",
            "message": "Server is busy. Try again later."
        }
    })
    .to_string();
    assert_eq!(
        detect_turn_error(&overloaded).expect("overloaded").class,
        crate::agents::TurnErrorClass::PausedOverloaded
    );

    let stalled = json!({
        "timestamp": "2026-06-11T07:18:30.000Z",
        "type": "event_msg",
        "payload": {
            "type": "stream_error",
            "message": "API Error: Response stalled mid-stream. The response above may be incomplete."
        }
    })
    .to_string();
    assert_eq!(
        detect_turn_error(&stalled).expect("stalled stream").class,
        crate::agents::TurnErrorClass::PausedOverloaded
    );

    let transient = json!({
        "timestamp": "2026-06-11T07:19:00.000Z",
        "type": "event_msg",
        "payload": {
            "type": "task_complete",
            "error": {
                "message": "API Error: Server Error",
                "codexErrorInfo": "internalServerError"
            }
        }
    })
    .to_string();
    let error = detect_turn_error(&transient).expect("transient error");
    assert_eq!(error.class, crate::agents::TurnErrorClass::PausedOverloaded);
    assert_eq!(error.label.as_deref(), Some("API Error: Server Error"));
    let class_from_kind = |kind: &str, message: &str| {
        let tail = json!({
            "timestamp": "2026-06-11T07:19:00.000Z",
            "type": "event_msg",
            "payload": {
                "type": "task_complete",
                "error": {
                    "message": message,
                    "codexErrorInfo": kind
                }
            }
        })
        .to_string();
        detect_turn_error(&tail).expect("known error").class
    };
    for (class, camel, snake) in [
        (
            crate::agents::TurnErrorClass::PausedRateLimit,
            "usageLimitExceeded",
            "usage_limit_exceeded",
        ),
        (
            crate::agents::TurnErrorClass::PausedOverloaded,
            "serverOverloaded",
            "server_overloaded",
        ),
        (
            crate::agents::TurnErrorClass::PausedOverloaded,
            "internalServerError",
            "internal_server_error",
        ),
        (
            crate::agents::TurnErrorClass::Failed,
            "contextWindowExceeded",
            "context_window_exceeded",
        ),
        (
            crate::agents::TurnErrorClass::Failed,
            "unauthorized",
            "unauthorized",
        ),
        (
            crate::agents::TurnErrorClass::Failed,
            "badRequest",
            "bad_request",
        ),
        (
            crate::agents::TurnErrorClass::Failed,
            "sandboxError",
            "sandbox_error",
        ),
        (
            crate::agents::TurnErrorClass::Failed,
            "cyberPolicy",
            "cyber_policy",
        ),
        (
            crate::agents::TurnErrorClass::Failed,
            "threadRollbackFailed",
            "thread_rollback_failed",
        ),
    ] {
        for kind in [camel, snake] {
            let contrary_label = if class == crate::agents::TurnErrorClass::Failed {
                "Service Unavailable"
            } else {
                "API Error: Bad Request"
            };
            assert_eq!(class_from_kind(kind, contrary_label), class, "{kind}");
        }
    }
    assert_eq!(
        class_from_kind("other", "Service Unavailable"),
        crate::agents::TurnErrorClass::PausedOverloaded
    );
    assert_eq!(
        class_from_kind("other", "API Error: Bad Request"),
        crate::agents::TurnErrorClass::Failed
    );

    let rollout_503 = r#"{"timestamp":"2026-07-25T09:24:51.105Z","type":"event_msg","payload":{"type":"task_complete","error":{"message":"unexpected status 503 Service Unavailable: Service Unavailable, url: https://chatgpt.com/backend-api/codex/responses, cf-ray: a20a1d2aca20f069-DFW, auth error: 503, auth error code: biscuit_baker_service_me_circuit_open","codex_error_info":"other"}}}"#;
    assert_eq!(
        detect_turn_error(rollout_503).expect("rollout 503").class,
        crate::agents::TurnErrorClass::PausedOverloaded
    );
    let rollout_high_demand = r#"{"timestamp":"2026-07-25T09:25:09.577Z","type":"event_msg","payload":{"type":"task_complete","error":{"message":"We're currently experiencing high demand, which may cause temporary errors.","codex_error_info":"internal_server_error"}}}"#;
    assert_eq!(
        detect_turn_error(rollout_high_demand)
            .expect("rollout high-demand error")
            .class,
        crate::agents::TurnErrorClass::PausedOverloaded
    );

    let terminal = json!({
        "timestamp": "2026-06-11T07:19:00.000Z",
        "type": "event_msg",
        "payload": {
            "type": "task_complete",
            "error": {
                "message": "API Error: Bad Request",
                "codexErrorInfo": "badRequest"
            }
        }
    })
    .to_string();
    let error = detect_turn_error(&terminal).expect("terminal error");
    assert_eq!(error.class, crate::agents::TurnErrorClass::Failed);
    assert_eq!(error.label.as_deref(), Some("API Error: Bad Request"));

    for empty_error in [json!(false), serde_json::Value::Null, json!(""), json!({})] {
        let benign = json!({
            "timestamp": "2026-06-11T07:20:00.000Z",
            "type": "event_msg",
            "payload": { "type": "task_complete", "error": empty_error }
        })
        .to_string();
        assert!(
            detect_turn_error(&benign).is_none(),
            "empty task_complete error must not mark a dead turn"
        );
    }
}

#[test]
fn turn_error_detector_self_clears_only_on_newer_live_clocked_records() {
    let error = json!({
        "timestamp": "2026-06-11T07:18:00.000Z",
        "type": "event_msg",
        "payload": { "type": "error", "message": "API Error: Server Error" }
    });
    let newer = json!({
        "timestamp": "2026-06-11T07:19:00.000Z",
        "type": "event_msg",
        "payload": {
            "type": "agent_message",
            "message": "I recovered"
        }
    });
    assert!(
        detect_turn_error(&format!("{error}\n{newer}\n")).is_none(),
        "a newer live rollout record means the session recovered"
    );

    let token_count = json!({
        "timestamp": "2026-06-11T07:19:00.000Z",
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": { "last_token_usage": { "total_tokens": 42 } }
        }
    });

    assert!(
        detect_turn_error(&format!("{error}\n{token_count}\n")).is_some(),
        "a token gauge after an error is not proof the turn recovered"
    );

    let unclocked = json!({
        "type": "event_msg",
        "payload": { "type": "error", "message": "API Error: Server Error" }
    })
    .to_string();
    assert!(detect_turn_error(&unclocked).is_none());

    let long = "x".repeat(160);
    let clocked = json!({
        "timestamp": "2026-06-11T07:18:00.000Z",
        "type": "event_msg",
        "payload": { "type": "error", "message": long }
    })
    .to_string();
    let error = detect_turn_error(&clocked).expect("clocked error");
    assert_eq!(error.label.unwrap().chars().count(), 80);
}

#[test]
fn turn_complete_detector_marks_clean_completion_but_skips_errored_and_superseded() {
    // A Codex `/review` closes on a clean `task_complete` that fires no `Stop`
    // hook — the success twin of the dead-turn detector. The tail mirrors a real
    // review session ending on a clean `task_complete`.
    let task_started = json!({"timestamp":"2026-06-14T05:51:39.805Z","type":"event_msg","payload":{"type":"task_started"}});
    let user_message = json!({"timestamp":"2026-06-14T05:51:40.861Z","type":"event_msg","payload":{"type":"user_message","message":"/review"}});
    let exited = json!({"timestamp":"2026-06-14T05:59:49.267Z","type":"event_msg","payload":{"type":"exited_review_mode"}});
    let agent_message = json!({"timestamp":"2026-06-14T05:59:49.267Z","type":"event_msg","payload":{"type":"agent_message","message":"patch is correct"}});
    let task_complete = json!({"timestamp":"2026-06-14T05:59:49.268Z","type":"event_msg","payload":{"type":"task_complete","last_agent_message":"patch is correct"}});
    let tail =
        format!("{task_started}\n{user_message}\n{exited}\n{agent_message}\n{task_complete}\n");
    assert_eq!(
        detect_turn_complete(&tail),
        Some(
            "2026-06-14T05:59:49.268Z"
                .parse::<jiff::Timestamp>()
                .unwrap()
        ),
        "a clean task_complete at the tail marks the turn done"
    );

    // An errored `task_complete` is a death owned by `detect_turn_error`, and an
    // empty `error` (`null`/`false`/`""`/`{}`) is too ambiguous to claim success
    // over — only a record with no `error` field at all can settle.
    let errored = json!({
        "timestamp": "2026-06-14T05:59:49.268Z",
        "type": "event_msg",
        "payload": { "type": "task_complete", "error": { "message": "API Error" } }
    })
    .to_string();
    assert!(detect_turn_complete(&errored).is_none());
    for empty_error in [json!(false), serde_json::Value::Null, json!(""), json!({})] {
        let ambiguous = json!({
            "timestamp": "2026-06-14T05:59:49.268Z",
            "type": "event_msg",
            "payload": { "type": "task_complete", "error": empty_error }
        })
        .to_string();
        assert!(
            detect_turn_complete(&ambiguous).is_none(),
            "an empty-error task_complete is too ambiguous to mark a completion"
        );
    }
    for last_agent_message in [serde_json::Value::Null, json!(""), json!("   ")] {
        let messageless = json!({
            "timestamp": "2026-06-14T05:59:49.268Z",
            "type": "event_msg",
            "payload": { "type": "task_complete", "last_agent_message": last_agent_message }
        })
        .to_string();
        assert!(
            detect_turn_complete(&messageless).is_none(),
            "a clean-looking task_complete still needs a final assistant message"
        );
    }

    // A fresh turn already underway after a prior completion is not at rest.
    let complete = json!({
        "timestamp": "2026-06-14T05:59:49.268Z",
        "type": "event_msg",
        "payload": { "type": "task_complete" }
    });
    let next_turn = json!({
        "timestamp": "2026-06-14T06:01:00.000Z",
        "type": "event_msg",
        "payload": { "type": "user_message", "message": "another prompt" }
    });
    assert!(
        detect_turn_complete(&format!("{complete}\n{next_turn}\n")).is_none(),
        "a newer prompt means a fresh turn, not a completed one"
    );
}

#[test]
fn plan_detector_requires_a_resting_same_turn_plan_item() {
    let plan = json!({
        "timestamp": "2026-07-13T10:00:01Z",
        "type": "event_msg",
        "payload": {
            "type": "item_completed",
            "turn_id": "turn-plan",
            "item": { "type": "Plan", "id": "turn-plan-plan", "text": "# Plan\n\nShip it." }
        }
    });
    let streamed_fallback = json!({
        "timestamp": "2026-07-13T10:00:02Z",
        "type": "event_msg",
        "payload": { "type": "agent_message", "message": "Codex says:" }
    });
    let complete = json!({
        "timestamp": "2026-07-13T10:00:03Z",
        "type": "event_msg",
        "payload": {
            "type": "task_complete",
            "turn_id": "turn-plan",
            "last_agent_message": "Codex says:"
        }
    });
    let tail = format!("{plan}\n{streamed_fallback}\n{complete}\n");
    let detected = detect_plan_proposed(&tail).expect("resting plan proposal");
    assert_eq!(detected.text, "# Plan\n\nShip it.");
    assert_eq!(
        detected.at,
        "2026-07-13T10:00:03Z".parse::<jiff::Timestamp>().unwrap()
    );
    assert_eq!(detect_turn_complete(&tail), None);

    let next_prompt = json!({
        "timestamp": "2026-07-13T10:01:00Z",
        "type": "event_msg",
        "payload": { "type": "user_message", "message": "keep planning" }
    });
    assert!(
        detect_plan_proposed(&format!("{tail}{next_prompt}\n")).is_none(),
        "a newer prompt self-clears the proposal marker"
    );

    let mismatched = tail.replace("\"turn-plan-plan\"", "\"other-plan\"");
    assert!(
        detect_plan_proposed(&mismatched).is_some(),
        "item id is opaque"
    );
    let mismatched = tail.replacen("\"turn-plan\"", "\"other-turn\"", 1);
    assert!(
        detect_plan_proposed(&mismatched).is_none(),
        "the Plan item must belong to the completed turn"
    );
}

#[test]
fn plan_detector_rejects_aborts_errors_and_update_plan_tools() {
    let aborted = concat!(
        r#"{"timestamp":"2026-07-13T10:00:01Z","type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-plan","item":{"type":"Plan","text":"ship"}}}"#,
        "\n",
        r#"{"timestamp":"2026-07-13T10:00:02Z","type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn-plan"}}"#,
    );
    assert!(detect_plan_proposed(aborted).is_none());

    let errored = concat!(
        r#"{"timestamp":"2026-07-13T10:00:01Z","type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-plan","item":{"type":"Plan","text":"ship"}}}"#,
        "\n",
        r#"{"timestamp":"2026-07-13T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-plan","error":{"message":"failed"}}}"#,
    );
    assert!(detect_plan_proposed(errored).is_none());

    let update_plan = concat!(
        r#"{"timestamp":"2026-07-13T10:00:01Z","type":"response_item","payload":{"type":"custom_tool_call","name":"update_plan","call_id":"1","input":"{}"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-13T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-default","last_agent_message":"done"}}"#,
    );
    assert!(detect_plan_proposed(update_plan).is_none());
}

#[test]
fn transcript_refresh_stamps_plan_marker_instead_of_completion() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-plan.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"timestamp":"2026-07-13T10:00:01Z","type":"event_msg","payload":{"type":"item_completed","turn_id":"turn-plan","item":{"type":"Plan","text":"ship"}}}"#,
            "\n",
            r#"{"timestamp":"2026-07-13T10:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-plan","last_agent_message":"Codex says:"}}"#,
            "\n",
        ),
    )
    .unwrap();

    let refresh = refresh_transcript_context(
        "sess-plan",
        None,
        path.to_str(),
        None,
        None,
        &dir.path().join("prices.json"),
    )
    .expect("transcript refresh");
    assert_eq!(
        refresh.context.settle,
        crate::agents::FieldPatch::Set(crate::agents::TurnSettle::new(
            "2026-07-13T10:00:02Z".parse().unwrap(),
            crate::agents::TurnSettleOutcome::PlanProposed,
        ))
    );
    assert_eq!(refresh.context.turn_error, crate::agents::FieldPatch::Clear);
}

#[test]
fn turn_interrupted_detector_marks_resting_abort_and_self_clears() {
    // Esc and `/clear` of a running Codex turn leave a resting
    // `turn_aborted` tail without a Stop hook. Any abort reason counts; the
    // "resting at the tail" filter is what keeps steer/replaced aborts from
    // sticking once the next turn starts.
    let interrupted = json!({
        "timestamp": "2026-07-07T14:12:00.000Z",
        "type": "event_msg",
        "payload": {
            "type": "turn_aborted",
            "reason": "interrupted",
            "turn_id": "turn-1",
            "completed_at": "2026-07-07T14:12:00.000Z"
        }
    });
    assert_eq!(
        detect_turn_interrupted(&interrupted.to_string()),
        Some(
            "2026-07-07T14:12:00.000Z"
                .parse::<jiff::Timestamp>()
                .unwrap()
        )
    );
    assert_eq!(detect_turn_complete(&interrupted.to_string()), None);

    let replaced = json!({
        "timestamp": "2026-07-07T14:12:01.000Z",
        "type": "event_msg",
        "payload": {
            "type": "turn_aborted",
            "reason": "replaced",
            "turn_id": "turn-1"
        }
    });
    assert!(
        detect_turn_interrupted(&replaced.to_string()).is_some(),
        "any abort reason is a marker while it rests at the tail"
    );

    for live_record in [
        json!({
            "timestamp": "2026-07-07T14:12:02.000Z",
            "type": "event_msg",
            "payload": { "type": "task_started" }
        }),
        json!({
            "timestamp": "2026-07-07T14:12:02.000Z",
            "type": "event_msg",
            "payload": { "type": "user_message", "message": "next prompt" }
        }),
    ] {
        assert!(
            detect_turn_interrupted(&format!("{interrupted}\n{live_record}\n")).is_none(),
            "a later live record means the abort no longer rests at the tail"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-interrupted.jsonl");
    let pricing_cache_path = dir.path().join("pricing-cache.json");
    std::fs::write(&path, format!("{interrupted}\n")).unwrap();
    let refresh = refresh_transcript_context(
        "sess-1",
        None,
        Some(path.to_string_lossy().as_ref()),
        None,
        None,
        &pricing_cache_path,
    )
    .expect("changed transcript refreshes");
    assert_eq!(
        refresh.context.settle,
        crate::agents::FieldPatch::Set(crate::agents::TurnSettle::new(
            "2026-07-07T14:12:00.000Z"
                .parse::<jiff::Timestamp>()
                .unwrap(),
            crate::agents::TurnSettleOutcome::Interrupted,
        ))
    );
    assert_eq!(refresh.context.turn_error, crate::agents::FieldPatch::Clear);
}

#[test]
fn messageless_task_complete_refreshes_as_overload_death() {
    for last_agent_message in [serde_json::Value::Null, json!("")] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-session.jsonl");
        let pricing_cache_path = dir.path().join("pricing-cache.json");
        std::fs::write(
            &path,
            format!(
                "{}\n",
                json!({
                    "timestamp": "2026-07-03T12:55:00.000Z",
                    "type": "event_msg",
                    "payload": {
                        "type": "task_complete",
                        "last_agent_message": last_agent_message
                    }
                })
            ),
        )
        .unwrap();

        let refresh = refresh_transcript_context(
            "sess-1",
            None,
            Some(path.to_string_lossy().as_ref()),
            None,
            None,
            &pricing_cache_path,
        )
        .expect("changed transcript refreshes");
        let error = refresh
            .context
            .turn_error
            .into_set()
            .expect("shape death is stamped");
        assert_eq!(error.class, crate::agents::TurnErrorClass::Unknown);
        assert_eq!(
            error.at,
            "2026-07-03T12:55:00.000Z"
                .parse::<jiff::Timestamp>()
                .unwrap()
        );
        assert_eq!(
            error.label.as_deref(),
            Some("turn ended with no final message")
        );
        assert_eq!(refresh.context.settle, crate::agents::FieldPatch::Clear);
    }
}

#[test]
fn resting_outcome_skips_compaction_blip_and_prefers_real_errors() {
    let compaction_complete = json!({
        "timestamp": "2026-07-03T12:55:00.000Z",
        "type": "event_msg",
        "payload": { "type": "task_complete", "last_agent_message": null }
    });
    let task_started = json!({
        "timestamp": "2026-07-03T12:55:00.100Z",
        "type": "event_msg",
        "payload": { "type": "task_started" }
    });
    let dir = tempfile::tempdir().unwrap();
    let pricing_cache_path = dir.path().join("pricing-cache.json");
    let path = dir.path().join("rollout-compaction.jsonl");
    std::fs::write(&path, format!("{compaction_complete}\n{task_started}\n")).unwrap();
    let refresh = refresh_transcript_context(
        "sess-1",
        None,
        Some(path.to_string_lossy().as_ref()),
        None,
        None,
        &pricing_cache_path,
    )
    .expect("changed transcript refreshes");
    assert_eq!(refresh.context.turn_error, crate::agents::FieldPatch::Clear);
    assert_eq!(refresh.context.settle, crate::agents::FieldPatch::Clear);

    let path = dir.path().join("rollout-error.jsonl");
    let real_error = json!({
        "timestamp": "2026-07-03T12:56:00.000Z",
        "type": "event_msg",
        "payload": {
            "type": "stream_error",
            "message": "Server is busy. Try again later."
        }
    });
    std::fs::write(&path, format!("{compaction_complete}\n{real_error}\n")).unwrap();
    let refresh = refresh_transcript_context(
        "sess-1",
        None,
        Some(path.to_string_lossy().as_ref()),
        None,
        None,
        &pricing_cache_path,
    )
    .expect("changed transcript refreshes");
    let error = refresh
        .context
        .turn_error
        .into_set()
        .expect("real error wins");
    assert_eq!(
        error.label.as_deref(),
        Some("Server is busy. Try again later.")
    );
    assert_eq!(error.class, crate::agents::TurnErrorClass::PausedOverloaded);
    assert_eq!(refresh.context.settle, crate::agents::FieldPatch::Clear);
}

#[test]
fn death_warning_from_frame_extracts_keyword_proven_banner_above_prompt() {
    let frame = "\
intro
⚠ Earlier warning

⚠ Selected model is at capacity. Please try a different model.
›
";
    assert_eq!(
        death_warning_from_frame(frame).as_deref(),
        Some("Selected model is at capacity. Please try a different model.")
    );

    let wrapped = "\
│ output │
│ ⚠ Selected model is at capacity. Please │
│ try a different model. │
│ ›  │
";
    assert_eq!(
        death_warning_from_frame(wrapped).as_deref(),
        Some("Selected model is at capacity. Please try a different model.")
    );

    let usage_limit = "\
› $rebase main and push

■ You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try
again at 6:35 AM.

› Implement {feature}
";
    let expected_usage_limit = "You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at 6:35 AM."
        .chars()
        .take(80)
        .collect::<String>();
    assert_eq!(
        death_warning_from_frame(usage_limit),
        Some(expected_usage_limit)
    );

    assert_eq!(
        death_warning_from_frame("You've hit your usage limit. Try again later.\n› \n").as_deref(),
        Some("You've hit your usage limit. Try again later.")
    );

    assert_eq!(death_warning_from_frame("no warning\n› \n"), None);
    assert_eq!(
        death_warning_from_frame(
            "⚠ Selected model is at capacity. Please try a different model.\n\n⚠ Provider ended turn early\n› \n"
        )
        .as_deref(),
        Some("Provider ended turn early"),
        "the nearest warning banner wins even when its text is unrecognized"
    );
    assert_eq!(
        death_warning_from_frame("› You've hit your usage limit\n› \n"),
        None,
        "prompt echoes are not provider warnings"
    );
    assert_eq!(
        death_warning_from_frame(
            "└ test client::reconnect ... FAILED: connection reset by peer\n› \n"
        ),
        None,
        "ordinary agent output is not a provider warning"
    );
}

#[test]
fn refine_turn_death_from_frame_parks_keyword_proven_and_adopts_banner() {
    let mut capacity = crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::Unknown,
        at: "2026-07-03T12:55:00.000Z".parse().unwrap(),
        label: Some("turn ended with no final message".to_owned()),
    };
    refine_turn_death_from_frame(
        &mut capacity,
        "⚠ Selected model is at capacity. Please try a different model.\n› \n",
    );
    assert_eq!(
        capacity.class,
        crate::agents::TurnErrorClass::PausedOverloaded
    );
    assert_eq!(
        capacity.label.as_deref(),
        Some("Selected model is at capacity. Please try a different model.")
    );

    let mut usage_limit = crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::Unknown,
        at: "2026-07-03T12:55:00.000Z".parse().unwrap(),
        label: Some("turn ended with no final message".to_owned()),
    };
    refine_turn_death_from_frame(
        &mut usage_limit,
        "\
› $rebase main and push

■ You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try
again at 6:35 AM.

› Implement {feature}
",
    );
    assert_eq!(
        usage_limit.class,
        crate::agents::TurnErrorClass::PausedRateLimit
    );
    let expected_usage_limit = "You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at 6:35 AM."
        .chars()
        .take(80)
        .collect::<String>();
    assert_eq!(
        usage_limit.label.as_deref(),
        Some(expected_usage_limit.as_str())
    );

    let mut unknown = crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::Unknown,
        at: "2026-07-03T12:55:00.000Z".parse().unwrap(),
        label: Some("turn ended with no final message".to_owned()),
    };
    refine_turn_death_from_frame(&mut unknown, "⚠ Provider ended turn early\n› \n");
    assert_eq!(unknown.class, crate::agents::TurnErrorClass::Failed);
    assert_eq!(unknown.label.as_deref(), Some("Provider ended turn early"));

    let mut numeric_pane_line = crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::Unknown,
        at: "2026-07-03T12:55:00.000Z".parse().unwrap(),
        label: Some("turn ended with no final message".to_owned()),
    };
    refine_turn_death_from_frame(
        &mut numeric_pane_line,
        "fetched https://example.com/a in 512ms\n› \n",
    );
    assert_eq!(
        numeric_pane_line.class,
        crate::agents::TurnErrorClass::Unknown
    );
    assert_eq!(
        numeric_pane_line.label.as_deref(),
        Some("turn ended with no final message")
    );
}

#[test]
fn infer_turn_death_from_spent_window_parks_only_generic_marker() {
    let now = "2026-07-07T10:17:26.638Z"
        .parse::<jiff::Timestamp>()
        .unwrap();
    let future_reset = now
        .checked_add(jiff::SignedDuration::from_secs(60 * 60))
        .unwrap();
    let past_reset = now
        .checked_sub(jiff::SignedDuration::from_secs(60))
        .unwrap();
    let capacity = |used_percentage, resets_at| {
        crate::agents::ProviderCapacity::from_windows(vec![crate::agents::RateLimitWindow {
            used_percentage,
            resets_at,
            duration_mins: Some(300),
            ..Default::default()
        }])
    };
    let generic = || crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::Unknown,
        at: now,
        label: Some("turn ended with no final message".to_owned()),
    };

    let spent = capacity(Some(100), Some(future_reset));
    let mut error = generic();
    infer_turn_death_from_spent_window(&mut error, Some(&spent), now);
    assert_eq!(error.class, crate::agents::TurnErrorClass::PausedRateLimit);
    assert_eq!(
        error.label.as_deref(),
        Some("usage limit inferred (rate-limit window spent)")
    );

    for capacity in [
        capacity(Some(99), Some(future_reset)),
        capacity(Some(100), Some(past_reset)),
        capacity(None, Some(future_reset)),
    ] {
        let mut error = generic();
        infer_turn_death_from_spent_window(&mut error, Some(&capacity), now);
        assert_eq!(error, generic());
    }

    let mut proven = crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::PausedOverloaded,
        at: now,
        label: Some("Selected model is at capacity.".to_owned()),
    };
    infer_turn_death_from_spent_window(&mut proven, Some(&spent), now);
    assert_eq!(
        proven,
        crate::agents::AgentTurnError {
            class: crate::agents::TurnErrorClass::PausedOverloaded,
            at: now,
            label: Some("Selected model is at capacity.".to_owned()),
        }
    );
}

#[test]
fn transcript_enrichment_maps_split_to_rich_usage() {
    // The latest-call split maps onto `current_usage` (cache-read and cache-write
    // slices removed from fresh input), with no baked percentage — the gauge
    // derives it from `current_usage` over the window downstream.
    let split = TranscriptUsage {
        context_window: Some(10_000),
        context_window_reported: true,
        total_tokens: Some(4_200),
        model: Some("gpt-5".to_owned()),
        effort: None,
        last_input_tokens: Some(1_200),
        last_cached_input_tokens: Some(1_000),
        last_cache_write_tokens: Some(100),
        last_output_tokens: Some(80),
    };
    let (tokens, model_id) = transcript_enrichment(&split, None);
    let tokens = tokens.expect("tokens are mapped");
    let current = tokens.current_usage.expect("current usage is mapped");
    assert_eq!(tokens.context_window_size, Some(10_000));
    assert_eq!(tokens.used_percentage, None);
    assert_eq!(tokens.remaining_percentage, None);
    assert_eq!(current.input_tokens, Some(100));
    assert_eq!(current.cache_read_input_tokens, Some(1_000));
    assert_eq!(current.cache_creation_input_tokens, Some(100));
    assert_eq!(current.output_tokens, Some(80));
    assert_eq!(
        current.input_tokens.unwrap()
            + current.cache_read_input_tokens.unwrap()
            + current.cache_creation_input_tokens.unwrap_or(0),
        split.last_input_tokens.unwrap(),
        "rich context numerator matches the row-level fallback"
    );
    assert_eq!(model_id.as_deref(), Some("gpt-5"));
}

#[test]
fn transcript_enrichment_uses_configured_model_when_tail_lacks_turn_context() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "model = \"gpt-5\"\n").unwrap();
    let usage = TranscriptUsage {
        context_window: None,
        context_window_reported: false,
        total_tokens: None,
        model: None,
        effort: None,
        last_input_tokens: None,
        last_cached_input_tokens: None,
        last_cache_write_tokens: None,
        last_output_tokens: None,
    };

    let (_tokens, model_id) = with_codex_config_path(&path, || transcript_enrichment(&usage, None));

    assert_eq!(model_id.as_deref(), Some("gpt-5"));
}

#[test]
fn refresh_transcript_context_stat_gate_skips_unchanged_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    let pricing_cache_path = dir.path().join("pricing-cache.json");
    std::fs::write(
        &path,
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\",\"effort\":\"xhigh\"}}\n",
    )
    .unwrap();
    let stat = TranscriptStat::from_path(&path).unwrap();
    let path_string = path.to_string_lossy().into_owned();
    assert!(
        refresh_transcript_context(
            "sess-1",
            None,
            Some(&path_string),
            Some(&stat),
            None,
            &pricing_cache_path,
        )
        .is_none(),
        "unchanged stat skips the tail read and sidecar write"
    );

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
              \"last_token_usage\":{\"input_tokens\":50,\"total_tokens\":60},\
              \"model_context_window\":100}}}\n",
        )
        .unwrap();
    let refresh = refresh_transcript_context(
        "sess-1",
        None,
        Some(&path_string),
        Some(&stat),
        None,
        &pricing_cache_path,
    )
    .expect("changed stat refreshes");
    assert_eq!(
        refresh.context.effort.as_set().map(String::as_str),
        Some("xhigh")
    );
    // The refresh carries the derivation inputs (window + current usage), not a
    // baked percentage — the gauge derives 50% (50 of 100) downstream.
    let tokens = refresh
        .context
        .tokens
        .as_value()
        .expect("changed stat refreshes tokens");
    assert_eq!(tokens.context_window_size, Some(100));
    assert_eq!(
        tokens
            .current_usage
            .as_ref()
            .and_then(|usage| usage.input_tokens),
        Some(50)
    );
    assert_ne!(refresh.transcript_stat, Some(stat));

    let unchanged_stat = TranscriptStat::from_path(&path).unwrap();
    assert!(
        refresh_transcript_context(
            "sess-1",
            None,
            Some(&path_string),
            Some(&unchanged_stat),
            None,
            &pricing_cache_path,
        )
        .is_none(),
        "unchanged stat remains gated regardless of prior effort"
    );

    let no_context = dir.path().join("rollout-no-context.jsonl");
    std::fs::write(
        &no_context,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
          \"last_token_usage\":{\"input_tokens\":50,\"total_tokens\":60},\
          \"model_context_window\":100}}}\n",
    )
    .unwrap();
    let no_context_path = no_context.to_string_lossy().into_owned();
    let refresh = refresh_transcript_context(
        "sess-1",
        None,
        Some(&no_context_path),
        None,
        None,
        &pricing_cache_path,
    )
    .expect("missing stat refreshes");
    assert_eq!(refresh.context.effort, crate::agents::FieldPatch::Keep);
}

#[test]
fn refresh_transcript_context_backfills_unchanged_legacy_fold() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    let pricing_cache_path = dir.path().join("pricing-cache.json");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"turn_context","payload":{"model":"gpt-5.6-sol"}}"#,
            "\n",
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:00.000Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":10,"total_tokens":110},"total_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":10},"model_context_window":272000}}}"#,
            "\n",
        ),
    )
    .unwrap();
    let stat = TranscriptStat::from_path(&path).unwrap();
    let legacy = LocalSpendFold {
        cursor: crate::agents::spending::SpendCursor {
            offset: stat.len,
            ..crate::agents::spending::SpendCursor::default()
        },
        total_usd: 1.0,
        ..LocalSpendFold::default()
    };

    let refresh = refresh_transcript_context(
        "sess-1",
        None,
        Some(path.to_string_lossy().as_ref()),
        Some(&stat),
        Some(&legacy),
        &pricing_cache_path,
    )
    .expect("unchanged legacy fold bypasses the stat gate for one cold backfill");
    let usage = refresh
        .context
        .tokens
        .as_value()
        .and_then(|tokens| tokens.session_usage.as_ref())
        .expect("backfill publishes session usage");

    assert_eq!(usage.input_tokens, Some(60));
    assert_eq!(usage.output_tokens, Some(10));
    assert_eq!(usage.cache_read_input_tokens, Some(40));
    assert_eq!(usage.cache_hit_percent(), Some(40));
    let rebuilt = refresh.spend_fold.into_set().unwrap();
    assert!(
        !live_fold_needs_token_counter_backfill(&rebuilt),
        "the rebuilt fold becomes stat-gateable again"
    );
    assert!(
        refresh_transcript_context(
            "sess-1",
            None,
            Some(path.to_string_lossy().as_ref()),
            Some(&stat),
            Some(&rebuilt),
            &pricing_cache_path,
        )
        .is_none(),
        "the unchanged rebuilt fold returns to the cheap stat-gated path"
    );
}

#[test]
fn refresh_transcript_context_prices_model_from_shared_cache() {
    let dir = tempfile::tempdir().unwrap();
    let transcript_path = dir.path().join("rollout-session.jsonl");
    let pricing_cache_path = dir.path().join("pricing-cache.json");
    std::fs::write(
        &transcript_path,
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-9.9-nova\"}}\n\
         {\"type\":\"event_msg\",\"timestamp\":\"2026-01-01T10:00:00.000Z\",\"payload\":{\"type\":\"token_count\",\"info\":{\
         \"last_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":40,\
         \"output_tokens\":10,\"total_tokens\":110},\
         \"total_token_usage\":{\"input_tokens\":100,\"cached_input_tokens\":40,\
         \"output_tokens\":10},\"model_context_window\":1000}}}\n",
    )
    .unwrap();
    std::fs::write(
        &pricing_cache_path,
        r#"{"schema":4,"models":{"gpt-9.9-nova":{"input":0.000001,"output":0.000002,"cache_read":0.0000005}}}"#,
    )
    .unwrap();

    let refresh = refresh_transcript_context(
        "sess-1",
        None,
        Some(transcript_path.to_string_lossy().as_ref()),
        None,
        None,
        &pricing_cache_path,
    )
    .expect("changed transcript refreshes");

    assert_eq!(
        refresh.context.model_id.as_set().map(String::as_str),
        Some("gpt-9.9-nova")
    );
    assert!(
        refresh
            .context
            .cost
            .into_set()
            .and_then(|cost| cost.total_cost_usd)
            .is_some(),
        "a shared-cache-only model prices the card"
    );
}

#[test]
fn refresh_prices_each_request_before_cumulative_input_crosses_long_tier() {
    let dir = tempfile::tempdir().unwrap();
    let transcript_path = dir.path().join("rollout-session.jsonl");
    let pricing_cache_path = dir.path().join("pricing-cache.json");
    std::fs::write(
        &transcript_path,
        concat!(
            r#"{"type":"turn_context","payload":{"model":"gpt-5.6-sol"}}"#,
            "\n",
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:00.000Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":200000,"cached_input_tokens":150000,"output_tokens":1000,"total_tokens":201000},"total_token_usage":{"input_tokens":200000,"cached_input_tokens":150000,"output_tokens":1000},"model_context_window":272000}}}"#,
            "\n",
            r#"{"type":"event_msg","timestamp":"2026-01-01T10:00:01.000Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":200000,"cached_input_tokens":150000,"output_tokens":1000,"total_tokens":201000},"total_token_usage":{"input_tokens":400000,"cached_input_tokens":300000,"output_tokens":2000},"model_context_window":272000}}}"#,
            "\n",
        ),
    )
    .unwrap();
    PriceBook::write_fixture_cache(&pricing_cache_path);

    let refresh = refresh_transcript_context(
        "sess-1",
        None,
        Some(transcript_path.to_string_lossy().as_ref()),
        None,
        None,
        &pricing_cache_path,
    )
    .expect("changed transcript refreshes");
    let actual = refresh
        .context
        .cost
        .as_set()
        .unwrap()
        .total_cost_usd
        .unwrap();
    let price = PriceBook::fixture().price("gpt-5.6-sol").unwrap();
    let per_request = price.cost_of(TokenSplit::new(50_000, 1_000).cached(0, 150_000));
    let old_cumulative = price.cost_of(TokenSplit::new(100_000, 2_000).cached(0, 300_000));

    assert!((actual - per_request * 2.0).abs() < 1e-12);
    assert!(actual < old_cumulative);
    assert_eq!(refresh.spend_fold.into_set().unwrap().total_usd, actual);
}

#[test]
fn find_session_transcript_walks_codex_date_hierarchy() {
    // Codex shards rollouts under `YYYY/MM/DD/`; the locator finds a file
    // whose name ends with `{session_id}.jsonl` regardless of how deep the
    // shard is.
    let dir = tempfile::tempdir().unwrap();
    let day_dir = dir.path().join("2026").join("05").join("26");
    std::fs::create_dir_all(&day_dir).unwrap();
    let expected = day_dir.join("rollout-2026-05-26T21-57-38-sess-abc.jsonl");
    std::fs::write(&expected, "{}\n").unwrap();
    // A noise file for a different session in the same day must not match.
    std::fs::write(day_dir.join("rollout-other-sess.jsonl"), "{}\n").unwrap();

    let found = find_session_transcript_under(dir.path(), "sess-abc").unwrap();
    assert_eq!(found, expected);
    assert!(find_session_transcript_under(dir.path(), "sess-missing").is_none());
}

#[test]
fn find_session_transcript_prefers_newest_reverted_rollout() {
    let dir = tempfile::tempdir().unwrap();
    let day_dir = dir.path().join("2026").join("05").join("26");
    std::fs::create_dir_all(&day_dir).unwrap();
    let base = day_dir.join("rollout-2026-05-26T21-57-38-sess-abc.jsonl");
    let older_revert = day_dir
        .join("rollout-2026-05-26T21-58-00-sess-abc_019d0000-0000-7000-8000-000000000001.jsonl");
    let newest_revert = day_dir
        .join("rollout-2026-05-26T21-58-00-sess-abc_019d0000-0000-7000-8000-000000000002.jsonl");
    for path in [&base, &older_revert, &newest_revert] {
        std::fs::write(path, "{}\n").unwrap();
    }
    std::fs::write(
        day_dir.join("rollout-2026-05-26T21-59-00-sess-abc-extra.jsonl"),
        "{}\n",
    )
    .unwrap();

    assert_eq!(
        find_session_transcript_under(dir.path(), "sess-abc").as_deref(),
        Some(newest_revert.as_path())
    );
}

#[test]
fn find_session_transcript_falls_back_to_flat_archive() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    let archived = dir.path().join("archived_sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::create_dir_all(&archived).unwrap();
    let expected = archived.join("rollout-2026-05-26T21-57-38-sess-archived.jsonl");
    std::fs::write(&expected, "{}\n").unwrap();

    let found = with_codex_sessions_root(&sessions, || find_session_transcript("sess-archived"));

    assert_eq!(found.as_deref(), Some(expected.as_path()));
}

#[test]
fn session_origin_reads_only_rollout_head_lineage() {
    let dir = tempfile::tempdir().unwrap();
    let day_dir = dir.path().join("2026").join("06").join("26");
    std::fs::create_dir_all(&day_dir).unwrap();
    let write_rollout = |session_id: &str, head: &str| {
        std::fs::write(
            day_dir.join(format!("rollout-2026-06-26T00-00-00-{session_id}.jsonl")),
            format!("{head}\n{{\"type\":\"turn_context\",\"payload\":{{\"model\":\"gpt-5\"}}}}\n"),
        )
        .unwrap();
    };

    write_rollout(
        "fresh",
        r#"{"type":"session_meta","payload":{"id":"fresh"}}"#,
    );
    write_rollout(
        "fork",
        r#"{"type":"session_meta","payload":{"id":"fork","forked_from_id":"fresh"}}"#,
    );
    write_rollout("not-meta", r#"{"type":"turn_context","payload":{}}"#);

    with_codex_sessions_root(dir.path(), || {
        assert_eq!(session_origin("fresh"), Some(SessionOrigin::Fresh));
        assert_eq!(session_origin("fork"), Some(SessionOrigin::Forked));
        assert_eq!(session_forked_from("fresh"), None);
        assert_eq!(session_forked_from("fork").as_deref(), Some("fresh"));
        assert_eq!(session_origin("not-meta"), None);
        assert_eq!(session_origin("missing"), None);
    });
}

#[test]
fn rollout_header_normalizes_v2_and_structured_child_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let v2 = dir.path().join("v2.jsonl");
    std::fs::write(
        &v2,
        r#"{"timestamp":"2026-06-26T00:00:00Z","type":"session_meta","payload":{"id":"child-v2","cwd":"/workspace/project","thread_source":"subagent","parent_thread_id":"root","agent_nickname":"Atlas","agent_path":"//root//research/explore_hooks/","agent_role":"explorer","multi_agent_version":"v2"}}"#,
    )
    .unwrap();
    let header = read_rollout_header(&v2).unwrap();
    assert!(header.is_subagent);
    assert_eq!(header.session_id.as_deref(), Some("child-v2"));
    assert_eq!(header.parent_thread_id.as_deref(), Some("root"));
    assert_eq!(header.agent_nickname.as_deref(), Some("Atlas"));
    assert_eq!(header.agent_path.as_deref(), Some("research/explore_hooks"));
    assert_eq!(header.agent_role.as_deref(), Some("explorer"));
    assert_eq!(header.multi_agent_version.as_deref(), Some("v2"));

    let structured = dir.path().join("structured.jsonl");
    std::fs::write(
        &structured,
        r#"{"timestamp":1735689600000,"type":"session_meta","payload":{"id":"child-v1","source":{"subagent":{"thread_spawn":{"parent_thread_id":"nested-parent","depth":2,"agent_path":"/root/research/worker","agent_nickname":"Scout","agent_role":"default"}}}}}"#,
    )
    .unwrap();
    let header = read_rollout_header(&structured).unwrap();
    assert!(header.is_subagent);
    assert_eq!(header.parent_thread_id.as_deref(), Some("nested-parent"));
    assert_eq!(header.depth, Some(2));
    assert_eq!(header.agent_path.as_deref(), Some("research/worker"));

    let root_only = dir.path().join("root-only.jsonl");
    std::fs::write(
        &root_only,
        r#"{"type":"session_meta","payload":{"id":"root-only","thread_source":"subagent","agent_path":"/root//"}}"#,
    )
    .unwrap();
    assert_eq!(read_rollout_header(&root_only).unwrap().agent_path, None);
}
