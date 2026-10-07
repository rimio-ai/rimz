use super::*;
use crate::ids::MuxName;

#[test]
fn recovery_child_ends_are_informational_session_diagnostics() {
    let wire = r#"{"kind":"recovery_child_ended","agent_kind":"codex","agent_id":"child"}"#;
    let decoded = serde_json::from_str::<DiagEvent>(wire);
    assert!(
        decoded.is_ok(),
        "child end diagnostic must decode: {decoded:?}"
    );
    let event = decoded.unwrap();
    assert_eq!(event.kind_name(), "recovery_child_ended");
    assert_eq!(event.severity(), DiagSeverity::Info);
    assert_eq!(event.identity_key(), "recovery_child_ended:codex:child");
    assert_eq!(
        event.summary(),
        "codex/child ended: recovery never resumes a launched child"
    );
    assert_eq!(serde_json::to_string(&event).unwrap(), wire);
}

#[test]
fn recovery_seat_refills_are_informational_session_diagnostics() {
    let wire = r#"{"kind":"recovery_seat_refilled","agent_kind":"codex","agent_id":"old-coder"}"#;
    let decoded = serde_json::from_str::<DiagEvent>(wire);
    assert!(
        decoded.is_ok(),
        "refill diagnostic must decode: {decoded:?}"
    );
    let event = decoded.unwrap();
    assert_eq!(event.kind_name(), "recovery_seat_refilled");
    assert_eq!(event.severity(), DiagSeverity::Info);
    assert_eq!(
        event.identity_key(),
        "recovery_seat_refilled:codex:old-coder"
    );
    assert!(event.summary().contains("old-coder"));
    assert!(event.summary().contains("live replacement"));
    assert_eq!(serde_json::to_string(&event).unwrap(), wire);
}

#[test]
fn provider_startup_exits_round_trip_exit_codes_and_signals() {
    let wires = [
        r#"{"kind":"provider_startup_exit","agent_kind":"codex","agent_name":"pruner","action":"launch","exit_code":2,"signal":null,"startup_ms":1,"relaunches":0}"#,
        r#"{"kind":"provider_startup_exit","agent_kind":"codex","agent_name":"pruner","action":"resume","exit_code":null,"signal":15,"startup_ms":5,"relaunches":3}"#,
    ];
    let decoded = wires.map(serde_json::from_str::<DiagEvent>);
    assert!(decoded.iter().all(Result::is_ok), "{decoded:?}");
    for (event, wire) in decoded.into_iter().zip(wires) {
        let event = event.unwrap();
        assert_eq!(serde_json::to_string(&event).unwrap(), wire);
        assert_eq!(event.severity(), DiagSeverity::Warn);
        assert_eq!(
            event.identity_key(),
            "provider_startup_exit:codex:Some(\"pruner\")"
        );
    }
}

#[test]
fn legacy_provider_aggregates_decode_as_default_logins() {
    for line in [
        r#"{"aggregate":"provider_spend","kind":"claude"}"#,
        r#"{"aggregate":"provider_mana","kind":"claude","duration_mins":300}"#,
    ] {
        let decoded = serde_json::from_str::<AggregateKey>(line);
        assert!(decoded.is_ok(), "legacy record: {decoded:?}");
        let decoded = decoded.unwrap();
        assert!(decoded.identity().contains("claude@default"));
        let json = serde_json::to_value(&decoded).unwrap();
        assert_eq!(json["login"], "claude@default");
        assert_eq!(
            serde_json::from_value::<AggregateKey>(json).unwrap(),
            decoded
        );
    }
}

#[test]
fn provider_key_vocabulary_keeps_prior_shapes_and_round_trips_new_ones() {
    let login = || "claude@default".parse::<crate::ids::LoginKey>().unwrap();
    let mana_field = |field| AggregateKey::ProviderManaField {
        login: login(),
        scope_id: None,
        duration_mins: Some(300),
        field,
    };
    let period = |period| AggregateKey::ProviderSpendPeriod {
        login: login(),
        period,
    };
    let rows = [
        (
            AggregateKey::ProviderSpend { login: login() },
            r#"{"aggregate":"provider_spend","login":"claude@default"}"#,
            "provider_spend:claude@default",
        ),
        (
            period(SpendPeriod::Headline),
            r#"{"aggregate":"provider_spend_period","login":"claude@default","period":"headline"}"#,
            "provider_spend:claude@default:headline",
        ),
        (
            period(SpendPeriod::Week),
            r#"{"aggregate":"provider_spend_period","login":"claude@default","period":"week"}"#,
            "provider_spend:claude@default:week",
        ),
        (
            period(SpendPeriod::Month),
            r#"{"aggregate":"provider_spend_period","login":"claude@default","period":"month"}"#,
            "provider_spend:claude@default:month",
        ),
        (
            AggregateKey::ProviderMana {
                login: login(),
                scope_id: None,
                duration_mins: Some(300),
            },
            r#"{"aggregate":"provider_mana","login":"claude@default","duration_mins":300}"#,
            "provider_mana:claude@default:300",
        ),
        (
            mana_field(WindowField::ResetsAt),
            r#"{"aggregate":"provider_mana_field","login":"claude@default","duration_mins":300,"field":"resets_at"}"#,
            "provider_mana:claude@default:300:resets_at",
        ),
        (
            mana_field(WindowField::Lifted),
            r#"{"aggregate":"provider_mana_field","login":"claude@default","duration_mins":300,"field":"lifted"}"#,
            "provider_mana:claude@default:300:lifted",
        ),
        (
            AggregateKey::ProviderManaField {
                login: login(),
                scope_id: Some("premium".to_owned()),
                duration_mins: None,
                field: WindowField::ResetsAt,
            },
            r#"{"aggregate":"provider_mana_field","login":"claude@default","scope_id":"premium","duration_mins":null,"field":"resets_at"}"#,
            "provider_mana:claude@default:scope:premium:resets_at",
        ),
    ];
    for (key, wire, identity) in rows {
        assert_eq!(serde_json::to_string(&key).unwrap(), wire);
        assert_eq!(serde_json::from_str::<AggregateKey>(wire).unwrap(), key);
        assert_eq!(key.identity(), identity);
    }

    for (field, name) in [
        (PanelField::Version, "version"),
        (PanelField::Plan, "plan"),
        (PanelField::Metered, "metered"),
        (PanelField::RemoteControl, "remote_control"),
        (PanelField::DayBudget, "day_budget"),
        (PanelField::ExtraCredits, "extra_credits"),
        (PanelField::ResetCredits, "reset_credits"),
        (PanelField::RedeemForecast, "redeem_forecast"),
    ] {
        let key = AggregateKey::ProviderField {
            login: login(),
            field,
        };
        let wire = format!(
            r#"{{"aggregate":"provider_field","login":"claude@default","field":"{name}"}}"#
        );
        assert_eq!(serde_json::to_string(&key).unwrap(), wire);
        assert_eq!(serde_json::from_str::<AggregateKey>(&wire).unwrap(), key);
        assert_eq!(
            key.identity(),
            format!("provider_field:claude@default:{name}")
        );
    }

    // A field an older build would have ignored is never how a key is told
    // apart: the year and used-% variants reject nothing but carry no member
    // a reader could miss.
    for wire in [
        r#"{"aggregate":"provider_spend","login":"claude@default","period":"week"}"#,
        r#"{"aggregate":"provider_mana","login":"claude@default","duration_mins":300,"field":"lifted"}"#,
    ] {
        let key: AggregateKey = serde_json::from_str(wire).unwrap();
        let json = serde_json::to_value(&key).unwrap();
        assert!(json.get("period").is_none() && json.get("field").is_none());
    }
}

#[test]
fn aggregate_backstep_round_trips_under_its_key_identity() {
    let wire = r#"{"detector":"aggregate_backstep","aggregate":{"aggregate":"provider_field","login":"claude@default","field":"version"},"from":"2.1.291","to":"2.1.289","pulled":null}"#;
    let anomaly = AnomalyKind::AggregateBackstep {
        aggregate: AggregateKey::ProviderField {
            login: "claude@default".parse().unwrap(),
            field: PanelField::Version,
        },
        from: "2.1.291".to_owned(),
        to: "2.1.289".to_owned(),
        pulled: None,
    };
    assert_eq!(serde_json::to_string(&anomaly).unwrap(), wire);
    assert_eq!(serde_json::from_str::<AnomalyKind>(wire).unwrap(), anomaly);
    assert_eq!(anomaly.key(), "aggregate_backstep");
    assert_eq!(
        anomaly.subject().as_deref(),
        Some("provider_field:claude@default:version")
    );
}

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

#[test]
fn link_alert_event_serializes_tier_as_snake_case() {
    let event = link_alert(LinkTier::Bad, Some(800), 40, 10, None);
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["tier"], "bad");
    assert_eq!(serde_json::from_value::<DiagEvent>(json).unwrap(), event);
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

    assert_eq!(envelope.v, "rimz.diag.v1");
    assert_eq!(envelope.build.as_deref(), crate::build_id::current());
    assert!(envelope.build.is_some());
    assert_eq!(envelope.severity, DiagSeverity::Warn);
    assert_eq!(envelope.suppressed_since_last, 0);
    assert!(envelope.is_current_version());

    let value = serde_json::to_value(&envelope).expect("encode");
    assert_eq!(value["severity"], "warn");
    assert!(value.get("suppressed_since_last").is_none());

    let mut suppressed_envelope = envelope.clone();
    suppressed_envelope.suppressed_since_last = 2;
    let suppressed = serde_json::to_value(suppressed_envelope).expect("encode");
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
fn agent_card_without_process_has_stable_diagnostic_identity() {
    let anomaly = AnomalyKind::AgentCardWithoutProcess {
        row_id: "agent:a".to_owned(),
        pane_id: Some("tmux:%1".to_owned()),
        pid: 123,
        kind: "claude".to_owned(),
    };
    assert_eq!(anomaly.key(), "agent_card_without_process");
    assert_eq!(anomaly.subject().as_deref(), Some("agent:a"));

    let event = DiagEvent::FrameAnomaly {
        anomaly,
        window_ms: None,
        frame: frame_stamp(1),
        events_recent: EventsSig::default(),
        gate_reject_streak: 0,
        health_failure_streak: 0,
        dropped_msgs: 0,
    };
    assert_eq!(event.severity(), DiagSeverity::Warn);
    assert_eq!(
        event.summary(),
        "observed agent_card_without_process on agent:a"
    );

    let mut retained = serde_json::to_value(&event).expect("encode anomaly");
    assert!(retained.get("role").is_none());
    let decoded: DiagEvent = serde_json::from_value(retained.clone()).expect("decode anomaly");
    assert_eq!(decoded, event);
    retained["role"] = serde_json::json!("consumer");
    let decoded: DiagEvent = serde_json::from_value(retained).expect("decode retained anomaly");
    assert_eq!(decoded, event);
}

#[test]
fn severity_table_pins_conditional_and_regression_categories() {
    let info = [
        DiagEvent::SidebarWidthIntent {
            trigger: SidebarWidthIntentTrigger::Narrower,
            own_cols: 40,
            base_cols: 40,
            view_cols: 200,
            step_cols: Some(10),
            step_exact: false,
            target_cols: Some(30),
            verdict: SidebarWidthIntentVerdict::Accepted,
        },
        DiagEvent::SidebarWidthNudge {
            trigger: SidebarWidthControlTrigger::Retarget,
            view_cols: 200,
            from_cols: 40,
            target_cols: 30,
        },
        DiagEvent::SidebarWidthSettle {
            settled_cols: 30,
            learned_step: Some(10),
            outcome: SidebarWidthSettleOutcome::FeedbackLearned,
        },
        DiagEvent::WorkPaneBoundaryMoved {
            view_id: ViewId::new_unchecked("1"),
            view_cols: 213,
            moves: vec![WorkPaneBoundaryMove {
                pane: pane("terminal_2"),
                from_x: 55,
                from_cols: 79,
                to_x: 55,
                to_cols: 47,
            }],
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
        view_cols: 200,
        step_cols: Some(10),
        step_exact: false,
        target_cols: Some(50),
        verdict: SidebarWidthIntentVerdict::Accepted,
    };

    let encoded = serde_json::to_value(&event).expect("encode width intent");
    assert_eq!(encoded["kind"], "sidebar_width_intent");
    assert_eq!(encoded["view_cols"], 200);
    assert_eq!(encoded["target_cols"], 50);
    assert_eq!(
        serde_json::from_value::<DiagEvent>(encoded).expect("decode width intent"),
        event
    );
}

#[test]
fn work_pane_boundary_move_round_trips_and_keys_by_tab() {
    let event = DiagEvent::WorkPaneBoundaryMoved {
        view_id: ViewId::new_unchecked("tab_3"),
        view_cols: 213,
        moves: vec![WorkPaneBoundaryMove {
            pane: pane("terminal_2"),
            from_x: 55,
            from_cols: 79,
            to_x: 55,
            to_cols: 47,
        }],
    };

    let encoded = serde_json::to_value(&event).expect("encode boundary move");
    assert_eq!(encoded["kind"], "work_pane_boundary_moved");
    assert_eq!(encoded["moves"][0]["to_cols"], 47);
    assert_eq!(event.identity_key(), "work_pane_boundary_moved:tab_3");
    assert_eq!(
        serde_json::from_value::<DiagEvent>(encoded).expect("decode boundary move"),
        event,
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
    let subagent = DiagEvent::SubagentOrphanReaped {
        agent_kind: AgentKind::new_unchecked("codex"),
        agent_id: AgentSessionId::from("child"),
        parent_agent_id: AgentSessionId::from("parent"),
        orphaned_at_ms: 900,
    };
    let subagent_failure = DiagEvent::SubagentOrphanRepairFailed {
        agent_kind: AgentKind::new_unchecked("codex"),
        agent_id: AgentSessionId::from("child"),
        parent_agent_id: AgentSessionId::from("parent"),
        orphaned_at_ms: 900,
        error: "pane close failed".to_owned(),
    };
    let digest = DiagEvent::SubagentDigestBackstopped {
        parent_agent_id: AgentSessionId::from("parent"),
        message_id: MessageId::new(),
    };

    let reaped_json = serde_json::to_value(&reaped).expect("encode orphan reap");
    assert_eq!(reaped_json["kind"], "sidebar_orphan_reaped");
    assert_eq!(reaped_json["sigkilled"], true);
    assert_eq!(
        serde_json::from_value::<DiagEvent>(reaped_json).expect("decode orphan reap"),
        reaped
    );
    let subagent_json = serde_json::to_value(&subagent).expect("encode subagent orphan reap");
    assert_eq!(subagent_json["kind"], "subagent_orphan_reaped");
    assert_eq!(subagent.severity(), DiagSeverity::Warn);
    assert_eq!(
        serde_json::from_value::<DiagEvent>(subagent_json).expect("decode subagent orphan reap"),
        subagent
    );
    let digest_json = serde_json::to_value(&digest).expect("encode digest backstop");
    assert_eq!(digest_json["kind"], "subagent_digest_backstopped");
    assert_eq!(digest.severity(), DiagSeverity::Info);
    assert_eq!(
        serde_json::from_value::<DiagEvent>(digest_json).expect("decode digest backstop"),
        digest
    );
    let failure_json =
        serde_json::to_value(&subagent_failure).expect("encode subagent orphan repair failure");
    assert_eq!(failure_json["kind"], "subagent_orphan_repair_failed");
    assert_eq!(subagent_failure.severity(), DiagSeverity::Warn);
    assert_eq!(
        serde_json::from_value::<DiagEvent>(failure_json)
            .expect("decode subagent orphan repair failure"),
        subagent_failure
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
            r#"{"kind":"frame_anomaly","anomaly":{"detector":"aggregate_oscillation","aggregate":{"aggregate":"provider_spend","login":"claude@default"},"from":"1234","via":"0","back":"1234","span_ms":7000,"pulled_via":"0"},"frame":{"produced_at_ms":13000,"rows":2,"agents":2,"processes":0,"pulled_rows":2,"pulled_panes_produced_at_ms":13000},"events_recent":{"pane_closed":[],"pane_opened":[]},"gate_reject_streak":0,"health_failure_streak":0,"dropped_msgs":0}"#,
            DiagEvent::FrameAnomaly {
                anomaly: AnomalyKind::AggregateOscillation {
                    aggregate: AggregateKey::ProviderSpend {
                        login: "claude@default".parse().unwrap(),
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
        (
            r#"{"kind":"frame_anomaly","anomaly":{"detector":"value_oscillation","row_id":"a","field":"status","from":"running","via":"waiting","span_ms":1000},"frame":{"produced_at_ms":13000,"rows":2,"agents":2,"processes":0,"pulled_rows":2,"pulled_panes_produced_at_ms":13000},"events_recent":{"pane_closed":[],"pane_opened":[]},"gate_reject_streak":0,"health_failure_streak":0,"dropped_msgs":0}"#,
            DiagEvent::FrameAnomaly {
                anomaly: AnomalyKind::ValueOscillation {
                    row_id: "a".to_owned(),
                    field: WatchedField::Status,
                    from: "running".to_owned(),
                    via: "waiting".to_owned(),
                    span_ms: 1_000,
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
        login: "plugin@default".parse().unwrap(),
        scope_id: Some("build_minutes".to_owned()),
        duration_mins: None,
    };
    let deployment = AggregateKey::ProviderMana {
        login: "plugin@default".parse().unwrap(),
        scope_id: Some("deployments".to_owned()),
        duration_mins: None,
    };
    assert_ne!(build.identity(), deployment.identity());
    assert_eq!(
        build.identity(),
        "provider_mana:plugin@default:scope:build_minutes"
    );

    let legacy: AggregateKey = serde_json::from_value(serde_json::json!({
        "aggregate": "provider_mana",
        "login": "codex@default",
        "duration_mins": 300
    }))
    .unwrap();
    assert_eq!(legacy.identity(), "provider_mana:codex@default:300");
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
