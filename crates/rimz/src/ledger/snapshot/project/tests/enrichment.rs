use super::*;

#[test]
fn lifecycle_carries_enrichment_forward() {
    let start = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
            "context_pct": 38,
            "total_tokens": 12_400,
            "cache_read_input_tokens": 9_000,
            "fresh_input_tokens": 1_200,
            "output_tokens": 800,
            "todo_done": 3,
            "todo_total": 5,
        }),
    );
    let prompt = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
            "task": "fix auth flow",
        }),
    );

    let agents = reduce_agent_states(&[start, prompt]);
    assert_eq!(agents.len(), 1);
    let agent = &agents[0];
    assert_eq!(agent.context_pct, Some(38));
    assert_eq!(agent.total_tokens, Some(12_400));
    assert_eq!(agent.cache_read_input_tokens, Some(9_000));
    assert_eq!(agent.fresh_input_tokens, Some(1_200));
    assert_eq!(agent.output_tokens, Some(800));
    assert_eq!(agent.todo_done, Some(3));
    assert_eq!(agent.todo_total, Some(5));
    assert_eq!(agent.task.as_deref(), Some("fix auth flow"));
}

#[test]
fn session_less_lifecycle_events_are_quarantined_not_merged() {
    // Identity is required: an event without an agent_id folds to nothing
    // (with a log) rather than collapsing into a shared per-kind bucket
    // where two distinct session-less instances would merge into one row.
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
