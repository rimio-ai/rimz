//! Runtime loop for the native sidebar process.
//!
//! `attachment` owns one pane's fixed-timestep loop, `plane` the data plane every pane of the host shares, and `loop_state` renderer transitions and loop-lifetime context. This module keeps their shared configuration, error type, panic diagnostics, and runtime-file cleanup.

use std::cell::Cell;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::config::NotificationsPrefs;
use crate::disk::paths::PathErr;
use crate::{MuxName, SidebarInstanceId, WorkspaceId};
use tracing::warn;

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

/// The fixture/gallery terminal as crossterm finds it: the controlling tty.
#[cfg(feature = "testkit")]
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
