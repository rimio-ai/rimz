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
        "PostCompact clears the transient marker"
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
