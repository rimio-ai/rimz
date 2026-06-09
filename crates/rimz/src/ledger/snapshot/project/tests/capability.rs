use super::*;

#[test]
fn lifecycle_carries_capability_forward_when_event_omits_it() {
    // SessionStart establishes the capability line.
    let start = raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
            "model": "GPT-5.5",
            "effort": "high",
            "context_window": 258_400,
            "worktree_branch": "main",
        }),
    );
    // A prompt-submit moves the agent to running but reports no model.
    let prompt = raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
            "task": "fix auth flow",
            "worktree_path": "/tmp/hook-subprocess-cwd",
            "worktree_branch": "wrong-branch",
        }),
    );

    let agents = reduce_agent_states(&[start, prompt]);
    assert_eq!(agents.len(), 1);
    let agent = &agents[0];
    assert_eq!(agent.status, AgentStatus::Running);
    assert_eq!(agent.task.as_deref(), Some("fix auth flow"));
    // Capability survives the prompt.
    assert_eq!(agent.model.as_deref(), Some("GPT-5.5"));
    assert_eq!(agent.effort.as_deref(), Some("high"));
    assert_eq!(agent.context_window, Some(258_400));
    assert_eq!(agent.worktree_branch.as_deref(), Some("main"));
}

#[test]
fn model_label_holds_canonical_across_suffix_drop() {
    // The live flip: SessionStart reports the suffixed id, the prompt omits
    // model entirely, and the first Stop falls back to the transcript's
    // bare id. Canonicalizing at reduce time keeps the label stable so the
    // `[1m]` tag never appears and then vanishes.
    let start = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
            "model": "claude-opus-4-8[1m]",
        }),
    );
    let prompt = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }),
    );
    let stop = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "Stop",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false },
            "model": "claude-opus-4-8",
        }),
    );

    let agents = reduce_agent_states(&[start, prompt, stop]);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].model.as_deref(), Some("claude-opus-4-8"));
}

// ---- Subagent observability (M6): parent link, nesting, retention, reaping ----
