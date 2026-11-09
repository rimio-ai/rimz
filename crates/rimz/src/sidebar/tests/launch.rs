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
        max_percent: 50,
        detected_view_size: None,
        rimz_bin: std::env::current_exe().expect("test executable"),
        pristine_birth: false,
        config: crate::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    }
}

fn pristine_opts(harness: &Harness) -> SidebarPaneOptions {
    SidebarPaneOptions {
        pristine_birth: true,
        ..sidebar_opts(harness)
    }
}

#[derive(Default)]
struct FakeSidebarMux {
    open_calls: Mutex<usize>,
    reconcile_scopes: Mutex<Vec<ReconcileScope>>,
    heal_calls: Mutex<usize>,
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
        self.reconcile_scopes().len()
    }

    fn reconcile_scopes(&self) -> Vec<ReconcileScope> {
        self.reconcile_scopes
            .lock()
            .expect("reconcile scopes")
            .clone()
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
        scope: ReconcileScope,
    ) -> crate::mux::Result<crate::mux::SidebarRecovery> {
        self.reconcile_scopes
            .lock()
            .expect("reconcile scopes")
            .push(scope);
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

    fn close_duplicate_sidebars(
        &self,
        _session_name: &str,
        _workspace_id: &crate::ids::WorkspaceId,
        _live: &dyn SidebarLivenessSource,
    ) -> crate::mux::Result<crate::mux::SidebarRecovery> {
        *self.heal_calls.lock().expect("heal calls") += 1;
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
    reconcile_session(
        &backend,
        &sidebar_opts(&h),
        ReconcileScope::Full,
        Duration::ZERO,
        || panic!("an unchanged pass must not request a diagnostic sink"),
    )
    .expect("reconcile")
    .expect("session lock available");
    assert_eq!(backend.reconcile_calls(), 1);
}

#[test]
fn reconcile_records_landed_repairs_and_failed_operations() {
    use crate::diag::DiagSink;
    use crate::diag::record::{DiagEnvelope, DiagSeverity, SidebarRepairStep};
    use crate::mux::{SidebarRecovery, SidebarRepairFailure, SidebarViewRepair};
    use std::cell::Cell;

    let repaired = SidebarRecovery {
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
    let failed = SidebarRecovery {
        failed: 1,
        failures: vec![
            SidebarRepairFailure {
                view: "@2".to_owned(),
                step: SidebarRepairStep::Add,
                pane: Some(PaneId::from_parts(MuxName::Tmux, "%88")),
                error: "unproven".to_owned(),
            },
            SidebarRepairFailure {
                view: "@2".to_owned(),
                step: SidebarRepairStep::CleanupKill,
                pane: Some(PaneId::from_parts(MuxName::Tmux, "%88")),
                error: "kill failed".to_owned(),
            },
        ],
        ..Default::default()
    };
    let repaired_wire = [
        serde_json::json!({
            "kind": "sidebar_view_repaired", "view": "@0",
            "closed": ["tmux:%1"], "added": "tmux:%3",
        }),
        serde_json::json!({
            "kind": "sidebar_view_repaired", "view": "@1", "closed": ["tmux:%2"],
        }),
    ];
    let failed_wire = [
        serde_json::json!({
            "kind": "sidebar_view_repair_failed", "view": "@2", "step": "add",
            "pane": "tmux:%88", "error": "unproven",
        }),
        serde_json::json!({
            "kind": "sidebar_view_repair_failed", "view": "@2", "step": "cleanup_kill",
            "pane": "tmux:%88", "error": "kill failed",
        }),
    ];
    let cases = [
        (repaired, &repaired_wire[..], DiagSeverity::Info),
        (failed, &failed_wire[..], DiagSeverity::Warn),
        (SidebarRecovery::default(), &[][..], DiagSeverity::Info),
    ];
    for (recovery, expected, severity) in cases {
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
            let result = reconcile_session(
                &backend,
                &opts,
                ReconcileScope::Full,
                Duration::ZERO,
                || {
                    sink_requests.set(sink_requests.get() + 1);
                    sink
                },
            );
            assert_eq!(backend.reconcile_calls(), 1);
            if fail_reconcile {
                assert!(matches!(result, Err(SidebarReconcileErr::Mux(_))));
            } else {
                assert_eq!(result.unwrap(), Some(recovery.clone()));
            }
            if fail_reconcile || expected.is_empty() {
                assert!(
                    !log_path.exists(),
                    "an unchanged pass or an outer error must write nothing"
                );
                assert_eq!(sink_requests.get(), 0);
                continue;
            }
            assert_eq!(sink_requests.get(), 1);
            let records: Vec<DiagEnvelope> = std::fs::read_to_string(log_path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            for record in &records {
                assert_eq!(record.v, "rimz.diag.v1");
                assert_eq!(record.workspace_id, h.workspace_id);
                assert_eq!(record.session_name, "session");
                assert_eq!(record.severity, severity);
            }
            assert_eq!(
                records
                    .iter()
                    .map(|record| serde_json::to_value(&record.event).unwrap())
                    .collect::<Vec<_>>(),
                expected
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
        backend.reconcile_scopes(),
        [ReconcileScope::Full],
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
fn heal_skips_a_held_session_lock_without_waiting() {
    let h = Harness::new();
    h.ensure_runtime();
    let backend = FakeSidebarMux {
        recovery: crate::mux::SidebarRecovery {
            closed: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let heal = || {
        heal_session(
            &backend,
            &h.runtime,
            "session",
            &h.workspace_id,
            &crate::mux::SidebarLiveness::default(),
            || panic!("a report without repairs or failures requests no sink"),
        )
        .expect("heal pass")
    };
    let guard = crate::disk::lock::WorkspaceLock::acquire(
        &h.runtime.sidebar_reconcile_lock(backend.name(), "session"),
    )
    .unwrap();
    let started = std::time::Instant::now();
    assert_eq!(heal(), None, "a held lock skips the pass");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "and never waits"
    );
    assert_eq!(*backend.heal_calls.lock().unwrap(), 0);

    drop(guard);
    assert_eq!(heal().map(|report| report.closed), Some(1));
    assert_eq!(*backend.heal_calls.lock().unwrap(), 1);
}

#[test]
fn pristine_birth_without_heartbeat_opens_once_without_reconcile() {
    let h = Harness::new();
    h.ensure_runtime();
    let backend = FakeSidebarMux::default();

    let outcome = launch_sidebar(&backend, &h.runtime, &pristine_opts(&h), None);

    assert_eq!(outcome, SidebarLaunchOutcome::Opened);
    assert_eq!(backend.open_calls(), 1);
    assert_eq!(backend.reconcile_calls(), 0);
}

#[test]
fn existing_session_without_heartbeat_only_adds_missing_sidebars() {
    let h = Harness::new();
    h.ensure_runtime();
    let backend = FakeSidebarMux::default();

    let outcome = launch_sidebar(&backend, &h.runtime, &sidebar_opts(&h), None);

    assert_eq!(outcome, SidebarLaunchOutcome::Unchanged);
    assert_eq!(
        backend.open_calls(),
        0,
        "a live session never splits blindly"
    );
    assert_eq!(backend.reconcile_scopes(), [ReconcileScope::AddMissing]);
}

#[test]
fn existing_session_add_opens_only_when_the_pass_defers_an_add() {
    let cases = [
        (
            crate::mux::SidebarRecovery {
                recovered: 1,
                ..Default::default()
            },
            SidebarLaunchOutcome::Opened,
            0,
        ),
        (
            crate::mux::SidebarRecovery {
                deferred: 1,
                deferred_adds: 1,
                ..Default::default()
            },
            SidebarLaunchOutcome::Opened,
            1,
        ),
        (
            crate::mux::SidebarRecovery {
                deferred: 1,
                ..Default::default()
            },
            SidebarLaunchOutcome::Unchanged,
            0,
        ),
    ];
    for (recovery, expected, opens) in cases {
        let h = Harness::new();
        h.ensure_runtime();
        let backend = FakeSidebarMux {
            recovery,
            ..Default::default()
        };

        let outcome = launch_sidebar(&backend, &h.runtime, &sidebar_opts(&h), None);

        assert_eq!(outcome, expected);
        assert_eq!(backend.open_calls(), opens);
        assert_eq!(backend.reconcile_scopes(), [ReconcileScope::AddMissing]);
    }
}

#[test]
fn existing_session_with_old_protocol_heartbeat_does_not_open() {
    let h = Harness::new();
    h.write_sidebar("sidebar.old.json", "rimz.plugin.v1");
    let backend = FakeSidebarMux::default();

    let outcome = launch_sidebar(&backend, &h.runtime, &sidebar_opts(&h), None);

    assert_eq!(outcome, SidebarLaunchOutcome::Unchanged);
    assert_eq!(backend.open_calls(), 0);
    assert_eq!(backend.reconcile_scopes(), [ReconcileScope::AddMissing]);
}

#[test]
fn existing_session_reconcile_error_fails_without_opening() {
    let h = Harness::new();
    h.ensure_runtime();
    let backend = FakeSidebarMux {
        fail_reconcile: true,
        ..Default::default()
    };

    let outcome = launch_sidebar(&backend, &h.runtime, &sidebar_opts(&h), None);

    assert_eq!(outcome, SidebarLaunchOutcome::Failed);
    assert_eq!(backend.open_calls(), 0);
    assert_eq!(backend.reconcile_calls(), 1);
}

#[test]
fn existing_session_launch_opens_nothing_while_session_lock_is_held() {
    let h = Harness::new();
    h.ensure_runtime();
    let backend = FakeSidebarMux::default();
    let _guard = crate::disk::lock::WorkspaceLock::acquire(
        &h.runtime.sidebar_reconcile_lock(backend.name(), "session"),
    )
    .unwrap();

    let outcome = launch_sidebar(&backend, &h.runtime, &sidebar_opts(&h), None);

    assert_eq!(outcome, SidebarLaunchOutcome::Unchanged);
    assert_eq!(backend.open_calls(), 0);
    assert_eq!(backend.reconcile_calls(), 0, "the holder excludes launch");
}

#[test]
fn sidebar_launch_after_rebirth_purge_ignores_fresh_heartbeat() {
    let h = Harness::new();
    let heartbeat = h.write_sidebar("sidebar.fresh.json", SIDEBAR_PROTOCOL_VERSION);
    let backend = FakeSidebarMux::default();

    purge_rebirth_heartbeats(&h.runtime);
    let outcome = launch_sidebar(&backend, &h.runtime, &pristine_opts(&h), None);

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

    let outcome = launch_sidebar(&backend, &h.runtime, &pristine_opts(&h), None);

    assert_eq!(outcome, SidebarLaunchOutcome::Failed);
    assert_eq!(backend.open_calls(), 1);
    assert_eq!(backend.reconcile_calls(), 0);
}
