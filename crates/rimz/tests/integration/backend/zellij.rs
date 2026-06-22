//! Live Zellij backend tests.
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

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::ids::{MuxName, PaneId, WorkspaceId};
use rimz::mux::{
    ClientFocusOptions, LayoutPanes, MuxBackend, PaneCmd, PaneListOptions, SessionHealth,
    SidebarLiveness, SidebarPaneOptions, SidebarRecovery, SidebarWidth, SplitPaneOptions,
    TabOptions, ZellijBackend, zellij,
};
use rimz::pane::PaneRef;
use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, Env, ScrubSessionEnvExt};

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
///
/// The dir doubles as the scoped `HOME`/`XDG_CONFIG_HOME` (see `scoped_zellij`),
/// and a seeded config keeps Zellij off its first-run setup wizard: a missing
/// config makes the server write one and float the wizard modal, which blocks
/// the screen thread's pane mounts — a wizard session silently drops every
/// `new-pane` while it shows. Zellij prefers (and creates) `$HOME/.config/zellij`
/// over `$XDG_CONFIG_HOME/zellij` (`home_unix.rs` in zellij), so the seed goes
/// to the home-relative path.
fn scoped_runtime_dir() -> TempDir {
    let dir = tempfile::Builder::new()
        .prefix("rz")
        .rand_bytes(6)
        .tempdir()
        .expect("xdg runtime tempdir");
    let zellij_config_dir = dir.path().join(".config").join("zellij");
    std::fs::create_dir_all(&zellij_config_dir).expect("zellij config dir");
    std::fs::write(
        zellij_config_dir.join("config.kdl"),
        "// Hermetic test config: stock behavior, no first-run wizard or tips UI.\nshow_startup_tips false\nshow_release_notes false\n",
    )
    .expect("zellij config.kdl");
    dir
}

/// A `zellij` command pinned to `xdg` as `XDG_RUNTIME_DIR`. Every raw `zellij`
/// call a test makes goes through this one path to stay on the test's private
/// server — the test-side counterpart to `ZellijBackend::cmd`. The single
/// chokepoint, so no stray command can leak to the user's default server.
fn scoped_zellij(xdg: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("zellij");
    cmd.scrub_session_env();
    // The first command against a runtime dir forks the session server, and
    // every pane command inherits the server's env — so scope the whole home
    // surface here, not just the socket root. A real `HOME` leaks the
    // developer's per-machine config and fleet transcript history into
    // renderer panes (the snapshot producer then cold-parses minutes of real
    // `~/.claude` history), and a real `XDG_CONFIG_HOME` leaks their Zellij
    // config into the session under test.
    cmd.env("XDG_RUNTIME_DIR", xdg)
        .env("XDG_STATE_HOME", xdg)
        .env("XDG_CONFIG_HOME", xdg)
        .env("XDG_CACHE_HOME", xdg)
        .env("HOME", xdg);
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
    writer: Box<dyn Write + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    _reader_thread: Option<std::thread::JoinHandle<()>>,
}

impl ZellijSession {
    fn spawn(name: impl Into<String>) -> Self {
        Self::attach_pty(scoped_runtime_dir(), name.into(), true)
    }

    /// Attach a PTY client to a session that already exists on `xdg` (born via
    /// `attach --create-background`). A detached server under load can drop a
    /// pane-exit or relayout outright and only reconciles on the next attach,
    /// so a test asserting prompt close/resize behaviour keeps a client
    /// attached for the session's lifetime.
    fn attach_existing(xdg: TempDir, name: impl Into<String>) -> Self {
        Self::attach_pty(xdg, name.into(), false)
    }

    fn attach_pty(xdg: TempDir, name: String, create: bool) -> Self {
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
        // Pin the attaching client to the test's private server, with the same
        // hermetic home surface as `scoped_zellij` (the client can be the one
        // that forks the server). `CommandBuilder` seeds its env from the
        // current process, so these override and leave PATH and friends intact.
        cmd.scrub_session_env();
        cmd.env("XDG_RUNTIME_DIR", xdg.path());
        cmd.env("XDG_STATE_HOME", xdg.path());
        cmd.env("XDG_CONFIG_HOME", xdg.path());
        cmd.env("XDG_CACHE_HOME", xdg.path());
        cmd.env("HOME", xdg.path());
        if create {
            cmd.args(["attach", "--create", &name]);
        } else {
            cmd.args(["attach", &name]);
        }
        let child = pair.slave.spawn_command(cmd).expect("spawn zellij");
        drop(pair.slave);
        let writer = pair.master.take_writer().expect("pty writer");

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
            writer,
            _child: child,
            _reader_thread: Some(reader_thread),
        };
        wait_until_session_ready(session.xdg.path(), &session.name);
        session
    }

    fn press_alt(&mut self, key: char) {
        self.writer
            .write_all(&[0x1b, key as u8])
            .expect("write alt key");
        self.writer.flush().expect("flush alt key");
    }
}

impl Drop for ZellijSession {
    fn drop(&mut self) {
        let _ = scoped_zellij(self.xdg.path())
            .args(["delete-session", &self.name, "--force"])
            .bounded_output();
    }
}

/// A live client attached to an existing session on the test's private server,
/// held open on a PTY of the given size so the session adopts real terminal
/// geometry (a background birth is tiny until a client attaches). Drop kills
/// the client; session teardown stays with [`ScopedSessionCleanup`].
struct AttachedClient {
    _master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl AttachedClient {
    fn attach(xdg: &Path, name: &str, cols: u16, rows: u16) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new("zellij");
        // The test process may itself run inside a Zellij pane (a client
        // refuses to attach when it believes it is already in a session), and
        // the server captures the spawning env for every pane it creates.
        cmd.scrub_session_env();
        cmd.env("XDG_RUNTIME_DIR", xdg);
        cmd.args(["attach", name]);
        let child = pair.slave.spawn_command(cmd).expect("spawn zellij attach");
        drop(pair.slave);
        // Drain the PTY in the background so the kernel buffer never fills and
        // stalls the client; the thread exits with the PTY on drop.
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        std::thread::spawn(move || {
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

impl Drop for AttachedClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
    cmd.scrub_session_env();
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
    sidebar_stub_alive_for(30)
}

/// A renderer stand-in that sleeps for `seconds` then exits. The width tests
/// wait through an attach and a tab open, which can outlive the shared 30s
/// stub, so they ask for a longer life.
fn sidebar_stub_alive_for(seconds: u32) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("stub dir");
    let path = dir.path().join("rimz-stub");
    std::fs::write(&path, format!("#!/bin/sh\nsleep {seconds}\n")).expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
    }
    (dir, path)
}

mod daemon;
mod presence;
mod self_close;

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
                project_root: cwd.path().to_path_buf(),
                cwd: cwd.path().to_path_buf(),
                width: SidebarWidth::default(),
                birth_size: SidebarWidth::default().birth_size(Some(120)),
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_tabs: Vec::new(),
                refresh_ms: None,
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

#[test]
fn sidebar_focus_command_targets_session_from_outside_room() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("focuscmd");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    backend
        .open_sidebar(
            &SidebarPaneOptions {
                session_name: name.clone(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-focuscmd")),
                project_root: cwd.path().to_path_buf(),
                cwd: cwd.path().to_path_buf(),
                width: SidebarWidth::default(),
                birth_size: SidebarWidth::default().birth_size(Some(200)),
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_tabs: Vec::new(),
                refresh_ms: None,
            },
            None,
        )
        .expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    let sidebar = raw_sidebar_pane(xdg.path(), &name);
    let sidebar_id = sidebar
        .get("id")
        .and_then(|value| value.as_u64())
        .expect("sidebar pane id");
    let tab_id = sidebar
        .get("tab_id")
        .and_then(|value| value.as_u64())
        .expect("sidebar tab id");
    let work_id = expect_list_panes_json(xdg.path(), &name)
        .as_array()
        .expect("pane array")
        .iter()
        .find_map(|pane| {
            (pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("tab_id").and_then(|value| value.as_u64()) == Some(tab_id)
                && pane.get("title").and_then(|value| value.as_str()) != Some("rimz-sidebar"))
            .then(|| pane.get("id").and_then(|value| value.as_u64()))
            .flatten()
        })
        .expect("work pane id");

    let _client = AttachedClient::attach(xdg.path(), &name, 200, 50);
    wait_for_attached_client(xdg.path(), &name);
    let focused_work = scoped_zellij(xdg.path())
        .args([
            "--session",
            &name,
            "action",
            "focus-pane-id",
            &format!("terminal_{work_id}"),
        ])
        .bounded_status()
        .expect("focus work pane");
    assert!(focused_work.success(), "focus work pane failed");
    assert_eq!(
        wait_for_focused_nonplugin_id_in_tab(xdg.path(), &name, tab_id, work_id),
        Some(work_id),
        "fixture must start focused on the work pane",
    );

    let env = Env::new();
    let run_focus_toggle = || {
        let output = env
            .rimz()
            .env("XDG_RUNTIME_DIR", xdg.path())
            .args([
                "--mux",
                "zellij",
                "sidebar",
                "focus",
                "--toggle",
                "--session-name",
                &name,
            ])
            .bounded_output()
            .expect("rimz sidebar focus");
        assert!(
            output.status.success(),
            "rimz sidebar focus failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    };

    run_focus_toggle();
    assert_eq!(
        wait_for_focused_nonplugin_id_in_tab(xdg.path(), &name, tab_id, sidebar_id),
        Some(sidebar_id),
        "out-of-session focus should land on the sidebar pane",
    );

    run_focus_toggle();
    assert_eq!(
        wait_for_focused_nonplugin_id_in_tab(xdg.path(), &name, tab_id, work_id),
        Some(work_id),
        "toggle should return focus to the work pane in the sidebar tab",
    );
}

#[test]
fn open_tab_unfocused_restores_attached_client_focus() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("tabfocus");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = SidebarPaneOptions {
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-tabfocus")),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(200)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    let _client = AttachedClient::attach(xdg.path(), &name, 200, 50);
    wait_for_attached_client(xdg.path(), &name);

    let source_tab = "focus source";
    backend
        .open_tab(&TabOptions {
            session_name: name.clone(),
            title: source_tab.to_owned(),
            cwd: cwd.path().to_path_buf(),
            panes: LayoutPanes {
                columns: vec![vec![PaneCmd {
                    argv: vec!["sleep".to_owned(), "600".to_owned()],
                }]],
            },
            focus: true,
            sidebar: sidebar.clone(),
        })
        .expect("open focused source tab");

    let source_panes = wait_for_named_work_pane_count(xdg.path(), &name, source_tab, 1);
    assert_eq!(
        source_panes.len(),
        1,
        "source tab should have one work pane: {source_panes:?}",
    );
    let source_pane =
        PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", source_panes[0].id));
    let focused = wait_for_focused_client_pane(&backend, &name, &source_pane);
    assert_eq!(
        focused,
        vec![source_pane.clone()],
        "the attached client should focus the source tab before the regression step: {focused:?}",
    );

    let background_tab = "background run";
    backend
        .open_tab(&TabOptions {
            session_name: name.clone(),
            title: background_tab.to_owned(),
            cwd: cwd.path().to_path_buf(),
            panes: LayoutPanes {
                columns: vec![vec![PaneCmd {
                    argv: vec!["sleep".to_owned(), "600".to_owned()],
                }]],
            },
            focus: false,
            sidebar,
        })
        .expect("open unfocused background tab");
    assert_eq!(
        wait_for_named_work_pane_count(xdg.path(), &name, background_tab, 1).len(),
        1,
        "background tab should open one work pane",
    );

    let focused = wait_for_focused_client_pane(&backend, &name, &source_pane);
    assert_eq!(
        focused,
        vec![source_pane],
        "unfocused open_tab must return the attached client to the source pane: {focused:?}",
    );
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
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(120)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
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
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(120)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
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
                project_root: cwd.path().to_path_buf(),
                cwd: cwd.path().to_path_buf(),
                width: SidebarWidth::default(),
                birth_size: SidebarWidth::default().birth_size(Some(120)),
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_tabs: Vec::new(),
                refresh_ms: None,
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

/// `split_pane` injects `RIMZ_*` env on Zellij too — parity with tmux's native
/// `-e`. Zellij's `new-pane` has no env flag, so the backend prefixes the
/// command with an `env KEY=VALUE` shim. The split command records the var to a
/// file we read back, which a background session writes reliably without
/// depending on a rendered, attached pane.
#[test]
fn split_pane_injects_env_vars() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("splitenv");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let marker_file = cwd.path().join("rimz-env-marker");

    // Birth a live background session with one long-lived pane to split from.
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
        .expect("create session");
    assert!(created.success(), "create-background failed for {name}");
    assert!(
        !wait_for_pane_count(xdg.path(), &name, 1).is_empty(),
        "session should have its working pane before the split",
    );

    let mut env = BTreeMap::new();
    env.insert("RIMZ_TEST_VAR".to_owned(), "marker-rimz-env".to_owned());
    ZellijBackend::with_runtime_dir(xdg.path())
        .split_pane(SplitPaneOptions {
            target_pane_id: None,
            cwd: None,
            command: Some(vec![
                "sh".to_owned(),
                "-c".to_owned(),
                format!(
                    "printf '%s' \"$RIMZ_TEST_VAR\" > {}; sleep 5",
                    marker_file.display()
                ),
            ]),
            env,
            focus: false,
        })
        .expect("split_pane");

    let deadline = Instant::now() + Duration::from_secs(10);
    let marker = loop {
        if let Ok(text) = std::fs::read_to_string(&marker_file)
            && !text.is_empty()
        {
            break text;
        }
        assert!(
            Instant::now() < deadline,
            "env-injected split never wrote the marker file",
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        marker, "marker-rimz-env",
        "Zellij split pane missed the injected RIMZ_TEST_VAR",
    );
}

/// Count this user's live sidebar-serve processes scoped to `session` — the
/// leak check for a deferred or failed in-place add. Scans `/proc` directly so
/// the test needs no extra tooling; empty on platforms without it.
fn serve_processes_for(session: &str) -> usize {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            std::fs::read(format!("/proc/{pid}/cmdline")).ok()
        })
        .filter(|cmdline| {
            let cmdline = String::from_utf8_lossy(cmdline).replace('\0', " ");
            cmdline.contains(session) && cmdline.contains("sidebar") && cmdline.contains("serve")
        })
        .count()
}

/// An in-place add on a *detached* session is deferred, never attempted:
/// Zellij's screen thread drops a `new-pane` mount when no client is attached
/// while the spawned process keeps running, so the only safe move is to wait
/// for an attached client. Regression test for the reload loop that leaked
/// (and then reaped) one serve pair per run against a detached session.
#[test]
fn reconcile_defers_the_add_on_a_detached_session() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("defer");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    // A detached background session with a working pane and no sidebar.
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
    let before = wait_for_pane_count(xdg.path(), &name, 1);
    assert!(
        !before.is_empty(),
        "plain session should have a pane before reconcile: {before:?}",
    );

    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = SidebarPaneOptions {
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-defer")),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(120)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    // A freshly born --create-background session whose only pane is still
    // materializing is the case most prone to reconcile's transient-empty read,
    // so retry until reconcile actually observes the working pane.
    let report = reconcile_until_observed(xdg.path(), &opts, &SidebarLiveness::default());

    assert_eq!(report.deferred, 1, "the detached session's add is deferred");
    assert_eq!(report.recovered, 0, "nothing is added without a client");
    assert_eq!(report.failed, 0, "a deferral is not a failure");
    // Poll rather than read once: a recovered add would already have tripped the
    // `recovered == 0` assertion above, so here a single empty `list-panes` answer
    // under load would only flake a settled result. `before` polls for the same reason.
    let after = wait_for_pane_count(xdg.path(), &name, 1);
    assert_eq!(after.len(), 1, "no pane was added detached: {after:?}");
    assert_eq!(
        serve_processes_for(&name),
        0,
        "no serve pair leaked for the deferred add",
    );
}

/// Poll until an attached client registers on `session` — `list-clients`
/// reports a row past the header. A pane action that lands while the client is
/// still mid-startup gets its mount dropped exactly like on a detached
/// session, so tests that mount panes against a PTY client gate on this first
/// (the same attachment signal reconcile's defer gate reads).
fn wait_for_attached_client(xdg: &Path, session: &str) {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        let attached = scoped_zellij(xdg)
            .args(["--session", session, "action", "list-clients"])
            .bounded_output()
            .is_ok_and(|out| {
                out.status.success() && String::from_utf8_lossy(&out.stdout).lines().count() > 1
            });
        if attached {
            return;
        }
        if Instant::now() > deadline {
            panic!("no client attached to {session}");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// The raw `list-panes` JSON object for the session's `rimz-sidebar` pane.
fn raw_sidebar_pane(xdg: &Path, session: &str) -> serde_json::Value {
    let output = scoped_zellij(xdg)
        .args(["--session", session, "action", "list-panes", "-j", "-a"])
        .bounded_output()
        .expect("list-panes for sidebar lookup");
    assert!(output.status.success(), "list-panes for sidebar lookup");
    let panes: serde_json::Value = serde_json::from_slice(&output.stdout).expect("list-panes json");
    panes
        .as_array()
        .expect("pane array")
        .iter()
        .find(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
        })
        .expect("rimz-sidebar pane")
        .clone()
}

/// A claimed live sidebar sitting off the layout's dock — the residue of the
/// pre-discovery mis-mount (right side, ~50%) — is converged in place by
/// reconcile: moved to the left column and resized toward the layout width,
/// with the renderer's pane (and so the renderer) untouched.
#[test]
fn reconcile_redocks_an_off_spec_claimed_sidebar() {
    require_zellij!();

    let xdg_dir = scoped_runtime_dir();
    let name = unique_session_name("redock");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg_dir.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    // A background session with one long-lived working pane.
    let layout = cwd.path().join("plain.kdl");
    std::fs::write(
        &layout,
        "layout {\n    pane command=\"sleep\" {\n        args \"600\"\n    }\n}\n",
    )
    .expect("write plain layout");
    let created = scoped_zellij(xdg_dir.path())
        .args(["attach", "--create-background", &name, "options"])
        .arg("--default-cwd")
        .arg(cwd.path())
        .arg("--default-layout")
        .arg(&layout)
        .bounded_status()
        .expect("create plain session");
    assert!(created.success(), "create-background failed for {name}");
    wait_for_pane_count(xdg_dir.path(), &name, 1);

    // A wide client: the 50% mis-mount must exceed the `max_cols` cap (72) to
    // trip the tolerant width trigger — at 240 columns it lands at ~120.
    let _client = AttachedClient::attach(xdg_dir.path(), &name, 240, 60);
    let xdg = xdg_dir.path().to_path_buf();
    wait_for_attached_client(&xdg, &name);
    let (_stub_dir, stub) = sidebar_command_stub();

    // Recreate the mis-mounted shape against the attached client: a right-side
    // 50% pane titled like the sidebar, exactly where a raced add left it.
    let spawned = scoped_zellij(&xdg)
        .args([
            "--session",
            &name,
            "action",
            "new-pane",
            "--direction",
            "right",
            "--name",
            "rimz-sidebar",
            "--",
        ])
        .arg(&stub)
        .bounded_output()
        .expect("new-pane");
    assert!(
        spawned.status.success(),
        "new-pane failed: {}",
        String::from_utf8_lossy(&spawned.stderr),
    );
    let panes = wait_for_pane_count(&xdg, &name, 2);
    assert!(panes.len() >= 2, "sidebar pane should mount: {panes:?}");
    let before = raw_sidebar_pane(&xdg, &name);
    let sidebar_id = before.get("id").and_then(|value| value.as_u64()).unwrap();
    assert!(
        before.get("pane_x").and_then(|value| value.as_u64()) > Some(0),
        "the recreated mis-mount starts off the left column: {before}",
    );

    let mut liveness = rimz::mux::SidebarLiveness::default();
    liveness.claimed_panes.insert(PaneId::from_parts(
        MuxName::Zellij,
        format!("terminal_{sidebar_id}"),
    ));
    let report = ZellijBackend::with_runtime_dir(&xdg)
        .reconcile_sidebars(
            &SidebarPaneOptions {
                session_name: name.clone(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-redock")),
                project_root: std::env::temp_dir(),
                cwd: std::env::temp_dir(),
                width: SidebarWidth::default(),
                birth_size: SidebarWidth::default().birth_size(Some(240)),
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_tabs: Vec::new(),
                refresh_ms: None,
            },
            &liveness,
        )
        .expect("reconcile_sidebars");

    assert_eq!(report.redocked, 1, "the off-spec claimed sidebar converges");
    assert_eq!(report.closed, 0, "the renderer's pane is never closed");
    assert_eq!(report.recovered, 0, "nothing needed adding");
    assert_eq!(report.failed, 0);
    assert_sidebar_is_left_thirty_percent(&xdg, &name);
    let after = raw_sidebar_pane(&xdg, &name);
    assert_eq!(
        after.get("id").and_then(|value| value.as_u64()),
        Some(sidebar_id),
        "the same pane survived the move — the renderer was never replaced",
    );
}

/// A claimed sidebar can sit at `x=0` while still not being a full-height left
/// column: a work pane spans the whole tab below it. Reconcile detects that
/// nested row, preserves the running renderer, and moves the work panes into a
/// right-side stack so the sidebar owns the full left column.
#[test]
fn reconcile_repairs_a_nested_sidebar_into_a_full_height_left_column() {
    require_zellij!();

    let xdg_dir = scoped_runtime_dir();
    let name = unique_session_name("nested");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg_dir.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let layout = cwd.path().join("plain.kdl");
    std::fs::write(
        &layout,
        "layout {\n    pane command=\"sleep\" {\n        args \"600\"\n    }\n}\n",
    )
    .expect("write plain layout");
    let created = scoped_zellij(xdg_dir.path())
        .args(["attach", "--create-background", &name, "options"])
        .arg("--default-cwd")
        .arg(cwd.path())
        .arg("--default-layout")
        .arg(&layout)
        .bounded_status()
        .expect("create plain session");
    assert!(created.success(), "create-background failed for {name}");
    let initial = wait_for_pane_count(xdg_dir.path(), &name, 1);
    let original_id = initial
        .first()
        .and_then(|pane| pane.pane_id.raw().strip_prefix("terminal_"))
        .and_then(|raw| raw.parse::<u64>().ok())
        .expect("initial terminal id");

    let _client = AttachedClient::attach(xdg_dir.path(), &name, 160, 60);
    let xdg = xdg_dir.path().to_path_buf();
    wait_for_attached_client(&xdg, &name);
    let (_stub_dir, stub) = sidebar_command_stub();

    let down = scoped_zellij(&xdg)
        .args([
            "--session",
            &name,
            "action",
            "new-pane",
            "--direction",
            "down",
            "--",
            "sleep",
            "600",
        ])
        .bounded_output()
        .expect("new-pane down");
    assert!(
        down.status.success(),
        "new-pane down failed: {}",
        String::from_utf8_lossy(&down.stderr),
    );
    let focused = scoped_zellij(&xdg)
        .args([
            "--session",
            &name,
            "action",
            "focus-pane-id",
            &format!("terminal_{original_id}"),
        ])
        .bounded_status()
        .expect("focus original pane");
    assert!(focused.success(), "focus original pane failed");
    let spawned = scoped_zellij(&xdg)
        .args([
            "--session",
            &name,
            "action",
            "new-pane",
            "--direction",
            "right",
            "--name",
            "rimz-sidebar",
            "--",
        ])
        .arg(&stub)
        .bounded_output()
        .expect("new-pane sidebar");
    assert!(
        spawned.status.success(),
        "new-pane sidebar failed: {}",
        String::from_utf8_lossy(&spawned.stderr),
    );
    let before = raw_sidebar_pane(&xdg, &name);
    let sidebar_id = before.get("id").and_then(|value| value.as_u64()).unwrap();
    let moved = scoped_zellij(&xdg)
        .args([
            "--session",
            &name,
            "action",
            "move-pane",
            "left",
            "--pane-id",
            &format!("terminal_{sidebar_id}"),
        ])
        .bounded_status()
        .expect("move sidebar left");
    assert!(moved.success(), "move sidebar left failed");
    let before = raw_sidebar_pane(&xdg, &name);
    assert_eq!(
        before.get("pane_x").and_then(|value| value.as_u64()),
        Some(0),
        "the nested sidebar starts in the left row band: {before}",
    );
    let tab_id = before
        .get("tab_id")
        .and_then(|value| value.as_u64())
        .expect("sidebar tab id");
    let refocused = scoped_zellij(&xdg)
        .args([
            "--session",
            &name,
            "action",
            "focus-pane-id",
            &format!("terminal_{original_id}"),
        ])
        .bounded_status()
        .expect("refocus original pane before reconcile");
    assert!(
        refocused.success(),
        "refocus original pane before reconcile failed"
    );
    assert_eq!(
        wait_for_focused_nonplugin_id_in_tab(&xdg, &name, tab_id, original_id),
        Some(original_id),
        "fixture must focus the original work pane before reconcile",
    );

    let mut liveness = rimz::mux::SidebarLiveness::default();
    liveness.claimed_panes.insert(PaneId::from_parts(
        MuxName::Zellij,
        format!("terminal_{sidebar_id}"),
    ));
    let report = ZellijBackend::with_runtime_dir(&xdg)
        .reconcile_sidebars(
            &SidebarPaneOptions {
                session_name: name.clone(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-nested")),
                project_root: cwd.path().to_path_buf(),
                cwd: cwd.path().to_path_buf(),
                width: SidebarWidth::default(),
                birth_size: SidebarWidth::default().birth_size(Some(160)),
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_tabs: Vec::new(),
                refresh_ms: None,
            },
            &liveness,
        )
        .expect("reconcile_sidebars");

    assert_eq!(report.redocked, 1, "the nested sidebar converges");
    assert_eq!(report.closed, 0, "geometry repair is not duplicate cleanup");
    assert_eq!(report.failed, 0);
    assert_eq!(report.misdocked, 0);
    assert_sidebar_is_left_thirty_percent(&xdg, &name);
    let after = raw_sidebar_pane(&xdg, &name);
    assert_eq!(
        after.get("id").and_then(|value| value.as_u64()),
        Some(sidebar_id),
        "the renderer pane survives the nested-row repair",
    );
    assert_eq!(
        focused_nonplugin_id_in_tab(&xdg, &name, tab_id),
        Some(original_id),
        "in-place nested repair restores the tab focus that existed before reconcile",
    );
}

/// A nested sidebar beside a user-made multi-column work layout is detected but
/// not rewritten: stacking every work pane would preserve processes while
/// collapsing the user's right-side columns.
#[test]
fn reconcile_reports_nested_multicolumn_sidebar_without_stacking_work_area() {
    require_zellij!();

    let xdg_dir = scoped_runtime_dir();
    let name = unique_session_name("nestedwide");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg_dir.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_command_stub();
    let stub_kdl = serde_json::to_string(&stub.to_string_lossy()).expect("stub kdl string");
    let cwd_kdl = serde_json::to_string(&cwd.path().to_string_lossy()).expect("cwd kdl string");
    let layout = cwd.path().join("nested-wide.kdl");
    std::fs::write(
        &layout,
        format!(
            r#"layout {{
    pane split_direction="horizontal" {{
        pane split_direction="vertical" {{
            pane name="rimz-sidebar" cwd={cwd_kdl} {{
                command {stub_kdl}
                start_suspended false
                close_on_exit true
            }}
            pane cwd={cwd_kdl} {{
                command "sleep"
                args "600"
                start_suspended false
                close_on_exit true
            }}
            pane cwd={cwd_kdl} {{
                command "sleep"
                args "600"
                start_suspended false
                close_on_exit true
            }}
        }}
        pane cwd={cwd_kdl} {{
            command "sleep"
            args "600"
            start_suspended false
            close_on_exit true
        }}
    }}
}}
"#,
        ),
    )
    .expect("write nested-wide layout");
    let created = scoped_zellij(xdg_dir.path())
        .args(["attach", "--create-background", &name, "options"])
        .arg("--default-cwd")
        .arg(cwd.path())
        .arg("--default-layout")
        .arg(&layout)
        .bounded_status()
        .expect("create nested-wide session");
    assert!(created.success(), "create-background failed for {name}");
    let initial = wait_for_pane_count(xdg_dir.path(), &name, 4);
    assert_eq!(
        initial.len(),
        4,
        "layout should birth four panes: {initial:?}"
    );

    let _client = AttachedClient::attach(xdg_dir.path(), &name, 240, 60);
    let xdg = xdg_dir.path().to_path_buf();
    wait_for_attached_client(&xdg, &name);

    let before_sidebar = raw_sidebar_pane(&xdg, &name);
    let sidebar_id = before_sidebar
        .get("id")
        .and_then(|value| value.as_u64())
        .unwrap();
    let before_sidebar_cols = before_sidebar
        .get("pane_columns")
        .and_then(|value| value.as_u64())
        .expect("sidebar columns before");
    let before_work = work_pane_geometry(&xdg, &name);
    let before_ids: BTreeSet<u64> = before_work.iter().map(|pane| pane.id).collect();
    let before_right_xs: BTreeSet<u64> = before_work
        .iter()
        .filter(|pane| pane.x >= before_sidebar_cols)
        .map(|pane| pane.x)
        .collect();
    assert!(
        before_work.iter().any(|pane| pane.x == 0) && before_right_xs.len() >= 2,
        "fixture should start as a nested sidebar with a multi-column work area: \
         sidebar={before_sidebar}, work={before_work:?}",
    );

    let mut liveness = rimz::mux::SidebarLiveness::default();
    liveness.claimed_panes.insert(PaneId::from_parts(
        MuxName::Zellij,
        format!("terminal_{sidebar_id}"),
    ));
    let report = ZellijBackend::with_runtime_dir(&xdg)
        .reconcile_sidebars(
            &SidebarPaneOptions {
                session_name: name.clone(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-nestedwide")),
                project_root: cwd.path().to_path_buf(),
                cwd: cwd.path().to_path_buf(),
                width: SidebarWidth::default(),
                birth_size: SidebarWidth::default().birth_size(Some(240)),
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_tabs: Vec::new(),
                refresh_ms: None,
            },
            &liveness,
        )
        .expect("reconcile_sidebars");

    assert_eq!(
        report.misdocked, 1,
        "the nested sidebar is reported for operator visibility",
    );
    assert_eq!(
        report.redocked, 0,
        "the arbitrary work layout is not repaired"
    );
    assert_eq!(report.closed, 0, "the claimed renderer pane is preserved");
    assert_eq!(report.failed, 0);

    let after_sidebar = raw_sidebar_pane(&xdg, &name);
    let after_sidebar_cols = after_sidebar
        .get("pane_columns")
        .and_then(|value| value.as_u64())
        .expect("sidebar columns after");
    let after_work = work_pane_geometry(&xdg, &name);
    let after_ids: BTreeSet<u64> = after_work.iter().map(|pane| pane.id).collect();
    let after_right_xs: BTreeSet<u64> = after_work
        .iter()
        .filter(|pane| pane.x >= after_sidebar_cols)
        .map(|pane| pane.x)
        .collect();
    assert_eq!(after_ids, before_ids, "work panes are not replaced");
    assert!(
        after_work.iter().any(|pane| pane.x == 0) && after_right_xs.len() >= 2,
        "reconcile must not collapse the user's multi-column work area: \
         sidebar={after_sidebar}, work={after_work:?}",
    );
    assert_eq!(
        after_sidebar.get("id").and_then(|value| value.as_u64()),
        Some(sidebar_id),
        "the renderer pane is not rebuilt",
    );
}

/// Adding a sidebar to a tab whose work panes are already row-stacked used to
/// birth the sidebar into only one row. The verified add path now repairs that
/// nested shape before reporting success.
#[test]
fn reconcile_add_ends_docked_in_a_row_stacked_tab() {
    require_zellij!();

    let xdg_dir = scoped_runtime_dir();
    let name = unique_session_name("rowadd");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg_dir.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let layout = cwd.path().join("plain.kdl");
    std::fs::write(
        &layout,
        "layout {\n    pane command=\"sleep\" {\n        args \"600\"\n    }\n}\n",
    )
    .expect("write plain layout");
    let created = scoped_zellij(xdg_dir.path())
        .args(["attach", "--create-background", &name, "options"])
        .arg("--default-cwd")
        .arg(cwd.path())
        .arg("--default-layout")
        .arg(&layout)
        .bounded_status()
        .expect("create plain session");
    assert!(created.success(), "create-background failed for {name}");
    wait_for_pane_count(xdg_dir.path(), &name, 1);

    let _client = AttachedClient::attach(xdg_dir.path(), &name, 160, 60);
    let xdg = xdg_dir.path().to_path_buf();
    wait_for_attached_client(&xdg, &name);
    let down = scoped_zellij(&xdg)
        .args([
            "--session",
            &name,
            "action",
            "new-pane",
            "--direction",
            "down",
            "--",
            "sleep",
            "600",
        ])
        .bounded_output()
        .expect("new-pane down");
    assert!(
        down.status.success(),
        "new-pane down failed: {}",
        String::from_utf8_lossy(&down.stderr),
    );
    wait_for_pane_count(&xdg, &name, 2);
    let (_stub_dir, stub) = sidebar_command_stub();

    let report = ZellijBackend::with_runtime_dir(&xdg)
        .reconcile_sidebars(
            &SidebarPaneOptions {
                session_name: name.clone(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-rowadd")),
                project_root: cwd.path().to_path_buf(),
                cwd: cwd.path().to_path_buf(),
                width: SidebarWidth::default(),
                birth_size: SidebarWidth::default().birth_size(Some(160)),
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_tabs: Vec::new(),
                refresh_ms: None,
            },
            &rimz::mux::SidebarLiveness::default(),
        )
        .expect("reconcile_sidebars");

    assert_eq!(report.recovered, 1, "the missing sidebar is added");
    assert_eq!(report.failed, 0);
    assert_eq!(report.misdocked, 0);
    assert_sidebar_is_left_thirty_percent(&xdg, &name);
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

/// Parsed `list-panes -j -a` for `session`. Callers that poll keep the last
/// error so deadline failures report the command failure instead of "no panes".
fn list_panes_json(xdg: &Path, session: &str) -> std::result::Result<serde_json::Value, String> {
    let output = scoped_zellij(xdg)
        .args(["--session", session, "action", "list-panes", "-j", "-a"])
        .bounded_output()
        .map_err(|err| format!("list-panes failed for {session}: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "list-panes failed for {session} with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|err| {
        format!(
            "parsing list-panes JSON for {session}: {err}; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    })
}

fn expect_list_panes_json(xdg: &Path, session: &str) -> serde_json::Value {
    list_panes_json(xdg, session).unwrap_or_else(|err| panic!("{err}"))
}

#[derive(Debug)]
struct PaneGeometry {
    id: u64,
    x: u64,
    y: u64,
    columns: u64,
    rows: u64,
}

fn named_work_pane_geometry(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> std::result::Result<Vec<PaneGeometry>, String> {
    let panes = list_panes_json(xdg, session)?;
    let mut work: Vec<PaneGeometry> = panes
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|pane| pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false))
        .filter(|pane| {
            pane.get("tab_name").and_then(|value| value.as_str()) == Some(tab_name)
                && pane.get("title").and_then(|value| value.as_str()) != Some("rimz-sidebar")
        })
        .filter_map(|pane| {
            Some(PaneGeometry {
                id: pane.get("id")?.as_u64()?,
                x: pane.get("pane_x")?.as_u64()?,
                y: pane.get("pane_y")?.as_u64()?,
                columns: pane.get("pane_columns")?.as_u64()?,
                rows: pane.get("pane_rows")?.as_u64()?,
            })
        })
        .collect();
    work.sort_by_key(|pane| pane.x);
    Ok(work)
}

fn named_sidebar_pane_geometry(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> std::result::Result<Option<PaneGeometry>, String> {
    let panes = list_panes_json(xdg, session)?;
    Ok(panes
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|pane| pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false))
        .find(|pane| {
            pane.get("tab_name").and_then(|value| value.as_str()) == Some(tab_name)
                && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
        })
        .and_then(|pane| {
            Some(PaneGeometry {
                id: pane.get("id")?.as_u64()?,
                x: pane.get("pane_x")?.as_u64()?,
                y: pane.get("pane_y")?.as_u64()?,
                columns: pane.get("pane_columns")?.as_u64()?,
                rows: pane.get("pane_rows")?.as_u64()?,
            })
        }))
}

fn named_compact_bar_pane_geometry(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> std::result::Result<Option<PaneGeometry>, String> {
    let panes = list_panes_json(xdg, session)?;
    Ok(panes
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .find(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(true)
                && pane.get("tab_name").and_then(|value| value.as_str()) == Some(tab_name)
                && pane
                    .get("title")
                    .and_then(|value| value.as_str())
                    .is_some_and(|title| title.contains("compact-bar"))
        })
        .and_then(|pane| {
            Some(PaneGeometry {
                id: pane.get("id")?.as_u64()?,
                x: pane.get("pane_x")?.as_u64()?,
                y: pane.get("pane_y")?.as_u64()?,
                columns: pane.get("pane_columns")?.as_u64()?,
                rows: pane.get("pane_rows")?.as_u64()?,
            })
        }))
}

fn wait_for_named_sidebar_pane(xdg: &Path, session: &str, tab_name: &str) -> Option<PaneGeometry> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match named_sidebar_pane_geometry(xdg, session, tab_name) {
            Ok(sidebar) if sidebar.is_some() => return sidebar,
            Ok(sidebar) if Instant::now() >= deadline => return sidebar,
            Ok(_) => {}
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for sidebar pane in {session}/{tab_name}; \
                         last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_named_compact_bar_pane(
    xdg: &Path,
    session: &str,
    tab_name: &str,
) -> Option<PaneGeometry> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match named_compact_bar_pane_geometry(xdg, session, tab_name) {
            Ok(bar) if bar.is_some() => return bar,
            Ok(bar) if Instant::now() >= deadline => return bar,
            Ok(_) => {}
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for compact bar pane in {session}/{tab_name}; \
                         last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_named_work_pane_state<F>(
    xdg: &Path,
    session: &str,
    tab_name: &str,
    want: usize,
    mut ready: F,
) -> Vec<PaneGeometry>
where
    F: FnMut(&[PaneGeometry]) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_work = Vec::new();
    loop {
        match named_work_pane_geometry(xdg, session, tab_name) {
            Ok(work) => {
                if (work.len() == want && ready(&work)) || Instant::now() >= deadline {
                    return work;
                }
                last_work = work;
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for {want} work panes in {session}/{tab_name}; \
                         last panes: {last_work:?}; last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn work_pane_geometry(xdg: &Path, session: &str) -> Vec<PaneGeometry> {
    let panes = expect_list_panes_json(xdg, session);
    let mut work: Vec<PaneGeometry> = panes
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|pane| pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false))
        .filter(|pane| pane.get("title").and_then(|value| value.as_str()) != Some("rimz-sidebar"))
        .filter(|pane| pane.get("is_held").and_then(|value| value.as_bool()) != Some(true))
        .filter(|pane| pane.get("exited").and_then(|value| value.as_bool()) != Some(true))
        .filter_map(|pane| {
            Some(PaneGeometry {
                id: pane.get("id")?.as_u64()?,
                x: pane.get("pane_x")?.as_u64()?,
                y: pane.get("pane_y")?.as_u64()?,
                columns: pane.get("pane_columns")?.as_u64()?,
                rows: pane.get("pane_rows")?.as_u64()?,
            })
        })
        .collect();
    work.sort_by_key(|pane| pane.id);
    work
}

fn wait_for_named_work_pane_count(
    xdg: &Path,
    session: &str,
    tab_name: &str,
    want: usize,
) -> Vec<PaneGeometry> {
    wait_for_named_work_pane_state(xdg, session, tab_name, want, |_| true)
}

fn spawn_sleep_pane(xdg: &Path, session: &str, cwd: &Path) {
    let spawned = scoped_zellij(xdg)
        .args(["--session", session, "action", "new-pane", "--cwd"])
        .arg(cwd)
        .args(["--", "sleep", "600"])
        .bounded_output()
        .expect("new-pane");
    assert!(
        spawned.status.success(),
        "new-pane failed: {}",
        String::from_utf8_lossy(&spawned.stderr),
    );
}

fn assert_work_panes_reopen_evenly_after_closing_first(
    xdg: &Path,
    session: &str,
    tab_name: &str,
    cwd: &Path,
    client_columns: u16,
    client_rows: u16,
) {
    let work = wait_for_named_work_pane_count(xdg, session, tab_name, 2);
    assert_eq!(
        work.len(),
        2,
        "tab should start with two work panes: {work:?}",
    );
    let close = format!("terminal_{}", work[0].id);
    let closed = scoped_zellij(xdg)
        .args([
            "--session",
            session,
            "action",
            "close-pane",
            "--pane-id",
            &close,
        ])
        .bounded_output()
        .expect("close-pane");
    assert!(
        closed.status.success(),
        "close-pane failed: {}",
        String::from_utf8_lossy(&closed.stderr),
    );

    let sidebar_after_close =
        wait_for_named_sidebar_pane(xdg, session, tab_name).expect("work tab keeps its sidebar");
    assert_eq!(
        sidebar_after_close.x, 0,
        "sidebar should stay docked left after close: {sidebar_after_close:?}",
    );
    let expected_work_columns =
        u64::from(client_columns).saturating_sub(sidebar_after_close.columns);
    let survivor = wait_for_named_work_pane_state(xdg, session, tab_name, 1, |work| {
        work[0].columns.abs_diff(expected_work_columns) <= 5
    });
    assert_eq!(
        survivor.len(),
        1,
        "closing one work pane should leave one survivor: {survivor:?}",
    );
    let survivor_diff = survivor[0].columns.abs_diff(expected_work_columns);
    assert!(
        survivor_diff <= 5,
        "surviving work pane should fill the work area after close; expected \
         about {expected_work_columns} cols, got {survivor:?}",
    );
    let focus = scoped_zellij(xdg)
        .args([
            "--session",
            session,
            "action",
            "focus-pane-id",
            &format!("terminal_{}", survivor[0].id),
        ])
        .bounded_output()
        .expect("focus-pane-id");
    assert!(
        focus.status.success(),
        "focus-pane-id failed: {}",
        String::from_utf8_lossy(&focus.stderr),
    );

    spawn_sleep_pane(xdg, session, cwd);

    let split = wait_for_named_work_pane_state(xdg, session, tab_name, 2, |work| {
        work[0].columns.abs_diff(work[1].columns) <= 5
    });
    assert_eq!(
        split.len(),
        2,
        "new terminal should land in the same work tab: {split:?}",
    );
    let diff = split[0].columns.abs_diff(split[1].columns);
    assert!(
        diff <= 5,
        "work panes should split evenly after reopening from one pane, got {split:?}",
    );
    let sidebar =
        wait_for_named_sidebar_pane(xdg, session, tab_name).expect("work tab keeps its sidebar");
    assert_eq!(sidebar.x, 0, "sidebar should stay docked left: {sidebar:?}");
    assert!(
        (68..=76).contains(&sidebar.columns),
        "sidebar should stay near the 72-column cap: {sidebar:?}",
    );
    let bar = wait_for_named_compact_bar_pane(xdg, session, tab_name)
        .expect("work tab keeps its compact-bar");
    assert_eq!(
        bar.x, 0,
        "compact bar should span from the left edge: {bar:?}"
    );
    assert_eq!(
        bar.columns,
        u64::from(client_columns),
        "compact bar should span the whole tab width: {bar:?}",
    );
    assert_eq!(bar.rows, 1, "compact bar should stay one row tall: {bar:?}");
    assert_eq!(
        bar.y + bar.rows,
        u64::from(client_rows),
        "compact bar should stay docked at the bottom: {bar:?}",
    );
}

fn assert_sidebars_not_held(xdg: &Path, session: &str, context: &str) {
    let panes = expect_list_panes_json(xdg, session);
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
    tab_ids_from_panes(&expect_list_panes_json(xdg, session))
}

fn tab_ids_from_panes(panes: &serde_json::Value) -> Vec<u64> {
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
    let panes = expect_list_panes_json(xdg, session);
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
    let panes = expect_list_panes_json(xdg, session);
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

/// Raw id of the focused non-plugin pane in `tab`, if any.
fn focused_nonplugin_id_in_tab(xdg: &Path, session: &str, tab: u64) -> Option<u64> {
    focused_nonplugin_id_in_tab_result(xdg, session, tab).unwrap_or_else(|err| panic!("{err}"))
}

fn focused_nonplugin_id_in_tab_result(
    xdg: &Path,
    session: &str,
    tab: u64,
) -> std::result::Result<Option<u64>, String> {
    let panes = list_panes_json(xdg, session)?;
    Ok(panes.as_array().and_then(|panes| {
        panes.iter().find_map(|p| {
            if p.get("is_plugin").and_then(|v| v.as_bool()) == Some(false)
                && p.get("tab_id").and_then(|v| v.as_u64()) == Some(tab)
                && p.get("is_focused").and_then(|v| v.as_bool()) == Some(true)
            {
                p.get("id").and_then(|v| v.as_u64())
            } else {
                None
            }
        })
    }))
}

fn wait_for_focused_nonplugin_id_in_tab(
    xdg: &Path,
    session: &str,
    tab: u64,
    want: u64,
) -> Option<u64> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match focused_nonplugin_id_in_tab_result(xdg, session, tab) {
            Ok(focused) => {
                if focused == Some(want) || Instant::now() >= deadline {
                    return focused;
                }
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for focused pane {want} in {session}/tab {tab}; \
                         last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_focused_client_pane(
    backend: &ZellijBackend,
    session: &str,
    want: &PaneId,
) -> Vec<PaneId> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let focused = backend
            .focused_client_panes(ClientFocusOptions {
                session_name: Some(session.to_owned()),
                ..Default::default()
            })
            .expect("focused_client_panes");
        if focused.iter().any(|pane| pane == want) || Instant::now() >= deadline {
            return focused;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Poll until at least `want` distinct tabs hold a non-plugin pane, or time out.
fn wait_for_tab_count(xdg: &Path, session: &str, want: usize) -> Vec<u64> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_ids = Vec::new();
    loop {
        match list_panes_json(xdg, session) {
            Ok(panes) => {
                let ids = tab_ids_from_panes(&panes);
                if ids.len() >= want || Instant::now() >= deadline {
                    return ids;
                }
                last_ids = ids;
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for {want} tabs in {session}; last ids: {last_ids:?}; \
                         last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Name of the first tab that appears after `before`, from the live pane list.
fn wait_for_new_tab_name(xdg: &Path, session: &str, before: &[u64]) -> String {
    let before: BTreeSet<u64> = before.iter().copied().collect();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_new_tabs = BTreeSet::new();
    let mut last_unnamed_nonplugin_tabs = BTreeSet::new();
    loop {
        match list_panes_json(xdg, session) {
            Ok(panes) => {
                last_new_tabs.clear();
                last_unnamed_nonplugin_tabs.clear();
                if let Some(panes) = panes.as_array() {
                    for pane in panes {
                        let Some(tab_id) = pane.get("tab_id").and_then(|value| value.as_u64())
                        else {
                            continue;
                        };
                        if before.contains(&tab_id) {
                            continue;
                        }
                        last_new_tabs.insert(tab_id);
                        if pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false) {
                            if let Some(name) =
                                pane.get("tab_name").and_then(|value| value.as_str())
                            {
                                return name.to_owned();
                            }
                            last_unnamed_nonplugin_tabs.insert(tab_id);
                        }
                    }
                }
                if Instant::now() >= deadline {
                    if !last_unnamed_nonplugin_tabs.is_empty() {
                        panic!(
                            "new tab(s) {last_unnamed_nonplugin_tabs:?} carried unnamed \
                             non-plugin panes after 10s"
                        );
                    }
                    if !last_new_tabs.is_empty() {
                        panic!("new tab(s) {last_new_tabs:?} carried only plugin panes after 10s");
                    }
                    panic!("no new tab appeared after 10s; before tabs were {before:?}");
                }
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for new tab in {session}; last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Poll `list_panes` until at least `want` panes appear (bounded). Returns the
/// last observation either way so the caller can assert and print it.
fn wait_for_pane_count(xdg: &Path, session: &str, want: usize) -> Vec<PaneRef> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_panes = Vec::new();
    loop {
        match ZellijBackend::with_runtime_dir(xdg).list_panes(PaneListOptions {
            session_name: Some(session.to_owned()),
            ..Default::default()
        }) {
            Ok(listing) => {
                let panes = listing.panes;
                if panes.len() >= want || Instant::now() >= deadline {
                    return panes;
                }
                last_panes = panes;
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    panic!(
                        "timed out waiting for {want} panes in {session}; last panes: \
                         {last_panes:?}; last list-panes error: {err}",
                    );
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Run `reconcile_sidebars` until it observes the session's live panes.
///
/// Reconcile reads the pane list once and early-returns a no-op
/// `SidebarRecovery::default()` when that read comes back empty. Under heavy CI
/// load Zellij's screen thread can briefly answer `[]` past the backend's
/// bounded empty-retry, so a freshly born session's first reconcile occasionally
/// sees nothing and does nothing. Every reconcile test sets up a view that needs
/// work, so an all-zeros report is that transient-empty race rather than the real
/// outcome — retry it. A no-op pass touches no panes, so re-running is safe; a
/// genuine regression keeps returning the default until the deadline, letting the
/// caller's assertion fire on the real (still wrong) report.
fn reconcile_until_observed(
    xdg: &Path,
    opts: &SidebarPaneOptions,
    live: &SidebarLiveness,
) -> SidebarRecovery {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let report = ZellijBackend::with_runtime_dir(xdg)
            .reconcile_sidebars(opts, live)
            .expect("reconcile_sidebars");
        if report != SidebarRecovery::default() || Instant::now() >= deadline {
            return report;
        }
        std::thread::sleep(Duration::from_millis(100));
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
    let sidebar_id = sidebar
        .get("id")
        .and_then(|value| value.as_u64())
        .expect("sidebar id");
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
    for pane in panes.iter().filter(|pane| {
        pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
            && pane.get("tab_id").and_then(|value| value.as_u64()) == Some(tab_id)
            && pane.get("id").and_then(|value| value.as_u64()) != Some(sidebar_id)
    }) {
        let x = pane
            .get("pane_x")
            .and_then(|value| value.as_u64())
            .expect("work pane x");
        assert!(
            x >= columns,
            "work pane intrudes into the sidebar column band: sidebar={sidebar}, pane={pane}",
        );
    }
    assert!(
        columns * 100 <= total_columns * 35,
        "sidebar should occupy roughly 30% of the tab: {columns}/{total_columns}",
    );
}

/// The `rimz-sidebar` pane's column width per tab, from the live pane listing.
/// Tabs without a sidebar are absent; an unanswerable listing is empty.
fn sidebar_columns_by_tab(xdg: &Path, session: &str) -> BTreeMap<u64, u64> {
    let Ok(output) = scoped_zellij(xdg)
        .args(["--session", session, "action", "list-panes", "-j", "-a"])
        .bounded_output()
    else {
        return BTreeMap::new();
    };
    let Ok(panes) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return BTreeMap::new();
    };
    panes
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
        })
        .filter_map(|pane| {
            Some((
                pane.get("tab_id")?.as_u64()?,
                pane.get("pane_columns")?.as_u64()?,
            ))
        })
        .collect()
}

/// Poll until `session` reports one sidebar per entry of `expected`, each
/// inside its tab's column range (ordered by tab id) — attach and tab-open
/// geometry settles asynchronously. `false` on timeout.
fn wait_for_sidebar_columns(
    xdg: &Path,
    session: &str,
    expected: &[std::ops::RangeInclusive<u64>],
) -> bool {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        let widths = sidebar_columns_by_tab(xdg, session);
        if widths.len() == expected.len()
            && widths
                .values()
                .zip(expected)
                .all(|(width, range)| range.contains(width))
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// A capped birth size is enforced by Zellij itself, in the spelling each tab
/// instantiates with: the birth tab carries the derived percentage (a fixed
/// size wider than the detached session's default geometry kills the session)
/// and lands within rounding of the cap once a real-size client attaches,
/// while every tab opened later is born from the `new_tab_template` at exactly
/// the cap — no post-birth resize anywhere.
#[test]
fn capped_birth_size_lands_the_cap_in_every_tab() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("fixedw");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let width = SidebarWidth::default();
    ZellijBackend::with_runtime_dir(xdg.path())
        .open_sidebar(
            &SidebarPaneOptions {
                session_name: name.clone(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-fixed-width")),
                project_root: cwd.path().to_path_buf(),
                cwd: cwd.path().to_path_buf(),
                width,
                // The launch path's decision on a 340-column terminal: 30%
                // would be 102, so the birth is capped — 72 columns, derived
                // as 21% for the tabs that instantiate detached.
                birth_size: width.birth_size(Some(340)),
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_tabs: Vec::new(),
                refresh_ms: None,
            },
            None,
        )
        .expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    // A real-size client: the detached birth geometry is tiny, so the cap
    // only shows once the session adopts the attaching terminal's 340 cols.
    // The birth tab is the derived 21% — within a column or two of the cap.
    let _client = AttachedClient::attach(xdg.path(), &name, 340, 80);
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[69..=72]),
        "the attached session must land the birth sidebar within rounding of \
         the 72-column cap, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );

    // A user-opened tab instantiates the `new_tab_template` at live geometry:
    // the fixed spelling lands exactly at the cap.
    open_new_tab(xdg.path(), &name);
    wait_for_tab_count(xdg.path(), &name, 2);
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[69..=72, 72..=72]),
        "a tab opened from an attached client must be born at exactly the \
         72-column cap, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );
}

/// A tab layout keeps the fixed sidebar outside the user's work area. Closing
/// back to `sidebar | one work pane` and then opening a no-direction terminal
/// must split the work area, not rebalance a flat root that still carries the
/// fixed sidebar constraint.
#[test]
fn tab_layout_reopens_work_panes_evenly_after_closing_to_one() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("worksplit");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let width = SidebarWidth::default();
    let sidebar = SidebarPaneOptions {
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-worksplit")),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        width,
        birth_size: width.birth_size(Some(298)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    let client_columns: u16 = 380;
    let client_rows: u16 = 46;
    let _client = AttachedClient::attach(xdg.path(), &name, client_columns, client_rows);
    wait_for_attached_client(xdg.path(), &name);

    let tab_name = "work split";
    backend
        .open_tab(&TabOptions {
            session_name: name.clone(),
            title: tab_name.to_owned(),
            cwd: cwd.path().to_path_buf(),
            panes: LayoutPanes {
                columns: vec![
                    vec![PaneCmd {
                        argv: vec!["sleep".to_owned(), "600".to_owned()],
                    }],
                    vec![PaneCmd {
                        argv: vec!["sleep".to_owned(), "600".to_owned()],
                    }],
                ],
            },
            focus: true,
            sidebar,
        })
        .expect("open tab layout");

    assert_work_panes_reopen_evenly_after_closing_first(
        xdg.path(),
        &name,
        tab_name,
        cwd.path(),
        client_columns,
        client_rows,
    );
}

/// The session birth layout carries the same work-area swap layout as explicit
/// tab layouts. A native Zellij `NewTab` followed by a close-first +
/// no-direction `NewPane` flow should rebalance the work area instead of
/// inheriting Zellij's stale 75/25 split.
#[test]
fn native_new_tab_reopens_work_panes_evenly_after_closing_to_one() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("nativesplit");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let width = SidebarWidth::default();
    let sidebar = SidebarPaneOptions {
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-native-worksplit")),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        width,
        birth_size: width.birth_size(Some(298)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    let client_columns: u16 = 380;
    let client_rows: u16 = 46;
    let _client = AttachedClient::attach(xdg.path(), &name, client_columns, client_rows);
    wait_for_attached_client(xdg.path(), &name);

    let before_tabs = tab_ids(xdg.path(), &name);
    open_new_tab(xdg.path(), &name);
    let tab_name = wait_for_new_tab_name(xdg.path(), &name, &before_tabs);
    wait_for_named_sidebar_pane(xdg.path(), &name, &tab_name)
        .expect("native tab should carry a sidebar");

    spawn_sleep_pane(xdg.path(), &name, cwd.path());
    let split = wait_for_named_work_pane_state(xdg.path(), &name, &tab_name, 2, |work| {
        work[0].columns.abs_diff(work[1].columns) <= 5
    });
    assert_eq!(
        split.len(),
        2,
        "native tab should split into two work panes: {split:?}",
    );
    let diff = split[0].columns.abs_diff(split[1].columns);
    assert!(
        diff <= 5,
        "native tab's first no-direction split should be even, got {split:?}",
    );

    assert_work_panes_reopen_evenly_after_closing_first(
        xdg.path(),
        &name,
        &tab_name,
        cwd.path(),
        client_columns,
        client_rows,
    );
}

/// `paste_text` writes one bracketed paste (`ESC[200~` … `ESC[201~`) wrapping
/// the payload as a raw decimal byte list — the steer/queue delivery path. A
/// bare shell renders the markers literally, so the inner text still lands in
/// the pane; assert it arrives byte-for-byte. A leading dash is the regression
/// guard: the byte-write path must never re-read the payload as a flag or key.
#[test]
fn paste_text_delivers_the_literal_payload() {
    require_zellij!();

    let session = ZellijSession::spawn(unique_session_name("paste"));
    let backend = ZellijBackend::with_runtime_dir(session.xdg.path());
    let panes = wait_for_pane_count(session.xdg.path(), &session.name, 1);
    let pane_id = panes[0].pane_id.clone();

    let payload = "-rf rimz-paste-marker";
    backend.paste_text(&pane_id, payload).expect("paste_text");

    let deadline = Instant::now() + Duration::from_secs(5);
    let captured = loop {
        let text = backend
            .capture_pane(&pane_id, None, false)
            .map(|capture| capture.raw_text)
            .unwrap_or_default();
        if text.contains(payload) || Instant::now() >= deadline {
            break text;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        captured.contains(payload),
        "the pasted payload should arrive contiguous and byte-safe, got: {captured:?}",
    );
}

/// `focused_client_panes` reads each client's focused pane from `list-clients`.
/// A background session with no client focuses nothing; an attached client
/// focuses its terminal pane. Drives the hook-ingestion pane-recovery probe.
#[test]
fn focused_client_panes_tracks_the_attached_client() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("focus");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };

    // Birth a background session: it exists and answers actions, but has no
    // attached client yet.
    let created = scoped_zellij(xdg.path())
        .args(["attach", "--create-background", &name])
        .bounded_output()
        .expect("attach --create-background");
    assert!(
        created.status.success(),
        "create-background failed: {}",
        String::from_utf8_lossy(&created.stderr),
    );
    wait_until_session_ready(xdg.path(), &name);

    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    // `--create-background` births the session without attaching, but the
    // bootstrap client that created it can still surface in `list-clients` for a
    // beat before it detaches — a window that widens under load. Poll until the
    // roster drains, then assert the steady state: a background session with no
    // client focuses nothing. A real regression (a detached session that keeps a
    // focused client) never drains and still fails here.
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    let detached = loop {
        let panes = backend
            .focused_client_panes(ClientFocusOptions {
                session_name: Some(name.clone()),
                ..Default::default()
            })
            .expect("focused_client_panes detached");
        if panes.is_empty() || Instant::now() >= deadline {
            break panes;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        detached.is_empty(),
        "a background session with no client focuses nothing: {detached:?}",
    );

    // Attach a client; its focused terminal pane is now reported.
    let _client = AttachedClient::attach(xdg.path(), &name, 200, 50);
    wait_for_attached_client(xdg.path(), &name);
    let pane_id = wait_for_pane_count(xdg.path(), &name, 1)[0].pane_id.clone();

    let deadline = Instant::now() + SPAWN_TIMEOUT;
    let focused = loop {
        let panes = backend
            .focused_client_panes(ClientFocusOptions {
                session_name: Some(name.clone()),
                ..Default::default()
            })
            .expect("focused_client_panes attached");
        if !panes.is_empty() || Instant::now() >= deadline {
            break panes;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        focused.len(),
        1,
        "one attached client focuses one pane: {focused:?}",
    );
    assert_eq!(
        focused[0], pane_id,
        "the attached client focuses the session's lone terminal pane: {focused:?}",
    );
}
