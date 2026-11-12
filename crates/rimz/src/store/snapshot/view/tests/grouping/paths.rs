use super::*;
use crate::store::snapshot::view::layout::{GroupEntry, GroupResolver};

#[test]
fn place_table_resolves_the_deepest_known_folder_without_cwd_guesses() {
    let root = "/repo/rimz";
    let marked = "/repo/rimz-worktrees/link-notices";
    let unmarked = "/elsewhere/feature-wt";
    let foreign = "/home/me/dotfiles";
    let nested = "/repo/rimz/.worktrees/x";
    let child = "/repo/rimz/child";
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (
        name,
        class,
        project,
        listed,
        row_root,
        path,
        branch,
        key,
        kind,
        label,
        root_lane,
        place,
        is_foreign,
    ) in [
        (
            "repo root",
            RootClass::Repo,
            Some(root),
            vec![root],
            None,
            Some(root),
            None,
            root,
            SidebarWorktreeKind::Worktree,
            "rimz",
            true,
            Some(root),
            false,
        ),
        (
            "repo subfolder",
            RootClass::Repo,
            Some(root),
            vec![],
            None,
            Some("/repo/rimz/crates"),
            None,
            root,
            SidebarWorktreeKind::Worktree,
            "rimz",
            true,
            Some(root),
            false,
        ),
        (
            "real team folder",
            RootClass::Repo,
            Some(root),
            vec![],
            None,
            Some("/repo/rimz/.rimz/team"),
            None,
            root,
            SidebarWorktreeKind::Worktree,
            "rimz",
            true,
            Some(root),
            false,
        ),
        (
            "directory subfolder",
            RootClass::Directory,
            Some(root),
            vec![],
            None,
            Some("/repo/rimz/crates"),
            None,
            root,
            SidebarWorktreeKind::Root,
            "main",
            true,
            Some(root),
            false,
        ),
        (
            "marker room subfolder",
            RootClass::Marker,
            Some(root),
            vec![],
            None,
            Some("/repo/rimz/crates"),
            None,
            root,
            SidebarWorktreeKind::Root,
            "main",
            true,
            Some(root),
            false,
        ),
        (
            "directory child repo",
            RootClass::Directory,
            Some(root),
            vec![],
            Some(child),
            Some("/repo/rimz/child/src"),
            None,
            child,
            SidebarWorktreeKind::Worktree,
            "child",
            false,
            Some(child),
            false,
        ),
        (
            "marked root",
            RootClass::Repo,
            Some(root),
            vec![marked],
            None,
            Some(marked),
            None,
            "channel:link-notices",
            SidebarWorktreeKind::Channel,
            "link-notices",
            false,
            Some(marked),
            false,
        ),
        (
            "marked subfolder",
            RootClass::Repo,
            Some(root),
            vec![marked],
            None,
            Some("/repo/rimz-worktrees/link-notices/.rimz"),
            None,
            "channel:link-notices",
            SidebarWorktreeKind::Channel,
            "link-notices",
            false,
            Some(marked),
            false,
        ),
        (
            "unmarked subfolder",
            RootClass::Repo,
            Some(root),
            vec![unmarked],
            None,
            Some("/elsewhere/feature-wt/src"),
            None,
            unmarked,
            SidebarWorktreeKind::Worktree,
            "feature-wt",
            false,
            Some(unmarked),
            false,
        ),
        (
            "foreign agent",
            RootClass::Repo,
            Some(root),
            vec![],
            Some(foreign),
            Some(foreign),
            Some("main"),
            foreign,
            SidebarWorktreeKind::Worktree,
            "main",
            false,
            Some(foreign),
            true,
        ),
        (
            "foreign shell with agent",
            RootClass::Repo,
            Some(root),
            vec![],
            Some(foreign),
            Some("/home/me/dotfiles/src"),
            None,
            foreign,
            SidebarWorktreeKind::Worktree,
            "dotfiles",
            false,
            Some(foreign),
            true,
        ),
        (
            "foreign shell alone",
            RootClass::Repo,
            Some(root),
            vec![],
            None,
            Some("/home/me/dotfiles/src"),
            None,
            "external",
            SidebarWorktreeKind::External,
            "external",
            false,
            None,
            false,
        ),
        (
            "unlisted home folder",
            RootClass::Repo,
            Some(root),
            vec![],
            None,
            Some(marked),
            None,
            "external",
            SidebarWorktreeKind::External,
            "external",
            false,
            None,
            false,
        ),
        (
            "resolved room state",
            RootClass::Repo,
            Some(root),
            vec![],
            None,
            Some("/home/me/.rimz/ws/room/team"),
            None,
            "external",
            SidebarWorktreeKind::External,
            "external",
            false,
            None,
            false,
        ),
        (
            "pathless branch",
            RootClass::Repo,
            Some(root),
            vec![],
            None,
            None,
            Some("topic"),
            "branch:topic",
            SidebarWorktreeKind::Worktree,
            "topic",
            false,
            None,
            false,
        ),
        (
            "no known roots",
            RootClass::Repo,
            None,
            vec![],
            None,
            Some("/home/user"),
            None,
            "external",
            SidebarWorktreeKind::External,
            "external",
            false,
            None,
            false,
        ),
        (
            "nested listing wins",
            RootClass::Repo,
            Some(root),
            vec![root, nested],
            None,
            Some("/repo/rimz/.worktrees/x/src"),
            None,
            nested,
            SidebarWorktreeKind::Worktree,
            "x",
            false,
            Some(nested),
            false,
        ),
        (
            "sibling home",
            RootClass::Repo,
            Some(root),
            vec!["/repo/x"],
            None,
            Some("/repo/x/src"),
            None,
            "/repo/x",
            SidebarWorktreeKind::Worktree,
            "x",
            false,
            Some("/repo/x"),
            false,
        ),
        (
            "no project means no foreign",
            RootClass::Repo,
            None,
            vec![],
            Some(foreign),
            Some(foreign),
            Some("main"),
            foreign,
            SidebarWorktreeKind::Worktree,
            "main",
            false,
            Some(foreign),
            false,
        ),
        (
            "branchless agent creates no place",
            RootClass::Repo,
            Some(root),
            vec![],
            None,
            Some(foreign),
            None,
            "external",
            SidebarWorktreeKind::External,
            "external",
            false,
            None,
            false,
        ),
    ] {
        let mut snapshot = room(Vec::new())
            .with_root_class(class)
            .with_project_root(project.map(PathBuf::from))
            .with_worktree_home(Some(PathBuf::from("/repo/rimz-worktrees")));
        snapshot.worktree_roots = listed.into_iter().map(PathBuf::from).collect();
        snapshot.worktree_channels =
            BTreeMap::from([(PathBuf::from(marked), "link-notices".to_owned())]);
        let entry = GroupEntry {
            channel: None,
            path,
            branch,
        };
        let entries = std::iter::once(entry).chain(row_root.map(|path| GroupEntry {
            channel: None,
            path: Some(path),
            branch: Some("main"),
        }));
        let resolver = GroupResolver::new(&snapshot, entries);
        let group = resolver.resolve(entry);
        actual.push((
            name,
            group.key,
            group.kind,
            group.label,
            group.root_lane,
            group.place.map(|place| (place.path, place.foreign)),
        ));
        expected.push((
            name,
            key.to_owned(),
            kind,
            label.to_owned(),
            root_lane,
            place.map(|path| (PathBuf::from(path), is_foreign)),
        ));
    }
    assert_eq!(actual, expected);
}

#[test]
fn lane_table_uses_the_marker_place_independently_of_the_rows_path() {
    let root = "/repo/rimz";
    let marked = "/repo/rimz-worktrees/link-notices";
    let mut snapshot = room(Vec::new()).with_project_root(Some(PathBuf::from(root)));
    snapshot.worktree_roots = vec![PathBuf::from(marked)];
    snapshot.worktree_channels =
        BTreeMap::from([(PathBuf::from(marked), "link-notices".to_owned())]);
    let resolver = GroupResolver::new(&snapshot, []);
    let actual = [
        (Some("docs"), Some(root)),
        (Some("link-notices"), Some("/elsewhere")),
        (Some("link-notices"), None),
        (None, Some(marked)),
        (Some(""), Some(marked)),
    ]
    .map(|(channel, path)| {
        let group = resolver.resolve(GroupEntry {
            channel,
            path,
            branch: None,
        });
        (
            group.key,
            group.kind,
            group.label,
            group.root_lane,
            group.place.map(|place| (place.path, place.foreign)),
        )
    });
    let marked_place = Some((PathBuf::from(marked), false));
    assert_eq!(
        actual,
        [
            (
                "channel:docs".to_owned(),
                SidebarWorktreeKind::Channel,
                "docs".to_owned(),
                false,
                None
            ),
            (
                "channel:link-notices".to_owned(),
                SidebarWorktreeKind::Channel,
                "link-notices".to_owned(),
                false,
                marked_place.clone()
            ),
            (
                "channel:link-notices".to_owned(),
                SidebarWorktreeKind::Channel,
                "link-notices".to_owned(),
                false,
                marked_place.clone()
            ),
            (
                "channel:link-notices".to_owned(),
                SidebarWorktreeKind::Channel,
                "link-notices".to_owned(),
                false,
                marked_place.clone()
            ),
            (
                "channel:link-notices".to_owned(),
                SidebarWorktreeKind::Channel,
                "link-notices".to_owned(),
                false,
                marked_place
            ),
        ]
    );
}

#[test]
fn duplicate_marker_names_choose_one_lexicographic_place_for_every_row() {
    let mut snapshot = room(Vec::new()).with_project_root(Some(PathBuf::from("/repo")));
    snapshot.worktree_roots = vec![PathBuf::from("/z/topic"), PathBuf::from("/a/topic")];
    snapshot.worktree_channels = snapshot
        .worktree_roots
        .iter()
        .map(|path| (path.clone(), "topic".to_owned()))
        .collect();
    let resolver = GroupResolver::new(&snapshot, []);
    let identities = [
        GroupEntry {
            channel: Some("topic"),
            path: None,
            branch: None,
        },
        GroupEntry {
            channel: None,
            path: Some("/z/topic/src"),
            branch: None,
        },
        GroupEntry {
            channel: None,
            path: Some("/a/topic/src"),
            branch: None,
        },
    ]
    .map(|entry| {
        let identity = resolver.resolve(entry);
        (identity.key, identity.place.map(|place| place.path))
    });
    assert_eq!(
        identities,
        std::array::from_fn(|_| ("channel:topic".to_owned(), Some(PathBuf::from("/a/topic"))))
    );
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
        expect_root_lane: bool,
    }

    let root = "/repo/rimz";
    let external = "/elsewhere/feature-wt";
    let mut root_lanes = Vec::new();
    let mut expected_root_lanes = Vec::new();
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
            expect_root_lane: true,
        },
        Case {
            label: "in-project worktree path keeps its own pod",
            project_root: Some(root),
            worktree_roots: vec!["/repo/rimz/.claude/worktrees/featureX"],
            panes: vec![pane("%1", "zsh", "/repo/rimz/.claude/worktrees/featureX")],
            expect_kind: SidebarWorktreeKind::Worktree,
            expect_key: "/repo/rimz/.claude/worktrees/featureX",
            expect_label: "featureX",
            expect_rows: 1,
            expect_root_lane: false,
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
            expect_root_lane: false,
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
            expect_root_lane: false,
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
            expect_root_lane: false,
        },
        Case {
            label: "no project root leaves a shell external",
            project_root: None,
            worktree_roots: Vec::new(),
            panes: vec![pane("%1", "zsh", "/home/user")],
            expect_kind: SidebarWorktreeKind::External,
            expect_key: "external",
            expect_label: "external",
            expect_rows: 1,
            expect_root_lane: false,
        },
    ] {
        let snapshot = room(Vec::new())
            .with_project_root(case.project_root.map(PathBuf::from))
            .with_worktree_roots(
                case.worktree_roots.into_iter().map(PathBuf::from).collect(),
                BTreeMap::new(),
            )
            .with_live_panes(case.panes, None);

        assert_eq!(snapshot.worktree_groups.len(), 1, "{}", case.label);
        let group = &snapshot.worktree_groups[0];
        assert_eq!(group.kind, case.expect_kind, "{}", case.label);
        assert_eq!(group.key, case.expect_key, "{}", case.label);
        assert_eq!(group.label, case.expect_label, "{}", case.label);
        assert_eq!(group.rows.len(), case.expect_rows, "{}", case.label);
        root_lanes.push((case.label, group.root_lane));
        expected_root_lanes.push((case.label, case.expect_root_lane));
    }
    assert_eq!(root_lanes, expected_root_lanes);
}

#[test]
fn every_branch_split_at_the_project_root_is_root_lane() {
    let root = "/repo/shared";
    let snapshot = room(vec![
        agent_in("feature", root, AgentStatus::Idle, 0)
            .branch("feature")
            .in_pane("%1"),
        agent_in("main", root, AgentStatus::Idle, 0)
            .branch("main")
            .in_pane("%2"),
    ])
    .with_project_root(Some(PathBuf::from(root)))
    .with_live_panes(
        vec![pane("%1", "claude", root), pane("%2", "claude", root)],
        None,
    );

    assert_eq!(
        snapshot
            .worktree_groups
            .iter()
            .map(|group| (group.key.as_str(), group.label.as_str(), group.root_lane))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            ("/repo/shared\nfeature", "feature", true),
            ("/repo/shared\nmain", "main", true),
        ])
    );
}

#[test]
fn same_branch_in_distinct_paths_keeps_distinct_group_identity() {
    let snapshot = room_with_agent_panes(vec![
        agent("claude", "rimz", AgentStatus::Running, 10)
            .worktree("/workspace/rimz")
            .branch("main"),
        agent("codex", "agents", AgentStatus::Running, 20)
            .worktree("/home/me/.agents")
            .branch("main"),
    ]);

    assert_eq!(snapshot.worktree_groups.len(), 2);
    assert_eq!(
        snapshot
            .worktree_groups
            .iter()
            .map(|group| (group.key.as_str(), group.label.as_str()))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([("/home/me/.agents", "main"), ("/workspace/rimz", "main"),])
    );
}

#[test]
fn unstamped_marked_worktree_rows_fold_into_channel_pods() {
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

    let mut snapshot = room(vec![stamped, unstamped])
        .with_project_root(Some(project_root))
        .with_worktree_home(Some(worktree_home));
    snapshot.worktree_roots = vec![PathBuf::from(worktree)];
    snapshot.worktree_channels =
        BTreeMap::from([(PathBuf::from(worktree), "message-channel".to_owned())]);
    let snapshot = snapshot.with_live_panes(
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
    assert_eq!(
        group.place.as_ref().map(|place| place.path.as_path()),
        Some(std::path::Path::new(worktree))
    );
}

#[test]
fn lone_unstamped_marked_worktree_row_uses_channel_pod() {
    let worktree = "/repo/rimz-worktrees/message-channel";
    let mut snapshot = room(vec![
        agent("claude", "bare", AgentStatus::Running, 20)
            .worktree(worktree)
            .in_pane("%1"),
    ])
    .with_project_root(Some(PathBuf::from("/repo/rimz")))
    .with_worktree_home(Some(PathBuf::from("/repo/rimz-worktrees")));
    snapshot.worktree_roots = vec![PathBuf::from(worktree)];
    snapshot.worktree_channels =
        BTreeMap::from([(PathBuf::from(worktree), "message-channel".to_owned())]);
    let snapshot = snapshot.with_live_panes(vec![pane("%1", "claude", worktree)], None);

    let group = snapshot.worktree_groups.first().expect("a group");
    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert_eq!(group.kind, SidebarWorktreeKind::Channel);
    assert_eq!(group.key, "channel:message-channel");
    assert_eq!(group.label, "message-channel");
    assert_eq!(
        group.place.as_ref().map(|place| place.path.as_path()),
        Some(std::path::Path::new(worktree))
    );
}

#[test]
fn worktree_home_does_not_admit_unlisted_shell_paths() {
    for (label, worktree_home, path, project_root, expect_kind, expect_key, expect_label) in [
        (
            "pure reducer leaves an unlisted shell external",
            None,
            "/repo/rimz-worktrees/message-channel",
            None,
            SidebarWorktreeKind::External,
            "external",
            "external",
        ),
        (
            "path outside worktree home stays external",
            Some("/repo/rimz-worktrees"),
            "/tmp/scratch",
            None,
            SidebarWorktreeKind::External,
            "external",
            "external",
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
            "normalized worktree home does not admit an unlisted shell",
            Some("/repo/rimz/../rimz-worktrees"),
            "/repo/rimz-worktrees/message-channel",
            Some("/repo/rimz"),
            SidebarWorktreeKind::External,
            "external",
            "external",
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
        let snapshot = room(Vec::new())
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
