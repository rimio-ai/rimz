use super::*;

#[test]
fn usage_from_transcript_reads_split_totals_and_separates_zero_from_unknown() {
    let dir = tempfile::tempdir().unwrap();

    // Codex reports token usage only in the rollout JSONL. A full `token_count`
    // event carries the model, the latest-call split, the cumulative billing
    // totals, and the model context window.
    let full = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &full,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"sess-1\"}}\n\
             {\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.5\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":\
             {\"last_token_usage\":{\"input_tokens\":129200,\"cached_input_tokens\":120000,\
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
    assert_eq!(usage.last_input_tokens, Some(129_200));
    assert_eq!(usage.last_cached_input_tokens, Some(120_000));
    assert_eq!(usage.last_output_tokens, Some(800));
    assert_eq!(usage.cumulative_input_tokens, Some(1000));
    assert_eq!(usage.cumulative_output_tokens, Some(200));
    assert_eq!(usage.cumulative_cached_tokens, 400);

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
    assert_eq!(usage.last_input_tokens, Some(0));
    assert_eq!(usage.last_cached_input_tokens, Some(0));
    assert_eq!(usage.last_output_tokens, Some(0));

    // An unreadable rollout is unknown, not zero — the gauge stays hidden.
    let usage = usage_from_transcript(Path::new("/nonexistent/path/rollout.jsonl"));
    assert_eq!(usage.context_window, None);
    assert_eq!(usage.total_tokens, None);

    // An older `last_token_usage` carrying only input + total leaves the
    // cached/output sides and the cumulative totals unknown rather than zero,
    // so no spurious cost estimate is produced.
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
    assert_eq!(usage.last_output_tokens, None);
    assert_eq!(usage.cumulative_input_tokens, None);
    assert_eq!(usage.cumulative_output_tokens, None);
    assert_eq!(usage.cumulative_cached_tokens, 0);
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
    // The rollout wrapper still supplies the timestamp Rimz needs for
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
    assert_eq!(
        class_from_kind("internalServerError", "API Error: Server Error"),
        crate::agents::TurnErrorClass::PausedOverloaded
    );
    for kind in [
        "contextWindowExceeded",
        "unauthorized",
        "badRequest",
        "sandboxError",
        "cyberPolicy",
        "threadRollbackFailed",
        "other",
    ] {
        assert_eq!(
            class_from_kind(kind, "API Error: Bad Request"),
            crate::agents::TurnErrorClass::Failed,
            "{kind}"
        );
    }

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
    let task_complete = json!({"timestamp":"2026-06-14T05:59:49.268Z","type":"event_msg","payload":{"type":"task_complete","last_agent_message":null}});
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
    // empty-or-absent `error` (`null`/`false`/`""`/`{}`) is too ambiguous to
    // claim success over — only a record with no `error` field at all settles.
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
fn transcript_enrichment_maps_split_to_rich_usage_and_prices_cumulative_totals() {
    // The latest-call split maps onto `current_usage` (cached slice removed from
    // the input numerator), with no baked percentage — the gauge derives it from
    // `current_usage` over the window downstream. No cumulative totals → no cost.
    let split = TranscriptUsage {
        context_window: Some(10_000),
        context_window_reported: true,
        total_tokens: Some(4_200),
        model: Some("gpt-5".to_owned()),
        last_input_tokens: Some(1_200),
        last_cached_input_tokens: Some(1_000),
        last_output_tokens: Some(80),
        cumulative_input_tokens: None,
        cumulative_cached_tokens: 0,
        cumulative_output_tokens: None,
    };
    let (tokens, cost, model_id) = transcript_enrichment(&split, None);
    let tokens = tokens.expect("tokens are mapped");
    let current = tokens.current_usage.expect("current usage is mapped");
    assert_eq!(tokens.context_window_size, Some(10_000));
    assert_eq!(tokens.used_percentage, None);
    assert_eq!(tokens.remaining_percentage, None);
    assert_eq!(current.input_tokens, Some(200));
    assert_eq!(current.cache_read_input_tokens, Some(1_000));
    assert_eq!(current.cache_creation_input_tokens, None);
    assert_eq!(current.output_tokens, Some(80));
    assert_eq!(
        current.input_tokens.unwrap()
            + current.cache_read_input_tokens.unwrap()
            + current.cache_creation_input_tokens.unwrap_or(0),
        split.last_input_tokens.unwrap(),
        "rich context numerator matches the row-level fallback"
    );
    assert_eq!(cost, None);
    assert_eq!(model_id.as_deref(), Some("gpt-5"));

    // Cumulative totals price against the known model — taken from the usage
    // record, or from the prior model hint when the tail carries no turn_context.
    let price = PriceBook::embedded().price("gpt-5").unwrap();
    let expected = 600.0 * price.input + 400.0 * price.cache_read + 200.0 * price.output;
    let cumulative = |model: Option<&str>| TranscriptUsage {
        context_window: None,
        context_window_reported: false,
        total_tokens: None,
        model: model.map(ToOwned::to_owned),
        last_input_tokens: None,
        last_cached_input_tokens: None,
        last_output_tokens: None,
        cumulative_input_tokens: Some(1_000),
        cumulative_cached_tokens: 400,
        cumulative_output_tokens: Some(200),
    };
    for (usage, hint, label) in [
        (cumulative(Some("gpt-5")), None, "known model from usage"),
        (cumulative(None), Some("gpt-5"), "prior model hint"),
    ] {
        let (_tokens, cost, model_id) = transcript_enrichment(&usage, hint);
        let cost = cost
            .and_then(|cost| cost.total_cost_usd)
            .unwrap_or_else(|| panic!("{label} prices cumulative totals"));
        assert!((cost - expected).abs() < f64::EPSILON, "{label}");
        assert_eq!(model_id.as_deref(), Some("gpt-5"), "{label}");
    }
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
        last_input_tokens: None,
        last_cached_input_tokens: None,
        last_output_tokens: None,
        cumulative_input_tokens: Some(1_000),
        cumulative_cached_tokens: 400,
        cumulative_output_tokens: Some(200),
    };

    let (_tokens, cost, model_id) =
        with_codex_config_path(&path, || transcript_enrichment(&usage, None));

    assert_eq!(model_id.as_deref(), Some("gpt-5"));
    assert!(
        cost.and_then(|cost| cost.total_cost_usd).is_some(),
        "configured model prices cumulative totals"
    );
}

#[test]
fn refresh_transcript_context_stat_gate_skips_unchanged_tail_but_stale_effort_reruns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n",
    )
    .unwrap();
    let stat = transcript_stat(&path).unwrap();
    let path_string = path.to_string_lossy().into_owned();
    assert!(
        refresh_transcript_context("sess-1", None, None, Some(&path_string), Some(&stat)).is_none(),
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
    let refresh = refresh_transcript_context("sess-1", None, None, Some(&path_string), Some(&stat))
        .expect("changed stat refreshes");
    // The refresh carries the derivation inputs (window + current usage), not a
    // baked percentage — the gauge derives 50% (50 of 100) downstream.
    let tokens = refresh
        .tokens
        .as_ref()
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

    let unchanged_stat = transcript_stat(&path).unwrap();
    let refresh = refresh_transcript_context(
        "sess-1",
        None,
        Some("medium"),
        Some(&path_string),
        Some(&unchanged_stat),
    )
    .expect("stale prior effort forces a local refresh despite unchanged stat");
    assert_eq!(
        refresh
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.context_window_size),
        Some(100)
    );
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
        assert_eq!(session_origin("not-meta"), None);
        assert_eq!(session_origin("missing"), None);
    });
}
