use super::rimz_api as rimz;
use rimz::RuntimePaths;
use rimz::ids::{MuxName, PaneId};
use rimz::mux as m;
use rimz::pane::PaneRef;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Default)]
pub(super) struct Backend {
    pub(super) tabs: Mutex<Vec<m::TabOptions>>,
    pub(super) listed: Option<Vec<PaneRef>>,
    pub(super) split_error: Mutex<Option<m::MuxErr>>,
    pub(super) tab_error: Mutex<Option<m::MuxErr>>,
    pub(super) appends: Mutex<Vec<m::SplitPaneOptions>>,
    pub(super) append_outcomes:
        Mutex<std::collections::VecDeque<Result<m::CompanionPaneAppend, m::CompanionAppendErr>>>,
    pub(super) captures: Mutex<Vec<(Option<u16>, bool)>>,
    pub(super) on_open: Option<Box<dyn Fn() + Send + Sync>>,
}

impl m::MuxBackend for Backend {
    fn view_geometry(
        &self,
        _session: &str,
        _anchor: &PaneId,
        _timeout: Duration,
    ) -> m::Result<Option<m::ViewGeometry>> {
        panic!("unexpected view_geometry")
    }

    fn name(&self) -> MuxName {
        MuxName::Tmux
    }
    fn list_panes(&self, opts: m::PaneListOptions) -> m::Result<m::PaneListing> {
        assert_eq!(opts.command_timeout, Some(Duration::from_millis(500)));
        assert_eq!(
            opts.consistency,
            m::PaneReadConsistency::RequireAuthoritative
        );
        if let Some(panes) = &self.listed {
            return Ok(m::PaneListing {
                authoritative: true,
                panes: panes.clone(),
                views: Vec::new(),
                observed_at_ms: 0,
                session_focus: None,
                client_view: None,
                client_sample_withheld: false,
            });
        }
        Err(m::MuxErr::Timeout {
            program: "fake".to_owned(),
            args: "list-panes".to_owned(),
            timeout: Duration::from_secs(1),
        })
    }
    fn split_pane(&self, _opts: m::SplitPaneOptions) -> m::Result<Option<PaneId>> {
        Err(self
            .split_error
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| m::MuxErr::Command {
                program: "fake".to_owned(),
                args: "split".to_owned(),
                stderr: "no room".to_owned(),
            }))
    }
    fn open_tab(&self, opts: &m::TabOptions) -> m::Result<()> {
        if let Some(on_open) = &self.on_open {
            on_open();
        }
        self.tabs.lock().unwrap().push(opts.clone());
        if let Some(error) = self.tab_error.lock().unwrap().take() {
            return Err(error);
        }
        Ok(())
    }
    fn ensure_session(&self, _opts: &m::SessionOptions) -> m::Result<()> {
        panic!("unexpected ensure_session")
    }
    fn attach_command(
        &self,
        _name: &str,
        _config: &rimz::config::MultiplexerConfig,
    ) -> m::CommandSpec {
        panic!("unexpected attach_command")
    }
    fn attach_existing_command(&self, _name: &str) -> m::CommandSpec {
        panic!("unexpected attach_existing_command")
    }
    fn attach_readonly_command(&self, _name: &str) -> m::CommandSpec {
        panic!("unexpected attach_readonly_command")
    }
    fn detach(&self, _name: &str) -> m::Result<()> {
        panic!("unexpected detach")
    }
    fn kill_session(&self, _name: &str) -> m::Result<()> {
        panic!("unexpected kill_session")
    }
    fn list_sessions_within(&self, _timeout: Duration) -> m::Result<Vec<String>> {
        panic!("unexpected list_sessions_within")
    }
    fn pane_content_size(
        &self,
        _pane: &PaneId,
        _session: Option<&str>,
        _timeout: Duration,
    ) -> m::Result<Option<m::PaneContentSize>> {
        panic!("unexpected pane_content_size")
    }
    fn append_companion_pane(
        &self,
        opts: m::SplitPaneOptions,
    ) -> Result<m::CompanionPaneAppend, m::CompanionAppendErr> {
        self.appends.lock().unwrap().push(opts);
        self.append_outcomes
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted append")
    }
    fn focus_pane(&self, _pane: &PaneId, _session: Option<&str>) -> m::Result<()> {
        panic!("unexpected focus_pane")
    }
    fn toggle_fullscreen(&self, _pane: &PaneId, _session: Option<&str>) -> m::Result<()> {
        panic!("unexpected toggle_fullscreen")
    }
    fn sidebar_width_step(
        &self,
        _runtime: &RuntimePaths,
        _session: &str,
        _pane: &PaneId,
        _min_observed_at_ms: Option<u64>,
    ) -> m::Result<m::WidthStep> {
        panic!("unexpected sidebar_width_step")
    }
    fn nudge_sidebar_width(
        &self,
        _session: &str,
        _pane: &PaneId,
        _current_cols: u16,
        _target_cols: u16,
    ) -> m::Result<()> {
        panic!("unexpected nudge_sidebar_width")
    }
    fn record_sidebar_width_default(&self, _session: &str, _cols: u16) -> m::Result<()> {
        panic!("unexpected record_sidebar_width_default")
    }
    fn capture_pane(
        &self,
        pane: &PaneId,
        _session: &str,
        lines: Option<u16>,
        ansi: bool,
    ) -> m::Result<m::PaneCapture> {
        self.captures.lock().unwrap().push((lines, ansi));
        let raw_text = if lines.is_some() {
            "earlier-line\nlast-line\n"
        } else {
            "last-line\n"
        };
        Ok(m::PaneCapture {
            pane_id: pane.clone(),
            raw_text: raw_text.to_owned(),
            lines: raw_text.lines().map(str::to_owned).collect(),
        })
    }
    fn send_keys(&self, _pane: &PaneId, _session: &str, _text: &str) -> m::Result<()> {
        panic!("unexpected send_keys")
    }
    fn send_key(
        &self,
        _pane: &PaneId,
        _session: &str,
        _key: rimz::pane::keys::NamedKey,
    ) -> m::Result<()> {
        panic!("unexpected send_key")
    }
    fn paste_text(&self, _pane: &PaneId, _session: &str, _text: &str) -> m::Result<()> {
        panic!("unexpected paste_text")
    }
    fn open_sidebar(
        &self,
        _opts: &m::SidebarPaneOptions,
        _daemon: Option<&m::DaemonView>,
    ) -> m::Result<()> {
        panic!("unexpected open_sidebar")
    }
    fn reconcile_sidebars(
        &self,
        _opts: &m::SidebarPaneOptions,
        _live: &dyn m::SidebarLivenessSource,
    ) -> m::Result<m::SidebarRecovery> {
        panic!("unexpected reconcile_sidebars")
    }
    fn open_background_view(
        &self,
        _opts: &m::BackgroundViewOptions,
    ) -> m::Result<m::BackgroundViewLaunch> {
        panic!("unexpected open_background_view")
    }
    fn confirm_resume_tabs(
        &self,
        _session: &str,
        _tabs: &[m::ResumeTab],
    ) -> Vec<std::result::Result<(), m::ResumeTabUnconfirmed>> {
        panic!("unexpected confirm_resume_tabs")
    }
    fn rename_tab(
        &self,
        _session: &str,
        _anchor: &PaneId,
        _name: &str,
        _intent: m::tab_name::TabNameIntent,
    ) -> m::Result<()> {
        panic!("unexpected rename_tab")
    }
    fn close_pane(&self, _session: &str, _pane: &PaneId) -> m::Result<()> {
        panic!("unexpected close_pane")
    }
    fn version(&self) -> m::Result<String> {
        panic!("unexpected version")
    }
}
