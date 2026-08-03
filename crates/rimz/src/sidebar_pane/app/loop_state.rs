use std::collections::HashSet;

use jiff::Timestamp;

use super::fetch::{FetchPhase, FetchRole, FetchUpdate, PaneFrame, SnapshotSource};
use super::gate::{apply_gate, gate_remaining};
use super::health::degraded_too_long;
use super::lifecycle::{grow_beyond_legit, self_close_decision};
use super::paint::FramePainter;
use super::reload::{ReloadAction, reload_action};
use super::remind::RemindState;
use super::selection::{reconcile_selection, row_index_of_pane, set_make_up_filter};
use super::state::{
    ApplyOutcome, FetchDiagnostics, ReadClear, RenderState, apply_manual_unread_guard,
    compute_next_state, emit_diagnostics, emit_unread_cleared_trace, read_receipt_for_row,
    read_receipts_for_all, read_receipts_for_tab, row_id_of_pane, session_focus_baseline,
    set_rows_unread,
};
use super::width_control::WidthController;
use super::*;
use crate::diag::record::{RendererExitCause, SidebarWidthControlTrigger as WidthControlTrigger};
use crate::observability::SIDEBAR_HEALTH_TARGET;
use crate::sidebar::read_marks::{ReadMarkStore, ReadMarks, write_manual_read_marks};
use crate::sidebar::unread::{self, UnreadClearCause};
use crate::sidebar_pane::pixel::PixelRenderCaps;
use crate::sidebar_pane::view::BodyFilter;
use crate::store::snapshot::{ProcessState, RowCard, SidebarOwnView, SidebarPresence, SidebarRow};

pub(super) struct MaintenanceContext<'a> {
    pub(super) config: &'a ServeConfig,
    pub(super) runtime: &'a RuntimePaths,
    pub(super) socket_path: &'a Path,
    pub(super) result_rx: &'a Receiver<FetchUpdate>,
    pub(super) anim_start: Instant,
    pub(super) diag: &'a crate::diag::DiagSink,
    pub(super) tick: Duration,
}

/// Compact projection of the glanceable sidebar content an off-screen pane
/// keeps fresh. It deliberately skips animation state, turn phase, gauges,
/// process metrics, spend, and git facts.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BackgroundContentKey {
    groups: Vec<BackgroundGroupKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BackgroundGroupKey {
    key: String,
    rows: Vec<BackgroundRowKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BackgroundRowKey {
    id: String,
    unread: bool,
    inactive: bool,
    status: BackgroundRowStatusKey,
}

struct FetchApplication {
    snapshot: std::result::Result<SidebarSnapshot, String>,
    role: FetchRole,
    source: Option<SnapshotSource>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackgroundRowStatusKey {
    Agent(crate::agents::AgentStatus),
    Process(ProcessState),
}

fn background_content_key(snapshot: &SidebarSnapshot) -> BackgroundContentKey {
    BackgroundContentKey {
        groups: snapshot
            .worktree_groups
            .iter()
            .map(|group| BackgroundGroupKey {
                key: group.key.clone(),
                rows: group
                    .rows
                    .iter()
                    .map(|row| BackgroundRowKey {
                        id: row.id.clone(),
                        unread: row.unread,
                        inactive: row.inactive,
                        status: row_status_key(row),
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn row_status_key(row: &SidebarRow) -> BackgroundRowStatusKey {
    match &row.card {
        RowCard::Agent(card) => BackgroundRowStatusKey::Agent(card.status),
        RowCard::Process(card) => BackgroundRowStatusKey::Process(card.state),
    }
}

fn own_tab_viewed(
    snapshot: &SidebarSnapshot,
    own_view: &SidebarOwnView,
    own_pane: &PaneId,
) -> bool {
    snapshot
        .viewed_panes
        .iter()
        .any(|pane| pane == own_pane || own_view.working_pane_ids.contains(pane))
}

#[derive(Clone)]
struct PendingFocusRepair {
    pane_id: PaneId,
    generation: u64,
    clients: Vec<crate::mux::ClientPaneView>,
    sent_at_ms: u64,
}

pub(super) struct LoopState {
    pub(super) current: SidebarSnapshot,
    session_name: String,
    /// Full pulled truth retained only while an overlay, focus fence, or gate
    /// hold can outlive its source. The steady overlay-free path moves the pull
    /// directly into `current` and keeps only the compact projections below.
    overlay_baseline: Option<SidebarSnapshot>,
    last_focus_observation: FocusObservation,
    last_pulled_sig: observe::PulledFrameSig,
    own_pane: Option<PaneId>,
    last_known_elder: bool,
    optimistic_watch_until: Option<Instant>,
    /// Deadline for the tab-view read sweep: armed when the own tab comes on
    /// screen, disarmed when it leaves. The sweep fires on the first fold at or
    /// past it while the tab is still viewed, so a pass-through never clears
    /// siblings. `None` when no dwell is pending.
    pub(super) tab_read_dwell_until: Option<Instant>,
    event_store: EventStore,
    pending_focus_repair: Option<PendingFocusRepair>,
    confirmed_focus_intent_ms: u64,
    observer: observe::Observer,
    observe_tx: SyncSender<ObserveMsg>,
    pub(super) health: Health,
    gate: GateState,
    self_close: SelfCloseState,
    pub(super) ui: UiState,
    paint: FramePainter,
    pub(super) read_marks: ReadMarkStore,
    remind: RemindState,
    dirty: bool,
    paint_hold: PaintHold,
    next_frame: Instant,
    fetched_at: Instant,
    last_bg_paint: Option<Instant>,
    last_bg_key: Option<BackgroundContentKey>,
    last_self_close_check: Instant,
    last_heartbeat: Option<Instant>,
    prev_width: Option<u16>,
    width_control: WidthController,
    pub(super) should_exit: bool,
    pub(super) exit_cause: Option<RendererExitCause>,
    pub(super) tab_emptied: bool,
    pub(super) reload_requested: bool,
}

pub(super) fn handle_wakeup(
    wakeup: Wakeup,
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
) -> InputOutcome {
    // The help popup is a transient modal: while it is up, the next
    // interaction dismisses it and is consumed, never also acting on the
    // sidebar beneath.
    if ui.help_visible
        && matches!(
            wakeup,
            Wakeup::ReloadKey | Wakeup::Key(_) | Wakeup::MouseClick { .. } | Wakeup::Scroll { .. }
        )
    {
        ui.help_visible = false;
        return InputOutcome::redraw();
    }
    match wakeup {
        Wakeup::Key(action) => handle_key(action, ui, snapshot),
        Wakeup::MouseClick { column, row } => handle_mouse_click(column, row, ui, snapshot),
        Wakeup::Scroll { down } => handle_scroll(down, ui),
        Wakeup::Resize => InputOutcome::redraw(),
        // The serve loop intercepts these before dispatching here: a tick, a
        // typed sidebar event is a re-fetch trigger, worker completions are
        // folded, and a reload re-execs.
        Wakeup::Tick
        | Wakeup::Event(_)
        | Wakeup::Reload
        | Wakeup::SupervisorHandoff
        | Wakeup::ReloadKey
        | Wakeup::Snapshot => InputOutcome::default(),
    }
}

impl LoopState {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        workspace_id: WorkspaceId,
        mux: MuxName,
        session_name: String,
        own_pane: Option<PaneId>,
        initial_width: Option<u16>,
        observe_tx: SyncSender<ObserveMsg>,
        read_marks: ReadMarkStore,
        pet_render_caps: PixelRenderCaps,
        pixel_wrap: bool,
    ) -> Self {
        let snapshot_now = Timestamp::now();
        let current = SidebarSnapshot::build_with_agents(workspace_id, Vec::new(), snapshot_now);
        let now = Instant::now();
        let width = crate::mux::SidebarWidth::from_config(
            &crate::config::MachineConfig::load_lenient().theme,
        );
        let width_control = WidthController::new(
            read_marks.runtime().clone(),
            session_name.clone(),
            own_pane.clone(),
            mux,
            width,
        );
        let make_up_filter = crate::sidebar::body_filter::load(read_marks.runtime());
        Self {
            session_name,
            last_focus_observation: FocusObservation::from_snapshot(&current),
            last_pulled_sig: observe::PulledFrameSig::from_snapshot(&current),
            overlay_baseline: None,
            current,
            own_pane,
            last_known_elder: true,
            optimistic_watch_until: None,
            tab_read_dwell_until: None,
            event_store: EventStore::default(),
            pending_focus_repair: None,
            confirmed_focus_intent_ms: 0,
            observer: observe::Observer::default(),
            observe_tx,
            health: Health::default(),
            gate: GateState::default(),
            self_close: SelfCloseState::default(),
            ui: UiState {
                make_up_filter,
                ..UiState::default()
            },
            paint: FramePainter::new(pet_render_caps, pixel_wrap),
            read_marks,
            remind: RemindState::default(),
            dirty: true,
            paint_hold: PaintHold::default(),
            next_frame: now,
            fetched_at: now,
            last_bg_paint: None,
            last_bg_key: None,
            last_self_close_check: now,
            last_heartbeat: None,
            prev_width: initial_width,
            width_control,
            should_exit: false,
            exit_cause: None,
            tab_emptied: false,
            reload_requested: false,
        }
    }

    pub(super) fn frame_timing(&self, tick: Duration, anim_start: Instant) -> (bool, Duration) {
        let phase = wall_clock_phase(anim_start, self.current.theme.display.resolved_refresh_ms());
        let alert_active = self.alert_active();
        let watched = self.watched();
        let animating = is_animating(&self.current, &self.ui, phase, alert_active);
        let active = !self.self_close.confirming_empty()
            && ((watched && animating) || (self.dirty && self.dirty_paintable(watched)));
        let mut timeout = if active {
            self.next_frame
                .saturating_duration_since(Instant::now())
                .max(FRAME_MIN_TIMEOUT)
        } else {
            // Cap by the watchdog so the self-close backstop fires on time even
            // when the data tick is much longer. Also wake at order-hold expiry:
            // a fold must run to release the frozen row/group order after idle.
            let watchdog_due =
                SELF_CLOSE_WATCHDOG.saturating_sub(self.last_self_close_check.elapsed());
            let mut timeout = tick.min(watchdog_due);
            if let Some(hold) = self.ui.order_hold.as_ref() {
                let now_ms = jiff::Timestamp::now().as_millisecond();
                let remaining = Duration::from_millis((hold.expires_ms - now_ms).max(0) as u64);
                timeout = timeout.min(remaining);
            }
            timeout.max(FRAME_MIN_TIMEOUT)
        };
        if let Some(until) = self
            .tab_read_dwell_until
            .filter(|_| self.current.own_view.is_some())
        {
            timeout = timeout.min(
                until
                    .saturating_duration_since(Instant::now())
                    .max(FRAME_MIN_TIMEOUT),
            );
        }
        if let Some(deadline) = self.width_control.feedback_deadline() {
            timeout = timeout.min(
                deadline
                    .saturating_duration_since(Instant::now())
                    .max(FRAME_MIN_TIMEOUT),
            );
        }
        if let Some(remaining) = gate_remaining(&self.gate, jiff::Timestamp::now()) {
            timeout = timeout.min(remaining.max(FRAME_MIN_TIMEOUT));
        }
        (active, timeout)
    }

    fn alert_active(&self) -> bool {
        self.health
            .alert
            .as_ref()
            .is_some_and(render::Alert::is_active)
    }

    /// Whether an attached client's focus currently lands in this sidebar's tab.
    /// Unknown ownership or own-view state reads as watched so uncertainty never
    /// suppresses motion.
    fn watched(&self) -> bool {
        if self
            .optimistic_watch_until
            .is_some_and(|until| Instant::now() < until)
        {
            return true;
        }
        let Some(own_pane) = self.own_pane.as_ref() else {
            return true;
        };
        let Some(view) = self.current.own_view.as_ref() else {
            return true;
        };
        own_tab_viewed(&self.current, view, own_pane)
    }

    /// Whether a dirty data fold should paint even though animation may be idle.
    /// Suppress dirty frames only when an attached client is known to be looking
    /// elsewhere. Detached sessions have no terminal stream to spam, and keeping
    /// their pane buffer current makes attach and `capture-pane` land on the
    /// latest frame.
    fn dirty_paintable(&self, watched: bool) -> bool {
        watched
            || !matches!(
                self.current.presence,
                Some(SidebarPresence::Active | SidebarPresence::Idle { .. })
            )
    }

    fn identity_free_fetch_immediate(&self) -> bool {
        self.watched() || self.last_known_elder
    }

    pub(super) fn on_snapshot(
        &mut self,
        config: &ServeConfig,
        fetch: &mut FetchDispatcher,
        result_rx: &Receiver<FetchUpdate>,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) {
        let mut latest = None;
        let mut saw_final = false;
        while let Ok(update) = result_rx.try_recv() {
            saw_final |= update.is_final();
            latest = Some(update);
        }
        let rejected = match latest {
            Some(update) => self.apply_latest_snapshot(config, update, anim_start, diag),
            None => false,
        };
        if saw_final {
            fetch.complete(!self.should_exit);
        }
        // A held transient regression schedules one reevaluation at the gate
        // deadline. Eager finals can otherwise feed themselves through the
        // fast fold and pin the renderer to its 1 ms gate wakeup.
        if !self.should_exit && saw_final && rejected {
            let defer_for = gate_remaining(&self.gate, jiff::Timestamp::now())
                .unwrap_or(crate::sidebar::timing::ACCEPT_REGRESSION_AFTER);
            fetch.defer_until(FetchRequest::default(), Instant::now() + defer_for);
        }
    }

    // ponytail: flat dispatch inputs stay explicit; bundle loop context if this set grows.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn on_wakeup(
        &mut self,
        config: &ServeConfig,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        result_rx: &Receiver<FetchUpdate>,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
        wakeup: Wakeup,
    ) -> Result<LoopFlow> {
        match wakeup {
            Wakeup::Snapshot => {
                self.on_snapshot(config, fetch, result_rx, anim_start, diag);
                Ok(LoopFlow::Continue)
            }
            Wakeup::Event(envelope) => {
                Ok(self.on_event(config, fetch, terminal, envelope, anim_start, diag))
            }
            // A recv timeout: the active grid reached a frame boundary, or the
            // idle backstop interval elapsed. It carries no state of its own —
            // the frame phase below advances the spin and paints, and the
            // backstop poll runs there too.
            Wakeup::Tick => {
                if self.paint.refresh_caps_if_stale(
                    config.mux,
                    &config.session_name,
                    Instant::now(),
                ) {
                    self.dirty = true;
                }
                Ok(LoopFlow::Continue)
            }
            Wakeup::Resize => {
                let settled_width = terminal.size().map(|s| s.width).ok();
                self.on_resize(config, fetch, terminal, settled_width, anim_start, diag)?;
                Ok(LoopFlow::Continue)
            }
            Wakeup::Reload => Ok(self.handle_reload(config, fetch)),
            Wakeup::SupervisorHandoff => {
                self.reload_requested = true;
                Ok(LoopFlow::Exit)
            }
            // The local `r` key uses a key-specific wakeup so the help overlay
            // can close on the keypress before it reaches the reload path.
            Wakeup::ReloadKey if self.ui.help_visible => {
                self.on_input(config, Wakeup::ReloadKey, terminal, fetch, anim_start, diag)?;
                Ok(LoopFlow::Continue)
            }
            Wakeup::ReloadKey => Ok(self.handle_reload(config, fetch)),
            wakeup => {
                self.on_input(config, wakeup, terminal, fetch, anim_start, diag)?;
                Ok(LoopFlow::Continue)
            }
        }
    }

    fn handle_reload(&mut self, config: &ServeConfig, fetch: &mut FetchDispatcher) -> LoopFlow {
        fetch.clear_deferred();
        if reload_or_refetch(&config.workspace_id, &config.session_name, fetch) {
            self.reload_requested = true;
            return LoopFlow::Exit;
        }
        LoopFlow::Continue
    }

    fn apply_latest_snapshot(
        &mut self,
        config: &ServeConfig,
        update: FetchUpdate,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> bool {
        self.last_known_elder = update.role().is_producer();
        if matches!(update, FetchUpdate::Unchanged { .. }) {
            self.fetched_at = Instant::now();
            return false;
        }
        let snapshot_ok = matches!(update, FetchUpdate::Snapshot { .. });
        let fresh_pane_frame = update.pane_frame() == PaneFrame::Fresh;
        let update = match update {
            FetchUpdate::Snapshot {
                snapshot,
                role,
                phase,
                pane_frame,
                source,
            } => {
                let now_ms = crate::sidebar::timing::unix_now_ms();
                self.event_store.prune(now_ms);
                let pulled = *snapshot;
                self.last_focus_observation = FocusObservation::from_snapshot(&pulled);
                self.last_pulled_sig = observe::PulledFrameSig::from_snapshot(&pulled);
                let intent = self.pending_focus_intent(now_ms);
                let (snapshot, baseline) =
                    fuse_owned(pulled, &self.event_store, intent.as_ref(), now_ms);
                self.overlay_baseline = baseline;
                FetchUpdate::Snapshot {
                    snapshot: Box::new(snapshot),
                    role,
                    phase,
                    pane_frame,
                    source,
                }
            }
            failed @ FetchUpdate::Failed { .. } => failed,
            FetchUpdate::Unchanged { .. } => unreachable!("handled above"),
        };
        self.fetched_at = Instant::now();
        let rejected = self.fold_outcome(config, update, true, anim_start, diag);
        if snapshot_ok {
            self.last_self_close_check = Instant::now();
            self.retry_pending_focus_repair(config);
        }
        self.release_paint_hold_after_snapshot(rejected, fresh_pane_frame);
        rejected
    }

    fn release_paint_hold_after_snapshot(&mut self, rejected: bool, fresh_pane_frame: bool) {
        if !self.should_exit
            && !rejected
            && !self.self_close.confirming_empty()
            && (fresh_pane_frame
                || self
                    .paint_hold
                    .releases_on_stamp(self.current.panes_observed_at_ms))
        {
            // The snapshot folded a post-signal pane frame. Its own-view
            // verdict has decided the resize-grow case: exit without
            // painting when alone, or release the hold and paint at the new
            // size when siblings remain.
            self.paint_hold.release();
        }
    }

    /// Fuse the last pulled snapshot with the overlay event store and any
    /// pending focus intent as of `now_ms`.
    fn fused_snapshot(&mut self, now_ms: u64) -> SidebarSnapshot {
        if self.overlay_baseline.is_none() {
            self.overlay_baseline = Some(self.current.clone());
        }
        let intent = self.pending_focus_intent(now_ms);
        fuse(
            self.overlay_baseline
                .as_ref()
                .expect("overlay baseline installed"),
            &self.event_store,
            intent.as_ref(),
            now_ms,
        )
    }

    /// Fold a synthetic fused frame and snap the frame deadline so this
    /// turn's frame phase paints it now.
    fn fold_fused_now(
        &mut self,
        config: &ServeConfig,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) {
        let fused = self.fused_snapshot(crate::sidebar::timing::unix_now_ms());
        self.fold_outcome(
            config,
            FetchUpdate::Snapshot {
                snapshot: Box::new(fused),
                phase: FetchPhase::Interim,
                pane_frame: PaneFrame::Held,
                source: SnapshotSource::Published,
                role: if self.last_known_elder {
                    FetchRole::Producer
                } else {
                    FetchRole::Consumer
                },
            },
            false,
            anim_start,
            diag,
        );
        self.next_frame = Instant::now();
    }

    pub(super) fn on_event(
        &mut self,
        config: &ServeConfig,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        envelope: SidebarEventEnvelope,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> LoopFlow {
        if !event_targets_this_renderer(&envelope, config) {
            return LoopFlow::Repoll;
        }
        let requests_verification = envelope.event.requests_producer_verification();
        let sent_at_ms = envelope.sent_at_ms;
        if matches!(
            &envelope.event,
            SidebarEvent::PaneClosed { .. }
                | SidebarEvent::PaneOpened { .. }
                | SidebarEvent::PanesChanged
        ) {
            let measured = terminal.size().ok().map(|size| size.width);
            self.width_control
                .note_structural(sent_at_ms, measured, diag);
        }
        match envelope.event {
            SidebarEvent::Reload => {
                return self.handle_reload(config, fetch);
            }
            SidebarEvent::WidthTargetChanged => {
                let measured = terminal.size().ok().map(|size| size.width);
                self.width_control
                    .reload_target(&self.current.theme, measured, diag);
            }
            SidebarEvent::BodyFilterChanged => {
                let filter = crate::sidebar::body_filter::load(self.read_marks.runtime());
                if set_make_up_filter(&mut self.ui, &self.current, filter) {
                    self.dirty = true;
                }
            }
            // A watched renderer and the producer fold every publication now.
            // Hidden consumers coalesce topology and metrics to the cadence of
            // the changed input, while presence remains immediate because it
            // establishes whether the tab is watched.
            SidebarEvent::PaneFramePublished { publication } => {
                use crate::sidebar::events::PaneFramePublicationKind;

                match publication {
                    PaneFramePublicationKind::Presence => {
                        fetch.request(FetchRequest::pane_frame_published(), true)
                    }
                    PaneFramePublicationKind::Topology => fetch.request_or_defer(
                        FetchRequest::pane_frame_published(),
                        self.identity_free_fetch_immediate(),
                        crate::sidebar::timing::UNWATCHED_FOLD_CLAMP,
                    ),
                    PaneFramePublicationKind::Metrics => fetch.request_or_defer(
                        FetchRequest::pane_frame_published(),
                        self.identity_free_fetch_immediate(),
                        crate::sidebar::timing::UNWATCHED_METRICS_FOLD_CLAMP,
                    ),
                }
            }
            SidebarEvent::StoreDelta { .. } => {
                let request = if requests_verification {
                    FetchRequest::producer_fresh_panes()
                } else {
                    FetchRequest::default()
                };
                fetch.request_or_defer(
                    request,
                    self.identity_free_fetch_immediate(),
                    crate::sidebar::timing::UNWATCHED_FOLD_CLAMP,
                )
            }
            event @ SidebarEvent::Notify { .. } => {
                self.handle_notification(config, terminal, event, diag);
            }
            SidebarEvent::FocusStranded {
                pane_id,
                generation,
                clients,
            } => {
                let repair = PendingFocusRepair {
                    pane_id,
                    generation,
                    clients,
                    sent_at_ms,
                };
                if !self.handle_focus_stranded(config, &repair) {
                    if let Some(replaced) = self.pending_focus_repair.replace(repair) {
                        self.record_abandoned_focus_repair(
                            config,
                            &replaced,
                            "superseded by a newer strand",
                        );
                    }
                    fetch.request(FetchRequest::producer_fresh_panes(), true);
                }
            }
            SidebarEvent::FocusIntent { .. } => {
                self.fold_fused_now(config, anim_start, diag);
            }
            event if event.is_overlay() => {
                self.handle_overlay_event(config, fetch, event, sent_at_ms, anim_start, diag);
            }
            // Identity-free nudges — `PanesChanged`, a `PaneOpened` without a
            // command: nothing to fuse, so refetch,
            // bypassing the pane cache when the event says topology moved.
            _ => {
                fetch.request_or_defer(
                    if requests_verification {
                        FetchRequest::producer_fresh_panes()
                    } else {
                        FetchRequest::default()
                    },
                    self.identity_free_fetch_immediate(),
                    crate::sidebar::timing::UNWATCHED_FOLD_CLAMP,
                );
            }
        }
        LoopFlow::Continue
    }

    fn handle_notification(
        &mut self,
        config: &ServeConfig,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        event: SidebarEvent,
        diag: &crate::diag::DiagSink,
    ) {
        // `on_event` calls this helper only from the typed notification arm.
        let SidebarEvent::Notify {
            title,
            body,
            panes,
            recheck_unread,
            notification_kind,
        } = event
        else {
            unreachable!("notification helper received a non-notification event");
        };
        let kind =
            notification_kind
                .as_deref()
                .unwrap_or(if recheck_unread { "agent" } else { "link" });
        match emit_terminal_notification(
            config,
            terminal,
            &self.current,
            BellNotice {
                title: &title,
                body: &body,
                panes: &panes,
                recheck_unread,
                kind,
            },
            diag,
        ) {
            Ok(true) => self.remind.note_ring(crate::sidebar::timing::unix_now_ms()),
            Ok(false) => {}
            Err(err) => debug!(error = %err, "terminal notification emit failed"),
        }
    }

    fn handle_focus_stranded(&mut self, config: &ServeConfig, repair: &PendingFocusRepair) -> bool {
        let now_ms = crate::sidebar::timing::unix_now_ms();
        let own_pane = crate::mux::own_pane_id(config.mux);
        if let Some(target) = focus_stranded_target(
            &self.current,
            &self.ui,
            &repair.pane_id,
            &repair.clients,
            own_pane.as_ref(),
            repair.sent_at_ms,
            now_ms,
        ) {
            let expected =
                (repair.pane_id.mux() == MuxName::Zellij).then(|| repair.clients.clone());
            spawn_pane_focus(
                target,
                &config.session_name,
                self.read_marks.runtime().clone(),
                crate::sidebar::focus_anchor::FocusOrigin::AutomaticRepair,
                expected,
                (self.ui.scroll_offset, Some(self.ui.last_order.clone())),
                Some(repair.generation),
            );
            return true;
        }
        debug!(
            pane = %repair.pane_id,
            generation = repair.generation,
            evidence = ?repair.clients,
            client_views = ?self.current.client_views,
            viewed_panes = ?self.current.viewed_panes,
            age_ms = now_ms.saturating_sub(repair.sent_at_ms),
            "sidebar focus repair deferred: snapshot disagrees with strand evidence",
        );
        false
    }

    /// Re-attempt a strand the last fold could not yet act on. The first
    /// snapshot after a strand routinely predates the client sample the plugin
    /// took, so a single miss says nothing about whether focus is healthy — the
    /// repair stays viable for the event's whole TTL and retries on each fold
    /// until the evidence agrees. Past the TTL it is abandoned with a record.
    fn retry_pending_focus_repair(&mut self, config: &ServeConfig) {
        let Some(repair) = self.pending_focus_repair.take() else {
            return;
        };
        if self.handle_focus_stranded(config, &repair) {
            return;
        }
        let now_ms = crate::sidebar::timing::unix_now_ms();
        if focus_repair_still_viable(repair.sent_at_ms, now_ms) {
            self.pending_focus_repair = Some(repair);
        } else {
            self.record_abandoned_focus_repair(config, &repair, "client evidence never converged");
        }
    }

    /// A strand RimZ decided not to act on still gets a durable diagnostic
    /// record explaining why it lapsed.
    fn record_abandoned_focus_repair(
        &self,
        config: &ServeConfig,
        repair: &PendingFocusRepair,
        reason: &str,
    ) {
        use crate::diag::focus_repair::{FocusRepairOutcome, FocusRepairRecord};
        debug!(
            pane = %repair.pane_id,
            generation = repair.generation,
            reason,
            "sidebar focus repair abandoned",
        );
        crate::diag::focus_repair::spawn_append(
            self.read_marks.runtime(),
            &FocusRepairRecord {
                at: jiff::Timestamp::now(),
                nonce: None,
                workspace_id: self.read_marks.runtime().workspace_id.clone(),
                session_name: config.session_name.clone(),
                generation: repair.generation,
                evidence: repair.clients.clone(),
                target: repair.pane_id.clone(),
                outcome: FocusRepairOutcome::Failed,
                error: Some(reason.to_owned()),
            },
        );
    }

    fn handle_overlay_event(
        &mut self,
        config: &ServeConfig,
        fetch: &mut FetchDispatcher,
        event: SidebarEvent,
        sent_at_ms: u64,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) {
        // An overlay event fuses into the in-memory state and paints this
        // frame. A topology overlay also asks the producer to verify with a
        // real pull, which supersedes the overlay once its fresh frame
        // folds in. A resize-grow paint hold stays held until a pulled
        // sibling-count verdict releases it.
        let own = self.own_pane.as_ref();
        let own_focused = matches!(&event, SidebarEvent::FocusChanged { focused, .. }
            if own.is_some_and(|pane| focused.contains(pane)));
        let own_unfocused = matches!(&event, SidebarEvent::FocusChanged { unfocused, .. }
            if own.is_some_and(|pane| unfocused.contains(pane)));
        let requests_verification = event.requests_producer_verification();
        let now_ms = crate::sidebar::timing::unix_now_ms();
        self.event_store.append(event, sent_at_ms, now_ms);
        self.fold_fused_now(config, anim_start, diag);
        if !self.should_exit && (requests_verification || own_focused) {
            fetch.request(FetchRequest::producer_fresh_panes(), true);
        }
        if own_focused {
            self.optimistic_watch_until = Some(Instant::now() + FOCUS_RESUME_WATCH_WINDOW);
        } else if own_unfocused {
            self.optimistic_watch_until = None;
            self.ui.help_visible = false;
        }
    }

    /// `settled_width` is the pane width observed after the resize wakeup,
    /// probed once at the dispatch site so tests can drive the feedback path.
    pub(super) fn on_resize(
        &mut self,
        config: &ServeConfig,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        settled_width: Option<u16>,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> Result<()> {
        self.paint.refresh_caps(config.mux, &config.session_name);
        // Once a sibling has been seen, hold only a grow beyond the configured
        // cap or room override: that is the shape of space freed by a closing
        // sibling. Startup and attach relayouts land at the legitimate width
        // and paint immediately.
        let held_grow = match settled_width {
            Some(width) => {
                let grew = resize_grew(self.prev_width, width);
                self.prev_width = Some(width);
                grew && grow_beyond_legit(width, self.max_legit_cols())
            }
            None => false,
        };
        self.run_width_control(terminal, WidthControlTrigger::ResizeFeedback, diag);
        if held_grow && self.self_close.seen_sibling {
            self.dirty = true;
            self.paint_hold
                .engage(Instant::now(), crate::sidebar::timing::unix_now_ms());
        } else {
            if self
                .apply_input(Wakeup::Resize, terminal, anim_start)?
                .redraw
            {
                self.dirty = false;
            }
            // A safe-width paint just landed; drop any stale hold a prior grow
            // left pending so it cannot suppress this frame.
            self.paint_hold.release();
        }
        self.last_self_close_check = Instant::now();
        // A resize is the mux telling us topology changed. Pull a fresh pane
        // list through the elected producer and require a cache produced after
        // this signal.
        fetch.request(FetchRequest::producer_fresh_panes(), true);
        Ok(())
    }

    pub(super) fn run_width_control(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        trigger: WidthControlTrigger,
        diag: &crate::diag::DiagSink,
    ) {
        let Ok(size) = terminal.size() else {
            return;
        };
        self.width_control.observe(size.width, trigger, diag);
    }

    pub(super) fn run_width_control_backstop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        diag: &crate::diag::DiagSink,
    ) {
        self.width_control.backstop(
            terminal.size().ok().map(|size| size.width),
            self.current
                .own_view
                .as_ref()
                .map(|view| view.sibling_count),
            self.current.panes_observed_at_ms,
            diag,
        );
    }

    #[cfg(test)]
    fn refresh_pet_render_caps_with(
        &mut self,
        mux: MuxName,
        session_name: &str,
        detect: impl FnOnce(MuxName, &str, PixelRenderCaps) -> PixelRenderCaps,
    ) {
        self.paint.refresh_caps_with(mux, session_name, detect);
    }

    #[cfg(test)]
    fn refresh_pet_render_caps_if_stale_with(
        &mut self,
        mux: MuxName,
        session_name: &str,
        now: Instant,
        detect: impl FnOnce(MuxName, &str, PixelRenderCaps) -> PixelRenderCaps,
    ) -> bool {
        self.paint
            .refresh_caps_if_stale_with(mux, session_name, now, detect)
    }

    pub(super) fn on_input(
        &mut self,
        config: &ServeConfig,
        wakeup: Wakeup,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        fetch: &mut FetchDispatcher,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> Result<()> {
        let applied = self.apply_input(wakeup, terminal, anim_start)?;
        if applied.redraw {
            // Key/mouse input paints synchronously for instant feedback; a
            // paint settles any frame the loop owed.
            self.dirty = false;
        }
        let interacted = applied.redraw || applied.effect.is_some();
        if interacted {
            order_hold::arm_order_hold(&mut self.ui, jiff::Timestamp::now().as_millisecond());
        }
        match applied.effect {
            Some(InputEffect::Focus(pane)) => {
                spawn_pane_focus(
                    pane,
                    &config.session_name,
                    self.read_marks.runtime().clone(),
                    crate::sidebar::focus_anchor::FocusOrigin::User,
                    None,
                    (self.ui.scroll_offset, Some(self.ui.last_order.clone())),
                    None,
                );
            }
            Some(InputEffect::Width(dir)) => {
                let Ok(size) = terminal.size() else {
                    return Ok(());
                };
                self.width_control.adjust(size.width, dir, diag);
            }
            Some(InputEffect::MarkRead(row_id)) => self.mark_row_read(fetch, &row_id, diag),
            Some(InputEffect::MarkUnread(row_id)) => self.mark_row_unread(fetch, &row_id, diag),
            Some(InputEffect::MarkAllRead) => self.mark_all_read(fetch, diag),
            Some(InputEffect::SyncFilter(filter)) => self.persist_body_filter(filter),
            Some(InputEffect::DismissAlert) | None => {}
        }
        Ok(())
    }

    fn persist_body_filter(&self, filter: Option<BodyFilter>) {
        let persisted = match filter {
            Some(filter) => crate::sidebar::body_filter::write(self.read_marks.runtime(), filter)
                .map_err(|err| err.to_string()),
            None => crate::sidebar::body_filter::clear(self.read_marks.runtime())
                .map_err(|err| err.to_string()),
        };
        if let Err(err) = persisted {
            debug!(error = %err, "sidebar body filter write failed");
            return;
        }
        if let Err(err) = crate::sidebar::wakeup::broadcast(
            self.read_marks.runtime(),
            Some(&self.session_name),
            SidebarEvent::BodyFilterChanged,
        ) {
            debug!(error = %err, "sidebar body filter broadcast failed");
        }
    }

    /// Apply an input wakeup (key/mouse/resize) to the local UI in place. Input
    /// never changes store data, so it redraws the *current* snapshot and may
    /// jump focus, but it never re-runs the snapshot burst — that per-keystroke
    /// refetch was the input lag. Input paints synchronously so a keypress or
    /// click feels instant rather than waiting for the next frame; the returned
    /// `InputOutcome::redraw` reports whether it painted, so the serve loop can
    /// clear its frame-pending flag.
    fn apply_input(
        &mut self,
        wakeup: Wakeup,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        anim_start: Instant,
    ) -> Result<InputOutcome> {
        let outcome = handle_wakeup(wakeup, &mut self.ui, &self.current);
        if matches!(outcome.effect, Some(InputEffect::DismissAlert)) {
            self.health.alert = None;
        }
        if outcome.redraw {
            // Carry the live spin phase into the instant paint so a keypress
            // mid-spin never rewinds the animation to a stale frame.
            self.ui.animation_phase =
                wall_clock_phase(anim_start, self.current.theme.display.resolved_refresh_ms());
            let alert_active = self.alert_active();
            self.paint
                .refresh_view(&mut self.ui, &self.current, alert_active);
            self.paint.draw_and_paint(
                terminal,
                &self.current,
                self.health.alert.as_ref(),
                &mut self.ui,
            )?;
        }
        Ok(outcome)
    }

    /// Mark a row read without jumping (`m`): write the durable manual receipt,
    /// clear the row locally for an instant repaint, trace the clear, wake the
    /// room so the elder prunes the episode and peer tabs converge, and refetch
    /// so the receipt lands in the pulled snapshot. A no-op when the row is
    /// already read.
    fn mark_row_read(
        &mut self,
        fetch: &mut FetchDispatcher,
        row_id: &str,
        diag: &crate::diag::DiagSink,
    ) {
        if self.ui.unread_guard.as_deref() == Some(row_id) {
            self.ui.unread_guard = None;
        }
        let runtime = self.read_marks.runtime().clone();
        let now = jiff::Timestamp::now();
        let marks = self.read_marks.load_merged();
        let clear = read_receipt_for_row(
            &self.current,
            Some(row_id),
            UnreadClearCause::MarkRead,
            &marks,
            now,
        );
        self.apply_read_clear(&runtime, clear, now, fetch, diag);
    }

    fn apply_read_clear(
        &mut self,
        runtime: &RuntimePaths,
        clear: ReadClear,
        now: jiff::Timestamp,
        fetch: &mut FetchDispatcher,
        diag: &crate::diag::DiagSink,
    ) {
        if clear.ids.is_empty() {
            return;
        }
        if let Err(err) = write_manual_read_marks(runtime, clear.ids.clone(), now.as_millisecond())
        {
            warn!(error = %err, "mark-read receipt write failed");
            return;
        }
        set_rows_unread(&mut self.current, &clear.ids, false);
        emit_unread_cleared_trace(diag, &clear.trace);
        self.commit_local_mark(runtime, fetch);
    }

    /// Mark every readable row read without jumping (`M`): write manual receipts
    /// for every unread or needs-a-look row, clear local unread bits, and refetch
    /// so peers converge through the producer.
    fn mark_all_read(&mut self, fetch: &mut FetchDispatcher, diag: &crate::diag::DiagSink) {
        self.ui.unread_guard = None;
        let runtime = self.read_marks.runtime().clone();
        let now = jiff::Timestamp::now();
        let marks = self.read_marks.load_merged();
        let clear = read_receipts_for_all(&self.current, UnreadClearCause::MarkRead, &marks, now);
        self.apply_read_clear(&runtime, clear, now, fetch, diag);
    }

    /// Re-flag a row unread without jumping (`m` on a read row): open a durable
    /// episode through the shared mark-unread path, set the row locally for an
    /// instant repaint, trace the open, wake the room, and refetch so the episode
    /// lands in the pulled snapshot. A no-op when the row has left the room.
    fn mark_row_unread(
        &mut self,
        fetch: &mut FetchDispatcher,
        row_id: &str,
        diag: &crate::diag::DiagSink,
    ) {
        let runtime = self.read_marks.runtime().clone();
        let Some(row) = self.current.rows().find(|row| row.id == row_id).cloned() else {
            return;
        };
        let now_ms = jiff::Timestamp::now().as_millisecond();
        let opened = match unread::mark_rows_unread(&runtime, std::slice::from_ref(&row), now_ms) {
            Ok(opened) => opened,
            Err(err) => {
                warn!(error = %err, "mark-unread episode write failed");
                return;
            }
        };
        set_rows_unread(&mut self.current, std::slice::from_ref(&row.id), true);
        self.ui.unread_guard = Some(row.id.clone());
        for item in &opened {
            diag.trace_notify(item.trace_event());
        }
        self.commit_local_mark(&runtime, fetch);
    }

    fn commit_local_mark(&mut self, runtime: &RuntimePaths, fetch: &mut FetchDispatcher) {
        wake_room(runtime);
        self.dirty = true;
        self.next_frame = Instant::now();
        fetch.request(FetchRequest::default(), true);
    }

    pub(super) fn run_maintenance(
        &mut self,
        fetch: &mut FetchDispatcher,
        ctx: MaintenanceContext<'_>,
    ) {
        // Snapshot wakeups are a latency hint, not the only correctness path.
        // `rimz reload` replaces the renderer in place and a ready-result
        // datagram can be lost around socket teardown/rebind; the frame/tick
        // path still drains the channel so startup cannot strand the
        // placeholder cockpit.
        self.on_snapshot(ctx.config, fetch, ctx.result_rx, ctx.anim_start, ctx.diag);
        fetch.fire_due(Instant::now());

        if gate_remaining(&self.gate, jiff::Timestamp::now())
            .is_some_and(|remaining| remaining.is_zero())
        {
            fetch.request(FetchRequest::force_fold(), false);
        }

        // Tab-view read dwell: once the user has stayed past the dwell, provoke
        // a fold so the normal clear path sweeps the unread siblings. The fold
        // disarms the deadline, so this fires at most a fetch or two.
        if self
            .tab_read_dwell_until
            .filter(|_| self.current.own_view.is_some())
            .is_some_and(|until| Instant::now() >= until)
        {
            fetch.request(FetchRequest::force_fold(), false);
        }

        // Data backstop: catch pane/git drift no store delta announced. It is
        // self-gated to the data tick; an armed clamp-deferred nudge merges
        // into this fold instead of waiting to echo it.
        if self.fetched_at.elapsed() >= ctx.tick {
            fetch.request(FetchRequest::default(), false);
        }

        // Heartbeat: fast in-process atomic write on the main thread so the
        // exit path never races a background writer.
        if heartbeat_write_due(self.last_heartbeat) {
            self.last_heartbeat = Some(Instant::now());
            if let Err(err) = write_heartbeat(ctx.config, ctx.runtime, ctx.socket_path) {
                warn!(
                    session = %ctx.config.session_name,
                    error = %err,
                    "heartbeat write failed",
                );
            }
        }

        // Self-close watchdog: if no resize or presence event fired, ask the
        // normal snapshot path to refresh so the snapshot's own-view count can
        // close a lone sidebar. Once a zero-sibling verdict is pending, force a
        // non-skippable fold so consumers do not sit behind the unchanged memo
        // for the full backstop window before the confirm timer can close them.
        if self.last_self_close_check.elapsed() >= SELF_CLOSE_WATCHDOG {
            self.last_self_close_check = Instant::now();
            let request = if self.self_close.confirming_empty() {
                FetchRequest::producer_fresh_panes()
            } else {
                FetchRequest::default()
            };
            fetch.request(request, false);
        }
        if self
            .ui
            .order_hold
            .as_ref()
            .is_some_and(|hold| jiff::Timestamp::now().as_millisecond() >= hold.expires_ms)
        {
            fetch.request(FetchRequest::default(), false);
        }
    }

    pub(super) fn maybe_remind(
        &mut self,
        config: &ServeConfig,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        diag: &crate::diag::DiagSink,
    ) {
        self.remind
            .maybe_remind(config, terminal, &self.current, diag);
    }

    pub(super) fn clear_pixel(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) {
        if let Err(err) = self.paint.clear(terminal.backend_mut()) {
            debug!(error = %err, "pet pixel clear failed");
        }
    }

    pub(super) fn paint_frame_if_due(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        anim_start: Instant,
        active: bool,
    ) -> Result<()> {
        let now = Instant::now();
        let paint_blocked = self.prepare_paint(terminal, now);
        let watched = self.watched();
        let background_key = self.background_paint_due(now, watched);
        let foreground = self.foreground_paint_due(now, active, watched);
        // Once the tab has emptied, never paint again. A grow resize also
        // defers its paint until the sibling-count verdict releases the hold.
        if !self.should_exit
            && !self.self_close.confirming_empty()
            && !paint_blocked
            && (foreground || background_key.is_some())
        {
            self.paint_now(terminal, anim_start, now, background_key)?;
        } else if !active && !self.dirty {
            // Idle re-arm only: with a fold pending, the armed boundary must
            // hold so a paint already due within one frame is not pushed out.
            self.next_frame = now + animation_frame(&self.current);
        }
        Ok(())
    }

    fn prepare_paint(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        now: Instant,
    ) -> bool {
        if self.dirty {
            let dirty_deadline = now + animation_frame(&self.current);
            if self.next_frame > dirty_deadline {
                self.next_frame = dirty_deadline;
            }
        }
        if !self.paint_hold.is_engaged()
            && let Ok(size) = terminal.size()
        {
            // Close the engage-gap: a data/animation frame can reach this paint
            // after the mux grew our pane but before the Resize wakeup arms the
            // hold. Arm here too; on_resize still owns prev_width and the fresh
            // pane fetch that resolves the sibling-count verdict.
            self.arm_paint_hold_on_grow(size.width, now);
        }
        let hold_was_engaged = self.paint_hold.is_engaged();
        let paint_blocked = self.paint_hold.blocks_paint(now);
        if hold_was_engaged && !paint_blocked {
            debug!("resize paint hold expired");
        }
        paint_blocked
    }

    fn foreground_paint_due(&self, now: Instant, active: bool, watched: bool) -> bool {
        ((active && watched) || (self.dirty && self.dirty_paintable(watched)))
            && now >= self.next_frame
    }

    /// Whether an off-screen dirty frame earns a background paint: content key
    /// changed and the minimum interval elapsed.
    fn background_paint_due(&self, now: Instant, watched: bool) -> Option<BackgroundContentKey> {
        if !self.dirty || watched || self.dirty_paintable(watched) {
            return None;
        }
        let key = background_content_key(&self.current);
        (self.last_bg_paint.is_none_or(|at| {
            now.saturating_duration_since(at)
                >= crate::sidebar::timing::BACKGROUND_PAINT_MIN_INTERVAL
        }) && self.last_bg_key.as_ref() != Some(&key))
        .then_some(key)
    }

    fn paint_now(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        anim_start: Instant,
        now: Instant,
        background_key: Option<BackgroundContentKey>,
    ) -> Result<()> {
        self.ui.animation_phase =
            wall_clock_phase(anim_start, self.current.theme.display.resolved_refresh_ms());
        let alert_active = self.alert_active();
        let animating = is_animating(
            &self.current,
            &self.ui,
            self.ui.animation_phase,
            alert_active,
        );
        if self.dirty || animating {
            let was_dirty = self.dirty;
            self.paint
                .refresh_view(&mut self.ui, &self.current, alert_active);
            self.paint.draw_and_paint(
                terminal,
                &self.current,
                self.health.alert.as_ref(),
                &mut self.ui,
            )?;
            self.dirty = false;
            if was_dirty {
                self.last_bg_key = Some(
                    background_key
                        .clone()
                        .unwrap_or_else(|| background_content_key(&self.current)),
                );
            }
            if background_key.is_some() {
                self.last_bg_paint = Some(now);
            }
        }
        self.next_frame = next_frame_after(
            self.next_frame,
            now,
            frame_interval(&self.current, &self.ui, alert_active),
        );
        Ok(())
    }

    fn arm_paint_hold_on_grow(&mut self, width: u16, now: Instant) -> bool {
        if !self.paint_hold.is_engaged()
            && self.self_close.seen_sibling
            && resize_grew(self.prev_width, width)
            && grow_beyond_legit(width, self.max_legit_cols())
        {
            self.paint_hold
                .engage(now, crate::sidebar::timing::unix_now_ms());
            return true;
        }
        false
    }

    fn max_legit_cols(&self) -> u16 {
        self.width_control.max_legit_cols()
    }

    /// Fold one fetch outcome into the render state: gate it against the
    /// last-known-good frame, update health, snapshot, and selection, and
    /// report whether the loop should exit.
    pub(super) fn apply_fetch_outcome(
        &mut self,
        config: &ServeConfig,
        update: FetchUpdate,
        allow_shared_filter_sync: bool,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> ApplyOutcome {
        let snapshot_ok = matches!(&update, FetchUpdate::Snapshot { .. });
        let application = match update {
            FetchUpdate::Snapshot {
                snapshot,
                role,
                source,
                ..
            } => FetchApplication {
                snapshot: Ok(*snapshot),
                role,
                source: Some(source),
            },
            FetchUpdate::Failed { error, role, .. } => FetchApplication {
                snapshot: Err(error),
                role,
                source: None,
            },
            FetchUpdate::Unchanged { .. } => {
                return ApplyOutcome {
                    should_exit: false,
                    tab_emptied: false,
                    rejected: false,
                };
            }
        };
        let (prev_good, rejected, now) = self.commit_fetch(application, diag);
        let authoritative_filter_fold = allow_shared_filter_sync && snapshot_ok && !rejected;
        let prev_selected = self.ui.selected_pane.clone();
        let (focused_pane, cleared) = self.sweep_read_receipts(now, diag);
        self.reconcile_selection_and_order(
            &prev_good,
            prev_selected,
            focused_pane,
            cleared,
            authoritative_filter_fold,
            now.as_millisecond(),
        );
        self.fold_spend_rolls(anim_start);
        self.exit_verdict(config, rejected)
    }

    fn exit_verdict(&mut self, config: &ServeConfig, rejected: bool) -> ApplyOutcome {
        // A renderer degraded this long is non-functional. Ask the pane-resident
        // supervisor to respawn the worker in place; the pane remains stable
        // while a transient mux/store outage clears.
        if degraded_too_long(&self.health, Timestamp::now()) {
            warn!(
                target: SIDEBAR_HEALTH_TARGET,
                session = %config.session_name,
                reason = self.health.alert.as_ref().map(|alert| alert.reason.as_str()),
                "sidebar degraded too long; respawning the renderer in place",
            );
            return ApplyOutcome {
                should_exit: true,
                tab_emptied: false,
                rejected,
            };
        }

        // Own-view (sibling count) rides in on the snapshot — the producer computes
        // it from the same pane list it already enumerated. Presence publication and
        // the poll backstop feed this latch; resize only decides whether to hold a
        // grown-width paint while the fresh fold is pending.
        if self_close_decision(
            &mut self.self_close,
            self.current
                .own_view
                .as_ref()
                .map(|view| view.sibling_count),
            Instant::now(),
        ) {
            debug!(
                session = %config.session_name,
                "sidebar tab emptied; exiting so the pane closes itself",
            );
            return ApplyOutcome {
                should_exit: true,
                tab_emptied: true,
                rejected,
            };
        }
        ApplyOutcome {
            should_exit: false,
            tab_emptied: false,
            rejected,
        }
    }

    fn commit_fetch(
        &mut self,
        application: FetchApplication,
        diag: &crate::diag::DiagSink,
    ) -> (SidebarSnapshot, bool, Timestamp) {
        let is_elder = application.role.is_producer();
        // The gate compares the incoming snapshot against the last frame we actually
        // committed; `current` still holds it until we overwrite it below.
        let fetch_was_ok = application.snapshot.is_ok();
        let fetch_failure = application.snapshot.as_ref().err().cloned();
        let producer_verdict = application.source == Some(SnapshotSource::Produced);
        let mut computed = compute_next_state(application.snapshot, &self.current, &self.health);
        if fetch_was_ok && application.role.is_producer() && !producer_verdict {
            // Published fast folds are paintable data, not a producer-health
            // verdict. Only a completed produce can recover the refresh episode,
            // so frameless/status-only folds cannot mask repeated pane-read failure.
            computed.health = self.health.clone();
        }
        let incoming_panes_produced_at_ms = computed.snapshot.panes_produced_at_ms;
        let now = Timestamp::now();
        let (state, next_gate, rejected, released_via_escape_hatch, rejected_snapshot) =
            apply_gate(computed, fetch_was_ok, &self.current, &self.gate, now);
        if self.overlay_baseline.is_none() {
            self.overlay_baseline = rejected_snapshot;
        }
        emit_diagnostics(
            diag,
            FetchDiagnostics {
                prev_snapshot: &self.current,
                incoming_panes_produced_at_ms,
                next_snapshot: &state.snapshot,
                prev_health: &self.health,
                next_health: &state.health,
                prev_gate: &self.gate,
                next_gate: &next_gate,
                fetch_failure,
                rejected,
                released_via_escape_hatch,
                is_elder,
                now,
            },
        );
        let prev_good = self.install_fetch_state(state, next_gate);
        (prev_good, rejected, now)
    }

    fn install_fetch_state(&mut self, state: RenderState, next_gate: GateState) -> SidebarSnapshot {
        self.gate = next_gate;
        self.ui.gate_notice = self.gate.rule.map(|rule| render::GateNotice { rule });
        if let Some(alert) = state
            .health
            .alert
            .as_ref()
            .filter(|alert| alert.is_active())
        {
            warn!(target: SIDEBAR_HEALTH_TARGET, reason = %alert.reason, "sidebar refresh degraded");
        }
        self.health = state.health;
        std::mem::replace(&mut self.current, state.snapshot)
    }

    fn sweep_read_receipts(
        &mut self,
        now: Timestamp,
        diag: &crate::diag::DiagSink,
    ) -> (Option<PaneId>, bool) {
        let focused_pane = session_focus_baseline(&self.current, self.own_pane.as_ref());
        let viewing_register_pane = self
            .current
            .focused_pane
            .as_ref()
            .is_some_and(|pane| self.current.viewed_panes.contains(pane));
        let focused_row_id = focused_pane
            .as_ref()
            .filter(|_| viewing_register_pane)
            .and_then(|pane| row_id_of_pane(&self.current, pane));
        let marks = self.read_marks.load_merged();
        let live: HashSet<String> = self.current.rows().map(|row| row.id.clone()).collect();
        let mut clear = read_receipt_for_row(
            &self.current,
            focused_row_id.as_deref(),
            UnreadClearCause::Focus,
            &marks,
            now,
        );
        self.sweep_tab_read_receipts(focused_row_id.as_deref(), &marks, now, &mut clear);
        apply_manual_unread_guard(&mut self.ui, focused_row_id.as_deref(), &mut clear);
        self.read_marks
            .observe_fold(clear.ids.clone(), now.as_millisecond(), &live);
        set_rows_unread(&mut self.current, &clear.ids, false);
        emit_unread_cleared_trace(diag, &clear.trace);
        (focused_pane, !clear.ids.is_empty())
    }

    fn sweep_tab_read_receipts(
        &mut self,
        focused_row_id: Option<&str>,
        marks: &ReadMarks,
        now: Timestamp,
        clear: &mut ReadClear,
    ) {
        let (Some(view), Some(own_pane)) = (self.current.own_view.as_ref(), self.own_pane.as_ref())
        else {
            return;
        };
        let now_viewing = own_tab_viewed(&self.current, view, own_pane);
        let switched_in = self.ui.viewing_own_tab == Some(false) && now_viewing;
        self.ui.viewing_own_tab = Some(now_viewing);
        if !now_viewing {
            // Left the tab: a scan, not a read. Drop the pending sweep.
            self.tab_read_dwell_until = None;
        } else if switched_in {
            self.tab_read_dwell_until = Some(Instant::now() + TAB_READ_DWELL);
        }
        if now_viewing
            && self
                .tab_read_dwell_until
                .is_some_and(|until| Instant::now() >= until)
        {
            self.tab_read_dwell_until = None;
            clear.merge(read_receipts_for_tab(
                &self.current,
                &view.working_pane_ids,
                focused_row_id,
                marks,
                now,
            ));
        }
    }

    fn reconcile_selection_and_order(
        &mut self,
        prev_good: &SidebarSnapshot,
        prev_selected: Option<PaneId>,
        focused_pane: Option<PaneId>,
        cleared: bool,
        authoritative_filter_fold: bool,
        now_ms: i64,
    ) {
        // Presentation sort reorders the snapshot's full row set. The order hold
        // below can keep this sorted order stable across a read-clear long enough
        // for the user to confirm where they landed.
        self.current.sort_groups_for_presentation();
        // Reconcile the highlight as part of the fold, before the next frame paints:
        // re-anchor the identity-keyed selection to its row (so a status-churn
        // reorder never slides it onto a neighbour) and re-derive the baseline from
        // the session focus register. Selection is derived state, queried from the
        // mux each fold and updated by focus events between pulls. The derivation
        // is filtered to a non-sidebar row: a sidebar-self focus or non-row focused
        // pane derives `None` and the baseline holds its last value. It is
        // deliberately blind to the make-up filter — the focused pane is real
        // however the body is narrowed, so a hidden baseline holds rather than
        // blanks.
        let derived =
            focused_pane.filter(|pane| row_index_of_pane(&self.current, None, pane).is_some());
        let derived_focus_pane = derived.is_some();
        if authoritative_filter_fold {
            let shared_filter = crate::sidebar::body_filter::load(self.read_marks.runtime());
            set_make_up_filter(&mut self.ui, &self.current, shared_filter);
        }
        let previous_filter = self.ui.make_up_filter;
        reconcile_selection(&mut self.ui, &self.current, derived);
        if authoritative_filter_fold
            && !self.current.worktree_groups.is_empty()
            && previous_filter.is_some()
            && self.ui.make_up_filter.is_none()
        {
            self.persist_body_filter(None);
        }
        // A fresh focus-register derivation that moved the highlight is an external
        // focus switch. Arm a one-shot reveal so the next paint brings the focused
        // card's worktree header on-screen with it. A sidebar jump also lands here,
        // but its fresh focus anchor cancels this in `apply_focus_anchor`.
        if derived_focus_pane
            && self.ui.selected_pane.is_some()
            && self.ui.selected_pane != prev_selected
        {
            self.ui.focus_group_reveal = true;
        }
        let acted = order_hold::focused_interaction(
            prev_good,
            &self.current,
            self.ui.selected_pane.as_ref(),
        );
        let interacted = cleared || self.ui.selected_pane != prev_selected || acted;
        order_hold::apply_order_hold(&mut self.ui, &mut self.current, interacted, now_ms);
        self.ui.last_order = order_hold::capture_order(&self.current, &self.ui);
    }

    fn fold_spend_rolls(&mut self, anim_start: Instant) {
        self.ui.animation_phase =
            wall_clock_phase(anim_start, self.current.theme.display.resolved_refresh_ms());
        // Fold the fresh headline spend into the count-up: a higher figure starts a
        // stepped roll that the next frames paint, a reset or first value snaps,
        // and an unchanged one is a no-op that leaves a climb in flight. The live
        // overlay is the preferred target — it moves with every statusline push —
        // falling back to the walked tally on a pre-overlay snapshot. A fetch
        // carrying neither leaves the roll untouched, so a transient missing
        // snapshot never snaps the figure to zero. The serve loop paints the
        // folded state on its next frame boundary; this path never draws.
        if let Some((usd, epoch)) = render::cockpit_spend_target(&self.current) {
            let shown = self.ui.spend_ratchet.observe(epoch, usd);
            self.ui.tally.observe(shown, self.ui.animation_phase);
        }
        // The per-card cost rolls fold beside it: live cards observe session cost,
        // while a finished cohort observes each durable seat's lifetime cost.
        // Both use the same resolved target the renderer paints, keyed by durable
        // row id, so finishing a group rolls each card up to the receipt figures.
        // A row without cost enrichment is not observed; its first cost snaps.
        self.ui.cost_rolls.observe(
            self.current.worktree_groups.iter().flat_map(|group| {
                group.rows.iter().filter_map(|row| {
                    render::agent_card_cost_usd(group, row).map(|usd| (row.id.clone(), usd))
                })
            }),
            self.ui.animation_phase,
        );
    }

    fn fold_outcome(
        &mut self,
        config: &ServeConfig,
        update: FetchUpdate,
        allow_shared_filter_sync: bool,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> bool {
        let applied =
            self.apply_fetch_outcome(config, update, allow_shared_filter_sync, anim_start, diag);
        self.should_exit = applied.should_exit;
        if applied.should_exit {
            self.exit_cause = Some(if applied.tab_emptied {
                RendererExitCause::SelfCloseEmptyTab
            } else {
                RendererExitCause::DegradedGaveUp
            });
        }
        self.tab_emptied |= applied.tab_emptied;
        self.apply_focus_anchor();
        self.observe_commit();
        self.dirty = true;
        applied.rejected
    }

    fn apply_focus_anchor(&mut self) {
        let Some(anchor) = crate::sidebar::focus_anchor::load(self.read_marks.runtime()) else {
            return;
        };
        let now_ms = crate::sidebar::timing::unix_now_ms();
        let presentation_at_ms = anchor.applied_at_ms.unwrap_or(anchor.issued_at_ms);
        if self.ui.selected_pane.as_ref() == Some(&anchor.pane_id)
            && anchor.issued_at_ms > self.ui.last_focus_anchor_ms
            && crate::sidebar::focus_anchor::is_fresh(presentation_at_ms, now_ms)
        {
            self.ui.scroll_offset = anchor.offset;
            self.ui.manual_scroll = None;
            // A sidebar jump freezes the clicked row; suppress the group reveal
            // armed for the same selection change.
            self.ui.focus_group_reveal = false;
            if let Some(order) = anchor.order {
                order_hold::adopt_shared_hold(
                    &mut self.ui,
                    &mut self.current,
                    order,
                    presentation_at_ms as i64,
                );
            }
            self.ui.last_focus_anchor_ms = anchor.issued_at_ms;
        }
        if self.confirmed_focus_intent_ms == anchor.issued_at_ms {
            crate::sidebar::focus_anchor::clear_matching(self.read_marks.runtime(), anchor.nonce);
            self.confirmed_focus_intent_ms = 0;
        }
    }

    fn pending_focus_intent(&mut self, now_ms: u64) -> Option<FocusPresentation> {
        let anchor = crate::sidebar::focus_anchor::load(self.read_marks.runtime())?;
        let outcome = if focus_intent_confirmed_from(
            &self.last_focus_observation,
            &self.event_store,
            &anchor,
            now_ms,
        ) {
            FocusObservationOutcome::Confirmed
        } else {
            crate::sidebar::focus_anchor::observation_outcome_from(
                &anchor,
                &self.last_focus_observation,
                now_ms,
            )
        };
        match outcome {
            FocusObservationOutcome::Present => Some(FocusPresentation::Target(Box::new(anchor))),
            FocusObservationOutcome::Fence => Some(FocusPresentation::Fence),
            FocusObservationOutcome::Confirmed
            | FocusObservationOutcome::Superseded
            | FocusObservationOutcome::Invalidated => {
                if outcome == FocusObservationOutcome::Confirmed
                    && anchor.origin == FocusOrigin::User
                {
                    self.confirmed_focus_intent_ms = anchor.issued_at_ms;
                    return None;
                }
                if crate::sidebar::focus_anchor::clear_matching(
                    self.read_marks.runtime(),
                    anchor.nonce,
                ) {
                    if self.confirmed_focus_intent_ms == anchor.issued_at_ms {
                        self.confirmed_focus_intent_ms = 0;
                    }
                    self.record_focus_resolution(&anchor, outcome);
                }
                None
            }
        }
    }

    fn record_focus_resolution(
        &self,
        anchor: &crate::sidebar::focus_anchor::FocusAnchor,
        outcome: FocusObservationOutcome,
    ) {
        if anchor.origin != FocusOrigin::AutomaticRepair {
            return;
        }
        use crate::diag::focus_repair::{FocusRepairOutcome, FocusRepairRecord};
        let outcome = match outcome {
            FocusObservationOutcome::Confirmed => FocusRepairOutcome::Confirmed,
            FocusObservationOutcome::Superseded => FocusRepairOutcome::Superseded,
            FocusObservationOutcome::Invalidated => FocusRepairOutcome::Invalidated,
            _ => return,
        };
        crate::diag::focus_repair::spawn_append(
            self.read_marks.runtime(),
            &FocusRepairRecord {
                at: jiff::Timestamp::now(),
                nonce: Some(anchor.nonce.to_string()),
                workspace_id: self.read_marks.runtime().workspace_id.clone(),
                session_name: anchor.session_name.clone(),
                generation: anchor.repair_generation.unwrap_or_default(),
                evidence: anchor.pre_action.clone(),
                target: anchor.pane_id.clone(),
                outcome,
                error: None,
            },
        );
    }

    fn observe_commit(&mut self) {
        let now_ms = crate::sidebar::timing::unix_now_ms();
        let sig = observe::extract_sig(
            &self.current,
            &self.last_pulled_sig,
            &self.event_store,
            self.gate.reject_streak,
            self.health.failure_streak,
            now_ms,
        );
        for draft in self.observer.observe(sig) {
            let carried_drops = draft.dropped_msgs;
            if self
                .observe_tx
                .try_send(ObserveMsg::Anomaly(Box::new(draft)))
                .is_err()
            {
                self.observer.dropped_msgs = self
                    .observer
                    .dropped_msgs
                    .saturating_add(carried_drops)
                    .saturating_add(1);
            }
        }
        if let Some(roster) = self.observer.pending_roster_update() {
            if self.observe_tx.try_send(ObserveMsg::Roster(roster)).is_ok() {
                self.observer.clear_roster_update();
            } else {
                self.observer.dropped_msgs = self.observer.dropped_msgs.saturating_add(1);
            }
        }
    }
}

/// Resolve a reload request — the `r` keypress and the typed `Reload` event
/// share this. `true` means a differing on-disk binary: the caller exits with
/// the supervisor reload code so the pane command converges onto the new
/// binary. A byte-identical or missing binary skips reload but still honours
/// the intent with an immediate producing refetch, so a reload always pulls
/// live data and un-sticks a tab whose producer has stalled.
fn reload_or_refetch(
    workspace_id: &crate::ids::WorkspaceId,
    session_name: &str,
    fetch: &mut FetchDispatcher,
) -> bool {
    match reload_action(workspace_id) {
        ReloadAction::Reexec(target) => {
            debug!(
                session = %session_name,
                target = %target.display(),
                "reload: on-disk binary differs; asking supervisor to re-exec",
            );
            return true;
        }
        ReloadAction::AlreadyCurrent => {
            debug!(
                session = %session_name,
                "reload: binary unchanged; refetching in place without re-exec",
            );
        }
        // A reload that cannot find its replacement (a partial or in-flight
        // install) must never make the sidebar vanish — keep serving the
        // current build and refetch.
        ReloadAction::Missing => {
            warn!(
                session = %session_name,
                "reload requested but no renderer binary is on disk; refetching in place",
            );
        }
    }
    fetch.request(FetchRequest::hard_refresh(), true);
    false
}

/// Ping every sidebar in the room to refold after a mark read/unread — the
/// elder prunes or keeps the episode and peer tabs converge on the new state.
fn wake_room(runtime: &RuntimePaths) {
    if let Err(err) = crate::sidebar::wakeup::wake_store_delta(runtime, None, None) {
        debug!(error = %err, "mark read/unread sidebar wake failed");
    }
}

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LoopFlow {
    Continue,
    Repoll,
    Exit,
}
