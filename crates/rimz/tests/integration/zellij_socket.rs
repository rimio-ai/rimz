//! Regression coverage for Zellij IPC socket path overflow handling.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, Env};

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
        !stderr.contains("Reset now?"),
        "reset should not be offered: {stderr}"
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
fn doctor_reports_zellij_socket_headroom_and_fix() {
    let env = Env::new();
    let shim = FakeZellij::new(FakeZellijMode::Normal);
    let long_socket_dir = format!("/tmp/{}", "x".repeat(140));

    let output = env
        .rimz()
        .args(["--mux", "zellij", "doctor"])
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("zellij socket : TOO LONG"),
        "doctor should render socket verdict, got: {stdout}"
    );
    assert!(
        stdout.contains("export ZELLIJ_SOCKET_DIR=/tmp/zellij"),
        "doctor should render socket fix, got: {stdout}"
    );
}

#[derive(Clone, Copy)]
enum FakeZellijMode {
    Normal,
    SocketOverflowOnBirth,
}

impl FakeZellijMode {
    fn env_value(self) -> &'static str {
        match self {
            Self::Normal => "",
            Self::SocketOverflowOnBirth => "socket-overflow-on-birth",
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
        let log = home.path().join("zellij.log");
        fs::write(&log, "").expect("create fake zellij log");
        Self {
            _home: home,
            bin,
            log,
            mode,
        }
    }
}

fn zellij_trace_shim() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zellij-trace"))
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
