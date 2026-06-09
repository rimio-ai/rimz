//! JSON-RPC transport for the Codex app-server.
//!
//! This module owns binary/home resolution, process/socket framing, request round trips, notifications, and the shared frame helpers used by the broker and read-only app-server client.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Override for the `codex` binary path (tests/tooling point this at a stub).
const CODEX_BIN_ENV: &str = "RIMZ_CODEX_BIN";

#[derive(Debug, thiserror::Error)]
pub(crate) enum AppServerErr {
    #[error("spawning codex app-server: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("codex app-server io: {0}")]
    Io(#[source] std::io::Error),
    #[error("codex app-server timed out")]
    Timeout,
    #[error("codex app-server protocol error: {0}")]
    Protocol(String),
    #[error("codex app-server returned error {code}: {message}")]
    JsonRpc { code: i64, message: String },
    #[error("codex app-server stream closed before responding")]
    Closed,
}

/// One JSON-RPC round-trip surface. The production impl spawns `codex
/// app-server`; tests feed canned responses so the mapping is exercised without
/// a process.
pub(crate) trait JsonRpcTransport {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, AppServerErr>;
    fn notify(&mut self, method: &str, params: Value) -> Result<(), AppServerErr>;
}

/// Resolve the `codex` binary: explicit override, then `PATH`, then the bare
/// name (which `Command` resolves against `PATH` at spawn). Shared with the
/// broker ([`crate::agents::codex::broker`]) so both resolve the same binary.
pub(crate) fn codex_bin() -> PathBuf {
    if let Some(raw) = std::env::var_os(CODEX_BIN_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(raw);
    }
    which::which("codex").unwrap_or_else(|_| PathBuf::from("codex"))
}

/// Codex's home directory: `CODEX_HOME` when set, else `~/.codex`. Mirrors the
/// resolution Codex itself uses, so the control socket — and the managed
/// standalone install [`crate::remote_control::codex_standalone_bin`] looks for —
/// are found where Codex places them.
pub(crate) fn codex_home() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(raw));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".codex"))
}

/// Spawn a thread draining newline-framed lines from `reader` into a channel, so
/// a request can wait with its remaining deadline. Shared by both transports and
/// the broker ([`crate::agents::codex::broker`]).
pub(crate) fn spawn_frame_reader<R: BufRead + Send + 'static>(reader: R) -> Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(std::mem::take(&mut line)).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx
}

/// Write one newline-framed JSON-RPC frame and flush.
pub(crate) fn write_frame(writer: &mut dyn Write, frame: &Value) -> Result<(), AppServerErr> {
    let mut bytes =
        serde_json::to_vec(frame).map_err(|err| AppServerErr::Protocol(err.to_string()))?;
    bytes.push(b'\n');
    writer.write_all(&bytes).map_err(AppServerErr::Io)?;
    writer.flush().map_err(AppServerErr::Io)
}

/// Wait for the response frame matching `id`, skipping non-JSON noise,
/// server notifications, and server-initiated requests (a different id or none),
/// until `deadline`.
pub(crate) fn recv_response(
    rx: &Receiver<String>,
    deadline: Instant,
    id: i64,
) -> Result<Value, AppServerErr> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(AppServerErr::Timeout)?;
        let line = rx.recv_timeout(remaining).map_err(|err| match err {
            RecvTimeoutError::Timeout => AppServerErr::Timeout,
            RecvTimeoutError::Disconnected => AppServerErr::Closed,
        })?;
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if value.get("id").and_then(Value::as_i64) != Some(id) {
            continue;
        }
        if let Some(err) = value.get("error") {
            return Err(AppServerErr::JsonRpc {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            });
        }
        return Ok(value.get("result").cloned().unwrap_or(Value::Null));
    }
}

/// Newline-framed JSON-RPC over one byte stream, with a reader thread draining
/// frames so each request can wait with its remaining deadline. Backs two
/// sources: a spawned `codex` child (cold-spawn, or `app-server proxy --sock ...`
/// bridged to the per-user daemon) and a [`UnixStream`] to the per-session broker
/// ([`crate::agents::codex::broker`]). Only the child case owns a process to reap.
pub(crate) struct FramedTransport {
    writer: Box<dyn Write + Send>,
    rx: Receiver<String>,
    next_id: i64,
    deadline: Instant,
    /// `Some` when we spawned a `codex` child — killed and reaped on drop. `None`
    /// for a unix-socket connection (dropping `writer` closes it; the broker
    /// reaps the client on EOF).
    child: Option<Child>,
}

impl FramedTransport {
    /// Spawn `bin` with `args` (e.g. `["app-server"]` or
    /// `["app-server", "proxy", "--sock", <path>]`), giving the handshake +
    /// reads `total` wall-clock.
    pub(super) fn spawn(
        bin: &Path,
        args: &[String],
        total: Duration,
    ) -> Result<Self, AppServerErr> {
        let mut child = Command::new(bin)
            .args(args)
            // stdin/stdout are the JSON-RPC channel; stderr is diagnostics we
            // never want — null it (the fresh-stdio invariant for helpers).
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(AppServerErr::Spawn)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppServerErr::Protocol("app-server stdin unavailable".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppServerErr::Protocol("app-server stdout unavailable".to_owned()))?;
        let rx = spawn_frame_reader(BufReader::new(stdout));
        Ok(Self {
            writer: Box::new(stdin),
            rx,
            next_id: 1,
            deadline: Instant::now() + total,
            child: Some(child),
        })
    }

    /// Connect to the per-session broker socket at `path`, giving the handshake +
    /// reads `total` wall-clock. The broker is warm, so this is the fast path.
    pub(super) fn connect(path: &Path, total: Duration) -> Result<Self, AppServerErr> {
        let stream = UnixStream::connect(path).map_err(AppServerErr::Io)?;
        let reader = stream.try_clone().map_err(AppServerErr::Io)?;
        let rx = spawn_frame_reader(BufReader::new(reader));
        Ok(Self {
            writer: Box::new(stream),
            rx,
            next_id: 1,
            deadline: Instant::now() + total,
            child: None,
        })
    }
}

impl JsonRpcTransport for FramedTransport {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, AppServerErr> {
        let id = self.next_id;
        self.next_id += 1;
        write_frame(
            &mut self.writer,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )?;
        recv_response(&self.rx, self.deadline, id)
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), AppServerErr> {
        write_frame(
            &mut self.writer,
            &json!({"jsonrpc": "2.0", "method": method, "params": params}),
        )
    }
}

impl Drop for FramedTransport {
    fn drop(&mut self) {
        // Kill+reap a spawned child so no wedged server lingers. A unix-socket
        // connection has no child; dropping `writer` closes it.
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
