//! Which worktree paths the heavy git lane is allowed to touch: the needed
//! set, the activity-keyed hot subset, and the viewed-pane focused subset.
//!
//! These cover `sidebar::refresh::git_stats`, not the fold itself.

use super::*;

/// A minimal agent/process row for the hot-set tests: only the fields
/// `hot_worktree_paths` reads vary.
/// The hot set, boundary-exact at `GIT_ACTIVITY_WINDOW`: running rows are hot
/// regardless of activity age, recent activity is hot through exactly the
/// window, process-only and `External`-kind groups are cold, and a dead dir
/// is excluded just as `needed_worktree_paths` excludes it.
#[test]
fn hot_worktree_paths_keys_on_running_or_recent_agent_rows() {
    let dir = tempfile::tempdir().unwrap();
    let wt = |name: &str| {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    };
    let now = Timestamp::from_second(1_750_000_000).unwrap();
    let window = SignedDuration::try_from(GIT_ACTIVITY_WINDOW).unwrap();
    let stale_activity = now - window - SignedDuration::from_secs(1);

    let running = wt("running");
    let recent = wt("recent");
    let boundary = wt("boundary");
    let idle = wt("idle");
    let procs = wt("procs");
    let external_kind = wt("external-kind");
    let dead = dir.path().join("dead-dir");

    let mut snapshot =
        SidebarSnapshot::build(WorkspaceId::from_project_root(dir.path()), Vec::new(), now);
    snapshot.worktree_groups = vec![
        // Running carries hotness on its own — its activity stamp is stale.
        worktree_group(
            &running,
            vec![activity_row(
                true,
                Some(AgentStatus::Running),
                stale_activity,
                &running,
            )],
        ),
        worktree_group(
            &recent,
            vec![activity_row(
                true,
                Some(AgentStatus::Idle),
                now - SignedDuration::from_secs(1),
                &recent,
            )],
        ),
        // Exactly the window boundary stays hot (<=, matching the TTL gates).
        worktree_group(
            &boundary,
            vec![activity_row(
                true,
                Some(AgentStatus::Idle),
                now - window,
                &boundary,
            )],
        ),
        worktree_group(
            &idle,
            vec![activity_row(
                true,
                Some(AgentStatus::Idle),
                stale_activity,
                &idle,
            )],
        ),
        // A busy process row is not an agent row: cold by definition.
        worktree_group(&procs, vec![activity_row(false, None, now, &procs)]),
        // An External-kind group never reaches the git refresh.
        {
            let mut group = worktree_group(
                &external_kind,
                vec![activity_row(
                    true,
                    Some(AgentStatus::Running),
                    now,
                    &external_kind,
                )],
            );
            group.kind = crate::SidebarWorktreeKind::External;
            group
        },
        // A running agent in a since-removed dir: hot <= needed, so excluded.
        worktree_group(
            &dead,
            vec![activity_row(true, Some(AgentStatus::Running), now, &dead)],
        ),
    ];

    let hot = hot_worktree_paths(&snapshot);

    let path_of = |p: &Path| p.display().to_string();
    assert!(hot.contains(&path_of(&running)));
    assert!(hot.contains(&path_of(&recent)));
    assert!(
        hot.contains(&path_of(&boundary)),
        "boundary-exact: <= window"
    );
    assert!(!hot.contains(&path_of(&idle)), "stale activity is cold");
    assert!(
        !hot.contains(&path_of(&procs)),
        "process rows carry no heat"
    );
    assert!(!hot.contains(&path_of(&external_kind)));
    assert!(!hot.contains(&path_of(&dead)), "hot is a subset of needed");
    assert_eq!(hot.len(), 3);
}

/// A future-stamped row (clock skew between writers) reads as hot — the safe
/// direction, mirroring the saturating TTL convention.
#[test]
fn hot_worktree_paths_treats_future_activity_as_hot() {
    let dir = tempfile::tempdir().unwrap();
    let now = Timestamp::from_second(1_750_000_000).unwrap();
    let mut snapshot =
        SidebarSnapshot::build(WorkspaceId::from_project_root(dir.path()), Vec::new(), now);
    snapshot.worktree_groups = vec![worktree_group(
        dir.path(),
        vec![activity_row(
            true,
            Some(AgentStatus::Idle),
            now + SignedDuration::from_secs(120),
            dir.path(),
        )],
    )];

    assert!(hot_worktree_paths(&snapshot).contains(&dir.path().display().to_string()));
}

#[test]
fn focused_worktree_paths_keys_on_viewed_row_panes() {
    let dir = tempfile::tempdir().unwrap();
    let wt = |name: &str| {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    };
    let focused = wt("focused");
    let background = wt("background");
    let external_kind = wt("external-kind");
    let dead = dir.path().join("dead-dir");
    let now = Timestamp::from_second(1_750_000_000).unwrap();

    let row = |raw: &str, path: &Path| {
        let mut row = activity_row(false, None, now, path);
        row.pane = Some(pane(raw, "zsh", &path.display().to_string()));
        row
    };
    let focused_pane = row("terminal_1", &focused)
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    let dead_pane = pane("terminal_4", "zsh", &dead.display().to_string()).pane_id;
    let mut snapshot =
        SidebarSnapshot::build(WorkspaceId::from_project_root(dir.path()), Vec::new(), now);
    snapshot.viewed_panes = vec![focused_pane, dead_pane];
    snapshot.worktree_groups = vec![
        worktree_group(&focused, vec![row("terminal_1", &focused)]),
        worktree_group(&background, vec![row("terminal_2", &background)]),
        {
            let mut group = worktree_group(&external_kind, vec![row("terminal_3", &external_kind)]);
            group.kind = crate::SidebarWorktreeKind::External;
            group
        },
        worktree_group(&dead, vec![row("terminal_4", &dead)]),
    ];

    let focused_paths = focused_worktree_paths(&snapshot);
    let needed: std::collections::BTreeSet<_> =
        needed_worktree_paths(&snapshot).into_iter().collect();

    assert_eq!(
        focused_paths,
        std::collections::BTreeSet::from([focused.display().to_string()])
    );
    assert!(
        focused_paths.is_subset(&needed),
        "focused paths stay inside live needed worktrees"
    );
}

#[test]
fn root_pod_is_excluded_from_git_reads() {
    // The root pod of a non-repo room is a known non-repo: it never enters
    // the producer's git fan-out, while a git-backed row's resolved worktree
    // pod does.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let child = dir.path().join("query-engine");
    std::fs::create_dir_all(&child).unwrap();
    let root_cwd = dir.path().to_string_lossy().into_owned();
    let child_cwd = child.to_string_lossy().into_owned();
    let child_pane = pane("terminal_1", "claude", &child_cwd);
    let mut agent = root_agent("claude", "child", None);
    agent.pane = Some(child_pane.clone());
    agent.worktree_path = Some(child_cwd.clone());
    agent.worktree_branch = Some("main".to_owned());

    let snapshot = SidebarSnapshot::build_with_agents(workspace, vec![agent], Timestamp::now())
        .with_root_class(crate::workspace::RootClass::Directory)
        .with_project_root(Some(dir.path().to_path_buf()))
        .with_live_panes(vec![pane("terminal_0", "zsh", &root_cwd), child_pane], None);

    let kinds: Vec<SidebarWorktreeKind> = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![SidebarWorktreeKind::Worktree, SidebarWorktreeKind::Root]
    );
    assert_eq!(needed_worktree_paths(&snapshot), vec![child_cwd]);
}
