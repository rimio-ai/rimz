use super::*;
use crate::agents::SessionOrigin;
use crate::agents::{AgentState, AgentStatus, TurnPhase};
use crate::ledger::atomic;
use crate::remote::link::{LinkStats, LinkStatsFile, LinkTier};
use crate::sidebar::cache::{
    AccountsCache, CodexDaemonReap, DiffStatsCacheEntry, read_codex_daemon_reap, unix_now_ms,
    write_codex_daemon_reap,
};
use crate::sidebar::test_support::{activity_row, pane, root_agent, worktree_group};
use jiff::SignedDuration;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn runtime() -> (tempfile::TempDir, RuntimePaths, SidebarSnapshot) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now());
    (dir, runtime, snapshot)
}

#[test]
fn detached_rimz_command_anchors_cwd_to_shared_root() {
    let (_dir, runtime, _snapshot) = runtime();
    let cmd = detached_rimz_command(std::path::PathBuf::from("/nonexistent/rimz"), &runtime);

    assert_eq!(cmd.get_current_dir(), Some(runtime.shared_root.as_path()));
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
        Vec::new(),
        Timestamp::now(),
    );
    snapshot.worktree_groups = vec![worktree_group(&worktree, Vec::new())];

    let mut states = BTreeMap::new();
    states.insert(
        worktree.display().to_string(),
        crate::WorktreePrState::Closed,
    );
    project_pr_state_map(&mut snapshot, &states);
    assert_eq!(
        snapshot.worktree_groups[0].pr_state,
        Some(crate::WorktreePrState::Closed)
    );

    project_pr_state_map(&mut snapshot, &BTreeMap::new());
    assert_eq!(snapshot.worktree_groups[0].pr_state, None);
}

fn stats(rtt_ms: Option<u32>, miss_pct: u16) -> LinkStats {
    LinkStats {
        rtt_ms,
        miss_pct,
        window: 30,
        bandwidth_bps: None,
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
    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(dir.path()),
        Vec::new(),
        Vec::new(),
        now,
    );
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
fn cleared_codex_reap_drops_only_fresh_same_pane_roots() {
    let old_at = Timestamp::from_second(1_000).unwrap();
    let fork_at = Timestamp::from_second(2_000).unwrap();
    let new_at = Timestamp::from_second(3_000).unwrap();
    let mut old = codex_root("old", "/repo/main", "terminal_1");
    old.worktree_branch = Some("main".to_owned());
    old.last_activity = old_at;
    old.origin = Some(SessionOrigin::Fresh);
    let mut fork = codex_root("fork", "/repo/main", "terminal_1");
    fork.worktree_branch = Some("main".to_owned());
    fork.last_activity = fork_at;
    fork.origin = Some(SessionOrigin::Forked);
    let mut new = codex_root("new", "/repo/main", "terminal_1");
    new.worktree_branch = Some("main".to_owned());
    new.last_activity = new_at;
    new.origin = Some(SessionOrigin::Fresh);
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        Vec::new(),
        vec![old, fork, new],
        Timestamp::now(),
    );

    snapshot.drop_cleared_codex_sessions(&[pane("terminal_1", "codex", "/repo/main")]);

    let ids: Vec<_> = snapshot
        .agents
        .iter()
        .map(|agent| agent.agent_id.as_str())
        .collect();
    assert_eq!(ids, vec!["fork", "new"]);
}

#[test]
fn cleared_codex_reap_requires_both_sessions_on_live_pane() {
    let mut live = codex_root("live", "/repo/main", "terminal_1");
    live.worktree_branch = Some("main".to_owned());
    live.last_activity = Timestamp::from_second(1_000).unwrap();
    live.origin = Some(SessionOrigin::Fresh);
    let mut closed = root_agent("codex", "closed", None);
    closed.worktree_path = Some("/repo/main".to_owned());
    closed.worktree_branch = Some("main".to_owned());
    closed.last_activity = Timestamp::from_second(2_000).unwrap();
    closed.origin = Some(SessionOrigin::Fresh);
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        Vec::new(),
        vec![live, closed],
        Timestamp::now(),
    );

    snapshot.drop_cleared_codex_sessions(&[pane("terminal_1", "codex", "/repo/main")]);

    assert_eq!(snapshot.agents.len(), 2);
}

#[test]
fn cleared_codex_reap_keeps_unknown_lineage() {
    let mut old = codex_root("old", "/repo/main", "terminal_1");
    old.last_activity = Timestamp::from_second(1_000).unwrap();
    old.origin = Some(SessionOrigin::Fresh);
    let mut new = codex_root("new", "/repo/main", "terminal_1");
    new.worktree_path = Some("/repo/main".to_owned());
    new.last_activity = Timestamp::from_second(2_000).unwrap();
    let mut snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        Vec::new(),
        vec![old, new],
        Timestamp::now(),
    );

    snapshot.drop_cleared_codex_sessions(&[pane("terminal_1", "codex", "/repo/main")]);

    assert_eq!(snapshot.agents.len(), 2);
}

#[test]
fn cached_enrich_reaps_codex_clear_session_before_pane_binding() {
    let (_dir, runtime, _) = runtime();
    let mut old = codex_root("old", "/repo/main", "terminal_1");
    old.last_activity = Timestamp::from_second(1_000).unwrap();
    old.origin = Some(SessionOrigin::Fresh);
    let mut new = codex_root("new", "/repo/main", "terminal_1");
    new.last_activity = Timestamp::from_second(2_000).unwrap();
    new.origin = Some(SessionOrigin::Fresh);
    let snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        Vec::new(),
        vec![old, new],
        Timestamp::now(),
    );
    let frame = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", "codex", "/repo/main")],
        1_000,
        "rimz-test",
    );

    let snapshot = enrich(
        snapshot,
        Some(frame),
        &runtime,
        None,
        None,
        EnrichMode::Cached,
        None,
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
    closed.agent_pid = Some(77);
    let mut open = root_agent("codex", "open", None);
    open.agent_pid = Some(77);
    let snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        Vec::new(),
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
        EnrichMode::Cached,
        None,
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
    kept.agent_pid = Some(77);
    let snapshot = SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(Path::new("/tmp/enrich")),
        Vec::new(),
        vec![kept],
        Timestamp::now(),
    );
    let snapshot = enrich(
        snapshot,
        None,
        &empty_runtime,
        None,
        None,
        EnrichMode::Cached,
        None,
    );
    assert_eq!(snapshot.agents.len(), 1, "absent cache reaps nothing");
}

#[test]
fn daemon_reap_due_tracks_cache_ttl() {
    let ttl_ms = CODEX_DAEMON_REAP_TTL.as_millis() as u64;
    let now_ms = ttl_ms * 2 + 10;

    assert!(daemon_reap_due(&None, now_ms));
    assert!(!daemon_reap_due(
        &Some(CodexDaemonReap {
            produced_at_ms: now_ms.saturating_sub(ttl_ms),
            daemon_pids: BTreeSet::new(),
            loaded: None,
        }),
        now_ms
    ));
    assert!(daemon_reap_due(
        &Some(CodexDaemonReap {
            produced_at_ms: now_ms.saturating_sub(ttl_ms).saturating_sub(1),
            daemon_pids: BTreeSet::new(),
            loaded: None,
        }),
        now_ms
    ));
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
        Vec::new(),
        vec![root_agent("codex", "pane-less", None)],
        Timestamp::now(),
    );

    let _ = enrich(
        snapshot,
        None,
        &runtime_paths,
        None,
        None,
        EnrichMode::Producing {
            roots: None,
            heavy: HeavyLanes::Project,
            config: Box::new(crate::config::MachineConfig::default()),
        },
        None,
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
        observed_at_ms: Some(1_000),
        build: None,
        session_name: "rimz-test".to_owned(),
        tabs: Vec::new(),
        carried_panes: Vec::new(),
        viewed_panes: vec![pane_id.clone()],
        presence: None,
    };

    let snapshot = enrich(
        snapshot,
        Some(frame),
        &runtime,
        None,
        None,
        EnrichMode::Cached,
        None,
    );

    assert_eq!(snapshot.viewed_panes, vec![pane_id]);
}

fn codex_root(id: &str, worktree: &str, pane_id: &str) -> AgentState {
    let mut agent = root_agent("codex", id, None);
    agent.worktree_path = Some(worktree.to_owned());
    agent.pane = Some(pane(pane_id, "codex", worktree));
    agent
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
        Some(frame),
        &runtime,
        None,
        None,
        EnrichMode::Cached,
        None,
    );

    assert_eq!(snapshot.presence, Some(crate::SidebarPresence::Detached));
}

fn stale_presence_frame() -> crate::sidebar::frame::PaneFrame {
    let mut frame = crate::sidebar::frame::assemble_frame(Vec::new(), 1_000, "rimz-test");
    frame.presence = Some(crate::PresenceSample {
        human_clients: 1,
        last_input_ms: Some(1_000),
        sampled_at_ms: 1_000_000,
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
        Some(frame),
        runtime,
        None,
        None,
        EnrichMode::Producing {
            roots: None,
            heavy: HeavyLanes::Project,
            config: Box::new(crate::config::MachineConfig::default()),
        },
        None,
    )
}

#[test]
fn local_tmux_presence_keeps_idle_detection() {
    let (_dir, runtime, snapshot) = runtime();

    let snapshot = enrich_presence_with_default_config(snapshot, stale_presence_frame(), &runtime);

    assert_eq!(
        snapshot.presence,
        Some(crate::SidebarPresence::Idle { idle_ms: 999_000 })
    );
}

#[test]
fn remote_tmux_presence_stays_active_while_attached() {
    let (_dir, runtime, snapshot) = runtime();
    let file = LinkStatsFile::new(unix_now_ms(), "client".to_owned(), stats(Some(42), 0));
    atomic::write_temp_then_rename_cache(&crate::remote::link::stats_path(&runtime), &file)
        .unwrap();

    let snapshot = enrich_presence_with_default_config(snapshot, stale_presence_frame(), &runtime);

    assert_eq!(snapshot.presence, Some(crate::SidebarPresence::Active));
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

    let snapshot =
        SidebarSnapshot::build_with_agents(workspace, Vec::new(), vec![agent], Timestamp::now())
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
        last_activity: jiff::Timestamp::now(),
        card: crate::RowCard::Agent(Box::new(crate::AgentCard {
            status: Some(AgentStatus::Running),
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
        last_activity: jiff::Timestamp::now(),
        card: crate::RowCard::Process(crate::ProcessCard::default()),
    };
    let mut groups = vec![crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: vec![agent_row(Some(85)), agent_row(Some(5)), process_row()],
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
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
    let config = crate::config::MachineConfig::load().unwrap_or_default();
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
        Timestamp::from_second(1_750_000_000).unwrap(),
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
        turn_error: None,
        turn_complete: None,
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
    let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now())
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
        1,
        &hash,
        &scoped,
    );

    snapshot = enrich(
        snapshot,
        None,
        &runtime,
        None,
        None,
        EnrichMode::Cached,
        None,
    );

    assert_eq!(
        snapshot
            .value_tally
            .as_ref()
            .map(|tally| tally.headline.usd),
        Some(50.0),
        "global tally remains available for the ledger"
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
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now());

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
        EnrichMode::Cached,
        None,
    );

    assert_eq!(
        snapshot.value_tally, None,
        "consumer folds ignore stale aggregate shapes instead of displaying old sidebar history"
    );
}

#[test]
fn cached_enrich_derives_workspace_spending_from_shared_cursor_on_cache_miss() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    atomic::write_temp_then_rename_cache(
        &runtime.shared_accounts_path(),
        &AccountsCache {
            refreshed_at_ms: unix_now_ms(),
            accounts: BTreeMap::new(),
            ok: true,
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
            mtime_secs: 1,
            len: 1,
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
                thread_id: None,
                is_sidechain: false,
                model: None,
                rolled: false,
            }],
            unknown_models: BTreeMap::new(),
        },
    );
    crate::agents::spending::write_spending_cache(&runtime.shared_spending_cursor_path(), &raw);
    crate::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        published_ms,
        &crate::agents::spending::Spending::default(),
    );

    let hash = workspace_scope_hash(&project);
    assert!(
        !runtime.workspace_spending_path(&hash).exists(),
        "test starts without the per-scope workspace cache"
    );

    let _discovered = crate::agents::spending::override_discovered_spending_files_for_test(vec![(
        &crate::agents::ClaudeAdapter as &'static dyn crate::agents::AgentAdapter,
        transcript,
    )]);
    let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now())
        .with_project_root(Some(project));
    let project = snapshot.project_root.clone().unwrap();
    snapshot.worktree_groups = vec![worktree_group(
        &project,
        vec![cost_row_at(
            "new-session",
            Some(0.25),
            Some(registered_after_publish),
            &project,
        )],
    )];
    let snapshot = enrich(
        snapshot,
        None,
        &runtime,
        None,
        None,
        EnrichMode::Cached,
        None,
    );

    assert_eq!(
        snapshot.workspace_value_tally.as_ref().map(|tally| (
            tally.headline.usd,
            tally.headline.sessions,
            tally.headline.tokens
        )),
        Some((3.75, 1, 27)),
        "consumer folds derive the cockpit tally from the shared cursor cache"
    );
    assert_eq!(
        snapshot.today_spend_live_usd,
        Some(4.0),
        "the derived cache keeps the producer publish stamp for live-spend overlay"
    );
    assert!(
        !runtime.workspace_spending_path(&hash).exists(),
        "the consumer derive path stays read-only"
    );
}
