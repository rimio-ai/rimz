use super::*;
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::sidebar::enrich::{FoldOpts, enrich};
use crate::sidebar::frame::{CarriedPane, assemble_frame};
use crate::sidebar::refresh::PrStateCache;
use crate::sidebar::refresh::git_stats::{DiffStatsCache, DiffStatsCacheEntry};
use crate::sidebar::test_support::{child_agent, pane, pane_in_tab, root_agent};
use crate::sidebar::timing::unix_now_ms;
use crate::store::atomic;
use crate::{RuntimePaths, SidebarSnapshot, SidebarWorktreeKind, StatePaths};
use jiff::Timestamp;
use std::path::{Path, PathBuf};

fn cached_opts() -> FoldOpts<'static> {
    FoldOpts {
        producing: false,
        fresh_roots: None,
        config: None,
        lanes: None,
    }
}

struct StampFixture {
    _state_root: tempfile::TempDir,
    _runtime_root: tempfile::TempDir,
    state: StatePaths,
    runtime: RuntimePaths,
}

impl StampFixture {
    fn new() -> Self {
        let state_root = tempfile::tempdir().unwrap();
        let runtime_root = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(state_root.path());
        let state = StatePaths::under(workspace.clone(), state_root.path()).unwrap();
        let runtime = RuntimePaths::under(workspace, runtime_root.path()).unwrap();
        state.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        std::fs::create_dir_all(&state.messages_dir).unwrap();

        for (name, path) in file_stamp_inputs(&state, &runtime) {
            write_stamp_file(&path, name);
        }

        Self {
            _state_root: state_root,
            _runtime_root: runtime_root,
            state,
            runtime,
        }
    }
}

fn file_stamp_inputs(state: &StatePaths, runtime: &RuntimePaths) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("events_log", state.events_log.clone()),
        ("latest_snapshot", state.latest_snapshot.clone()),
        ("rollup_cache", state.rollup_cache.clone()),
        ("agents_carryover", state.agents_carryover.clone()),
        ("workspace_record", state.workspace_record.clone()),
        ("message_queue", state.messages_dir.join("queue.json")),
        ("pane_frame", runtime.pane_frame_path()),
        ("diff_stats", runtime.diff_stats_path()),
        ("pr_state", runtime.pr_state_path()),
        ("unread", runtime.unread_path()),
        ("link_stats", crate::remote::link::stats_path(runtime)),
        ("accounts", runtime.shared_accounts_path()),
        ("rate_limits", runtime.shared_rate_limits_path()),
        ("credits", runtime.shared_credits_path()),
        ("provider_spending", runtime.shared_provider_spending_path()),
        ("spending_cursor", runtime.shared_spending_cursor_path()),
        ("metrics_sample", runtime.root.join("metrics-sample.json")),
        (
            "codex_daemon_reap",
            crate::sidebar::refresh::daemon_reap::codex_daemon_reap_path(runtime),
        ),
    ]
}

fn dir_stamp_inputs(state: &StatePaths, runtime: &RuntimePaths) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("messages_dir", state.messages_dir.clone()),
        ("agent_context_dir", runtime.agent_context_dir.clone()),
        ("subagent_context_dir", runtime.subagent_context_dir.clone()),
        ("agent_activity_dir", runtime.agent_activity_dir.clone()),
        ("read_marks_dir", runtime.read_marks_dir.clone()),
    ]
}

fn write_stamp_file(path: &Path, value: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, format!("{value}-baseline")).unwrap();
}

#[test]
fn consumer_fold_inputs_stamp_is_stable_for_unchanged_inputs() {
    let fixture = StampFixture::new();

    assert_eq!(
        consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime),
        consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime)
    );
}

#[test]
fn consumer_fold_inputs_stamp_changes_for_each_file_input() {
    for name in [
        "events_log",
        "latest_snapshot",
        "rollup_cache",
        "agents_carryover",
        "workspace_record",
        "message_queue",
        "pane_frame",
        "diff_stats",
        "pr_state",
        "unread",
        "link_stats",
        "accounts",
        "rate_limits",
        "credits",
        "provider_spending",
        "spending_cursor",
        "metrics_sample",
        "codex_daemon_reap",
    ] {
        let fixture = StampFixture::new();
        let before = consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime);
        let path = file_stamp_inputs(&fixture.state, &fixture.runtime)
            .into_iter()
            .find(|(candidate, _)| *candidate == name)
            .expect("case path")
            .1;

        std::fs::write(&path, format!("{name}-changed-with-a-longer-body")).unwrap();

        assert_ne!(
            consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime),
            before,
            "{name} must participate in the consumer fold input stamp",
        );
    }
}

#[test]
fn consumer_fold_inputs_stamp_changes_for_each_dir_input() {
    for name in [
        "messages_dir",
        "agent_context_dir",
        "subagent_context_dir",
        "agent_activity_dir",
        "read_marks_dir",
    ] {
        let fixture = StampFixture::new();
        let before = consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime);
        let path = dir_stamp_inputs(&fixture.state, &fixture.runtime)
            .into_iter()
            .find(|(candidate, _)| *candidate == name)
            .expect("case path")
            .1;

        std::fs::remove_dir_all(&path).unwrap();

        assert_ne!(
            consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime),
            before,
            "{name} must participate in the consumer fold input stamp",
        );
    }
}

#[test]
fn consumer_fold_inputs_stamp_ignores_unrelated_runtime_churn() {
    let fixture = StampFixture::new();
    let baseline = consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime);
    let unrelated = [
        fixture.runtime.root.join("snapshot.lock"),
        fixture.runtime.root.join("presence.stamp"),
        fixture.runtime.root.join("client-presence-probe.stamp"),
        fixture.runtime.root.join("producer-cache.json"),
    ];
    for path in unrelated {
        std::fs::write(&path, b"churn").unwrap();
        assert_eq!(
            consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime),
            baseline,
            "{} is not a consumer fold input",
            path.display(),
        );
        std::fs::remove_file(path).unwrap();
    }

    let temp = fixture.runtime.root.join(".snapshot.tmp-123");
    let renamed = fixture.runtime.root.join("producer-only.cache");
    std::fs::write(&temp, b"temp").unwrap();
    std::fs::rename(&temp, &renamed).unwrap();
    std::fs::remove_file(renamed).unwrap();
    let heartbeat = fixture.runtime.heartbeat_dir.join("sidebar.unrelated.json");
    std::fs::write(heartbeat, b"{}").unwrap();
    assert_eq!(
        consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime),
        baseline,
    );
}

#[test]
fn consumer_fold_inputs_stamp_tracks_filtered_dynamic_files() {
    let fixture = StampFixture::new();
    for path in [
        fixture
            .runtime
            .root
            .join("workspace-spending.0123456789abcdef.json"),
        fixture.runtime.root.join("budget.0123456789abcdef.json"),
        fixture.runtime.root.join("budget.fleet.json"),
        fixture.runtime.root.join("budget.scopes.json"),
        fixture
            .runtime
            .root
            .join("auto-continue.0123456789abcdef.json"),
        fixture
            .runtime
            .persistent_shared_root
            .join("budget.account.codex.json"),
    ] {
        let before_create = consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime);
        write_stamp_file(&path, "created");
        let after_create = consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime);
        assert_ne!(after_create, before_create, "create {}", path.display());

        std::fs::write(&path, b"replaced-with-a-longer-payload").unwrap();
        let after_replace = consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime);
        assert_ne!(after_replace, after_create, "replace {}", path.display());

        std::fs::remove_file(&path).unwrap();
        assert_ne!(
            consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime),
            after_replace,
            "remove {}",
            path.display(),
        );
    }
}

#[test]
fn read_published_snapshot_folds_caches_without_forking() {
    // A real on-disk worktree so the live-dir projection fires.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let worktree = dir.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let wt = worktree.to_string_lossy().into_owned();

    // Publish the rollup (project root = the worktree) to `latest.json`, where
    // the consumer reads it fresh, and the live panes to `snapshot.json`. `own`
    // is excluded; a sibling pane becomes a row.
    let mut rollup = SidebarSnapshot::build(workspace.clone(), Vec::new(), Timestamp::now());
    rollup = rollup.with_project_root(Some(worktree.clone()));
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();
    let panes = vec![
        pane("terminal_0", "zsh", &wt),
        pane("terminal_own", "rimz-sidebar", &wt),
    ];
    let base = assemble_frame(panes, unix_now_ms(), "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &base).unwrap();

    // Publish diff stats for the worktree path: +7 / -2, 3 commits ahead and
    // 1 behind a remote-default trunk, on branch `feat`.
    let mut diff = DiffStatsCache::default();
    diff.entries.insert(
        wt.clone(),
        DiffStatsCacheEntry {
            refreshed_at_ms: unix_now_ms(),
            commit_refreshed_at_ms: Some(unix_now_ms()),
            added: Some(7),
            removed: Some(2),
            commits: Some(3),
            behind: Some(1),
            trunk: Some("origin/main".to_owned()),
            branch: Some("feat".to_owned()),
            clean: Some(false),
            landed: Some(false),
            did_work: Some(true),
            merge_in_progress: Some(false),
            ..DiffStatsCacheEntry::default()
        },
    );
    atomic::write_temp_then_rename_cache(&runtime.diff_stats_path(), &diff).unwrap();
    let mut pr = PrStateCache::default();
    pr.states.insert(
        wt.clone(),
        crate::sidebar::refresh::pr::PrLink {
            state: crate::WorktreePrState::Open,
            number: Some(91),
        },
    );
    atomic::write_temp_then_rename_cache(&runtime.pr_state_path(), &pr).unwrap();

    let own = PaneId::from_parts(MuxName::Zellij, "terminal_own");
    let snapshot = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        Some(&own),
    )
    .expect("published base");

    // The worktree group carries the cached +7/-2 and the live branch label,
    // projected from the cache with no git fork.
    let group = snapshot
        .worktree_groups
        .iter()
        .find(|group| group.kind == SidebarWorktreeKind::Worktree)
        .expect("a worktree group");
    assert_eq!(group.diff_added, Some(7));
    assert_eq!(group.diff_removed, Some(2));
    assert_eq!(group.commits_ahead, Some(3));
    assert_eq!(group.commits_behind, Some(1));
    assert_eq!(
        group.trunk.as_deref(),
        Some("main"),
        "the ≡/✓ markers name the branch, so origin/ strips for display",
    );
    assert_eq!(group.label, "feat");
    assert_eq!(group.clean, Some(false), "the status verdict projects too");
    assert_eq!(group.landed, Some(false), "the landed verdict projects too");
    assert_eq!(
        group.trunk_sync,
        Some(crate::WorktreeTrunkSync::Diverged),
        "the trunk-sync classifier projects from cached git facts"
    );
    assert_eq!(
        group.pr_state,
        Some(crate::WorktreePrState::Open),
        "the PR state projects from cache with no forge CLI fork"
    );
    assert_eq!(group.pr_number, Some(91));
    // The own (sidebar) pane is excluded; the sibling renders as a row.
    assert!(
        snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .all(|row| {
                row.pane
                    .as_ref()
                    .is_none_or(|pane| pane.pane_id.as_str() != own.as_str())
            }),
        "the renderer's own pane is never a row"
    );
}

#[test]
fn read_published_snapshot_folds_subagent_context() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();

    let worktree = dir.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let wt = worktree.to_string_lossy().into_owned();
    let live_pane = pane("terminal_parent", "claude", &wt);
    let mut parent = root_agent("claude", "parent-1", None);
    parent.worktree_path = Some(wt.clone());
    parent.pane = Some(live_pane.clone());
    let mut child = child_agent("claude", "parent-1", "child-1");
    child.worktree_path = Some(wt.clone());
    child.pane = Some(live_pane.clone());
    child.task = None;
    let mut rollup = SidebarSnapshot::build_with_agents(
        workspace.clone(),
        vec![parent, child],
        Timestamp::now(),
    );
    rollup = rollup.with_project_root(Some(worktree));
    rollup.reflects_log = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();

    let base = assemble_frame(vec![live_pane], unix_now_ms(), "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &base).unwrap();
    let now = Timestamp::now();
    crate::store::subagent_context::write(
        &runtime,
        "claude",
        "child-1",
        &crate::agents::context::SubagentContext {
            agent_type: Some("Explore".to_owned()),
            description: Some("trace the sidebar rows".to_owned()),
            token_count: Some(12_400),
            started_at: Some(now),
            observed_at: now,
        },
    )
    .unwrap();

    let snapshot = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        None,
    )
    .expect("published base");
    let parent = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .find(|row| row.id == "parent-1")
        .expect("parent row");

    assert_eq!(parent.sub_agents().len(), 1);
    assert_eq!(parent.sub_agents()[0].id, "child-1");
    assert_eq!(parent.sub_agents()[0].name, "Explore");
    assert_eq!(
        parent.sub_agents()[0].description.as_deref(),
        Some("trace the sidebar rows"),
    );
    assert_eq!(parent.sub_agents()[0].total_tokens, Some(12_400));
}

#[test]
fn consumer_own_view_counts_siblings_in_its_own_tab() {
    // A consumer reads the producer's session-wide pane list (`list-panes
    // -a`) and folds its own-view from it. An orphan sidebar — alone in its
    // tab — must see `Some(0)` siblings so self-close can fire, even though
    // the producer lives in another tab with its own siblings.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    let main_sb = pane_in_tab("main_sb", "@0");
    let main_term = pane_in_tab("main_term", "@0");
    let orphan_sb = pane_in_tab("orphan_sb", "@1");
    let base = assemble_frame(
        vec![main_sb, main_term, orphan_sb],
        unix_now_ms(),
        "rimz-test",
    );
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &base).unwrap();
    // The rollup the consumer folds the panes over: an empty room, published
    // to `latest.json` where the consumer reads it fresh.
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    let rollup = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
    atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();

    let orphan_own = PaneId::from_parts(MuxName::Zellij, "orphan_sb");
    let snapshot = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        Some(&orphan_own),
    )
    .expect("base");
    assert_eq!(
        snapshot.own_view.map(|view| view.sibling_count),
        Some(0),
        "an orphan sidebar sees zero siblings in its own tab so self-close can fire"
    );
}

#[test]
fn read_published_snapshot_is_frameless_until_the_producer_publishes() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    // No published pane set yet (the producer hasn't run), so the consumer
    // read folds the store rollup without pane-admitted cards rather than
    // reporting a failed snapshot.
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    let mut rollup = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
    rollup.display_name = "cold-room".to_owned();
    rollup.reflects_log = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();

    let snapshot = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        None,
    )
    .expect("frameless rollup");

    assert_eq!(snapshot.display_name, "cold-room");
    assert_eq!(snapshot.panes_produced_at_ms, None);
    assert!(snapshot.worktree_groups.is_empty());
}

#[test]
fn read_published_snapshot_reports_why_the_store_was_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let state = StatePaths::under(workspace, dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    // A directory where the event log should be: the row scan's read fails,
    // and with no `latest.json` the rollup read has no fallback.
    std::fs::create_dir_all(&state.events_log).unwrap();

    let err = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        None,
    )
    .expect_err("an unreadable store rollup is the one failed consumer read");
    assert!(
        err.to_string()
            .contains(&state.events_log.display().to_string()),
        "the error names the unreadable path, got: {err}"
    );
}

#[test]
fn no_frame_enrich_preserves_rollup_metadata_but_emits_no_groups() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let agent = root_agent("claude", "sess-1", None);

    let snapshot = enrich(
        SidebarSnapshot::build_with_agents(workspace, vec![agent], Timestamp::now()),
        None,
        &runtime,
        None,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(snapshot.panes_produced_at_ms, None);
    assert_eq!(snapshot.agents.len(), 1);
    assert!(snapshot.worktree_groups.is_empty());
}

#[test]
fn enrich_maps_carried_frame_to_truth_notice() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let carried_id = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let mut frame = assemble_frame(
        vec![pane("terminal_1", "zsh", "/repo/main")],
        1_234,
        "rimz-test",
    );
    frame.carried_panes = vec![CarriedPane {
        pane_id: carried_id.clone(),
        pid: Some(42),
        start_ticks: Some(9),
        carried_since_ms: 1_000,
    }];

    let snapshot = enrich(
        SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now()),
        Some(&frame),
        &runtime,
        None,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(
        snapshot.truth_degraded,
        Some(crate::TruthNotice {
            carried: 1,
            since_ms: 1_000,
            pane_ids: vec![carried_id],
        })
    );
}

#[test]
fn consumer_reflects_a_fresh_rollup_over_a_stale_pane_cache() {
    // The event-fresh split: the consumer reads the rollup from `latest.json`
    // each call, so a status change shows even when the producer's published
    // pane cache has not moved. Republishing `latest.json` alone changes the
    // rendered rollup.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();

    // A published (and never re-published) pane cache.
    let panes = assemble_frame(Vec::new(), unix_now_ms(), "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &panes).unwrap();

    // A served publish carries the extent stamp; the workspace has no
    // events, so the matching extent is the empty log's.
    let stamp = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    let mut alpha = SidebarSnapshot::build(workspace.clone(), Vec::new(), Timestamp::now());
    alpha.display_name = "alpha".to_owned();
    alpha.reflects_log = stamp;
    atomic::write_temp_then_rename(&state.latest_snapshot, &alpha).unwrap();
    let first = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        None,
    )
    .expect("base");
    assert_eq!(first.display_name, "alpha");

    // Republish ONLY `latest.json` (a different length so the parse cache
    // cannot mask the change); the pane cache is untouched.
    let mut bravo = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
    bravo.display_name = "bravo-the-second-rollup".to_owned();
    bravo.reflects_log = stamp;
    atomic::write_temp_then_rename(&state.latest_snapshot, &bravo).unwrap();
    let second = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        None,
    )
    .expect("base");
    assert_eq!(
        second.display_name, "bravo-the-second-rollup",
        "the consumer folds the fresh rollup, not a cached one"
    );
}
