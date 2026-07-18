use super::*;
use crate::agents::SessionOrigin;
use crate::agents::{AgentState, AgentStatus, TurnPhase};
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
use crate::remote::link::{LinkStats, LinkStatsFile, LinkTier};
use crate::sidebar::refresh::AccountsCache;
use crate::sidebar::refresh::PrLink;
use crate::sidebar::refresh::git_stats::{
    DiffStatsCache, DiffStatsCacheEntry, WorktreeRootsCache, focused_worktree_paths,
    hot_worktree_paths, needed_worktree_paths,
};
use crate::sidebar::refresh::{CodexDaemonReap, read_codex_daemon_reap, write_codex_daemon_reap};
use crate::sidebar::test_support::{activity_row, pane, root_agent, worktree_group};
use crate::sidebar::timing::GIT_ACTIVITY_WINDOW;
use crate::sidebar::timing::unix_now_ms;
use crate::store::atomic;
use jiff::SignedDuration;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn runtime() -> (tempfile::TempDir, RuntimePaths, SidebarSnapshot) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
    (dir, runtime, snapshot)
}

fn cached_opts() -> FoldOpts<'static> {
    FoldOpts {
        producing: false,
        fresh_roots: None,
        config: None,
        lanes: None,
        local_sessions: Vec::new(),
        wiring: Default::default(),
    }
}

fn producing_opts() -> FoldOpts<'static> {
    FoldOpts {
        producing: true,
        fresh_roots: None,
        config: Some(std::sync::Arc::new(crate::config::MachineConfig::default())),
        lanes: None,
        local_sessions: Vec::new(),
        wiring: Default::default(),
    }
}

#[test]
fn remote_control_badge_follows_enablement_and_probe_health() {
    use crate::RemoteControlBadge::{Down, Healthy, Hidden};

    let cases = [
        (true, false, Some(true), Healthy, "configured and up"),
        (true, false, Some(false), Down, "configured and down"),
        (true, false, None, Healthy, "configured before first probe"),
        (
            false,
            true,
            Some(true),
            Healthy,
            "pane auto with live probe",
        ),
        (
            false,
            true,
            Some(false),
            Healthy,
            "pane auto ignores server probe",
        ),
        (
            true,
            true,
            Some(false),
            Down,
            "configured host wins over pane auto",
        ),
        (false, false, Some(false), Hidden, "disabled"),
    ];

    for (config_toggle, pane_auto, server_alive, expected, label) in cases {
        assert_eq!(
            remote_control_badge(config_toggle, pane_auto, server_alive),
            expected,
            "{label}"
        );
    }
}

#[test]
fn provider_store_adapters_are_wired_for_identityless_idle_cards() {
    let wired = crate::sidebar::agent_wiring::probe_current();
    assert!(wired.kinds.iter().any(|kind| kind == "antigravity"));
    assert!(wired.kinds.iter().any(|kind| kind == "kiro"));
}

fn diff_entry(
    clean: bool,
    landed: bool,
    did_work: Option<bool>,
    ahead: u32,
    behind: u32,
) -> DiffStatsCacheEntry {
    DiffStatsCacheEntry {
        refreshed_at_ms: 0,
        commit_refreshed_at_ms: Some(0),
        added: Some(0),
        removed: Some(0),
        commits: Some(ahead),
        behind: Some(behind),
        trunk: Some("main".to_owned()),
        branch: Some("feature".to_owned()),
        clean: Some(clean),
        landed: Some(landed),
        did_work,
        merge_in_progress: Some(false),
        ..DiffStatsCacheEntry::default()
    }
}

fn write_worktree_marker(path: &Path, name: &str) {
    let git_dir = path.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    let marker = crate::worktree::WorktreeMarker {
        version: 1,
        name: name.to_owned(),
        branch: name.to_owned(),
        base_branch: Some("main".to_owned()),
        from_pr: None,
        base_ref: "HEAD".to_owned(),
        repo_root: path.to_path_buf(),
        worktree_path: path.to_path_buf(),
        created_at: Timestamp::now(),
    };
    atomic::write_temp_then_rename(&git_dir.join("rimz-worktree.json"), &marker).unwrap();
}

fn channel_group(label: &str, path: &Path) -> crate::SidebarWorktreeGroup {
    let mut group = worktree_group(
        path,
        vec![activity_row(false, None, Timestamp::now(), path)],
    );
    group.key = format!("channel:{label}");
    group.label = label.to_owned();
    group.kind = SidebarWorktreeKind::Channel;
    group
}

fn diff_cache_with_marker(path: &Path, name: &str) -> DiffStatsCache {
    DiffStatsCache {
        worktrees: Some(WorktreeRootsCache {
            refreshed_at_ms: unix_now_ms(),
            roots: vec![path.to_path_buf()],
            marker_names: Some(BTreeMap::from([(path.to_path_buf(), name.to_owned())])),
        }),
        ..DiffStatsCache::default()
    }
}

#[test]
fn trunk_sync_classifier_uses_marker_and_local_git_state() {
    assert_eq!(
        classify_trunk_sync(
            &diff_entry(true, true, Some(false), 0, 0),
            "feature",
            "main"
        ),
        Some(WorktreeTrunkSync::Pristine)
    );
    assert_eq!(
        classify_trunk_sync(
            &diff_entry(false, true, Some(false), 0, 0),
            "feature",
            "main"
        ),
        Some(WorktreeTrunkSync::Diverged)
    );
    assert_eq!(
        classify_trunk_sync(&diff_entry(true, true, Some(true), 2, 5), "feature", "main"),
        Some(WorktreeTrunkSync::Merged)
    );
    let mut reconciling = diff_entry(true, true, Some(true), 0, 0);
    reconciling.merge_in_progress = Some(true);
    assert_eq!(
        classify_trunk_sync(&reconciling, "feature", "main"),
        Some(WorktreeTrunkSync::Reconciling)
    );
    assert_eq!(
        classify_trunk_sync(&diff_entry(true, true, Some(true), 0, 0), "main", "main"),
        None,
        "trunk checkout is exempt"
    );
    assert_eq!(
        classify_trunk_sync(&diff_entry(true, true, None, 0, 0), "feature", "main"),
        Some(WorktreeTrunkSync::Diverged),
        "unmarked worktrees stay conservative"
    );
    assert_eq!(
        classify_trunk_sync(
            &diff_entry(true, true, Some(false), 0, 1),
            "feature",
            "main"
        ),
        Some(WorktreeTrunkSync::Diverged),
        "fresh fork behind trunk is not pristine"
    );
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
    snapshot.worktree_groups = vec![worktree_group(&worktree, Vec::new())];
    snapshot.worktree_groups[0].pr_number = Some(69);

    let mut states = BTreeMap::new();
    states.insert(
        worktree.display().to_string(),
        PrLink {
            state: crate::WorktreePrState::Closed,
            number: Some(91),
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
    assert_eq!(snapshot.worktree_groups[0].pr_ci, None);

    states.insert(
        worktree.display().to_string(),
        PrLink {
            state: crate::WorktreePrState::Open,
            number: Some(91),
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
}

#[test]
fn pr_state_projection_reaches_marked_worktree_channels() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path().join("feature");
    std::fs::create_dir_all(&worktree).unwrap();
    write_worktree_marker(&worktree, "feature");
    let diff_cache = diff_cache_with_marker(&worktree, "feature");
    std::fs::remove_dir_all(worktree.join(".git")).unwrap();
    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(dir.path()),
        Vec::new(),
        Timestamp::now(),
    );
    snapshot.worktree_groups = vec![channel_group("feature", &worktree)];

    let mut states = BTreeMap::new();
    states.insert(
        worktree.display().to_string(),
        PrLink {
            state: crate::WorktreePrState::Merged,
            number: Some(91),
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
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path().join("feature");
    std::fs::create_dir_all(&worktree).unwrap();
    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(dir.path()),
        Vec::new(),
        Timestamp::now(),
    );
    snapshot.worktree_groups = vec![channel_group("feature", &worktree)];

    let mut states = BTreeMap::new();
    states.insert(
        worktree.display().to_string(),
        PrLink {
            state: crate::WorktreePrState::Merged,
            number: Some(91),
            ci: None,
            merge_sha: Some("merged-sha".to_owned()),
        },
    );
    project_pr_state_map(&mut snapshot, &states, &DiffStatsCache::default());
    assert_eq!(snapshot.worktree_groups[0].pr_state, None);
}

#[test]
fn diff_projection_keeps_worktree_channel_label_and_uses_live_branch() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path().join("codex-resets");
    std::fs::create_dir_all(&worktree).unwrap();
    write_worktree_marker(&worktree, "codex-resets");
    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(dir.path()),
        Vec::new(),
        Timestamp::now(),
    );
    snapshot.worktree_groups = vec![channel_group("codex-resets", &worktree)];

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
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path().join("codex-resets");
    std::fs::create_dir_all(&worktree).unwrap();
    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(dir.path()),
        Vec::new(),
        Timestamp::now(),
    );
    snapshot.worktree_groups = vec![channel_group("codex-resets", &worktree)];

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
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path().join("codex-resets");
    std::fs::create_dir_all(&worktree).unwrap();
    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(dir.path()),
        Vec::new(),
        Timestamp::now(),
    );
    snapshot.worktree_groups = vec![channel_group("codex-resets", &worktree)];
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

    let snapshot = enrich(
        snapshot,
        None,
        &runtime,
        None,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );

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

fn stats(rtt_ms: Option<u32>, miss_pct: u16) -> LinkStats {
    LinkStats {
        rtt_ms,
        miss_pct,
        window: 30,
    }
}

#[test]
fn link_stats_sidecar_folds_into_snapshot() {
    let (_dir, runtime, mut snapshot) = runtime();
    let file = LinkStatsFile::new(1_000, "client".to_owned(), stats(Some(230), 4));
    atomic::write_temp_then_rename_cache(&crate::remote::link::stats_path(&runtime), &file)
        .unwrap();

    fold_link_stats(&mut snapshot, &runtime, 1_500);

    let link = snapshot.link.expect("link badge");
    assert_eq!(link.rtt_ms, Some(230));
    assert_eq!(link.miss_pct, 4);
    assert_eq!(link.tier, LinkTier::Degraded);
    assert_eq!(link.freshness, crate::SidebarLinkFreshness::Fresh);
    assert_eq!(link.sampled_at_ms, 1_000);
}

#[test]
fn stale_link_stats_render_as_stale_until_expired() {
    let (_dir, runtime, mut snapshot) = runtime();
    let file = LinkStatsFile::new(1_000, "client".to_owned(), stats(Some(42), 0));
    atomic::write_temp_then_rename_cache(&crate::remote::link::stats_path(&runtime), &file)
        .unwrap();

    fold_link_stats(&mut snapshot, &runtime, 12_000);
    assert_eq!(
        snapshot.link.as_ref().unwrap().freshness,
        crate::SidebarLinkFreshness::Stale
    );

    fold_link_stats(&mut snapshot, &runtime, 122_001);
    assert!(snapshot.link.is_none(), "expired stats disappear");
}

#[test]
fn corrupt_or_wrong_version_stats_disappear() {
    let (_dir, runtime, mut snapshot) = runtime();
    atomic::write_bytes_atomically(&crate::remote::link::stats_path(&runtime), b"not json")
        .unwrap();
    fold_link_stats(&mut snapshot, &runtime, 1_000);
    assert!(snapshot.link.is_none());

    let path = crate::remote::link::stats_path(&runtime);
    let mut file = LinkStatsFile::new(1_000, "client".to_owned(), stats(Some(42), 0));
    file.v = "rimz.link.v0".to_owned();
    atomic::write_temp_then_rename_cache(&path, &file).unwrap();
    fold_link_stats(&mut snapshot, &runtime, 1_000);
    assert!(snapshot.link.is_none());
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
fn cached_enrich_binds_reaped_codex_clear_session() {
    let (_dir, runtime, _) = runtime();
    let now = Timestamp::now();
    let mut old = codex_root("old", "/repo/main", "terminal_1");
    old.status = AgentStatus::Success;
    old.last_activity = now - SignedDuration::from_secs(120);
    old.origin = Some(SessionOrigin::Fresh);
    let mut new = codex_root("new", "/repo/main", "terminal_1");
    new.last_activity = now - SignedDuration::from_secs(60);
    new.origin = Some(SessionOrigin::Fresh);
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        vec![old, new],
        now,
    );
    snapshot.reap_stale_sessions();
    let frame = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", "codex", "/repo/main")],
        1_000,
        "rimz-test",
    );

    let snapshot = enrich(
        snapshot,
        Some(&frame),
        &runtime,
        None,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(
        snapshot
            .agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>(),
        vec!["new"]
    );
}

#[test]
fn cached_enrich_uses_published_codex_daemon_reap_inputs() {
    let (_dir, runtime_paths, _) = runtime();
    let mut closed = root_agent("codex", "closed", None);
    closed.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "closed",
        77,
        None,
    ));
    let mut open = root_agent("codex", "open", None);
    open.runtime_owner = Some(RuntimeOwner::new(RuntimeOwnerKind::Agent, "open", 77, None));
    let snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        vec![closed, open],
        Timestamp::now(),
    );
    write_codex_daemon_reap(
        &runtime_paths,
        &CodexDaemonReap {
            produced_at_ms: 1_000,
            daemon_pids: BTreeSet::from([77]),
            loaded: Some(BTreeSet::from(["open".to_owned()])),
        },
    )
    .unwrap();

    let snapshot = enrich(
        snapshot,
        None,
        &runtime_paths,
        None,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(
        snapshot
            .agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>(),
        vec!["open"]
    );

    let (_empty_dir, empty_runtime, _) = runtime();
    let mut kept = root_agent("codex", "kept", None);
    kept.runtime_owner = Some(RuntimeOwner::new(RuntimeOwnerKind::Agent, "kept", 77, None));
    let snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        vec![kept],
        Timestamp::now(),
    );
    let snapshot = enrich(
        snapshot,
        None,
        &empty_runtime,
        None,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );
    assert_eq!(snapshot.agents.len(), 1, "absent cache reaps nothing");
}

#[test]
fn project_lane_enrich_reads_stale_codex_daemon_reap_without_rewriting() {
    let (_dir, runtime_paths, _) = runtime();
    write_codex_daemon_reap(
        &runtime_paths,
        &CodexDaemonReap {
            produced_at_ms: 1,
            daemon_pids: BTreeSet::new(),
            loaded: None,
        },
    )
    .unwrap();
    let snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        vec![root_agent("codex", "pane-less", None)],
        Timestamp::now(),
    );

    let _ = enrich(
        snapshot,
        None,
        &runtime_paths,
        None,
        None,
        producing_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(
        read_codex_daemon_reap(&runtime_paths)
            .expect("codex reap cache")
            .produced_at_ms,
        1
    );
}

#[test]
fn frame_fold_carries_viewed_panes_onto_snapshot() {
    let (_dir, runtime, snapshot) = runtime();
    let pane_id = crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_1");
    let frame = crate::sidebar::frame::PaneFrame {
        produced_at_ms: 1_000,
        observed_at_ms: 1_000,
        build: None,
        session_name: "rimz-test".to_owned(),
        tabs: Vec::new(),
        carried_panes: Vec::new(),
        viewed_panes: vec![pane_id.clone()],
        focused_pane: Some(pane_id.clone()),
        presence: None,
    };

    let snapshot = enrich(
        snapshot,
        Some(&frame),
        &runtime,
        None,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(snapshot.viewed_panes, vec![pane_id]);
    assert_eq!(
        snapshot.focused_pane,
        snapshot.viewed_panes.first().cloned()
    );
}

fn codex_root(id: &str, worktree: &str, pane_id: &str) -> AgentState {
    let mut agent = root_agent("codex", id, None);
    agent.worktree_path = Some(worktree.to_owned());
    agent.pane = Some(pane(pane_id, "codex", worktree));
    agent
}

#[test]
fn producer_binding_log_dedups_unchanged_lazy_pairing_ambiguity() {
    let (_dir, runtime, snapshot) = runtime();
    let worktree = "/repo/main";
    let mut agent = root_agent("codex", "lazy-session", None);
    agent.worktree_path = Some(worktree.to_owned());
    let snapshot = SidebarSnapshot::build_with_agents(
        snapshot.workspace_id.clone(),
        vec![agent.clone()],
        Timestamp::now(),
    );
    let frame = crate::sidebar::frame::assemble_frame(
        vec![
            pane("terminal_1", "codex", worktree),
            pane("terminal_2", "codex", worktree),
        ],
        1_000,
        "rimz-test",
    );

    let _ = enrich(
        snapshot.clone(),
        Some(&frame),
        &runtime,
        None,
        None,
        producing_opts(),
        &crate::diag::DiagSink::disabled(),
    );
    let _ = enrich(
        snapshot.clone(),
        Some(&frame),
        &runtime,
        None,
        None,
        producing_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(binding_log_lines(&runtime), 1);

    let mut active_agent = agent.clone();
    active_agent.last_activity += SignedDuration::from_secs(1);
    let active_snapshot = SidebarSnapshot::build_with_agents(
        snapshot.workspace_id.clone(),
        vec![active_agent],
        Timestamp::now(),
    );
    let _ = enrich(
        active_snapshot,
        Some(&frame),
        &runtime,
        None,
        None,
        producing_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(binding_log_lines(&runtime), 1);

    let mut later_pane = pane("terminal_2", "codex", worktree);
    later_pane.pane_process_start =
        Some(agent.registered_at.unwrap_or(agent.last_activity) - SignedDuration::from_secs(1));
    let changed_frame = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", "codex", worktree), later_pane],
        1_000,
        "rimz-test",
    );
    let _ = enrich(
        snapshot,
        Some(&changed_frame),
        &runtime,
        None,
        None,
        producing_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(binding_log_lines(&runtime), 2);
}

fn binding_log_lines(runtime: &RuntimePaths) -> usize {
    let path = crate::diag::binding::log(runtime).path().to_path_buf();
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

#[test]
fn frame_fold_carries_presence_onto_snapshot() {
    let (_dir, runtime, snapshot) = runtime();
    let mut frame = crate::sidebar::frame::assemble_frame(Vec::new(), 1_000, "rimz-test");
    frame.presence = Some(crate::PresenceSample {
        human_clients: 0,
        last_input_ms: None,
        sampled_at_ms: 1_000,
    });

    let snapshot = enrich(
        snapshot,
        Some(&frame),
        &runtime,
        None,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(snapshot.presence, Some(crate::SidebarPresence::Detached));
}

fn snapshot_now_ms(snapshot: &SidebarSnapshot) -> u64 {
    snapshot.now.as_millisecond().max(0) as u64
}

fn stale_presence_frame(now_ms: u64) -> crate::sidebar::frame::PaneFrame {
    let mut frame = crate::sidebar::frame::assemble_frame(Vec::new(), 1_000, "rimz-test");
    frame.presence = Some(crate::PresenceSample {
        human_clients: 1,
        last_input_ms: Some(now_ms - 999_000),
        sampled_at_ms: now_ms - 10_000,
    });
    frame
}

fn enrich_presence_with_default_config(
    snapshot: SidebarSnapshot,
    frame: crate::sidebar::frame::PaneFrame,
    runtime: &RuntimePaths,
) -> SidebarSnapshot {
    enrich(
        snapshot,
        Some(&frame),
        runtime,
        None,
        None,
        producing_opts(),
        &crate::diag::DiagSink::disabled(),
    )
}

#[test]
fn local_tmux_presence_keeps_idle_detection() {
    let (_dir, runtime, snapshot) = runtime();
    let frame = stale_presence_frame(snapshot_now_ms(&snapshot));

    let snapshot = enrich_presence_with_default_config(snapshot, frame, &runtime);

    assert_eq!(
        snapshot.presence,
        Some(crate::SidebarPresence::Idle { idle_ms: 999_000 })
    );
}

#[test]
fn remote_tmux_presence_detects_idle() {
    let (_dir, runtime, snapshot) = runtime();
    let file = LinkStatsFile::new(unix_now_ms(), "client".to_owned(), stats(Some(42), 0));
    atomic::write_temp_then_rename_cache(&crate::remote::link::stats_path(&runtime), &file)
        .unwrap();
    let frame = stale_presence_frame(snapshot_now_ms(&snapshot));

    let snapshot = enrich_presence_with_default_config(snapshot, frame, &runtime);

    assert_eq!(
        snapshot.presence,
        Some(crate::SidebarPresence::Idle { idle_ms: 999_000 })
    );
}

#[test]
fn presence_idle_duration_tracks_snapshot_now() {
    let (_dir, runtime, snapshot) = runtime();
    let now = Timestamp::from_second(1_750_000_000).unwrap();
    let now_ms = now.as_millisecond().max(0) as u64;
    let snapshot = SidebarSnapshot::build(snapshot.workspace_id.clone(), Vec::new(), now);
    let mut frame = crate::sidebar::frame::assemble_frame(Vec::new(), 1_000, "rimz-test");
    frame.presence = Some(crate::PresenceSample {
        human_clients: 1,
        last_input_ms: Some(now_ms - 999_000),
        sampled_at_ms: now_ms - 30_000,
    });

    let snapshot = enrich_presence_with_default_config(snapshot, frame, &runtime);

    assert_eq!(
        snapshot.presence,
        Some(crate::SidebarPresence::Idle { idle_ms: 999_000 })
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

/// The config fold stamps every *agent* row's context-severity verdict from
/// the `[theme.display.context_meter]` bands — the one classification the renderer's color
/// ramp and any future signal emitter read — and leaves process rows `None`.
#[test]
fn config_fold_stamps_agent_context_severity() {
    let agent_row = |pct: Option<u8>| crate::SidebarRow {
        id: "row".to_owned(),
        name: "claude".to_owned(),
        pane: None,
        worktree_path: None,
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: jiff::Timestamp::now(),
        card: crate::RowCard::Agent(Box::new(crate::AgentCard {
            status: AgentStatus::Running,
            phase: TurnPhase::Idle,
            context_pct: pct,
            ..crate::AgentCard::default()
        })),
    };
    let process_row = || crate::SidebarRow {
        id: "row".to_owned(),
        name: "zsh".to_owned(),
        pane: None,
        worktree_path: None,
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: jiff::Timestamp::now(),
        card: crate::RowCard::Process(crate::ProcessCard::default()),
    };
    let mut groups = vec![crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: vec![agent_row(Some(85)), agent_row(Some(5)), process_row()],
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        worktree_backed: false,
        finished: false,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
        pr_ci: None,
        pr_number: None,
    }];

    stamp_context_severity(&mut groups, &crate::config::ContextMeterConfig::default());

    let rows = &groups[0].rows;
    assert_eq!(
        rows[0].as_agent().and_then(|agent| agent.context_severity),
        Some(crate::agents::ContextSeverity::Amber),
        "85% crosses the default amber band"
    );
    assert_eq!(
        rows[1].as_agent().and_then(|agent| agent.context_severity),
        Some(crate::agents::ContextSeverity::Calm)
    );
    assert_eq!(
        rows[2].as_agent().and_then(|agent| agent.context_severity),
        None,
        "a process row carries no context verdict"
    );
}

/// The cockpit scope hash for a project root, derived the way cached enrich
/// derives it: project root plus the durable worktree home resolved from the
/// loaded machine config. Tests that pre-write a per-scope workspace cache key
/// it through here so the consumer reads back the same hash.
fn workspace_scope_hash(project: &Path) -> String {
    let config = crate::config::MachineConfig::load_lenient();
    let home = crate::worktree::worktree_parent(project, &config.agents.worktree).ok();
    crate::agents::spending::SpendScope::for_workspace(Some(project), &[], home.as_deref()).hash()
}

fn cost_row_at(
    id: &str,
    usd: Option<f64>,
    registered_at: Option<Timestamp>,
    worktree_path: &Path,
) -> crate::SidebarRow {
    let mut row = activity_row(
        true,
        Some(AgentStatus::Running),
        Timestamp::now(),
        worktree_path,
    );
    row.id = id.to_owned();
    let agent = row.as_agent_mut().unwrap();
    agent.registered_at = registered_at;
    agent.context = usd.map(|usd| crate::agents::AgentContext {
        source: "claude".to_owned(),
        session_name: None,
        session_preview: None,
        model_id: None,
        model_display_name: None,
        effort: None,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: None,
        exceeds_200k_tokens: None,
        cost: Some(crate::agents::AgentCost {
            total_cost_usd: Some(usd),
            ..Default::default()
        }),
        tokens: None,
        rate_limits: None,
        pr: None,
        account: None,
        turn_opened_by: Vec::new(),
        turn_error: None,
        turn_complete: None,
        plan_proposed: None,
        native_permission_wait: None,
        turn_interrupted: None,
        observed_at: Timestamp::from_second(1_750_000_000).unwrap(),
    });
    row
}

#[test]
fn cached_enrich_reads_workspace_spending_cache_separately_from_global() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    let project = dir.path().join("repo");
    let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now())
        .with_project_root(Some(project.clone()));

    let mut global = crate::agents::spending::Spending::default();
    global.total.headline.usd = 50.0;
    global.total.year.usd = 50.0;
    crate::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        unix_now_ms(),
        &global,
    );

    let mut scoped = crate::SpendTally::default();
    scoped.headline.usd = 1.25;
    scoped.headline.sessions = 3;
    scoped.year.usd = 1.25;
    let hash = workspace_scope_hash(&project);
    // An ancient stamp (`refreshed_at_ms = 1`): age is ignored once the hash
    // matches, so consumer tabs hold the last matching workspace tally instead
    // of flapping to zero.
    crate::agents::spending::write_workspace_spending_cache(
        &runtime.workspace_spending_path(&hash),
        &crate::agents::spending::WorkspaceSpendingCache {
            refreshed_at_ms: 1,
            scope_hash: hash.clone(),
            tally: scoped,
            ..Default::default()
        },
    );

    snapshot = enrich(
        snapshot,
        None,
        &runtime,
        None,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(
        snapshot
            .value_tally
            .as_ref()
            .map(|tally| tally.headline.usd),
        Some(50.0),
        "global tally remains available for the store"
    );
    assert_eq!(
        snapshot
            .workspace_value_tally
            .as_ref()
            .map(|tally| (tally.headline.usd, tally.headline.sessions)),
        Some((1.25, 3)),
        "cockpit tally comes from the hash-matching workspace cache regardless of age"
    );
}

#[test]
fn cached_enrich_ignores_old_provider_spending_version() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());

    let mut global = crate::agents::spending::Spending::default();
    global.total.month.usd = 99.0;
    global.total.year.usd = 99.0;
    let old = crate::agents::spending::ProviderSpendingCache {
        version: crate::agents::spending::PROVIDER_SPENDING_VERSION - 1,
        refreshed_at_ms: unix_now_ms(),
        spending: global,
        ..Default::default()
    };
    atomic::write_temp_then_rename_cache(&runtime.shared_provider_spending_path(), &old).unwrap();

    let snapshot = enrich(
        snapshot,
        None,
        &runtime,
        None,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(
        snapshot.value_tally, None,
        "consumer folds ignore stale aggregate shapes instead of displaying old sidebar history"
    );
}

#[test]
fn cached_enrich_waits_for_producer_workspace_publication() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    atomic::write_temp_then_rename_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            providers: BTreeMap::new(),
        },
    )
    .unwrap();

    let project = dir.path().join("repo");
    let transcript = dir.path().join("claude.jsonl");
    let now_secs = crate::agents::spending::unix_secs_now();
    let published = Timestamp::now() - SignedDuration::from_secs(30);
    let published_ms = published.as_millisecond().max(0) as u64;
    let registered_after_publish = Timestamp::now() - SignedDuration::from_secs(10);
    let mut raw =
        crate::agents::spending::read_spending_cache(&runtime.shared_spending_cursor_path());
    raw.files.insert(
        transcript.to_string_lossy().into_owned(),
        crate::agents::spending::FileCacheEntry {
            stat: crate::agents::TranscriptStat {
                mtime_secs: 1,
                len: 1,
                ..crate::agents::TranscriptStat::default()
            },
            cursor: crate::agents::spending::SpendCursor::default(),
            origin_path: Some(project.join("src")),
            entries: vec![crate::agents::spending::CachedEntry {
                ts_secs: now_secs,
                cost_usd: 3.75,
                input: 20,
                output: 7,
                cache_write: 0,
                cache_read: 0,
                message_id: Some("msg-miss".to_owned()),
                request_id: Some("req-miss".to_owned()),
                dedup_key: None,
                thread_id: None,
                is_sidechain: false,
                has_speed: false,
                model: None,
                rolled: false,
            }],
            unknown_models: BTreeMap::new(),
        },
    );
    crate::agents::spending::write_spending_cache(&runtime.shared_spending_cursor_path(), &raw);
    let mut provider_spending = crate::agents::spending::Spending::default();
    provider_spending.total.headline.usd = 9.0;
    provider_spending.total.year.usd = 9.0;
    crate::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        published_ms,
        &provider_spending,
    );

    let hash = workspace_scope_hash(&project);
    assert!(
        !runtime.workspace_spending_path(&hash).exists(),
        "test starts without the per-scope workspace cache"
    );

    let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now())
        .with_project_root(Some(project));
    let project = snapshot.project_root.clone().unwrap();
    let mut agent = root_agent("claude", "new-session", None);
    agent.worktree_path = Some(project.display().to_string());
    agent.transcript_path = Some(transcript.display().to_string());
    snapshot.agents = vec![agent];
    snapshot.worktree_groups = vec![worktree_group(
        &project,
        vec![cost_row_at(
            "new-session",
            Some(0.25),
            Some(registered_after_publish),
            &project,
        )],
    )];
    let mut config = crate::config::MachineConfig::default();
    config.sidebar.spend_window = crate::agents::spending::SpendWindowMode::Today;
    let missing = enrich(
        snapshot.clone(),
        None,
        &runtime,
        None,
        None,
        FoldOpts {
            config: Some(std::sync::Arc::new(config.clone())),
            ..cached_opts()
        },
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(missing.value_tally.unwrap().headline.usd, 9.0);
    assert_eq!(missing.workspace_value_tally, None);
    assert!(
        !runtime.workspace_spending_path(&hash).exists(),
        "consumer never creates a missing workspace sidecar"
    );

    let mut tally = crate::agents::spending::SpendTally::default();
    tally.headline = crate::agents::spending::SpendWindow {
        usd: 3.75,
        tokens: 27,
        sessions: 1,
        ..Default::default()
    };
    tally.year = tally.headline;
    crate::agents::spending::write_workspace_spending_cache(
        &runtime.workspace_spending_path(&hash),
        &crate::agents::spending::WorkspaceSpendingCache {
            version: crate::agents::spending::WORKSPACE_SPENDING_VERSION,
            refreshed_at_ms: published_ms,
            scope_hash: hash,
            tally,
            live_baselines: BTreeMap::from([(transcript.display().to_string(), 3.75)]),
            ..Default::default()
        },
    );
    let published = enrich(
        snapshot,
        None,
        &runtime,
        None,
        None,
        FoldOpts {
            config: Some(std::sync::Arc::new(config)),
            ..cached_opts()
        },
        &crate::diag::DiagSink::disabled(),
    );
    assert_eq!(published.workspace_value_tally.unwrap().headline.usd, 3.75);
}
