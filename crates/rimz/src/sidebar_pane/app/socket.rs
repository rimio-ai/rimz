//! Liveness heartbeat refresh and terminal input words on the attach stream.

use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
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

/// Publish a changed claim or renew an unchanged one by pathname mtime.
pub(super) fn write_heartbeat(
    config: &ServeConfig,
    runtime: &RuntimePaths,
    socket_path: &Path,
    size: Option<crate::wakeup::heartbeat::SidebarSize>,
    lease: &mut crate::wakeup::heartbeat::HeartbeatLease,
) -> Result<()> {
    let mut heartbeat = crate::wakeup::heartbeat::SidebarHeartbeat::new(
        config.room.workspace_id.clone(),
        config.instance_id.clone(),
        config.room.mux,
        &config.room.session_name,
        socket_path.to_path_buf(),
        Some(config.own_pane.clone()),
    );
    heartbeat.build = crate::build_id::current().map(str::to_owned);
    heartbeat.version = Some(crate::build_id::VERSION.to_owned());
    heartbeat.size = size;
    lease
        .renew(runtime, &heartbeat)
        .map_err(|err| SidebarAppErr::Heartbeat(err.to_string()))
}

/// How often a stoppable forwarder looks for its stop request. Events still
/// wake it at once.
const FORWARDER_STOP_POLL: Duration = Duration::from_millis(250);

pub(super) fn send_input(stream: &mut UnixStream, word: &str) -> io::Result<()> {
    stream.set_write_timeout(Some(FORWARDER_STOP_POLL))?;
    stream.write_all(format!("{word}\n").as_bytes())
}

/// The supervisor's terminal input forwarding for one host attachment.
pub(in crate::sidebar_pane) struct EventForwarder {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl EventForwarder {
    pub(in crate::sidebar_pane) fn start(mut stream: UnixStream) -> Self {
        // Settle any resize that raced the fd handover before forwarding input.
        let _ = send_input(&mut stream, "resize");
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            forward_events(stream, FORWARDER_STOP_POLL, &stopped);
        });
        Self { stop, thread }
    }

    /// Returns once the thread has left the terminal's event source.
    pub(in crate::sidebar_pane) fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.thread.join();
    }
}

fn forward_events(mut stream: UnixStream, poll: Duration, stop: &AtomicBool) {
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
            && let Err(err) = send_input(&mut stream, &encoded)
            && !matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            )
        {
            return;
        }
    }
}
