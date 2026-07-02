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
        .turn_error(10, "API Error: Bad Request");

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
        Some("API Error: Bad Request"),
        "the row carries the upstream error text for the card's line 2"
    );
    assert!(
        snapshot.worktree_groups[0]
            .status_counts
            .iter()
            .any(|count| count.status == AgentStatus::Failed && count.count == 1),
        "the dead turn counts in the attention tally"
    );
    let rolled_up = rollup_agent(&snapshot, "live-claude");
    assert_eq!(
        rolled_up.status,
        AgentStatus::Running,
        "the rollup keeps the agent-owned status; only the display row escalates"
    );
}

#[test]
fn codex_stop_over_rate_limit_terminal_row_parks_until_budget_resets() {
    for case in [
        (
            "spent window parks a failed Stop over the rollout marker",
            vec![window(100, 3_600)],
            vec![window(100, 3_600)],
            AgentStatus::Paused,
            None,
        ),
        (
            "recovered fused budget does not turn a live park into an actionable failure",
            vec![window(100, -60)],
            vec![window(0, 3_600)],
            AgentStatus::Paused,
            None,
        ),
        (
            "a reset short window still waits on a longer spent window",
            vec![window(100, -60), window(100, 86_400)],
            vec![window(100, -60), window(100, 86_400)],
            AgentStatus::Paused,
            None,
        ),
        (
            "a spent window with no recovery clock becomes actionable",
            vec![window(100, -60)],
            vec![unprojectable_spent_window(-60)],
            AgentStatus::Failed,
            Some("You've hit your usage limit"),
        ),
    ] {
        let (label, agent_windows, budget_windows, expected_status, expected_error_label) = case;
        let session = agent("codex", "codex-stop-error", AgentStatus::Failed, 0)
            .worktree("/repo/main")
            .in_pane("%1")
            .turn_started_ago(120)
            .active_ago(5)
            .limits(agent_windows)
            .paused_turn_error(10, "You've hit your usage limit");

        let snapshot = room_with_agent_panes_and_budgets(
            Vec::new(),
            vec![session],
            account_budget("codex", budget_windows),
        );
        let row = row(&snapshot, "codex-stop-error");
        assert_eq!(row.status(), Some(expected_status), "{label}");
        assert_eq!(row.turn_error_label(), expected_error_label, "{label}");
    }
}

#[test]
fn codex_stop_over_spend_limit_terminal_row_parks_until_budget_resets() {
    for case in [
        (
            "spent window parks a failed Stop over the rollout marker",
            vec![window(100, 3_600)],
            vec![window(100, 3_600)],
            AgentStatus::Paused,
            None,
        ),
        (
            "recovered fused budget keeps the spend-limit park non-actionable",
            vec![window(100, -60)],
            vec![window(0, 3_600)],
            AgentStatus::Paused,
            None,
        ),
        (
            "a spent window with no recovery clock becomes actionable",
            vec![window(100, -60)],
            vec![unprojectable_spent_window(-60)],
            AgentStatus::Failed,
            Some("You've hit your monthly spend limit."),
        ),
    ] {
        let (label, agent_windows, budget_windows, expected_status, expected_error_label) = case;
        let session = agent("codex", "codex-stop-error", AgentStatus::Failed, 0)
            .worktree("/repo/main")
            .in_pane("%1")
            .turn_started_ago(120)
            .active_ago(5)
            .limits(agent_windows)
            .spend_limit_turn_error(10, "You've hit your monthly spend limit.");

        let snapshot = room_with_agent_panes_and_budgets(
            Vec::new(),
            vec![session],
            account_budget("codex", budget_windows),
        );
        let row = row(&snapshot, "codex-stop-error");
        assert_eq!(row.status(), Some(expected_status), "{label}");
        assert_eq!(row.turn_error_label(), expected_error_label, "{label}");
    }
}

#[test]
fn legacy_session_limit_marker_parks_while_budget_is_spent() {
    let session = agent("claude", "session-limited", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(60)
        .limits(vec![window(100, 3_600)])
        .turn_error(10, "You've hit your session limit · resets 10:50am (UTC)");

    let snapshot = room_with_agent_panes_and_budgets(
        Vec::new(),
        vec![session],
        account_budget("claude", vec![window(100, 3_600)]),
    );
    let row = row(&snapshot, "session-limited");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Paused),
        "older sidecars with a failed-class session-limit marker still render as parked"
    );
    assert!(
        row.turn_error_label().is_none(),
        "a parked limit keeps the upstream text out of the actionable-failure line"
    );
}

fn unprojectable_spent_window(resets_in_secs: i64) -> RateLimitWindow {
    RateLimitWindow {
        duration_mins: None,
        ..window(100, resets_in_secs)
    }
}

#[test]
fn terminal_turn_error_before_current_turn_does_not_repark_row() {
    let session = agent("codex", "codex-stale-error", AgentStatus::Failed, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .turn_started_ago(30)
        .active_ago(5)
        .limits(vec![window(100, 3_600)])
        .paused_turn_error(60, "You've hit your usage limit");

    let snapshot = room_with_agent_panes(Vec::new(), vec![session]);
    let row = row(&snapshot, "codex-stale-error");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Failed),
        "a marker from an older turn must not turn a fresh failed row into parked state"
    );
    assert!(
        row.turn_error_label().is_none(),
        "the older marker is ignored rather than shown as this turn's cause"
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

#[test]
fn overloaded_turn_error_stays_paused_past_the_stall_window() {
    let session = agent("claude", "busy-claude", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(default_stall_secs() + 3_600)
        .overloaded_turn_error(10, "API Error: Overloaded");

    let snapshot = room(Vec::new(), vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = row(&snapshot, "busy-claude");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Paused),
        "an overloaded park remains paused even when the generic stall backstop would fail a running row"
    );
}

#[test]
fn transient_server_error_stays_paused_past_the_stall_window() {
    let temporary_500 = concat!(
        "API Error: 500 Internal server error. ",
        "This is a server-side issue, usually temporary — try again in a moment."
    );
    let session = agent("claude", "busy-claude", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(default_stall_secs() + 3_600)
        .overloaded_turn_error(10, temporary_500);

    let snapshot = room(Vec::new(), vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = row(&snapshot, "busy-claude");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Paused),
        "a transient server-error park remains paused even when the generic stall backstop would fail a running row"
    );
}

#[test]
fn stalled_stream_error_stays_paused_past_the_stall_window() {
    let label = "API Error: Response stalled mid-stream. The response above may be incomplete.";
    let session = agent("claude", "busy-claude", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(default_stall_secs() + 3_600)
        .overloaded_turn_error(10, label);

    let snapshot = room(Vec::new(), vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = row(&snapshot, "busy-claude");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Paused),
        "a stalled stream parks for backoff instead of falling through to the stall failure"
    );
}

// ── The precedence ladder, pinned as an ordering ─────────────────────────────
//
// docs/internals/agents/agent.md commits to a strict order among the derived display
// states: a human-blocked `waiting` outranks them all, then a paused-class
// marker, then the live-subagent exemption, then a failed marker, then the
// stalled-running fallback (paused when the kind's window is spent, failed
// otherwise). The single-cause cases above each prove one rung; this grid pins
// the order by stacking causes.
