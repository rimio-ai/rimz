use super::*;
use crate::sidebar_pane::app::fixtures::{agent_snapshot, pane, snapshot_with_panes, workspace};
use crate::sidebar_pane::app::input::KeyAction;

fn read_marks(ws: &WorkspaceId) -> (tempfile::TempDir, ReadMarkStore) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    let store = ReadMarkStore::new(runtime, SidebarInstanceId::new());
    (dir, store)
}

fn serve_config(ws: &WorkspaceId) -> ServeConfig {
    ServeConfig {
        workspace_id: ws.clone(),
        mux: crate::MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        instance_id: SidebarInstanceId::new(),
        tick_seconds: 1,
        refresh_ms_override: None,
        timezone: jiff::tz::TimeZone::UTC,
        notification_prefs: crate::config::NotificationsPrefs::default(),
        own_pane: None,
    }
}

fn loop_state(ws: &WorkspaceId) -> (tempfile::TempDir, LoopState) {
    loop_state_with_own_pane(ws, None)
}

fn loop_state_with_own_pane(
    ws: &WorkspaceId,
    own_pane: Option<PaneId>,
) -> (tempfile::TempDir, LoopState) {
    let (tx, _rx) = std::sync::mpsc::sync_channel(64);
    let (dir, store) = read_marks(ws);
    (
        dir,
        LoopState::new(
            ws.clone(),
            own_pane,
            None,
            tx,
            store,
            PetRenderCaps::default(),
            true,
        ),
    )
}

fn process_snapshot(ws: &WorkspaceId, observed_at_ms: u64) -> SidebarSnapshot {
    let mut snapshot = snapshot_with_panes(ws, vec![pane("terminal_9", "tab_0", false)]);
    snapshot.panes_observed_at_ms = Some(observed_at_ms);
    snapshot
}

fn agent_snapshot_observed(ws: &WorkspaceId, observed_at_ms: u64) -> SidebarSnapshot {
    let mut snapshot = agent_snapshot(ws);
    snapshot.panes_observed_at_ms = Some(observed_at_ms);
    snapshot
}

fn animating_agent_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = agent_snapshot(ws);
    let row = &mut snapshot.worktree_groups[0].rows[0];
    let crate::RowCard::Agent(card) = &mut row.card else {
        panic!("fixture row is an agent");
    };
    card.status = crate::agents::AgentStatus::Running;
    card.phase = crate::agents::TurnPhase::Acting;
    snapshot
}

fn own_view(own_is_active: bool, active_pane_is_viewed: bool) -> crate::SidebarOwnView {
    crate::SidebarOwnView {
        sibling_count: 1,
        own_is_active,
        active_pane_id: Some(pane("terminal_9", "tab_0", false).pane_id),
        active_pane_is_viewed,
        working_pane_ids: vec![pane("terminal_9", "tab_0", false).pane_id],
        focus_contested: false,
        own_view_is_daemon: false,
    }
}

fn snapshot_with_active_pane(ws: &WorkspaceId, active: PaneId) -> SidebarSnapshot {
    let first = pane("terminal_1", "tab_0", false);
    let second = pane("terminal_2", "tab_0", false);
    let working_pane_ids = vec![first.pane_id.clone(), second.pane_id.clone()];
    let mut snapshot = snapshot_with_panes(ws, vec![first, second]);
    snapshot.own_view = Some(crate::SidebarOwnView {
        sibling_count: 2,
        own_is_active: false,
        active_pane_id: Some(active),
        active_pane_is_viewed: true,
        working_pane_ids,
        focus_contested: false,
        own_view_is_daemon: false,
    });
    snapshot
}

fn fold_snapshot(
    state: &mut LoopState,
    config: &ServeConfig,
    fetch: &mut FetchDispatcher,
    snapshot: SidebarSnapshot,
    fresh_pane_frame: bool,
) {
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    result_tx
        .send(FetchOutcome {
            snapshot: Ok(snapshot),
            final_for_request: true,
            fresh_pane_frame,
            unchanged: false,
        })
        .expect("send fetch outcome");
    state
        .on_snapshot(
            config,
            fetch,
            &result_rx,
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("fold snapshot");
}

fn fetch_dispatcher() -> (FetchDispatcher, std::sync::mpsc::Receiver<FetchRequest>) {
    let (request_tx, request_rx) = std::sync::mpsc::channel();
    (FetchDispatcher::new(request_tx), request_rx)
}

fn frame_active(state: &LoopState) -> bool {
    state
        .frame_timing(Duration::from_secs(10), Instant::now())
        .0
}

fn fixed_terminal() -> Terminal<CrosstermBackend<io::Stdout>> {
    let viewport = ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 24));
    Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        ratatui::TerminalOptions { viewport },
    )
    .expect("terminal")
}

fn hidden_attached_agent_snapshot(
    ws: &WorkspaceId,
    status: crate::agents::AgentStatus,
) -> SidebarSnapshot {
    let mut snapshot = agent_snapshot(ws);
    set_agent_status(&mut snapshot, status);
    snapshot.own_view = Some(own_view(false, false));
    snapshot.viewed_panes.clear();
    snapshot.presence = Some(crate::SidebarPresence::Active);
    snapshot
}

fn hidden_attached_process_snapshot(
    ws: &WorkspaceId,
    state: crate::ProcessState,
) -> SidebarSnapshot {
    let mut snapshot = process_snapshot(ws, 1);
    let card = snapshot.worktree_groups[0].rows[0]
        .as_process_mut()
        .expect("process row");
    card.state = state;
    snapshot.own_view = Some(own_view(false, false));
    snapshot.viewed_panes.clear();
    snapshot.presence = Some(crate::SidebarPresence::Active);
    snapshot
}

fn set_agent_status(snapshot: &mut SidebarSnapshot, status: crate::agents::AgentStatus) {
    let card = snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .expect("agent row");
    card.status = status;
}

fn set_agent_phase(snapshot: &mut SidebarSnapshot, phase: crate::agents::TurnPhase) {
    let card = snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .expect("agent row");
    card.phase = phase;
}

#[test]
fn maintenance_drains_ready_snapshot_outcomes_without_snapshot_wakeup() {
    let ws = workspace();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let runtime = RuntimePaths::under(ws.clone(), runtime_dir.path()).expect("runtime");
    let socket_path = sidebar_socket_path(&runtime, &SidebarInstanceId::new());
    let (_dir, mut state) = loop_state(&ws);
    state.last_heartbeat = Some(Instant::now());
    let config = serve_config(&ws);
    let (mut fetch, _request_rx) = fetch_dispatcher();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    result_tx
        .send(FetchOutcome {
            snapshot: Ok(agent_snapshot(&ws)),
            final_for_request: true,
            fresh_pane_frame: false,
            unchanged: false,
        })
        .expect("send fetch outcome");

    state
        .run_maintenance(
            &mut fetch,
            MaintenanceContext {
                config: &config,
                runtime: &runtime,
                socket_path: &socket_path,
                result_rx: &result_rx,
                anim_start: Instant::now(),
                diag: &crate::diag::DiagSink::disabled(),
                tick: Duration::from_secs(60),
            },
        )
        .expect("maintenance drains ready outcome");

    assert_eq!(state.current.worktree_groups.len(), 1);
    assert_eq!(state.current.worktree_groups[0].rows[0].name, "claude");
    assert!(state.last_snapshot.is_some());
    assert!(state.dirty, "the folded snapshot is paint-pending");
}

#[test]
fn unchanged_fetch_outcome_clears_in_flight_without_dirtying_frame() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    state.dirty = false;
    let config = serve_config(&ws);
    let (mut fetch, request_rx) = fetch_dispatcher();
    fetch.request(FetchRequest::default(), false);
    assert!(request_rx.try_recv().is_ok());
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    result_tx
        .send(FetchOutcome {
            snapshot: Err("unchanged".to_owned()),
            final_for_request: true,
            fresh_pane_frame: false,
            unchanged: true,
        })
        .expect("send unchanged outcome");

    state
        .on_snapshot(
            &config,
            &mut fetch,
            &result_rx,
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("apply unchanged outcome");

    assert!(!state.dirty);
    fetch.request(FetchRequest::default(), false);
    assert!(
        request_rx.try_recv().is_ok(),
        "unchanged final outcome must release the single-flight request"
    );
}

#[test]
fn unchanged_fetch_outcome_dispatches_queued_refetch() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    state.dirty = false;
    let config = serve_config(&ws);
    let (mut fetch, request_rx) = fetch_dispatcher();
    fetch.request(FetchRequest::default(), false);
    assert!(request_rx.try_recv().is_ok());
    fetch.request(FetchRequest::producer_fresh_panes(), true);
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    result_tx
        .send(FetchOutcome {
            snapshot: Err("unchanged".to_owned()),
            final_for_request: true,
            fresh_pane_frame: false,
            unchanged: true,
        })
        .expect("send unchanged outcome");

    state
        .on_snapshot(
            &config,
            &mut fetch,
            &result_rx,
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("apply unchanged outcome");

    assert!(!state.dirty);
    assert!(
        request_rx
            .try_recv()
            .expect("pending refetch dispatched")
            .is_producer_fresh_panes(),
        "unchanged final outcome must not strand a queued forced refetch"
    );
}

#[test]
fn maintenance_requests_releasing_fetch_when_order_hold_expires() {
    let ws = workspace();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let runtime = RuntimePaths::under(ws.clone(), runtime_dir.path()).expect("runtime");
    let socket_path = sidebar_socket_path(&runtime, &SidebarInstanceId::new());
    let (_dir, mut state) = loop_state(&ws);
    state.last_heartbeat = Some(Instant::now());
    state.last_self_close_check = Instant::now();
    state.ui.order_hold = Some(crate::sidebar_pane::render::OrderHold {
        frozen: crate::sidebar_pane::render::FrozenOrder::default(),
        expires_ms: jiff::Timestamp::now().as_millisecond() - 1,
    });
    let config = serve_config(&ws);
    let (mut fetch, request_rx) = fetch_dispatcher();
    let (_result_tx, result_rx) = std::sync::mpsc::channel();

    state
        .run_maintenance(
            &mut fetch,
            MaintenanceContext {
                config: &config,
                runtime: &runtime,
                socket_path: &socket_path,
                result_rx: &result_rx,
                anim_start: Instant::now(),
                diag: &crate::diag::DiagSink::disabled(),
                tick: Duration::from_secs(60),
            },
        )
        .expect("maintenance requests release fold");

    assert!(
        request_rx.try_recv().is_ok(),
        "expired order hold schedules a fold to release the frozen order"
    );
}

#[test]
fn frame_timing_suspends_unwatched_animation() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane.clone()));
    state.current = animating_agent_snapshot(&ws);
    state.current.own_view = Some(own_view(false, false));
    state.current.viewed_panes.clear();
    state.dirty = false;

    assert!(!frame_active(&state));

    state.current.own_view = Some(own_view(false, true));
    assert!(frame_active(&state));

    state.current.own_view = Some(own_view(true, false));
    state.current.viewed_panes = vec![own_pane];
    assert!(frame_active(&state));
}

#[test]
fn frame_timing_resumes_on_own_pane_focus() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let foreign_pane = pane("terminal_2", "tab_0", false).pane_id;
    let mut snapshot = animating_agent_snapshot(&ws);
    snapshot.own_view = Some(own_view(false, false));
    snapshot.viewed_panes.clear();

    let config = serve_config(&ws);
    let viewport = ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 24));
    let mut terminal = Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        ratatui::TerminalOptions { viewport },
    )
    .expect("terminal");
    for (focused, resumes) in [(own_pane.clone(), true), (foreign_pane, false)] {
        let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane.clone()));
        state.last_pulled = snapshot.clone();
        state.current = snapshot.clone();
        state.dirty = false;
        let (mut fetch, request_rx) = fetch_dispatcher();
        state
            .on_event(
                &config,
                &mut fetch,
                &mut terminal,
                SidebarEventEnvelope::new(
                    ws.clone(),
                    Some("rimz-test".to_owned()),
                    crate::sidebar::timing::unix_now_ms(),
                    SidebarEvent::FocusChanged {
                        focused: vec![focused],
                        unfocused: Vec::new(),
                    },
                ),
                Instant::now(),
                &crate::diag::DiagSink::disabled(),
            )
            .expect("focus event folds");
        state.dirty = false;

        assert_eq!(state.optimistic_watch_until.is_some(), resumes);
        assert_eq!(frame_active(&state), resumes);
        assert_eq!(
            request_rx
                .try_recv()
                .ok()
                .map(FetchRequest::is_producer_fresh_panes),
            resumes.then_some(true)
        );
    }
}

#[test]
fn frame_timing_keeps_unknown_or_detached_dirty_hot_but_holds_hidden_dirty() {
    let ws = workspace();
    let (_dir, mut state) = loop_state_with_own_pane(&ws, None);
    state.current = animating_agent_snapshot(&ws);
    state.dirty = false;
    assert!(frame_active(&state));

    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane.clone()));
    state.current = animating_agent_snapshot(&ws);
    state.current.own_view = None;
    state.dirty = false;
    assert!(frame_active(&state));

    state.current.own_view = Some(own_view(false, false));
    state.current.viewed_panes.clear();
    state.dirty = true;
    state.current.presence = Some(crate::SidebarPresence::Active);
    assert!(!frame_active(&state));

    state.current.presence = Some(crate::SidebarPresence::Detached);
    assert!(frame_active(&state));

    state.current.presence = Some(crate::SidebarPresence::Active);
    state.current.own_view = Some(own_view(true, false));
    state.current.viewed_panes = vec![own_pane];
    assert!(frame_active(&state));
}

#[test]
fn frame_timing_caps_idle_timeout_at_order_hold_expiry() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    state.dirty = false;
    state.last_self_close_check = Instant::now();
    state.ui.theme(&state.current.theme);
    let now_ms = jiff::Timestamp::now().as_millisecond();
    state.ui.order_hold = Some(crate::sidebar_pane::render::OrderHold {
        frozen: crate::sidebar_pane::render::FrozenOrder::default(),
        expires_ms: now_ms + 200,
    });

    let (_active, timeout) = state.frame_timing(Duration::from_secs(10), Instant::now());

    assert!(
        timeout <= Duration::from_millis(200),
        "idle wait wakes for the order-hold release fold"
    );
}

#[test]
fn input_browse_arms_order_hold_before_next_fold() {
    let ws = workspace();
    let config = serve_config(&ws);
    let (_dir, mut state) = loop_state(&ws);
    let panes = vec![
        pane("terminal_1", "tab_0", false),
        pane("terminal_2", "tab_0", false),
    ];
    let first = panes[0].pane_id.clone();
    let second = panes[1].pane_id.clone();
    state.current = snapshot_with_panes(&ws, panes);
    state.ui.selected_pane = Some(first);
    state.ui.selected_index = 0;
    state.ui.last_order = super::super::order_hold::capture_order(&state.current);
    let (mut fetch, _request_rx) = fetch_dispatcher();
    let mut terminal = fixed_terminal();

    state
        .on_input(
            &config,
            Wakeup::Key(KeyAction::Down),
            &mut terminal,
            &mut fetch,
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("browse input");

    assert_eq!(state.ui.selected_pane, Some(second));
    assert!(
        state.ui.order_hold.is_some(),
        "arrow-key browse arms the order hold immediately, without waiting for a fold"
    );
}

#[test]
fn focus_anchor_write_carries_current_scroll_offset() {
    let ws = workspace();
    let (dir, mut state) = loop_state(&ws);
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_2");
    state.ui.scroll_offset = 11;

    state.record_focus_anchor(&pane);

    let anchor = crate::sidebar::focus_anchor::load(&runtime).expect("focus anchor");
    assert_eq!(anchor.pane_id, pane);
    assert_eq!(anchor.offset, 11);
    assert!(crate::sidebar::focus_anchor::is_fresh(
        anchor.stamp_ms,
        crate::sidebar::timing::unix_now_ms(),
    ));
}

#[test]
fn fresh_focus_anchor_seeds_scroll_on_matching_fold() {
    let ws = workspace();
    let config = serve_config(&ws);
    let (dir, mut state) = loop_state(&ws);
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    let target = PaneId::from_parts(crate::MuxName::Zellij, "terminal_2");
    let stamp_ms = crate::sidebar::timing::unix_now_ms();
    crate::sidebar::focus_anchor::store(
        &runtime,
        &crate::sidebar::focus_anchor::FocusAnchor {
            pane_id: target.clone(),
            offset: 7,
            stamp_ms,
        },
    )
    .expect("store anchor");
    state.ui.scroll_offset = 2;
    state.ui.manual_scroll = Some(crate::sidebar_pane::render::ManualScroll {
        selection_at_start: Some(PaneId::from_parts(crate::MuxName::Zellij, "terminal_1")),
    });
    let (mut fetch, _request_rx) = fetch_dispatcher();

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        snapshot_with_active_pane(&ws, target.clone()),
        true,
    );

    assert_eq!(state.ui.selected_pane, Some(target));
    assert_eq!(state.ui.scroll_offset, 7);
    assert_eq!(state.ui.manual_scroll, None);
    assert!(
        !state.ui.focus_group_reveal,
        "a sidebar jump's fresh anchor suppresses external-focus group reveal"
    );
    assert_eq!(state.ui.last_focus_anchor_ms, stamp_ms);
}

#[test]
fn external_focus_change_arms_group_reveal_once() {
    let ws = workspace();
    let config = serve_config(&ws);
    let (_dir, mut state) = loop_state(&ws);
    let target = PaneId::from_parts(crate::MuxName::Zellij, "terminal_2");
    let (mut fetch, _request_rx) = fetch_dispatcher();

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        snapshot_with_active_pane(&ws, target.clone()),
        true,
    );

    assert_eq!(state.ui.selected_pane, Some(target.clone()));
    assert!(
        state.ui.focus_group_reveal,
        "the first active pane learned on attach arms a one-shot group reveal"
    );

    state.ui.focus_group_reveal = false;
    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        snapshot_with_active_pane(&ws, target),
        true,
    );

    assert!(
        !state.ui.focus_group_reveal,
        "unchanged active pane refolds leave the consumed reveal off"
    );
}

#[test]
fn focus_anchor_for_other_pane_leaves_scroll_untouched() {
    let ws = workspace();
    let config = serve_config(&ws);
    let (dir, mut state) = loop_state(&ws);
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    let selected = PaneId::from_parts(crate::MuxName::Zellij, "terminal_2");
    crate::sidebar::focus_anchor::store(
        &runtime,
        &crate::sidebar::focus_anchor::FocusAnchor {
            pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_1"),
            offset: 7,
            stamp_ms: crate::sidebar::timing::unix_now_ms(),
        },
    )
    .expect("store anchor");
    state.ui.scroll_offset = 3;
    let (mut fetch, _request_rx) = fetch_dispatcher();

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        snapshot_with_active_pane(&ws, selected.clone()),
        true,
    );

    assert_eq!(state.ui.selected_pane, Some(selected));
    assert_eq!(state.ui.scroll_offset, 3);
    assert_eq!(state.ui.last_focus_anchor_ms, 0);
}

#[test]
fn focus_anchor_stamp_applies_once() {
    let ws = workspace();
    let config = serve_config(&ws);
    let (dir, mut state) = loop_state(&ws);
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    let target = PaneId::from_parts(crate::MuxName::Zellij, "terminal_2");
    let stamp_ms = crate::sidebar::timing::unix_now_ms();
    crate::sidebar::focus_anchor::store(
        &runtime,
        &crate::sidebar::focus_anchor::FocusAnchor {
            pane_id: target.clone(),
            offset: 7,
            stamp_ms,
        },
    )
    .expect("store anchor");
    let (mut fetch, _request_rx) = fetch_dispatcher();

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        snapshot_with_active_pane(&ws, target.clone()),
        true,
    );
    assert_eq!(state.ui.scroll_offset, 7);

    state.ui.scroll_offset = 4;
    state.ui.manual_scroll = Some(crate::sidebar_pane::render::ManualScroll {
        selection_at_start: Some(target.clone()),
    });
    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        snapshot_with_active_pane(&ws, target),
        true,
    );

    assert_eq!(state.ui.scroll_offset, 4);
    assert!(state.ui.manual_scroll.is_some());
}

#[test]
fn stale_focus_anchor_is_ignored() {
    let ws = workspace();
    let config = serve_config(&ws);
    let (dir, mut state) = loop_state(&ws);
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    let target = PaneId::from_parts(crate::MuxName::Zellij, "terminal_2");
    let ttl_ms = crate::sidebar::timing::FOCUS_ANCHOR_FRESH.as_millis() as u64;
    let stale_stamp = crate::sidebar::timing::unix_now_ms().saturating_sub(ttl_ms + 1);
    crate::sidebar::focus_anchor::store(
        &runtime,
        &crate::sidebar::focus_anchor::FocusAnchor {
            pane_id: target.clone(),
            offset: 7,
            stamp_ms: stale_stamp,
        },
    )
    .expect("store anchor");
    state.ui.scroll_offset = 3;
    let (mut fetch, _request_rx) = fetch_dispatcher();

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        snapshot_with_active_pane(&ws, target.clone()),
        true,
    );

    assert_eq!(state.ui.selected_pane, Some(target));
    assert_eq!(state.ui.scroll_offset, 3);
    assert_eq!(state.ui.last_focus_anchor_ms, 0);
}

#[test]
fn background_paint_updates_hidden_attached_sidebar_on_status_change() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane));
    let idle = hidden_attached_agent_snapshot(&ws, crate::agents::AgentStatus::Idle);
    let running = hidden_attached_agent_snapshot(&ws, crate::agents::AgentStatus::Running);
    state.current = running;
    state.last_bg_key = Some(background_content_key(&idle));
    state.dirty = true;
    state.next_frame = Instant::now() + Duration::from_secs(60);

    let mut terminal = fixed_terminal();
    state
        .paint_frame_if_due(&mut terminal, Instant::now(), false)
        .expect("background paint");

    let current_key = background_content_key(&state.current);
    assert!(!state.dirty, "meaningful background change paints");
    assert!(state.last_bg_paint.is_some());
    assert_eq!(state.last_bg_key.as_ref(), Some(&current_key));
}

#[test]
fn background_paint_skips_unchanged_glanceable_key() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane));
    let mut snapshot = hidden_attached_agent_snapshot(&ws, crate::agents::AgentStatus::Running);
    set_agent_phase(&mut snapshot, crate::agents::TurnPhase::Reasoning);
    let mut phase_only = snapshot.clone();
    set_agent_phase(&mut phase_only, crate::agents::TurnPhase::Acting);
    state.current = phase_only;
    state.last_bg_key = Some(background_content_key(&snapshot));
    state.last_bg_paint =
        Some(Instant::now() - crate::sidebar::timing::BACKGROUND_PAINT_MIN_INTERVAL);
    state.dirty = true;

    let stamp = state.last_bg_paint;
    let mut terminal = fixed_terminal();
    state
        .paint_frame_if_due(&mut terminal, Instant::now(), false)
        .expect("background skip");

    assert!(state.dirty, "phase-only background change stays pending");
    assert_eq!(state.last_bg_paint, stamp);
}

#[test]
fn background_paint_throttles_changed_hidden_content() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane));
    let idle = hidden_attached_agent_snapshot(&ws, crate::agents::AgentStatus::Idle);
    let waiting = hidden_attached_agent_snapshot(&ws, crate::agents::AgentStatus::Waiting);
    let stamp = Instant::now();
    state.current = waiting;
    state.last_bg_key = Some(background_content_key(&idle));
    state.last_bg_paint = Some(stamp);
    state.dirty = true;

    let mut terminal = fixed_terminal();
    state
        .paint_frame_if_due(&mut terminal, Instant::now(), false)
        .expect("background throttle");

    assert!(state.dirty, "throttled background change stays pending");
    assert_eq!(state.last_bg_paint, Some(stamp));
}

#[test]
fn background_paint_tracks_process_stuck_state() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane));
    let idle = hidden_attached_process_snapshot(&ws, crate::ProcessState::Idle);
    let stuck = hidden_attached_process_snapshot(&ws, crate::ProcessState::Stuck);
    state.current = stuck;
    state.last_bg_key = Some(background_content_key(&idle));
    state.dirty = true;
    state.next_frame = Instant::now() + Duration::from_secs(60);

    let mut terminal = fixed_terminal();
    state
        .paint_frame_if_due(&mut terminal, Instant::now(), false)
        .expect("process stuck background paint");

    assert!(!state.dirty, "idle-to-stuck process state paints");
    assert_eq!(
        state.last_bg_key.as_ref(),
        Some(&background_content_key(&state.current))
    );
}

#[test]
fn detached_dirty_sidebar_still_paints_through_existing_path() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane));
    state.current = hidden_attached_agent_snapshot(&ws, crate::agents::AgentStatus::Waiting);
    state.current.presence = Some(crate::SidebarPresence::Detached);
    state.dirty = true;

    let mut terminal = fixed_terminal();
    state
        .paint_frame_if_due(&mut terminal, Instant::now(), false)
        .expect("detached paint");

    assert!(!state.dirty, "detached dirty path stays hot");
    assert_eq!(state.last_bg_paint, None);
}

#[test]
fn resize_hold_releases_on_escape_hatch_accepting_post_engage_stamp() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    let config = serve_config(&ws);
    let (mut fetch, _request_rx) = fetch_dispatcher();
    let mut prior = agent_snapshot(&ws);
    prior.panes_observed_at_ms = Some(90);
    state.last_snapshot = Some(prior.clone());
    state.current = prior;
    state.paint_hold.engage(Instant::now(), 100);

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        process_snapshot(&ws, 150),
        false,
    );
    assert!(
        state.paint_hold.is_engaged(),
        "the rejected fold stays held"
    );
    assert_eq!(state.gate.reject_streak, 1);

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        process_snapshot(&ws, 151),
        false,
    );
    assert!(
        state.paint_hold.is_engaged(),
        "the second rejected fold still stays held"
    );
    assert_eq!(state.gate.reject_streak, 2);

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        process_snapshot(&ws, 152),
        false,
    );
    assert!(
        !state.paint_hold.is_engaged(),
        "the escape-hatch accepted fold releases by pane stamp"
    );
}

#[test]
fn resize_hold_releases_on_accepted_default_fetch_with_post_engage_stamp() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    let config = serve_config(&ws);
    let (mut fetch, _request_rx) = fetch_dispatcher();
    state.current = agent_snapshot(&ws);
    state.paint_hold.engage(Instant::now(), 100);

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        agent_snapshot_observed(&ws, 101),
        false,
    );

    assert!(
        !state.paint_hold.is_engaged(),
        "a normal accepted fetch releases when it carries a post-resize pane stamp"
    );
}

#[test]
fn resize_hold_stays_held_on_accepted_default_fetch_with_pre_engage_stamp() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    let config = serve_config(&ws);
    let (mut fetch, _request_rx) = fetch_dispatcher();
    state.current = agent_snapshot(&ws);
    state.paint_hold.engage(Instant::now(), 100);

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        agent_snapshot_observed(&ws, 99),
        false,
    );

    assert!(
        state.paint_hold.is_engaged(),
        "an old pane stamp is not proof the resize verdict landed"
    );
}

#[test]
fn paint_path_arms_resize_hold_on_grow_without_advancing_prev_width() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    state.prev_width = Some(60);
    state.self_close.seen_sibling = true;

    assert!(state.arm_paint_hold_on_grow(120, Instant::now()));
    assert!(state.paint_hold.is_engaged(), "grow arms the paint hold");
    assert_eq!(
        state.prev_width,
        Some(60),
        "resize wakeup still owns prev_width advancement"
    );
}

#[test]
fn paint_path_does_not_arm_resize_hold_without_grow() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    state.prev_width = Some(120);
    state.self_close.seen_sibling = true;

    assert!(!state.arm_paint_hold_on_grow(120, Instant::now()));
    assert!(
        !state.paint_hold.is_engaged(),
        "same-width paint does not arm the hold"
    );
    assert!(!state.arm_paint_hold_on_grow(60, Instant::now()));
    assert!(
        !state.paint_hold.is_engaged(),
        "shrink paint does not arm the hold"
    );
}

#[test]
fn arm_paint_hold_does_not_engage_before_a_sibling_is_seen() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    state.prev_width = Some(60);

    assert!(!state.arm_paint_hold_on_grow(120, Instant::now()));
    assert!(
        !state.paint_hold.is_engaged(),
        "startup grow paints immediately before any sibling has been observed"
    );
}

#[test]
fn resize_reprobe_refreshes_pet_render_caps_for_session() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    let mut observed = None;

    state.refresh_pet_render_caps_with(crate::MuxName::Tmux, "rimz-test", |mux, session| {
        observed = Some((mux, session.to_owned()));
        PetRenderCaps {
            pixel_transport: true,
            kitty_term: true,
        }
    });

    assert_eq!(
        observed,
        Some((crate::MuxName::Tmux, "rimz-test".to_owned()))
    );
    assert_eq!(
        state.paint.caps(),
        PetRenderCaps {
            pixel_transport: true,
            kitty_term: true,
        }
    );
}

#[test]
fn resize_reprobe_can_downgrade_enabled_pet_render_caps() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    state.paint.set_caps(PetRenderCaps {
        pixel_transport: true,
        kitty_term: true,
    });

    state.refresh_pet_render_caps_with(crate::MuxName::Tmux, "rimz-test", |_, _| PetRenderCaps {
        pixel_transport: false,
        kitty_term: false,
    });

    assert_eq!(state.paint.caps(), PetRenderCaps::default());
}

#[test]
fn failed_anomaly_send_preserves_carried_drop_count() {
    let ws = workspace();
    let (tx, _rx) = std::sync::mpsc::sync_channel(0);
    let (_dir, store) = read_marks(&ws);
    let mut state = LoopState::new(
        ws.clone(),
        None,
        None,
        tx,
        store,
        PetRenderCaps::default(),
        true,
    );
    let mut current = agent_snapshot(&ws);
    let mut duplicate = current.worktree_groups[0].rows[0].clone();
    duplicate.pane = duplicate.pane.map(|mut pane| {
        pane.pane_id = crate::ids::PaneId::from_parts(crate::MuxName::Zellij, "terminal_10");
        pane
    });
    current.worktree_groups[0].rows.push(duplicate);
    current.worktree_groups[0].status_counts[0].count = 2;
    state.current = current;
    state.observer.dropped_msgs = 3;

    state.observe_commit();

    assert_eq!(
        state.observer.dropped_msgs, 5,
        "the failed anomaly send carries the prior drop count, then the failed roster send adds one"
    );
    state.observe_commit();
    assert_eq!(
        state.observer.dropped_msgs, 7,
        "a consecutive full-channel commit keeps accumulating without losing the carried count or pending roster retry"
    );
}
