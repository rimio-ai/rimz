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
        .turn_interrupted(10);

    let snapshot = room_with_agent_panes(Vec::new(), vec![session]);
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
fn interruption_before_last_activity_leaves_row_running() {
    // A newer prompt advanced `last_activity` past the abort marker, so the
    // stale marker belongs to a prior turn and must not settle fresh work.
    let session = agent("codex", "codex-clear", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .turn_started_ago(20)
        .active_ago(5)
        .turn_interrupted(120);

    let snapshot = room_with_agent_panes(Vec::new(), vec![session]);
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
        .turn_interrupted(10)
        .turn_error(10, "API Error: Bad Request");

    let snapshot = room_with_agent_panes(Vec::new(), vec![session]);
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
