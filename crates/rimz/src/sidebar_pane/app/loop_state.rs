use std::collections::HashSet;

use jiff::Timestamp;

use super::gate::apply_gate;
use super::health::degraded_too_long;
use super::lifecycle::self_close_decision;
use super::paint::FramePainter;
use super::remind::RemindState;
use super::selection::{reconcile_selection, row_index_of_pane};
use super::state::{
    ApplyOutcome, FetchDiagnostics, ReadClear, RenderState, apply_manual_unread_guard,
    compute_next_state, emit_diagnostics, emit_unread_cleared_trace, read_receipt_for_row,
    read_receipts_for_all, read_receipts_for_tab, row_id_of_pane, session_focus_baseline,
    set_rows_unread,
};
use super::*;
use crate::diag::record::RendererExitCause;
use crate::observability::SIDEBAR_HEALTH_TARGET;
use crate::sidebar::observe::writer::{RoleCache, crosscheck_enabled};
use crate::sidebar::read_marks::{ReadMarkStore, ReadMarks, write_manual_read_marks};
use crate::sidebar::unread::{self, UnreadClearCause};
use crate::sidebar_pane::pets::PetRenderCaps;

pub(super) struct MaintenanceContext<'a> {
    pub(super) config: &'a ServeConfig,
    pub(super) runtime: &'a RuntimePaths,
    pub(super) socket_path: &'a Path,
    pub(super) result_rx: &'a Receiver<FetchOutcome>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackgroundRowStatusKey {
    Agent(crate::agents::AgentStatus),
    Process(crate::ProcessState),
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

fn row_status_key(row: &crate::SidebarRow) -> BackgroundRowStatusKey {
    match &row.card {
        crate::RowCard::Agent(card) => BackgroundRowStatusKey::Agent(card.status),
        crate::RowCard::Process(card) => BackgroundRowStatusKey::Process(card.state),
    }
}

const MIN_ADJUSTABLE_WIDTH: u16 = 24;

fn width_adjust_allowed(dir: crate::mux::WidthAdjust, current_width: Option<u16>) -> bool {
    dir != crate::mux::WidthAdjust::Narrower
        || current_width.is_some_and(|width| width > MIN_ADJUSTABLE_WIDTH)
}

fn own_tab_viewed(
    snapshot: &SidebarSnapshot,
    own_view: &crate::SidebarOwnView,
    own_pane: &PaneId,
) -> bool {
    snapshot
        .viewed_panes
        .iter()
        .any(|pane| pane == own_pane || own_view.working_pane_ids.contains(pane))
}

pub(super) struct LoopState {
    pub(super) last_snapshot: Option<SidebarSnapshot>,
    pub(super) current: SidebarSnapshot,
    last_pulled: SidebarSnapshot,
    own_pane: Option<PaneId>,
    last_known_elder: bool,
    elder_role: RoleCache,
    pending_fetch: Option<PendingFetch>,
    optimistic_watch_until: Option<Instant>,
    /// Deadline for the tab-view read sweep: armed when the own tab comes on
    /// screen, disarmed when it leaves. The sweep fires on the first fold at or
    /// past it while the tab is still viewed, so a pass-through never clears
    /// siblings. `None` when no dwell is pending.
    pub(super) tab_read_dwell_until: Option<Instant>,
    event_store: EventStore,
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
    width_adjust_pending: Option<Instant>,
    pub(super) should_exit: bool,
    pub(super) exit_cause: Option<RendererExitCause>,
    pub(super) tab_emptied: bool,
    pub(super) reload_requested: bool,
}

#[derive(Clone, Copy, Debug)]
struct PendingFetch {
    due_at: Instant,
    request: FetchRequest,
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
        Wakeup::Tick | Wakeup::Event(_) | Wakeup::Reload | Wakeup::ReloadKey | Wakeup::Snapshot => {
            InputOutcome::default()
        }
    }
}

impl LoopState {
    pub(super) fn new(
        workspace_id: WorkspaceId,
        own_pane: Option<PaneId>,
        initial_width: Option<u16>,
        observe_tx: SyncSender<ObserveMsg>,
        read_marks: ReadMarkStore,
        pet_render_caps: PetRenderCaps,
        pixel_wrap: bool,
    ) -> Self {
        let current = placeholder_snapshot(workspace_id);
        let now = Instant::now();
        Self {
            last_snapshot: None,
            last_pulled: current.clone(),
            current,
            own_pane,
            last_known_elder: true,
            elder_role: RoleCache::default(),
            pending_fetch: None,
            optimistic_watch_until: None,
            tab_read_dwell_until: None,
            event_store: EventStore::default(),
            confirmed_focus_intent_ms: 0,
            observer: observe::Observer::default(),
            observe_tx,
            health: Health::default(),
            gate: GateState::default(),
            self_close: SelfCloseState::default(),
            ui: UiState::default(),
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
            width_adjust_pending: None,
            should_exit: false,
            exit_cause: None,
            tab_emptied: false,
            reload_requested: false,
        }
    }

    pub(super) fn frame_timing(&self, tick: Duration, anim_start: Instant) -> (bool, Duration) {
        let phase = wall_clock_phase(anim_start, self.current.theme.display.resolved_refresh_ms());
        let alert_active = self
            .health
            .alert
            .as_ref()
            .is_some_and(render::Alert::is_active);
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
        if let Some(pending) = self.pending_fetch {
            timeout = timeout.min(
                pending
                    .due_at
                    .saturating_duration_since(Instant::now())
                    .max(FRAME_MIN_TIMEOUT),
            );
        }
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
        (active, timeout)
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

    pub(super) fn help_visible(&self) -> bool {
        self.ui.help_visible
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
                Some(crate::SidebarPresence::Active | crate::SidebarPresence::Idle { .. })
            )
    }

    fn identity_free_fetch_immediate(&self) -> bool {
        self.watched() || self.last_known_elder
    }

    fn request_or_defer_identity_free(
        &mut self,
        fetch: &mut FetchDispatcher,
        request: FetchRequest,
    ) {
        if self.identity_free_fetch_immediate() {
            self.request_now_merging_pending(fetch, request, true);
        } else {
            self.defer_fetch(request);
        }
    }

    fn defer_fetch(&mut self, request: FetchRequest) {
        if let Some(pending) = &mut self.pending_fetch {
            pending.request.merge(request);
            return;
        }
        self.pending_fetch = Some(PendingFetch {
            due_at: Instant::now() + crate::sidebar::timing::UNWATCHED_FOLD_CLAMP,
            request,
        });
    }

    fn request_now_merging_pending(
        &mut self,
        fetch: &mut FetchDispatcher,
        mut request: FetchRequest,
        force_after: bool,
    ) {
        if let Some(pending) = self.pending_fetch.take() {
            request.merge(pending.request);
        }
        fetch.request(request, force_after);
    }

    pub(super) fn clear_pending_fetch(&mut self) {
        self.pending_fetch = None;
    }

    fn fire_due_pending_fetch(&mut self, fetch: &mut FetchDispatcher) {
        let Some(pending) = self.pending_fetch else {
            return;
        };
        if Instant::now() < pending.due_at {
            return;
        }
        self.pending_fetch = None;
        fetch.request(pending.request, true);
    }

    pub(super) fn on_snapshot(
        &mut self,
        config: &ServeConfig,
        fetch: &mut FetchDispatcher,
        result_rx: &Receiver<FetchOutcome>,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> Result<()> {
        let mut latest = None;
        let mut saw_final = false;
        while let Ok(outcome) = result_rx.try_recv() {
            saw_final |= outcome.final_for_request;
            latest = Some(outcome);
        }
        if saw_final {
            fetch.mark_request_complete();
        }
        let rejected = match latest {
            Some(outcome) => self.apply_latest_snapshot(config, outcome, anim_start, diag)?,
            None => false,
        };
        self.finish_snapshot_requests(fetch, saw_final, rejected);
        Ok(())
    }

    fn apply_latest_snapshot(
        &mut self,
        config: &ServeConfig,
        mut outcome: FetchOutcome,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> Result<bool> {
        self.last_known_elder = outcome.producer;
        if outcome.unchanged {
            self.fetched_at = Instant::now();
            return Ok(false);
        }
        let snapshot_ok = outcome.snapshot.is_ok();
        let fresh_pane_frame = outcome.fresh_pane_frame;
        if let Ok(pulled) = outcome.snapshot {
            self.last_pulled = pulled;
            let now_ms = crate::sidebar::timing::unix_now_ms();
            self.event_store.prune(now_ms);
            outcome.snapshot = Ok(self.fused_snapshot(now_ms));
        }
        self.fetched_at = Instant::now();
        let rejected = self.fold_outcome(config, outcome, anim_start, diag)?;
        if snapshot_ok {
            self.last_self_close_check = Instant::now();
        }
        self.release_paint_hold_after_snapshot(rejected, fresh_pane_frame);
        Ok(rejected)
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

    fn finish_snapshot_requests(
        &mut self,
        fetch: &mut FetchDispatcher,
        saw_final: bool,
        rejected: bool,
    ) {
        if !self.should_exit
            && saw_final
            && let Some(request) = fetch.take_pending()
        {
            fetch.request(request, false);
        }
        // A held transient regression asks for one more read so the
        // last-known-good cache heals to the next good frame. Single-flight
        // bounds this to one extra run.
        if !self.should_exit && saw_final && rejected {
            fetch.request(FetchRequest::default(), false);
        }
    }

    /// Fuse the last pulled snapshot with the overlay event store and any
    /// pending focus intent as of `now_ms`.
    fn fused_snapshot(&mut self, now_ms: u64) -> SidebarSnapshot {
        let intent = self.pending_focus_intent(now_ms);
        fuse(
            &self.last_pulled,
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
    ) -> Result<()> {
        let fused = self.fused_snapshot(crate::sidebar::timing::unix_now_ms());
        self.fold_outcome(
            config,
            FetchOutcome {
                snapshot: Ok(fused),
                final_for_request: false,
                fresh_pane_frame: false,
                unchanged: false,
                producer: self.last_known_elder,
            },
            anim_start,
            diag,
        )?;
        self.next_frame = Instant::now();
        Ok(())
    }

    pub(super) fn on_event(
        &mut self,
        config: &ServeConfig,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        envelope: SidebarEventEnvelope,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> Result<LoopFlow> {
        if !event_targets_this_renderer(&envelope, config) {
            return Ok(LoopFlow::Repoll);
        }
        let requests_verification = envelope.event.requests_producer_verification();
        let sent_at_ms = envelope.sent_at_ms;
        match envelope.event {
            SidebarEvent::Reload => {
                self.clear_pending_fetch();
                if reload_or_refetch(&config.session_name, fetch) {
                    self.reload_requested = true;
                    return Ok(LoopFlow::Exit);
                }
            }
            // The producer published a fresh shared pane frame: fold it from
            // cache immediately; consumers stay read-only and the producer's
            // own receipt is cheap because the frame is just-published.
            SidebarEvent::PaneFramePublished => {
                fetch.request(FetchRequest::pane_frame_published(), true);
            }
            event @ SidebarEvent::Notify { .. } => {
                self.handle_notification(config, terminal, event, diag);
            }
            SidebarEvent::FocusStranded { pane_id } => {
                self.handle_focus_stranded(config, pane_id, sent_at_ms, anim_start, diag)?;
            }
            SidebarEvent::FocusIntent { .. } => {
                self.fold_fused_now(config, anim_start, diag)?;
            }
            event if event.is_overlay() => {
                self.handle_overlay_event(config, fetch, event, sent_at_ms, anim_start, diag)?;
            }
            // Identity-free nudges — `StoreDelta`, `PanesChanged`, a
            // `PaneOpened` without a command: nothing to fuse, so refetch,
            // bypassing the pane cache when the event says topology moved.
            _ => {
                self.request_or_defer_identity_free(
                    fetch,
                    if requests_verification {
                        FetchRequest::producer_fresh_panes()
                    } else {
                        FetchRequest::default()
                    },
                );
            }
        }
        Ok(LoopFlow::Continue)
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

    fn handle_focus_stranded(
        &mut self,
        config: &ServeConfig,
        pane_id: PaneId,
        sent_at_ms: u64,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> Result<()> {
        let now_ms = crate::sidebar::timing::unix_now_ms();
        let own_pane = crate::mux::own_pane_id(config.mux);
        if let Some(target) = focus_stranded_target(
            &self.current,
            &self.ui,
            &pane_id,
            own_pane.as_ref(),
            sent_at_ms,
            now_ms,
        ) {
            // Match sidebar jumps: broadcast the intent before the mux
            // switch so peer tabs repaint while still hidden.
            self.record_focus_intent(config, target.clone(), anim_start, diag)?;
            spawn_pane_focus(target, &config.session_name);
        }
        Ok(())
    }

    fn handle_overlay_event(
        &mut self,
        config: &ServeConfig,
        fetch: &mut FetchDispatcher,
        event: SidebarEvent,
        sent_at_ms: u64,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> Result<()> {
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
        self.fold_fused_now(config, anim_start, diag)?;
        if !self.should_exit && (requests_verification || own_focused) {
            self.request_now_merging_pending(fetch, FetchRequest::producer_fresh_panes(), true);
        }
        if own_focused {
            self.optimistic_watch_until = Some(Instant::now() + FOCUS_RESUME_WATCH_WINDOW);
        } else if own_unfocused {
            self.optimistic_watch_until = None;
            self.ui.help_visible = false;
        }
        Ok(())
    }

    pub(super) fn on_resize(
        &mut self,
        config: &ServeConfig,
        runtime: &RuntimePaths,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        anim_start: Instant,
    ) -> Result<()> {
        self.paint.refresh_caps(config.mux, &config.session_name);
        // Once a sibling has been seen, a grow is the mux handing the sidebar
        // freed sibling space — the precondition for the self-close full-width
        // flash. Hold the paint until the next fresh pane-frame fold carries
        // the sibling count. Before that first sibling observation, the grow is
        // startup sizing and the first frame should paint immediately.
        let settled_width = terminal.size().map(|s| s.width).ok();
        if let Some(pending) = self.width_adjust_pending {
            if pending.elapsed() <= Duration::from_secs(3)
                && let Some(cols) = settled_width.and_then(std::num::NonZeroU16::new)
                && let Err(err) = crate::sidebar::width_override::write(runtime, cols)
            {
                warn!(error = %err, "sidebar width override write failed");
            }
            self.width_adjust_pending = None;
        }
        let grew = match settled_width {
            Some(width) => {
                let grew = resize_grew(self.prev_width, width);
                self.prev_width = Some(width);
                grew
            }
            None => false,
        };
        if grew && self.self_close.seen_sibling {
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
        self.request_now_merging_pending(fetch, FetchRequest::producer_fresh_panes(), true);
        Ok(())
    }

    #[cfg(test)]
    fn refresh_pet_render_caps_with(
        &mut self,
        mux: MuxName,
        session_name: &str,
        detect: impl FnOnce(MuxName, &str, PetRenderCaps) -> PetRenderCaps,
    ) {
        self.paint.refresh_caps_with(mux, session_name, detect);
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
        let interacted = applied.redraw
            || applied.focus.is_some()
            || applied.width.is_some()
            || applied.mark_read.is_some()
            || applied.mark_unread.is_some()
            || applied.mark_all_read;
        if interacted {
            order_hold::arm_order_hold(&mut self.ui, jiff::Timestamp::now().as_millisecond());
        }
        if let Some(pane) = applied.focus {
            // A jump records and broadcasts the focus intent so peer tabs
            // adopt the anchor offset and repaint while still hidden. The mux
            // focus switch fires last, so the destination tab is already at
            // the synced offset when it becomes visible. The producer pull
            // verifies the optimistic focus on the next wakeup.
            self.record_focus_intent(config, pane.clone(), anim_start, diag)?;
            spawn_pane_focus(pane, &config.session_name);
        }
        if let (Some(dir), Some(pane)) = (applied.width, config.own_pane.clone()) {
            let current_width = terminal.size().map(|size| size.width).ok();
            if width_adjust_allowed(dir, current_width) {
                self.width_adjust_pending = Some(Instant::now());
                spawn_width_adjust(pane, &config.session_name, dir);
            }
        }
        if let Some(row_id) = applied.mark_read {
            self.mark_row_read(fetch, &row_id, diag);
        }
        if let Some(row_id) = applied.mark_unread {
            self.mark_row_unread(fetch, &row_id, diag);
        }
        if applied.mark_all_read {
            self.mark_all_read(fetch, diag);
        }
        Ok(())
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
        if outcome.dismiss {
            self.health.alert = None;
        }
        if outcome.redraw {
            // Carry the live spin phase into the instant paint so a keypress
            // mid-spin never rewinds the animation to a stale frame.
            self.ui.animation_phase =
                wall_clock_phase(anim_start, self.current.theme.display.resolved_refresh_ms());
            self.paint.refresh_view(
                &mut self.ui,
                &self.current,
                self.health
                    .alert
                    .as_ref()
                    .is_some_and(render::Alert::is_active),
            );
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
        wake_room(runtime);
        self.dirty = true;
        self.next_frame = Instant::now();
        fetch.request(FetchRequest::default(), true);
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
        let Some(row) = self
            .current
            .worktree_groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .find(|row| row.id == row_id)
            .cloned()
        else {
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
        wake_room(&runtime);
        self.dirty = true;
        self.next_frame = Instant::now();
        fetch.request(FetchRequest::default(), true);
    }

    pub(super) fn run_maintenance(
        &mut self,
        fetch: &mut FetchDispatcher,
        ctx: MaintenanceContext<'_>,
    ) -> Result<()> {
        // Snapshot wakeups are a latency hint, not the only correctness path.
        // `rimz reload` replaces the renderer in place and a ready-result
        // datagram can be lost around socket teardown/rebind; the frame/tick
        // path still drains the channel so startup cannot strand the
        // placeholder cockpit.
        self.on_snapshot(ctx.config, fetch, ctx.result_rx, ctx.anim_start, ctx.diag)?;
        self.fire_due_pending_fetch(fetch);

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
        // self-gated to the data tick and no-ops while a fetch is in flight.
        if self.pending_fetch.is_none() && self.fetched_at.elapsed() >= ctx.tick {
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
        Ok(())
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
        let alert_active = self
            .health
            .alert
            .as_ref()
            .is_some_and(render::Alert::is_active);
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
        {
            self.paint_hold
                .engage(now, crate::sidebar::timing::unix_now_ms());
            return true;
        }
        false
    }

    /// Fold one fetch outcome into the render state: gate it against the
    /// last-known-good frame, update health, snapshot, and selection, and
    /// report whether the loop should exit.
    pub(super) fn apply_fetch_outcome(
        &mut self,
        config: &ServeConfig,
        outcome: FetchOutcome,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> Result<ApplyOutcome> {
        let (prev_good, rejected, now) = self.commit_fetch(config, outcome, diag);
        let prev_selected = self.ui.selected_pane.clone();
        let (focused_pane, cleared) = self.sweep_read_receipts(now, diag);
        self.reconcile_selection_and_order(
            &prev_good,
            prev_selected,
            focused_pane,
            cleared,
            now.as_millisecond(),
        );
        self.fold_spend_rolls(anim_start);
        Ok(self.exit_verdict(config, rejected))
    }

    fn exit_verdict(&mut self, config: &ServeConfig, rejected: bool) -> ApplyOutcome {
        // A renderer degraded this long is non-functional and, with a now-stale
        // heartbeat, unreachable by `rimz reload` — so it gives up rather than
        // lingering as a zombie showing a frozen frame. Exiting closes its
        // `close_on_exit` pane; reload/attach recovery then rebuilds a current
        // sidebar against the live panes.
        if degraded_too_long(&self.health, Timestamp::now()) {
            warn!(
                target: SIDEBAR_HEALTH_TARGET,
                session = %config.session_name,
                reason = self.health.alert.as_ref().map(|alert| alert.reason.as_str()),
                "sidebar degraded too long; exiting so the pane closes and reload/attach can rebuild it",
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
        config: &ServeConfig,
        outcome: FetchOutcome,
        diag: &crate::diag::DiagSink,
    ) -> (SidebarSnapshot, bool, Timestamp) {
        let is_elder = !diag.is_enabled()
            || RuntimePaths::for_workspace(config.workspace_id.clone())
                .ok()
                .map(|rt| crosscheck_enabled(self.elder_role.current(&rt, &config.instance_id)))
                .unwrap_or(true);
        // The gate compares the incoming snapshot against the last frame we actually
        // committed; `current` still holds it until we overwrite it below.
        let fetch_was_ok = outcome.snapshot.is_ok();
        let fetch_failure = outcome.snapshot.as_ref().err().cloned();
        let final_for_request = outcome.final_for_request;
        let prev_good = self.current.clone();
        let prev_health = self.health.clone();
        let prev_gate = self.gate.clone();
        let mut computed = compute_next_state(
            &config.workspace_id,
            None,
            outcome.snapshot,
            self.last_snapshot.take(),
            &self.health,
        );
        if fetch_was_ok && !final_for_request {
            // A fast-lane frame inside an open fetch cycle is paintable data, not a
            // health verdict. Let the final produce outcome recover or extend the
            // refresh episode so a repeated produce failure is not masked by the
            // frameless/status-only fast fold that precedes it.
            computed.health = self.health.clone();
        }
        let incoming_snapshot = computed.snapshot.clone();
        let now = Timestamp::now();
        let (state, next_gate, rejected, released_via_escape_hatch) =
            apply_gate(computed, fetch_was_ok, &prev_good, &self.gate, now);
        emit_diagnostics(
            diag,
            FetchDiagnostics {
                prev_snapshot: &prev_good,
                incoming_snapshot: &incoming_snapshot,
                next_snapshot: &state.snapshot,
                prev_health: &prev_health,
                next_health: &state.health,
                prev_gate: &prev_gate,
                next_gate: &next_gate,
                fetch_failure,
                rejected,
                released_via_escape_hatch,
                is_elder,
                now,
            },
        );
        self.install_fetch_state(state, next_gate);
        (prev_good, rejected, now)
    }

    fn install_fetch_state(&mut self, state: RenderState, next_gate: GateState) {
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
        self.last_snapshot = state.last_snapshot;
        self.health = state.health;
        self.current = state.snapshot;
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
        let live: HashSet<String> = self
            .current
            .worktree_groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .map(|row| row.id.clone())
            .collect();
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
        reconcile_selection(&mut self.ui, &self.current, derived);
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
        let answered = order_hold::focused_attention_dropped(
            prev_good,
            &self.current,
            self.ui.selected_pane.as_ref(),
        );
        let interacted = cleared || self.ui.selected_pane != prev_selected || answered;
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
        let today_usd = self.current.today_spend_live_usd.or(self
            .current
            .workspace_value_tally
            .as_ref()
            .map(|tally| tally.headline.usd));
        if let Some(usd) = today_usd {
            let shown = self
                .ui
                .spend_ratchet
                .observe(self.current.today_spend_epoch_secs, usd);
            self.ui.tally.observe(shown, self.ui.animation_phase);
        }
        // The per-card cost rolls fold beside it: observe each agent row's session
        // cost under its durable row id (pruning rows the snapshot no longer
        // carries), so a card's `$cost` ticks up on the next frames the same way.
        // A row without the cost enrichment is simply not observed; when its first
        // cost lands, the first observation snaps — never a `0 → cost` boot roll.
        self.ui.cost_rolls.observe(
            self.current
                .worktree_groups
                .iter()
                .flat_map(|group| group.rows.iter())
                .filter_map(|row| {
                    row.as_agent()
                        .and_then(|agent| agent.context.as_ref())
                        .and_then(|context| context.cost.as_ref())
                        .and_then(|cost| cost.total_cost_usd)
                        .map(|usd| (row.id.clone(), usd))
                }),
            self.ui.animation_phase,
        );
    }

    fn fold_outcome(
        &mut self,
        config: &ServeConfig,
        outcome: FetchOutcome,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> Result<bool> {
        let applied = self.apply_fetch_outcome(config, outcome, anim_start, diag)?;
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
        Ok(applied.rejected)
    }

    fn apply_focus_anchor(&mut self) {
        let Some(selected) = self.ui.selected_pane.clone() else {
            return;
        };
        let Some(anchor) = crate::sidebar::focus_anchor::load(self.read_marks.runtime()) else {
            return;
        };
        let now_ms = crate::sidebar::timing::unix_now_ms();
        if anchor.stamp_ms > self.ui.last_focus_anchor_ms
            && anchor.pane_id == selected
            && crate::sidebar::focus_anchor::is_fresh(anchor.stamp_ms, now_ms)
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
                    anchor.stamp_ms as i64,
                );
            }
            self.ui.last_focus_anchor_ms = anchor.stamp_ms;
        }
    }

    fn pending_focus_intent(&mut self, now_ms: u64) -> Option<FocusAnchor> {
        let anchor = crate::sidebar::focus_anchor::load(self.read_marks.runtime())?;
        if !crate::sidebar::focus_anchor::is_fresh(anchor.stamp_ms, now_ms)
            || anchor.stamp_ms <= self.confirmed_focus_intent_ms
        {
            return None;
        }
        if focus_intent_confirmed(&self.last_pulled, &self.event_store, &anchor, now_ms) {
            self.confirmed_focus_intent_ms = anchor.stamp_ms;
            return None;
        }
        Some(anchor)
    }

    fn record_focus_intent(
        &mut self,
        config: &ServeConfig,
        pane: PaneId,
        anim_start: Instant,
        diag: &crate::diag::DiagSink,
    ) -> Result<()> {
        let now_ms = crate::sidebar::timing::unix_now_ms();
        let anchor = FocusAnchor {
            pane_id: pane.clone(),
            offset: self.ui.scroll_offset,
            stamp_ms: now_ms,
            order: Some(self.ui.last_order.clone()),
        };
        if let Err(err) = crate::sidebar::focus_anchor::store(self.read_marks.runtime(), &anchor) {
            debug!(error = %err, "focus anchor write failed");
        }
        self.fold_fused_now(config, anim_start, diag)?;
        if let Ok(runtime) = RuntimePaths::for_workspace(config.workspace_id.clone())
            && let Err(err) = crate::store::wakeup::broadcast_sidebar_event(
                &runtime,
                Some(&config.session_name),
                SidebarEvent::FocusIntent {
                    pane_id: pane.clone(),
                },
            )
        {
            debug!(pane = %pane, error = %err, "renderer focus intent broadcast failed");
        }
        Ok(())
    }

    fn observe_commit(&mut self) {
        let now_ms = crate::sidebar::timing::unix_now_ms();
        let sig = observe::extract_sig(
            &self.current,
            &self.last_pulled,
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

/// Ping every sidebar in the room to refold after a mark read/unread — the
/// elder prunes or keeps the episode and peer tabs converge on the new state.
fn wake_room(runtime: &RuntimePaths) {
    if let Err(err) = crate::store::wakeup::wake_sidebars(runtime) {
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
