//! `LoopState` lends client views through `ViewAccess` to pane-local `PaneState` transitions.
//! Fetch application, focus repair, maintenance deadlines, and paint eligibility stay pane-local.

use std::collections::HashSet;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::diag::focus_trace::{FocusTraceEvent, PushBlock, PushVerdict};
use crate::diag::record::{
    AnomalyKind, ClickedRowDisplacementCause, SidebarWidthControlTrigger as WidthControlTrigger,
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
use crate::sidebar_pane::render::{self, UiState, ViewState};
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
    FetchHandle, FetchPhase, FetchRequest, FetchUpdate, SnapshotSource, WorkspaceFold,
};
use super::focus::{FocusRepairDispatch, spawn_pane_focus};
#[cfg(test)]
use super::focus::{finish_focus_request, request_focus_until};
use super::focus_push::{FocusPeers, FocusPush};
use super::gate::{GateState, gate_remaining};
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
use super::{Result, ServeConfig};
#[cfg(test)]
use crate::mux::focus_anchor::FocusActionError;

const RESIZE_CAPS_SETTLE: Duration = Duration::from_millis(300);

mod shared;

use shared::{Route, ViewAccess, ViewBorrow, ViewTarget};

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

impl FoldPreparation {
    fn load(
        runtime: &RuntimePaths,
        inputs: Option<Arc<super::fetch::FoldInputs>>,
        visible: Option<bool>,
        events: EventStore,
    ) -> Self {
        Self {
            visible,
            events,
            marks: ReadMarks::load(runtime),
            filter: inputs.as_ref().map_or_else(
                || crate::sidebar::body_filter::load(runtime),
                |inputs| inputs.filter.clone(),
            ),
            anchor: inputs.map_or_else(
                || crate::mux::focus_anchor::load(runtime),
                |inputs| inputs.anchor.clone(),
            ),
        }
    }
}

#[derive(Default)]
struct FoldEffects {
    diagnostics: Vec<crate::diag::record::DiagEvent>,
    traces: Vec<FocusTraceEvent>,
    receipts: Vec<(ReadClear, Timestamp)>,
    filter: Option<BodyLens>,
    retirements: Vec<(
        ViewTarget,
        crate::mux::focus_anchor::FocusAnchor,
        Option<FocusObservationOutcome>,
    )>,
    observations: Vec<observe::OwnFrameSig>,
    anomalies: Vec<observe::AnomalyDraft>,
    degraded: Option<String>,
    invalidate: bool,
    retry_focus_repair: bool,
}

pub(super) struct LoopState {
    pub(super) pane: PaneState,
    pub(super) views: ViewAccess,
    pub(super) room_events: Option<super::plane::RoomEvents>,
}

pub(super) struct PaneState {
    pub(super) fetch: FetchHandle,
    #[cfg(test)]
    pub(super) content_applications: u64,
    #[cfg(test)]
    pub(super) projections: u64,
    #[cfg(test)]
    fold_preparations: u64,
    fold_effects: FoldEffects,
    baseline_seed: Option<PaneId>,
    /// The keymap beside the `[sidebar.keys]` table it was built from, rebuilt only when the snapshot's table differs.
    nav_keys: (crate::config::SidebarKeys, super::NavKeymap),
    config: ServeConfig,
    clock: LoopClock,
    runtime: RuntimePaths,
    host_wake_path: PathBuf,
    diag: crate::diag::DiagSink,
    own: super::views::Cursor,
    pending_focus_push: Option<FocusPush>,
    painted_focus_anchor_ms: u64,
    painted_at: Instant,
    painted_height: u16,
    paints: u64,
    paint_reports: Vec<(FocusPush, PushVerdict)>,
    optimistic_watch_until: Option<Instant>,
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
    last_self_close_check: Instant,
    last_heartbeat: Option<Instant>,
    prev_width: Option<u16>,
    caps_refresh_deadline: Option<Instant>,
    width_control: WidthController,
    pub(super) titled: bool,
    pub(super) should_exit: bool,
    pub(super) request_reload: Box<dyn Fn() + Send>,
    pub(super) baseline_changed: BaselineReporter,
    last_reported_baseline: Option<PaneId>,
    painted_baseline: Option<PaneId>,
    painted_selection: Option<String>,
}

pub(super) fn handle_wakeup(
    wakeup: Wakeup,
    view: &mut ViewState,
    ui: &mut UiState,
    snapshot: &SidebarSnapshot,
    keymap: &super::NavKeymap,
    alert_active: bool,
) -> InputOutcome {
    let wakeup = match wakeup {
        Wakeup::Press { code, mods } => {
            let mode = if view.help_visible {
                InputMode::Help
            } else if view.search_draft.is_some() {
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
    if view.help_visible
        && matches!(
            wakeup,
            Wakeup::Key(_) | Wakeup::MouseClick { .. } | Wakeup::Scroll { .. }
        )
    {
        view.help_visible = false;
        return InputOutcome::redraw();
    }
    match wakeup {
        Wakeup::Key(KeyAction::Search) if alert_active => InputOutcome::default(),
        Wakeup::Key(action) => handle_key(action, view, ui, snapshot),
        Wakeup::MouseClick { column, row } => {
            if let Some(render::HitTarget::Row(index)) = ui.interactions.target_at(column, row)
                && !ui
                    .painted_roster
                    .get(index)
                    .is_some_and(|id| view.visible_roster(snapshot).ordinal_of_id(id).is_some())
            {
                return InputOutcome::default();
            }
            if view.search_draft.is_some() {
                // Resolve the pane against the painted draft before clearing widens the roster.
                let pane = ui
                    .interactions
                    .target_at(column, row)
                    .and_then(|target| click_focus_pane(&target, view, ui, snapshot));
                if pane.is_some() {
                    let click = handle_mouse_click(column, row, view, ui, snapshot);
                    let mut outcome = finish_search(view, snapshot, false);
                    outcome.redraw |= click.redraw;
                    outcome.effects.extend(click.effects);
                    return outcome;
                }
            }
            let mut outcome = finish_search(view, snapshot, true);
            let click = handle_mouse_click(column, row, view, ui, snapshot);
            outcome.redraw |= click.redraw;
            outcome.effects.extend(click.effects);
            outcome
        }
        Wakeup::Scroll { down } => handle_scroll(down, view, ui),
        Wakeup::Resize => InputOutcome::redraw(),
        // The serve loop intercepts ticks, room events and worker completions;
        // raw presses resolve against the current UI mode.
        Wakeup::Tick
        | Wakeup::Event(_)
        | Wakeup::Press { .. }
        | Wakeup::Snapshot
        | Wakeup::FocusFailed { .. } => InputOutcome::default(),
    }
}

impl LoopState {
    pub(in crate::sidebar_pane::app) fn inspect_view<T>(
        &self,
        inspect: impl FnOnce(&ViewState, &ViewContext) -> T,
    ) -> T {
        self.views.inspect(&self.pane.config.own_pane, inspect)
    }

    fn observed_watched(&self) -> bool {
        let (resolved, observed) = self
            .views
            .visibility(&self.pane.config.own_pane, || self.pane.clock.now());
        resolved || observed.unwrap_or_else(|| self.pane.watches_snapshot(self.views.mirror()))
    }

    pub(in crate::sidebar_pane::app) fn share_views(
        &mut self,
        views: Arc<Mutex<super::views::Views>>,
        revision: u64,
    ) {
        self.pane.baseline_seed = self.views.share(views);
        self.pane.own = super::views::Cursor {
            revision,
            accepted: None,
        };
    }

    /// Routes like production: a hidden, unresolved pane borrows Scratch and drops its shared
    /// view's paint mapping, so a test that needs that view's id reads it before hiding the pane.
    #[cfg(test)]
    fn with_view_for_test<T>(
        &mut self,
        route: Route<'_>,
        advance: impl FnOnce(&mut PaneState, &mut ViewBorrow<'_>) -> T,
    ) -> T {
        self.views.with(&mut self.pane, route, advance)
    }

    fn fold_transition<T>(
        &mut self,
        advance: impl FnOnce(&mut PaneState, &mut ViewBorrow<'_>, &FoldPreparation) -> T,
    ) -> T {
        let inputs = self.inspect_view(|_, view| view.fold_inputs.clone());
        self.view_transition(false, inputs, None, advance)
    }

    fn view_transition<T>(
        &mut self,
        input: bool,
        inputs: Option<Arc<super::fetch::FoldInputs>>,
        visible: Option<bool>,
        advance: impl FnOnce(&mut PaneState, &mut ViewBorrow<'_>, &FoldPreparation) -> T,
    ) -> T {
        #[cfg(test)]
        {
            self.pane.fold_preparations += 1;
        }
        let events = self.events().clone();
        let prepared = FoldPreparation::load(&self.pane.runtime, inputs, visible, events);
        let result = self
            .views
            .with(&mut self.pane, Route::fold(input, &prepared), |pane, v| {
                advance(pane, v, &prepared)
            });
        self.finish_view_effects();
        result
    }

    fn paint_transition<T>(
        &mut self,
        advance: impl FnOnce(&mut PaneState, &mut ViewBorrow<'_>) -> T,
    ) -> T {
        let result = self.views.with(&mut self.pane, Route::PAINT, advance);
        self.finish_view_effects();
        result
    }

    fn finish_view_effects(&mut self) {
        let effects = std::mem::take(&mut self.pane.fold_effects);
        emit_diagnostics(&self.pane.diag, effects.diagnostics);
        for trace in effects.traces {
            self.pane.diag.trace_focus(trace, HashSet::new());
        }
        for (clear, now) in effects.receipts {
            if let Err(err) = write_read_marks(&self.pane.runtime, clear.ids, now.as_millisecond())
            {
                debug!(error = %err, "sidebar read-mark write failed");
            }
            emit_unread_cleared_trace(&self.pane.diag, &clear.trace);
        }
        if let Some(filter) = effects.filter {
            self.persist_body_filter(filter);
        }
        for (target, anchor, outcome) in effects.retirements {
            if crate::mux::focus_anchor::clear_matching(&self.pane.runtime, anchor.nonce) {
                self.views
                    .retire_anchor(target, &anchor, || self.pane.clock.now());
                if let Some(outcome) = outcome {
                    self.pane.record_focus_resolution(&anchor, outcome);
                }
            }
        }
        for sig in effects.observations {
            self.pane.observer.observe_into(
                sig,
                &self.pane.observe_tx,
                &self.pane.config.instance_id,
            );
        }
        for draft in effects.anomalies {
            self.pane.observer.send_draft(
                draft,
                &self.pane.observe_tx,
                &self.pane.config.instance_id,
            );
        }
        if let Some(reason) = effects.degraded {
            warn!(target: SIDEBAR_HEALTH_TARGET, reason, "sidebar refresh degraded");
        }
        if effects.invalidate {
            self.pane.own.accepted = None;
        }
        if effects.retry_focus_repair {
            self.retry_pending_focus_repair();
        }
    }

    #[cfg(feature = "testkit")]
    pub(super) fn set_play_cosmetic_phase(&mut self, phase: Option<u64>) {
        self.views.with(&mut self.pane, Route::READ, |_, v| {
            v.state.cosmetic_phase = phase;
        });
    }

    #[cfg(feature = "testkit")]
    pub(super) fn prepare_play_pet(&mut self, snapshot: &SidebarSnapshot) -> bool {
        let presentation = self.inspect_view(|ui, _| ui.clone());
        self.pane
            .paint
            .prepare_play_pet(&presentation, &mut self.pane.ui, snapshot)
    }

    #[cfg(feature = "testkit")]
    pub(super) fn play_frame_settled(&self) -> bool {
        !self.pane.dirty
            && self.inspect_view(|ui, _| {
                !ui.tally.any_rolling(ui.animation_phase)
                    && !ui.cost_rolls.any_rolling(ui.animation_phase)
                    && !ui.scroll_ease.in_flight()
            })
    }

    fn parked(&self, now_ms: u64) -> bool {
        let pane = &self.pane.config.own_pane;
        let (sampled, event) = {
            let views = self
                .views
                .registry
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            (
                views.has_sample().then(|| views.parked(pane)),
                views.attributed_focus_event(pane),
            )
        };
        let events = self.events().clone();
        self.inspect_view(|_, view| parked(view, &events, pane, sampled, event, now_ms))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        clock: LoopClock,
        config: ServeConfig,
        baseline_pane: Option<PaneId>,
        runtime: RuntimePaths,
        host_wake_path: PathBuf,
        diag: crate::diag::DiagSink,
        fetch: FetchHandle,
        initial_width: Option<u16>,
        observe_tx: SyncSender<ObserveMsg>,
        pet_render_caps: PixelRenderCaps,
        pixel_slot: Option<PixelSlot>,
    ) -> Self {
        let snapshot_now = clock.timestamp();
        let current = SidebarSnapshot::build_with_agents(
            config.room.workspace_id.clone(),
            Vec::new(),
            snapshot_now,
        );
        let now = clock.now();
        let pixel_wrap = config.room.mux == MuxName::Tmux;
        let width = crate::mux::SidebarWidth::from_config(
            &crate::config::MachineConfig::load_lenient().theme,
        );
        let width_control = WidthController::new(
            runtime.clone(),
            config.room.session_name.clone(),
            clock.effects_enabled().then(|| config.own_pane.clone()),
            config.room.mux,
            width,
        );
        let make_up_filter = crate::sidebar::body_filter::load(&runtime);
        let nav_keys = (
            current.sidebar.keys.clone(),
            super::NavKeymap::from_config(&current.sidebar.keys),
        );
        Self {
            views: ViewAccess::new(
                ViewState {
                    make_up_filter,
                    baseline_pane: baseline_pane.clone(),
                    ..Default::default()
                },
                ViewContext::new(current, now),
            ),
            room_events: None,
            pane: PaneState {
                fetch,
                #[cfg(test)]
                content_applications: 0,
                #[cfg(test)]
                projections: 0,
                #[cfg(test)]
                fold_preparations: 0,
                nav_keys,
                config,
                clock,
                runtime,
                host_wake_path,
                diag,
                own: super::views::Cursor::default(),
                fold_effects: FoldEffects::default(),
                baseline_seed: None,
                pending_focus_push: None,
                painted_focus_anchor_ms: 0,
                painted_at: now,
                painted_height: 0,
                paints: 0,
                paint_reports: Vec::new(),
                optimistic_watch_until: None,
                pending_focus_repair: None,
                observer: observe::OwnObserver::default(),
                clicked_row: None,
                click_guard: None,
                hold_expired_since_paint: false,
                observe_tx,
                self_close: SelfCloseState::default(),
                ui: UiState::default(),
                paint: FramePainter::new(pet_render_caps, pixel_wrap, pixel_slot),
                room_caps: Arc::default(),
                focus_peers: None,
                input_jump: None,
                wake: None,
                dirty: true,
                paint_hold: PaintHold::default(),
                next_frame: now,
                last_self_close_check: now,
                last_heartbeat: None,
                prev_width: initial_width,
                caps_refresh_deadline: None,
                width_control,
                titled: false,
                should_exit: false,
                request_reload: Box::new(|| {}),
                baseline_changed: Box::new(|_| {}),
                last_reported_baseline: baseline_pane.clone(),
                painted_baseline: baseline_pane,
                painted_selection: None,
            },
        }
    }

    /// Run one serve-loop iteration, the decision point for frame timing.
    ///
    /// Read activity again after dispatch so an input-started animation avoids the idle re-arm.
    pub(super) fn step(
        &mut self,
        inbox: &mut WakeupInbox,
        terminal: &mut Terminal<PaneBackend>,
    ) -> Result<()> {
        self.report_baseline();
        let (_, timeout) = self.frame_timing();
        inbox.wait(timeout);
        self.advance(inbox, terminal).map(|_| ())
    }

    /// Run the nonblocking iteration and return whether it painted and the next driver deadline.
    pub(super) fn advance(
        &mut self,
        inbox: &mut WakeupInbox,
        terminal: &mut Terminal<PaneBackend>,
    ) -> Result<(bool, Instant)> {
        let paints = self.pane.paints;
        self.retire_superseded_push();
        let pushed = if let Some(push) = self
            .pane
            .focus_peers
            .as_ref()
            .and_then(FocusPeers::take_push)
        {
            self.on_focus_push(push, terminal)?;
            true
        } else {
            false
        };
        self.report_baseline();
        if !pushed {
            inbox.dispatch_next(self, terminal, Duration::ZERO)?;
        }
        // A pane the mux had not laid out at attach gets its title
        // with its first size.
        if !self.pane.titled {
            self.pane.titled = terminal.backend_mut().set_pane_title()?;
        }

        self.run_maintenance(terminal);
        self.run_width_control_backstop(terminal);
        let (active, _) = self.frame_timing();
        self.paint_frame_if_due(terminal, active)?;
        let (_, timeout) = self.frame_timing();
        let now = self.pane.clock.now();
        let mut deadline = now + timeout;
        if self.watched()
            && let Some(remaining) =
                self.freeze_remaining(self.pane.clock.timestamp_at(now).as_millisecond())
        {
            deadline = deadline.min(now + remaining.max(FRAME_MIN_TIMEOUT));
        }
        Ok((self.pane.paints != paints, deadline))
    }

    pub(super) fn report_baseline(&mut self) {
        let baseline = &self.pane.painted_baseline;
        if *baseline != self.pane.last_reported_baseline {
            (self.pane.baseline_changed)(baseline.as_ref());
            self.pane.last_reported_baseline.clone_from(baseline);
        }
    }

    pub(super) fn seed(&mut self, fold: &WorkspaceFold) {
        let snapshot = Self::project_fold(
            fold,
            Some(&self.pane.config.own_pane),
            self.pane.config.refresh_ms_override,
        );
        #[cfg(test)]
        {
            self.pane.projections += 1;
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
            self.draw_frame(&mut terminal, self.pane.clock.now(), false)
                .expect("benchmark paint");
            assert!(self.pane.ui.theme_cache.is_some());
        }
        std::hint::black_box(self.frame_timing());
        self.watched()
    }

    fn freeze_remaining(&mut self, now_ms: i64) -> Option<Duration> {
        self.views.with(&mut self.pane, Route::READ, |pane, v| {
            pane.freeze_remaining(v, now_ms)
        })
    }

    fn frame_timing(&mut self) -> (bool, Duration) {
        self.views
            .with(&mut self.pane, Route::READ, PaneState::frame_timing)
    }

    /// Whether an attached client's focus currently lands in this sidebar's tab.
    /// Unknown own-view state reads as watched so uncertainty never suppresses
    /// motion.
    fn watched(&self) -> bool {
        // Input and corroborated resize keep a painting view warm between samples.
        if self
            .pane
            .optimistic_watch_until
            .is_some_and(|until| self.pane.clock.now() < until)
        {
            return true;
        }
        self.observed_watched()
    }

    pub(super) fn on_snapshot(&mut self) {
        self.drain_snapshot(true);
    }

    fn drain_snapshot(&mut self, allow_defer: bool) {
        let (pending, revision, completed) = {
            let views = self
                .views
                .registry
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            (
                views.latest.since(self.pane.own.revision),
                views.revision,
                self.pane.fetch.completed(),
            )
        };
        let deferred = allow_defer
            && pending.iter().all(FetchUpdate::is_success)
            && pending
                .iter()
                .find_map(|update| update.preview_for(&self.pane.config.own_pane))
                .is_some_and(|(own_view, viewed)| self.skips_hidden(own_view.as_ref(), viewed));
        if deferred {
            if completed {
                self.pane.last_self_close_check = self.pane.clock.now();
            }
        } else {
            for update in pending {
                self.apply_latest_snapshot(update);
            }
            self.pane.own.revision = revision;
        }
    }

    fn skips_hidden(&self, own_view: Option<&SidebarOwnView>, viewed_panes: &[PaneId]) -> bool {
        let hidden = !self.watched()
            && !self.pane.watches_view(own_view, viewed_panes)
            && own_view == self.views.mirror().own_view.as_ref()
            && !self.pane.should_exit
            && !self.pane.self_close.confirming_empty()
            && self.pane.pending_focus_repair.is_none();
        hidden
            && self.inspect_view(|_, view| {
                view.gate == GateState::default()
                    && view.health.failure_streak == 0
                    && view.tab_read_dwell_until.is_none()
                    && view.pending_input_focus.is_none()
            })
    }

    #[cfg(test)]
    fn admits_clock_only(&self, rejected: bool) -> bool {
        self.inspect_view(|state, context| self.pane.admits_clock_only(state, context, rejected))
    }

    #[cfg(test)]
    fn alert_active(&self) -> bool {
        self.inspect_view(|_, view| alert_active(view))
    }

    pub(super) fn share_width_geometry(
        &mut self,
        geometry: Arc<Mutex<crate::mux::zellij::WidthMemo>>,
    ) {
        self.pane.width_control.geometry = geometry;
    }

    /// The pane's cell aspect as its terminal reports it, read by whoever
    /// holds the pane's output.
    pub(super) fn set_probed_aspect(&mut self, aspect: Option<crate::config::CellAspect>) {
        self.pane.paint.set_probed_aspect(aspect);
    }

    pub(super) fn on_wakeup(
        &mut self,
        terminal: &mut Terminal<PaneBackend>,
        wakeup: Wakeup,
    ) -> Result<()> {
        match wakeup {
            Wakeup::FocusFailed { pane, error } => {
                self.on_focus_failed(pane, error);
                Ok(())
            }
            Wakeup::Snapshot => {
                self.on_snapshot();
                Ok(())
            }
            Wakeup::Event(envelope) => {
                self.on_event(terminal, envelope);
                Ok(())
            }
            // A recv timeout: the active grid reached a frame boundary, or the
            // pane's idle tick elapsed. Maintenance checks view deadlines, and
            // the frame phase advances animation and paints.
            Wakeup::Tick => Ok(()),
            Wakeup::Resize => {
                let settled_width = terminal.size().map(|s| s.width).ok();
                self.on_resize(terminal, settled_width)?;
                Ok(())
            }
            wakeup => self.on_input(wakeup, terminal),
        }
    }

    fn on_focus_failed(&mut self, pane: PaneId, error: String) {
        if !self.fail_rerouted_focus(&pane, &error) {
            self.fold_transition(|state, v, prepared| {
                state.on_focus_failed(v, prepared, pane, error)
            });
        }
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
        let mut views = self
            .views
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some((state, context)) = views.dispatched(pane) else {
            return false;
        };
        if context.pending_input_focus.as_ref() == Some(pane) {
            context.pending_input_focus = None;
        }
        let Some(label) = context.roll_back_focus(state, pane, self.pane.clock.now()) else {
            return false;
        };
        context.alert_focus_failure(&label, error, self.pane.clock.timestamp());
        true
    }

    pub(super) fn apply_latest_snapshot(&mut self, update: FetchUpdate) -> bool {
        self.apply_latest_snapshot_with_seed(update, false)
    }

    fn apply_latest_snapshot_with_seed(&mut self, update: FetchUpdate, seed: bool) -> bool {
        let inputs = match &update {
            FetchUpdate::Shared { context, .. } => Some(context.inputs.clone()),
            _ => None,
        };
        let visible = update
            .preview_for(&self.pane.config.own_pane)
            .map(|(own_view, viewed)| self.pane.watches_view(own_view.as_ref(), viewed));
        let mut reduction = None;
        let rejected = self.view_transition(false, inputs, visible, |state, v, prepared| {
            if v.target.shared_id().is_some() && v.latest {
                state.own.accepted = None;
                return v.context.gate != GateState::default();
            }
            if seed {
                return state.reduce_snapshot(v, prepared, update, true);
            }
            let clock_only = state.admits_clock_only(v.state, v.context, false);
            let config = &state.config;
            #[cfg(test)]
            let mut projections = 0;
            let update = state.own.replay(
                update,
                |fold| {
                    #[cfg(test)]
                    {
                        projections += 1;
                    }
                    Self::project_fold(fold, Some(&config.own_pane), config.refresh_ms_override)
                },
                clock_only,
            );
            #[cfg(test)]
            {
                state.projections += projections;
            }
            if update.is_success() {
                reduction = Some(update.is_unchanged());
            }
            state.reduce_snapshot(v, prepared, update, false)
        });
        if let Some(unchanged) = reduction {
            self.views.count_reduction(unchanged);
        }
        rejected
    }

    fn fold_fused_now(&mut self) {
        self.fold_transition(PaneState::fold_fused_now);
    }

    fn fold_focus_intent(&mut self, anchor: crate::mux::focus_anchor::FocusAnchor) {
        self.fold_transition(|pane, v, prepared| pane.fold_focus_intent(v, prepared, anchor));
    }

    fn on_focus_push(
        &mut self,
        mut push: FocusPush,
        terminal: &mut Terminal<PaneBackend>,
    ) -> Result<()> {
        let arrived = self.pane.clock.now();
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
        self.pane.optimistic_watch_until = Some(arrived + FOCUS_RESUME_WATCH_WINDOW);
        if let Some(source) = push.source_view {
            self.views
                .registry
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .land(source, &self.pane.config.own_pane, self.pane.clock.now());
            self.views.with(&mut self.pane, Route::READ, |_, v| {
                v.context.live_focus_source = Some(push.anchor.nonce);
            });
        }
        // Read the view the landing paints: a scratch fold while hidden folded into no view.
        if self.views.with(&mut self.pane, Route::READ, |_, v| {
            v.context.last_folded_focus_intent
        }) != Some(push.anchor.nonce)
        {
            self.prepare_action();
            push.peer.snapshot_ms = Some(push.elapsed_ms());
            if !push.is_current() {
                push.finish(PushVerdict::Superseded);
                return Ok(());
            }
            self.fold_focus_intent(push.anchor.clone());
            push.peer.folded_ms = Some(push.elapsed_ms());
        }
        if push.anchor.session_name != self.pane.config.room.session_name
            || !self.owns_focus_target(&push.anchor.pane_id)
        {
            push.finish(PushVerdict::NotDestination);
            return Ok(());
        }
        if self.pane.should_exit {
            if let Some(pending) = self.pane.pending_focus_push.take() {
                pending.finish(PushVerdict::Exiting);
            }
            push.finish(PushVerdict::Exiting);
            return Ok(());
        }
        if self.focus_push_size_matches(&push, terminal) {
            self.pane.paint_hold.release();
        }
        if !push.is_current() {
            push.finish(PushVerdict::Superseded);
            return Ok(());
        }
        if let Some(verdict) = self.painted_focus_verdict(&push.anchor, Some(&push)) {
            push.finish_painted(verdict, self.pane.painted_at);
            return Ok(());
        }
        if let Some(pending) = self.pane.pending_focus_push.replace(push) {
            pending.finish(PushVerdict::Superseded);
        }
        self.paint_frame_if_due(terminal, true)
    }

    fn known_non_owner(&self, target: &PaneId) -> bool {
        self.views.mirror().row_owning_pane(target).is_some()
            && !self.owns_focus_target(target)
            && self
                .views
                .mirror()
                .own_view
                .as_ref()
                .is_some_and(|own_view| {
                    self.pending_preview()
                        .is_none_or(|(pending, _)| pending.as_ref() == Some(own_view))
                })
    }

    fn retire_superseded_push(&mut self) -> bool {
        if self
            .pane
            .pending_focus_push
            .as_ref()
            .is_some_and(|push| !push.is_current())
        {
            if let Some(push) = self.pane.pending_focus_push.take() {
                push.finish(PushVerdict::Superseded);
            }
            return true;
        }
        false
    }

    pub(super) fn on_event(
        &mut self,
        terminal: &mut Terminal<PaneBackend>,
        envelope: SidebarEventEnvelope,
    ) {
        let sent_at_ms = envelope.sent_at_ms;
        let fold = self.event_needs_fold(&envelope.event);
        if matches!(
            &envelope.event,
            SidebarEvent::PaneClosed { .. }
                | SidebarEvent::PaneOpened { .. }
                | SidebarEvent::PanesChanged
        ) {
            let measured = terminal.size().ok().map(|size| size.width);
            self.pane
                .width_control
                .note_structural(sent_at_ms, measured, &self.pane.diag);
        }
        match envelope.event {
            SidebarEvent::WidthTargetChanged => {
                if fold {
                    self.prepare_action();
                }
                let measured = terminal.size().ok().map(|size| size.width);
                self.pane.width_control.reload_target(
                    &self.views.mirror().theme,
                    measured,
                    &self.pane.diag,
                );
            }
            SidebarEvent::BodyFilterChanged => {
                if fold {
                    self.prepare_action();
                }
                let filter = crate::sidebar::body_filter::load(&self.pane.runtime);
                self.views.with(&mut self.pane, Route::READ, |state, v| {
                    v.context.filter_observed_at = Some(state.clock.now());
                    if set_make_up_filter(v.state, &v.context.current, filter) {
                        state.dirty = true;
                    }
                });
            }
            event @ SidebarEvent::Notify { .. } => {
                if fold {
                    self.prepare_action();
                }
                self.handle_notification(terminal, event);
            }
            SidebarEvent::FocusStranded {
                pane_id,
                generation,
                clients,
            } => {
                if self.pane.config.own_pane != pane_id {
                    trace!(
                        pane = %pane_id,
                        own_pane = %self.pane.config.own_pane,
                        generation,
                        "sidebar ignored foreign strand",
                    );
                    return;
                }
                self.prepare_action();
                let repair = PendingFocusRepair {
                    pane_id,
                    generation,
                    clients,
                    sent_at_ms,
                };
                if !self.handle_focus_stranded(&repair) {
                    if let Some(replaced) = self.pane.pending_focus_repair.replace(repair) {
                        self.record_abandoned_focus_repair(
                            &replaced,
                            "superseded by a newer strand",
                        );
                    }
                    self.pane.fetch.request(FetchRequest::fresh_panes());
                }
            }
            SidebarEvent::FocusIntent { nonce, .. } => {
                if !fold {
                    return;
                }
                if self
                    .pane
                    .focus_peers
                    .as_ref()
                    .is_some_and(|peers| !peers.accepts(Some(nonce), self.pane.clock.now()))
                {
                    return;
                }
                if self.inspect_view(|_, view| view.last_folded_focus_intent) == Some(nonce) {
                    return;
                }
                // Set before draining, so the jump folds the latest into the view.
                if self.reveals_own_tab(&envelope.event) {
                    self.pane.optimistic_watch_until =
                        Some(self.pane.clock.now() + FOCUS_RESUME_WATCH_WINDOW);
                }
                self.prepare_action();
                if let Some(anchor) = self
                    .focus_anchor(self.pane.clock.now())
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
                    self.pane.optimistic_watch_until =
                        Some(self.pane.clock.now() + FOCUS_RESUME_WATCH_WINDOW);
                }
                if fold {
                    self.prepare_action();
                }
                self.handle_overlay_event(event, fold);
            }
            _ => {}
        }
    }

    fn pending_preview(&self) -> Option<(Option<SidebarOwnView>, Vec<PaneId>)> {
        self.views
            .registry
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .latest
            .since(self.pane.own.revision)
            .iter()
            .find_map(|update| update.preview_for(&self.pane.config.own_pane))
            .map(|(own_view, viewed)| (own_view, viewed.to_vec()))
    }

    fn event_needs_fold(&mut self, event: &SidebarEvent) -> bool {
        let preview = self.pending_preview();
        let (view, viewed) = preview.as_ref().map_or(
            (
                self.views.mirror().own_view.as_ref(),
                self.views.mirror().viewed_panes.as_slice(),
            ),
            |(view, viewed)| (view.as_ref(), viewed.as_slice()),
        );
        if !self.skips_hidden(view, viewed) {
            return true;
        }
        match event {
            SidebarEvent::FocusChanged { focused, .. } => self.pane.watches_view(view, focused),
            SidebarEvent::Notify { panes, .. } => view.is_some_and(|view| {
                panes
                    .iter()
                    .any(|pane| view.working_pane_ids.contains(pane))
            }),
            SidebarEvent::FocusIntent { nonce, .. } => {
                let view = view.cloned();
                self.focus_anchor(self.pane.clock.now())
                    .filter(|anchor| anchor.nonce == *nonce)
                    .is_some_and(|anchor| {
                        self.pane
                            .watches_view(view.as_ref(), std::slice::from_ref(&anchor.pane_id))
                    })
            }
            _ => false,
        }
    }

    fn prepare_action(&mut self) {
        // Require content before flushing, then invalidate reuse again:
        // the action can change renderer state after the drain.
        self.pane.own.accepted = None;
        self.drain_snapshot(false);
        self.pane.own.accepted = None;
    }

    fn handle_notification(&mut self, terminal: &mut Terminal<PaneBackend>, event: SidebarEvent) {
        if !self.pane.clock.effects_enabled() {
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
            &self.pane.config,
            terminal,
            self.views.mirror(),
            BellNotice {
                title: &title,
                body: &body,
                panes: &panes,
                recheck_unread,
                kind,
            },
            &self.pane.diag,
        ) {
            Ok(_) => {}
            Err(err) => debug!(error = %err, "terminal notification emit failed"),
        }
    }

    fn handle_focus_stranded(&mut self, repair: &PendingFocusRepair) -> bool {
        let now_ms = self.pane.clock.now_ms();
        let own_pane = self.pane.config.own_pane.clone();
        let verdict = self.views.with(&mut self.pane, Route::READ, |_, v| {
            focus_stranded_target(
                &v.context.current,
                v.state,
                &repair.pane_id,
                &repair.clients,
                &own_pane,
                repair.sent_at_ms,
                now_ms,
            )
        });
        let target = match verdict {
            Ok(target) => target,
            Err(reason) => {
                debug!(
                    pane = %repair.pane_id,
                    own_pane = %own_pane,
                    generation = repair.generation,
                    evidence = ?repair.clients,
                    own_view = ?self.views.mirror().own_view,
                    client_views = ?self.views.mirror().client_views,
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
        let (placement, order) = self.views.with(&mut self.pane, Route::READ, |state, v| {
            (
                state.focus_placement(v.state, v.context, &target),
                v.state.last_order.clone(),
            )
        });
        spawn_pane_focus(
            &self.pane.clock,
            self.pane.clock.now(),
            target,
            &self.pane.config.room.session_name,
            self.pane.runtime.clone(),
            crate::mux::focus_anchor::FocusOrigin::AutomaticRepair,
            expected,
            (placement, Some(order)),
            Some(dispatch),
            None,
            self.pane.wake.clone(),
        );
        true
    }

    /// Re-attempt a strand the last fold could not yet act on: the snapshot may
    /// still lack the owning view or a visible working sibling row. The repair
    /// stays viable for the event's whole TTL and retries on each fold. Past
    /// the TTL it is abandoned with a record.
    fn retry_pending_focus_repair(&mut self) {
        let Some(repair) = self.pane.pending_focus_repair.take() else {
            return;
        };
        if self.handle_focus_stranded(&repair) {
            return;
        }
        let now_ms = self.pane.clock.now_ms();
        if focus_repair_still_viable(repair.sent_at_ms, now_ms) {
            self.pane.pending_focus_repair = Some(repair);
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
            &self.pane.runtime,
            &FocusRepairRecord {
                at: self.pane.clock.timestamp(),
                nonce: None,
                workspace_id: self.pane.runtime.workspace_id.clone(),
                session_name: self.pane.config.room.session_name.clone(),
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
        let own = &self.pane.config.own_pane;
        let targets = match event {
            SidebarEvent::FocusChanged { focused, .. } => focused.as_slice(),
            SidebarEvent::FocusIntent { pane_id, .. } => std::slice::from_ref(pane_id),
            _ => return false,
        };
        let preview = self.pending_preview();
        let own_view = preview
            .as_ref()
            .map_or(self.views.mirror().own_view.as_ref(), |(view, _)| {
                view.as_ref()
            });
        targets.contains(own) || own_view.is_some_and(|view| own_tab_viewed(targets, view, own))
    }

    fn handle_overlay_event(&mut self, event: SidebarEvent, fold: bool) {
        // A relevant overlay fuses into the in-memory state and paints this
        // frame. The plane asks the producer to verify topology overlays; this
        // pane asks only for its own reveal. A resize-grow paint hold stays
        // held until a pulled sibling-count verdict releases it.
        let own = &self.pane.config.own_pane;
        let own_focused = self.reveals_own_tab(&event);
        let own_revealed = own_focused && !self.observed_watched();
        let own_unfocused = matches!(&event, SidebarEvent::FocusChanged { focused, unfocused }
            if unfocused.contains(own)
                || (self.views.mirror().client_views.len() == 1
                    && matches!(focused.as_slice(), [pane] if pane != own)));
        if fold {
            self.fold_fused_now();
        }
        if !self.pane.should_exit && own_revealed {
            self.pane.fetch.request(FetchRequest::fresh_panes());
        }
        if own_focused {
            self.pane.optimistic_watch_until =
                Some(self.pane.clock.now() + FOCUS_RESUME_WATCH_WINDOW);
        } else if own_unfocused {
            self.pane.optimistic_watch_until = None;
            self.views.with(&mut self.pane, Route::READ, |state, v| {
                v.state.help_visible = false;
                if v.state.search_draft.take().is_some() {
                    reconcile_selection(v.state, &v.context.current, None);
                    state.dirty = true;
                }
            });
        }
    }

    /// `settled_width` is the pane width observed after the resize wakeup,
    /// probed once at the dispatch site so tests can drive the feedback path.
    pub(super) fn on_resize(
        &mut self,
        terminal: &mut Terminal<PaneBackend>,
        settled_width: Option<u16>,
    ) -> Result<()> {
        if self.pane.clock.effects_enabled() {
            self.pane.caps_refresh_deadline = Some(self.pane.clock.now() + RESIZE_CAPS_SETTLE);
        }
        // A hidden resize is no viewing evidence, and revokes none: a jump that just landed here
        // keeps painting until the plane samples the tab.
        if self.observed_watched() {
            self.pane.optimistic_watch_until =
                Some(self.pane.clock.now() + FOCUS_RESUME_WATCH_WINDOW);
        }
        // Watched resize uses the newest fold; hidden content stays deferred.
        self.drain_snapshot(true);
        // Once a sibling has been seen, hold only a grow beyond the configured
        // cap or room override: that is the shape of space freed by a closing
        // sibling. Startup and attach relayouts land at the legitimate width
        // and paint immediately.
        let held_grow = match settled_width {
            Some(width) => {
                let grew = resize_grew(self.pane.prev_width, width);
                self.pane.prev_width = Some(width);
                grew && grow_beyond_legit(width, self.pane.width_control.max_legit_cols())
            }
            None => false,
        };
        self.run_width_control(terminal, WidthControlTrigger::ResizeFeedback);
        if held_grow
            && self.pane.self_close.seen_sibling
            && !self
                .pane
                .pending_focus_push
                .as_ref()
                .is_some_and(|push| self.focus_push_size_matches(push, terminal))
        {
            self.pane.dirty = true;
            self.pane
                .paint_hold
                .engage(self.pane.clock.now(), self.pane.clock.now_ms());
        } else {
            if self.apply_input([Wakeup::Resize], terminal)?.redraw {
                self.pane.dirty = false;
            }
            // A safe-width paint just landed; drop any stale hold a prior grow
            // left pending so it cannot suppress this frame.
            self.pane.paint_hold.release();
        }
        self.pane.last_self_close_check = self.pane.clock.now();
        // The heartbeat carries this pane's size for `sidebar frame`: restamp
        // it on this pass rather than a beat later.
        self.pane.last_heartbeat = None;
        // A resize is the mux telling us topology changed. Pull a fresh pane
        // list through the host and require a cache produced after
        // this signal.
        self.pane.fetch.request(FetchRequest::fresh_panes());
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
        self.pane
            .width_control
            .observe(size.width, trigger, &self.pane.diag);
    }

    fn run_width_control_backstop(&mut self, terminal: &mut Terminal<PaneBackend>) {
        self.pane.width_control.backstop(
            terminal.size().ok().map(|size| size.width),
            self.views
                .mirror()
                .own_view
                .as_ref()
                .map(|view| view.sibling_count),
            self.views.mirror().panes_observed_at_ms,
            &self.pane.diag,
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
        self.pane.paint.refresh_caps_with(
            mux,
            session_name,
            terminal.backend().cell_aspect(),
            detect,
        );
    }

    fn refresh_pet_render_caps_if_stale_with(
        &mut self,
        mux: MuxName,
        session_name: &str,
        now: Instant,
        terminal: &Terminal<PaneBackend>,
        detect: impl FnOnce(MuxName, &str, PixelRenderCaps) -> PixelRenderCaps,
    ) -> bool {
        let changed = if let Some(deadline) = self.pane.caps_refresh_deadline {
            if now < deadline {
                return false;
            }
            self.pane.caps_refresh_deadline = None;
            self.pane.paint.refresh_caps_with(
                mux,
                session_name,
                terminal.backend().cell_aspect(),
                detect,
            )
        } else {
            self.pane
                .paint
                .refresh_caps_if_stale_with(mux, session_name, now, detect)
        };
        self.pane.dirty |= changed;
        changed
    }

    pub(super) fn on_input(
        &mut self,
        wakeup: Wakeup,
        terminal: &mut Terminal<PaneBackend>,
    ) -> Result<()> {
        self.on_inputs([wakeup], terminal)
    }

    pub(super) fn on_inputs(
        &mut self,
        wakeups: impl IntoIterator<Item = Wakeup>,
        terminal: &mut Terminal<PaneBackend>,
    ) -> Result<()> {
        self.prepare_action();
        let applied = match self.apply_input(wakeups, terminal) {
            Ok(applied) => applied,
            Err(error) => {
                self.pane.input_jump = None;
                return Err(error);
            }
        };
        if applied.redraw && self.watched() {
            // Key/mouse input paints synchronously for instant feedback; a
            // paint settles any frame the loop owed.
            self.pane.dirty = false;
        }
        for effect in applied.effects {
            match effect {
                InputEffect::Focus(pane) => {
                    let peers = self.pane.input_jump.take();
                    let clicked = peers
                        .as_ref()
                        .and_then(FocusPeers::clicked)
                        .unwrap_or_else(|| self.pane.clock.now());
                    let (placement, order) =
                        self.views.with(&mut self.pane, Route::READ, |state, v| {
                            (
                                state.focus_placement(v.state, v.context, &pane),
                                v.state.last_order.clone(),
                            )
                        });
                    spawn_pane_focus(
                        &self.pane.clock,
                        clicked,
                        pane,
                        &self.pane.config.room.session_name,
                        self.pane.runtime.clone(),
                        crate::mux::focus_anchor::FocusOrigin::User,
                        None,
                        (placement, Some(order)),
                        None,
                        peers,
                        self.pane.wake.clone(),
                    );
                }
                InputEffect::Width(dir) => {
                    let Ok(size) = terminal.size() else {
                        return Ok(());
                    };
                    self.pane.width_control.adjust(size.width, dir);
                }
                InputEffect::MarkRead(row_id) => self.mark_row_read(&row_id),
                InputEffect::MarkUnread(row_id) => self.mark_row_unread(&row_id),
                InputEffect::MarkAllRead => self.mark_all_read(),
                InputEffect::SyncFilter(filter) => self.persist_body_filter(filter),
                InputEffect::Reload => (self.pane.request_reload)(),
                InputEffect::DismissAlert => {}
            }
        }
        Ok(())
    }

    fn persist_body_filter(&mut self, filter: BodyLens) {
        self.views.with(&mut self.pane, Route::READ, |state, v| {
            v.context.filter_observed_at = Some(state.clock.now());
        });
        let persisted = crate::sidebar::body_filter::write(&self.pane.runtime, &filter);
        if let Err(err) = persisted {
            debug!(error = %err, "sidebar body filter write failed");
            return;
        }
        if self.pane.clock.effects_enabled()
            && let Some(room_events) = &self.room_events
        {
            room_events.dispatch(SidebarEventEnvelope::new(
                self.pane.config.room.workspace_id.clone(),
                Some(self.pane.config.room.session_name.clone()),
                self.pane.clock.now_ms(),
                SidebarEvent::BodyFilterChanged,
            ));
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
            self.pane.optimistic_watch_until =
                Some(self.pane.clock.now() + FOCUS_RESUME_WATCH_WINDOW);
        }
        let (outcome, source_view) = self.view_transition(user_input, None, None, |state, v, _| {
            (
                state.reduce_input(v, wakeups, state.clock.now()),
                v.target.shared_id(),
            )
        });
        for effect in &outcome.effects {
            if let InputEffect::Focus(pane) = effect {
                self.pane.input_jump = self
                    .pane
                    .clock
                    .effects_enabled()
                    .then(|| self.focus_peers_for(pane))
                    .flatten()
                    .map(|peers| {
                        peers
                            .with_source_view(source_view)
                            .arm(pane.clone(), self.pane.clock.now())
                    });
                if self.pane.clock.effects_enabled()
                    && self.pane.input_jump.is_none()
                    && let Some(peers) = &self.pane.focus_peers
                {
                    peers.supersede();
                }
            }
        }
        if outcome.redraw {
            if !self.watched() {
                self.pane.dirty = true;
                return Ok(outcome);
            }
            self.draw_frame(terminal, self.pane.clock.now(), user_input)?;
            self.views.with(&mut self.pane, Route::READ, |state, v| {
                let now = state.clock.now();
                if outcome
                    .effects
                    .iter()
                    .any(|effect| matches!(effect, InputEffect::Focus(_)))
                {
                    layout_freeze::arm_freeze(
                        v.state,
                        state.clock.timestamp_at(now).as_millisecond(),
                    );
                }
                let interval = render::animation_interval(
                    &v.context.current,
                    v.state,
                    &state.ui,
                    v.state.animation_phase,
                    alert_active(v.context),
                )
                .unwrap_or(animation_frame(&v.context.current));
                let deadline = now + interval;
                if state.next_frame <= now || state.next_frame > deadline {
                    state.next_frame = deadline;
                }
            });
        }
        Ok(outcome)
    }

    /// Mark a row read without jumping (`m`): write the room receipt,
    /// clear the row locally for an instant repaint, trace the clear, wake the
    /// room so the host prunes the episode and peer tabs converge, and refetch
    /// so the receipt lands in the pulled snapshot. A no-op when the row is
    /// already read.
    fn mark_row_read(&mut self, row_id: &str) {
        let now = self.pane.clock.timestamp();
        let marks = ReadMarks::load(&self.pane.runtime);
        let clear = self.views.with(&mut self.pane, Route::READ, |_, v| {
            if v.state.unread_guard.as_deref() == Some(row_id) {
                v.state.unread_guard = None;
            }
            read_receipt_for_row(
                &v.context.current,
                Some(row_id),
                UnreadClearCause::MarkRead,
                &marks,
                now,
            )
        });
        self.apply_read_clear(clear, now);
    }

    fn apply_read_clear(&mut self, clear: ReadClear, now: jiff::Timestamp) {
        if clear.ids.is_empty() {
            return;
        }
        if let Err(err) =
            write_read_marks(&self.pane.runtime, clear.ids.clone(), now.as_millisecond())
        {
            warn!(error = %err, "mark-read receipt write failed");
            return;
        }
        self.views.with(&mut self.pane, Route::READ, |_, v| {
            set_rows_unread(&mut v.context.current, &clear.ids, false);
        });
        emit_unread_cleared_trace(&self.pane.diag, &clear.trace);
        self.commit_local_mark();
    }

    /// Mark every readable row read without jumping (`M`): write room receipts
    /// for every unread or needs-a-look row, clear local unread bits, and refetch
    /// so peers converge through the producer.
    fn mark_all_read(&mut self) {
        let now = self.pane.clock.timestamp();
        let marks = ReadMarks::load(&self.pane.runtime);
        let clear = self.views.with(&mut self.pane, Route::READ, |_, v| {
            v.state.unread_guard = None;
            read_receipts_for_all(&v.context.current, UnreadClearCause::MarkRead, &marks, now)
        });
        self.apply_read_clear(clear, now);
    }

    /// Re-flag a row unread without jumping (`m` on a read row): open a durable
    /// episode through the shared mark-unread path, set the row locally for an
    /// instant repaint, trace the open, wake the room, and refetch so the episode
    /// lands in the pulled snapshot. A no-op when the row has left the room.
    fn mark_row_unread(&mut self, row_id: &str) {
        let Some(row) = self
            .views
            .mirror()
            .rows()
            .find(|row| row.id == row_id)
            .cloned()
        else {
            return;
        };
        let now_ms = self.pane.clock.timestamp().as_millisecond();
        let opened = match unread::mark_rows_unread(
            &self.pane.runtime,
            std::slice::from_ref(&row),
            now_ms,
        ) {
            Ok(opened) => opened,
            Err(err) => {
                warn!(error = %err, "mark-unread episode write failed");
                return;
            }
        };
        self.views.with(&mut self.pane, Route::READ, |_, v| {
            set_rows_unread(&mut v.context.current, std::slice::from_ref(&row.id), true);
            v.state.unread_guard = Some(row.id.clone());
        });
        for item in &opened {
            self.pane.diag.trace_notify(item.trace_event());
        }
        self.commit_local_mark();
    }

    fn commit_local_mark(&mut self) {
        if self.pane.clock.effects_enabled() {
            wake_room(&self.pane.runtime);
        }
        self.pane.dirty = true;
        self.pane.next_frame = self.pane.clock.now();
        self.pane.fetch.request(FetchRequest::default());
    }

    fn run_maintenance(&mut self, terminal: &mut Terminal<PaneBackend>) {
        let own_pane = self.pane.config.own_pane.clone();
        let room_caps = self.pane.room_caps.clone();
        if self.pane.clock.effects_enabled() {
            self.refresh_pet_render_caps_if_stale_with(
                self.pane.config.room.mux,
                &self.pane.config.room.session_name.clone(),
                self.pane.clock.now(),
                terminal,
                |mux, session_name, prev| {
                    // A probe publishes its whole sample at once; a panic leaves
                    // the previous sample intact even if the lock was poisoned.
                    room_caps
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .detect(mux, session_name, prev, &own_pane)
                },
            );
        }
        // Snapshot wakeups are a latency hint, not the only correctness path.
        // `rimz reload` replaces the renderer in place and a ready-result
        // datagram can be lost around socket teardown/rebind; the frame/tick
        // path still drains pending results so startup cannot strand the
        // placeholder cockpit.
        self.on_snapshot();

        let (gate_due, dwell_due) = self.inspect_view(|_, view| {
            (
                gate_remaining(&view.gate, self.pane.clock.timestamp())
                    .is_some_and(|remaining| remaining.is_zero()),
                view.tab_read_dwell_until
                    .filter(|_| view.current.own_view.is_some())
                    .is_some_and(|until| self.pane.clock.now() >= until),
            )
        });
        if gate_due {
            self.pane.fetch.request_if_idle(FetchRequest::default());
        }

        // Tab-view read dwell: once the user has stayed past the dwell, provoke
        // a fold so the normal clear path sweeps the unread siblings. The fold
        // disarms the deadline, so this fires at most a fetch or two.
        if dwell_due && self.watched() {
            self.pane.fetch.request_if_idle(FetchRequest::default());
        }

        // Heartbeat: fast in-process atomic write on the main thread so the
        // exit path never races a background writer.
        if self.pane.clock.effects_enabled() && heartbeat_write_due(self.pane.last_heartbeat) {
            self.pane.last_heartbeat = Some(self.pane.clock.now());
            if let Err(err) = write_heartbeat(
                &self.pane.config,
                &self.pane.runtime,
                &self.pane.host_wake_path,
                terminal.backend().pane_size(),
            ) {
                warn!(
                    session = %self.pane.config.room.session_name,
                    error = %err,
                    "heartbeat write failed",
                );
            }
        }

        // Self-close watchdog: if no resize or presence event fired, ask the
        // normal snapshot path to refresh so the snapshot's own-view count can
        // close a lone sidebar. While confirming zero siblings, require fresh
        // topology before the confirm timer can close it.
        if self
            .pane
            .clock
            .now()
            .saturating_duration_since(self.pane.last_self_close_check)
            >= SELF_CLOSE_WATCHDOG
        {
            self.pane.last_self_close_check = self.pane.clock.now();
            let request = if self.pane.self_close.confirming_empty() {
                FetchRequest::fresh_panes()
            } else {
                FetchRequest::default()
            };
            self.pane.fetch.request_if_idle(request);
        }
        if !self.inspect_view(|ui, _| ui.freeze.is_some()) {
            return;
        }
        let parked = self.parked(self.pane.clock.now_ms());
        if self.watched()
            && self.inspect_view(|ui, _| {
                ui.freeze.as_ref().is_some_and(|hold| {
                    self.pane.clock.timestamp().as_millisecond() >= hold.deadline_ms(parked)
                })
            })
        {
            self.pane.fetch.request_if_idle(FetchRequest::default());
        }
    }

    pub(super) fn clear_pixel(&mut self, terminal: &mut Terminal<PaneBackend>) {
        if let Err(err) = self.pane.paint.clear(terminal.backend_mut()) {
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
        let now = self.pane.clock.now();
        let paint_blocked = self.prepare_paint(terminal, now);
        if let Some(push) = &mut self.pane.pending_focus_push {
            if !self.pane.should_exit && push.peer.blocked_by.is_none() {
                push.peer.blocked_by = if paint_blocked {
                    Some(PushBlock::PaintHold)
                } else if self.pane.self_close.confirming_empty() {
                    Some(PushBlock::ConfirmingEmpty)
                } else {
                    None
                };
            }
            if now >= push.until || self.pane.should_exit {
                let verdict = if self.pane.should_exit {
                    PushVerdict::Exiting
                } else {
                    PushVerdict::Expired
                };
                if let Some(push) = self.pane.pending_focus_push.take() {
                    push.finish(verdict);
                }
            }
        }
        let watched = self.watched();
        let foreground = self.foreground_paint_due(now, active, watched);
        // Once the tab has emptied, never paint again. A grow resize also
        // defers its paint until the sibling-count verdict releases the hold.
        if !self.pane.should_exit
            && !self.pane.self_close.confirming_empty()
            && !paint_blocked
            && foreground
        {
            if self.retire_superseded_push()
                || self.pane.focus_peers.as_ref().is_some_and(|peers| {
                    !peers.accepts(
                        self.inspect_view(|_, view| view.last_folded_focus_intent),
                        self.pane.clock.now(),
                    )
                })
            {
                return Ok(());
            }
            self.paint_now(terminal, now)?;
        } else if !active && !self.pane.dirty {
            // Idle re-arm only: with a fold pending, the armed boundary must
            // hold so a paint already due within one frame is not pushed out.
            self.pane.next_frame = now + animation_frame(self.views.mirror());
        }
        Ok(())
    }

    fn prepare_paint(&mut self, terminal: &mut Terminal<PaneBackend>, now: Instant) -> bool {
        if self.pane.dirty {
            let dirty_deadline = now + animation_frame(self.views.mirror());
            if self.pane.next_frame > dirty_deadline {
                self.pane.next_frame = dirty_deadline;
            }
        }
        if !self.pane.paint_hold.is_engaged()
            && let Ok(size) = terminal.size()
        {
            // Close the engage-gap: a data/animation frame can reach this paint
            // after the mux grew our pane but before the Resize wakeup arms the
            // hold. Arm here too; on_resize still owns prev_width and the fresh
            // pane fetch that resolves the sibling-count verdict.
            self.arm_paint_hold_on_grow(size.width, now);
        }
        let hold_was_engaged = self.pane.paint_hold.is_engaged();
        let paint_blocked = self.pane.paint_hold.blocks_paint(now);
        if hold_was_engaged && !paint_blocked {
            debug!("resize paint hold expired");
        }
        paint_blocked
    }

    fn foreground_paint_due(&self, now: Instant, active: bool, watched: bool) -> bool {
        let marker_due = self.inspect_view(|_, view| matches!(view.focus_marker, Some(FocusMarker::Dwelling(until)) if now >= until));
        (watched && (active || self.pane.dirty))
            && (now >= self.pane.next_frame || (watched && marker_due))
    }

    fn paint_now(&mut self, terminal: &mut Terminal<PaneBackend>, now: Instant) -> Result<()> {
        let (paint, interval, nonce) = self.views.with(&mut self.pane, Route::READ, |state, v| {
            frame_clock(
                now,
                v.context.anim_start,
                v.context.current.theme.display.resolved_refresh_ms(),
            )
            .stamp(v.state);
            let interval = render::animation_interval(
                &v.context.current,
                v.state,
                &state.ui,
                v.state.animation_phase,
                alert_active(v.context),
            );
            (
                state.dirty || interval.is_some() || state.focus_marker_due(v, now),
                interval.unwrap_or(animation_frame(&v.context.current)),
                v.context.last_folded_focus_intent,
            )
        });
        if paint {
            if self.retire_superseded_push()
                || self
                    .pane
                    .focus_peers
                    .as_ref()
                    .is_some_and(|peers| !peers.accepts(nonce, self.pane.clock.now()))
            {
                return Ok(());
            }
            self.draw_frame(terminal, now, false)?;
            self.pane.dirty = false;
        }
        self.pane.next_frame = next_frame_after(self.pane.next_frame, now, interval);
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
            let (token, snapshot, view, alert_active) = self.paint_transition(|_, v| {
                frame_clock(
                    now,
                    v.context.anim_start,
                    v.context.current.theme.display.resolved_refresh_ms(),
                )
                .stamp(v.state);
                (
                    v.token,
                    v.context.current.clone(),
                    v.state.clone(),
                    alert_active(v.context),
                )
            });
            self.pane
                .paint
                .refresh_view(&mut self.pane.ui, &view, &snapshot, alert_active);
            let nonce = self.inspect_view(|_, view| view.last_folded_focus_intent);
            let permit = self
                .pane
                .focus_peers
                .as_ref()
                .map(|peers| peers.paint_permit(nonce, self.pane.clock.now()));
            if matches!(permit, Some(None)) {
                return Ok(());
            }
            let composed = self.paint_transition(|state, v| {
                if v.token != token {
                    return None;
                }
                if permit
                    .as_ref()
                    .and_then(Option::as_ref)
                    .is_some_and(|permit| !permit())
                {
                    return None;
                }
                Some(state.compose_frame(v, terminal, now, user_input))
            });
            let Some((result, frame)) = composed else {
                continue;
            };
            let output = frame.write(terminal.backend_mut());
            result.and(output)?;
            self.pane.painted_at = self.pane.clock.now();
            for (push, verdict) in self.pane.paint_reports.drain(..) {
                push.finish_painted(verdict, self.pane.painted_at);
            }
            return Ok(());
        }
    }

    fn arm_paint_hold_on_grow(&mut self, width: u16, now: Instant) -> bool {
        if !self.pane.paint_hold.is_engaged()
            && self.pane.self_close.seen_sibling
            && resize_grew(self.pane.prev_width, width)
            && grow_beyond_legit(width, self.pane.width_control.max_legit_cols())
        {
            self.pane.paint_hold.engage(
                now,
                self.pane.clock.timestamp_at(now).as_millisecond().max(0) as u64,
            );
            return true;
        }
        false
    }

    fn focus_anchor(&mut self, observed: Instant) -> Option<crate::mux::focus_anchor::FocusAnchor> {
        self.fold_transition(|pane, v, prepared| pane.focus_anchor(v, prepared, observed))
    }

    #[cfg(test)]
    fn focus_placement(&self, pane: &PaneId) -> Option<crate::mux::focus_anchor::FocusPlacement> {
        self.inspect_view(|state, context| self.pane.focus_placement(state, context, pane))
    }

    fn owns_focus_target(&self, pane: &PaneId) -> bool {
        self.pane.owns_focus_target(self.views.mirror(), pane)
    }

    fn focus_peers_for(&self, pane: &PaneId) -> Option<FocusPeers> {
        if self.owns_focus_target(pane) {
            return None;
        }
        self.pane.focus_peers.clone()
    }

    fn focus_push_size_matches(&self, push: &FocusPush, terminal: &Terminal<PaneBackend>) -> bool {
        let Some(expected) = push
            .expected_panes
            .as_ref()
            .and_then(|panes| panes.get(&self.pane.config.own_pane))
        else {
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
        self.inspect_view(|state, _| {
            self.pane.painted_focus_verdict(
                state,
                self.views.mirror(),
                &self.pane.painted_selection,
                anchor,
                push,
            )
        })
    }

    fn events(&self) -> MutexGuard<'_, EventStore> {
        self.views
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl PaneState {
    fn freeze_remaining(&mut self, v: &mut ViewBorrow<'_>, now_ms: i64) -> Option<Duration> {
        let hold = v.state.freeze.as_ref()?;
        Some(Duration::from_millis(
            (hold.deadline_ms(self.parked(v, now_ms as u64)) - now_ms).max(0) as u64,
        ))
    }

    fn frame_timing(&mut self, v: &mut ViewBorrow<'_>) -> (bool, Duration) {
        let clock = frame_clock(
            self.clock.now(),
            v.context.anim_start,
            v.context.current.theme.display.resolved_refresh_ms(),
        );
        let alert_active = alert_active(v.context);
        let watched = self.watched(v);
        let animating = watched
            && render::animation_interval(
                &v.context.current,
                v.state,
                &self.ui,
                clock.phase,
                alert_active,
            )
            .is_some();
        let active = !self.self_close.confirming_empty()
            && (watched && (animating || self.dirty || self.focus_marker_due(v, self.clock.now())));
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
                    self.freeze_remaining(v, self.clock.timestamp().as_millisecond())
            {
                timeout = timeout.min(remaining);
            }
            timeout.max(FRAME_MIN_TIMEOUT)
        };
        if watched && let Some(FocusMarker::Dwelling(until)) = v.context.focus_marker {
            timeout = timeout.min(
                until
                    .saturating_duration_since(self.clock.now())
                    .max(FRAME_MIN_TIMEOUT),
            );
        }
        if let Some(until) = v
            .context
            .tab_read_dwell_until
            .filter(|_| v.context.current.own_view.is_some())
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
        if let Some(remaining) = gate_remaining(&v.context.gate, self.clock.timestamp()) {
            timeout = timeout.min(remaining.max(FRAME_MIN_TIMEOUT));
        }
        (active, timeout)
    }

    fn watches_snapshot(&self, snapshot: &SidebarSnapshot) -> bool {
        self.watches_view(snapshot.own_view.as_ref(), &snapshot.viewed_panes)
    }

    fn watches_view(&self, own_view: Option<&SidebarOwnView>, viewed_panes: &[PaneId]) -> bool {
        let Some(view) = own_view else {
            return true;
        };
        own_tab_viewed(viewed_panes, view, &self.config.own_pane)
    }

    /// Whether the next fold equal to the one just applied may arrive as a
    /// clock-only delivery. Renderer-local reconciliation state blocks it, so
    /// that state always meets full content. Exhaustive: a new field must be
    /// named here, as a blocker or as irrelevant to delivery.
    fn admits_clock_only(&self, ui: &ViewState, view: &ViewContext, rejected: bool) -> bool {
        let Self {
            // Blockers: an exit, a held overlay or focus fence, a gate episode,
            // a failure streak, a frozen order, an armed read dwell, an empty
            // confirmation, and a pending focus repair each resolve on a fold.
            should_exit,
            fold_effects: _,
            baseline_seed: _,
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
            fetch: _,
            own: _,
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
            last_self_close_check: _,
            last_heartbeat: _,
            // Baseline reporting, read from `ui` outside the fold.
            last_reported_baseline: _,
            painted_baseline: _,
            painted_selection: _,
            focus_peers: _,
            input_jump: _,
        } = self;
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
    }

    fn on_focus_failed(
        &mut self,
        v: &mut ViewBorrow<'_>,
        prepared: &FoldPreparation,
        pane: PaneId,
        error: String,
    ) {
        if v.context.pending_input_focus.as_ref() == Some(&pane) {
            v.context.pending_input_focus = None;
        }
        let Some(label) = v.context.roll_back_focus(v.state, &pane, self.clock.now()) else {
            return;
        };
        self.fold_fused_now(v, prepared);
        v.context
            .alert_focus_failure(&label, &error, self.clock.timestamp());
        self.dirty = true;
    }

    fn reduce_snapshot(
        &mut self,
        v: &mut ViewBorrow<'_>,
        prepared: &FoldPreparation,
        update: FetchUpdate,
        seed: bool,
    ) -> bool {
        let (update, context) = update.into_parts();
        v.context.fold_inputs = context.as_ref().map(|context| context.inputs.clone());
        if let Some(observation) = context.and_then(|context| context.observation.clone()) {
            v.context.observation = Some(observation);
        }
        if let FetchUpdate::Unchanged { clock, .. } = update {
            clock.apply(&mut v.context.current);
            v.context.last_focus_observation.panes_observed_at_ms =
                v.context.current.panes_observed_at_ms;
            self.last_self_close_check = self.clock.now();
            self.dirty = true;
            v.context.fold_inputs = None;
            self.release_paint_hold_after_snapshot(v, false);
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
                v.context.last_focus_observation = FocusObservation::from_snapshot(&pulled);
                if v.context.observation.is_some() {
                    v.context.last_pulled_rows = pulled.rows().count();
                }
                let intent = self.pending_focus_intent(v, prepared, now_ms, self.clock.now());
                let events = v.events.clone();
                let (mut snapshot, focus_event_sent_at_ms) =
                    v.context
                        .fuse_pulled(pulled, events, intent.as_ref(), now_ms);
                if intent.is_none()
                    && focus_event_sent_at_ms.is_none_or(|stamp| v.focus_event != Some(stamp))
                    && let Some(focused) = &v.focused
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
            FetchUpdate::Fold { .. } => unreachable!("projected before reduction"),
        };
        let rejected = self.fold_outcome(v, prepared, update, true, seed, fused_event_sent_at_ms);
        v.context.fold_inputs = None;
        if snapshot_ok {
            self.last_self_close_check = self.clock.now();
            self.fold_effects.retry_focus_repair = true;
        }
        self.release_paint_hold_after_snapshot(v, rejected);
        rejected
    }

    fn release_paint_hold_after_snapshot(&mut self, v: &mut ViewBorrow<'_>, rejected: bool) {
        // Release by pane stamp alone. A `Fresh` frame is fresh against the
        // request that fetched it, and a request sent before the grow can land
        // after it with a pane list that still counts the closed sibling.
        if !self.should_exit
            && !rejected
            && !self.self_close.confirming_empty()
            && self
                .paint_hold
                .releases_on_stamp(v.context.current.panes_observed_at_ms)
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
    fn fused_snapshot(
        &mut self,
        v: &mut ViewBorrow<'_>,
        prepared: &FoldPreparation,
        now_ms: u64,
    ) -> (SidebarSnapshot, Option<u64>) {
        let pulled = v
            .context
            .overlay_baseline
            .take()
            .unwrap_or_else(|| v.context.current.clone());
        let intent = self.pending_focus_intent(v, prepared, now_ms, self.clock.now());
        let events = v.events.clone();
        let (mut snapshot, stamp) = v
            .context
            .fuse_pulled(pulled, events, intent.as_ref(), now_ms);
        if intent.is_none()
            && stamp.is_none_or(|stamp| v.focus_event != Some(stamp))
            && let Some(focused) = &v.focused
        {
            snapshot.focused_pane.clone_from(focused);
        }
        (snapshot, stamp)
    }

    /// Fold a synthetic fused frame and snap the frame deadline so this
    /// turn's frame phase paints it now.
    fn fold_fused_now(&mut self, v: &mut ViewBorrow<'_>, prepared: &FoldPreparation) {
        self.fold_effects.invalidate = true;
        let (fused, fused_event_sent_at_ms) = self.fused_snapshot(v, prepared, self.clock.now_ms());
        self.fold_outcome(
            v,
            prepared,
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

    fn fold_focus_intent(
        &mut self,
        v: &mut ViewBorrow<'_>,
        prepared: &FoldPreparation,
        anchor: crate::mux::focus_anchor::FocusAnchor,
    ) {
        if anchor.session_name != self.config.room.session_name
            || v.context.last_folded_focus_intent.replace(anchor.nonce) == Some(anchor.nonce)
        {
            return;
        }
        // The pushed intent is newer than every cut folded before it, the catch-up fold included,
        // even on a clock that has not moved since: that older read must not retire this anchor.
        let sampled_at = self.clock.now();
        v.context.observed_anchor = Some((sampled_at, Some(anchor.clone())));
        let previous = v
            .context
            .fold_inputs
            .replace(Arc::new(super::fetch::FoldInputs {
                sampled_at,
                anchor: Some(anchor),
                filter: v.state.make_up_filter.clone(),
            }));
        self.fold_fused_now(v, prepared);
        v.context.fold_inputs = previous;
    }

    fn reduce_input(
        &mut self,
        v: &mut ViewBorrow<'_>,
        wakeups: Vec<Wakeup>,
        now: Instant,
    ) -> InputOutcome {
        let keys = &v.context.current.sidebar.keys;
        if self.nav_keys.0 != *keys {
            self.nav_keys = (keys.clone(), super::NavKeymap::from_config(keys));
        }
        let mut outcome = InputOutcome::default();
        let mut user_input = false;
        for wakeup in wakeups {
            user_input |= !matches!(wakeup, Wakeup::Resize);
            let wheel = matches!(wakeup, Wakeup::Scroll { .. });
            let alert_active = alert_active(v.context);
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
            let previous_selection = v.state.selected_pane.clone();
            let previous_row = v
                .state
                .selected_index
                .and_then(|index| v.state.visible_roster(&v.context.current).row(index))
                .map(|row| row.id.clone());
            let applied = handle_wakeup(
                wakeup,
                v.state,
                &mut self.ui,
                &v.context.current,
                &self.nav_keys.1,
                alert_active,
            );
            if let Some((interactions, roster)) = live_frame {
                self.ui.interactions = interactions;
                self.ui.painted_roster = roster;
            }
            self.clicked_row = None;
            let selected_row = v
                .state
                .selected_index
                .and_then(|index| v.state.visible_roster(&v.context.current).row(index))
                .map(|row| row.id.clone());
            if selected_row != previous_row {
                v.context.pending_input_focus = None;
                v.state.own_input_reveal = true;
                v.context.focus_marker = None;
                v.state.focus_marker_ready = false;
            }
            if v.state.selected_pane != previous_selection {
                v.context.focus_rollback = None;
            }
            for effect in &applied.effects {
                if let InputEffect::Focus(pane) = effect {
                    v.context.pending_input_focus = Some(pane.clone());
                    let owner = v
                        .state
                        .selected_pane
                        .as_ref()
                        .and_then(|selected| v.context.current.row_owning_pane(selected))
                        .filter(|row| row.pane_ids().any(|id| id == pane))
                        .or_else(|| v.context.current.row_owning_pane(pane));
                    v.context.focus_rollback = owner.map(|row| FocusRollback {
                        target: pane.clone(),
                        baseline: v.state.baseline_pane.clone(),
                        label: row.display_name().to_owned(),
                    });
                    self.arm_clicked_row(v, pane, now);
                }
            }
            if applied.redraw && !wheel {
                v.state.scroll_ease.cancel();
            }
            if applied.effects.contains(&InputEffect::DismissAlert) {
                v.context.health.alert = None;
                if v.state
                    .hook_drainer_alert
                    .as_ref()
                    .is_some_and(|alert| !alert.is_active())
                {
                    v.state.hook_drainer_alert = None;
                }
            }
            outcome.redraw |= applied.redraw;
            outcome.effects.extend(applied.effects);
        }
        if user_input
            && (outcome.redraw || !outcome.effects.is_empty())
            && !outcome.effects.contains(&InputEffect::Reload)
        {
            layout_freeze::arm_freeze(v.state, self.clock.timestamp_at(now).as_millisecond());
        }
        outcome
    }

    fn compose_frame(
        &mut self,
        v: &mut ViewBorrow<'_>,
        terminal: &mut Terminal<PaneBackend>,
        now: Instant,
        user_input: bool,
    ) -> (std::io::Result<()>, BufferedFrame) {
        if v.target.shared_id().is_some() && v.state.baseline_pane.is_none() {
            v.state.baseline_pane = self.baseline_seed.take();
            reconcile_selection(v.state, &v.context.current, None);
        }
        v.state.focus_marker_ready = self.watched(v)
            && match v.context.focus_marker {
                Some(FocusMarker::Ready) => true,
                Some(FocusMarker::Dwelling(until)) => now >= until,
                None => false,
            };
        // The notice is pane chrome over the view's gate, which another pane may have moved.
        self.ui.gate_notice = v.context.gate.rule.map(|rule| render::GateNotice { rule });
        let user_input = user_input || v.state.scroll_ease.in_flight();
        let interactions = std::mem::take(&mut self.ui.interactions);
        let roster = std::mem::take(&mut self.ui.painted_roster);
        let rows = std::mem::take(&mut self.ui.painted_rows);
        let (result, frame) = self.paint.compose_frame(
            terminal,
            &v.context.current,
            v.context.health.alert.as_ref(),
            v.state,
            &mut self.ui,
        );
        if result.is_err() {
            return (result, frame);
        }
        self.painted_at = self.clock.now();
        self.paints += 1;
        self.painted_height = terminal.get_frame().area().height;
        self.painted_baseline.clone_from(&v.state.baseline_pane);
        self.baseline_seed = None;
        self.painted_selection.clone_from(&v.state.drawn_selection);
        let selected_visible = v
            .state
            .drawn_selection
            .as_ref()
            .is_some_and(|selected| self.ui.painted_rows.iter().any(|(id, _)| id == selected));
        if selected_visible {
            v.context.focus_marker = None;
            v.state.focus_marker_ready = false;
        } else if v.state.focus_marker_ready {
            v.context.focus_marker = Some(FocusMarker::Ready);
        }
        v.state.last_order = layout_freeze::capture_order(&v.context.current, v.state);
        v.state.arrival_pending = false;
        if user_input || !self.watched(v) {
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
        self.observe_paint(v, now);
        (Ok(()), frame)
    }

    /// Fold one fetch outcome into the render state: gate it against the
    /// last-known-good frame, update health, snapshot, and selection, and
    /// report whether the loop should exit.
    fn apply_fetch_outcome(
        &mut self,
        v: &mut ViewBorrow<'_>,
        prepared: &FoldPreparation,
        update: FetchUpdate,
        allow_shared_filter_sync: bool,
    ) -> ApplyOutcome {
        let (update, _) = update.into_parts();
        let snapshot_ok = matches!(&update, FetchUpdate::Snapshot { .. });
        let now = self.clock.timestamp();
        let searching = v.state.search_draft.is_some();
        let (prev_good, rejected, diagnostics) = v.context.commit_fetch(update, v.state, now);
        self.fold_effects.diagnostics.extend(diagnostics);
        self.ui.gate_notice = v.context.gate.rule.map(|rule| render::GateNotice { rule });
        if let Some(alert) = v
            .context
            .health
            .alert
            .as_ref()
            .filter(|alert| alert.is_active())
        {
            self.fold_effects.degraded = Some(alert.reason.clone());
        }
        self.dirty |= searching && v.state.search_draft.is_none();
        let authoritative_filter_fold = allow_shared_filter_sync
            && snapshot_ok
            && !rejected
            && v.context.fold_inputs.as_ref().is_none_or(|inputs| {
                v.context
                    .filter_observed_at
                    .is_none_or(|observed| inputs.sampled_at > observed)
            });
        let prev_selected = v.state.selected_pane.clone();
        let (focused_pane, cleared) = self.sweep_read_receipts(v, prepared, now);
        self.reconcile_selection_and_order(
            v,
            prepared,
            &prev_good,
            prev_selected,
            focused_pane,
            cleared,
            authoritative_filter_fold,
            self.clock.instant_at(now),
        );
        v.context.fold_spend_rolls(v.state, &self.clock);
        self.exit_verdict(v, rejected)
    }

    fn exit_verdict(&mut self, v: &mut ViewBorrow<'_>, rejected: bool) -> ApplyOutcome {
        // Own-view (sibling count) rides in on the snapshot — the producer computes
        // it from the same pane list it already enumerated. Presence publication and
        // the poll backstop feed this latch; resize only decides whether to hold a
        // grown-width paint while the fresh fold is pending.
        ApplyOutcome {
            should_exit: self.self_close.should_close(
                v.context
                    .current
                    .own_view
                    .as_ref()
                    .map(|view| view.sibling_count),
                self.clock.now(),
            ),
            rejected,
        }
    }

    fn sweep_read_receipts(
        &mut self,
        v: &mut ViewBorrow<'_>,
        prepared: &FoldPreparation,
        now: Timestamp,
    ) -> (Option<PaneId>, bool) {
        if v.target.shared_id().is_none() {
            return (None, false);
        }
        let marks = prepared.marks.clone();
        self.prepare_tab_arrival(v);
        let (focused, clear) = v.context.read_receipts(
            v.state,
            &ReadContext {
                own_pane: &self.config.own_pane,
                marks: &marks,
                now,
                at: self.clock.now(),
            },
        );
        let live: HashSet<String> = v.context.current.rows().map(|row| row.id.clone()).collect();
        let cleared = !clear.ids.is_empty();
        let mut clear = clear;
        clear.ids.retain(|id| live.contains(id));
        self.fold_effects.receipts.push((clear, now));
        (focused, cleared)
    }

    fn prepare_tab_arrival(&mut self, v: &mut ViewBorrow<'_>) {
        let arriving = v.state.viewing_own_tab == Some(false)
            && v.context.current.own_view.as_ref().is_some_and(|view| {
                own_tab_viewed(&v.context.current.viewed_panes, view, &self.config.own_pane)
            });
        if arriving {
            self.next_frame = self.clock.now();
        }
    }

    #[cfg(test)]
    fn sweep_tab_read_receipts(
        &mut self,
        v: &mut ViewBorrow<'_>,
        focused_row_id: Option<&str>,
        marks: &ReadMarks,
        now: Timestamp,
        clear: &mut ReadClear,
    ) {
        self.prepare_tab_arrival(v);
        v.context.sweep_tab_read_receipts(
            v.state,
            focused_row_id,
            &ReadContext {
                own_pane: &self.config.own_pane,
                marks,
                now,
                at: self.clock.now(),
            },
            clear,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_selection_and_order(
        &mut self,
        v: &mut ViewBorrow<'_>,
        prepared: &FoldPreparation,
        prev_good: &SidebarSnapshot,
        prev_selected: Option<PaneId>,
        focused_pane: Option<PaneId>,
        cleared: bool,
        authoritative_filter_fold: bool,
        now: Instant,
    ) {
        let filter = authoritative_filter_fold.then(|| {
            if let Some(inputs) = &v.context.fold_inputs {
                return inputs.filter.clone();
            }
            let filter = prepared.filter.clone();
            v.context.filter_observed_at = Some(self.clock.now());
            filter
        });
        let parked = self.parked(v, self.clock.timestamp_at(now).as_millisecond() as u64);
        let (persist_filter, expired) = v.context.reconcile_selection_and_order(
            v.state,
            SelectionFold {
                previous: prev_good,
                selected: prev_selected,
                focused: focused_pane,
                cleared,
                filter,
                mux: self.config.room.mux,
                clock: &self.clock,
                now,
                parked,
            },
        );
        self.hold_expired_since_paint |= expired;
        if persist_filter {
            self.fold_effects.filter = Some(v.state.make_up_filter.clone());
        }
    }

    fn fold_outcome(
        &mut self,
        v: &mut ViewBorrow<'_>,
        prepared: &FoldPreparation,
        update: FetchUpdate,
        allow_shared_filter_sync: bool,
        seed: bool,
        fused_event_sent_at_ms: Option<u64>,
    ) -> bool {
        let trace = match &update {
            FetchUpdate::Snapshot { source, phase, .. } => Some((*source, *phase)),
            _ => None,
        };
        let selected_before = v.state.selected_pane.clone();
        let applied = self.apply_fetch_outcome(v, prepared, update, allow_shared_filter_sync);
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
                panes_produced_at_ms: v.context.current.panes_produced_at_ms,
                panes_observed_at_ms: v.context.current.panes_observed_at_ms,
                snapshot_focused_pane: v.context.current.focused_pane.clone(),
                fused_event_sent_at_ms,
                own_view: v.context.current.own_view.is_some(),
                baseline: session_focus_baseline(&v.context.current, &self.config.own_pane),
                selected_before,
                selected_after: v.state.selected_pane.clone(),
            });
        }
        self.should_exit = applied.should_exit;
        self.apply_focus_anchor(v, prepared);
        self.observe_commit(v);
        self.dirty = true;
        applied.rejected
    }

    fn focus_anchor(
        &mut self,
        v: &mut ViewBorrow<'_>,
        prepared: &FoldPreparation,
        observed: Instant,
    ) -> Option<crate::mux::focus_anchor::FocusAnchor> {
        let loaded = v
            .context
            .fold_inputs
            .is_none()
            .then(|| prepared.anchor.clone())
            .flatten();
        let own_pane = v.target.shared_id().map(|_| &self.config.own_pane);
        v.context
            .observe_focus_anchor(&self.config.room.session_name, observed, loaded, own_pane)
    }

    fn focus_placement(
        &self,
        view: &ViewState,
        context: &ViewContext,
        pane: &PaneId,
    ) -> Option<crate::mux::focus_anchor::FocusPlacement> {
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
    }

    fn owns_focus_target(&self, snapshot: &SidebarSnapshot, pane: &PaneId) -> bool {
        snapshot.own_view.as_ref().is_some_and(|view| {
            own_tab_viewed(std::slice::from_ref(pane), view, &self.config.own_pane)
        })
    }

    fn painted_focus_verdict(
        &self,
        ui: &ViewState,
        snapshot: &SidebarSnapshot,
        drawn_selection: &Option<String>,
        anchor: &crate::mux::focus_anchor::FocusAnchor,
        push: Option<&FocusPush>,
    ) -> Option<PushVerdict> {
        let portable_matches =
            ui.last_focus_anchor_ms == anchor.issued_at_ms && ui.focus_placement.is_none();
        if self.painted_focus_anchor_ms != anchor.issued_at_ms
            || !portable_matches
            || push
                .and_then(|push| push.expected_panes.as_ref())
                .and_then(|rows| rows.get(&self.config.own_pane))
                .is_some_and(|pane| pane.rows != self.painted_height)
        {
            return None;
        }
        let row = snapshot.row_owning_pane(&anchor.pane_id)?;
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

    fn apply_focus_anchor(&mut self, v: &mut ViewBorrow<'_>, prepared: &FoldPreparation) {
        let Some(anchor) = self.focus_anchor(v, prepared, self.clock.now()) else {
            return;
        };
        if v.context.apply_focus_anchor(v.state, &anchor, &self.clock) {
            self.fold_effects.retirements.push((v.target, anchor, None));
        }
    }

    fn pending_focus_intent(
        &mut self,
        v: &mut ViewBorrow<'_>,
        prepared: &FoldPreparation,
        now_ms: u64,
        now: Instant,
    ) -> Option<FocusPresentation> {
        let anchor = self.focus_anchor(v, prepared, now);
        let (presentation, retirement) =
            v.context
                .pending_focus_intent(anchor.as_ref(), &v.events, now_ms);
        if let (Some(anchor), Some(outcome)) = (anchor, retirement) {
            self.fold_effects
                .retirements
                .push((v.target, anchor, Some(outcome)));
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

    fn focus_marker_due(&self, v: &ViewBorrow<'_>, now: Instant) -> bool {
        self.watched(v)
            && matches!(v.context.focus_marker, Some(FocusMarker::Dwelling(until)) if now >= until)
    }

    fn observe_commit(&mut self, v: &mut ViewBorrow<'_>) {
        let Some(common) = &v.context.observation else {
            return;
        };
        let now_ms = self.clock.now_ms();
        let sig = observe::OwnFrameSig::extract(
            &v.context.current,
            (
                v.context.last_pulled_rows,
                common.pulled_panes_produced_at_ms,
            ),
            &v.events,
            v.context.gate.reject_streak,
            v.context.health.failure_streak,
            now_ms,
        );
        self.fold_effects.observations.push(sig);
    }

    fn arm_clicked_row(&mut self, v: &mut ViewBorrow<'_>, pane: &PaneId, now: Instant) {
        self.clicked_row = v.context.current.row_owning_pane(pane).and_then(|row| {
            self.ui
                .painted_rows
                .iter()
                .find(|(id, _)| id == &row.id)
                .map(|(id, y)| ClickedRowWatch {
                    row_id: id.clone(),
                    y: *y,
                    clicked_at: now,
                    selection: v.state.drawn_selection.clone(),
                    banner_shown: self.ui.banner_shown,
                    gliding: v.state.scroll_ease.in_flight(),
                })
        });
    }

    fn observe_paint(&mut self, v: &mut ViewBorrow<'_>, now: Instant) {
        self.painted_focus_anchor_ms = v.state.last_focus_anchor_ms;
        let live_landing = v
            .context
            .focus_landing
            .as_ref()
            .is_some_and(|(anchor, _)| v.context.live_focus_source == Some(anchor.nonce));
        // This paint consumed the placement, so the landing is judged here or never.
        let landing = v
            .context
            .focus_landing
            .take()
            .and_then(|(anchor, applied_at)| {
                let push = self
                    .pending_focus_push
                    .as_ref()
                    .filter(|push| push.anchor.nonce == anchor.nonce);
                let verdict = self.painted_focus_verdict(
                    v.state,
                    &v.context.current,
                    &v.state.drawn_selection,
                    &anchor,
                    push,
                )?;
                Some((verdict, anchor.placement?, applied_at))
            });
        if landing.is_some() && !live_landing {
            layout_freeze::arm_freeze(v.state, self.clock.timestamp_at(now).as_millisecond());
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
                selection: v.state.drawn_selection.clone(),
                banner_shown: self.ui.banner_shown,
                gliding: false,
            });
            seeded_landing = true;
        }
        let verdict = self.pending_focus_push.as_ref().and_then(|push| {
            if now >= push.until
                || !self.owns_focus_target(&v.context.current, &push.anchor.pane_id)
            {
                return None;
            }
            self.painted_focus_verdict(
                v.state,
                &v.context.current,
                &v.state.drawn_selection,
                &push.anchor,
                Some(push),
            )
        });
        if let Some(verdict) = verdict
            && let Some(push) = self.pending_focus_push.take()
        {
            self.paint_reports.push((push, verdict));
        }
        if let Some(push) = &mut self.pending_focus_push {
            push.peer.unmatched_paints = push.peer.unmatched_paints.saturating_add(1);
            if v.context
                .current
                .row_owning_pane(&push.anchor.pane_id)
                .is_some_and(|row| v.state.drawn_selection.as_ref() == Some(&row.id))
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
        } else if watch.selection != v.state.drawn_selection {
            ClickedRowDisplacementCause::SelectionChanged
        } else {
            ClickedRowDisplacementCause::HeightAboveChanged
        };
        watch.banner_shown = self.ui.banner_shown;
        watch.selection = v.state.drawn_selection.clone();
        let glided = std::mem::replace(&mut watch.gliding, v.state.scroll_ease.in_flight());
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
            &v.context.current,
            (
                v.context.last_pulled_rows,
                v.context
                    .observation
                    .as_ref()
                    .and_then(|sig| sig.pulled_panes_produced_at_ms),
            ),
            &v.events,
            v.context.gate.reject_streak,
            v.context.health.failure_streak,
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

    fn watched(&self, v: &ViewBorrow<'_>) -> bool {
        v.target.shared_id().is_some()
            || self
                .optimistic_watch_until
                .is_some_and(|until| self.clock.now() < until)
            || v.resolved
            || v.observed
                .unwrap_or_else(|| self.watches_snapshot(&v.context.current))
    }

    fn parked(&self, v: &ViewBorrow<'_>, now_ms: u64) -> bool {
        parked(
            v.context,
            &v.events,
            &self.config.own_pane,
            v.parked,
            v.focus_event,
            now_ms,
        )
    }
}

fn alert_active(view: &ViewContext) -> bool {
    view.health
        .alert
        .as_ref()
        .is_some_and(render::Alert::is_active)
}

fn parked(
    view: &ViewContext,
    events: &EventStore,
    pane: &PaneId,
    sampled: Option<bool>,
    event: Option<u64>,
    now_ms: u64,
) -> bool {
    let pulled = view.overlay_baseline.as_ref().unwrap_or(&view.current);
    let baseline = pulled
        .panes_observed_at_ms
        .or(pulled.panes_produced_at_ms)
        .unwrap_or(0);
    if sampled.is_none()
        || event.is_some_and(|stamp| stamp > baseline && events.has_focus_since(stamp, now_ms))
    {
        crate::sidebar::fuse::pane_is_focused(pulled, events, pane, now_ms)
    } else {
        sampled.unwrap_or(false)
    }
}

fn focus_stranded_target(
    snapshot: &SidebarSnapshot,
    ui: &ViewState,
    stranded_pane_id: &PaneId,
    evidence: &[crate::pane::ClientPaneView],
    own_pane_id: &PaneId,
    sent_at_ms: u64,
    now_ms: u64,
) -> std::result::Result<PaneId, &'static str> {
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
