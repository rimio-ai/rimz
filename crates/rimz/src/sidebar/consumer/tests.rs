use super::*;
use crate::feed::{FeedItem, FeedKind, Surface};
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::ledger::atomic;
use crate::sidebar::cache::{DiffStatsCache, DiffStatsCacheEntry, unix_now_ms};
use crate::sidebar::enrich::{EnrichMode, enrich};
use crate::sidebar::frame::{CarriedPane, assemble_frame};
use crate::sidebar::test_support::{child_agent, pane, pane_in_tab, root_agent};
use crate::{RuntimePaths, SidebarSnapshot, SidebarWorktreeKind, StatePaths};
use jiff::Timestamp;

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
    let mut rollup =
        SidebarSnapshot::build(workspace.clone(), Vec::new(), Vec::new(), Timestamp::now());
    rollup = rollup.with_project_root(Some(worktree.clone()));
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();
    let panes = vec![
        pane("terminal_0", "zsh", &wt),
        pane("terminal_own", "rimz-sidebar", &wt),
    ];
    let base = assemble_frame(panes, unix_now_ms(), "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.root.join("snapshot.json"), &base).unwrap();

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
        },
    );
    atomic::write_temp_then_rename_cache(&runtime.root.join("diff-stats.json"), &diff).unwrap();

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
        Vec::new(),
        vec![parent, child],
        Timestamp::now(),
    );
    rollup = rollup.with_project_root(Some(worktree));
    rollup.reflects_log = Some(crate::ledger::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();

    let base = assemble_frame(vec![live_pane], unix_now_ms(), "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.root.join("snapshot.json"), &base).unwrap();
    let now = Timestamp::now();
    crate::ledger::subagent_context::write(
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
    atomic::write_temp_then_rename_cache(&runtime.root.join("snapshot.json"), &base).unwrap();
    // The rollup the consumer folds the panes over: an empty room, published
    // to `latest.json` where the consumer reads it fresh.
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    let rollup = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now());
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
    // read folds the ledger rollup without pane-admitted cards rather than
    // reporting a failed snapshot.
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    let mut rollup = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now());
    rollup.display_name = "cold-room".to_owned();
    rollup.reflects_log = Some(crate::ledger::event_log::LogExtent {
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
fn read_published_snapshot_reports_why_the_ledger_was_unreadable() {
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
    .expect_err("an unreadable ledger rollup is the one failed consumer read");
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
    let mut item = FeedItem::new(
        workspace.clone(),
        Surface::Script,
        FeedKind::Question,
        "approve deploy?",
        "deploy",
        "script",
    );
    item.pane = Some(pane("terminal_1", "deploy", "/repo/main"));
    let agent = root_agent("claude", "sess-1", None);

    let snapshot = enrich(
        SidebarSnapshot::build_with_agents(workspace, vec![item], vec![agent], Timestamp::now()),
        None,
        &runtime,
        None,
        EnrichMode::Cached,
        None,
    );

    assert_eq!(snapshot.panes_produced_at_ms, None);
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.needs_attention.len(), 1);
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
        SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now()),
        Some(frame),
        &runtime,
        None,
        EnrichMode::Cached,
        None,
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
    atomic::write_temp_then_rename_cache(&runtime.root.join("snapshot.json"), &panes).unwrap();

    // A served publish carries the extent stamp; the workspace has no
    // events, so the matching extent is the empty log's.
    let stamp = Some(crate::ledger::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    let mut alpha =
        SidebarSnapshot::build(workspace.clone(), Vec::new(), Vec::new(), Timestamp::now());
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
    let mut bravo = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now());
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
