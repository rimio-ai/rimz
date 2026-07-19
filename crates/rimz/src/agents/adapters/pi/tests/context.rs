use super::*;

use crate::agents::AgentHookClass;

/// Every context assertion runs through one frozen `observed_at` so the
/// snapshot stays stable.
fn normalized_context(mut payload: Value) -> Option<AgentContext> {
    payload["session_id"] = json!("sess-1");
    let mut context = PiAdapter.observe_context("pi", &payload)?.context;
    context.observed_at = jiff::Timestamp::from_second(1_700_000_000).unwrap();
    Some(context)
}

#[test]
fn observed_context_projects_the_extension_envelope() {
    let context = normalized_context(json!({
        "model": "gpt-5.5",
        "session_name": "Parser cleanup",
        "effort": "high",
        "context_pct": 42,
        "context_window": 272_000,
        "total_tokens": 114_000,
        "total_cost_usd": 0.125,
        "input_tokens": 10,
        "cache_write_input_tokens": 4,
        "cache_read_input_tokens": 30,
        "output_tokens": 2,
        "rate_limits": [
            {
                "used_percentage": 72,
                "resets_at": 1_700_018_000i64,
                "duration_mins": 300,
                "observed_at": 1_700_000_000i64
            },
            {
                "used_percentage": 35,
                "resets_at": 1_700_604_800i64,
                "duration_mins": 10_080,
                "observed_at": 1_700_000_000i64
            }
        ]
    }))
    .expect("rich context");
    insta::assert_json_snapshot!(context, @r###"
        {
          "source": "pi",
          "session_name": "Parser cleanup",
          "model_id": "gpt-5.5",
          "effort": "high",
          "cost": {
            "total_cost_usd": 0.125
          },
          "tokens": {
            "context_window_size": 272000,
            "used_percentage": 42,
            "current_usage": {
              "input_tokens": 10,
              "output_tokens": 2,
              "cache_creation_input_tokens": 4,
              "cache_read_input_tokens": 30
            }
          },
          "rate_limits": {
            "windows": [
              {
                "used_percentage": 72,
                "resets_at": "2023-11-15T03:13:20Z",
                "duration_mins": 300,
                "observed_at": "2023-11-14T22:13:20Z"
              },
              {
                "used_percentage": 35,
                "resets_at": "2023-11-21T22:13:20Z",
                "duration_mins": 10080,
                "observed_at": "2023-11-14T22:13:20Z"
              }
            ]
          },
          "observed_at": "2023-11-14T22:13:20Z"
        }
        "###);
}

#[test]
fn observed_context_omits_absent_and_zero_sections() {
    let without_cost = normalized_context(json!({
        "context_pct": 7,
        "context_window": 128_000,
        "input_tokens": 9
    }))
    .expect("context without cost");
    assert!(without_cost.cost.is_none());
    assert_eq!(
        without_cost.tokens.as_ref().unwrap().used_percentage,
        Some(7)
    );

    let without_rate_limits = normalized_context(json!({
        "context_pct": 12,
        "context_window": 128_000,
        "input_tokens": 6,
        "output_tokens": 1
    }))
    .expect("context without windows");
    assert!(without_rate_limits.rate_limits.is_none());
    assert_eq!(
        without_rate_limits
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.current_usage.as_ref())
            .and_then(|usage| usage.input_tokens),
        Some(6)
    );

    let zero_split = normalized_context(json!({
        "context_pct": 0,
        "context_window": 128_000,
        "input_tokens": 0,
        "cache_write_input_tokens": 0,
        "cache_read_input_tokens": 0,
        "output_tokens": 0
    }))
    .expect("zero split still carries the window");
    assert!(
        zero_split.tokens.as_ref().unwrap().current_usage.is_none(),
        "all-zero token split drops the per-call breakdown"
    );

    assert!(
        PiAdapter
            .observe_context("pi", &json!({ "context_window": "not a number" }))
            .is_none(),
        "malformed context payloads degrade to no enrichment"
    );
}

/// The sibling-drift case is `14bbe96c0 fix(pi): preserve rate limits across
/// sibling drift`. A type mismatch anywhere degrades the typed parse to its
/// default, so windows are extracted independently — otherwise one bad
/// neighbouring field silently dropped every provider window.
#[test]
fn rate_limit_windows_tolerate_wire_drift() {
    let context = normalized_context(json!({
        "model": "gpt-5",
        "rateLimits": [
            {
                "usedPercent": "101.4",
                "resetsAt": "2023-11-15T03:13:20Z",
                "durationMins": 300,
                "observedAt": "1700000000"
            },
            {
                "used_percentage": -2.0,
                "resets_at": "1700018000"
            },
            { "used_percentage": "NaN", "duration_mins": "bad" },
            { "observed_at": 1700000000 },
            "invalid"
        ]
    }))
    .unwrap();
    let windows = context.rate_limits.unwrap().windows;
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].used_percentage, Some(100));
    assert_eq!(windows[0].duration_mins, Some(300));
    assert_eq!(
        windows[0].resets_at.unwrap().to_string(),
        "2023-11-15T03:13:20Z"
    );
    assert_eq!(windows[1].used_percentage, Some(0));
    assert_eq!(
        windows[1].resets_at.unwrap().to_string(),
        "2023-11-15T03:13:20Z"
    );

    for rate_limits in [json!([]), json!({"bad": true})] {
        let context = normalized_context(json!({
            "model": "kept",
            "rate_limits": rate_limits
        }))
        .unwrap();
        assert_eq!(context.model_id.as_deref(), Some("kept"));
        assert!(context.rate_limits.is_none());
    }

    let context = normalized_context(json!({
        "total_cost_usd": "malformed sibling",
        "rate_limits": [{"used_percentage": 50}]
    }))
    .unwrap();
    assert_eq!(
        context.rate_limits.unwrap().windows[0].used_percentage,
        Some(50),
        "a malformed sibling field must not discard independently valid windows"
    );
}

#[test]
fn model_select_enriches_without_a_lifecycle_signal() {
    let payload = json!({ "session_id": "s", "model": "gpt-5.5", "effort": "high" });
    assert_eq!(
        decode("model_select", &payload).class(),
        AgentHookClass::Lifecycle
    );
    assert_eq!(signal("model_select", &payload), None);
    assert_eq!(
        PiAdapter
            .observe_context("pi", &payload)
            .unwrap()
            .context
            .model_id
            .as_deref(),
        Some("gpt-5.5")
    );
}
