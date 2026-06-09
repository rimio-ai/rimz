use super::*;

#[test]
fn api_error_turn_escalates_running_to_attention() {
    // A turn that died on a provider API error fires no Stop hook, so the
    // rollup keeps `running` — but the transcript marker postdates the
    // agent's own activity, and the projection escalates at once. The
    // headline: the agent is *inside* the stall window (silent only a
    // minute), so this beats the configured backstop.
    let session = agent("claude", "live-claude", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(60)
        .turn_error(10, "API Error: Server Error");

    let snapshot = room(Vec::new(), vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status(),
        Some(AgentStatus::Failed),
        "the explicit death certificate escalates without waiting out the stall window"
    );
    assert_eq!(
        row.turn_error_label(),
        Some("API Error: Server Error"),
        "the row carries the upstream error text for the card's line 2"
    );
    assert!(
        snapshot.worktree_groups[0]
            .status_counts
            .iter()
            .any(|count| count.status == AgentStatus::Failed && count.count == 1),
        "the dead turn counts in the attention tally"
    );
    let rolled_up = snapshot
        .agents
        .iter()
        .find(|a| a.agent_id == "live-claude")
        .expect("agent in rollup");
    assert_eq!(
        rolled_up.status,
        AgentStatus::Running,
        "the rollup keeps the agent-owned status; only the display row escalates"
    );
}

#[test]
fn api_error_self_clears_when_activity_resumes() {
    // Any newer hook event (a prompt, a resume, a rewind) advances
    // `last_activity` past the stale marker and the escalation drops with
    // no human action — the self-clear guard.
    let session = agent("claude", "live-claude", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(30)
        .overloaded_turn_error(120, "API Error: Overloaded");

    let snapshot = room(Vec::new(), vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status(),
        Some(AgentStatus::Running),
        "activity newer than the marker means the session moved on"
    );
    assert!(
        row.turn_error_label().is_none(),
        "a cleared escalation leaves no stale reason label"
    );
}

// ── The precedence ladder, pinned as an ordering ─────────────────────────────
//
// docs/internals/agent.md commits to a strict order among the derived display
// states: a human-blocked `waiting` outranks them all, then a paused-class
// marker, then the live-subagent exemption, then a failed marker, then the
// stalled-running fallback (paused when the kind's window is spent, failed
// otherwise). The single-cause cases above each prove one rung; this grid pins
// the order by stacking causes.
