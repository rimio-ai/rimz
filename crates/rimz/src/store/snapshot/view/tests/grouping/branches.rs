use super::*;

#[test]
fn agents_on_different_branches_in_one_path_form_one_group() {
    // One checkout is one lane whatever branch each row last reported: the
    // group is headed by the checkout and carries a branch beside it.
    let feature = agent("claude", "sess-a", AgentStatus::Idle, 1_000)
        .worktree("/repo/shared")
        .branch("feature");
    let main = agent("claude", "sess-b", AgentStatus::Idle, 1_100)
        .worktree("/repo/shared")
        .branch("main");

    let snapshot = room_with_agent_panes(vec![feature, main]);

    assert_eq!(
        snapshot.worktree_groups.len(),
        1,
        "two branches under one path share one group: {:?}",
        snapshot.worktree_groups
    );
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.key, "/repo/shared");
    assert_eq!(
        group.label, "shared",
        "the checkout, not a branch, heads it"
    );
    assert_eq!(group.rows.len(), 2);
    assert!(
        matches!(group.branch.as_deref(), Some("feature" | "main")),
        "a row's branch rides beside the label: {:?}",
        group.branch
    );
}

#[test]
fn one_branch_path_keeps_agent_and_shell_in_one_group() {
    // The common case must not fragment: a process/shell row carries no
    // branch, so it stays with the single-branch agent in its worktree.
    let claude = agent("claude", "sess-a", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .branch("main")
        .in_pane("%1");

    let snapshot = room(vec![claude]).with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "zsh", "/repo/main"),
        ],
        None,
    );

    assert_eq!(
        snapshot.worktree_groups.len(),
        1,
        "agent and its shell share one worktree group: {:?}",
        snapshot.worktree_groups,
    );
    assert_eq!(snapshot.worktree_groups[0].label, "main");
    assert_eq!(snapshot.worktree_groups[0].branch.as_deref(), Some("main"));
    let rows = &snapshot.worktree_groups[0].rows;
    assert!(rows.iter().any(|row| row.is_agent()));
    assert!(rows.iter().any(|row| row.is_process() && row.name == "zsh"));
}
