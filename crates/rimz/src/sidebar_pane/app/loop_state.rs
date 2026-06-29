use super::remind::RemindState;
use super::state::{
    emit_unread_cleared_trace, emit_unread_marked_trace, read_receipt_for_row, set_rows_unread,
};
use super::*;
use crate::sidebar::read_marks::{ReadMarkStore, write_manual_read_marks};
use crate::sidebar::unread::{self, UnreadClearCause};
use crate::sidebar_pane::pets::{PetAssets, PetRenderCaps, PixelPainter, detect_pet_render_caps};

pub(super) struct LoopState {
    last_snapshot: Option<SidebarSnapshot>,
    current: SidebarSnapshot,
    last_pulled: SidebarSnapshot,
    own_pane: Option<PaneId>,
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
        let animating = is_animating(&self.current, &self.ui, phase, alert_active);
        let active = (animating && self.watched()) || self.dirty;
        let timeout = if active {
            self.next_frame
                .saturating_duration_since(Instant::now())
                .max(FRAME_MIN_TIMEOUT)
        } else {
            // Cap by the watchdog so the self-close backstop fires on time even
            // when the data tick is much longer.
            let watchdog_due =
                SELF_CLOSE_WATCHDOG.saturating_sub(self.last_self_close_check.elapsed());
            tick.min(watchdog_due).max(FRAME_MIN_TIMEOUT)
        };
        (active, timeout)
    }

    /// Whether an attached client's focus currently lands in this sidebar's tab.
    /// Unknown ownership or own-view state reads as watched so uncertainty never
    /// suppresses motion.
    fn watched(&self) -> bool {
        let Some(own_pane) = self.own_pane.as_ref() else {
            return true;
        };
        let Some(view) = self.current.own_view.as_ref() else {
            return true;
        };
        view.active_pane_is_viewed
            || (view.own_is_active && self.current.viewed_panes.contains(own_pane))
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
        if let Some(pane) = applied.focused {
            // A jump fires the one-way focus command at the resolved pane. The
            // highlight moves only when the derived baseline catches up on a
            // later fold — late, never wrong — and any make-up filter clears
            // as focus leaves the tab.
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
        config: &ServeConfig,
        runtime: &RuntimePaths,
        socket_path: &Path,
        fetch: &mut FetchDispatcher,
        tick: Duration,
    ) {
        // Data backstop: catch pane/git drift no ledger delta announced. It is
        // self-gated to the data tick and no-ops while a fetch is in flight.
        if self.fetched_at.elapsed() >= tick {
            fetch.request(FetchRequest::default(), false);
        }

        // Heartbeat: fast in-process atomic write on the main thread so the
        // exit path never races a background writer.
        if heartbeat_write_due(self.last_heartbeat) {
            self.last_heartbeat = Some(Instant::now());
            if let Err(err) = write_heartbeat(config, runtime, socket_path) {
                warn!(
                    session = %config.session_name,
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
        if !self.should_exit && !paint_blocked && (active || self.dirty) && now >= self.next_frame {
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
        self.observe_commit();
        self.dirty = true;
        Ok(applied.rejected)
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
mod tests {
    use super::*;
    use crate::sidebar_pane::app::fixtures::{
        agent_snapshot, pane, snapshot_with_panes, workspace,
    };

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
            notification_prefs: crate::config::NotificationsPrefs::default(),
            pet_glyphs: crate::config::PetsGlyphMode::Auto,
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
        card.status = Some(crate::agents::AgentStatus::Running);
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
            })
            .expect("send fetch outcome");
        state
            .on_snapshot(config, fetch, &result_rx, Instant::now(), None)
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
    fn frame_timing_keeps_animation_when_visibility_is_unknown_or_dirty() {
        let ws = workspace();
        let (_dir, mut state) = loop_state_with_own_pane(&ws, None);
        state.current = animating_agent_snapshot(&ws);
        state.dirty = false;
        assert!(frame_active(&state));

        let own_pane = pane("terminal_1", "tab_0", false).pane_id;
        let (_dir, mut state) = loop_state_with_own_pane(&ws, Some(own_pane));
        state.current = animating_agent_snapshot(&ws);
        state.current.own_view = None;
        state.dirty = false;
        assert!(frame_active(&state));

        state.current.own_view = Some(own_view(false, false));
        state.current.viewed_panes.clear();
        state.dirty = true;
        assert!(frame_active(&state));
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
    fn resize_reprobe_refreshes_pet_render_caps_with_current_glyph_mode() {
        let ws = workspace();
        let (_dir, mut state) = loop_state(&ws);
        state.current.theme.pets.glyphs = crate::config::PetsGlyphMode::Pixel;
        let mut observed = None;

        state.refresh_pet_render_caps_with(
            crate::MuxName::Tmux,
            "rimz-test",
            |mux, mode, session| {
                observed = Some((mux, mode, session.to_owned()));
                PetRenderCaps { pixel: true }
            },
        );

        assert_eq!(
            observed,
            Some((
                crate::MuxName::Tmux,
                crate::config::PetsGlyphMode::Pixel,
                "rimz-test".to_owned()
            ))
        );
        assert_eq!(state.pet_render_caps, PetRenderCaps { pixel: true });
    }

    #[test]
    fn resize_reprobe_can_downgrade_enabled_pet_render_caps() {
        let ws = workspace();
        let (_dir, mut state) = loop_state(&ws);
        state.pet_render_caps = PetRenderCaps { pixel: true };

        state.refresh_pet_render_caps_with(crate::MuxName::Tmux, "rimz-test", |_, _, _| {
            PetRenderCaps { pixel: false }
        });

        assert_eq!(state.pet_render_caps, PetRenderCaps::default());
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LoopFlow {
    Continue,
    Repoll,
    Exit,
}
