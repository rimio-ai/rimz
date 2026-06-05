use super::*;
use crate::agents::TurnPhase;
use crate::feed::{AgentState, AgentStatus};
use crate::ids::{MuxName, WorkspaceId};
use crate::ledger::atomic;

fn pane(id: &str, command: &str, cwd: &str) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, id),
        session_name: "rimz-test".to_owned(),
        view_id: Some("@0".to_owned()),
        view_kind: None,
        view_name: None,
        is_focused: false,
        command: Some(command.to_owned()),
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
    }
}

fn pane_in_tab(id: &str, view_id: &str) -> PaneRef {
    PaneRef {
        view_id: Some(view_id.to_owned()),
        ..pane(id, "zsh", "/tmp")
    }
}

#[test]
fn published_frame_age_is_session_scoped_and_saturating() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    let produced_at_ms = 1_700_000_000_000;
    let cache = SnapshotCache {
        produced_at_ms,
        session_name: "rimz-test".to_owned(),
        panes: Vec::new(),
    };
    atomic::write_temp_then_rename_cache(&runtime.root.join("snapshot.json"), &cache).unwrap();

    assert_eq!(
        published_frame_age_ms(&runtime, "rimz-test", produced_at_ms + 1_500),
        Some(1_500)
    );
    // A clock that ran backwards saturates to age 0 rather than wrapping huge
    // and forcing a needless fork.
    assert_eq!(
        published_frame_age_ms(&runtime, "rimz-test", produced_at_ms - 1),
        Some(0)
    );
    // A frame stamped for another session never matches: the fork gate reads
    // `None` as "no usable frame", which is the election's job to fill.
    assert_eq!(
        published_frame_age_ms(&runtime, "other-session", produced_at_ms),
        None
    );

    // No published frame at all → `None` (the cold start).
    let empty = tempfile::tempdir().unwrap();
    let empty_rt =
        RuntimePaths::under(WorkspaceId::from_project_root(empty.path()), empty.path()).unwrap();
    empty_rt.ensure_dirs().unwrap();
    assert_eq!(
        published_frame_age_ms(&empty_rt, "rimz-test", produced_at_ms),
        None
    );
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
    let base = SnapshotCache {
        produced_at_ms: unix_now_ms(),
        session_name: "rimz-test".to_owned(),
        panes,
    };
    atomic::write_temp_then_rename_cache(&runtime.root.join("snapshot.json"), &base).unwrap();

    // Publish diff stats for the worktree path: +7 / -2, 3 commits ahead and
    // 1 behind a remote-default trunk, on branch `feat`.
    let mut diff = DiffStatsCache::default();
    diff.entries.insert(
        wt.clone(),
        DiffStatsCacheEntry::new(
            unix_now_ms(),
            Some(DiffStats {
                added: 7,
                removed: 2,
            }),
            Some(3),
            Some(1),
            Some("origin/main".to_owned()),
            Some("feat".to_owned()),
            Some(false),
        ),
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

    let base = SnapshotCache {
        produced_at_ms: unix_now_ms(),
        session_name: "rimz-test".to_owned(),
        panes: vec![live_pane],
    };
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

    assert_eq!(parent.sub_agents.len(), 1);
    assert_eq!(parent.sub_agents[0].id, "child-1");
    assert_eq!(parent.sub_agents[0].name, "Explore");
    assert_eq!(
        parent.sub_agents[0].description.as_deref(),
        Some("trace the sidebar rows"),
    );
    assert_eq!(parent.sub_agents[0].total_tokens, Some(12_400));
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
    let base = SnapshotCache {
        produced_at_ms: unix_now_ms(),
        session_name: "rimz-test".to_owned(),
        panes: vec![main_sb, main_term, orphan_sb],
    };
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
fn read_published_snapshot_is_none_until_the_producer_publishes() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    // No published pane set yet (the producer hasn't run), so the consumer
    // read is `None` regardless of the rollup — the caller holds last-good.
    let state = StatePaths::under(workspace, dir.path()).unwrap();
    assert!(
        read_published_snapshot(
            &mut RollupCursor::new(),
            &state,
            &runtime,
            "rimz-test",
            None
        )
        .is_none()
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
    let panes = SnapshotCache {
        produced_at_ms: unix_now_ms(),
        session_name: "rimz-test".to_owned(),
        panes: Vec::new(),
    };
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

#[test]
fn read_snapshot_cache_misses_a_different_session() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    let cache = SnapshotCache {
        produced_at_ms: unix_now_ms(),
        session_name: "rimz-one".to_owned(),
        panes: Vec::new(),
    };
    atomic::write_temp_then_rename(&path, &cache).unwrap();
    assert!(read_snapshot_cache(&path, "rimz-one").is_some());
    assert!(read_snapshot_cache(&path, "rimz-other").is_none());
}

#[test]
fn read_snapshot_cache_reflects_a_changed_file() {
    // The thread-local parse cache must invalidate when the file changes, or
    // a consumer would serve a stale base forever. Keyed on (mtime, len), so
    // a differently-sized rewrite is caught even if the filesystem's mtime
    // granularity is too coarse to register two fast writes.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");

    let first = SnapshotCache {
        produced_at_ms: unix_now_ms(),
        session_name: "rimz-one".to_owned(),
        panes: Vec::new(),
    };
    atomic::write_temp_then_rename_cache(&path, &first).unwrap();
    // Populate this thread's parse cache.
    assert_eq!(
        read_snapshot_cache(&path, "rimz-one").map(|c| c.panes.len()),
        Some(0),
    );

    // Republish a longer, different-session frame in place.
    let second = SnapshotCache {
        produced_at_ms: unix_now_ms() + 1,
        session_name: "rimz-two".to_owned(),
        panes: vec![pane("terminal_0", "zsh", "/tmp")],
    };
    atomic::write_temp_then_rename_cache(&path, &second).unwrap();
    // The stale (rimz-one) entry must not be served; the fresh frame wins.
    assert!(read_snapshot_cache(&path, "rimz-one").is_none());
    assert_eq!(
        read_snapshot_cache(&path, "rimz-two").map(|c| c.panes.len()),
        Some(1),
    );
}

#[test]
fn diff_stats_cache_entry_expires_after_ttl() {
    let entry = DiffStatsCacheEntry::new(
        1_000,
        Some(DiffStats {
            added: 2,
            removed: 1,
        }),
        Some(4),
        Some(2),
        Some("main".to_owned()),
        Some("feature-migration".to_owned()),
        Some(true),
    );

    assert!(entry.is_fresh(1_000 + DIFF_STATS_TTL.as_millis() as u64));
    assert!(!entry.is_fresh(1_001 + DIFF_STATS_TTL.as_millis() as u64));
    assert_eq!(
        entry.stats(),
        Some(DiffStats {
            added: 2,
            removed: 1,
        })
    );
    assert_eq!(entry.commits, Some(4));
    assert_eq!(entry.behind, Some(2));
    assert_eq!(entry.trunk.as_deref(), Some("main"));
    assert_eq!(entry.branch.as_deref(), Some("feature-migration"));
    assert_eq!(entry.clean, Some(true));
}

/// An old producer's cache entry predates the `clean` column; it must read
/// back as "not probed" (`None`), never flash a landed marker it can't prove.
#[test]
fn diff_stats_cache_entry_without_clean_reads_none() {
    let entry: DiffStatsCacheEntry = serde_json::from_str(
        r#"{"refreshed_at_ms":1000,"added":0,"removed":0,"commits":0,"behind":3,"trunk":"main","branch":"feat"}"#,
    )
    .unwrap();

    assert_eq!(entry.clean, None);
    assert_eq!(entry.stats(), Some(DiffStats::default()));
}

/// A 5-hour budget window for tests — a known `duration_mins` so the
/// projection and per-duration reconciliation have something to key on.
fn rl_window(used: u8, resets_at: Option<Timestamp>) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage: Some(used),
        resets_at,
        duration_mins: Some(300),
    }
}

fn provider_panel(kind: &str, windows: Vec<RateLimitWindow>) -> crate::SidebarProviderPanel {
    crate::SidebarProviderPanel {
        kind: kind.to_owned(),
        product_name: kind.to_owned(),
        art: Vec::new(),
        color: 0,
        version: None,
        plan: None,
        metered: true,
        remote_control: false,
        spending: None,
        windows,
    }
}

fn snapshot_with_panels(
    workspace: WorkspaceId,
    panels: Vec<crate::SidebarProviderPanel>,
) -> SidebarSnapshot {
    let mut snapshot = SidebarSnapshot::build(workspace, Vec::new(), Vec::new(), Timestamp::now());
    snapshot.providers = panels;
    snapshot
}

fn root_agent(kind: &str, agent_id: &str, model: Option<&str>) -> AgentState {
    let now = Timestamp::now();
    AgentState {
        agent_id: agent_id.into(),
        kind: crate::ids::AgentKind::new_unchecked(kind),
        status: AgentStatus::Running,
        phase: TurnPhase::Idle,
        pane: None,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        parent_agent_id: None,
        worktree_path: None,
        worktree_branch: None,
        task: None,
        prompt: None,
        model: model.map(ToOwned::to_owned),
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        compacting_since: None,
        last_seen: now,
        last_activity: now,
        registered_at: Some(now),
    }
}

fn child_agent(kind: &str, parent_id: &str, agent_id: &str) -> AgentState {
    let mut agent = root_agent(kind, agent_id, None);
    agent.parent_agent_id = Some(parent_id.into());
    agent
}

/// A window whose reset instant is still in the future projects unchanged —
/// the last-known drained reading stands while the budget is genuinely spent.
#[test]
fn idle_window_before_reset_shows_last_known() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let future = Timestamp::from_second(2_000_010_000).unwrap();
    let cached = rl_window(80, Some(future));
    let projected = project_idle_window(cached.clone(), now);
    assert_eq!(projected, cached, "before reset the cached reading stands");
}

/// Once `now` reaches the reset instant with no fresh reading, the window has
/// refilled: it projects to full (0% used) with its reset rolled its own
/// duration forward, so the countdown still reads sensibly.
#[test]
fn idle_window_past_reset_refills_to_full_and_rolls_forward() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let passed = Timestamp::from_second(1_999_990_000).unwrap();
    let projected = project_idle_window(rl_window(95, Some(passed)), now);
    assert_eq!(projected.used_percentage, Some(0), "a reset window is full");
    assert_eq!(
        projected.resets_at,
        now.checked_add(SignedDuration::from_secs(300 * 60)).ok(),
        "the reset rolls one window length (300 min) forward from now"
    );
}

/// A cached window that can't be aged — no reset instant, or a passed reset
/// with no known duration to roll by — projects as-is.
#[test]
fn idle_window_unageable_shows_as_is() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let passed = Timestamp::from_second(1_999_990_000).unwrap();
    let undated = rl_window(40, None);
    assert_eq!(project_idle_window(undated.clone(), now), undated);
    let no_duration = RateLimitWindow {
        used_percentage: Some(90),
        resets_at: Some(passed),
        duration_mins: None,
    };
    assert_eq!(project_idle_window(no_duration.clone(), now), no_duration);
}

/// The producer persists a live reading as ground truth; once the session is
/// idle (no live window), a reader projects that reading back onto the panel,
/// so the dashboard shows last-known budgets instead of an empty bar.
#[test]
fn producer_persists_live_windows_for_idle_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();

    // A live frame reports 60% used on the 5h window; the producer writes it.
    let mut producing = snapshot_with_panels(
        workspace.clone(),
        vec![provider_panel("claude", vec![rl_window(60, Some(future))])],
    );
    apply_rate_limit_cache(&mut producing, &runtime, true);
    let cache = read_rate_limits_cache(&runtime.root.join("rate_limits.json"));
    assert_eq!(
        cache
            .windows
            .get("claude")
            .and_then(|limits| limits.windows.first())
            .and_then(|window| window.used_percentage),
        Some(60),
        "the live reading is persisted as ground truth"
    );

    // The session goes idle (no live window). A reader projects the cached
    // reading back onto the panel — the dashboard is not empty.
    let mut idle = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    apply_rate_limit_cache(&mut idle, &runtime, false);
    assert_eq!(
        idle.providers[0]
            .windows
            .first()
            .and_then(|window| window.used_percentage),
        Some(60),
        "an idle frame still shows the last-known budget"
    );
}

/// An idle window whose reset has long passed projects to full, but the
/// producer keeps persisting the real last reading — the synthesized full
/// window is a read-time projection, never written back.
#[test]
fn idle_past_reset_shows_full_without_persisting_the_synthetic_window() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let passed = Timestamp::from_second(1_000_000_000).unwrap(); // 2001 — always past

    // Seed a drained reading whose reset has long since passed.
    let path = runtime.root.join("rate_limits.json");
    write_rate_limits_cache(
        &path,
        &RateLimitsCache {
            refreshed_at_ms: 0,
            windows: BTreeMap::from([(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![rl_window(90, Some(passed))],
                },
            )]),
        },
    );

    // An idle producer frame with no live window: the display projects to
    // full, while the persisted ground truth stays the real 90% reading.
    let mut idle = snapshot_with_panels(workspace, vec![provider_panel("claude", Vec::new())]);
    apply_rate_limit_cache(&mut idle, &runtime, true);
    let shown = idle.providers[0].windows.first().expect("a full window");
    assert_eq!(shown.used_percentage, Some(0), "a reset window shows full");
    assert!(shown.resets_at.is_some(), "with a rolled-forward countdown");

    let persisted = read_rate_limits_cache(&path);
    assert_eq!(
        persisted
            .windows
            .get("claude")
            .and_then(|limits| limits.windows.first())
            .and_then(|window| window.used_percentage),
        Some(90),
        "the cache retains ground truth, not the synthesized full window"
    );
}

/// When one provider logs out while another stays, the logged-out kind loses
/// its panel, so the producer's next write — rebuilt from the panels alone —
/// drops its cached windows while the surviving kind's are kept. Cache
/// presence tracks login, so no stale budget can flash on a later re-login.
#[test]
fn producer_drops_windows_for_a_logged_out_provider() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let future = Timestamp::from_second(4_000_000_000).unwrap();
    let path = runtime.root.join("rate_limits.json");
    let windows = |used| vec![rl_window(used, Some(future))];

    // Seed windows for both providers through a live frame.
    let mut seeded = snapshot_with_panels(
        workspace.clone(),
        vec![
            provider_panel("claude", windows(40)),
            provider_panel("codex", windows(30)),
        ],
    );
    apply_rate_limit_cache(&mut seeded, &runtime, true);
    let seeded_cache = read_rate_limits_cache(&path);
    assert!(seeded_cache.windows.contains_key("claude"));
    assert!(seeded_cache.windows.contains_key("codex"));

    // Codex logs out: only claude has a panel now. The next producer write
    // rebuilds the cache from the surviving panels, so codex drops out while
    // claude's windows are kept.
    let mut codex_gone =
        snapshot_with_panels(workspace, vec![provider_panel("claude", windows(40))]);
    apply_rate_limit_cache(&mut codex_gone, &runtime, true);
    let after = read_rate_limits_cache(&path);
    assert!(
        after.windows.contains_key("claude"),
        "a still-logged-in provider keeps its windows"
    );
    assert!(
        !after.windows.contains_key("codex"),
        "a logged-out provider's windows drop on the next write"
    );
}

/// The out-of-band helper seeds one kind's windows into the shared cache
/// without disturbing another kind's, so an idle provider's bars paint from
/// the next producer frame.
#[test]
fn merge_account_rate_limits_seeds_a_kind_without_clobbering_others() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let path = runtime.root.join("rate_limits.json");

    // Claude already has cached windows from a live session this run.
    write_rate_limits_cache(
        &path,
        &RateLimitsCache {
            refreshed_at_ms: 1,
            windows: BTreeMap::from([(
                "claude".to_owned(),
                AgentRateLimits {
                    windows: vec![rl_window(20, None)],
                },
            )]),
        },
    );

    merge_account_rate_limits(
        &runtime,
        "codex",
        AgentRateLimits {
            windows: vec![rl_window(55, None)],
        },
    );

    let cache = read_rate_limits_cache(&path);
    assert_eq!(
        cache
            .windows
            .get("codex")
            .and_then(|limits| limits.windows.first())
            .and_then(|w| w.used_percentage),
        Some(55),
        "the idle provider's windows are seeded"
    );
    assert!(
        cache.windows.contains_key("claude"),
        "an existing kind's windows are preserved"
    );
}

#[test]
fn active_codex_session_refreshes_context_even_when_windows_exist() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let mut snapshot = snapshot_with_panels(
        workspace,
        vec![provider_panel("codex", vec![rl_window(42, None)])],
    );
    snapshot
        .agents
        .push(root_agent("codex", "sess-active", Some("gpt-5.5-codex")));

    assert_eq!(
        codex_rate_limit_refreshes(&snapshot),
        vec![CodexRateLimitRefresh::Session {
            session_id: "sess-active".to_owned(),
            model_hint: Some("gpt-5.5-codex".to_owned()),
        }],
        "active Codex sessions refresh their sidecars even when the dashboard already has windows"
    );
}

#[test]
fn idle_metered_codex_refreshes_account_cache() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let snapshot = snapshot_with_panels(
        workspace,
        vec![provider_panel("codex", vec![rl_window(25, None)])],
    );

    assert_eq!(
        codex_rate_limit_refreshes(&snapshot),
        vec![CodexRateLimitRefresh::Account],
        "idle Codex accounts refresh the shared cache even with prior windows"
    );
}

#[test]
fn active_codex_sessions_win_over_account_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let mut snapshot = snapshot_with_panels(workspace, vec![provider_panel("codex", Vec::new())]);
    snapshot
        .agents
        .push(root_agent("codex", "sess-active", None));

    assert_eq!(
        codex_rate_limit_refreshes(&snapshot),
        vec![CodexRateLimitRefresh::Session {
            session_id: "sess-active".to_owned(),
            model_hint: None,
        }],
        "a stale account cache cannot refresh underneath a live Codex sidecar"
    );
}

#[test]
fn unmetered_or_non_codex_providers_do_not_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let mut unmetered_codex = provider_panel("codex", Vec::new());
    unmetered_codex.metered = false;
    let snapshot = snapshot_with_panels(
        workspace,
        vec![unmetered_codex, provider_panel("claude", Vec::new())],
    );

    assert!(
        codex_rate_limit_refreshes(&snapshot).is_empty(),
        "only metered Codex has an out-of-band budget read"
    );
}

/// The per-target throttle marker gates the out-of-band fetch: the first call is
/// due (and touches the marker), the immediate next is not.
#[test]
fn codex_rate_limit_probe_throttles_per_target() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let account = CodexRateLimitRefresh::Account;
    let session = CodexRateLimitRefresh::Session {
        session_id: "sess/one".to_owned(),
        model_hint: None,
    };
    let other_session = CodexRateLimitRefresh::Session {
        session_id: "sess/two".to_owned(),
        model_hint: None,
    };

    assert!(codex_rate_limit_probe_due(&runtime, &account));
    assert!(
        !codex_rate_limit_probe_due(&runtime, &account),
        "a freshly-stamped account backs off"
    );
    assert!(codex_rate_limit_probe_due(&runtime, &session));
    assert!(
        !codex_rate_limit_probe_due(&runtime, &session),
        "a freshly-stamped session backs off"
    );
    assert!(
        codex_rate_limit_probe_due(&runtime, &other_session),
        "a different session has its own marker"
    );

    let old = SystemTime::now()
        .checked_sub(CODEX_RATE_LIMIT_REFRESH_INTERVAL + Duration::from_secs(1))
        .unwrap();
    std::fs::File::open(codex_rate_limit_probe_marker(&runtime, &session))
        .unwrap()
        .set_modified(old)
        .unwrap();
    assert!(
        codex_rate_limit_probe_due(&runtime, &session),
        "the session becomes due again after the 60s interval"
    );
}

/// Only Codex exposes an account-scoped window read today; Claude's windows
/// have no source outside a live statusline, so it never triggers a fetch.
#[test]
fn only_codex_has_an_out_of_band_window_read() {
    assert!(provider_has_out_of_band_windows("codex"));
    assert!(!provider_has_out_of_band_windows("claude"));
    assert!(!provider_has_out_of_band_windows("pi"));
}

/// The config fold stamps every *agent* row's context-severity verdict from
/// the `[sidebar.context]` bands — the one classification the renderer's color
/// ramp and any future signal emitter read — and leaves process rows `None`.
#[test]
fn config_fold_stamps_agent_context_severity() {
    let row = |kind: crate::SidebarRowKind, pct: Option<u8>| crate::SidebarRow {
        row_kind: kind,
        id: "row".to_owned(),
        name: "claude".to_owned(),
        status: Some(AgentStatus::Running),
        phase: TurnPhase::Idle,
        pane: None,
        request_id: None,
        surface: None,
        task: None,
        prompt: None,
        model: None,
        effort: None,
        context_pct: pct,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        context_severity: None,
        worktree_path: None,
        worktree_branch: None,
        last_activity: jiff::Timestamp::now(),
        registered_at: None,
        resolver: None,
        options: Vec::new(),
        sub_agents: Vec::new(),
        process_active: false,
        command_detail: None,
        compacting: false,
        turn_error_label: None,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
    };
    let mut groups = vec![crate::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: vec![
            row(crate::SidebarRowKind::Agent, Some(85)),
            row(crate::SidebarRowKind::Agent, Some(5)),
            row(crate::SidebarRowKind::Process, None),
        ],
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
    }];

    stamp_context_severity(
        &mut groups,
        &crate::config::ContextSeverityConfig::default(),
    );

    let rows = &groups[0].rows;
    assert_eq!(
        rows[0].context_severity,
        Some(crate::feed::ContextSeverity::Amber),
        "85% crosses the default amber band"
    );
    assert_eq!(
        rows[1].context_severity,
        Some(crate::feed::ContextSeverity::Calm)
    );
    assert_eq!(
        rows[2].context_severity, None,
        "a process row carries no context verdict"
    );
}

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
fn activity_row(
    row_kind: crate::SidebarRowKind,
    status: Option<AgentStatus>,
    last_activity: Timestamp,
    worktree_path: &Path,
) -> crate::SidebarRow {
    crate::SidebarRow {
        row_kind,
        id: "row".to_owned(),
        name: "claude".to_owned(),
        status,
        phase: TurnPhase::Idle,
        pane: None,
        request_id: None,
        surface: None,
        task: None,
        prompt: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        context_severity: None,
        worktree_path: Some(worktree_path.display().to_string()),
        worktree_branch: None,
        last_activity,
        registered_at: None,
        resolver: None,
        options: Vec::new(),
        sub_agents: Vec::new(),
        process_active: false,
        command_detail: None,
        turn_error_label: None,
        compacting: false,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
    }
}

fn worktree_group(path: &Path, rows: Vec<crate::SidebarRow>) -> crate::SidebarWorktreeGroup {
    crate::SidebarWorktreeGroup {
        key: path.display().to_string(),
        label: "wt".to_owned(),
        kind: crate::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows,
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        clean: None,
    }
}

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
                crate::SidebarRowKind::Agent,
                Some(AgentStatus::Running),
                stale_activity,
                &running,
            )],
        ),
        worktree_group(
            &recent,
            vec![activity_row(
                crate::SidebarRowKind::Agent,
                Some(AgentStatus::Idle),
                now - SignedDuration::from_secs(1),
                &recent,
            )],
        ),
        // Exactly the window boundary stays hot (<=, matching the TTL gates).
        worktree_group(
            &boundary,
            vec![activity_row(
                crate::SidebarRowKind::Agent,
                Some(AgentStatus::Idle),
                now - window,
                &boundary,
            )],
        ),
        worktree_group(
            &idle,
            vec![activity_row(
                crate::SidebarRowKind::Agent,
                Some(AgentStatus::Idle),
                stale_activity,
                &idle,
            )],
        ),
        // A busy process row is not an agent row: cold by definition.
        worktree_group(
            &procs,
            vec![activity_row(
                crate::SidebarRowKind::Process,
                None,
                now,
                &procs,
            )],
        ),
        // An External-kind group never reaches the git refresh.
        {
            let mut group = worktree_group(
                &external_kind,
                vec![activity_row(
                    crate::SidebarRowKind::Agent,
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
            vec![activity_row(
                crate::SidebarRowKind::Agent,
                Some(AgentStatus::Running),
                now,
                &dead,
            )],
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
            crate::SidebarRowKind::Agent,
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

#[test]
fn presence_event_mode_boundary_is_inclusive() {
    let fresh_edge = PRESENCE_STAMP_FRESH.as_millis() as u64;
    assert!(presence_event_mode(Some(0)));
    assert!(presence_event_mode(Some(fresh_edge)));
    assert!(!presence_event_mode(Some(fresh_edge + 1)));
    assert!(!presence_event_mode(None), "absent stamp is poll mode");
}

#[test]
fn effective_pane_ttl_selects_by_mode() {
    assert_eq!(effective_pane_ttl(Some(0)), EVENT_PANE_TTL);
    assert_eq!(
        effective_pane_ttl(Some(PRESENCE_STAMP_FRESH.as_millis() as u64 + 1)),
        SNAPSHOT_CACHE_TTL
    );
    assert_eq!(effective_pane_ttl(None), SNAPSHOT_CACHE_TTL);
}

#[test]
fn presence_stamp_round_trips_through_the_runtime_root() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();

    assert_eq!(
        presence_stamp_age_ms(&runtime),
        None,
        "no stamp yet: poll mode"
    );
    write_presence_stamp(&runtime);
    let age = presence_stamp_age_ms(&runtime).expect("stamp written and readable");
    assert!(
        age < 1_000,
        "a just-written stamp reads as young, got {age}ms"
    );
    assert!(presence_event_mode(Some(age)));
}

#[test]
fn presence_stamp_from_a_future_clock_saturates_to_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();

    let future = PresenceStamp {
        written_at_ms: unix_now_ms() + 60_000,
    };
    atomic::write_temp_then_rename_cache(&presence_stamp_path(&runtime), &future).unwrap();
    assert_eq!(
        presence_stamp_age_ms(&runtime),
        Some(0),
        "a stamp ahead of this reader's clock saturates to age 0, never poll mode"
    );
}

#[test]
fn unreadable_presence_stamp_reads_poll_mode() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();

    std::fs::create_dir_all(&runtime.root).unwrap();
    std::fs::write(presence_stamp_path(&runtime), b"{ not json").unwrap();
    assert_eq!(presence_stamp_age_ms(&runtime), None);
    assert!(!presence_event_mode(presence_stamp_age_ms(&runtime)));
}

fn cache_produced_at(produced_at_ms: u64) -> SnapshotCache {
    SnapshotCache {
        produced_at_ms,
        session_name: "rimz-test".to_owned(),
        panes: Vec::new(),
    }
}

#[test]
fn event_mode_serves_a_cache_poll_mode_would_reject() {
    let now = unix_now_ms();
    let five_seconds_old = cache_produced_at(now - 5_000);
    assert!(
        snapshot_cache_is_fresh(&five_seconds_old, now, None, EVENT_PANE_TTL),
        "5s-old cache serves under the 10s event TTL: no list-panes fork"
    );
    assert!(
        !snapshot_cache_is_fresh(&five_seconds_old, now, None, SNAPSHOT_CACHE_TTL),
        "the same cache misses under the 750ms poll TTL"
    );

    let one_second_old = cache_produced_at(now - 1_000);
    assert!(
        !snapshot_cache_is_fresh(&one_second_old, now, None, SNAPSHOT_CACHE_TTL),
        "a stale stamp reverts to poll mode: a 1s-old cache no longer serves"
    );
}

#[test]
fn forced_pane_freshness_overrides_event_mode() {
    let now = unix_now_ms();
    let five_seconds_old = cache_produced_at(now - 5_000);
    assert!(
        !snapshot_cache_is_fresh(&five_seconds_old, now, Some(now), EVENT_PANE_TTL),
        "a lifecycle/resize floor rejects a pre-signal cache regardless of TTL"
    );
    assert!(
        snapshot_cache_is_fresh(&five_seconds_old, now, Some(now - 5_000), EVENT_PANE_TTL),
        "a cache at the floor is usable"
    );
}

#[test]
fn snapshot_cache_age_saturates_on_a_future_producer_clock() {
    let now = unix_now_ms();
    let future = cache_produced_at(now + 60_000);
    assert!(
        snapshot_cache_is_fresh(&future, now, None, SNAPSHOT_CACHE_TTL),
        "a cache stamped ahead of this reader serves rather than re-producing every call"
    );
}
