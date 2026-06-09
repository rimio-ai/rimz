use super::*;

#[test]
fn sub_agents_sort_by_creation_time_ascending() {
    // Spawn order, not activity, keys the list: the child that started
    // first leads however fresh its siblings' activity is, so the list
    // holds still across refreshes. A child with no reported start time
    // sorts after the dated ones, by id.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    // The youngest-started child is the most recently active — an
    // activity-keyed sort would lead with it; creation order must not.
    let mut first = child_state("sess-root", "c-late-id", AgentStatus::Idle, 40);
    first.subagent_started_at = Some(ago(90));
    let mut second = child_state("sess-root", "c-early-id", AgentStatus::Running, 2);
    second.subagent_started_at = Some(ago(60));
    let undated = child_state("sess-root", "c-undated", AgentStatus::Running, 1);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(
        &mut rows,
        &[parent.clone(), undated, second, first],
        epoch(),
    );
    let ids: Vec<&str> = rows[0].sub_agents().iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["c-late-id", "c-early-id", "c-undated"]);
}

#[test]
fn duplicate_children_collapse_to_one_row() {
    // Two reduced states aliasing the same child id must render as one row,
    // so `subagents (N)` never double-counts. Freshest activity wins.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let stale = child_state("sess-root", "child-dup", AgentStatus::Running, 50);
    let fresh = child_state("sess-root", "child-dup", AgentStatus::Running, 5);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), stale, fresh], epoch());
    assert_eq!(
        rows[0].sub_agents().len(),
        1,
        "the same child can't appear twice"
    );
    assert_eq!(rows[0].sub_agents()[0].id, "child-dup");
}

#[test]
fn typeless_child_renders_degraded_label_never_the_kind() {
    // A child with no type label must not borrow the provider kind, which
    // would render as a phantom `claude` row. This is the "3 Explore + 3
    // claude" regression.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    let mut child = child_state("sess-root", "child-1", AgentStatus::Running, 5);
    child.task = None;
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    let name = &rows[0].sub_agents()[0].name;
    assert!(name.starts_with("subagent"), "got {name}");
    assert_ne!(name, "claude");
}

#[test]
fn running_child_past_ghost_ttl_is_reaped() {
    // A running child that never sent `SubagentStop` and has been silent past
    // the generous ghost TTL is a leftover — reaped so it can't freeze the
    // parent's delegated-wait head, even with no fresh turn boundary.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    assert!(parent.turn_started_at.is_none());
    let child = child_state(
        "sess-root",
        "child-1",
        AgentStatus::Running,
        GHOST_SESSION_TTL_SECS + 10,
    );
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert!(
        rows[0].sub_agents().is_empty(),
        "a running child silent past the ghost TTL is reaped"
    );
}

#[test]
fn finished_child_inside_ghost_ttl_is_kept_without_turn_boundary() {
    // The regression: a finished child used to clear on a 5-minute TTL even
    // mid-turn, vanishing from the list while its siblings still ran. A
    // finished child stays through the parent's turn; the ghost TTL is only
    // the age backstop when no turn boundary exists.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    assert!(parent.turn_started_at.is_none());
    let child = child_state("sess-root", "child-1", AgentStatus::Success, 60 * 60);
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert_eq!(
        rows[0].sub_agents().len(),
        1,
        "a finished child inside the ghost TTL is kept without a turn boundary"
    );
}

#[test]
fn finished_child_without_turn_boundary_reaped_past_ghost_ttl() {
    // If the parent never recorded a turn boundary, the ghost TTL is the
    // fallback that keeps a finished child from becoming ledger-scoped.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 100);
    assert!(parent.turn_started_at.is_none());
    let child = child_state(
        "sess-root",
        "child-1",
        AgentStatus::Success,
        GHOST_SESSION_TTL_SECS + 10,
    );
    let mut rows = vec![row_from_agent(&parent, epoch())];
    attach_sub_agents(&mut rows, &[parent.clone(), child], epoch());
    assert!(
        rows[0].sub_agents().is_empty(),
        "a finished child silent past the ghost TTL is reaped without a turn boundary"
    );
}

#[test]
fn newer_subagent_does_not_expire_parent_attention() {
    // A child shares the parent's pane and worktree, so it can be newer than
    // the parent without superseding the parent's human decision surface.
    let item = agent_ask(FeedKind::Permission, "claude", "parent-claude");

    let parent = agent("claude", "parent-claude", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let mut child = agent("claude", "child-claude", AgentStatus::Idle, 2_000)
        .worktree("/repo/main")
        .in_pane("%1");
    child.parent_agent_id = Some("parent-claude".into());

    let snapshot = room(vec![item.clone()], vec![parent, child])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    assert_eq!(
        snapshot.needs_attention[0].request_id, item.request_id,
        "the child must not make the parent's ask stale"
    );
    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.id, "parent-claude");
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(row.request_id().cloned(), Some(item.request_id));
}
