use super::*;

#[test]
fn field_patch_keeps_sets_and_clears_optional_values() {
    let mut value = Some("prior".to_owned());
    FieldPatch::Keep.apply(&mut value);
    assert_eq!(value.as_deref(), Some("prior"));
    FieldPatch::Set("next".to_owned()).apply(&mut value);
    assert_eq!(value.as_deref(), Some("next"));
    FieldPatch::Clear.apply(&mut value);
    assert_eq!(value, None);
}

#[test]
fn local_token_patches_preserve_established_and_replace_current_usage() {
    let definition = crate::agents::spec_by_kind("codex").expect("Codex definition is registered");
    let exact_window = definition.default_context_window.unwrap() - 1_000;
    let mut context = AgentContext::new("codex", Timestamp::now());
    context.model_id = Some("gpt-5.5".to_owned());
    context.tokens = Some(AgentTokenUsage {
        context_window_size: Some(exact_window),
        used_percentage: Some(42),
        current_usage: Some(AgentCurrentUsage {
            input_tokens: Some(100),
            ..AgentCurrentUsage::default()
        }),
        session_usage: Some(AgentSessionUsage {
            input_tokens: Some(500),
            output_tokens: Some(20),
            ..AgentSessionUsage::default()
        }),
        ..AgentTokenUsage::default()
    });

    LocalContextPatch {
        tokens: LocalTokenPatch::PreserveEstablished(Some(AgentTokenUsage {
            context_window_size: definition.default_context_window,
            current_usage: Some(AgentCurrentUsage::default()),
            ..AgentTokenUsage::default()
        })),
        ..LocalContextPatch::default()
    }
    .apply(&mut context, definition);
    let preserved = context.tokens.as_ref().unwrap();
    assert_eq!(preserved.used_percentage, Some(42));
    assert_eq!(preserved.context_window_size, Some(exact_window));

    LocalContextPatch {
        tokens: LocalTokenPatch::ReplaceCurrentPreservingSession(Some(AgentTokenUsage {
            current_usage: Some(AgentCurrentUsage {
                input_tokens: Some(7),
                ..AgentCurrentUsage::default()
            }),
            session_usage: Some(AgentSessionUsage {
                input_tokens: Some(450),
                output_tokens: Some(25),
                ..AgentSessionUsage::default()
            }),
            ..AgentTokenUsage::default()
        })),
        ..LocalContextPatch::default()
    }
    .apply(&mut context, definition);
    let replaced = context.tokens.as_ref().unwrap();
    assert_eq!(replaced.context_window_size, Some(exact_window));
    assert_eq!(
        replaced.current_usage.as_ref().unwrap().input_tokens,
        Some(7)
    );
    assert_eq!(
        replaced.session_usage.as_ref().unwrap().input_tokens,
        Some(500)
    );
    assert_eq!(
        replaced.session_usage.as_ref().unwrap().output_tokens,
        Some(25)
    );

    LocalContextPatch {
        model_id: FieldPatch::Set("gpt-next".to_owned()),
        tokens: LocalTokenPatch::ReplaceCurrentPreservingSession(None),
        ..LocalContextPatch::default()
    }
    .apply(&mut context, definition);
    let cleared = context.tokens.as_ref().unwrap();
    assert_eq!(cleared.context_window_size, None);
    assert_eq!(cleared.current_usage, None);
    assert!(cleared.session_usage.is_some());
}

#[test]
fn authoritative_current_clears_unestablished_tokens_but_preserves_a_real_gauge() {
    let definition =
        crate::agents::spec_by_kind("cursor").expect("Cursor definition is registered");
    let mut fresh = AgentContext::new("cursor", Timestamp::now());
    fresh.tokens = Some(AgentTokenUsage {
        current_usage: Some(AgentCurrentUsage::default()),
        ..AgentTokenUsage::default()
    });

    LocalContextPatch::authoritative_current().apply(&mut fresh, definition);
    assert_eq!(fresh.tokens, None);

    let established = AgentTokenUsage {
        used_percentage: Some(42),
        ..AgentTokenUsage::default()
    };
    fresh.tokens = Some(established.clone());
    LocalContextPatch::authoritative_current().apply(&mut fresh, definition);
    assert_eq!(fresh.tokens, Some(established));
}

#[test]
fn percentage_clamp_rejects_non_finite_values_and_bounds_finite_values() {
    assert_eq!(clamp_pct(Some(f64::NAN)), None);
    assert_eq!(clamp_pct(Some(-1.0)), Some(0));
    assert_eq!(clamp_pct(Some(99.5)), Some(100));
    assert_eq!(clamp_pct(Some(101.0)), Some(100));
}

#[test]
fn cost_coverage_controls_additive_spend_and_wire_shape() {
    let session = AgentCost {
        total_cost_usd: Some(1.0),
        ..AgentCost::default()
    };
    assert!(session.coverage.contributes_to_live_spend());
    assert_eq!(
        serde_json::to_value(&session).unwrap(),
        serde_json::json!({"total_cost_usd": 1.0})
    );

    let current_usage = AgentCost {
        total_cost_usd: Some(1.0),
        coverage: CostCoverage::CurrentUsage,
        ..AgentCost::default()
    };
    assert!(!current_usage.coverage.contributes_to_live_spend());
    assert_eq!(
        serde_json::to_value(&current_usage).unwrap(),
        serde_json::json!({"total_cost_usd": 1.0, "coverage": "current_usage"})
    );

    let legacy_with_basis: AgentCost = serde_json::from_value(
        serde_json::json!({"total_cost_usd": 1.0, "basis": "locally_priced"}),
    )
    .unwrap();
    assert_eq!(legacy_with_basis.coverage, CostCoverage::Session);
}

fn window(used: Option<u8>) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage: used,
        resets_at: None,
        duration_mins: Some(300),
        ..Default::default()
    }
}

#[test]
fn window_is_spent_only_at_the_cap() {
    assert!(window(Some(100)).is_spent());
    assert!(!window(Some(99)).is_spent());
    assert!(!window(Some(0)).is_spent());
    // An unreported window is not a spent one.
    assert!(!window(None).is_spent());
}

#[test]
fn window_not_started_keys_on_reset_distance_above_the_floor() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let full = SignedDuration::from_secs(300 * 60);
    let started = |used, reset| RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(reset),
        duration_mins: Some(300),
        ..Default::default()
    };

    // Reset slid a full window out at the ~1% floor — the clock has not begun.
    assert!(started(1, now.checked_add(full).unwrap()).not_started(now));
    // Any usage above the floor is a clearly-started window, reset notwithstanding.
    assert!(!started(2, now.checked_add(full).unwrap()).not_started(now));
    // A reset that has ticked well below full is a real countdown — started.
    assert!(
        !started(1, now.checked_add(SignedDuration::from_secs(3600)).unwrap()).not_started(now)
    );
    // An absent reset or duration can't be judged, so it reads as started.
    assert!(!window(Some(0)).not_started(now));
}

#[test]
fn scoped_window_identity_projection_and_wire_round_trip() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let scope = RateLimitWindowScope {
        id: "build_minutes".to_owned(),
        label: "bld".to_owned(),
    };
    let window = RateLimitWindow {
        scope: Some(scope.clone()),
        used_percentage: Some(40),
        resets_at: Some(now - SignedDuration::from_secs(1)),
        duration_mins: None,
        ..Default::default()
    };
    assert_eq!(
        window.key(),
        RateLimitWindowKey::Scope("build_minutes".to_owned())
    );
    assert_eq!(window.clone().projected_at(now), window);

    let encoded = serde_json::to_value(&window).unwrap();
    assert_eq!(encoded["scope"]["id"], "build_minutes");
    assert_eq!(encoded["scope"]["label"], "bld");
    assert!(encoded.get("share_pct").is_none());
    assert_eq!(
        serde_json::from_value::<RateLimitWindow>(encoded).unwrap(),
        window
    );
    assert_eq!(
        serde_json::from_value::<RateLimitWindow>(serde_json::json!({
            "used_percentage": 20,
            "duration_mins": 300
        }))
        .unwrap()
        .scope,
        None,
        "legacy duration-only windows remain wire-compatible"
    );
    let sub_cap = RateLimitWindow {
        duration_mins: Some(10_080),
        ..window
    };
    let mut encoded = serde_json::to_value(&sub_cap).unwrap();
    assert!(encoded.get("share_pct").is_none());
    encoded["share_pct"] = serde_json::json!(50);
    assert_eq!(
        serde_json::from_value::<RateLimitWindow>(encoded).unwrap(),
        sub_cap,
        "existing cache windows ignore the retired share field"
    );
    assert_eq!(
        sub_cap
            .clone()
            .projected_at(now - SignedDuration::from_secs(2)),
        sub_cap
    );
    assert_eq!(sub_cap.clone().projected_at(now), sub_cap);
    let parent = RateLimitWindow {
        scope: None,
        ..sub_cap
    };
    let projected = parent.projected_at(now);
    assert_eq!(projected.used_percentage, Some(0));
    assert_eq!(
        projected.resets_at,
        Some(now + SignedDuration::from_secs(10_080 * 60))
    );
}

#[test]
fn sub_cap_parent_matching() {
    let parent = RateLimitWindow {
        duration_mins: Some(10_080),
        ..Default::default()
    };
    let mut sub_cap = RateLimitWindow {
        scope: Some(RateLimitWindowScope {
            id: "model:fable".to_owned(),
            label: "Fable".to_owned(),
        }),
        duration_mins: parent.duration_mins,
        used_percentage: Some(58),
        ..Default::default()
    };
    assert!(sub_cap.sub_cap_of(&parent));
    assert!(!parent.sub_cap_of(&sub_cap));
    assert!(!sub_cap.sub_cap_of(&sub_cap));
    sub_cap.duration_mins = Some(300);
    assert!(!sub_cap.sub_cap_of(&parent));
    sub_cap.duration_mins = None;
    assert!(!sub_cap.sub_cap_of(&parent));
    assert!(!parent.sub_cap_of(&parent));
}

#[test]
fn scoped_reset_is_content_staleness_fallback_only_without_a_duration_clock() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let scoped = |reset| RateLimitWindow {
        scope: Some(RateLimitWindowScope {
            id: "deployments".to_owned(),
            label: "dep".to_owned(),
        }),
        used_percentage: Some(20),
        resets_at: Some(reset),
        ..Default::default()
    };
    assert!(
        AgentRateLimits {
            windows: vec![scoped(now - SignedDuration::from_secs(1))]
        }
        .content_stale_at(now)
    );

    let limits = AgentRateLimits {
        windows: vec![
            scoped(now - SignedDuration::from_secs(1)),
            RateLimitWindow {
                duration_mins: Some(300),
                resets_at: Some(now + SignedDuration::from_secs(60)),
                ..Default::default()
            },
        ],
    };
    assert!(
        !limits.content_stale_at(now),
        "a real duration clock remains the primary freshness signal"
    );
}

#[test]
fn current_usage_token_accounting() {
    // used_tokens sums the current message's window composition, excluding
    // output (it joins the window only next turn), and is None before the
    // first API call.
    assert_eq!(AgentTokenUsage::default().used_tokens(), None);
    let tokens = AgentTokenUsage {
        context_window_size: Some(1_000_000),
        used_percentage: Some(30),
        current_usage: Some(AgentCurrentUsage {
            input_tokens: Some(5_000),
            output_tokens: Some(9_999),
            cache_creation_input_tokens: Some(100_000),
            cache_read_input_tokens: Some(200_000),
        }),
        ..AgentTokenUsage::default()
    };
    assert_eq!(tokens.used_tokens(), Some(305_000));
    let mut scalar = tokens.clone();
    scalar.current_context_tokens = Some(42);
    assert_eq!(scalar.used_tokens(), Some(42));
    assert_eq!(
        serde_json::to_value(&scalar).unwrap()["current_context_tokens"],
        42
    );
    assert_eq!(
        serde_json::from_value::<AgentTokenUsage>(serde_json::json!({}))
            .unwrap()
            .current_context_tokens,
        None
    );

    // is_zero holds when every count is absent or explicitly zero, and fails
    // the moment one is non-zero.
    assert!(AgentCurrentUsage::default().is_zero());
    assert!(
        AgentCurrentUsage {
            input_tokens: Some(0),
            ..AgentCurrentUsage::default()
        }
        .is_zero()
    );
    assert!(
        !AgentCurrentUsage {
            cache_read_input_tokens: Some(1),
            ..AgentCurrentUsage::default()
        }
        .is_zero()
    );
}

#[test]
fn cache_hit_percent_rounds_and_classifies_health() {
    let usage = |input, cache_write, cache_read| AgentSessionUsage {
        input_tokens: Some(input),
        cache_creation_input_tokens: Some(cache_write),
        cache_read_input_tokens: Some(cache_read),
        ..AgentSessionUsage::default()
    };

    assert_eq!(AgentSessionUsage::default().cache_hit_percent(), None);
    assert_eq!(usage(1, 0, 2).cache_hit_percent(), Some(67));
    assert_eq!(usage(1, 0, 1).cache_hit_percent(), Some(50));
    assert_eq!(cache_hit_percent(u64::MAX, 0), Some(100));
    assert_eq!(
        usage(u64::MAX, u64::MAX, u64::MAX).cache_hit_percent(),
        Some(50),
        "wide input-side arithmetic remains bounded"
    );
    assert_eq!(CacheHealth::classify(69), CacheHealth::Alarm);
    assert_eq!(CacheHealth::classify(70), CacheHealth::Caution);
    assert_eq!(CacheHealth::classify(89), CacheHealth::Caution);
    assert_eq!(CacheHealth::classify(90), CacheHealth::Good);
}

#[test]
fn turn_error_class_round_trips_and_defaults_to_failed() {
    for (class, wire, label) in [
        (
            TurnErrorClass::PausedRateLimit,
            "paused_rate_limit",
            "You've hit your usage limit",
        ),
        (
            TurnErrorClass::PausedSpendLimit,
            "paused_spend_limit",
            "You've hit your monthly spend limit",
        ),
        (
            TurnErrorClass::PausedOverloaded,
            "paused_overloaded",
            "API Error: Overloaded",
        ),
        (
            TurnErrorClass::Unknown,
            "unknown",
            "turn ended with no final message",
        ),
        (TurnErrorClass::Failed, "failed", "API Error: Bad Request"),
    ] {
        let error = AgentTurnError {
            class,
            at: Timestamp::from_second(1_700_000_000).unwrap(),
            label: Some(label.to_owned()),
        };
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["class"], wire);
        let back: AgentTurnError = serde_json::from_value(value).unwrap();
        assert_eq!(back, error);
    }

    let legacy: AgentTurnError = serde_json::from_value(serde_json::json!({
        "at": "2023-11-14T22:13:20Z",
        "label": "API Error: Server Error"
    }))
    .unwrap();
    assert_eq!(legacy.class, TurnErrorClass::Failed);
}

#[test]
fn turn_error_label_classifier_maps_provider_labels() {
    for (label, class) in [
        (
            "You've hit your monthly spend limit.",
            TurnErrorClass::PausedSpendLimit,
        ),
        (
            "You've hit your session limit · resets 10:50am (UTC)",
            TurnErrorClass::PausedRateLimit,
        ),
        (
            "API Error: rate limit exceeded",
            TurnErrorClass::PausedRateLimit,
        ),
        ("API Error: Server Error", TurnErrorClass::PausedOverloaded),
        (
            "API Error: No response from API",
            TurnErrorClass::PausedOverloaded,
        ),
        (
            "API Error: Response stalled mid-stream. The response above may be incomplete.",
            TurnErrorClass::PausedOverloaded,
        ),
        (
            "API Error: request timed out",
            TurnErrorClass::PausedOverloaded,
        ),
        (
            "API Error: connection error",
            TurnErrorClass::PausedOverloaded,
        ),
        (
            "API Error: Connection closed mid-response. The response above may be incomplete.",
            TurnErrorClass::PausedOverloaded,
        ),
        (
            "API Error: connection closed",
            TurnErrorClass::PausedOverloaded,
        ),
        (
            "API Error: connection reset",
            TurnErrorClass::PausedOverloaded,
        ),
        (
            "API Error: connection lost",
            TurnErrorClass::PausedOverloaded,
        ),
        (
            "API Error: socket hang up",
            TurnErrorClass::PausedOverloaded,
        ),
        ("API Error: broken pipe", TurnErrorClass::PausedOverloaded),
        ("API Error: ECONNRESET", TurnErrorClass::PausedOverloaded),
        (
            "API Error: response ended mid-response",
            TurnErrorClass::PausedOverloaded,
        ),
        (
            "API Error: response ended mid-stream",
            TurnErrorClass::PausedOverloaded,
        ),
        (
            "Selected model is at capacity. Please try a different model.",
            TurnErrorClass::PausedOverloaded,
        ),
        (
            "unexpected status 503 Service Unavailable: Service Unavailable, url: https://chatgpt.com/backend-api/codex/responses, cf-ray: a20a1d2aca20f069-DFW, auth error: 503, auth error code: biscuit_baker_service_me_circuit_open",
            TurnErrorClass::PausedOverloaded,
        ),
        (
            "We're currently experiencing high demand, which may cause temporary errors.",
            TurnErrorClass::PausedOverloaded,
        ),
        ("unexpected status 429", TurnErrorClass::PausedRateLimit),
        ("unexpected status 400 Bad Request", TurnErrorClass::Failed),
        ("cf-ray a20a1d2aca20f069-503x", TurnErrorClass::Failed),
        (
            "fetched https://example.com/a in 512ms",
            TurnErrorClass::Failed,
        ),
        ("src/http.rs:512:    let x = 1;", TurnErrorClass::Failed),
        (
            "compacted 1200 tokens, http request ok, 540 remaining",
            TurnErrorClass::Failed,
        ),
        ("API Error: connection refused", TurnErrorClass::Failed),
        ("API Error: fetch failed", TurnErrorClass::Failed),
        ("API Error: invalid API key", TurnErrorClass::Failed),
        ("API Error: Bad Request", TurnErrorClass::Failed),
    ] {
        assert_eq!(
            TurnErrorClass::classify_label(Some(label)),
            class,
            "{label}"
        );
    }
    assert_eq!(TurnErrorClass::classify_label(None), TurnErrorClass::Failed);
}
