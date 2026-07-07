use super::*;

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
            project_root: Some("/home/user"),
            worktree_roots: Vec::new(),
            panes: vec![pane("%1", "zsh", "/home/userX/repo")],
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
            panes: vec![pane("%1", "zsh", "/home/user")],
            expect_kind: SidebarWorktreeKind::External,
            expect_key: "external",
            expect_label: "external",
            expect_rows: 1,
        },
        Case {
            label: "no project root preserves per-path grouping",
            project_root: None,
            worktree_roots: Vec::new(),
            panes: vec![pane("%1", "zsh", "/home/user")],
            expect_kind: SidebarWorktreeKind::Worktree,
            expect_key: "/home/user",
            expect_label: "user",
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

#[test]
fn unstamped_rimz_worktree_rows_fold_into_channel_pods() {
    let project_root = PathBuf::from("/repo/rimz");
    let worktree_home = PathBuf::from("/repo/rimz-worktrees");
    let worktree = "/repo/rimz-worktrees/message-channel";
    let mut stamped = agent("claude", "stamped", AgentStatus::Running, 20)
        .worktree(worktree)
        .in_pane("%1");
    stamped.channel = Some("message-channel".to_owned());
    let unstamped = agent("codex", "bare", AgentStatus::Idle, 10)
        .worktree(worktree)
        .in_pane("%2");

    let snapshot = room(Vec::new(), vec![stamped, unstamped])
        .with_project_root(Some(project_root))
        .with_worktree_home(Some(worktree_home))
        .with_live_panes(
            vec![
                pane("%1", "claude", worktree),
                pane("%2", "codex", worktree),
            ],
            None,
        );

    assert_eq!(
        snapshot.worktree_groups.len(),
        1,
        "stamped and unstamped rows share the channel pod: {:?}",
        snapshot.worktree_groups,
    );
    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.kind, SidebarWorktreeKind::Channel);
    assert_eq!(group.key, "channel:message-channel");
    assert_eq!(group.label, "message-channel");
    assert_eq!(group.rows.len(), 2);
}

#[test]
fn lone_unstamped_rimz_worktree_row_uses_channel_pod() {
    let worktree = "/repo/rimz-worktrees/message-channel";
    let snapshot = room(
        Vec::new(),
        vec![
            agent("claude", "bare", AgentStatus::Running, 20)
                .worktree(worktree)
                .in_pane("%1"),
        ],
    )
    .with_project_root(Some(PathBuf::from("/repo/rimz")))
    .with_worktree_home(Some(PathBuf::from("/repo/rimz-worktrees")))
    .with_live_panes(vec![pane("%1", "claude", worktree)], None);

    let group = snapshot.worktree_groups.first().expect("a group");
    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert_eq!(group.kind, SidebarWorktreeKind::Channel);
    assert_eq!(group.key, "channel:message-channel");
    assert_eq!(group.label, "message-channel");
}

#[test]
fn worktree_home_fallback_stays_scoped_to_rimz_owned_worktrees() {
    for (label, worktree_home, path, project_root, expect_kind, expect_key, expect_label) in [
        (
            "pure reducer path keeps per-path grouping",
            None,
            "/repo/rimz-worktrees/message-channel",
            None,
            SidebarWorktreeKind::Worktree,
            "/repo/rimz-worktrees/message-channel",
            "message-channel",
        ),
        (
            "path outside worktree home keeps per-path grouping",
            Some("/repo/rimz-worktrees"),
            "/tmp/scratch",
            None,
            SidebarWorktreeKind::Worktree,
            "/tmp/scratch",
            "scratch",
        ),
        (
            "known project root keeps outside home external",
            Some("/repo/rimz-worktrees"),
            "/tmp/scratch",
            Some("/repo/rimz"),
            SidebarWorktreeKind::External,
            "external",
            "external",
        ),
        (
            "worktree home is normalized before grouping",
            Some("/repo/rimz/../rimz-worktrees"),
            "/repo/rimz-worktrees/message-channel",
            Some("/repo/rimz"),
            SidebarWorktreeKind::Channel,
            "channel:message-channel",
            "message-channel",
        ),
        (
            "room root inside worktree home keeps root worktree grouping",
            Some("/repo/rimz-worktrees"),
            "/repo/rimz-worktrees/message-channel",
            Some("/repo/rimz-worktrees/message-channel"),
            SidebarWorktreeKind::Worktree,
            "/repo/rimz-worktrees/message-channel",
            "message-channel",
        ),
    ] {
        let snapshot = room(Vec::new(), Vec::new())
            .with_project_root(project_root.map(PathBuf::from))
            .with_worktree_home(worktree_home.map(PathBuf::from))
            .with_live_panes(vec![pane("%1", "zsh", path)], None);

        assert_eq!(snapshot.worktree_groups.len(), 1, "{label}");
        let group = &snapshot.worktree_groups[0];
        assert_eq!(group.kind, expect_kind, "{label}");
        assert_eq!(group.key, expect_key, "{label}");
        assert_eq!(group.label, expect_label, "{label}");
    }
}

// ── Fleet rooms: directory/marker roots, row-derived git pods, root pod ──────
