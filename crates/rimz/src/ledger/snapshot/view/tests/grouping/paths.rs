use super::*;

#[test]
fn is_within_compares_path_components() {
    let root = Path::new("/home/marvin");
    assert!(is_within(root, root));
    assert!(is_within(root, Path::new("/home/marvin/")));
    assert!(is_within(root, Path::new("/home/marvin/sub/dir")));
    // A shared string prefix that is not a component boundary is outside.
    assert!(!is_within(root, Path::new("/home/marvinX")));
    assert!(!is_within(root, Path::new("/home/other")));
    assert!(!is_within(root, Path::new("/")));
}

#[test]
fn path_grouping_uses_project_roots_worktree_roots_and_safe_fallbacks() {
    struct Case {
        label: &'static str,
        project_root: Option<&'static str>,
        worktree_roots: Vec<&'static str>,
        panes: Vec<PaneRef>,
        expect_kind: SidebarWorktreeKind,
        expect_key: &'static str,
        expect_label: &'static str,
        expect_rows: usize,
    }

    let root = "/repo/rimz";
    let external = "/elsewhere/feature-wt";
    for case in [
        Case {
            label: "main checkout is a repo worktree pod",
            project_root: Some(root),
            worktree_roots: Vec::new(),
            panes: vec![pane("%1", "zsh", root)],
            expect_kind: SidebarWorktreeKind::Worktree,
            expect_key: root,
            expect_label: "rimz",
            expect_rows: 1,
        },
        Case {
            label: "in-project worktree path keeps its own pod",
            project_root: Some(root),
            worktree_roots: Vec::new(),
            panes: vec![pane("%1", "zsh", "/repo/rimz/.claude/worktrees/featureX")],
            expect_kind: SidebarWorktreeKind::Worktree,
            expect_key: "/repo/rimz/.claude/worktrees/featureX",
            expect_label: "featureX",
            expect_rows: 1,
        },
        Case {
            label: "shared string prefix is external",
            project_root: Some("/home/marvin"),
            worktree_roots: Vec::new(),
            panes: vec![pane("%1", "zsh", "/home/marvinX/repo")],
            expect_kind: SidebarWorktreeKind::External,
            expect_key: "external",
            expect_label: "external",
            expect_rows: 1,
        },
        Case {
            label: "enumerated external worktree owns nested panes",
            project_root: Some(root),
            worktree_roots: vec![root, external],
            panes: vec![
                pane("%1", "claude", external),
                pane("%2", "zsh", "/elsewhere/feature-wt/src"),
            ],
            expect_kind: SidebarWorktreeKind::Worktree,
            expect_key: external,
            expect_label: "feature-wt",
            expect_rows: 2,
        },
        Case {
            label: "known roots leave unrelated cwd as external",
            project_root: Some(root),
            worktree_roots: vec![root, external],
            panes: vec![pane("%1", "zsh", "/home/marvin")],
            expect_kind: SidebarWorktreeKind::External,
            expect_key: "external",
            expect_label: "external",
            expect_rows: 1,
        },
        Case {
            label: "no project root preserves per-path grouping",
            project_root: None,
            worktree_roots: Vec::new(),
            panes: vec![pane("%1", "zsh", "/home/marvin")],
            expect_kind: SidebarWorktreeKind::Worktree,
            expect_key: "/home/marvin",
            expect_label: "marvin",
            expect_rows: 1,
        },
    ] {
        let snapshot = room(Vec::new(), Vec::new())
            .with_project_root(case.project_root.map(PathBuf::from))
            .with_worktree_roots(case.worktree_roots.into_iter().map(PathBuf::from).collect())
            .with_live_panes(case.panes, None);

        assert_eq!(snapshot.worktree_groups.len(), 1, "{}", case.label);
        let group = &snapshot.worktree_groups[0];
        assert_eq!(group.kind, case.expect_kind, "{}", case.label);
        assert_eq!(group.key, case.expect_key, "{}", case.label);
        assert_eq!(group.label, case.expect_label, "{}", case.label);
        assert_eq!(group.rows.len(), case.expect_rows, "{}", case.label);
    }
}

// ── Fleet rooms: directory/marker roots, child-repo pods, the root pod ───────
