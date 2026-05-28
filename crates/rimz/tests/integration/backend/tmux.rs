//! Live tmux backend tests for the M0c spike.
//!
//! Each test spawns its own tmux server on a tempdir socket so it never
//! collides with the user's running sessions. The whole file becomes a
//! no-op (early-return per test, message printed once) when the `tmux`
//! binary is not on PATH.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use rimz::ids::{MuxName, WorkspaceId};
use rimz::mux::tmux::{self, MIN_TMUX_VERSION};
use rimz::mux::{
    MuxBackend, PaneListOptions, SessionOptions, SidebarPaneOptions, SplitPaneOptions, TmuxBackend,
};
use tempfile::TempDir;

/// Skip the test (return) if the host has no `tmux` binary on PATH.
macro_rules! require_tmux {
    () => {
        if which::which("tmux").is_err() {
            eprintln!("tmux not on PATH; skipping test");
            return;
        }
    };
}

/// Owns an isolated tmux server for the duration of one test. The server
/// listens on a tempdir socket; Drop tears it down with `kill-server`.
struct TmuxServer {
    backend: TmuxBackend,
    socket: PathBuf,
    _tempdir: TempDir,
}

impl TmuxServer {
    fn new() -> Self {
        let tempdir = TempDir::new().expect("tempdir");
        let socket = tempdir.path().join("tmux.sock");
        Self {
            backend: TmuxBackend::with_socket(&socket),
            socket,
            _tempdir: tempdir,
        }
    }

    fn ensure_with_shell(&self, session: &str) {
        // Use `sh` as the pane process so send-keys/capture-pane have a
        // shell to talk to; the default `new-session` shell varies by host.
        // `.output()` captures stderr so test logs stay clean.
        Command::new("tmux")
            .args([
                "-S",
                self.socket.to_str().expect("utf8 socket"),
                "new-session",
                "-d",
                "-s",
                session,
                "sh",
            ])
            .output()
            .expect("spawn tmux new-session");
    }

    fn pane_current_path(&self, session: &str) -> String {
        let output = Command::new("tmux")
            .args([
                "-S",
                self.socket.to_str().expect("utf8 socket"),
                "display-message",
                "-p",
                "-t",
                session,
                "#{pane_current_path}",
            ])
            .output()
            .expect("spawn tmux display-message");
        assert!(
            output.status.success(),
            "display-message failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        // `.output()` captures stderr so the "no server" message from
        // tests that never started a server doesn't leak into test logs.
        let _ = Command::new("tmux")
            .args(["-S", self.socket.to_str().unwrap_or(""), "kill-server"])
            .output();
    }
}

/// Sanity: ensure a session, see it in `list_sessions`. Establishes that
/// the per-test socket gives us a usable tmux server.
#[test]
fn ensure_and_list_sessions_round_trip() {
    require_tmux!();

    let server = TmuxServer::new();
    let cwd = TempDir::new().expect("cwd tempdir");
    server
        .backend
        .ensure_session(&SessionOptions {
            session_name: "rimz-test".to_owned(),
            cwd: cwd.path().to_path_buf(),
        })
        .expect("ensure");

    let listed = server.backend.list_sessions().expect("list_sessions");
    assert!(
        listed.iter().any(|s| s == "rimz-test"),
        "expected `rimz-test` in {listed:?}",
    );
    assert_eq!(
        server.pane_current_path("rimz-test"),
        cwd.path().display().to_string()
    );
}

/// A fresh server-less socket has no sessions. `list_sessions` translates
/// the `no server running` stderr into an empty Vec rather than erroring.
#[test]
fn list_sessions_returns_empty_when_no_server() {
    require_tmux!();

    let tempdir = TempDir::new().expect("tempdir");
    let socket = tempdir.path().join("never-started.sock");
    let backend = TmuxBackend::with_socket(&socket);
    let listed = backend.list_sessions().expect("list_sessions");
    assert!(
        listed.is_empty(),
        "expected empty list before any server start, got {listed:?}",
    );
}

/// `list_panes` against a fresh session returns the implicit shell pane,
/// stamped with the session name and a `%`-prefixed raw id.
#[test]
fn list_panes_with_session_returns_terminals() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("panes");

    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("panes".to_owned()),
        })
        .expect("list_panes");
    assert_eq!(panes.len(), 1, "expected single shell pane, got {panes:?}");
    let pane = &panes[0];
    assert_eq!(pane.pane_id.mux(), MuxName::Tmux);
    assert!(
        pane.pane_id.raw().starts_with('%'),
        "raw tmux pane ID should start with `%`, got {}",
        pane.pane_id.raw(),
    );
    assert_eq!(pane.session_name, "panes");
    assert_eq!(pane.command.as_deref(), Some("sh"));
    assert!(
        pane.cwd.as_deref().is_some_and(|cwd| !cwd.is_empty()),
        "tmux should report pane_current_path into PaneRef::cwd: {pane:?}",
    );
}

/// `split_pane` accepts `RIMZ_*` env injection via `tmux -e`. We split a
/// shell, give it a moment to print the var, then capture and check.
#[test]
fn split_pane_injects_env_vars() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("split");

    let mut env = BTreeMap::new();
    env.insert("RIMZ_TEST_VAR".to_owned(), "marker-rimz-env".to_owned());
    server
        .backend
        .split_pane(SplitPaneOptions {
            target_pane_id: None,
            cwd: None,
            command: Some(vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "printf RIMZ_TEST_VAR=$RIMZ_TEST_VAR; sleep 5".to_owned(),
            ]),
            env,
        })
        .expect("split_pane");

    thread::sleep(Duration::from_millis(400));

    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("split".to_owned()),
        })
        .expect("list_panes after split");
    assert_eq!(
        panes.len(),
        2,
        "expected 2 panes after split, got {panes:?}"
    );

    let new_pane = panes
        .iter()
        .find(|p| p.pane_id.raw() != "%0")
        .expect("split created a new pane id");
    let capture = server
        .backend
        .capture_pane(&new_pane.pane_id, None, false)
        .expect("capture_pane");
    assert!(
        capture.raw_text.contains("marker-rimz-env"),
        "split-pane should expose RIMZ_TEST_VAR; capture was: {:?}",
        capture.raw_text,
    );
}

/// `send_keys` + `capture_pane` round-trip: write a marker, see it.
#[test]
fn capture_and_send_keys_round_trip() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("io");

    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("io".to_owned()),
        })
        .expect("list_panes");
    let pane_id = panes[0].pane_id.clone();

    server
        .backend
        .send_keys(&pane_id, "printf rimz-marker-io\n")
        .expect("send_keys");
    thread::sleep(Duration::from_millis(400));

    let capture = server
        .backend
        .capture_pane(&pane_id, None, false)
        .expect("capture");
    assert!(
        capture.raw_text.contains("rimz-marker-io"),
        "expected marker in capture, got: {:?}",
        capture.raw_text,
    );
}

/// `open_sidebar` runs `split-window -d -h -l <width>% -b` against the
/// session. We verify the tmux CLI surface succeeds and that a second pane
/// was spawned.
#[test]
fn open_sidebar_split_window_succeeds() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("sidebar");
    let (_stub_dir, stub) = sidebar_command_stub();
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-sidebar-test"));

    server
        .backend
        .open_sidebar(&SidebarPaneOptions {
            session_name: "sidebar".to_owned(),
            workspace_id,
            cwd: std::env::current_dir().expect("cwd"),
            width_percent: 30,
            rimz_bin: stub,
            replace_existing: false,
        })
        .expect("open_sidebar");

    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("sidebar".to_owned()),
        })
        .expect("list_panes");
    assert_eq!(
        panes.len(),
        2,
        "sidebar split should keep a second pane: {panes:?}"
    );
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

/// `detach` on a daemon session with no attached client is a benign no-op
/// at the tmux level — it surfaces a "no current client" error. The
/// backend's wakeup-walk path doesn't depend on detach succeeding, so we
/// only assert that the binary is reachable, not that detach found a
/// client to kick.
#[test]
fn wake_sidebar_is_noop() {
    require_tmux!();

    let server = TmuxServer::new();
    server
        .backend
        .wake_sidebar("any-session", b"ignored payload")
        .expect("wake_sidebar is a no-op for tmux");
}

/// Capability probe must parse the binary's version string and compare it
/// against `MIN_TMUX_VERSION`. No session required.
#[test]
fn version_floor_parses_and_compares() {
    require_tmux!();

    let caps = tmux::capabilities().expect("capabilities() against a live tmux");
    let (maj, min, patch) = caps
        .parsed_version
        .expect("parsed_version is Some for any 3.x build");
    assert!(
        (maj, min, patch) >= MIN_TMUX_VERSION,
        "test host has tmux {maj}.{min}.{patch}; M0c requires >= {MIN_TMUX_VERSION:?}",
    );
    assert!(caps.meets_min_version);
    assert!(caps.popup_supported);
    assert!(caps.binary_version.contains("tmux"));
}

/// Cross-backend parity (DESIGN.md): every view the user opens should be born
/// with its own left sidebar + focused right terminal, like every Zellij tab
/// (`backend/zellij.rs::new_tab_is_born_with_a_right_terminal`). `open_sidebar`
/// installs an `after-new-window` hook so a fresh tmux window is born with the
/// same left split.
#[test]
fn new_window_is_born_with_a_sidebar_and_focused_terminal() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("room");
    let (_stub_dir, stub) = sidebar_command_stub();
    server
        .backend
        .open_sidebar(&SidebarPaneOptions {
            session_name: "room".to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-newwindow")),
            cwd: std::env::current_dir().expect("cwd"),
            width_percent: 30,
            rimz_bin: stub,
            replace_existing: false,
        })
        .expect("open_sidebar");

    // The user opens a second window.
    Command::new("tmux")
        .args([
            "-S",
            server.socket.to_str().expect("utf8 socket"),
            "new-window",
            "-t",
            "room",
        ])
        .output()
        .expect("tmux new-window");

    let panes = window_pane_count(&server, "room", 1);
    assert!(
        panes >= 2,
        "a new tmux window should be born with a sidebar beside its terminal, got {panes} pane(s)",
    );
}

/// Count the panes in `session:window` via `list-panes`.
fn window_pane_count(server: &TmuxServer, session: &str, window: u32) -> usize {
    let out = Command::new("tmux")
        .args([
            "-S",
            server.socket.to_str().expect("utf8 socket"),
            "list-panes",
            "-t",
            &format!("{session}:{window}"),
            "-F",
            "#{pane_id}",
        ])
        .output()
        .expect("tmux list-panes");
    if !out.status.success() {
        return 0;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .count()
}
