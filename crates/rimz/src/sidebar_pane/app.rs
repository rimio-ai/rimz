//! Runtime loop for the native sidebar process.
//!
//! `serve` owns the fixed-timestep event loop and the wiring; each concern the
//! loop folds lives in its own submodule — [`fetch`] (the two-speed off-thread
//! fetch cycle), [`state`] (the pure `compute_next_state` reducer and the fold
//! integrator), [`gate`] (the last-known-good regression hold), [`health`]
//! (failure debounce and give-up), [`lifecycle`] (self-close and the bounded
//! resize-grow paint hold), [`reload`] (binary resolution and re-exec), and
//! [`selection`] (the identity-keyed highlight and input handlers).

use std::cell::Cell;
use std::io::{self, Write};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::{Duration, Instant};

use crate::config::{NotificationsPrefs, PetsSize};
use crate::ids::PaneId;
use crate::ledger::paths::PathErr;
use crate::schema::sidebar_event::{SidebarEvent, SidebarEventEnvelope};
use crate::sidebar::events::EventStore;
use crate::sidebar::fuse::fuse;
use crate::sidebar::observe::{self, ObserveMsg};
use crate::sidebar::read_marks::ReadMarkStore;
use crate::sidebar::timing::{FOCUS_STRANDED_EVENT_TTL, HEARTBEAT_WRITE_INTERVAL};
use crate::sidebar_pane::osc;
use crate::sidebar_pane::pets::{PetAssets, PetGridSize};
use crate::{MuxName, RuntimePaths, SidebarInstanceId, SidebarSnapshot, WorkspaceId};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use tracing::{debug, warn};

use crate::sidebar_pane::render::{self, UiState};
use crate::tui::{MouseCapture, TerminalModeGuard};

mod fetch;
#[cfg(test)]
mod fixtures;
mod gate;
mod health;
mod input;
mod lifecycle;
mod loop_state;
mod notify;
mod reload;
mod remind;
mod selection;
mod socket;
mod state;
mod timing;
mod tmux_watch;
mod transcript_watch;

use self::loop_state::{LoopFlow, LoopState};
use self::{notify::*, socket::*, timing::*};
use fetch::{FetchDispatcher, FetchOutcome, FetchRequest, spawn_fetch_worker};
use gate::GateState;
use input::{Wakeup, encode_key, encode_mouse, wait_for_wakeup};
use lifecycle::{PaintHold, SELF_CLOSE_WATCHDOG, SelfCloseState, resize_grew};
use reload::{ReloadAction, reexec_self, reload_action};
use selection::{InputOutcome, handle_key, handle_mouse_click, handle_scroll, row_index_of_pane};
use state::{apply_fetch_outcome, placeholder_snapshot};

pub use health::Health;
pub use state::{RenderState, compute_next_state};

const SIDEBAR_TERMINAL_TITLE: &str = "rimz-sidebar";
const PET_FALLBACK_TERMINAL_SIZE: (u16, u16) = (54, 34);

thread_local! {
    static PRODUCE_PANIC_DIAGNOSTIC_SUPPRESSED: Cell<bool> = const { Cell::new(false) };
}

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
    crate::build_id::warm();
    set_terminal_title()?;
    let runtime = RuntimePaths::for_workspace(config.workspace_id.clone())?;
    runtime.ensure_dirs()?;
    let diag = crate::diag::DiagSink::for_workspace(
        config.workspace_id.clone(),
        config.session_name.clone(),
        Some(config.instance_id.clone()),
    );
    install_panic_diagnostic_hook(diag.clone());
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

    let initial_width = terminal.size().map(|s| s.width).ok();
    // Without a diagnostics sink there is nowhere to record anomalies, so the
    // receiver drops here and the loop's sends simply count as dropped.
    let (observe_tx, observe_rx) = std::sync::mpsc::sync_channel::<ObserveMsg>(64);
    let _observe_handle = diag.clone().map(|sink| {
        observe::writer::spawn(
            runtime.clone(),
            sink,
            config.instance_id.clone(),
            observe_rx,
        )
    });
    let read_marks = ReadMarkStore::new(runtime.clone(), config.instance_id.clone());
    let mut state = LoopState::new(
        config.workspace_id.clone(),
        initial_width,
        observe_tx,
        read_marks,
    );
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
        diag.clone(),
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
    while !state.should_exit {
        let (active, timeout) = state.frame_timing(tick, anim_start);
        socket.set_read_timeout(Some(timeout))?;
        match wait_for_wakeup(&socket)? {
            Wakeup::Snapshot => {
                state.on_snapshot(&config, &mut fetch, &result_rx, anim_start, diag.as_ref())?;
            }
            Wakeup::Event(envelope) => {
                match state.on_event(
                    &config,
                    &mut fetch,
                    &mut terminal,
                    envelope,
                    anim_start,
                    diag.as_ref(),
                )? {
                    LoopFlow::Continue => {}
                    LoopFlow::Repoll => continue,
                    LoopFlow::Exit => break,
                }
            }
            // A recv timeout: the active grid reached a frame boundary, or the
            // idle backstop interval elapsed. It carries no state of its own —
            // the frame phase below advances the spin and paints, and the
            // backstop poll runs there too.
            Wakeup::Tick => {}
            Wakeup::Resize => {
                state.on_resize(&mut fetch, &mut terminal, anim_start)?;
            }
            // The `r` keypress rides the local `reload` control word; an
            // external `rimz reload` arrives as the typed event. Both resolve
            // through the same helper.
            Wakeup::Reload => {
                if let Some(target) = reload_or_refetch(&config.session_name, &mut fetch) {
                    state.reexec_to = Some(target);
                    break;
                }
            }
            wakeup => {
                state.on_input(
                    &config,
                    wakeup,
                    &mut terminal,
                    &mut fetch,
                    anim_start,
                    diag.as_ref(),
                )?;
            }
        }

        state.run_maintenance(&config, &runtime, &socket_path, &mut fetch, tick);
        state.maybe_remind(&config, &mut terminal, diag.as_ref());
        state.paint_frame_if_due(&mut terminal, anim_start, active)?;
    }
    if state.tab_emptied {
        close_self_closing_view_floating_panes(&config);
    }
    if let Some(target) = state.reexec_to {
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

fn install_panic_diagnostic_hook(diag: Option<crate::diag::DiagSink>) {
    let Some(diag) = diag else {
        return;
    };
    let prior = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if produce_panic_diagnostic_suppressed() {
            prior(info);
            return;
        }
        diag.emit_unlimited(crate::schema::diag::DiagEvent::RendererPanic {
            message: panic_message(info),
            backtrace: Some(std::backtrace::Backtrace::force_capture().to_string()),
        });
        prior(info);
    }));
}

fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(message) = info.payload().downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.clone()
    } else {
        "renderer panicked".to_owned()
    }
}

fn with_produce_panic_diagnostic_suppressed<T>(f: impl FnOnce() -> T) -> T {
    struct Reset(bool);

    impl Drop for Reset {
        fn drop(&mut self) {
            PRODUCE_PANIC_DIAGNOSTIC_SUPPRESSED.with(|suppressed| suppressed.set(self.0));
        }
    }

    let previous = PRODUCE_PANIC_DIAGNOSTIC_SUPPRESSED.with(|suppressed| {
        let previous = suppressed.get();
        suppressed.set(true);
        previous
    });
    let _reset = Reset(previous);
    f()
}

fn produce_panic_diagnostic_suppressed() -> bool {
    PRODUCE_PANIC_DIAGNOSTIC_SUPPRESSED.with(Cell::get)
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

/// Apply an input wakeup (key/mouse/resize) to the local UI in place. Input
/// never changes ledger data, so it redraws the *current* snapshot and may jump
/// focus, but it never re-runs the snapshot burst — that per-keystroke refetch
/// was the input lag. Input paints synchronously so a keypress or click feels
/// instant rather than waiting for the next frame; the returned `bool` reports
/// whether it painted, so the serve loop can clear its frame-pending flag.
fn apply_input(
    wakeup: Wakeup,
    ui: &mut UiState,
    pet_assets: &mut PetAssets,
    health: &mut Health,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    snapshot: &SidebarSnapshot,
    anim_start: Instant,
) -> Result<InputApply> {
    let outcome = handle_wakeup(wakeup, ui, snapshot);
    if outcome.dismiss {
        health.alert = None;
    }
    if outcome.redraw {
        // Carry the live spin phase into the instant paint so a keypress mid-spin
        // never rewinds the animation to a stale frame.
        ui.animation_phase = wall_clock_phase(anim_start, snapshot.sidebar.resolved_refresh_ms());
        refresh_pet_view(
            ui,
            pet_assets,
            snapshot,
            health.alert.as_ref().is_some_and(render::Alert::is_active),
            terminal.size().ok().map(|size| (size.width, size.height)),
        );
        render::draw_to_terminal_with_ui(terminal, snapshot, health.alert.as_ref(), ui)?;
    }
    if let Some(pane) = &outcome.focus {
        // A jump fires the one-way focus command at the resolved pane. The
        // highlight moves only when the derived baseline catches up on a later
        // fold — late, never wrong — and any make-up filter clears as focus
        // leaves the tab (the redraw above repaints the reshaped body).
        spawn_pane_focus(pane.clone());
    }
    Ok(InputApply {
        painted: outcome.redraw,
        focused: outcome.focus,
        // The `m`/`M` keys name a row to mark read/unread; the loop does the
        // durable write and repaint (`on_input`), which owns the runtime paths.
        mark_read: outcome.mark_read,
        mark_unread: outcome.mark_unread,
    })
}

fn refresh_pet_view(
    ui: &mut UiState,
    pet_assets: &mut PetAssets,
    snapshot: &SidebarSnapshot,
    alert_active: bool,
    terminal_size: Option<(u16, u16)>,
) {
    let (width, height) = terminal_size.unwrap_or(PET_FALLBACK_TERMINAL_SIZE);
    let action = render::selected_pet_action(snapshot, ui);
    let size = if snapshot.sidebar.pets.enabled
        && render::dashboard_present(snapshot, alert_active)
        && render::pet_body_enabled(snapshot)
    {
        let inner = width.saturating_sub(2);
        match snapshot.sidebar.pets.size {
            PetsSize::Medium => PetGridSize::for_dashboard_column(inner, height),
            PetsSize::Small => match render::active_dashboard_block_rows(snapshot, ui) {
                Some(rows) => PetGridSize::for_dashboard_block(rows, inner, height),
                None => PetGridSize::for_standalone_dashboard(inner, height),
            },
        }
    } else {
        None
    };
    let unread_triggered = if snapshot.sidebar.pets.enabled {
        pet_assets.observe_unread_rows(render::unread_pet_row_ids(snapshot))
    } else {
        false
    };
    ui.pet = pet_assets.view(
        &snapshot.sidebar.pets,
        action,
        ui.animation_phase,
        snapshot.sidebar.resolved_refresh_ms(),
        size,
        render::pet_motion_enabled(snapshot, action),
        unread_triggered,
    );
}

struct InputApply {
    painted: bool,
    focused: Option<PaneId>,
    mark_read: Option<String>,
    mark_unread: Option<String>,
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

#[cfg(test)]
pub(crate) static PANIC_HOOK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// Removes a per-instance runtime file (wakeup socket, heartbeat)
/// when the sidebar exits, so a later `rimz` launch sees an honest "no sidebar
/// here" and rebirths one rather than trusting a stale artifact.
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
