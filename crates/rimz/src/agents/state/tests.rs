use super::*;
use crate::agents::TurnSettle;

#[test]
fn seed_sets_status_phase_clocks_and_empty_enrichment() {
    let at = Timestamp::from_second(1_700_000_000).unwrap();
    let running = AgentState::seed(
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("sess-running"),
        AgentStatus::Running,
        at,
    );

    assert_eq!(running.status, AgentStatus::Running);
    assert_eq!(running.phase, TurnPhase::Reasoning);
    assert_eq!(running.last_seen, at);
    assert_eq!(running.last_activity, at);
    assert_eq!(running.registered_at, Some(at));
    assert!(running.name.is_none());
    assert!(running.pane.is_none());
    assert!(running.runtime_owner.is_none());
    assert!(running.recent_prompts.is_empty());
    assert!(running.usage.context_pct.is_none());
    assert!(running.usage.context_window.is_none());
    assert!(running.usage.total_tokens.is_none());
    assert!(running.context.is_none());
    assert!(running.budget_park.is_none());
    assert!(running.subagent_description.is_none());
    assert!(running.open_ask.is_none());
    assert_eq!(running.compaction_count, 0);
    assert!(running.tool_calls.is_empty());
    assert!(running.tool_repeat.is_none());

    let waiting = AgentState::seed(
        AgentKind::new_unchecked("codex"),
        AgentSessionId::from("sess-waiting"),
        AgentStatus::Waiting,
        at,
    );
    assert_eq!(waiting.phase, TurnPhase::Idle);
}

#[test]
fn tool_looping_requires_running_status_and_threshold() {
    let repeat = ToolRepeat {
        digest: "digest".to_owned(),
        tool: "Bash".to_owned(),
        count: 20,
        since: Timestamp::from_second(1_700_000_000).unwrap(),
    };

    assert!(!is_tool_looping(AgentStatus::Running, None, 20));
    assert!(!is_tool_looping(
        AgentStatus::Running,
        Some(&ToolRepeat {
            count: 19,
            ..repeat.clone()
        }),
        20
    ));
    assert!(is_tool_looping(AgentStatus::Running, Some(&repeat), 20));
    assert!(!is_tool_looping(AgentStatus::Idle, Some(&repeat), 20));
    assert!(!is_tool_looping(AgentStatus::Failed, Some(&repeat), 20));
}

#[test]
fn agent_status_labels_are_stable() {
    assert_eq!(AgentStatus::Running.as_str(), "running");
    assert_eq!(AgentStatus::Waiting.as_str(), "waiting");
    assert_eq!(AgentStatus::Idle.as_str(), "idle");
    assert_eq!(AgentStatus::Success.as_str(), "success");
    assert_eq!(AgentStatus::Failed.as_str(), "failed");
    assert_eq!(AgentStatus::Paused.as_str(), "paused");
}

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
    assert_eq!(agent.ended_at, None);

    let encoded = serde_json::to_value(&agent).expect("encode");
    assert!(encoded.get("agent_pid").is_none());
    assert!(encoded.get("agent_process_start").is_none());
    assert!(encoded.get("ended_at").is_none());
}

#[test]
fn usage_summary_stays_flat_in_persisted_state_wire() {
    let mut agent = AgentState::seed(
        AgentKind::new_unchecked("codex"),
        AgentSessionId::from("sess-usage"),
        AgentStatus::Running,
        Timestamp::from_second(1_700_000_000).unwrap(),
    );
    agent.usage = AgentUsageSummary {
        context_pct: Some(42),
        context_window: Some(200_000),
        total_tokens: Some(84_000),
        cache_read_input_tokens: Some(60_000),
        cache_write_input_tokens: Some(4_000),
        fresh_input_tokens: Some(20_000),
        output_tokens: Some(1_000),
    };

    let encoded = serde_json::to_value(&agent).expect("encode state");
    assert!(encoded.get("usage").is_none());
    for key in [
        "context_pct",
        "context_window",
        "total_tokens",
        "cache_read_input_tokens",
        "cache_write_input_tokens",
        "fresh_input_tokens",
        "output_tokens",
    ] {
        assert!(encoded.get(key).is_some(), "missing flat key {key}");
    }
    let decoded: AgentState = serde_json::from_value(encoded).expect("decode persisted state");
    assert_eq!(decoded.usage, agent.usage);
}

#[test]
fn tool_calls_round_trip_and_default_for_legacy_state() {
    let mut agent = AgentState::seed(
        AgentKind::new_unchecked("claude"),
        AgentSessionId::from("sess-tools"),
        AgentStatus::Running,
        Timestamp::from_second(1_700_000_000).unwrap(),
    );
    agent.tool_calls = BTreeMap::from([("Bash".to_owned(), 2), ("Read".to_owned(), 3)]);

    let encoded = serde_json::to_value(&agent).expect("encode state");
    assert_eq!(encoded["tool_calls"]["Read"], 3);
    let decoded: AgentState = serde_json::from_value(encoded).expect("decode state");
    assert_eq!(decoded.tool_calls, agent.tool_calls);

    let legacy: AgentState = serde_json::from_value(serde_json::json!({
        "agent_id": "sess-legacy",
        "kind": "claude",
        "status": "idle",
        "last_seen": "2026-07-01T00:00:00Z",
        "last_activity": "2026-07-01T00:00:00Z"
    }))
    .expect("legacy state");
    assert!(legacy.tool_calls.is_empty());
}

#[test]
fn activity_description_prefers_rich_context_then_fallbacks() {
    let mut agent = test_agent(AgentStatus::Running, 1_000);
    agent.prompt = Some("latest prompt".to_owned());
    agent.first_prompt = Some("first prompt with detail".to_owned());
    agent.task = Some("live task".to_owned());
    agent.description = Some("launch label".to_owned());
    agent.context = Some(AgentContext {
        session_name: Some("thread name".to_owned()),
        session_preview: Some("thread preview".to_owned()),
        ..AgentContext::new("codex", Timestamp::from_second(1_000).unwrap())
    });

    assert_eq!(agent.activity_description(), Some("thread name"));
    agent.context.as_mut().unwrap().session_name = Some("first\n  prompt".to_owned());
    assert_eq!(agent.activity_description(), Some("thread preview"));
    agent.context.as_mut().unwrap().session_name = Some("First prompt".to_owned());
    assert_eq!(agent.activity_description(), Some("First prompt"));
    agent.context.as_mut().unwrap().session_name = None;
    assert_eq!(agent.activity_description(), Some("thread preview"));
    agent.context = None;
    assert_eq!(agent.activity_description(), Some("launch label"));
    agent.description = None;
    assert_eq!(agent.activity_description(), Some("live task"));
    agent.task = None;
    assert_eq!(
        agent.activity_description(),
        Some("first prompt with detail")
    );
    agent.first_prompt = None;
    assert_eq!(agent.activity_description(), Some("latest prompt"));
}

#[test]
fn activity_description_checks_latest_prompt_when_first_prompt_is_absent() {
    let mut agent = test_agent(AgentStatus::Running, 1_000);
    agent.prompt = Some("latest prompt with detail".to_owned());
    agent.context = Some(AgentContext {
        session_name: Some("latest\tprompt".to_owned()),
        session_preview: Some("thread preview".to_owned()),
        ..AgentContext::new("codex", Timestamp::from_second(1_000).unwrap())
    });

    assert_eq!(agent.activity_description(), Some("thread preview"));
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

fn context_settle(at: Option<i64>, outcome: TurnSettleOutcome) -> AgentContext {
    AgentContext {
        settle: at.map(|at| TurnSettle::new(Timestamp::from_second(at).unwrap(), outcome)),
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
fn effective_status_settles_background_park_to_success() {
    let mut parked = test_agent(AgentStatus::Running, 1_000);
    parked.phase = TurnPhase::Parked;
    assert_eq!(parked.effective_status(), AgentStatus::Success);

    parked.phase = TurnPhase::Reasoning;
    assert_eq!(parked.effective_status(), AgentStatus::Running);
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

    waiting.context = Some(context_settle(Some(1_010), TurnSettleOutcome::Interrupted));
    assert_eq!(waiting.effective_status(), AgentStatus::Idle);
}

#[test]
fn effective_status_projects_hookless_turn_settle_markers() {
    let mut plan = test_agent(AgentStatus::Running, 1_000);
    let plan_context = context_settle(Some(1_010), TurnSettleOutcome::PlanProposed);
    plan.context = Some(plan_context);
    assert_eq!(plan.effective_status(), AgentStatus::Waiting);
    assert!(plan.is_awaiting_input());

    let mut native = test_agent(AgentStatus::Running, 1_000);
    let native_context = context_settle(Some(1_010), TurnSettleOutcome::NativeWait);
    native.context = Some(native_context);
    assert_eq!(native.effective_status(), AgentStatus::Waiting);
    assert!(native.is_awaiting_input());

    let mut stale_native = test_agent(AgentStatus::Running, 1_000);
    let stale_native_context = context_settle(Some(990), TurnSettleOutcome::NativeWait);
    stale_native.context = Some(stale_native_context);
    assert_eq!(stale_native.effective_status(), AgentStatus::Running);
    assert!(!stale_native.is_awaiting_input());

    let mut stale_plan = test_agent(AgentStatus::Running, 1_000);
    let stale_plan_context = context_settle(Some(990), TurnSettleOutcome::PlanProposed);
    stale_plan.context = Some(stale_plan_context);
    assert_eq!(stale_plan.effective_status(), AgentStatus::Running);
    assert!(!stale_plan.is_awaiting_input());

    let mut complete = test_agent(AgentStatus::Running, 1_000);
    complete.context = Some(context_settle(Some(1_010), TurnSettleOutcome::Complete));
    assert_eq!(complete.effective_status(), AgentStatus::Success);

    let mut interrupted = test_agent(AgentStatus::Running, 1_000);
    interrupted.context = Some(context_settle(Some(1_010), TurnSettleOutcome::Interrupted));
    assert_eq!(interrupted.effective_status(), AgentStatus::Idle);

    let mut interrupted_waiting = test_agent(AgentStatus::Waiting, 1_000);
    interrupted_waiting.context = Some(context_settle(Some(1_010), TurnSettleOutcome::Interrupted));
    assert_eq!(interrupted_waiting.effective_status(), AgentStatus::Idle);

    let mut stale_waiting = test_agent(AgentStatus::Waiting, 1_000);
    stale_waiting.context = Some(context_settle(Some(990), TurnSettleOutcome::Interrupted));
    assert_eq!(stale_waiting.effective_status(), AgentStatus::Waiting);

    let mut stale = test_agent(AgentStatus::Running, 1_000);
    stale.context = Some(context_settle(Some(990), TurnSettleOutcome::Complete));
    assert_eq!(stale.effective_status(), AgentStatus::Running);

    let mut non_running = test_agent(AgentStatus::Idle, 1_000);
    non_running.context = Some(context_settle(Some(1_010), TurnSettleOutcome::Complete));
    assert_eq!(non_running.effective_status(), AgentStatus::Idle);

    let mut parked = test_agent(AgentStatus::Running, 1_000);
    let mut context = context_error(TurnErrorClass::PausedRateLimit, 1_010);
    context.settle = Some(TurnSettle::new(
        Timestamp::from_second(1_010).unwrap(),
        TurnSettleOutcome::Complete,
    ));
    parked.context = Some(context);
    assert_eq!(parked.effective_status(), AgentStatus::Paused);
}

#[test]
fn open_turn_predicate_follows_lifecycle_and_rest_certificates() {
    for status in [AgentStatus::Running, AgentStatus::Waiting] {
        assert!(test_agent(status, 1_000).holds_open_turn(), "{status:?}");
    }
    for status in [
        AgentStatus::Idle,
        AgentStatus::Success,
        AgentStatus::Failed,
        AgentStatus::Paused,
    ] {
        assert!(!test_agent(status, 1_000).holds_open_turn(), "{status:?}");
    }

    for class in [
        TurnErrorClass::PausedSpendLimit,
        TurnErrorClass::PausedRateLimit,
        TurnErrorClass::PausedOverloaded,
        TurnErrorClass::Unknown,
        TurnErrorClass::Failed,
    ] {
        let mut agent = test_agent(AgentStatus::Running, 1_000);
        agent.context = Some(context_error(class, 1_010));
        assert!(!agent.holds_open_turn(), "{class:?}");

        agent.last_activity = Timestamp::from_second(1_010).unwrap();
        assert!(agent.holds_open_turn(), "stale {class:?}");
    }

    for outcome in [TurnSettleOutcome::Complete, TurnSettleOutcome::Interrupted] {
        let mut agent = test_agent(AgentStatus::Running, 1_000);
        agent.context = Some(context_settle(Some(1_010), outcome));
        assert!(!agent.holds_open_turn(), "{outcome:?}");
    }
    for outcome in [
        TurnSettleOutcome::PlanProposed,
        TurnSettleOutcome::NativeWait,
    ] {
        let mut agent = test_agent(AgentStatus::Running, 1_000);
        agent.context = Some(context_settle(Some(1_010), outcome));
        assert!(agent.holds_open_turn(), "{outcome:?}");
    }

    let mut waiting = test_agent(AgentStatus::Waiting, 1_000);
    waiting.context = Some(context_settle(Some(1_010), TurnSettleOutcome::Interrupted));
    assert!(!waiting.holds_open_turn());

    let mut parked = test_agent(AgentStatus::Running, 1_000);
    parked.phase = TurnPhase::Parked;
    assert!(!parked.holds_open_turn());

    let mut budget_parked = test_agent(AgentStatus::Running, 1_000);
    budget_parked.budget_park = Some(crate::harness::budget::BudgetPark {
        cap_usd: 5.0,
        spend_usd: 5.25,
        window: crate::harness::budget::BudgetWindow::Session,
        at: Timestamp::from_second(1_000).unwrap(),
        scope: crate::harness::budget::BudgetScope::Agent,
        account_kind: None,
        resets_at: None,
    });
    assert!(!budget_parked.holds_open_turn());
}

#[test]
fn an_open_turn_projects_to_running_or_waiting() {
    for mut agent in [
        test_agent(AgentStatus::Running, 1_000),
        test_agent(AgentStatus::Waiting, 1_000),
    ] {
        assert!(agent.holds_open_turn());
        assert!(matches!(
            agent.effective_status(),
            AgentStatus::Running | AgentStatus::Waiting
        ));

        agent.context = Some(context_settle(Some(1_010), TurnSettleOutcome::PlanProposed));
        if agent.holds_open_turn() {
            assert!(matches!(
                agent.effective_status(),
                AgentStatus::Running | AgentStatus::Waiting
            ));
        }
    }
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
