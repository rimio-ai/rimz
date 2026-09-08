//! Renderer wakeup socket binding, liveness heartbeat refresh, and terminal event forwarding onto the serve loop's wakeup path.

use std::io;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::RuntimePaths;
use crate::sidebar::timing::HEARTBEAT_WRITE_INTERVAL;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use tracing::warn;

use super::input::{encode_key, encode_mouse};
use super::{NavKeymap, Result, ServeConfig, SidebarAppErr};

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
) -> Result<()> {
    crate::wakeup::heartbeat::write_heartbeat(
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

pub(super) fn bind_socket(path: &Path) -> io::Result<UnixDatagram> {
    crate::sock::validate_socket_path(path).map_err(io::Error::other)?;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    UnixDatagram::bind(path)
}

/// How long the resize watcher blocks per poll. A resize event wakes it
/// immediately regardless; this only bounds how often it loops while idle.
pub(super) const RESIZE_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Watch the terminal for resize and key events and wake the serve loop. Runs
/// on its own thread for the life of the process; it self-wakes by sending to
/// `wake_path` (the loop's bound wakeup socket), which keeps redraw and input
/// on one path. Stops quietly if the event source or socket goes away.
pub(super) fn spawn_event_waker(wake_path: PathBuf, keymap: NavKeymap) {
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
                        if let Some(encoded) = encode_key(&keymap, key.code, key.modifiers)
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
