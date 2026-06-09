use super::*;

// ── Activity-tiered git freshness ───────────────────────────────────────────────

/// The same entry holds different verdicts under the two tiers: stale on the
/// fast TTL is still fresh on the idle one, boundary-exact on both.
#[test]
fn diff_stats_entry_freshness_is_ttl_parameterized() {
    let entry = DiffStatsCacheEntry::new(1_000, None, None, None, None, None, None);
    let fast = DIFF_STATS_TTL.as_millis() as u64;
    let idle = DIFF_STATS_IDLE_TTL.as_millis() as u64;

    assert!(entry.is_fresh_for(1_000 + fast, DIFF_STATS_TTL));
    assert!(!entry.is_fresh_for(1_001 + fast, DIFF_STATS_TTL));
    assert!(entry.is_fresh_for(1_000 + idle, DIFF_STATS_IDLE_TTL));
    assert!(!entry.is_fresh_for(1_001 + idle, DIFF_STATS_IDLE_TTL));
    // The tiering's whole point: a hot-stale entry is idle-fresh, so an idle
    // worktree skips the forks a hot one pays.
    assert!(entry.is_fresh_for(1_001 + fast, DIFF_STATS_IDLE_TTL));
}

#[test]
fn worktree_roots_cache_expires_after_roots_ttl() {
    let cache = WorktreeRootsCache {
        refreshed_at_ms: 1_000,
        roots: Vec::new(),
    };
    let ttl = WORKTREE_ROOTS_TTL.as_millis() as u64;
    assert!(cache.is_fresh(1_000 + ttl));
    assert!(!cache.is_fresh(1_001 + ttl));
    // A clock that ran backwards reads fresh (saturating).
    assert!(cache.is_fresh(500));
}

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

    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(dir.path()),
        Vec::new(),
        Vec::new(),
        now,
    );
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
        // A running agent in a since-removed dir: hot ⊆ needed, so excluded.
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
    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(dir.path()),
        Vec::new(),
        Vec::new(),
        now,
    );
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
fn root_pod_is_excluded_from_git_reads() {
    // The root pod of a non-repo room is a known non-repo: it never enters
    // the producer's git fan-out, while child-repo pods do.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let child = dir.path().join("query-engine");
    std::fs::create_dir_all(&child).unwrap();
    let root_cwd = dir.path().to_string_lossy().into_owned();
    let child_cwd = child.to_string_lossy().into_owned();

    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now())
        .with_root_class(crate::workspace::RootClass::Directory)
        .with_project_root(Some(dir.path().to_path_buf()))
        .with_worktree_roots(vec![child.clone()])
        .with_live_panes(
            vec![
                pane("terminal_0", "zsh", &root_cwd),
                pane("terminal_1", "claude", &child_cwd),
            ],
            None,
        );

    let kinds: Vec<SidebarWorktreeKind> = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![SidebarWorktreeKind::Root, SidebarWorktreeKind::Worktree]
    );
    assert_eq!(needed_worktree_paths(&snapshot), vec![child_cwd]);
}

// --- Presence stamp and the two-mode pane TTL ---
