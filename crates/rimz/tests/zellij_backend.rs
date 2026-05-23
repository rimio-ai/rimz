//! Live Zellij backend tests for the M0b spike.
//!
//! Each test spawns a real `zellij` session in a portable-pty and runs the
//! `ZellijBackend` against it. The whole file becomes a no-op (early-return
//! per test, message printed once) when the `zellij` binary is not on PATH.
//! The trace-shim wakeup-walk test that verifies the broadcast `zellij
//! pipe` invocation lives in a separate file (`wakeup_pipe.rs`) so its env
//! mutation does not race with these tests.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Read;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::ids::MuxName;
use rimz::mux::{MuxBackend, PaneListOptions, ZellijBackend, zellij};

const SPAWN_TIMEOUT: Duration = Duration::from_secs(30);

/// Skip the test (return) if the host has no `zellij` binary on PATH.
macro_rules! require_zellij {
    () => {
        if which::which("zellij").is_err() {
            eprintln!("zellij not on PATH; skipping test");
            return;
        }
    };
}

/// Owns a live Zellij session for the duration of one test. Spawned via a
/// portable-pty so the child has the terminal it expects; the master is
/// kept alive (and silently drained) to avoid SIGHUP'ing the session.
struct ZellijSession {
    name: String,
    _serial: MutexGuard<'static, ()>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    _reader_thread: Option<std::thread::JoinHandle<()>>,
}

impl ZellijSession {
    fn spawn(name: impl Into<String>) -> Self {
        let serial = zellij_test_lock()
            .lock()
            .expect("zellij test lock poisoned");
        let name = name.into();
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new("zellij");
        cmd.args(["attach", "--create", &name]);
        let child = pair.slave.spawn_command(cmd).expect("spawn zellij");
        drop(pair.slave);

        // Drain the PTY in the background so the kernel buffer never fills
        // and stalls the child. We do not parse anything; the channel of
        // record is the `zellij action ...` round-trip.
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => continue,
                }
            }
        });

        let session = Self {
            name,
            _serial: serial,
            _master: pair.master,
            _child: child,
            _reader_thread: Some(reader_thread),
        };
        session.wait_until_listed();
        session
    }

    /// Poll `zellij list-sessions` until our name appears. Sessions take
    /// 300–800 ms to register on a quiet host; we give it 10 s for slow
    /// CI machines. We grep with `contains` because the line is wrapped in
    /// ANSI color codes on Zellij 0.41+.
    fn wait_until_listed(&self) {
        let deadline = Instant::now() + SPAWN_TIMEOUT;
        loop {
            if Instant::now() > deadline {
                panic!(
                    "zellij session {} never appeared in list-sessions",
                    self.name,
                );
            }
            let listed = std::process::Command::new("zellij")
                .arg("list-sessions")
                .output();
            if let Ok(out) = listed
                && out.status.success()
            {
                let text = String::from_utf8_lossy(&out.stdout);
                if text.contains(&self.name) {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

fn zellij_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

impl Drop for ZellijSession {
    fn drop(&mut self) {
        let _ = std::process::Command::new("zellij")
            .args(["delete-session", &self.name, "--force"])
            .output();
    }
}

fn unique_session_name(prefix: &str) -> String {
    let id = uuid::Uuid::now_v7().simple().to_string();
    format!("rimz-{prefix}-{}", &id[..12])
}

/// Sanity: spawn a session, see it in `list_sessions`. Establishes that the
/// portable-pty harness can reach a usable Zellij.
#[test]
fn ensure_and_list_sessions_round_trip() {
    require_zellij!();

    let name = unique_session_name("list");
    let _session = ZellijSession::spawn(&name);

    let listed = ZellijBackend
        .list_sessions()
        .expect("list_sessions succeeds against a live zellij");
    assert!(
        listed.iter().any(|s| s == &name),
        "expected session {name} in {listed:?}",
    );
}

/// `open_sidebar` must surface `MuxErr::NotInstalled` when the WASM plugin
/// file is absent. The path resolves via XDG; we don't mutate env (forbidden
/// by `unsafe_code = "forbid"`), so we just assert against whatever the
/// host has — if the file happens to exist locally, the call must succeed
/// instead. Either branch proves the pre-flight check fires.
#[test]
fn open_sidebar_pre_flight_matches_disk_presence() {
    require_zellij!();

    let name = unique_session_name("sidebar");
    let _session = ZellijSession::spawn(&name);

    let plugin_path = zellij::sidebar_plugin_path();
    match ZellijBackend.open_sidebar(&name, 30) {
        Ok(()) => {
            assert!(
                plugin_path.is_file(),
                "open_sidebar reported Ok but plugin path {} is missing",
                plugin_path.display(),
            );
        }
        Err(rimz::mux::MuxErr::NotInstalled { program }) => {
            assert!(
                !plugin_path.is_file(),
                "open_sidebar reported NotInstalled({program}) but plugin path {} exists",
                plugin_path.display(),
            );
            assert_eq!(program, "rimz-sidebar.wasm");
        }
        Err(other) => panic!("unexpected open_sidebar error: {other:?}"),
    }
}

/// `wake_sidebar` issues `zellij --session <name> pipe --name rimz::feed --
/// <payload>`. Without a live plugin to receive, Zellij still accepts the
/// pipe; we assert the subprocess returns success.
#[test]
fn wake_sidebar_pipe_invocation_succeeds() {
    require_zellij!();

    let name = unique_session_name("pipe");
    let _session = ZellijSession::spawn(&name);

    let payload = br#"{"kind":"ledger_delta","workspace_id":"ws_test","request_id":"req_test","protocol_version":"rimz.plugin.v1"}"#;
    ZellijBackend
        .wake_sidebar(&name, payload)
        .expect("wake_sidebar succeeds against a live zellij with no plugin");
}

/// `list_panes` parses `zellij action list-panes -j -a` JSON. A fresh
/// session has at least one terminal pane (the implicit shell).
#[test]
fn list_panes_with_session_returns_terminals() {
    require_zellij!();

    let name = unique_session_name("panes");
    let _session = ZellijSession::spawn(&name);

    // Zellij needs a beat between session-ready and a queryable layout.
    std::thread::sleep(Duration::from_millis(400));

    let panes = ZellijBackend
        .list_panes(PaneListOptions {
            session_name: Some(name.clone()),
        })
        .expect("list_panes against a live zellij");
    assert!(
        !panes.is_empty(),
        "expected ≥1 terminal pane in fresh session {name}, got {panes:?}",
    );
    for pane in &panes {
        assert_eq!(pane.pane_id.mux(), MuxName::Zellij);
        assert!(
            pane.pane_id.raw().starts_with("terminal_"),
            "list_panes should filter plugins out; got {}",
            pane.pane_id,
        );
        assert_eq!(pane.session_name, name);
    }
}

/// Capability probe must parse the binary's version string and compare it
/// against `MIN_ZELLIJ_VERSION`. No session required.
#[test]
fn version_floor_parses_and_compares() {
    require_zellij!();

    let caps = zellij::capabilities().expect("capabilities() against a live zellij");
    let (maj, min, patch) = caps
        .parsed_version
        .expect("parsed_version is Some for any 0.41+ build");
    assert!(
        (maj, min, patch) >= zellij::MIN_ZELLIJ_VERSION,
        "test host has zellij {maj}.{min}.{patch}; M0b requires ≥ {:?}",
        zellij::MIN_ZELLIJ_VERSION,
    );
    assert!(caps.meets_min_version);
    assert!(caps.binary_version.contains("zellij"));
}
