use super::*;

#[test]
fn fresh_native_permission_marker_routes_attention_without_mutating_rollup_truth() {
    let mut session = agent("antigravity", "agy-wait", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(60)
        .native_permission_wait(10);
    session.pane.as_mut().unwrap().command = Some("agy".to_owned());

    let snapshot = room_with_agent_panes(vec![session]);
    assert_eq!(
        row(&snapshot, "agy-wait").status(),
        Some(AgentStatus::Waiting),
        "a newer read-only status marker raises the card for pane routing"
    );
    assert_eq!(
        rollup_agent(&snapshot, "agy-wait").status,
        AgentStatus::Running,
        "the provider statusline does not manufacture durable lifecycle truth"
    );
}

#[test]
fn newer_lifecycle_activity_self_clears_a_stale_native_permission_marker() {
    let mut session = agent("antigravity", "agy-working", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(5)
        .native_permission_wait(60);
    session.pane.as_mut().unwrap().command = Some("agy".to_owned());

    let snapshot = room_with_agent_panes(vec![session]);
    assert_eq!(
        row(&snapshot, "agy-working").status(),
        Some(AgentStatus::Running),
        "a later post-tool or turn hook proves the native dialog moved on"
    );
}
