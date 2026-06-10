use super::*;

#[test]
fn compaction_end_clears_marker_and_counts_completed_brackets() {
    let prompt = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }),
    );
    let compact = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "PreCompact",
            "agent_id": "sess-1",
            "signal": { "signal": "compacting" },
        }),
    );
    let post = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "PostCompact",
            "agent_id": "sess-1",
            "signal": { "signal": "compaction_ended", "auto": false },
        }),
    );
    let next_prompt = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }),
    );
    let second_compact = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "PreCompact",
            "agent_id": "sess-1",
            "signal": { "signal": "compacting" },
        }),
    );
    let second_post = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "PostCompact",
            "agent_id": "sess-1",
            "signal": { "signal": "compaction_ended", "auto": true },
        }),
    );

    let agents = reduce_agent_states(&[prompt.clone(), compact.clone(), post.clone()]);
    assert_eq!(agents[0].status, AgentStatus::Idle);
    assert_eq!(agents[0].phase, TurnPhase::Idle);
    assert!(
        agents[0].compacting_since.is_none(),
        "a compaction-close signal clears the transient marker"
    );

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
fn auto_compaction_ended_resumes_running_and_clears_marker() {
    let prompt = raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }),
    );
    let edit = raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": "PostToolUse",
            "agent_id": "sess-1",
            "signal": { "signal": "tool_used", "mutates": true, "edits": true },
        }),
    );
    let compact = raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": "PreCompact",
            "agent_id": "sess-1",
            "signal": { "signal": "compacting" },
        }),
    );
    let post = raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": "PostCompact",
            "agent_id": "sess-1",
            "signal": { "signal": "compaction_ended", "auto": true },
        }),
    );

    let agents = reduce_agent_states(&[prompt, edit, compact, post]);
    assert_eq!(agents[0].status, AgentStatus::Running);
    assert_eq!(
        agents[0].phase,
        TurnPhase::Acting,
        "auto compaction carries the interrupted turn phase"
    );
    assert!(agents[0].compacting_since.is_none());
}

#[test]
fn unknown_compaction_end_only_clears_marker() {
    let prompt = raw_lifecycle(
        "pi",
        serde_json::json!({
            "event_name": "before_agent_start",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }),
    );
    let edit = raw_lifecycle(
        "pi",
        serde_json::json!({
            "event_name": "tool_execution_end",
            "agent_id": "sess-1",
            "signal": { "signal": "tool_used", "mutates": true, "edits": true },
        }),
    );
    let compact = raw_lifecycle(
        "pi",
        serde_json::json!({
            "event_name": "session_before_compact",
            "agent_id": "sess-1",
            "signal": { "signal": "compacting" },
        }),
    );
    let post = raw_lifecycle(
        "pi",
        serde_json::json!({
            "event_name": "session_compact",
            "agent_id": "sess-1",
            "signal": { "signal": "compaction_ended" },
        }),
    );

    let agents = reduce_agent_states(&[prompt, edit, compact, post]);
    assert_eq!(agents[0].status, AgentStatus::Running);
    assert_eq!(agents[0].phase, TurnPhase::Acting);
    assert!(agents[0].compacting_since.is_none());
}

#[test]
fn missed_trailing_hook_counts_when_next_signal_closes_bracket() {
    let prompt = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }),
    );
    let compact = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "PreCompact",
            "agent_id": "sess-1",
            "signal": { "signal": "compacting" },
        }),
    );
    let next = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "PreToolUse",
            "agent_id": "sess-1",
            "signal": { "signal": "tool_used", "mutates": false, "edits": false },
        }),
    );

    let agents = reduce_agent_states(&[prompt, compact, next]);
    assert_eq!(agents[0].compaction_count, 1);
    assert!(agents[0].compacting_since.is_none());
    assert_eq!(agents[0].status, AgentStatus::Running);
}

#[test]
fn repeated_compacting_signal_refreshes_marker_without_counting() {
    let prompt = raw_lifecycle_at(
        "claude",
        0,
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }),
    );
    let compact = raw_lifecycle_at(
        "claude",
        1,
        serde_json::json!({
            "event_name": "PreCompact",
            "agent_id": "sess-1",
            "signal": { "signal": "compacting" },
        }),
    );
    let later_second_compact = raw_lifecycle_at(
        "claude",
        crate::feed::COMPACTING_WINDOW_SECS + 5,
        serde_json::json!({
            "event_name": "PreCompact",
            "agent_id": "sess-1",
            "signal": { "signal": "compacting" },
        }),
    );

    let agents = reduce_agent_states(&[prompt, compact, later_second_compact]);
    assert_eq!(agents[0].compaction_count, 0);
    assert_eq!(
        agents[0].compacting_since,
        Some(
            Timestamp::from_second(epoch().as_second() + crate::feed::COMPACTING_WINDOW_SECS + 5)
                .unwrap()
        )
    );
    assert_eq!(agents[0].status, AgentStatus::Running);
}

#[test]
fn double_compaction_end_events_count_once_in_either_order() {
    let prompt = raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }),
    );
    let compact = raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": "PreCompact",
            "agent_id": "sess-1",
            "signal": { "signal": "compacting" },
        }),
    );
    let post = raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": "PostCompact",
            "agent_id": "sess-1",
            "signal": { "signal": "compaction_ended", "auto": true },
        }),
    );
    let session_compact = raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "compaction_ended" },
        }),
    );

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
fn unbracketed_compaction_end_applies_edge_without_counting() {
    let prompt = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }),
    );
    let manual_end = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "PostCompact",
            "agent_id": "sess-1",
            "signal": { "signal": "compaction_ended", "auto": false },
        }),
    );

    let agents = reduce_agent_states(&[prompt, manual_end]);
    assert_eq!(agents[0].status, AgentStatus::Idle);
    assert_eq!(agents[0].compaction_count, 0);
    assert!(agents[0].compacting_since.is_none());
}

#[test]
fn display_expired_bracket_still_counts_on_next_signal() {
    let prompt = raw_lifecycle_at(
        "claude",
        0,
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }),
    );
    let compact = raw_lifecycle_at(
        "claude",
        1,
        serde_json::json!({
            "event_name": "PreCompact",
            "agent_id": "sess-1",
            "signal": { "signal": "compacting" },
        }),
    );
    let later = raw_lifecycle_at(
        "claude",
        crate::feed::COMPACTING_WINDOW_SECS + 5,
        serde_json::json!({
            "event_name": "Stop",
            "agent_id": "sess-1",
            "signal": {
                "signal": "turn_ended",
                "errored": false,
                "parked_on_background": false
            },
        }),
    );

    let agents = reduce_agent_states(&[prompt, compact, later]);
    assert_eq!(agents[0].compaction_count, 1);
    assert!(agents[0].compacting_since.is_none());
    assert_eq!(agents[0].status, AgentStatus::Success);
}

#[test]
fn turn_started_at_is_stamped_when_progress_reconciles_a_turn_open() {
    let registered = raw_lifecycle_at(
        "claude",
        0,
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
        }),
    );
    let tool = raw_lifecycle_at(
        "claude",
        5,
        serde_json::json!({
            "event_name": "PostToolUse",
            "agent_id": "sess-1",
            "signal": { "signal": "tool_used", "mutates": true, "edits": true },
        }),
    );

    let agents = reduce_agent_states(&[registered, tool]);
    assert_eq!(
        agents[0].turn_started_at,
        Some(Timestamp::from_second(epoch().as_second() + 5).unwrap())
    );
}
