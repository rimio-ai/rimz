use super::*;

#[test]
fn clean_review_completion_settles_running_to_success() {
    // A Codex `/review` finishes in review mode and fires no Stop hook, so the
    // rollup keeps `running`. The rollout-tail completion marker postdates the
    // agent's last activity, and the projection settles the row to success
    // instead of leaving it spinning until the stall window misreads it.
    let session = agent("codex", "codex-review", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .turn_started_ago(120)
        .active_ago(60)
        .settle(10, TurnSettleOutcome::Complete);

    let snapshot = room_with_agent_panes(vec![session]);
    let row = row(&snapshot, "codex-review");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Success),
        "a clean task_complete past last_activity settles the falsely-running row"
    );
    let rolled_up = rollup_agent(&snapshot, "codex-review");
    assert_eq!(
        rolled_up.status,
        AgentStatus::Running,
        "the rollup keeps the agent-owned status; only the display row settles"
    );
}

#[test]
fn completion_before_last_activity_leaves_row_running() {
    // A newer prompt advanced `last_activity` past the prior turn's completion
    // marker, so the stale marker must not settle the fresh turn — the same
    // self-clear guard the dead-turn marker uses.
    let session = agent("codex", "codex-review", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .turn_started_ago(20)
        .active_ago(5)
        .settle(120, TurnSettleOutcome::Complete);

    let snapshot = room_with_agent_panes(vec![session]);
    let row = row(&snapshot, "codex-review");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Running),
        "a completion marker older than last_activity belongs to a finished turn"
    );
}

#[test]
fn turn_error_outranks_completion_marker() {
    // If a turn both errored and left a completion marker, the failure wins —
    // a dead turn is never a success. Pins the completion branch below the
    // failed-marker rung of the precedence ladder.
    let session = agent("codex", "codex-review", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .turn_started_ago(120)
        .active_ago(60)
        .settle(10, TurnSettleOutcome::Complete)
        .turn_error(10, "API Error: Bad Request");

    let snapshot = room_with_agent_panes(vec![session]);
    let row = row(&snapshot, "codex-review");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Failed),
        "a failed turn-error marker outranks a completion marker"
    );
    assert_eq!(
        row.turn_error_label(),
        Some("API Error: Bad Request"),
        "the failure keeps the upstream reason on the card"
    );
}

#[test]
fn budget_park_outranks_completion_and_background_park() {
    let park = crate::agents::BudgetPark {
        cap_usd: 5.0,
        spend_usd: 5.25,
        window: crate::agents::BudgetWindow::Session,
        at: epoch(),
        scope: crate::agents::BudgetScope::Agent,
        account_kind: None,
        resets_at: None,
        reset_window: None,
    };
    let mut completed = agent("codex", "completed", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .settle(10, TurnSettleOutcome::Complete);
    completed.budget_park = Some(park.clone());
    let mut parked = agent("codex", "parked", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .parked();
    parked.budget_park = Some(park.clone());

    let snapshot = room_with_agent_panes(vec![completed, parked]);
    for id in ["completed", "parked"] {
        let row = row(&snapshot, id);
        assert_eq!(row.status(), Some(AgentStatus::Paused), "{id}");
        assert_eq!(row.turn_error_label(), Some(park.label().as_str()), "{id}");
    }
}
