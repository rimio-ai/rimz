use super::*;

#[test]
fn logical_card_matches_exact_sessions_or_shared_stable_names() {
    let claude = AgentKind::new_unchecked("claude");
    let codex = AgentKind::new_unchecked("codex");
    let launch = AgentSessionId::from("launch_pending");
    let session = AgentSessionId::from("session-live");
    let other = AgentSessionId::from("session-other");

    let provisional = AgentCardRef::new(&claude, &launch, Some("coder"));
    assert!(provisional.matches(AgentCardRef::new(&claude, &session, Some("coder"))));
    assert!(provisional.matches(AgentCardRef::new(&claude, &launch, None)));
    assert!(!provisional.matches(AgentCardRef::new(&claude, &other, Some("reviewer"))));
    assert!(!provisional.matches(AgentCardRef::new(&codex, &session, Some("coder"))));
}

#[test]
fn compacting_marker_expires_after_delivery_window() {
    let now = Timestamp::from_second(1_000).unwrap();
    let mut agent = AgentState::stub("claude", "sess-compact", AgentStatus::Idle);
    agent.compacting_since =
        Some(now - jiff::SignedDuration::from_secs(COMPACTING_WINDOW_SECS - 1));
    assert!(agent.is_compacting(now));

    agent.compacting_since = Some(now - jiff::SignedDuration::from_secs(COMPACTING_WINDOW_SECS));
    assert!(!agent.is_compacting(now));
}

#[test]
fn legacy_agent_pid_deserializes_to_runtime_owner() {
    let agent: AgentState = serde_json::from_value(serde_json::json!({
        "agent_id": "sess-1",
        "kind": "codex",
        "status": "running",
        "agent_pid": 4242,
        "agent_process_start": "12345",
        "last_seen": "2026-07-01T00:00:00Z",
        "last_activity": "2026-07-01T00:00:00Z"
    }))
    .expect("legacy agent state");

    let owner = agent.runtime_owner.as_ref().expect("owner synthesized");
    assert_eq!(owner.kind, RuntimeOwnerKind::Agent);
    assert_eq!(owner.subject_id, "sess-1");
    assert_eq!(owner.pid, 4242);
    assert_eq!(owner.process_start.as_deref(), Some("12345"));

    let encoded = serde_json::to_value(&agent).expect("encode");
    assert!(encoded.get("agent_pid").is_none());
    assert!(encoded.get("agent_process_start").is_none());
}

#[test]
fn activity_description_prefers_rich_context_then_fallbacks() {
    let mut agent = test_agent(AgentStatus::Running, 1_000);
    agent.prompt = Some("latest prompt".to_owned());
    agent.first_prompt = Some("first prompt".to_owned());
    agent.task = Some("live task".to_owned());
    agent.description = Some("launch label".to_owned());
    agent.context = Some(AgentContext {
        session_name: Some("thread name".to_owned()),
        session_preview: Some("thread preview".to_owned()),
        ..AgentContext::new("codex", Timestamp::from_second(1_000).unwrap())
    });

    assert_eq!(agent.activity_description(), Some("thread preview"));
    agent.context.as_mut().unwrap().session_preview = None;
    assert_eq!(agent.activity_description(), Some("thread name"));
    agent.context = None;
    assert_eq!(agent.activity_description(), Some("launch label"));
    agent.description = None;
    assert_eq!(agent.activity_description(), Some("live task"));
    agent.task = None;
    assert_eq!(agent.activity_description(), Some("first prompt"));
    agent.first_prompt = None;
    assert_eq!(agent.activity_description(), Some("latest prompt"));
}

#[test]
fn activity_description_rejects_blank_and_control_text() {
    let mut agent = test_agent(AgentStatus::Running, 1_000);
    agent.task = Some(" \n\t".to_owned());
    agent.first_prompt = Some("<system-reminder>synthetic</system-reminder>".to_owned());
    agent.prompt = Some("<task-notification>synthetic</task-notification> real prompt".to_owned());

    assert_eq!(agent.activity_description(), None);
    assert_eq!(
        single_line_description("ship\nwide\tlabel\rnow\u{0007}").as_deref(),
        Some("ship wide label now")
    );
}

#[test]
fn activity_line_collapses_description_whitespace() {
    let mut agent = test_agent(AgentStatus::Running, 1_000);
    agent.description = Some("ship\nwide\tlabel\rnow".to_owned());

    assert_eq!(
        agent.activity_line().as_deref(),
        Some("ship wide label now")
    );
}

/// The context tier climbs calm → yellow → amber → red, taking the worse
/// of two axes — fill percentage and absolute tokens. Defaults: the Yellow
/// tier starts warming at 50% / 128k, amber starts at 80% / 256k, and red
/// starts at 90% / 384k.
#[test]
fn context_severity_takes_the_worse_of_percent_and_tokens() {
    let bands = crate::config::ContextMeterConfig::default();
    let tier = |percent, tokens| ContextSeverity::classify(percent, tokens, &bands);
    // Low fill, low tokens: calm.
    assert_eq!(tier(20, Some(50_000)), ContextSeverity::Calm);
    // Just under both green-start bounds stays calm; the bound itself enters.
    assert_eq!(tier(49, Some(127_999)), ContextSeverity::Calm);
    assert_eq!(tier(50, Some(10_000)), ContextSeverity::Yellow);
    assert_eq!(tier(10, Some(128_000)), ContextSeverity::Yellow);
    // The percentage ramp alone climbs through all four tiers.
    assert_eq!(tier(80, Some(10_000)), ContextSeverity::Amber);
    assert_eq!(tier(90, Some(10_000)), ContextSeverity::Red);
    // Calm by percentage, but the token volume escalates it.
    assert_eq!(tier(20, Some(256_000)), ContextSeverity::Amber);
    assert_eq!(tier(20, Some(384_000)), ContextSeverity::Red);
    // The worse severity wins regardless of which axis it comes from.
    assert_eq!(tier(89, Some(383_999)), ContextSeverity::Amber);
    // No token reading falls back to the percentage ramp alone.
    assert_eq!(tier(80, None), ContextSeverity::Amber);
    assert_eq!(tier(10, None), ContextSeverity::Calm);
    // An out-of-range percent clamps to full and reads red.
    assert_eq!(tier(200, None), ContextSeverity::Red);
    // The tiers order, so a future hook threshold reads naturally.
    assert!(ContextSeverity::Amber > ContextSeverity::Yellow);
}

#[test]
fn effective_turn_error_class_parks_legacy_limit_labels() {
    let turn_error = |label: &str| AgentTurnError {
        class: TurnErrorClass::Failed,
        at: Timestamp::from_second(1_700_000_000).unwrap(),
        label: Some(label.to_owned()),
    };

    assert_eq!(
        effective_turn_error_class(&turn_error("You've hit your monthly spend limit.")),
        TurnErrorClass::PausedSpendLimit
    );
    assert_eq!(
        effective_turn_error_class(&turn_error(
            "You've hit your session limit · resets 10:50am (UTC)"
        )),
        TurnErrorClass::PausedRateLimit
    );
    assert_eq!(
        effective_turn_error_class(&turn_error("API Error: Bad Request")),
        TurnErrorClass::Failed
    );
}

fn test_agent(status: AgentStatus, activity: i64) -> AgentState {
    let at = Timestamp::from_second(activity).unwrap();
    AgentState {
        status,
        ..crate::testkit::agent_state("claude", "sess", at)
    }
}

fn context_error(class: TurnErrorClass, at: i64) -> AgentContext {
    AgentContext {
        turn_error: Some(AgentTurnError {
            class,
            at: Timestamp::from_second(at).unwrap(),
            label: Some("provider parked".to_owned()),
        }),
        ..AgentContext::new("claude", Timestamp::from_second(at).unwrap())
    }
}

fn context_settle(complete: Option<i64>, interrupted: Option<i64>) -> AgentContext {
    AgentContext {
        turn_complete: complete.map(|at| Timestamp::from_second(at).unwrap()),
        turn_interrupted: interrupted.map(|at| Timestamp::from_second(at).unwrap()),
        ..AgentContext::new("codex", Timestamp::from_second(1_000).unwrap())
    }
}

#[test]
fn effective_status_projects_active_provider_parks_to_paused() {
    for class in [
        TurnErrorClass::PausedSpendLimit,
        TurnErrorClass::PausedRateLimit,
        TurnErrorClass::PausedOverloaded,
    ] {
        let mut agent = test_agent(AgentStatus::Running, 1_000);
        agent.context = Some(context_error(class, 1_010));
        assert_eq!(agent.effective_status(), AgentStatus::Paused, "{class:?}");
    }
}

#[test]
fn effective_status_keeps_raw_status_without_active_park() {
    let mut failed = test_agent(AgentStatus::Failed, 1_000);
    failed.turn_started_at = Some(Timestamp::from_second(900).unwrap());
    failed.context = Some(context_error(TurnErrorClass::PausedSpendLimit, 1_010));
    assert_eq!(failed.effective_status(), AgentStatus::Failed);

    let mut running = test_agent(AgentStatus::Running, 1_000);
    running.context = Some(context_error(TurnErrorClass::Failed, 1_010));
    assert_eq!(running.effective_status(), AgentStatus::Running);

    let mut unknown = test_agent(AgentStatus::Running, 1_000);
    unknown.context = Some(context_error(TurnErrorClass::Unknown, 1_010));
    assert_eq!(unknown.effective_status(), AgentStatus::Running);
}

#[test]
fn waiting_and_interruption_outrank_a_budget_park() {
    let mut waiting = test_agent(AgentStatus::Waiting, 1_000);
    waiting.budget_park = Some(crate::harness::budget::BudgetPark {
        cap_usd: 5.0,
        spend_usd: 5.25,
        window: crate::harness::budget::BudgetWindow::Session,
        at: Timestamp::from_second(1_000).unwrap(),
        scope: crate::harness::budget::BudgetScope::Agent,
        account_kind: None,
        resets_at: None,
    });
    assert_eq!(waiting.effective_status(), AgentStatus::Waiting);

    waiting.context = Some(context_settle(None, Some(1_010)));
    assert_eq!(waiting.effective_status(), AgentStatus::Idle);
}

#[test]
fn effective_status_projects_hookless_turn_settle_markers() {
    let mut plan = test_agent(AgentStatus::Running, 1_000);
    let mut plan_context = context_settle(None, None);
    plan_context.plan_proposed = Some(Timestamp::from_second(1_010).unwrap());
    plan.context = Some(plan_context);
    assert_eq!(plan.effective_status(), AgentStatus::Waiting);
    assert!(plan.is_awaiting_input());

    let mut native = test_agent(AgentStatus::Running, 1_000);
    let mut native_context = context_settle(None, None);
    native_context.native_permission_wait = Some(Timestamp::from_second(1_010).unwrap());
    native.context = Some(native_context);
    assert_eq!(native.effective_status(), AgentStatus::Waiting);
    assert!(native.is_awaiting_input());

    let mut stale_native = test_agent(AgentStatus::Running, 1_000);
    let mut stale_native_context = context_settle(None, None);
    stale_native_context.native_permission_wait = Some(Timestamp::from_second(990).unwrap());
    stale_native.context = Some(stale_native_context);
    assert_eq!(stale_native.effective_status(), AgentStatus::Running);
    assert!(!stale_native.is_awaiting_input());

    let mut stale_plan = test_agent(AgentStatus::Running, 1_000);
    let mut stale_plan_context = context_settle(None, None);
    stale_plan_context.plan_proposed = Some(Timestamp::from_second(990).unwrap());
    stale_plan.context = Some(stale_plan_context);
    assert_eq!(stale_plan.effective_status(), AgentStatus::Running);
    assert!(!stale_plan.is_awaiting_input());

    let mut complete = test_agent(AgentStatus::Running, 1_000);
    complete.context = Some(context_settle(Some(1_010), None));
    assert_eq!(complete.effective_status(), AgentStatus::Success);

    let mut interrupted = test_agent(AgentStatus::Running, 1_000);
    interrupted.context = Some(context_settle(None, Some(1_010)));
    assert_eq!(interrupted.effective_status(), AgentStatus::Idle);

    let mut interrupted_waiting = test_agent(AgentStatus::Waiting, 1_000);
    interrupted_waiting.context = Some(context_settle(None, Some(1_010)));
    assert_eq!(interrupted_waiting.effective_status(), AgentStatus::Idle);

    let mut stale_waiting = test_agent(AgentStatus::Waiting, 1_000);
    stale_waiting.context = Some(context_settle(None, Some(990)));
    assert_eq!(stale_waiting.effective_status(), AgentStatus::Waiting);

    let mut stale = test_agent(AgentStatus::Running, 1_000);
    stale.context = Some(context_settle(Some(990), Some(990)));
    assert_eq!(stale.effective_status(), AgentStatus::Running);

    let mut non_running = test_agent(AgentStatus::Idle, 1_000);
    non_running.context = Some(context_settle(Some(1_010), Some(1_010)));
    assert_eq!(non_running.effective_status(), AgentStatus::Idle);

    let mut parked = test_agent(AgentStatus::Running, 1_000);
    let mut context = context_error(TurnErrorClass::PausedRateLimit, 1_010);
    context.turn_complete = Some(Timestamp::from_second(1_010).unwrap());
    context.turn_interrupted = Some(Timestamp::from_second(1_010).unwrap());
    parked.context = Some(context);
    assert_eq!(parked.effective_status(), AgentStatus::Paused);
}

#[test]
fn keyed_wait_outranks_newer_activity_while_keyless_wait_self_clears() {
    let waiting_since = Timestamp::from_second(1_000).unwrap();
    let mut keyed = test_agent(AgentStatus::Waiting, 1_010);
    keyed.waiting_since = Some(waiting_since);
    keyed.open_ask = Some(OpenAsk {
        id: AskId::parse("ask_0123456789abcdef").unwrap(),
        kind: AskKind::Question,
        detail: Some("Which route?".to_owned()),
        native_key: Some("ask-call".to_owned()),
        since: waiting_since,
    });
    assert!(keyed.is_awaiting_input());

    keyed.waiting_since = None;
    assert!(keyed.is_awaiting_input());
    keyed.waiting_since = Some(waiting_since);

    let mut keyless = keyed;
    keyless.open_ask.as_mut().unwrap().native_key = None;
    assert!(!keyless.is_awaiting_input());
}

#[test]
fn displayed_turn_error_projects_active_running_marker() {
    let mut agent = test_agent(AgentStatus::Running, 1_000);
    agent.context = Some(context_error(TurnErrorClass::PausedOverloaded, 1_010));

    assert_eq!(
        agent.displayed_turn_error(),
        Some((TurnErrorClass::PausedOverloaded, Some("provider parked")))
    );
}

#[test]
fn displayed_turn_error_projects_terminal_marker_in_current_turn() {
    let mut agent = test_agent(AgentStatus::Failed, 1_100);
    agent.turn_started_at = Some(Timestamp::from_second(1_000).unwrap());
    agent.context = Some(context_error(TurnErrorClass::Failed, 1_010));

    assert_eq!(
        agent.displayed_turn_error(),
        Some((TurnErrorClass::Failed, Some("provider parked")))
    );
}

#[test]
fn displayed_turn_error_self_clears_when_marker_is_stale() {
    let mut running = test_agent(AgentStatus::Running, 1_100);
    running.context = Some(context_error(TurnErrorClass::PausedOverloaded, 1_000));
    assert_eq!(running.displayed_turn_error(), None);

    let mut failed = test_agent(AgentStatus::Failed, 1_100);
    failed.turn_started_at = Some(Timestamp::from_second(1_050).unwrap());
    failed.context = Some(context_error(TurnErrorClass::Failed, 1_000));
    assert_eq!(failed.displayed_turn_error(), None);
}

/// The bands come from `[theme.display.context_meter]`, so a custom set moves every
/// edge; a misordered set degrades to the highest matching tier (the red
/// band is checked first), never to a calmer one.
#[test]
fn context_severity_honours_custom_and_misordered_bands() {
    use crate::config::{ContextBand, ContextMeterConfig};
    let tight = ContextMeterConfig {
        green: ContextBand {
            percent: 10,
            tokens: 1_000,
        },
        yellow: ContextBand {
            percent: 20,
            tokens: 2_000,
        },
        amber: ContextBand {
            percent: 30,
            tokens: 3_000,
        },
        red: ContextBand {
            percent: 40,
            tokens: 4_000,
        },
        ..ContextMeterConfig::default()
    };
    assert_eq!(
        ContextSeverity::classify(5, Some(500), &tight),
        ContextSeverity::Calm
    );
    assert_eq!(
        ContextSeverity::classify(25, Some(0), &tight),
        ContextSeverity::Yellow
    );
    assert_eq!(
        ContextSeverity::classify(35, Some(0), &tight),
        ContextSeverity::Amber
    );
    assert_eq!(
        ContextSeverity::classify(5, Some(4_000), &tight),
        ContextSeverity::Red
    );

    // Red configured *below* yellow: a mid fill reaches the red band even
    // though the calmer tiers do not — worst-first keeps the warning loud.
    let misordered = ContextMeterConfig {
        green: ContextBand {
            percent: 95,
            tokens: 950_000,
        },
        yellow: ContextBand {
            percent: 90,
            tokens: 900_000,
        },
        amber: ContextBand {
            percent: 80,
            tokens: 800_000,
        },
        red: ContextBand {
            percent: 50,
            tokens: 500_000,
        },
        ..ContextMeterConfig::default()
    };
    assert_eq!(
        ContextSeverity::classify(60, None, &misordered),
        ContextSeverity::Red
    );
}

/// Pins the signal's wire shape now, so the first emitter and handler
/// build against a stable contract rather than re-negotiating it.
#[test]
fn agent_signal_serializes_to_a_tagged_wire_shape() {
    assert_eq!(
        serde_json::to_value(AgentSignal::ContextSeverity {
            from: ContextSeverity::Yellow,
            to: ContextSeverity::Amber,
        })
        .unwrap(),
        serde_json::json!({
            "kind": "context_severity",
            "from": "yellow",
            "to": "amber",
        })
    );
    assert_eq!(
        serde_json::to_value(AgentSignal::Attention {
            status: AgentStatus::Waiting,
        })
        .unwrap(),
        serde_json::json!({ "kind": "attention", "status": "waiting" })
    );
}

#[test]
fn attention_predicates_split_actionable_from_parked() {
    // The two intentional flavors: ranking spans the parked Paused,
    // the triage/heat subset does not. Calm states are in neither.
    for status in [AgentStatus::Waiting, AgentStatus::Failed] {
        assert!(status.is_attention());
        assert!(status.is_actionable());
        assert!(status.needs_a_look());
    }
    assert!(AgentStatus::Paused.is_attention());
    assert!(!AgentStatus::Paused.is_actionable());
    assert!(AgentStatus::Paused.needs_a_look());
    assert!(!AgentStatus::Success.is_attention());
    assert!(!AgentStatus::Success.is_actionable());
    assert!(AgentStatus::Success.needs_a_look());
    for status in [AgentStatus::Running, AgentStatus::Idle] {
        assert!(!status.is_attention());
        assert!(!status.is_actionable());
        assert!(!status.needs_a_look());
    }
}

#[test]
fn agent_status_round_trips_including_paused() {
    for status in [
        AgentStatus::Running,
        AgentStatus::Waiting,
        AgentStatus::Idle,
        AgentStatus::Success,
        AgentStatus::Failed,
        AgentStatus::Paused,
    ] {
        let wire = serde_json::to_string(&status).unwrap();
        let back: AgentStatus = serde_json::from_str(&wire).unwrap();
        assert_eq!(status, back);
    }
    // The derived state has a stable snake_case wire form like the rest.
    assert_eq!(
        serde_json::to_string(&AgentStatus::Paused).unwrap(),
        "\"paused\""
    );
}
