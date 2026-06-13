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
use std::time::{Duration, Instant};

use rimz::ids::{MuxName, PaneId, WorkspaceId};
use rimz::mux::{
    MuxBackend, NamedKey, PaneListOptions, SessionOptions, SidebarPaneOptions, SidebarWidth,
    SplitPaneOptions, TmuxBackend,
};
use tempfile::TempDir;

/// Poll `capture_pane` on `pane_id` until its text contains `needle` or the
/// budget elapses; returns the last capture seen either way. Faster than a flat
/// settle sleep on the common path and more robust when the shell is slow.
fn capture_pane_until(
    backend: &TmuxBackend,
    pane_id: &PaneId,
    needle: &str,
    budget: Duration,
) -> String {
    let deadline = Instant::now() + budget;
    let mut last = String::new();
    loop {
        if let Ok(capture) = backend.capture_pane(pane_id, None, false) {
            last = capture.raw_text;
            if last.contains(needle) {
                return last;
            }
        }
        if Instant::now() >= deadline {
            return last;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

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
        self.display(session, "#{pane_current_path}")
    }

    /// `display-message -p -t <target> <format>`, asserting success.
    fn display(&self, target: &str, format: &str) -> String {
        let output = Command::new("tmux")
            .args([
                "-S",
                self.socket.to_str().expect("utf8 socket"),
                "display-message",
                "-p",
                "-t",
                target,
                format,
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

    /// Run a raw tmux command against this server's socket, asserting success.
    fn tmux(&self, args: &[&str]) {
        let output = Command::new("tmux")
            .arg("-S")
            .arg(self.socket.to_str().expect("utf8 socket"))
            .args(args)
            .output()
            .expect("spawn tmux");
        assert!(
            output.status.success(),
            "tmux {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
    }

    /// Block briefly until some pane in `session` reports `command` as its current
    /// command — an `exec`'d pane needs a tick before `pane_current_command`
    /// settles off the launching shell.
    fn wait_for_pane_command(&self, session: &str, command: &str) {
        for _ in 0..40 {
            let listed = self
                .backend
                .list_panes(PaneListOptions {
                    session_name: Some(session.to_owned()),
                    ..Default::default()
                })
                .expect("list_panes")
                .panes;
            if listed
                .iter()
                .any(|pane| pane.command.as_deref() == Some(command))
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("no pane in `{session}` ran `{command}` within the deadline");
    }

    fn window_names(&self, session: &str) -> Vec<String> {
        let output = Command::new("tmux")
            .args([
                "-S",
                self.socket.to_str().expect("utf8 socket"),
                "list-windows",
                "-t",
                session,
                "-F",
                "#{window_name}",
            ])
            .output()
            .expect("spawn tmux list-windows");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().to_owned())
            .collect()
    }

    /// `show-options <scope-args…> -v <option>`, asserting success — reads one
    /// option's live value back from the server.
    fn show_option(&self, scope_args: &[&str], option: &str) -> String {
        let output = Command::new("tmux")
            .args(["-S", self.socket.to_str().expect("utf8 socket")])
            .arg("show-options")
            .args(scope_args)
            .args(["-v", option])
            .output()
            .expect("spawn tmux show-options");
        assert!(
            output.status.success(),
            "show-options {option} failed: {}",
            String::from_utf8_lossy(&output.stderr),
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

/// `tmux show-environment -t <session> <name>` — the session-scoped env the
/// identity pin is stamped into.
fn show_session_environment(server: &TmuxServer, session: &str, name: &str) -> String {
    let output = Command::new("tmux")
        .args(["-S", server.socket.to_str().expect("utf8 socket")])
        .args(["show-environment", "-t", session, name])
        .output()
        .expect("spawn tmux show-environment");
    assert!(
        output.status.success(),
        "show-environment {name} failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// `ensure_session` applies the room options in one batched client invocation
/// (`TmuxBackend::batch` joins the twelve option sets with standalone `;`
/// tokens). Assert a representative option from each scope actually took on
/// the live server — server (`escape-time 0`), session (`mouse on`), and
/// window (`allow-passthrough on`) — proving the `;` tokenization reached
/// tmux as a command sequence, not as arguments.
#[test]
fn ensure_session_applies_room_options_in_one_batch() {
    require_tmux!();

    let server = TmuxServer::new();
    let cwd = TempDir::new().expect("cwd tempdir");
    server
        .backend
        .ensure_session(&SessionOptions {
            session_name: "rimz-options".to_owned(),
            workspace_id: WorkspaceId::from_project_root(cwd.path()),
            project_root: cwd.path().to_path_buf(),
            cwd: cwd.path().to_path_buf(),
            config: rimz::config::MultiplexerConfig::default(),
            detected_size: None,
        })
        .expect("ensure");

    assert_eq!(server.show_option(&["-s"], "escape-time"), "0");
    assert_eq!(server.show_option(&["-t", "rimz-options"], "mouse"), "on");
    assert_eq!(
        server.show_option(&["-w", "-t", "rimz-options"], "allow-passthrough"),
        "on",
    );

    let listed = server.backend.list_sessions().expect("list_sessions");
    assert!(
        listed.iter().any(|s| s == "rimz-options"),
        "expected `rimz-options` in {listed:?}",
    );
    let expected = cwd.path().display().to_string();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut current = server.pane_current_path("rimz-options");
    while current != expected && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
        current = server.pane_current_path("rimz-options");
    }
    assert_eq!(current, expected);

    let pin = show_session_environment(&server, "rimz-options", rimz::workspace::ENV_WORKSPACE_ID);
    assert_eq!(
        pin,
        format!(
            "{}={}",
            rimz::workspace::ENV_WORKSPACE_ID,
            WorkspaceId::from_project_root(cwd.path()),
        ),
    );
    let root = show_session_environment(&server, "rimz-options", rimz::workspace::ENV_PROJECT_ROOT);
    assert_eq!(
        root,
        format!(
            "{}={}",
            rimz::workspace::ENV_PROJECT_ROOT,
            cwd.path().display(),
        ),
    );
}

/// `focus_pane` lands cross-window: tmux's `select-pane` activates within its
/// window only, so the backend batches `select-window` (a pane id resolves as
/// a window target to the window holding it) before `select-pane`. The
/// session's current window must follow the jump.
#[test]
fn focus_pane_switches_the_containing_window() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("rimz-jump");
    // A second window, opened without focus so the first stays current.
    server.tmux(&["new-window", "-d", "-t", "rimz-jump", "-n", "second", "sh"]);

    let target = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-jump".to_owned()),
            ..Default::default()
        })
        .expect("list_panes")
        .panes
        .into_iter()
        .find(|pane| pane.view_name.as_deref() == Some("second"))
        .expect("the second window's pane");
    let window = target
        .view_id
        .clone()
        .expect("tmux panes carry a window id");
    assert_ne!(
        server.display("rimz-jump", "#{window_id}"),
        window,
        "the second window must start out not current",
    );

    server
        .backend
        .focus_pane(&target.pane_id)
        .expect("focus_pane");

    assert_eq!(
        server.display("rimz-jump", "#{window_id}"),
        window,
        "a cross-window jump must switch the session's current window",
    );
    assert_eq!(
        server.display("rimz-jump", "#{pane_id}"),
        target.pane_id.raw(),
        "and land on the target pane",
    );
}

/// The `after-new-window` hook pins the start verdict's fixed columns, so a
/// window opened after the terminal grows is still born at the width resolved
/// at launch — a raw percentage in the hook would re-evaluate against the new
/// geometry, which is exactly how the cap used to vanish from a session.
#[test]
fn new_window_pins_the_start_verdict_after_a_resize() {
    require_tmux!();

    let server = TmuxServer::new();
    let width = SidebarWidth::default();
    server
        .backend
        .ensure_session(&SessionOptions {
            session_name: "verdict".to_owned(),
            workspace_id: WorkspaceId::from_project_root(&std::env::temp_dir()),
            project_root: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            config: rimz::config::MultiplexerConfig::default(),
            detected_size: Some((200, 50)),
        })
        .expect("ensure_session");
    let (_stub_dir, stub) = sidebar_command_stub();
    server
        .backend
        .open_sidebar(
            &SidebarPaneOptions {
                session_name: "verdict".to_owned(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-verdict")),
                project_root: std::env::temp_dir(),
                cwd: std::env::temp_dir(),
                width,
                // The verdict on a 200-column terminal: 30% is 60 ≤ the 72
                // cap — the under-cap case the old percentage spelling leaked.
                birth_size: width.birth_size(Some(200)),
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_panes: Vec::new(),
                refresh_ms: None,
            },
            None,
        )
        .expect("open_sidebar");

    // The terminal "grows": windows born from now on adopt 340 columns.
    server.tmux(&["set-option", "-t", "verdict", "default-size", "340x50"]);
    server.tmux(&["new-window", "-t", "verdict"]);
    assert_eq!(
        server.display("verdict:1", "#{window_width}"),
        "340",
        "the new window adopts the grown geometry",
    );

    assert_eq!(
        left_pane_width(&server, "verdict:1"),
        Some(60),
        "a window opened after the terminal grew must be born at the start \
         verdict (60 columns), not a re-evaluated percentage of 340",
    );
}

/// The width of the left (`pane_left == 0`) pane in `target`, polling until
/// the window holds a second, hook-docked pane or the budget elapses.
fn left_pane_width(server: &TmuxServer, target: &str) -> Option<u64> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let out = Command::new("tmux")
            .args([
                "-S",
                server.socket.to_str().expect("utf8 socket"),
                "list-panes",
                "-t",
                target,
                "-F",
                "#{pane_left}:#{pane_width}",
            ])
            .output()
            .expect("tmux list-panes");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let panes: Vec<&str> = stdout.lines().filter(|line| !line.is_empty()).collect();
        if out.status.success()
            && panes.len() >= 2
            && let Some(width) = panes.iter().find_map(|line| {
                let (left, width) = line.split_once(':')?;
                (left == "0").then(|| width.parse().ok()).flatten()
            })
        {
            return Some(width);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(25));
    }
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

    // `#{pane_current_command}` can read transiently as the launcher shell while
    // the spawned `sh` is still exec'ing — surfaces only under heavy parallel
    // load — so poll until the command settles before asserting on it.
    let deadline = Instant::now() + Duration::from_secs(5);
    let panes = loop {
        let panes = server
            .backend
            .list_panes(PaneListOptions {
                session_name: Some("panes".to_owned()),
                ..Default::default()
            })
            .expect("list_panes")
            .panes;
        if panes.first().and_then(|p| p.command.as_deref()) == Some("sh")
            || Instant::now() >= deadline
        {
            break panes;
        }
        thread::sleep(Duration::from_millis(25));
    };
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

/// `open_background_view` opens a dedicated, named window for the host; the
/// session's `after-new-window` hook (installed by `open_sidebar`, as `rimz
/// start` does) docks the global sidebar on its left, so the window is born
/// `sidebar | host`. Idempotent on the window name: a second call launches
/// nothing.
#[test]
fn open_background_view_creates_named_window_idempotently() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("rimz-bgview");
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = SidebarPaneOptions {
        session_name: "rimz-bgview".to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-bgview")),
        project_root: std::env::temp_dir(),
        cwd: std::env::temp_dir(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(80)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_panes: Vec::new(),
        refresh_ms: None,
    };
    // Install the `after-new-window` sidebar hook the way `rimz start` does
    // before launching the host.
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");

    let opts = rimz::mux::BackgroundViewOptions {
        name: "rimzd".to_owned(),
        hosts: vec![rimz::mux::HostPane {
            argv: vec!["sleep".to_owned(), "120".to_owned()],
            cwd: std::env::temp_dir(),
        }],
        sidebar,
    };

    let first = server
        .backend
        .open_background_view(&opts)
        .expect("first launch");
    assert_eq!(first, rimz::mux::BackgroundViewLaunch::Launched);
    assert!(
        server
            .window_names("rimz-bgview")
            .iter()
            .any(|name| name == "rimzd"),
        "expected a rimzd window after launch, got {:?}",
        server.window_names("rimz-bgview"),
    );
    // Forced to the front: the daemon window leads the session.
    assert_eq!(
        server
            .window_names("rimz-bgview")
            .first()
            .map(String::as_str),
        Some("rimzd"),
        "daemon window must lead the session, got {:?}",
        server.window_names("rimz-bgview"),
    );
    // Born `sidebar | host`: the hook-docked sidebar beside the host pane.
    let rc_panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-bgview".to_owned()),
            ..Default::default()
        })
        .expect("list panes")
        .panes
        .into_iter()
        .filter(|pane| pane.view_name.as_deref() == Some("rimzd"))
        .count();
    assert_eq!(rc_panes, 2, "rimzd window should be born sidebar | host");

    let second = server
        .backend
        .open_background_view(&opts)
        .expect("second launch");
    assert_eq!(
        second,
        rimz::mux::BackgroundViewLaunch::AlreadyRunning,
        "relaunching into a session that already carries the view is a no-op",
    );
}

/// `open_sidebar` re-seeds the reborn session's prior agents: each
/// `resume_panes` entry becomes its own window, born `sidebar | agent` via the
/// `after-new-window` hook. Idempotent on the window name, so a re-run never
/// doubles an agent window.
#[test]
fn open_sidebar_seeds_resume_windows_idempotently() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("rimz-resume");
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = SidebarPaneOptions {
        session_name: "rimz-resume".to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-resume")),
        project_root: std::env::temp_dir(),
        cwd: std::env::temp_dir(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(80)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        // A harmless stand-in for the agent CLIs (`claude`/`codex` aren't on a CI
        // PATH); the seeding contract is the window, not what runs in it.
        resume_panes: vec![rimz::mux::ResumePane {
            command: vec!["sleep".to_owned(), "120".to_owned()],
            cwd: std::env::temp_dir(),
            label: "claude:feature".to_owned(),
        }],
        refresh_ms: None,
    };

    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");
    assert!(
        server
            .window_names("rimz-resume")
            .iter()
            .any(|name| name == "claude:feature"),
        "expected a resumed agent window, got {:?}",
        server.window_names("rimz-resume"),
    );
    // Born `sidebar | agent`: the hook-docked sidebar beside the agent pane.
    let agent_panes = server
        .backend
        .list_panes(rimz::mux::PaneListOptions {
            session_name: Some("rimz-resume".to_owned()),
            ..Default::default()
        })
        .expect("list panes")
        .panes
        .into_iter()
        .filter(|pane| pane.view_name.as_deref() == Some("claude:feature"))
        .count();
    assert_eq!(
        agent_panes, 2,
        "resumed window should be born sidebar | agent"
    );

    // A re-run finds the window already present and seeds nothing new.
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("second open_sidebar");
    let resumed = server
        .window_names("rimz-resume")
        .into_iter()
        .filter(|name| name == "claude:feature")
        .count();
    assert_eq!(
        resumed, 1,
        "resume seeding is idempotent on the window name"
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
            focus: false,
        })
        .expect("split_pane");

    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("split".to_owned()),
            ..Default::default()
        })
        .expect("list_panes after split")
        .panes;
    assert_eq!(
        panes.len(),
        2,
        "expected 2 panes after split, got {panes:?}"
    );

    let new_pane = panes
        .iter()
        .find(|p| p.pane_id.raw() != "%0")
        .expect("split created a new pane id");
    let capture = capture_pane_until(
        &server.backend,
        &new_pane.pane_id,
        "marker-rimz-env",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains("marker-rimz-env"),
        "split-pane should expose RIMZ_TEST_VAR; capture was: {capture:?}",
    );
}

/// `send_keys`, `send_key`, and `capture_pane` round-trip through a live pane.
#[test]
fn capture_send_keys_and_named_key_round_trip() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("io");

    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("io".to_owned()),
            ..Default::default()
        })
        .expect("list_panes")
        .panes;
    let pane_id = panes[0].pane_id.clone();

    server
        .backend
        .send_keys(&pane_id, "printf rimz-marker-io\n")
        .expect("send_keys");

    let capture = capture_pane_until(
        &server.backend,
        &pane_id,
        "rimz-marker-io",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains("rimz-marker-io"),
        "expected marker in capture, got: {capture:?}",
    );

    server
        .backend
        .send_keys(&pane_id, "printf rimz-marker-key")
        .expect("send_keys");
    server
        .backend
        .send_key(&pane_id, NamedKey::Enter)
        .expect("send_key");

    let capture = capture_pane_until(
        &server.backend,
        &pane_id,
        "rimz-marker-key",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains("rimz-marker-key"),
        "expected marker in capture, got: {capture:?}",
    );
}

/// `reconcile_sidebars` collapses an orphan sidebar-only window — a wedged
/// renderer whose working siblings all closed but which never self-closed — by
/// closing its sidebar pane, so the window disappears. A working window with no
/// sidebar still gains one in the same pass; the two behaviours coexist without a
/// rebirth.
#[test]
fn reconcile_sidebars_collapses_an_orphan_sidebar_only_window() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("multi"); // window 0: a working `sh` pane
    server.tmux(&["rename-window", "-t", "multi:0", "room"]);
    // window 1: a lone sidebar-titled pane, no working sibling — the orphan.
    server.tmux(&[
        "new-window",
        "-t",
        "multi",
        "-n",
        "ghost",
        "printf '\\033]2;rimz-sidebar\\007'; exec sleep 600",
    ]);
    server.wait_for_pane_command("multi", "rimz-sidebar");
    assert_eq!(
        server.window_names("multi"),
        vec!["room".to_owned(), "ghost".to_owned()],
        "two windows before reconcile",
    );

    let (_rimz_dir, rimz_bin) = sidebar_command_stub();
    let report = server
        .backend
        .reconcile_sidebars(
            &SidebarPaneOptions {
                session_name: "multi".to_owned(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-orphan")),
                project_root: std::env::current_dir().expect("cwd"),
                cwd: std::env::current_dir().expect("cwd"),
                width: SidebarWidth::default(),
                birth_size: SidebarWidth::default().birth_size(Some(80)),
                rimz_bin,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_panes: Vec::new(),
                refresh_ms: None,
            },
            // No live sidebars known: the orphan's pane is unclaimed, so it closes.
            &rimz::mux::SidebarLiveness::default(),
        )
        .expect("reconcile_sidebars");

    assert_eq!(report.closed, 1, "the orphan's lone sidebar pane is closed");
    assert_eq!(report.recovered, 1, "the working window gains a sidebar");
    assert_eq!(report.failed, 0);
    assert_eq!(
        server.window_names("multi"),
        vec!["room".to_owned()],
        "the orphan window collapsed; the working window survives",
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

/// The control-mode presence stream surfaces topology changes as nudges — the
/// tmux fast path the elder sidebar consumes. A new window must produce a
/// presence event within the budget, and killing the server must end the
/// stream (`None`) rather than wedging it, so a dead watcher degrades to the
/// poll instead of a stuck frame.
#[test]
fn presence_watch_nudges_on_topology_and_ends_with_the_server() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("presence");

    let mut watch = rimz::mux::tmux::PresenceWatch::attach(Some(&server.socket), "presence")
        .expect("attach control client");
    // `attach` returns once the control client *spawns*; tmux registers it a
    // beat later, and a topology change firing before registration is invisible
    // to the stream. Production tolerates that (the poll is truth) — the test
    // must not race it, so wait until `list-clients` reports the control client
    // before touching topology.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let out = Command::new("tmux")
            .args([
                "-S",
                server.socket.to_str().expect("utf8 socket"),
                "list-clients",
                "-t",
                "presence",
                "-F",
                "#{client_control_mode}",
            ])
            .output()
            .expect("tmux list-clients");
        if String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line.trim() == "1")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "control client never registered with the tmux server"
        );
        thread::sleep(Duration::from_millis(25));
    }

    // Drain on a helper thread so the main thread owns the timeout. A single
    // topology change fans out as a burst of control lines (`%window-add` plus
    // `%layout-change`), so report the first nudge, then drain to the end.
    let (tx, rx) = std::sync::mpsc::channel::<Option<()>>();
    let drain = thread::spawn(move || {
        let _ = tx.send(watch.next_presence());
        while watch.next_presence().is_some() {}
        let _ = tx.send(None);
    });

    server.tmux(&["new-window", "-t", "presence", "sh"]);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).expect("nudge"),
        Some(()),
        "a new window posts a presence nudge"
    );

    server.tmux(&["kill-server"]);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).expect("stream end"),
        None,
        "a dead server ends the stream instead of wedging it"
    );
    drain.join().expect("drain thread");
}
