use super::*;
use crate::agents::AgentLifecycleObservation;
use crate::store::event::EventEnvelope;

#[test]
fn pi_session_envelopes_and_bridge_events_converge_on_one_rich_child() {
    for bridge_first in [true, false] {
        let mut parent = AgentLifecycleObservation::new(
            Some("parent-session".into()),
            LifecycleSignal::Registered,
        );
        parent.worktree_path = Some("/repo/main".to_owned());

        let mut child_start = AgentLifecycleObservation::new(
            Some("child-session".into()),
            LifecycleSignal::Registered,
        );
        child_start.launch.model = Some("gpt-5.6-sol".to_owned());
        child_start.launch.effort = Some("xhigh".to_owned());
        child_start.usage.total_tokens = Some(12_345);

        let mut child_settled = AgentLifecycleObservation::new(
            Some("child-session".into()),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        );
        child_settled.launch.model = Some("gpt-5.6-sol".to_owned());
        child_settled.launch.effort = Some("xhigh".to_owned());
        child_settled.usage.total_tokens = Some(12_345);

        let mut bridge = AgentLifecycleObservation::new(
            Some("child-session".into()),
            LifecycleSignal::SubagentStarted,
        );
        bridge.parent_agent_id = Some("parent-session".into());
        bridge.task = Some("general-purpose: inspect the bridge".to_owned());

        let event = |event_name, observation| {
            let mut event = EventEnvelope::agent_lifecycle(
                workspace(),
                "session",
                "pi",
                event_name,
                &observation,
            );
            event.timestamp = epoch();
            event
        };
        let parent = event("session_start", parent);
        let child_start = event("session_start", child_start);
        let child_settled = event("agent_settled", child_settled);
        let adoption = event("SubagentAdopted", bridge.clone());
        let bridge = event("subagent_started", bridge);
        let events = if bridge_first {
            vec![parent, bridge, child_start, child_settled]
        } else {
            vec![parent, child_start, bridge, adoption, child_settled]
        };

        let agents = reduce_agent_states(&events);
        assert_eq!(
            agents
                .iter()
                .filter(|agent| agent.agent_id == "child-session")
                .count(),
            1,
            "bridge_first={bridge_first}",
        );
        let snapshot = room_with_agent_panes(agents);
        assert_eq!(rows(&snapshot).len(), 1, "bridge_first={bridge_first}");
        let children = row(&snapshot, "parent-session").sub_agents();
        assert_eq!(children.len(), 1, "bridge_first={bridge_first}");
        assert_eq!(children[0].id, "child-session");
        assert_eq!(children[0].model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(children[0].effort.as_deref(), Some("xhigh"));
        assert_eq!(children[0].total_tokens, Some(12_345));
    }
}

#[test]
fn sub_agent_nests_under_parent_and_orphans_drop() {
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    // Only the parent built a row; the paneless child attaches onto it.
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert_eq!(rows.len(), 1, "the child is never its own top-level row");
    assert_eq!(rows[0].sub_agents().len(), 1);
    assert_eq!(rows[0].sub_agents()[0].id, "child-1");
    assert_eq!(rows[0].sub_agents()[0].name, "Explore");

    let child = child_state("missing-parent", "child-1", AgentStatus::Running, 5);
    let mut rows: Vec<SidebarRow> = Vec::new();
    attach_sub_agents(&mut rows, &[child], epoch());
    assert!(rows.is_empty(), "a child with no parent row never renders");
}

#[test]
fn waiting_provider_child_lifts_parent_card_attention() {
    let parent = agent("claude", "sess-root", AgentStatus::Idle, 100);
    let child = child_state("sess-root", "child-1", AgentStatus::Waiting, 5);
    let snapshot = room_with_agent_panes(vec![parent, child]);
    let row = row(&snapshot, "sess-root");

    assert_eq!(row.status(), Some(AgentStatus::Idle));
    assert_eq!(row.attention_status(), Some(AgentStatus::Waiting));
    assert!(row.attention_score >= 600);
    assert!(
        snapshot.worktree_groups[0]
            .status_counts
            .iter()
            .any(|count| count.status == AgentStatus::Waiting && count.count == 1)
    );
}

// ── Child activity folds onto the parent's displayed clock ───────────────────

#[test]
fn recently_finished_child_holds_off_the_stall() {
    // The fold runs before the displayed-status projection, so the stall
    // check reads the folded clock: a parent silent past the stall window
    // whose child finished four minutes ago is alive, not wedged.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100).active_ago(660);
    let child = child_state("sess-root", "child-1", AgentStatus::Success, 240);
    let snapshot = room_with_agent_panes(vec![parent, child]);

    let row = row(&snapshot, "sess-root");
    assert_eq!(row.status(), Some(AgentStatus::Running), "not a stall");
    assert_eq!(row.last_activity, ago(240));
    let rollup = rollup_agent(&snapshot, "sess-root");
    assert_eq!(
        rollup.last_activity,
        ago(660),
        "the fold is display-only; the rollup keeps the parent's own clock"
    );
}

#[test]
fn child_activity_does_not_reclock_parent_attention_or_dead_turns() {
    for (label, parent, child_status, expected_status, expected_error) in [
        (
            "waiting parent keeps ask clock",
            agent("claude", "sess-root", AgentStatus::Waiting, 100).active_ago(120),
            AgentStatus::Running,
            AgentStatus::Waiting,
            None,
        ),
        (
            "turn-dead parent keeps death certificate",
            agent("claude", "sess-root", AgentStatus::Running, 100)
                .active_ago(120)
                .turn_error(60, "API Error: Bad Request"),
            AgentStatus::Success,
            AgentStatus::Failed,
            Some("API Error: Bad Request"),
        ),
    ] {
        let child = child_state("sess-root", "child-1", child_status, 5);
        let snapshot = room_with_agent_panes(vec![parent, child]);

        let row = row(&snapshot, "sess-root");
        assert_eq!(row.status(), Some(expected_status), "{label}");
        assert_eq!(row.last_activity, ago(120), "{label}");
        assert_eq!(row.turn_error_label(), expected_error, "{label}");
    }
}
