use std::sync::Mutex;

use super::*;

fn sidebar_opts(harness: &Harness) -> SidebarPaneOptions {
    let cwd = harness.path().to_path_buf();
    SidebarPaneOptions {
        runtime: harness.runtime.clone(),
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
    publish_before_sample: bool,
    fail_reconcile: bool,
    recovery: crate::mux::SidebarRecovery,
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
        opts: &SidebarPaneOptions,
        live: &dyn SidebarLivenessSource,
    ) -> crate::mux::Result<crate::mux::SidebarRecovery> {
        *self.reconcile_calls.lock().expect("reconcile calls") += 1;
        if self.fail_reconcile {
            return Err(crate::mux::MuxErr::Command {
                program: "fake".to_owned(),
                args: "reconcile-sidebars".to_owned(),
                stderr: "boom".to_owned(),
            });
        }
        if self.publish_before_sample {
            let pane = PaneId::from_parts(MuxName::Tmux, "%42");
            let mut heartbeat = SidebarHeartbeat::new(
                opts.workspace_id.clone(),
                SidebarInstanceId::new(),
                MuxName::Tmux,
                &opts.session_name,
                opts.runtime.sock_dir.join("sidebar.sock"),
                Some(pane.clone()),
            );
            heartbeat.build = Some(crate::build_id::of_file(&opts.rimz_bin).unwrap());
            opts.runtime.ensure_dirs().unwrap();
            std::fs::write(
                opts.runtime.sidebar_heartbeat_path(&heartbeat.instance_id),
                serde_json::to_vec(&heartbeat).unwrap(),
            )
            .unwrap();
            assert!(
                live.sample().claimed_panes.contains(&pane),
                "the source must see a heartbeat published after entering the backend"
            );
        }
        Ok(self.recovery.clone())
    }
}

#[test]
fn reconcile_samples_heartbeat_claims_inside_the_backend() {
    let h = Harness::new();
    let backend = FakeSidebarMux {
        publish_before_sample: true,
        ..Default::default()
    };
    reconcile_session(&backend, &sidebar_opts(&h), Duration::ZERO, || {
        panic!("an unchanged pass must not request a diagnostic sink")
    })
    .expect("reconcile")
    .expect("session lock available");
    assert_eq!(backend.reconcile_calls(), 1);
}

#[test]
fn reconcile_records_only_landed_view_repairs() {
    use crate::diag::DiagSink;
    use crate::diag::record::{DiagEnvelope, DiagSeverity};
    use crate::mux::{SidebarRecovery, SidebarViewRepair};
    use std::cell::Cell;

    let repaired = SidebarRecovery {
        failed: 1,
        repairs: vec![
            SidebarViewRepair {
                view: "@0".to_owned(),
                closed: vec![PaneId::from_parts(MuxName::Tmux, "%1")],
                added: Some(PaneId::from_parts(MuxName::Tmux, "%3")),
            },
            SidebarViewRepair {
                view: "@1".to_owned(),
                closed: vec![PaneId::from_parts(MuxName::Tmux, "%2")],
                added: None,
            },
        ],
        ..Default::default()
    };
    for recovery in [repaired, SidebarRecovery::default()] {
        for fail_reconcile in [false, true] {
            let h = Harness::new();
            let opts = sidebar_opts(&h);
            let backend = FakeSidebarMux {
                recovery: recovery.clone(),
                fail_reconcile,
                ..Default::default()
            };
            let sink = DiagSink::under(
                h.path().to_path_buf(),
                h.workspace_id.clone(),
                "session",
                None,
            );
            let log_path = sink.log_path().unwrap();
            let sink_requests = Cell::new(0);
            let result = reconcile_session(&backend, &opts, Duration::ZERO, || {
                sink_requests.set(sink_requests.get() + 1);
                sink
            });
            assert_eq!(backend.reconcile_calls(), 1);
            if fail_reconcile {
                assert!(matches!(result, Err(SidebarReconcileErr::Mux(_))));
            } else {
                assert_eq!(result.unwrap(), Some(recovery.clone()));
            }
            if fail_reconcile || recovery.repairs.is_empty() {
                assert!(
                    !log_path.exists(),
                    "an unchanged or failed pass must write nothing"
                );
                assert_eq!(sink_requests.get(), 0);
                continue;
            }
            assert!(
                log_path.exists(),
                "landed view repairs must leave diagnostic records"
            );
            assert_eq!(sink_requests.get(), 1);
            let records: Vec<DiagEnvelope> = std::fs::read_to_string(log_path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(records.len(), 2, "{records:?}");
            for record in &records {
                assert_eq!(record.v, "rimz.diag.v1");
                assert_eq!(record.workspace_id, h.workspace_id);
                assert_eq!(record.session_name, "session");
                assert_eq!(record.severity, DiagSeverity::Info);
            }
            assert_eq!(
                records
                    .iter()
                    .map(|record| serde_json::to_value(&record.event).unwrap())
                    .collect::<Vec<_>>(),
                [
                    serde_json::json!({
                        "kind": "sidebar_view_repaired", "view": "@0",
                        "closed": ["tmux:%1"], "added": "tmux:%3",
                    }),
                    serde_json::json!({
                        "kind": "sidebar_view_repaired", "view": "@1", "closed": ["tmux:%2"],
                    }),
                ]
            );
        }
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
fn fresh_sidebar_launch_skips_reconcile_while_session_lock_is_held() {
    let h = Harness::new();
    h.write_sidebar("sidebar.fresh.json", SIDEBAR_PROTOCOL_VERSION);
    let backend = FakeSidebarMux::default();
    let _guard = crate::disk::lock::WorkspaceLock::acquire(
        &h.runtime.sidebar_reconcile_lock(backend.name(), "session"),
    )
    .unwrap();

    let outcome = launch_sidebar(&backend, &h.runtime, &sidebar_opts(&h), None);

    assert_eq!(outcome, SidebarLaunchOutcome::SkippedFresh);
    assert_eq!(backend.open_calls(), 0);
    assert_eq!(backend.reconcile_calls(), 0, "the holder excludes attach");
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
