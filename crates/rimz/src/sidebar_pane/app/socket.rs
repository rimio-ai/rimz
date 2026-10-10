//! Renderer wakeup socket binding, liveness heartbeat refresh, and terminal event forwarding onto the serve loop's wakeup path.

use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::RuntimePaths;
use crate::sidebar::timing::HEARTBEAT_WRITE_INTERVAL;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use tracing::warn;

use super::input::{encode_key, encode_mouse};
use super::{Result, ServeConfig, SidebarAppErr};

pub(super) fn heartbeat_write_due(last_heartbeat: Option<Instant>) -> bool {
    last_heartbeat.is_none_or(|last| last.elapsed() >= HEARTBEAT_WRITE_INTERVAL)
}

/// Refresh this instance's liveness heartbeat. Written in-process — no `rimz
/// sidebar heartbeat` fork per tick — through the shared liveness helper, which
/// keeps the JSON shape and atomic write identical to what the store wakeup
/// fanout and launch freshness gate expect.
pub(super) fn write_heartbeat(
    config: &ServeConfig,
    runtime: &RuntimePaths,
    socket_path: &Path,
    size: Option<crate::wakeup::heartbeat::SidebarSize>,
) -> Result<()> {
    crate::wakeup::heartbeat::write_heartbeat(
        runtime,
        config.workspace_id.clone(),
        &config.instance_id,
        config.mux,
        &config.session_name,
        socket_path,
        config.own_pane.clone(),
        size,
    )
    .map_err(|err| SidebarAppErr::Heartbeat(err.to_string()))
}

pub(super) fn bind_socket(path: &Path) -> io::Result<UnixDatagram> {
    crate::sock::validate_socket_path(path).map_err(io::Error::other)?;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    UnixDatagram::bind(path)
}

/// How often a stoppable forwarder looks for its stop request. Events still
/// wake it at once.
const FORWARDER_STOP_POLL: Duration = Duration::from_millis(250);

/// The supervisor's terminal input forwarding for one host attachment.
pub(in crate::sidebar_pane) struct EventForwarder {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl EventForwarder {
    pub(in crate::sidebar_pane) fn start(wake_path: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            forward_events(&wake_path, FORWARDER_STOP_POLL, &stopped);
        });
        Self { stop, thread }
    }

    /// Returns once the thread has left the terminal's event source.
    pub(in crate::sidebar_pane) fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.thread.join();
    }
}

fn forward_events(wake_path: &Path, poll: Duration, stop: &AtomicBool) {
    let waker = match forwarding_socket() {
        Ok(socket) => socket,
        Err(err) => {
            warn!(error = %err, "event waker disabled; input waits for the tick");
            return;
        }
    };
    while !stop.load(Ordering::SeqCst) {
        let encoded = match event::poll(poll) {
            Ok(true) => match event::read() {
                Ok(Event::Resize(_, _)) => Some("resize".to_owned()),
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    encode_key(key.code, key.modifiers)
                }
                Ok(Event::Mouse(mouse)) => encode_mouse(mouse.kind, mouse.column, mouse.row),
                Ok(_) => None,
                Err(err) => {
                    warn!(error = %err, "event waker stopping: event read failed");
                    return;
                }
            },
            Ok(false) => None,
            Err(err) => {
                warn!(error = %err, "event waker stopping: event poll failed");
                return;
            }
        };
        if let Some(encoded) = encoded
            && !forward(&waker, encoded.as_bytes(), wake_path, stop)
        {
            return;
        }
    }
}

/// The forwarder's sending end. A send into a full inbox gives up after
/// [`FORWARDER_STOP_POLL`], so a renderer that stopped reading cannot hold the
/// forwarder past its stop.
pub(super) fn forwarding_socket() -> io::Result<UnixDatagram> {
    let socket = UnixDatagram::unbound()?;
    socket.set_write_timeout(Some(FORWARDER_STOP_POLL))?;
    Ok(socket)
}

/// Send one word, waiting for room in the inbox until `stop`. `false` when the
/// word cannot be sent: the inbox is gone, or the forwarder was stopped.
pub(super) fn forward(
    waker: &UnixDatagram,
    word: &[u8],
    wake_path: &Path,
    stop: &AtomicBool,
) -> bool {
    loop {
        match waker.send_to(word, wake_path) {
            Ok(_) => return true,
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) && !stop.load(Ordering::SeqCst) => {}
            Err(_) => return false,
        }
    }
}
