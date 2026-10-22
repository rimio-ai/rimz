use super::*;
use crate::diag::focus_trace::{PushBlock, PushDelivery, PushPeer, PushVerdict};
use crate::sidebar_pane::app::fixtures::{
    pane, serve_config, snapshot, snapshot_with_panes, workspace,
};
use crate::sidebar_pane::app::input::KeyAction;
use crate::sidebar_pane::view::BodyFilter;
use crate::{SidebarInstanceId, WorkspaceId};
use std::collections::HashSet;

mod clock;
mod displacement;
mod fetch;
mod focus;
mod hidden;
mod maintenance;
mod paint;
mod scroll;
mod shared;

fn focus_push(
    rig: &mut Rig,
    anchor: crate::mux::focus_anchor::FocusAnchor,
) -> (FocusPush, Receiver<PushPeer>) {
    let (tx, _rx) = std::sync::mpsc::sync_channel(64);
    let _subscription = rig.focus_plane.subscribe(&rig.state.config, tx);
    let clicked = rig.state.clock.now();
    let lease = rig
        .focus_plane
        .focus_peers(SidebarInstanceId::new(), crate::diag::DiagSink::disabled())
        .arm(anchor.pane_id.clone(), clicked);
    let (_, _, _, ack, _) = lease.push(&anchor, clicked, || clicked).unwrap();
    let peers = rig.focus_plane.focus_peers(
        rig.state.config.instance_id.clone(),
        crate::diag::DiagSink::disabled(),
    );
    let push = peers.take_push().expect("priority slot delivers the push");
    rig.state.focus_peers = Some(peers);
    rig.focus_lease = Some(lease);
    (push, ack)
}

fn assert_push_report(
    ack: &Receiver<PushPeer>,
    rig: &Rig,
    verdict: PushVerdict,
    folded: bool,
) -> PushPeer {
    let report = ack
        .try_iter()
        .find(|report| report.instance_id == rig.state.config.instance_id)
        .expect("peer must report its verdict");
    assert_eq!(report.instance_id, rig.state.config.instance_id);
    assert_eq!(report.delivery, PushDelivery::Sent);
    assert_eq!(report.verdict, Some(verdict));
    let received = report.received_ms.expect("arrival stamp");
    let finished = report.finished_ms.expect("finish stamp");
    assert!(received <= finished);
    if folded {
        let snapshot = report.snapshot_ms.expect("snapshot stamp");
        let folded = report.folded_ms.expect("fold stamp");
        assert!(received <= snapshot && snapshot <= folded && folded <= finished);
    } else {
        assert_eq!(report.snapshot_ms, None);
        assert_eq!(report.folded_ms, None);
    }
    assert_eq!(
        ack.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Disconnected),
        "exactly one report, then release"
    );
    report
}

#[derive(Clone, Default)]
struct FrameOutput(
    Arc<std::sync::Mutex<Vec<u8>>>,
    Arc<std::sync::atomic::AtomicUsize>,
);

impl std::io::Write for FrameOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// One `LoopState` under test with the runtime, dispatcher, request channel,
/// and terminal it needs. Fields stay reachable: these tests drive internal
/// state by design, and the rig only removes the wiring around that reach.
struct Rig {
    _dir: tempfile::TempDir,
    ws: WorkspaceId,
    runtime: RuntimePaths,
    state: LoopState,
    result_tx: super::super::fetch::ResultSender,
    fetch: FetchDispatcher,
    requests: Receiver<FetchRequest>,
    terminal: Terminal<PaneBackend>,
    focus_plane: super::super::plane::DataPlane,
    focus_lease: Option<FocusPeers>,
    views: Arc<Mutex<super::super::views::Views>>,
    synthetic_own: bool,
}

struct RigView<'a> {
    state: &'a mut LoopState,
    views: std::sync::MutexGuard<'a, super::super::views::Views>,
    pane: Option<PaneId>,
}

impl std::ops::Deref for RigView<'_> {
    type Target = LoopState;

    fn deref(&self) -> &LoopState {
        self.state
    }
}

impl std::ops::DerefMut for RigView<'_> {
    fn deref_mut(&mut self) -> &mut LoopState {
        self.state
    }
}

impl Drop for RigView<'_> {
    fn drop(&mut self) {
        if let Some(pane) = &self.pane {
            let (_, entry) = self.views.presentation(pane, self.state.clock.now(), false);
            std::mem::swap(&mut self.state.ui.view, &mut entry.state);
            let context = entry.context.as_mut().unwrap();
            std::mem::swap(&mut self.state.view, context);
            self.state.view.current.clone_from(&context.current);
            self.state.view_scope = None;
        }
    }
}

impl Rig {
    fn share_views(&mut self, views: Arc<Mutex<super::super::views::Views>>) {
        self.views = views.clone();
        self.state.share_views(views);
    }

    /// Access portable fields without advancing, projecting, or painting the view.
    fn view(&mut self) -> RigView<'_> {
        let mut views = self.views.lock().unwrap();
        let pane = self
            .state
            .config
            .own_pane
            .clone()
            .filter(|pane| views.peek(pane).is_some());
        if let Some(pane) = &pane {
            let (id, entry) = views.presentation(pane, self.state.clock.now(), false);
            std::mem::swap(&mut self.state.ui.view, &mut entry.state);
            std::mem::swap(&mut self.state.view, entry.context.as_mut().unwrap());
            self.state.view_scope = Some(ViewScope::Shared(id));
        }
        RigView {
            state: &mut self.state,
            views,
            pane,
        }
    }

    fn observe_paint(&mut self, now: Instant) {
        let events = self.state.events().clone();
        self.state
            .paint_transition(|state| state.observe_paint(now, &events));
    }

    fn new() -> Self {
        Self::build(None, BodyLens::default())
    }

    fn without_own_pane() -> Self {
        let mut rig = Self::new();
        rig.state.config.own_pane = None;
        rig.synthetic_own = false;
        settle_width(&mut rig);
        rig
    }

    fn with_own_pane(own_pane: PaneId) -> Self {
        Self::build(Some(own_pane), BodyLens::default())
    }

    fn with_filter(filter: BodyLens) -> Self {
        Self::build(None, filter)
    }

    /// The terminal viewport width. Only the attach-resize path cares.
    fn width(mut self, cols: u16) -> Self {
        self.terminal = terminal_with_width(cols);
        self
    }

    fn build(own_pane: Option<PaneId>, filter: BodyLens) -> Self {
        Self::with_baseline(own_pane, filter, None)
    }

    fn with_baseline(own_pane: Option<PaneId>, filter: BodyLens, baseline: Option<PaneId>) -> Self {
        Self::with_clock(own_pane, filter, baseline, LoopClock::wall())
    }

    fn with_clock(
        own_pane: Option<PaneId>,
        filter: BodyLens,
        baseline: Option<PaneId>,
        clock: LoopClock,
    ) -> Self {
        let carried = baseline.is_some();
        let synthetic_own = own_pane.is_none();
        let ws = workspace();
        let dir = tempfile::tempdir_in("/tmp").expect("tempdir");
        let runtime = RuntimePaths::under(ws.clone(), dir.path()).expect("runtime");
        crate::sidebar::body_filter::write(&runtime, &filter).expect("write initial filter");
        let instance_id = SidebarInstanceId::new();
        let mut config = serve_config(&ws);
        config.mux = own_pane.as_ref().map_or(MuxName::Tmux, PaneId::mux);
        config.own_pane = own_pane.or_else(|| Some(PaneId::from_parts(MuxName::Tmux, "%99")));
        config.instance_id = instance_id;
        config.tick_seconds = 60;
        let socket_path = runtime.sidebar_host_wake_socket_path(config.mux, &config.session_name);
        let (observe_tx, _observe_rx) = std::sync::mpsc::sync_channel(64);
        let (request_tx, requests) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = super::super::fetch::result_channel();
        let views = Arc::new(Mutex::new(super::super::views::Views::default()));
        let mut rig = Self {
            state: LoopState::new(
                clock,
                config,
                baseline,
                runtime.clone(),
                socket_path,
                crate::diag::DiagSink::disabled(),
                result_rx,
                None,
                observe_tx,
                PixelRenderCaps::default(),
                Some(crate::sidebar_pane::pixel::PixelSlot::new(0)),
            ),
            result_tx,
            fetch: FetchDispatcher::new(request_tx),
            requests,
            terminal: terminal_with_width(80),
            focus_plane: super::super::plane::DataPlane::detached(),
            focus_lease: None,
            views: views.clone(),
            synthetic_own,
            runtime,
            ws,
            _dir: dir,
        };
        rig.share_views(views);
        rig.state.with_shared_view(false, false, |_| {});
        if carried {
            rig.state
                .draw_frame(&mut rig.terminal, rig.state.clock.now(), false)
                .unwrap();
        }
        rig
    }

    fn record_event(&self, envelope: &SidebarEventEnvelope) {
        super::super::plane::RoomEvents::new(
            &self.runtime,
            self.views.clone(),
            self.state.events.clone(),
            super::super::fetch::Subscribers::default(),
        )
        .record(envelope, self.state.clock.now_ms());
    }

    fn event(&mut self, event: SidebarEvent) {
        // An external sender's wall clock is independent of the receiver's virtual time.
        let envelope = SidebarEventEnvelope::new(
            self.ws.clone(),
            Some("rimz-test".to_owned()),
            crate::utils::time::unix_now_ms(),
            event,
        );
        self.record_event(&envelope);
        self.state
            .on_event(&mut self.fetch, &mut self.terminal, envelope);
    }

    fn fold(&mut self, snapshot: SidebarSnapshot, source: SnapshotSource) {
        self.deliver(FetchUpdate::Snapshot {
            snapshot: Box::new(snapshot),
            phase: FetchPhase::Final,
            source,
        });
    }

    fn shared_fold(
        &mut self,
        snapshot: SidebarSnapshot,
        inputs: Arc<super::super::fetch::FoldInputs>,
    ) {
        self.deliver(FetchUpdate::Shared {
            update: Box::new(FetchUpdate::Snapshot {
                snapshot: Box::new(snapshot),
                phase: FetchPhase::Final,
                source: SnapshotSource::Produced,
            }),
            context: Arc::new(super::super::fetch::FoldShared {
                inputs,
                ..Default::default()
            }),
        });
    }

    /// Drive `on_snapshot` over a channel carrying exactly `update`.
    fn deliver(&mut self, update: FetchUpdate) {
        self.send(update).expect("send fetch outcome");
        self.state.on_snapshot(&mut self.fetch);
    }

    fn send(
        &mut self,
        mut update: FetchUpdate,
    ) -> std::result::Result<(), std::sync::mpsc::SendError<FetchUpdate>> {
        let local = match &mut update {
            FetchUpdate::Shared { update, .. } => update.as_mut(),
            update => update,
        };
        let fold = match local {
            FetchUpdate::Fold { fold, .. } => Some(fold.clone()),
            FetchUpdate::Snapshot { snapshot, .. } => {
                let synthesized = self.synthetic_own && snapshot.own_view.is_none();
                if synthesized {
                    snapshot.own_view = Some(SidebarOwnView {
                        sibling_count: snapshot.rows().count(),
                        working_pane_ids: snapshot
                            .rows()
                            .flat_map(|row| row.pane_ids())
                            .cloned()
                            .collect(),
                        own_view_is_daemon: false,
                    });
                }
                let frame = snapshot.own_view.as_ref().map(|view| {
                    let mut panes: Vec<_> =
                        snapshot.rows().filter_map(|row| row.pane.clone()).collect();
                    let tab = panes
                        .iter()
                        .find(|pane| view.working_pane_ids.contains(&pane.pane_id))
                        .and_then(|pane| pane.view_id.clone())
                        .unwrap_or_else(|| "tab_sidebar".into());
                    // The own view names the sidebar's tab; fixture rows default every pane onto
                    // one tab, so move the rest off it.
                    for other in panes
                        .iter_mut()
                        .filter(|pane| !view.working_pane_ids.contains(&pane.pane_id))
                        .filter(|pane| pane.view_id.as_deref() == Some(tab.as_str()))
                    {
                        other.view_id = Some(format!("{tab}_other"));
                    }
                    for id in &view.working_pane_ids {
                        if !panes.iter().any(|pane| pane.pane_id == *id) {
                            let mut working = pane(id.raw(), &tab, false);
                            working.pane_id = id.clone();
                            panes.push(working);
                        }
                    }
                    // The plane samples one room frame: it keeps every other attached sidebar
                    // where the last fold placed it.
                    let views = self.views.lock().unwrap();
                    let previous = views.fold.as_ref().and_then(|fold| fold.frame.clone());
                    let sidebars = previous.iter().flat_map(|frame| {
                        frame.tabs.iter().flat_map(|tab| {
                            tab.panes
                                .iter()
                                .filter(|pane| views.peek(&pane.pane_id).is_some())
                                .map(|pane| (pane.pane_id.clone(), tab.view_id.to_string()))
                        })
                    });
                    let own = self.state.config.own_pane.clone();
                    let own = own.into_iter().map(|own| (own, tab.clone()));
                    for (id, tab) in own.chain(sidebars) {
                        if !panes.iter().any(|pane| pane.pane_id == id) {
                            let mut sidebar = pane(id.raw(), &tab, false);
                            sidebar.pane_id = id;
                            sidebar.command = Some("rimz-sidebar".into());
                            panes.push(sidebar);
                        }
                    }
                    drop(views);
                    let mut frame = crate::sidebar::frame::assemble_frame(
                        panes,
                        snapshot.panes_observed_at_ms.unwrap_or(1),
                        "rimz-test",
                    );
                    frame.client_views = snapshot.client_views.clone();
                    Arc::new(frame)
                });
                let mut workspace = *snapshot.clone();
                // A fixture that names no own view leaves visibility unknown, which reads as
                // watched; the plane observes that through the sidebar's own pane being viewed.
                if synthesized && let Some(own) = self.state.config.own_pane.clone() {
                    workspace.viewed_panes.push(own);
                }
                Some(Arc::new(WorkspaceFold {
                    workspace: crate::sidebar::enrich::WorkspaceSnapshot(workspace),
                    frame,
                }))
            }
            _ => None,
        };
        let mut views = self.views.lock().unwrap();
        let own = self.state.config.own_pane.as_ref();
        let attached =
            fold.as_ref()
                .and_then(|fold| fold.frame.as_ref())
                .is_some_and(|frame| {
                    frame.tabs.iter().flat_map(|tab| &tab.panes).any(|pane| {
                        Some(&pane.pane_id) != own && views.peek(&pane.pane_id).is_some()
                    })
                });
        // Beside another attachment the plane retains the unprojected fold, as production does,
        // so each projects its own view. A lone rig retains what it delivers.
        let retained = match (&update, &fold) {
            (FetchUpdate::Snapshot { phase, source, .. }, Some(fold)) if attached => {
                FetchUpdate::Fold {
                    fold: fold.clone(),
                    own_pane: None,
                    refresh_override: None,
                    phase: *phase,
                    source: *source,
                }
            }
            _ => update.clone(),
        };
        views.publish(fold, retained);
        drop(views);
        self.result_tx.send(update)
    }

    /// A maintenance sweep over an idle result channel. The heartbeat and
    /// self-close stamps start fresh; a test wanting an expired watchdog
    /// backdates `last_self_close_check` before calling.
    fn maintenance(&mut self) {
        self.maintenance_draining(None);
    }

    fn maintenance_draining(&mut self, update: Option<FetchUpdate>) {
        if let Some(update) = update {
            self.send(update).expect("send fetch outcome");
        }
        self.state.last_heartbeat.get_or_insert_with(Instant::now);
        self.state
            .run_maintenance(&mut self.fetch, &mut self.terminal);
    }

    fn input(&mut self, action: KeyAction) {
        self.state
            .on_input(Wakeup::Key(action), &mut self.terminal, &mut self.fetch)
            .expect("input");
    }

    fn paint(&mut self, active: bool) {
        self.state
            .paint_frame_if_due(&mut self.terminal, active)
            .expect("paint");
    }

    fn step(&mut self, inbox: &mut WakeupInbox) {
        self.state
            .step(inbox, &mut self.fetch, &mut self.terminal)
            .expect("step");
    }

    fn next_request(&mut self) -> Option<FetchRequest> {
        self.requests.try_recv().ok()
    }

    /// Park the renderer off screen with animating content and no viewed pane.
    fn hide(&mut self) {
        self.view().current = animating_agent_snapshot(&self.ws);
        self.view().current.own_view = Some(own_view());
        self.view().current.viewed_panes.clear();
        let pulled = self.state.current.clone();
        self.set_pulled(&pulled);
        self.set_plane_viewed(false);
    }

    /// Bring the renderer back on screen by viewing a sibling pane.
    fn watch(&mut self) {
        self.view().current.viewed_panes = vec![pane("terminal_9", "tab_0", false).pane_id];
        self.set_plane_viewed(true);
    }

    /// Point the plane's latest fold at the sidebar's tab being viewed or not.
    fn set_plane_viewed(&mut self, viewed: bool) {
        let own = self.state.config.own_pane.clone();
        let mut views = self.views.lock().unwrap();
        let Some(fold) = views.fold.as_mut() else {
            return;
        };
        let viewed_panes = &mut Arc::make_mut(fold).workspace.0.viewed_panes;
        viewed_panes.clear();
        viewed_panes.extend(own.filter(|_| viewed));
    }

    fn set_pulled(&mut self, snapshot: &SidebarSnapshot) {
        self.view().last_focus_observation = FocusObservation::from_snapshot(snapshot);
        self.view().overlay_baseline = Some(snapshot.clone());
    }

    fn frame_active(&mut self) -> bool {
        self.state.config.tick_seconds = 10;
        self.state.frame_timing().0
    }
}

fn terminal_with_width(cols: u16) -> Terminal<PaneBackend> {
    let viewport = ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, cols, 24));
    Terminal::with_options(
        PaneBackend::headless(std::io::sink()),
        ratatui::TerminalOptions { viewport },
    )
    .expect("terminal")
}

fn settle_width(rig: &mut Rig) {
    // A test that starts after width admission, not at the initial probe every own pane arms.
    rig.state.width_control = WidthController::new(
        rig.runtime.clone(),
        rig.state.config.session_name.clone(),
        None,
        rig.state.config.mux,
        crate::mux::SidebarWidth::from_config(&rig.state.current.theme),
    );
}

fn own_view() -> crate::store::snapshot::SidebarOwnView {
    crate::store::snapshot::SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![pane("terminal_9", "tab_0", false).pane_id],
        own_view_is_daemon: false,
    }
}

fn empty_own_view() -> crate::store::snapshot::SidebarOwnView {
    crate::store::snapshot::SidebarOwnView {
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

fn agent_snapshot(ws: &WorkspaceId) -> SidebarSnapshot {
    let mut snapshot = crate::sidebar_pane::app::fixtures::agent_snapshot(ws);
    let working_pane_ids: Vec<_> = snapshot
        .rows()
        .flat_map(|row| row.pane_ids())
        .cloned()
        .collect();
    snapshot.viewed_panes = working_pane_ids.clone();
    snapshot.own_view = Some(SidebarOwnView {
        sibling_count: working_pane_ids.len(),
        working_pane_ids,
        own_view_is_daemon: false,
    });
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
    snapshot.own_view = Some(crate::store::snapshot::SidebarOwnView {
        sibling_count: 2,
        working_pane_ids,
        own_view_is_daemon: false,
    });
    snapshot.focused_pane = Some(active);
    snapshot.viewed_panes = snapshot.own_view.as_ref().unwrap().working_pane_ids.clone();
    snapshot
}

fn set_agent_status(snapshot: &mut SidebarSnapshot, status: crate::agents::AgentStatus) {
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .as_agent_mut()
    .expect("agent row")
    .status = status;
}

fn set_agent_phase(snapshot: &mut SidebarSnapshot, phase: crate::agents::TurnPhase) {
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
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
    rig.fold(high, SnapshotSource::Produced);
    assert_eq!(rig.view().ui.spend_ratchet.display(Some(10), 5.0), 5.0);

    let mut lower = agent_snapshot(&rig.ws);
    lower.today_spend_live_usd = Some(3.0);
    lower.today_spend_epoch_secs = Some(10);
    rig.fold(lower, SnapshotSource::Produced);
    assert_eq!(rig.view().ui.spend_ratchet.display(Some(10), 3.0), 5.0);

    let mut older = agent_snapshot(&rig.ws);
    older.today_spend_live_usd = Some(1.0);
    rig.fold(older, SnapshotSource::Produced);
    assert_eq!(rig.view().ui.spend_ratchet.display(None, 1.0), 1.0);

    let mut next = agent_snapshot(&rig.ws);
    next.today_spend_live_usd = Some(2.0);
    next.today_spend_epoch_secs = Some(11);
    rig.fold(next, SnapshotSource::Produced);
    assert_eq!(rig.view().ui.spend_ratchet.display(Some(11), 2.0), 2.0);
}

#[test]
fn tripped_budget_ratchet_observes_the_day_spend_epoch_it_displays() {
    let mut rig = Rig::new();
    let capped = |ws: &WorkspaceId, spend_usd| {
        let mut snapshot = agent_snapshot(ws);
        snapshot.today_spend_live_usd = Some(1.0);
        snapshot.today_spend_epoch_secs = Some(10);
        snapshot.fleet_day_spend_epoch_secs = Some(20);
        snapshot.fleet_budget = Some(crate::store::snapshot::DailyBudgetView {
            cap_usd: 10.0,
            spend_usd,
            parked: true,
        });
        snapshot
    };

    let high = capped(&rig.ws, 5.0);
    rig.fold(high, SnapshotSource::Produced);
    let lower = capped(&rig.ws, 3.0);
    rig.fold(lower, SnapshotSource::Produced);

    let (usd, epoch) = render::cockpit_spend_target(&rig.state.current).expect("tripped spend");
    assert_eq!((usd, epoch), (3.0, Some(20)));
    assert_eq!(rig.view().ui.spend_ratchet.display(epoch, usd), 5.0);
}
