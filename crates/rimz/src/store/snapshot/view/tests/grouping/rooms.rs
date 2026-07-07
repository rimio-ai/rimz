use super::*;

#[test]
fn directory_room_groups_git_backed_rows_by_resolved_worktree() {
    // A directory room no longer scans children. Git-backed rows contribute
    // their own resolved toplevels, so every pane under one live checkout shares
    // that pod; panes at the room root take the name-only `Root` pod; a cwd
    // outside the room stays external.
    let query = agent("claude", "query", AgentStatus::Running, 20)
        .worktree("/srv/agents/query-engine")
        .branch("main")
        .in_pane("%1");
    let billing = agent("codex", "billing", AgentStatus::Running, 10)
        .worktree("/srv/agents/billing")
        .branch("billing")
        .in_pane("%3");
    let snapshot = room(vec![query, billing])
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/srv/agents")))
        .with_live_panes(
            vec![
                pane("%1", "claude", "/srv/agents/query-engine/src"),
                pane("%2", "zsh", "/srv/agents/query-engine/src"),
                pane("%3", "codex", "/srv/agents/billing"),
                pane("%4", "zsh", "/srv/agents"),
                pane("%5", "zsh", "/tmp/elsewhere"),
            ],
            None,
        );

    let summary: Vec<(SidebarWorktreeKind, &str, &str, usize)> = snapshot
        .worktree_groups
        .iter()
        .map(|group| {
            (
                group.kind,
                group.key.as_str(),
                group.label.as_str(),
                group.rows.len(),
            )
        })
        .collect();
    // Groups order by their earliest pane's creation ordinal: query-engine
    // (`%1`) before billing (`%3`) before the room root pod (`%4`), with external
    // tailing. Membership and counts are what this test pins; the order tracks
    // pane creation rather than the label.
    assert_eq!(
        summary,
        vec![
            (
                SidebarWorktreeKind::Worktree,
                "/srv/agents/query-engine",
                "main",
                2
            ),
            (
                SidebarWorktreeKind::Worktree,
                "/srv/agents/billing",
                "billing",
                1
            ),
            (SidebarWorktreeKind::Root, "/srv/agents", "agents", 1),
            (SidebarWorktreeKind::External, "external", "external", 1),
        ],
    );
}

#[test]
fn non_repo_room_variants_share_the_root_pod_rule() {
    for (label, root_class, root, worktree_roots, panes, expect_label, expect_rows) in [
        (
            "non-git nested cwd folds into directory root",
            RootClass::Directory,
            "/srv/agents",
            Vec::<&str>::new(),
            vec![pane("%1", "zsh", "/srv/agents/org/repo")],
            "agents",
            1,
        ),
        (
            "scratch room is one flat root pod",
            RootClass::Directory,
            "/tmp/scratch",
            Vec::<&str>::new(),
            vec![
                pane("%1", "claude", "/tmp/scratch"),
                pane("%2", "zsh", "/tmp/scratch/logs"),
            ],
            "scratch",
            2,
        ),
        (
            "marker room reads like directory room",
            RootClass::Marker,
            "/srv/app",
            Vec::<&str>::new(),
            vec![pane("%1", "zsh", "/srv/app/src")],
            "app",
            1,
        ),
    ] {
        let snapshot = room(Vec::new())
            .with_root_class(root_class)
            .with_project_root(Some(PathBuf::from(root)))
            .with_worktree_roots(worktree_roots.into_iter().map(PathBuf::from).collect())
            .with_live_panes(panes, None);

        assert_eq!(snapshot.worktree_groups.len(), 1, "{label}");
        let group = &snapshot.worktree_groups[0];
        assert_eq!(group.kind, SidebarWorktreeKind::Root, "{label}");
        assert_eq!(group.key, root, "{label}");
        assert_eq!(group.label, expect_label, "{label}");
        assert_eq!(group.rows.len(), expect_rows, "{label}");
    }
}

#[test]
fn deeply_nested_git_backed_row_gets_own_worktree_pod() {
    let agent = agent("claude", "nested", AgentStatus::Running, 20)
        .worktree("/srv/agents/org/team/repo")
        .branch("feature/nested")
        .in_pane("%1");

    let snapshot = room(vec![agent])
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/srv/agents")))
        .with_live_panes(
            vec![
                pane("%1", "claude", "/srv/agents/org/team/repo/src"),
                pane("%2", "zsh", "/srv/agents"),
            ],
            None,
        );

    let summary: Vec<(SidebarWorktreeKind, &str, &str, usize)> = snapshot
        .worktree_groups
        .iter()
        .map(|group| {
            (
                group.kind,
                group.key.as_str(),
                group.label.as_str(),
                group.rows.len(),
            )
        })
        .collect();

    assert_eq!(
        summary,
        vec![
            (
                SidebarWorktreeKind::Worktree,
                "/srv/agents/org/team/repo",
                "feature/nested",
                1
            ),
            (SidebarWorktreeKind::Root, "/srv/agents", "agents", 1),
        ],
    );
}
