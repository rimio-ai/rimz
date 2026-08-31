use super::*;

#[test]
fn fresh_native_permission_marker_routes_attention_without_mutating_rollup_truth() {
    let mut session = agent("antigravity", "agy-wait", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(60)
        .settle(10, TurnSettleOutcome::NativeWait);
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
        .settle(60, TurnSettleOutcome::NativeWait);
    session.pane.as_mut().unwrap().command = Some("agy".to_owned());

    let snapshot = room_with_agent_panes(vec![session]);
    assert_eq!(
        row(&snapshot, "agy-working").status(),
        Some(AgentStatus::Running),
        "a later post-tool or turn hook proves the native dialog moved on"
    );
}

#[test]
fn plan_proposal_outranks_budget_park_on_the_card() {
    let mut session = agent("codex", "codex-plan", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(60)
        .settle(10, TurnSettleOutcome::PlanProposed);
    session.budget_park = Some(crate::harness::budget::BudgetPark {
        cap_usd: 5.0,
        spend_usd: 5.25,
        window: crate::harness::budget::BudgetWindow::Session,
        at: epoch(),
        scope: crate::harness::budget::BudgetScope::Agent,
        account_kind: None,
        resets_at: None,
    });

    let snapshot = room_with_agent_panes(vec![session]);

    assert_eq!(
        row(&snapshot, "codex-plan").status(),
        Some(AgentStatus::Waiting),
        "a proposed plan needs the same attention as a native input prompt"
    );
}
