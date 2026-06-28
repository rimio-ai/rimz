//! Verifies the wakeup-walk contract: after a ledger write, the walk fans out a
//! per-instance UDP datagram to every fresh sidebar — and does **not** shell out
//! to `zellij`. The broadcast `zellij pipe` it used to issue alongside the
//! datagram had no consumer — the native pane wakes over the socket — so
//! spawning a `zellij` subprocess per write per session was pure cost and was
//! removed. This locks in that the datagram still arrives and that no
//! `zellij pipe` is spawned.
//!
//! No live Zellij needed — the `rimz` subprocess we spawn here gets
//! `RIMZ_ZELLIJ_BIN` pointed at a `zellij-trace` shim built by Cargo from
//! `crates/rimz/tests/fixtures/zellij-trace/main.rs`. The shim logs its
//! argv to `RIMZ_TEST_ZELLIJ_LOG` and exits 0; we assert it is never invoked.
//!
//! `unsafe_code = "forbid"` is workspace-wide and includes test targets, so
//! we cannot mutate process env from inside this test. Instead we run the
//! whole ledger write through a `rimz feed push` subprocess and seed its
//! env. The subprocess constructs its own `Ledger` rooted at the test's
//! `XDG_STATE_HOME`/`XDG_RUNTIME_DIR` overrides, walks the heartbeats we
//! planted there, and sends one wakeup datagram per fresh sidebar — never
//! shelling out to the trace shim.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rimz::ids::{MuxName, SidebarInstanceId, WorkspaceId};
use rimz::ledger::RuntimePaths;
use rimz::schema::heartbeat::SidebarHeartbeat;
use tempfile::TempDir;

use crate::common::ScrubSessionEnvExt;

const SESSION_NAME: &str = "rimz-wakeup-pipe-test";

#[test]
fn wakeup_walk_sends_datagram_and_spawns_no_zellij_pipe() {
    let trace_bin = trace_shim_path();
    assert_trace_shim_exists(&trace_bin);
    let rimz_bin = rimz_cli_path();

    let Some(fixture) = wakeup_fixture() else {
        return;
    };

    run_wakeup_trigger(&rimz_bin, &trace_bin, &fixture);

    // The datagram — the channel of record — reaches *both* fresh sidebars on the
    // session: the walk fans out one datagram per instance, which is what replaced
    // the old per-session pipe dedup. Each payload is the typed `LedgerDelta` event
    // the renderer folds.
    assert_ledger_delta(&fixture.recv_a, "sidebar.a", &fixture.workspace_id);
    assert_ledger_delta(&fixture.recv_b, "sidebar.b", &fixture.workspace_id);

    // The walk must not shell out to `zellij`: the consumerless pipe broadcast
    // was removed, so the trace shim is never invoked. The push has returned, so
    // any (erroneous) synchronous pipe spawn would already be logged.
    assert_no_zellij_pipe_spawned(&fixture.log_path);
    drop(fixture.tempdir);
}

struct WakeupFixture {
    tempdir: TempDir,
    project_root: PathBuf,
    state_root: PathBuf,
    runtime_root: PathBuf,
    log_path: PathBuf,
    workspace_id: WorkspaceId,
    recv_a: std::os::unix::net::UnixDatagram,
    recv_b: std::os::unix::net::UnixDatagram,
}

fn assert_trace_shim_exists(trace_bin: &Path) {
    assert!(
        trace_bin.is_file(),
        "zellij-trace shim binary not found at {}; \
         `cargo xtask test` should build it via the `[[bin]] name = \"zellij-trace\"` \
         declaration in crates/rimz/Cargo.toml",
        trace_bin.display(),
    );
}

fn wakeup_fixture() -> Option<WakeupFixture> {
    let tempdir = TempDir::new().expect("tempdir");
    let project_root = tempdir.path().join("project");
    let state_root = tempdir.path().join("state");
    let runtime_root = tempdir.path().join("runtime");
    let log_path = tempdir.path().join("zellij-trace.log");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    std::fs::create_dir_all(&state_root).expect("mkdir state");
    std::fs::create_dir_all(&runtime_root).expect("mkdir runtime");
    let workspace_id = WorkspaceId::from_project_root(&project_root);
    let runtime =
        RuntimePaths::under(workspace_id.clone(), &runtime_root).expect("RuntimePaths::under");
    runtime.ensure_dirs().expect("ensure runtime dirs");
    if crate::common::af_unix_bind_sandboxed(&runtime.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return None;
    }
    let (recv_a, recv_b) = bind_wakeup_receivers(&runtime, &workspace_id);
    Some(WakeupFixture {
        tempdir,
        project_root,
        state_root,
        runtime_root,
        log_path,
        workspace_id,
        recv_a,
        recv_b,
    })
}

fn bind_wakeup_receivers(
    runtime: &RuntimePaths,
    workspace_id: &WorkspaceId,
) -> (
    std::os::unix::net::UnixDatagram,
    std::os::unix::net::UnixDatagram,
) {
    let sock_a = runtime.sock_dir.join("sidebar.a.sock");
    let recv_a = bind_receiver(&sock_a, "sock_a");
    let sock_b = runtime.sock_dir.join("sidebar.b.sock");
    let recv_b = bind_receiver(&sock_b, "sock_b");
    write_sidebar_heartbeat(runtime, "sidebar.a.json", workspace_id, sock_a);
    write_sidebar_heartbeat(runtime, "sidebar.b.json", workspace_id, sock_b);
    (recv_a, recv_b)
}

fn bind_receiver(path: &Path, name: &str) -> std::os::unix::net::UnixDatagram {
    let recv =
        std::os::unix::net::UnixDatagram::bind(path).unwrap_or_else(|_| panic!("bind {name}"));
    recv.set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    recv
}

fn write_sidebar_heartbeat(
    runtime: &RuntimePaths,
    filename: &str,
    workspace_id: &WorkspaceId,
    socket: PathBuf,
) {
    write_heartbeat(
        runtime,
        filename,
        SidebarHeartbeat::new(
            workspace_id.clone(),
            SidebarInstanceId::new(),
            MuxName::Zellij,
            SESSION_NAME,
            socket,
            None,
        ),
    );
}

fn run_wakeup_trigger(rimz_bin: &Path, trace_bin: &Path, fixture: &WakeupFixture) {
    let output = Command::new(rimz_bin)
        .scrub_session_env()
        .args([
            "feed",
            "push",
            "--kind",
            "generic",
            "--title",
            "trigger wakeup",
        ])
        .current_dir(&fixture.project_root)
        .env("XDG_STATE_HOME", &fixture.state_root)
        .env("XDG_RUNTIME_DIR", &fixture.runtime_root)
        .env("RIMZ_ZELLIJ_BIN", trace_bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &fixture.log_path)
        .output()
        .expect("spawn rimz feed push");
    assert!(
        output.status.success(),
        "rimz feed push failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
}

fn assert_ledger_delta(
    recv: &std::os::unix::net::UnixDatagram,
    who: &str,
    workspace_id: &WorkspaceId,
) {
    let mut buf = [0u8; 4096];
    let len = recv
        .recv(&mut buf)
        .unwrap_or_else(|err| panic!("the wakeup walk sends a datagram to {who}: {err}"));
    let parsed: serde_json::Value =
        serde_json::from_slice(&buf[..len]).expect("datagram payload is JSON");
    assert_eq!(
        parsed["v"],
        rimz::schema::sidebar_event::SIDEBAR_EVENT_VERSION
    );
    assert_eq!(
        parsed["workspace_id"],
        serde_json::to_value(workspace_id).expect("workspace_id JSON"),
    );
    assert!(parsed.get("session_name").is_none());
    assert!(parsed["sent_at_ms"].as_u64().is_some());
    assert_eq!(parsed["event"]["kind"], "ledger_delta");
}

fn assert_no_zellij_pipe_spawned(log_path: &Path) {
    let lines = read_trace_lines(log_path, Duration::from_millis(200));
    assert!(
        lines.is_empty(),
        "the wakeup walk must not spawn a zellij pipe, but the trace shim logged: {lines:?}",
    );
}

fn write_heartbeat(runtime: &RuntimePaths, filename: &str, hb: SidebarHeartbeat) {
    std::fs::write(
        runtime.heartbeat_dir.join(filename),
        serde_json::to_vec(&hb).expect("serialize heartbeat"),
    )
    .expect("write heartbeat");
}

fn trace_shim_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace"))
    })
    .clone()
}

fn rimz_cli_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz")))
        .clone()
}

fn read_trace_lines(log_path: &Path, timeout: Duration) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(bytes) = std::fs::read(log_path) {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<String> = text
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect();
            if !lines.is_empty() {
                return lines;
            }
        }
        if Instant::now() > deadline {
            return Vec::new();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
