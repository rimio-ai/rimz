//! The per-session Codex app-server broker (`rimz codex app-server serve`).
//!
//! It holds one warm `codex app-server` — here the `codex-appserver-stub`
//! fixture, pointed at by `RIMZ_CODEX_BIN` — and serves it over the session's
//! unix socket: `initialize` is answered from the cached handshake, the
//! read-only methods are forwarded to the child and routed back by id.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{Value, json};

use crate::common::Env;

/// Absolute path to the built `codex app-server` stub fixture.
fn codex_appserver_stub() -> std::path::PathBuf {
    Command::cargo_bin("codex-appserver-stub")
        .expect("cargo-bin stub")
        .get_program()
        .to_owned()
        .into()
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
