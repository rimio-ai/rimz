use super::*;

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
