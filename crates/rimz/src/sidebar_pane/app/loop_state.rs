//! Renderer state transitions and loop-lifetime context, including fetch application, focus repair, maintenance deadlines, and paint eligibility.

use std::collections::HashSet;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::diag::focus_trace::FocusTraceEvent;
use crate::diag::record::{RendererExitCause, SidebarWidthControlTrigger as WidthControlTrigger};
use crate::ids::{FocusNonce, PaneId};
use crate::mux::focus_anchor::{
    FocusActionError, FocusObservation, FocusObservationOutcome, FocusOrigin, FocusPresentation,
};
use crate::observability::SIDEBAR_HEALTH_TARGET;
use crate::sidebar::enrich::project_local;
use crate::sidebar::event_store::EventStore;
use crate::sidebar::fuse::{focus_intent_confirmed_from, fuse_owned};
use crate::sidebar::observe::{self, ObserveMsg};
use crate::sidebar::read_marks::{ReadMarks, write_read_marks};
use crate::sidebar::timing::{FOCUS_REPAIR_RETRY_STEP, FOCUS_STRANDED_EVENT_TTL, TAB_READ_DWELL};
use crate::sidebar::unread::{self, UnreadClearCause};
use crate::sidebar_pane::pixel::{PixelRenderCaps, PixelSlot};
use crate::sidebar_pane::render::{self, UiState};
use crate::sidebar_pane::view::BodyLens;
use crate::store::snapshot::{SidebarOwnView, SidebarSnapshot};
use crate::wakeup::events::{SidebarEvent, SidebarEventEnvelope};
use crate::{MuxName, RuntimePaths};

use jiff::Timestamp;
use ratatui::Terminal;
use tracing::{debug, trace, warn};

use super::backend::PaneBackend;
use super::fetch::{
    FetchDispatcher, FetchPhase, FetchRequest, FetchUpdate, ResultReceiver, SnapshotSource,
    WorkspaceFold,
};
use super::gate::{GateState, apply_gate, gate_remaining};
use super::health::{Health, degraded_too_long};
use super::inbox::WakeupInbox;
use super::input::{InputMode, KeyAction, Wakeup, resolve_key};
use super::lifecycle::{
    PaintHold, SELF_CLOSE_WATCHDOG, SelfCloseState, grow_beyond_legit, resize_grew,
};
use super::notify::{BellNotice, emit_terminal_notification};
use super::order_hold;
use super::paint::FramePainter;
use super::remind::RemindState;
use super::selection::{
    InputEffect, InputOutcome, finish_search, handle_key, handle_mouse_click, handle_scroll,
    reconcile_selection, row_index_of_pane, set_make_up_filter,
};
use super::socket::{heartbeat_write_due, write_heartbeat};
use super::state::{
    ApplyOutcome, FetchDiagnostics, ReadClear, RenderState, apply_manual_unread_guard,
    compute_next_state, emit_diagnostics, emit_unread_cleared_trace, read_receipt_for_row,
    read_receipts_for_all, read_receipts_for_tab, row_id_of_pane, session_focus_baseline,
    set_rows_unread,
};
use super::timing::{
    FOCUS_RESUME_WATCH_WINDOW, FRAME_MIN_TIMEOUT, animation_frame, duration_millis, frame_clock,
    next_frame_after, tick_for,
};
use super::width_control::WidthController;
use super::{Result, ServeConfig, fetch_deadline_timeout};

const RESIZE_CAPS_SETTLE: Duration = Duration::from_millis(300);

pub(super) type BaselineReporter = Box<dyn Fn(Option<&PaneId>) + Send>;

struct FetchApplication {
    snapshot: std::result::Result<SidebarSnapshot, String>,
    source: Option<SnapshotSource>,
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
    clients: Vec<crate::pane::ClientPaneView>,
    sent_at_ms: u64,
}

pub(super) struct LoopState {
    #[cfg(test)]
    pub(super) content_applications: u64,
    pub(super) current: SidebarSnapshot,
    /// The keymap beside the `[sidebar.keys]` table it was built from, rebuilt only when the snapshot's table differs.
    nav_keys: (crate::config::SidebarKeys, super::NavKeymap),
    config: ServeConfig,
    runtime: RuntimePaths,
    host_wake_path: PathBuf,
    diag: crate::diag::DiagSink,
    result_rx: ResultReceiver,
    anim_start: Instant,
    /// Full pulled truth retained only while an overlay, focus fence, or gate
    /// hold can outlive its source. The steady overlay-free path moves the pull
    /// directly into `current` and keeps only the compact projections below.
    overlay_baseline: Option<SidebarSnapshot>,
    last_focus_observation: FocusObservation,
    observation: Option<Arc<observe::FrameSig>>,
    fold_inputs: Option<Arc<super::fetch::FoldInputs>>,
    observed_anchor: Option<(Instant, Option<crate::mux::focus_anchor::FocusAnchor>)>,
    retired_anchor: Option<(Instant, u64)>,
    filter_observed_at: Option<Instant>,
    last_pulled_rows: usize,
    optimistic_watch_until: Option<Instant>,
    /// Deadline for the tab-view read sweep: armed when the own tab comes on
    /// screen, disarmed when it leaves. The sweep fires on the first fold at or
    /// past it while the tab is still viewed, so a pass-through never clears
    /// siblings. `None` when no dwell is pending.
    pub(super) tab_read_dwell_until: Option<Instant>,
    pub(super) events: Arc<Mutex<EventStore>>,
    pending_focus_repair: Option<PendingFocusRepair>,
    confirmed_focus_intent_ms: u64,
    observer: observe::OwnObserver,
    observe_tx: SyncSender<ObserveMsg>,
    pub(super) health: Health,
    gate: GateState,
    self_close: SelfCloseState,
    pub(super) ui: UiState,
    paint: FramePainter,
    pub(super) room_caps: Arc<Mutex<crate::sidebar_pane::pixel::probe::RoomCaps>>,
    remind: RemindState,
    dirty: bool,
    paint_hold: PaintHold,
    next_frame: Instant,
    fetched_at: Instant,
    last_self_close_check: Instant,
    last_heartbeat: Option<Instant>,
    prev_width: Option<u16>,
    caps_refresh_deadline: Option<Instant>,
    width_control: WidthController,
    pub(super) titled: bool,
    pub(super) should_exit: bool,
    pub(super) exit_cause: Option<RendererExitCause>,
    pub(super) tab_emptied: bool,
    pub(super) request_reload: Box<dyn Fn() + Send>,
    pub(super) baseline_changed: BaselineReporter,
    last_reported_baseline: Option<PaneId>,
}

pub(super) fn handle_wakeup(
    wakeup: Wakeup,
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    keymap: &super::NavKeymap,
    alert_active: bool,
) -> InputOutcome {
    let wakeup = match wakeup {
        Wakeup::Press { code, mods } => {
            let mode = if ui.help_visible {
                InputMode::Help
            } else if ui.search_draft.is_some() {
                InputMode::Typing
            } else {
                InputMode::Normal
            };
            Wakeup::Key(resolve_key(keymap, mode, code, mods))
        }
        wakeup => wakeup,
    };
    // The help popup is a transient modal: while it is up, the next
    // interaction dismisses it and is consumed, never also acting on the
    // sidebar beneath.
    if ui.help_visible
        && matches!(
            wakeup,
            Wakeup::Key(_) | Wakeup::MouseClick { .. } | Wakeup::Scroll { .. }
        )
    {
        ui.help_visible = false;
        return InputOutcome::redraw();
    }
    match wakeup {
        Wakeup::Key(KeyAction::Search) if alert_active => InputOutcome::default(),
        Wakeup::Key(action) => handle_key(action, ui, snapshot),
        Wakeup::MouseClick { column, row } => {
            let mut outcome = finish_search(ui, snapshot, true);
            let click = handle_mouse_click(column, row, ui, snapshot);
            outcome.redraw |= click.redraw;
            outcome.effects.extend(click.effects);
            outcome
        }
        Wakeup::Scroll { down } => handle_scroll(down, ui),
        Wakeup::Resize => InputOutcome::redraw(),
        // The serve loop intercepts these before dispatching here: a tick, a
        // typed sidebar event is a re-fetch trigger, worker completions are
        // folded, and raw presses are resolved against the current UI mode.
        Wakeup::Tick | Wakeup::Event(_) | Wakeup::Press { .. } | Wakeup::Snapshot => {
            InputOutcome::default()
        }
    }
}

impl LoopState {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        config: ServeConfig,
        baseline_pane: Option<PaneId>,
        runtime: RuntimePaths,
        host_wake_path: PathBuf,
        diag: crate::diag::DiagSink,
        result_rx: ResultReceiver,
        initial_width: Option<u16>,
        observe_tx: SyncSender<ObserveMsg>,
        pet_render_caps: PixelRenderCaps,
        pixel_slot: Option<PixelSlot>,
    ) -> Self {
        let snapshot_now = Timestamp::now();
        let current = SidebarSnapshot::build_with_agents(
            config.workspace_id.clone(),
            Vec::new(),
            snapshot_now,
        );
        let now = Instant::now();
        let pixel_wrap = config.mux == MuxName::Tmux;
        let width = crate::mux::SidebarWidth::from_config(
            &crate::config::MachineConfig::load_lenient().theme,
        );
        let width_control = WidthController::new(
            runtime.clone(),
            config.session_name.clone(),
            config.own_pane.clone(),
            config.mux,
            width,
        );
        let make_up_filter = crate::sidebar::body_filter::load(&runtime);
        Self {
            #[cfg(test)]
            content_applications: 0,
            nav_keys: (
                current.sidebar.keys.clone(),
                super::NavKeymap::from_config(&current.sidebar.keys),
            ),
            config,
            runtime,
            host_wake_path,
            diag,
            result_rx,
            anim_start: now,
            last_focus_observation: FocusObservation::from_snapshot(&current),
            observation: None,
            fold_inputs: None,
            observed_anchor: None,
            retired_anchor: None,
            filter_observed_at: None,
            last_pulled_rows: 0,
            overlay_baseline: None,
            current,
            optimistic_watch_until: None,
            tab_read_dwell_until: None,
            events: Arc::default(),
            pending_focus_repair: None,
            confirmed_focus_intent_ms: 0,
            observer: observe::OwnObserver::default(),
            observe_tx,
            health: Health::default(),
            gate: GateState::default(),
            self_close: SelfCloseState::default(),
            ui: UiState {
                make_up_filter,
                baseline_pane: baseline_pane.clone(),
                ..UiState::default()
            },
            paint: FramePainter::new(pet_render_caps, pixel_wrap, pixel_slot),
            room_caps: Arc::default(),
            remind: RemindState::default(),
            dirty: true,
            paint_hold: PaintHold::default(),
            next_frame: now,
            fetched_at: now,
            last_self_close_check: now,
            last_heartbeat: None,
            prev_width: initial_width,
            caps_refresh_deadline: None,
            width_control,
            titled: false,
            should_exit: false,
            exit_cause: None,
            tab_emptied: false,
            request_reload: Box::new(|| {}),
            baseline_changed: Box::new(|_| {}),
            last_reported_baseline: baseline_pane,
        }
    }

    /// Run one serve-loop iteration, the decision point for frame timing.
    ///
    /// Read activity again after dispatch so an input-started animation avoids the idle re-arm.
    pub(super) fn step(
        &mut self,
        inbox: &mut WakeupInbox,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<PaneBackend>,
    ) -> Result<()> {
        self.report_baseline();
        let (_, mut timeout) = self.frame_timing();
        timeout = fetch_deadline_timeout(timeout, fetch.next_deadline(), Instant::now());
        inbox.dispatch_next(self, fetch, terminal, timeout)?;
        // A pane the mux had not laid out at attach gets its title
        // with its first size.
        if !self.titled {
            self.titled = terminal.backend_mut().set_pane_title()?;
        }

        self.run_maintenance(fetch, terminal);
        self.run_width_control_backstop(terminal);
        self.maybe_remind(terminal);
        let (active, _) = self.frame_timing();
        self.paint_frame_if_due(terminal, active)
    }

    pub(super) fn report_baseline(&mut self) {
        if self.ui.baseline_pane != self.last_reported_baseline {
            (self.baseline_changed)(self.ui.baseline_pane.as_ref());
            self.last_reported_baseline
                .clone_from(&self.ui.baseline_pane);
        }
    }

    pub(super) fn seed(&mut self, fold: WorkspaceFold) {
        let mut snapshot = project_local(
            fold.workspace,
            fold.frame.as_deref(),
            self.config.own_pane.as_ref(),
        );
        if let Some(refresh_ms) = self.config.refresh_ms_override {
            snapshot.theme.display.refresh_ms = refresh_ms;
        }
        self.apply_latest_snapshot_with_seed(
            FetchUpdate::Snapshot {
                snapshot: Box::new(snapshot),
                phase: FetchPhase::Interim,
                source: SnapshotSource::Cached,
            },
            true,
        );
    }

    #[cfg(feature = "testkit")]
    pub(super) fn bench_frame_timing(&mut self, paint: bool) -> bool {
        if paint {
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(40, 60))
                .expect("benchmark terminal");
            render::draw_to_terminal(&mut terminal, &self.current, None, &mut self.ui)
                .expect("benchmark paint");
            assert!(self.ui.theme_cache.is_some());
        }
        std::hint::black_box(self.frame_timing());
        self.watched()
    }

    fn frame_timing(&self) -> (bool, Duration) {
        let clock = frame_clock(
            self.anim_start,
            self.current.theme.display.resolved_refresh_ms(),
        );
        let alert_active = self.alert_active();
        let watched = self.watched();
        let animating = watched
            && render::animation_interval(&self.current, &self.ui, clock.phase, alert_active)
                .is_some();
        let active = !self.self_close.confirming_empty() && watched && (animating || self.dirty);
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
            let mut timeout = tick_for(self.config.tick_seconds).min(watchdog_due);
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
        if let Some(deadline) = self
            .width_control
            .feedback_deadline()
            .into_iter()
            .chain(self.caps_refresh_deadline)
            .min()
        {
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
        let Some(own_pane) = self.config.own_pane.as_ref() else {
            return true;
        };
        let Some(view) = self.current.own_view.as_ref() else {
            return true;
        };
        own_tab_viewed(&self.current, view, own_pane)
    }

    pub(super) fn on_snapshot(&mut self, fetch: &mut FetchDispatcher) {
        let pending = self.result_rx.take();
        let delivered = pending.snapshot.is_some() || pending.outcome.is_some();
        let saw_final = pending.completed;
        let mut rejected = false;
        for update in pending.snapshot.into_iter().chain(pending.outcome) {
            rejected |= self.apply_latest_snapshot(update);
        }
        if delivered {
            self.result_rx
                .allow_unchanged(self.admits_clock_only(rejected));
        }
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

    /// Whether the next fold equal to the one just applied may arrive as a
    /// clock-only delivery. Renderer-local reconciliation state blocks it, so
    /// that state always meets full content. Exhaustive: a new field must be
    /// named here, as a blocker or as irrelevant to delivery.
    fn admits_clock_only(&self, rejected: bool) -> bool {
        let Self {
            // Blockers: an exit, a held overlay or focus fence, a gate episode,
            // a failure streak, a frozen order, an armed read dwell, an empty
            // confirmation, and a pending focus repair each resolve on a fold.
            should_exit,
            overlay_baseline,
            gate,
            health,
            ui,
            tab_read_dwell_until,
            self_close,
            pending_focus_repair,
            // The acknowledged baseline the comparison runs against.
            current: _,
            #[cfg(test)]
                content_applications: _,
            // Derived from `current`, which equal content leaves unchanged.
            nav_keys: _,
            last_focus_observation: _,
            last_pulled_rows: _,
            // Loop wiring and identity, fixed for the attachment.
            config: _,
            runtime: _,
            host_wake_path: _,
            diag: _,
            result_rx: _,
            observe_tx: _,
            room_caps: _,
            request_reload: _,
            baseline_changed: _,
            // Per-delivery fold context, replaced by every delivery.
            observation: _,
            fold_inputs: _,
            // Focus-anchor bookkeeping: a live intent or fence holds
            // `overlay_baseline`, and an anchor change is content.
            observed_anchor: _,
            retired_anchor: _,
            confirmed_focus_intent_ms: _,
            // Overlay events: an active one holds `overlay_baseline`.
            events: _,
            // Shared-filter fence: a shared filter change is content.
            filter_observed_at: _,
            // Paint, animation, and frame scheduling, which a clock-only
            // delivery drives itself.
            anim_start: _,
            paint: _,
            titled: _,
            dirty: _,
            paint_hold: _,
            next_frame: _,
            prev_width: _,
            caps_refresh_deadline: _,
            width_control: _,
            observer: _,
            optimistic_watch_until: _,
            // Maintenance stamps and reminders, read against `current`.
            fetched_at: _,
            last_self_close_check: _,
            last_heartbeat: _,
            remind: _,
            // Exit detail and baseline reporting, set beside `should_exit` or
            // from `ui` outside the fold.
            exit_cause: _,
            tab_emptied: _,
            last_reported_baseline: _,
        } = self;
        !should_exit
            && !rejected
            && overlay_baseline.is_none()
            && *gate == GateState::default()
            && health.failure_streak == 0
            && ui.order_hold.is_none()
            && tab_read_dwell_until.is_none()
            && !self_close.confirming_empty()
            && pending_focus_repair.is_none()
    }

    pub(super) fn share_width_geometry(
        &mut self,
        geometry: Arc<Mutex<crate::mux::zellij::WidthMemo>>,
    ) {
        self.width_control.geometry = geometry;
    }

    /// The pane's cell aspect as its terminal reports it, read by whoever
    /// holds the pane's output.
    pub(super) fn set_probed_aspect(&mut self, aspect: Option<crate::config::CellAspect>) {
        self.paint.set_probed_aspect(aspect);
    }

    pub(super) fn on_wakeup(
        &mut self,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<PaneBackend>,
        wakeup: Wakeup,
    ) -> Result<()> {
        match wakeup {
            Wakeup::Snapshot => {
                self.on_snapshot(fetch);
                Ok(())
            }
            Wakeup::Event(envelope) => {
                self.on_event(fetch, terminal, envelope);
                Ok(())
            }
            // A recv timeout: the active grid reached a frame boundary, or the
            // idle backstop interval elapsed. It carries no state of its own —
            // the frame phase below advances the spin and paints, and the
            // backstop poll runs there too.
            Wakeup::Tick => Ok(()),
            Wakeup::Resize => {
                let settled_width = terminal.size().map(|s| s.width).ok();
                self.on_resize(fetch, terminal, settled_width)?;
                Ok(())
            }
            wakeup => self.on_input(wakeup, terminal, fetch),
        }
    }

    pub(super) fn apply_latest_snapshot(&mut self, update: FetchUpdate) -> bool {
        self.apply_latest_snapshot_with_seed(update, false)
    }

    fn apply_latest_snapshot_with_seed(&mut self, update: FetchUpdate, seed: bool) -> bool {
        let delivered = update.clone();
        let (update, context) = update.into_parts();
        self.fold_inputs = context.as_ref().map(|context| context.inputs.clone());
        if let Some(observation) = context.and_then(|context| context.observation.clone()) {
            self.observation = Some(observation);
        }
        if let FetchUpdate::Unchanged { clock, .. } = update {
            clock.apply(&mut self.current);
            self.last_focus_observation.panes_observed_at_ms = self.current.panes_observed_at_ms;
            self.fetched_at = Instant::now();
            self.last_self_close_check = Instant::now();
            self.dirty = true;
            self.fold_inputs = None;
            self.result_rx.acknowledge(delivered, false);
            self.release_paint_hold_after_snapshot(false);
            return false;
        }
        let snapshot_ok = matches!(update, FetchUpdate::Snapshot { .. });
        let mut fused_event_sent_at_ms = None;
        let update = match update {
            FetchUpdate::Snapshot {
                snapshot,
                phase,
                source,
            } => {
                #[cfg(test)]
                {
                    self.content_applications += 1;
                }
                let now_ms = crate::utils::time::unix_now_ms();
                self.events().prune(now_ms);
                let pulled = *snapshot;
                self.last_focus_observation = FocusObservation::from_snapshot(&pulled);
                if self.observation.is_some() {
                    self.last_pulled_rows = pulled.rows().count();
                }
                let intent = self.pending_focus_intent(now_ms);
                let (snapshot, baseline, focus_event_sent_at_ms) =
                    fuse_owned(pulled, &self.events(), intent.as_ref(), now_ms);
                fused_event_sent_at_ms = focus_event_sent_at_ms;
                self.overlay_baseline = baseline;
                FetchUpdate::Snapshot {
                    snapshot: Box::new(snapshot),
                    phase,
                    source,
                }
            }
            failed @ FetchUpdate::Failed { .. } => failed,
            FetchUpdate::Unchanged { .. } => unreachable!("clock-only delivery handled above"),
            FetchUpdate::Shared { .. } => unreachable!("unwrapped above"),
        };
        self.fetched_at = Instant::now();
        let rejected = self.fold_outcome(update, true, seed, fused_event_sent_at_ms);
        self.result_rx.acknowledge(delivered, false);
        self.fold_inputs = None;
        if snapshot_ok {
            self.last_self_close_check = Instant::now();
            self.retry_pending_focus_repair();
        }
        self.release_paint_hold_after_snapshot(rejected);
        rejected
    }

    fn release_paint_hold_after_snapshot(&mut self, rejected: bool) {
        // Release by pane stamp alone. A `Fresh` frame is fresh against the
        // request that fetched it, and a request sent before the grow can land
        // after it with a pane list that still counts the closed sibling.
        if !self.should_exit
            && !rejected
            && !self.self_close.confirming_empty()
            && self
                .paint_hold
                .releases_on_stamp(self.current.panes_observed_at_ms)
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
    fn fused_snapshot(&mut self, now_ms: u64) -> (SidebarSnapshot, Option<u64>) {
        let pulled = self
            .overlay_baseline
            .take()
            .unwrap_or_else(|| self.current.clone());
        let intent = self.pending_focus_intent(now_ms);
        let (fused, baseline, focus_event_sent_at_ms) =
            fuse_owned(pulled, &self.events(), intent.as_ref(), now_ms);
        self.overlay_baseline = baseline;
        (fused, focus_event_sent_at_ms)
    }

    /// Fold a synthetic fused frame and snap the frame deadline so this
    /// turn's frame phase paints it now.
    fn fold_fused_now(&mut self) {
        self.result_rx.invalidate();
        let (fused, fused_event_sent_at_ms) =
            self.fused_snapshot(crate::utils::time::unix_now_ms());
        self.fold_outcome(
            FetchUpdate::Snapshot {
                snapshot: Box::new(fused),
                phase: FetchPhase::Interim,
                source: SnapshotSource::Cached,
            },
            false,
            false,
            fused_event_sent_at_ms,
        );
        self.next_frame = Instant::now();
    }

    pub(super) fn on_event(
        &mut self,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<PaneBackend>,
        envelope: SidebarEventEnvelope,
    ) {
        let requests_verification = envelope.event.requests_producer_verification();
        self.result_rx.invalidate();
        let sent_at_ms = envelope.sent_at_ms;
        if matches!(
            &envelope.event,
            SidebarEvent::PaneClosed { .. }
                | SidebarEvent::PaneOpened { .. }
                | SidebarEvent::PanesChanged
        ) {
            let measured = terminal.size().ok().map(|size| size.width);
            self.width_control
                .note_structural(sent_at_ms, measured, &self.diag);
        }
        match envelope.event {
            SidebarEvent::WidthTargetChanged => {
                let measured = terminal.size().ok().map(|size| size.width);
                self.width_control
                    .reload_target(&self.current.theme, measured, &self.diag);
            }
            SidebarEvent::BodyFilterChanged => {
                self.filter_observed_at = Some(Instant::now());
                let filter = crate::sidebar::body_filter::load(&self.runtime);
                if set_make_up_filter(&mut self.ui, &self.current, filter) {
                    self.dirty = true;
                }
            }
            event @ SidebarEvent::Notify { .. } => {
                self.handle_notification(terminal, event);
            }
            SidebarEvent::FocusStranded {
                pane_id,
                generation,
                clients,
            } => {
                if self.config.own_pane.as_ref() != Some(&pane_id) {
                    trace!(pane = %pane_id, own_pane = ?self.config.own_pane, generation, "sidebar ignored foreign strand");
                    return;
                }
                let repair = PendingFocusRepair {
                    pane_id,
                    generation,
                    clients,
                    sent_at_ms,
                };
                if !self.handle_focus_stranded(&repair) {
                    if let Some(replaced) = self.pending_focus_repair.replace(repair) {
                        self.record_abandoned_focus_repair(
                            &replaced,
                            "superseded by a newer strand",
                        );
                    }
                    fetch.request(FetchRequest::fresh_panes(), true);
                }
            }
            SidebarEvent::FocusIntent { .. } => {
                self.fold_fused_now();
            }
            event if event.is_overlay() => {
                self.handle_overlay_event(fetch, event, sent_at_ms);
            }
            // Identity-free nudges — store deltas, `PanesChanged`, a `PaneOpened` without a command: nothing to fuse, so refetch, bypassing the pane cache when the event says topology moved.
            _ => {
                fetch.request(
                    if requests_verification {
                        FetchRequest::fresh_panes()
                    } else {
                        FetchRequest::default()
                    },
                    true,
                );
            }
        }
    }

    fn handle_notification(&mut self, terminal: &mut Terminal<PaneBackend>, event: SidebarEvent) {
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
            &self.config,
            terminal,
            &self.current,
            BellNotice {
                title: &title,
                body: &body,
                panes: &panes,
                recheck_unread,
                kind,
            },
            &self.diag,
        ) {
            Ok(true) => self.remind.note_ring(crate::utils::time::unix_now_ms()),
            Ok(false) => {}
            Err(err) => debug!(error = %err, "terminal notification emit failed"),
        }
    }

    fn handle_focus_stranded(&mut self, repair: &PendingFocusRepair) -> bool {
        let now_ms = crate::utils::time::unix_now_ms();
        let own_pane = self.config.own_pane.clone();
        let verdict = focus_stranded_target(
            &self.current,
            &self.ui,
            &repair.pane_id,
            &repair.clients,
            own_pane.as_ref(),
            repair.sent_at_ms,
            now_ms,
        );
        let target = match verdict {
            Ok(target) => target,
            Err(reason) => {
                debug!(
                    pane = %repair.pane_id,
                    own_pane = ?own_pane,
                    generation = repair.generation,
                    evidence = ?repair.clients,
                    own_view = ?self.current.own_view,
                    client_views = ?self.current.client_views,
                    age_ms = now_ms.saturating_sub(repair.sent_at_ms),
                    reason,
                    "sidebar focus repair deferred",
                );
                return false;
            }
        };
        let expected = (repair.pane_id.mux() == MuxName::Zellij).then(|| repair.clients.clone());
        let dispatch = FocusRepairDispatch {
            generation: repair.generation,
            deadline_ms: expected.as_ref().map(|_| {
                repair
                    .sent_at_ms
                    .saturating_add(duration_millis(FOCUS_STRANDED_EVENT_TTL))
            }),
        };
        spawn_pane_focus(
            target,
            &self.config.session_name,
            self.runtime.clone(),
            crate::mux::focus_anchor::FocusOrigin::AutomaticRepair,
            expected,
            (self.ui.scroll_offset, Some(self.ui.last_order.clone())),
            Some(dispatch),
        );
        true
    }

    /// Re-attempt a strand the last fold could not yet act on: the snapshot may
    /// still lack the owning view or a visible working sibling row. The repair
    /// stays viable for the event's whole TTL and retries on each fold. Past
    /// the TTL it is abandoned with a record.
    fn retry_pending_focus_repair(&mut self) {
        let Some(repair) = self.pending_focus_repair.take() else {
            return;
        };
        if self.handle_focus_stranded(&repair) {
            return;
        }
        let now_ms = crate::utils::time::unix_now_ms();
        if focus_repair_still_viable(repair.sent_at_ms, now_ms) {
            self.pending_focus_repair = Some(repair);
        } else {
            self.record_abandoned_focus_repair(
                &repair,
                "no unambiguous working sibling row within the strand TTL",
            );
        }
    }

    /// A strand RimZ decided not to act on still gets a durable diagnostic
    /// record explaining why it lapsed.
    fn record_abandoned_focus_repair(&self, repair: &PendingFocusRepair, reason: &str) {
        use crate::diag::focus_repair::{FocusRepairOutcome, FocusRepairRecord};
        debug!(
            pane = %repair.pane_id,
            generation = repair.generation,
            reason,
            "sidebar focus repair abandoned",
        );
        crate::diag::focus_repair::spawn_append(
            &self.runtime,
            &FocusRepairRecord {
                at: jiff::Timestamp::now(),
                nonce: None,
                workspace_id: self.runtime.workspace_id.clone(),
                session_name: self.config.session_name.clone(),
                generation: repair.generation,
                evidence: repair.clients.clone(),
                observed: Vec::new(),
                attempts: 0,
                target: repair.pane_id.clone(),
                outcome: FocusRepairOutcome::Failed,
                error: Some(reason.to_owned()),
            },
        );
    }

    fn handle_overlay_event(
        &mut self,
        fetch: &mut FetchDispatcher,
        event: SidebarEvent,
        sent_at_ms: u64,
    ) {
        // An overlay event fuses into the in-memory state and paints this
        // frame. A topology overlay also asks the producer to verify with a
        // real pull, which supersedes the overlay once its fresh frame
        // folds in. A resize-grow paint hold stays held until a pulled
        // sibling-count verdict releases it.
        let own = self.config.own_pane.as_ref();
        let own_focused = matches!(&event, SidebarEvent::FocusChanged { focused, .. }
            if own.is_some_and(|pane| focused.contains(pane)));
        let own_unfocused = matches!(&event, SidebarEvent::FocusChanged { unfocused, .. }
            if own.is_some_and(|pane| unfocused.contains(pane)));
        let requests_verification = event.requests_producer_verification();
        let now_ms = crate::utils::time::unix_now_ms();
        self.events().append(event, sent_at_ms, now_ms);
        self.fold_fused_now();
        if !self.should_exit && (requests_verification || own_focused) {
            fetch.request(FetchRequest::fresh_panes(), true);
        }
        if own_focused {
            self.optimistic_watch_until = Some(Instant::now() + FOCUS_RESUME_WATCH_WINDOW);
        } else if own_unfocused {
            self.optimistic_watch_until = None;
            self.ui.help_visible = false;
            if self.ui.search_draft.take().is_some() {
                reconcile_selection(&mut self.ui, &self.current, None);
                self.dirty = true;
            }
        }
    }

    /// `settled_width` is the pane width observed after the resize wakeup,
    /// probed once at the dispatch site so tests can drive the feedback path.
    pub(super) fn on_resize(
        &mut self,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<PaneBackend>,
        settled_width: Option<u16>,
    ) -> Result<()> {
        self.caps_refresh_deadline = Some(Instant::now() + RESIZE_CAPS_SETTLE);
        self.optimistic_watch_until = Some(Instant::now() + FOCUS_RESUME_WATCH_WINDOW);
        // Once a sibling has been seen, hold only a grow beyond the configured
        // cap or room override: that is the shape of space freed by a closing
        // sibling. Startup and attach relayouts land at the legitimate width
        // and paint immediately.
        let held_grow = match settled_width {
            Some(width) => {
                let grew = resize_grew(self.prev_width, width);
                self.prev_width = Some(width);
                grew && grow_beyond_legit(width, self.width_control.max_legit_cols())
            }
            None => false,
        };
        self.run_width_control(terminal, WidthControlTrigger::ResizeFeedback);
        if held_grow && self.self_close.seen_sibling {
            self.dirty = true;
            self.paint_hold
                .engage(Instant::now(), crate::utils::time::unix_now_ms());
        } else {
            if self.apply_input([Wakeup::Resize], terminal)?.redraw {
                self.dirty = false;
            }
            // A safe-width paint just landed; drop any stale hold a prior grow
            // left pending so it cannot suppress this frame.
            self.paint_hold.release();
        }
        self.last_self_close_check = Instant::now();
        // The heartbeat carries this pane's size for `sidebar frame`: restamp
        // it on this pass rather than a beat later.
        self.last_heartbeat = None;
        // A resize is the mux telling us topology changed. Pull a fresh pane
        // list through the host and require a cache produced after
        // this signal.
        fetch.request(FetchRequest::fresh_panes(), true);
        Ok(())
    }

    pub(super) fn run_width_control(
        &mut self,
        terminal: &mut Terminal<PaneBackend>,
        trigger: WidthControlTrigger,
    ) {
        let Ok(size) = terminal.size() else {
            return;
        };
        self.width_control.observe(size.width, trigger, &self.diag);
    }

    fn run_width_control_backstop(&mut self, terminal: &mut Terminal<PaneBackend>) {
        self.width_control.backstop(
            terminal.size().ok().map(|size| size.width),
            self.current
                .own_view
                .as_ref()
                .map(|view| view.sibling_count),
            self.current.panes_observed_at_ms,
            &self.diag,
        );
    }

    #[cfg(test)]
    fn refresh_pet_render_caps_with(
        &mut self,
        mux: MuxName,
        session_name: &str,
        terminal: &Terminal<PaneBackend>,
        detect: impl FnOnce(MuxName, &str, PixelRenderCaps) -> PixelRenderCaps,
    ) {
        self.paint
            .refresh_caps_with(mux, session_name, terminal.backend().cell_aspect(), detect);
    }

    fn refresh_pet_render_caps_if_stale_with(
        &mut self,
        mux: MuxName,
        session_name: &str,
        now: Instant,
        terminal: &Terminal<PaneBackend>,
        detect: impl FnOnce(MuxName, &str, PixelRenderCaps) -> PixelRenderCaps,
    ) -> bool {
        let changed = if let Some(deadline) = self.caps_refresh_deadline {
            if now < deadline {
                return false;
            }
            self.caps_refresh_deadline = None;
            self.paint.refresh_caps_with(
                mux,
                session_name,
                terminal.backend().cell_aspect(),
                detect,
            )
        } else {
            self.paint
                .refresh_caps_if_stale_with(mux, session_name, now, detect)
        };
        self.dirty |= changed;
        changed
    }

    pub(super) fn on_input(
        &mut self,
        wakeup: Wakeup,
        terminal: &mut Terminal<PaneBackend>,
        fetch: &mut FetchDispatcher,
    ) -> Result<()> {
        self.on_inputs([wakeup], terminal, fetch)
    }

    pub(super) fn on_inputs(
        &mut self,
        wakeups: impl IntoIterator<Item = Wakeup>,
        terminal: &mut Terminal<PaneBackend>,
        fetch: &mut FetchDispatcher,
    ) -> Result<()> {
        self.result_rx.invalidate();
        let applied = self.apply_input(wakeups, terminal)?;
        if applied.redraw && self.watched() {
            // Key/mouse input paints synchronously for instant feedback; a
            // paint settles any frame the loop owed.
            self.dirty = false;
        }
        let interacted = applied.redraw || !applied.effects.is_empty();
        if interacted && !applied.effects.contains(&InputEffect::Reload) {
            order_hold::arm_order_hold(&mut self.ui, jiff::Timestamp::now().as_millisecond());
        }
        for effect in applied.effects {
            match effect {
                InputEffect::Focus(pane) => {
                    spawn_pane_focus(
                        pane,
                        &self.config.session_name,
                        self.runtime.clone(),
                        crate::mux::focus_anchor::FocusOrigin::User,
                        None,
                        (self.ui.scroll_offset, Some(self.ui.last_order.clone())),
                        None,
                    );
                }
                InputEffect::Width(dir) => {
                    let Ok(size) = terminal.size() else {
                        return Ok(());
                    };
                    self.width_control.adjust(size.width, dir);
                }
                InputEffect::MarkRead(row_id) => self.mark_row_read(fetch, &row_id),
                InputEffect::MarkUnread(row_id) => self.mark_row_unread(fetch, &row_id),
                InputEffect::MarkAllRead => self.mark_all_read(fetch),
                InputEffect::SyncFilter(filter) => self.persist_body_filter(filter),
                InputEffect::Reload => (self.request_reload)(),
                InputEffect::DismissAlert => {}
            }
        }
        Ok(())
    }

    fn persist_body_filter(&mut self, filter: BodyLens) {
        self.filter_observed_at = Some(Instant::now());
        let persisted = crate::sidebar::body_filter::write(&self.runtime, &filter);
        if let Err(err) = persisted {
            debug!(error = %err, "sidebar body filter write failed");
            return;
        }
        if let Err(err) = crate::wakeup::broadcast(
            &self.runtime,
            Some(&self.config.session_name),
            SidebarEvent::BodyFilterChanged,
        ) {
            debug!(error = %err, "sidebar body filter broadcast failed");
        }
    }

    /// Apply a single input or queued wheel run to the local UI in order. Input
    /// never changes store data, so it redraws the *current* snapshot and may
    /// jump focus, but it never re-runs the snapshot burst — that per-keystroke
    /// refetch was the input lag. Input paints synchronously so a keypress or
    /// click feels instant rather than waiting for the next frame. Wheel ticks
    /// share one paint after the run, never an intermediate paint. A hidden
    /// attachment keeps the redraw pending until it is watched.
    fn apply_input(
        &mut self,
        wakeups: impl IntoIterator<Item = Wakeup>,
        terminal: &mut Terminal<PaneBackend>,
    ) -> Result<InputOutcome> {
        let keys = &self.current.sidebar.keys;
        if self.nav_keys.0 != *keys {
            self.nav_keys = (keys.clone(), super::NavKeymap::from_config(keys));
        }
        let mut outcome = InputOutcome::default();
        for wakeup in wakeups {
            let wheel = matches!(wakeup, Wakeup::Scroll { .. });
            let alert_active = self.alert_active();
            let applied = handle_wakeup(
                wakeup,
                &mut self.ui,
                &self.current,
                &self.nav_keys.1,
                alert_active,
            );
            if applied.redraw && !wheel {
                self.ui.scroll_ease.cancel();
            }
            if applied.effects.contains(&InputEffect::DismissAlert) {
                self.health.alert = None;
            }
            outcome.redraw |= applied.redraw;
            outcome.effects.extend(applied.effects);
        }
        if outcome.redraw {
            if !self.watched() {
                self.dirty = true;
                return Ok(outcome);
            }
            // Carry the live spin phase into the instant paint so a keypress
            // mid-spin never rewinds the animation to a stale frame.
            frame_clock(
                self.anim_start,
                self.current.theme.display.resolved_refresh_ms(),
            )
            .stamp(&mut self.ui);
            let alert_active = self.alert_active();
            self.paint
                .refresh_view(&mut self.ui, &self.current, alert_active);
            self.paint.draw_and_paint(
                terminal,
                &self.current,
                self.health.alert.as_ref(),
                &mut self.ui,
            )?;
            let now = Instant::now();
            let interval = render::animation_interval(
                &self.current,
                &self.ui,
                self.ui.animation_phase,
                alert_active,
            )
            .unwrap_or(animation_frame(&self.current));
            // This paint satisfied a lapsed boundary; a sooner pending one stands.
            let deadline = now + interval;
            if self.next_frame <= now || self.next_frame > deadline {
                self.next_frame = deadline;
            }
        }
        Ok(outcome)
    }

    /// Mark a row read without jumping (`m`): write the room receipt,
    /// clear the row locally for an instant repaint, trace the clear, wake the
    /// room so the host prunes the episode and peer tabs converge, and refetch
    /// so the receipt lands in the pulled snapshot. A no-op when the row is
    /// already read.
    fn mark_row_read(&mut self, fetch: &mut FetchDispatcher, row_id: &str) {
        if self.ui.unread_guard.as_deref() == Some(row_id) {
            self.ui.unread_guard = None;
        }
        let now = jiff::Timestamp::now();
        let marks = ReadMarks::load(&self.runtime);
        let clear = read_receipt_for_row(
            &self.current,
            Some(row_id),
            UnreadClearCause::MarkRead,
            &marks,
            now,
        );
        self.apply_read_clear(clear, now, fetch);
    }

    fn apply_read_clear(
        &mut self,
        clear: ReadClear,
        now: jiff::Timestamp,
        fetch: &mut FetchDispatcher,
    ) {
        if clear.ids.is_empty() {
            return;
        }
        if let Err(err) = write_read_marks(&self.runtime, clear.ids.clone(), now.as_millisecond()) {
            warn!(error = %err, "mark-read receipt write failed");
            return;
        }
        set_rows_unread(&mut self.current, &clear.ids, false);
        emit_unread_cleared_trace(&self.diag, &clear.trace);
        self.commit_local_mark(fetch);
    }

    /// Mark every readable row read without jumping (`M`): write room receipts
    /// for every unread or needs-a-look row, clear local unread bits, and refetch
    /// so peers converge through the producer.
    fn mark_all_read(&mut self, fetch: &mut FetchDispatcher) {
        self.ui.unread_guard = None;
        let now = jiff::Timestamp::now();
        let marks = ReadMarks::load(&self.runtime);
        let clear = read_receipts_for_all(&self.current, UnreadClearCause::MarkRead, &marks, now);
        self.apply_read_clear(clear, now, fetch);
    }

    /// Re-flag a row unread without jumping (`m` on a read row): open a durable
    /// episode through the shared mark-unread path, set the row locally for an
    /// instant repaint, trace the open, wake the room, and refetch so the episode
    /// lands in the pulled snapshot. A no-op when the row has left the room.
    fn mark_row_unread(&mut self, fetch: &mut FetchDispatcher, row_id: &str) {
        let Some(row) = self.current.rows().find(|row| row.id == row_id).cloned() else {
            return;
        };
        let now_ms = jiff::Timestamp::now().as_millisecond();
        let opened =
            match unread::mark_rows_unread(&self.runtime, std::slice::from_ref(&row), now_ms) {
                Ok(opened) => opened,
                Err(err) => {
                    warn!(error = %err, "mark-unread episode write failed");
                    return;
                }
            };
        set_rows_unread(&mut self.current, std::slice::from_ref(&row.id), true);
        self.ui.unread_guard = Some(row.id.clone());
        for item in &opened {
            self.diag.trace_notify(item.trace_event());
        }
        self.commit_local_mark(fetch);
    }

    fn commit_local_mark(&mut self, fetch: &mut FetchDispatcher) {
        wake_room(&self.runtime);
        self.dirty = true;
        self.next_frame = Instant::now();
        fetch.request(FetchRequest::default(), true);
    }

    fn run_maintenance(&mut self, fetch: &mut FetchDispatcher, terminal: &Terminal<PaneBackend>) {
        let own_pane = self.config.own_pane.clone();
        let room_caps = self.room_caps.clone();
        self.refresh_pet_render_caps_if_stale_with(
            self.config.mux,
            &self.config.session_name.clone(),
            Instant::now(),
            terminal,
            |mux, session_name, prev| {
                // A probe publishes its whole sample at once; a panic leaves
                // the previous sample intact even if the lock was poisoned.
                room_caps
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .detect(mux, session_name, prev, own_pane.as_ref())
            },
        );
        // Snapshot wakeups are a latency hint, not the only correctness path.
        // `rimz reload` replaces the renderer in place and a ready-result
        // datagram can be lost around socket teardown/rebind; the frame/tick
        // path still drains pending results so startup cannot strand the
        // placeholder cockpit.
        self.on_snapshot(fetch);
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
        if self.fetched_at.elapsed() >= tick_for(self.config.tick_seconds) {
            fetch.request(FetchRequest::default(), false);
        }

        // Heartbeat: fast in-process atomic write on the main thread so the
        // exit path never races a background writer.
        if heartbeat_write_due(self.last_heartbeat) {
            self.last_heartbeat = Some(Instant::now());
            if let Err(err) = write_heartbeat(
                &self.config,
                &self.runtime,
                &self.host_wake_path,
                terminal.backend().pane_size(),
            ) {
                warn!(
                    session = %self.config.session_name,
                    error = %err,
                    "heartbeat write failed",
                );
            }
        }

        // Self-close watchdog: if no resize or presence event fired, ask the
        // normal snapshot path to refresh so the snapshot's own-view count can
        // close a lone sidebar. Once a zero-sibling verdict is pending, force a
        // new fold so a prior cycle cannot absorb this renderer-local change
        // before the confirm timer can close it.
        if self.last_self_close_check.elapsed() >= SELF_CLOSE_WATCHDOG {
            self.last_self_close_check = Instant::now();
            let request = if self.self_close.confirming_empty() {
                FetchRequest::fresh_panes()
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

    fn maybe_remind(&mut self, terminal: &mut Terminal<PaneBackend>) {
        self.remind
            .maybe_remind(&self.config, terminal, &self.current, &self.diag);
    }

    pub(super) fn clear_pixel(&mut self, terminal: &mut Terminal<PaneBackend>) {
        if let Err(err) = self.paint.clear(terminal.backend_mut()) {
            debug!(error = %err, "pet pixel clear failed");
        }
    }

    fn paint_frame_if_due(
        &mut self,
        terminal: &mut Terminal<PaneBackend>,
        active: bool,
    ) -> Result<()> {
        let now = Instant::now();
        let paint_blocked = self.prepare_paint(terminal, now);
        let watched = self.watched();
        let foreground = self.foreground_paint_due(now, active, watched);
        // Once the tab has emptied, never paint again. A grow resize also
        // defers its paint until the sibling-count verdict releases the hold.
        if !self.should_exit && !self.self_close.confirming_empty() && !paint_blocked && foreground
        {
            self.paint_now(terminal, now)?;
        } else if !active && !self.dirty {
            // Idle re-arm only: with a fold pending, the armed boundary must
            // hold so a paint already due within one frame is not pushed out.
            self.next_frame = now + animation_frame(&self.current);
        }
        Ok(())
    }

    fn prepare_paint(&mut self, terminal: &mut Terminal<PaneBackend>, now: Instant) -> bool {
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
        watched && (active || self.dirty) && now >= self.next_frame
    }

    fn paint_now(&mut self, terminal: &mut Terminal<PaneBackend>, now: Instant) -> Result<()> {
        frame_clock(
            self.anim_start,
            self.current.theme.display.resolved_refresh_ms(),
        )
        .stamp(&mut self.ui);
        let alert_active = self.alert_active();
        let animating = render::animation_interval(
            &self.current,
            &self.ui,
            self.ui.animation_phase,
            alert_active,
        )
        .is_some();
        if self.dirty || animating {
            self.paint
                .refresh_view(&mut self.ui, &self.current, alert_active);
            self.paint.draw_and_paint(
                terminal,
                &self.current,
                self.health.alert.as_ref(),
                &mut self.ui,
            )?;
            self.dirty = false;
        }
        self.next_frame = next_frame_after(
            self.next_frame,
            now,
            render::animation_interval(
                &self.current,
                &self.ui,
                self.ui.animation_phase,
                alert_active,
            )
            .unwrap_or(animation_frame(&self.current)),
        );
        Ok(())
    }

    fn arm_paint_hold_on_grow(&mut self, width: u16, now: Instant) -> bool {
        if !self.paint_hold.is_engaged()
            && self.self_close.seen_sibling
            && resize_grew(self.prev_width, width)
            && grow_beyond_legit(width, self.width_control.max_legit_cols())
        {
            self.paint_hold
                .engage(now, crate::utils::time::unix_now_ms());
            return true;
        }
        false
    }

    /// Fold one fetch outcome into the render state: gate it against the
    /// last-known-good frame, update health, snapshot, and selection, and
    /// report whether the loop should exit.
    pub(super) fn apply_fetch_outcome(
        &mut self,
        update: FetchUpdate,
        allow_shared_filter_sync: bool,
    ) -> ApplyOutcome {
        let (update, _) = update.into_parts();
        let snapshot_ok = matches!(&update, FetchUpdate::Snapshot { .. });
        let application = match update {
            FetchUpdate::Snapshot {
                snapshot, source, ..
            } => FetchApplication {
                snapshot: Ok(*snapshot),
                source: Some(source),
            },
            FetchUpdate::Failed { error, .. } => FetchApplication {
                snapshot: Err(error),
                source: None,
            },
            FetchUpdate::Shared { .. } => unreachable!("unwrapped above"),
            FetchUpdate::Unchanged { .. } => {
                unreachable!("clock-only delivery bypasses the content path")
            }
        };
        let (prev_good, rejected, now) = self.commit_fetch(application);
        let authoritative_filter_fold = allow_shared_filter_sync
            && snapshot_ok
            && !rejected
            && self.fold_inputs.as_ref().is_none_or(|inputs| {
                self.filter_observed_at
                    .is_none_or(|observed| inputs.sampled_at > observed)
            });
        let prev_selected = self.ui.selected_pane.clone();
        let (focused_pane, cleared) = self.sweep_read_receipts(now);
        self.reconcile_selection_and_order(
            &prev_good,
            prev_selected,
            focused_pane,
            cleared,
            authoritative_filter_fold,
            now.as_millisecond(),
        );
        self.fold_spend_rolls();
        self.exit_verdict(rejected)
    }

    fn exit_verdict(&mut self, rejected: bool) -> ApplyOutcome {
        // A renderer degraded this long is non-functional. Close the attachment so the supervisor reattaches in place while the mux/store outage clears.
        if degraded_too_long(&self.health, Timestamp::now()) {
            warn!(
                target: SIDEBAR_HEALTH_TARGET,
                session = %self.config.session_name,
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
        if self.self_close.should_close(
            self.current
                .own_view
                .as_ref()
                .map(|view| view.sibling_count),
            Instant::now(),
        ) {
            debug!(
                session = %self.config.session_name,
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
    ) -> (SidebarSnapshot, bool, Timestamp) {
        // The gate compares the incoming snapshot against the last frame we actually
        // committed; `current` still holds it until we overwrite it below.
        let fetch_was_ok = application.snapshot.is_ok();
        let fetch_failure = application.snapshot.as_ref().err().cloned();
        let producer_verdict = application.source == Some(SnapshotSource::Produced);
        let mut computed = compute_next_state(application.snapshot, &self.current, &self.health);
        if fetch_was_ok && !producer_verdict {
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
            &self.diag,
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
        let prev_good = std::mem::replace(&mut self.current, state.snapshot);
        if self.alert_active() && self.ui.search_draft.take().is_some() {
            self.dirty = true;
        }
        prev_good
    }

    fn sweep_read_receipts(&mut self, now: Timestamp) -> (Option<PaneId>, bool) {
        let focused_pane = session_focus_baseline(&self.current, self.config.own_pane.as_ref());
        let viewing_register_pane = self
            .current
            .focused_pane
            .as_ref()
            .is_some_and(|pane| self.current.viewed_panes.contains(pane));
        let focused_row_id = focused_pane
            .as_ref()
            .filter(|pane| {
                viewing_register_pane && self.current.focused_pane.as_ref() == Some(*pane)
            })
            .and_then(|pane| row_id_of_pane(&self.current, pane));
        let marks = ReadMarks::load(&self.runtime);
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
        if let Err(err) = write_read_marks(
            &self.runtime,
            clear.ids.iter().filter(|id| live.contains(*id)).cloned(),
            now.as_millisecond(),
        ) {
            debug!(error = %err, "sidebar read-mark write failed");
        }
        set_rows_unread(&mut self.current, &clear.ids, false);
        emit_unread_cleared_trace(&self.diag, &clear.trace);
        (focused_pane, !clear.ids.is_empty())
    }

    fn sweep_tab_read_receipts(
        &mut self,
        focused_row_id: Option<&str>,
        marks: &ReadMarks,
        now: Timestamp,
        clear: &mut ReadClear,
    ) {
        let (Some(view), Some(own_pane)) = (
            self.current.own_view.as_ref(),
            self.config.own_pane.as_ref(),
        ) else {
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
        let derived_focus_pane = focused_pane.is_some();
        if authoritative_filter_fold {
            let shared_filter = if let Some(inputs) = &self.fold_inputs {
                inputs.filter.clone()
            } else {
                self.filter_observed_at = Some(Instant::now());
                crate::sidebar::body_filter::load(&self.runtime)
            };
            set_make_up_filter(&mut self.ui, &self.current, shared_filter);
        }
        let previous_filter = self.ui.make_up_filter.filter;
        reconcile_selection(&mut self.ui, &self.current, focused_pane);
        if authoritative_filter_fold
            && !self.current.worktree_groups.is_empty()
            && previous_filter.is_some()
            && self.ui.make_up_filter.filter.is_none()
        {
            self.persist_body_filter(self.ui.make_up_filter.clone());
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

    fn fold_spend_rolls(&mut self) {
        frame_clock(
            self.anim_start,
            self.current.theme.display.resolved_refresh_ms(),
        )
        .stamp(&mut self.ui);
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
        update: FetchUpdate,
        allow_shared_filter_sync: bool,
        seed: bool,
        fused_event_sent_at_ms: Option<u64>,
    ) -> bool {
        let trace = match &update {
            FetchUpdate::Snapshot { source, phase, .. } => Some((*source, *phase)),
            _ => None,
        };
        let selected_before = self.ui.selected_pane.clone();
        let applied = self.apply_fetch_outcome(update, allow_shared_filter_sync);
        if let Some((source, phase)) = trace {
            self.diag.trace_focus(
                FocusTraceEvent::FoldDecided {
                    source: match source {
                        SnapshotSource::Produced => "produced",
                        SnapshotSource::Cached => "published",
                    }
                    .to_owned(),
                    phase: match phase {
                        FetchPhase::Interim => "interim",
                        FetchPhase::Final => "final",
                    }
                    .to_owned(),
                    seed,
                    panes_produced_at_ms: self.current.panes_produced_at_ms,
                    panes_observed_at_ms: self.current.panes_observed_at_ms,
                    snapshot_focused_pane: self.current.focused_pane.clone(),
                    fused_event_sent_at_ms,
                    own_view: self.current.own_view.is_some(),
                    baseline: session_focus_baseline(&self.current, self.config.own_pane.as_ref()),
                    selected_before,
                    selected_after: self.ui.selected_pane.clone(),
                },
                HashSet::new(),
            );
        }
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

    fn focus_anchor(&mut self) -> Option<crate::mux::focus_anchor::FocusAnchor> {
        if let Some(inputs) = &self.fold_inputs {
            if let Some((observed, anchor)) = &self.observed_anchor
                && inputs.sampled_at <= *observed
            {
                return anchor.clone();
            }
            self.observed_anchor = Some((inputs.sampled_at, inputs.anchor.clone()));
            return inputs.anchor.clone();
        }
        let observed = Instant::now();
        let anchor = crate::mux::focus_anchor::load(&self.runtime);
        if anchor.is_none()
            && let Some((_, Some(previous))) = &self.observed_anchor
        {
            self.retired_anchor = Some((observed, previous.issued_at_ms));
        }
        self.observed_anchor = Some((observed, anchor.clone()));
        anchor
    }

    fn apply_focus_anchor(&mut self) {
        let Some(anchor) = self.focus_anchor() else {
            return;
        };
        let now_ms = crate::utils::time::unix_now_ms();
        let presentation_at_ms = anchor.applied_at_ms.unwrap_or(anchor.issued_at_ms);
        if self
            .current
            .row_owning_pane(&anchor.pane_id)
            .and_then(|row| row.pane.as_ref())
            .is_some_and(|pane| self.ui.selected_pane.as_ref() == Some(&pane.pane_id))
            && anchor.issued_at_ms > self.ui.last_focus_anchor_ms
            && crate::mux::focus_anchor::is_fresh(presentation_at_ms, now_ms)
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
            if crate::mux::focus_anchor::clear_matching(&self.runtime, anchor.nonce) {
                let retired = Instant::now();
                self.observed_anchor = Some((retired, None));
                self.retired_anchor = Some((retired, anchor.issued_at_ms));
            }
            self.confirmed_focus_intent_ms = 0;
        }
    }

    fn pending_focus_intent(&mut self, now_ms: u64) -> Option<FocusPresentation> {
        let Some(anchor) = self.focus_anchor() else {
            // A live retirement must fence an older cut's focus observation,
            // not revive either the intent or the pre-jump selection.
            return self
                .fold_inputs
                .as_ref()
                .filter(|inputs| {
                    self.retired_anchor
                        .as_ref()
                        .is_some_and(|(retired, issued_at_ms)| {
                            inputs.sampled_at <= *retired
                                && !self.events().has_focus_since(*issued_at_ms, now_ms)
                        })
                })
                .map(|_| FocusPresentation::Fence);
        };
        let outcome = if focus_intent_confirmed_from(
            &self.last_focus_observation,
            &self.events(),
            &anchor,
            now_ms,
        ) {
            FocusObservationOutcome::Confirmed
        } else {
            crate::mux::focus_anchor::observation_outcome_from(
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
                if crate::mux::focus_anchor::clear_matching(&self.runtime, anchor.nonce) {
                    let retired = Instant::now();
                    self.observed_anchor = Some((retired, None));
                    self.retired_anchor = Some((retired, anchor.issued_at_ms));
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
        anchor: &crate::mux::focus_anchor::FocusAnchor,
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
            &self.runtime,
            &FocusRepairRecord {
                at: jiff::Timestamp::now(),
                nonce: Some(anchor.nonce.to_string()),
                workspace_id: self.runtime.workspace_id.clone(),
                session_name: anchor.session_name.clone(),
                generation: anchor.repair_generation.unwrap_or_default(),
                evidence: anchor.pre_action.clone(),
                observed: Vec::new(),
                attempts: 0,
                target: anchor.pane_id.clone(),
                outcome,
                error: None,
            },
        );
    }

    fn events(&self) -> MutexGuard<'_, EventStore> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn observe_commit(&mut self) {
        let Some(common) = &self.observation else {
            return;
        };
        let now_ms = crate::utils::time::unix_now_ms();
        let sig = observe::OwnFrameSig::extract(
            &self.current,
            (self.last_pulled_rows, common.pulled_panes_produced_at_ms),
            &self.events(),
            self.gate.reject_streak,
            self.health.failure_streak,
            now_ms,
        );
        self.observer
            .observe_into(sig, &self.observe_tx, &self.config.instance_id);
    }
}

fn focus_stranded_target(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
    stranded_pane_id: &PaneId,
    evidence: &[crate::pane::ClientPaneView],
    own_pane_id: Option<&PaneId>,
    sent_at_ms: u64,
    now_ms: u64,
) -> std::result::Result<PaneId, &'static str> {
    let Some(own_pane_id) = own_pane_id else {
        return Err("missing owner pane");
    };
    if own_pane_id != stranded_pane_id {
        return Err("foreign owner pane");
    }
    if now_ms.saturating_sub(sent_at_ms) > duration_millis(FOCUS_STRANDED_EVENT_TTL) {
        return Err("strand TTL expired");
    }
    // `focus-pane-id` is session-global: with clients viewing distinct panes it
    // would yank a client looking elsewhere. Only observed client views count,
    // never fused focus or intents: Zellij checks settled evidence, fenced by a
    // live sample at dispatch; tmux checks the pulled client sample.
    if stranded_pane_id.mux() == MuxName::Zellij {
        let Some(first) = evidence.first() else {
            return Err("empty settled evidence");
        };
        if evidence.iter().any(|view| view.pane_id != first.pane_id) {
            return Err("distinct panes in settled evidence");
        }
    } else if let Some(first) = snapshot.client_views.first()
        && snapshot
            .client_views
            .iter()
            .any(|view| view.pane_id != first.pane_id)
    {
        return Err("distinct panes in pulled client views");
    }
    let Some(view) = snapshot.own_view.as_ref() else {
        return Err("missing own view");
    };
    if let Some(baseline) = ui.baseline_pane.as_ref()
        && view.working_pane_ids.contains(baseline)
        && row_index_of_pane(snapshot, &BodyLens::default(), baseline).is_some()
    {
        return Ok(baseline.clone());
    }
    view.working_pane_ids
        .iter()
        .find(|pane| row_index_of_pane(snapshot, &BodyLens::default(), pane).is_some())
        .cloned()
        .ok_or("no visible working sibling row")
}

/// Whether a strand the renderer could not yet act on is still worth retrying.
/// The window is the strand event's own TTL — the same bound
/// `focus_stranded_target` applies — so a repair is retried for exactly as long
/// as its evidence is allowed to authorize one.
fn focus_repair_still_viable(sent_at_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(sent_at_ms) <= duration_millis(FOCUS_STRANDED_EVENT_TTL)
}

struct FocusRepairDispatch {
    generation: u64,
    deadline_ms: Option<u64>,
}

struct FocusRequestOutcome {
    requested: Option<std::result::Result<FocusNonce, FocusActionError>>,
    observed: Vec<crate::pane::ClientPaneView>,
    attempts: u32,
}

fn request_focus_until(
    deadline_ms: Option<u64>,
    expected: &[crate::pane::ClientPaneView],
    mut request: impl FnMut() -> std::result::Result<FocusNonce, FocusActionError>,
    mut now_ms: impl FnMut() -> u64,
    mut sleep: impl FnMut(Duration),
) -> FocusRequestOutcome {
    let mut outcome = FocusRequestOutcome {
        requested: None,
        observed: Vec::new(),
        attempts: 0,
    };
    while deadline_ms.is_none_or(|deadline| now_ms() < deadline) {
        outcome.attempts += 1;
        let requested = request();
        match &requested {
            Err(FocusActionError::PreObservationChanged { observed }) => {
                outcome.observed = observed.clone();
            }
            Ok(_) | Err(FocusActionError::Store(_)) => {
                outcome.observed = expected.to_vec();
                outcome.observed.sort();
                outcome.observed.dedup();
            }
            _ => {}
        }
        let retry = matches!(
            &requested,
            Err(FocusActionError::PreObservationChanged { .. })
        );
        outcome.requested = Some(requested);
        if !retry {
            return outcome;
        }
        let Some(deadline) = deadline_ms else {
            return outcome;
        };
        let remaining_ms = deadline.saturating_sub(now_ms());
        if remaining_ms == 0 {
            return outcome;
        }
        sleep(FOCUS_REPAIR_RETRY_STEP.min(Duration::from_millis(remaining_ms)));
    }
    outcome
}

/// Focus the pane on a detached thread so the keypress/click returns instantly:
/// `focus_pane` forks the mux client (`zellij action focus-pane-id` / the tmux
/// equivalent), which must never block the loop. The snapshot-bound pane is
/// focused directly — no `rimz pane focus` child and no per-click pane roster.
/// The shared intent helper takes a fresh attached-client sample immediately
/// before dispatch and serializes concurrent focus actions. An automatic
/// Zellij repair re-samples a mismatching fence until its strand deadline.
/// Errors are logged at `debug!`, not surfaced: a pane recycled in the
/// sub-second window since the snapshot is a benign, self-correcting race, so
/// the line stays local under `RUST_LOG=debug` and off the off-box error
/// channel. The durable request wakes every renderer before mux dispatch;
/// command acceptance applies it, and later native observations confirm or
/// supersede it.
fn spawn_pane_focus(
    pane_id: PaneId,
    session_name: &str,
    runtime: crate::disk::paths::RuntimePaths,
    origin: crate::mux::focus_anchor::FocusOrigin,
    expected_pre_action: Option<Vec<crate::pane::ClientPaneView>>,
    presentation: (usize, Option<crate::mux::focus_anchor::FrozenOrder>),
    repair: Option<FocusRepairDispatch>,
) {
    let session_name = session_name.to_owned();
    std::thread::spawn(move || {
        crate::harness::hibernate::spawn_wake(&runtime, &pane_id);
        let backend = crate::mux::backend_for(pane_id.mux());
        let repair_generation = repair.as_ref().map(|repair| repair.generation);
        let FocusRequestOutcome {
            requested,
            observed,
            attempts,
        } = request_focus_until(
            repair.as_ref().and_then(|repair| repair.deadline_ms),
            expected_pre_action.as_deref().unwrap_or_default(),
            || {
                crate::mux::focus_anchor::request_action(
                    backend.as_ref(),
                    &runtime,
                    &session_name,
                    crate::mux::focus_anchor::FocusActionRequest {
                        pane_id: pane_id.clone(),
                        origin,
                        repair_generation,
                        expected_pre_action: expected_pre_action.as_deref(),
                        offset: presentation.0,
                        order: presentation.1.clone(),
                    },
                )
            },
            crate::utils::time::unix_now_ms,
            std::thread::sleep,
        );
        let outcome = match requested {
            Some(Ok(nonce)) => match crate::mux::focus_anchor::dispatch_action(
                backend.as_ref(),
                &runtime,
                &session_name,
                &pane_id,
                nonce,
                Default::default(),
            ) {
                Ok(true) => (Some(nonce), "accepted_unconfirmed", None),
                Ok(false) => (
                    Some(nonce),
                    "failed",
                    Some("superseded before focus dispatch".to_owned()),
                ),
                Err(err) => (Some(nonce), "failed", Some(err.to_string())),
            },
            Some(Err(err)) => (None, "failed", Some(err.to_string())),
            None => (
                None,
                "failed",
                Some("strand TTL expired before focus request".to_owned()),
            ),
        };
        if let Some(generation) = repair_generation {
            use crate::diag::focus_repair::{FocusRepairOutcome, FocusRepairRecord};
            crate::diag::focus_repair::spawn_append(
                &runtime,
                &FocusRepairRecord {
                    at: jiff::Timestamp::now(),
                    nonce: outcome.0.map(|nonce| nonce.to_string()),
                    workspace_id: runtime.workspace_id.clone(),
                    session_name: session_name.clone(),
                    generation,
                    evidence: expected_pre_action.clone().unwrap_or_default(),
                    observed: observed.clone(),
                    attempts,
                    target: pane_id.clone(),
                    outcome: if outcome.1 == "accepted_unconfirmed" {
                        FocusRepairOutcome::AcceptedUnconfirmed
                    } else {
                        FocusRepairOutcome::Failed
                    },
                    error: outcome.2.clone(),
                },
            );
        }
        if let Some(error) = outcome.2 {
            debug!(pane = %pane_id, error, expected = ?expected_pre_action, observed = ?observed, attempts, "sidebar pane focus failed");
        }
    });
}

/// Ping every sidebar in the room to refold after a mark read/unread — the
/// host prunes or keeps the episode and peer tabs converge on the new state.
fn wake_room(runtime: &RuntimePaths) {
    if let Err(err) = crate::wakeup::wake_store_delta(runtime, None, None) {
        debug!(error = %err, "mark read/unread sidebar wake failed");
    }
}

#[cfg(test)]
mod tests;
