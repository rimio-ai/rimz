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
fn health_alert(since_ms: u64, recovered_after_ms: Option<u64>) -> DiagEvent {
    DiagEvent::HealthAlert {
        reason: "snapshot failed".to_owned(),
        since_ms,
        recovered_after_ms,
    }
}
fn link_alert(
    tier: LinkTier,
    rtt_ms: Option<u32>,
    miss_pct: u16,
    since_ms: u64,
    recovered_after_ms: Option<u64>,
) -> DiagEvent {
    DiagEvent::LinkAlert {
        tier,
        rtt_ms,
        miss_pct,
        since_ms,
        recovered_after_ms,
    }
}

fn hosted_carry(reason: HostedCarryDropReason) -> DiagEvent {
    DiagEvent::HostedCarryDropped {
        pane_id: pane("terminal_5"),
        agent_kind: AgentKind::new_unchecked("codex"),
        reason,
    }
}

fn local_bind_rejected(reason: LocalSessionBindRejectReason) -> DiagEvent {
    DiagEvent::LocalSessionBindRejected {
        agent_kind: AgentKind::new_unchecked("codex"),
        agent_session_id: AgentSessionId::from("sess-old"),
        pane_id: pane("terminal_5"),
        reason,
    }
}

fn ghost_bind() -> DiagEvent {
    DiagEvent::GhostSessionBind {
        agent_kind: AgentKind::new_unchecked("codex"),
        agent_session_id: AgentSessionId::from("sess-old"),
        pane_id: pane("terminal_5"),
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
fn envelope_keeps_current_and_legacy_wire_contract() {
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
fn row_presence_gap_evidence_is_backward_compatible() {
    let legacy = serde_json::json!({
        "detector": "row_presence_flap",
        "row_id": "agent:a",
        "pane_id": "zellij:terminal_1",
        "gone_at_ms": 10,
        "back_at_ms": 20
    });
    let decoded: AnomalyKind = serde_json::from_value(legacy).expect("decode legacy row flap");
    assert!(matches!(
        decoded,
        AnomalyKind::RowPresenceFlap {
            gap_evidence: None,
            ..
        }
    ));

    let populated = AnomalyKind::RowPresenceFlap {
        row_id: "agent:a".to_owned(),
        pane_id: Some("zellij:terminal_1".to_owned()),
        gone_at_ms: 10,
        back_at_ms: 20,
        gap_evidence: Some(RowPresenceGapEvidence {
            frame: frame_stamp(7),
            pulled_row_present: true,
            pulled_pane_present: Some(true),
        }),
    };
    let value = serde_json::to_value(&populated).expect("encode populated row flap");
    let round_trip: AnomalyKind = serde_json::from_value(value).expect("decode populated row flap");

    assert_eq!(round_trip, populated);
}

#[test]
fn severity_table_pins_conditional_and_regression_categories() {
    let info = [
        DiagEvent::SidebarWidthIntent {
            trigger: SidebarWidthIntentTrigger::Narrower,
            own_cols: 40,
            base_cols: 40,
            step_cols: Some(10),
            step_exact: false,
            target_cols: Some(30),
            verdict: SidebarWidthIntentVerdict::Accepted,
        },
        DiagEvent::SidebarWidthNudge {
            trigger: SidebarWidthControlTrigger::Retarget,
            from_cols: 40,
            target_cols: 30,
        },
        DiagEvent::SidebarWidthSettle {
            settled_cols: 30,
            learned_step: Some(10),
            outcome: SidebarWidthSettleOutcome::FeedbackLearned,
        },
        health_alert(10, Some(20)),
        link_alert(LinkTier::Good, Some(42), 0, 10, Some(40_000)),
        tick_budget_breach(TickLoop::CacheRefresh, 20, Some(8_000)),
        hosted_carry(HostedCarryDropReason::ProbeReportsAbsent),
        hosted_carry(HostedCarryDropReason::CarryExpired),
        local_bind_rejected(LocalSessionBindRejectReason::NoEvidence),
        DiagEvent::RendererExit {
            cause: RendererExitCause::SelfCloseEmptyTab,
        },
        DiagEvent::ClientReaped {
            killed_pids: vec![42],
            pre_clients: Some(2),
            post_clients: Some(1),
            settled: true,
            timed_out: false,
            errors: Vec::new(),
        },
    ];
    let warn = [
        health_alert(10, None),
        link_alert(LinkTier::Degraded, Some(230), 4, 10, None),
        tick_budget_breach(TickLoop::Fetch, 10, None),
        hosted_carry(HostedCarryDropReason::StartRegressed),
        hosted_carry(HostedCarryDropReason::ForegroundKindMismatch),
        DiagEvent::RendererExit {
            cause: RendererExitCause::DegradedGaveUp,
        },
        DiagEvent::ClientReaped {
            killed_pids: vec![42],
            pre_clients: Some(2),
            post_clients: Some(2),
            settled: false,
            timed_out: true,
            errors: Vec::new(),
        },
        DiagEvent::SidebarOrphanReaped {
            pane_id: "zellij:terminal_5".to_owned(),
            pid: 42,
            first_confirmed_at_ms: 1_000,
            second_confirmed_at_ms: 1_500,
            sigkilled: false,
        },
        DiagEvent::PaneCacheDivergence {
            pane_id: "zellij:terminal_5".to_owned(),
            pid: 42,
            cache_observed_at_ms: Some(900),
            authoritative_observed_at_ms: 1_000,
        },
    ];
    let error = [
        ghost_bind(),
        DiagEvent::RendererPanic {
            message: "boom".to_owned(),
            backtrace: None,
        },
        DiagEvent::RendererSignalDeath {
            signal: Some(6),
            exit_code: None,
            stderr_excerpt: "memory allocation failed".to_owned(),
        },
    ];

    for (events, severity) in [
        (info.as_slice(), DiagSeverity::Info),
        (warn.as_slice(), DiagSeverity::Warn),
        (error.as_slice(), DiagSeverity::Error),
    ] {
        for event in events {
            assert_eq!(event.severity(), severity, "{event:?}");
        }
    }
}

#[test]
fn sidebar_width_trace_round_trips() {
    let event = DiagEvent::SidebarWidthIntent {
        trigger: SidebarWidthIntentTrigger::Wider,
        own_cols: 30,
        base_cols: 40,
        step_cols: Some(10),
        step_exact: false,
        target_cols: Some(50),
        verdict: SidebarWidthIntentVerdict::Accepted,
    };

    let encoded = serde_json::to_value(&event).expect("encode width intent");
    assert_eq!(encoded["kind"], "sidebar_width_intent");
    assert_eq!(encoded["target_cols"], 50);
    assert_eq!(
        serde_json::from_value::<DiagEvent>(encoded).expect("decode width intent"),
        event
    );
}

#[test]
fn orphan_reap_events_keep_their_evidence_on_the_wire() {
    let reaped = DiagEvent::SidebarOrphanReaped {
        pane_id: "zellij:terminal_5".to_owned(),
        pid: 42,
        first_confirmed_at_ms: 1_000,
        second_confirmed_at_ms: 1_500,
        sigkilled: true,
    };
    let divergence = DiagEvent::PaneCacheDivergence {
        pane_id: "zellij:terminal_5".to_owned(),
        pid: 42,
        cache_observed_at_ms: None,
        authoritative_observed_at_ms: 1_000,
    };

    let reaped_json = serde_json::to_value(&reaped).expect("encode orphan reap");
    assert_eq!(reaped_json["kind"], "sidebar_orphan_reaped");
    assert_eq!(reaped_json["sigkilled"], true);
    assert_eq!(
        serde_json::from_value::<DiagEvent>(reaped_json).expect("decode orphan reap"),
        reaped
    );

    let divergence_json = serde_json::to_value(&divergence).expect("encode divergence");
    assert_eq!(divergence_json["kind"], "pane_cache_divergence");
    assert!(divergence_json.get("cache_observed_at_ms").is_none());
    assert_eq!(
        serde_json::from_value::<DiagEvent>(divergence_json).expect("decode divergence"),
        divergence
    );
}

#[test]
fn identity_keys_partition_episodes_and_subjects() {
    let key = |event: DiagEvent| event.identity_key();
    let health_active = key(health_alert(10, None));
    let health_recovered = key(health_alert(10, Some(500)));
    assert_ne!(health_active, health_recovered);
    assert_ne!(health_active, key(health_alert(20, None)));
    assert_eq!(
        health_recovered,
        key(health_alert(10, Some(900))),
        "recovery duration is payload, not episode identity"
    );
    assert_eq!(
        key(link_alert(LinkTier::Bad, Some(800), 40, 10, None)),
        key(link_alert(LinkTier::Bad, Some(200), 4, 10, None)),
        "link measurements do not split one tier episode"
    );
    assert_ne!(
        key(link_alert(LinkTier::Bad, Some(800), 40, 10, None)),
        key(link_alert(LinkTier::Good, Some(42), 0, 10, Some(500)))
    );
    assert_ne!(
        key(tick_budget_breach(TickLoop::Fetch, 10, None)),
        key(tick_budget_breach(TickLoop::Fetch, 10, Some(500)))
    );
    assert_ne!(
        key(tick_budget_breach(TickLoop::Fetch, 10, None)),
        key(tick_budget_breach(TickLoop::CacheRefresh, 10, None))
    );
    assert_ne!(
        key(DiagEvent::DuplicatePaneId {
            pane_id: pane("terminal_1")
        }),
        key(DiagEvent::DuplicatePaneId {
            pane_id: pane("terminal_2")
        })
    );
    let conflict = |session, conflicting_pane| {
        key(DiagEvent::RowConflict {
            agent_kind: AgentKind::new_unchecked("claude"),
            agent_session_id: AgentSessionId::from(session),
            bound_pane: pane("terminal_1"),
            conflicting_pane: pane(conflicting_pane),
        })
    };
    assert_ne!(
        conflict("sess-1", "terminal_2"),
        conflict("sess-2", "terminal_2")
    );
    assert_ne!(
        conflict("sess-1", "terminal_2"),
        conflict("sess-1", "terminal_3")
    );
    assert_ne!(
        key(hosted_carry(HostedCarryDropReason::ProbeReportsAbsent)),
        key(hosted_carry(HostedCarryDropReason::ForegroundKindMismatch))
    );
    assert_ne!(
        key(local_bind_rejected(
            LocalSessionBindRejectReason::NoEvidence
        )),
        key(local_bind_rejected(
            LocalSessionBindRejectReason::StaleLaunchClock
        ))
    );
    assert_ne!(
        key(local_bind_rejected(
            LocalSessionBindRejectReason::PaneReserved
        )),
        key(ghost_bind())
    );
    let renderer_death = |signal, exit_code, stderr: &str| {
        key(DiagEvent::RendererSignalDeath {
            signal,
            exit_code,
            stderr_excerpt: stderr.to_owned(),
        })
    };
    assert_eq!(
        renderer_death(Some(6), None, "first"),
        renderer_death(Some(6), None, "changed"),
        "stderr detail does not split one renderer death"
    );
    assert_ne!(
        renderer_death(Some(6), None, "first"),
        renderer_death(None, Some(6), "first")
    );
    assert_ne!(
        key(DiagEvent::RendererExit {
            cause: RendererExitCause::SelfCloseEmptyTab
        }),
        key(DiagEvent::RendererExit {
            cause: RendererExitCause::DegradedGaveUp
        })
    );
}

#[test]
fn representative_events_keep_json_wire_shape() {
    let rows = [
        (
            r#"{"kind":"frame_rejected","reason":{"reason":"empty"},"prior_pane_count":2,"fresh_pane_count":0,"frames_ref":"frame.1.0.frame_rejected.json"}"#,
            frame_rejected(Some("frame.1.0.frame_rejected.json")),
        ),
        (
            r#"{"kind":"hosted_carry_dropped","pane_id":"zellij:terminal_5","agent_kind":"codex","reason":"foreground_kind_mismatch"}"#,
            hosted_carry(HostedCarryDropReason::ForegroundKindMismatch),
        ),
        (
            r#"{"kind":"local_session_bind_rejected","agent_kind":"codex","agent_session_id":"sess-old","pane_id":"zellij:terminal_5","reason":"stale_launch_clock"}"#,
            local_bind_rejected(LocalSessionBindRejectReason::StaleLaunchClock),
        ),
        (
            r#"{"kind":"ghost_session_bind","agent_kind":"codex","agent_session_id":"sess-old","pane_id":"zellij:terminal_5"}"#,
            ghost_bind(),
        ),
        (
            r#"{"kind":"renderer_exit","cause":"self_close_empty_tab"}"#,
            DiagEvent::RendererExit {
                cause: RendererExitCause::SelfCloseEmptyTab,
            },
        ),
        (
            r#"{"kind":"frame_anomaly","role":"consumer","anomaly":{"detector":"aggregate_oscillation","aggregate":{"aggregate":"provider_spend","kind":"claude"},"from":"1234","via":"0","back":"1234","span_ms":7000,"pulled_via":"0"},"frame":{"produced_at_ms":13000,"rows":2,"agents":2,"processes":0,"pulled_rows":2,"pulled_panes_produced_at_ms":13000},"events_recent":{"pane_closed":[],"pane_opened":[]},"gate_reject_streak":0,"health_failure_streak":0,"dropped_msgs":0}"#,
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
                dropped_msgs: 0,
            },
        ),
    ];

    for (wire, expected) in rows {
        let value: serde_json::Value = serde_json::from_str(wire).expect("valid fixture");
        let decoded: DiagEvent = serde_json::from_value(value.clone()).expect("decode");

        assert_eq!(decoded, expected);
        assert_eq!(serde_json::to_value(&decoded).expect("encode"), value);
    }
}

#[test]
fn provider_mana_identity_prefers_scope_and_keeps_legacy_duration_wire() {
    let build = AggregateKey::ProviderMana {
        kind: "plugin".to_owned(),
        scope_id: Some("build_minutes".to_owned()),
        duration_mins: None,
    };
    let deployment = AggregateKey::ProviderMana {
        kind: "plugin".to_owned(),
        scope_id: Some("deployments".to_owned()),
        duration_mins: None,
    };
    assert_ne!(build.identity(), deployment.identity());
    assert_eq!(build.identity(), "provider_mana:plugin:scope:build_minutes");

    let legacy: AggregateKey = serde_json::from_value(serde_json::json!({
        "aggregate": "provider_mana",
        "kind": "codex",
        "duration_mins": 300
    }))
    .unwrap();
    assert_eq!(legacy.identity(), "provider_mana:codex:300");
}

#[test]
fn pane_drop_evidence_defaults_for_legacy_records() {
    let drop: DiagEvent = serde_json::from_value(serde_json::json!({
        "kind": "pane_count_drop",
        "prior": 3,
        "new": 1,
        "removed": ["zellij:terminal_1", "zellij:terminal_2"],
        "added": []
    }))
    .unwrap();
    assert!(matches!(
        drop,
        DiagEvent::PaneCountDrop { evidence: None, .. }
    ));
}

#[test]
fn summary_includes_frame_ref_and_producer_peer_ids() {
    let rejected = DiagEvent::FrameRejected {
        reason: FrameRejectReason::MissingOwnPane,
        prior_pane_count: 3,
        fresh_pane_count: 2,
        frames_ref: Some("frame.42.0.frame_rejected.json".to_owned()),
    }
    .summary();
    assert!(rejected.contains("frame.42.0.frame_rejected.json"));

    let elder = sidebar("sb_019e8c565bbd708097fce9514f79da04");
    assert!(
        DiagEvent::ProducerElected {
            prior_elder: elder.clone(),
        }
        .summary()
        .contains(elder.as_str())
    );
    assert!(
        DiagEvent::ProducerDemoted {
            new_elder: elder.clone(),
        }
        .summary()
        .contains(elder.as_str())
    );

    let tick = DiagEvent::TickBudgetBreach {
        tick_loop: TickLoop::Fetch,
        over_ticks: 5,
        last_wall_ms: 900,
        last_mux_wait_ms: 250,
        last_fold_bytes: 1_024,
        last_spawns: 1,
        wall_ms: 1_500,
        mux_wait_ms: 900,
        fold_bytes: 300_000,
        spawns: 40,
        budget_wall_ms: 1_000,
        budget_mux_wait_ms: 5_000,
        budget_fold_bytes: 262_144,
        budget_spawns: 32,
        since_ms: 10,
        recovered_after_ms: None,
    }
    .summary();
    assert!(tick.contains("last 900ms (250ms mux)/1024B/1 spawns"));
    assert!(tick.contains("worst 1500ms (900ms mux)/300000B/40 spawns"));
}

#[test]
fn summary_describes_renderer_exit_without_cleanly_label() {
    assert_eq!(
        DiagEvent::RendererExit {
            cause: RendererExitCause::SelfCloseEmptyTab,
        }
        .summary(),
        "renderer exited: self_close_empty_tab"
    );
    assert_eq!(
        DiagEvent::RendererExit {
            cause: RendererExitCause::DegradedGaveUp,
        }
        .summary(),
        "renderer exited: degraded_gave_up"
    );
}

#[test]
fn summary_attributes_row_presence_gap_at_missing_edge() {
    let row_flap = |gap_evidence| DiagEvent::FrameAnomaly {
        role: ObserveRole::Consumer,
        anomaly: AnomalyKind::RowPresenceFlap {
            row_id: "agent:a".to_owned(),
            pane_id: Some("zellij:terminal_1".to_owned()),
            gone_at_ms: 10,
            back_at_ms: 25,
            gap_evidence,
        },
        window_ms: Some(10_000),
        frame: FrameStamp {
            produced_at_ms: Some(8),
            rows: 2,
            agents: 2,
            processes: 0,
            pulled_rows: Some(2),
            pulled_panes_produced_at_ms: Some(8),
        },
        events_recent: EventsSig::default(),
        gate_reject_streak: 0,
        health_failure_streak: 0,
        dropped_msgs: 0,
    };

    assert_eq!(
        row_flap(None).summary(),
        "observed row_presence_flap on agent:a"
    );
    assert_eq!(
        row_flap(Some(RowPresenceGapEvidence {
            frame: FrameStamp {
                produced_at_ms: Some(7),
                rows: 1,
                agents: 1,
                processes: 0,
                pulled_rows: Some(2),
                pulled_panes_produced_at_ms: Some(7),
            },
            pulled_row_present: true,
            pulled_pane_present: Some(true),
        }))
        .summary(),
        "observed row_presence_flap on agent:a; gap 15ms; pulled row present=true; pulled pane present=true"
    );
}
