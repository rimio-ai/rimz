use super::*;

#[test]
fn interrupted_turn_settles_running_to_idle_before_stall() {
    // Codex Esc or `/clear` of a running turn writes `turn_aborted` and fires no
    // Stop hook, so the rollup stays `running`. The interrupted marker postdates
    // last activity and settles the row to idle, not failed, even after the
    // stall window.
    let session = agent("codex", "codex-clear", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .turn_started_ago(default_stall_secs() + 120)
        .active_ago(default_stall_secs() + 60)
        .settle(10, TurnSettleOutcome::Interrupted);

    let snapshot = room_with_agent_panes(vec![session]);
    let row = row(&snapshot, "codex-clear");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Idle),
        "a resting turn_aborted tail settles the falsely-running row to idle"
    );
    let rolled_up = rollup_agent(&snapshot, "codex-clear");
    assert_eq!(
        rolled_up.status,
        AgentStatus::Running,
        "the rollup keeps the agent-owned status; only the display row settles"
    );
}

#[test]
fn interrupted_native_ask_settles_waiting_to_idle() {
    let mut session = agent("claude", "claude-ask", AgentStatus::Waiting, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(60)
        .settle(10, TurnSettleOutcome::Interrupted);
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
        Some(AgentStatus::Idle),
        "Esc-cancelling Claude's native ask clears false attention even with a budget park"
    );
    assert_eq!(
        row.turn_error_label(),
        None,
        "an idle interrupted ask does not retain a misleading budget description"
    );
    assert_eq!(
        rollup_agent(&snapshot, "claude-ask").status,
        AgentStatus::Waiting,
        "the transcript marker refines only the display projection"
    );
}

#[test]
fn interruption_before_last_activity_leaves_row_running() {
    // A newer prompt advanced `last_activity` past the abort marker, so the
    // stale marker belongs to a prior turn and must not settle fresh work.
    let session = agent("codex", "codex-clear", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .turn_started_ago(20)
        .active_ago(5)
        .settle(120, TurnSettleOutcome::Interrupted);

    let snapshot = room_with_agent_panes(vec![session]);
    let row = row(&snapshot, "codex-clear");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Running),
        "an interruption marker older than last_activity belongs to a prior turn"
    );
}

#[test]
fn turn_error_outranks_interruption_marker() {
    let session = agent("codex", "codex-clear", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .turn_started_ago(120)
        .active_ago(60)
        .settle(10, TurnSettleOutcome::Interrupted)
        .turn_error(10, "API Error: Bad Request");

    let snapshot = room_with_agent_panes(vec![session]);
    let row = row(&snapshot, "codex-clear");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Failed),
        "a failed turn-error marker outranks an interruption marker"
    );
    assert_eq!(
        row.turn_error_label(),
        Some("API Error: Bad Request"),
        "the failure keeps the upstream reason on the card"
    );
}
