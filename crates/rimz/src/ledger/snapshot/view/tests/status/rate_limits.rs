use super::*;

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
