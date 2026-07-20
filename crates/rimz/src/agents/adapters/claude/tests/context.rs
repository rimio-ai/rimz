use super::*;
use crate::agents::testkit::{hook_lifecycle, hook_output};

#[test]
fn transcript_tail_drives_context_window_and_tokens() {
    // Claude reports token usage only in the transcript JSONL; the Stop hook
    // reads its tail for the gauge numerator. A bare model id carries no `[1m]`
    // marker, so the adapter asserts no window — the fold applies the 200k
    // definition default (100k of 200k = 50%).
    let dir = tempfile::tempdir().unwrap();
    let bare = dir.path().join("bare.jsonl");
    std::fs::write(
            &bare,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\"}}\n{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-7\",\"usage\":{\"input_tokens\":100000,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0,\"output_tokens\":500}}}\n",
        )
        .unwrap();
    let obs = hook_lifecycle(
        &ClaudeAdapter,
        "Stop",
        &json!({ "session_id": "sess-1", "transcript_path": bare.to_str().unwrap() }),
    );
    assert_eq!(obs.usage.total_tokens, Some(100_500));
    assert_eq!(obs.usage.context_window, None);
    assert_eq!(obs.launch.model.as_deref(), Some("claude-opus-4-7"));
    // The total is a sum; the components survive it so the card can show where
    // the window went rather than one opaque figure.
    assert_eq!(obs.usage.fresh_input_tokens, Some(100_000));
    assert_eq!(obs.usage.cache_read_input_tokens, Some(0));
    assert_eq!(obs.usage.cache_write_input_tokens, Some(0));
    assert_eq!(obs.usage.output_tokens, Some(500));

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
    let obs = hook_lifecycle(
        &ClaudeAdapter,
        "Stop",
        &json!({
            "session_id": "sess-1",
            "model": "claude-opus-4-8[1m]",
            "transcript_path": extended.to_str().unwrap(),
        }),
    );
    assert_eq!(obs.usage.context_window, Some(1_000_000));
    assert_eq!(obs.usage.total_tokens, Some(100_500));
    assert_eq!(obs.launch.model.as_deref(), Some("claude-opus-4-8[1m]"));
}

#[test]
fn stop_hook_reads_turn_error_from_the_transcript_path() {
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
        .decode_hook(
            "Stop",
            &json!({
                "session_id": "sess-1",
                "transcript_path": transcript.to_str().unwrap(),
            }),
        )
        .expect("Stop decodes")
        .turn_error()
        .cloned()
        .expect("the dead turn is detected");
    assert_eq!(error.class, TurnErrorClass::PausedOverloaded);
    assert_eq!(error.label.as_deref(), Some("API Error: Overloaded"));

    assert!(
        ClaudeAdapter
            .decode_hook("Stop", &json!({ "session_id": "sess-1" }))
            .expect("Stop decodes")
            .turn_error()
            .cloned()
            .is_none(),
        "no transcript path, no marker"
    );
    assert!(
        ClaudeAdapter
            .decode_hook(
                "Stop",
                &json!({
                    "session_id": "sess-1",
                    "transcript_path": dir.path().join("gone.jsonl").to_str().unwrap(),
                }),
            )
            .expect("Stop decodes")
            .turn_error()
            .cloned()
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
        ClaudeAdapter
            .observe_context(
                "claude",
                &json!({
                    "session_id": "sess-1",
                    "transcript_path": transcript.to_str().unwrap(),
                }),
            )
            .and_then(|observation| observation.context.settle.map(|settle| settle.at)),
        Some("2026-06-04T03:01:00Z".parse::<Timestamp>().unwrap())
    );
    assert!(
        ClaudeAdapter
            .observe_context("claude", &json!({ "session_id": "sess-1" }))
            .is_some_and(|observation| observation.context.settle.is_none())
    );
}

#[test]
fn stop_failure_hook_maps_to_turn_error_marker() {
    let marker = |error: &str| {
        hook_output(
            &ClaudeAdapter,
            "StopFailure",
            &json!({
                "session_id": "sess-1",
                "error": error,
                "last_assistant_message": "You've hit your usage limit"
            }),
        )
        .turn_error()
        .cloned()
        .expect("marker")
    };

    assert_eq!(marker("rate_limit").class, TurnErrorClass::PausedRateLimit);
    assert_eq!(marker("overloaded").class, TurnErrorClass::PausedOverloaded);

    let transient = hook_output(
        &ClaudeAdapter,
        "StopFailure",
        &json!({
            "session_id": "sess-1",
            "error": "api_error",
            "last_assistant_message": "API Error: Server Error"
        }),
    )
    .turn_error()
    .cloned()
    .expect("marker");
    assert_eq!(transient.class, TurnErrorClass::PausedOverloaded);
    assert_eq!(transient.label.as_deref(), Some("API Error: Server Error"));

    let failed = hook_output(
        &ClaudeAdapter,
        "StopFailure",
        &json!({
            "session_id": "sess-1",
            "error": "api_error",
            "last_assistant_message": "API Error: Bad Request"
        }),
    )
    .turn_error()
    .cloned()
    .expect("marker");
    assert_eq!(failed.class, TurnErrorClass::Failed);
    assert_eq!(failed.label.as_deref(), Some("API Error: Bad Request"));

    assert!(
        hook_output(
            &ClaudeAdapter,
            "StopFailure",
            &json!({ "session_id": "sess-1" })
        )
        .turn_error()
        .cloned()
        .is_none(),
        "missing error has no marker"
    );
    assert!(
        hook_output(
            &ClaudeAdapter,
            "Stop",
            &json!({
                "session_id": "sess-1",
                "error": "rate_limit"
            })
        )
        .turn_error()
        .cloned()
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
    let obs = hook_lifecycle(
        &ClaudeAdapter,
        "SessionStart",
        &json!({ "session_id": "sess-1", "transcript_path": fresh.to_str().unwrap() }),
    );
    assert_eq!(obs.usage.total_tokens, Some(0));
    assert_eq!(obs.usage.context_window, None);
    // The zero is a gauge baseline for the total only. Claiming four confident
    // zeroes for the split would be asserting a fact no record carries.
    assert_eq!(obs.usage.fresh_input_tokens, None);
    assert_eq!(obs.usage.cache_read_input_tokens, None);
    assert_eq!(obs.usage.cache_write_input_tokens, None);
    assert_eq!(obs.usage.output_tokens, None);

    // No readable transcript means unknown (None), not a false 0%.
    let obs = hook_lifecycle(
        &ClaudeAdapter,
        "SessionStart",
        &json!({ "session_id": "sess-1", "transcript_path": "/nonexistent/session.jsonl" }),
    );
    assert_eq!(obs.usage.total_tokens, None);
    assert_eq!(obs.usage.context_window, None);

    // Transcript reads are keyed by the agent's own session identity: a path
    // with no session id stays unknown even when the file carries usage.
    let usage = dir.path().join("usage.jsonl");
    std::fs::write(
        &usage,
        "{\"message\":{\"model\":\"claude-opus-4-7\",\"usage\":\
             {\"input_tokens\":100000,\"output_tokens\":500}}}\n",
    )
    .unwrap();
    let obs = hook_lifecycle(
        &ClaudeAdapter,
        "SessionStart",
        &json!({ "transcript_path": usage.to_str().unwrap() }),
    );
    assert_eq!(obs.usage.total_tokens, None);
    assert_eq!(obs.usage.context_window, None);
}

#[test]
fn subagent_usage_comes_from_the_child_transcript_not_the_parents() {
    // A SubagentStop payload names both transcripts. Reading the parent's would
    // stamp the parent's model and token total onto the child's row, which is
    // how a cheap Haiku child inherits its Opus parent's figures.
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent.jsonl");
    std::fs::write(
        &parent,
        "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":\
             {\"input_tokens\":900000,\"output_tokens\":1000}}}\n",
    )
    .unwrap();
    let child = dir.path().join("child.jsonl");
    std::fs::write(
        &child,
        "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-haiku-4-5\",\"usage\":\
             {\"input_tokens\":10,\"cache_read_input_tokens\":40,\"output_tokens\":7}}}\n",
    )
    .unwrap();
    let obs = hook_lifecycle(
        &ClaudeAdapter,
        "SubagentStop",
        &json!({
            "session_id": "parent-sess",
            "agent_id": "child-sess",
            "agent_type": "Explore",
            "transcript_path": parent.to_str().unwrap(),
            "agent_transcript_path": child.to_str().unwrap(),
        }),
    );
    assert_eq!(obs.usage.total_tokens, Some(57));
    assert_eq!(obs.usage.fresh_input_tokens, Some(10));
    assert_eq!(obs.usage.cache_read_input_tokens, Some(40));
    assert_eq!(obs.usage.output_tokens, Some(7));
    assert_eq!(obs.launch.model.as_deref(), Some("claude-haiku-4-5"));
}

#[test]
fn subagent_without_its_own_transcript_reports_unknown_usage() {
    // No child transcript means unknown, never the parent's figures borrowed.
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("parent-only.jsonl");
    std::fs::write(
        &parent,
        "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":\
             {\"input_tokens\":900000,\"output_tokens\":1000}}}\n",
    )
    .unwrap();
    let obs = hook_lifecycle(
        &ClaudeAdapter,
        "SubagentStart",
        &json!({
            "session_id": "parent-sess",
            "agent_id": "child-sess",
            "agent_type": "Explore",
            "transcript_path": parent.to_str().unwrap(),
        }),
    );
    assert_eq!(obs.usage.total_tokens, None);
    assert_eq!(obs.usage.fresh_input_tokens, None);
    assert_eq!(obs.launch.model.as_deref(), None);
}

#[test]
fn transcript_tail_splits_a_cache_heavy_turn() {
    // The steady state of a warm session: almost all context is cache reads,
    // and `input_tokens` is a couple of fresh tokens. Reporting only the sum
    // makes a 250k-token card look like a runaway agent; the split shows the
    // window is cache reuse, which is the figure a human acts on.
    let dir = tempfile::tempdir().unwrap();
    let warm = dir.path().join("warm.jsonl");
    std::fs::write(
        &warm,
        "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":\
             {\"input_tokens\":2,\"cache_read_input_tokens\":226584,\
              \"cache_creation_input_tokens\":7853,\"output_tokens\":491}}}\n",
    )
    .unwrap();
    let obs = hook_lifecycle(
        &ClaudeAdapter,
        "Stop",
        &json!({ "session_id": "sess-1", "transcript_path": warm.to_str().unwrap() }),
    );
    assert_eq!(obs.usage.fresh_input_tokens, Some(2));
    assert_eq!(obs.usage.cache_read_input_tokens, Some(226_584));
    assert_eq!(obs.usage.cache_write_input_tokens, Some(7_853));
    assert_eq!(obs.usage.output_tokens, Some(491));
    // The components still reconcile against the total the gauge scales.
    assert_eq!(obs.usage.total_tokens, Some(2 + 226_584 + 7_853 + 491));
}
