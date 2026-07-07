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

fn lifecycle_for_agent(
    agent_id: &str,
    event_name: &str,
    signal: serde_json::Value,
) -> EventEnvelope {
    raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": event_name,
            "agent_id": agent_id,
            "signal": signal,
        }),
    )
}

#[test]
fn compacting_for_unknown_session_folds_to_nothing() {
    let compact = lifecycle_for_agent("fresh-compact", "PreCompact", signal("compacting"));

    assert!(reduce_agent_states(&[compact]).is_empty());
}

#[test]
fn compaction_ended_for_unknown_session_folds_to_nothing() {
    let post = lifecycle_for_agent("fresh-compact", "SessionStart", compaction_ended(None));

    assert!(reduce_agent_states(&[post]).is_empty());
}

#[test]
fn compacting_after_registration_still_stamps_the_head() {
    let registered = lifecycle_for_agent("session-a", "SessionStart", signal("registered"));
    let compact = lifecycle_for_agent("session-a", "PreCompact", signal("compacting"));

    let agents = reduce_agent_states(&[registered, compact]);

    assert_eq!(agents.len(), 1);
    assert!(agents[0].compacting_since.is_some());
}

#[test]
fn aborted_compaction_rotation_does_not_create_a_ghost_session() {
    let original = lifecycle_for_agent("session-a", "SessionStart", signal("registered"));
    let aborted_rotation = lifecycle_for_agent("session-b", "PreCompact", signal("compacting"));
    let replacement = lifecycle_for_agent("session-c", "SessionStart", signal("registered"));

    let agents = reduce_agent_states(&[original, aborted_rotation, replacement]);

    assert_eq!(agents.len(), 2);
    assert!(agents.iter().any(|agent| agent.agent_id == "session-a"));
    assert!(agents.iter().any(|agent| agent.agent_id == "session-c"));
    assert!(agents.iter().all(|agent| agent.agent_id != "session-b"));
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
                crate::agents::COMPACTING_WINDOW_SECS + 5,
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
fn turn_started_at_is_stamped_when_progress_reconciles_a_turn_open() {
    let registered = lifecycle_at("claude", 0, "SessionStart", signal("registered"));
    let tool = lifecycle_at("claude", 5, "PostToolUse", tool_used());

    let agents = reduce_agent_states(&[registered, tool]);
    assert_eq!(
        agents[0].turn_started_at,
        Some(Timestamp::from_second(epoch().as_second() + 5).unwrap())
    );
}
