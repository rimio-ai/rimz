use super::*;
use crate::disk::atomic;
use crate::sidebar::produce::test_support::pane;
use crate::sidebar::timing::SNAPSHOT_CACHE_TTL;

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
    let snapshot = crate::store::snapshot::SidebarSnapshot::build(
        workspace,
        Vec::new(),
        jiff::Timestamp::now(),
    )
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

#[test]
fn pane_drop_evidence_distinguishes_plain_managed_partial_and_mass_loss() {
    let in_tab = |id: &str, tab: &str, command: &str| crate::pane::PaneRef {
        view_id: Some(tab.to_owned()),
        ..pane(id, Some(command), Some("/repo/main"))
    };
    let prior = frame(vec![
        in_tab("terminal_1", "tab_1", "zsh"),
        in_tab("terminal_2", "tab_2", "zsh"),
    ]);
    let fresh = frame(vec![in_tab("terminal_2", "tab_2", "zsh")]);
    let removed = vec![crate::PaneId::from_parts(
        crate::MuxName::Zellij,
        "terminal_1",
    )];
    let evidence = pane_drop_evidence(&prior, &fresh, &removed);
    assert!(evidence.affected_views[0].removed_completely());
    assert!(evidence.affected_views[0].managed_panes.is_empty());
    assert!(!evidence.mass_shrink);

    let managed_prior = frame(vec![
        in_tab("terminal_1", "tab_1", "codex"),
        in_tab("terminal_2", "tab_2", "zsh"),
    ]);
    let managed = pane_drop_evidence(&managed_prior, &fresh, &removed);
    assert_eq!(
        managed.affected_views[0].managed_panes[0].agent_kind,
        "codex"
    );

    let partial_prior = frame(vec![
        in_tab("terminal_1", "tab_1", "zsh"),
        in_tab("terminal_3", "tab_1", "zsh"),
    ]);
    let partial_fresh = frame(vec![in_tab("terminal_3", "tab_1", "zsh")]);
    let partial = pane_drop_evidence(&partial_prior, &partial_fresh, &removed);
    assert!(!partial.affected_views[0].removed_completely());

    let mass_prior = frame(vec![
        in_tab("terminal_1", "tab_1", "zsh"),
        in_tab("terminal_2", "tab_2", "zsh"),
        in_tab("terminal_3", "tab_3", "zsh"),
    ]);
    let mass_fresh = frame(vec![in_tab("terminal_3", "tab_3", "zsh")]);
    let mass = pane_drop_evidence(
        &mass_prior,
        &mass_fresh,
        &[
            crate::PaneId::from_parts(crate::MuxName::Zellij, "terminal_1"),
            crate::PaneId::from_parts(crate::MuxName::Zellij, "terminal_2"),
        ],
    );
    assert!(mass.mass_shrink);
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
    let runtime = crate::RuntimePaths::under(
        crate::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let mut frame = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", None, Some("/repo"))],
        unix_now_ms() + 60_000,
        "s",
    );
    frame.presence = Some(presence_sample(1, Some(1), 1));
    let mut producer = PaneProducer {
        prior: Some(Arc::new(frame)),
        metrics: Default::default(),
        presence_probed_at_ms: None,
    };
    let calls = std::cell::Cell::new(0);
    for _ in 0..2 {
        producer
            .tick_with(
                &runtime,
                MuxName::Tmux,
                "s",
                None,
                None,
                &crate::diag::DiagSink::disabled(),
                &mut |_, _| panic!("fresh prior must not list panes"),
                &|_, _| {
                    calls.set(calls.get() + 1);
                    None
                },
            )
            .unwrap();
    }
    assert_eq!(calls.get(), 1, "failed presence sampling is throttled");
    let attempted_at_ms = producer.presence_probed_at_ms.unwrap();
    let ttl = crate::sidebar::timing::PRESENCE_SAMPLE_TTL.as_millis() as u64;
    let frame = producer.prior.as_ref().unwrap();
    assert!(!presence_sample_due(
        frame,
        producer.presence_probed_at_ms,
        attempted_at_ms + ttl - 1
    ));
    assert!(presence_sample_due(
        frame,
        producer.presence_probed_at_ms,
        attempted_at_ms + ttl
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
    let dir = tempfile::tempdir().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let cache_path = runtime.pane_frame_path();
    let mut frame = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", None, Some("/repo"))],
        unix_now_ms() + 60_000,
        "s",
    );
    frame.presence = Some(presence_sample(1, Some(1_000), 1_000));
    frame.topology_stamp_ms = Some(10);
    frame.metrics_stamp_ms = Some(11);
    let produced_at_ms = frame.produced_at_ms;
    atomic::write_temp_then_rename_cache(&cache_path, &frame).unwrap();
    let before = std::fs::read(&cache_path).unwrap();
    let mut producer = PaneProducer::seeded(&runtime, "s");
    let returned = producer
        .tick_with(
            &runtime,
            MuxName::Tmux,
            "s",
            None,
            None,
            &crate::diag::DiagSink::disabled(),
            &mut |_, _| panic!("fresh prior must not list panes"),
            &|_, _| Some(presence_sample(1, Some(1_000), 2_000)),
        )
        .unwrap();
    assert_eq!(
        std::fs::read(&cache_path).unwrap(),
        before,
        "an unchanged sample writes nothing"
    );
    assert_eq!(returned.presence.unwrap().sampled_at_ms, 2_000);
    producer.presence_probed_at_ms = None;
    producer
        .tick_with(
            &runtime,
            MuxName::Tmux,
            "s",
            None,
            None,
            &crate::diag::DiagSink::disabled(),
            &mut |_, _| panic!("fresh prior must not list panes"),
            &|_, _| Some(presence_sample(2, Some(1_000), 3_000)),
        )
        .unwrap();
    let published: PaneFrame =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    assert_eq!(published.presence.unwrap().human_clients, 2);
    assert_eq!(published.produced_at_ms, produced_at_ms);
    assert_eq!(published.topology_stamp_ms, Some(10));
    assert_eq!(published.metrics_stamp_ms, Some(11));
}

#[test]
fn publication_stamps_only_workspace_affecting_sections() {
    use crate::wakeup::events::PaneFramePublicationKind;

    let mut frame = frame(Vec::new());
    stamp_publication(&mut frame, PaneFramePublicationKind::Topology);
    let topology = frame.topology_stamp_ms.expect("topology stamp");
    let initial_metrics = frame.metrics_stamp_ms.expect("metrics stamp");
    assert_eq!(topology, initial_metrics);

    stamp_publication(&mut frame, PaneFramePublicationKind::Metrics);
    assert_eq!(frame.topology_stamp_ms, Some(topology));
    assert!(frame.metrics_stamp_ms.unwrap() > initial_metrics);

    let sections = (frame.topology_stamp_ms, frame.metrics_stamp_ms);
    stamp_publication(&mut frame, PaneFramePublicationKind::Presence);
    assert_eq!((frame.topology_stamp_ms, frame.metrics_stamp_ms), sections);
}
