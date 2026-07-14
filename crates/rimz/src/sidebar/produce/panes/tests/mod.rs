use super::*;
use crate::sidebar::produce::test_support::pane;
use crate::sidebar::timing::SNAPSHOT_CACHE_TTL;
use crate::store::atomic;

mod cache;
mod fields;

fn frame(panes: Vec<crate::pane::PaneRef>) -> crate::sidebar::frame::PaneFrame {
    crate::sidebar::frame::assemble_frame(panes, 1, "s")
}

#[test]
fn resume_and_process_start_stamping_dispatch_for_kiro_commands() {
    let start: jiff::Timestamp = "2025-01-01T00:00:00Z".parse().unwrap();
    for command in ["kiro-cli chat --v3", "kiro-cli-chat"] {
        let mut pane = pane("terminal_1", Some(command), Some("/repo/main"));
        pane.pane_pid = Some(42);
        let mut frame = frame(vec![pane]);
        let unstamped = natively_unstamped(&frame);
        stamp_pane_resumed_session_ids(&mut frame, &|pid| {
            (pid == 42).then(|| {
                crate::ids::AgentSessionId::from("sess_11111111-1111-4111-8111-111111111111")
            })
        });
        stamp_pane_process_starts(
            &mut frame,
            &unstamped,
            &|kind, pid| {
                assert_eq!(kind, "kiro", "{command}");
                assert_eq!(pid, 42, "{command}");
                Some(start)
            },
            &|_, _| -> Vec<jiff::Timestamp> { panic!("root-pid derivation owns {command}") },
        );
        assert_eq!(
            first(&frame).current.resumed_session_id.as_deref(),
            Some("sess_11111111-1111-4111-8111-111111111111"),
            "{command}"
        );
        assert_eq!(first(&frame).current.started_at, Some(start), "{command}");
    }
}

fn first(frame: &crate::sidebar::frame::PaneFrame) -> &crate::sidebar::frame::PaneState {
    &frame.tabs[0].panes[0]
}

fn first_mut(
    frame: &mut crate::sidebar::frame::PaneFrame,
) -> &mut crate::sidebar::frame::PaneState {
    &mut frame.tabs[0].panes[0]
}

fn live_row_ids(frame: &crate::sidebar::frame::PaneFrame) -> Vec<String> {
    let workspace = crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/repo"));
    let snapshot = crate::SidebarSnapshot::build(workspace, Vec::new(), jiff::Timestamp::now())
        .with_live_panes(frame.to_pane_refs(), None);
    let mut ids = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter().map(|row| row.id.clone()))
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn write_snapshot_cache(path: &Path, session: &str, produced_at_ms: u64, carried: bool) {
    let mut cache = crate::sidebar::frame::assemble_frame(Vec::new(), produced_at_ms, session);
    if carried {
        cache.carried_panes = vec![crate::sidebar::frame::CarriedPane {
            pane_id: crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_9"),
            pid: Some(909),
            start_ticks: Some(90),
            carried_since_ms: produced_at_ms,
        }];
    }
    atomic::write_temp_then_rename(path, &cache).expect("write snapshot cache");
}

fn presence_sample(
    human_clients: usize,
    last_input_ms: Option<u64>,
    sampled_at_ms: u64,
) -> PresenceSample {
    PresenceSample {
        human_clients,
        last_input_ms,
        sampled_at_ms,
    }
}

fn frame_with_presence(presence: Option<PresenceSample>) -> crate::sidebar::frame::PaneFrame {
    let mut frame = frame(Vec::new());
    frame.presence = presence;
    frame
}

fn focused_pane_in_tab(id: &str, tab: &str, tab_name: &str) -> crate::pane::PaneRef {
    crate::pane::PaneRef {
        is_focused: true,
        view_id: Some(tab.to_owned()),
        view_name: Some(tab_name.to_owned()),
        ..pane(id, Some("zsh"), Some("/repo/main"))
    }
}

#[test]
fn multi_focus_topology_detects_multiple_focused_tiled_panes_per_tab() {
    let mut floating = focused_pane_in_tab("terminal_3", "tab_7", "work");
    floating.is_floating = true;
    let anomalies = multi_focus_topology_anomalies(&[
        focused_pane_in_tab("terminal_1", "tab_7", "work"),
        focused_pane_in_tab("terminal_2", "tab_7", "work"),
        floating,
        focused_pane_in_tab("terminal_4", "tab_8", "other"),
    ]);

    assert_eq!(
        anomalies,
        vec![AnomalyKind::MultiFocusTopology {
            tab_name: Some("work".to_owned()),
            tab_position: Some(7),
            pane_ids: vec![
                "zellij:terminal_1".to_owned(),
                "zellij:terminal_2".to_owned(),
            ],
        }],
    );
}

#[test]
fn presence_sample_due_requires_idle_capable_attached_stale_sample() {
    let now = 10_000;
    let ttl = crate::sidebar::timing::PRESENCE_SAMPLE_TTL.as_millis() as u64;
    let stale = now - ttl;
    let fresh = now - ttl + 1;

    assert!(presence_sample_due(
        &frame_with_presence(Some(presence_sample(1, Some(stale - 1), stale),)),
        None,
        now
    ));
    assert!(!presence_sample_due(&frame_with_presence(None), None, now));
    assert!(!presence_sample_due(
        &frame_with_presence(Some(presence_sample(0, Some(stale - 1), stale),)),
        None,
        now
    ));
    assert!(!presence_sample_due(
        &frame_with_presence(Some(presence_sample(1, None, stale),)),
        None,
        now
    ));
    assert!(!presence_sample_due(
        &frame_with_presence(Some(presence_sample(1, Some(fresh - 1), fresh),)),
        None,
        now
    ));
    assert!(presence_sample_due(
        &frame_with_presence(Some(presence_sample(1, Some(1), 1))),
        Some(stale),
        now,
    ));
    assert!(!presence_sample_due(
        &frame_with_presence(Some(presence_sample(1, Some(1), 1))),
        Some(fresh),
        now,
    ));
    assert!(!presence_sample_due(
        &frame_with_presence(Some(presence_sample(1, Some(1), 1))),
        Some(now + 1_000),
        now,
    ));
}

#[test]
fn failed_presence_attempt_stamp_suppresses_retries_until_ttl() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = crate::WorkspaceId::from_project_root(dir.path());
    let runtime = crate::RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let frame = frame_with_presence(Some(presence_sample(1, Some(1), 1)));
    let attempted_at_ms = 10_000;
    let ttl = crate::sidebar::timing::PRESENCE_SAMPLE_TTL.as_millis() as u64;

    assert!(presence_sample_due(&frame, None, attempted_at_ms));
    crate::sidebar::cache::write_presence_probe_stamp(&runtime, attempted_at_ms).unwrap();
    let probe_at_ms = crate::sidebar::cache::read_presence_probe_stamp(&runtime);
    assert!(!presence_sample_due(
        &frame,
        probe_at_ms,
        attempted_at_ms + ttl - 1,
    ));
    assert!(presence_sample_due(
        &frame,
        probe_at_ms,
        attempted_at_ms + ttl,
    ));
}

#[test]
fn presence_meaningfully_changed_ignores_sample_timestamp() {
    let prior = presence_sample(1, Some(1_000), 1_000);
    let restamped = presence_sample(1, Some(1_000), 2_000);

    assert!(presence_meaningfully_changed(None, &restamped));
    assert!(!presence_meaningfully_changed(Some(&prior), &restamped));
    assert!(presence_meaningfully_changed(
        Some(&prior),
        &presence_sample(1, Some(1_500), 2_000),
    ));
    assert!(presence_meaningfully_changed(
        Some(&prior),
        &presence_sample(2, Some(1_000), 2_000),
    ));
}

#[test]
fn unchanged_presence_updates_returned_sample_without_selecting_publish() {
    let prior = presence_sample(1, Some(1_000), 1_000);
    let mut frame = frame_with_presence(Some(prior));

    assert!(!apply_presence_sample(
        &mut frame,
        presence_sample(1, Some(1_000), 2_000),
    ));
    assert_eq!(frame.presence.unwrap().sampled_at_ms, 2_000);

    assert!(apply_presence_sample(
        &mut frame,
        presence_sample(2, Some(1_000), 3_000),
    ));
    assert!(apply_presence_sample(
        &mut frame,
        presence_sample(2, Some(1_500), 4_000),
    ));
}

#[test]
fn unchanged_presence_preserves_pane_frame_and_sends_no_wakeup() {
    use std::os::unix::net::UnixDatagram;

    let dir = tempfile::tempdir().unwrap();
    let workspace = crate::WorkspaceId::from_project_root(dir.path());
    let runtime = crate::RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let socket_path = runtime.sock_dir.join("sidebar.test.sock");
    let socket = UnixDatagram::bind(&socket_path).unwrap();
    socket.set_nonblocking(true).unwrap();
    let instance = crate::SidebarInstanceId::new();
    crate::sidebar::write_heartbeat(
        &runtime,
        workspace,
        &instance,
        crate::MuxName::Tmux,
        "rimz-test",
        &socket_path,
        None,
    )
    .unwrap();
    let cache_path = runtime.pane_frame_path();
    let mut frame = frame_with_presence(Some(presence_sample(1, Some(1_000), 1_000)));
    let produced_at_ms = frame.produced_at_ms;
    atomic::write_temp_then_rename_cache(&cache_path, &frame).unwrap();

    apply_presence_sample_and_publish(
        &mut frame,
        presence_sample(1, Some(1_000), 2_000),
        &runtime,
        &cache_path,
    );

    let published: PaneFrame =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    assert_eq!(published.presence.unwrap().sampled_at_ms, 1_000);
    assert_eq!(published.produced_at_ms, produced_at_ms);
    assert_eq!(frame.presence.unwrap().sampled_at_ms, 2_000);
    let mut buffer = [0; 256];
    assert_eq!(
        socket.recv(&mut buffer).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
    );

    apply_presence_sample_and_publish(
        &mut frame,
        presence_sample(2, Some(1_000), 3_000),
        &runtime,
        &cache_path,
    );
    let published: PaneFrame =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    assert_eq!(published.presence.unwrap().human_clients, 2);
    assert_eq!(published.produced_at_ms, produced_at_ms);
    assert!(socket.recv(&mut buffer).unwrap() > 0);
}
