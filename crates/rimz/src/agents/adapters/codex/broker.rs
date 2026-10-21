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
//!   (enrichment is single-flight per the refresh throttle). Every request,
//!   `initialize` included, first re-resolves the login and respawns the child
//!   when its home or credentials moved; a resolver error keeps the child. A client
//!   `initialize` is then answered from the cached result while the child is live,
//!   and respawns it first otherwise; `initialized` is swallowed.
//! - **Identity**: every successful response frame carries a top-level
//!   `rimzServedBy` member beside `result`, naming the canonical Codex home and
//!   auth stamp of the child that produced it, read under the same lock hold as
//!   the round-trip. The broker reports and the client decides: a client accepts
//!   a response only when that identity is the one it expects, and an older
//!   broker, which never adds the member, is therefore not accepted.
//! - **Child death**: a round-trip that hits EOF/IO respawns the child once and
//!   retries; a wedged child times out (the client falls back). The child reads
//!   JSON-RPC on stdin, so when this process dies its stdin pipe closes and the
//!   child exits — no orphan.
//! - **Socket**: bound on a per-session path derived from the workspace id
//!   ([`crate::disk::paths::RuntimePaths::codex_app_server_socket_path`]); a
//!   stale file is unlinked first, and a [`SocketGuard`] removes it on a graceful
//!   exit. A leftover socket is harmless — the next broker unlinks it on bind.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

use super::app_server::{
    AppServerErr, FramedTransport, JsonRpcTransport, ServedBy, codex_bin, initialize,
    installed_codex_bin, write_frame,
};
use super::oauth_usage;
use crate::sock::SocketGuard;

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
pub(in crate::agents) struct BrokerInfo<'a> {
    /// Session name shown in the banner; the session line is omitted when `None`.
    pub(in crate::agents) session: Option<&'a str>,
    /// The per-session broker socket the pane binds and serves on.
    pub(in crate::agents) socket_path: &'a Path,
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

/// The held `codex app-server` transport and the cached `initialize` result so
/// client handshakes need no round-trip. The auth stamp tracks the credential
/// file the child read at spawn so an account switch respawns it before serving.
struct ChildIo {
    program: PathBuf,
    login_key: crate::ids::LoginKey,
    login_env: BTreeMap<String, String>,
    transport: FramedTransport,
    init_result: Value,
    auth_stamp: Option<u64>,
}

impl ChildIo {
    /// Kill the dead child and replace this with a freshly handshaked one.
    fn respawn(
        &mut self,
        key: crate::ids::LoginKey,
        login_env: &BTreeMap<String, String>,
    ) -> Result<(), AppServerErr> {
        self.transport.stop_child();
        *self = spawn_and_handshake(&self.program, key, login_env)?;
        Ok(())
    }
}

/// Spawn `codex app-server`, complete the `initialize`/`initialized` handshake,
/// and cache the result. The child's stdin/stdout are the JSON-RPC channel;
/// stderr is nulled (the fresh-stdio invariant — the pane shows this broker's own
/// `tracing`, not the child's diagnostics).
fn spawn_and_handshake(
    program: &Path,
    login_key: crate::ids::LoginKey,
    login_env: &BTreeMap<String, String>,
) -> Result<ChildIo, AppServerErr> {
    let mut transport = FramedTransport::spawn(program, HANDSHAKE_DEADLINE, login_env)?;
    let auth_stamp = oauth_usage::credentials_stamp(login_env);
    transport.set_deadline(HANDSHAKE_DEADLINE);
    let init_result = initialize(&mut transport, None)?;
    Ok(ChildIo {
        program: program.to_owned(),
        login_key,
        login_env: login_env.clone(),
        transport,
        init_result,
        auth_stamp,
    })
}

impl ChildIo {
    /// The identity this child's responses carry; `None` when its home cannot be
    /// canonicalized, which a client reads as unconfirmed.
    fn served_by(&self) -> Option<ServedBy> {
        ServedBy::of(&self.login_env, self.auth_stamp)
    }
}

/// The result one client request produced and the child that produced it.
struct Served {
    result: Value,
    served_by: Option<ServedBy>,
}

/// Lock the shared child, recovering from a poisoned mutex (a panicked handler
/// thread must not wedge the whole broker — the child state is still valid).
fn lock(shared: &Mutex<ChildIo>) -> std::sync::MutexGuard<'_, ChildIo> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Serve one client request against the warm child. Every request re-resolves
/// the login first and respawns the child when its home or credentials moved; a
/// resolver error keeps the current child, which the reported identity then names
/// truthfully. `initialize` is answered from the cache while the child is live
/// (the amortization — clients skip the handshake); with the child gone it
/// respawns first, so a dead broker reports the spawn error instead of a stale
/// handshake. Anything else is a locked round-trip; an EOF/IO failure respawns
/// the child once and retries.
fn serve_request(
    shared: &Mutex<ChildIo>,
    resolve_login: &dyn Fn() -> Result<crate::agents::ProviderLogin, crate::agents::RoomLoginErr>,
    method: &str,
    params: Value,
) -> Result<Served, AppServerErr> {
    let mut io = lock(shared);
    let resolved = match resolve_login() {
        Ok(login) => Some((login.key(), login.env(&crate::agents::ambient_env()))),
        Err(error) => {
            tracing::warn!(%error, "keeping current codex app-server login");
            None
        }
    };
    let moved = resolved.as_ref().is_some_and(|(key, login_env)| {
        let auth_stamp = oauth_usage::credentials_stamp(login_env);
        io.login_key != *key
            || io.auth_stamp != auth_stamp
            || io.served_by() != ServedBy::of(login_env, auth_stamp)
    });
    let dead = method == "initialize" && !io.transport.child_is_live()?;
    if moved || dead {
        if moved {
            tracing::info!("codex login or auth changed; respawning app-server child");
        }
        let (key, login_env) =
            resolved.unwrap_or_else(|| (io.login_key.clone(), io.login_env.clone()));
        io.respawn(key, &login_env)?;
    }
    let result = if method == "initialize" {
        io.init_result.clone()
    } else {
        io.transport.set_deadline(REQUEST_DEADLINE);
        match io.transport.request(method, params.clone()) {
            Err(AppServerErr::Closed | AppServerErr::Io(_)) => {
                tracing::warn!("codex app-server child gone; respawning");
                let key = io.login_key.clone();
                let login_env = io.login_env.clone();
                io.respawn(key, &login_env)?;
                io.transport.set_deadline(REQUEST_DEADLINE);
                io.transport.request(method, params)?
            }
            other => other?,
        }
    };
    Ok(Served {
        result,
        served_by: io.served_by(),
    })
}

/// Handle one client connection: a newline-framed JSON-RPC stream. Requests
/// (carry an `id`) get a forwarded response; notifications carry none —
/// `initialized` is swallowed (the child is already initialized), others are
/// forwarded best-effort. Returns when the client disconnects.
fn handle_client(
    stream: UnixStream,
    shared: Arc<Mutex<ChildIo>>,
    resolve_login: Arc<
        impl Fn() -> Result<crate::agents::ProviderLogin, crate::agents::RoomLoginErr>,
    >,
) {
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
                    let _ = lock(&shared).transport.notify_frame(&request);
                }
            }
            Some(id) => {
                let frame = match serve_request(&shared, resolve_login.as_ref(), &method, params) {
                    Ok(served) => {
                        let mut frame =
                            json!({"jsonrpc": "2.0", "id": id, "result": served.result});
                        if let Some(served_by) = served.served_by {
                            served_by.insert_into(&mut frame);
                        }
                        frame
                    }
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

/// The binary [`serve`] hosts, when codex is installed.
pub(in crate::agents) fn installed_bin() -> Option<PathBuf> {
    installed_codex_bin()
}

/// Run the broker: bring up the warm child, bind the per-session socket, and
/// serve clients until the pane closes. Returns `Ok(())` and exits cleanly when
/// `codex` is unavailable so the pane closes and enrichment cold-spawns instead.
pub(in crate::agents) fn serve(
    info: BrokerInfo<'_>,
    resolve_login: impl Fn() -> Result<crate::agents::ProviderLogin, crate::agents::RoomLoginErr>
    + Send
    + Sync
    + 'static,
) -> std::io::Result<()> {
    let socket_path = info.socket_path;
    let login = resolve_login().map_err(std::io::Error::other)?;
    let child = match spawn_and_handshake(
        &codex_bin(),
        login.key(),
        &login.env(&crate::agents::ambient_env()),
    ) {
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
    let resolve_login = Arc::new(resolve_login);
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let shared = Arc::clone(&shared);
                let resolve_login = Arc::clone(&resolve_login);
                std::thread::spawn(move || handle_client(stream, shared, resolve_login));
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
    fn the_next_request_respawns_under_the_resolved_login() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("codex");
        std::fs::write(&program, "#!/bin/sh\nrequest_id=0\nwhile read -r line; do\ncase \"$line\" in *'\"id\"'*) request_id=$((request_id + 1)); printf '{\"id\":%s,\"result\":{\"home\":\"%s\"}}\\n' \"$request_id\" \"$CODEX_HOME\";; esac\ndone\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let login = |name: &str| {
            crate::agents::ProviderLogin::named(
                crate::ids::AgentKind::new_unchecked("codex"),
                name.parse().unwrap(),
                dir.path().join(name),
            )
            .unwrap()
        };
        let work = login("work");
        let next = login("spare");
        let shared = Mutex::new(
            spawn_and_handshake(&program, work.key(), &work.env(&BTreeMap::new())).unwrap(),
        );
        assert_eq!(
            serve_request(&shared, &|| Ok(work.clone()), "initialize", Value::Null)
                .unwrap()
                .result["home"],
            work.home().unwrap().to_str().unwrap()
        );
        assert_eq!(
            serve_request(&shared, &|| Ok(next.clone()), "initialize", Value::Null)
                .unwrap()
                .result["home"],
            next.home().unwrap().to_str().unwrap()
        );
        assert_eq!(
            serve_request(&shared, &|| Ok(next.clone()), "account/read", Value::Null)
                .unwrap()
                .result["home"],
            next.home().unwrap().to_str().unwrap()
        );
        assert_eq!(
            serve_request(
                &shared,
                &|| crate::agents::session_login(
                    &crate::ids::AgentKind::new_unchecked("codex"),
                    Some(&"removed".parse().unwrap()),
                    &Default::default(),
                ),
                "account/read",
                Value::Null,
            )
            .unwrap()
            .result["home"],
            next.home().unwrap().to_str().unwrap()
        );
        lock(&shared).transport.stop_child();
        assert_eq!(
            serve_request(&shared, &|| Ok(work.clone()), "initialize", Value::Null)
                .unwrap()
                .result["home"],
            work.home().unwrap().to_str().unwrap()
        );
        lock(&shared).transport.stop_child();
        assert_eq!(
            serve_request(
                &shared,
                &|| crate::agents::session_login(
                    &crate::ids::AgentKind::new_unchecked("codex"),
                    Some(&"removed".parse().unwrap()),
                    &Default::default(),
                ),
                "initialize",
                Value::Null,
            )
            .unwrap()
            .result["home"],
            work.home().unwrap().to_str().unwrap()
        );
        assert!(lock(&shared).transport.child_is_live().unwrap());
    }

    #[test]
    fn every_response_names_the_child_that_served_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("codex");
        std::fs::write(&program, "#!/bin/sh\nrequest_id=0\nwhile read -r line; do\ncase \"$line\" in *'\"id\"'*) request_id=$((request_id + 1)); printf '{\"id\":%s,\"result\":{}}\\n' \"$request_id\";; esac\ndone\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let login = |name: &str| {
            let home = dir.path().join(name);
            std::fs::create_dir(&home).unwrap();
            std::fs::write(home.join("auth.json"), "{}").unwrap();
            crate::agents::ProviderLogin::named(
                crate::ids::AgentKind::new_unchecked("codex"),
                name.parse().unwrap(),
                home,
            )
            .unwrap()
        };
        let identity = |login: &crate::agents::ProviderLogin| {
            let env = login.env(&BTreeMap::new());
            ServedBy::of(&env, oauth_usage::credentials_stamp(&env)).unwrap()
        };
        let work = login("work");
        let next = login("spare");
        assert_ne!(identity(&work), identity(&next));
        let shared = Mutex::new(
            spawn_and_handshake(&program, work.key(), &work.env(&BTreeMap::new())).unwrap(),
        );
        let served_by = |resolve: &dyn Fn() -> _, method: &str| {
            serve_request(&shared, resolve, method, Value::Null)
                .unwrap()
                .served_by
        };

        assert_eq!(
            served_by(&|| Ok(work.clone()), "initialize"),
            Some(identity(&work))
        );
        assert_eq!(
            served_by(&|| Ok(work.clone()), "account/read"),
            Some(identity(&work))
        );
        assert_eq!(
            served_by(&|| Ok(next.clone()), "initialize"),
            Some(identity(&next))
        );
        let removed = || {
            crate::agents::session_login(
                &crate::ids::AgentKind::new_unchecked("codex"),
                Some(&"removed".parse().unwrap()),
                &Default::default(),
            )
        };
        assert_eq!(served_by(&removed, "account/read"), Some(identity(&next)));
        assert_eq!(served_by(&removed, "initialize"), Some(identity(&next)));

        lock(&shared).transport.stop_child();
        assert_eq!(
            served_by(&|| Ok(work.clone()), "account/read"),
            Some(identity(&work))
        );
        lock(&shared).transport.stop_child();
        assert_eq!(served_by(&removed, "account/read"), Some(identity(&work)));
    }

    #[test]
    fn the_frame_carries_the_serving_identity_beside_the_result() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("codex");
        std::fs::write(&program, "#!/bin/sh\nrequest_id=0\nwhile read -r line; do\ncase \"$line\" in *'\"id\"'*) request_id=$((request_id + 1)); printf '{\"id\":%s,\"result\":{}}\\n' \"$request_id\";; esac\ndone\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = dir.path().join("work");
        std::fs::create_dir(&home).unwrap();
        let work = crate::agents::ProviderLogin::named(
            crate::ids::AgentKind::new_unchecked("codex"),
            "work".parse().unwrap(),
            home.clone(),
        )
        .unwrap();
        let shared = Arc::new(Mutex::new(
            spawn_and_handshake(&program, work.key(), &work.env(&BTreeMap::new())).unwrap(),
        ));
        let (client, server) = UnixStream::pair().unwrap();
        let resolver = Arc::new(move || Ok(work.clone()));
        let handler = std::thread::spawn(move || handle_client(server, shared, resolver));
        let mut writer = client.try_clone().unwrap();
        write_frame(
            &mut writer,
            &json!({"jsonrpc": "2.0", "id": 7, "method": "account/read"}),
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        drop(writer);
        handler.join().unwrap();
        let frame: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["id"], 7);
        assert_eq!(frame["result"], json!({}));
        assert_eq!(
            frame["rimzServedBy"],
            json!({"codexHome": home.canonicalize().unwrap(), "authStamp": null})
        );
    }

    #[test]
    fn initialize_reports_a_failed_respawn_instead_of_cached_success() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("codex");
        std::fs::write(&program, "#!/bin/sh\nrequest_id=0\nwhile read -r line; do\ncase \"$line\" in *'\"id\"'*) request_id=$((request_id + 1)); printf '{\"id\":%s,\"result\":{}}\\n' \"$request_id\";; esac\ndone\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let next = crate::agents::ProviderLogin::default_for(crate::ids::AgentKind::new_unchecked(
            "codex",
        ));
        let shared =
            Mutex::new(spawn_and_handshake(&program, next.key(), &BTreeMap::new()).unwrap());
        lock(&shared).program = dir.path().join("missing-codex");
        assert!(
            lock(&shared)
                .respawn(next.key(), &next.env(&BTreeMap::new()))
                .is_err()
        );
        assert!(matches!(
            serve_request(&shared, &|| Ok(next.clone()), "initialize", Value::Null),
            Err(AppServerErr::Spawn(_))
        ));
    }

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
