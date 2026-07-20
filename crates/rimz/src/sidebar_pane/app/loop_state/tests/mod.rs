use super::*;
use crate::sidebar_pane::app::fixtures::{
    agent_snapshot, pane, serve_config, snapshot_with_panes, workspace,
};
use crate::sidebar_pane::app::input::KeyAction;
use std::collections::HashSet;
use std::path::PathBuf;

mod fetch;
mod focus;
mod maintenance;
mod paint;

/// One `LoopState` under test with the runtime, dispatcher, request channel,
/// and terminal it needs. Fields stay reachable: these tests drive internal
/// state by design, and the rig only removes the wiring around that reach.
struct Rig {
    _dir: tempfile::TempDir,
    ws: WorkspaceId,
    runtime: RuntimePaths,
    socket_path: PathBuf,
    config: ServeConfig,
    state: LoopState,
    fetch: FetchDispatcher,
    requests: Receiver<FetchRequest>,
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl Rig {
    fn new() -> Self {
        Self::build(None, None)
    }

    fn with_own_pane(own_pane: PaneId) -> Self {
        Self::build(Some(own_pane), None)
    }

    fn with_filter(filter: BodyFilter) -> Self {
        Self::build(None, Some(filter))
    }

    /// The terminal viewport width. Only the attach-resize path cares.
    fn width(mut self, cols: u16) -> Self {
        self.terminal = terminal_with_width(cols);
        self
    }

    fn build(own_pane: Option<PaneId>, filter: Option<BodyFilter>) -> Self {
        let ws = workspace();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
        if let Some(filter) = filter {
            crate::sidebar::body_filter::write(&runtime, filter).expect("write initial filter");
        }
        let instance_id = SidebarInstanceId::new();
        let socket_path = sidebar_socket_path(&runtime, &instance_id);
        let read_marks = ReadMarkStore::new(runtime.clone(), instance_id);
        let (observe_tx, _observe_rx) = std::sync::mpsc::sync_channel(64);
        let (request_tx, requests) = std::sync::mpsc::channel();
        Self {
            state: LoopState::new(
                ws.clone(),
                own_pane.as_ref().map_or(MuxName::Tmux, PaneId::mux),
                "rimz-test".to_owned(),
                own_pane,
                None,
                observe_tx,
                read_marks,
                PixelRenderCaps::default(),
                true,
            ),
            config: serve_config(&ws),
            fetch: FetchDispatcher::new(request_tx),
            requests,
            terminal: terminal_with_width(80),
            socket_path,
            runtime,
            ws,
            _dir: dir,
        }
    }

    fn event(&mut self, event: SidebarEvent) {
        let envelope = SidebarEventEnvelope::new(
            self.ws.clone(),
            Some("rimz-test".to_owned()),
            crate::sidebar::timing::unix_now_ms(),
            event,
        );
        self.state.on_event(
            &self.config,
            &mut self.fetch,
            &mut self.terminal,
            envelope,
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        );
    }

    fn fold(&mut self, snapshot: SidebarSnapshot, fresh_pane_frame: bool) {
        self.deliver(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            role: FetchRole::Producer,
            phase: FetchPhase::Final,
            pane_frame: if fresh_pane_frame {
                PaneFrame::Fresh
            } else {
                PaneFrame::Held
            },
        });
    }

    /// Drive `on_snapshot` over a channel carrying exactly `update`.
    fn deliver(&mut self, update: FetchUpdate) {
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        result_tx.send(update).expect("send fetch outcome");
        self.state.on_snapshot(
            &self.config,
            &mut self.fetch,
            &result_rx,
            Instant::now(),
            &crate::diag::DiagSink::disabled(),
        );
    }

    /// A maintenance sweep over an idle result channel. The heartbeat and
    /// self-close stamps start fresh; a test wanting an expired watchdog
    /// backdates `last_self_close_check` before calling.
    fn maintenance(&mut self) {
        self.maintenance_draining(None);
    }

    fn maintenance_draining(&mut self, update: Option<FetchUpdate>) {
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        if let Some(update) = update {
            result_tx.send(update).expect("send fetch outcome");
        }
        self.state.last_heartbeat.get_or_insert_with(Instant::now);
        self.state.run_maintenance(
            &mut self.fetch,
            MaintenanceContext {
                config: &self.config,
                runtime: &self.runtime,
                socket_path: &self.socket_path,
                result_rx: &result_rx,
                anim_start: Instant::now(),
                diag: &crate::diag::DiagSink::disabled(),
                tick: Duration::from_secs(60),
            },
        );
    }

    fn input(&mut self, action: KeyAction) {
        self.state
            .on_input(
                &self.config,
                Wakeup::Key(action),
                &mut self.terminal,
                &mut self.fetch,
                Instant::now(),
                &crate::diag::DiagSink::disabled(),
            )
            .expect("input");
    }

    fn paint(&mut self, active: bool) {
        self.state
            .paint_frame_if_due(&mut self.terminal, Instant::now(), active)
            .expect("paint");
    }

    fn next_request(&mut self) -> Option<FetchRequest> {
        self.requests.try_recv().ok()
    }

    /// Park the renderer off screen: animating content, no viewed pane, not
    /// the elder. The unwatched-path fixture.
    fn hide_consumer(&mut self) {
        self.state.current = animating_agent_snapshot(&self.ws);
        self.state.current.own_view = Some(own_view());
        self.state.current.viewed_panes.clear();
        let pulled = self.state.current.clone();
        self.set_pulled(&pulled);
        self.state.last_known_elder = false;
    }

    /// Bring the renderer back on screen by viewing a sibling pane.
    fn watch(&mut self) {
        self.state.current.viewed_panes = vec![pane("terminal_9", "tab_0", false).pane_id];
    }

    fn set_pulled(&mut self, snapshot: &SidebarSnapshot) {
        self.state.last_focus_observation = FocusObservation::from_snapshot(snapshot);
        self.state.last_pulled_sig = observe::PulledFrameSig::from_snapshot(snapshot);
        self.state.overlay_baseline = Some(snapshot.clone());
    }

    fn frame_active(&self) -> bool {
        self.state
            .frame_timing(Duration::from_secs(10), Instant::now())
            .0
    }
}

fn terminal_with_width(cols: u16) -> Terminal<CrosstermBackend<io::Stdout>> {
    let viewport = ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, cols, 24));
    Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        ratatui::TerminalOptions { viewport },
    )
    .expect("terminal")
}

fn own_view() -> crate::SidebarOwnView {
    crate::SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![pane("terminal_9", "tab_0", false).pane_id],
        own_view_is_daemon: false,
    }
}

fn empty_own_view() -> crate::SidebarOwnView {
    crate::SidebarOwnView {
        sibling_count: 0,
        working_pane_ids: Vec::new(),
        own_view_is_daemon: false,
    }
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
    set_agent_status(&mut snapshot, crate::agents::AgentStatus::Running);
    set_agent_phase(&mut snapshot, crate::agents::TurnPhase::Acting);
    snapshot
}

/// Two sibling panes in one tab with `active` focused — the fixture every
/// focus-anchor test resolves its anchor against.
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

fn hidden_attached_agent_snapshot(
    ws: &WorkspaceId,
    status: crate::agents::AgentStatus,
) -> SidebarSnapshot {
    let mut snapshot = agent_snapshot(ws);
    set_agent_status(&mut snapshot, status);
    hide(snapshot)
}

fn hidden_attached_process_snapshot(
    ws: &WorkspaceId,
    state: crate::ProcessState,
) -> SidebarSnapshot {
    let mut snapshot = process_snapshot(ws, 1);
    snapshot.worktree_groups[0].rows[0]
        .as_process_mut()
        .expect("process row")
        .state = state;
    hide(snapshot)
}

fn hide(mut snapshot: SidebarSnapshot) -> SidebarSnapshot {
    snapshot.own_view = Some(own_view());
    snapshot.viewed_panes.clear();
    snapshot.presence = Some(crate::SidebarPresence::Active);
    snapshot
}

fn set_agent_status(snapshot: &mut SidebarSnapshot, status: crate::agents::AgentStatus) {
    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .expect("agent row")
        .status = status;
}

fn set_agent_phase(snapshot: &mut SidebarSnapshot, phase: crate::agents::TurnPhase) {
    snapshot.worktree_groups[0].rows[0]
        .as_agent_mut()
        .expect("agent row")
        .phase = phase;
}

fn store_delta() -> SidebarEvent {
    SidebarEvent::StoreDelta {
        event_method: None,
        agent_signal: None,
    }
}

fn pane_publication(publication: crate::sidebar::events::PaneFramePublicationKind) -> SidebarEvent {
    SidebarEvent::PaneFramePublished { publication }
}

/// An armed gate whose reevaluation deadline sits `age_ms` in the past.
fn armed_gate(reject_streak: u32, age_ms: i64) -> GateState {
    let now_ms = jiff::Timestamp::now().as_millisecond();
    GateState {
        reject_streak,
        rejecting_since: Some(jiff::Timestamp::from_millisecond(now_ms - age_ms).unwrap()),
        rule: Some(crate::diag::record::GateRule::AgentDemotedToProcess),
        ..GateState::default()
    }
}

#[test]
fn spend_ratchet_holds_within_epoch_and_resets_across_epochs() {
    let mut rig = Rig::new();

    let mut high = agent_snapshot(&rig.ws);
    high.today_spend_live_usd = Some(5.0);
    high.today_spend_epoch_secs = Some(10);
    rig.fold(high, true);
    assert_eq!(rig.state.ui.spend_ratchet.display(Some(10), 5.0), 5.0);

    let mut lower = agent_snapshot(&rig.ws);
    lower.today_spend_live_usd = Some(3.0);
    lower.today_spend_epoch_secs = Some(10);
    rig.fold(lower, true);
    assert_eq!(rig.state.ui.spend_ratchet.display(Some(10), 3.0), 5.0);

    let mut older = agent_snapshot(&rig.ws);
    older.today_spend_live_usd = Some(1.0);
    rig.fold(older, true);
    assert_eq!(rig.state.ui.spend_ratchet.display(None, 1.0), 1.0);

    let mut next = agent_snapshot(&rig.ws);
    next.today_spend_live_usd = Some(2.0);
    next.today_spend_epoch_secs = Some(11);
    rig.fold(next, true);
    assert_eq!(rig.state.ui.spend_ratchet.display(Some(11), 2.0), 2.0);
}

#[test]
fn tripped_budget_ratchet_observes_the_day_spend_epoch_it_displays() {
    let mut rig = Rig::new();
    let capped = |ws: &WorkspaceId, spend_usd| {
        let mut snapshot = agent_snapshot(ws);
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

    let high = capped(&rig.ws, 5.0);
    rig.fold(high, true);
    let lower = capped(&rig.ws, 3.0);
    rig.fold(lower, true);

    let (usd, epoch) = render::cockpit_spend_target(&rig.state.current).expect("tripped spend");
    assert_eq!((usd, epoch), (3.0, Some(20)));
    assert_eq!(rig.state.ui.spend_ratchet.display(epoch, usd), 5.0);
}

#[test]
fn failed_anomaly_send_preserves_carried_drop_count() {
    // A zero-capacity channel fails every send, so this builds its own
    // `LoopState` rather than taking the rig's buffered observe channel.
    let ws = workspace();
    let dir = tempfile::TempDir::new().expect("tempdir");
    let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
    let (tx, _rx) = std::sync::mpsc::sync_channel(0);
    let mut state = LoopState::new(
        ws.clone(),
        MuxName::Tmux,
        "rimz-test".to_owned(),
        None,
        None,
        tx,
        ReadMarkStore::new(runtime, SidebarInstanceId::new()),
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
