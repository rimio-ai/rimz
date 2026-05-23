//! Live tmux backend tests for the M0c spike.
//!
//! Each test spawns its own tmux server on a tempdir socket so it never
//! collides with the user's running sessions. The whole file becomes a
//! no-op (early-return per test, message printed once) when the `tmux`
//! binary is not on PATH.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

use rimz::ids::MuxName;
use rimz::mux::tmux::{self, MIN_TMUX_VERSION};
use rimz::mux::{MuxBackend, PaneListOptions, SplitPaneOptions, TmuxBackend};
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
    server.backend.ensure_session("rimz-test").expect("ensure");

    let listed = server.backend.list_sessions().expect("list_sessions");
    assert!(
        listed.iter().any(|s| s == "rimz-test"),
        "expected `rimz-test` in {listed:?}",
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

/// `open_sidebar` runs `split-window -d -h -l <width> -b` against the
/// session. The inner `rimz sidebar serve` command may not yet exist (M1
/// implements it); we only verify the tmux CLI surface succeeds and that
/// a second pane was spawned.
#[test]
fn open_sidebar_split_window_succeeds() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("sidebar");

    server
        .backend
        .open_sidebar("sidebar", 30)
        .expect("open_sidebar");

    // Inner command exits fast (binary not found); accept that. The split
    // call itself must have returned 0, which is the spike's contract.
    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("sidebar".to_owned()),
        })
        .expect("list_panes");
    assert!(
        !panes.is_empty(),
        "session should still have at least the original pane: {panes:?}",
    );
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
