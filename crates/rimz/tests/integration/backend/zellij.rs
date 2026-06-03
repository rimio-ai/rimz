//! Live Zellij backend tests for the M0b spike.
//!
//! Each test spawns a real `zellij` server under its own throwaway
//! `XDG_RUNTIME_DIR` (Zellij locates its server socket there) and drives the
//! `ZellijBackend` against it via [`ZellijBackend::with_runtime_dir`]. The
//! per-test runtime dir is the isolation seam — it gives every test a private
//! server, so the file runs in parallel and concurrently across git worktrees
//! with no shared lock. Mirrors the tmux backend's `with_socket` isolation.
//! The whole file becomes a no-op (early-return per test, message printed once)
//! when the `zellij` binary is not on PATH. The trace-shim wakeup-walk test
//! that verifies the broadcast `zellij pipe` invocation lives in a separate
//! file (`wakeup_pipe.rs`).

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::feed::PaneRef;
use rimz::ids::{MuxName, WorkspaceId};
use rimz::mux::{
    DaemonView, HostPane, MuxBackend, PaneListOptions, SessionHealth, SidebarPaneOptions,
    ZellijBackend, zellij,
};
use tempfile::TempDir;

use crate::common::CommandTimeoutExt;

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

/// A short-prefixed throwaway `XDG_RUNTIME_DIR` for one test. Zellij locates its
/// server socket there, so a private dir gives each test its own server — the
/// isolation that lets these tests run in parallel and across worktrees with no
/// shared lock. The `rz` prefix + 6 random bytes keeps the socket path (and
/// rimz's own per-instance wakeup socket beneath it) under the 108-byte AF_UNIX
/// limit; `TempDir::new`'s long default prefix would not.
fn scoped_runtime_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("rz")
        .rand_bytes(6)
        .tempdir()
        .expect("xdg runtime tempdir")
}

/// A `zellij` command pinned to `xdg` as `XDG_RUNTIME_DIR`. Every raw `zellij`
/// call a test makes goes through this one path to stay on the test's private
/// server — the test-side counterpart to `ZellijBackend::cmd`. The single
/// chokepoint, so no stray command can leak to the user's default server.
fn scoped_zellij(xdg: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("zellij");
    cmd.env("XDG_RUNTIME_DIR", xdg);
    cmd
}

/// Owns a live Zellij session for the duration of one test, on its own private
/// `XDG_RUNTIME_DIR`. Spawned via a portable-pty so the child has the terminal
/// it expects; the master is kept alive (and silently drained) to avoid
/// SIGHUP'ing the session. The runtime dir is held here so it outlives the
/// session and the `Drop` teardown below.
struct ZellijSession {
    name: String,
    xdg: TempDir,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    _reader_thread: Option<std::thread::JoinHandle<()>>,
}

impl ZellijSession {
    fn spawn(name: impl Into<String>) -> Self {
        let xdg = scoped_runtime_dir();
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
        // Pin the attaching client to the test's private server. `CommandBuilder`
        // seeds its env from the current process, so this overrides one var and
        // leaves PATH and friends intact.
        cmd.env("XDG_RUNTIME_DIR", xdg.path());
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
            xdg,
            _master: pair.master,
            _child: child,
            _reader_thread: Some(reader_thread),
        };
        wait_until_session_ready(session.xdg.path(), &session.name);
        session
    }
}

impl Drop for ZellijSession {
    fn drop(&mut self) {
        let _ = scoped_zellij(self.xdg.path())
            .args(["delete-session", &self.name, "--force"])
            .bounded_output();
    }
}

/// Tear down a runtime-scoped session even if an assertion panics first. Used by
/// tests that birth a background session directly (no attached PTY client). The
/// test owns the `XDG_RUNTIME_DIR` tempdir; this only needs its path.
struct ScopedSessionCleanup {
    name: String,
    xdg: PathBuf,
}

impl Drop for ScopedSessionCleanup {
    fn drop(&mut self) {
        let _ = scoped_zellij(&self.xdg)
            .args(["delete-session", &self.name, "--force"])
            .bounded_output();
    }
}

/// Poll until `session` is active enough to answer `action` commands, not just
/// listed. A freshly `attach --create`d session can appear in `list-sessions` a
/// beat before its server accepts actions; under the parallelism this file now
/// enables, that gap is real, so gate on a lightweight action (`query-tab-names`,
/// which a default session always answers) succeeding rather than on the bare
/// listing. Sessions take 300–800 ms to come up on a quiet host; we give it 30 s
/// for slow/loaded CI machines.
fn wait_until_session_ready(xdg: &Path, name: &str) {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        let ready = scoped_zellij(xdg)
            .args(["--session", name, "action", "query-tab-names"])
            .bounded_output()
            .is_ok_and(|out| out.status.success());
        if ready {
            return;
        }
        if Instant::now() > deadline {
            panic!("zellij session {name} never became ready for actions");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn capture_pty_output(spec: &rimz::mux::CommandSpec, duration: Duration) -> Vec<u8> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");
    let mut cmd = CommandBuilder::new(&spec.program);
    cmd.args(spec.args.iter().map(String::as_str));
    for (key, value) in &spec.env {
        cmd.env(key, value);
    }
    let mut child = pair.slave.spawn_command(cmd).expect("spawn zellij");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone reader");
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
        output
    });

    std::thread::sleep(duration);
    let _ = child.kill();
    let _ = child.wait();
    drop(pair.master);
    reader_thread.join().expect("join reader")
}

fn unique_session_name(prefix: &str) -> String {
    let id = uuid::Uuid::now_v7().simple().to_string();
    format!("rimz-{prefix}-{}", &id[..12])
}

fn sidebar_command_stub() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("stub dir");
    let path = dir.path().join("rimz-stub");
    std::fs::write(&path, "#!/bin/sh\nsleep 30\n").expect("write stub");
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
    let session = ZellijSession::spawn(&name);

    let listed = ZellijBackend::with_runtime_dir(session.xdg.path())
        .list_sessions()
        .expect("list_sessions succeeds against a live zellij");
    assert!(
        listed.iter().any(|s| s == &name),
        "expected session {name} in {listed:?}",
    );
}

/// Zellij 0.44.3 suppresses terminal mouse reporting when an attach command
/// explicitly passes `options --mouse-mode true`. Rimz keeps the enabled case
/// implicit so clicks reach the tab bar and sidebar, while still applying the
/// rest of the room options.
#[test]
fn attach_command_keeps_terminal_mouse_reporting_enabled() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("mouse");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let spec = ZellijBackend::with_runtime_dir(xdg.path())
        .attach_command(&name, &rimz::config::MultiplexerConfig::default());
    assert!(
        !spec
            .args
            .windows(2)
            .any(|pair| pair[0] == "--mouse-mode" && pair[1] == "true"),
        "Zellij 0.44.3 disables mouse reporting for `--mouse-mode true`: {spec:?}",
    );

    let output = capture_pty_output(&spec, Duration::from_millis(900));
    assert!(
        output
            .windows(b"\x1b[?1006h".len())
            .any(|w| w == b"\x1b[?1006h")
            && output
                .windows(b"\x1b[?1000h".len())
                .any(|w| w == b"\x1b[?1000h"),
        "attach output did not enable terminal mouse reporting",
    );
}

/// `open_sidebar` births the full Zellij room shape once: left sidebar, focused
/// right terminal, bottom bar, running command panes, and a default tab template
/// that gives future tabs the same sidebar + terminal pair.
#[test]
fn open_sidebar_births_native_layout_and_template() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("sidebar");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_command_stub();
    ZellijBackend::with_runtime_dir(xdg.path())
        .open_sidebar(
            &SidebarPaneOptions {
                session_name: name.clone(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-sidebar-test")),
                cwd: cwd.path().to_path_buf(),
                width_percent: 30,
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_panes: Vec::new(),
            },
            None,
        )
        .expect("open_sidebar");

    let panes = wait_for_pane_count(xdg.path(), &name, 2);
    assert!(
        panes.len() >= 2,
        "layout should create a sidebar + terminal pane in {name}: {panes:?}",
    );
    assert_sidebar_is_left_thirty_percent(xdg.path(), &name);
    assert_session_has_bottom_bar(xdg.path(), &name);
    assert_sidebars_not_held(xdg.path(), &name, "initial tab");

    let template = new_tab_template_dump(xdg.path(), &name);
    assert!(
        template.contains("rimz-sidebar"),
        "new tab template should carry the sidebar pane:\n{template}",
    );
    assert!(
        template.contains("pane focus=true"),
        "new tab template should carry an explicit focused right terminal:\n{template}",
    );

    open_new_tab(xdg.path(), &name);
    wait_for_tab_count(xdg.path(), &name, 2);
    assert_sidebars_not_held(xdg.path(), &name, "new tab");

    for tab in tab_ids(xdg.path(), &name) {
        let terminals = nonplugin_titles_in_tab(xdg.path(), &name, tab);
        let has_sidebar = terminals.iter().any(|t| t == "rimz-sidebar");
        let has_terminal = terminals.iter().any(|t| t != "rimz-sidebar");
        assert!(
            has_sidebar && has_terminal,
            "tab {tab} should carry the sidebar and a right terminal, got {terminals:?}",
        );
        let focused = focused_nonplugin_title_in_tab(xdg.path(), &name, tab)
            .unwrap_or_else(|| panic!("tab {tab} has no focused terminal pane"));
        assert_ne!(
            focused, "rimz-sidebar",
            "tab {tab} focuses the sidebar; focus must land on the right terminal",
        );
    }
}

/// Re-running `open_sidebar` against a *live* session takes the no-op arm of the
/// session-state branch: it neither errors nor injects a second sidebar, and the
/// 30% layout is preserved. (The exited arm — delete then rebirth — cannot be
/// driven headlessly: an EXITED-resurrectable session requires a prior attach +
/// serialization. Its classifier is covered by the `session_state` unit test.)
#[test]
fn open_sidebar_on_live_session_is_idempotent() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("idem");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = SidebarPaneOptions {
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-sidebar-idem")),
        cwd: cwd.path().to_path_buf(),
        width_percent: 30,
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_panes: Vec::new(),
    };

    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    backend
        .open_sidebar(&opts, None)
        .expect("first open_sidebar");
    let first = wait_for_pane_count(xdg.path(), &name, 2);
    assert!(
        first.len() >= 2,
        "first birth should create a sidebar + terminal pane: {first:?}",
    );

    // Second call sees a live session and must leave it untouched.
    backend
        .open_sidebar(&opts, None)
        .expect("second open_sidebar");
    let second = wait_for_pane_count(xdg.path(), &name, 2);
    assert_eq!(
        second.len(),
        first.len(),
        "re-opening a live session must not add or drop panes: {second:?}",
    );
    assert_sidebar_is_left_thirty_percent(xdg.path(), &name);
}

/// The pre-attach health gate: an absent room is born clean and RUNNING
/// (`Reborn`), a probe of the resulting live room reports `Healthy`, and a second
/// gate call leaves the working panes untouched (`Healthy`, no rebirth). This is
/// the un-bypassable check that replaces the old "attach and hope" path.
#[test]
fn ensure_clean_session_births_running_then_is_idempotent() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("cleanroom");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = SidebarPaneOptions {
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-cleanroom")),
        cwd: cwd.path().to_path_buf(),
        width_percent: 30,
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_panes: Vec::new(),
    };
    let backend = ZellijBackend::with_runtime_dir(xdg.path());

    // Absent → born clean and running.
    assert_eq!(
        backend
            .ensure_clean_session(&opts, None)
            .expect("ensure_clean_session births the absent room"),
        SessionHealth::Reborn,
    );
    let born = wait_for_pane_count(xdg.path(), &name, 2);
    assert!(
        born.len() >= 2,
        "the gate should birth a sidebar + terminal pane: {born:?}",
    );
    // No pane is held at a "Waiting to run" prompt — the room came up running.
    assert_sidebars_not_held(xdg.path(), &name, "reborn room");

    // A read-only probe of the now-live, clean room reports healthy.
    assert_eq!(
        backend
            .probe_session_health(&name)
            .expect("probe a live clean room"),
        SessionHealth::Healthy,
    );

    // A clean live room is left untouched — the gate never rebirths working panes.
    assert_eq!(
        backend
            .ensure_clean_session(&opts, None)
            .expect("ensure_clean_session on a clean live room"),
        SessionHealth::Healthy,
    );
    let again = wait_for_pane_count(xdg.path(), &name, 2);
    assert_eq!(
        again.len(),
        born.len(),
        "the gate must not add or drop panes on a clean room: {again:?}",
    );
}

/// A *live* session that has no sidebar (the renderer self-closed or crashed
/// while the session itself survived, or a prior launch was skipped and the
/// session was born by a plain `attach --create`) must regain one on the next
/// `open_sidebar` — a sidebar-less rimz session is non-functional, and the
/// only way to place a left pane in Zellij is at session birth. Regression
/// test for "fresh `rimz .` shows a single full-width pane, no sidebar" on a
/// workspace whose session already existed without a sidebar.
#[test]
fn open_sidebar_heals_a_live_session_missing_its_sidebar() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("nosb");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    // Birth a live session with a plain, sidebar-less layout. The pane runs a
    // long sleep so the unattached background session stays alive deterministically.
    let layout = cwd.path().join("plain.kdl");
    std::fs::write(
        &layout,
        "layout {\n    pane command=\"sleep\" {\n        args \"60\"\n    }\n}\n",
    )
    .expect("write plain layout");
    let created = scoped_zellij(xdg.path())
        .args(["attach", "--create-background", &name, "options"])
        .arg("--default-cwd")
        .arg(cwd.path())
        .arg("--default-layout")
        .arg(&layout)
        .bounded_status()
        .expect("create plain session");
    assert!(created.success(), "create-background failed for {name}");
    let plain = wait_for_pane_count(xdg.path(), &name, 1);
    assert!(
        !plain.is_empty(),
        "plain session should have a pane before open_sidebar: {plain:?}",
    );

    // `open_sidebar` must heal it: tear the sidebar-less session down and
    // rebirth one that carries the sidebar.
    let (_stub_dir, stub) = sidebar_command_stub();
    ZellijBackend::with_runtime_dir(xdg.path())
        .open_sidebar(
            &SidebarPaneOptions {
                session_name: name.clone(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-sidebar-nosb")),
                cwd: cwd.path().to_path_buf(),
                width_percent: 30,
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_panes: Vec::new(),
            },
            None,
        )
        .expect("open_sidebar");

    let healed = wait_for_pane_count(xdg.path(), &name, 2);
    assert!(
        healed.len() >= 2,
        "open_sidebar should rebirth a sidebar-less live session with a sidebar: {healed:?}",
    );
    assert_sidebar_is_left_thirty_percent(xdg.path(), &name);
}

/// The sidebar layout replaces Zellij's default tab template, so it must re-add
/// the bottom bar plugin itself. Assert the born session actually carries it —
/// not just that the layout string mentions it.
fn assert_session_has_bottom_bar(xdg: &Path, session: &str) {
    let output = scoped_zellij(xdg)
        .args(["--session", session, "action", "list-panes", "-j", "-a"])
        .bounded_output()
        .expect("list-panes for bar check");
    assert!(output.status.success(), "list-panes for bar check failed");
    let panes: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("list-panes bar json");
    let has_bar = panes.as_array().expect("pane array").iter().any(|pane| {
        pane.get("is_plugin").and_then(|v| v.as_bool()) == Some(true)
            && pane
                .get("title")
                .and_then(|v| v.as_str())
                .is_some_and(|title| title.contains("compact-bar"))
    });
    assert!(
        has_bar,
        "session {session} should carry a bottom bar plugin: {panes:?}"
    );
}

/// Open a second tab the way a user would, from the default tab template.
fn open_new_tab(xdg: &Path, session: &str) {
    let output = scoped_zellij(xdg)
        .args(["--session", session, "action", "new-tab"])
        .bounded_output()
        .expect("new-tab");
    assert!(
        output.status.success(),
        "new-tab failed for {session}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Parsed `list-panes -j -a` for `session`, or an empty array on any failure.
fn list_panes_json(xdg: &Path, session: &str) -> serde_json::Value {
    scoped_zellij(xdg)
        .args(["--session", session, "action", "list-panes", "-j", "-a"])
        .bounded_output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| serde_json::from_slice(&out.stdout).ok())
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()))
}

fn assert_sidebars_not_held(xdg: &Path, session: &str, context: &str) {
    let panes = list_panes_json(xdg, session);
    let sidebars: Vec<&serde_json::Value> = panes
        .as_array()
        .expect("pane array")
        .iter()
        .filter(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
        })
        .collect();
    assert!(
        !sidebars.is_empty(),
        "rimz-sidebar pane missing while checking {context}:\n{panes}",
    );
    for sidebar in sidebars {
        assert_ne!(
            sidebar.get("is_held").and_then(|value| value.as_bool()),
            Some(true),
            "sidebar command pane is waiting for Enter instead of running in {context}:\n{sidebar}",
        );
    }
}

/// Dump just the `new_tab_template` section for readable assertions.
fn new_tab_template_dump(xdg: &Path, session: &str) -> String {
    let output = scoped_zellij(xdg)
        .args(["--session", session, "action", "dump-layout"])
        .bounded_output()
        .expect("dump-layout");
    assert!(
        output.status.success(),
        "dump-layout failed for {session}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let dump = String::from_utf8_lossy(&output.stdout);
    let start = dump
        .find("new_tab_template")
        .unwrap_or_else(|| panic!("dump-layout has no new_tab_template:\n{dump}"));
    dump[start..].to_owned()
}

/// Distinct tab ids that currently hold a non-plugin pane.
fn tab_ids(xdg: &Path, session: &str) -> Vec<u64> {
    let panes = list_panes_json(xdg, session);
    let mut ids: Vec<u64> = panes
        .as_array()
        .map(|panes| {
            panes
                .iter()
                .filter(|p| p.get("is_plugin").and_then(|v| v.as_bool()) == Some(false))
                .filter_map(|p| p.get("tab_id").and_then(|v| v.as_u64()))
                .collect()
        })
        .unwrap_or_default();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Titles of the non-plugin panes in `tab`.
fn nonplugin_titles_in_tab(xdg: &Path, session: &str, tab: u64) -> Vec<String> {
    let panes = list_panes_json(xdg, session);
    panes
        .as_array()
        .map(|panes| {
            panes
                .iter()
                .filter(|p| p.get("is_plugin").and_then(|v| v.as_bool()) == Some(false))
                .filter(|p| p.get("tab_id").and_then(|v| v.as_u64()) == Some(tab))
                .filter_map(|p| p.get("title").and_then(|v| v.as_str()).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Title of the focused non-plugin pane in `tab`, if any.
fn focused_nonplugin_title_in_tab(xdg: &Path, session: &str, tab: u64) -> Option<String> {
    let panes = list_panes_json(xdg, session);
    panes.as_array()?.iter().find_map(|p| {
        (p.get("is_plugin").and_then(|v| v.as_bool()) == Some(false)
            && p.get("tab_id").and_then(|v| v.as_u64()) == Some(tab)
            && p.get("is_focused").and_then(|v| v.as_bool()) == Some(true))
        .then(|| {
            p.get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned()
        })
    })
}

/// Poll until at least `want` distinct tabs hold a non-plugin pane, or time out.
fn wait_for_tab_count(xdg: &Path, session: &str, want: usize) -> Vec<u64> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let ids = tab_ids(xdg, session);
        if ids.len() >= want || Instant::now() >= deadline {
            return ids;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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

    let name = unique_session_name("selfclose");
    let cwd = TempDir::new().expect("cwd tempdir");
    // One private XDG_RUNTIME_DIR for everything: zellij's *server* socket and
    // rimz's *wakeup* socket both live under it, so every zellij call touching
    // this session shares it.
    let xdg = scoped_runtime_dir();
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
        .bounded_status()
        .expect("create background session");
    assert!(created.success(), "create-background failed for {name}");

    assert!(
        wait_for_nonplugin_panes(xdg.path(), &name, 2, Duration::from_secs(15)),
        "expected sidebar + terminal before self-close for {name}",
    );
    assert!(
        wait_for_nonplugin_panes(xdg.path(), &name, 0, Duration::from_secs(7)),
        "lone sidebar did not close promptly after the terminal exited for {name}",
    );

    // On exit the sidebar removes its heartbeat (RuntimeFileGuard); otherwise it
    // stays mtime-fresh for the TTL and a later `rimz` launch skips relaunch,
    // rebirthing the session with no sidebar. Assert none lingers once gone.
    let heartbeat_dir = xdg
        .path()
        .join("rimz")
        .join("ws_0123456789abcdef01234567")
        .join("heartbeat");
    assert!(
        wait_for_no_sidebar_heartbeat(&heartbeat_dir, Duration::from_secs(5)),
        "sidebar heartbeat should be removed on self-close, found: {:?}",
        std::fs::read_dir(&heartbeat_dir)
            .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
}

/// Poll until no `sidebar.*.json` heartbeat remains in `dir` (a missing dir
/// counts as none), or the timeout elapses.
fn wait_for_no_sidebar_heartbeat(dir: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let lingering = std::fs::read_dir(dir)
            .map(|entries| {
                entries.flatten().any(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("sidebar.") && n.ends_with(".json"))
                })
            })
            .unwrap_or(false);
        if !lingering {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Layout for the self-close test: a real `rimz-sidebar` renderer on the left
/// (env-scoped to a throwaway XDG dir) and a terminal that exits after a beat.
/// Both panes are `close_on_exit`, so each disappears when its command ends.
fn self_close_layout(session: &str, rimz: &Path, sidebar: &Path, xdg: &Path) -> String {
    let q = |s: String| serde_json::to_string(&s).expect("kdl escape");
    // Keep the data tick deliberately slow. The test should pass via the
    // resize-triggered fast self-close probe, not by waiting for the normal
    // snapshot backstop.
    let serve = sidebar_serve_command_with_tick(session, rimz, sidebar, xdg, 20);
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

fn sidebar_serve_command_with_tick(
    session: &str,
    rimz: &Path,
    sidebar: &Path,
    xdg: &Path,
    tick_seconds: u64,
) -> String {
    format!(
        "XDG_STATE_HOME={xdg} XDG_RUNTIME_DIR={xdg} RIMZ_BIN={rimz} \
         exec {sidebar} serve --mux zellij --workspace-id ws_0123456789abcdef01234567 \
         --session-name {session} --tick-seconds {tick_seconds}",
        xdg = xdg.display(),
        rimz = rimz.display(),
        sidebar = sidebar.display(),
    )
}

/// Poll `list_panes` until a terminal pane reports its command metadata, then
/// return the listing (bounded). A pane can surface in `list-panes` a beat
/// before Zellij fills in command/cwd/pid — under load that window widens — so a
/// test that asserts on that metadata waits for it here rather than for the bare
/// pane to exist.
fn wait_for_pane_with_command(xdg: &Path, session: &str) -> Vec<PaneRef> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let panes = ZellijBackend::with_runtime_dir(xdg)
            .list_panes(PaneListOptions {
                session_name: Some(session.to_owned()),
                ..Default::default()
            })
            .unwrap_or_default();
        let ready = panes.iter().any(|pane| {
            pane.command
                .as_deref()
                .is_some_and(|command| !command.is_empty())
        });
        if ready || Instant::now() >= deadline {
            return panes;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Poll `list_panes` until at least `want` panes appear (bounded). Returns the
/// last observation either way so the caller can assert and print it.
fn wait_for_pane_count(xdg: &Path, session: &str, want: usize) -> Vec<PaneRef> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let panes = ZellijBackend::with_runtime_dir(xdg)
            .list_panes(PaneListOptions {
                session_name: Some(session.to_owned()),
                ..Default::default()
            })
            .unwrap_or_default();
        if panes.len() >= want || Instant::now() >= deadline {
            return panes;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Count a runtime-scoped session's non-plugin (terminal) panes. A session
/// whose tab has emptied answers `action list-panes` with only plugin panes
/// (tab/status bars); a torn-down session fails the call. Both map to zero.
fn session_nonplugin_count(xdg: &Path, name: &str) -> usize {
    scoped_zellij(xdg)
        .args(["--session", name, "action", "list-panes", "-j", "-a"])
        .bounded_output()
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

fn assert_sidebar_is_left_thirty_percent(xdg: &Path, session: &str) {
    let output = scoped_zellij(xdg)
        .args(["--session", session, "action", "list-panes", "-j", "-a"])
        .bounded_output()
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

/// A `BackgroundViewOptions` for a session whose host is a long-lived `sleep`
/// and whose sidebar runs the alive-keeping `stub`, so the launched tab is a
/// faithful `sidebar | host`.
fn background_view_opts(session: &str, stub: &Path) -> rimz::mux::BackgroundViewOptions {
    rimz::mux::BackgroundViewOptions {
        name: "rimzd".to_owned(),
        hosts: vec![rimz::mux::HostPane {
            argv: vec!["sleep".to_owned(), "120".to_owned()],
            cwd: std::env::temp_dir(),
        }],
        sidebar: SidebarPaneOptions {
            session_name: session.to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-bgview")),
            cwd: std::env::temp_dir(),
            width_percent: 30,
            rimz_bin: stub.to_path_buf(),
            replace_existing: false,
            config: rimz::config::MultiplexerConfig::default(),
            resume_panes: Vec::new(),
        },
    }
}

/// `open_background_view` opens a dedicated, named tab born `sidebar | host`, and
/// is idempotent on that tab name: a second call launches nothing.
#[test]
fn open_background_view_creates_named_tab_idempotently() {
    require_zellij!();

    let name = unique_session_name("bgview");
    let session = ZellijSession::spawn(&name);
    let backend = ZellijBackend::with_runtime_dir(session.xdg.path());
    let (_stub_dir, stub) = sidebar_command_stub();

    let opts = background_view_opts(&name, &stub);

    let first = backend.open_background_view(&opts).expect("first launch");
    assert_eq!(first, rimz::mux::BackgroundViewLaunch::Launched);
    assert!(
        wait_for_tab_named(session.xdg.path(), &name, "rimzd"),
        "expected a rimzd tab after launch",
    );

    let second = backend.open_background_view(&opts).expect("second launch");
    assert_eq!(
        second,
        rimz::mux::BackgroundViewLaunch::AlreadyRunning,
        "relaunching into a session that already carries the view is a no-op",
    );
}

/// `open_background_view` (the *late-add* path: a session born without the daemon
/// tab gains one later) must not leave the user's focus on the appended tab.
/// Zellij `new-tab` creates *and focuses* the new tab, so without the focus
/// restore the session's active tab becomes `rimzd` and the imminent `attach`
/// dumps the user straight into a host pane. `ZellijSession` keeps a real client
/// attached, so `dump-layout` marks the active tab `focus=true`; we assert it is
/// not the `rimzd` tab.
#[test]
fn open_background_view_keeps_focus_off_the_daemon_tab() {
    require_zellij!();

    let name = unique_session_name("bgfocus");
    let session = ZellijSession::spawn(&name);
    let (_stub_dir, stub) = sidebar_command_stub();

    ZellijBackend::with_runtime_dir(session.xdg.path())
        .open_background_view(&background_view_opts(&name, &stub))
        .expect("open_background_view");
    assert!(
        wait_for_tab_named(session.xdg.path(), &name, "rimzd"),
        "expected a rimzd tab after launch",
    );

    let focused = wait_for_focused_tab_off_daemon(session.xdg.path(), &name)
        .expect("an attached client should report a focused tab");
    assert_ne!(
        focused, "rimzd",
        "focus was left on the rimzd daemon tab; an attach would dump the user into a host pane",
    );
}

/// `open_sidebar` with a daemon view leads the session with the daemon tab.
/// Zellij can't reorder tabs after birth, so the session is born from a two-tab
/// layout — the daemon (`rimzd`) tab first, the focused working tab second — and
/// this asserts `rimzd` leads the resulting tab list.
#[test]
fn open_sidebar_with_a_daemon_leads_with_the_daemon_tab() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("bgfirst");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();

    let daemon = DaemonView {
        name: "rimzd".to_owned(),
        hosts: vec![HostPane {
            argv: vec!["sleep".to_owned(), "120".to_owned()],
            cwd: cwd.path().to_path_buf(),
        }],
    };
    ZellijBackend::with_runtime_dir(xdg.path())
        .open_sidebar(
            &SidebarPaneOptions {
                session_name: name.clone(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-bgfirst")),
                cwd: cwd.path().to_path_buf(),
                width_percent: 30,
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_panes: Vec::new(),
            },
            Some(&daemon),
        )
        .expect("open_sidebar with daemon");

    assert!(
        wait_for_tab_named(xdg.path(), &name, "rimzd"),
        "expected a rimzd tab after birth",
    );
    assert!(
        wait_for_first_tab(xdg.path(), &name, "rimzd"),
        "daemon tab must lead the session; saw {:?}",
        tab_names_in_order(xdg.path(), &name),
    );
    // Two tabs: the daemon tab and the working tab born beside it.
    assert_eq!(
        tab_names_in_order(xdg.path(), &name).len(),
        2,
        "birth layout should produce exactly the daemon + working tabs",
    );
}

/// The name of the tab Zellij marks `focus=true` in `dump-layout` — the active
/// tab an attaching client lands on. `None` until an attached client has
/// realized one (the marker only renders for a live client).
fn focused_tab_name(xdg: &Path, session: &str) -> Option<String> {
    let out = scoped_zellij(xdg)
        .args(["--session", session, "action", "dump-layout"])
        .bounded_output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim_start)
        .find(|line| line.starts_with("tab ") && line.contains("focus=true"))
        .and_then(|line| {
            let start = line.find("name=\"")? + "name=\"".len();
            let rest = &line[start..];
            let end = rest.find('"')?;
            Some(rest[..end].to_owned())
        })
}

/// Poll until the attached client's focused tab settles off `rimzd`, or time
/// out. Returns the last focused tab seen so the caller can assert on it: the
/// fix settles it on the working tab quickly; the unfixed code leaves it pinned
/// to `rimzd` until the deadline.
fn wait_for_focused_tab_off_daemon(xdg: &Path, session: &str) -> Option<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = None;
    loop {
        if let Some(tab) = focused_tab_name(xdg, session) {
            if tab != "rimzd" {
                return Some(tab);
            }
            last = Some(tab);
        }
        if Instant::now() >= deadline {
            return last;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// The session's tab names in tab order (`query-tab-names` prints one per line).
fn tab_names_in_order(xdg: &Path, session: &str) -> Vec<String> {
    let out = scoped_zellij(xdg)
        .args(["--session", session, "action", "query-tab-names"])
        .bounded_output();
    out.ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|line| line.trim().to_owned())
                .filter(|line| !line.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Poll until the session's first tab is `expected`, or time out.
fn wait_for_first_tab(xdg: &Path, session: &str, expected: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if tab_names_in_order(xdg, session).first().map(String::as_str) == Some(expected) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Poll `query-tab-names` until a tab named `tab_name` appears, or time out.
fn wait_for_tab_named(xdg: &Path, session: &str, tab_name: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let listed = scoped_zellij(xdg)
            .args(["--session", session, "action", "query-tab-names"])
            .bounded_output();
        if let Ok(out) = listed
            && out.status.success()
            && String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line.trim() == tab_name)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `wake_sidebar` issues `zellij --session <name> pipe --name rimz::feed --
/// <payload>`. We assert the subprocess returns success even when no
/// pipe-aware client consumes the payload.
#[test]
fn wake_sidebar_pipe_invocation_succeeds() {
    require_zellij!();

    let name = unique_session_name("pipe");
    let session = ZellijSession::spawn(&name);

    let payload = br#"{"kind":"ledger_delta","workspace_id":"ws_test","request_id":"req_test","protocol_version":"rimz.plugin.v3"}"#;
    ZellijBackend::with_runtime_dir(session.xdg.path())
        .wake_sidebar(&name, payload)
        .expect("wake_sidebar succeeds against a live zellij session");
}

/// `list_panes` parses `zellij action list-panes -j -a` JSON. A fresh
/// session has at least one terminal pane (the implicit shell).
#[test]
fn list_panes_with_session_returns_terminals() {
    require_zellij!();

    let name = unique_session_name("panes");
    let session = ZellijSession::spawn(&name);

    // Poll until the implicit shell pane reports its command metadata, not just
    // until it exists: a pane can surface in `list-panes` a beat before Zellij
    // fills in command/cwd/pid, and under load that window widens. This test
    // asserts on that metadata, so it must wait for it.
    let panes = wait_for_pane_with_command(session.xdg.path(), &name);
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
        assert!(
            pane.command
                .as_deref()
                .is_some_and(|command| !command.is_empty()),
            "zellij should report pane_command into PaneRef::command: {pane:?}",
        );
        assert!(
            pane.cwd.as_deref().is_some_and(|cwd| !cwd.is_empty()),
            "zellij should report pane_cwd into PaneRef::cwd: {pane:?}",
        );
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
