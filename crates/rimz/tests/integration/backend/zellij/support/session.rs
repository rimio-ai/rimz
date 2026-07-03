use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::ids::WorkspaceId;
use rimz::mux::{LayoutColumn, PaneCmd, SidebarPaneOptions, SidebarWidth};
use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, ScrubSessionEnvExt};

pub(in crate::backend::zellij) const SPAWN_TIMEOUT: Duration = Duration::from_secs(30);
pub(in crate::backend::zellij) const LIST_PANES_JSON_TIMEOUT: Duration =
    Duration::from_millis(1500);
pub(in crate::backend::zellij) const LIST_PANES_JSON_ATTEMPTS: u32 = 5;
pub(in crate::backend::zellij) const LIST_PANES_JSON_RETRY_DELAY: Duration =
    Duration::from_millis(50);
pub(in crate::backend::zellij) const ACTION_ATTEMPTS: u32 = 3;
pub(in crate::backend::zellij) const ACTION_CONFIRM_WINDOW: Duration = Duration::from_secs(3);
pub(in crate::backend::zellij) const ACTION_CONFIRM_STEP: Duration = Duration::from_millis(50);
pub(in crate::backend::zellij) const CLIENT_ATTACH_CONFIRM_WINDOW: Duration =
    Duration::from_millis(750);
pub(in crate::backend::zellij) const CLIENT_ATTACH_PROBE_STEP: Duration =
    Duration::from_millis(100);
pub(in crate::backend::zellij) const DUMP_LAYOUT_ATTEMPTS: u32 = 10;
pub(in crate::backend::zellij) const DUMP_LAYOUT_RETRY_DELAY: Duration = Duration::from_millis(100);

pub(in crate::backend::zellij) fn tiled_column(panes: Vec<PaneCmd>) -> LayoutColumn {
    LayoutColumn {
        panes,
        stacked: false,
    }
}

pub(in crate::backend::zellij) fn sidebar_opts(
    name: &str,
    cwd: &Path,
    stub: PathBuf,
    detected_cols: u16,
) -> SidebarPaneOptions {
    let workspace_root = PathBuf::from(format!("/tmp/rimz-{name}"));
    SidebarPaneOptions {
        session_name: name.to_owned(),
        workspace_id: WorkspaceId::from_project_root(&workspace_root),
        project_root: cwd.to_path_buf(),
        cwd: cwd.to_path_buf(),
        birth_size: SidebarWidth::default().birth_size(Some(detected_cols)),
        rimz_bin: stub,
        replace_existing: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    }
}

pub(in crate::backend::zellij) fn create_plain_background_session(
    xdg: &Path,
    name: &str,
    cwd: &Path,
    sleep: &str,
) {
    let layout = cwd.join("plain.kdl");
    std::fs::write(
        &layout,
        format!("layout {{\n    pane command=\"sleep\" {{\n        args \"{sleep}\"\n    }}\n}}\n"),
    )
    .expect("write plain layout");
    let created = scoped_zellij(xdg)
        .args(["attach", "--create-background", name, "options"])
        .arg("--default-cwd")
        .arg(cwd)
        .arg("--default-layout")
        .arg(&layout)
        .bounded_status()
        .expect("create plain session");
    assert!(created.success(), "create-background failed for {name}");
}

pub(in crate::backend::zellij) fn scoped_runtime_dir() -> TempDir {
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
pub(in crate::backend::zellij) fn scoped_zellij(xdg: &Path) -> std::process::Command {
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
pub(in crate::backend::zellij) struct ZellijSession {
    pub(in crate::backend::zellij) name: String,
    pub(in crate::backend::zellij) xdg: TempDir,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    _reader_thread: Option<std::thread::JoinHandle<()>>,
}

impl ZellijSession {
    pub(in crate::backend::zellij) fn spawn(name: impl Into<String>) -> Self {
        Self::attach_pty(scoped_runtime_dir(), name.into(), true)
    }

    /// Attach a PTY client to a session that already exists on `xdg` (born via
    /// `attach --create-background`). A detached server under load can drop a
    /// pane-exit or relayout outright and only reconciles on the next attach,
    /// so a test asserting prompt close/resize behaviour keeps a client
    /// attached for the session's lifetime.
    pub(in crate::backend::zellij) fn attach_existing(
        xdg: TempDir,
        name: impl Into<String>,
    ) -> Self {
        Self::attach_pty(xdg, name.into(), false)
    }

    pub(in crate::backend::zellij) fn attach_pty(xdg: TempDir, name: String, create: bool) -> Self {
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

    pub(in crate::backend::zellij) fn press_alt(&mut self, key: char) {
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
pub(in crate::backend::zellij) struct AttachedClient {
    _master: Box<dyn portable_pty::MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl AttachedClient {
    pub(in crate::backend::zellij) fn attach(xdg: &Path, name: &str, cols: u16, rows: u16) -> Self {
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
        cmd.env("XDG_STATE_HOME", xdg);
        cmd.env("XDG_CONFIG_HOME", xdg);
        cmd.env("XDG_CACHE_HOME", xdg);
        cmd.env("HOME", xdg);
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
pub(in crate::backend::zellij) struct ScopedSessionCleanup {
    pub(in crate::backend::zellij) name: String,
    pub(in crate::backend::zellij) xdg: PathBuf,
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
pub(in crate::backend::zellij) fn wait_until_session_ready(xdg: &Path, name: &str) {
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

pub(in crate::backend::zellij) fn capture_pty_output(
    spec: &rimz::mux::CommandSpec,
    duration: Duration,
) -> Vec<u8> {
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

pub(in crate::backend::zellij) fn unique_session_name(prefix: &str) -> String {
    let id = uuid::Uuid::now_v7().simple().to_string();
    format!("rimz-{prefix}-{}", &id[..12])
}

pub(in crate::backend::zellij) fn sidebar_command_stub() -> (TempDir, PathBuf) {
    sidebar_stub_alive_for(30)
}

/// A renderer stand-in that sleeps for `seconds` then exits. The width tests
/// wait through an attach and a tab open, which can outlive the shared 30s
/// stub, so they ask for a longer life.
pub(in crate::backend::zellij) fn sidebar_stub_alive_for(seconds: u32) -> (TempDir, PathBuf) {
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
