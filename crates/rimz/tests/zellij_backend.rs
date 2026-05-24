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
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::feed::PaneRef;
use rimz::ids::{MuxName, WorkspaceId};
use rimz::mux::{MuxBackend, PaneListOptions, SidebarPaneOptions, ZellijBackend, zellij};
use tempfile::TempDir;

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
        wait_until_session_listed(&session.name);
        session
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

struct ZellijSessionCleanup {
    name: String,
}

impl ZellijSessionCleanup {
    fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl Drop for ZellijSessionCleanup {
    fn drop(&mut self) {
        let _ = std::process::Command::new("zellij")
            .args(["delete-session", &self.name, "--force"])
            .output();
    }
}

/// Poll `zellij list-sessions` until our name appears. Sessions take
/// 300–800 ms to register on a quiet host; we give it 30 s for slow
/// CI machines. We grep with `contains` because the line is wrapped in
/// ANSI color codes on Zellij 0.41+.
fn wait_until_session_listed(name: &str) {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        if Instant::now() > deadline {
            panic!("zellij session {name} never appeared in list-sessions");
        }
        let listed = std::process::Command::new("zellij")
            .arg("list-sessions")
            .output();
        if let Ok(out) = listed
            && out.status.success()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            if text.contains(name) {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn unique_session_name(prefix: &str) -> String {
    let id = uuid::Uuid::now_v7().simple().to_string();
    format!("rimz-{prefix}-{}", &id[..12])
}

fn sidebar_command_stub() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("stub dir");
    let path = dir.path().join("rimz-stub");
    std::fs::write(&path, "#!/bin/sh\nsleep 5\n").expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
    }
    (dir, path)
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

/// `open_sidebar` births the session from a layout: a left ~30% native
/// `rimz-sidebar` pane plus a focused terminal pane. No pre-create, no WASM
/// plugin, no post-creation move/resize.
#[test]
fn open_sidebar_creates_native_pane() {
    require_zellij!();

    let _serial = zellij_test_lock()
        .lock()
        .expect("zellij test lock poisoned");
    let name = unique_session_name("sidebar");
    let _cleanup = ZellijSessionCleanup::new(&name);
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_command_stub();
    ZellijBackend
        .open_sidebar(&SidebarPaneOptions {
            session_name: name.clone(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-sidebar-test")),
            cwd: cwd.path().to_path_buf(),
            width_percent: 30,
            rimz_bin: stub,
            session_preexisting: false,
        })
        .expect("open_sidebar");

    let panes = wait_for_pane_count(&name, 2);
    assert!(
        panes.len() >= 2,
        "layout should create a sidebar + terminal pane in {name}: {panes:?}",
    );
    assert_sidebar_is_left_thirty_percent(&name);
}

/// End-to-end self-close: a real `rimz-sidebar` shares a tab with a terminal
/// pane that exits on its own. The sidebar polls `rimz pane list`, sees it is
/// alone, and exits; being `close_on_exit`, its pane then closes. We assert the
/// lone sidebar removes its own pane — the tab drops from two terminal panes to
/// zero. (Tearing down the now-empty tab/session is the multiplexer's job once
/// a client is attached; a never-attached background session lingers empty.)
#[test]
fn sidebar_self_closes_when_its_tab_empties() {
    require_zellij!();

    let rimz = assert_cmd::cargo::cargo_bin("rimz");
    let sidebar = assert_cmd::cargo::cargo_bin("rimz-sidebar");
    if !rimz.exists() || !sidebar.exists() {
        eprintln!("rimz/rimz-sidebar binaries not built; skipping self-close test");
        return;
    }

    let _serial = zellij_test_lock()
        .lock()
        .expect("zellij test lock poisoned");
    let name = unique_session_name("selfclose");
    let cwd = TempDir::new().expect("cwd tempdir");
    // One short XDG_RUNTIME_DIR for everything: zellij's *server* socket and
    // rimz's *wakeup* socket both live under it, so every zellij call touching
    // this session must share it — and it must stay short enough that the
    // workspace + 35-char instance id keep the socket under the 108-byte
    // AF_UNIX limit. A `prefix("rz")` tempdir buys that headroom.
    let xdg = tempfile::Builder::new()
        .prefix("rz")
        .rand_bytes(6)
        .tempdir()
        .expect("xdg tempdir");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };

    let layout = self_close_layout(&name, &rimz, &sidebar, xdg.path());
    let layout_path = cwd.path().join("layout.kdl");
    std::fs::write(&layout_path, layout).expect("write layout");

    let created = scoped_zellij(xdg.path())
        .args(["attach", "--create-background", &name, "options"])
        .arg("--default-cwd")
        .arg(cwd.path())
        .arg("--default-layout")
        .arg(&layout_path)
        .status()
        .expect("create background session");
    assert!(created.success(), "create-background failed for {name}");

    assert!(
        wait_for_nonplugin_panes(xdg.path(), &name, 2, Duration::from_secs(15)),
        "expected sidebar + terminal before self-close for {name}",
    );
    assert!(
        wait_for_nonplugin_panes(xdg.path(), &name, 0, Duration::from_secs(30)),
        "lone sidebar did not close its own pane after the terminal exited for {name}",
    );
}

/// Layout for the self-close test: a real `rimz-sidebar` renderer on the left
/// (env-scoped to a throwaway XDG dir) and a terminal that exits after a beat.
/// Both panes are `close_on_exit`, so each disappears when its command ends.
fn self_close_layout(session: &str, rimz: &Path, sidebar: &Path, xdg: &Path) -> String {
    let q = |s: String| serde_json::to_string(&s).expect("kdl escape");
    let serve = format!(
        "XDG_STATE_HOME={xdg} XDG_RUNTIME_DIR={xdg} RIMZ_BIN={rimz} \
         exec {sidebar} serve --mux zellij --workspace-id ws_0123456789abcdef01234567 \
         --session-name {session} --tick-seconds 1",
        xdg = xdg.display(),
        rimz = rimz.display(),
        sidebar = sidebar.display(),
    );
    format!(
        r#"layout {{
    default_tab_template split_direction="vertical" {{
        pane size="30%" name="rimz-sidebar" {{
            command "sh"
            args "-c" {serve}
            close_on_exit true
        }}
        children
    }}
    tab name="rimz" {{
        pane focus=true {{
            command "sleep"
            args "3"
            close_on_exit true
        }}
    }}
}}
"#,
        serve = q(serve),
    )
}

/// Poll `list_panes` until at least `want` panes appear (bounded). Returns the
/// last observation either way so the caller can assert and print it.
fn wait_for_pane_count(session: &str, want: usize) -> Vec<PaneRef> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let panes = ZellijBackend
            .list_panes(PaneListOptions {
                session_name: Some(session.to_owned()),
            })
            .unwrap_or_default();
        if panes.len() >= want || Instant::now() >= deadline {
            return panes;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A `zellij` command pinned to a specific `XDG_RUNTIME_DIR`. Zellij locates
/// its server socket there, so the self-close test creates, inspects, and
/// tears down its session through this one runtime dir.
fn scoped_zellij(xdg: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("zellij");
    cmd.env("XDG_RUNTIME_DIR", xdg);
    cmd
}

/// Tear down a runtime-scoped session even if an assertion panics first.
struct ScopedSessionCleanup {
    name: String,
    xdg: PathBuf,
}

impl Drop for ScopedSessionCleanup {
    fn drop(&mut self) {
        let _ = scoped_zellij(&self.xdg)
            .args(["delete-session", &self.name, "--force"])
            .output();
    }
}

/// Count a runtime-scoped session's non-plugin (terminal) panes. A session
/// whose tab has emptied answers `action list-panes` with only plugin panes
/// (tab/status bars); a torn-down session fails the call. Both map to zero.
fn session_nonplugin_count(xdg: &Path, name: &str) -> usize {
    scoped_zellij(xdg)
        .args(["--session", name, "action", "list-panes", "-j", "-a"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| serde_json::from_slice::<serde_json::Value>(&out.stdout).ok())
        .and_then(|panes| {
            panes.as_array().map(|panes| {
                panes
                    .iter()
                    .filter(|pane| {
                        pane.get("is_plugin").and_then(|b| b.as_bool()) == Some(false)
                            && pane.get("is_suppressed").and_then(|b| b.as_bool()) != Some(true)
                    })
                    .count()
            })
        })
        .unwrap_or(0)
}

/// Poll until the session's non-plugin pane count equals `target`, or the
/// timeout elapses.
fn wait_for_nonplugin_panes(xdg: &Path, name: &str, target: usize, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if session_nonplugin_count(xdg, name) == target {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn assert_sidebar_is_left_thirty_percent(session: &str) {
    let output = std::process::Command::new("zellij")
        .args(["--session", session, "action", "list-panes", "-j", "-a"])
        .output()
        .expect("list-panes geometry");
    assert!(output.status.success(), "list-panes geometry failed");
    let panes: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("list-panes geometry json");
    let panes = panes.as_array().expect("pane geometry array");
    let sidebar = panes
        .iter()
        .find(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
        })
        .expect("rimz-sidebar pane");
    let tab_id = sidebar
        .get("tab_id")
        .and_then(|value| value.as_u64())
        .expect("sidebar tab id");
    let columns = sidebar
        .get("pane_columns")
        .and_then(|value| value.as_u64())
        .expect("sidebar columns");
    let total_columns = panes
        .iter()
        .filter(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("tab_id").and_then(|value| value.as_u64()) == Some(tab_id)
        })
        .filter_map(|pane| {
            Some(pane.get("pane_x")?.as_u64()? + pane.get("pane_columns")?.as_u64()?)
        })
        .max()
        .expect("tab width");
    assert_eq!(
        sidebar.get("pane_x").and_then(|value| value.as_u64()),
        Some(0),
        "sidebar should be the left pane",
    );
    assert!(
        columns * 100 <= total_columns * 35,
        "sidebar should occupy roughly 30% of the tab: {columns}/{total_columns}",
    );
}

/// `wake_sidebar` issues `zellij --session <name> pipe --name rimz::feed --
/// <payload>`. We assert the subprocess returns success even when no
/// pipe-aware client consumes the payload.
#[test]
fn wake_sidebar_pipe_invocation_succeeds() {
    require_zellij!();

    let name = unique_session_name("pipe");
    let _session = ZellijSession::spawn(&name);

    let payload = br#"{"kind":"ledger_delta","workspace_id":"ws_test","request_id":"req_test","protocol_version":"rimz.plugin.v1"}"#;
    ZellijBackend
        .wake_sidebar(&name, payload)
        .expect("wake_sidebar succeeds against a live zellij session");
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
