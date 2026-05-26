//! Sidebar launch orchestration over the backend trait.

use std::path::PathBuf;
use std::sync::Mutex;

use rimz::feed::PaneRef;
use rimz::ids::{MuxName, PaneId, SidebarInstanceId, WorkspaceId};
use rimz::mux::{
    CommandSpec, MuxBackend, MuxErr, PaneCapture, PaneListOptions, SessionOptions,
    SidebarPaneOptions, SplitPaneOptions,
};
use rimz::schema::SIDEBAR_PROTOCOL_VERSION;
use rimz::schema::heartbeat::SidebarHeartbeat;
use rimz::sidebar::{SidebarLaunchOutcome, launch_sidebar_if_needed};
use rimz::{RuntimePaths, ViewKind};
use tempfile::TempDir;

#[test]
fn sidebar_launch_skips_when_fresh_heartbeat_exists() {
    let h = SidebarHarness::new();
    h.write_heartbeat();
    let backend = FakeBackend::default();

    let outcome = launch_sidebar_if_needed(&backend, &h.runtime, &h.sidebar_opts());

    assert_eq!(outcome, SidebarLaunchOutcome::SkippedFresh);
    assert_eq!(backend.open_calls(), 0);
}

#[test]
fn sidebar_launch_opens_once_without_heartbeat() {
    let h = SidebarHarness::new();
    h.runtime.ensure_dirs().expect("runtime dirs");
    let backend = FakeBackend::default();

    let outcome = launch_sidebar_if_needed(&backend, &h.runtime, &h.sidebar_opts());

    assert_eq!(outcome, SidebarLaunchOutcome::Opened);
    assert_eq!(backend.open_calls(), 1);
}

#[test]
fn sidebar_launch_error_is_non_fatal() {
    let h = SidebarHarness::new();
    h.runtime.ensure_dirs().expect("runtime dirs");
    let backend = FakeBackend::failing();

    let outcome = launch_sidebar_if_needed(&backend, &h.runtime, &h.sidebar_opts());

    assert_eq!(outcome, SidebarLaunchOutcome::Failed);
    assert_eq!(backend.open_calls(), 1);
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
        SidebarPaneOptions {
            session_name: "session".to_owned(),
            workspace_id: self.workspace_id.clone(),
            cwd: self.cwd.clone(),
            width_percent: 30,
            rimz_bin: PathBuf::from("rimz"),
        }
    }

    fn write_heartbeat(&self) {
        self.runtime.ensure_dirs().expect("runtime dirs");
        let heartbeat = SidebarHeartbeat::new(
            self.workspace_id.clone(),
            SidebarInstanceId::new(),
            MuxName::Tmux,
            "session",
            self.runtime.sock_dir.join("sidebar.sock"),
        );
        assert_eq!(heartbeat.protocol_version, SIDEBAR_PROTOCOL_VERSION);
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
    fail_open: bool,
}

impl FakeBackend {
    fn failing() -> Self {
        Self {
            open_calls: Mutex::new(0),
            fail_open: true,
        }
    }

    fn open_calls(&self) -> usize {
        *self.open_calls.lock().expect("open calls")
    }
}

impl MuxBackend for FakeBackend {
    fn name(&self) -> MuxName {
        MuxName::Tmux
    }

    fn ensure_session(&self, _opts: &SessionOptions) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn attach_command(&self, name: &str) -> CommandSpec {
        CommandSpec::new("fake").arg(name)
    }

    fn detach(&self, _name: &str) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn list_sessions(&self) -> rimz::mux::Result<Vec<String>> {
        Ok(Vec::new())
    }

    fn list_panes(&self, _opts: PaneListOptions) -> rimz::mux::Result<Vec<PaneRef>> {
        Ok(vec![PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
            session_name: "session".to_owned(),
            view_id: Some("@1".to_owned()),
            view_kind: Some(ViewKind::Window),
            pane_process_start: None,
        }])
    }

    fn split_pane(&self, _opts: SplitPaneOptions) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn focus_pane(&self, _pane: &PaneId) -> rimz::mux::Result<()> {
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

    fn open_sidebar(&self, _opts: &SidebarPaneOptions) -> rimz::mux::Result<()> {
        *self.open_calls.lock().expect("open calls") += 1;
        if self.fail_open {
            return Err(MuxErr::Command {
                program: "fake".to_owned(),
                args: "open-sidebar".to_owned(),
                stderr: "boom".to_owned(),
            });
        }
        Ok(())
    }

    fn wake_sidebar(&self, _session_name: &str, _bytes: &[u8]) -> rimz::mux::Result<()> {
        Ok(())
    }

    fn version(&self) -> rimz::mux::Result<String> {
        Ok("fake 1.0.0".to_owned())
    }
}
