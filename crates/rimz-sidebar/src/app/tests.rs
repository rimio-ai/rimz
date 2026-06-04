use super::*;
use rimz::feed::PaneRef;

fn workspace() -> WorkspaceId {
    WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap()
}

fn snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    placeholder_snapshot(ws.clone())
}

fn pane(raw: &str, view: &str, focused: bool) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some(view.to_owned()),
        view_kind: Some(rimz::ids::ViewKind::Tab),
        view_name: None,
        is_focused: focused,
        command: Some("zsh".to_owned()),
        cwd: Some("/repo/main".to_owned()),
        pane_pid: None,
        pane_process_start: None,
        rss_kb: None,
        cpu_pct: None,
        io_bps: None,
    }
}

fn snapshot_with_panes(ws: &WorkspaceId, panes: Vec<PaneRef>) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    snapshot.worktree_groups = vec![rimz::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: rimz::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: panes
            .into_iter()
            .map(|pane| rimz::SidebarRow {
                row_kind: rimz::SidebarRowKind::Process,
                id: pane.pane_id.to_string(),
                name: pane.command.clone().unwrap_or_else(|| "process".to_owned()),
                status: None,
                phase: rimz::agents::TurnPhase::Idle,
                pane: Some(pane),
                request_id: None,
                surface: None,
                task: None,
                prompt: None,
                model: None,
                effort: None,
                context_pct: None,
                context_window: None,
                total_tokens: None,
                todo_done: None,
                todo_total: None,
                context: None,
                worktree_path: Some("/repo/main".to_owned()),
                worktree_branch: Some("main".to_owned()),
                last_activity: Timestamp::now(),
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
            })
            .collect(),
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
    }];
    snapshot
}

fn agent_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    let row = rimz::SidebarRow {
        row_kind: rimz::SidebarRowKind::Agent,
        id: "agent-1".to_owned(),
        name: "claude".to_owned(),
        status: Some(rimz::feed::AgentStatus::Idle),
        phase: rimz::agents::TurnPhase::Idle,
        pane: Some(pane("terminal_9", "tab_0", false)),
        request_id: None,
        surface: None,
        task: Some("inspect auth".to_owned()),
        prompt: None,
        model: Some("Opus".to_owned()),
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        last_activity: Timestamp::now(),
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
    snapshot.worktree_groups = vec![rimz::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: rimz::SidebarWorktreeKind::Worktree,
        status_counts: vec![rimz::SidebarStatusCount {
            status: rimz::feed::AgentStatus::Idle,
            count: 1,
        }],
        rows: vec![row],
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
    }];
    snapshot
}

/// A group whose first row is a multi-line agent card (model, effort, and
/// context% set so it carries identity + description + gauge, and selecting
/// it reveals its deeper budget-bar and stats lines), followed by a
/// single-line process row, with a non-zero hidden count so a `+K more` line
/// renders. The fixture for the whole-block clickability regression guard.
fn clickable_block_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = snapshot(ws);
    let agent = rimz::SidebarRow {
        row_kind: rimz::SidebarRowKind::Agent,
        id: "agent-1".to_owned(),
        name: "claude".to_owned(),
        status: Some(rimz::feed::AgentStatus::Running),
        phase: rimz::agents::TurnPhase::Idle,
        pane: Some(pane("terminal_9", "tab_0", false)),
        request_id: None,
        surface: None,
        task: Some("inspect auth".to_owned()),
        prompt: None,
        model: Some("Opus".to_owned()),
        effort: Some("high".to_owned()),
        context_pct: Some(38),
        context_window: None,
        total_tokens: Some(12_400),
        todo_done: Some(3),
        todo_total: Some(5),
        context: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        last_activity: Timestamp::now(),
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
    let process = rimz::SidebarRow {
        row_kind: rimz::SidebarRowKind::Process,
        id: "terminal_10".to_owned(),
        name: "zsh".to_owned(),
        status: None,
        phase: rimz::agents::TurnPhase::Idle,
        pane: Some(pane("terminal_10", "tab_0", false)),
        request_id: None,
        surface: None,
        task: None,
        prompt: None,
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        last_activity: Timestamp::now(),
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
    snapshot.worktree_groups = vec![rimz::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: rimz::SidebarWorktreeKind::Worktree,
        status_counts: vec![rimz::SidebarStatusCount {
            status: rimz::feed::AgentStatus::Running,
            count: 1,
        }],
        rows: vec![agent, process],
        hidden_count: 2,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
    }];
    snapshot
}

/// Health seeded with a live alert, as if a failure already crossed the
/// debounce threshold — the starting point for recovery/sticky tests.
fn degraded_health(reason: &str) -> Health {
    Health {
        failure_streak: ALERT_AFTER_FAILURES,
        alert: Some(Alert::active(reason)),
    }
}

#[test]
fn first_ok_fetch_clears_status_and_records_snapshot() {
    let ws = workspace();
    let snap = snapshot(&ws);
    let state = compute_next_state(&ws, None, Ok(snap.clone()), None, &Health::default());
    assert!(state.health.alert.is_none());
    assert_eq!(state.health.failure_streak, 0);
    assert!(state.last_snapshot.is_some());
    assert_eq!(state.snapshot.workspace_id, ws);
}

#[test]
fn single_failure_is_absorbed_without_an_alert() {
    // One flaky tick must not flash a banner: the streak climbs but no
    // alert arms yet, and the last good frame is reused.
    let ws = workspace();
    let previous = snapshot(&ws);
    let state = compute_next_state(
        &ws,
        None,
        Err("ledger not found".to_owned()),
        Some(previous.clone()),
        &Health::default(),
    );
    assert!(state.health.alert.is_none(), "one blip must not alarm");
    assert_eq!(state.health.failure_streak, 1);
    assert!(state.last_snapshot.is_some());
    assert_eq!(state.snapshot.workspace_id, previous.workspace_id);
}

#[test]
fn sustained_failure_raises_active_alert_after_threshold() {
    let ws = workspace();
    let previous = snapshot(&ws);
    let first = compute_next_state(
        &ws,
        None,
        Err("ledger not found".to_owned()),
        Some(previous.clone()),
        &Health::default(),
    );
    let second = compute_next_state(
        &ws,
        None,
        Err("ledger not found".to_owned()),
        first.last_snapshot,
        &first.health,
    );
    let alert = second.health.alert.expect("a sustained failure alerts");
    assert!(alert.is_active());
    assert!(alert.reason.contains("snapshot failed"));
    assert!(alert.reason.contains("ledger not found"));
    assert!(second.last_snapshot.is_some());
}

#[test]
fn sustained_failure_without_previous_snapshot_uses_placeholder() {
    let ws = workspace();
    let err = || Err::<SidebarSnapshot, String>("ledger not found".to_owned());
    let first = compute_next_state(&ws, None, err(), None, &Health::default());
    let second = compute_next_state(&ws, None, err(), None, &first.health);
    assert!(second.health.alert.is_some_and(|alert| alert.is_active()));
    assert!(second.last_snapshot.is_none());
    assert_eq!(second.snapshot.workspace_id, ws);
    assert!(second.snapshot.needs_attention.is_empty());
}

#[test]
fn sustained_heartbeat_failure_alerts_but_keeps_fresh_snapshot() {
    let ws = workspace();
    let snap = snapshot(&ws);
    let first = compute_next_state(
        &ws,
        Some("hb failed".to_owned()),
        Ok(snap.clone()),
        None,
        &Health::default(),
    );
    let second = compute_next_state(
        &ws,
        Some("hb failed".to_owned()),
        Ok(snap.clone()),
        first.last_snapshot,
        &first.health,
    );
    let alert = second
        .health
        .alert
        .expect("sustained heartbeat failure alerts");
    assert!(alert.reason.contains("heartbeat failed"));
    // Heartbeat failing does not invalidate a fresh snapshot.
    assert!(second.last_snapshot.is_some());
}

#[test]
fn active_alert_since_stays_pinned_across_the_episode() {
    let ws = workspace();
    let armed = degraded_health("snapshot failed: first");
    let first_since = armed.alert.as_ref().unwrap().since;
    let next = compute_next_state(
        &ws,
        None,
        Err("second".to_owned()),
        Some(snapshot(&ws)),
        &armed,
    );
    let alert = next.health.alert.expect("still degraded");
    assert_eq!(alert.since, first_since, "since must remain pinned");
    assert!(alert.reason.contains("second"));
}

#[test]
fn recovery_marks_alert_recovered_and_keeps_it_sticky() {
    // Recovery does not erase the alert: it lingers, recovered, until the
    // user dismisses it.
    let ws = workspace();
    let armed = degraded_health("snapshot failed: x");
    let recovered = compute_next_state(&ws, None, Ok(snapshot(&ws)), None, &armed);
    let alert = recovered.health.alert.expect("recovered alert lingers");
    assert!(!alert.is_active());
    assert!(alert.recovered_at.is_some());
    assert_eq!(recovered.health.failure_streak, 0);
}

/// Health seeded with an alert whose episode started at `since`. `recovered`
/// flips it to the sticky-but-inactive (last fetch succeeded) state.
fn degraded_since(since: Timestamp, recovered: bool) -> Health {
    Health {
        failure_streak: ALERT_AFTER_FAILURES,
        alert: Some(Alert {
            reason: "snapshot failed: boom".to_owned(),
            since,
            recovered_at: recovered.then_some(since),
        }),
    }
}

#[test]
fn gives_up_after_sustained_degradation() {
    let base = 1_700_000_000;
    let since = Timestamp::from_second(base).unwrap();
    let now = Timestamp::from_second(base + GIVE_UP_AFTER_DEGRADED.as_secs() as i64).unwrap();
    assert!(degraded_too_long(&degraded_since(since, false), now));
}

#[test]
fn holds_while_degradation_is_still_brief() {
    // A few seconds of failure must not close the sidebar — that is a hiccup
    // or the sub-second gap while `cargo install` swaps the binary.
    let base = 1_700_000_000;
    let since = Timestamp::from_second(base).unwrap();
    let now = Timestamp::from_second(base + 5).unwrap();
    assert!(!degraded_too_long(&degraded_since(since, false), now));
}

#[test]
fn never_gives_up_once_recovered() {
    // A recovered (sticky but inactive) alert means the latest fetch
    // succeeded: the renderer is healthy and must not exit, however old the
    // past episode is.
    let base = 1_700_000_000;
    let since = Timestamp::from_second(base).unwrap();
    let now = Timestamp::from_second(base + 1_000).unwrap();
    assert!(!degraded_too_long(&degraded_since(since, true), now));
}

#[test]
fn never_gives_up_without_an_alert() {
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    assert!(!degraded_too_long(&Health::default(), now));
}

#[test]
fn strip_deleted_suffix_removes_only_the_kernel_annotation() {
    assert_eq!(
        strip_deleted_suffix(Path::new("/usr/bin/rimz-sidebar (deleted)")),
        Some(PathBuf::from("/usr/bin/rimz-sidebar"))
    );
    // A path the kernel did not annotate is left alone.
    assert_eq!(
        strip_deleted_suffix(Path::new("/usr/bin/rimz-sidebar")),
        None
    );
    // " (deleted)" only counts as a trailing suffix, never mid-path.
    assert_eq!(
        strip_deleted_suffix(Path::new("/opt/my (deleted)/rimz-sidebar")),
        None
    );
}

#[test]
fn reexec_target_resolves_the_replacement_after_an_install() {
    // Post-`cargo install`: the inode behind our `current_exe()` was
    // unlinked, so it reads "<path> (deleted)" while the freshly-installed
    // binary now sits at the un-annotated path — that is what we re-exec.
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("rimz-sidebar");
    std::fs::write(&real, b"x").unwrap();
    let deleted = PathBuf::from(format!("{} (deleted)", real.display()));
    assert!(!deleted.is_file(), "the annotated path must not exist");
    assert_eq!(resolve_reexec_target(deleted), Some(real.clone()));
    // The ordinary, not-replaced case uses the live path as-is.
    assert_eq!(resolve_reexec_target(real.clone()), Some(real));
}

#[test]
fn reexec_target_is_none_when_nothing_exists_on_disk() {
    // A partial or in-flight install: neither the annotated nor the
    // stripped path is a file, so the loop keeps serving the current build
    // rather than re-execing into nothing and vanishing.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("rimz-sidebar");
    let deleted = PathBuf::from(format!("{} (deleted)", missing.display()));
    assert_eq!(resolve_reexec_target(deleted), None);
    assert_eq!(resolve_reexec_target(missing), None);
}

#[test]
fn decide_reload_reexecs_only_when_the_on_disk_binary_differs() {
    let target = PathBuf::from("/some/rimz-sidebar");
    // Byte-identical to what we run: skip the re-exec churn.
    assert!(matches!(
        decide_reload(Some(target.clone()), Some(true)),
        ReloadAction::AlreadyCurrent
    ));
    // Content differs: re-exec onto the freshly-installed build.
    assert!(matches!(
        decide_reload(Some(target.clone()), Some(false)),
        ReloadAction::Reexec(t) if t == target
    ));
    // Running image unreadable (non-Linux / IO race): re-exec, preserving
    // the always-load-the-on-disk-build behavior.
    assert!(matches!(
        decide_reload(Some(target.clone()), None),
        ReloadAction::Reexec(t) if t == target
    ));
    // No binary on disk: keep the current build regardless of the compare.
    assert!(matches!(decide_reload(None, None), ReloadAction::Missing));
    assert!(matches!(
        decide_reload(None, Some(true)),
        ReloadAction::Missing
    ));
}

#[test]
fn same_file_contents_detects_byte_equality() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original");
    let identical = dir.path().join("identical");
    let same_len_differs = dir.path().join("same_len_differs");
    let shorter = dir.path().join("shorter");
    std::fs::write(&original, b"freshly-installed build").unwrap();
    std::fs::write(&identical, b"freshly-installed build").unwrap();
    std::fs::write(&same_len_differs, b"freshly-installed BUILD").unwrap();
    std::fs::write(&shorter, b"shorter").unwrap();
    assert!(same_file_contents(&original, &identical).unwrap());
    assert!(!same_file_contents(&original, &same_len_differs).unwrap());
    assert!(!same_file_contents(&original, &shorter).unwrap());
}

#[test]
fn snapshot_bin_uses_the_cached_path_while_it_exists() {
    // The sibling `rimz` captured at launch is still on disk — drive the
    // snapshot with exactly that build, so a dev worktree's changes apply.
    let dir = tempfile::tempdir().unwrap();
    let cached = dir.path().join("rimz");
    std::fs::write(&cached, b"x").unwrap();
    assert_eq!(resolve_snapshot_bin(&cached), cached);
}

#[test]
fn snapshot_bin_falls_back_to_path_when_the_cached_binary_vanished() {
    // The dev worktree this sidebar launched from was removed, deleting the
    // sibling `rimz` it cached. Recover via the installed binary on `PATH`
    // rather than forking a path that no longer exists every tick.
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("rimz");
    assert!(!gone.is_file(), "the cached path must not exist");
    assert_eq!(resolve_snapshot_bin(&gone), PathBuf::from("rimz"));
}

#[test]
fn tick_for_honours_above_two_seconds() {
    assert_eq!(tick_for(5), Duration::from_secs(5));
}

#[test]
fn tick_for_clamps_zero_to_one() {
    assert_eq!(tick_for(0), Duration::from_secs(1));
}

#[test]
fn producer_skips_the_fork_while_its_frame_is_within_one_tick() {
    // The two-speed contract: a ledger-delta storm paints per delta off the
    // in-process fast lane, forking at most once per data tick.
    assert!(!produce_this_cycle(true, false, false, Some(100), 1000));
    assert!(produce_this_cycle(true, false, false, Some(1000), 1000));
    assert!(
        produce_this_cycle(true, false, false, None, 1000),
        "no usable frame (cold start) always produces"
    );
}

#[test]
fn forced_requests_always_fork() {
    assert!(produce_this_cycle(true, true, false, Some(0), 1000));
    assert!(
        produce_this_cycle(false, true, false, Some(0), 1000),
        "a consumer reload/resize forks regardless of election"
    );
}

#[test]
fn consumer_forks_only_to_self_heal_a_stale_producer() {
    assert!(!produce_this_cycle(false, false, false, Some(5_000), 1000));
    assert!(!produce_this_cycle(false, false, false, None, 1000));
    assert!(produce_this_cycle(false, false, true, None, 1000));
}

#[test]
fn frame_grid_advances_one_frame_when_on_time() {
    let base = Instant::now();
    // Painted at the scheduled boundary: the next boundary is exactly one
    // frame later, holding the fixed cadence.
    assert_eq!(
        next_frame_after(base, base, ANIMATION_FRAME),
        base + ANIMATION_FRAME
    );
}

#[test]
fn frame_grid_snaps_forward_when_behind() {
    let base = Instant::now();
    // Scheduled several frames in the past relative to `now`: rather than
    // replaying every missed boundary, the grid snaps to one frame ahead of
    // `now`, so a slow paint never spirals into a burst of catch-up frames.
    let now = base + ANIMATION_FRAME * 5;
    assert_eq!(
        next_frame_after(base, now, ANIMATION_FRAME),
        now + ANIMATION_FRAME
    );
}

#[test]
fn frame_interval_slows_cosmetic_animation_only() {
    let ws = workspace();
    let mut slow = snapshot(&ws);
    slow.worktree_groups = vec![rimz::SidebarWorktreeGroup {
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        kind: rimz::SidebarWorktreeKind::Worktree,
        status_counts: Vec::new(),
        rows: vec![rimz::SidebarRow {
            row_kind: rimz::SidebarRowKind::Agent,
            id: "claude-1".to_owned(),
            name: "claude".to_owned(),
            status: Some(rimz::feed::AgentStatus::Waiting),
            phase: rimz::agents::TurnPhase::Idle,
            pane: None,
            request_id: None,
            surface: None,
            task: Some("allow cargo fmt".to_owned()),
            prompt: None,
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            worktree_path: Some("/repo/main".to_owned()),
            worktree_branch: Some("main".to_owned()),
            last_activity: Timestamp::now(),
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
        }],
        hidden_count: 0,
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
    }];

    assert_eq!(
        frame_interval(&slow, &UiState::default()),
        SLOW_ANIMATION_FRAME
    );

    slow.worktree_groups[0].rows[0].status = Some(rimz::feed::AgentStatus::Running);
    assert_eq!(frame_interval(&slow, &UiState::default()), ANIMATION_FRAME);
}

#[test]
fn heartbeat_write_due_on_first_or_aged_write_only() {
    assert!(heartbeat_write_due(None));
    assert!(!heartbeat_write_due(Some(Instant::now())));
    assert!(heartbeat_write_due(Some(
        Instant::now() - HEARTBEAT_WRITE_INTERVAL
    )));
}

#[test]
fn fetch_request_sends_immediately_when_idle() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut in_flight = false;
    let mut pending = None;
    let request = FetchRequest::fresh_panes();

    request_fetch(&tx, &mut in_flight, &mut pending, request, true);

    assert!(in_flight);
    assert!(rx.try_recv().unwrap().force_produce);
    assert!(pending.is_none());
}

#[test]
fn fetch_request_preserves_forced_pane_refresh_while_in_flight() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut in_flight = true;
    let mut pending = Some(FetchRequest::default());
    let request = FetchRequest::fresh_panes();
    let min_pane_cache_ms = request.min_pane_cache_ms;

    request_fetch(&tx, &mut in_flight, &mut pending, request, true);

    let pending = pending.expect("pending refetch");
    assert!(pending.force_produce);
    assert_eq!(pending.min_pane_cache_ms, min_pane_cache_ms);
}

#[test]
fn self_close_probe_request_sends_when_idle() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut in_flight = false;
    let mut pending = None;

    request_self_close_probe(&tx, &mut in_flight, &mut pending, Duration::ZERO);

    assert!(in_flight);
    assert_eq!(
        rx.try_recv().unwrap(),
        SelfCloseProbeRequest {
            delay: Duration::ZERO
        }
    );
    assert_eq!(pending, None);
}

#[test]
fn self_close_probe_request_coalesces_to_shortest_pending_delay() {
    let (tx, _rx) = std::sync::mpsc::channel();
    let mut in_flight = true;
    let mut pending = Some(Duration::from_secs(2));

    request_self_close_probe(&tx, &mut in_flight, &mut pending, Duration::from_millis(50));

    assert!(in_flight);
    assert_eq!(pending, Some(Duration::from_millis(50)));
}

#[test]
fn self_close_probe_outcome_uses_the_existing_latch() {
    let config = ServeConfig {
        workspace_id: workspace(),
        mux: MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        instance_id: SidebarInstanceId::new(),
        tick_seconds: 2,
        rimz_bin: PathBuf::from("rimz"),
    };
    let mut state = SelfCloseState::default();

    assert!(!apply_self_close_probe_outcome(
        &config,
        SelfCloseProbeOutcome {
            sibling_count: Some(1),
            error: None,
        },
        &mut state,
    ));
    assert!(state.seen_sibling);
    assert!(apply_self_close_probe_outcome(
        &config,
        SelfCloseProbeOutcome {
            sibling_count: Some(0),
            error: None,
        },
        &mut state,
    ));
}

#[test]
fn self_close_waits_for_a_sibling_before_ever_closing() {
    let mut state = SelfCloseState::default();
    // Startup: no sibling yet (terminal pane not materialized). Give Zellij
    // one observation to finish materializing the sibling.
    assert!(!self_close_decision(&mut state, Some(0)));
    assert!(!state.seen_sibling);
}

#[test]
fn self_close_fires_when_a_sibling_never_appears() {
    let mut state = SelfCloseState::default();
    assert!(!self_close_decision(&mut state, Some(0)));
    assert!(self_close_decision(&mut state, Some(0)));
}

#[test]
fn self_close_latches_then_fires_when_alone() {
    let mut state = SelfCloseState::default();
    assert!(!self_close_decision(&mut state, Some(1)));
    assert!(state.seen_sibling, "seeing a sibling must latch");
    // Sibling went away: now alone, so close.
    assert!(self_close_decision(&mut state, Some(0)));
}

#[test]
fn self_close_holds_while_siblings_remain() {
    let mut state = SelfCloseState {
        seen_sibling: true,
        empty_startup_observations: 0,
    };
    assert!(!self_close_decision(&mut state, Some(2)));
}

#[test]
fn self_close_never_fires_on_unknown_count() {
    let mut state = SelfCloseState {
        seen_sibling: true,
        empty_startup_observations: 0,
    };
    assert!(!self_close_decision(&mut state, None));
    assert!(
        state.seen_sibling,
        "an unknown count must not clear the latch"
    );
}

#[test]
fn resize_grew_treats_strictly_larger_width_as_grow() {
    // A grow is the flash precondition (the mux handed us a sibling's space),
    // so it takes the held path; a shrink or same width keeps the instant
    // repaint, and the first resize (no prior width) is held cautiously.
    assert!(resize_grew(Some(30), 120), "wider pane is a grow");
    assert!(!resize_grew(Some(120), 30), "narrower pane is not a grow");
    assert!(!resize_grew(Some(80), 80), "same width is not a grow");
    assert!(
        resize_grew(None, 1),
        "an unknown previous width counts as a grow"
    );
}

#[test]
fn session_exit_holds_at_birth_before_a_working_view() {
    // The `rimzd` tab is born first, so "only the daemon view" is birth, not
    // teardown: hold, and do not latch.
    let mut state = SessionExitState::default();
    assert!(!state.should_detach(Some(true)));
    assert!(!state.seen_other_view);
}

#[test]
fn session_exit_latches_then_detaches_when_the_room_empties() {
    let mut state = SessionExitState::default();
    assert!(!state.should_detach(Some(false))); // a working view appears → latch
    assert!(state.seen_other_view);
    assert!(state.should_detach(Some(true))); // it closed → only the daemon view → detach
}

#[test]
fn session_exit_holds_while_a_working_view_exists() {
    let mut state = SessionExitState {
        seen_other_view: true,
        fired: false,
    };
    assert!(!state.should_detach(Some(false)));
}

#[test]
fn session_exit_never_fires_on_none() {
    let mut state = SessionExitState {
        seen_other_view: true,
        fired: false,
    };
    assert!(!state.should_detach(None));
    assert!(
        state.seen_other_view,
        "an unknown signal must not clear the latch"
    );
}

#[test]
fn session_exit_fires_exactly_once() {
    let mut state = SessionExitState {
        seen_other_view: true,
        fired: false,
    };
    assert!(state.should_detach(Some(true)));
    assert!(
        !state.should_detach(Some(true)),
        "a second tick must not re-detach"
    );
}

/// A browse pick of `pane`, begun while the derived baseline was `baseline`.
fn browse(pane: &PaneId, baseline: Option<&PaneId>) -> Browse {
    Browse {
        pane: pane.clone(),
        baseline_at_start: baseline.cloned(),
    }
}

#[test]
fn cold_start_derives_from_first_active_pane() {
    // No baseline and no local layer: the first frame's active-pane
    // derivation seeds both the baseline and the highlight.
    let ws = workspace();
    let active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", true),
        ],
    );
    let mut ui = UiState::default();

    reconcile_selection(&mut ui, &snapshot, Some(active.clone()));

    assert_eq!(ui.selected_index, 1);
    assert_eq!(ui.selected_pane, Some(active.clone()));
    assert_eq!(ui.baseline_pane, Some(active));
}

#[test]
fn cold_start_with_no_derivation_holds_none() {
    // No baseline, no local layer, a None derivation: nothing to follow, so
    // the highlight stays unseated (index clamped to row 0) until a frame
    // derives an active row — never a fleet-row guess that may sit in
    // another tab.
    let ws = workspace();
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState::default();

    reconcile_selection(&mut ui, &snapshot, None);

    assert_eq!(ui.selected_pane, None);
    assert_eq!(ui.selected_index, 0);
}

#[test]
fn baseline_change_moves_the_highlight() {
    // No local layer: the highlight follows the derived baseline, so a
    // genuine external move (the user focused terminal_3) lands on the very
    // next fold.
    let ws = workspace();
    let was = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let now_active = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
            pane("terminal_3", "tab_0", true),
        ],
    );
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(was.clone()),
        baseline_pane: Some(was),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, Some(now_active.clone()));

    assert_eq!(ui.selected_index, 2);
    assert_eq!(ui.selected_pane, Some(now_active.clone()));
    assert_eq!(ui.baseline_pane, Some(now_active));
}

#[test]
fn none_derivation_holds_last_baseline() {
    // The sidebar itself is the view's active pane (the user focused it to
    // type), or the active pane is not a row: the derivation is None, the
    // baseline holds, and the highlight stays put.
    let ws = workspace();
    let held = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(held.clone()),
        baseline_pane: Some(held.clone()),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, None);

    assert_eq!(ui.selected_pane, Some(held.clone()));
    assert_eq!(ui.baseline_pane, Some(held));
}

#[test]
fn highlight_moves_only_when_the_baseline_catches_up() {
    // The "accepts latency" contract behind the one-packet jump: a jump
    // action fires the focus command and mutates nothing, so a fold still
    // deriving the old pane keeps the old highlight, and the jumped pane
    // lights up only once the mux reports it focused.
    let ws = workspace();
    let from = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let jumped = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(from.clone()),
        baseline_pane: Some(from.clone()),
        line_map: line_map_for(&snapshot, 0),
        ..Default::default()
    };

    // Click terminal_2's row: the outcome carries the target, the UI holds.
    let row1 = ui.line_map.iter().position(|m| *m == Some(1)).unwrap();
    let outcome = handle_mouse_click(1, screen_row_for(row1), &mut ui, &snapshot);
    assert_eq!(outcome.focus, Some(jumped.clone()));
    assert_eq!(ui.selected_pane, Some(from.clone()));

    // A fold still deriving the pre-jump pane keeps the old highlight.
    reconcile_selection(&mut ui, &snapshot, Some(from.clone()));
    assert_eq!(ui.selected_pane, Some(from));

    // The fold that derives the jumped pane moves it.
    reconcile_selection(&mut ui, &snapshot, Some(jumped.clone()));
    assert_eq!(ui.selected_pane, Some(jumped.clone()));
    assert_eq!(ui.baseline_pane, Some(jumped));
}

#[test]
fn browse_roams_other_tabs_rows() {
    // The browse pick may walk every visible row — another tab's included
    // (the cross-tab peek that expands a remote card) — while the derived
    // baseline stays untouched underneath.
    let ws = workspace();
    let here = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let remote = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_9", "tab_7", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 0,
        selected_pane: Some(here.clone()),
        baseline_pane: Some(here.clone()),
        ..Default::default()
    };

    select_row(&mut ui, &snapshot, 1);
    begin_or_continue_browse(&mut ui);
    // While browsing the user has the sidebar focused, so frames derive None.
    reconcile_selection(&mut ui, &snapshot, None);

    assert_eq!(ui.selected_pane, Some(remote), "the pick roams cross-tab");
    assert_eq!(ui.baseline_pane, Some(here), "the baseline never moves");
}

#[test]
fn browse_holds_across_inert_frames() {
    // Browsing with the baseline unchanged: None derivations hold the
    // baseline, the anchor still matches, the pick holds.
    let ws = workspace();
    let picked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let baseline = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(picked.clone()),
        baseline_pane: Some(baseline.clone()),
        browse: Some(browse(&picked, Some(&baseline))),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, None);
    reconcile_selection(&mut ui, &snapshot, None);

    assert_eq!(ui.selected_pane, Some(picked));
    assert!(ui.browse.is_some(), "still browsing");
}

#[test]
fn browse_clears_on_baseline_change() {
    // A genuine baseline change — the user focused another working pane —
    // ends the browse, and the highlight follows the new baseline.
    let ws = workspace();
    let picked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let moved = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
            pane("terminal_3", "tab_0", true),
        ],
    );
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(picked.clone()),
        baseline_pane: Some(anchor.clone()),
        browse: Some(browse(&picked, Some(&anchor))),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, Some(moved.clone()));

    assert_eq!(ui.browse, None, "a real move ends the browse");
    assert_eq!(ui.selected_pane, Some(moved));
}

#[test]
fn browse_survives_a_jump_and_ends_on_baseline_change() {
    // A jump mutates nothing, the browse included: an Enter mid-browse
    // leaves the pick in place, so the highlight holds still until the
    // derived baseline catches up underneath it — no flicker back to the
    // old pane. The browse then ends on the genuine baseline change.
    let ws = workspace();
    let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let picked = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        baseline_pane: Some(anchor.clone()),
        ..Default::default()
    };

    select_row(&mut ui, &snapshot, 1);
    begin_or_continue_browse(&mut ui);
    let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);
    assert_eq!(outcome.focus, Some(picked.clone()));
    assert!(ui.browse.is_some(), "the jump leaves the browse in place");

    // An inert fold (baseline unchanged) keeps the pick pinned.
    reconcile_selection(&mut ui, &snapshot, Some(anchor));
    assert!(ui.browse.is_some());
    assert_eq!(ui.selected_pane, Some(picked.clone()));

    // The fold that derives the jumped pane ends the browse seamlessly —
    // the baseline takes over on the same pane.
    reconcile_selection(&mut ui, &snapshot, Some(picked.clone()));
    assert_eq!(ui.browse, None, "a real baseline change ends the browse");
    assert_eq!(ui.selected_pane, Some(picked));
}

#[test]
fn continued_browse_keeps_the_first_anchor() {
    // The second arrow continues the browse: the pick moves, but the anchor
    // (baseline_at_start) stays the one captured when browsing began, so a
    // baseline change mid-browse still ends it.
    let ws = workspace();
    let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
            pane("terminal_3", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        baseline_pane: Some(anchor.clone()),
        ..Default::default()
    };

    select_row(&mut ui, &snapshot, 1);
    begin_or_continue_browse(&mut ui);
    // The baseline advances mid-browse (rule 1 of an intervening fold)...
    ui.baseline_pane = Some(PaneId::from_parts(MuxName::Zellij, "terminal_3"));
    select_row(&mut ui, &snapshot, 2);
    begin_or_continue_browse(&mut ui);

    assert_eq!(
        ui.browse.as_ref().map(|b| b.baseline_at_start.clone()),
        Some(Some(anchor)),
        "the anchor is the browse-start baseline, not the latest one"
    );
}

#[test]
fn selection_reanchors_to_its_pane_after_a_reorder() {
    // terminal_2 moved from row 1 to row 0 between folds with no baseline
    // change; the highlight follows its pane, not the old index.
    let ws = workspace();
    let active = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_2", "tab_0", true),
            pane("terminal_1", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(active.clone()),
        baseline_pane: Some(active.clone()),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, Some(active.clone()));

    assert_eq!(ui.selected_index, 0, "re-anchored to the pane's new row");
    assert_eq!(ui.selected_pane, Some(active));
}

#[test]
fn selection_drops_when_its_pane_leaves_the_room() {
    // The baseline's pane is gone from the snapshot: drop the dangling
    // identity and clamp, so the next derivation can re-seat it.
    let ws = workspace();
    let gone = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(gone.clone()),
        baseline_pane: Some(gone),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, None);

    assert_eq!(ui.selected_pane, None, "dangling identity dropped");
    assert_eq!(ui.baseline_pane, None, "absent baseline cleared");
    assert!(ui.selected_index < 2, "clamped to a valid row");
}

#[test]
fn browse_drops_when_its_pane_leaves_the_room() {
    // A browse picks terminal_9, which then closes. The pick must not keep
    // shadowing the baseline — it is dropped, so the highlight reconverges
    // on the next fold.
    let ws = workspace();
    let gone = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let real = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(gone.clone()),
        baseline_pane: Some(real.clone()),
        browse: Some(browse(&gone, Some(&real))),
        ..Default::default()
    };

    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.browse, None, "the dead pick is dropped");

    // The next fold reconverges on the live baseline.
    reconcile_selection(&mut ui, &snapshot, None);
    assert_eq!(ui.selected_pane, Some(real));
}

/// Lay out `snapshot` at a generous size through the real render path,
/// returning the freshly-composed hit-test map — the same map the live draw
/// stores on `UiState`. Width/height are wide and tall enough that nothing
/// the tests probe is clipped.
fn line_map_for(snapshot: &SidebarSnapshot, selected: usize) -> Vec<Option<usize>> {
    let ui = UiState {
        selected_index: selected,
        help_visible: false,
        animation_phase: 0,
        line_map: Vec::new(),
        ..Default::default()
    };
    let (_lines, map) = render::compose_lines(snapshot, None, &ui, 54, 64);
    map
}

/// The screen row a content-line index maps to: borderless, the body fills
/// the frame from row 0, so map index `i` is screen row `i`.
fn screen_row_for(map_index: usize) -> u16 {
    u16::try_from(map_index).unwrap()
}

#[test]
fn row_index_maps_process_row_screen_positions() {
    let ws = workspace();
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let ui = UiState {
        line_map: line_map_for(&snapshot, 0),
        ..UiState::default()
    };

    // The worktree header is the first line that routes to row 0 — clicking
    // the pod name jumps into its first row — and the first process row
    // follows directly beneath it. Both route to row 0.
    let header = ui.line_map.iter().position(|m| *m == Some(0)).unwrap();
    let row0 = header + 1;
    let row1 = ui.line_map.iter().position(|m| *m == Some(1)).unwrap();
    assert_eq!(
        ui.line_map[row0],
        Some(0),
        "the first process row follows its worktree header"
    );

    // The borderless title line at screen row 0 is inert chrome.
    assert_eq!(
        row_index_at_screen_position(&ui, 0),
        None,
        "the title line is not clickable content"
    );
    assert_eq!(
        row_index_at_screen_position(&ui, screen_row_for(header)),
        Some(0),
        "the worktree header jumps into its first row"
    );
    assert_eq!(
        row_index_at_screen_position(&ui, screen_row_for(row0)),
        Some(0)
    );
    assert_eq!(
        row_index_at_screen_position(&ui, screen_row_for(row1)),
        Some(1)
    );
    // The line just above the worktree header is the section gap — inert.
    assert_eq!(
        row_index_at_screen_position(&ui, screen_row_for(header - 1)),
        None,
        "the section gap is not a row"
    );
}

#[test]
fn every_line_of_an_agent_block_routes_to_that_agent() {
    // The user-visible contract: the whole multi-line agent card is one
    // click target, the worktree header that jumps into it routes there too,
    // the gaps and `+K more` are inert, and a process row's single line
    // routes to its own index.
    let ws = workspace();
    let snapshot = clickable_block_snapshot(&ws);
    // Select the agent so its deeper stats lines appear too.
    let map = line_map_for(&snapshot, 0);

    // Index 0 is the agent (a multi-line card) plus the worktree header that
    // jumps into it; index 1 is the process row.
    let agent_lines = map.iter().filter(|m| **m == Some(0)).count();
    assert!(
        agent_lines >= 4,
        "the worktree header plus the selected agent card (identity + \
             description + gauge + stats) route to row 0, not {agent_lines} lines",
    );
    let process_lines = map.iter().filter(|m| **m == Some(1)).count();
    assert_eq!(process_lines, 1, "a process row is a single line");

    // No content line of the agent block is missed: every map slot routes
    // through the hit-test to exactly the row it was tagged with.
    let ui = UiState {
        line_map: map.clone(),
        ..UiState::default()
    };
    for (i, entry) in map.iter().enumerate() {
        let got = row_index_at_screen_position(&ui, screen_row_for(i));
        assert_eq!(got, *entry, "screen row {i} mismatched its map slot");
    }

    // The cockpit header, gaps, and the `+K more` hidden-count line are inert.
    assert!(
        map.contains(&None),
        "cockpit header / gaps / +K more stay inert"
    );
}

#[test]
fn mouse_click_fires_focus_without_moving_selection() {
    let ws = workspace();
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 0,
        help_visible: false,
        animation_phase: 0,
        line_map: line_map_for(&snapshot, 0),
        ..Default::default()
    };
    let row1 = ui.line_map.iter().position(|m| *m == Some(1)).unwrap();

    let outcome = handle_mouse_click(1, screen_row_for(row1), &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::focus(target));
    assert!(!outcome.redraw, "a jump changes nothing to repaint");
    assert_eq!(ui.selected_index, 0, "the click moves no selection");
    assert_eq!(ui.selected_pane, None);
    assert_eq!(ui.browse, None);
}

#[test]
fn digit_fires_focus_at_the_ordinal_row_without_selecting() {
    let ws = workspace();
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState::default();

    let outcome = handle_key(KeyAction::Digit(2), &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::focus(target));
    assert_eq!(ui.selected_index, 0, "the digit moves no selection");
    assert_eq!(ui.selected_pane, None);

    // An out-of-range ordinal resolves no pane and does nothing.
    let outcome = handle_key(KeyAction::Digit(9), &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default());
}

#[test]
fn space_fires_focus_at_the_next_attention_row_without_selecting() {
    let ws = workspace();
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let mut snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", true),
            pane("terminal_2", "tab_0", false),
        ],
    );
    snapshot.worktree_groups[0].rows[1].status = Some(rimz::feed::AgentStatus::Waiting);
    let mut ui = UiState::default();

    let outcome = handle_key(KeyAction::Space, &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::focus(target));
    assert_eq!(ui.selected_index, 0, "the triage key moves no selection");
    assert_eq!(ui.selected_pane, None);
}

#[test]
fn arrow_key_reports_immediate_ui_change() {
    let ws = workspace();
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 0,
        help_visible: false,
        animation_phase: 0,
        line_map: Vec::new(),
        ..Default::default()
    };

    let outcome = handle_key(KeyAction::Down, &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::redraw());
    assert_eq!(ui.selected_index, 1);
    assert!(ui.browse.is_some(), "an arrow begins a browse pick");
}

#[test]
fn dismiss_key_requests_alert_dismissal() {
    let ws = workspace();
    let snapshot = snapshot_with_panes(&ws, vec![pane("terminal_1", "tab_0", false)]);
    let mut ui = UiState::default();

    let outcome = handle_key(KeyAction::Dismiss, &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::dismiss());
    assert!(outcome.dismiss);
    assert!(outcome.redraw);
    // Dismiss never moves the selection.
    assert_eq!(ui.selected_index, 0);
}

#[test]
fn enter_fires_focus_at_the_selected_pane_without_mutating_ui() {
    let ws = workspace();
    let selected = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_panes(
        &ws,
        vec![
            pane("terminal_1", "tab_0", false),
            pane("terminal_2", "tab_0", false),
        ],
    );
    let mut ui = UiState {
        selected_index: 1,
        selected_pane: Some(selected.clone()),
        help_visible: false,
        animation_phase: 0,
        line_map: Vec::new(),
        ..Default::default()
    };

    let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);

    assert_eq!(outcome, InputOutcome::focus(selected.clone()));
    assert_eq!(ui.selected_index, 1);
    assert_eq!(
        ui.selected_pane,
        Some(selected),
        "Enter reads, never writes"
    );

    // With nothing selected there is no target and nothing happens.
    ui.selected_pane = None;
    let outcome = handle_key(KeyAction::Enter, &mut ui, &snapshot);
    assert_eq!(outcome, InputOutcome::default());
}

// ---- last-known-good commit gate -------------------------------------

fn gate_now() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

/// A snapshot whose single pane renders as a bare process row.
fn process_on(ws: &WorkspaceId, raw: &str) -> SidebarSnapshot {
    snapshot_with_panes(ws, vec![pane(raw, "tab_0", false)])
}

#[test]
fn gate_accepts_first_frame_against_placeholder() {
    let ws = workspace();
    // The placeholder prev has no panes; the first real frame is never a
    // regression to hold.
    assert_eq!(
        gate_commit(
            &snapshot(&ws),
            &agent_snapshot(&ws),
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::Accept
    );
}

#[test]
fn gate_holds_transient_agent_to_process_demotion() {
    let ws = workspace();
    // Same pane set {terminal_9}, but the agent row became a bare process —
    // the phantom flicker. Held until the escape hatch opens.
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on(&ws, "terminal_9"),
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::KeepPrior
    );
}

#[test]
fn gate_releases_demotion_after_reject_count() {
    let ws = workspace();
    let gate = GateState {
        reject_streak: ACCEPT_REGRESSION_AFTER_REJECTS,
        rejecting_since: Some(gate_now()),
    };
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on(&ws, "terminal_9"),
            &gate,
            gate_now()
        ),
        CommitDecision::Accept,
        "a stuck demotion must surface, not freeze forever"
    );
}

#[test]
fn gate_releases_demotion_after_timeout_but_holds_while_brief() {
    let ws = workspace();
    let base = 1_700_000_000;
    let gate = GateState {
        reject_streak: 1,
        rejecting_since: Some(Timestamp::from_second(base).unwrap()),
    };
    let ceiling = ACCEPT_REGRESSION_AFTER.as_secs() as i64;
    // Still brief: held.
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on(&ws, "terminal_9"),
            &gate,
            Timestamp::from_second(base + ceiling - 1).unwrap()
        ),
        CommitDecision::KeepPrior
    );
    // Past the ceiling: released.
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on(&ws, "terminal_9"),
            &gate,
            Timestamp::from_second(base + ceiling).unwrap()
        ),
        CommitDecision::Accept
    );
}

#[test]
fn gate_accepts_when_the_panel_set_changes() {
    let ws = workspace();
    // A pane closed (the demotion is on a different id): the room genuinely
    // changed, so accept rather than hold against a stale baseline.
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &process_on(&ws, "terminal_8"),
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::Accept
    );
}

#[test]
fn gate_accepts_a_non_regression() {
    let ws = workspace();
    assert_eq!(
        gate_commit(
            &agent_snapshot(&ws),
            &agent_snapshot(&ws),
            &GateState::default(),
            gate_now()
        ),
        CommitDecision::Accept
    );
}

#[test]
fn reject_holds_prior_frame_as_render_and_baseline() {
    let ws = workspace();
    let prior = agent_snapshot(&ws);
    // A fresh fetch that demoted the agent on terminal_9 to a process row.
    let computed = compute_next_state(
        &ws,
        None,
        Ok(process_on(&ws, "terminal_9")),
        Some(prior.clone()),
        &Health::default(),
    );
    let (state, gate, rejected) =
        apply_gate(computed, true, &prior, &GateState::default(), gate_now());
    assert!(rejected);
    // Both the rendered frame AND the next-tick baseline stay the good
    // frame, so the cache never advances onto the demotion.
    assert!(matches!(
        state.snapshot.worktree_groups[0].rows[0].row_kind,
        rimz::SidebarRowKind::Agent
    ));
    let baseline = state.last_snapshot.expect("baseline retained");
    assert!(matches!(
        baseline.worktree_groups[0].rows[0].row_kind,
        rimz::SidebarRowKind::Agent
    ));
    assert_eq!(gate.reject_streak, 1);
    assert!(gate.rejecting_since.is_some());
    // Orthogonal to Health: a held regression is a *successful* fetch, so it
    // never arms the degraded alert nor counts toward self-close.
    assert!(state.health.alert.is_none());
    assert_eq!(state.health.failure_streak, 0);
}

#[test]
fn accept_resets_the_gate() {
    let ws = workspace();
    let prior = agent_snapshot(&ws);
    let computed = compute_next_state(
        &ws,
        None,
        Ok(agent_snapshot(&ws)),
        Some(prior.clone()),
        &Health::default(),
    );
    // Carry a prior reject episode in; a clean accept clears it.
    let prev_gate = GateState {
        reject_streak: 2,
        rejecting_since: Some(gate_now()),
    };
    let (state, gate, rejected) = apply_gate(computed, true, &prior, &prev_gate, gate_now());
    assert!(!rejected);
    assert_eq!(gate, GateState::default());
    assert!(matches!(
        state.snapshot.worktree_groups[0].rows[0].row_kind,
        rimz::SidebarRowKind::Agent
    ));
}
