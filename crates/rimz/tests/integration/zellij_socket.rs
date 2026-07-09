//! Regression coverage for Zellij pre-attach failure handling.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::workspace::WorkspaceResolver;
use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, Env, ScrubSessionEnvExt};

#[test]
fn socket_preflight_fails_before_calling_zellij() {
    let env = Env::new();
    let shim = FakeZellij::new(FakeZellijMode::Normal);
    let long_socket_dir = format!("/tmp/{}", "x".repeat(140));

    let output = env
        .rimz()
        .args(["--mux", "zellij", "start", "--no-attach"])
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_ZELLIJ_MODE", shim.mode.env_value())
        .env("ZELLIJ_SOCKET_DIR", long_socket_dir)
        .bounded_output()
        .expect("run rimz start");

    assert!(!output.status.success(), "start should fail preflight");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Zellij can't create this room's IPC socket"),
        "stderr should explain socket overflow, got: {stderr}"
    );
    assert!(
        stderr.contains("export ZELLIJ_SOCKET_DIR=/tmp/zellij"),
        "stderr should include the fix, got: {stderr}"
    );
    assert!(
        read_trace_lines(&shim.log, Duration::from_millis(100)).is_empty(),
        "preflight should run before any zellij command"
    );
}

#[test]
fn socket_stderr_classification_does_not_offer_reset_or_dump_argv() {
    let env = Env::new();
    let shim = FakeZellij::new(FakeZellijMode::SocketOverflowOnBirth);

    let output = env
        .rimz()
        .args(["--mux", "zellij", "start", "--no-attach"])
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_ZELLIJ_MODE", shim.mode.env_value())
        .bounded_output()
        .expect("run rimz start");

    assert!(
        !output.status.success(),
        "reactive socket overflow should fail before attach"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Zellij reported that this room's IPC socket path is too long"),
        "stderr should report zellij's socket failure without contradictory local headroom, got: {stderr}"
    );
    assert!(
        !stderr.contains("resetting the"),
        "reset should not run for socket overflow: {stderr}"
    );
    assert!(
        !stderr.contains("--default-mode"),
        "user-facing error should not dump zellij option argv: {stderr}"
    );
    let lines = read_trace_lines(&shim.log, Duration::from_millis(200));
    assert!(
        lines
            .iter()
            .any(|line| line.contains("attach\t--create-background")),
        "reactive path should reach zellij birth, got: {lines:?}"
    );
}

#[test]
fn terminal_stuck_room_resets_without_second_prompt() {
    let env = Env::new();
    let setup = env
        .rimz()
        .args(["setup", "--yes"])
        .bounded_output()
        .expect("run setup");
    assert!(
        setup.status.success(),
        "setup should seed config: {}",
        String::from_utf8_lossy(&setup.stderr)
    );
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("resolve");
    let shim = FakeZellij::new(FakeZellijMode::BirthFails);

    let output = rimz_start_pty_output(&env, &shim, &workspace.session_name);

    assert!(
        output.contains(&format!(
            "rimz: resetting the '{}' room to clear a wedged mux session...",
            workspace.session_name
        )),
        "terminal start should auto-reset a stuck room: {output}"
    );
    assert!(
        !output.contains("Reset now?"),
        "terminal start should not show a second reset prompt: {output}"
    );
}

#[test]
fn doctor_reports_zellij_socket_headroom_and_fix() {
    let env = Env::new();
    let shim = FakeZellij::new(FakeZellijMode::Normal);
    let long_socket_dir = format!("/tmp/{}", "x".repeat(140));

    let output = env
        .rimz()
        .args(["--mux", "zellij", "doctor", "--json"])
        .env("RIMZ_ZELLIJ_BIN", &shim.bin)
        .env("RIMZ_TEST_ZELLIJ_LOG", &shim.log)
        .env("RIMZ_TEST_ZELLIJ_MODE", shim.mode.env_value())
        .env("ZELLIJ_SOCKET_DIR", long_socket_dir)
        .bounded_output()
        .expect("run rimz doctor");

    assert!(
        output.status.success(),
        "doctor should report instead of failing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor --json emits valid json");
    let socket = &report["mux"]["ready"]["zellij_socket"];
    assert_eq!(
        socket["fits"], false,
        "doctor should report the socket overflow: {report}"
    );
    assert!(
        socket["fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("export ZELLIJ_SOCKET_DIR=/tmp/zellij")),
        "doctor should render the socket fix: {socket}"
    );
}

#[derive(Clone, Copy)]
enum FakeZellijMode {
    Normal,
    SocketOverflowOnBirth,
    BirthFails,
}

impl FakeZellijMode {
    fn env_value(self) -> &'static str {
        match self {
            Self::Normal => "",
            Self::SocketOverflowOnBirth => "socket-overflow-on-birth",
            Self::BirthFails => "birth-fails",
        }
    }
}

struct FakeZellij {
    _home: TempDir,
    bin: PathBuf,
    log: PathBuf,
    mode: FakeZellijMode,
}

impl FakeZellij {
    fn new(mode: FakeZellijMode) -> Self {
        let home = TempDir::new().expect("fake zellij home");
        let bin = zellij_trace_shim();
        let log_name = match mode {
            FakeZellijMode::Normal => "zellij.normal.log",
            FakeZellijMode::SocketOverflowOnBirth => "zellij.socket-overflow-on-birth.log",
            FakeZellijMode::BirthFails => "zellij.birth-fails.log",
        };
        let log = home.path().join(log_name);
        fs::write(&log, "").expect("create fake zellij log");
        fs::write(log.with_extension("mode"), mode.env_value()).expect("write fake zellij mode");
        Self {
            _home: home,
            bin,
            log,
            mode,
        }
    }
}

fn zellij_trace_shim() -> PathBuf {
    crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace"))
}

fn rimz_start_pty_output(env: &Env, shim: &FakeZellij, session_name: &str) -> String {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(env.rimz_bin());
    cmd.scrub_session_env();
    cmd.args([
        "--mux",
        "zellij",
        "start",
        env.project_root.to_str().expect("utf-8 project root"),
        "--no-attach",
    ]);
    cmd.env("XDG_STATE_HOME", env.state_root());
    cmd.env("XDG_RUNTIME_DIR", &env.runtime_root);
    cmd.env("XDG_CONFIG_HOME", env.config_root());
    cmd.env("HOME", &env.home_root);
    cmd.env("SHELL", "/bin/sh");
    cmd.env("PATH", shim.log.parent().expect("fake zellij home"));
    cmd.env("RIMZ_MESSAGE_INTERVAL_MS", "0");
    cmd.env("RIMZ_ZELLIJ_BIN", &shim.bin);
    cmd.env("RIMZ_TEST_ZELLIJ_LOG", &shim.log);
    cmd.env("RIMZ_TEST_ZELLIJ_MODE", shim.mode.env_value());
    cmd.env(
        "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
        format!("{session_name} [Created 1m ago] (EXITED - attach to resurrect)\n"),
    );
    cmd.env_remove("ENV");
    cmd.env_remove("BASH_ENV");
    cmd.env_remove("ZDOTDIR");
    cmd.env_remove("RUST_LOG");

    let mut child = pair.slave.spawn_command(cmd).expect("spawn rimz");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
        output
    });

    // CI runs the full `rimz start` birth/reset path under load; this assertion
    // cares that the process exits without waiting for a second prompt, not
    // that every preflight completes inside a short wall-clock budget.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exited = false;
    while Instant::now() < deadline {
        if child.try_wait().expect("poll rimz").is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(pair.master);
    let output =
        String::from_utf8_lossy(&reader_thread.join().expect("join pty reader")).into_owned();
    assert!(exited, "rimz start did not exit; output:\n{output}");
    output
}

fn read_trace_lines(log_path: &Path, timeout: Duration) -> Vec<String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Ok(bytes) = fs::read(log_path) {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<String> = text
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect();
            if !lines.is_empty() {
                return lines;
            }
        }
        if std::time::Instant::now() > deadline {
            return Vec::new();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
