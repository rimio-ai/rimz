use super::*;
use crate::agents::TurnErrorClass;

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

    let snapshot =
        room(vec![session]).with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

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
fn limit_marker_terminal_row_parks_until_budget_resets() {
    for case in [
        (
            "spent window parks a failed Stop over the rollout marker",
            "You've hit your usage limit",
            false,
            vec![window(100, 3_600)],
            vec![window(100, 3_600)],
            AgentStatus::Paused,
            None,
        ),
        (
            "recovered fused budget does not turn a live park into an actionable failure",
            "You've hit your usage limit",
            false,
            vec![window(100, -60)],
            vec![window(0, 3_600)],
            AgentStatus::Paused,
            None,
        ),
        (
            "a reset short window still waits on a longer spent window",
            "You've hit your usage limit",
            false,
            vec![window(100, -60), window(100, 86_400)],
            vec![window(100, -60), window(100, 86_400)],
            AgentStatus::Paused,
            None,
        ),
        (
            "a spent window with no recovery clock becomes actionable",
            "You've hit your usage limit",
            false,
            vec![window(100, -60)],
            vec![unprojectable_spent_window(-60)],
            AgentStatus::Failed,
            Some("You've hit your usage limit"),
        ),
        (
            "spent window parks a failed Stop over the rollout marker",
            "You've hit your monthly spend limit.",
            true,
            vec![window(100, 3_600)],
            vec![window(100, 3_600)],
            AgentStatus::Paused,
            None,
        ),
        (
            "recovered fused budget keeps the spend-limit park non-actionable",
            "You've hit your monthly spend limit.",
            true,
            vec![window(100, -60)],
            vec![window(0, 3_600)],
            AgentStatus::Paused,
            None,
        ),
        (
            "a spent window with no recovery clock becomes actionable",
            "You've hit your monthly spend limit.",
            true,
            vec![window(100, -60)],
            vec![unprojectable_spent_window(-60)],
            AgentStatus::Failed,
            Some("You've hit your monthly spend limit."),
        ),
    ] {
        let (
            label,
            marker_label,
            spend_limit,
            agent_windows,
            budget_windows,
            expected_status,
            expected_error_label,
        ) = case;
        let session = agent("codex", "codex-stop-error", AgentStatus::Failed, 0)
            .worktree("/repo/main")
            .in_pane("%1")
            .turn_started_ago(120)
            .active_ago(5)
            .limits(agent_windows);
        let session = if spend_limit {
            session.spend_limit_turn_error(10, marker_label)
        } else {
            session.paused_turn_error(10, marker_label)
        };

        let snapshot = room_with_agent_panes_and_capacities(
            vec![session],
            provider_capacity("codex", budget_windows),
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

    let snapshot = room_with_agent_panes_and_capacities(
        vec![session],
        provider_capacity("claude", vec![window(100, 3_600)]),
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

#[test]
fn terminal_turn_error_before_current_turn_does_not_repark_row() {
    let session = agent("codex", "codex-stale-error", AgentStatus::Failed, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .turn_started_ago(30)
        .active_ago(5)
        .limits(vec![window(100, 3_600)])
        .paused_turn_error(60, "You've hit your usage limit");

    let snapshot = room_with_agent_panes(vec![session]);
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

    let snapshot =
        room(vec![session]).with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

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
fn paused_class_marker_survives_the_stall_window() {
    let temporary_500 = concat!(
        "API Error: 500 Internal server error. ",
        "This is a server-side issue, usually temporary — try again in a moment."
    );
    for (label, expected) in [
        (
            "API Error: Overloaded",
            "an overloaded park remains paused even when the generic stall backstop would fail a running row",
        ),
        (
            temporary_500,
            "a transient server-error park remains paused even when the generic stall backstop would fail a running row",
        ),
        (
            "API Error: Response stalled mid-stream. The response above may be incomplete.",
            "a stalled stream parks for backoff instead of falling through to the stall failure",
        ),
    ] {
        let session = agent("claude", "busy-claude", AgentStatus::Running, 0)
            .worktree("/repo/main")
            .in_pane("%1")
            .active_ago(default_stall_secs() + 3_600)
            .overloaded_turn_error(10, label);

        let snapshot =
            room(vec![session]).with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

        let row = row(&snapshot, "busy-claude");
        assert_eq!(row.status(), Some(AgentStatus::Paused), "{expected}");
    }
}

#[test]
fn messageless_task_complete_marker_fails_running_codex_row_until_pane_proves_park() {
    let session = agent("codex", "codex-capacity", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(60)
        .turn_error_class(
            10,
            "turn ended with no final message",
            TurnErrorClass::Unknown,
        );

    let snapshot = room_with_agent_panes(vec![session]);
    let row = row(&snapshot, "codex-capacity");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Failed),
        "a message-less task_complete is a turn-death marker, not success, but it waits for pane proof before parking"
    );
}
