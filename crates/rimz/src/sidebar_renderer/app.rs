//! Runtime loop for the native sidebar process.
//!
//! `serve` owns the fixed-timestep event loop and the wiring; each concern the
//! loop folds lives in its own submodule — [`fetch`] (the two-speed off-thread
//! fetch cycle), [`state`] (the pure `compute_next_state` reducer and the fold
//! integrator), [`gate`] (the last-known-good regression hold), [`health`]
//! (failure debounce and give-up), [`lifecycle`] (self-close and resize-grow
//! classification), [`reload`] (binary resolution and re-exec), and
//! [`selection`] (the identity-keyed highlight and input handlers).

use std::io::{self, Write};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::NotificationsPrefs;
use crate::ids::PaneId;
use crate::ledger::paths::PathErr;
use crate::schema::sidebar_event::{SidebarEvent, SidebarEventEnvelope};
use crate::sidebar::events::EventStore;
use crate::sidebar::fuse::fuse;
use crate::sidebar::timing::{FOCUS_STRANDED_EVENT_TTL, HEARTBEAT_WRITE_INTERVAL};
use crate::sidebar_renderer::osc;
use crate::{MuxName, RuntimePaths, SidebarInstanceId, SidebarSnapshot, WorkspaceId};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use tracing::{debug, warn};

use crate::sidebar_renderer::render::{self, UiState};
use crate::tui::{MouseCapture, TerminalModeGuard};

mod fetch;
#[cfg(test)]
mod fixtures;
mod gate;
mod health;
mod input;
mod lifecycle;
mod reload;
mod selection;
mod state;
mod tmux_watch;
mod transcript_watch;

use fetch::{FetchDispatcher, FetchOutcome, FetchRequest, spawn_fetch_worker};
use gate::GateState;
use input::{Wakeup, encode_key, encode_mouse, wait_for_wakeup};
use lifecycle::{SELF_CLOSE_WATCHDOG, SelfCloseState, resize_grew};
use reload::{ReloadAction, reexec_self, reload_action};
use selection::{InputOutcome, handle_key, handle_mouse_click, handle_scroll, row_index_of_pane};
use state::{apply_fetch_outcome, placeholder_snapshot};

pub use health::Health;
pub use state::{RenderState, compute_next_state};

const SIDEBAR_TERMINAL_TITLE: &str = "rimz-sidebar";

#[derive(Clone, Debug)]
pub struct ServeConfig {
    pub workspace_id: WorkspaceId,
    pub mux: MuxName,
    pub session_name: String,
    pub instance_id: SidebarInstanceId,
    pub tick_seconds: u64,
    /// One-shot render-cadence override from the launch argv. It is applied to
    /// this renderer's folded snapshots only; shared producer caches stay
    /// config-shaped so recovery can fall back to `[sidebar].refresh_ms`.
    pub refresh_ms_override: Option<u16>,
    pub notification_prefs: NotificationsPrefs,
    /// The sidebar's own mux pane, resolved once from the per-pane env at
    /// launch (`crate::mux::own_pane_id`) — the fold's self-exclusion and the
    /// heartbeat's pane claim. `None` outside a pane. Carried here rather than
    /// re-read ambiently so the fetch worker stays hermetic: a test (or any
    /// embedder) folds exactly the panes it published, regardless of the env
    /// the process inherited.
    pub own_pane: Option<crate::ids::PaneId>,
}

#[derive(Debug, thiserror::Error)]
pub enum SidebarAppErr {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Paths(#[from] PathErr),
    #[error("running `{program}`: {source}")]
    CommandIo {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("heartbeat write failed: {0}")]
    Heartbeat(String),
}

pub type Result<T> = std::result::Result<T, SidebarAppErr>;

pub fn serve(config: ServeConfig) -> Result<()> {
    set_terminal_title()?;
    let runtime = RuntimePaths::for_workspace(config.workspace_id.clone())?;
    runtime.ensure_dirs()?;
    let socket_path = sidebar_socket_path(&runtime, &config.instance_id);
    let socket = bind_socket(&socket_path)?;
    let _socket_cleanup = RuntimeFileGuard {
        path: socket_path.clone(),
    };
    // Drop the heartbeat on exit too — including the self-close below. A
    // lingering heartbeat stays mtime-fresh for `SIDEBAR_HEARTBEAT_TTL`, during
    // which `rimz`'s freshness gate would skip relaunch and let a plain
    // `attach` rebirth the session with no sidebar.
    let _heartbeat_cleanup = RuntimeFileGuard {
        path: runtime.sidebar_heartbeat_path(&config.instance_id),
    };
    let tick = tick_for(config.tick_seconds);

    // Redraw the instant the pane is resized — most importantly when a user
    // attaches to a background session and Zellij sizes the pane for the first
    // time. The watcher nudges this loop through the same wakeup socket the
    // ledger uses, so a resize is just another wakeup; without it the first
    // usable frame waits for the next `tick`, reading as a blank sidebar.
    let _input_mode = TerminalModeGuard::enable(MouseCapture::Stdout)?;
    spawn_event_waker(socket_path.clone());
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut last_snapshot: Option<SidebarSnapshot> = None;
    let mut current = placeholder_snapshot(config.workspace_id.clone());
    let mut last_pulled = current.clone();
    let mut event_store = EventStore::default();
    let mut health = Health::default();
    let mut gate = GateState::default();
    let mut self_close = SelfCloseState::default();
    let mut ui = UiState::default();
    let mut reexec_to: Option<PathBuf> = None;
    // Monotonic base for the animation frame. Deriving the phase from elapsed
    // wall-clock (rather than a per-tick counter) keeps the spin continuous
    // across re-fetches and ledger deltas, so no redraw path can stall it.
    let anim_start = Instant::now();

    // The snapshot fetch (fast in-process fold plus optional produce) runs on a
    // background worker, so animation and input never block on it. The worker
    // posts `SNAPSHOT_WAKEUP` when a result is ready. The dispatcher coalesces
    // requests so a ledger-delta storm or a slow produce can never queue more
    // than one extra run.
    let (request_tx, request_rx) = std::sync::mpsc::channel::<FetchRequest>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<FetchOutcome>();
    // `JoinHandle` drops without blocking: the thread runs to completion on its
    // own when `request_tx` is dropped at function exit.
    let _fetch_handle = spawn_fetch_worker(
        config.clone(),
        runtime.clone(),
        socket_path.clone(),
        config.notification_prefs.clone(),
        request_rx,
        result_tx,
    );
    let mut fetch = FetchDispatcher::new(request_tx);

    // tmux fast path: the elected producer streams control-mode topology
    // nudges into this loop's socket so a pane open/close can publish a fresh
    // pane frame in tens of milliseconds instead of waiting out the poll.
    // Latency only — the poll stays the presence backstop. Zellij reaches the
    // same producer-publication path through its presence plugin.
    if config.mux == MuxName::Tmux {
        let _ = tmux_watch::spawn(
            runtime.clone(),
            config.instance_id.clone(),
            config.session_name.clone(),
        );
    }

    // Codex rollout fast path: the elected producer watches each live root
    // Codex session's transcript file and runs the stat-gated sidecar refresh
    // on the write, so mid-turn token/cost updates repaint without waiting for
    // the next hook push or producer tick. Latency only — the tick backstop
    // stays truth. Backend-independent; the elder gate inside scopes the work.
    let _ = transcript_watch::spawn(runtime.clone(), config.instance_id.clone());

    // Write the heartbeat immediately so the freshness gate never sees a gap.
    // Errors are non-fatal; the gate re-probes after the TTL.
    if let Err(err) = write_heartbeat(&config, &runtime, &socket_path) {
        warn!(
            session = %config.session_name,
            error = %err,
            "initial heartbeat write failed",
        );
    }

    // Fire the first fetch on the background worker and start the main loop
    // immediately rather than blocking on a synchronous call: the first fetch
    // can take several seconds (Zellij just started, git cold-start), and a
    // blocked main thread delays the self-close watchdog, stalling cleanup.
    // The placeholder snapshot renders while the first real result is in flight.
    let mut fetched_at = Instant::now();
    let mut should_exit = false;
    let mut tab_emptied = false;
    fetch.request(FetchRequest::default(), false);

    // One fixed-timestep event loop. Events fold into the in-process model and
    // mark the frame dirty; the loop paints at most once per configured base
    // frame boundary, coalescing every change that landed mid-frame into a
    // single paint. Data and animation ride this frame grid; input paints
    // synchronously for instant feedback (see `apply_input`). The grid stays
    // warm while there is something to show (`active`) and relaxes to the
    // `tick` backstop when idle, snapping back the instant an event or
    // animation arrives. The loop blocks only in `recv`, so no path forks a
    // subprocess on the render thread and a busy fetch never freezes the spin
    // or swallows a keypress.
    let mut dirty = true;
    let mut next_frame = Instant::now();
    let mut last_self_close_check = Instant::now();
    // The sidebar's own pane width as of the last resize the loop processed. A
    // resize that grows it is the precondition for the self-close full-width
    // flash, so a grow holds its repaint (`paint_held`) until the sibling-count
    // verdict lands — close exits without painting, stay paints at the new size.
    let mut prev_width: Option<u16> = terminal.size().map(|s| s.width).ok();
    let mut paint_held = false;
    // Heartbeat writes live on the main thread (fast in-process atomic writes)
    // so the exit path can remove the file without racing a background writer.
    let mut last_heartbeat: Option<Instant> = None;
    while !should_exit {
        // A live row's spinner or a value-corner count-up keeps the frame grid
        // warm; a pending fold (`dirty`) does too. With neither, drop to the
        // slow data backstop until the next wakeup re-arms the grid.
        let phase = wall_clock_phase(anim_start, current.sidebar.resolved_refresh_ms());
        let animating = render::has_live_animation(&current)
            || ui.tally.any_rolling(phase)
            || ui.cost_rolls.any_rolling(phase)
            || ui.scrollbar.fading(phase)
            || ui.effects.any_active();
        let active = animating || dirty;
        let timeout = if active {
            next_frame
                .saturating_duration_since(Instant::now())
                .max(FRAME_MIN_TIMEOUT)
        } else {
            // Cap by the watchdog so the self-close backstop fires on time even
            // when the data tick is much longer.
            let watchdog_due = SELF_CLOSE_WATCHDOG.saturating_sub(last_self_close_check.elapsed());
            tick.min(watchdog_due).max(FRAME_MIN_TIMEOUT)
        };
        socket.set_read_timeout(Some(timeout))?;
        match wait_for_wakeup(&socket)? {
            // A background fetch posted a result. Take the most recent one
            // (drop any older queued posts — later is fresher), fold it, then
            // fire the deferred refetch a ledger delta asked for while the
            // cycle was in flight. A cycle's fast in-process frame arrives
            // marked non-final while its fork still runs; only a final outcome
            // closes the cycle for the single-flight accounting.
            Wakeup::Snapshot => {
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
                        last_pulled = pulled;
                        let now_ms = crate::sidebar::cache::unix_now_ms();
                        event_store.prune(now_ms);
                        outcome.snapshot = Ok(fuse(&last_pulled, &event_store, now_ms));
                    }
                    fetched_at = Instant::now();
                    let applied = apply_fetch_outcome(
                        &config,
                        outcome,
                        &mut last_snapshot,
                        &mut current,
                        &mut health,
                        &mut gate,
                        &mut self_close,
                        &mut ui,
                        anim_start,
                    )?;
                    should_exit = applied.should_exit;
                    tab_emptied |= applied.tab_emptied;
                    rejected = applied.rejected;
                    if snapshot_ok {
                        last_self_close_check = Instant::now();
                    }
                    // The fold mutated the model; the frame phase paints it.
                    dirty = true;
                    if !should_exit && !applied.rejected && fresh_pane_frame {
                        // The snapshot folded a post-signal pane frame. Its
                        // own-view verdict has decided the resize-grow case:
                        // exit without painting when alone, or release the hold
                        // and paint at the new size when siblings remain.
                        paint_held = false;
                    }
                }
                if !should_exit
                    && saw_final
                    && let Some(request) = fetch.take_pending()
                {
                    fetch.request(request, false);
                }
                // A held transient regression: ask for one more read so the
                // last-known-good cache heals to the next good frame. Single-
                // flight bounds this to one extra run; once the escape hatch
                // opens, the fetch is accepted and `rejected` clears, so this
                // never spins.
                if !should_exit && saw_final && rejected {
                    fetch.request(FetchRequest::default(), false);
                }
            }
            Wakeup::Event(envelope) => {
                if !event_targets_this_renderer(&envelope, &config) {
                    continue;
                }
                let requests_verification = envelope.event.requests_producer_verification();
                let sent_at_ms = envelope.sent_at_ms;
                match envelope.event {
                    SidebarEvent::Reload => {
                        if let Some(target) = reload_or_refetch(&config.session_name, &mut fetch) {
                            reexec_to = Some(target);
                            break;
                        }
                    }
                    // The producer published a fresh shared pane frame: fold it
                    // from cache immediately; consumers stay read-only and the
                    // producer's own receipt is cheap because the frame is
                    // just-published.
                    SidebarEvent::PaneFramePublished => {
                        fetch.request(FetchRequest::pane_frame_published(), true);
                    }
                    SidebarEvent::Notify { title, body, panes } => {
                        if let Err(err) = emit_terminal_notification(
                            &config,
                            &mut terminal,
                            &current,
                            &config.notification_prefs,
                            &title,
                            &body,
                            &panes,
                        ) {
                            debug!(error = %err, "terminal notification emit failed");
                        }
                    }
                    SidebarEvent::FocusStranded { pane_id } => {
                        let now_ms = crate::sidebar::cache::unix_now_ms();
                        let own_pane = crate::mux::own_pane_id(config.mux);
                        if let Some(target) = focus_stranded_target(
                            &current,
                            &ui,
                            &pane_id,
                            own_pane.as_ref(),
                            sent_at_ms,
                            now_ms,
                        ) {
                            spawn_pane_focus(target);
                        }
                    }
                    // An overlay event fuses into the in-memory state and paints
                    // this frame — the zero-latency path. A topology overlay also
                    // asks the producer to verify with a real pull, which
                    // supersedes the overlay once its fresh frame folds in. The
                    // resize-grow `paint_held` deliberately stays held: only a
                    // *pulled* sibling-count verdict may release it, so a fused
                    // close never paints the grown full-width frame on its way out.
                    event if event.is_overlay() => {
                        let now_ms = crate::sidebar::cache::unix_now_ms();
                        event_store.append(event, sent_at_ms, now_ms);
                        let fused = fuse(&last_pulled, &event_store, now_ms);
                        let applied = apply_fetch_outcome(
                            &config,
                            FetchOutcome {
                                snapshot: Ok(fused),
                                final_for_request: false,
                                fresh_pane_frame: false,
                            },
                            &mut last_snapshot,
                            &mut current,
                            &mut health,
                            &mut gate,
                            &mut self_close,
                            &mut ui,
                            anim_start,
                        )?;
                        should_exit = applied.should_exit;
                        tab_emptied |= applied.tab_emptied;
                        dirty = true;
                        // Snap the frame deadline so this turn's frame phase
                        // paints the fused frame now — the same instant
                        // feedback input gets — instead of waiting out a grid
                        // boundary armed before this event landed. The grid
                        // re-anchors off this paint, so a burst of events
                        // still coalesces to one paint per base frame.
                        next_frame = Instant::now();
                        if !should_exit && requests_verification {
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
            }
            // A recv timeout: the active grid reached a frame boundary, or the
            // idle backstop interval elapsed. It carries no state of its own —
            // the frame phase below advances the spin and paints, and the
            // backstop poll runs there too.
            Wakeup::Tick => {}
            Wakeup::Resize => {
                // A grow is the mux handing the sidebar a freed sibling's space —
                // the precondition for the self-close full-width flash. Hold the
                // paint until the next fresh pane-frame fold carries the sibling
                // count: a "close" verdict exits without ever painting the grown
                // frame (the frame phase guards on `should_exit`); a "stay" verdict
                // releases the hold and paints at the new size. A shrink, a
                // same-width resize, or an unreadable size cannot flash, so each
                // keeps the instant repaint for snappy attach/redraw feedback.
                let grew = match terminal.size().map(|s| s.width).ok() {
                    Some(width) => {
                        let grew = resize_grew(prev_width, width);
                        prev_width = Some(width);
                        grew
                    }
                    None => false,
                };
                if grew {
                    dirty = true;
                    paint_held = true;
                } else {
                    if apply_input(
                        Wakeup::Resize,
                        &mut ui,
                        &mut health,
                        &mut terminal,
                        &current,
                        anim_start,
                    )? {
                        dirty = false;
                    }
                    // A safe-width paint just landed; drop any stale hold a prior
                    // grow left pending so it cannot suppress this frame.
                    paint_held = false;
                }
                last_self_close_check = Instant::now();
                // A resize is the mux telling us topology changed: a split
                // opened/closed, or the sidebar got space back. Pull a fresh
                // pane list through the elected producer and require a cache
                // produced after this signal; consumers wait for the producer's
                // publication wake instead of locally producing.
                fetch.request(FetchRequest::producer_fresh_panes(), true);
            }
            // The `r` keypress rides the local `reload` control word; an
            // external `rimz reload` arrives as the typed event. Both resolve
            // through the same helper.
            Wakeup::Reload => {
                if let Some(target) = reload_or_refetch(&config.session_name, &mut fetch) {
                    reexec_to = Some(target);
                    break;
                }
            }
            wakeup => {
                // Key/mouse input paints synchronously for instant feedback; a
                // paint settles any frame the loop owed.
                if apply_input(
                    wakeup,
                    &mut ui,
                    &mut health,
                    &mut terminal,
                    &current,
                    anim_start,
                )? {
                    dirty = false;
                }
            }
        }

        // Data backstop: catch pane/git drift no ledger delta announced. Self-
        // gated to the `tick` interval and a no-op while a fetch is in flight, so
        // it neither double-fires nor rides the frame grid (the removed
        // ACTIVE_REFRESH anti-pattern). Runs once per loop turn regardless of why
        // we woke.
        if fetched_at.elapsed() >= tick {
            fetch.request(FetchRequest::default(), false);
        }

        // Heartbeat: fast in-process atomic write on the main thread so the
        // exit path (drop _heartbeat_cleanup) never races a background writer.
        if heartbeat_write_due(last_heartbeat) {
            last_heartbeat = Some(Instant::now());
            if let Err(err) = write_heartbeat(&config, &runtime, &socket_path) {
                warn!(
                    session = %config.session_name,
                    error = %err,
                    "heartbeat write failed",
                );
            }
        }

        // Self-close watchdog: if no resize or presence event fired (e.g.
        // background sessions where the mux omits SIGWINCH after a pane closes),
        // ask the normal snapshot path to refresh so the snapshot's own-view
        // count can close a lone sidebar. This preserves the one-producer bound:
        // consumers read the shared pane cache in process instead of each
        // forking `list-panes`.
        if last_self_close_check.elapsed() >= SELF_CLOSE_WATCHDOG {
            last_self_close_check = Instant::now();
            fetch.request(FetchRequest::default(), false);
        }

        // Frame phase: at the boundary, advance the spin and paint once, folding
        // every change that landed this frame into a single draw. Paint when the
        // model changed (`dirty`) or a row is animating; an idle frame is a bare
        // timer wake with no recompose. While idle, keep the grid armed so the
        // next event paints within one base frame.
        let now = Instant::now();
        if dirty {
            let dirty_deadline = now + animation_frame(&current);
            if next_frame > dirty_deadline {
                next_frame = dirty_deadline;
            }
        }
        // `!should_exit`: once the tab has emptied, never paint again — this is
        // what stops the last frame from flashing at the grown/full width on the
        // way out. `!paint_held`: a grow resize defers its paint until the
        // sibling-count verdict releases the hold (see the resize handler).
        // `active || dirty`: `active` is the turn-entry view, so a fold this
        // turn re-activates the phase through the `dirty` it set — without it,
        // an overlay event landing in an idle room would wait out one more
        // recv before its snapped deadline could paint.
        if !should_exit && !paint_held && (active || dirty) && now >= next_frame {
            ui.animation_phase =
                wall_clock_phase(anim_start, current.sidebar.resolved_refresh_ms());
            let animating = render::animation_cadence(&current) != render::AnimationCadence::None
                || ui.tally.any_rolling(ui.animation_phase)
                || ui.cost_rolls.any_rolling(ui.animation_phase)
                || ui.scrollbar.fading(ui.animation_phase)
                || ui.effects.any_active();
            if dirty || animating {
                render::draw_to_terminal_with_ui(
                    &mut terminal,
                    &current,
                    health.alert.as_ref(),
                    &mut ui,
                )?;
                dirty = false;
            }
            next_frame = next_frame_after(next_frame, now, frame_interval(&current, &ui));
        } else if !active && !dirty {
            // Idle re-arm only: with a fold pending (`dirty`), the armed
            // boundary must hold — re-arming here would push a paint already
            // due within one frame out by another.
            next_frame = now + animation_frame(&current);
        }
    }
    if tab_emptied {
        close_self_closing_view_floating_panes(&config);
    }
    if let Some(target) = reexec_to {
        // Restore the terminal and release this instance's runtime files before
        // replacing the process image — `exec` never returns, so their RAII
        // Drop would otherwise be skipped and leak a stale socket + heartbeat.
        drop(_input_mode);
        drop(_socket_cleanup);
        drop(_heartbeat_cleanup);
        return Err(reexec_self(&target));
    }
    Ok(())
}

fn close_self_closing_view_floating_panes(config: &ServeConfig) {
    let Some(anchor) = config.own_pane.as_ref() else {
        return;
    };
    match crate::mux::backend_for(config.mux)
        .close_view_floating_panes(&config.session_name, anchor)
    {
        Ok(closed) if closed.is_empty() => {}
        Ok(closed) => debug!(
            session = %config.session_name,
            panes = ?closed,
            "closed floating panes left in the self-closing sidebar tab",
        ),
        Err(err) => warn!(
            session = %config.session_name,
            pane = %anchor,
            error = %err,
            "could not close floating panes left in the self-closing sidebar tab",
        ),
    }
}

fn set_terminal_title() -> io::Result<()> {
    let mut stdout = io::stdout();
    write!(stdout, "\x1b]2;{SIDEBAR_TERMINAL_TITLE}\x07")?;
    stdout.flush()
}

/// Animation tick: how often an animated row advances a spin frame — a running
/// agent's head, a resolver, or an active process spinning on real work. Pure
/// in-process redraw from the cached snapshot — it never forks a fetch — so the
/// spin layer is decoupled from the data layer and stays smooth regardless of
/// fetch latency. Clamped against the data tick so a slow `tick_seconds` never
/// stutters, and only used while [`render::has_live_animation`] reports
/// something to move.
/// Floor for the frame-boundary recv timeout. When the loop is at or past the
/// next frame boundary, the time-to-boundary is zero; a 1ms floor lets an
/// already-queued datagram drain on this turn without a zero-timeout busy spin.
const FRAME_MIN_TIMEOUT: Duration = Duration::from_millis(1);

/// The animation frame index for `now`, derived from elapsed wall-clock since
/// the serve loop's monotonic base. Every redraw path sets the phase from this,
/// so the spin advances on real time and survives re-fetches and ledger deltas
/// without a per-tick counter that a break-and-refetch could reset.
fn wall_clock_phase(start: Instant, refresh_ms: u16) -> u64 {
    (start.elapsed().as_millis() / u128::from(refresh_ms)) as u64
}

fn frame_interval(snapshot: &SidebarSnapshot, ui: &UiState) -> Duration {
    let refresh_ms = snapshot.sidebar.resolved_refresh_ms();
    let base = crate::sidebar::timing::animation_frame(refresh_ms);
    // A decaying one-shot flash needs the fast grid to read as motion; it is
    // brief and self-terminating, so the cost is bounded to the transition
    // window. The continuous attention glow deliberately rides the slow
    // cosmetic cadence below — the breath already keeps it warm.
    if ui.scrollbar.fading(ui.animation_phase) || ui.effects.any_active() {
        return base;
    }
    // The money rolls click once per `CLICK_PHASES` phases, so a rolling room
    // samples on the matching money grid — one paint per distinct click, and
    // the one-click settle flash can never fall between samples. A fast room
    // (a working spinner) keeps the fast grid; the roll's painted value simply
    // holds across the extra frames. A slow-cadence room drops to the money
    // grid while a climb is in flight — the cosmetic breath repaints
    // idempotently, and the climb window bounds the extra paints.
    let money_rolling =
        ui.tally.any_rolling(ui.animation_phase) || ui.cost_rolls.any_rolling(ui.animation_phase);
    match render::animation_cadence(snapshot) {
        render::AnimationCadence::Fast => base,
        _ if money_rolling => {
            crate::sidebar::timing::money_animation_frame(refresh_ms, render::CLICK_PHASES)
        }
        render::AnimationCadence::Slow => crate::sidebar::timing::slow_animation_frame(refresh_ms),
        render::AnimationCadence::None => base,
    }
}

fn animation_frame(snapshot: &SidebarSnapshot) -> Duration {
    crate::sidebar::timing::animation_frame(snapshot.sidebar.resolved_refresh_ms())
}

fn tick_for(seconds: u64) -> Duration {
    Duration::from_secs(seconds.max(1))
}

/// The next frame boundary after painting the frame scheduled for `scheduled`
/// at wall-clock `now`. The grid normally advances by exactly one `frame`, so
/// paints hold a fixed cadence regardless of how long a paint took. When the
/// loop has fallen a full frame or more behind — a slow paint or a scheduler
/// hiccup — it snaps onto the boundary one `frame` ahead of `now` rather than
/// replaying every missed boundary, so a backlog can never spiral into a burst
/// of catch-up paints.
fn next_frame_after(scheduled: Instant, now: Instant, frame: Duration) -> Instant {
    let advanced = scheduled + frame;
    if advanced <= now {
        now + frame
    } else {
        advanced
    }
}

fn heartbeat_write_due(last_heartbeat: Option<Instant>) -> bool {
    last_heartbeat.is_none_or(|last| last.elapsed() >= HEARTBEAT_WRITE_INTERVAL)
}

/// Refresh this instance's liveness heartbeat. Written in-process — no `rimz
/// sidebar heartbeat` fork per tick — through the shared liveness helper, which
/// keeps the JSON shape and atomic write identical to what the ledger wakeup
/// fanout and launch freshness gate expect.
fn write_heartbeat(config: &ServeConfig, runtime: &RuntimePaths, socket_path: &Path) -> Result<()> {
    crate::sidebar::write_heartbeat(
        runtime,
        config.workspace_id.clone(),
        &config.instance_id,
        config.mux,
        &config.session_name,
        socket_path,
        config.own_pane.clone(),
    )
    .map_err(|err| SidebarAppErr::Heartbeat(err.to_string()))
}

fn sidebar_socket_path(runtime: &RuntimePaths, instance_id: &SidebarInstanceId) -> PathBuf {
    // Use the short (12-hex) id, not the full `sb_<32 hex>`: the bound path must
    // fit the 108-byte AF_UNIX budget, same as the per-request feed socket. The
    // heartbeat carries this path verbatim, so senders stay in sync.
    runtime
        .sock_dir
        .join(format!("sidebar.{}.sock", instance_id.short()))
}

fn bind_socket(path: &Path) -> io::Result<UnixDatagram> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    UnixDatagram::bind(path)
}

fn emit_terminal_notification(
    config: &ServeConfig,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    snapshot: &SidebarSnapshot,
    prefs: &NotificationsPrefs,
    title: &str,
    body: &str,
    panes: &[PaneId],
) -> io::Result<()> {
    let mut bytes = Vec::new();
    if desktop_notification_targets_renderer(config.mux, snapshot, panes) {
        bytes.extend(osc::desktop_notification_bytes(
            config.mux,
            prefs.desktop,
            title,
            body,
        ));
    }
    if notification_targets_own_view(snapshot, panes) {
        bytes.extend(osc::sound_notification_bytes(prefs.sound));
    }
    if bytes.is_empty() {
        return Ok(());
    }
    let backend = terminal.backend_mut();
    backend.write_all(&bytes)?;
    backend.flush()
}

fn desktop_notification_targets_renderer(
    mux: MuxName,
    snapshot: &SidebarSnapshot,
    panes: &[PaneId],
) -> bool {
    match mux {
        MuxName::Tmux => snapshot.own_view.is_some(),
        MuxName::Zellij => notification_targets_own_view(snapshot, panes),
    }
}

fn notification_targets_own_view(snapshot: &SidebarSnapshot, panes: &[PaneId]) -> bool {
    snapshot.own_view.as_ref().is_some_and(|view| {
        panes
            .iter()
            .any(|pane| view.working_pane_ids.contains(pane))
    })
}

/// How long the resize watcher blocks per poll. A resize event wakes it
/// immediately regardless; this only bounds how often it loops while idle.
const RESIZE_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Watch the terminal for resize and key events and wake the serve loop. Runs
/// on its own thread for the life of the process; it self-wakes by sending to
/// `wake_path` (the loop's bound wakeup socket), which keeps redraw and input
/// on one path. Stops quietly if the event source or socket goes away.
fn spawn_event_waker(wake_path: PathBuf) {
    std::thread::spawn(move || {
        let waker = match UnixDatagram::unbound() {
            Ok(socket) => socket,
            Err(err) => {
                warn!(error = %err, "event waker disabled; input waits for the tick");
                return;
            }
        };
        loop {
            match event::poll(RESIZE_POLL_INTERVAL) {
                Ok(true) => match event::read() {
                    Ok(Event::Resize(_, _)) => {
                        if waker.send_to(b"resize", &wake_path).is_err() {
                            return;
                        }
                    }
                    Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                        if let Some(encoded) = encode_key(key.code)
                            && waker.send_to(encoded.as_bytes(), &wake_path).is_err()
                        {
                            return;
                        }
                    }
                    Ok(Event::Mouse(mouse)) => {
                        if let Some(encoded) = encode_mouse(mouse.kind, mouse.column, mouse.row)
                            && waker.send_to(encoded.as_bytes(), &wake_path).is_err()
                        {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        warn!(error = %err, "event waker stopping: event read failed");
                        return;
                    }
                },
                Ok(false) => {}
                Err(err) => {
                    warn!(error = %err, "event waker stopping: event poll failed");
                    return;
                }
            }
        }
    });
}

/// Apply an input wakeup (key/mouse/resize) to the local UI in place. Input
/// never changes ledger data, so it redraws the *current* snapshot and may jump
/// focus, but it never re-runs the snapshot burst — that per-keystroke refetch
/// was the input lag. Input paints synchronously so a keypress or click feels
/// instant rather than waiting for the next frame; the returned `bool` reports
/// whether it painted, so the serve loop can clear its frame-pending flag.
fn apply_input(
    wakeup: Wakeup,
    ui: &mut UiState,
    health: &mut Health,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    snapshot: &SidebarSnapshot,
    anim_start: Instant,
) -> Result<bool> {
    let outcome = handle_wakeup(wakeup, ui, snapshot);
    if outcome.dismiss {
        health.alert = None;
    }
    if outcome.redraw {
        // Carry the live spin phase into the instant paint so a keypress mid-spin
        // never rewinds the animation to a stale frame.
        ui.animation_phase = wall_clock_phase(anim_start, snapshot.sidebar.resolved_refresh_ms());
        render::draw_to_terminal_with_ui(terminal, snapshot, health.alert.as_ref(), ui)?;
    }
    if let Some(pane) = outcome.focus {
        // A jump fires the one-way focus command at the resolved pane and
        // mutates no UI state. The highlight moves only when the derived
        // baseline catches up on a later fold — late, never wrong.
        spawn_pane_focus(pane);
    }
    Ok(outcome.redraw)
}

fn handle_wakeup(wakeup: Wakeup, ui: &mut UiState, snapshot: &SidebarSnapshot) -> InputOutcome {
    match wakeup {
        Wakeup::Key(action) => handle_key(action, ui, snapshot),
        Wakeup::MouseClick { column, row } => handle_mouse_click(column, row, ui, snapshot),
        Wakeup::Scroll { down } => handle_scroll(down, ui),
        Wakeup::Resize => InputOutcome::redraw(),
        // The serve loop intercepts these before dispatching here: a tick, a
        // typed sidebar event is a re-fetch trigger, worker
        // completions are folded, and a reload re-execs.
        Wakeup::Tick | Wakeup::Event(_) | Wakeup::Reload | Wakeup::Snapshot => {
            InputOutcome::default()
        }
    }
}

/// A workspace-scoped envelope (`session_name: None`) targets every renderer
/// of the workspace; a session-scoped one only the renderers of that session
/// — pane ids are meaningless outside the session that issued them.
fn event_targets_this_renderer(envelope: &SidebarEventEnvelope, config: &ServeConfig) -> bool {
    envelope.workspace_id == config.workspace_id
        && envelope
            .session_name
            .as_deref()
            .is_none_or(|session| session == config.session_name)
}

fn focus_stranded_target(
    snapshot: &SidebarSnapshot,
    ui: &UiState,
    stranded_pane_id: &PaneId,
    own_pane_id: Option<&PaneId>,
    sent_at_ms: u64,
    now_ms: u64,
) -> Option<PaneId> {
    let own_pane_id = own_pane_id?;
    if own_pane_id != stranded_pane_id {
        return None;
    }
    if now_ms.saturating_sub(sent_at_ms) > duration_millis(FOCUS_STRANDED_EVENT_TTL) {
        return None;
    }
    let view = snapshot.own_view.as_ref()?;
    if let Some(baseline) = ui.baseline_pane.as_ref()
        && view.working_pane_ids.contains(baseline)
        && row_index_of_pane(snapshot, None, baseline).is_some()
    {
        return Some(baseline.clone());
    }
    view.working_pane_ids
        .iter()
        .find(|pane| row_index_of_pane(snapshot, None, pane).is_some())
        .cloned()
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

/// Resolve a reload request — the `r` keypress and the typed `Reload` event
/// share this. `Some(target)` means a differing on-disk binary: the caller
/// re-execs onto it and exits the loop. A byte-identical or missing binary
/// skips the re-exec churn but still honours the reload intent with an
/// immediate producing refetch, so a reload always pulls live data and
/// un-sticks a tab whose producer has stalled.
fn reload_or_refetch(
    session_name: &str,
    fetch: &mut FetchDispatcher,
) -> Option<std::path::PathBuf> {
    match reload_action() {
        ReloadAction::Reexec(target) => {
            debug!(
                session = %session_name,
                target = %target.display(),
                "reload: on-disk binary differs; re-execing the renderer in place",
            );
            return Some(target);
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
    None
}

/// Focus the pane on a detached thread so the keypress/click returns instantly:
/// `focus_pane` forks the mux client (`zellij action focus-pane-id` / the tmux
/// equivalent), which must never block the loop. The snapshot-bound pane is
/// focused directly — no `rimz pane focus` child, no per-click `list-panes`
/// re-validation; a pane recycled in the sub-second window since the snapshot
/// self-corrects on the next refresh.
/// Errors are logged, not surfaced — a missed jump is a retriable annoyance,
/// never a reason to block the UI. The command is the whole jump: no local
/// state changes, and the highlight converges on the next data fold (the
/// backstop tick or a ledger wakeup) once the mux reports the new focus.
fn spawn_pane_focus(pane_id: PaneId) {
    std::thread::spawn(move || {
        let backend = crate::mux::backend_for(pane_id.mux());
        if let Err(err) = backend.focus_pane(&pane_id) {
            warn!(pane = %pane_id, error = %err, "sidebar pane focus failed");
        }
    });
}

/// Removes a per-instance runtime file (wakeup socket, heartbeat) when the
/// sidebar exits, so a later `rimz` launch sees an honest "no sidebar here" and
/// rebirths one rather than trusting a stale artifact.
struct RuntimeFileGuard {
    path: PathBuf,
}

impl Drop for RuntimeFileGuard {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                warn!(path = %self.path.display(), error = %err, "sidebar runtime file cleanup failed")
            }
        }
    }
}

#[cfg(test)]
mod tests;
