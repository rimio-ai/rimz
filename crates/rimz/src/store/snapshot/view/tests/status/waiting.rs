use super::*;

#[test]
fn awaiting_input_outranks_derived_statuses() {
    let mut session = agent("claude", "claude-ask", AgentStatus::Waiting, 0)
        .worktree("/repo/main")
        .active_ago(default_stall_secs() + 60)
        .turn_error(10, "API Error: Bad Request");
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
    let row = row(&snapshot, "claude-ask");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Waiting),
        "a current native prompt outranks budget, turn-error, and stall projections"
    );
    assert_eq!(
        row.turn_error_label(),
        None,
        "an untouched waiting row receives no derived error label"
    );
}

#[test]
fn fresh_stale_waiting_preserves_source_fallback() {
    let mut session = agent("claude", "claude-ask", AgentStatus::Waiting, 0)
        .worktree("/repo/main")
        .active_ago(5);
    session.waiting_since = Some(ago(60));

    let snapshot = room_with_agent_panes(vec![session]);
    let row = row(&snapshot, "claude-ask");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Waiting),
        "the source agent's effective-status clock remains the fallback"
    );
    assert_eq!(
        row.phase(),
        TurnPhase::Idle,
        "the fallback Waiting verdict drops the temporary reasoning phase"
    );
}

#[test]
fn stale_waiting_with_live_child_projects_running() {
    let mut session = agent("claude", "root", AgentStatus::Waiting, 0)
        .worktree("/repo/main")
        .active_ago(5);
    session.waiting_since = Some(ago(60));

    let snapshot = room_with_agent_panes(vec![
        session,
        child_state("root", "child", AgentStatus::Running, 5),
    ]);
    let row = row(&snapshot, "root");
    assert_eq!(row.status(), Some(AgentStatus::Running));
    assert_eq!(
        row.phase(),
        TurnPhase::Reasoning,
        "the live-child rung makes stale Waiting's Running promotion observable"
    );
}

#[test]
fn stale_waiting_uses_running_stall_outcome() {
    let stalled_secs = default_stall_secs() + 60;
    let mut session = agent("claude", "failed", AgentStatus::Waiting, 0)
        .worktree("/repo/main")
        .active_ago(stalled_secs);
    session.waiting_since = Some(ago(stalled_secs + 60));

    let failed = room_with_agent_panes(vec![session]);
    assert_eq!(
        row(&failed, "failed").status(),
        Some(AgentStatus::Failed),
        "stale waiting without a spent window fails like stalled running"
    );
}
