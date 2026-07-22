use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::ids::WorkspaceId;
use rimz::mux::{
    LayoutColumn, MuxBackend, PaneCmd, SidebarPaneOptions, SidebarWidth, ZellijBackend,
};
use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, ScrubSessionEnvExt};

pub(in crate::backend::zellij) const SPAWN_TIMEOUT: Duration = Duration::from_secs(60);
pub(in crate::backend::zellij) const LIST_PANES_JSON_TIMEOUT: Duration =
    Duration::from_millis(1500);
pub(in crate::backend::zellij) const LIST_PANES_JSON_ATTEMPTS: u32 = 5;
pub(in crate::backend::zellij) const LIST_PANES_JSON_RETRY_DELAY: Duration =
    Duration::from_millis(50);
pub(in crate::backend::zellij) const DUMP_LAYOUT_ATTEMPTS: u32 = 10;
pub(in crate::backend::zellij) const DUMP_LAYOUT_RETRY_DELAY: Duration = Duration::from_millis(100);

const ATTACHED_CLIENT_REGISTRATION_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(15);
const ATTACHED_CLIENT_OUTPUT_TAIL_BYTES: usize = 8 * 1024;

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
        extra_env: BTreeMap::from([(
            "RIMZ_TEST_ASSUME_SIDEBAR_HEARTBEAT".to_owned(),
            "1".to_owned(),
        )]),
        cwd: cwd.to_path_buf(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(detected_cols)),
        detected_view_size: None,
        width_override: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    }
}

/// Publish the build-stable room executable that production records before a
/// Zellij birth. Presence topology must use this pointer rather than falling
/// back to an unrelated `rimz` installed on the test runner's PATH.
pub(in crate::backend::zellij) fn publish_room_bin(state_root: &Path, opts: &SidebarPaneOptions) {
    let state = rimz::StatePaths::under(opts.workspace_id.clone(), state_root)
        .expect("test room state paths");
    state.ensure_dirs().expect("test room state dirs");
    std::fs::copy(&opts.rimz_bin, &state.room_bin).expect("publish test room binary");
    rimz::store::workspace_record::write(
        &state,
        &rimz::WorkspaceRecord {
            workspace_id: opts.workspace_id.clone(),
            project_root: opts.project_root.clone(),
            worktree_root: None,
            session_name: opts.session_name.clone(),
            root_class: rimz::workspace::RootClass::Directory,
            rimz_bin: Some(state.room_bin.clone()),
            rimz_build: None,
            updated_at: jiff::Timestamp::now(),
        },
    )
    .expect("publish test workspace record");
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
    // config into the session under test. A real `TMPDIR` leaks the test
    // server's log lines into the user's shared Zellij log.
    cmd.env("XDG_RUNTIME_DIR", xdg)
        .env("XDG_STATE_HOME", xdg)
        .env("XDG_CONFIG_HOME", xdg)
        .env("XDG_CACHE_HOME", xdg)
        .env("HOME", xdg)
        .env("TMPDIR", xdg);
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
        cmd.env("TMPDIR", xdg.path());
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
    xdg: PathBuf,
    name: String,
    cols: u16,
    rows: u16,
    lineage: Option<String>,
    create: bool,
    output_tail: Arc<Mutex<Vec<u8>>>,
    process: AttachedClientProcess,
}

struct AttachedClientProcess {
    _master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl AttachedClientProcess {
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl AttachedClient {
    pub(in crate::backend::zellij) fn attach(xdg: &Path, name: &str, cols: u16, rows: u16) -> Self {
        let mut client = Self::attach_inner(xdg, name, cols, rows, None, false);
        client.wait_until_registered();
        client
    }

    /// Attach carrying a remote lineage, under the exact `attach --create
    /// <session>` argv a production remote attach spawns — the reaper selects
    /// its victims on that argv, so the fixture reproduces it. Birth the
    /// session first; `--create` here attaches to what already exists.
    pub(in crate::backend::zellij) fn attach_with_lineage(
        xdg: &Path,
        name: &str,
        lineage: &str,
        cols: u16,
        rows: u16,
    ) -> Self {
        Self::attach_inner(xdg, name, cols, rows, Some(lineage), true)
    }

    fn attach_inner(
        xdg: &Path,
        name: &str,
        cols: u16,
        rows: u16,
        lineage: Option<&str>,
        create: bool,
    ) -> Self {
        let output_tail = Arc::new(Mutex::new(Vec::new()));
        let process = Self::spawn_process(
            xdg,
            name,
            cols,
            rows,
            lineage,
            create,
            Arc::clone(&output_tail),
        );
        Self {
            xdg: xdg.to_path_buf(),
            name: name.to_owned(),
            cols,
            rows,
            lineage: lineage.map(str::to_owned),
            create,
            output_tail,
            process,
        }
    }

    fn spawn_process(
        xdg: &Path,
        name: &str,
        cols: u16,
        rows: u16,
        lineage: Option<&str>,
        create: bool,
        output_tail: Arc<Mutex<Vec<u8>>>,
    ) -> AttachedClientProcess {
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
        cmd.env("TMPDIR", xdg);
        if let Some(lineage) = lineage {
            cmd.env(rimz::remote::REMOTE_LINEAGE_ENV, lineage);
        }
        if create {
            cmd.args(["attach", "--create", name]);
        } else {
            cmd.args(["attach", name]);
        }
        let child = pair.slave.spawn_command(cmd).expect("spawn zellij attach");
        drop(pair.slave);
        let writer = pair.master.take_writer().expect("PTY writer");
        // Drain the PTY in the background so the kernel buffer never fills and
        // stalls the client. Keep a bounded tail so registration failures name
        // what the attach process reported instead of timing out silently.
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(read) => {
                        let mut tail = output_tail
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        tail.extend_from_slice(&buf[..read]);
                        let excess = tail.len().saturating_sub(ATTACHED_CLIENT_OUTPUT_TAIL_BYTES);
                        if excess > 0 {
                            tail.drain(..excess);
                        }
                    }
                }
            }
        });
        AttachedClientProcess {
            _master: pair.master,
            writer,
            child,
        }
    }

    fn respawn(&mut self) {
        self.process.stop();
        self.process = Self::spawn_process(
            &self.xdg,
            &self.name,
            self.cols,
            self.rows,
            self.lineage.as_deref(),
            self.create,
            Arc::clone(&self.output_tail),
        );
    }

    fn wait_until_registered(&mut self) {
        let deadline = Instant::now() + SPAWN_TIMEOUT;
        let mut attempt_started = Instant::now();
        let mut attempts = 1;
        let mut consecutive_matches = 0;
        let mut last_view = Vec::new();
        let mut last_error = String::new();
        let mut last_exit_status = None;
        let backend = ZellijBackend::with_runtime_dir(&self.xdg);

        loop {
            if let Some(status) = self.exit_status() {
                last_exit_status = Some(status.to_string());
                if Instant::now() >= deadline {
                    break;
                }
                self.respawn();
                attempts += 1;
                attempt_started = Instant::now();
                consecutive_matches = 0;
                continue;
            }

            match super::client::client_viewed_panes(&backend, &self.name) {
                Ok(view) if !view.is_empty() => {
                    last_view = view;
                    last_error.clear();
                    consecutive_matches += 1;
                    if consecutive_matches == 2 {
                        return;
                    }
                }
                Ok(view) => {
                    last_view = view;
                    last_error.clear();
                    consecutive_matches = 0;
                }
                Err(err) => {
                    last_error = err;
                    consecutive_matches = 0;
                }
            }

            let now = Instant::now();
            if now >= deadline {
                break;
            }
            if now.duration_since(attempt_started) >= ATTACHED_CLIENT_REGISTRATION_ATTEMPT_TIMEOUT {
                self.respawn();
                attempts += 1;
                attempt_started = Instant::now();
                consecutive_matches = 0;
                continue;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        let child_exit_status = self
            .exit_status()
            .map(|status| status.to_string())
            .unwrap_or_else(|| "running".to_owned());
        let output_tail = self.output_tail();
        panic!(
            "attached client for {} did not register within {:?}; attempts: {attempts}; last view: {last_view:?}; last error: {last_error}; child exit status: {child_exit_status}; last exited attempt: {last_exit_status:?}; PTY output tail: {output_tail:?}",
            self.name, SPAWN_TIMEOUT,
        );
    }

    fn output_tail(&self) -> String {
        let tail = self
            .output_tail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(&tail).into_owned()
    }

    pub(in crate::backend::zellij) fn press_alt(&mut self, key: char) {
        self.process
            .writer
            .write_all(&[0x1b, key as u8])
            .expect("write alt key");
        self.process.writer.flush().expect("flush alt key");
    }

    pub(in crate::backend::zellij) fn pid(&self) -> u32 {
        self.process
            .child
            .process_id()
            .expect("attached client process id")
    }

    /// The client's exit status once it has stopped, and `None` while it runs.
    /// A wait that depends on this client being registered with the server can
    /// end the moment the client is gone.
    pub(in crate::backend::zellij) fn exit_status(&mut self) -> Option<portable_pty::ExitStatus> {
        self.process.child.try_wait().ok().flatten()
    }

    pub(in crate::backend::zellij) fn go_to_tab(&mut self, tab: u8) {
        assert!((1..=9).contains(&tab), "test helper supports tabs 1-9");
        self.process
            .writer
            .write_all(&[0x14, b'0' + tab])
            .expect("write tab-mode key sequence");
        self.process
            .writer
            .flush()
            .expect("flush tab-mode key sequence");
    }

    pub(in crate::backend::zellij) fn send_line(&mut self, line: &str) {
        self.process
            .writer
            .write_all(line.as_bytes())
            .expect("write line");
        self.process.writer.write_all(b"\r").expect("write enter");
        self.process.writer.flush().expect("flush line");
    }
}

impl Drop for AttachedClient {
    fn drop(&mut self) {
        self.process.stop();
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
/// listing. Sessions take 300–800 ms to come up on a quiet host; starved CI can
/// exceed 30 s, so widen the in-test window rather than burn a full retry cycle.
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

pub(in crate::backend::zellij) fn wait_for_live_session(
    backend: &ZellijBackend,
    name: &str,
) -> Vec<String> {
    super::actions::poll_until(
        Duration::from_secs(15),
        || backend.list_sessions().map_err(|err| err.to_string()),
        |sessions| sessions.iter().any(|session| session == name),
        &format!("live session {name}"),
    )
}

pub(in crate::backend::zellij) fn capture_pty_output_until(
    spec: &rimz::mux::CommandSpec,
    timeout: Duration,
    mut ready: impl FnMut(&[u8]) -> bool,
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
    let (output_tx, output_rx) = mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => {
                    if output_tx.send(buffer[..read].to_vec()).is_err() {
                        return;
                    }
                }
            }
        }
    });

    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    while !ready(&output) {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match output_rx.recv_timeout(Duration::from_millis(100).min(deadline - now)) {
            Ok(chunk) => output.extend_from_slice(&chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    drop(pair.master);
    reader_thread.join().expect("join reader");
    for chunk in output_rx.try_iter() {
        output.extend_from_slice(&chunk);
    }
    output
}

/// A session name no concurrent test can also draw. The leading 48 bits of a v7
/// UUID are its millisecond clock, so a name cut from the front alone repeats
/// across tests that start in the same millisecond — and a repeat is visible to
/// anything that scans the process table by session name rather than by runtime
/// dir, the reaper included. Pair the clock tail with random bytes so the name
/// stays roughly time-ordered and still unique.
pub(in crate::backend::zellij) fn unique_session_name(prefix: &str) -> String {
    let id = uuid::Uuid::now_v7().simple().to_string();
    format!("rimz-{prefix}-{}-{}", &id[6..12], &id[26..32])
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
    let rimz = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             if [ \"$1\" = sidebar ] && [ \"$2\" = wake ]; then\n\
             \texec {} \"$@\"\n\
             fi\n\
             sleep {seconds}\n",
            shell_quote(&rimz.display().to_string()),
        ),
    )
    .expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
    }
    (dir, path)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
