//! Git-fact projection onto groups: diff stats, PR links, and the worktree
//! marker that gives a channel its worktree identity.

use super::*;

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
    snapshot.worktree_groups = vec![worktree_group(&worktree, Vec::new())];
    snapshot.worktree_groups[0].pr_number = Some(69);

    let mut states = BTreeMap::new();
    states.insert(
        worktree.display().to_string(),
        PrLink {
            state: crate::WorktreePrState::Closed,
            number: Some(91),
            url: Some("https://github.com/org/repo/pull/91".to_owned()),
            ci: Some(crate::WorktreePrCi::Failing),
            merge_sha: None,
        },
    );
    project_pr_state_map(&mut snapshot, &states, &DiffStatsCache::default());
    assert_eq!(
        snapshot.worktree_groups[0].pr_state,
        Some(crate::WorktreePrState::Closed)
    );
    assert_eq!(snapshot.worktree_groups[0].pr_number, Some(91));
    assert_eq!(
        snapshot.worktree_groups[0].pr_url.as_deref(),
        Some("https://github.com/org/repo/pull/91")
    );
    assert_eq!(snapshot.worktree_groups[0].pr_ci, None);

    states.insert(
        worktree.display().to_string(),
        PrLink {
            state: crate::WorktreePrState::Open,
            number: Some(91),
            url: Some("https://github.com/org/repo/pull/91".to_owned()),
            ci: Some(crate::WorktreePrCi::Passing),
            merge_sha: None,
        },
    );
    project_pr_state_map(&mut snapshot, &states, &DiffStatsCache::default());
    assert_eq!(
        snapshot.worktree_groups[0].pr_ci,
        Some(crate::WorktreePrCi::Passing)
    );

    states.insert(
        worktree.display().to_string(),
        PrLink {
            state: crate::WorktreePrState::Merged,
            number: Some(91),
            url: Some("https://github.com/org/repo/pull/91".to_owned()),
            ci: Some(crate::WorktreePrCi::Failing),
            merge_sha: Some("merged-sha".to_owned()),
        },
    );
    project_pr_state_map(&mut snapshot, &states, &DiffStatsCache::default());
    assert_eq!(
        snapshot.worktree_groups[0].pr_ci,
        Some(crate::WorktreePrCi::Failing)
    );

    snapshot.worktree_groups[0].pr_number = Some(69);
    project_pr_state_map(&mut snapshot, &BTreeMap::new(), &DiffStatsCache::default());
    assert_eq!(snapshot.worktree_groups[0].pr_state, None);
    assert_eq!(snapshot.worktree_groups[0].pr_number, Some(69));
    assert_eq!(snapshot.worktree_groups[0].pr_url, None);
}

#[test]
fn pr_state_projection_reaches_marked_worktree_channels() {
    let (_dir, worktree, mut snapshot) = channel_snapshot("feature", true);
    let diff_cache = diff_cache_with_marker(&worktree, "feature");
    std::fs::remove_dir_all(worktree.join(".git")).unwrap();

    let mut states = BTreeMap::new();
    states.insert(
        worktree.display().to_string(),
        PrLink {
            state: crate::WorktreePrState::Merged,
            number: Some(91),
            url: None,
            ci: None,
            merge_sha: Some("merged-sha".to_owned()),
        },
    );
    project_pr_state_map(&mut snapshot, &states, &diff_cache);
    assert_eq!(
        snapshot.worktree_groups[0].pr_state,
        Some(crate::WorktreePrState::Merged)
    );
}

#[test]
fn pr_state_projection_leaves_unmarked_channels_plain() {
    let (_dir, worktree, mut snapshot) = channel_snapshot("feature", false);

    let mut states = BTreeMap::new();
    states.insert(
        worktree.display().to_string(),
        PrLink {
            state: crate::WorktreePrState::Merged,
            number: Some(91),
            url: None,
            ci: None,
            merge_sha: Some("merged-sha".to_owned()),
        },
    );
    project_pr_state_map(&mut snapshot, &states, &DiffStatsCache::default());
    assert_eq!(snapshot.worktree_groups[0].pr_state, None);
}

#[test]
fn diff_projection_keeps_worktree_channel_label_and_uses_live_branch() {
    let (_dir, worktree, mut snapshot) = channel_snapshot("codex-resets", true);

    let mut cache = diff_cache_with_marker(&worktree, "codex-resets");
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
    let (_dir, worktree, mut snapshot) = channel_snapshot("codex-resets", false);

    project_diff_stats(
        &mut snapshot,
        &diff_cache_with_marker(&worktree, "codex-resets"),
    );

    let group = &snapshot.worktree_groups[0];
    assert!(
        group.worktree_backed,
        "marker-backed channel keeps worktree identity while git cache is empty"
    );
    assert_eq!(group.trunk, None, "no git facts were projected");
}

#[test]
fn diff_projection_requires_an_exact_channel_marker_name() {
    let (_dir, worktree, mut snapshot) = channel_snapshot("codex-resets", false);
    snapshot.worktree_groups[0].worktree_backed = true;

    project_diff_stats(
        &mut snapshot,
        &diff_cache_with_marker(&worktree, "another-channel"),
    );

    assert!(!snapshot.worktree_groups[0].worktree_backed);
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
    ];

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
