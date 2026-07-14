use super::*;

#[test]
fn transcript_tail_drives_context_window_and_tokens() {
    // Claude reports token usage only in the transcript JSONL; the Stop hook
    // reads its tail for the gauge numerator. A bare model id carries no `[1m]`
    // marker, so the adapter asserts no window — the fold applies the 200k
    // descriptor default (100k of 200k = 50%).
    let dir = tempfile::tempdir().unwrap();
    let bare = dir.path().join("bare.jsonl");
    std::fs::write(
            &bare,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\"}}\n{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-7\",\"usage\":{\"input_tokens\":100000,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0,\"output_tokens\":500}}}\n",
        )
        .unwrap();
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "Stop",
            &json!({ "session_id": "sess-1", "transcript_path": bare.to_str().unwrap() }),
        )
        .unwrap();
    assert_eq!(obs.total_tokens, Some(100_500));
    assert_eq!(obs.context_window, None);
    assert_eq!(obs.launch.model.as_deref(), Some("claude-opus-4-7"));

    // The 1M beta is signalled by a `[1m]` marker that rides only the hook
    // payload's model field — the transcript writes the bare id. The adapter
    // asserts the 1M window from the payload-resolved model (100k of 1M = 10%,
    // where the bare-id 200k default would over-read it as 50%).
    let extended = dir.path().join("extended.jsonl");
    std::fs::write(
            &extended,
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":100000,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0,\"output_tokens\":500}}}\n",
        )
        .unwrap();
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "Stop",
            &json!({
                "session_id": "sess-1",
                "model": "claude-opus-4-8[1m]",
                "transcript_path": extended.to_str().unwrap(),
            }),
        )
        .unwrap();
    assert_eq!(obs.context_window, Some(1_000_000));
    assert_eq!(obs.total_tokens, Some(100_500));
    assert_eq!(obs.launch.model.as_deref(), Some("claude-opus-4-8[1m]"));
}

#[test]
fn observe_turn_error_reads_the_tail_from_the_payload_path() {
    // End-to-end over the real file path: the statusline payload names the
    // transcript, the adapter reads its bounded tail, and the verified
    // incident shape (flagged assistant entry + turn_duration, no Stop)
    // yields the marker. A missing path or file yields None, never an error.
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("session.jsonl");
    std::fs::write(
            &transcript,
            concat!(
                "{\"type\":\"assistant\",\"isApiErrorMessage\":true,\"timestamp\":\"2026-06-04T02:56:32.919Z\",",
                "\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"API Error: Overloaded\"}]}}\n",
                "{\"type\":\"system\",\"subtype\":\"turn_duration\",\"timestamp\":\"2026-06-04T02:56:32.923Z\"}\n",
            ),
        )
        .unwrap();
    let error = ClaudeAdapter
        .observe_turn_error(&json!({
            "session_id": "sess-1",
            "transcript_path": transcript.to_str().unwrap(),
        }))
        .expect("the dead turn is detected");
    assert_eq!(error.class, TurnErrorClass::PausedOverloaded);
    assert_eq!(error.label.as_deref(), Some("API Error: Overloaded"));

    assert!(
        ClaudeAdapter
            .observe_turn_error(&json!({ "session_id": "sess-1" }))
            .is_none(),
        "no transcript path, no marker"
    );
    assert!(
        ClaudeAdapter
            .observe_turn_error(&json!({
                "session_id": "sess-1",
                "transcript_path": dir.path().join("gone.jsonl").to_str().unwrap(),
            }))
            .is_none(),
        "an unreadable transcript degrades to no marker"
    );
}

#[test]
fn turn_interrupted_reads_the_tail_from_the_payload_path() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            "{\"type\":\"user\",\"timestamp\":\"2026-06-04T03:01:00.000Z\",",
            "\"message\":{\"content\":\"[Request interrupted by user]\"}}\n",
            "{\"type\":\"system\",\"subtype\":\"turn_duration\"}\n",
        ),
    )
    .unwrap();

    assert_eq!(
        ClaudeAdapter.turn_interrupted(&json!({
            "session_id": "sess-1",
            "transcript_path": transcript.to_str().unwrap(),
        })),
        Some("2026-06-04T03:01:00Z".parse::<Timestamp>().unwrap())
    );
    assert!(
        ClaudeAdapter
            .turn_interrupted(&json!({ "session_id": "sess-1" }))
            .is_none()
    );
}

#[test]
fn stop_failure_hook_maps_to_turn_error_marker() {
    let marker = |error: &str| {
        ClaudeAdapter
            .observe_turn_error_from_hook(
                "StopFailure",
                &json!({
                    "session_id": "sess-1",
                    "error": error,
                    "last_assistant_message": "You've hit your usage limit"
                }),
            )
            .expect("marker")
    };

    assert_eq!(marker("rate_limit").class, TurnErrorClass::PausedRateLimit);
    assert_eq!(marker("overloaded").class, TurnErrorClass::PausedOverloaded);

    let transient = ClaudeAdapter
        .observe_turn_error_from_hook(
            "StopFailure",
            &json!({
                "session_id": "sess-1",
                "error": "api_error",
                "last_assistant_message": "API Error: Server Error"
            }),
        )
        .expect("marker");
    assert_eq!(transient.class, TurnErrorClass::PausedOverloaded);
    assert_eq!(transient.label.as_deref(), Some("API Error: Server Error"));

    let failed = ClaudeAdapter
        .observe_turn_error_from_hook(
            "StopFailure",
            &json!({
                "session_id": "sess-1",
                "error": "api_error",
                "last_assistant_message": "API Error: Bad Request"
            }),
        )
        .expect("marker");
    assert_eq!(failed.class, TurnErrorClass::Failed);
    assert_eq!(failed.label.as_deref(), Some("API Error: Bad Request"));

    assert!(
        ClaudeAdapter
            .observe_turn_error_from_hook("StopFailure", &json!({ "session_id": "sess-1" }))
            .is_none(),
        "missing error has no marker"
    );
    assert!(
        ClaudeAdapter
            .observe_turn_error_from_hook(
                "Stop",
                &json!({
                    "session_id": "sess-1",
                    "error": "rate_limit"
                }),
            )
            .is_none(),
        "only StopFailure carries this marker"
    );
}

#[test]
fn transcript_usage_absent_reports_zero_or_unknown() {
    let dir = tempfile::tempdir().unwrap();

    // A brand-new session has a transcript with no assistant usage yet: it
    // reports an explicit zero numerator (Some(0)), not None, so the fold draws
    // an empty (0%) gauge for a just-launched idle agent instead of hiding it.
    let fresh = dir.path().join("fresh.jsonl");
    std::fs::write(
        &fresh,
        "{\"type\":\"user\",\"message\":{\"role\":\"user\"}}\n",
    )
    .unwrap();
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "SessionStart",
            &json!({ "session_id": "sess-1", "transcript_path": fresh.to_str().unwrap() }),
        )
        .unwrap();
    assert_eq!(obs.total_tokens, Some(0));
    assert_eq!(obs.context_window, None);

    // No readable transcript means unknown (None), not a false 0%.
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "SessionStart",
            &json!({ "session_id": "sess-1", "transcript_path": "/nonexistent/session.jsonl" }),
        )
        .unwrap();
    assert_eq!(obs.total_tokens, None);
    assert_eq!(obs.context_window, None);

    // Transcript reads are keyed by the agent's own session identity: a path
    // with no session id stays unknown even when the file carries usage.
    let usage = dir.path().join("usage.jsonl");
    std::fs::write(
        &usage,
        "{\"message\":{\"model\":\"claude-opus-4-7\",\"usage\":\
             {\"input_tokens\":100000,\"output_tokens\":500}}}\n",
    )
    .unwrap();
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "SessionStart",
            &json!({ "transcript_path": usage.to_str().unwrap() }),
        )
        .unwrap();
    assert_eq!(obs.total_tokens, None);
    assert_eq!(obs.context_window, None);
}
