//! Verifies the M0b wiring: after a ledger write, the wakeup walk dispatches
//! a broadcast `zellij pipe` for every Zellij heartbeat in addition to the
//! per-sidebar UDP datagrams.
//!
//! No live Zellij needed — the `rimz` subprocess we spawn here gets
//! `RIMZ_ZELLIJ_BIN` pointed at a `zellij-trace` shim built by Cargo from
//! `crates/rimz/tests/fixtures/zellij-trace/main.rs`. The shim logs its
//! argv to `RIMZ_TEST_ZELLIJ_LOG` and exits 0.
//!
//! `unsafe_code = "forbid"` is workspace-wide and includes test targets, so
//! we cannot mutate process env from inside this test. Instead we run the
//! whole ledger write through a `rimz feed push` subprocess and seed its
//! env. The subprocess constructs its own `Ledger` rooted at the test's
//! `XDG_STATE_HOME`/`XDG_RUNTIME_DIR` overrides, walks the heartbeats we
//! planted there, and invokes the trace shim once per Zellij heartbeat
//! session (deduped).

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rimz::ids::{MuxName, SidebarInstanceId, WorkspaceId};
use rimz::ledger::RuntimePaths;
use rimz::schema::heartbeat::SidebarHeartbeat;
use tempfile::TempDir;

const SESSION_NAME: &str = "rimz-wakeup-pipe-test";

#[test]
fn wakeup_walk_dispatches_zellij_pipe_for_zellij_heartbeat() {
    let trace_bin = trace_shim_path();
    assert!(
        trace_bin.is_file(),
        "zellij-trace shim binary not found at {}; \
         `cargo test` should build it via the `[[bin]] name = \"zellij-trace\"` \
         declaration in crates/rimz/Cargo.toml",
        trace_bin.display(),
    );
    let rimz_bin = rimz_cli_path();

    let tempdir = TempDir::new().expect("tempdir");
    let project_root = tempdir.path().join("project");
    let state_root = tempdir.path().join("state");
    let runtime_root = tempdir.path().join("runtime");
    let log_path = tempdir.path().join("zellij-trace.log");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    std::fs::create_dir_all(&state_root).expect("mkdir state");
    std::fs::create_dir_all(&runtime_root).expect("mkdir runtime");

    // Compute the workspace id the subprocess will derive and the runtime
    // paths it will use, then plant two Zellij heartbeats under the same
    // session to exercise the dedupe path.
    let workspace_id = WorkspaceId::from_project_root(&project_root);
    let runtime =
        RuntimePaths::under(workspace_id.clone(), &runtime_root).expect("RuntimePaths::under");
    runtime.ensure_dirs().expect("ensure runtime dirs");

    if crate::common::af_unix_bind_sandboxed(&runtime.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let sock_a = runtime.sock_dir.join("sidebar.a.sock");
    let _recv_a = std::os::unix::net::UnixDatagram::bind(&sock_a).expect("bind sock_a");
    let sock_b = runtime.sock_dir.join("sidebar.b.sock");
    let _recv_b = std::os::unix::net::UnixDatagram::bind(&sock_b).expect("bind sock_b");

    write_heartbeat(
        &runtime,
        "sidebar.a.json",
        SidebarHeartbeat::new(
            workspace_id.clone(),
            SidebarInstanceId::new(),
            MuxName::Zellij,
            SESSION_NAME,
            sock_a,
        ),
    );
    write_heartbeat(
        &runtime,
        "sidebar.b.json",
        SidebarHeartbeat::new(
            workspace_id.clone(),
            SidebarInstanceId::new(),
            MuxName::Zellij,
            SESSION_NAME,
            sock_b,
        ),
    );

    let output = Command::new(&rimz_bin)
        .args([
            "feed",
            "push",
            "--kind",
            "generic",
            "--title",
            "trigger wakeup",
        ])
        .current_dir(&project_root)
        .env("XDG_STATE_HOME", &state_root)
        .env("XDG_RUNTIME_DIR", &runtime_root)
        .env("RIMZ_ZELLIJ_BIN", &trace_bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &log_path)
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_PANE_ID")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .expect("spawn rimz feed push");
    assert!(
        output.status.success(),
        "rimz feed push failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let lines = read_trace_lines(&log_path, Duration::from_secs(2));
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one pipe invocation (dedupe by session), got: {lines:?}",
    );
    let argv: Vec<&str> = lines[0].split('\t').collect();
    assert_eq!(argv.first().copied(), trace_bin.to_str());
    assert!(argv.contains(&"pipe"), "expected `pipe` in argv: {argv:?}");
    assert!(
        argv.contains(&"--name") && argv.contains(&"rimz::feed"),
        "expected `--name rimz::feed` in argv: {argv:?}",
    );
    assert!(
        argv.contains(&"--session") && argv.contains(&SESSION_NAME),
        "expected `--session {SESSION_NAME}` in argv: {argv:?}",
    );
    let payload = argv.last().expect("payload present");
    let parsed: serde_json::Value = serde_json::from_str(payload).expect("payload is JSON");
    assert_eq!(parsed["kind"], "ledger_delta");
    assert_eq!(parsed["protocol_version"], "rimz.plugin.v2");
    assert_eq!(
        parsed["workspace_id"],
        serde_json::to_value(&workspace_id).expect("workspace_id JSON"),
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
    PATH.get_or_init(|| PathBuf::from(env!("CARGO_BIN_EXE_zellij-trace")))
        .clone()
}

fn rimz_cli_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| PathBuf::from(env!("CARGO_BIN_EXE_rimz")))
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
