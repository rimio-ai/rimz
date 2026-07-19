use super::*;
use rimz::diag::record::{
    AnomalyKind, DiagEnvelope, DiagEvent, EventsSig, FetchFoldCause, FetchFoldCauseStats,
    FrameStamp, HostedCarryDropReason, ObserveRole, PaneDropEvidence, PaneDropViewEvidence,
    TickLoop,
};
use rimz::remote::link::LinkTier;

fn sidebar(raw: &str) -> rimz::SidebarInstanceId {
    rimz::SidebarInstanceId::parse(raw).expect("valid sidebar id")
}

fn heartbeat(
    session_name: &str,
    instance_id: &str,
    pane: Option<&str>,
) -> rimz::sidebar::heartbeat::SidebarHeartbeat {
    heartbeat_on(rimz::MuxName::Zellij, session_name, instance_id, pane)
}

fn heartbeat_on(
    mux: rimz::MuxName,
    session_name: &str,
    instance_id: &str,
    pane: Option<&str>,
) -> rimz::sidebar::heartbeat::SidebarHeartbeat {
    rimz::sidebar::heartbeat::SidebarHeartbeat::new(
        rimz::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
        sidebar(instance_id),
        mux,
        session_name,
        "/tmp/sidebar.sock".into(),
        pane.map(|pane| rimz::PaneId::parse(pane).unwrap()),
    )
}

fn diag_record(at_ms: u64, event: DiagEvent) -> DiagEnvelope {
    DiagEnvelope::new(
        rimz::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
        "rimz-test".to_owned(),
        None,
        at_ms,
        event,
    )
}

fn topology_writer(
    plugin_id: u32,
    loaded_at_ms: u64,
) -> rimz::mux::zellij::pane_topology::TopologyWriter {
    rimz::mux::zellij::pane_topology::TopologyWriter {
        plugin_id,
        loaded_at_ms,
        build: None,
        config: None,
    }
}

fn identified_topology_writer(
    plugin_id: u32,
    loaded_at_ms: u64,
    build: &str,
) -> rimz::mux::zellij::pane_topology::TopologyWriter {
    rimz::mux::zellij::pane_topology::TopologyWriter {
        plugin_id,
        loaded_at_ms,
        build: Some(build.to_owned()),
        config: Some(format!("{build}-config")),
    }
}

fn plugin_span(
    plugin_id: u32,
    loaded_at_ms: u64,
    build: Option<&str>,
    last_at_ms: u64,
) -> rimz::diag::plugin_presence::PluginPresenceSpan {
    rimz::diag::plugin_presence::PluginPresenceSpan {
        plugin_id,
        loaded_at_ms,
        build: build.map(str::to_owned),
        sample_count: 2,
        first_at_ms: last_at_ms.saturating_sub(100),
        last_at_ms,
        zellij_version: Some("0.44.3".to_owned()),
        page_growth: 1,
        byte_growth: rimz::diag::plugin_presence::WASM_PAGE_BYTES as i64,
        commands_completed_delta: 2,
        commands_succeeded_delta: Some(1),
        stale_writer_rejections_delta: Some(0),
        topology_failures_delta: Some(0),
        other_failures_delta: Some(0),
        last_failure: None,
    }
}

fn ready_presence_plugins(
    value: Option<model::Probe<model::PresencePlugins>>,
) -> model::PresencePlugins {
    match value {
        Some(model::Probe::Ready(plugins)) => plugins,
        other => panic!("expected ready presence plugins, got {other:?}"),
    }
}

fn tick_breach(since_ms: u64, recovered_after_ms: Option<u64>, over_ticks: u32) -> DiagEvent {
    DiagEvent::TickBudgetBreach {
        tick_loop: TickLoop::Fetch,
        over_ticks,
        last_wall_ms: 1_100,
        last_mux_wait_ms: 0,
        last_fold_bytes: 0,
        last_spawns: 0,
        wall_ms: 1_500,
        mux_wait_ms: 0,
        fold_bytes: 0,
        spawns: 0,
        budget_wall_ms: 1_000,
        budget_mux_wait_ms: 5_000,
        budget_fold_bytes: 262_144,
        budget_spawns: 32,
        since_ms,
        recovered_after_ms,
    }
}

fn frame_anomaly(produced_at_ms: u64) -> DiagEvent {
    DiagEvent::FrameAnomaly {
        role: ObserveRole::Consumer,
        anomaly: AnomalyKind::RosterFlap {
            rows_before: 2,
            empty_at_ms: 10,
            restored_at_ms: 20,
            rows_after: 2,
        },
        window_ms: Some(10),
        frame: FrameStamp {
            produced_at_ms: Some(produced_at_ms),
            rows: 2,
            agents: 2,
            processes: 0,
            pulled_rows: Some(2),
            pulled_panes_produced_at_ms: Some(produced_at_ms),
        },
        events_recent: EventsSig::default(),
        gate_reject_streak: 0,
        health_failure_streak: 0,
        suppressed_since_last: 0,
        dropped_msgs: 0,
    }
}

#[test]
fn tmux_poll_presence_is_expected_when_watch_attached() {
    for stamp_age_ms in [None, Some(61_000)] {
        let presence = tmux_poll_presence(stamp_age_ms, true, true);
        match presence {
            model::Presence::Poll { reason, expected } => {
                assert!(
                    expected,
                    "attached watch is the expected idle polling state"
                );
                assert!(reason.contains("live tmux watch attached"), "{reason}");
                if stamp_age_ms.is_some() {
                    assert!(reason.contains("last poke 61s ago"), "{reason}");
                } else {
                    assert!(!reason.contains("last poke"), "{reason}");
                }
                assert!(!reason.contains("old tmux"), "{reason}");
            }
            other => panic!("expected poll verdict, got {other:?}"),
        }
    }
}

#[test]
fn presence_plugins_classify_active_rejected_and_inactive_generations() {
    let now_ms = 1_000_000;
    let active = identified_topology_writer(49, 300, "desired-build");
    let rejected = identified_topology_writer(41, 200, "old-build");
    let cache = rimz::mux::zellij::pane_topology::PaneTopologyCache {
        session_name: "rimz-test".to_owned(),
        produced_at_ms: now_ms,
        writer: Some(active),
        focused_pane: None,
        clients: None,
        panes: Vec::new(),
    };
    let conflict = rimz::sidebar::presence::TopologyWriterConflict {
        stale_writer: Some(rejected),
        accepted_writer: cache.writer.clone(),
        rejected_count: 4,
        last_ms: now_ms.saturating_sub(1_000),
        last_diag_ms: now_ms.saturating_sub(1_000),
    };
    let desired = rimz::sidebar::cache::PresenceDesired {
        build: "desired-build".to_owned(),
        config: "desired-config".to_owned(),
        recorded_at_ms: now_ms,
    };

    let plugins = ready_presence_plugins(presence_plugins_view(
        Ok(vec![49, 41]),
        vec![
            plugin_span(49, 300, None, now_ms.saturating_sub(2_000)),
            plugin_span(31, 100, Some("old-build"), now_ms.saturating_sub(3_000)),
        ],
        Some(&cache),
        Some(&conflict),
        Some(&desired),
        vec!["/tmp/plugin-presence.log.jsonl".to_owned()],
        now_ms,
    ));

    assert_eq!(plugins.desired_build.as_deref(), Some("desired-build"));
    assert_eq!(plugins.rows.len(), 2);
    assert_eq!(plugins.rows[0].plugin_id, 49);
    assert_eq!(plugins.rows[0].loaded_at_ms, Some(300));
    assert_eq!(plugins.rows[0].status, model::PresencePluginStatus::Active);
    assert_eq!(plugins.rows[0].build.as_deref(), Some("desired-build"));
    assert!(plugins.rows[0].telemetry.is_some());
    assert!(!plugins.rows[0].outdated);
    assert_eq!(plugins.rows[1].plugin_id, 41);
    assert_eq!(plugins.rows[1].loaded_at_ms, Some(200));
    assert_eq!(
        plugins.rows[1].status,
        model::PresencePluginStatus::Rejected
    );
    assert_eq!(plugins.rows[1].rejected_count, Some(4));
    assert!(plugins.rows[1].outdated);
    assert!(plugins.rows[1].telemetry.is_none());
    assert_eq!(plugins.history, vec!["/tmp/plugin-presence.log.jsonl"]);
    assert!(plugins.rows.iter().all(|row| row.plugin_id != 31));
}

#[test]
fn presence_plugins_pick_newest_generation_unless_fresh_writer_names_one() {
    let now_ms = 1_000_000;
    let spans = vec![
        plugin_span(7, 100, Some("old"), now_ms - 2_000),
        plugin_span(7, 200, Some("new"), now_ms - 1_000),
    ];
    let newest = ready_presence_plugins(presence_plugins_view(
        Ok(vec![7]),
        spans.clone(),
        None,
        None,
        None,
        Vec::new(),
        now_ms,
    ));
    assert_eq!(newest.rows.len(), 1);
    assert_eq!(newest.rows[0].loaded_at_ms, Some(200));
    assert_eq!(newest.rows[0].build.as_deref(), Some("new"));

    let cache = rimz::mux::zellij::pane_topology::PaneTopologyCache {
        session_name: "rimz-test".to_owned(),
        produced_at_ms: now_ms,
        writer: Some(identified_topology_writer(7, 100, "old")),
        focused_pane: None,
        clients: None,
        panes: Vec::new(),
    };
    let writer = ready_presence_plugins(presence_plugins_view(
        Ok(vec![7]),
        spans,
        Some(&cache),
        None,
        None,
        Vec::new(),
        now_ms,
    ));
    assert_eq!(writer.rows.len(), 1);
    assert_eq!(writer.rows[0].loaded_at_ms, Some(100));
    assert_eq!(writer.rows[0].build.as_deref(), Some("old"));
    assert_eq!(writer.rows[0].status, model::PresencePluginStatus::Active);
}

#[test]
fn presence_plugins_omit_dead_ids_and_keep_bare_live_ids() {
    let now_ms = 1_000_000;
    let plugins = ready_presence_plugins(presence_plugins_view(
        Ok(vec![88]),
        vec![plugin_span(31, 100, Some("dead"), now_ms - 1_000)],
        None,
        None,
        None,
        Vec::new(),
        now_ms,
    ));

    assert_eq!(plugins.rows.len(), 1);
    assert_eq!(plugins.rows[0].plugin_id, 88);
    assert_eq!(plugins.rows[0].loaded_at_ms, None);
    assert_eq!(plugins.rows[0].build, None);
    assert_eq!(
        plugins.rows[0].status,
        model::PresencePluginStatus::Inactive
    );
    assert!(plugins.rows[0].telemetry.is_none());
}

#[test]
fn presence_plugins_report_live_listing_unavailable_without_history_fallback() {
    let value = presence_plugins_view(
        Err("list-panes failed".to_owned()),
        vec![plugin_span(31, 100, Some("dead"), 900_000)],
        None,
        None,
        None,
        vec!["/tmp/plugin-presence.log.jsonl".to_owned()],
        1_000_000,
    );

    match value {
        Some(model::Probe::Unavailable { error }) => assert_eq!(error, "list-panes failed"),
        other => panic!("expected unavailable presence plugins, got {other:?}"),
    }
}

#[test]
fn presence_plugins_suppress_empty_live_view_without_desired_build() {
    assert!(
        presence_plugins_view(
            Ok(Vec::new()),
            Vec::new(),
            None,
            None,
            None,
            Vec::new(),
            1_000_000,
        )
        .is_none()
    );

    let desired = rimz::sidebar::cache::PresenceDesired {
        build: "desired-build".to_owned(),
        config: "desired-config".to_owned(),
        recorded_at_ms: 1_000_000,
    };
    let plugins = ready_presence_plugins(presence_plugins_view(
        Ok(Vec::new()),
        Vec::new(),
        None,
        None,
        Some(&desired),
        Vec::new(),
        1_000_000,
    ));
    assert!(plugins.rows.is_empty());
}

#[test]
fn tmux_poll_presence_is_expected_without_sidebar() {
    for stamp_age_ms in [None, Some(61_000)] {
        let presence = tmux_poll_presence(stamp_age_ms, false, false);
        match presence {
            model::Presence::Poll { reason, expected } => {
                assert!(expected, "no sidebar is the expected polling state");
                assert!(reason.contains("no sidebar running"), "{reason}");
                assert!(!reason.contains("last poke"), "{reason}");
                assert!(!reason.contains("old tmux"), "{reason}");
            }
            other => panic!("expected poll verdict, got {other:?}"),
        }
    }
}

#[test]
fn tmux_poll_presence_warns_when_sidebar_lacks_watch() {
    let presence = tmux_poll_presence(Some(61_000), true, false);
    match presence {
        model::Presence::Poll { reason, expected } => {
            assert!(!expected, "running sidebar without a watch is unhealthy");
            assert!(
                reason.contains("live tmux watch is not attached"),
                "{reason}"
            );
            assert!(!reason.contains("last poke"), "{reason}");
            assert!(!reason.contains("old tmux"), "{reason}");
        }
        other => panic!("expected poll verdict, got {other:?}"),
    }
}

#[test]
fn newer_cache_writer_supersedes_only_older_conflicts() {
    let older = Some(topology_writer(1, 100));
    let newer = Some(topology_writer(2, 200));

    assert!(topology_conflict_superseded(older.as_ref(), None));
    assert!(topology_conflict_superseded(newer.as_ref(), older.as_ref()));
    assert!(!topology_conflict_superseded(
        older.as_ref(),
        older.as_ref()
    ));
    assert!(!topology_conflict_superseded(
        older.as_ref(),
        newer.as_ref()
    ));
    assert!(!topology_conflict_superseded(None, None));
}

#[test]
fn diagnostic_incidents_collapse_same_episode_records() {
    let incidents = diagnostic_incidents(
        vec![
            diag_record(1, tick_breach(10, None, 5)),
            diag_record(2, tick_breach(10, None, 6)),
        ],
        12,
        None,
    );

    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0].last_at_ms, 2);
    assert_eq!(incidents[0].record_count, 2);
    assert!(incidents[0].summary.contains("over budget for 6 ticks"));
}

#[test]
fn diagnostic_incidents_pair_recovery_and_keep_recurrence_distinct() {
    let mut records = Vec::new();
    for at_ms in 1..=13 {
        records.push(diag_record(at_ms, tick_breach(10, None, at_ms as u32)));
    }
    records.push(diag_record(20, tick_breach(10, Some(10), 13)));
    records.push(diag_record(21, tick_breach(30, None, 5)));

    let incidents = diagnostic_incidents(records, 12, None);

    assert_eq!(incidents.len(), 2);
    assert_eq!(incidents[0].record_count, 14);
    assert_eq!(incidents[0].state, model::DoctorState::Recovered);
    assert!(incidents[0].summary.contains("recovered after 10ms"));
    assert!(incidents[1].summary.contains("over budget for 5 ticks"));
}

#[test]
fn diagnostic_incidents_pair_link_recovery_by_stable_episode_start() {
    let active = DiagEvent::LinkAlert {
        tier: LinkTier::Degraded,
        rtt_ms: Some(231),
        miss_pct: 0,
        since_ms: 10,
        recovered_after_ms: None,
    };
    let recovered = DiagEvent::LinkAlert {
        tier: LinkTier::Good,
        rtt_ms: Some(50),
        miss_pct: 0,
        since_ms: 10,
        recovered_after_ms: Some(500),
    };
    let recurrence = DiagEvent::LinkAlert {
        tier: LinkTier::Bad,
        rtt_ms: None,
        miss_pct: 100,
        since_ms: 30,
        recovered_after_ms: None,
    };

    let incidents = diagnostic_incidents(
        vec![
            diag_record(10, active),
            diag_record(20, recovered),
            diag_record(30, recurrence),
        ],
        12,
        None,
    );

    assert_eq!(incidents.len(), 2);
    assert_eq!(incidents[0].record_count, 2);
    assert_eq!(incidents[0].state, model::DoctorState::Recovered);
    assert_eq!(incidents[0].impact, model::DoctorImpact::Info);
    assert_eq!(incidents[1].record_count, 1);
    assert_eq!(incidents[1].state, model::DoctorState::Investigate);
}

#[test]
fn diagnostic_incidents_collapse_cross_observer_frame_copies_only() {
    let mut first = diag_record(1, frame_anomaly(900));
    first.instance_id = Some(sidebar("sb_019e8c565bbd708097fce9514f79da04"));
    let mut second = diag_record(2, frame_anomaly(900));
    second.instance_id = Some(sidebar("sb_019eb7da41f478b2a84079743e472a87"));
    let mut recurrence = diag_record(3, frame_anomaly(901));
    recurrence.instance_id = second.instance_id.clone();

    let incidents = diagnostic_incidents(vec![first, second, recurrence], 12, None);

    assert_eq!(incidents.len(), 2);
    assert_eq!(incidents[0].record_count, 2);
    assert_eq!(incidents[0].distinct_observer_count, 2);
    assert_eq!(incidents[1].record_count, 1);
}

#[test]
fn diagnostic_incidents_split_same_identity_after_quiet_gap() {
    let incidents = diagnostic_incidents(
        vec![
            diag_record(1, tick_breach(10, None, 1)),
            diag_record(60_002, tick_breach(10, None, 2)),
        ],
        12,
        None,
    );

    assert_eq!(incidents.len(), 2);

    let mut first = frame_anomaly(1);
    let mut second = frame_anomaly(2);
    for event in [&mut first, &mut second] {
        if let DiagEvent::FrameAnomaly { frame, .. } = event {
            frame.produced_at_ms = None;
        }
    }
    let incidents = diagnostic_incidents(
        vec![diag_record(1, first), diag_record(60_002, second)],
        12,
        None,
    );
    assert_eq!(
        incidents.len(),
        2,
        "missing frame identity follows the bounded episode rule"
    );
}

#[test]
fn diagnostic_classification_requires_complete_positive_evidence() {
    let pane = |raw| rimz::PaneId::from_parts(rimz::MuxName::Zellij, raw);
    let expected_drop = DiagEvent::PaneCountDrop {
        prior: 2,
        new: 1,
        removed: vec![pane("terminal_1")],
        added: Vec::new(),
        evidence: Some(PaneDropEvidence {
            prior_panes: 2,
            fresh_panes: 1,
            mass_shrink: false,
            affected_views: vec![PaneDropViewEvidence {
                view_id: "tab_1".to_owned(),
                prior_panes: 1,
                remaining_panes: 0,
                removed_pane_ids: vec![pane("terminal_1")],
                managed_panes: Vec::new(),
            }],
        }),
        frames_ref: None,
    };
    let legacy_drop = DiagEvent::PaneCountDrop {
        prior: 2,
        new: 1,
        removed: vec![pane("terminal_1")],
        added: Vec::new(),
        evidence: None,
        frames_ref: None,
    };
    assert_eq!(
        classify_diagnostic(&expected_drop, expected_drop.severity()).0,
        model::DoctorState::Expected
    );
    assert_eq!(
        classify_diagnostic(&legacy_drop, legacy_drop.severity()).0,
        model::DoctorState::Investigate
    );
}

#[test]
fn diagnostic_classification_covers_retained_and_reason_sensitive_events() {
    let hosted = |reason| DiagEvent::HostedCarryDropped {
        pane_id: rimz::PaneId::from_parts(rimz::MuxName::Zellij, "terminal_5"),
        agent_kind: rimz::ids::AgentKind::new_unchecked("codex"),
        reason,
    };
    let cases = [
        (
            DiagEvent::FetchFoldStats {
                interval_ms: 30_000,
                causes: vec![FetchFoldCauseStats {
                    cause: FetchFoldCause::Backstop,
                    memo_skips: 1,
                    full_folds: 0,
                    adoptions: 0,
                    fallbacks: 0,
                    fold_ms: 0,
                }],
            },
            model::DoctorState::Expected,
            model::DoctorImpact::Info,
        ),
        (
            hosted(HostedCarryDropReason::ProbeReportsAbsent),
            model::DoctorState::Expected,
            model::DoctorImpact::Info,
        ),
        (
            hosted(HostedCarryDropReason::CarryExpired),
            model::DoctorState::Expected,
            model::DoctorImpact::Info,
        ),
        (
            hosted(HostedCarryDropReason::StartRegressed),
            model::DoctorState::Investigate,
            model::DoctorImpact::Warn,
        ),
        (
            hosted(HostedCarryDropReason::ForegroundKindMismatch),
            model::DoctorState::Investigate,
            model::DoctorImpact::Warn,
        ),
        (
            DiagEvent::LinkAlert {
                tier: LinkTier::Degraded,
                rtt_ms: Some(231),
                miss_pct: 0,
                since_ms: 10,
                recovered_after_ms: None,
            },
            model::DoctorState::Investigate,
            model::DoctorImpact::Warn,
        ),
        (
            DiagEvent::LinkAlert {
                tier: LinkTier::Good,
                rtt_ms: Some(50),
                miss_pct: 0,
                since_ms: 10,
                recovered_after_ms: Some(500),
            },
            model::DoctorState::Recovered,
            model::DoctorImpact::Info,
        ),
    ];

    for (event, expected_state, expected_impact) in cases {
        assert_eq!(
            classify_diagnostic(&event, event.severity()),
            (expected_state, expected_impact),
            "{}",
            event.kind_name()
        );
    }
}

#[test]
fn diagnostic_incidents_drop_records_at_or_before_watermark() {
    let incidents = diagnostic_incidents(
        vec![
            diag_record(9, tick_breach(9, None, 1)),
            diag_record(10, tick_breach(10, None, 2)),
            diag_record(11, tick_breach(11, None, 3)),
        ],
        12,
        Some(10),
    );

    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0].last_at_ms, 11);
}

#[test]
fn stale_build_requires_two_known_different_builds() {
    assert!(!stale_build(Some("current"), Some("current")));
    assert!(stale_build(Some("old"), Some("current")));
    assert!(!stale_build(None, Some("current")));
    assert!(!stale_build(Some("old"), None));
    assert!(!stale_build(None, None));
}

#[test]
fn duplicate_sidebar_sessions_group_by_backend_and_session() {
    let same_session = vec![
        heartbeat(
            "rimz-current",
            "sb_019eb7da41f478b2a84079743e472a87",
            Some("zellij:terminal_1"),
        ),
        heartbeat(
            "rimz-current",
            "sb_019eb7da43787c6081a474afb02c2067",
            Some("zellij:terminal_2"),
        ),
    ];
    assert!(
        duplicate_sidebar_session_groups(&same_session).is_empty(),
        "multiple sidebars in one session are normal"
    );

    let cross_backend = vec![
        heartbeat_on(
            rimz::MuxName::Zellij,
            "rimz-current",
            "sb_019eb7da41f478b2a84079743e472a87",
            Some("zellij:terminal_1"),
        ),
        heartbeat_on(
            rimz::MuxName::Tmux,
            "rimz-current",
            "sb_019eb7da43787c6081a474afb02c2067",
            Some("tmux:%2"),
        ),
    ];
    let groups = duplicate_sidebar_session_groups(&cross_backend);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].mux, rimz::MuxName::Zellij);
    assert_eq!(groups[1].mux, rimz::MuxName::Tmux);

    let duplicate_sessions = vec![
        heartbeat(
            "rimz-current",
            "sb_019eb7da41f478b2a84079743e472a87",
            Some("zellij:terminal_1"),
        ),
        heartbeat(
            "rimz-old",
            "sb_019eb7da2dda7992b4286dee69d33358",
            Some("zellij:terminal_7"),
        ),
        heartbeat("rimz-old", "sb_019eb7da2de17752994de2401b433b70", None),
    ];
    let groups = duplicate_sidebar_session_groups(&duplicate_sessions);
    assert_eq!(
        groups,
        vec![
            SidebarSessionGroup {
                mux: rimz::MuxName::Zellij,
                session_name: "rimz-current".to_owned(),
                sidebar_count: 1,
                pane_ids: vec!["zellij:terminal_1".to_owned()],
            },
            SidebarSessionGroup {
                mux: rimz::MuxName::Zellij,
                session_name: "rimz-old".to_owned(),
                sidebar_count: 2,
                pane_ids: vec!["zellij:terminal_7".to_owned()],
            },
        ]
    );
}
