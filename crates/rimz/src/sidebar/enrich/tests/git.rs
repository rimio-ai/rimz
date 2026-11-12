//! Git-fact projection onto groups: diff stats, PR links, and the worktree
//! marker that gives a channel its worktree identity.

use super::*;

#[test]
fn cached_fold_reads_group_roots_from_their_own_lane() {
    let (dir, runtime, snapshot) = runtime();
    let roots = vec![dir.path().join("external-checkout")];
    atomic::write_temp_then_rename_cache(
        &runtime.group_roots_path(),
        &serde_json::json!({
            "refreshed_at_ms": unix_now_ms(),
            "roots": roots,
            "marker_names": { roots[0].display().to_string(): "feature" }
        }),
    )
    .unwrap();
    let snapshot = snapshot.with_project_root(Some(dir.path().to_path_buf()));

    let folded = fold_cached(snapshot.clone(), None, &runtime);
    assert_eq!(folded.worktree_roots, roots);
    assert_eq!(
        folded.worktree_channels,
        BTreeMap::from([(roots[0].clone(), "feature".to_owned())])
    );
    std::fs::remove_file(runtime.group_roots_path()).unwrap();
    assert!(
        fold_cached(snapshot, None, &runtime)
            .worktree_roots
            .is_empty()
    );
}

#[test]
fn marked_worktree_subfolder_shells_keep_one_place_and_its_git_facts() {
    let (dir, runtime, snapshot) = runtime();
    let worktree = dir.path().join("worktrees/link-notices");
    let subfolder = worktree.join(".rimz");
    std::fs::create_dir_all(&subfolder).unwrap();
    let roots = vec![dir.path().to_path_buf(), worktree.clone()];
    let channels = BTreeMap::from([(worktree.clone(), "link-notices".to_owned())]);
    atomic::write_temp_then_rename_cache(
        &runtime.group_roots_path(),
        &serde_json::json!({
            "refreshed_at_ms": unix_now_ms(), "roots": roots, "marker_names": channels
        }),
    )
    .unwrap();
    let mut diff = DiffStatsCache::default();
    diff.entries.insert(
        worktree.display().to_string(),
        DiffStatsCacheEntry {
            added: Some(17),
            removed: Some(3),
            ..Default::default()
        },
    );
    atomic::write_temp_then_rename_cache(&runtime.diff_stats_path(), &diff).unwrap();
    let mut prs = crate::forge::pr_state::PrStateCache::default();
    prs.states.insert(
        worktree.display().to_string(),
        PrLink {
            open: None,
            stack: Default::default(),
            branch: None,
            incarnation: None,
            state: WorktreePrState::Open,
            number: Some(91),
            url: None,
            ci: None,
            ci_sha: None,
            merge_sha: None,
        },
    );
    atomic::write_temp_then_rename_cache(&runtime.pr_state_path(), &prs).unwrap();
    let now = snapshot.now;
    for with_agent in [true, false] {
        for fresh in [true, false] {
            let mut agent = root_agent("claude", "stamped", None);
            agent.channel = Some("link-notices".to_owned());
            agent.worktree_path = Some(worktree.display().to_string());
            agent.worktree_branch = Some("link-notices".to_owned());
            agent.pane = Some(pane("%1", "claude", worktree.to_str().unwrap()));
            let mut panes = vec![pane("%2", "zsh", subfolder.to_str().unwrap())];
            if with_agent {
                panes.push(agent.pane.clone().unwrap());
            }
            let snapshot = SidebarSnapshot::build_with_agents(
                snapshot.workspace_id.clone(),
                if with_agent { vec![agent] } else { Vec::new() },
                now,
            )
            .with_project_root(Some(dir.path().to_path_buf()))
            .with_live_panes(panes.clone(), None);
            let frame = crate::sidebar::frame::assemble_frame(panes, unix_now_ms(), "test");
            let mut opts = cached_opts();
            if fresh {
                opts.fresh_roots = Some((roots.clone(), channels.clone()));
            }
            let folded = fold(snapshot, Some(&frame), &runtime, opts);
            assert_eq!(folded.worktree_channels, channels);
            assert_eq!(folded.worktree_groups.len(), 1);
            let group = &folded.worktree_groups[0];
            assert_eq!(group.key, "channel:link-notices");
            assert_eq!(
                group.place.as_ref().map(|place| place.path.as_path()),
                Some(worktree.as_path())
            );
            assert!(group.worktree_backed());
            assert_eq!((group.diff_added, group.diff_removed), (Some(17), Some(3)));
            assert_eq!(group.pr_number, Some(91));
        }
    }
}

#[test]
fn an_open_pr_on_a_listed_worktree_holds_a_rowless_group_until_it_ends() {
    let (dir, runtime, snapshot) = runtime();
    let snapshot = snapshot.with_project_root(Some(dir.path().to_path_buf()));
    let [feature, trunk, unlisted] = ["feature", "trunk", "unlisted"].map(|name| {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    });
    atomic::write_temp_then_rename_cache(
        &runtime.group_roots_path(),
        &serde_json::json!({
            "refreshed_at_ms": unix_now_ms(),
            "roots": [dir.path(), &feature, &trunk],
            "marker_names": {}
        }),
    )
    .unwrap();
    let mut diff = DiffStatsCache::default();
    diff.entries.insert(
        trunk.display().to_string(),
        DiffStatsCacheEntry {
            branch: Some("main".to_owned()),
            trunk: Some("origin/main".to_owned()),
            ..Default::default()
        },
    );
    atomic::write_temp_then_rename_cache(&runtime.diff_stats_path(), &diff).unwrap();
    let frame = crate::sidebar::frame::assemble_frame(
        vec![pane("%1", "zsh", dir.path().to_str().unwrap())],
        unix_now_ms(),
        "test",
    );
    let links = |state| {
        let link = PrLink {
            open: Some(crate::forge::pr_state::OpenPrFacts {
                head: "head".to_owned(),
                base: None,
                behind_by: None,
                mergeability: Some(crate::forge::pr_state::SettledMergeability::Conflicting(
                    "head".to_owned(),
                )),
                auto_merge: false,
                review: None,
                queue: None,
            }),
            stack: Default::default(),
            branch: None,
            incarnation: None,
            state,
            number: Some(1614),
            url: Some("https://github.com/org/repo/pull/1614".to_owned()),
            ci: Some(WorktreeCi::Failing),
            ci_sha: None,
            merge_sha: None,
        };
        [&feature, &trunk, &unlisted]
            .map(|path| (path.display().to_string(), link.clone()))
            .into()
    };
    let pr_groups = |state, producing: bool| {
        let pr_states: BTreeMap<_, _> = links(state);
        let folded = if producing {
            let lanes = crate::sidebar::refresh::RefreshedLanes {
                spending: Default::default(),
                accounts: BTreeMap::new(),
                pr_states,
                branch_ci: BTreeMap::new(),
            };
            let mut opts = cached_opts();
            opts.lanes = Some(&lanes);
            fold(snapshot.clone(), Some(&frame), &runtime, opts)
        } else {
            let cache = crate::forge::pr_state::PrStateCache {
                states: pr_states,
                ..Default::default()
            };
            atomic::write_temp_then_rename_cache(&runtime.pr_state_path(), &cache).unwrap();
            fold_cached(snapshot.clone(), Some(&frame), &runtime)
        };
        assert!(folded.worktree_groups.iter().any(|group| group.root_lane));
        folded
            .worktree_groups
            .iter()
            .filter(|group| !group.root_lane)
            .cloned()
            .collect::<Vec<_>>()
    };
    for producing in [false, true, false] {
        let groups = pr_groups(WorktreePrState::Open, producing);
        let facts = groups
            .iter()
            .map(|group| {
                (
                    group.place.as_ref().map(|place| place.path.clone()),
                    group.rows.len(),
                    group.inactive,
                    group.pr_state,
                    group.pr_number,
                    group.ci,
                    group.pr_conflicting,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            facts,
            [(
                Some(feature.clone()),
                0,
                None,
                Some(WorktreePrState::Open),
                Some(1614),
                Some(WorktreeCi::Failing),
                true
            )],
            "only the listed non-trunk worktree is held (producing: {producing})"
        );
        for ended in [WorktreePrState::Merged, WorktreePrState::Closed] {
            assert!(pr_groups(ended, producing).is_empty(), "{ended:?}");
        }
    }
}

#[test]
fn cached_fold_reads_channel_markers_from_the_roots_lane() {
    let (dir, runtime, mut snapshot) = runtime();
    let worktree = dir.path().join("feature");
    snapshot = snapshot.with_project_root(Some(dir.path().to_path_buf()));
    let frame = crate::sidebar::frame::assemble_frame(
        vec![pane("%1", "zsh", worktree.to_str().unwrap())],
        unix_now_ms(),
        "test",
    );
    atomic::write_temp_then_rename_cache(
        &runtime.group_roots_path(),
        &serde_json::json!({
            "refreshed_at_ms": unix_now_ms(),
            "roots": [worktree],
            "marker_names": { worktree.display().to_string(): "feature" }
        }),
    )
    .unwrap();
    let mut diff = DiffStatsCache::default();
    diff.entries.insert(
        worktree.display().to_string(),
        diff_entry(false, false, Some(true), 0, 0),
    );
    atomic::write_temp_then_rename_cache(&runtime.diff_stats_path(), &diff).unwrap();
    let mut prs = crate::forge::pr_state::PrStateCache::default();
    prs.states.insert(
        worktree.display().to_string(),
        PrLink {
            open: None,
            stack: Default::default(),
            branch: None,
            incarnation: None,
            state: WorktreePrState::Merged,
            number: Some(91),
            url: None,
            ci: None,
            ci_sha: None,
            merge_sha: None,
        },
    );
    atomic::write_temp_then_rename_cache(&runtime.pr_state_path(), &prs).unwrap();

    let folded = fold_cached(snapshot.clone(), Some(&frame), &runtime);
    let group = &folded.worktree_groups[0];
    assert!(
        group.worktree_backed(),
        "channel identity comes from the roots lane"
    );
    assert_eq!(group.diff_added, Some(0));
    assert_eq!(group.pr_state, Some(WorktreePrState::Merged));
    std::fs::remove_file(runtime.group_roots_path()).unwrap();
    let folded = fold_cached(snapshot, Some(&frame), &runtime);
    assert!(!folded.worktree_groups[0].worktree_backed());
    assert_eq!(folded.worktree_groups[0].pr_state, None);
}

#[test]
fn pr_stack_projection_matches_cache_and_clears_stale_groups() {
    let (dir, runtime, mut snapshot) = runtime();
    let worktree = dir.path().join("feature");
    std::fs::create_dir_all(&worktree).unwrap();
    snapshot.worktree_groups = vec![worktree_group(&worktree, Vec::new())].into();
    let path = worktree.display().to_string();
    let stack = crate::store::snapshot::PrStack {
        below: vec![crate::store::snapshot::StackPr {
            number: 522,
            url: Some("https://github.com/org/repo/pull/522".to_owned()),
        }],
        above: Vec::new(),
    };
    let mut cache = crate::forge::pr_state::PrStateCache::default();
    cache.states.insert(
        path.clone(),
        PrLink {
            open: None,
            stack: stack.clone(),
            branch: Some("feature".to_owned()),
            incarnation: None,
            state: WorktreePrState::Open,
            number: Some(530),
            url: None,
            ci: None,
            ci_sha: None,
            merge_sha: None,
        },
    );
    let mut cached = snapshot.clone();
    project_pr_state_map(
        &mut snapshot,
        &cache.states,
        &cache.branch_ci,
        &DiffStatsCache::default(),
    );
    assert_eq!(snapshot.worktree_groups[0].pr_stack, stack);
    atomic::write_temp_then_rename_cache(&runtime.pr_state_path(), &cache).unwrap();
    let published = crate::forge::pr_state::read_pr_state_cache(&runtime.pr_state_path());
    project_pr_state_map(
        &mut cached,
        &published.states,
        &published.branch_ci,
        &DiffStatsCache::default(),
    );
    assert_eq!(cached.worktree_groups[0].pr_stack, stack);
    project_pr_state_map(
        &mut snapshot,
        &BTreeMap::new(),
        &BTreeMap::new(),
        &DiffStatsCache::default(),
    );
    assert!(snapshot.worktree_groups[0].pr_stack.is_empty());
    let mut diff = DiffStatsCache::default();
    diff.entries.insert(
        path,
        DiffStatsCacheEntry {
            branch: Some("main".to_owned()),
            trunk: Some("origin/main".to_owned()),
            ..Default::default()
        },
    );
    project_pr_state_map(&mut cached, &cache.states, &cache.branch_ci, &diff);
    assert!(cached.worktree_groups[0].pr_stack.is_empty());
}

#[test]
fn pr_state_projection_uses_the_given_map() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path().join("feature");
    std::fs::create_dir_all(&worktree).unwrap();
    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(dir.path()),
        Vec::new(),
        Timestamp::now(),
    );
    snapshot.worktree_groups = vec![worktree_group(&worktree, Vec::new())].into();
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].pr_number = Some(69);

    let branch_ci = BTreeMap::from([(
        worktree.display().to_string(),
        crate::store::snapshot::WorktreeCi::Failing,
    )]);
    let mut states = BTreeMap::new();
    states.insert(
        worktree.display().to_string(),
        PrLink {
            open: None,
            stack: Default::default(),
            branch: None,
            incarnation: None,
            state: crate::store::snapshot::WorktreePrState::Closed,
            number: Some(91),
            url: Some("https://github.com/org/repo/pull/91".to_owned()),
            ci: Some(crate::store::snapshot::WorktreeCi::Failing),
            ci_sha: None,
            merge_sha: None,
        },
    );
    project_pr_state_map(
        &mut snapshot,
        &states,
        &branch_ci,
        &DiffStatsCache::default(),
    );
    assert_eq!(
        snapshot.worktree_groups[0].pr_state,
        Some(crate::store::snapshot::WorktreePrState::Closed)
    );
    assert_eq!(snapshot.worktree_groups[0].pr_number, Some(91));
    assert_eq!(
        snapshot.worktree_groups[0].pr_url.as_deref(),
        Some("https://github.com/org/repo/pull/91")
    );
    assert_eq!(snapshot.worktree_groups[0].ci, None);

    states.insert(
        worktree.display().to_string(),
        PrLink {
            open: Some(crate::forge::pr_state::OpenPrFacts {
                head: "head".to_owned(),
                base: None,
                behind_by: None,
                mergeability: None,
                auto_merge: false,
                review: None,
                queue: Some(crate::forge::pr_state::PrQueueFact::Dequeued {
                    at: "2026-10-03T12:46:03Z".to_owned(),
                    reason: None,
                    commit: None,
                }),
            }),
            stack: Default::default(),
            branch: None,
            incarnation: None,
            state: crate::store::snapshot::WorktreePrState::Open,
            number: Some(91),
            url: Some("https://github.com/org/repo/pull/91".to_owned()),
            ci: Some(crate::store::snapshot::WorktreeCi::Passing),
            ci_sha: None,
            merge_sha: None,
        },
    );
    project_pr_state_map(
        &mut snapshot,
        &states,
        &branch_ci,
        &DiffStatsCache::default(),
    );
    assert_eq!(
        snapshot.worktree_groups[0].ci,
        Some(crate::store::snapshot::WorktreeCi::Passing)
    );
    assert_eq!(
        snapshot.worktree_groups[0].pr_queue,
        Some(crate::store::snapshot::WorktreePrQueue::Dequeued)
    );

    let mut readings = Vec::new();
    let mut expected = Vec::new();
    for (case, settled_head, trunk, linked, checkout, conflicting) in [
        ("current", Some("head"), false, true, true, true),
        ("stale", Some("older"), false, true, true, false),
        ("unknown", None, false, true, true, false),
        ("trunk", Some("head"), true, true, true, false),
        ("lost link", Some("head"), false, false, true, false),
        ("lost checkout", Some("head"), false, true, false, false),
    ] {
        let mut projected = snapshot.clone();
        let group = &mut Arc::make_mut(&mut projected.worktree_groups)[0];
        group.pr_conflicting = !conflicting;
        if !checkout {
            group.place = None;
        }
        let mut links = states.clone();
        let link = links.get_mut(&worktree.display().to_string()).unwrap();
        link.open.as_mut().unwrap().mergeability = settled_head
            .map(|head| crate::forge::pr_state::SettledMergeability::Conflicting(head.to_owned()));
        if !linked {
            links.clear();
        }
        let mut diff = DiffStatsCache::default();
        if trunk {
            diff.entries.insert(
                worktree.display().to_string(),
                DiffStatsCacheEntry {
                    branch: Some("main".to_owned()),
                    trunk: Some("origin/main".to_owned()),
                    ..Default::default()
                },
            );
        }
        project_pr_state_map(&mut projected, &links, &branch_ci, &diff);
        readings.push((case, projected.worktree_groups[0].pr_conflicting));
        expected.push((case, conflicting));
    }
    assert_eq!(readings, expected);

    for state in ["active", "parked"] {
        let mut rowless = snapshot.clone();
        let group = &mut Arc::make_mut(&mut rowless.worktree_groups)[0];
        group.key = "channel:feature".into();
        group.label = "feature".into();
        group.kind = SidebarWorktreeKind::Channel;
        group.place = Some(crate::store::snapshot::SidebarPlace {
            path: worktree.clone(),
            foreign: false,
        });
        group.rows = Vec::new().into();
        group.workflow = Some(
            serde_json::from_value(serde_json::json!({
                "run": crate::ids::WorkflowRunId::new(), "workflow":"land", "scope_path":worktree,
                "state":state, "track":[],
                "current":{"name":"watch", "kind":"check", "reached":false},
                "detour_parent":null, "visits":0, "capped":false, "word":"queued",
                "since":rowless.now, "hold_limit":null, "entered_at":rowless.now,
                "created_at":rowless.now, "agent":null
            }))
            .unwrap(),
        );
        group.pr_state = None;
        group.pr_queue = None;
        group.ci = None;
        project_pr_state_map(
            &mut rowless,
            &states,
            &branch_ci,
            &DiffStatsCache::default(),
        );
        let group = &rowless.worktree_groups[0];
        assert_eq!(group.pr_state, Some(WorktreePrState::Open));
        assert_eq!(group.pr_number, Some(91));
        assert_eq!(group.ci, Some(WorktreeCi::Passing));
        assert_eq!(group.pr_queue, Some(WorktreePrQueue::Dequeued));
    }

    states.insert(
        worktree.display().to_string(),
        PrLink {
            open: None,
            stack: Default::default(),
            branch: None,
            incarnation: None,
            state: crate::store::snapshot::WorktreePrState::Merged,
            number: Some(91),
            url: Some("https://github.com/org/repo/pull/91".to_owned()),
            ci: Some(crate::store::snapshot::WorktreeCi::Failing),
            ci_sha: None,
            merge_sha: Some("merged-sha".to_owned()),
        },
    );
    project_pr_state_map(
        &mut snapshot,
        &states,
        &branch_ci,
        &DiffStatsCache::default(),
    );
    assert_eq!(
        snapshot.worktree_groups[0].ci,
        Some(crate::store::snapshot::WorktreeCi::Failing)
    );
    assert_eq!(snapshot.worktree_groups[0].pr_queue, None);

    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].pr_number = Some(69);
    project_pr_state_map(
        &mut snapshot,
        &BTreeMap::new(),
        &branch_ci,
        &DiffStatsCache::default(),
    );
    assert_eq!(snapshot.worktree_groups[0].pr_state, None);
    assert_eq!(snapshot.worktree_groups[0].pr_number, Some(69));
    assert_eq!(snapshot.worktree_groups[0].pr_url, None);
    // A non-trunk branch without a PR does not display its branch CI.
    assert_eq!(snapshot.worktree_groups[0].ci, None);
}

#[test]
fn pr_state_projection_reaches_marked_worktree_channels() {
    let (_dir, worktree, mut snapshot) = channel_snapshot("feature", true);
    std::fs::remove_dir_all(worktree.join(".git")).unwrap();

    let mut states = BTreeMap::new();
    states.insert(
        worktree.display().to_string(),
        PrLink {
            open: None,
            stack: Default::default(),
            branch: None,
            incarnation: None,
            state: crate::store::snapshot::WorktreePrState::Merged,
            number: Some(91),
            url: None,
            ci: None,
            ci_sha: None,
            merge_sha: Some("merged-sha".to_owned()),
        },
    );
    project_pr_state_map(
        &mut snapshot,
        &states,
        &BTreeMap::new(),
        &DiffStatsCache::default(),
    );
    assert_eq!(
        snapshot.worktree_groups[0].pr_state,
        Some(crate::store::snapshot::WorktreePrState::Merged)
    );
}

#[test]
fn pr_state_projection_leaves_unmarked_channels_plain() {
    let (_dir, worktree, mut snapshot) = channel_snapshot("feature", false);

    let mut states = BTreeMap::new();
    states.insert(
        worktree.display().to_string(),
        PrLink {
            open: None,
            stack: Default::default(),
            branch: None,
            incarnation: None,
            state: crate::store::snapshot::WorktreePrState::Merged,
            number: Some(91),
            url: None,
            ci: None,
            ci_sha: None,
            merge_sha: Some("merged-sha".to_owned()),
        },
    );
    project_pr_state_map(
        &mut snapshot,
        &states,
        &BTreeMap::new(),
        &DiffStatsCache::default(),
    );
    assert_eq!(snapshot.worktree_groups[0].pr_state, None);
}

#[test]
fn pr_state_projection_keeps_trunk_pr_free_but_projects_branch_ci() {
    let dir = tempfile::tempdir().unwrap();
    let trunk = dir.path().join("trunk");
    let feature = dir.path().join("feature");
    std::fs::create_dir_all(&trunk).unwrap();
    std::fs::create_dir_all(&feature).unwrap();
    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(dir.path()),
        Vec::new(),
        Timestamp::now(),
    );
    snapshot.worktree_groups = vec![
        worktree_group(&trunk, Vec::new()),
        worktree_group(&feature, Vec::new()),
    ]
    .into();
    let mut states = BTreeMap::new();
    for (path, branch, number) in [(&trunk, "old-feature", 90), (&feature, "feature", 91)] {
        states.insert(
            path.display().to_string(),
            PrLink {
                open: None,
                stack: Default::default(),
                branch: Some(branch.to_owned()),
                incarnation: None,
                state: crate::store::snapshot::WorktreePrState::Merged,
                number: Some(number),
                url: Some(format!("https://github.com/org/repo/pull/{number}")),
                ci: Some(crate::store::snapshot::WorktreeCi::Passing),
                ci_sha: None,
                merge_sha: Some(format!("sha-{number}")),
            },
        );
    }
    let mut diff_cache = DiffStatsCache::default();
    diff_cache.entries.insert(
        trunk.display().to_string(),
        DiffStatsCacheEntry {
            branch: Some("main".to_owned()),
            trunk: Some("origin/main".to_owned()),
            ..DiffStatsCacheEntry::default()
        },
    );
    diff_cache.entries.insert(
        feature.display().to_string(),
        DiffStatsCacheEntry {
            branch: Some("feature".to_owned()),
            trunk: Some("origin/main".to_owned()),
            ..DiffStatsCacheEntry::default()
        },
    );

    let branch_ci = BTreeMap::from([(
        trunk.display().to_string(),
        crate::store::snapshot::WorktreeCi::Passing,
    )]);
    project_pr_state_map(&mut snapshot, &states, &branch_ci, &diff_cache);

    let trunk = &snapshot.worktree_groups[0];
    assert_eq!(trunk.pr_state, None);
    assert_eq!(trunk.pr_number, None);
    assert_eq!(trunk.pr_url, None);
    assert_eq!(trunk.ci, Some(crate::store::snapshot::WorktreeCi::Passing));
    let feature = &snapshot.worktree_groups[1];
    assert_eq!(
        feature.pr_state,
        Some(crate::store::snapshot::WorktreePrState::Merged)
    );
    assert_eq!(feature.pr_number, Some(91));
    assert_eq!(
        feature.pr_url.as_deref(),
        Some("https://github.com/org/repo/pull/91")
    );
}

#[test]
fn diff_projection_keeps_worktree_channel_label_and_uses_live_branch() {
    let (_dir, worktree, mut snapshot) = channel_snapshot("codex-resets", true);

    let mut cache = DiffStatsCache::default();
    let mut entry = diff_entry(true, true, Some(true), 0, 0);
    entry.branch = Some("main".to_owned());
    entry.from_pr = Some(69);
    cache.entries.insert(worktree.display().to_string(), entry);
    std::fs::remove_dir_all(worktree.join(".git")).unwrap();

    project_diff_stats(&mut snapshot, &cache);

    let group = &snapshot.worktree_groups[0];
    assert_eq!(group.label, "codex-resets");
    assert_eq!(group.pr_number, Some(69));
    assert_eq!(
        group.trunk_sync, None,
        "trunk detection uses the live branch, not the channel label"
    );
}

#[test]
fn diff_projection_marks_worktree_channel_before_git_facts_arrive() {
    let (_dir, worktree, mut snapshot) = channel_snapshot("codex-resets", true);
    std::fs::remove_dir_all(worktree.join(".git")).unwrap();
    assert!(snapshot.worktree_groups[0].worktree_backed());

    project_diff_stats(&mut snapshot, &DiffStatsCache::default());

    let group = &snapshot.worktree_groups[0];
    assert!(
        group.worktree_backed(),
        "marker-backed channel keeps worktree identity while git cache is empty"
    );
    assert_eq!(group.trunk, None, "no git facts were projected");
}

#[test]
fn diff_projection_requires_an_exact_channel_marker_name() {
    let (dir, worktree, snapshot) = channel_snapshot("codex-resets", false);
    let mut agent = root_agent("claude", "stamped", None);
    agent.channel = Some("codex-resets".to_owned());
    agent.worktree_path = Some(worktree.display().to_string());
    agent.worktree_branch = Some("main".to_owned());
    let mut snapshot =
        SidebarSnapshot::build_with_agents(snapshot.workspace_id, vec![agent], snapshot.now)
            .with_project_root(Some(dir.path().to_path_buf()))
            .with_worktree_roots(
                vec![worktree.clone()],
                BTreeMap::from([(worktree.clone(), "another-channel".to_owned())]),
            )
            .with_live_panes(vec![pane("%1", "claude", worktree.to_str().unwrap())], None);
    let mut cache = DiffStatsCache::default();
    cache.entries.insert(
        worktree.display().to_string(),
        DiffStatsCacheEntry {
            added: Some(17),
            removed: Some(3),
            ..Default::default()
        },
    );
    project_diff_stats(&mut snapshot, &cache);
    let group = &snapshot.worktree_groups[0];
    assert!(!group.worktree_backed());
    assert_eq!(group.git_checkout(), None);
    assert_eq!(group.diff_added, None);
}

#[test]
fn cached_enrich_resorts_groups_after_git_projection() {
    let (dir, runtime, mut snapshot) = runtime();
    let dirty = dir.path().join("dirty");
    let merged = dir.path().join("merged");
    std::fs::create_dir_all(&dirty).unwrap();
    std::fs::create_dir_all(&merged).unwrap();
    let now = snapshot.now;
    snapshot.worktree_groups = vec![
        worktree_group(
            &merged,
            vec![activity_row(true, Some(AgentStatus::Idle), now, &merged)],
        ),
        worktree_group(
            &dirty,
            vec![activity_row(true, Some(AgentStatus::Idle), now, &dirty)],
        ),
    ]
    .into();

    let mut cache = DiffStatsCache::default();
    let mut dirty_entry = diff_entry(false, false, Some(true), 0, 0);
    dirty_entry.branch = Some("dirty".to_owned());
    let mut merged_entry = diff_entry(true, true, Some(true), 0, 0);
    merged_entry.branch = Some("merged".to_owned());
    cache
        .entries
        .insert(dirty.display().to_string(), dirty_entry);
    cache
        .entries
        .insert(merged.display().to_string(), merged_entry);
    atomic::write_temp_then_rename_cache(&runtime.diff_stats_path(), &cache).unwrap();

    let snapshot = fold_cached(snapshot, None, &runtime);

    assert_eq!(
        snapshot
            .worktree_groups
            .iter()
            .map(|group| group.label.as_str())
            .collect::<Vec<_>>(),
        vec!["dirty", "merged"],
        "cached git facts re-rank the already-built group spine"
    );
}
