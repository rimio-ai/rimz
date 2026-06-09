use super::*;

#[test]
fn idle_agent_in_spent_account_is_not_paused() {
    // A spent account reading is budget display, not a row-wide park. Calm
    // agents stay calm, and a running agent that is still inside the stall
    // window keeps working.
    let reporter = agent("claude", "sess-spent", AgentStatus::Success, 1_000)
        .worktree("/repo/main")
        .limits(vec![window(100, 3_600)]);
    let fresh = agent("claude", "sess-fresh", AgentStatus::Idle, 1_100).worktree("/repo/main");
    let working = agent("claude", "sess-busy", AgentStatus::Running, 1_200).worktree("/repo/main");

    let snapshot = room_with_agent_panes(Vec::new(), vec![reporter, fresh, working]);
    assert_eq!(
        row(&snapshot, "sess-spent").status(),
        Some(AgentStatus::Success)
    );
    assert_eq!(
        row(&snapshot, "sess-fresh").status(),
        Some(AgentStatus::Idle),
        "a fresh idle session does not inherit an account-wide park"
    );
    assert_eq!(
        row(&snapshot, "sess-busy").status(),
        Some(AgentStatus::Running),
        "a live running session is not paused until it stalls or carries a marker"
    );
}

#[test]
fn a_window_spent_but_already_reset_does_not_park() {
    // A spent reading whose reset has passed is stale, not limiting — the
    // budget has refilled, so a resting agent reads idle, not parked.
    let idle = agent("claude", "sess-1", AgentStatus::Idle, 1_000)
        .worktree("/repo/main")
        .limits(vec![window(100, -60)]);

    let snapshot = room_with_agent_panes(Vec::new(), vec![idle]);
    assert_eq!(
        snapshot.worktree_groups[0].rows[0].status(),
        Some(AgentStatus::Idle),
        "a passed reset means the budget refilled — not paused"
    );
}

#[test]
fn paused_rate_limit_marker_lifts_only_after_every_spent_window_resets() {
    for case in [
        (
            "spent window parks the affected agent",
            vec![window(100, 3_600)],
            AgentStatus::Paused,
            None,
        ),
        (
            "after reset the still-dead turn becomes resumable failure",
            vec![window(100, -60)],
            AgentStatus::Failed,
            Some("You've hit your usage limit"),
        ),
        (
            "reset short window waits for longer spent window",
            vec![window(100, -60), window(100, 86_400)],
            AgentStatus::Paused,
            None,
        ),
    ] {
        let (label, windows, expected_status, expected_error_label) = case;
        let session = agent("claude", "limited-dead", AgentStatus::Running, 0)
            .worktree("/repo/main")
            .active_ago(60)
            .limits(windows)
            .paused_turn_error(10, "You've hit your usage limit");

        let snapshot = room_with_agent_panes(Vec::new(), vec![session]);
        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(row.status(), Some(expected_status), "{label}");
        assert_eq!(row.turn_error_label(), expected_error_label, "{label}");
    }
}
