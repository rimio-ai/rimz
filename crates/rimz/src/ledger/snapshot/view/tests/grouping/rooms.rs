use super::*;

#[test]
fn directory_room_groups_panes_by_child_repo() {
    // A directory room (`/srv/agents` holding repos): each enumerated child
    // repo is a group root, so every pane under one child shares one pod keyed
    // on the child's root; panes at the room root take the name-only `Root`
    // pod; a cwd outside the room stays external.
    let snapshot = room(Vec::new(), Vec::new())
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/srv/agents")))
        .with_worktree_roots(vec![
            PathBuf::from("/srv/agents/billing"),
            PathBuf::from("/srv/agents/query-engine"),
        ])
        .with_live_panes(
            vec![
                pane("%1", "claude", "/srv/agents/query-engine"),
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
    assert_eq!(
        summary,
        vec![
            (SidebarWorktreeKind::Root, "/srv/agents", "agents", 1),
            (
                SidebarWorktreeKind::Worktree,
                "/srv/agents/billing",
                "billing",
                1
            ),
            (
                SidebarWorktreeKind::Worktree,
                "/srv/agents/query-engine",
                "query-engine",
                2
            ),
            (SidebarWorktreeKind::External, "external", "external", 1),
        ],
    );
}

#[test]
fn non_repo_room_variants_share_the_root_pod_rule() {
    for (label, root_class, root, worktree_roots, panes, expect_label, expect_rows) in [
        (
            "depth-two repo folds into directory root",
            RootClass::Directory,
            "/srv/agents",
            vec!["/srv/agents/billing"],
            vec![pane("%1", "zsh", "/srv/agents/org/repo")],
            "agents",
            1,
        ),
        (
            "scratch room is one flat root pod",
            RootClass::Directory,
            "/tmp/scratch",
            Vec::new(),
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
            Vec::new(),
            vec![pane("%1", "zsh", "/srv/app/src")],
            "app",
            1,
        ),
    ] {
        let snapshot = room(Vec::new(), Vec::new())
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
fn stale_branch_row_never_relabels_the_root_pod() {
    // A row claiming a branch at a non-repo room root is stale by definition
    // (the root has no git story); the pod keeps its directory name.
    let live = pane("%scratch", "rimz-ask", "/tmp/scratch");
    let mut item = FeedItem::new(
        workspace(),
        Surface::Script,
        FeedKind::Question,
        "Should I proceed?",
        "rimz",
        "cli",
    );
    item.worktree_path = Some("/tmp/scratch".to_owned());
    item.worktree_branch = Some("main".to_owned());
    item.pane = Some(live.clone());

    let snapshot = room(vec![item], Vec::new())
        .with_root_class(RootClass::Directory)
        .with_project_root(Some(PathBuf::from("/tmp/scratch")))
        .with_live_panes(vec![live], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Root);
    assert_eq!(group.label, "scratch");
}
