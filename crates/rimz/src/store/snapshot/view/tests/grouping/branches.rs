use super::*;

#[test]
fn agents_on_different_branches_in_one_path_form_two_groups() {
    // Root cause 5: stale rows put two branches under one path, collapsing
    // into a single mislabeled section. Keying on branch splits them into
    // two correctly-labeled groups.
    let feature = agent("claude", "sess-a", AgentStatus::Idle, 1_000)
        .worktree("/repo/shared")
        .branch("feature");
    let main = agent("claude", "sess-b", AgentStatus::Idle, 1_100)
        .worktree("/repo/shared")
        .branch("main");

    let snapshot = room_with_agent_panes(vec![feature, main]);

    assert_eq!(
        snapshot.worktree_groups.len(),
        2,
        "two branches under one path split into two groups"
    );
    for group in &snapshot.worktree_groups {
        assert_eq!(group.rows.len(), 1);
        assert_eq!(
            group.rows[0].worktree_branch.as_deref(),
            Some(group.label.as_str()),
            "each group's label matches its branch"
        );
    }
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
    let rows = &snapshot.worktree_groups[0].rows;
    assert!(rows.iter().any(|row| row.is_agent()));
    assert!(rows.iter().any(|row| row.is_process() && row.name == "zsh"));
}
