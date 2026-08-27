use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::ids::PaneId;
use rimz::mux::{ClientFocusOptions, ClientView, MuxBackend, ZellijBackend};

use super::session::{LiveZellijSession, SPAWN_TIMEOUT};

const REGISTRATION_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(15);
const OUTPUT_TAIL_BYTES: usize = 64 * 1024;
const CLIENT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const FOCUS_TIMEOUT: Duration = Duration::from_secs(30);
const INPUT_DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);
const MARKER_RETYPE_INTERVAL: Duration = Duration::from_millis(500);
const MARKER_CAPTURE_LINES: u16 = 200;

enum AttachMode {
    Normal,
    Create,
    ExactLineage(String),
    RemoteWrapper,
}

pub(in crate::backend::zellij) struct AttachedClient {
    xdg: PathBuf,
    name: String,
    cols: u16,
    rows: u16,
    mode: AttachMode,
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
    /// Attach to an existing session and return after one new human client is
    /// stably registered.
    pub(in crate::backend::zellij) fn attach(
        session: &LiveZellijSession,
        cols: u16,
        rows: u16,
    ) -> Self {
        wait_for_human_client_count(session.backend(), session.name(), 0);
        let mut client = Self::spawn(session, cols, rows, AttachMode::Normal);
        client.wait_until_registered(1);
        client
    }

    /// Create a session through the attached client and return after both the
    /// client roster and action server are stable.
    pub(in crate::backend::zellij) fn create_and_attach(
        session: &LiveZellijSession,
        cols: u16,
        rows: u16,
    ) -> Self {
        let mut client = Self::spawn(session, cols, rows, AttachMode::Create);
        client.wait_until_registered(1);
        session.wait_until_ready();
        client
    }

    /// Attach one exact `attach --create` process carrying remote lineage.
    /// This mode never respawns because the reaper test asserts its PID.
    pub(in crate::backend::zellij) fn attach_with_lineage(
        session: &LiveZellijSession,
        lineage: &str,
        cols: u16,
        rows: u16,
    ) -> Self {
        Self::spawn(
            session,
            cols,
            rows,
            AttachMode::ExactLineage(lineage.to_owned()),
        )
    }

    pub(in crate::backend::zellij) fn attach_remote_wrapper(
        session: &LiveZellijSession,
        cols: u16,
        rows: u16,
    ) -> Self {
        Self::spawn(session, cols, rows, AttachMode::RemoteWrapper)
    }

    fn spawn(session: &LiveZellijSession, cols: u16, rows: u16, mode: AttachMode) -> Self {
        let output_tail = Arc::new(Mutex::new(Vec::new()));
        let process = spawn_process_at(
            session.path(),
            session.name(),
            cols,
            rows,
            &mode,
            Arc::clone(&output_tail),
        );
        Self {
            xdg: session.namespace().path().to_path_buf(),
            name: session.name().to_owned(),
            cols,
            rows,
            mode,
            output_tail,
            process,
        }
    }

    fn respawn(&mut self) {
        assert!(
            !matches!(self.mode, AttachMode::ExactLineage(_)),
            "exact-lineage clients cannot respawn"
        );
        self.process.stop();
        self.process = spawn_process_at(
            &self.xdg,
            &self.name,
            self.cols,
            self.rows,
            &self.mode,
            Arc::clone(&self.output_tail),
        );
    }

    fn wait_until_registered(&mut self, expected_clients: usize) {
        let deadline = Instant::now() + SPAWN_TIMEOUT;
        let mut attempt_started = Instant::now();
        let mut attempts = 1;
        let mut consecutive_registrations = 0;
        let mut consecutive_terminal_views = 0;
        let mut last_human_clients = 0;
        let mut last_terminal_view = Vec::new();
        let mut last_error = String::new();
        let mut last_exit_status = None;
        let backend = ZellijBackend::with_runtime_dir(&self.xdg);

        loop {
            if let Some(status) = self.exit_status() {
                last_exit_status = Some(status.to_string());
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(CLIENT_POLL_INTERVAL);
                self.respawn();
                attempts += 1;
                attempt_started = Instant::now();
                consecutive_registrations = 0;
                consecutive_terminal_views = 0;
                continue;
            }

            match observe_client_view(&backend, &self.name) {
                Ok(view) => {
                    last_human_clients = view.presence.human_clients;
                    last_terminal_view = view.viewed_panes;
                    last_error.clear();
                    if last_human_clients == expected_clients {
                        consecutive_registrations += 1;
                        if consecutive_registrations >= 2 && !last_terminal_view.is_empty() {
                            consecutive_terminal_views += 1;
                            if consecutive_terminal_views == 2 {
                                return;
                            }
                        } else {
                            consecutive_terminal_views = 0;
                        }
                    } else {
                        consecutive_registrations = 0;
                        consecutive_terminal_views = 0;
                    }
                }
                Err(error) => {
                    last_error = error;
                    consecutive_registrations = 0;
                    consecutive_terminal_views = 0;
                }
            }

            let now = Instant::now();
            if now >= deadline {
                break;
            }
            if now.duration_since(attempt_started) >= REGISTRATION_ATTEMPT_TIMEOUT {
                self.respawn();
                attempts += 1;
                attempt_started = Instant::now();
                consecutive_registrations = 0;
                consecutive_terminal_views = 0;
                continue;
            }
            std::thread::sleep(CLIENT_POLL_INTERVAL);
        }

        let current_status = self
            .exit_status()
            .map(|status| status.to_string())
            .unwrap_or_else(|| "running".to_owned());
        panic!(
            "attached client for {} did not become registered and terminal-ready within {:?}; attempts: {attempts}; expected human clients: {expected_clients}; last human clients: {last_human_clients}; last terminal view: {last_terminal_view:?}; last probe error: {last_error}; current child status: {current_status}; last exited attempt: {last_exit_status:?}; PTY output tail: {:?}",
            self.name,
            SPAWN_TIMEOUT,
            self.output_tail(),
        );
    }

    pub(in crate::backend::zellij) fn view(&self) -> ClientView {
        observe_client_view(&ZellijBackend::with_runtime_dir(&self.xdg), &self.name)
            .unwrap_or_else(|error| panic!("client view for {}: {error}", self.name))
    }

    pub(in crate::backend::zellij) fn wait_until_focused(
        &self,
        want: &PaneId,
        context: &str,
    ) -> Vec<PaneId> {
        self.wait_for_stable_focus(want, context, None)
    }

    pub(in crate::backend::zellij) fn press_alt_until(
        &mut self,
        key: char,
        want: &PaneId,
        context: &str,
    ) -> Vec<PaneId> {
        self.converge_focus(want, context, |client| client.press_alt(key))
    }

    pub(in crate::backend::zellij) fn go_to_tab_until(
        &mut self,
        tab: u8,
        want: &PaneId,
        context: &str,
    ) -> Vec<PaneId> {
        self.converge_focus(want, context, |client| client.go_to_tab(tab))
    }

    fn converge_focus(
        &mut self,
        want: &PaneId,
        context: &str,
        mut nudge: impl FnMut(&mut Self),
    ) -> Vec<PaneId> {
        let deadline = Instant::now() + FOCUS_TIMEOUT;
        let backend = ZellijBackend::with_runtime_dir(&self.xdg);
        let mut stable = 0;
        let mut attempts = 0;
        let mut last_view = Vec::new();
        let mut last_error = String::new();
        loop {
            match observe_client_view(&backend, &self.name) {
                Ok(view) => {
                    last_view = view.viewed_panes;
                    last_error.clear();
                    if last_view.contains(want) {
                        stable += 1;
                        if stable == 2 {
                            return last_view;
                        }
                    } else {
                        stable = 0;
                        nudge(self);
                        attempts += 1;
                    }
                }
                Err(error) => {
                    last_error = error;
                    stable = 0;
                    nudge(self);
                    attempts += 1;
                }
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out focusing {context} ({want}) through the attached client in {}; attempts: {attempts}; last client view: {last_view:?}; last error: {last_error}",
                    self.name,
                );
            }
            std::thread::sleep(CLIENT_POLL_INTERVAL);
        }
    }

    fn wait_for_stable_focus(
        &self,
        want: &PaneId,
        context: &str,
        timeout: Option<Duration>,
    ) -> Vec<PaneId> {
        let deadline = Instant::now() + timeout.unwrap_or(FOCUS_TIMEOUT);
        let backend = ZellijBackend::with_runtime_dir(&self.xdg);
        let mut stable = 0;
        let mut last_view = Vec::new();
        let mut last_error = String::new();
        loop {
            match observe_client_view(&backend, &self.name) {
                Ok(view) => {
                    last_view = view.viewed_panes;
                    last_error.clear();
                    if last_view.contains(want) {
                        stable += 1;
                        if stable == 2 {
                            return last_view;
                        }
                    } else {
                        stable = 0;
                    }
                }
                Err(error) => {
                    last_error = error;
                    stable = 0;
                }
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {context} ({want}) in {}; last client view: {last_view:?}; last error: {last_error}",
                    self.name,
                );
            }
            std::thread::sleep(CLIENT_POLL_INTERVAL);
        }
    }

    /// Prove stable routed input with two unique markers in target scrollback.
    pub(in crate::backend::zellij) fn assert_input_reaches(
        &mut self,
        want: &PaneId,
        context: &str,
    ) {
        let backend = ZellijBackend::with_runtime_dir(&self.xdg);
        let work_panes: HashSet<_> = super::panes::PaneSnapshot::expect(&self.xdg, &self.name)
            .panes
            .iter()
            .filter(|pane| pane.is_live_terminal() && !pane.is_sidebar())
            .map(|pane| pane.pane_ref(&self.name).pane_id)
            .collect();
        let deadline = Instant::now() + INPUT_DELIVERY_TIMEOUT;
        let marker = format!("rimz-routed-{}", uuid::Uuid::now_v7().simple());
        let markers = [marker.clone(), format!("{marker}-confirmed")];
        let mut marker_index = 0;
        let mut next_sample_at = Instant::now();
        let mut last_capture = String::new();
        let mut last_view = Vec::new();
        let mut last_capture_error = String::new();
        let mut last_view_error = String::new();
        loop {
            if Instant::now() >= next_sample_at {
                next_sample_at = Instant::now() + MARKER_RETYPE_INTERVAL;
                match observe_client_view(&backend, &self.name) {
                    Ok(view) => {
                        if view
                            .viewed_panes
                            .iter()
                            .any(|pane| work_panes.contains(pane))
                        {
                            self.send_line(&markers[marker_index]);
                        }
                        last_view = view.viewed_panes;
                        last_view_error.clear();
                    }
                    Err(error) => last_view_error = error,
                }
            }
            match backend.capture_pane(want, Some(MARKER_CAPTURE_LINES), false) {
                Ok(capture) => {
                    if capture.raw_text.contains(&markers[marker_index]) {
                        if marker_index + 1 == markers.len() {
                            return;
                        }
                        marker_index += 1;
                        next_sample_at = Instant::now() + MARKER_RETYPE_INTERVAL;
                    }
                    last_capture = capture.raw_text;
                    last_capture_error.clear();
                }
                Err(error) => last_capture_error = error.to_string(),
            }
            if Instant::now() >= deadline {
                panic!(
                    "attached input did not settle on {context} ({want}) in {}; pending marker: {}; last client view: {last_view:?}; last capture: {last_capture:?}; last view error: {last_view_error}; last capture error: {last_capture_error}",
                    self.name, markers[marker_index],
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub(in crate::backend::zellij) fn press_alt(&mut self, key: char) {
        self.process
            .writer
            .write_all(&[0x1b, key as u8])
            .expect("write alt key");
        self.process.writer.flush().expect("flush alt key");
    }

    pub(in crate::backend::zellij) fn press_detach(&mut self) {
        self.process
            .writer
            .write_all(&[0x0f, b'd'])
            .expect("write detach key sequence");
        self.process
            .writer
            .flush()
            .expect("flush detach key sequence");
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

    pub(in crate::backend::zellij) fn pid(&self) -> u32 {
        self.process
            .child
            .process_id()
            .expect("attached client process id")
    }

    pub(in crate::backend::zellij) fn exit_status(&mut self) -> Option<portable_pty::ExitStatus> {
        self.process.child.try_wait().ok().flatten()
    }

    pub(in crate::backend::zellij) fn output_bytes(&self) -> Vec<u8> {
        self.output_tail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn output_tail(&self) -> String {
        let tail = self
            .output_tail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(&tail).into_owned()
    }
}

impl Drop for AttachedClient {
    fn drop(&mut self) {
        self.process.stop();
    }
}

fn spawn_process_at(
    xdg: &std::path::Path,
    name: &str,
    cols: u16,
    rows: u16,
    mode: &AttachMode,
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
    let program = match mode {
        AttachMode::RemoteWrapper => crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz")),
        _ => PathBuf::from("zellij"),
    };
    let mut command = CommandBuilder::new(program);
    crate::common::ZellijNamespace::pin_pty_at(xdg, &mut command);
    if let AttachMode::ExactLineage(lineage) = mode {
        command.env(rimz::remote::REMOTE_LINEAGE_ENV, lineage);
    }
    match mode {
        AttachMode::Normal => command.args(["attach", name]),
        AttachMode::Create | AttachMode::ExactLineage(_) => {
            command.args(["attach", "--create", name])
        }
        AttachMode::RemoteWrapper => {
            command.env(rimz::remote::REMOTE_LINEAGE_ENV, "0123456789abcdef");
            command.env(rimz::remote::REMOTE_SUPERVISED_ENV, "1");
            command.args(["attach", "--zellij", "--attach", name])
        }
    };
    let child = pair
        .slave
        .spawn_command(command)
        .expect("spawn zellij attach");
    drop(pair.slave);
    let writer = pair.master.take_writer().expect("PTY writer");
    let mut reader = pair.master.try_clone_reader().expect("clone reader");
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => {
                    let mut tail = output_tail
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    tail.extend_from_slice(&buffer[..read]);
                    let excess = tail.len().saturating_sub(OUTPUT_TAIL_BYTES);
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

pub(in crate::backend::zellij) fn observe_client_view(
    backend: &ZellijBackend,
    session: &str,
) -> Result<ClientView, String> {
    backend
        .client_view(ClientFocusOptions {
            session_name: Some(session.to_owned()),
            ..Default::default()
        })
        .map_err(|error| error.to_string())
}

/// Observe an explicit attached/detached client-count assertion.
pub(in crate::backend::zellij) fn wait_for_human_client_count(
    backend: &ZellijBackend,
    session: &str,
    want: usize,
) -> ClientView {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    let mut consecutive_matches = 0;
    let mut last_view = ClientView::default();
    let mut last_error = String::new();
    loop {
        match observe_client_view(backend, session) {
            Ok(view) => {
                consecutive_matches = if view.presence.human_clients == want {
                    consecutive_matches + 1
                } else {
                    0
                };
                last_view = view;
                last_error.clear();
                if consecutive_matches == 2 {
                    return last_view;
                }
            }
            Err(error) => {
                last_error = error;
                consecutive_matches = 0;
            }
        }
        if Instant::now() >= deadline {
            panic!(
                "human client count for {session} did not stabilize at {want}; last count: {}; last terminal view: {:?}; last error: {last_error}",
                last_view.presence.human_clients, last_view.viewed_panes,
            );
        }
        std::thread::sleep(CLIENT_POLL_INTERVAL);
    }
}
