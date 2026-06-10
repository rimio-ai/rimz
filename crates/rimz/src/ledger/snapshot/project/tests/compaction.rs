use super::*;

fn lifecycle(source: &str, event_name: &str, signal: serde_json::Value) -> EventEnvelope {
    raw_lifecycle(
        source,
        serde_json::json!({
            "event_name": event_name,
            "agent_id": "sess-1",
            "signal": signal,
        }),
    )
}

fn lifecycle_at(
    source: &str,
    secs_after_epoch: i64,
    event_name: &str,
    signal: serde_json::Value,
) -> EventEnvelope {
    raw_lifecycle_at(
        source,
        secs_after_epoch,
        serde_json::json!({
            "event_name": event_name,
            "agent_id": "sess-1",
            "signal": signal,
        }),
    )
}

fn signal(name: &str) -> serde_json::Value {
    serde_json::json!({ "signal": name })
}

fn compaction_ended(auto: Option<bool>) -> serde_json::Value {
    let mut signal = signal("compaction_ended");
    if let Some(auto) = auto {
        signal["auto"] = serde_json::Value::Bool(auto);
    }
    signal
}

fn tool_used() -> serde_json::Value {
    serde_json::json!({ "signal": "tool_used", "mutates": true, "edits": true })
}

#[test]
fn compaction_end_clears_marker_and_counts_completed_brackets() {
    let prompt = lifecycle("claude", "UserPromptSubmit", signal("turn_started"));
    let compact = lifecycle("claude", "PreCompact", signal("compacting"));
    let post = lifecycle("claude", "PostCompact", compaction_ended(Some(false)));
    let next_prompt = lifecycle("claude", "UserPromptSubmit", signal("turn_started"));
    let second_compact = lifecycle("claude", "PreCompact", signal("compacting"));
    let second_post = lifecycle("claude", "PostCompact", compaction_ended(Some(true)));

    let agents = reduce_agent_states(&[prompt.clone(), compact.clone(), post.clone()]);
    assert_eq!(agents[0].status, AgentStatus::Idle);
    assert_eq!(agents[0].phase, TurnPhase::Idle);
    assert!(agents[0].compacting_since.is_none());

    for (label, events, expected_count) in [
        (
            "one completed bracket",
            vec![prompt.clone(), compact.clone(), post.clone()],
            1,
        ),
        (
            "non-compaction lifecycle events carry the count forward",
            vec![
                prompt.clone(),
                compact.clone(),
                post.clone(),
                next_prompt.clone(),
            ],
            1,
        ),
        (
            "two completed brackets",
            vec![
                prompt,
                compact,
                post,
                next_prompt,
                second_compact,
                second_post,
            ],
            2,
        ),
    ] {
        assert_eq!(
            reduce_agent_states(&events)[0].compaction_count,
            expected_count,
            "{label}",
        );
    }
}

#[test]
fn compaction_end_resumes_interrupted_turn_for_auto_or_unknown_edges() {
    for (source, prompt_event, edit_event, compact_event, post_event, end_signal) in [
        (
            "codex",
            "UserPromptSubmit",
            "PostToolUse",
            "PreCompact",
            "PostCompact",
            compaction_ended(Some(true)),
        ),
        (
            "pi",
            "before_agent_start",
            "tool_execution_end",
            "session_before_compact",
            "session_compact",
            compaction_ended(None),
        ),
    ] {
        let prompt = lifecycle(source, prompt_event, signal("turn_started"));
        let edit = lifecycle(source, edit_event, tool_used());
        let compact = lifecycle(source, compact_event, signal("compacting"));
        let post = lifecycle(source, post_event, end_signal);

        let agents = reduce_agent_states(&[prompt, edit, compact, post]);
        assert_eq!(agents[0].status, AgentStatus::Running, "{source}");
        assert_eq!(agents[0].phase, TurnPhase::Acting, "{source}");
        assert!(agents[0].compacting_since.is_none(), "{source}");
    }
}

#[test]
fn next_lifecycle_signal_closes_missed_compaction_bracket() {
    for (label, next) in [
        (
            "next tool signal",
            lifecycle(
                "claude",
                "PreToolUse",
                serde_json::json!({ "signal": "tool_used", "mutates": false, "edits": false }),
            ),
        ),
        (
            "expired display marker",
            lifecycle_at(
                "claude",
                crate::feed::COMPACTING_WINDOW_SECS + 5,
                "Stop",
                serde_json::json!({
                    "signal": "turn_ended",
                    "errored": false,
                    "parked_on_background": false
                }),
            ),
        ),
    ] {
        let prompt = lifecycle_at("claude", 0, "UserPromptSubmit", signal("turn_started"));
        let compact = lifecycle_at("claude", 1, "PreCompact", signal("compacting"));
        let agents = reduce_agent_states(&[prompt, compact, next]);

        assert_eq!(agents[0].compaction_count, 1, "{label}");
        assert!(agents[0].compacting_since.is_none(), "{label}");
    }
}

#[test]
fn repeated_compacting_signal_refreshes_marker_without_counting() {
    let prompt = lifecycle_at("claude", 0, "UserPromptSubmit", signal("turn_started"));
    let compact = lifecycle_at("claude", 1, "PreCompact", signal("compacting"));
    let second_at = crate::feed::COMPACTING_WINDOW_SECS + 5;
    let later_second_compact =
        lifecycle_at("claude", second_at, "PreCompact", signal("compacting"));

    let agents = reduce_agent_states(&[prompt, compact, later_second_compact]);
    assert_eq!(agents[0].compaction_count, 0);
    assert_eq!(
        agents[0].compacting_since,
        Some(Timestamp::from_second(epoch().as_second() + second_at).unwrap())
    );
    assert_eq!(agents[0].status, AgentStatus::Running);
}

#[test]
fn double_compaction_end_events_count_once_in_either_order() {
    let prompt = lifecycle("codex", "UserPromptSubmit", signal("turn_started"));
    let compact = lifecycle("codex", "PreCompact", signal("compacting"));
    let post = lifecycle("codex", "PostCompact", compaction_ended(Some(true)));
    let session_compact = lifecycle("codex", "SessionStart", compaction_ended(None));

    for events in [
        vec![
            prompt.clone(),
            compact.clone(),
            post.clone(),
            session_compact.clone(),
        ],
        vec![prompt, compact, session_compact, post],
    ] {
        let agents = reduce_agent_states(&events);
        assert_eq!(agents[0].compaction_count, 1);
        assert!(agents[0].compacting_since.is_none());
        assert_eq!(agents[0].status, AgentStatus::Running);
    }
}

#[test]
fn unbracketed_manual_compaction_end_applies_edge_without_counting() {
    let prompt = lifecycle("claude", "UserPromptSubmit", signal("turn_started"));
    let manual_end = lifecycle("claude", "PostCompact", compaction_ended(Some(false)));

    let agents = reduce_agent_states(&[prompt, manual_end]);
    assert_eq!(agents[0].status, AgentStatus::Idle);
    assert_eq!(agents[0].compaction_count, 0);
    assert!(agents[0].compacting_since.is_none());
}

#[test]
fn turn_started_at_is_stamped_when_progress_reconciles_a_turn_open() {
    let registered = lifecycle_at("claude", 0, "SessionStart", signal("registered"));
    let tool = lifecycle_at("claude", 5, "PostToolUse", tool_used());

    let agents = reduce_agent_states(&[registered, tool]);
    assert_eq!(
        agents[0].turn_started_at,
        Some(Timestamp::from_second(epoch().as_second() + 5).unwrap())
    );
}
