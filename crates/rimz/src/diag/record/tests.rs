use super::*;
use crate::ids::MuxName;

fn workspace_id() -> WorkspaceId {
    WorkspaceId::from_project_root(std::path::Path::new("/repo"))
}

fn pane(raw: &str) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, raw)
}

fn sidebar(raw: &str) -> SidebarInstanceId {
    SidebarInstanceId::parse(raw).expect("valid sidebar instance id")
}

fn frame_rejected(frames_ref: Option<&str>) -> DiagEvent {
    DiagEvent::FrameRejected {
        reason: FrameRejectReason::Empty,
        prior_pane_count: 2,
        fresh_pane_count: 0,
        frames_ref: frames_ref.map(str::to_owned),
    }
}

fn frame_stamp(produced_at_ms: u64) -> FrameStamp {
    FrameStamp {
        produced_at_ms: Some(produced_at_ms),
        rows: 2,
        agents: 2,
        processes: 0,
        pulled_rows: Some(2),
        pulled_panes_produced_at_ms: Some(produced_at_ms),
    }
}

fn frame_anomaly(anomaly: AnomalyKind) -> DiagEvent {
    DiagEvent::FrameAnomaly {
        role: ObserveRole::Consumer,
        anomaly,
        window_ms: None,
        frame: frame_stamp(13_000),
        events_recent: EventsSig::default(),
        gate_reject_streak: 0,
        health_failure_streak: 0,
        suppressed_since_last: 0,
        dropped_msgs: 0,
    }
}

fn tick_budget_breach(
    tick_loop: TickLoop,
    since_ms: u64,
    recovered_after_ms: Option<u64>,
) -> DiagEvent {
    DiagEvent::TickBudgetBreach {
        tick_loop,
        over_ticks: 5,
        last_wall_ms: 1_200,
        last_mux_wait_ms: 0,
        last_fold_bytes: 300_000,
        last_spawns: 2,
        wall_ms: 1_200,
        mux_wait_ms: 0,
        fold_bytes: 300_000,
        spawns: 2,
        budget_wall_ms: 1_000,
        budget_mux_wait_ms: 5_000,
        budget_fold_bytes: 262_144,
        budget_spawns: 32,
        since_ms,
        recovered_after_ms,
    }
}

#[test]
fn new_envelopes_pin_schema_build_severity_suppression_and_legacy_build() {
    let envelope = DiagEnvelope::new(
        workspace_id(),
        "rimz-test".to_owned(),
        Some(sidebar("sb_019e8c565bbd708097fce9514f79da04")),
        42,
        frame_rejected(None),
    );

    assert_eq!(envelope.v, DIAG_SCHEMA_VERSION);
    assert_eq!(envelope.build.as_deref(), crate::build_id::current());
    assert!(envelope.build.is_some());
    assert_eq!(envelope.severity, DiagSeverity::Warn);
    assert_eq!(envelope.suppressed_since_last, 0);
    assert!(envelope.is_current_version());

    let value = serde_json::to_value(&envelope).expect("encode");
    assert_eq!(value["severity"], "warn");
    assert!(value.get("suppressed_since_last").is_none());

    let suppressed = serde_json::to_value(envelope.clone().with_suppressed(2)).expect("encode");
    assert_eq!(suppressed["suppressed_since_last"], 2);

    let mut legacy = value;
    legacy.as_object_mut().expect("object").remove("build");
    let decoded: DiagEnvelope = serde_json::from_value(legacy).expect("decode");

    assert_eq!(decoded.build, None);
    assert!(decoded.is_current_version());
}

#[test]
fn tick_budget_breach_deserializes_legacy_records_without_last_sample() {
    let value = serde_json::json!({
        "kind": "tick_budget_breach",
        "tick_loop": "fetch",
        "over_ticks": 5,
        "wall_ms": 1_200,
        "fold_bytes": 300_000,
        "spawns": 2,
        "budget_wall_ms": 1_000,
        "budget_fold_bytes": 262_144,
        "budget_spawns": 32,
        "since_ms": 10
    });

    let decoded: DiagEvent = serde_json::from_value(value).expect("decode legacy breach");

    assert_eq!(
        decoded,
        DiagEvent::TickBudgetBreach {
            tick_loop: TickLoop::Fetch,
            over_ticks: 5,
            last_wall_ms: 0,
            last_mux_wait_ms: 0,
            last_fold_bytes: 0,
            last_spawns: 0,
            wall_ms: 1_200,
            mux_wait_ms: 0,
            fold_bytes: 300_000,
            spawns: 2,
            budget_wall_ms: 1_000,
            budget_mux_wait_ms: 0,
            budget_fold_bytes: 262_144,
            budget_spawns: 32,
            since_ms: 10,
            recovered_after_ms: None,
        }
    );
}

#[test]
fn severity_table_pins_product_categories() {
    let rows = [
        (frame_rejected(None), DiagSeverity::Warn),
        (
            DiagEvent::HealthAlert {
                reason: "snapshot failed".to_owned(),
                since_ms: 10,
                recovered_after_ms: None,
            },
            DiagSeverity::Warn,
        ),
        (
            DiagEvent::LinkAlert {
                tier: LinkTier::Degraded,
                rtt_ms: Some(230),
                miss_pct: 4,
                since_ms: 10,
                recovered_after_ms: None,
            },
            DiagSeverity::Warn,
        ),
        (
            tick_budget_breach(TickLoop::Fetch, 10, None),
            DiagSeverity::Warn,
        ),
        (
            DiagEvent::HealthAlert {
                reason: "snapshot failed".to_owned(),
                since_ms: 10,
                recovered_after_ms: Some(20),
            },
            DiagSeverity::Info,
        ),
        (
            DiagEvent::LinkAlert {
                tier: LinkTier::Good,
                rtt_ms: Some(42),
                miss_pct: 0,
                since_ms: 10,
                recovered_after_ms: Some(40_000),
            },
            DiagSeverity::Info,
        ),
        (
            tick_budget_breach(TickLoop::CacheRefresh, 20, Some(8_000)),
            DiagSeverity::Info,
        ),
        (
            DiagEvent::PaneCarryRefuted {
                carried: vec![pane("terminal_1")],
                pids: vec![42],
                prior: 2,
                fresh: 1,
                verified: 2,
                frames_ref: None,
            },
            DiagSeverity::Info,
        ),
        (
            DiagEvent::NewbornQuarantined {
                pane_id: pane("terminal_1"),
            },
            DiagSeverity::Info,
        ),
        (
            DiagEvent::ProducerElected {
                prior_elder: sidebar("sb_019e8c565bbd708097fce9514f79da04"),
            },
            DiagSeverity::Info,
        ),
        (
            DiagEvent::ProducerDemoted {
                new_elder: sidebar("sb_019e8c565bbd7b22854f93a905e1034c"),
            },
            DiagSeverity::Info,
        ),
        (
            DiagEvent::MixedBuildWriters {
                prior_build: "0f3a9c21d4be".to_owned(),
                own_build: "8e7d6c5b4a39".to_owned(),
            },
            DiagSeverity::Info,
        ),
        (
            DiagEvent::RendererPanic {
                message: "boom".to_owned(),
                backtrace: None,
            },
            DiagSeverity::Error,
        ),
        (
            DiagEvent::RendererSignalDeath {
                signal: Some(6),
                exit_code: None,
                stderr_excerpt: "memory allocation failed".to_owned(),
            },
            DiagSeverity::Error,
        ),
        (
            DiagEvent::RendererExit {
                cause: RendererExitCause::SelfCloseEmptyTab,
            },
            DiagSeverity::Info,
        ),
        (
            DiagEvent::RendererExit {
                cause: RendererExitCause::DegradedGaveUp,
            },
            DiagSeverity::Warn,
        ),
    ];

    for (event, severity) in rows {
        assert_eq!(event.severity(), severity, "{event:?}");
    }
}

#[test]
fn identity_key_table_pins_phase_episode_loop_and_subjects() {
    let rows = [
        (
            DiagEvent::HealthAlert {
                reason: "snapshot failed".to_owned(),
                since_ms: 10,
                recovered_after_ms: None,
            },
            "health_alert:snapshot failed:active:10",
        ),
        (
            DiagEvent::HealthAlert {
                reason: "snapshot failed".to_owned(),
                since_ms: 10,
                recovered_after_ms: Some(500),
            },
            "health_alert:snapshot failed:recovered:10",
        ),
        (
            DiagEvent::HealthAlert {
                reason: "snapshot failed".to_owned(),
                since_ms: 20,
                recovered_after_ms: None,
            },
            "health_alert:snapshot failed:active:20",
        ),
        (
            DiagEvent::LinkAlert {
                tier: LinkTier::Bad,
                rtt_ms: Some(800),
                miss_pct: 40,
                since_ms: 10,
                recovered_after_ms: None,
            },
            "link_alert:Bad:active:10",
        ),
        (
            DiagEvent::LinkAlert {
                tier: LinkTier::Good,
                rtt_ms: Some(42),
                miss_pct: 0,
                since_ms: 10,
                recovered_after_ms: Some(40_000),
            },
            "link_alert:Good:recovered:10",
        ),
        (
            tick_budget_breach(TickLoop::Fetch, 10, None),
            "tick_budget_breach:Fetch:active:10",
        ),
        (
            tick_budget_breach(TickLoop::Fetch, 10, Some(500)),
            "tick_budget_breach:Fetch:recovered:10",
        ),
        (
            tick_budget_breach(TickLoop::CacheRefresh, 10, None),
            "tick_budget_breach:CacheRefresh:active:10",
        ),
        (
            DiagEvent::DuplicatePaneId {
                pane_id: pane("terminal_1"),
            },
            "duplicate_pane_id:zellij:terminal_1",
        ),
        (
            DiagEvent::DuplicatePaneId {
                pane_id: pane("terminal_2"),
            },
            "duplicate_pane_id:zellij:terminal_2",
        ),
        (
            DiagEvent::RowConflict {
                agent_kind: AgentKind::new_unchecked("claude"),
                agent_session_id: AgentSessionId::from("sess-1"),
                bound_pane: pane("terminal_1"),
                conflicting_pane: pane("terminal_2"),
            },
            "row_conflict:claude:sess-1:zellij:terminal_1:zellij:terminal_2",
        ),
        (
            DiagEvent::MixedBuildWriters {
                prior_build: "0f3a9c21d4be".to_owned(),
                own_build: "8e7d6c5b4a39".to_owned(),
            },
            "mixed_build_writers:0f3a9c21d4be:8e7d6c5b4a39",
        ),
        (
            DiagEvent::RendererSignalDeath {
                signal: Some(6),
                exit_code: None,
                stderr_excerpt: "memory allocation failed".to_owned(),
            },
            "renderer_signal_death:Some(6):None",
        ),
        (
            DiagEvent::RendererExit {
                cause: RendererExitCause::SelfCloseEmptyTab,
            },
            "renderer_exit:self_close_empty_tab",
        ),
        (
            DiagEvent::RendererExit {
                cause: RendererExitCause::DegradedGaveUp,
            },
            "renderer_exit:degraded_gave_up",
        ),
    ];

    for (event, identity) in rows {
        assert_eq!(event.identity_key(), identity, "{event:?}");
    }
}

#[test]
fn frame_anomaly_schema_and_identity_pin_detector_subjects() {
    let row = frame_anomaly(AnomalyKind::RowPresenceFlap {
        row_id: "agent-1".to_owned(),
        pane_id: Some("zellij:terminal_1".to_owned()),
        gone_at_ms: 11_000,
        back_at_ms: 12_000,
    });
    let aggregate = frame_anomaly(AnomalyKind::AggregateReset {
        aggregate: AggregateKey::ProviderSpend {
            kind: "claude".to_owned(),
        },
        from: "1234".to_owned(),
        pulled: Some("0".to_owned()),
    });

    assert_eq!(
        row.identity_key(),
        "frame_anomaly:row_presence_flap:agent-1"
    );
    assert_eq!(
        aggregate.identity_key(),
        "frame_anomaly:aggregate_reset:provider_spend:claude"
    );

    let value = serde_json::to_value(&aggregate).expect("encode");
    assert_eq!(value["kind"], "frame_anomaly");
    assert_eq!(value["anomaly"]["detector"], "aggregate_reset");
    assert_eq!(value["anomaly"]["aggregate"]["aggregate"], "provider_spend");
    assert_eq!(value["anomaly"]["aggregate"]["kind"], "claude");
}

#[test]
fn representative_events_keep_json_wire_shape() {
    let rows = [
        (
            serde_json::json!({
                "kind": "frame_rejected",
                "reason": { "reason": "empty" },
                "prior_pane_count": 2,
                "fresh_pane_count": 0,
                "frames_ref": "frame.1.0.frame_rejected.json"
            }),
            frame_rejected(Some("frame.1.0.frame_rejected.json")),
        ),
        (
            serde_json::json!({
                "kind": "link_alert",
                "tier": "bad",
                "rtt_ms": 800,
                "miss_pct": 40,
                "since_ms": 10
            }),
            DiagEvent::LinkAlert {
                tier: LinkTier::Bad,
                rtt_ms: Some(800),
                miss_pct: 40,
                since_ms: 10,
                recovered_after_ms: None,
            },
        ),
        (
            serde_json::json!({
                "kind": "link_alert",
                "tier": "good",
                "rtt_ms": 42,
                "miss_pct": 0,
                "since_ms": 10,
                "recovered_after_ms": 40_000
            }),
            DiagEvent::LinkAlert {
                tier: LinkTier::Good,
                rtt_ms: Some(42),
                miss_pct: 0,
                since_ms: 10,
                recovered_after_ms: Some(40_000),
            },
        ),
        (
            serde_json::json!({
                "kind": "renderer_exit",
                "cause": "self_close_empty_tab"
            }),
            DiagEvent::RendererExit {
                cause: RendererExitCause::SelfCloseEmptyTab,
            },
        ),
        (
            serde_json::json!({
                "kind": "frame_anomaly",
                "role": "consumer",
                "anomaly": {
                    "detector": "aggregate_oscillation",
                    "aggregate": {
                        "aggregate": "provider_spend",
                        "kind": "claude"
                    },
                    "from": "1234",
                    "via": "0",
                    "back": "1234",
                    "span_ms": 7_000,
                    "pulled_via": "0"
                },
                "frame": {
                    "produced_at_ms": 13_000,
                    "rows": 2,
                    "agents": 2,
                    "processes": 0,
                    "pulled_rows": 2,
                    "pulled_panes_produced_at_ms": 13_000
                },
                "events_recent": {
                    "pane_closed": [],
                    "pane_opened": []
                },
                "gate_reject_streak": 0,
                "health_failure_streak": 0,
                "suppressed_since_last": 3,
                "dropped_msgs": 0
            }),
            DiagEvent::FrameAnomaly {
                role: ObserveRole::Consumer,
                anomaly: AnomalyKind::AggregateOscillation {
                    aggregate: AggregateKey::ProviderSpend {
                        kind: "claude".to_owned(),
                    },
                    from: "1234".to_owned(),
                    via: "0".to_owned(),
                    back: "1234".to_owned(),
                    span_ms: 7_000,
                    pulled_via: Some("0".to_owned()),
                },
                window_ms: None,
                frame: frame_stamp(13_000),
                events_recent: EventsSig::default(),
                gate_reject_streak: 0,
                health_failure_streak: 0,
                suppressed_since_last: 3,
                dropped_msgs: 0,
            },
        ),
    ];

    for (value, expected) in rows {
        let decoded: DiagEvent = serde_json::from_value(value.clone()).expect("decode");

        assert_eq!(decoded, expected);
        assert_eq!(serde_json::to_value(&decoded).expect("encode"), value);
    }
}

#[test]
fn renderer_exit_envelope_serializes_schema_cause_and_severity() {
    let rows = [
        (
            RendererExitCause::SelfCloseEmptyTab,
            DiagSeverity::Info,
            "info",
        ),
        (
            RendererExitCause::DegradedGaveUp,
            DiagSeverity::Warn,
            "warn",
        ),
    ];

    for (cause, severity, severity_wire) in rows {
        let envelope = DiagEnvelope::new(
            workspace_id(),
            "rimz-test".to_owned(),
            Some(sidebar("sb_019e8c565bbd708097fce9514f79da04")),
            42,
            DiagEvent::RendererExit { cause },
        );

        assert_eq!(envelope.v, DIAG_SCHEMA_VERSION);
        assert_eq!(envelope.severity, severity);

        let value = serde_json::to_value(&envelope).expect("encode");
        assert_eq!(value["v"], DIAG_SCHEMA_VERSION);
        assert_eq!(value["severity"], severity_wire);
        assert_eq!(value["event"]["kind"], "renderer_exit");
        assert_eq!(value["event"]["cause"], cause.as_str());

        let decoded: DiagEnvelope = serde_json::from_value(value).expect("decode");
        assert_eq!(decoded.event, DiagEvent::RendererExit { cause });
        assert_eq!(decoded.severity, severity);
    }
}
