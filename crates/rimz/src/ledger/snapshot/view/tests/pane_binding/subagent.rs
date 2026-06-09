use super::*;

#[test]
fn subagent_never_steals_its_parents_pane() {
    // A subagent runs in its parent's pane, so its lifecycle hooks stamp the
    // parent's pane id — parent and child both claim `%1`. The child here is
    // strictly more recently active than the parked parent, which would let
    // `max_by_key(last_activity)` bind the pane to the child. Panes bind root
    // agents only: `%1` stays the parent's row and the child nests under it.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    // Newer activity than the parent (5s ago vs ~99s ago) — the flip trigger.
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5)
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(Vec::new(), vec![parent, child])
        .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "one pane binds exactly one top-level row");
    assert_eq!(
        rows[0].id, "sess-root",
        "the pane binds the root, not the child"
    );
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
    assert_eq!(
        rows[0].sub_agents().len(),
        1,
        "the child nests under the parent"
    );
    assert_eq!(rows[0].sub_agents()[0].id, "child-1");
    assert_eq!(rows[0].sub_agents()[0].name, "Explore");
}
