//! Live tmux backend tests for the M0c spike.
//!
//! Each test spawns its own tmux server on a tempdir socket so it never
//! collides with the user's running sessions. The whole file becomes a
//! no-op (early-return per test, message printed once) when the `tmux`
//! binary is not on PATH.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::RuntimePaths;
use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use rimz::ids::{AgentKind, MuxName, PaneId, SidebarInstanceId, WorkspaceId};
use rimz::mux::{
    ClientFocusOptions, LayoutPanes, MuxBackend, NamedKey, PaneCmd, PaneListOptions,
    SessionOptions, SidebarPaneOptions, SidebarWidth, SplitPaneOptions, TabOptions, TmuxBackend,
};
use rimz::sidebar::{SidebarLaunchOutcome, launch_sidebar_if_needed, write_heartbeat};
use rimz::workspace::WorkspaceResolver;
use tempfile::TempDir;

use crate::common::{Env, ScrubSessionEnvExt};

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

/// One pane's live placement: its raw id, left/top edge in cells, and current
/// working directory — read from `list-panes -F` to assert layout geometry.
#[derive(Clone, Debug)]
struct PaneGeom {
    #[allow(dead_code)]
    id: String,
    left: u64,
    top: u64,
    path: String,
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

    /// Live geometry for every pane in `target` (a `session:window` address),
    /// polling until at least `want` panes are present or the budget elapses.
    /// Reads the left edge, top edge, and current path per pane — enough to
    /// assert the imperative `open_tab` builder's column/row placement.
    fn wait_for_panes(&self, target: &str, want: usize) -> Vec<PaneGeom> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let out = Command::new("tmux")
                .args([
                    "-S",
                    self.socket.to_str().expect("utf8 socket"),
                    "list-panes",
                    "-t",
                    target,
                    "-F",
                    "#{pane_id}\t#{pane_left}\t#{pane_top}\t#{pane_current_path}",
                ])
                .output()
                .expect("spawn tmux list-panes");
            let panes: Vec<PaneGeom> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| {
                    let mut cols = line.split('\t');
                    Some(PaneGeom {
                        id: cols.next()?.to_owned(),
                        left: cols.next()?.parse().ok()?,
                        top: cols.next()?.parse().ok()?,
                        path: cols.next().unwrap_or_default().to_owned(),
                    })
                })
                .collect();
            if panes.len() >= want || Instant::now() >= deadline {
                return panes;
            }
            thread::sleep(Duration::from_millis(25));
        }
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

    fn show_hooks(&self, session: &str) -> String {
        let output = Command::new("tmux")
            .args(["-S", self.socket.to_str().expect("utf8 socket")])
            .args(["show-hooks", "-t", session])
            .output()
            .expect("spawn tmux show-hooks");
        assert!(
            output.status.success(),
            "show-hooks failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn has_after_new_window_hook(&self, session: &str) -> bool {
        self.show_hooks(session)
            .lines()
            .any(|line| line.contains("after-new-window"))
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

/// A live tmux client attached to a session on the test's private socket, held
/// open on a PTY of the given size so `list-clients` reports it. Drop kills the
/// client; server teardown stays with [`TmuxServer`]. Mirrors the Zellij
/// backend suite's `AttachedClient`.
struct AttachedTmuxClient {
    _master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl AttachedTmuxClient {
    fn attach(socket: &Path, session: &str, cols: u16, rows: u16) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new("tmux");
        // The test process may itself run inside a mux pane, and a control
        // command captures the spawning env; scrub the session vars so the
        // attach lands on the test's private socket alone.
        cmd.scrub_session_env();
        cmd.env("TERM", "xterm-256color");
        cmd.args([
            "-S",
            socket.to_str().expect("utf8 socket"),
            "attach",
            "-t",
            session,
        ]);
        let child = pair.slave.spawn_command(cmd).expect("spawn tmux attach");
        drop(pair.slave);
        // Drain the PTY in the background so the kernel buffer never fills and
        // stalls the client; the thread exits with the PTY on drop.
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => continue,
                }
            }
        });
        Self {
            _master: pair.master,
            child,
        }
    }
}

impl Drop for AttachedTmuxClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
        .focus_pane(&target.pane_id, None)
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
                resume_tabs: Vec::new(),
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

#[test]
fn reconcile_sidebars_reinstalls_after_new_window_hook() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("rimz-hook-reconcile");
    let (_stub_dir, stub) = sidebar_command_stub();
    let width = SidebarWidth::default();
    let opts = SidebarPaneOptions {
        session_name: "rimz-hook-reconcile".to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-hook-reconcile")),
        project_root: std::env::temp_dir(),
        cwd: std::env::temp_dir(),
        width,
        birth_size: width.birth_size(Some(80)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };

    let report = server
        .backend
        .reconcile_sidebars(&opts, &rimz::mux::SidebarLiveness::default())
        .expect("reconcile_sidebars");
    assert_eq!(report.recovered, 1);
    assert!(
        server.has_after_new_window_hook("rimz-hook-reconcile"),
        "reconcile should install the hook"
    );
    server.wait_for_pane_command("rimz-hook-reconcile", "rimz-sidebar");

    server.tmux(&[
        "set-hook",
        "-u",
        "-t",
        "rimz-hook-reconcile",
        "after-new-window",
    ]);
    assert!(
        !server.has_after_new_window_hook("rimz-hook-reconcile"),
        "test setup should remove the hook before the second reconcile"
    );

    server
        .backend
        .reconcile_sidebars(&opts, &rimz::mux::SidebarLiveness::default())
        .expect("second reconcile_sidebars");
    assert!(
        server.has_after_new_window_hook("rimz-hook-reconcile"),
        "reconcile should re-install a missing hook"
    );
}

#[test]
fn launch_sidebar_skipped_by_foreign_heartbeat_still_ensures_tmux_session_view() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("rimz-foreign");
    server.tmux(&["rename-window", "-t", "rimz-foreign:0", "work"]);

    let workspace = TempDir::new().expect("workspace");
    let workspace_id = WorkspaceId::from_project_root(workspace.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), workspace.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    write_heartbeat(
        &runtime,
        workspace_id.clone(),
        &SidebarInstanceId::new(),
        MuxName::Zellij,
        "prior-zellij",
        &runtime.sock_dir.join("foreign.sock"),
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_7")),
    )
    .expect("foreign heartbeat");
    assert!(rimz::sidebar::fresh_sidebar_present(&runtime));
    assert!(
        !server.has_after_new_window_hook("rimz-foreign"),
        "fixture starts with no session hook"
    );

    let (_stub_dir, stub) = sidebar_command_stub();
    let width = SidebarWidth::default();
    let opts = SidebarPaneOptions {
        session_name: "rimz-foreign".to_owned(),
        workspace_id: workspace_id.clone(),
        project_root: workspace.path().to_path_buf(),
        cwd: workspace.path().to_path_buf(),
        width,
        birth_size: width.birth_size(Some(80)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };

    let outcome = launch_sidebar_if_needed(&server.backend, &runtime, &opts, None);

    assert_eq!(outcome, SidebarLaunchOutcome::SkippedFresh);
    server.wait_for_pane_command("rimz-foreign", "rimz-sidebar");
    assert!(
        server.has_after_new_window_hook("rimz-foreign"),
        "skipping producer launch should still install the tmux hook"
    );
    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-foreign".to_owned()),
            ..Default::default()
        })
        .expect("list_panes")
        .panes;
    assert_eq!(
        panes
            .iter()
            .filter(|pane| pane.command.as_deref() == Some("rimz-sidebar"))
            .count(),
        1,
        "the working window should gain exactly one sidebar: {panes:?}",
    );
    assert!(
        panes.iter().any(|pane| {
            pane.view_name.as_deref() == Some("work")
                && pane.command.as_deref() == Some("rimz-sidebar")
        }),
        "the sidebar should be in the working window: {panes:?}",
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

/// `open_background_view` opens a dedicated, named window for stats and the hosts; the
/// session's `after-new-window` hook (installed by `open_sidebar`, as `rimz
/// start` does) docks the global sidebar on its left, so the window is born
/// `sidebar | stats | stacked hosts`. Idempotent on the window name: a second call
/// launches nothing.
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
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    // Install the `after-new-window` sidebar hook the way `rimz start` does
    // before launching the host.
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");
    let working_window = server.display("rimz-bgview", "#{window_id}");

    let opts = rimz::mux::BackgroundViewOptions {
        name: "rimzd".to_owned(),
        stats: rimz::mux::HostPane {
            argv: vec!["sleep".to_owned(), "120".to_owned()],
            cwd: std::env::temp_dir(),
        },
        hosts: vec![
            rimz::mux::HostPane {
                argv: vec!["sleep".to_owned(), "120".to_owned()],
                cwd: std::env::temp_dir(),
            },
            rimz::mux::HostPane {
                argv: vec!["sleep".to_owned(), "120".to_owned()],
                cwd: std::env::temp_dir(),
            },
        ],
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
    assert_eq!(
        server.display("rimz-bgview", "#{window_id}"),
        working_window,
        "launch must leave focus on the pre-existing working window",
    );
    assert_ne!(
        server.display("rimz-bgview", "#{window_name}"),
        "rimzd",
        "launch must not focus the daemon window",
    );
    // Born `sidebar | stats | stacked hosts`: the hook-docked sidebar beside
    // stats and the daemon host column.
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
    assert_eq!(
        rc_panes, 4,
        "rimzd window should be born sidebar | stats | stacked hosts"
    );
    let panes = server.wait_for_panes("rimz-bgview:rimzd", 4);
    assert_eq!(panes.len(), 4, "expected four rimzd panes, got {panes:?}");
    let mut by_left: BTreeMap<u64, Vec<&PaneGeom>> = BTreeMap::new();
    for pane in &panes {
        by_left.entry(pane.left).or_default().push(pane);
    }
    assert_eq!(
        by_left.len(),
        3,
        "rimzd should have three columns: sidebar | stats | hosts, got {panes:?}",
    );
    let right_column = by_left
        .iter()
        .next_back()
        .map(|(_, panes)| panes)
        .expect("right column");
    assert_eq!(
        right_column.len(),
        2,
        "daemon hosts should share the right column, got {panes:?}",
    );
    let mut host_tops: Vec<u64> = right_column.iter().map(|pane| pane.top).collect();
    host_tops.sort_unstable();
    host_tops.dedup();
    assert_eq!(
        host_tops.len(),
        2,
        "daemon hosts should be vertically stacked, got {panes:?}",
    );

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

/// `open_background_view` creates a stats-only `rimzd` window when no daemon
/// hosts apply: the sidebar hook docks render on the left and stats fills the
/// remaining work area.
#[test]
fn open_background_view_creates_stats_only_window() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("rimz-bgstats");
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = SidebarPaneOptions {
        session_name: "rimz-bgstats".to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-bgstats")),
        project_root: std::env::temp_dir(),
        cwd: std::env::temp_dir(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(80)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");

    let opts = rimz::mux::BackgroundViewOptions {
        name: "rimzd".to_owned(),
        stats: rimz::mux::HostPane {
            argv: vec!["sleep".to_owned(), "120".to_owned()],
            cwd: std::env::temp_dir(),
        },
        hosts: Vec::new(),
        sidebar,
    };

    let first = server
        .backend
        .open_background_view(&opts)
        .expect("first launch");
    assert_eq!(first, rimz::mux::BackgroundViewLaunch::Launched);
    assert_eq!(
        server
            .window_names("rimz-bgstats")
            .first()
            .map(String::as_str),
        Some("rimzd"),
        "stats-only daemon window must lead the session, got {:?}",
        server.window_names("rimz-bgstats"),
    );
    let rc_panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-bgstats".to_owned()),
            ..Default::default()
        })
        .expect("list panes")
        .panes
        .into_iter()
        .filter(|pane| pane.view_name.as_deref() == Some("rimzd"))
        .count();
    assert_eq!(
        rc_panes, 2,
        "rimzd window should be born sidebar | stats without daemon hosts"
    );
}

/// `open_sidebar` re-seeds the reborn session's prior agents: each
/// `resume_tabs` entry becomes its own `#channel` window, born
/// `sidebar | agents…` via the `after-new-window` hook. Idempotent on the
/// window name, so a re-run never doubles an agent window.
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
        resume_tabs: vec![rimz::mux::ResumeTab {
            label: "#feature".to_owned(),
            cwd: std::env::temp_dir(),
            panes: vec![
                vec!["sleep".to_owned(), "120".to_owned()],
                vec!["sleep".to_owned(), "120".to_owned()],
            ],
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
            .any(|name| name == "#feature"),
        "expected a resumed channel window, got {:?}",
        server.window_names("rimz-resume"),
    );
    // Born `sidebar | agents…`: the hook-docked sidebar beside the agent panes.
    let agent_panes = server
        .backend
        .list_panes(rimz::mux::PaneListOptions {
            session_name: Some("rimz-resume".to_owned()),
            ..Default::default()
        })
        .expect("list panes")
        .panes
        .into_iter()
        .filter(|pane| pane.view_name.as_deref() == Some("#feature"))
        .count();
    assert_eq!(
        agent_panes, 3,
        "resumed window should be born sidebar | agents"
    );
    assert_eq!(
        left_pane_width(&server, "rimz-resume:#feature"),
        Some(u64::from(sidebar.birth_size.cols.get())),
        "resume seeding keeps the hook-docked sidebar at the birth width"
    );

    // A re-run finds the window already present and seeds nothing new.
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("second open_sidebar");
    let resumed = server
        .window_names("rimz-resume")
        .into_iter()
        .filter(|name| name == "#feature")
        .count();
    assert_eq!(
        resumed, 1,
        "resume seeding is idempotent on the window name"
    );
}

/// Closing one agent tab while the tmux session still has another window is a
/// voluntary close: the wrapper records `agent.ended`, and the next resume plan
/// leaves that session out.
#[test]
fn closing_agent_tab_records_end_trace_when_session_survives() {
    require_tmux!();

    let env = Env::new();
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let worktree = env.project_root.join("rimz-gc");
    std::fs::create_dir_all(&worktree).expect("mkdir worktree");
    let agent_id = "sess-closed";

    let mut observation =
        AgentLifecycleObservation::new(Some(agent_id.into()), LifecycleSignal::Registered);
    observation.agent_name = Some("closed-lane".to_owned());
    observation.worktree_path = Some(worktree.display().to_string());
    observation.worktree_branch = Some("gc-fixes".to_owned());
    observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, "%99"));
    env.ledger()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "SessionStart",
            &observation,
        ))
        .expect("append registered agent");

    let before = plan_from_env(&env);
    assert_eq!(before.tabs.len(), 1, "seeded agent should be recoverable");

    let server = TmuxServer::new();
    server
        .backend
        .ensure_session(&SessionOptions {
            session_name: workspace.session_name.clone(),
            workspace_id: workspace.workspace_id.clone(),
            project_root: workspace.project_root.clone(),
            cwd: workspace.worktree_root.clone(),
            config: rimz::config::MultiplexerConfig::default(),
            detected_size: Some((160, 40)),
        })
        .expect("ensure session");

    let agent_bin = write_sleeping_agent_shim(&env, "claude");
    let ready = env.home_root.join("agent-ready");
    let path = path_with_front(&agent_bin);
    let rimz_bin = env.rimz_bin().to_string_lossy().into_owned();
    let command = vec![
        "/usr/bin/env".to_owned(),
        format!("XDG_STATE_HOME={}", env.state_root().display()),
        format!("XDG_RUNTIME_DIR={}", env.runtime_root.display()),
        format!("XDG_CONFIG_HOME={}", env.config_root().display()),
        format!("HOME={}", env.home_root.display()),
        // A non-launchable shell disables the login-shell launch wrapper so the
        // agent shim resolves against the PATH below. A real login shell sources
        // /etc/profile, which on Debian-family CI hosts overwrites PATH and drops
        // the test's agent-bin dir, leaving the bare `claude` argv unresolvable.
        "SHELL=/definitely/not/a/shell".to_owned(),
        format!("PATH={path}"),
        format!("RIMZ_TEST_AGENT_READY={}", ready.display()),
        rimz_bin,
        "--mux".to_owned(),
        "tmux".to_owned(),
        "agents".to_owned(),
        "exec".to_owned(),
        "claude".to_owned(),
        "--resume".to_owned(),
        agent_id.to_owned(),
        "--close-pane-on-exit".to_owned(),
    ];
    let (_stub_dir, stub) = sidebar_command_stub();
    server
        .backend
        .open_tab(&TabOptions {
            session_name: workspace.session_name.clone(),
            title: "#rimz-gc".to_owned(),
            cwd: worktree.clone(),
            panes: LayoutPanes {
                columns: vec![vec![PaneCmd { argv: command }]],
            },
            focus: false,
            sidebar: SidebarPaneOptions {
                session_name: workspace.session_name.clone(),
                workspace_id: workspace.workspace_id.clone(),
                project_root: workspace.project_root.clone(),
                cwd: worktree.clone(),
                width: SidebarWidth::default(),
                birth_size: SidebarWidth::default().birth_size(Some(160)),
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_tabs: Vec::new(),
                refresh_ms: None,
            },
        })
        .expect("open agent tab");
    wait_for_path(&ready, "agent shim did not start");

    let target = format!("{}:#rimz-gc", workspace.session_name);
    server.tmux(&["kill-window", "-t", target.as_str()]);
    assert!(
        server
            .backend
            .list_sessions()
            .expect("list sessions")
            .contains(&workspace.session_name),
        "closing one tab must leave the room alive"
    );
    wait_for_agent_tombstone(&env, agent_id);

    let after = plan_from_env(&env);
    assert!(
        after.is_empty(),
        "a closed-tab end trace removes the agent from resume candidates"
    );
}

/// `open_tab` builds a caller-specified multi-column layout imperatively: the
/// first pane is the `new-window`, the remaining rows of a column split `-v`
/// below it, and each later column splits `-h` to the right of the previous
/// one. The session's `after-new-window` hook docks the global sidebar on the
/// left, so the tab is born `sidebar | work…`. Mirrors the Zellij `open_tab`
/// layout test (`backend::zellij::tab_layout_reopens_work_panes_evenly...`),
/// but tmux splits fine on a detached session, so no attached client is needed.
#[test]
fn open_tab_builds_multi_column_layout() {
    require_tmux!();

    let server = TmuxServer::new();
    let cwd = TempDir::new().expect("cwd tempdir");
    let width = SidebarWidth::default();
    server
        .backend
        .ensure_session(&SessionOptions {
            session_name: "rimz-tab".to_owned(),
            workspace_id: WorkspaceId::from_project_root(cwd.path()),
            project_root: cwd.path().to_path_buf(),
            cwd: cwd.path().to_path_buf(),
            config: rimz::config::MultiplexerConfig::default(),
            // Wide enough that the sidebar plus two work columns — one split in
            // two — all fit without tmux refusing a split for want of space.
            detected_size: Some((300, 50)),
        })
        .expect("ensure_session");

    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = SidebarPaneOptions {
        session_name: "rimz-tab".to_owned(),
        workspace_id: WorkspaceId::from_project_root(cwd.path()),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        width,
        birth_size: width.birth_size(Some(300)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    // Installs the `after-new-window` hook so the new tab is born with a sidebar.
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");

    let work_pane = || PaneCmd {
        argv: vec!["sleep".to_owned(), "600".to_owned()],
    };
    server
        .backend
        .open_tab(&TabOptions {
            session_name: "rimz-tab".to_owned(),
            title: "work".to_owned(),
            cwd: cwd.path().to_path_buf(),
            panes: LayoutPanes {
                columns: vec![
                    // Column 0: two stacked rows — the `new-window` pane plus a
                    // `-v` split, exercising the in-column anchor tracking.
                    vec![work_pane(), work_pane()],
                    // Column 1: one pane to the right — the `-h` split path.
                    vec![work_pane()],
                ],
            },
            focus: true,
            sidebar,
        })
        .expect("open_tab");

    // The hook-docked sidebar plus three work panes.
    let panes = server.wait_for_panes("rimz-tab:work", 4);
    assert_eq!(
        panes.len(),
        4,
        "tab should be born with a sidebar and three work panes: {panes:?}",
    );

    // The hook-docked sidebar is the sole pane at the left edge.
    assert_eq!(
        panes.iter().filter(|p| p.left == 0).count(),
        1,
        "exactly one pane (the hook-docked sidebar) sits at the left edge: {panes:?}",
    );

    // The three work panes form two columns: column 0 stacked into two rows
    // (same left edge, different top edge), column 1 a single pane to the right.
    let work: Vec<_> = panes.iter().filter(|p| p.left > 0).collect();
    assert_eq!(
        work.len(),
        3,
        "three work panes sit right of the sidebar: {work:?}"
    );
    let column_left = work.iter().map(|p| p.left).min().expect("a work pane");
    let column0: Vec<_> = work.iter().filter(|p| p.left == column_left).collect();
    let column1: Vec<_> = work.iter().filter(|p| p.left > column_left).collect();
    assert_eq!(
        column0.len(),
        2,
        "column 0 splits into two stacked rows: {work:?}"
    );
    assert_ne!(
        column0[0].top, column0[1].top,
        "column 0's rows stack vertically — same left, different top: {work:?}",
    );
    assert_eq!(
        column1.len(),
        1,
        "column 1 is a single pane to the right of column 0: {work:?}",
    );

    // Every work pane runs in the requested cwd.
    let want_cwd = cwd.path().canonicalize().expect("canonicalize cwd");
    for pane in &work {
        assert_eq!(
            Path::new(&pane.path).canonicalize().ok().as_deref(),
            Some(want_cwd.as_path()),
            "each work pane runs in the tab cwd: {pane:?}",
        );
    }

    // `focus: true` made the new tab the session's current window.
    assert_eq!(
        server.display("rimz-tab", "#{window_name}"),
        "work",
        "focus: true should select the new window",
    );
}

/// A single-column, single-pane layout births exactly the bare working tab: the
/// `new-window` pane beside the hook-docked sidebar, no extra splits. Locks the
/// `new-window`-only path and confirms `focus: false` leaves the user's current
/// window untouched.
#[test]
fn open_tab_single_pane_layout_docks_one_work_pane_beside_the_sidebar() {
    require_tmux!();

    let server = TmuxServer::new();
    let cwd = TempDir::new().expect("cwd tempdir");
    let width = SidebarWidth::default();
    server
        .backend
        .ensure_session(&SessionOptions {
            session_name: "rimz-solo".to_owned(),
            workspace_id: WorkspaceId::from_project_root(cwd.path()),
            project_root: cwd.path().to_path_buf(),
            cwd: cwd.path().to_path_buf(),
            config: rimz::config::MultiplexerConfig::default(),
            detected_size: Some((200, 50)),
        })
        .expect("ensure_session");
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = SidebarPaneOptions {
        session_name: "rimz-solo".to_owned(),
        workspace_id: WorkspaceId::from_project_root(cwd.path()),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        width,
        birth_size: width.birth_size(Some(200)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");

    server
        .backend
        .open_tab(&TabOptions {
            session_name: "rimz-solo".to_owned(),
            title: "solo".to_owned(),
            cwd: cwd.path().to_path_buf(),
            panes: LayoutPanes {
                columns: vec![vec![PaneCmd {
                    argv: vec!["sleep".to_owned(), "600".to_owned()],
                }]],
            },
            focus: false,
            sidebar,
        })
        .expect("open_tab");

    let panes = server.wait_for_panes("rimz-solo:solo", 2);
    assert_eq!(
        panes.len(),
        2,
        "a single-pane layout is born `sidebar | work`: {panes:?}",
    );
    assert_eq!(
        panes.iter().filter(|p| p.left == 0).count(),
        1,
        "the sidebar docks at the left edge: {panes:?}",
    );
    let work = panes
        .iter()
        .find(|p| p.left > 0)
        .expect("a work pane to the right of the sidebar");
    let want_cwd = cwd.path().canonicalize().expect("canonicalize cwd");
    assert_eq!(
        Path::new(&work.path).canonicalize().ok().as_deref(),
        Some(want_cwd.as_path()),
        "the work pane runs in the tab cwd: {work:?}",
    );

    // `focus: false` leaves the session on its original window, not the new tab.
    assert_ne!(
        server.display("rimz-solo", "#{window_name}"),
        "solo",
        "focus: false should not switch the session's current window",
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

/// `paste_text` injects one bracketed paste (`ESC[200~` … `ESC[201~`) wrapping
/// the literal payload — the steer/queue delivery path. A bare shell renders
/// the markers literally, so the inner text still lands in the pane; assert the
/// payload arrives byte-for-byte. A leading dash is the regression guard: the
/// `send-keys -l --` spelling must never re-read the bytes as flags or key names.
#[test]
fn paste_text_delivers_the_literal_payload() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("paste");
    let pane_id = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("paste".to_owned()),
            ..Default::default()
        })
        .expect("list_panes")
        .panes[0]
        .pane_id
        .clone();

    let payload = "-rf rimz-paste-marker";
    server
        .backend
        .paste_text(&pane_id, payload)
        .expect("paste_text");

    let capture = capture_pane_until(
        &server.backend,
        &pane_id,
        "rimz-paste-marker",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains(payload),
        "the pasted payload should arrive contiguous and byte-safe, got: {capture:?}",
    );
}

/// `focused_client_panes` reads each client's focused pane from `list-clients`.
/// A detached session has no client, so it reports nothing; an attached client
/// focuses the session's pane. Drives the hook-ingestion pane-recovery probe.
#[test]
fn focused_client_panes_tracks_the_attached_client() {
    require_tmux!();

    let server = TmuxServer::new();
    server.ensure_with_shell("focus");
    let pane_id = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("focus".to_owned()),
            ..Default::default()
        })
        .expect("list_panes")
        .panes[0]
        .pane_id
        .clone();

    // No client attached: list-clients is empty, so the focus set is too.
    let detached = server
        .backend
        .focused_client_panes(ClientFocusOptions {
            session_name: Some("focus".to_owned()),
            ..Default::default()
        })
        .expect("focused_client_panes detached");
    assert!(
        detached.is_empty(),
        "a detached session focuses no client panes: {detached:?}",
    );

    // Attach a client; its focused pane is the session's lone pane.
    let _client = AttachedTmuxClient::attach(&server.socket, "focus", 200, 50);
    let deadline = Instant::now() + Duration::from_secs(10);
    let focused = loop {
        let panes = server
            .backend
            .focused_client_panes(ClientFocusOptions {
                session_name: Some("focus".to_owned()),
                ..Default::default()
            })
            .expect("focused_client_panes attached");
        if !panes.is_empty() || Instant::now() >= deadline {
            break panes;
        }
        thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(
        focused,
        vec![pane_id],
        "an attached client focuses the session's lone pane: {focused:?}",
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
                resume_tabs: Vec::new(),
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
    std::fs::write(
        &path,
        "#!/bin/sh\nprintf '\\033]2;rimz-sidebar\\007'\nsleep 600\n",
    )
    .expect("write stub");
    chmod_executable(&path);
    (dir, path)
}

fn write_sleeping_agent_shim(env: &Env, agent: &str) -> PathBuf {
    let dir = env.home_root.join("agent-bin");
    std::fs::create_dir_all(&dir).expect("mkdir agent bin");
    let path = dir.join(agent);
    std::fs::write(
        &path,
        "#!/bin/sh\n\
         printf ready > \"$RIMZ_TEST_AGENT_READY\"\n\
         trap 'exit 0' HUP TERM INT\n\
         while :; do sleep 1; done\n",
    )
    .expect("write agent shim");
    chmod_executable(&path);
    dir
}

fn chmod_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod");
    }
}

fn path_with_front(dir: &Path) -> String {
    let original = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&original));
    std::env::join_paths(paths)
        .expect("join PATH")
        .to_string_lossy()
        .into_owned()
}

fn wait_for_path(path: &Path, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("{message}: {}", path.display());
}

fn wait_for_agent_tombstone(env: &Env, agent_id: &str) {
    let key = (AgentKind::new_unchecked("claude"), agent_id.into());
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let events = env.read_events();
        let ended = rimz::ledger::snapshot::agent_tombstones_for_events(&events);
        if ended.contains(&key) {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("agent.ended tombstone was not recorded for {agent_id}");
}

fn plan_from_env(env: &Env) -> rimz::resume::ResumePlan {
    let projection = env
        .ledger()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("audit projection");
    let ended = rimz::ledger::snapshot::agent_tombstones_for_events(&projection.events);
    rimz::resume::plan_resume(
        &projection.agents,
        &ended,
        rimz::resume::DEFAULT_RESUME_MAX,
        |path| path.is_dir(),
        &env.rimz_bin(),
    )
}

fn recv_presence_line_until<F>(
    rx: &std::sync::mpsc::Receiver<Option<rimz::mux::tmux::ControlLine>>,
    budget: Duration,
    label: &str,
    mut matches: F,
) -> rimz::mux::tmux::ControlLine
where
    F: FnMut(&rimz::mux::tmux::ControlLine) -> bool,
{
    let deadline = Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(Some(line)) if matches(&line) => return line,
            Ok(Some(_)) => {}
            Ok(None) => panic!("presence stream ended before {label}"),
            Err(err) => panic!("timed out waiting for {label}: {err}"),
        }
    }
}

/// The control-mode presence stream surfaces typed subscription changes — the
/// tmux fast path the elder sidebar consumes. Command changes and window closes
/// must produce typed control lines within the budget, and killing the server
/// must end the stream (`None`) rather than wedging it, so a dead watcher
/// degrades to the poll instead of a stuck frame.
#[test]
fn presence_watch_streams_typed_lines_and_ends_with_the_server() {
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

    // Drain on a helper thread so the main thread owns the timeout. Initial
    // subscription values race with the first stimulus, so each assertion
    // filters for the line shape it caused.
    let (tx, rx) = std::sync::mpsc::channel::<Option<rimz::mux::tmux::ControlLine>>();
    let drain = thread::spawn(move || {
        while let Some(line) = watch.next_line() {
            let _ = tx.send(Some(line));
        }
        let _ = tx.send(None);
    });

    server.tmux(&["send-keys", "-t", "presence:0", "exec sleep 30", "Enter"]);
    recv_presence_line_until(
        &rx,
        Duration::from_secs(5),
        "sleep command change",
        |line| {
            matches!(
                line,
                rimz::mux::tmux::ControlLine::Subscription {
                    command: Some(command),
                    ..
                } if command == "sleep"
            )
        },
    );

    server.tmux(&["new-window", "-d", "-t", "presence", "-n", "gone", "sh"]);
    recv_presence_line_until(&rx, Duration::from_secs(5), "new window presence", |line| {
        matches!(line, rimz::mux::tmux::ControlLine::Nudge)
            || matches!(
                line,
                rimz::mux::tmux::ControlLine::Subscription {
                    command: Some(command),
                    ..
                } if command == "sh"
            )
    });

    server.tmux(&["kill-window", "-t", "presence:gone"]);
    recv_presence_line_until(&rx, Duration::from_secs(5), "window close", |line| {
        matches!(line, rimz::mux::tmux::ControlLine::WindowClosed { .. })
    });

    server.tmux(&["kill-server"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(err) => panic!("a dead server did not end the stream: {err}"),
        }
    }
    drain.join().expect("drain thread");
}
