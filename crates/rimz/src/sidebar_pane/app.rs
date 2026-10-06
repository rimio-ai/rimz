//! Runtime loop for the native sidebar process.
//!
//! `serve` owns the fallback worker's process shell; `attachment` owns one pane's fixed-timestep loop, `plane` the data plane every pane of a process shares, and `loop_state` renderer transitions and loop-lifetime context. Its collaborators own the fetch cycle, state and unread folds, regression gate, health debounce, lifecycle latches, order holds, reload decisions, and selection.

use std::cell::Cell;
use std::io;
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::config::NotificationsPrefs;
use crate::disk::paths::PathErr;
use crate::{MuxName, RuntimePaths, SidebarInstanceId, WorkspaceId};
use tracing::{debug, warn};

use crate::tui::{MouseCapture, Screen, TerminalModeGuard};

mod attachment;
mod backend;
mod cache_refresh;
#[cfg(feature = "testkit")]
mod demo;
mod fetch;
#[cfg(test)]
pub(super) mod fixtures;
mod gate;
mod health;
mod input;
mod keymap;
mod lifecycle;
mod loop_state;
mod notify;
mod order_hold;
mod paint;
mod plane;
mod reload;
mod remind;
mod selection;
mod socket;
mod state;
mod timing;
mod tmux_watch;
mod transcript_watch;
mod width_control;

use self::socket::spawn_event_waker;
use self::timing::FRAME_MIN_TIMEOUT;

pub(super) use self::attachment::{Attachment, AttachmentExit, CloseHandle};
pub(super) use self::backend::PaneBackend;
#[cfg(test)]
pub(super) use self::backend::tests::Pty;
pub(super) use self::plane::DataPlane;
pub(super) use self::socket::EventForwarder;

#[cfg(feature = "testkit")]
pub use demo::{serve_fixture, serve_gallery};
pub use keymap::NavKeymap;

/// Send a left-button press through the renderer's ordinary input wakeup path.
#[cfg(feature = "testkit")]
pub fn send_click(wakeup_socket: &std::path::Path, column: u16, row: u16) -> io::Result<()> {
    let wire = input::encode_click(column, row);
    std::os::unix::net::UnixDatagram::unbound()?.send_to(wire.as_bytes(), wakeup_socket)?;
    Ok(())
}

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

pub(super) type Result<T> = std::result::Result<T, SidebarAppErr>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeOutcome {
    Stopped,
    SelfCloseRequested,
}

/// The worker's terminal as crossterm finds it: the controlling tty, so a
/// worker whose stdout goes elsewhere still sizes from the pane it runs in.
fn ambient_window() -> io::Result<ratatui::backend::WindowSize> {
    ratatui::crossterm::terminal::window_size()
        .map(|window| ratatui::backend::WindowSize {
            columns_rows: ratatui::layout::Size::new(window.columns, window.rows),
            pixels: ratatui::layout::Size::new(window.width, window.height),
        })
        .or_else(|_| {
            let (columns, rows) = ratatui::crossterm::terminal::size()?;
            Ok(ratatui::backend::WindowSize {
                columns_rows: ratatui::layout::Size::new(columns, rows),
                pixels: ratatui::layout::Size::default(),
            })
        })
}

/// The fallback worker: one attachment over this process's own stdout, with
/// its own data plane. It holds the pane's terminal modes and reads its input,
/// which a pane painted by the room host leaves to the supervisor.
pub(super) fn serve(config: ServeConfig) -> Result<ServeOutcome> {
    crate::build_id::warm();
    reap_inherited_zombies();
    let runtime = RuntimePaths::for_workspace(config.workspace_id.clone())?;
    runtime.ensure_dirs()?;
    let diag = crate::diag::DiagSink::for_workspace(
        config.workspace_id.clone(),
        config.session_name.clone(),
        Some(config.instance_id.clone()),
    );
    install_panic_diagnostic_hook(diag.clone());
    let attachment = Attachment::open(config.clone(), &runtime, diag.clone())?;
    // Redraw the instant the pane is resized — most importantly when a user
    // attaches to a background session and Zellij sizes the pane for the first
    // time. The watcher nudges the attachment through the same wakeup socket
    // the store uses, so a resize is just another wakeup; without it the first
    // usable frame waits for the next `tick`, reading as a blank sidebar.
    let input_mode = TerminalModeGuard::enable(MouseCapture::Stdout, Screen::Main)?;
    spawn_event_waker(
        attachment.socket_path().to_path_buf(),
        config.nav_keys.clone(),
    );
    let backend = PaneBackend::ambient(io::stdout().as_fd().try_clone_to_owned()?, ambient_window)?;
    let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGTERM])?;
    let signal_stop = signals.handle();
    let close = attachment.close_handle();
    let signal_thread = std::thread::spawn(move || {
        if signals.forever().next().is_some() {
            close.close();
        }
    });
    let plane = DataPlane::start(&config, &runtime, &diag);
    let outcome = attachment.run(&plane, backend);
    signal_stop.close();
    let _ = signal_thread.join();
    // `process::exit` never runs RAII drops, so every arm below relies on
    // `run` having released the attachment's runtime files before it returned.
    match outcome? {
        AttachmentExit::GaveUp => {
            std::process::exit(crate::sidebar_pane::supervise::RESPAWN_EXIT_CODE)
        }
        AttachmentExit::Reload => {
            // Keep raw mode and mouse capture alive across the supervisor re-exec:
            // the replacement worker re-enables the same modes, and disabling them
            // here opens a reporting gap that outer terminals can turn into arrow
            // keys sent to the active pane.
            input_mode.preserve_for_reexec();
            std::process::exit(crate::sidebar_pane::supervise::RELOAD_EXIT_CODE)
        }
        AttachmentExit::SelfClose => {
            // A cache-backed empty fold is only a request. Keep terminal modes
            // continuous while the supervisor checks mux truth; it restores them
            // if the authoritative verdict really closes the pane, or the
            // replacement worker reasserts them after a rejected request.
            input_mode.preserve_for_reexec();
            Ok(ServeOutcome::SelfCloseRequested)
        }
        AttachmentExit::Closed => Ok(ServeOutcome::Stopped),
    }
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

pub(super) fn install_panic_diagnostic_hook(diag: crate::diag::DiagSink) {
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
