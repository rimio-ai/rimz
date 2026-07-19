//! Per-session Codex app-server broker — a warm, held `codex app-server`.
//!
//! Codex enrichment ([`super::app_server`]) otherwise cold-spawns a fresh
//! `codex app-server` per datapoint and pays the full JSON-RPC handshake each
//! time. This broker holds one long-lived child, handshakes it once, and serves
//! it over a per-session unix socket so each refresh skips the handshake. It runs
//! as a visible pane in the `rimzd` daemon tab (`rimz codex app-server serve`).
//!
//! Scope: **local read-only enrichment**, not the account-linking remote-control
//! feature ([`crate::remote_control`]). It links no account and only forwards the
//! read-only methods the client speaks, so it runs whenever `codex` is on PATH —
//! no opt-in — and degrades to nothing when it isn't.
//!
//! Lifecycle and ownership (the risk [`docs/internals/performance.md`] flags):
//! - **Startup**: spawn + handshake. If `codex` is absent or won't handshake,
//!   exit cleanly (return `Ok`) — the pane closes and enrichment cold-spawns.
//! - **Serving**: one mutex serializes all child access, so each client request
//!   is an atomic round-trip — no id demux across in-flight requests is needed
//!   (enrichment is single-flight per the refresh throttle). A client
//!   `initialize` is answered from the cached result; `initialized` is swallowed.
//! - **Child death**: a round-trip that hits EOF/IO respawns the child once and
//!   retries; a wedged child times out (the client falls back). The child reads
//!   JSON-RPC on stdin, so when this process dies its stdin pipe closes and the
//!   child exits — no orphan.
//! - **Socket**: bound on a per-session path derived from the workspace id
//!   ([`crate::store::paths::RuntimePaths::codex_app_server_socket_path`]); a
//!   stale file is unlinked first, and a [`SocketGuard`] removes it on a graceful
//!   exit. A leftover socket is harmless — the next broker unlinks it on bind.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::app_server::{AppServerErr, codex_bin, recv_response, spawn_frame_reader, write_frame};
use super::oauth_usage;
use crate::harness::run_wake::SocketGuard;

/// Wall-clock for the startup (and respawn) handshake — generous like the client
/// cold-spawn budget, since it spawns a process and waits for `initialize`.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(6);

/// Per-request budget for one forwarded round-trip. A read-only method answers
/// in well under this; exceeding it means a wedged child — the request fails and
/// the client falls back rather than the broker hanging under its lock.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

/// ANSI clear-screen + cursor-home. Dependency-free escapes, the same idiom the
/// `mux::zellij` layout code uses.
const CLEAR_SCREEN: &str = "\x1b[2J\x1b[H";

/// Display context for the broker pane's status banner. Presentation only — never
/// consulted on the serving path, so a render that fails or lies cannot affect
/// enrichment.
pub struct BrokerInfo<'a> {
    /// Session name shown in the banner; the session line is omitted when `None`.
    pub session: Option<&'a str>,
    /// The per-session broker socket the pane binds and serves on.
    pub socket_path: &'a Path,
}

/// The broker pane's status banner: a screen-clear followed by the daemon's
/// identity and ready state. Pure so it is unit-testable without a socket or a
/// terminal; the success path writes it once so the pane reads as a live daemon
/// rather than a black screen.
fn render_banner(info: &BrokerInfo<'_>) -> String {
    let session_line = match info.session {
        Some(session) => format!("session: {session}\n"),
        None => String::new(),
    };
    format!(
        "{CLEAR_SCREEN}rimz · codex app-server broker\n{session_line}socket : {}\nstatus : ready · serving Codex enrichment\n",
        info.socket_path.display(),
    )
}

/// The held `codex app-server` child: write half, the reader-thread channel for
/// its frames, the request id counter, and the cached `initialize` result so
/// client handshakes need no round-trip. The auth stamp tracks the credential
/// file the child read at spawn so an account switch respawns it before serving.
struct ChildIo {
    stdin: ChildStdin,
    rx: Receiver<String>,
    next_id: i64,
    init_result: Value,
    auth_stamp: Option<u64>,
    child: Child,
}

impl ChildIo {
    /// Forward one request to the child and wait for its matching response.
    fn round_trip(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, AppServerErr> {
        self.next_id += 1;
        let id = self.next_id;
        write_frame(
            &mut self.stdin,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )?;
        recv_response(&self.rx, deadline, id)
    }

    /// Kill the dead child and replace this with a freshly handshaked one.
    fn respawn(&mut self) -> Result<(), AppServerErr> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        *self = spawn_and_handshake()?;
        Ok(())
    }
}

impl Drop for ChildIo {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `codex app-server`, complete the `initialize`/`initialized` handshake,
/// and cache the result. The child's stdin/stdout are the JSON-RPC channel;
/// stderr is nulled (the fresh-stdio invariant — the pane shows this broker's own
/// `tracing`, not the child's diagnostics).
fn spawn_and_handshake() -> Result<ChildIo, AppServerErr> {
    let bin = codex_bin();
    let mut child = Command::new(&bin)
        .arg("app-server")
        // Mark this as a RimZ-internal enrichment server so the lifecycle hooks
        // it fires on startup no-op in `rimz hooks feed` rather than recursing
        // through `refresh-context` into another app-server spawn.
        .env(crate::agents::adapters::codex::ENV_INTERNAL_APP_SERVER, "1")
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
    let mut io = ChildIo {
        stdin,
        rx,
        next_id: 0,
        init_result: Value::Null,
        auth_stamp: oauth_usage::credentials_stamp(),
        child,
    };
    let init = io.round_trip(
        "initialize",
        json!({ "clientInfo": { "name": "rimz", "version": env!("CARGO_PKG_VERSION") } }),
        Instant::now() + HANDSHAKE_DEADLINE,
    )?;
    write_frame(
        &mut io.stdin,
        &json!({"jsonrpc": "2.0", "method": "initialized"}),
    )?;
    io.init_result = init;
    Ok(io)
}

/// Lock the shared child, recovering from a poisoned mutex (a panicked handler
/// thread must not wedge the whole broker — the child state is still valid).
fn lock(shared: &Mutex<ChildIo>) -> std::sync::MutexGuard<'_, ChildIo> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Serve one client request against the warm child. `initialize` is answered from
/// the cache (the amortization — clients skip the handshake). Anything else is a
/// locked round-trip; an EOF/IO failure respawns the child once and retries.
fn serve_request(
    shared: &Mutex<ChildIo>,
    method: &str,
    params: Value,
) -> Result<Value, AppServerErr> {
    if method == "initialize" {
        return Ok(lock(shared).init_result.clone());
    }
    let mut io = lock(shared);
    let auth_stamp = oauth_usage::credentials_stamp();
    if io.auth_stamp != auth_stamp {
        tracing::info!("codex auth changed; respawning app-server child");
        io.respawn()?;
    }
    match io.round_trip(method, params.clone(), Instant::now() + REQUEST_DEADLINE) {
        Err(AppServerErr::Closed | AppServerErr::Io(_)) => {
            tracing::warn!("codex app-server child gone; respawning");
            io.respawn()?;
            io.round_trip(method, params, Instant::now() + REQUEST_DEADLINE)
        }
        other => other,
    }
}

/// Handle one client connection: a newline-framed JSON-RPC stream. Requests
/// (carry an `id`) get a forwarded response; notifications carry none —
/// `initialized` is swallowed (the child is already initialized), others are
/// forwarded best-effort. Returns when the client disconnects.
fn handle_client(stream: UnixStream, shared: Arc<Mutex<ChildIo>>) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut writer = stream;
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let Ok(request) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        match request.get("id").cloned() {
            None => {
                // A notification. The child is already initialized; forward
                // anything else best-effort (read-only clients send none).
                if method != "initialized" {
                    let _ = write_frame(&mut lock(&shared).stdin, &request);
                }
            }
            Some(id) => {
                let frame = match serve_request(&shared, &method, params) {
                    Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    Err(err) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32000, "message": err.to_string() }
                    }),
                };
                if write_frame(&mut writer, &frame).is_err() {
                    return;
                }
            }
        }
    }
}

/// Run the broker: bring up the warm child, bind the per-session socket, and
/// serve clients until the pane closes. Returns `Ok(())` and exits cleanly when
/// `codex` is unavailable so the pane closes and enrichment cold-spawns instead.
pub fn serve(info: BrokerInfo<'_>) -> std::io::Result<()> {
    let socket_path = info.socket_path;
    let child = match spawn_and_handshake() {
        Ok(io) => io,
        Err(err) => {
            tracing::warn!(
                error = %err,
                "codex app-server unavailable; enrichment will cold-spawn per datapoint",
            );
            return Ok(());
        }
    };

    // Unlink any stale socket (a previous broker that didn't clean up), then bind
    // and lock it down to the owner.
    crate::sock::validate_socket_path(socket_path).map_err(std::io::Error::other)?;
    if socket_path.exists() {
        let _ = std::fs::remove_file(socket_path);
    }
    let listener = UnixListener::bind(socket_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600));
    }
    let _guard = SocketGuard::new(socket_path.to_path_buf());
    tracing::info!(socket = %socket_path.display(), "codex app-server broker ready");

    // Paint the pane so it reads as a live daemon, not a black screen. Best-effort
    // presentation: a write failure must never interrupt serving.
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(render_banner(&info).as_bytes());
    let _ = stdout.flush();
    drop(stdout);

    let shared = Arc::new(Mutex::new(child));
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || handle_client(stream, shared));
            }
            Err(err) => tracing::warn!(error = %err, "broker accept failed"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_banner_clears_screen_then_shows_session_socket_and_status() {
        let with_session = render_banner(&BrokerInfo {
            session: Some("query-engine"),
            socket_path: Path::new("/run/user/1000/rimz/ws/codex-app-server.sock"),
        });
        assert!(
            with_session.starts_with("\x1b[2J\x1b[H"),
            "{with_session:?}"
        );
        assert!(with_session.contains("query-engine"), "{with_session:?}");
        assert!(
            with_session.contains("codex-app-server.sock"),
            "{with_session:?}"
        );
        assert!(with_session.contains("ready"), "{with_session:?}");

        let no_session = render_banner(&BrokerInfo {
            session: None,
            socket_path: Path::new("/run/x/codex-app-server.sock"),
        });
        assert!(!no_session.contains("session:"), "{no_session:?}");
        assert!(
            no_session.contains("codex-app-server.sock"),
            "{no_session:?}"
        );
    }
}
