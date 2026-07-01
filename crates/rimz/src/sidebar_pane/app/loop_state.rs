use super::remind::RemindState;
use super::state::{
    emit_unread_cleared_trace, emit_unread_marked_trace, read_receipt_for_row, set_rows_unread,
};
use super::*;
use crate::sidebar::read_marks::{ReadMarkStore, write_manual_read_marks};
use crate::sidebar::unread::{self, UnreadClearCause};
use crate::sidebar_pane::pets::{PetAssets, PetRenderCaps, PixelPainter, detect_pet_render_caps};

pub(super) struct MaintenanceContext<'a> {
    pub(super) config: &'a ServeConfig,
    pub(super) runtime: &'a RuntimePaths,
    pub(super) socket_path: &'a Path,
    pub(super) result_rx: &'a Receiver<FetchOutcome>,
    pub(super) anim_start: Instant,
    pub(super) diag: Option<&'a crate::diag::DiagSink>,
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
    hidden_count: usize,
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
    Agent(Option<crate::agents::AgentStatus>),
    Process(crate::ProcessState),
}

fn background_content_key(snapshot: &SidebarSnapshot) -> BackgroundContentKey {
    BackgroundContentKey {
        groups: snapshot
            .worktree_groups
            .iter()
            .map(|group| BackgroundGroupKey {
                key: group.key.clone(),
                hidden_count: group.hidden_count,
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

pub(super) struct LoopState {
    last_snapshot: Option<SidebarSnapshot>,
    current: SidebarSnapshot,
    last_pulled: SidebarSnapshot,
    own_pane: Option<PaneId>,
    optimistic_watch_until: Option<Instant>,
    event_store: EventStore,
    observer: observe::Observer,
    observe_tx: SyncSender<ObserveMsg>,
    health: Health,
    gate: GateState,
    self_close: SelfCloseState,
    ui: UiState,
    pet_assets: PetAssets,
    pixel_painter: PixelPainter,
    pet_render_caps: PetRenderCaps,
    read_marks: ReadMarkStore,
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
    pub(super) should_exit: bool,
    pub(super) tab_emptied: bool,
    pub(super) reexec_to: Option<PathBuf>,
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
            optimistic_watch_until: None,
            event_store: EventStore::default(),
            observer: observe::Observer::default(),
            observe_tx,
            health: Health::default(),
            gate: GateState::default(),
            self_close: SelfCloseState::default(),
            ui: UiState::default(),
            pet_assets: PetAssets::default(),
            pixel_painter: PixelPainter::new(pixel_wrap),
            pet_render_caps,
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
            should_exit: false,
            tab_emptied: false,
            reexec_to: None,
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
        let transition_effect_active = self.ui.effects.any_active();
        // Transition effects are bounded one-shots; keep their decay grid hot
        // even when continuous animation yields on an unwatched pane.
        let active = (watched && animating)
            || transition_effect_active
            || (self.dirty && self.dirty_paintable(watched));
        let timeout = if active {
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
        view.active_pane_is_viewed
            || (view.own_is_active && self.current.viewed_panes.contains(own_pane))
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

    pub(super) fn on_snapshot(
        &mut self,
        config: &ServeConfig,
        fetch: &mut FetchDispatcher,
        result_rx: &Receiver<FetchOutcome>,
        anim_start: Instant,
        diag: Option<&crate::diag::DiagSink>,
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
        let mut rejected = false;
        if let Some(mut outcome) = latest {
            let snapshot_ok = outcome.snapshot.is_ok();
            let fresh_pane_frame = outcome.fresh_pane_frame;
            if let Ok(pulled) = outcome.snapshot {
                self.last_pulled = pulled;
                let now_ms = crate::sidebar::cache::unix_now_ms();
                self.event_store.prune(now_ms);
                outcome.snapshot = Ok(fuse(&self.last_pulled, &self.event_store, now_ms));
            }
            self.fetched_at = Instant::now();
            rejected = self.fold_outcome(config, outcome, anim_start, diag)?;
            if snapshot_ok {
                self.last_self_close_check = Instant::now();
            }
            if !self.should_exit
                && !rejected
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
        Ok(())
    }

    pub(super) fn on_event(
        &mut self,
        config: &ServeConfig,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        envelope: SidebarEventEnvelope,
        anim_start: Instant,
        diag: Option<&crate::diag::DiagSink>,
    ) -> Result<LoopFlow> {
        if !event_targets_this_renderer(&envelope, config) {
            return Ok(LoopFlow::Repoll);
        }
        let requests_verification = envelope.event.requests_producer_verification();
        let sent_at_ms = envelope.sent_at_ms;
        match envelope.event {
            SidebarEvent::Reload => {
                if let Some(target) = reload_or_refetch(&config.session_name, fetch) {
                    self.reexec_to = Some(target);
                    return Ok(LoopFlow::Exit);
                }
            }
            // The producer published a fresh shared pane frame: fold it from
            // cache immediately; consumers stay read-only and the producer's
            // own receipt is cheap because the frame is just-published.
            SidebarEvent::PaneFramePublished => {
                fetch.request(FetchRequest::pane_frame_published(), true);
            }
            SidebarEvent::Notify {
                title,
                body,
                panes,
                recheck_unread,
                notification_kind,
            } => {
                let kind = notification_kind.as_deref().unwrap_or(if recheck_unread {
                    "agent"
                } else {
                    "link"
                });
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
                    Ok(true) => self.remind.note_ring(crate::sidebar::cache::unix_now_ms()),
                    Ok(false) => {}
                    Err(err) => debug!(error = %err, "terminal notification emit failed"),
                }
            }
            SidebarEvent::FocusStranded { pane_id } => {
                let now_ms = crate::sidebar::cache::unix_now_ms();
                let own_pane = crate::mux::own_pane_id(config.mux);
                if let Some(target) = focus_stranded_target(
                    &self.current,
                    &self.ui,
                    &pane_id,
                    own_pane.as_ref(),
                    sent_at_ms,
                    now_ms,
                ) {
                    spawn_pane_focus(target.clone(), &config.session_name);
                    self.record_focus_intent(config, target, anim_start, diag)?;
                }
            }
            // An overlay event fuses into the in-memory state and paints this
            // frame. A topology overlay also asks the producer to verify with a
            // real pull, which supersedes the overlay once its fresh frame
            // folds in. A resize-grow paint hold stays held until a pulled
            // sibling-count verdict releases it.
            event if event.is_overlay() => {
                let own = self.own_pane.as_ref();
                let own_focused = matches!(&event, SidebarEvent::FocusChanged { focused, .. }
                    if own.is_some_and(|pane| focused.contains(pane)));
                let own_unfocused = matches!(&event, SidebarEvent::FocusChanged { unfocused, .. }
                    if own.is_some_and(|pane| unfocused.contains(pane)));
                let now_ms = crate::sidebar::cache::unix_now_ms();
                self.event_store.append(event, sent_at_ms, now_ms);
                let fused = fuse(&self.last_pulled, &self.event_store, now_ms);
                self.fold_outcome(
                    config,
                    FetchOutcome {
                        snapshot: Ok(fused),
                        final_for_request: false,
                        fresh_pane_frame: false,
                    },
                    anim_start,
                    diag,
                )?;
                // Snap the frame deadline so this turn's frame phase paints
                // the fused frame now instead of waiting out a previously armed
                // grid boundary.
                self.next_frame = Instant::now();
                if !self.should_exit && requests_verification {
                    fetch.request(FetchRequest::producer_fresh_panes(), true);
                }
                if own_focused {
                    self.optimistic_watch_until = Some(Instant::now() + FOCUS_RESUME_WATCH_WINDOW);
                    if !self.should_exit {
                        fetch.request(FetchRequest::producer_fresh_panes(), true);
                    }
                } else if own_unfocused {
                    self.optimistic_watch_until = None;
                }
            }
            // Identity-free nudges — `LedgerDelta`, `PanesChanged`, a
            // `PaneOpened` without a command: nothing to fuse, so refetch,
            // bypassing the pane cache when the event says topology moved.
            _ => {
                fetch.request(
                    if requests_verification {
                        FetchRequest::producer_fresh_panes()
                    } else {
                        FetchRequest::default()
                    },
                    true,
                );
            }
        }
        Ok(LoopFlow::Continue)
    }

    pub(super) fn on_resize(
        &mut self,
        config: &ServeConfig,
        fetch: &mut FetchDispatcher,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        anim_start: Instant,
    ) -> Result<()> {
        self.refresh_pet_render_caps(config.mux, &config.session_name);
        // Once a sibling has been seen, a grow is the mux handing the sidebar
        // freed sibling space — the precondition for the self-close full-width
        // flash. Hold the paint until the next fresh pane-frame fold carries
        // the sibling count. Before that first sibling observation, the grow is
        // startup sizing and the first frame should paint immediately.
        let grew = match terminal.size().map(|s| s.width).ok() {
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
                .engage(Instant::now(), crate::sidebar::cache::unix_now_ms());
            self.clear_pixel(terminal);
        } else {
            if grew {
                self.clear_pixel(terminal);
            }
            if apply_input(
                Wakeup::Resize,
                &mut self.ui,
                &mut PetRender {
                    assets: &mut self.pet_assets,
                    pixel_painter: &mut self.pixel_painter,
                    caps: self.pet_render_caps,
                },
                &mut self.health,
                terminal,
                &self.current,
                anim_start,
            )?
            .painted
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

    fn refresh_pet_render_caps(&mut self, mux: MuxName, session_name: &str) {
        self.refresh_pet_render_caps_with(mux, session_name, detect_pet_render_caps);
    }

    fn refresh_pet_render_caps_with(
        &mut self,
        mux: MuxName,
        session_name: &str,
        detect: impl FnOnce(MuxName, crate::config::PetsGlyphMode, &str) -> PetRenderCaps,
    ) {
        self.pet_render_caps = detect(mux, self.current.theme.pets.glyphs, session_name);
    }

    pub(super) fn on_input(
        &mut self,
        config: &ServeConfig,
        wakeup: Wakeup,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        fetch: &mut FetchDispatcher,
        anim_start: Instant,
        diag: Option<&crate::diag::DiagSink>,
    ) -> Result<()> {
        let applied = apply_input(
            wakeup,
            &mut self.ui,
            &mut PetRender {
                assets: &mut self.pet_assets,
                pixel_painter: &mut self.pixel_painter,
                caps: self.pet_render_caps,
            },
            &mut self.health,
            terminal,
            &self.current,
            anim_start,
        )?;
        if applied.painted {
            // Key/mouse input paints synchronously for instant feedback; a
            // paint settles any frame the loop owed.
            self.dirty = false;
        }
        let interacted = applied.painted
            || applied.focused.is_some()
            || applied.mark_read.is_some()
            || applied.mark_unread.is_some();
        if interacted {
            order_hold::arm_order_hold(&mut self.ui, jiff::Timestamp::now().as_millisecond());
        }
        if let Some(pane) = applied.focused {
            // A jump fires the one-way focus command at the resolved pane. The
            // highlight moves only when the derived baseline catches up on a
            // later fold — late, never wrong — and any make-up filter clears
            // as focus leaves the tab.
            self.record_focus_anchor(&pane);
            spawn_pane_focus(pane.clone(), &config.session_name);
            self.record_focus_intent(config, pane, anim_start, diag)?;
        }
        if let Some(row_id) = applied.mark_read {
            self.mark_row_read(config, fetch, &row_id, diag);
        }
        if let Some(row_id) = applied.mark_unread {
            self.mark_row_unread(config, fetch, &row_id, diag);
        }
        Ok(())
    }

    /// Mark a row read without jumping (`m`): write the durable manual receipt,
    /// clear the row locally for an instant repaint, trace the clear, wake the
    /// room so the elder prunes the episode and peer tabs converge, and refetch
    /// so the receipt lands in the pulled snapshot. A no-op when the row is
    /// already read.
    fn mark_row_read(
        &mut self,
        config: &ServeConfig,
        fetch: &mut FetchDispatcher,
        row_id: &str,
        diag: Option<&crate::diag::DiagSink>,
    ) {
        if self.ui.unread_guard.as_deref() == Some(row_id) {
            self.ui.unread_guard = None;
        }
        let Ok(runtime) = RuntimePaths::for_workspace(config.workspace_id.clone()) else {
            return;
        };
        let now = jiff::Timestamp::now();
        let marks = self.read_marks.load_merged();
        let clear = read_receipt_for_row(
            &self.current,
            Some(row_id),
            UnreadClearCause::MarkRead,
            &marks,
            now,
        );
        if clear.ids.is_empty() {
            return;
        }
        if let Err(err) = write_manual_read_marks(&runtime, clear.ids.clone(), now.as_millisecond())
        {
            warn!(error = %err, "mark-read receipt write failed");
            return;
        }
        set_rows_unread(&mut self.current, &clear.ids, false);
        if let Some(diag) = diag {
            emit_unread_cleared_trace(diag, &clear.trace);
        }
        wake_room(&runtime);
        self.dirty = true;
        self.next_frame = Instant::now();
        fetch.request(FetchRequest::default(), true);
    }

    /// Re-flag a row unread without jumping (`M`): open a durable episode through
    /// the shared mark-unread path, set the row locally for an instant repaint,
    /// trace the open, wake the room, and refetch so the episode lands in the
    /// pulled snapshot. A no-op when the row has left the room.
    fn mark_row_unread(
        &mut self,
        config: &ServeConfig,
        fetch: &mut FetchDispatcher,
        row_id: &str,
        diag: Option<&crate::diag::DiagSink>,
    ) {
        let Ok(runtime) = RuntimePaths::for_workspace(config.workspace_id.clone()) else {
            return;
        };
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
        if let Some(diag) = diag {
            emit_unread_marked_trace(diag, &opened);
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

        // Data backstop: catch pane/git drift no ledger delta announced. It is
        // self-gated to the data tick and no-ops while a fetch is in flight.
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
        // close a lone sidebar.
        if self.last_self_close_check.elapsed() >= SELF_CLOSE_WATCHDOG {
            self.last_self_close_check = Instant::now();
            fetch.request(FetchRequest::default(), false);
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
        diag: Option<&crate::diag::DiagSink>,
    ) {
        self.remind
            .maybe_remind(config, terminal, &self.current, diag);
    }

    pub(super) fn clear_pixel(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) {
        if let Err(err) = self.pixel_painter.clear(terminal.backend_mut()) {
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
        // Once the tab has emptied, never paint again. A grow resize also
        // defers its paint until the sibling-count verdict releases the hold.
        let watched = self.watched();
        let background_key = (self.dirty && !watched && !self.dirty_paintable(watched))
            .then(|| background_content_key(&self.current));
        let background = background_key.as_ref().is_some_and(|key| {
            self.last_bg_paint.is_none_or(|at| {
                now.saturating_duration_since(at)
                    >= crate::sidebar::timing::BACKGROUND_PAINT_MIN_INTERVAL
            }) && self.last_bg_key.as_ref() != Some(key)
        });
        let paintable = (active && watched)
            || self.ui.effects.any_active()
            || (self.dirty && self.dirty_paintable(watched));
        if !self.should_exit
            && !paint_blocked
            && ((paintable && now >= self.next_frame) || background)
        {
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
                refresh_pet_view(
                    &mut self.ui,
                    &mut self.pet_assets,
                    &self.current,
                    self.pet_render_caps,
                    alert_active,
                );
                super::draw_frame_and_paint_pet_pixel(
                    terminal,
                    &self.current,
                    self.health.alert.as_ref(),
                    &mut self.ui,
                    &self.pet_assets,
                    &mut self.pixel_painter,
                )?;
                self.dirty = false;
                if was_dirty {
                    self.last_bg_key = Some(
                        background_key
                            .clone()
                            .unwrap_or_else(|| background_content_key(&self.current)),
                    );
                }
                if background {
                    self.last_bg_paint = Some(now);
                }
            }
            self.next_frame = next_frame_after(
                self.next_frame,
                now,
                frame_interval(&self.current, &self.ui, alert_active),
            );
        } else if !active && !self.dirty {
            // Idle re-arm only: with a fold pending, the armed boundary must
            // hold so a paint already due within one frame is not pushed out.
            self.next_frame = now + animation_frame(&self.current);
        }
        Ok(())
    }

    fn arm_paint_hold_on_grow(&mut self, width: u16, now: Instant) -> bool {
        if !self.paint_hold.is_engaged()
            && self.self_close.seen_sibling
            && resize_grew(self.prev_width, width)
        {
            self.paint_hold
                .engage(now, crate::sidebar::cache::unix_now_ms());
            return true;
        }
        false
    }

    fn fold_outcome(
        &mut self,
        config: &ServeConfig,
        outcome: FetchOutcome,
        anim_start: Instant,
        diag: Option<&crate::diag::DiagSink>,
    ) -> Result<bool> {
        let applied = apply_fetch_outcome(
            config,
            outcome,
            &mut self.last_snapshot,
            &mut self.current,
            &mut self.health,
            &mut self.gate,
            &mut self.self_close,
            &mut self.ui,
            &mut self.read_marks,
            anim_start,
            diag,
        )?;
        self.should_exit = applied.should_exit;
        self.tab_emptied |= applied.tab_emptied;
        self.apply_focus_anchor();
        self.observe_commit();
        self.dirty = true;
        Ok(applied.rejected)
    }

    fn record_focus_anchor(&self, pane: &PaneId) {
        let anchor = crate::sidebar::focus_anchor::FocusAnchor {
            pane_id: pane.clone(),
            offset: self.ui.scroll_offset,
            stamp_ms: crate::sidebar::cache::unix_now_ms(),
        };
        if let Err(err) = crate::sidebar::focus_anchor::store(self.read_marks.runtime(), &anchor) {
            debug!(error = %err, "focus anchor write failed");
        }
    }

    fn apply_focus_anchor(&mut self) {
        let Some(selected) = self.ui.selected_pane.clone() else {
            return;
        };
        let Some(anchor) = crate::sidebar::focus_anchor::load(self.read_marks.runtime()) else {
            return;
        };
        let now_ms = crate::sidebar::cache::unix_now_ms();
        if anchor.stamp_ms > self.ui.last_focus_anchor_ms
            && anchor.pane_id == selected
            && crate::sidebar::focus_anchor::is_fresh(anchor.stamp_ms, now_ms)
        {
            self.ui.scroll_offset = anchor.offset;
            self.ui.manual_scroll = None;
            self.ui.last_focus_anchor_ms = anchor.stamp_ms;
        }
    }

    fn record_focus_intent(
        &mut self,
        config: &ServeConfig,
        pane: PaneId,
        anim_start: Instant,
        diag: Option<&crate::diag::DiagSink>,
    ) -> Result<()> {
        let now_ms = crate::sidebar::cache::unix_now_ms();
        let event = SidebarEvent::FocusChanged {
            focused: vec![pane.clone()],
            unfocused: Vec::new(),
        };
        self.event_store.append(event.clone(), now_ms, now_ms);
        let fused = fuse(&self.last_pulled, &self.event_store, now_ms);
        self.fold_outcome(
            config,
            FetchOutcome {
                snapshot: Ok(fused),
                final_for_request: false,
                fresh_pane_frame: false,
            },
            anim_start,
            diag,
        )?;
        self.next_frame = Instant::now();
        if let Ok(runtime) = RuntimePaths::for_workspace(config.workspace_id.clone())
            && let Err(err) = crate::ledger::wakeup::broadcast_sidebar_event(
                &runtime,
                Some(&config.session_name),
                event,
            )
        {
            debug!(pane = %pane, error = %err, "renderer focus event broadcast failed");
        }
        Ok(())
    }

    fn observe_commit(&mut self) {
        let now_ms = crate::sidebar::cache::unix_now_ms();
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
    if let Err(err) = crate::ledger::wakeup::wake_sidebars(runtime) {
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
