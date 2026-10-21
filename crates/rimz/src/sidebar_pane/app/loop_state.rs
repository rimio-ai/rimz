//! Renderer state transitions and loop-lifetime context, including fetch application, focus repair, maintenance deadlines, and paint eligibility.

use std::collections::HashSet;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::diag::focus_trace::{FocusTraceEvent, PushBlock, PushVerdict};
use crate::diag::record::{
    AnomalyKind, ClickedRowDisplacementCause, RendererExitCause,
    SidebarWidthControlTrigger as WidthControlTrigger,
};
use crate::ids::PaneId;
use crate::mux::focus_anchor::{
    FocusObservation, FocusObservationOutcome, FocusOrigin, FocusPresentation,
};
use crate::observability::SIDEBAR_HEALTH_TARGET;
use crate::sidebar::enrich::project_local;
use crate::sidebar::event_store::EventStore;
use crate::sidebar::observe::{self, ObserveMsg};
use crate::sidebar::read_marks::{ReadMarks, write_read_marks};
#[cfg(test)]
use crate::sidebar::timing::TAB_READ_DWELL;
use crate::sidebar::timing::{FOCUS_STRANDED_EVENT_TTL, LAYOUT_FREEZE};
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

use super::backend::{BufferedFrame, PaneBackend};
use super::clock::LoopClock;
use super::fetch::{
    FetchDispatcher, FetchPhase, FetchRequest, FetchUpdate, ResultReceiver, SnapshotSource,
    WorkspaceFold,
};
use super::focus::{FocusRepairDispatch, spawn_pane_focus};
#[cfg(test)]
use super::focus::{finish_focus_request, request_focus_until};
use super::focus_push::{FocusPeers, FocusPush};
use super::gate::{GateState, gate_remaining};
use super::health::degraded_too_long;
use super::inbox::WakeupInbox;
use super::input::{InputMode, KeyAction, Wakeup, resolve_key};
use super::layout_freeze;
use super::lifecycle::{
    PaintHold, SELF_CLOSE_WATCHDOG, SelfCloseState, grow_beyond_legit, resize_grew,
};
use super::notify::{BellNotice, emit_terminal_notification};
use super::paint::FramePainter;
use super::selection::{
    InputEffect, InputOutcome, click_focus_pane, finish_search, handle_key, handle_mouse_click,
    handle_scroll, reconcile_selection, row_index_of_pane, set_make_up_filter,
};
use super::socket::{heartbeat_write_due, write_heartbeat};
use super::state::{
    ApplyOutcome, ReadClear, emit_diagnostics, emit_unread_cleared_trace, read_receipt_for_row,
    read_receipts_for_all, session_focus_baseline, set_rows_unread,
};
use super::timing::{
    FOCUS_RESUME_WATCH_WINDOW, FRAME_MIN_TIMEOUT, animation_frame, duration_millis, frame_clock,
    next_frame_after, tick_for,
};
use super::view_state::{FocusMarker, FocusRollback, ReadContext, SelectionFold, ViewContext};
use super::width_control::WidthController;
use super::{Result, ServeConfig, fetch_deadline_timeout};
#[cfg(test)]
use crate::mux::focus_anchor::FocusActionError;

const RESIZE_CAPS_SETTLE: Duration = Duration::from_millis(300);

mod shared;

#[derive(Clone, Copy)]
enum ViewScope {
    Scratch,
    Shared(super::views::ViewId),
}

pub(super) type BaselineReporter = Box<dyn Fn(Option<&PaneId>) + Send>;

pub(super) fn own_tab_viewed(
    viewed_panes: &[PaneId],
    own_view: &SidebarOwnView,
    own_pane: &PaneId,
) -> bool {
    viewed_panes
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

struct ClickedRowWatch {
    row_id: String,
    y: isize,
    clicked_at: Instant,
    selection: Option<String>,
    banner_shown: bool,
    /// A wheel glide was in flight at the baseline, so the next paint's motion is the user's own.
    gliding: bool,
}

struct ClickGuard {
    until: Instant,
    interactions: render::FrameInteractions,
    roster: Vec<String>,
}

struct FoldPreparation {
    visible: Option<bool>,
    events: EventStore,
    marks: Arc<ReadMarks>,
    filter: BodyLens,
    anchor: Option<crate::mux::focus_anchor::FocusAnchor>,
}

enum FoldExit {
    Degraded(Option<String>),
    EmptyTab,
}

#[derive(Default)]
struct FoldEffects {
    diagnostics: Vec<crate::diag::record::DiagEvent>,
    traces: Vec<FocusTraceEvent>,
    receipts: Vec<(ReadClear, Timestamp)>,
    filter: Option<BodyLens>,
    retirements: Vec<(
        crate::mux::focus_anchor::FocusAnchor,
        Option<FocusObservationOutcome>,
    )>,
    observations: Vec<observe::OwnFrameSig>,
    anomalies: Vec<observe::AnomalyDraft>,
    degraded: Option<String>,
    exit: Option<FoldExit>,
    acknowledgments: Vec<Arc<FetchUpdate>>,
    invalidate: bool,
    retry_focus_repair: bool,
}

pub(super) struct LoopState {
    #[cfg(test)]
    pub(super) content_applications: u64,
    #[cfg(test)]
    pub(super) projections: u64,
    #[cfg(test)]
    fold_preparations: u64,
    pub(super) view: ViewContext,
    fold_preparation: Option<FoldPreparation>,
    fold_effects: FoldEffects,
    views: Arc<Mutex<super::views::Views>>,
    view_scope: Option<ViewScope>,
    transition_view: Option<super::views::ViewId>,
    view_focus: Option<Option<PaneId>>,
    view_focus_event: Option<u64>,
    view_parked: Option<bool>,
    view_events: Option<EventStore>,
    view_latest: bool,
    was_resolved: bool,
    baseline_seed: Option<PaneId>,
    view_token: Option<(super::views::ViewId, u64, u64)>,
    /// The keymap beside the `[sidebar.keys]` table it was built from, rebuilt only when the snapshot's table differs.
    nav_keys: (crate::config::SidebarKeys, super::NavKeymap),
    config: ServeConfig,
    clock: LoopClock,
    runtime: RuntimePaths,
    host_wake_path: PathBuf,
    diag: crate::diag::DiagSink,
    result_rx: ResultReceiver,
    pending_focus_push: Option<FocusPush>,
    painted_focus_anchor_ms: u64,
    painted_at: Instant,
    painted_height: u16,
    paints: u64,
    paint_reports: Vec<(FocusPush, PushVerdict)>,
    optimistic_watch_until: Option<Instant>,
    pub(super) events: Arc<Mutex<EventStore>>,
    pending_focus_repair: Option<PendingFocusRepair>,
    observer: observe::OwnObserver,
    clicked_row: Option<ClickedRowWatch>,
    click_guard: Option<ClickGuard>,
    hold_expired_since_paint: bool,
    observe_tx: SyncSender<ObserveMsg>,
    self_close: SelfCloseState,
    pub(super) ui: UiState,
    paint: FramePainter,
    pub(super) room_caps: Arc<Mutex<crate::sidebar_pane::pixel::probe::RoomCaps>>,
    pub(super) focus_peers: Option<FocusPeers>,
    input_jump: Option<FocusPeers>,
    pub(super) wake: Option<SyncSender<Wakeup>>,
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
    painted_baseline: Option<PaneId>,
    painted_selection: Option<String>,
}

impl std::ops::Deref for LoopState {
    type Target = ViewContext;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl std::ops::DerefMut for LoopState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.view
    }
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
            if let Some(render::HitTarget::Row(index)) = ui.interactions.target_at(column, row)
                && !ui
                    .painted_roster
                    .get(index)
                    .is_some_and(|id| ui.visible_roster(snapshot).ordinal_of_id(id).is_some())
            {
                return InputOutcome::default();
            }
            if ui.search_draft.is_some() {
                // Resolve the pane against the painted draft before clearing widens the roster.
                let pane = ui
                    .interactions
                    .target_at(column, row)
                    .and_then(|target| click_focus_pane(&target, ui, snapshot));
                if pane.is_some() {
                    let click = handle_mouse_click(column, row, ui, snapshot);
                    let mut outcome = finish_search(ui, snapshot, false);
                    outcome.redraw |= click.redraw;
                    outcome.effects.extend(click.effects);
                    return outcome;
                }
            }
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
        Wakeup::Tick
        | Wakeup::Event(_)
        | Wakeup::Press { .. }
        | Wakeup::Snapshot
        | Wakeup::FocusFailed { .. } => InputOutcome::default(),
    }
}

impl LoopState {
    fn fold_transition<T>(&mut self, advance: impl FnOnce(&mut Self) -> T) -> T {
        let inputs = self.inspect_view(|_, view| view.fold_inputs.clone());
        self.fold_transition_with_inputs(inputs, advance)
    }

    fn fold_transition_with_inputs<T>(
        &mut self,
        inputs: Option<Arc<super::fetch::FoldInputs>>,
        advance: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.view_transition(false, inputs, None, None, advance)
    }

    fn view_transition<T>(
        &mut self,
        input: bool,
        inputs: Option<Arc<super::fetch::FoldInputs>>,
        visible: Option<bool>,
        delivered: Option<Arc<FetchUpdate>>,
        advance: impl FnOnce(&mut Self) -> T,
    ) -> T {
        if self.fold_preparation.is_some() {
            return advance(self);
        }
        #[cfg(test)]
        {
            self.fold_preparations += 1;
        }
        let events = self.events().clone();
        self.fold_preparation = Some(FoldPreparation {
            visible,
            events,
            marks: ReadMarks::load(&self.runtime),
            filter: inputs.as_ref().map_or_else(
                || crate::sidebar::body_filter::load(&self.runtime),
                |inputs| inputs.filter.clone(),
            ),
            anchor: inputs.map_or_else(
                || crate::mux::focus_anchor::load(&self.runtime),
                |inputs| inputs.anchor.clone(),
            ),
        });
        let result = self.with_projected_view(input, true, delivered, advance);
        self.fold_preparation = None;
        self.finish_view_effects();
        result
    }

    fn paint_transition<T>(&mut self, advance: impl FnOnce(&mut Self) -> T) -> T {
        let result = self.with_shared_view(false, true, advance);
        self.finish_view_effects();
        result
    }

    fn finish_view_effects(&mut self) {
        let transition_view = self.transition_view.take();
        let effects = std::mem::take(&mut self.fold_effects);
        emit_diagnostics(&self.diag, effects.diagnostics);
        for trace in effects.traces {
            self.diag.trace_focus(trace, HashSet::new());
        }
        for (clear, now) in effects.receipts {
            if let Err(err) = write_read_marks(&self.runtime, clear.ids, now.as_millisecond()) {
                debug!(error = %err, "sidebar read-mark write failed");
            }
            emit_unread_cleared_trace(&self.diag, &clear.trace);
        }
        if let Some(filter) = effects.filter {
            self.persist_body_filter(filter);
        }
        for (anchor, outcome) in effects.retirements {
            if crate::mux::focus_anchor::clear_matching(&self.runtime, anchor.nonce) {
                self.retire_shared_anchor(transition_view, &anchor);
                if let Some(outcome) = outcome {
                    self.record_focus_resolution(&anchor, outcome);
                }
            }
        }
        for sig in effects.observations {
            self.observer
                .observe_into(sig, &self.observe_tx, &self.config.instance_id);
        }
        for draft in effects.anomalies {
            self.observer
                .send_draft(draft, &self.observe_tx, &self.config.instance_id);
        }
        if let Some(reason) = effects.degraded {
            warn!(target: SIDEBAR_HEALTH_TARGET, reason, "sidebar refresh degraded");
        }
        match effects.exit {
            Some(FoldExit::Degraded(reason)) => warn!(
                target: SIDEBAR_HEALTH_TARGET,
                session = %self.config.session_name,
                reason = reason.as_deref(),
                "sidebar degraded too long; respawning the renderer in place",
            ),
            Some(FoldExit::EmptyTab) => debug!(
                session = %self.config.session_name,
                "sidebar tab emptied; exiting so the pane closes itself",
            ),
            None => {}
        }
        if effects.invalidate {
            self.result_rx.invalidate();
        }
        for delivered in effects.acknowledgments {
            self.result_rx.acknowledge(delivered, false);
        }
        if effects.retry_focus_repair {
            self.retry_pending_focus_repair();
        }
    }

    fn event_snapshot(&self) -> EventStore {
        self.fold_preparation
            .as_ref()
            .map(|prepared| &prepared.events)
            .or(self.view_events.as_ref())
            .cloned()
            .unwrap_or_else(|| self.events().clone())
    }

    #[cfg(feature = "testkit")]
    pub(super) fn prepare_play_pet(&mut self, snapshot: &SidebarSnapshot) -> bool {
        self.paint.prepare_play_pet(&mut self.ui, snapshot)
    }

    #[cfg(feature = "testkit")]
    pub(super) fn play_frame_settled(&self) -> bool {
        !self.dirty
            && self.inspect_view(|ui, _| {
                !ui.tally.any_rolling(ui.animation_phase)
                    && !ui.cost_rolls.any_rolling(ui.animation_phase)
                    && !ui.scroll_ease.in_flight()
            })
    }

    fn parked(&self, now_ms: u64) -> bool {
        let Some(pane) = self.config.own_pane.as_ref() else {
            return false;
        };
        let (sampled, event) = if self.view_scope.is_some() {
            (self.view_parked, self.view_focus_event)
        } else {
            let views = self.views.lock().unwrap_or_else(PoisonError::into_inner);
            (
                views.has_sample().then(|| views.parked(pane)),
                views.attributed_focus_event(pane),
            )
        };
        let events = self.event_snapshot();
        self.inspect_view(|_, view| {
            let pulled = view.overlay_baseline.as_ref().unwrap_or(&view.current);
            let baseline = pulled
                .panes_observed_at_ms
                .or(pulled.panes_produced_at_ms)
                .unwrap_or(0);
            if sampled.is_none()
                || event
                    .is_some_and(|stamp| stamp > baseline && events.has_focus_since(stamp, now_ms))
            {
                crate::sidebar::fuse::pane_is_focused(pulled, &events, pane, now_ms)
            } else {
                sampled.unwrap_or(false)
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        clock: LoopClock,
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
        let snapshot_now = clock.timestamp();
        let current = SidebarSnapshot::build_with_agents(
            config.workspace_id.clone(),
            Vec::new(),
            snapshot_now,
        );
        let now = clock.now();
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
            #[cfg(test)]
            projections: 0,
            #[cfg(test)]
            fold_preparations: 0,
            nav_keys: (
                current.sidebar.keys.clone(),
                super::NavKeymap::from_config(&current.sidebar.keys),
            ),
            config,
            clock,
            runtime,
            host_wake_path,
            diag,
            result_rx,
            view: ViewContext::new(current, now),
            fold_preparation: None,
            fold_effects: FoldEffects::default(),
            views: Arc::default(),
            view_scope: None,
            transition_view: None,
            view_focus: None,
            view_focus_event: None,
            view_parked: None,
            view_events: None,
            view_latest: false,
            was_resolved: false,
            baseline_seed: None,
            view_token: None,
            pending_focus_push: None,
            painted_focus_anchor_ms: 0,
            painted_at: now,
            painted_height: 0,
            paints: 0,
            paint_reports: Vec::new(),
            optimistic_watch_until: None,
            events: Arc::default(),
            pending_focus_repair: None,
            observer: observe::OwnObserver::default(),
            clicked_row: None,
            click_guard: None,
            hold_expired_since_paint: false,
            observe_tx,
            self_close: SelfCloseState::default(),
            ui: UiState {
                view: render::ViewState {
                    make_up_filter,
                    baseline_pane: baseline_pane.clone(),
                    ..Default::default()
                },
                ..UiState::default()
            },
            paint: FramePainter::new(pet_render_caps, pixel_wrap, pixel_slot),
            room_caps: Arc::default(),
            focus_peers: None,
            input_jump: None,
            wake: None,
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
            last_reported_baseline: baseline_pane.clone(),
            painted_baseline: baseline_pane,
            painted_selection: None,
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
        timeout = fetch_deadline_timeout(timeout, fetch.next_deadline(), self.clock.now());
        inbox.wait(timeout);
        self.advance(inbox, fetch, terminal).map(|_| ())
    }

    /// Run the nonblocking iteration and return whether it painted and the next driver deadline.
    pub(super) fn advance(
        &mut self,
        inbox: &mut WakeupInbox,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<PaneBackend>,
    ) -> Result<(bool, Instant)> {
        let paints = self.paints;
        self.retire_superseded_push();
        let pushed = if let Some(push) = self.focus_peers.as_ref().and_then(FocusPeers::take_push) {
            self.on_focus_push(push, fetch, terminal)?;
            true
        } else {
            false
        };
        self.report_baseline();
        if !pushed {
            inbox.dispatch_next(self, fetch, terminal, Duration::ZERO)?;
        }
        // A pane the mux had not laid out at attach gets its title
        // with its first size.
        if !self.titled {
            self.titled = terminal.backend_mut().set_pane_title()?;
        }

        self.run_maintenance(fetch, terminal);
        self.run_width_control_backstop(terminal);
        let (active, _) = self.frame_timing();
        self.paint_frame_if_due(terminal, active)?;
        let (_, timeout) = self.frame_timing();
        let now = self.clock.now();
        let timeout = fetch_deadline_timeout(timeout, fetch.next_deadline(), now);
        let mut deadline = now + timeout;
        if self.watched()
            && let Some(remaining) =
                self.freeze_remaining(self.clock.timestamp_at(now).as_millisecond())
        {
            deadline = deadline.min(now + remaining.max(FRAME_MIN_TIMEOUT));
        }
        Ok((self.paints != paints, deadline))
    }

    pub(super) fn report_baseline(&mut self) {
        let baseline = &self.painted_baseline;
        if *baseline != self.last_reported_baseline {
            (self.baseline_changed)(baseline.as_ref());
            self.last_reported_baseline.clone_from(baseline);
        }
    }

    pub(super) fn seed(&mut self, fold: &WorkspaceFold) {
        let snapshot = Self::project_fold(
            fold,
            self.config.own_pane.as_ref(),
            self.config.refresh_ms_override,
        );
        #[cfg(test)]
        {
            self.projections += 1;
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

    pub(super) fn project_fold(
        fold: &WorkspaceFold,
        own_pane: Option<&PaneId>,
        refresh_override: Option<u16>,
    ) -> SidebarSnapshot {
        let mut snapshot = project_local(fold.workspace.clone(), fold.frame.as_deref(), own_pane);
        if let Some(refresh_ms) = refresh_override {
            snapshot.theme.display.refresh_ms = refresh_ms;
        }
        snapshot
    }

    #[cfg(feature = "testkit")]
    pub(super) fn bench_frame_timing(&mut self, paint: bool) -> bool {
        if paint && self.watched() {
            let mut terminal =
                Terminal::new(PaneBackend::captured(40, 60)).expect("benchmark terminal");
            self.draw_frame(&mut terminal, self.clock.now(), false)
                .expect("benchmark paint");
            assert!(self.ui.theme_cache.is_some());
        }
        std::hint::black_box(self.frame_timing());
        self.watched()
    }

    fn freeze_remaining(&mut self, now_ms: i64) -> Option<Duration> {
        if self.view_scope.is_none() {
            return self.with_shared_view(false, false, |state| state.freeze_remaining(now_ms));
        }
        let hold = self.ui.freeze.as_ref()?;
        Some(Duration::from_millis(
            (hold.deadline_ms(self.parked(now_ms as u64)) - now_ms).max(0) as u64,
        ))
    }

    fn frame_timing(&mut self) -> (bool, Duration) {
        if self.view_scope.is_none() {
            return self.with_shared_view(false, false, Self::frame_timing);
        }
        let clock = frame_clock(
            self.clock.now(),
            self.view.anim_start,
            self.view.current.theme.display.resolved_refresh_ms(),
        );
        let alert_active = self.alert_active();
        let watched = self.watched();
        let animating = watched
            && render::animation_interval(&self.view.current, &self.ui, clock.phase, alert_active)
                .is_some();
        let active = !self.self_close.confirming_empty()
            && (watched && (animating || self.dirty || self.focus_marker_due(self.clock.now())));
        let mut timeout = if active {
            self.next_frame
                .saturating_duration_since(self.clock.now())
                .max(FRAME_MIN_TIMEOUT)
        } else {
            // Cap by the watchdog so the self-close backstop fires on time even
            // when the data tick is much longer.
            let watchdog_due = SELF_CLOSE_WATCHDOG.saturating_sub(
                self.clock
                    .now()
                    .saturating_duration_since(self.last_self_close_check),
            );
            let mut timeout = tick_for(self.config.tick_seconds).min(watchdog_due);
            if watched
                && let Some(remaining) =
                    self.freeze_remaining(self.clock.timestamp().as_millisecond())
            {
                timeout = timeout.min(remaining);
            }
            timeout.max(FRAME_MIN_TIMEOUT)
        };
        if watched && let Some(FocusMarker::Dwelling(until)) = self.view.focus_marker {
            timeout = timeout.min(
                until
                    .saturating_duration_since(self.clock.now())
                    .max(FRAME_MIN_TIMEOUT),
            );
        }
        if let Some(until) = self
            .view
            .tab_read_dwell_until
            .filter(|_| self.view.current.own_view.is_some())
        {
            timeout = timeout.min(
                until
                    .saturating_duration_since(self.clock.now())
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
                    .saturating_duration_since(self.clock.now())
                    .max(FRAME_MIN_TIMEOUT),
            );
        }
        if let Some(remaining) = gate_remaining(&self.view.gate, self.clock.timestamp()) {
            timeout = timeout.min(remaining.max(FRAME_MIN_TIMEOUT));
        }
        (active, timeout)
    }

    fn alert_active(&self) -> bool {
        self.inspect_view(|_, view| {
            view.health
                .alert
                .as_ref()
                .is_some_and(render::Alert::is_active)
        })
    }

    /// Whether an attached client's focus currently lands in this sidebar's tab.
    /// Unknown ownership or own-view state reads as watched so uncertainty never
    /// suppresses motion.
    fn watched(&self) -> bool {
        if self.shared_view_id().is_some() {
            return true;
        }
        // Input and corroborated resize keep a painting view warm between samples.
        if self
            .optimistic_watch_until
            .is_some_and(|until| self.clock.now() < until)
        {
            return true;
        }
        self.observed_watched()
    }

    fn watches_snapshot(&self, snapshot: &SidebarSnapshot) -> bool {
        self.watches_view(snapshot.own_view.as_ref(), &snapshot.viewed_panes)
    }

    fn watches_view(&self, own_view: Option<&SidebarOwnView>, viewed_panes: &[PaneId]) -> bool {
        let Some(own_pane) = self.config.own_pane.as_ref() else {
            return true;
        };
        let Some(view) = own_view else {
            return true;
        };
        own_tab_viewed(viewed_panes, view, own_pane)
    }

    pub(super) fn on_snapshot(&mut self, fetch: &mut FetchDispatcher) {
        self.drain_snapshot(fetch, true);
    }

    fn drain_snapshot(&mut self, fetch: &mut FetchDispatcher, allow_defer: bool) {
        #[cfg(test)]
        let mut projections = 0;
        let (pending, deferred) = self.result_rx.take_unless(
            |own_view, viewed_panes| allow_defer && self.skips_hidden(own_view, viewed_panes),
            |fold, own_pane, refresh_override| {
                #[cfg(test)]
                {
                    projections += 1;
                }
                Self::project_fold(fold, own_pane, refresh_override)
            },
        );
        #[cfg(test)]
        {
            self.projections += projections;
        }
        if deferred && pending.completed {
            self.fetched_at = self.clock.now();
            self.last_self_close_check = self.fetched_at;
        }
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
            let defer_for = self
                .inspect_view(|_, view| gate_remaining(&view.gate, self.clock.timestamp()))
                .unwrap_or(crate::sidebar::timing::ACCEPT_REGRESSION_AFTER);
            fetch.defer_until(FetchRequest::default(), self.clock.now() + defer_for);
        }
    }

    fn skips_hidden(&self, own_view: Option<&SidebarOwnView>, viewed_panes: &[PaneId]) -> bool {
        let hidden = !self.watched()
            && !self.watches_view(own_view, viewed_panes)
            && own_view == self.view.current.own_view.as_ref()
            && !self.should_exit
            && !self.self_close.confirming_empty()
            && self.pending_focus_repair.is_none();
        hidden
            && self.inspect_view(|_, view| {
                view.gate == GateState::default()
                    && view.health.failure_streak == 0
                    && view.tab_read_dwell_until.is_none()
                    && view.pending_input_focus.is_none()
            })
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
            view: _,
            fold_preparation: _,
            fold_effects: _,
            views: _,
            view_scope: _,
            transition_view: _,
            view_focus: _,
            view_focus_event: _,
            view_parked: _,
            view_events: _,
            view_latest: _,
            was_resolved: _,
            baseline_seed: _,
            view_token: _,
            ui: _,
            self_close,
            pending_focus_repair,
            #[cfg(test)]
                content_applications: _,
            #[cfg(test)]
                projections: _,
            #[cfg(test)]
                fold_preparations: _,
            // Derived from `current`, which equal content leaves unchanged.
            nav_keys: _,
            // Loop wiring and identity, fixed for the attachment.
            config: _,
            clock: _,
            runtime: _,
            host_wake_path: _,
            diag: _,
            result_rx: _,
            observe_tx: _,
            wake: _,
            room_caps: _,
            request_reload: _,
            baseline_changed: _,
            // Focus-anchor bookkeeping: a live intent or fence holds
            // `overlay_baseline`, and an anchor change is content.
            pending_focus_push: _,
            painted_focus_anchor_ms: _,
            painted_at: _,
            painted_height: _,
            paints: _,
            paint_reports: _,
            // Overlay events: an active one holds `overlay_baseline`.
            events: _,
            // Paint, animation, and frame scheduling, which a clock-only
            // delivery drives itself.
            paint: _,
            titled: _,
            dirty: _,
            paint_hold: _,
            next_frame: _,
            prev_width: _,
            caps_refresh_deadline: _,
            width_control: _,
            observer: _,
            clicked_row: _,
            click_guard: _,
            hold_expired_since_paint: _,
            optimistic_watch_until: _,
            // Maintenance stamps, read against `current`.
            fetched_at: _,
            last_self_close_check: _,
            last_heartbeat: _,
            // Exit detail and baseline reporting, set beside `should_exit` or
            // from `ui` outside the fold.
            exit_cause: _,
            tab_emptied: _,
            last_reported_baseline: _,
            painted_baseline: _,
            painted_selection: _,
            focus_peers: _,
            input_jump: _,
        } = self;
        self.inspect_view(|ui, view| {
            let ViewContext {
                overlay_baseline,
                gate,
                health,
                tab_read_dwell_until,
                pending_input_focus,
                focus_marker: _,
                current: _,
                last_focus_observation: _,
                last_pulled_rows: _,
                observation: _,
                fold_inputs: _,
                observed_anchor: _,
                last_folded_focus_intent: _,
                live_focus_source: _,
                focus_landing: _,
                focus_rollback: _,
                retired_anchor: _,
                confirmed_focus_intent_ms: _,
                filter_observed_at: _,
                anim_start: _,
            } = view;
            !should_exit
                && !rejected
                && overlay_baseline.is_none()
                && *gate == GateState::default()
                && health.failure_streak == 0
                && ui.freeze.is_none()
                && tab_read_dwell_until.is_none()
                && !self_close.confirming_empty()
                && pending_focus_repair.is_none()
                && pending_input_focus.is_none()
        })
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
            Wakeup::FocusFailed { pane, error } => {
                self.on_focus_failed(pane, error);
                Ok(())
            }
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

    fn on_focus_failed(&mut self, pane: PaneId, error: String) {
        if self.fold_preparation.is_none() {
            if self.fail_rerouted_focus(&pane, &error) {
                return;
            }
            self.fold_transition(|state| state.on_focus_failed(pane, error));
            return;
        }
        if self.view.pending_input_focus.as_ref() == Some(&pane) {
            self.view.pending_input_focus = None;
        }
        let Some(label) = self
            .view
            .roll_back_focus(&mut self.ui.view, &pane, self.clock.now())
        else {
            return;
        };
        self.fold_fused_now();
        self.view
            .alert_focus_failure(&label, &error, self.clock.timestamp());
        self.dirty = true;
    }

    /// A client swap between dispatch and failure routes this pane away from the view that
    /// clicked; the rollback and its alert stay with that view, whose selection follows at its next fold.
    fn fail_rerouted_focus(&mut self, pane: &PaneId, error: &str) -> bool {
        let dispatched_here = self.inspect_view(|_, view| {
            view.focus_rollback
                .as_ref()
                .is_some_and(|rollback| rollback.target == *pane)
        });
        if dispatched_here {
            return false;
        }
        let mut views = self.views.lock().unwrap_or_else(PoisonError::into_inner);
        let Some((state, context)) = views.dispatched(pane) else {
            return false;
        };
        if context.pending_input_focus.as_ref() == Some(pane) {
            context.pending_input_focus = None;
        }
        let Some(label) = context.roll_back_focus(state, pane, self.clock.now()) else {
            return false;
        };
        context.alert_focus_failure(&label, error, self.clock.timestamp());
        true
    }

    pub(super) fn apply_latest_snapshot(&mut self, update: FetchUpdate) -> bool {
        self.apply_latest_snapshot_with_seed(update, false)
    }

    fn apply_latest_snapshot_with_seed(&mut self, update: FetchUpdate, seed: bool) -> bool {
        let unchanged = update.is_unchanged();
        let delivered = update.into_full();
        let (incoming, inputs) = match delivered.as_ref() {
            FetchUpdate::Shared { update, context } => {
                (update.as_ref(), Some(context.inputs.clone()))
            }
            update => (update, None),
        };
        let visible = match incoming {
            FetchUpdate::Snapshot { snapshot, .. } => Some(self.watches_snapshot(snapshot)),
            _ => None,
        };
        let acknowledgment = if unchanged {
            Arc::new(FetchUpdate::unchanged(delivered.clone()))
        } else {
            delivered.clone()
        };
        self.view_transition(false, inputs, visible, Some(delivered.clone()), |state| {
            let rejected = if state.shared_view_id().is_some() && state.view_latest {
                state.view.gate != GateState::default()
            } else if unchanged {
                state.reduce_snapshot(FetchUpdate::unchanged(delivered), seed)
            } else {
                state.reduce_snapshot((*delivered).clone(), seed)
            };
            state.fold_effects.acknowledgments.push(acknowledgment);
            rejected
        })
    }

    fn reduce_snapshot(&mut self, update: FetchUpdate, seed: bool) -> bool {
        let (update, context) = update.into_parts();
        self.view.fold_inputs = context.as_ref().map(|context| context.inputs.clone());
        if let Some(observation) = context.and_then(|context| context.observation.clone()) {
            self.view.observation = Some(observation);
        }
        if let FetchUpdate::Unchanged { clock, .. } = update {
            clock.apply(&mut self.view.current);
            self.view.last_focus_observation.panes_observed_at_ms =
                self.view.current.panes_observed_at_ms;
            self.fetched_at = self.clock.now();
            self.last_self_close_check = self.clock.now();
            self.dirty = true;
            self.view.fold_inputs = None;
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
                let now_ms = self.clock.now_ms();
                let pulled = *snapshot;
                self.view.last_focus_observation = FocusObservation::from_snapshot(&pulled);
                if self.view.observation.is_some() {
                    self.view.last_pulled_rows = pulled.rows().count();
                }
                let intent = self.pending_focus_intent(now_ms, self.clock.now());
                let events = self.event_snapshot();
                let (mut snapshot, focus_event_sent_at_ms) =
                    self.view
                        .fuse_pulled(pulled, events, intent.as_ref(), now_ms);
                if intent.is_none()
                    && focus_event_sent_at_ms
                        .is_none_or(|stamp| self.view_focus_event != Some(stamp))
                    && let Some(focused) = &self.view_focus
                {
                    snapshot.focused_pane.clone_from(focused);
                }
                fused_event_sent_at_ms = focus_event_sent_at_ms;
                FetchUpdate::Snapshot {
                    snapshot: Box::new(snapshot),
                    phase,
                    source,
                }
            }
            failed @ FetchUpdate::Failed { .. } => failed,
            FetchUpdate::Unchanged { .. } => unreachable!("clock-only delivery handled above"),
            FetchUpdate::Shared { .. } => unreachable!("unwrapped above"),
            FetchUpdate::Fold { .. } => unreachable!("projected at take"),
        };
        self.fetched_at = self.clock.now();
        let rejected = self.fold_outcome(update, true, seed, fused_event_sent_at_ms);
        self.view.fold_inputs = None;
        if snapshot_ok {
            self.last_self_close_check = self.clock.now();
            self.fold_effects.retry_focus_repair = true;
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
                .releases_on_stamp(self.view.current.panes_observed_at_ms)
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
            .view
            .overlay_baseline
            .take()
            .unwrap_or_else(|| self.view.current.clone());
        let intent = self.pending_focus_intent(now_ms, self.clock.now());
        let events = self.event_snapshot();
        let (mut snapshot, stamp) = self
            .view
            .fuse_pulled(pulled, events, intent.as_ref(), now_ms);
        if intent.is_none()
            && stamp.is_none_or(|stamp| self.view_focus_event != Some(stamp))
            && let Some(focused) = &self.view_focus
        {
            snapshot.focused_pane.clone_from(focused);
        }
        (snapshot, stamp)
    }

    /// Fold a synthetic fused frame and snap the frame deadline so this
    /// turn's frame phase paints it now.
    fn fold_fused_now(&mut self) {
        if self.fold_preparation.is_none() {
            self.fold_transition(Self::fold_fused_now);
            return;
        }
        self.fold_effects.invalidate = true;
        let (fused, fused_event_sent_at_ms) = self.fused_snapshot(self.clock.now_ms());
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
        self.next_frame = self.clock.now();
    }

    fn fold_focus_intent(&mut self, anchor: crate::mux::focus_anchor::FocusAnchor) {
        if self.fold_preparation.is_none() {
            self.fold_transition(|state| state.fold_focus_intent(anchor));
            return;
        }
        if anchor.session_name != self.config.session_name
            || self.view.last_folded_focus_intent.replace(anchor.nonce) == Some(anchor.nonce)
        {
            return;
        }
        // The pushed intent is newer than every cut folded before it, the catch-up fold included,
        // even on a clock that has not moved since: that older read must not retire this anchor.
        let sampled_at = self.clock.now();
        self.view.observed_anchor = Some((sampled_at, Some(anchor.clone())));
        let previous = self
            .view
            .fold_inputs
            .replace(Arc::new(super::fetch::FoldInputs {
                sampled_at,
                anchor: Some(anchor),
                filter: self.ui.make_up_filter.clone(),
            }));
        self.fold_fused_now();
        self.view.fold_inputs = previous;
    }

    fn on_focus_push(
        &mut self,
        mut push: FocusPush,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<PaneBackend>,
    ) -> Result<()> {
        let arrived = self.clock.now();
        push.peer.received_ms = Some(push.elapsed_ms());
        self.retire_superseded_push();
        if !push.is_current() {
            push.finish(PushVerdict::Superseded);
            return Ok(());
        }
        if self.known_non_owner(&push.anchor.pane_id) {
            push.finish(PushVerdict::NotDestination);
            return Ok(());
        }
        // A jump to this pane's tab is viewing evidence: the landing paints a view, never scratch.
        self.optimistic_watch_until = Some(arrived + FOCUS_RESUME_WATCH_WINDOW);
        if let Some((pane, source)) = self.config.own_pane.as_ref().zip(push.source_view) {
            self.views
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .land(source, pane, self.clock.now());
            self.with_shared_view(false, false, |state| {
                state.view.live_focus_source = Some(push.anchor.nonce);
            });
        }
        // Read the view the landing paints: a scratch fold while hidden folded into no view.
        if self.with_shared_view(false, false, |state| state.view.last_folded_focus_intent)
            != Some(push.anchor.nonce)
        {
            self.prepare_action(fetch);
            push.peer.snapshot_ms = Some(push.elapsed_ms());
            if !push.is_current() {
                push.finish(PushVerdict::Superseded);
                return Ok(());
            }
            self.fold_focus_intent(push.anchor.clone());
            push.peer.folded_ms = Some(push.elapsed_ms());
        }
        if push.anchor.session_name != self.config.session_name
            || !self.owns_focus_target(&push.anchor.pane_id)
        {
            push.finish(PushVerdict::NotDestination);
            return Ok(());
        }
        if self.should_exit {
            if let Some(pending) = self.pending_focus_push.take() {
                pending.finish(PushVerdict::Exiting);
            }
            push.finish(PushVerdict::Exiting);
            return Ok(());
        }
        if self.focus_push_size_matches(&push, terminal) {
            self.paint_hold.release();
        }
        if !push.is_current() {
            push.finish(PushVerdict::Superseded);
            return Ok(());
        }
        if let Some(verdict) = self.painted_focus_verdict(&push.anchor, Some(&push)) {
            push.finish_painted(verdict, self.painted_at);
            return Ok(());
        }
        if let Some(pending) = self.pending_focus_push.replace(push) {
            pending.finish(PushVerdict::Superseded);
        }
        self.paint_frame_if_due(terminal, true)
    }

    fn known_non_owner(&self, target: &PaneId) -> bool {
        self.view.current.row_owning_pane(target).is_some()
            && !self.owns_focus_target(target)
            && self
                .view
                .current
                .own_view
                .as_ref()
                .is_some_and(|own_view| !self.result_rx.has_pending_own_view_change(own_view))
    }

    fn retire_superseded_push(&mut self) -> bool {
        if self
            .pending_focus_push
            .as_ref()
            .is_some_and(|push| !push.is_current())
        {
            if let Some(push) = self.pending_focus_push.take() {
                push.finish(PushVerdict::Superseded);
            }
            return true;
        }
        false
    }

    pub(super) fn on_event(
        &mut self,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<PaneBackend>,
        envelope: SidebarEventEnvelope,
    ) {
        let requests_verification = envelope.event.requests_producer_verification();
        let sent_at_ms = envelope.sent_at_ms;
        if let SidebarEvent::FocusChanged { focused, unfocused } = &envelope.event {
            self.views
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .note_focus_event(focused, unfocused, sent_at_ms);
        }
        let fold = self.event_needs_fold(&envelope.event);
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
                if fold {
                    self.prepare_action(fetch);
                }
                let measured = terminal.size().ok().map(|size| size.width);
                self.width_control
                    .reload_target(&self.view.current.theme, measured, &self.diag);
            }
            SidebarEvent::BodyFilterChanged => {
                if fold {
                    self.prepare_action(fetch);
                }
                let filter = crate::sidebar::body_filter::load(&self.runtime);
                self.with_shared_view(false, false, |state| {
                    state.view.filter_observed_at = Some(state.clock.now());
                    if set_make_up_filter(&mut state.ui, &state.view.current, filter) {
                        state.dirty = true;
                    }
                });
            }
            event @ SidebarEvent::Notify { .. } => {
                if fold {
                    self.prepare_action(fetch);
                }
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
                self.prepare_action(fetch);
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
            SidebarEvent::FocusIntent { nonce, .. } => {
                if !fold {
                    return;
                }
                if self
                    .focus_peers
                    .as_ref()
                    .is_some_and(|peers| !peers.accepts(Some(nonce), self.clock.now()))
                {
                    return;
                }
                if self.inspect_view(|_, view| view.last_folded_focus_intent) == Some(nonce) {
                    return;
                }
                // Set before draining, so the jump folds the latest into the view.
                if self.reveals_own_tab(&envelope.event) {
                    self.optimistic_watch_until =
                        Some(self.clock.now() + FOCUS_RESUME_WATCH_WINDOW);
                }
                self.prepare_action(fetch);
                if let Some(anchor) = self
                    .focus_anchor(self.clock.now())
                    .filter(|anchor| anchor.nonce == nonce)
                {
                    self.fold_focus_intent(anchor);
                } else {
                    self.fold_fused_now();
                }
            }
            event if event.is_overlay() => {
                // Set before draining, so a revealing focus folds the latest into the view.
                if self.reveals_own_tab(&event) {
                    self.optimistic_watch_until =
                        Some(self.clock.now() + FOCUS_RESUME_WATCH_WINDOW);
                }
                if fold {
                    self.prepare_action(fetch);
                }
                self.handle_overlay_event(fetch, event, sent_at_ms, fold);
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

    fn event_needs_fold(&mut self, event: &SidebarEvent) -> bool {
        let preview = self.result_rx.preview();
        let (view, viewed) = preview.as_ref().map_or(
            (
                self.view.current.own_view.as_ref(),
                self.view.current.viewed_panes.as_slice(),
            ),
            |(view, viewed)| (view.as_ref(), viewed.as_slice()),
        );
        if !self.skips_hidden(view, viewed) {
            return true;
        }
        match event {
            SidebarEvent::FocusChanged { focused, .. } => self.watches_view(view, focused),
            SidebarEvent::Notify { panes, .. } => view.is_some_and(|view| {
                panes
                    .iter()
                    .any(|pane| view.working_pane_ids.contains(pane))
            }),
            SidebarEvent::FocusIntent { nonce, .. } => {
                let view = view.cloned();
                self.focus_anchor(self.clock.now())
                    .filter(|anchor| anchor.nonce == *nonce)
                    .is_some_and(|anchor| {
                        self.watches_view(view.as_ref(), std::slice::from_ref(&anchor.pane_id))
                    })
            }
            _ => false,
        }
    }

    fn prepare_action(&mut self, fetch: &mut FetchDispatcher) {
        // Require content before flushing, then invalidate reuse again:
        // the action can change renderer state after acknowledgement.
        self.result_rx.invalidate();
        self.drain_snapshot(fetch, false);
        self.result_rx.invalidate();
    }

    fn handle_notification(&mut self, terminal: &mut Terminal<PaneBackend>, event: SidebarEvent) {
        if !self.clock.effects_enabled() {
            return;
        }
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
            &self.view.current,
            BellNotice {
                title: &title,
                body: &body,
                panes: &panes,
                recheck_unread,
                kind,
            },
            &self.diag,
        ) {
            Ok(_) => {}
            Err(err) => debug!(error = %err, "terminal notification emit failed"),
        }
    }

    fn handle_focus_stranded(&mut self, repair: &PendingFocusRepair) -> bool {
        let now_ms = self.clock.now_ms();
        let own_pane = self.config.own_pane.clone();
        let verdict = self.with_shared_view(false, false, |state| {
            focus_stranded_target(
                &state.view.current,
                &state.ui,
                &repair.pane_id,
                &repair.clients,
                own_pane.as_ref(),
                repair.sent_at_ms,
                now_ms,
            )
        });
        let target = match verdict {
            Ok(target) => target,
            Err(reason) => {
                debug!(
                    pane = %repair.pane_id,
                    own_pane = ?own_pane,
                    generation = repair.generation,
                    evidence = ?repair.clients,
                    own_view = ?self.view.current.own_view,
                    client_views = ?self.view.current.client_views,
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
        let (placement, order) = self.with_shared_view(false, false, |state| {
            (state.focus_placement(&target), state.ui.last_order.clone())
        });
        spawn_pane_focus(
            &self.clock,
            self.clock.now(),
            target,
            &self.config.session_name,
            self.runtime.clone(),
            crate::mux::focus_anchor::FocusOrigin::AutomaticRepair,
            expected,
            (placement, Some(order)),
            Some(dispatch),
            None,
            self.wake.clone(),
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
        let now_ms = self.clock.now_ms();
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
                at: self.clock.timestamp(),
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

    /// Focus landing anywhere on this tab, or a jump to a pane on it, is viewing evidence the
    /// plane's next sample confirms. Ownership reads the latest delivery, as hidden skipping does.
    fn reveals_own_tab(&self, event: &SidebarEvent) -> bool {
        let Some(own) = self.config.own_pane.as_ref() else {
            return false;
        };
        let targets = match event {
            SidebarEvent::FocusChanged { focused, .. } => focused.as_slice(),
            SidebarEvent::FocusIntent { pane_id, .. } => std::slice::from_ref(pane_id),
            _ => return false,
        };
        let preview = self.result_rx.preview();
        let own_view = preview
            .as_ref()
            .map_or(self.view.current.own_view.as_ref(), |(view, _)| {
                view.as_ref()
            });
        targets.contains(own) || own_view.is_some_and(|view| own_tab_viewed(targets, view, own))
    }

    fn handle_overlay_event(
        &mut self,
        fetch: &mut FetchDispatcher,
        event: SidebarEvent,
        sent_at_ms: u64,
        fold: bool,
    ) {
        // A relevant overlay fuses into the in-memory state and paints this
        // frame. Every topology overlay asks the producer to verify with a
        // real pull, which supersedes the overlay once its fresh frame
        // folds in. A resize-grow paint hold stays held until a pulled
        // sibling-count verdict releases it.
        let own = self.config.own_pane.as_ref();
        let own_focused = self.reveals_own_tab(&event);
        let own_revealed = own_focused && !self.observed_watched();
        let own_unfocused = matches!(&event, SidebarEvent::FocusChanged { focused, unfocused }
            if own.is_some_and(|own| unfocused.contains(own)
                || (self.view.current.client_views.len() == 1
                    && matches!(focused.as_slice(), [pane] if pane != own))));
        let requests_verification = event.requests_producer_verification();
        let now_ms = self.clock.now_ms();
        self.events().append(event, sent_at_ms, now_ms);
        if fold {
            self.fold_fused_now();
        }
        if !self.should_exit && (requests_verification || own_revealed) {
            fetch.request(FetchRequest::fresh_panes(), true);
        }
        if own_focused {
            self.optimistic_watch_until = Some(self.clock.now() + FOCUS_RESUME_WATCH_WINDOW);
        } else if own_unfocused {
            self.optimistic_watch_until = None;
            self.with_shared_view(false, false, |state| {
                state.ui.help_visible = false;
                if state.ui.search_draft.take().is_some() {
                    reconcile_selection(&mut state.ui, &state.view.current, None);
                    state.dirty = true;
                }
            });
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
        if self.clock.effects_enabled() {
            self.caps_refresh_deadline = Some(self.clock.now() + RESIZE_CAPS_SETTLE);
        }
        // A hidden resize is no viewing evidence, and revokes none: a jump that just landed here
        // keeps painting until the plane samples the tab.
        if self.observed_watched() {
            self.optimistic_watch_until = Some(self.clock.now() + FOCUS_RESUME_WATCH_WINDOW);
        }
        // Watched resize uses the newest fold; hidden content stays deferred.
        self.drain_snapshot(fetch, true);
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
        if held_grow
            && self.self_close.seen_sibling
            && !self
                .pending_focus_push
                .as_ref()
                .is_some_and(|push| self.focus_push_size_matches(push, terminal))
        {
            self.dirty = true;
            self.paint_hold
                .engage(self.clock.now(), self.clock.now_ms());
        } else {
            if self.apply_input([Wakeup::Resize], terminal)?.redraw {
                self.dirty = false;
            }
            // A safe-width paint just landed; drop any stale hold a prior grow
            // left pending so it cannot suppress this frame.
            self.paint_hold.release();
        }
        self.last_self_close_check = self.clock.now();
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
            self.view
                .current
                .own_view
                .as_ref()
                .map(|view| view.sibling_count),
            self.view.current.panes_observed_at_ms,
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
        self.prepare_action(fetch);
        let applied = match self.apply_input(wakeups, terminal) {
            Ok(applied) => applied,
            Err(error) => {
                self.input_jump = None;
                return Err(error);
            }
        };
        if applied.redraw && self.watched() {
            // Key/mouse input paints synchronously for instant feedback; a
            // paint settles any frame the loop owed.
            self.dirty = false;
        }
        for effect in applied.effects {
            match effect {
                InputEffect::Focus(pane) => {
                    let peers = self.input_jump.take();
                    let clicked = peers
                        .as_ref()
                        .and_then(FocusPeers::clicked)
                        .unwrap_or_else(|| self.clock.now());
                    let (placement, order) = self.with_shared_view(false, false, |state| {
                        (state.focus_placement(&pane), state.ui.last_order.clone())
                    });
                    spawn_pane_focus(
                        &self.clock,
                        clicked,
                        pane,
                        &self.config.session_name,
                        self.runtime.clone(),
                        crate::mux::focus_anchor::FocusOrigin::User,
                        None,
                        (placement, Some(order)),
                        None,
                        peers,
                        self.wake.clone(),
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
        self.with_shared_view(false, false, |state| {
            state.view.filter_observed_at = Some(state.clock.now());
        });
        let persisted = crate::sidebar::body_filter::write(&self.runtime, &filter);
        if let Err(err) = persisted {
            debug!(error = %err, "sidebar body filter write failed");
            return;
        }
        if self.clock.effects_enabled()
            && let Err(err) = crate::wakeup::broadcast(
                &self.runtime,
                Some(&self.config.session_name),
                SidebarEvent::BodyFilterChanged,
            )
        {
            debug!(error = %err, "sidebar body filter broadcast failed");
        }
    }

    /// Apply a single input or queued wheel run to the local UI in order. Input
    /// never changes store data, so it redraws the *current* snapshot and may
    /// jump focus, but it never re-runs the snapshot burst — that per-keystroke
    /// refetch was the input lag. Input paints synchronously so a keypress or
    /// click feels instant rather than waiting for the next frame. Wheel ticks
    /// share one paint after the run, never an intermediate paint. User input
    /// is viewing evidence; a hidden attachment's relayout keeps its redraw
    /// pending until it is watched.
    fn apply_input(
        &mut self,
        wakeups: impl IntoIterator<Item = Wakeup>,
        terminal: &mut Terminal<PaneBackend>,
    ) -> Result<InputOutcome> {
        let wakeups: Vec<_> = wakeups.into_iter().collect();
        let user_input = wakeups
            .iter()
            .any(|wakeup| !matches!(wakeup, Wakeup::Resize));
        if user_input {
            self.optimistic_watch_until = Some(self.clock.now() + FOCUS_RESUME_WATCH_WINDOW);
        }
        let (outcome, source_view) = self.view_transition(user_input, None, None, None, |state| {
            (
                state.reduce_input(wakeups, state.clock.now()),
                state.shared_view_id(),
            )
        });
        for effect in &outcome.effects {
            if let InputEffect::Focus(pane) = effect {
                self.input_jump = self
                    .clock
                    .effects_enabled()
                    .then(|| self.focus_peers_for(pane))
                    .flatten()
                    .map(|peers| {
                        peers
                            .with_source_view(source_view)
                            .arm(pane.clone(), self.clock.now())
                    });
                if self.clock.effects_enabled()
                    && self.input_jump.is_none()
                    && let Some(peers) = &self.focus_peers
                {
                    peers.supersede();
                }
            }
        }
        if outcome.redraw {
            if !self.watched() {
                self.dirty = true;
                return Ok(outcome);
            }
            self.draw_frame(terminal, self.clock.now(), user_input)?;
            self.with_shared_view(false, false, |state| {
                let now = state.clock.now();
                if outcome
                    .effects
                    .iter()
                    .any(|effect| matches!(effect, InputEffect::Focus(_)))
                {
                    layout_freeze::arm_freeze(
                        &mut state.ui,
                        state.clock.timestamp_at(now).as_millisecond(),
                    );
                }
                let interval = render::animation_interval(
                    &state.view.current,
                    &state.ui,
                    state.ui.animation_phase,
                    state.alert_active(),
                )
                .unwrap_or(animation_frame(&state.view.current));
                let deadline = now + interval;
                if state.next_frame <= now || state.next_frame > deadline {
                    state.next_frame = deadline;
                }
            });
        }
        Ok(outcome)
    }

    fn reduce_input(&mut self, wakeups: Vec<Wakeup>, now: Instant) -> InputOutcome {
        let keys = &self.view.current.sidebar.keys;
        if self.nav_keys.0 != *keys {
            self.nav_keys = (keys.clone(), super::NavKeymap::from_config(keys));
        }
        let mut outcome = InputOutcome::default();
        let mut user_input = false;
        for wakeup in wakeups {
            user_input |= !matches!(wakeup, Wakeup::Resize);
            let wheel = matches!(wakeup, Wakeup::Scroll { .. });
            let alert_active = self.alert_active();
            let live_frame = self
                .click_guard
                .as_ref()
                .filter(|guard| now < guard.until && matches!(wakeup, Wakeup::MouseClick { .. }))
                .map(|guard| {
                    (
                        std::mem::replace(&mut self.ui.interactions, guard.interactions.clone()),
                        std::mem::replace(&mut self.ui.painted_roster, guard.roster.clone()),
                    )
                });
            let previous_selection = self.ui.selected_pane.clone();
            let previous_row = self
                .ui
                .selected_index
                .and_then(|index| self.ui.visible_roster(&self.view.current).row(index))
                .map(|row| row.id.clone());
            let applied = handle_wakeup(
                wakeup,
                &mut self.ui,
                &self.view.current,
                &self.nav_keys.1,
                alert_active,
            );
            if let Some((interactions, roster)) = live_frame {
                self.ui.interactions = interactions;
                self.ui.painted_roster = roster;
            }
            self.clicked_row = None;
            let selected_row = self
                .ui
                .selected_index
                .and_then(|index| self.ui.visible_roster(&self.view.current).row(index))
                .map(|row| row.id.clone());
            if selected_row != previous_row {
                self.view.pending_input_focus = None;
                self.ui.own_input_reveal = true;
                self.view.focus_marker = None;
                self.ui.focus_marker_ready = false;
            }
            if self.ui.selected_pane != previous_selection {
                self.view.focus_rollback = None;
            }
            for effect in &applied.effects {
                if let InputEffect::Focus(pane) = effect {
                    self.view.pending_input_focus = Some(pane.clone());
                    let owner = self
                        .ui
                        .selected_pane
                        .as_ref()
                        .and_then(|selected| self.view.current.row_owning_pane(selected))
                        .filter(|row| row.pane_ids().any(|id| id == pane))
                        .or_else(|| self.view.current.row_owning_pane(pane));
                    self.view.focus_rollback = owner.map(|row| FocusRollback {
                        target: pane.clone(),
                        baseline: self.ui.baseline_pane.clone(),
                        label: row.display_name().to_owned(),
                    });
                    self.arm_clicked_row(pane, now);
                }
            }
            if applied.redraw && !wheel {
                self.ui.scroll_ease.cancel();
            }
            if applied.effects.contains(&InputEffect::DismissAlert) {
                self.view.health.alert = None;
            }
            outcome.redraw |= applied.redraw;
            outcome.effects.extend(applied.effects);
        }
        if user_input
            && (outcome.redraw || !outcome.effects.is_empty())
            && !outcome.effects.contains(&InputEffect::Reload)
        {
            layout_freeze::arm_freeze(&mut self.ui, self.clock.timestamp_at(now).as_millisecond());
        }
        outcome
    }

    /// Mark a row read without jumping (`m`): write the room receipt,
    /// clear the row locally for an instant repaint, trace the clear, wake the
    /// room so the host prunes the episode and peer tabs converge, and refetch
    /// so the receipt lands in the pulled snapshot. A no-op when the row is
    /// already read.
    fn mark_row_read(&mut self, fetch: &mut FetchDispatcher, row_id: &str) {
        let now = self.clock.timestamp();
        let marks = ReadMarks::load(&self.runtime);
        let clear = self.with_shared_view(false, false, |state| {
            if state.ui.unread_guard.as_deref() == Some(row_id) {
                state.ui.unread_guard = None;
            }
            read_receipt_for_row(
                &state.view.current,
                Some(row_id),
                UnreadClearCause::MarkRead,
                &marks,
                now,
            )
        });
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
        self.with_shared_view(false, false, |state| {
            set_rows_unread(&mut state.view.current, &clear.ids, false);
        });
        emit_unread_cleared_trace(&self.diag, &clear.trace);
        self.commit_local_mark(fetch);
    }

    /// Mark every readable row read without jumping (`M`): write room receipts
    /// for every unread or needs-a-look row, clear local unread bits, and refetch
    /// so peers converge through the producer.
    fn mark_all_read(&mut self, fetch: &mut FetchDispatcher) {
        let now = self.clock.timestamp();
        let marks = ReadMarks::load(&self.runtime);
        let clear = self.with_shared_view(false, false, |state| {
            state.ui.unread_guard = None;
            read_receipts_for_all(&state.view.current, UnreadClearCause::MarkRead, &marks, now)
        });
        self.apply_read_clear(clear, now, fetch);
    }

    /// Re-flag a row unread without jumping (`m` on a read row): open a durable
    /// episode through the shared mark-unread path, set the row locally for an
    /// instant repaint, trace the open, wake the room, and refetch so the episode
    /// lands in the pulled snapshot. A no-op when the row has left the room.
    fn mark_row_unread(&mut self, fetch: &mut FetchDispatcher, row_id: &str) {
        let Some(row) = self
            .view
            .current
            .rows()
            .find(|row| row.id == row_id)
            .cloned()
        else {
            return;
        };
        let now_ms = self.clock.timestamp().as_millisecond();
        let opened =
            match unread::mark_rows_unread(&self.runtime, std::slice::from_ref(&row), now_ms) {
                Ok(opened) => opened,
                Err(err) => {
                    warn!(error = %err, "mark-unread episode write failed");
                    return;
                }
            };
        self.with_shared_view(false, false, |state| {
            set_rows_unread(&mut state.view.current, std::slice::from_ref(&row.id), true);
            state.ui.unread_guard = Some(row.id.clone());
        });
        for item in &opened {
            self.diag.trace_notify(item.trace_event());
        }
        self.commit_local_mark(fetch);
    }

    fn commit_local_mark(&mut self, fetch: &mut FetchDispatcher) {
        if self.clock.effects_enabled() {
            wake_room(&self.runtime);
        }
        self.dirty = true;
        self.next_frame = self.clock.now();
        fetch.request(FetchRequest::default(), true);
    }

    fn run_maintenance(
        &mut self,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<PaneBackend>,
    ) {
        let own_pane = self.config.own_pane.clone();
        let room_caps = self.room_caps.clone();
        if self.clock.effects_enabled() {
            self.refresh_pet_render_caps_if_stale_with(
                self.config.mux,
                &self.config.session_name.clone(),
                self.clock.now(),
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
        }
        // Snapshot wakeups are a latency hint, not the only correctness path.
        // `rimz reload` replaces the renderer in place and a ready-result
        // datagram can be lost around socket teardown/rebind; the frame/tick
        // path still drains pending results so startup cannot strand the
        // placeholder cockpit.
        self.on_snapshot(fetch);
        fetch.fire_due(self.clock.now());

        let (gate_due, dwell_due) = self.inspect_view(|_, view| {
            (
                gate_remaining(&view.gate, self.clock.timestamp())
                    .is_some_and(|remaining| remaining.is_zero()),
                view.tab_read_dwell_until
                    .filter(|_| view.current.own_view.is_some())
                    .is_some_and(|until| self.clock.now() >= until),
            )
        });
        if gate_due {
            fetch.request(FetchRequest::force_fold(), false);
        }

        // Tab-view read dwell: once the user has stayed past the dwell, provoke
        // a fold so the normal clear path sweeps the unread siblings. The fold
        // disarms the deadline, so this fires at most a fetch or two.
        if dwell_due && self.watched() {
            fetch.request(FetchRequest::force_fold(), false);
        }

        // Data backstop: catch pane/git drift no store delta announced. It is
        // self-gated to the data tick; an armed clamp-deferred nudge merges
        // into this fold instead of waiting to echo it.
        if self.clock.now().saturating_duration_since(self.fetched_at)
            >= tick_for(self.config.tick_seconds)
        {
            fetch.request(FetchRequest::default(), false);
        }

        // Heartbeat: fast in-process atomic write on the main thread so the
        // exit path never races a background writer.
        if self.clock.effects_enabled() && heartbeat_write_due(self.last_heartbeat) {
            self.last_heartbeat = Some(self.clock.now());
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
        if self
            .clock
            .now()
            .saturating_duration_since(self.last_self_close_check)
            >= SELF_CLOSE_WATCHDOG
        {
            self.last_self_close_check = self.clock.now();
            let request = if self.self_close.confirming_empty() {
                FetchRequest::fresh_panes()
            } else {
                FetchRequest::default()
            };
            fetch.request(request, false);
        }
        if !self.inspect_view(|ui, _| ui.freeze.is_some()) {
            return;
        }
        let parked = self.parked(self.clock.now_ms());
        if self.watched()
            && self.inspect_view(|ui, _| {
                ui.freeze.as_ref().is_some_and(|hold| {
                    self.clock.timestamp().as_millisecond() >= hold.deadline_ms(parked)
                })
            })
        {
            fetch.request(FetchRequest::default(), false);
        }
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
        if self.retire_superseded_push() {
            return Ok(());
        }
        let now = self.clock.now();
        let paint_blocked = self.prepare_paint(terminal, now);
        if let Some(push) = &mut self.pending_focus_push {
            if !self.should_exit && push.peer.blocked_by.is_none() {
                push.peer.blocked_by = if paint_blocked {
                    Some(PushBlock::PaintHold)
                } else if self.self_close.confirming_empty() {
                    Some(PushBlock::ConfirmingEmpty)
                } else {
                    None
                };
            }
            if now >= push.until || self.should_exit {
                let verdict = if self.should_exit {
                    PushVerdict::Exiting
                } else {
                    PushVerdict::Expired
                };
                if let Some(push) = self.pending_focus_push.take() {
                    push.finish(verdict);
                }
            }
        }
        let watched = self.watched();
        let foreground = self.foreground_paint_due(now, active, watched);
        // Once the tab has emptied, never paint again. A grow resize also
        // defers its paint until the sibling-count verdict releases the hold.
        if !self.should_exit && !self.self_close.confirming_empty() && !paint_blocked && foreground
        {
            if self.retire_superseded_push()
                || self.focus_peers.as_ref().is_some_and(|peers| {
                    !peers.accepts(
                        self.inspect_view(|_, view| view.last_folded_focus_intent),
                        self.clock.now(),
                    )
                })
            {
                return Ok(());
            }
            self.paint_now(terminal, now)?;
        } else if !active && !self.dirty {
            // Idle re-arm only: with a fold pending, the armed boundary must
            // hold so a paint already due within one frame is not pushed out.
            self.next_frame = now + animation_frame(&self.view.current);
        }
        Ok(())
    }

    fn prepare_paint(&mut self, terminal: &mut Terminal<PaneBackend>, now: Instant) -> bool {
        if self.dirty {
            let dirty_deadline = now + animation_frame(&self.view.current);
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
        let marker_due = self.inspect_view(|_, view| matches!(view.focus_marker, Some(FocusMarker::Dwelling(until)) if now >= until));
        (watched && (active || self.dirty)) && (now >= self.next_frame || (watched && marker_due))
    }

    fn paint_now(&mut self, terminal: &mut Terminal<PaneBackend>, now: Instant) -> Result<()> {
        let (paint, interval, nonce) = self.with_shared_view(false, false, |state| {
            frame_clock(
                now,
                state.view.anim_start,
                state.view.current.theme.display.resolved_refresh_ms(),
            )
            .stamp(&mut state.ui);
            let interval = render::animation_interval(
                &state.view.current,
                &state.ui,
                state.ui.animation_phase,
                state.alert_active(),
            );
            (
                state.dirty || interval.is_some() || state.focus_marker_due(now),
                interval.unwrap_or(animation_frame(&state.view.current)),
                state.view.last_folded_focus_intent,
            )
        });
        if paint {
            if self.retire_superseded_push()
                || self
                    .focus_peers
                    .as_ref()
                    .is_some_and(|peers| !peers.accepts(nonce, self.clock.now()))
            {
                return Ok(());
            }
            self.draw_frame(terminal, now, false)?;
            self.dirty = false;
        }
        self.next_frame = next_frame_after(self.next_frame, now, interval);
        Ok(())
    }

    fn draw_frame(
        &mut self,
        terminal: &mut Terminal<PaneBackend>,
        now: Instant,
        user_input: bool,
    ) -> Result<()> {
        if terminal.backend().is_unsized() {
            return Ok(());
        }
        loop {
            let (token, snapshot, view, alert_active) = self.paint_transition(|state| {
                frame_clock(
                    now,
                    state.view.anim_start,
                    state.view.current.theme.display.resolved_refresh_ms(),
                )
                .stamp(&mut state.ui);
                (
                    state.view_token,
                    state.view.current.clone(),
                    state.ui.view.clone(),
                    state.alert_active(),
                )
            });
            let scratch = std::mem::replace(&mut self.ui.view, view);
            self.paint
                .refresh_view(&mut self.ui, &snapshot, alert_active);
            self.ui.view = scratch;
            let nonce = self.inspect_view(|_, view| view.last_folded_focus_intent);
            let permit = self
                .focus_peers
                .as_ref()
                .map(|peers| peers.paint_permit(nonce, self.clock.now()));
            if matches!(permit, Some(None)) {
                return Ok(());
            }
            let events = self.events().clone();
            let composed = self.paint_transition(|state| {
                if state.view_token != token {
                    return None;
                }
                if permit
                    .as_ref()
                    .and_then(Option::as_ref)
                    .is_some_and(|permit| !permit())
                {
                    return None;
                }
                Some(state.compose_frame(terminal, now, user_input, &events))
            });
            let Some((result, frame)) = composed else {
                continue;
            };
            let output = frame.write(terminal.backend_mut());
            result.and(output)?;
            self.painted_at = self.clock.now();
            for (push, verdict) in self.paint_reports.drain(..) {
                push.finish_painted(verdict, self.painted_at);
            }
            return Ok(());
        }
    }

    fn compose_frame(
        &mut self,
        terminal: &mut Terminal<PaneBackend>,
        now: Instant,
        user_input: bool,
        events: &EventStore,
    ) -> (std::io::Result<()>, BufferedFrame) {
        if self.shared_view_id().is_some() && self.ui.baseline_pane.is_none() {
            self.ui.baseline_pane = self.baseline_seed.take();
            reconcile_selection(&mut self.ui, &self.view.current, None);
        }
        self.ui.focus_marker_ready = self.watched()
            && match self.view.focus_marker {
                Some(FocusMarker::Ready) => true,
                Some(FocusMarker::Dwelling(until)) => now >= until,
                None => false,
            };
        // The notice is pane chrome over the borrowed view's gate, which another pane may have moved.
        self.ui.gate_notice = self.view.gate.rule.map(|rule| render::GateNotice { rule });
        let user_input = user_input || self.ui.scroll_ease.in_flight();
        let interactions = std::mem::take(&mut self.ui.interactions);
        let roster = std::mem::take(&mut self.ui.painted_roster);
        let rows = std::mem::take(&mut self.ui.painted_rows);
        let (result, frame) = self.paint.compose_frame(
            terminal,
            &self.view.current,
            self.view.health.alert.as_ref(),
            &mut self.ui,
        );
        if result.is_err() {
            return (result, frame);
        }
        self.painted_at = self.clock.now();
        self.paints += 1;
        self.painted_height = terminal.get_frame().area().height;
        self.painted_baseline.clone_from(&self.ui.baseline_pane);
        self.baseline_seed = None;
        self.painted_selection.clone_from(&self.ui.drawn_selection);
        let selected_visible = self
            .ui
            .drawn_selection
            .as_ref()
            .is_some_and(|selected| self.ui.painted_rows.iter().any(|(id, _)| id == selected));
        if selected_visible {
            self.view.focus_marker = None;
            self.ui.focus_marker_ready = false;
        } else if self.ui.focus_marker_ready {
            self.view.focus_marker = Some(FocusMarker::Ready);
        }
        self.ui.last_order = layout_freeze::capture_order(&self.view.current, &self.ui);
        self.ui.arrival_pending = false;
        if user_input || !self.watched() {
            self.click_guard = None;
        } else if self
            .click_guard
            .as_ref()
            .is_none_or(|guard| now >= guard.until)
            && rows.iter().any(|row| !self.ui.painted_rows.contains(row))
        {
            self.click_guard = Some(ClickGuard {
                until: now + crate::sidebar::timing::CLICK_GUARD,
                interactions,
                roster,
            });
        }
        self.observe_paint(now, events);
        (Ok(()), frame)
    }

    fn arm_paint_hold_on_grow(&mut self, width: u16, now: Instant) -> bool {
        if !self.paint_hold.is_engaged()
            && self.self_close.seen_sibling
            && resize_grew(self.prev_width, width)
            && grow_beyond_legit(width, self.width_control.max_legit_cols())
        {
            self.paint_hold.engage(
                now,
                self.clock.timestamp_at(now).as_millisecond().max(0) as u64,
            );
            return true;
        }
        false
    }

    /// Fold one fetch outcome into the render state: gate it against the
    /// last-known-good frame, update health, snapshot, and selection, and
    /// report whether the loop should exit.
    fn apply_fetch_outcome(
        &mut self,
        update: FetchUpdate,
        allow_shared_filter_sync: bool,
    ) -> ApplyOutcome {
        let (update, _) = update.into_parts();
        let snapshot_ok = matches!(&update, FetchUpdate::Snapshot { .. });
        let now = self.clock.timestamp();
        let searching = self.ui.search_draft.is_some();
        let (prev_good, rejected, diagnostics) = self.view.commit_fetch(update, &mut self.ui, now);
        self.fold_effects.diagnostics.extend(diagnostics);
        self.ui.gate_notice = self.view.gate.rule.map(|rule| render::GateNotice { rule });
        if let Some(alert) = self
            .view
            .health
            .alert
            .as_ref()
            .filter(|alert| alert.is_active())
        {
            self.fold_effects.degraded = Some(alert.reason.clone());
        }
        self.dirty |= searching && self.ui.search_draft.is_none();
        let authoritative_filter_fold = allow_shared_filter_sync
            && snapshot_ok
            && !rejected
            && self.view.fold_inputs.as_ref().is_none_or(|inputs| {
                self.view
                    .filter_observed_at
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
            self.clock.instant_at(now),
        );
        self.view.fold_spend_rolls(&mut self.ui, &self.clock);
        self.exit_verdict(rejected)
    }

    fn exit_verdict(&mut self, rejected: bool) -> ApplyOutcome {
        // A renderer degraded this long is non-functional. Close the attachment so the supervisor reattaches in place while the mux/store outage clears.
        if degraded_too_long(&self.view.health, self.clock.timestamp()) {
            self.fold_effects.exit = Some(FoldExit::Degraded(
                self.view
                    .health
                    .alert
                    .as_ref()
                    .map(|alert| alert.reason.clone()),
            ));
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
            self.view
                .current
                .own_view
                .as_ref()
                .map(|view| view.sibling_count),
            self.clock.now(),
        ) {
            self.fold_effects.exit = Some(FoldExit::EmptyTab);
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

    fn sweep_read_receipts(&mut self, now: Timestamp) -> (Option<PaneId>, bool) {
        if self.fold_preparation.is_none() {
            return self.fold_transition(|state| state.sweep_read_receipts(now));
        }
        if self.shared_view_id().is_none() {
            return (None, false);
        }
        let marks = self
            .fold_preparation
            .as_ref()
            .expect("fold transition prepared read marks")
            .marks
            .clone();
        self.prepare_tab_arrival();
        let (focused, clear) = self.view.read_receipts(
            &mut self.ui,
            &ReadContext {
                own_pane: self.config.own_pane.as_ref(),
                marks: &marks,
                now,
                at: self.clock.now(),
            },
        );
        let live: HashSet<String> = self.view.current.rows().map(|row| row.id.clone()).collect();
        let cleared = !clear.ids.is_empty();
        let mut clear = clear;
        clear.ids.retain(|id| live.contains(id));
        self.fold_effects.receipts.push((clear, now));
        (focused, cleared)
    }

    fn prepare_tab_arrival(&mut self) {
        let arriving = self.ui.viewing_own_tab == Some(false)
            && self
                .view
                .current
                .own_view
                .as_ref()
                .zip(self.config.own_pane.as_ref())
                .is_some_and(|(view, own)| {
                    own_tab_viewed(&self.view.current.viewed_panes, view, own)
                });
        if arriving {
            self.next_frame = self.clock.now();
        }
    }

    #[cfg(test)]
    fn sweep_tab_read_receipts(
        &mut self,
        focused_row_id: Option<&str>,
        marks: &ReadMarks,
        now: Timestamp,
        clear: &mut ReadClear,
    ) {
        self.prepare_tab_arrival();
        self.view.sweep_tab_read_receipts(
            &mut self.ui,
            focused_row_id,
            &ReadContext {
                own_pane: self.config.own_pane.as_ref(),
                marks,
                now,
                at: self.clock.now(),
            },
            clear,
        );
    }

    fn reconcile_selection_and_order(
        &mut self,
        prev_good: &SidebarSnapshot,
        prev_selected: Option<PaneId>,
        focused_pane: Option<PaneId>,
        cleared: bool,
        authoritative_filter_fold: bool,
        now: Instant,
    ) {
        if self.fold_preparation.is_none() {
            self.fold_transition(|state| {
                state.reconcile_selection_and_order(
                    prev_good,
                    prev_selected,
                    focused_pane,
                    cleared,
                    authoritative_filter_fold,
                    now,
                );
            });
            return;
        }
        let filter = authoritative_filter_fold.then(|| {
            if let Some(inputs) = &self.view.fold_inputs {
                return inputs.filter.clone();
            }
            let filter = self
                .fold_preparation
                .as_ref()
                .expect("fold transition prepared filter")
                .filter
                .clone();
            self.view.filter_observed_at = Some(self.clock.now());
            filter
        });
        let parked = self.parked(self.clock.timestamp_at(now).as_millisecond() as u64);
        let (persist_filter, expired) = self.view.reconcile_selection_and_order(
            &mut self.ui,
            SelectionFold {
                previous: prev_good,
                selected: prev_selected,
                focused: focused_pane,
                cleared,
                filter,
                mux: self.config.mux,
                clock: &self.clock,
                now,
                parked,
            },
        );
        self.hold_expired_since_paint |= expired;
        if persist_filter {
            self.fold_effects.filter = Some(self.ui.make_up_filter.clone());
        }
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
            self.fold_effects.traces.push(FocusTraceEvent::FoldDecided {
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
                panes_produced_at_ms: self.view.current.panes_produced_at_ms,
                panes_observed_at_ms: self.view.current.panes_observed_at_ms,
                snapshot_focused_pane: self.view.current.focused_pane.clone(),
                fused_event_sent_at_ms,
                own_view: self.view.current.own_view.is_some(),
                baseline: session_focus_baseline(&self.view.current, self.config.own_pane.as_ref()),
                selected_before,
                selected_after: self.ui.selected_pane.clone(),
            });
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

    fn focus_anchor(&mut self, observed: Instant) -> Option<crate::mux::focus_anchor::FocusAnchor> {
        if self.view_scope.is_none() {
            return self.fold_transition(|state| state.focus_anchor(observed));
        }
        let loaded = self
            .view
            .fold_inputs
            .is_none()
            .then(|| {
                self.fold_preparation.as_ref().map_or_else(
                    || crate::mux::focus_anchor::load(&self.runtime),
                    |prepared| prepared.anchor.clone(),
                )
            })
            .flatten();
        let own_pane = self.shared_view_id().and(self.config.own_pane.as_ref());
        self.view
            .observe_focus_anchor(&self.config.session_name, observed, loaded, own_pane)
    }

    fn focus_placement(&self, pane: &PaneId) -> Option<crate::mux::focus_anchor::FocusPlacement> {
        self.inspect_view(|view, context| {
            let row = context.current.row_owning_pane(pane)?;
            self.ui
                .painted_rows
                .iter()
                .find(|(id, _)| id == &row.id)
                .map(|(id, y)| crate::mux::focus_anchor::FocusPlacement {
                    row_id: id.clone(),
                    viewport_row: y - self.ui.scroll_zone_top as isize,
                    held: view
                        .held_expansions
                        .iter()
                        .chain(view.drawn_selection.iter())
                        .cloned()
                        .collect(),
                })
        })
    }

    fn owns_focus_target(&self, pane: &PaneId) -> bool {
        self.view.current.own_view.as_ref().is_some_and(|view| {
            self.config
                .own_pane
                .as_ref()
                .is_some_and(|own| own_tab_viewed(std::slice::from_ref(pane), view, own))
        })
    }

    fn focus_peers_for(&self, pane: &PaneId) -> Option<FocusPeers> {
        if self.owns_focus_target(pane) {
            return None;
        }
        self.focus_peers.clone()
    }

    fn focus_push_size_matches(&self, push: &FocusPush, terminal: &Terminal<PaneBackend>) -> bool {
        let Some(expected) = self.config.own_pane.as_ref().and_then(|pane| {
            push.expected_panes
                .as_ref()
                .and_then(|panes| panes.get(pane))
        }) else {
            return false;
        };
        terminal
            .size()
            .is_ok_and(|size| expected.cols == Some(size.width) && expected.rows == size.height)
    }

    fn painted_focus_verdict(
        &self,
        anchor: &crate::mux::focus_anchor::FocusAnchor,
        push: Option<&FocusPush>,
    ) -> Option<PushVerdict> {
        let portable_matches = self.inspect_view(|ui, _| {
            ui.last_focus_anchor_ms == anchor.issued_at_ms && ui.focus_placement.is_none()
        });
        if self.painted_focus_anchor_ms != anchor.issued_at_ms
            || !portable_matches
            || push
                .and_then(|push| push.expected_panes.as_ref())
                .and_then(|rows| {
                    self.config
                        .own_pane
                        .as_ref()
                        .and_then(|pane| rows.get(pane))
                })
                .is_some_and(|pane| pane.rows != self.painted_height)
        {
            return None;
        }
        let row = self.view.current.row_owning_pane(&anchor.pane_id)?;
        let drawn_selection = if self.view_scope.is_none() {
            &self.painted_selection
        } else {
            &self.ui.drawn_selection
        };
        if drawn_selection.as_ref() != Some(&row.id) {
            return None;
        }
        Some(
            if anchor.placement.as_ref().is_none_or(|placement| {
                self.ui.painted_rows.iter().any(|(id, y)| {
                    id == &placement.row_id
                        && *y == self.ui.scroll_zone_top as isize + placement.viewport_row
                })
            }) {
                PushVerdict::Painted
            } else {
                PushVerdict::Displaced
            },
        )
    }

    fn apply_focus_anchor(&mut self) {
        if self.fold_preparation.is_none() {
            self.fold_transition(Self::apply_focus_anchor);
            return;
        }
        let Some(anchor) = self.focus_anchor(self.clock.now()) else {
            return;
        };
        if self
            .view
            .apply_focus_anchor(&mut self.ui, &anchor, &self.clock)
        {
            self.fold_effects.retirements.push((anchor, None));
        }
    }

    fn pending_focus_intent(&mut self, now_ms: u64, now: Instant) -> Option<FocusPresentation> {
        if self.fold_preparation.is_none() {
            return self.fold_transition(|state| state.pending_focus_intent(now_ms, now));
        }
        let anchor = self.focus_anchor(now);
        let events = self.event_snapshot();
        let (presentation, retirement) =
            self.view
                .pending_focus_intent(anchor.as_ref(), &events, now_ms);
        if let (Some(anchor), Some(outcome)) = (anchor, retirement) {
            self.fold_effects.retirements.push((anchor, Some(outcome)));
        }
        presentation
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
                at: self.clock.timestamp(),
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

    fn focus_marker_due(&self, now: Instant) -> bool {
        self.watched()
            && self.inspect_view(|_, view| matches!(view.focus_marker, Some(FocusMarker::Dwelling(until)) if now >= until))
    }

    fn events(&self) -> MutexGuard<'_, EventStore> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn observe_commit(&mut self) {
        let Some(common) = &self.view.observation else {
            return;
        };
        let now_ms = self.clock.now_ms();
        let sig = observe::OwnFrameSig::extract(
            &self.view.current,
            (
                self.view.last_pulled_rows,
                common.pulled_panes_produced_at_ms,
            ),
            &self.event_snapshot(),
            self.view.gate.reject_streak,
            self.view.health.failure_streak,
            now_ms,
        );
        self.fold_effects.observations.push(sig);
    }

    fn arm_clicked_row(&mut self, pane: &PaneId, now: Instant) {
        self.clicked_row = self.view.current.row_owning_pane(pane).and_then(|row| {
            self.ui
                .painted_rows
                .iter()
                .find(|(id, _)| id == &row.id)
                .map(|(id, y)| ClickedRowWatch {
                    row_id: id.clone(),
                    y: *y,
                    clicked_at: now,
                    selection: self.ui.drawn_selection.clone(),
                    banner_shown: self.ui.banner_shown,
                    gliding: self.ui.scroll_ease.in_flight(),
                })
        });
    }

    fn observe_paint(&mut self, now: Instant, events: &EventStore) {
        self.painted_focus_anchor_ms = self.ui.last_focus_anchor_ms;
        let live_landing = self
            .view
            .focus_landing
            .as_ref()
            .is_some_and(|(anchor, _)| self.view.live_focus_source == Some(anchor.nonce));
        // This paint consumed the placement, so the landing is judged here or never.
        let landing = self
            .view
            .focus_landing
            .take()
            .and_then(|(anchor, applied_at)| {
                let push = self
                    .pending_focus_push
                    .as_ref()
                    .filter(|push| push.anchor.nonce == anchor.nonce);
                let verdict = self.painted_focus_verdict(&anchor, push)?;
                Some((verdict, anchor.placement?, applied_at))
            });
        if landing.is_some() && !live_landing {
            layout_freeze::arm_freeze(&mut self.ui, self.clock.timestamp_at(now).as_millisecond());
        }
        let mut seeded_landing = false;
        if let Some((PushVerdict::Displaced, placement, applied_at)) = landing
            && !self
                .clicked_row
                .as_ref()
                .is_some_and(|watch| watch.row_id == placement.row_id)
        {
            self.clicked_row = Some(ClickedRowWatch {
                row_id: placement.row_id,
                y: self.ui.scroll_zone_top as isize + placement.viewport_row,
                clicked_at: applied_at,
                selection: self.ui.drawn_selection.clone(),
                banner_shown: self.ui.banner_shown,
                gliding: false,
            });
            seeded_landing = true;
        }
        let verdict = self.pending_focus_push.as_ref().and_then(|push| {
            if now >= push.until || !self.owns_focus_target(&push.anchor.pane_id) {
                return None;
            }
            self.painted_focus_verdict(&push.anchor, Some(push))
        });
        if let Some(verdict) = verdict
            && let Some(push) = self.pending_focus_push.take()
        {
            self.paint_reports.push((push, verdict));
        }
        if let Some(push) = &mut self.pending_focus_push {
            push.peer.unmatched_paints = push.peer.unmatched_paints.saturating_add(1);
            if self
                .view
                .current
                .row_owning_pane(&push.anchor.pane_id)
                .is_some_and(|row| self.ui.drawn_selection.as_ref() == Some(&row.id))
            {
                push.peer.misplaced_paints = push.peer.misplaced_paints.saturating_add(1);
            }
        }
        let hold_expired = std::mem::take(&mut self.hold_expired_since_paint);
        let Some(mut watch) = self.clicked_row.take() else {
            return;
        };
        let elapsed = now.saturating_duration_since(watch.clicked_at);
        if elapsed >= LAYOUT_FREEZE && !seeded_landing {
            return;
        }
        let to_y = self
            .ui
            .painted_rows
            .iter()
            .find(|(id, _)| id == &watch.row_id)
            .map(|(_, y)| *y);
        let cause = if self.ui.anchor_clamped {
            ClickedRowDisplacementCause::NoRoomClamp
        } else if hold_expired && !seeded_landing {
            ClickedRowDisplacementCause::HoldExpired
        } else if watch.banner_shown != self.ui.banner_shown {
            ClickedRowDisplacementCause::BannerToggled
        } else if watch.selection != self.ui.drawn_selection {
            ClickedRowDisplacementCause::SelectionChanged
        } else {
            ClickedRowDisplacementCause::HeightAboveChanged
        };
        watch.banner_shown = self.ui.banner_shown;
        watch.selection = self.ui.drawn_selection.clone();
        let glided = std::mem::replace(&mut watch.gliding, self.ui.scroll_ease.in_flight());
        let kind = (!glided && to_y != Some(watch.y)).then(|| AnomalyKind::ClickedRowDisplaced {
            row_id: watch.row_id.clone(),
            from_y: watch.y,
            to_y,
            dt_ms: duration_millis(elapsed),
            cause,
        });
        if let Some(y) = to_y {
            watch.y = y;
            self.clicked_row = Some(watch);
        }
        let Some(kind) = kind else {
            return;
        };
        let sig = observe::OwnFrameSig::extract(
            &self.view.current,
            (
                self.view.last_pulled_rows,
                self.view
                    .observation
                    .as_ref()
                    .and_then(|sig| sig.pulled_panes_produced_at_ms),
            ),
            events,
            self.view.gate.reject_streak,
            self.view.health.failure_streak,
            self.clock.timestamp_at(now).as_millisecond().max(0) as u64,
        );
        self.fold_effects
            .anomalies
            .push(observe::AnomalyDraft::from_own_sig(
                &sig,
                kind,
                Some(duration_millis(LAYOUT_FREEZE)),
            ));
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

/// Ping every sidebar in the room to refold after a mark read/unread — the
/// host prunes or keeps the episode and peer tabs converge on the new state.
fn wake_room(runtime: &RuntimePaths) {
    if let Err(err) = crate::wakeup::wake_store_delta(runtime, None, None) {
        debug!(error = %err, "mark read/unread sidebar wake failed");
    }
}

#[cfg(test)]
mod tests;
