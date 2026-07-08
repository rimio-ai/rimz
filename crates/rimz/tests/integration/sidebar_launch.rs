//! Sidebar launch orchestration over the backend trait.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use rimz::ids::{MuxName, PaneId, SidebarInstanceId, WorkspaceId};
use rimz::mux::{
    CommandSpec, DaemonView, MuxBackend, MuxErr, NamedKey, PaneCapture, PaneListOptions,
    SessionOptions, SidebarPaneOptions, SidebarWidth, SplitPaneOptions,
};
use rimz::pane::PaneRef;
use rimz::sidebar::heartbeat::SIDEBAR_PROTOCOL_VERSION;
use rimz::sidebar::heartbeat::SidebarHeartbeat;
use rimz::sidebar::{SidebarLaunchOutcome, launch_sidebar_if_needed};
use rimz::{RuntimePaths, ViewKind};
use tempfile::TempDir;

#[test]
fn sidebar_launch_skips_when_fresh_heartbeat_exists() {
    let h = SidebarHarness::new();
    h.write_heartbeat();
    let backend = FakeBackend::default();

    let outcome = launch_sidebar_if_needed(&backend, &h.runtime, &h.sidebar_opts(), None);

    assert_eq!(outcome, SidebarLaunchOutcome::SkippedFresh);
    assert_eq!(backend.open_calls(), 0);
    assert_eq!(
        backend.reconcile_calls(),
        1,
        "a fresh workspace producer still ensures this session's view",
    );
}

#[test]
fn sidebar_launch_opens_once_without_heartbeat() {
    let h = SidebarHarness::new();
    h.runtime.ensure_dirs().expect("runtime dirs");
    let backend = FakeBackend::default();

    let outcome = launch_sidebar_if_needed(&backend, &h.runtime, &h.sidebar_opts(), None);

    assert_eq!(outcome, SidebarLaunchOutcome::Opened);
    assert_eq!(backend.open_calls(), 1);
    assert_eq!(backend.reconcile_calls(), 0);
    assert_eq!(
        backend.replace_existing_values(),
        vec![true],
        "missing/stale heartbeat means any existing sidebar pane may be stale \
         and must be replaced"
    );
}

#[test]
fn sidebar_launch_replaces_old_protocol_heartbeat() {
    let h = SidebarHarness::new();
    h.write_heartbeat_with_protocol("rimz.plugin.v1");
    let backend = FakeBackend::default();

    let outcome = launch_sidebar_if_needed(&backend, &h.runtime, &h.sidebar_opts(), None);

    assert_eq!(outcome, SidebarLaunchOutcome::Opened);
    assert_eq!(backend.open_calls(), 1);
    assert_eq!(backend.reconcile_calls(), 0);
    assert_eq!(backend.replace_existing_values(), vec![true]);
}

#[test]
fn sidebar_launch_error_is_non_fatal() {
    let h = SidebarHarness::new();
    h.runtime.ensure_dirs().expect("runtime dirs");
    let backend = FakeBackend::failing();

    let outcome = launch_sidebar_if_needed(&backend, &h.runtime, &h.sidebar_opts(), None);

    assert_eq!(outcome, SidebarLaunchOutcome::Failed);
    assert_eq!(backend.open_calls(), 1);
    assert_eq!(backend.reconcile_calls(), 0);
}

struct SidebarHarness {
    _dir: TempDir,
    runtime: RuntimePaths,
    workspace_id: WorkspaceId,
    cwd: PathBuf,
}

impl SidebarHarness {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
        Self {
            cwd: dir.path().to_path_buf(),
            _dir: dir,
            runtime,
            workspace_id,
        }
    }

    fn sidebar_opts(&self) -> SidebarPaneOptions {
        let width = SidebarWidth::default();
        SidebarPaneOptions {
            session_name: "session".to_owned(),
            workspace_id: self.workspace_id.clone(),
            project_root: self.cwd.clone(),
            cwd: self.cwd.clone(),
            birth_size: width.birth_size(None),
            rimz_bin: PathBuf::from("rimz"),
            replace_existing: false,
            pristine_birth: false,
            config: rimz::config::MultiplexerConfig::default(),
            resume_tabs: Vec::new(),
            refresh_ms: None,
        }
    }

    fn write_heartbeat(&self) {
        self.write_heartbeat_with_protocol(SIDEBAR_PROTOCOL_VERSION);
    }

    fn write_heartbeat_with_protocol(&self, protocol_version: &str) {
        self.runtime.ensure_dirs().expect("runtime dirs");
        let mut heartbeat = SidebarHeartbeat::new(
            self.workspace_id.clone(),
            SidebarInstanceId::new(),
            MuxName::Tmux,
            "session",
            self.runtime.sock_dir.join("sidebar.sock"),
            None,
        );
        heartbeat.protocol_version = protocol_version.to_owned();
        std::fs::write(
            self.runtime.heartbeat_dir.join("sidebar.fresh.json"),
            serde_json::to_vec(&heartbeat).expect("json"),
        )
        .expect("write heartbeat");
    }
}

#[derive(Default)]
struct FakeBackend {
    open_calls: Mutex<usize>,
    reconcile_calls: Mutex<usize>,
    replace_existing_values: Mutex<Vec<bool>>,
    fail_open: bool,
}

impl FakeBackend {
    fn failing() -> Self {
        Self {
            open_calls: Mutex::new(0),
            reconcile_calls: Mutex::new(0),
            replace_existing_values: Mutex::new(Vec::new()),
            fail_open: true,
        }
    }

    fn open_calls(&self) -> usize {
        *self.open_calls.lock().expect("open calls")
    }

    fn reconcile_calls(&self) -> usize {
        *self.reconcile_calls.lock().expect("reconcile calls")
    }

    fn replace_existing_values(&self) -> Vec<bool> {
        self.replace_existing_values
            .lock()
            .expect("replace existing values")
            .clone()
    }
}

impl MuxBackend for FakeBackend {
    fn name(&self) -> MuxName {
        MuxName::Tmux
    }

    fn ensure_session(&self, _opts: &SessionOptions) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn attach_command(&self, name: &str, _config: &rimz::config::MultiplexerConfig) -> CommandSpec {
        CommandSpec::new("fake").arg(name)
    }

    fn detach(&self, _name: &str) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn kill_session(&self, _name: &str) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn list_sessions_within(&self, _timeout: Duration) -> rimz::mux::Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn list_panes(&self, _opts: PaneListOptions) -> rimz::mux::Result<rimz::mux::PaneListing> {
        Ok(rimz::mux::PaneListing {
            panes: vec![PaneRef {
                pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
                session_name: "session".to_owned(),
                view_id: Some("@1".to_owned()),
                view_kind: Some(ViewKind::Window),
                view_name: None,
                is_focused: false,
                is_floating: false,
                command: Some("sh".to_owned()),
                spawn_command: None,
                cwd: None,
                pane_pid: None,
                pane_process_start: None,
                hosted_agent_kind: None,
                hosted_agent_process_start: None,
                resumed_session_id: None,
                elevated_agent: None,
                first_seen_at_ms: None,
            }],
            observed_at_ms: rimz::sidebar::timing::unix_now_ms(),
            authoritative_focus: None,
            client_view: None,
        })
    }

    fn split_pane(&self, _opts: SplitPaneOptions) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn focus_pane(&self, _pane: &PaneId, _session: Option<&str>) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn capture_pane(
        &self,
        pane: &PaneId,
        _lines: Option<u16>,
        _ansi: bool,
    ) -> rimz::mux::Result<PaneCapture> {
        Ok(PaneCapture {
            pane_id: pane.clone(),
            raw_text: String::new(),
            lines: Vec::new(),
        })
    }

    fn send_keys(&self, _pane: &PaneId, _text: &str) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn send_key(&self, _pane: &PaneId, _key: NamedKey) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn paste_text(&self, _pane: &PaneId, _text: &str) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn open_sidebar(
        &self,
        opts: &SidebarPaneOptions,
        _daemon: Option<&DaemonView>,
    ) -> rimz::mux::Result<()> {
        *self.open_calls.lock().expect("open calls") += 1;
        self.replace_existing_values
            .lock()
            .expect("replace existing values")
            .push(opts.replace_existing);
        if self.fail_open {
            return Err(MuxErr::Command {
                program: "fake".to_owned(),
                args: "open-sidebar".to_owned(),
                stderr: "boom".to_owned(),
            });
        }
        Ok(())
    }

    fn reconcile_sidebars(
        &self,
        _opts: &SidebarPaneOptions,
        _live: &rimz::mux::SidebarLiveness,
    ) -> rimz::mux::Result<rimz::mux::SidebarRecovery> {
        *self.reconcile_calls.lock().expect("reconcile calls") += 1;
        Ok(rimz::mux::SidebarRecovery::default())
    }

    fn open_background_view(
        &self,
        _opts: &rimz::mux::BackgroundViewOptions,
    ) -> rimz::mux::Result<rimz::mux::BackgroundViewLaunch> {
        Ok(rimz::mux::BackgroundViewLaunch::Launched)
    }

    fn open_tab(&self, _opts: &rimz::mux::TabOptions) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn close_pane(&self, _session: &str, _pane: &PaneId) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn version(&self) -> rimz::mux::Result<String> {
        Ok("fake 1.0.0".to_owned())
    }
}
