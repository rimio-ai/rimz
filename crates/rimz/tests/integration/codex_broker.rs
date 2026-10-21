//! The per-session Codex app-server broker (`rimz codex app-server serve`).
//!
//! It holds one warm `codex app-server` — here the `codex-appserver-stub`
//! fixture, pointed at by `RIMZ_CODEX_BIN` — and serves it over the session's
//! unix socket: `initialize` is answered from the cached handshake, the
//! read-only methods are forwarded to the child and routed back by id.

use std::fs::{File, FileTimes};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::common::{Env, codex_appserver_stub};

#[test]
fn explicit_card_refresh_still_spawns_context_helper() {
    let harness = crate::common::Harness::new();
    let snapshot = harness.store.snapshot().unwrap();
    let agent =
        rimz::testkit::agent_state("codex", "explicit-refresh", jiff::Timestamp::UNIX_EPOCH);
    let before = rimz::testkit::spawn_count();
    let refreshed = rimz::sidebar::refresh::force_refresh_session_context(
        &snapshot,
        &harness.runtime_paths,
        &agent,
    )
    .unwrap();
    assert!(refreshed.helper_spawned);
    assert!(rimz::testkit::spawn_count() > before);
}

fn old_login_room() -> Env {
    let env = Env::new();
    assert!(
        env.rimz()
            .args(["accounts", "add", "codex", "work"])
            .output()
            .unwrap()
            .status
            .success()
    );
    env.record(&env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    let store = env.store();
    let kind = rimz::ids::AgentKind::new_unchecked("codex");
    store
        .begin_agent_launch_batch(
            &[rimz::store::writer::AgentLaunchRequest {
                kind: kind.clone(),
                login: rimz::store::writer::LaunchLogin::Pinned("work".parse().unwrap()),
                agent_id: "old-login".into(),
                name: rimz::store::writer::AgentLaunchName::Explicit("old-login".into()),
                launch: Default::default(),
                run_id: None,
                prompt: None,
            }],
            rimz::store::writer::AgentLaunchScope {
                session_name: workspace.session_name.clone(),
                cwd: env.project_root.clone(),
                branch: None,
                description: None,
            },
        )
        .unwrap();
    store
        .switch_room_login(&workspace, &kind, &"default".parse().unwrap())
        .unwrap();
    env
}

#[test]
fn old_login_usage_refresh_does_not_connect_to_room_broker() {
    let env = old_login_room();
    let runtime = env.runtime_paths();
    let socket = runtime.codex_app_server_socket_path();
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let claim_id = uuid::Uuid::now_v7();
    let now = rimz::utils::time::unix_now_ms();
    std::fs::write(
        runtime.shared_credits_path(),
        serde_json::to_vec(&json!({
            "refreshed_at_ms": now,
            "logins": { "codex@work": {
                "observed_at_ms": 0, "ok": false,
                "direct_query_claim": {
                    "nonce": claim_id, "claimed_at_ms": now,
                    "requested_scope": rimz::agents::ProviderAccountScope::default()
                }
            }}
        }))
        .unwrap(),
    )
    .unwrap();
    let request = rimz::sidebar::refresh::usage::AccountUsageRefreshRequest {
        workspace_id: runtime.workspace_id.clone(),
        login: "codex@work".parse().unwrap(),
        claim_id,
    };
    let output = env
        .rimz()
        .args(rimz::child_process::agent_helper_argv(
            "refresh-usage",
            &request,
        ))
        .env("RIMZ_OAUTH_USAGE_OFFLINE", "0")
        .env("RIMZ_CODEX_BIN", codex_appserver_stub())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let credits: Value =
        serde_json::from_slice(&std::fs::read(runtime.shared_credits_path()).unwrap()).unwrap();
    assert!(
        credits["logins"]["codex@work"]["oauth_read_at_ms"]
            .as_u64()
            .unwrap()
            >= now,
        "the claimed direct probe must still run for the old login"
    );
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "old-login usage must never touch the current-login broker"
    );
}

#[test]
fn old_login_context_refresh_does_not_connect_to_room_broker() {
    let env = old_login_room();
    let workspace = env.resolve_workspace(&env.project_root);
    let kind = rimz::ids::AgentKind::new_unchecked("codex");
    let socket = env.runtime_paths().codex_app_server_socket_path();
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let request = rimz::agents::LifecycleRefreshRequest {
        kind,
        session_id: "old-login".into(),
        workspace_id: workspace.workspace_id,
        model: None,
        server_url: None,
    };
    let output = env
        .rimz()
        .args(rimz::child_process::agent_helper_argv(
            "refresh-context",
            &request,
        ))
        .env("RIMZ_CODEX_BIN", codex_appserver_stub())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "old-login refresh must never touch the current-login broker"
    );
}

/// A room whose Codex default is login `rimio`, beside a second login `team-1`,
/// and a listener on the room's broker socket that behaves as a pre-fix broker
/// (no `rimzServedBy`) proxying a stub child under team-1's home. The stub
/// reports the plan its serving home names: `team` for team-1, `pro` for rimio.
struct RimioRoom {
    env: Env,
    methods: Arc<Mutex<Vec<String>>>,
    _homes: tempfile::TempDir,
}

fn rimio_room_behind_a_prefix_broker() -> RimioRoom {
    let env = Env::new();
    let homes = tempfile::tempdir().unwrap();
    for (name, plan) in [("rimio", "pro"), ("team-1", "team")] {
        let home = homes.path().join(name);
        assert!(
            env.rimz()
                .args(["accounts", "add", "codex", name, "--home"])
                .arg(&home)
                .output()
                .unwrap()
                .status
                .success()
        );
        std::fs::write(home.join("stub-plan"), plan).unwrap();
    }
    env.record(&env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    let kind = rimz::ids::AgentKind::new_unchecked("codex");
    let store = env.store();
    store
        .begin_agent_launch_batch(
            &[rimz::store::writer::AgentLaunchRequest {
                kind: kind.clone(),
                login: rimz::store::writer::LaunchLogin::Pinned("rimio".parse().unwrap()),
                agent_id: "rimio-session".into(),
                name: rimz::store::writer::AgentLaunchName::Explicit("rimio-session".into()),
                launch: Default::default(),
                run_id: None,
                prompt: None,
            }],
            rimz::store::writer::AgentLaunchScope {
                session_name: workspace.session_name.clone(),
                cwd: env.project_root.clone(),
                branch: None,
                description: None,
            },
        )
        .unwrap();
    store
        .switch_room_login(&workspace, &kind, &"rimio".parse().unwrap())
        .unwrap();

    let socket = env.runtime_paths().codex_app_server_socket_path();
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = UnixListener::bind(socket).unwrap();
    let methods = Arc::new(Mutex::new(Vec::new()));
    let served = Arc::clone(&methods);
    let team_home = homes.path().join("team-1");
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let served = Arc::clone(&served);
            let team_home = team_home.clone();
            std::thread::spawn(move || proxy_to_stub_child(stream, &team_home, &served));
        }
    });
    RimioRoom {
        env,
        methods,
        _homes: homes,
    }
}

/// Answer each request line from a stub child under `home`, in the frame shape
/// a broker from before `rimzServedBy` wrote: `{jsonrpc, id, result}`.
fn proxy_to_stub_child(stream: UnixStream, home: &PathBuf, served: &Mutex<Vec<String>>) {
    let mut child = std::process::Command::new(codex_appserver_stub())
        .arg("app-server")
        .env("RIMZ_CODEX_INTERNAL_APP_SERVER", "1")
        .env("CODEX_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child_in = child.stdin.take().unwrap();
    let mut child_out = BufReader::new(child.stdout.take().unwrap());
    let mut writer = stream.try_clone().unwrap();
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = request.get("id") else {
            continue;
        };
        let method = request["method"].as_str().unwrap_or_default().to_owned();
        served.lock().unwrap().push(method);
        writeln!(child_in, "{request}").unwrap();
        let mut reply = String::new();
        child_out.read_line(&mut reply).unwrap();
        let reply: Value = serde_json::from_str(reply.trim()).unwrap();
        let frame = json!({ "jsonrpc": "2.0", "id": id, "result": reply["result"] });
        if writeln!(writer, "{frame}").is_err() {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl RimioRoom {
    fn assert_only_the_handshake_reached_the_prefix_broker(&self) {
        let methods = self.methods.lock().unwrap();
        assert!(!methods.is_empty(), "the ladder must try the broker first");
        assert!(
            methods.iter().all(|method| method == "initialize"),
            "no read may be served by a broker that cannot name its child: {methods:?}"
        );
    }

    fn rimio_plan_in_credits(&self) -> Option<String> {
        let credits: Value = serde_json::from_slice(
            &std::fs::read(self.env.runtime_paths().shared_credits_path()).unwrap(),
        )
        .unwrap();
        credits["logins"]["codex@rimio"]["plan"]
            .as_str()
            .map(ToOwned::to_owned)
    }
}

#[test]
fn account_refresh_ignores_a_broker_that_cannot_name_its_child() {
    let room = rimio_room_behind_a_prefix_broker();
    let runtime = room.env.runtime_paths();
    let claim_id = uuid::Uuid::now_v7();
    let now = rimz::utils::time::unix_now_ms();
    std::fs::write(
        runtime.shared_credits_path(),
        serde_json::to_vec(&json!({
            "refreshed_at_ms": now,
            "logins": { "codex@rimio": {
                "observed_at_ms": 0, "ok": false,
                "direct_query_claim": {
                    "nonce": claim_id, "claimed_at_ms": now,
                    "requested_scope": rimz::agents::ProviderAccountScope::default()
                }
            }}
        }))
        .unwrap(),
    )
    .unwrap();
    let request = rimz::sidebar::refresh::usage::AccountUsageRefreshRequest {
        workspace_id: runtime.workspace_id.clone(),
        login: "codex@rimio".parse().unwrap(),
        claim_id,
    };
    let output = room
        .env
        .rimz()
        .args(rimz::child_process::agent_helper_argv(
            "refresh-usage",
            &request,
        ))
        .env("RIMZ_OAUTH_USAGE_OFFLINE", "0")
        .env("RIMZ_CODEX_BIN", codex_appserver_stub())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    room.assert_only_the_handshake_reached_the_prefix_broker();
    assert_eq!(room.rimio_plan_in_credits().as_deref(), Some("pro"));
}

#[test]
fn session_context_refresh_ignores_a_broker_that_cannot_name_its_child() {
    let room = rimio_room_behind_a_prefix_broker();
    let workspace = room.env.resolve_workspace(&room.env.project_root);
    let request = rimz::agents::LifecycleRefreshRequest {
        kind: rimz::ids::AgentKind::new_unchecked("codex"),
        session_id: "rimio-session".into(),
        workspace_id: workspace.workspace_id,
        model: None,
        server_url: None,
    };
    let output = room
        .env
        .rimz()
        .args(rimz::child_process::agent_helper_argv(
            "refresh-context",
            &request,
        ))
        .env("RIMZ_OAUTH_USAGE_OFFLINE", "0")
        .env("RIMZ_CODEX_BIN", codex_appserver_stub())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    room.assert_only_the_handshake_reached_the_prefix_broker();
    assert_eq!(room.rimio_plan_in_credits().as_deref(), Some("pro"));
    let plans: Vec<_> = room
        .env
        .agent_contexts()
        .iter()
        .filter_map(|record| record.context.account.as_ref()?.plan.clone())
        .collect();
    assert_eq!(plans, ["pro"], "the sidecar's account plan");
}

/// One JSON-RPC round-trip over the broker socket: write a framed request, then
/// read frames until the one matching `id` and return its `result`.
fn rpc(writer: &mut UnixStream, reader: &mut impl BufRead, id: i64, method: &str) -> Value {
    let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method });
    writeln!(writer, "{frame}").expect("write request");
    writer.flush().expect("flush request");
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).expect("read response");
        assert!(read > 0, "broker closed before answering id {id}");
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if value.get("id").and_then(Value::as_i64) == Some(id) {
            return value.get("result").cloned().unwrap_or(Value::Null);
        }
    }
}

/// Poll until the broker's socket exists and accepts a connection, or time out.
fn connect_with_deadline(socket: &Path, timeout: Duration) -> Option<UnixStream> {
    let deadline = Instant::now() + timeout;
    loop {
        if socket.exists()
            && let Ok(stream) = UnixStream::connect(socket)
        {
            return Some(stream);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn broker_serves_a_warm_app_server_over_its_socket() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    // `rimz start` records the room before it launches the broker, which
    // resolves its dirs by id.
    env.record(&env.project_root);
    let stub = codex_appserver_stub();

    // Launch the broker as `rimz start` would, with the stub standing in for the
    // real `codex`. `env.rimz()` scopes `XDG_RUNTIME_DIR`, which roots the socket.
    let mut broker = env
        .rimz()
        .args([
            "codex",
            "app-server",
            "serve",
            "--workspace-id",
            env.workspace_id.as_str(),
        ])
        .env("RIMZ_CODEX_BIN", &stub)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn broker");

    let socket = env.runtime_paths().codex_app_server_socket_path();
    // Drive the round-trips inside a guard so the broker child is always reaped,
    // even on assertion failure.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut stream = connect_with_deadline(&socket, Duration::from_secs(10))
            .expect("broker socket should appear and accept a connection");
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        // `initialize` is answered from the broker's cached handshake (the stub's
        // userAgent), so the client skips the round-trip to the child.
        let init = rpc(&mut stream, &mut reader, 1, "initialize");
        assert_eq!(
            init.get("userAgent").and_then(Value::as_str),
            Some("rimz/9.9.9 (Test 1.0; x86_64)"),
            "initialize served from cache: {init}",
        );

        // Read-only methods are forwarded to the warm child and routed back by id.
        let limits = rpc(&mut stream, &mut reader, 2, "account/rateLimits/read");
        assert_eq!(
            limits["rateLimits"]["primary"]["usedPercent"],
            json!(42),
            "rate limits forwarded: {limits}",
        );
        let models = rpc(&mut stream, &mut reader, 3, "model/list");
        assert_eq!(
            models["data"][0]["displayName"],
            json!("GPT-5.5 Codex"),
            "model list forwarded: {models}",
        );
    }));

    let _ = broker.kill();
    let _ = broker.wait();
    outcome.expect("broker round-trips succeed");
}

#[test]
fn broker_respawns_after_credentials_stamp_changes() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    // `rimz start` records the room before it launches the broker, which
    // resolves its dirs by id.
    env.record(&env.project_root);
    let codex_home = tempfile::tempdir().expect("codex home");
    let auth_path = codex_home.path().join("auth.json");
    let auth_a = r#"{"marker":"A"}"#;
    let auth_b = r#"{"marker":"B"}"#;
    let mtime = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    std::fs::write(&auth_path, auth_a).expect("write original auth");
    File::open(&auth_path)
        .expect("open original auth")
        .set_times(FileTimes::new().set_modified(mtime))
        .expect("set original auth stamp");

    let mut broker = env
        .rimz()
        .args([
            "codex",
            "app-server",
            "serve",
            "--workspace-id",
            env.workspace_id.as_str(),
        ])
        .env("RIMZ_CODEX_BIN", codex_appserver_stub())
        .env("CODEX_HOME", codex_home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn broker");

    let socket = env.runtime_paths().codex_app_server_socket_path();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut stream = connect_with_deadline(&socket, Duration::from_secs(10))
            .expect("broker socket should appear and accept a connection");
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

        let init = rpc(&mut stream, &mut reader, 1, "initialize");
        assert_eq!(init["rimzFixture"]["auth"], auth_a);
        let original_pid = init["rimzFixture"]["pid"].as_u64().expect("child pid");
        let limits = rpc(&mut stream, &mut reader, 2, "account/rateLimits/read");
        assert_eq!(limits["rimzFixture"], init["rimzFixture"]);

        std::fs::write(&auth_path, auth_b).expect("write replacement auth");
        File::open(&auth_path)
            .expect("open replacement auth")
            .set_times(FileTimes::new().set_modified(mtime + Duration::from_secs(1)))
            .expect("set replacement auth stamp");

        // `initialize` re-resolves the login too, so the changed stamp respawns the child
        // before the handshake is answered from the fresh child's cache.
        let replacement_init = rpc(&mut stream, &mut reader, 3, "initialize");
        assert_eq!(replacement_init["rimzFixture"]["auth"], auth_b);
        let replacement_pid = replacement_init["rimzFixture"]["pid"]
            .as_u64()
            .expect("replacement child pid");
        assert_ne!(replacement_pid, original_pid);
        let replacement = rpc(&mut stream, &mut reader, 4, "account/rateLimits/read");
        assert_eq!(replacement["rimzFixture"], replacement_init["rimzFixture"]);

        let stable = rpc(&mut stream, &mut reader, 5, "account/rateLimits/read");
        assert_eq!(stable["rimzFixture"], replacement["rimzFixture"]);
    }));

    let _ = broker.kill();
    let _ = broker.wait();
    outcome.expect("broker credential-stamp round-trips succeed");
}
