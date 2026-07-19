use super::*;
use crate::ids::MuxName;

fn writer(plugin_id: u32, loaded_at_ms: u64) -> TopologyWriter {
    TopologyWriter {
        plugin_id,
        loaded_at_ms,
        build: None,
        config: None,
    }
}

fn identified_writer(
    plugin_id: u32,
    loaded_at_ms: u64,
    build: &str,
    config: &str,
) -> TopologyWriter {
    TopologyWriter {
        plugin_id,
        loaded_at_ms,
        build: Some(build.to_owned()),
        config: Some(config.to_owned()),
    }
}

fn desired() -> PresenceDesired {
    PresenceDesired {
        build: "desired-build".to_owned(),
        config: "desired-config".to_owned(),
        recorded_at_ms: 1,
    }
}

fn topology(produced_at_ms: u64, writer: Option<TopologyWriter>) -> PaneTopologyCache {
    PaneTopologyCache {
        session_name: "rimz-test".to_owned(),
        produced_at_ms,
        writer,
        focused_pane: None,
        clients: None,
        panes: Vec::new(),
    }
}

fn topology_pane(
    id: u64,
    tab_position: u64,
    title: &str,
) -> crate::mux::zellij::pane_topology::PaneTopologyPane {
    crate::mux::zellij::pane_topology::PaneTopologyPane {
        id,
        is_plugin: false,
        is_held: false,
        exited: false,
        is_suppressed: false,
        is_floating: false,
        tab_position,
        tab_name: None,
        pane_columns: None,
        pane_x: None,
        title: Some(title.to_owned()),
        pane_command: None,
        pane_cwd: None,
        pane_pid: None,
        terminal_command: None,
    }
}

fn wake(reason: ZellijWakeReason) -> ZellijWake {
    ZellijWake {
        reason,
        session_name: Some("rimz-test".to_owned()),
        pane_id: None,
        active_tab: None,
        focus_generation: None,
        focus_clients: Vec::new(),
        topology: None,
        telemetry: None,
    }
}

fn zellij_pane(raw: &str) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, raw)
}

fn paths() -> (tempfile::TempDir, StatePaths, RuntimePaths) {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = crate::WorkspaceId::from_project_root(dir.path());
    let state = StatePaths::under(workspace.clone(), dir.path()).expect("state paths");
    let runtime = RuntimePaths::under(workspace, dir.path()).expect("runtime paths");
    state.ensure_dirs().expect("state dirs");
    runtime.ensure_dirs().expect("runtime dirs");
    (dir, state, runtime)
}

#[test]
fn generation_classification_fences_only_fresh_older_writers() {
    let now_ms = crate::sidebar::timing::PRESENCE_STAMP_FRESH.as_millis() as u64 + 100_000;
    let fresh = topology(now_ms, Some(writer(2, 200)));
    let stale = topology(0, Some(writer(2, 200)));
    let older = topology(now_ms, Some(writer(1, 100)));
    let newer = topology(now_ms, Some(writer(3, 300)));

    assert_eq!(
        topology_decision(None, &older, None, now_ms),
        TopologyDecision::Accept
    );
    assert_eq!(
        topology_decision(Some(&fresh), &older, None, now_ms),
        TopologyDecision::Reject,
    );
    assert_eq!(
        topology_decision(Some(&fresh), &newer, None, now_ms),
        TopologyDecision::Accept,
    );
    assert_eq!(
        topology_decision(Some(&stale), &older, None, now_ms),
        TopologyDecision::Accept,
    );
    let legacy = topology(now_ms, None);
    assert_eq!(
        topology_decision(Some(&legacy), &legacy, None, now_ms),
        TopologyDecision::Accept,
    );
}

#[test]
fn desired_identity_outranks_later_nonmatching_writers() {
    let now_ms = 100_000;
    let desired = desired();
    let accepted = topology(
        now_ms,
        Some(identified_writer(1, 100, &desired.build, &desired.config)),
    );
    let later_other = topology(
        now_ms,
        Some(identified_writer(2, 200, "other-build", "other-config")),
    );

    assert_eq!(
        topology_decision(Some(&accepted), &later_other, Some(&desired), now_ms),
        TopologyDecision::Reject,
    );
    assert_eq!(
        topology_decision(Some(&later_other), &later_other, Some(&desired), now_ms),
        TopologyDecision::Accept,
        "a sole nonmatching writer keeps refreshing its cache",
    );
    assert_eq!(
        topology_decision(Some(&later_other), &accepted, Some(&desired), now_ms),
        TopologyDecision::Accept,
    );
}

#[test]
fn desired_record_fences_a_later_nonmatching_wake() {
    let (_dir, state, runtime) = paths();
    let desired = desired();
    crate::sidebar::cache::write_presence_desired(&runtime, &desired).unwrap();
    let produced_at_ms = unix_now_ms();
    let accepted = topology(
        produced_at_ms,
        Some(identified_writer(1, 100, &desired.build, &desired.config)),
    );
    write_pane_topology_cache(&runtime, &accepted).unwrap();
    let mut incoming = wake(ZellijWakeReason::Alive);
    incoming.topology = Some(topology(
        produced_at_ms,
        Some(identified_writer(2, 200, "other-build", "other-config")),
    ));

    assert_eq!(
        ingest_zellij_wake(&state, &runtime, &incoming).unwrap(),
        ZellijWakeOutcome::RejectedStaleWriter,
    );
    assert_eq!(
        read_pane_topology_cache(&runtime, "rimz-test"),
        Some(accepted),
    );
}

#[test]
fn writer_conflict_count_restarts_when_the_incident_changes() {
    let (_dir, state, runtime) = paths();
    write_topology_writer_conflict(
        &runtime,
        &TopologyWriterConflict {
            stale_writer: Some(writer(1, 100)),
            accepted_writer: Some(writer(2, 200)),
            rejected_count: 7,
            last_ms: 800,
            last_diag_ms: 500,
        },
    )
    .expect("seed writer conflict");
    let incoming = topology(900, Some(writer(3, 300)));
    let existing = topology(900, Some(writer(4, 400)));

    record_topology_write_rejected(&state, &runtime, &incoming, &existing, 1_000).unwrap();
    let conflict = read_topology_writer_conflict(&runtime).expect("updated conflict");
    assert_eq!(conflict.rejected_count, 1);
    assert_eq!(
        conflict.last_diag_ms, 500,
        "diagnostic throttle spans incidents"
    );

    record_topology_write_rejected(&state, &runtime, &incoming, &existing, 1_001).unwrap();
    assert_eq!(
        read_topology_writer_conflict(&runtime)
            .expect("updated conflict")
            .rejected_count,
        2,
    );
}

#[test]
fn newer_accepted_writer_clears_a_superseded_conflict() {
    let (_dir, state, runtime) = paths();
    let produced_at_ms = unix_now_ms();
    let existing = topology(produced_at_ms, Some(writer(2, 200)));
    write_pane_topology_cache(&runtime, &existing).expect("seed topology cache");
    write_topology_writer_conflict(
        &runtime,
        &TopologyWriterConflict {
            stale_writer: Some(writer(1, 100)),
            accepted_writer: existing.writer,
            rejected_count: 3,
            last_ms: produced_at_ms,
            last_diag_ms: produced_at_ms,
        },
    )
    .expect("seed writer conflict");
    let mut accepted = wake(ZellijWakeReason::Alive);
    accepted.topology = Some(topology(produced_at_ms, Some(writer(3, 300))));

    assert_eq!(
        ingest_zellij_wake(&state, &runtime, &accepted).unwrap(),
        ZellijWakeOutcome::Accepted(Vec::new()),
    );
    assert!(read_topology_writer_conflict(&runtime).is_none());
}

#[test]
fn equal_or_older_writer_keeps_the_conflict_sidecar() {
    let (_dir, _state, runtime) = paths();
    let conflict = TopologyWriterConflict {
        stale_writer: Some(writer(1, 100)),
        accepted_writer: Some(writer(2, 200)),
        rejected_count: 3,
        last_ms: 300,
        last_diag_ms: 300,
    };
    write_topology_writer_conflict(&runtime, &conflict).expect("seed writer conflict");

    clear_superseded_conflict(&runtime, Some(&writer(2, 200)), None).unwrap();
    assert!(read_topology_writer_conflict(&runtime).is_some());
    clear_superseded_conflict(&runtime, Some(&writer(9, 100)), None).unwrap();
    assert!(read_topology_writer_conflict(&runtime).is_some());
}

#[test]
fn desired_writer_clears_a_newer_nonmatching_conflict() {
    let (_dir, _state, runtime) = paths();
    let desired = desired();
    write_topology_writer_conflict(
        &runtime,
        &TopologyWriterConflict {
            stale_writer: None,
            accepted_writer: Some(identified_writer(2, 200, "other-build", "other-config")),
            rejected_count: 3,
            last_ms: 300,
            last_diag_ms: 300,
        },
    )
    .expect("seed writer conflict");
    let matching = identified_writer(1, 100, &desired.build, &desired.config);

    clear_superseded_conflict(&runtime, Some(&matching), Some(&desired)).unwrap();

    assert!(read_topology_writer_conflict(&runtime).is_none());
}

#[test]
fn sparse_requests_keep_their_fallbacks() {
    let (_dir, state, runtime) = paths();
    assert_eq!(
        ingest_zellij_wake(&state, &runtime, &wake(ZellijWakeReason::Announced)).unwrap(),
        ZellijWakeOutcome::Accepted(vec![SidebarEvent::PanesChanged]),
    );
    assert_eq!(
        ingest_zellij_wake(&state, &runtime, &wake(ZellijWakeReason::Alive)).unwrap(),
        ZellijWakeOutcome::Accepted(Vec::new()),
    );

    let mut stranded = wake(ZellijWakeReason::FocusStranded);
    stranded.pane_id = Some(zellij_pane("terminal_7"));
    stranded.focus_generation = Some(8);
    assert_eq!(
        ingest_zellij_wake(&state, &runtime, &stranded).unwrap(),
        ZellijWakeOutcome::Accepted(vec![SidebarEvent::FocusStranded {
            pane_id: zellij_pane("terminal_7"),
            generation: 8,
            clients: Vec::new(),
        }]),
    );
}

#[test]
fn settled_switch_classifies_against_the_accepted_topology() {
    let (_dir, state, runtime) = paths();
    let mut incoming = wake(ZellijWakeReason::SwitchSettled);
    incoming.active_tab = Some(1);
    incoming.focus_generation = Some(8);
    incoming.focus_clients = vec![crate::mux::ClientPaneView {
        client_id: crate::mux::MuxClientId::Zellij(1),
        pane_id: zellij_pane("terminal_10"),
    }];
    let mut accepted = topology(unix_now_ms(), Some(writer(2, 200)));
    accepted.panes = vec![
        topology_pane(10, 1, crate::pane::SIDEBAR_CHROME_TITLE),
        topology_pane(11, 1, "work"),
    ];
    incoming.topology = Some(accepted);

    assert_eq!(
        ingest_zellij_wake(&state, &runtime, &incoming).unwrap(),
        ZellijWakeOutcome::Accepted(vec![SidebarEvent::FocusStranded {
            pane_id: zellij_pane("terminal_10"),
            generation: 8,
            clients: incoming.focus_clients,
        }]),
    );
}

#[test]
fn launch_chrome_is_agents_launch_not_agents_subcommand() {
    assert!(command_is_launch_chrome(
        "rimz agents claude,codex --worktree=quality-pass"
    ));
    assert!(command_is_launch_chrome(
        "/home/me/.cargo/bin/rimz agents claude --worktree"
    ));
    for command in [
        "cargo build",
        "rimz agents exec codex",
        "rimz agents wait swift-otter",
        "rimz agents list",
        "rimz agents ls",
        "rimz agents show swift-otter",
        "rimz agents focus swift-otter",
        "rimz agents stop swift-otter",
    ] {
        assert!(!command_is_launch_chrome(command), "{command}");
    }
}

#[test]
fn one_snapshot_can_open_multiple_card_panes() {
    let current_writer = writer(1, 100);
    let mut existing = topology(1, Some(current_writer.clone()));
    existing.panes = vec![topology_pane(1, 1, "work")];
    let mut working = topology_pane(2, 1, "work");
    working.pane_command = Some("codex --search".to_owned());
    let mut launch = topology_pane(3, 1, "work");
    launch.terminal_command = Some("rimz agents claude,codex".to_owned());
    let mut incoming = topology(2, Some(current_writer));
    incoming.panes = vec![existing.panes[0].clone(), working, launch];

    assert_eq!(
        project_presence(derive_zellij_transitions(Some(&existing), &incoming, true,)),
        vec![
            SidebarEvent::PaneOpened {
                pane_id: zellij_pane("terminal_2"),
                command: Some("codex --search".to_owned()),
            },
            SidebarEvent::PaneOpened {
                pane_id: zellij_pane("terminal_3"),
                command: None,
            },
        ],
    );
    let sidebar_events = zellij_event_eligibility(PresencePaneRole::Sidebar);
    assert!(
        !sidebar_events.open
            && sidebar_events.close
            && sidebar_events.command
            && sidebar_events.direct_focus
    );
}

#[test]
fn announced_snapshot_is_sanitized_before_diff_and_persist() {
    let (_dir, state, runtime) = paths();
    let current_writer = writer(2, 200);
    let mut existing = topology(unix_now_ms(), Some(current_writer.clone()));
    existing
        .panes
        .push(crate::mux::zellij::pane_topology::PaneTopologyPane {
            id: 7,
            is_plugin: false,
            is_held: false,
            exited: false,
            is_suppressed: false,
            is_floating: false,
            tab_position: 1,
            tab_name: None,
            pane_columns: None,
            pane_x: None,
            title: Some("work".to_owned()),
            pane_command: Some("cargo build".to_owned()),
            pane_cwd: None,
            pane_pid: None,
            terminal_command: None,
        });
    write_pane_topology_cache(&runtime, &existing).unwrap();
    let mut incoming = existing.clone();
    incoming.produced_at_ms += 1;
    incoming.panes[0].pane_command =
        Some("rimz agents claude,codex --worktree=quality-pass".to_owned());
    let mut announced = wake(ZellijWakeReason::Announced);
    announced.topology = Some(incoming);

    assert_eq!(
        ingest_zellij_wake(&state, &runtime, &announced).unwrap(),
        ZellijWakeOutcome::Accepted(vec![SidebarEvent::PanesChanged]),
    );
    assert_eq!(
        read_pane_topology_cache(&runtime, "rimz-test")
            .unwrap()
            .panes[0]
            .pane_command,
        None,
    );
}

#[test]
fn topology_write_failure_returns_error_before_accepted_side_effects() {
    let (_dir, state, runtime) = paths();
    std::fs::create_dir_all(crate::sidebar::cache::pane_topology_cache_path(&runtime)).unwrap();
    let mut incoming = wake(ZellijWakeReason::Alive);
    incoming.topology = Some(topology(unix_now_ms(), Some(writer(2, 200))));
    incoming.telemetry = Some(ZellijPluginTelemetry {
        plugin_id: Some(2),
        build: Some("wasm-build".to_owned()),
        loaded_at_ms: 200,
        pages: 1,
        uptime_ms: 1,
        commands: 1,
        commands_succeeded: Some(1),
        stale_writer_rejections: Some(0),
        topology_failures: Some(0),
        other_failures: Some(0),
        zellij_version: Some("0.44.3".to_owned()),
        last_failure: None,
    });

    assert!(matches!(
        ingest_zellij_wake(&state, &runtime, &incoming),
        Err(ZellijWakeError::TopologyWrite(_))
    ));
    assert!(!crate::sidebar::cache::presence_stamp_path(&runtime).exists());
    assert!(
        !crate::diag::plugin_presence::log(&state.root)
            .path()
            .exists()
    );
}

#[test]
fn topology_writer_lock_contention_returns_typed_timeout() {
    let (_dir, state, runtime) = paths();
    let _held =
        crate::store::lock::WorkspaceLock::acquire(&runtime.topology_writer_lock()).unwrap();
    let mut incoming = wake(ZellijWakeReason::Alive);
    incoming.topology = Some(topology(unix_now_ms(), Some(writer(2, 200))));

    assert!(matches!(
        ingest_zellij_wake(&state, &runtime, &incoming),
        Err(ZellijWakeError::TopologyLock(
            crate::store::lock::LockErr::Timeout { .. }
        ))
    ));
}

#[test]
fn concurrent_topology_writers_finish_on_newest_generation() {
    for round in 0..16 {
        let (_dir, state, runtime) = paths();
        let at_ms = unix_now_ms();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let run = |plugin_id, loaded_at_ms| {
            let state = state.clone();
            let runtime = runtime.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut incoming = wake(ZellijWakeReason::Alive);
                incoming.topology = Some(topology(
                    at_ms,
                    Some(writer(plugin_id, loaded_at_ms + round)),
                ));
                barrier.wait();
                ingest_zellij_wake(&state, &runtime, &incoming)
            })
        };
        let older = run(1, 100);
        let newer = run(2, 200);
        barrier.wait();
        older.join().unwrap().unwrap();
        newer.join().unwrap().unwrap();

        assert_eq!(
            read_pane_topology_cache(&runtime, "rimz-test")
                .unwrap()
                .writer,
            Some(writer(2, 200 + round))
        );
    }
}
