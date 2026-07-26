use std::sync::Mutex;

use super::*;

fn sidebar_opts(harness: &Harness) -> SidebarPaneOptions {
    let cwd = harness.path().to_path_buf();
    SidebarPaneOptions {
        session_name: "session".to_owned(),
        workspace_id: harness.workspace_id.clone(),
        project_root: cwd.clone(),
        extra_env: Default::default(),
        cwd,
        target: crate::mux::SidebarTarget {
            share: crate::mux::WidthPermille::from_percent(25),
            max_cols: std::num::NonZeroU16::new(72).expect("nonzero test width"),
            pinned: false,
        },
        detected_view_size: None,
        rimz_bin: std::env::current_exe().expect("test executable"),
        pristine_birth: false,
        config: crate::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    }
}

#[derive(Default)]
struct FakeSidebarMux {
    open_calls: Mutex<usize>,
    reconcile_calls: Mutex<usize>,
    fail_open: bool,
}

impl FakeSidebarMux {
    fn failing() -> Self {
        Self {
            fail_open: true,
            ..Self::default()
        }
    }

    fn open_calls(&self) -> usize {
        *self.open_calls.lock().expect("open calls")
    }

    fn reconcile_calls(&self) -> usize {
        *self.reconcile_calls.lock().expect("reconcile calls")
    }
}

impl SidebarMux for FakeSidebarMux {
    fn name(&self) -> MuxName {
        MuxName::Tmux
    }

    fn open_sidebar(
        &self,
        _opts: &SidebarPaneOptions,
        _daemon: Option<&DaemonView>,
    ) -> crate::mux::Result<()> {
        *self.open_calls.lock().expect("open calls") += 1;
        if self.fail_open {
            return Err(crate::mux::MuxErr::Command {
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
        _live: &SidebarLiveness,
    ) -> crate::mux::Result<crate::mux::SidebarRecovery> {
        *self.reconcile_calls.lock().expect("reconcile calls") += 1;
        Ok(crate::mux::SidebarRecovery::default())
    }
}

#[test]
fn sidebar_launch_skips_when_fresh_heartbeat_exists() {
    let h = Harness::new();
    h.write_sidebar("sidebar.fresh.json", SIDEBAR_PROTOCOL_VERSION);
    let backend = FakeSidebarMux::default();

    let outcome = launch_sidebar(&backend, &h.runtime, &sidebar_opts(&h), None);

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
    let h = Harness::new();
    h.ensure_runtime();
    let backend = FakeSidebarMux::default();

    let outcome = launch_sidebar(&backend, &h.runtime, &sidebar_opts(&h), None);

    assert_eq!(outcome, SidebarLaunchOutcome::Opened);
    assert_eq!(backend.open_calls(), 1);
    assert_eq!(backend.reconcile_calls(), 0);
}

#[test]
fn sidebar_launch_replaces_old_protocol_heartbeat() {
    let h = Harness::new();
    h.write_sidebar("sidebar.old.json", "rimz.plugin.v1");
    let backend = FakeSidebarMux::default();

    let outcome = launch_sidebar(&backend, &h.runtime, &sidebar_opts(&h), None);

    assert_eq!(outcome, SidebarLaunchOutcome::Opened);
    assert_eq!(backend.open_calls(), 1);
    assert_eq!(backend.reconcile_calls(), 0);
}

#[test]
fn sidebar_launch_after_rebirth_purge_ignores_fresh_heartbeat() {
    let h = Harness::new();
    let heartbeat = h.write_sidebar("sidebar.fresh.json", SIDEBAR_PROTOCOL_VERSION);
    let backend = FakeSidebarMux::default();

    purge_rebirth_heartbeats(&h.runtime);
    let outcome = launch_sidebar(&backend, &h.runtime, &sidebar_opts(&h), None);

    assert_eq!(outcome, SidebarLaunchOutcome::Opened);
    assert!(!heartbeat.exists(), "rebirth purges the stale incarnation");
    assert_eq!(backend.open_calls(), 1);
    assert_eq!(
        backend.reconcile_calls(),
        0,
        "purged heartbeat must not route through reconcile",
    );
}

#[test]
fn sidebar_launch_error_is_non_fatal() {
    let h = Harness::new();
    h.ensure_runtime();
    let backend = FakeSidebarMux::failing();

    let outcome = launch_sidebar(&backend, &h.runtime, &sidebar_opts(&h), None);

    assert_eq!(outcome, SidebarLaunchOutcome::Failed);
    assert_eq!(backend.open_calls(), 1);
    assert_eq!(backend.reconcile_calls(), 0);
}
