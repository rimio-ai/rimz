//! Liveness heartbeat refresh and terminal input words on the attach stream.

use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
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
    write_word(stream, word)
}

/// Write one word whole or not at all. A timeout before its first byte drops
/// the word; a timeout after it would leave a fragment the host joins to the
/// next word, so it fails as a stream error instead.
fn write_word(out: &mut impl Write, word: &str) -> io::Result<()> {
    let line = format!("{word}\n");
    let mut written = 0;
    while written < line.len() {
        match out.write(&line.as_bytes()[written..]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => written += count,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) if written > 0 && dropped(&err) => {
                return Err(io::Error::other(format!(
                    "input word torn after {written} of {} bytes: {err}",
                    line.len()
                )));
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// A write that timed out and so dropped its word; forwarding goes on.
fn dropped(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Why a forwarder stopped forwarding before it was asked to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) enum ForwarderEnd {
    /// The terminal's event source failed.
    Input,
    /// The attach stream failed or tore a word.
    Stream,
}

/// The supervisor's terminal input forwarding for one host attachment.
pub(in crate::sidebar_pane) struct EventForwarder {
    stop: Arc<AtomicBool>,
    ended: Arc<OnceLock<ForwarderEnd>>,
    thread: std::thread::JoinHandle<()>,
}

impl EventForwarder {
    pub(in crate::sidebar_pane) fn start(mut stream: UnixStream) -> Self {
        // Settle any resize that raced the fd handover before forwarding input.
        let settled = send_input(&mut stream, "resize");
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let ended = Arc::new(OnceLock::new());
        let end = ended.clone();
        let thread = std::thread::spawn(move || {
            let reason = match settled {
                Err(err) if !dropped(&err) => Some(ForwarderEnd::Stream),
                _ => forward_events(&mut stream, terminal_word, FORWARDER_STOP_POLL, &stopped),
            };
            if let Some(reason) = reason {
                let _ = end.set(reason);
            }
        });
        Self {
            stop,
            ended,
            thread,
        }
    }

    /// Why the forwarder ended on its own, once it has.
    pub(in crate::sidebar_pane) fn ended(&self) -> Option<ForwarderEnd> {
        self.ended.get().copied()
    }

    /// Returns once the thread has left the terminal's event source.
    pub(in crate::sidebar_pane) fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.thread.join();
    }
}

/// Forward until stopped (`None`) or until the terminal or the stream fails.
pub(super) fn forward_events(
    out: &mut impl Write,
    mut next_word: impl FnMut(Duration) -> io::Result<Option<String>>,
    poll: Duration,
    stop: &AtomicBool,
) -> Option<ForwarderEnd> {
    while !stop.load(Ordering::SeqCst) {
        let word = match next_word(poll) {
            Ok(word) => word,
            Err(err) => {
                warn!(error = %err, "event waker stopping: terminal events failed");
                return Some(ForwarderEnd::Input);
            }
        };
        if let Some(word) = word
            && let Err(err) = write_word(out, &word)
            && !dropped(&err)
        {
            return Some(ForwarderEnd::Stream);
        }
    }
    None
}

/// The next input word from the terminal, waiting at most `poll` for an event.
fn terminal_word(poll: Duration) -> io::Result<Option<String>> {
    if !event::poll(poll)? {
        return Ok(None);
    }
    Ok(match event::read()? {
        Event::Resize(_, _) => Some("resize".to_owned()),
        Event::Key(key) if key.kind == KeyEventKind::Press => encode_key(key.code, key.modifiers),
        Event::Mouse(mouse) => encode_mouse(mouse.kind, mouse.column, mouse.row),
        _ => None,
    })
}
