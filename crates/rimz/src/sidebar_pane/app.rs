//! Runtime loop for the native sidebar process.
//!
//! `serve` owns the fixed-timestep event loop shell and wiring; [`loop_state`]
//! dispatches wakeups, and each concern the loop folds lives in its own
//! submodule — [`fetch`] (the two-speed off-thread fetch cycle), [`state`]
//! (fetch-state and unread-fold reducers), [`gate`] (the last-known-good
//! regression hold), [`health`]
//! (failure debounce and give-up), [`lifecycle`] (self-close and the bounded
//! resize-grow paint hold), [`order_hold`] (renderer-local row/group order
//! freeze), [`reload`] (binary-change detection), and [`selection`] (the
//! identity-keyed highlight and input handlers).

use std::cell::Cell;
use std::io::{self, Write};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::{Duration, Instant};

use crate::config::NotificationsPrefs;
use crate::diag::record::DiagEvent;
use crate::disk::paths::PathErr;
use crate::ids::PaneId;
use crate::mux::focus_anchor::{
    FocusObservation, FocusObservationOutcome, FocusOrigin, FocusPresentation,
};
use crate::sidebar::event_store::EventStore;
use crate::sidebar::fuse::{focus_intent_confirmed_from, fuse, fuse_owned};
use crate::sidebar::observe::{self, ObserveMsg};
use crate::sidebar::timing::{FOCUS_STRANDED_EVENT_TTL, HEARTBEAT_WRITE_INTERVAL, TAB_READ_DWELL};
use crate::sidebar_pane::pixel::probe::escalate_own_pane_passthrough;
use crate::sidebar_pane::pixel::{PixelRenderCaps, detect_pixel_render_caps};
use crate::store::snapshot::SidebarSnapshot;
use crate::wakeup::events::{SidebarEvent, SidebarEventEnvelope};
use crate::{MuxName, RuntimePaths, SidebarInstanceId, WorkspaceId};
use ratatui::Terminal;
use ratatui::backend::{ClearType, CrosstermBackend};
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use tracing::{debug, warn};

use crate::sidebar_pane::render::{self, UiState};
use crate::tui::{MouseCapture, Screen, TerminalModeGuard};

mod cache_refresh;
mod demo;
mod fetch;
#[cfg(test)]
mod fixtures;
mod gate;
mod health;
mod input;
mod keymap;
mod lifecycle;
mod loop_state;
mod notify;
mod order_hold;
mod paint;
mod reload;
mod remind;
mod selection;
mod socket;
mod state;
mod timing;
mod tmux_watch;
mod transcript_watch;
mod width_control;

#[cfg(test)]
use self::loop_state::handle_wakeup;
use self::loop_state::{LoopFlow, LoopState};
use self::notify::*;
use self::socket::*;
use self::timing::*;
use fetch::{FetchDispatcher, FetchRequest, FetchUpdate, spawn_fetch_worker};
use gate::GateState;
use input::{Wakeup, encode_key, encode_mouse, wait_for_wakeup};
use lifecycle::{PaintHold, SELF_CLOSE_WATCHDOG, SelfCloseState, resize_grew};
use selection::{
    InputEffect, InputOutcome, handle_key, handle_mouse_click, handle_scroll, row_index_of_pane,
};

pub use demo::{serve_fixture, serve_gallery};
pub use keymap::NavKeymap;

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
    pub timezone: jiff::tz::TimeZone,
    pub notification_prefs: NotificationsPrefs,
    pub nav_keys: NavKeymap,
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
    #[error("heartbeat write failed: {0}")]
    Heartbeat(String),
}

pub type Result<T> = std::result::Result<T, SidebarAppErr>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeOutcome {
    Stopped,
    SelfCloseRequested,
}

fn initial_pet_render_caps(mux: MuxName, session_name: &str) -> PixelRenderCaps {
    detect_pixel_render_caps(mux, session_name, PixelRenderCaps::default())
}

pub fn serve(config: ServeConfig) -> Result<ServeOutcome> {
    crate::build_id::warm();
    reap_inherited_zombies();
    set_terminal_title()?;
    let runtime = RuntimePaths::for_workspace(config.workspace_id.clone())?;
    runtime.ensure_dirs()?;
    let election =
        crate::sidebar::ProducerElectionTracker::new(runtime.clone(), config.instance_id.clone());
    let diag = crate::diag::DiagSink::for_workspace(
        config.workspace_id.clone(),
        config.session_name.clone(),
        Some(config.instance_id.clone()),
    );
    install_panic_diagnostic_hook(diag.clone());
    let socket_path = runtime.sidebar_socket_path(&config.instance_id);
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
    // Redraw the instant the pane is resized — most importantly when a user
    // attaches to a background session and Zellij sizes the pane for the first
    // time. The watcher nudges this loop through the same wakeup socket the
    // store uses, so a resize is just another wakeup; without it the first
    // usable frame waits for the next `tick`, reading as a blank sidebar.
    let _input_mode = TerminalModeGuard::enable(MouseCapture::Stdout, Screen::Main)?;
    let pet_render_caps = initial_pet_render_caps(config.mux, &config.session_name);
    if config.mux == MuxName::Tmux
        && let Err(err) = escalate_own_pane_passthrough()
    {
        warn!(
            session = %config.session_name,
            error = %err,
            "sidebar pane passthrough escalation failed",
        );
    }
    spawn_event_waker(socket_path.clone(), config.nav_keys.clone());
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    ratatui::backend::Backend::clear_region(terminal.backend_mut(), ClearType::All)?;
    terminal.swap_buffers();

    let initial_width = terminal.size().map(|s| s.width).ok();
    // Without a diagnostics sink there is nowhere to record anomalies, so the
    // receiver drops here and the loop's sends simply count as dropped.
    let (observe_tx, observe_rx) = std::sync::mpsc::sync_channel::<ObserveMsg>(64);
    let _observe_handle = diag.is_enabled().then(|| {
        observe::writer::spawn(runtime.clone(), diag.clone(), election.clone(), observe_rx)
    });
    let (request_tx, request_rx) = std::sync::mpsc::channel::<FetchRequest>();
    let (result_tx, result_rx) = std::sync::mpsc::channel::<FetchUpdate>();
    let mut state = LoopState::new(
        config.clone(),
        runtime.clone(),
        socket_path.clone(),
        diag.clone(),
        result_rx,
        initial_width,
        observe_tx,
        pet_render_caps,
    );
    // Zellij's percentage template needs a startup trim on capped wide views.
    // tmux births through its live absolute-column hook; its resize wakeups
    // own later convergence, avoiding a startup resize that can reflow the
    // sidebar's scrollback before the self-close paint hold is armed.
    if initial_width.is_some() && config.mux == MuxName::Zellij {
        state.run_width_control(
            &mut terminal,
            crate::diag::record::SidebarWidthControlTrigger::Retarget,
        );
    }

    // The snapshot fetch (fast in-process fold plus optional produce) runs on a
    // background worker, so animation and input never block on it. The worker
    // posts `SNAPSHOT_WAKEUP` when a result is ready; the frame/tick path also
    // drains the result channel so that wakeup stays a latency hint. The
    // dispatcher coalesces requests so a store-delta storm or a slow produce
    // can never queue more than one extra run.
    // `JoinHandle` drops without blocking: the thread runs to completion on its
    // own when `request_tx` is dropped at function exit.
    let _fetch_handle = spawn_fetch_worker(
        config.clone(),
        runtime.clone(),
        diag.clone(),
        election.clone(),
        request_rx,
        (result_tx, socket_path.clone()),
    );
    let _cache_refresh_handle = cache_refresh::spawn(
        config.clone(),
        runtime.clone(),
        diag.clone(),
        election.clone(),
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
            config.session_name.clone(),
            election.clone(),
        );
    }

    // Codex rollout fast path: the elected producer watches each live root
    // Codex session's transcript file and runs the stat-gated sidecar refresh
    // on the write, so mid-turn token/cost updates repaint without waiting for
    // the next hook push or producer tick. Latency only — the tick backstop
    // stays truth. Backend-independent; the elder gate inside scopes the work.
    let _ = transcript_watch::spawn(runtime.clone(), election);

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
        let (active, mut timeout) = state.frame_timing();
        timeout = fetch_deadline_timeout(timeout, fetch.next_deadline(), Instant::now());
        socket.set_read_timeout(Some(timeout))?;
        match state.on_wakeup(&mut fetch, &mut terminal, wait_for_wakeup(&socket)?)? {
            LoopFlow::Continue => {}
            LoopFlow::Repoll => continue,
            LoopFlow::Exit => break,
        }

        state.run_maintenance(&mut fetch);
        state.run_width_control_backstop(&mut terminal);
        state.maybe_remind(&mut terminal);
        state.paint_frame_if_due(&mut terminal, active)?;
    }
    if !state.reload_requested
        && !state.tab_emptied
        && let Some(cause) = state.exit_cause
    {
        diag.emit_unlimited(DiagEvent::RendererExit { cause });
    }
    state.clear_pixel(&mut terminal);
    if state.exit_cause == Some(crate::diag::record::RendererExitCause::DegradedGaveUp) {
        drop(_socket_cleanup);
        drop(_heartbeat_cleanup);
        std::process::exit(crate::sidebar_pane::supervise::RESPAWN_EXIT_CODE);
    }
    if state.reload_requested {
        // Keep raw mode and mouse capture alive across the supervisor re-exec:
        // the replacement worker re-enables the same modes, and disabling them
        // here opens a reporting gap that outer terminals can turn into arrow
        // keys sent to the active pane. Runtime files still release explicitly
        // because `process::exit` never runs RAII drops.
        _input_mode.preserve_for_reexec();
        drop(_socket_cleanup);
        drop(_heartbeat_cleanup);
        std::process::exit(crate::sidebar_pane::supervise::RELOAD_EXIT_CODE);
    }
    if state.tab_emptied {
        // A cache-backed empty fold is only a request. Keep terminal modes
        // continuous while the supervisor checks mux truth; it restores them
        // if the authoritative verdict really closes the pane, or the
        // replacement worker reasserts them after a rejected request.
        _input_mode.preserve_for_reexec();
        return Ok(ServeOutcome::SelfCloseRequested);
    }
    Ok(ServeOutcome::Stopped)
}

fn fetch_deadline_timeout(base: Duration, deadline: Option<Instant>, now: Instant) -> Duration {
    deadline.map_or(base, |deadline| {
        base.min(
            deadline
                .saturating_duration_since(now)
                .max(FRAME_MIN_TIMEOUT),
        )
    })
}

fn install_panic_diagnostic_hook(diag: crate::diag::DiagSink) {
    let prior = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if produce_panic_diagnostic_suppressed() {
            prior(info);
            return;
        }
        diag.emit_unlimited(crate::diag::record::DiagEvent::RendererPanic {
            message: panic_payload_message(info.payload(), "renderer panicked"),
            backtrace: Some(std::backtrace::Backtrace::force_capture().to_string()),
        });
        prior(info);
    }));
}

#[cfg(unix)]
fn reap_inherited_zombies() {
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;

    // A reload re-exec can orphan an in-flight Codex app-server child by
    // replacing the process image while its `Child` handle exists. At serve
    // startup no Rust-owned child handles exist yet, so a non-blocking
    // waitpid(-1) drain cannot steal another component's child status.
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => break,
            Ok(WaitStatus::Exited(pid, status)) => {
                debug!(pid = pid.as_raw(), status, "reaped inherited zombie child");
            }
            Ok(WaitStatus::Signaled(pid, signal, _)) => {
                debug!(
                    pid = pid.as_raw(),
                    signal = ?signal,
                    "reaped inherited zombie child"
                );
            }
            Ok(status) => {
                debug!(status = ?status, "reaped inherited child status");
            }
            Err(nix::errno::Errno::ECHILD) => break,
            Err(err) => {
                debug!(error = %err, "inherited zombie reap failed");
                break;
            }
        }
    }
}

#[cfg(not(unix))]
fn reap_inherited_zombies() {}

fn panic_payload_message(payload: &(dyn std::any::Any + Send), fallback: &str) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        fallback.to_owned()
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

fn set_terminal_title() -> io::Result<()> {
    let mut stdout = io::stdout();
    write!(stdout, "\x1b]2;{}\x07", crate::pane::SIDEBAR_CHROME_TITLE)?;
    stdout.flush()
}

#[cfg(test)]
pub(crate) static PANIC_HOOK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
