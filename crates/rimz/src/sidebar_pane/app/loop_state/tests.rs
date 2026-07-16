use super::*;

#[test]
fn narrower_stops_before_the_minimum_frame_width() {
    use crate::mux::WidthAdjust;

    assert!(!width_adjust_allowed(WidthAdjust::Narrower, None));
    assert!(!width_adjust_allowed(WidthAdjust::Narrower, Some(24)));
    assert!(width_adjust_allowed(WidthAdjust::Narrower, Some(25)));
    assert!(width_adjust_allowed(WidthAdjust::Wider, None));
}
use crate::sidebar_pane::app::fixtures::{agent_snapshot, pane, snapshot_with_panes, workspace};
use crate::sidebar_pane::app::input::KeyAction;
use std::collections::HashSet;

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
        nav_keys: crate::sidebar_pane::app::NavKeymap::from_config(
            &crate::config::SidebarKeys::default(),
        ),
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
            PixelRenderCaps::default(),
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

fn own_view(_own_focused: bool, _pane_viewed: bool) -> crate::SidebarOwnView {
    crate::SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![pane("terminal_9", "tab_0", false).pane_id],
        own_view_is_daemon: false,
    }
}

fn snapshot_with_focused_pane(ws: &WorkspaceId, active: PaneId) -> SidebarSnapshot {
    let first = pane("terminal_1", "tab_0", false);
    let second = pane("terminal_2", "tab_0", false);
    let working_pane_ids = vec![first.pane_id.clone(), second.pane_id.clone()];
    let mut snapshot = snapshot_with_panes(ws, vec![first, second]);
    snapshot.own_view = Some(crate::SidebarOwnView {
        sibling_count: 2,
        working_pane_ids,
        own_view_is_daemon: false,
    });
    snapshot.focused_pane = Some(active);
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
        .send(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            role: FetchRole::Producer,
            phase: FetchPhase::Final,
            pane_frame: if fresh_pane_frame {
                PaneFrame::Fresh
            } else {
                PaneFrame::Held
            },
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

#[test]
fn spend_ratchet_holds_within_epoch_and_resets_across_epochs() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    let config = serve_config(&ws);
    let (mut fetch, _rx) = fetch_dispatcher();

    let mut high = agent_snapshot(&ws);
    high.today_spend_live_usd = Some(5.0);
    high.today_spend_epoch_secs = Some(10);
    fold_snapshot(&mut state, &config, &mut fetch, high, true);
    assert_eq!(state.ui.spend_ratchet.display(Some(10), 5.0), 5.0);

    let mut lower = agent_snapshot(&ws);
    lower.today_spend_live_usd = Some(3.0);
    lower.today_spend_epoch_secs = Some(10);
    fold_snapshot(&mut state, &config, &mut fetch, lower, true);
    assert_eq!(state.ui.spend_ratchet.display(Some(10), 3.0), 5.0);

    let mut older = agent_snapshot(&ws);
    older.today_spend_live_usd = Some(1.0);
    fold_snapshot(&mut state, &config, &mut fetch, older, true);
    assert_eq!(state.ui.spend_ratchet.display(None, 1.0), 1.0);

    let mut next = agent_snapshot(&ws);
    next.today_spend_live_usd = Some(2.0);
    next.today_spend_epoch_secs = Some(11);
    fold_snapshot(&mut state, &config, &mut fetch, next, true);
    assert_eq!(state.ui.spend_ratchet.display(Some(11), 2.0), 2.0);
}

#[test]
fn tripped_budget_ratchet_observes_the_day_spend_epoch_it_displays() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    let config = serve_config(&ws);
    let (mut fetch, _rx) = fetch_dispatcher();

    let capped = |spend_usd| {
        let mut snapshot = agent_snapshot(&ws);
        snapshot.today_spend_live_usd = Some(1.0);
        snapshot.today_spend_epoch_secs = Some(10);
        snapshot.fleet_day_spend_epoch_secs = Some(20);
        snapshot.fleet_budget = Some(crate::DailyBudgetView {
            cap_usd: 10.0,
            spend_usd,
            parked: true,
        });
        snapshot
    };
    fold_snapshot(&mut state, &config, &mut fetch, capped(5.0), true);
    fold_snapshot(&mut state, &config, &mut fetch, capped(3.0), true);

    let (usd, epoch) = render::cockpit_spend_target(&state.current).expect("tripped spend");
    assert_eq!((usd, epoch), (3.0, Some(20)));
    assert_eq!(state.ui.spend_ratchet.display(epoch, usd), 5.0);
}

fn fixed_terminal() -> Terminal<CrosstermBackend<io::Stdout>> {
    let viewport = ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 24));
    Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        ratatui::TerminalOptions { viewport },
    )
    .expect("terminal")
}

fn event_envelope(ws: &WorkspaceId, event: SidebarEvent) -> SidebarEventEnvelope {
    SidebarEventEnvelope::new(
        ws.clone(),
        Some("rimz-test".to_owned()),
        crate::sidebar::timing::unix_now_ms(),
        event,
    )
}

fn pane_publication(publication: crate::sidebar::events::PaneFramePublicationKind) -> SidebarEvent {
    SidebarEvent::PaneFramePublished { publication }
}

fn hide_consumer(state: &mut LoopState, ws: &WorkspaceId) {
    state.current = animating_agent_snapshot(ws);
    state.current.own_view = Some(own_view(false, false));
    state.current.viewed_panes.clear();
    state.last_pulled = state.current.clone();
    state.last_known_elder = false;
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
        .send(FetchUpdate::Snapshot {
            snapshot: Box::new(agent_snapshot(&ws)),
            role: FetchRole::Producer,
            phase: FetchPhase::Final,
            pane_frame: PaneFrame::Held,
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
        .send(FetchUpdate::Unchanged {
            role: FetchRole::Producer,
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
        .send(FetchUpdate::Unchanged {
            role: FetchRole::Producer,
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
fn maintenance_requests_force_fold_when_tab_read_dwell_expires() {
    let ws = workspace();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let runtime = RuntimePaths::under(ws.clone(), runtime_dir.path()).expect("runtime");
    let socket_path = sidebar_socket_path(&runtime, &SidebarInstanceId::new());
    let (_dir, mut state) = loop_state(&ws);
    state.current.own_view = Some(own_view(false, true));
    state.last_heartbeat = Some(Instant::now());
    state.last_self_close_check = Instant::now();
    state.tab_read_dwell_until = Some(Instant::now() - Duration::from_secs(1));
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
        .expect("maintenance requests tab dwell fold");

    assert!(
        request_rx
            .try_recv()
            .expect("tab dwell fold request")
            .forces_fold(),
        "expired tab dwell must bypass the unchanged-input skip"
    );
}

#[test]
fn maintenance_waits_for_own_view_before_tab_read_dwell_fetch() {
    let ws = workspace();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let runtime = RuntimePaths::under(ws.clone(), runtime_dir.path()).expect("runtime");
    let socket_path = sidebar_socket_path(&runtime, &SidebarInstanceId::new());
    let (_dir, mut state) = loop_state(&ws);
    state.current.own_view = None;
    state.last_heartbeat = Some(Instant::now());
    state.last_self_close_check = Instant::now();
    state.tab_read_dwell_until = Some(Instant::now() - Duration::from_secs(1));
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
        .expect("maintenance handles missing own-view");

    assert!(
        request_rx.try_recv().is_err(),
        "a frameless snapshot cannot prove the tab is still viewed"
    );
}

#[test]
fn self_close_watchdog_bypasses_unchanged_skip_while_empty_confirming() {
    let ws = workspace();
    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let runtime = RuntimePaths::under(ws.clone(), runtime_dir.path()).expect("runtime");
    let socket_path = sidebar_socket_path(&runtime, &SidebarInstanceId::new());
    let (_dir, mut state) = loop_state(&ws);
    let config = serve_config(&ws);
    let (mut fetch, request_rx) = fetch_dispatcher();
    let mut empty = agent_snapshot(&ws);
    empty.own_view = Some(crate::SidebarOwnView {
        sibling_count: 0,
        working_pane_ids: Vec::new(),
        own_view_is_daemon: false,
    });
    // Birth/resurrection path: no sibling observed yet, so zero enters the
    // confirm window and the watchdog forces a fresh producer fold.
    fold_snapshot(&mut state, &config, &mut fetch, empty, true);
    assert!(state.self_close.confirming_empty());
    state.last_heartbeat = Some(Instant::now());
    state.last_self_close_check = Instant::now() - SELF_CLOSE_WATCHDOG;
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
        .expect("maintenance requests self-close fold");

    assert!(
        request_rx
            .try_recv()
            .expect("self-close watchdog fetch")
            .is_producer_fresh_panes(),
        "pending empty confirmation must bypass the unchanged consumer memo"
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
    state.current.viewed_panes = vec![pane("terminal_9", "tab_0", false).pane_id];
    assert!(frame_active(&state));

    state.current.own_view = Some(own_view(true, false));
    state.current.viewed_panes = vec![own_pane];
    assert!(frame_active(&state));
}

#[test]
fn frame_timing_wakes_for_elapsed_tab_read_dwell() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane));
    state.current = animating_agent_snapshot(&ws);
    state.current.own_view = Some(own_view(false, false));
    state.current.viewed_panes.clear();
    state.dirty = false;
    state.tab_read_dwell_until = Some(Instant::now() - Duration::from_secs(1));

    let (active, timeout) = state.frame_timing(Duration::from_secs(60), Instant::now());

    assert!(!active);
    assert_eq!(timeout, FRAME_MIN_TIMEOUT);
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
fn unwatched_consumer_coalesces_identity_free_fetches_until_clamp_deadline() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane));
    hide_consumer(&mut state, &ws);
    let config = serve_config(&ws);
    let mut terminal = fixed_terminal();
    let (mut fetch, request_rx) = fetch_dispatcher();

    for event in [
        SidebarEvent::StoreDelta {
            event_method: None,
            agent_signal: None,
        },
        SidebarEvent::PanesChanged,
        SidebarEvent::StoreDelta {
            event_method: None,
            agent_signal: None,
        },
    ] {
        state
            .on_event(
                &config,
                &mut fetch,
                &mut terminal,
                event_envelope(&ws, event),
                Instant::now(),
                &crate::diag::DiagSink::disabled(),
            )
            .expect("identity-free event");
    }

    assert!(
        request_rx.try_recv().is_err(),
        "unwatched consumer defers the burst"
    );
    assert!(
        fetch
            .deferred_request()
            .expect("pending fetch")
            .is_producer_fresh_panes(),
        "coalescing preserves the strongest freshness requirement"
    );
    fetch.defer_until(
        FetchRequest::default(),
        Instant::now() - Duration::from_millis(1),
    );

    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let runtime = RuntimePaths::under(ws.clone(), runtime_dir.path()).expect("runtime");
    let socket_path = sidebar_socket_path(&runtime, &SidebarInstanceId::new());
    let (_result_tx, result_rx) = std::sync::mpsc::channel();
    state.last_heartbeat = Some(Instant::now());
    state.last_self_close_check = Instant::now();
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
        .expect("maintenance fires due pending fetch");

    assert!(
        request_rx
            .try_recv()
            .expect("one deferred fetch")
            .is_producer_fresh_panes()
    );
    assert!(request_rx.try_recv().is_err(), "burst emits one fetch");
}

#[test]
fn repeated_hidden_metrics_publications_fold_once_at_the_background_deadline() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane));
    hide_consumer(&mut state, &ws);
    let config = serve_config(&ws);
    let mut terminal = fixed_terminal();
    let (mut fetch, request_rx) = fetch_dispatcher();

    for _ in 0..3 {
        state
            .on_event(
                &config,
                &mut fetch,
                &mut terminal,
                event_envelope(
                    &ws,
                    pane_publication(crate::sidebar::events::PaneFramePublicationKind::Metrics),
                ),
                Instant::now(),
                &crate::diag::DiagSink::disabled(),
            )
            .expect("defer metrics publication");
    }
    assert!(request_rx.try_recv().is_err());
    let deadline = fetch.next_deadline().expect("one deferred fetch");
    assert!(
        deadline.saturating_duration_since(Instant::now())
            <= crate::sidebar::timing::UNWATCHED_METRICS_FOLD_CLAMP
    );
    fetch.defer_until(
        FetchRequest::default(),
        Instant::now() - Duration::from_millis(1),
    );

    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let runtime = RuntimePaths::under(ws.clone(), runtime_dir.path()).expect("runtime");
    let socket_path = sidebar_socket_path(&runtime, &SidebarInstanceId::new());
    let (_result_tx, result_rx) = std::sync::mpsc::channel();
    state.last_heartbeat = Some(Instant::now());
    state.last_self_close_check = Instant::now();
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
        .expect("maintenance folds due metrics");

    assert!(
        request_rx.try_recv().is_ok(),
        "the metrics burst emits one fetch at its deadline"
    );
    assert!(request_rx.try_recv().is_err());
    assert!(fetch.next_deadline().is_none());
}

#[test]
fn topology_and_store_publications_shorten_a_metrics_deadline() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let config = serve_config(&ws);
    let mut terminal = fixed_terminal();

    for shorter in [
        pane_publication(crate::sidebar::events::PaneFramePublicationKind::Topology),
        SidebarEvent::StoreDelta {
            event_method: None,
            agent_signal: None,
        },
    ] {
        let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane.clone()));
        hide_consumer(&mut state, &ws);
        let (mut fetch, request_rx) = fetch_dispatcher();
        state
            .on_event(
                &config,
                &mut fetch,
                &mut terminal,
                event_envelope(
                    &ws,
                    pane_publication(crate::sidebar::events::PaneFramePublicationKind::Metrics),
                ),
                Instant::now(),
                &crate::diag::DiagSink::disabled(),
            )
            .expect("defer metrics publication");
        let metrics_due = fetch.next_deadline().expect("metrics pending");

        state
            .on_event(
                &config,
                &mut fetch,
                &mut terminal,
                event_envelope(&ws, shorter),
                Instant::now(),
                &crate::diag::DiagSink::disabled(),
            )
            .expect("shorter publication");
        let shortened = fetch.next_deadline().expect("shortened pending fetch");
        assert!(shortened < metrics_due);

        state
            .on_event(
                &config,
                &mut fetch,
                &mut terminal,
                event_envelope(&ws, SidebarEvent::PanesChanged),
                Instant::now(),
                &crate::diag::DiagSink::disabled(),
            )
            .expect("stronger topology request");
        assert_eq!(fetch.next_deadline(), Some(shortened));
        assert!(
            fetch
                .deferred_request()
                .expect("merged pending fetch")
                .is_producer_fresh_panes()
        );
        assert!(request_rx.try_recv().is_err());
    }
}

#[test]
fn watched_metrics_and_hidden_presence_publications_fold_immediately() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let config = serve_config(&ws);
    let mut terminal = fixed_terminal();

    for (publication, watched) in [
        (
            crate::sidebar::events::PaneFramePublicationKind::Metrics,
            true,
        ),
        (
            crate::sidebar::events::PaneFramePublicationKind::Presence,
            false,
        ),
    ] {
        let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane.clone()));
        hide_consumer(&mut state, &ws);
        if watched {
            state.current.viewed_panes = vec![pane("terminal_9", "tab_0", false).pane_id];
        }
        let (mut fetch, request_rx) = fetch_dispatcher();

        state
            .on_event(
                &config,
                &mut fetch,
                &mut terminal,
                event_envelope(&ws, pane_publication(publication)),
                Instant::now(),
                &crate::diag::DiagSink::disabled(),
            )
            .expect("immediate publication");

        assert!(request_rx.try_recv().is_ok());
        assert!(fetch.next_deadline().is_none());
    }
}

#[test]
fn width_target_event_reloads_the_override_without_a_producer_fetch() {
    let ws = workspace();
    let (dir, mut state) = loop_state(&ws);
    let config = serve_config(&ws);
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    crate::sidebar::width_override::write(
        &runtime,
        std::num::NonZeroU16::new(60).expect("nonzero width"),
    )
    .expect("write width override");
    let mut terminal = fixed_terminal();
    let (mut fetch, request_rx) = fetch_dispatcher();

    state
        .on_event(
            &config,
            &mut fetch,
            &mut terminal,
            event_envelope(&ws, SidebarEvent::WidthTargetChanged),
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("width target event");

    assert_eq!(
        state.width_control.decide(50, Instant::now()),
        Some((50, 60))
    );
    assert!(
        request_rx.try_recv().is_err(),
        "width propagation stays out of the producer path",
    );
}

#[test]
fn settled_native_width_adjustment_records_retargets_and_broadcasts() {
    let ws = workspace();
    let (dir, mut state) = loop_state(&ws);
    let config = serve_config(&ws);
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    let instance = SidebarInstanceId::new();
    let socket_path = runtime.sock_dir.join("width-target-test.sock");
    let socket = UnixDatagram::bind(&socket_path).expect("bind wakeup socket");
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("set socket timeout");
    crate::sidebar::write_heartbeat(
        &runtime,
        ws.clone(),
        &instance,
        crate::MuxName::Zellij,
        &config.session_name,
        &socket_path,
        None,
    )
    .expect("write heartbeat");
    let mut terminal = fixed_terminal();
    let (mut fetch, _request_rx) = fetch_dispatcher();
    state.width_adjust_pending = Some(Instant::now());
    state.width_control.set_suspended(true);

    state
        .on_resize(
            &config,
            &runtime,
            &mut fetch,
            &mut terminal,
            Some(80),
            Instant::now(),
        )
        .expect("settle native width adjustment");

    let recorded = std::num::NonZeroU16::new(80).expect("nonzero width");
    assert_eq!(
        crate::sidebar::width_override::load(&runtime),
        Some(recorded)
    );
    assert_eq!(
        state.width_control.decide(70, Instant::now()),
        Some((70, 80)),
        "recording the target also resumes and retargets local control",
    );
    let mut payload = [0_u8; 1024];
    let received = socket.recv(&mut payload).expect("receive target broadcast");
    let envelope: SidebarEventEnvelope =
        serde_json::from_slice(&payload[..received]).expect("decode target broadcast");
    assert_eq!(envelope.event, SidebarEvent::WidthTargetChanged);
}

#[test]
fn maintenance_watchdog_absorbs_deferred_unwatched_fetch() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane));
    hide_consumer(&mut state, &ws);
    let config = serve_config(&ws);
    let mut terminal = fixed_terminal();
    let (mut fetch, request_rx) = fetch_dispatcher();

    state
        .on_event(
            &config,
            &mut fetch,
            &mut terminal,
            event_envelope(
                &ws,
                SidebarEvent::StoreDelta {
                    event_method: None,
                    agent_signal: None,
                },
            ),
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("defer store delta");
    assert!(fetch.next_deadline().is_some());

    let runtime_dir = tempfile::TempDir::new().expect("runtime tempdir");
    let runtime = RuntimePaths::under(ws.clone(), runtime_dir.path()).expect("runtime");
    let socket_path = sidebar_socket_path(&runtime, &SidebarInstanceId::new());
    let (_result_tx, result_rx) = std::sync::mpsc::channel();
    state.last_heartbeat = Some(Instant::now());
    state.last_self_close_check = Instant::now() - SELF_CLOSE_WATCHDOG;
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
        .expect("maintenance runs watchdog fetch");

    assert!(
        request_rx.try_recv().is_ok(),
        "watchdog dispatches one fetch"
    );
    assert!(fetch.next_deadline().is_none());
    assert!(
        request_rx.try_recv().is_err(),
        "the deferred nudge merges into the watchdog fetch"
    );
}

#[test]
fn watched_renderer_and_elder_fetch_identity_free_events_immediately() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let config = serve_config(&ws);
    let mut terminal = fixed_terminal();

    for (watched, elder) in [(true, false), (false, true)] {
        let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane.clone()));
        hide_consumer(&mut state, &ws);
        state.last_known_elder = elder;
        if watched {
            state.current.own_view = Some(own_view(false, true));
            state.current.viewed_panes = vec![pane("terminal_9", "tab_0", false).pane_id];
        }
        let (mut fetch, request_rx) = fetch_dispatcher();

        state
            .on_event(
                &config,
                &mut fetch,
                &mut terminal,
                event_envelope(
                    &ws,
                    SidebarEvent::StoreDelta {
                        event_method: None,
                        agent_signal: None,
                    },
                ),
                Instant::now(),
                &crate::diag::DiagSink::disabled(),
            )
            .expect("identity-free event");

        assert!(request_rx.try_recv().is_ok());
        assert!(fetch.next_deadline().is_none());
    }
}

#[test]
fn focus_resume_flushes_pending_metrics_fetch() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane.clone()));
    hide_consumer(&mut state, &ws);
    let config = serve_config(&ws);
    let mut terminal = fixed_terminal();
    let (mut fetch, request_rx) = fetch_dispatcher();

    state
        .on_event(
            &config,
            &mut fetch,
            &mut terminal,
            event_envelope(
                &ws,
                pane_publication(crate::sidebar::events::PaneFramePublicationKind::Metrics),
            ),
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("defer metrics publication");
    assert!(fetch.next_deadline().is_some());

    state
        .on_event(
            &config,
            &mut fetch,
            &mut terminal,
            event_envelope(
                &ws,
                SidebarEvent::FocusChanged {
                    focused: vec![own_pane],
                    unfocused: Vec::new(),
                },
            ),
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("focus resumes");

    assert!(fetch.next_deadline().is_none());
    assert!(
        request_rx
            .try_recv()
            .expect("focus flushed pending fetch")
            .is_producer_fresh_panes()
    );
}

#[test]
fn focus_out_closes_help_popup() {
    let ws = workspace();
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane.clone()));
    let snapshot = snapshot_with_focused_pane(&ws, own_pane.clone());
    state.last_pulled = snapshot.clone();
    state.current = snapshot;
    state.ui.help_visible = true;
    state.optimistic_watch_until = Some(Instant::now() + Duration::from_secs(1));
    let config = serve_config(&ws);
    let mut terminal = fixed_terminal();
    let (mut fetch, _request_rx) = fetch_dispatcher();

    state
        .on_event(
            &config,
            &mut fetch,
            &mut terminal,
            event_envelope(
                &ws,
                SidebarEvent::FocusChanged {
                    focused: Vec::new(),
                    unfocused: vec![own_pane],
                },
            ),
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("focus-out event folds");

    assert!(!state.ui.help_visible);
    assert!(state.optimistic_watch_until.is_none());
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
    state.ui.last_order = super::super::order_hold::capture_order(&state.current, &state.ui);
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
fn answering_focused_agent_holds_the_pre_answer_order() {
    let ws = workspace();
    let config = serve_config(&ws);
    let (_dir, mut state) = loop_state(&ws);
    let mut before = agent_snapshot(&ws);
    let selected = before.worktree_groups[0].rows[0]
        .pane
        .as_ref()
        .expect("agent pane")
        .pane_id
        .clone();
    let mut other = before.worktree_groups[0].rows[0].clone();
    other.id = "agent-2".to_owned();
    other.pane = Some(pane("terminal_1", "tab_0", false));
    other.attention_score = 200;
    before.worktree_groups[0].rows[0].attention_score = 600;
    set_agent_status(&mut before, crate::agents::AgentStatus::Waiting);
    other.as_agent_mut().expect("agent row").status = crate::agents::AgentStatus::Running;
    before.worktree_groups[0].rows.push(other);
    before.focused_pane = Some(selected.clone());
    // Keep this fold independent of the existing focus-read hold trigger.
    before.viewed_panes.clear();
    before.sort_groups_for_presentation();
    state.current = before.clone();
    state.ui.selected_pane = Some(selected.clone());
    state.ui.baseline_pane = Some(selected);
    state.ui.last_order = super::super::order_hold::capture_order(&state.current, &state.ui);

    let mut after = before;
    after.panes_produced_at_ms = Some(2);
    after.worktree_groups[0].rows[0].attention_score = 200;
    set_agent_status(&mut after, crate::agents::AgentStatus::Running);
    let mut ranked = after.clone();
    ranked.sort_groups_for_presentation();
    assert_eq!(
        ranked.worktree_groups[0].rows[0].id, "agent-2",
        "the live rank moves the answered row down"
    );
    let (mut fetch, _request_rx) = fetch_dispatcher();

    fold_snapshot(&mut state, &config, &mut fetch, after, true);

    assert!(state.ui.order_hold.is_some());
    assert_eq!(
        state.current.worktree_groups[0]
            .rows
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        vec!["agent-1", "agent-2"]
    );
}

#[test]
fn mark_all_read_clears_every_unread_row_and_writes_receipts() {
    let ws = workspace();
    let config = serve_config(&ws);
    let (_dir, mut state) = loop_state(&ws);
    let mut snapshot = agent_snapshot(&ws);
    let mut second = snapshot.worktree_groups[0].rows[0].clone();
    snapshot.worktree_groups[0].rows[0].id = "agent-1".to_owned();
    snapshot.worktree_groups[0].rows[0].unread = true;
    second.id = "agent-2".to_owned();
    second.unread = true;
    snapshot.worktree_groups[0].rows.push(second);
    state.current = snapshot;
    state.ui.unread_guard = Some("agent-1".to_owned());
    state.ui.last_order = super::super::order_hold::capture_order(&state.current, &state.ui);
    let (mut fetch, request_rx) = fetch_dispatcher();
    let mut terminal = fixed_terminal();

    state
        .on_input(
            &config,
            Wakeup::Key(KeyAction::MarkAllRead),
            &mut terminal,
            &mut fetch,
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("mark all read");

    assert!(
        state
            .current
            .worktree_groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .all(|row| !row.unread),
        "all unread bits clear locally"
    );
    assert_eq!(state.ui.unread_guard, None);
    let marks = state.read_marks.load_merged();
    assert!(marks.cleared_at_ms("agent-1").is_some());
    assert!(marks.cleared_at_ms("agent-2").is_some());
    assert!(state.dirty);
    assert!(
        request_rx.try_recv().is_ok(),
        "mark-all schedules a convergence refetch"
    );
}

#[test]
fn record_focus_intent_writes_anchor_without_storing_an_overlay() {
    let ws = workspace();
    let (dir, mut state) = loop_state(&ws);
    let config = serve_config(&ws);
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_2");
    state.last_pulled = snapshot_with_focused_pane(
        &ws,
        PaneId::from_parts(crate::MuxName::Zellij, "terminal_1"),
    );
    state.ui.scroll_offset = 11;
    state.ui.last_order = crate::sidebar_pane::render::FrozenOrder {
        groups: vec!["main".to_owned()],
        rows: vec![
            crate::sidebar_pane::render::FrozenRow {
                id: "row-2".to_owned(),
                pane: None,
            },
            crate::sidebar_pane::render::FrozenRow {
                id: "row-1".to_owned(),
                pane: None,
            },
        ],
        visible: HashSet::from(["row-2".to_owned()]),
    };
    let recorded_order = state.ui.last_order.clone();

    state
        .record_focus_intent(
            &config,
            pane.clone(),
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("record focus intent");

    let anchor = crate::sidebar::focus_anchor::load(&runtime).expect("focus anchor");
    assert_eq!(anchor.pane_id, pane);
    assert_eq!(anchor.offset, 11);
    assert_eq!(anchor.order, Some(recorded_order));
    assert!(crate::sidebar::focus_anchor::is_fresh(
        anchor.stamp_ms,
        crate::sidebar::timing::unix_now_ms(),
    ));
    assert!(state.event_store.is_empty());
}

#[test]
fn confirmed_focus_intent_does_not_mask_a_later_focus_change() {
    let ws = workspace();
    let (dir, mut state) = loop_state(&ws);
    let config = serve_config(&ws);
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    let first = PaneId::from_parts(crate::MuxName::Zellij, "terminal_1");
    let target = PaneId::from_parts(crate::MuxName::Zellij, "terminal_2");
    let snapshot = snapshot_with_focused_pane(&ws, first.clone());
    state.last_pulled = snapshot.clone();
    state.current = snapshot;
    state
        .record_focus_intent(
            &config,
            target.clone(),
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("record focus intent");
    let anchor = crate::sidebar::focus_anchor::load(&runtime).expect("focus anchor");
    let mut terminal = fixed_terminal();
    let (mut fetch, _request_rx) = fetch_dispatcher();

    state
        .on_event(
            &config,
            &mut fetch,
            &mut terminal,
            event_envelope(
                &ws,
                SidebarEvent::FocusChanged {
                    focused: vec![target],
                    unfocused: vec![first.clone()],
                },
            ),
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("confirming focus event");
    assert_eq!(state.confirmed_focus_intent_ms, anchor.stamp_ms);

    state
        .on_event(
            &config,
            &mut fetch,
            &mut terminal,
            event_envelope(
                &ws,
                SidebarEvent::FocusChanged {
                    focused: vec![first.clone()],
                    unfocused: Vec::new(),
                },
            ),
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        )
        .expect("later focus event");

    assert_eq!(state.current.focused_pane, Some(first));
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
            order: None,
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
        snapshot_with_focused_pane(&ws, target.clone()),
        true,
    );

    assert_eq!(state.ui.selected_pane, Some(target.clone()));
    assert_eq!(state.ui.scroll_offset, 7);
    assert_eq!(state.ui.manual_scroll, None);
    assert!(
        !state.ui.focus_group_reveal,
        "a sidebar jump's fresh anchor suppresses external-focus group reveal"
    );
    assert_eq!(state.ui.last_focus_anchor_ms, stamp_ms);
}

#[test]
fn fresh_focus_anchor_with_order_installs_shared_hold() {
    let ws = workspace();
    let config = serve_config(&ws);
    let (dir, mut state) = loop_state(&ws);
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    let first = PaneId::from_parts(crate::MuxName::Zellij, "terminal_1");
    let target = PaneId::from_parts(crate::MuxName::Zellij, "terminal_2");
    let stamp_ms = crate::sidebar::timing::unix_now_ms();
    crate::sidebar::focus_anchor::store(
        &runtime,
        &crate::sidebar::focus_anchor::FocusAnchor {
            pane_id: target.clone(),
            offset: 7,
            stamp_ms,
            order: Some(crate::sidebar_pane::render::FrozenOrder {
                groups: vec!["/repo/main".to_owned()],
                rows: vec![
                    crate::sidebar_pane::render::FrozenRow {
                        id: target.to_string(),
                        pane: Some(target.to_string()),
                    },
                    crate::sidebar_pane::render::FrozenRow {
                        id: first.to_string(),
                        pane: Some(first.to_string()),
                    },
                ],
                visible: HashSet::from([target.to_string()]),
            }),
        },
    )
    .expect("store anchor");
    let (mut fetch, _request_rx) = fetch_dispatcher();

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        snapshot_with_focused_pane(&ws, target.clone()),
        true,
    );

    assert_eq!(state.ui.selected_pane, Some(target.clone()));
    assert_eq!(state.ui.scroll_offset, 7);
    assert_eq!(
        state.current.worktree_groups[0]
            .rows
            .iter()
            .map(|row| row.pane.as_ref().expect("pane").pane_id.clone())
            .collect::<Vec<_>>(),
        vec![target.clone(), first],
        "fold snapshot adopts anchor row order before paint"
    );
    let hold = state.ui.order_hold.as_ref().expect("shared hold");
    assert_eq!(hold.frozen.visible, HashSet::from([target.to_string()]));
    assert_eq!(
        hold.expires_ms,
        stamp_ms as i64 + crate::sidebar::timing::REORDER_HOLD.as_millis() as i64
    );
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
        snapshot_with_focused_pane(&ws, target.clone()),
        true,
    );

    assert_eq!(state.ui.selected_pane, Some(target.clone()));
    assert!(
        state.ui.focus_group_reveal,
        "the first focused pane learned on attach arms a one-shot group reveal"
    );

    state.ui.focus_group_reveal = false;
    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        snapshot_with_focused_pane(&ws, target),
        true,
    );

    assert!(
        !state.ui.focus_group_reveal,
        "unchanged focused pane refolds leave the consumed reveal off"
    );
}

#[test]
fn confirmed_focus_anchor_for_other_pane_leaves_scroll_untouched() {
    let ws = workspace();
    let config = serve_config(&ws);
    let (dir, mut state) = loop_state(&ws);
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    let selected = PaneId::from_parts(crate::MuxName::Zellij, "terminal_2");
    let stamp_ms = crate::sidebar::timing::unix_now_ms();
    crate::sidebar::focus_anchor::store(
        &runtime,
        &crate::sidebar::focus_anchor::FocusAnchor {
            pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_1"),
            offset: 7,
            stamp_ms,
            order: None,
        },
    )
    .expect("store anchor");
    state.confirmed_focus_intent_ms = stamp_ms;
    state.ui.scroll_offset = 3;
    let (mut fetch, _request_rx) = fetch_dispatcher();

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        snapshot_with_focused_pane(&ws, selected.clone()),
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
            order: None,
        },
    )
    .expect("store anchor");
    let (mut fetch, _request_rx) = fetch_dispatcher();

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        snapshot_with_focused_pane(&ws, target.clone()),
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
        snapshot_with_focused_pane(&ws, target),
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
            order: None,
        },
    )
    .expect("store anchor");
    state.ui.scroll_offset = 3;
    let (mut fetch, _request_rx) = fetch_dispatcher();

    fold_snapshot(
        &mut state,
        &config,
        &mut fetch,
        snapshot_with_focused_pane(&ws, target.clone()),
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
fn empty_close_suppresses_widened_paint_until_exit() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    let config = serve_config(&ws);
    let (mut fetch, _request_rx) = fetch_dispatcher();
    state.current = agent_snapshot(&ws);
    state.self_close.seen_sibling = true;
    state.paint_hold.engage(Instant::now(), 100);

    let mut empty = agent_snapshot_observed(&ws, 200);
    empty.own_view = Some(crate::SidebarOwnView {
        sibling_count: 0,
        working_pane_ids: Vec::new(),
        own_view_is_daemon: false,
    });
    fold_snapshot(&mut state, &config, &mut fetch, empty, true);

    assert!(
        state.should_exit,
        "seen-sibling zero exits on the producer-verified empty fold"
    );
    assert!(
        !state.self_close.confirming_empty(),
        "seen-sibling empty tabs skip the confirm window"
    );
    let mut terminal = fixed_terminal();
    state
        .paint_frame_if_due(&mut terminal, Instant::now(), true)
        .expect("paint attempt");
    assert!(
        state.dirty,
        "the closing fold suppresses full-width paint instead of clearing dirty"
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

    state.refresh_pet_render_caps_with(crate::MuxName::Tmux, "rimz-test", |mux, session, _prev| {
        observed = Some((mux, session.to_owned()));
        PixelRenderCaps {
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
        PixelRenderCaps {
            pixel_transport: true,
            kitty_term: true,
        }
    );
}

#[test]
fn resize_reprobe_can_downgrade_enabled_pet_render_caps() {
    let ws = workspace();
    let (_dir, mut state) = loop_state(&ws);
    state.paint.set_caps(PixelRenderCaps {
        pixel_transport: true,
        kitty_term: true,
    });

    state.refresh_pet_render_caps_with(crate::MuxName::Tmux, "rimz-test", |_, _, _| {
        PixelRenderCaps {
            pixel_transport: false,
            kitty_term: false,
        }
    });

    assert_eq!(state.paint.caps(), PixelRenderCaps::default());
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
        PixelRenderCaps::default(),
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
