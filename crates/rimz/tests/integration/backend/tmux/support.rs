pub(super) use std::collections::BTreeMap;
pub(super) use std::io::Read;
pub(super) use std::path::{Path, PathBuf};
pub(super) use std::process::Command;
pub(super) use std::thread;
pub(super) use std::time::{Duration, Instant};

pub(super) use portable_pty::{CommandBuilder, PtySize, native_pty_system};
pub(super) use rimz::RuntimePaths;
pub(super) use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
pub(super) use rimz::ids::{AgentKind, MuxName, PaneId, SidebarInstanceId, WorkspaceId};
pub(super) use rimz::mux::{
    ClientFocusOptions, LayoutColumn, LayoutPanes, MuxBackend, NamedKey, PaneCmd, PaneListOptions,
    PaneReadConsistency, SessionOptions, SidebarPaneOptions, SidebarWidth, SplitPaneOptions,
    SplitPlacement, SplitTarget, TabOptions, TmuxBackend,
};
pub(super) use rimz::pane::PaneRef;
pub(super) use rimz::sidebar::{SidebarLaunchOutcome, launch_sidebar_if_needed, write_heartbeat};
pub(super) use rimz::workspace::WorkspaceResolver;
pub(super) use tempfile::TempDir;

pub(super) use crate::common::{Env, ScrubSessionEnvExt, write_failing_agent_shim};

pub(super) fn tiled_column(panes: Vec<PaneCmd>) -> LayoutColumn {
    LayoutColumn {
        panes,
        stacked: false,
    }
}

pub(super) fn sidebar_opts(
    session: &str,
    stub: PathBuf,
    detected_cols: Option<u16>,
) -> SidebarPaneOptions {
    let workspace_root = PathBuf::from(format!("/tmp/rimz-{session}"));
    SidebarPaneOptions {
        session_name: session.to_owned(),
        workspace_id: WorkspaceId::from_project_root(&workspace_root),
        project_root: std::env::temp_dir(),
        extra_env: BTreeMap::from([(
            "RIMZ_TEST_ASSUME_SIDEBAR_HEARTBEAT".to_owned(),
            "1".to_owned(),
        )]),
        cwd: std::env::temp_dir(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(detected_cols),
        detected_view_size: detected_cols.map(|cols| (cols, 50)),
        width_override: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    }
}

pub(super) fn ensure_rimz_session(server: &TmuxServer, name: &str, size: Option<(u16, u16)>) {
    let workspace_root = PathBuf::from(format!("/tmp/rimz-{name}"));
    server
        .backend
        .ensure_session(&session_opts(
            name,
            WorkspaceId::from_project_root(&workspace_root),
            &std::env::temp_dir(),
            &std::env::temp_dir(),
            size,
        ))
        .expect("ensure_session");
}

pub(super) fn session_opts(
    session: &str,
    workspace_id: WorkspaceId,
    project_root: &Path,
    cwd: &Path,
    detected_size: Option<(u16, u16)>,
) -> SessionOptions {
    SessionOptions {
        session_name: session.to_owned(),
        workspace_id,
        project_root: project_root.to_path_buf(),
        extra_env: Default::default(),
        cwd: cwd.to_path_buf(),
        config: rimz::config::MultiplexerConfig::default(),
        detected_size,
        truecolor: false,
    }
}

pub(super) fn sleep_host() -> rimz::mux::HostPane {
    rimz::mux::HostPane {
        argv: vec!["sleep".to_owned(), "120".to_owned()],
        cwd: std::env::temp_dir(),
    }
}

/// Poll `capture_pane` on `pane_id` until its text contains `needle` or the
/// budget elapses; returns the last capture seen either way. Faster than a flat
/// settle sleep on the common path and more robust when the shell is slow.
pub(super) fn capture_pane_until(
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

pub(super) fn find_pane_with_capture_until(
    backend: &TmuxBackend,
    pane_ids: &[PaneId],
    needle: &str,
    budget: Duration,
) -> (PaneId, String) {
    let deadline = Instant::now() + budget;
    let mut last = Vec::new();
    loop {
        last.clear();
        for pane_id in pane_ids {
            match backend.capture_pane(pane_id, None, false) {
                Ok(capture) => {
                    if capture.raw_text.contains(needle) {
                        return (pane_id.clone(), capture.raw_text);
                    }
                    last.push(format!("{pane_id:?}: {:?}", capture.raw_text));
                }
                Err(err) => last.push(format!("{pane_id:?}: {err}")),
            }
        }
        if Instant::now() >= deadline {
            panic!("no pane captured {needle:?} before timeout: {last:?}");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

/// One pane's live placement: its raw id, left/top edge, size, and current
/// working directory — read from `list-panes -F` to assert layout geometry.
#[derive(Clone, Debug)]
pub(super) struct PaneGeom {
    #[allow(dead_code)]
    pub(super) id: String,
    pub(super) left: u64,
    pub(super) top: u64,
    pub(super) width: u64,
    pub(super) height: u64,
    pub(super) path: String,
}

/// Owns an isolated tmux server for the duration of one test. The server
/// listens on a tempdir socket; Drop tears it down with `kill-server`.
pub(super) struct TmuxServer {
    pub(super) backend: TmuxBackend,
    pub(super) socket: PathBuf,
    pub(super) _tempdir: TempDir,
}

impl TmuxServer {
    pub(super) fn new() -> Self {
        let tempdir = TempDir::new().expect("tempdir");
        let socket = tempdir.path().join("tmux.sock");
        Self {
            backend: TmuxBackend::with_socket(&socket),
            socket,
            _tempdir: tempdir,
        }
    }
    pub(super) fn ensure_with_shell(&self, session: &str) {
        self.output(&["new-session", "-d", "-s", session, "sh"]);
        assert!(!self.wait_for_panes(session, 1).is_empty());
    }
    pub(super) fn pane_current_path(&self, session: &str) -> String {
        self.display(session, "#{pane_current_path}")
    }
    pub(super) fn output(&self, args: &[&str]) -> std::process::Output {
        let output = Command::new("tmux")
            .scrub_session_env()
            .arg("-S")
            .arg(&self.socket)
            .args(args)
            .output()
            .unwrap_or_else(|err| panic!("spawn tmux {args:?}: {err}"));
        assert!(
            output.status.success(),
            "tmux {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        output
    }
    pub(super) fn stdout(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.output(args).stdout)
            .trim()
            .to_owned()
    }
    pub(super) fn display(&self, target: &str, format: &str) -> String {
        self.stdout(&["display-message", "-p", "-t", target, format])
    }
    pub(super) fn tmux(&self, args: &[&str]) {
        self.output(args);
    }
    pub(super) fn supports_floating_panes(&self) -> bool {
        let raw = self.stdout(&["-V"]);
        let mut parts = raw.split([' ', '.']).skip(1);
        let version = (
            parts.next().and_then(|part| part.parse::<u32>().ok()),
            parts.next().and_then(|part| {
                part.trim_end_matches(|ch: char| !ch.is_ascii_digit())
                    .parse::<u32>()
                    .ok()
            }),
        );
        matches!(version, (Some(major), Some(minor)) if (major, minor) >= (3, 7))
    }
    /// Wait until an exec'd pane reports the settled current command.
    pub(super) fn wait_for_pane_command(&self, session: &str, command: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let listed = list_session_panes(self, session);
            if listed
                .iter()
                .any(|pane| pane.command.as_deref() == Some(command))
            {
                return;
            }
            if Instant::now() >= deadline {
                panic!("no pane in `{session}` ran `{command}` within the deadline: {listed:?}");
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
    pub(super) fn window_names(&self, session: &str) -> Vec<String> {
        self.stdout(&["list-windows", "-t", session, "-F", "#{window_name}"])
            .lines()
            .map(|line| line.trim().to_owned())
            .collect()
    }
    pub(super) fn client_widths(&self, session: &str) -> Vec<u64> {
        self.stdout(&["list-clients", "-t", session, "-F", "#{client_width}"])
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect()
    }
    pub(super) fn wait_for_control_client(&self, session: &str) {
        // `PresenceWatch::attach` returns once the control client *spawns*; tmux
        // registers it a beat later, and commands fired before registration may
        // still observe a clientless session.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if self
                .stdout(&[
                    "list-clients",
                    "-t",
                    session,
                    "-F",
                    "#{client_control_mode}",
                ])
                .lines()
                .any(|line| line.trim() == "1")
            {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "control client never registered with the tmux server"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }
    /// Poll live pane geometry in `target` until `want` panes arrive or the budget elapses.
    /// Captures placement and cwd for layout assertions.
    pub(super) fn wait_for_panes(&self, target: &str, want: usize) -> Vec<PaneGeom> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let stdout = self.stdout(&[
                    "list-panes",
                    "-t",
                    target,
                    "-F",
                    "#{pane_id},#{pane_left},#{pane_top},#{pane_width},#{pane_height},#{s/,/_/g:pane_current_path}",
                ]);
            let panes: Vec<PaneGeom> = stdout
                .lines()
                .filter_map(|line| {
                    let mut cols = line.split(',');
                    Some(PaneGeom {
                        id: cols.next()?.to_owned(),
                        left: cols.next()?.parse().ok()?,
                        top: cols.next()?.parse().ok()?,
                        width: cols.next()?.parse().ok()?,
                        height: cols.next()?.parse().ok()?,
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
    pub(super) fn show_option(&self, scope_args: &[&str], option: &str) -> String {
        let mut args = vec!["show-options"];
        args.extend_from_slice(scope_args);
        args.extend(["-v", option]);
        self.stdout(&args)
    }
    pub(super) fn list_keys(&self, table: &str) -> String {
        self.stdout(&["list-keys", "-T", table])
    }
    pub(super) fn show_hooks(&self, session: &str) -> String {
        self.stdout(&["show-hooks", "-t", session])
    }
    pub(super) fn has_after_new_window_hook(&self, session: &str) -> bool {
        self.show_hooks(session)
            .lines()
            .any(|line| line.contains("after-new-window"))
    }
}

pub(super) fn list_session_panes(server: &TmuxServer, session: &str) -> Vec<PaneRef> {
    server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some(session.to_owned()),
            ..Default::default()
        })
        .expect("list_panes")
        .panes
        .into_iter()
        .filter(|pane| pane.session_name == session)
        .collect()
}

pub(super) fn sidebar_pane_ids(
    server: &TmuxServer,
    session: &str,
    window_id: Option<&str>,
) -> Vec<PaneId> {
    list_session_panes(server, session)
        .into_iter()
        .filter(|pane| {
            pane.command.as_deref() == Some("rimz-sidebar")
                && window_id.is_none_or(|window_id| pane.view_id.as_deref() == Some(window_id))
        })
        .map(|pane| pane.pane_id)
        .collect()
}

pub(super) fn wait_for_sidebar_pane(
    server: &TmuxServer,
    session: &str,
    window_id: Option<&str>,
) -> PaneId {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(pane) = sidebar_pane_ids(server, session, window_id)
            .into_iter()
            .next()
        {
            return pane;
        }
        if Instant::now() >= deadline {
            panic!("no sidebar pane in `{session}` window {window_id:?} within the deadline");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub(super) fn wait_for_hook_docked_window_panes(
    server: &TmuxServer,
    session: &str,
    window_id: &str,
) -> Vec<PaneRef> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let panes: Vec<PaneRef> = list_session_panes(server, session)
            .into_iter()
            .filter(|pane| pane.view_id.as_deref() == Some(window_id))
            .collect();
        if panes.len() >= 2
            && panes
                .iter()
                .any(|pane| pane.command.as_deref() == Some("rimz-sidebar"))
        {
            return panes;
        }
        if Instant::now() >= deadline {
            panic!("window `{window_id}` was not hook-docked within the deadline: {panes:?}");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        // `.output()` captures stderr so the "no server" message from
        // tests that never started a server doesn't leak into test logs.
        let _ = Command::new("tmux")
            .scrub_session_env()
            .args(["-S", self.socket.to_str().unwrap_or(""), "kill-server"])
            .output();
    }
}

/// A live tmux client held on a sized PTY so `list-clients` reports it.
/// Drop kills the client; server teardown stays with [`TmuxServer`].
pub(super) struct AttachedTmuxClient {
    pub(super) _master: Box<dyn portable_pty::MasterPty + Send>,
    pub(super) child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl AttachedTmuxClient {
    pub(super) fn attach(socket: &Path, session: &str, cols: u16, rows: u16) -> Self {
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
pub(super) fn show_session_environment(server: &TmuxServer, session: &str, name: &str) -> String {
    server.stdout(&["show-environment", "-t", session, name])
}

pub(super) fn sidebar_command_stub() -> (TempDir, PathBuf) {
    sidebar_command_stub_with_script("#!/bin/sh\nprintf '\\033]2;rimz-sidebar\\007'\nsleep 600\n")
}

pub(super) fn delayed_sidebar_title_command_stub() -> (TempDir, PathBuf) {
    sidebar_command_stub_with_script("#!/bin/sh\nsleep 600\nprintf '\\033]2;rimz-sidebar\\007'\n")
}

pub(super) fn sidebar_command_stub_with_script(script: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("stub dir");
    let path = dir.path().join("rimz-stub");
    std::fs::write(&path, script).expect("write stub");
    chmod_executable(&path);
    (dir, path)
}

pub(super) fn chmod_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod");
    }
}
