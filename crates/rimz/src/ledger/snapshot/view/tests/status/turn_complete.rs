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
        .turn_complete(10);

    let snapshot = room_with_agent_panes(Vec::new(), vec![session]);
    let row = row(&snapshot, "codex-review");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Success),
        "a clean task_complete past last_activity settles the falsely-running row"
    );
    let rolled_up = snapshot
        .agents
        .iter()
        .find(|a| a.agent_id == "codex-review")
        .expect("agent in rollup");
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
        .turn_complete(120);

    let snapshot = room_with_agent_panes(Vec::new(), vec![session]);
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
        .turn_complete(10)
        .turn_error(10, "API Error: Server Error");

    let snapshot = room_with_agent_panes(Vec::new(), vec![session]);
    let row = row(&snapshot, "codex-review");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Failed),
        "a failed turn-error marker outranks a completion marker"
    );
    assert_eq!(
        row.turn_error_label(),
        Some("API Error: Server Error"),
        "the failure keeps the upstream reason on the card"
    );
}
