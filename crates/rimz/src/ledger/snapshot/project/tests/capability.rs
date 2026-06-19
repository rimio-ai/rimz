use super::*;

#[test]
fn lifecycle_carries_stable_fields_forward_when_event_omits_them() {
    let launch = EventEnvelope::agent_launched(
        workspace(),
        "session",
        &AgentKind::new_unchecked("codex"),
        AgentLaunchPayload {
            agent_id: "sess-1".into(),
            agent_name: "lucid-atlas".to_owned(),
            profile: Some("codex-coder".to_owned()),
            role: Some("coder".to_owned()),
            kind_ordinal: None,
            state: AgentLaunchState::Starting,
            run_id: None,
            pane_id: None,
            runtime_owner: None,
            worktree_path: Some("/tmp/x".to_owned()),
            worktree_branch: Some("main".to_owned()),
            prompt: None,
        },
    );
    // SessionStart establishes the capability and progress lines.
    let start = raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
            "model": "GPT-5.5",
            "effort": "high",
            "context_window": 258_400,
            "total_tokens": 12_400,
            "cache_read_input_tokens": 9_000,
            "fresh_input_tokens": 1_200,
            "output_tokens": 800,
            "todo_done": 3,
            "todo_total": 5,
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

    let agents = reduce_agent_states(&[launch, start, prompt]);
    assert_eq!(agents.len(), 1);
    let agent = &agents[0];
    assert_eq!(agent.status, AgentStatus::Running);
    assert_eq!(agent.task.as_deref(), Some("fix auth flow"));
    // Capability survives the prompt.
    assert_eq!(agent.model.as_deref(), Some("GPT-5.5"));
    assert_eq!(agent.effort.as_deref(), Some("high"));
    assert_eq!(agent.context_window, Some(258_400));
    // The gauge percentage is derived from the carried window and used split
    // (cache_read + fresh_input = 10_200 of 258_400 ≈ 3%), so it stays
    // consistent with the window label across an event that omits it.
    assert_eq!(agent.context_pct, Some(3));
    assert_eq!(agent.total_tokens, Some(12_400));
    assert_eq!(agent.cache_read_input_tokens, Some(9_000));
    assert_eq!(agent.fresh_input_tokens, Some(1_200));
    assert_eq!(agent.output_tokens, Some(800));
    assert_eq!(agent.todo_done, Some(3));
    assert_eq!(agent.todo_total, Some(5));
    assert_eq!(agent.profile.as_deref(), Some("codex-coder"));
    assert_eq!(agent.role.as_deref(), Some("coder"));
    assert_eq!(agent.worktree_branch.as_deref(), Some("main"));
}

#[test]
fn context_gauge_tracks_the_carried_window_across_a_marker_drop() {
    // The exact drift bug: an early hook detects the 1M window from the `[1m]`
    // marker, a later hook drops the marker and carries only token totals. The
    // percentage must track the carried 1M window (164k of 1M ≈ 16%), never
    // recompute against a 200k default and read 82%.
    let start = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
            "model": "claude-opus-4-8[1m]",
            "context_window": 1_000_000,
            "total_tokens": 5_000,
        }),
    );
    let stop = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "Stop",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false },
            "model": "claude-opus-4-8",
            "total_tokens": 164_270,
        }),
    );

    let agents = reduce_agent_states(&[start, stop]);
    assert_eq!(agents.len(), 1);
    let agent = &agents[0];
    assert_eq!(agent.context_window, Some(1_000_000));
    assert_eq!(agent.context_pct, Some(16));
    assert_eq!(agent.total_tokens, Some(164_270));
}

#[test]
fn context_gauge_derives_from_the_descriptor_default_when_no_window_reported() {
    // A bare-model Claude session that never sees the `[1m]` marker reports no
    // window; the gauge derives against the 200k descriptor default (100k of
    // 200k = 50%) so the bar still reads a percentage.
    let stop = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "Stop",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_ended", "errored": false, "parked_on_background": false },
            "model": "claude-opus-4-8",
            "total_tokens": 100_000,
        }),
    );

    let agents = reduce_agent_states(&[stop]);
    assert_eq!(agents.len(), 1);
    let agent = &agents[0];
    assert_eq!(agent.context_window, None);
    assert_eq!(agent.context_pct, Some(50));
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

#[test]
fn session_less_lifecycle_events_are_quarantined_not_merged() {
    // Identity is required: an event without an agent_id folds to nothing
    // rather than collapsing distinct session-less instances into one row.
    let event = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SessionStart",
            "signal": { "signal": "registered" },
        }),
    );
    assert!(
        reduce_agent_states(&[event]).is_empty(),
        "a session-less event produces no rollup entry"
    );
}
