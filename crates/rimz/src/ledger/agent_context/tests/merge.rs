use super::*;
use crate::agents::{
    AgentCost, AgentCurrentUsage, AgentRateLimits, AgentTokenUsage, AgentTurnError,
    LocalContextRefresh, RateLimitWindow, TranscriptStat, TurnErrorClass,
};

struct MergeCase {
    name: &'static str,
    prior: fn(Timestamp) -> AgentContextRecord,
    refresh: fn() -> LocalContextRefresh,
    assert: fn(&AgentContextRecord, Timestamp, Timestamp),
}

#[test]
fn merge_local_context_preserves_prior_fields_by_case() {
    for case in [
        MergeCase {
            name: "app-server fields survive local transcript refresh",
            prior: prior_app_server_fields,
            refresh: full_local_refresh,
            assert: assert_app_server_fields,
        },
        MergeCase {
            name: "unpriced refresh keeps prior cumulative cost",
            prior: prior_cost,
            refresh: unpriced_refresh,
            assert: assert_prior_cost,
        },
        MergeCase {
            name: "unknown local tokens keep prior meter",
            prior: prior_tokens,
            refresh: unknown_tokens_refresh,
            assert: assert_prior_tokens,
        },
        MergeCase {
            name: "fresh Codex zero usage keeps established usage",
            prior: prior_established_codex_usage,
            refresh: fresh_zero_codex_refresh,
            assert: assert_established_codex_usage,
        },
        MergeCase {
            name: "cached exact Codex window beats default fallback",
            prior: prior_exact_codex_window,
            refresh: fallback_window_refresh,
            assert: assert_exact_window,
        },
    ] {
        let (_dir, runtime) = runtime();
        let prior_at = Timestamp::from_second(1_700_000_000).unwrap();
        let local_at = Timestamp::from_second(1_700_000_030).unwrap();
        write_record(&runtime, &(case.prior)(prior_at)).unwrap();

        merge_local_context(
            &runtime,
            "codex",
            "sess-1",
            read_one(&runtime, "codex", "sess-1"),
            (case.refresh)(),
            local_at,
        )
        .unwrap();

        let merged = read_one(&runtime, "codex", "sess-1").unwrap();
        assert_eq!(merged.agent_id.as_str(), "sess-1", "{}", case.name);
        (case.assert)(&merged, prior_at, local_at);
    }
}

#[test]
fn merge_turn_error_skips_identical_marker() {
    let (_dir, runtime) = runtime();
    let marker = AgentTurnError {
        class: TurnErrorClass::PausedRateLimit,
        at: Timestamp::from_second(1_700_000_000).unwrap(),
        label: Some("You've hit your usage limit".to_owned()),
    };

    assert!(
        merge_turn_error(&runtime, "codex", "sess-1", marker.clone()).unwrap(),
        "first marker write updates the sidecar"
    );
    let first = read_one(&runtime, "codex", "sess-1").unwrap();

    assert!(
        !merge_turn_error(&runtime, "codex", "sess-1", marker).unwrap(),
        "same marker is already present and should not rewrite"
    );
    let second = read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(second, first);
}

fn codex_record(observed_at: Timestamp) -> AgentContextRecord {
    let mut context = ctx(observed_at);
    context.source = "codex".to_owned();
    new_record("codex", "sess-1", context)
}

fn prior_app_server_fields(observed_at: Timestamp) -> AgentContextRecord {
    let mut record = codex_record(observed_at);
    record.context.model_id = Some("gpt-5".to_owned());
    record.context.model_display_name = Some("GPT-5".to_owned());
    record.context.rate_limits = Some(AgentRateLimits {
        windows: vec![RateLimitWindow {
            used_percentage: Some(12),
            resets_at: None,
            duration_mins: Some(300),
            ..Default::default()
        }],
    });
    record.rate_limits_observed_at = Some(observed_at);
    record
}

fn prior_cost(observed_at: Timestamp) -> AgentContextRecord {
    let mut record = codex_record(observed_at);
    record.context.cost = Some(cost(0.42));
    record
}

fn prior_tokens(observed_at: Timestamp) -> AgentContextRecord {
    let mut record = codex_record(observed_at);
    record.context.tokens = Some(tokens(1_000, 10, 90, None));
    record
}

fn prior_established_codex_usage(observed_at: Timestamp) -> AgentContextRecord {
    let mut record = codex_record(observed_at);
    record.context.tokens = Some(codex_tokens(
        258_400,
        Some(current_usage(9_200, 800, 0, 120_000)),
    ));
    record
}

fn prior_exact_codex_window(observed_at: Timestamp) -> AgentContextRecord {
    let mut record = codex_record(observed_at);
    record.context.model_id = Some("gpt-5.5".to_owned());
    record.context.tokens = Some(tokens(258_400, 50, 50, None));
    record
}

fn full_local_refresh() -> LocalContextRefresh {
    LocalContextRefresh {
        model_id: Some("gpt-5.5".to_owned()),
        effort: Some("xhigh".to_owned()),
        tokens: Some(tokens(
            1_000,
            40,
            60,
            Some(AgentCurrentUsage {
                input_tokens: Some(30),
                output_tokens: Some(4),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: Some(10),
            }),
        )),
        cost: Some(cost(0.12)),
        turn_complete: None,
        transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
        transcript_stat: Some(stat()),
    }
}

fn unpriced_refresh() -> LocalContextRefresh {
    LocalContextRefresh {
        model_id: Some("gpt-5".to_owned()),
        effort: Some("high".to_owned()),
        tokens: Some(tokens(1_000, 10, 90, None)),
        cost: None,
        turn_complete: None,
        transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
        transcript_stat: Some(stat()),
    }
}

fn unknown_tokens_refresh() -> LocalContextRefresh {
    LocalContextRefresh {
        tokens: None,
        ..unpriced_refresh()
    }
}

fn fresh_zero_codex_refresh() -> LocalContextRefresh {
    LocalContextRefresh {
        model_id: None,
        effort: Some("high".to_owned()),
        tokens: Some(codex_tokens(
            codex_default_window(),
            Some(current_usage(0, 0, 0, 0)),
        )),
        cost: None,
        turn_complete: None,
        transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
        transcript_stat: Some(stat()),
    }
}

fn fallback_window_refresh() -> LocalContextRefresh {
    LocalContextRefresh {
        model_id: None,
        effort: Some("high".to_owned()),
        tokens: Some(tokens(codex_default_window(), 10, 90, None)),
        cost: None,
        turn_complete: None,
        transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
        transcript_stat: Some(stat()),
    }
}

/// Codex's descriptor-default fallback window — what a refresh carries before a
/// rollout's exact `model_context_window` appears. Sourced from the descriptor
/// so these fixtures track the adapter instead of rotting as a literal when the
/// fallback moves (it went `258_000` → `272_000` and left this test stale).
fn codex_default_window() -> u64 {
    crate::agents::descriptor_by_kind("codex")
        .and_then(|descriptor| descriptor.default_context_window)
        .expect("codex descriptor declares a default context window")
}

fn assert_app_server_fields(merged: &AgentContextRecord, prior_at: Timestamp, local_at: Timestamp) {
    assert_eq!(merged.context.model_id.as_deref(), Some("gpt-5.5"));
    assert_eq!(merged.context.effort.as_deref(), Some("xhigh"));
    assert_eq!(merged.context.model_display_name.as_deref(), Some("GPT-5"));
    assert_eq!(
        merged
            .context
            .rate_limits
            .as_ref()
            .and_then(|limits| limits.windows.first())
            .and_then(|window| window.used_percentage),
        Some(12),
        "app-server rate limits survive a local refresh",
    );
    assert_eq!(merged.rate_limits_observed_at, Some(prior_at));
    assert_eq!(merged.context.observed_at, local_at);
    assert_eq!(used_pct(merged), Some(40));
    assert_eq!(total_cost(merged), Some(0.12));
    assert_eq!(
        merged.transcript_path.as_deref(),
        Some("/tmp/rollout.jsonl")
    );
}

fn assert_prior_cost(merged: &AgentContextRecord, _: Timestamp, _: Timestamp) {
    assert_eq!(
        total_cost(merged),
        Some(0.42),
        "an unpriced refresh keeps the last known cumulative session cost",
    );
    assert_eq!(
        used_pct(merged),
        Some(10),
        "window tokens still update independently of cost pricing",
    );
}

fn assert_prior_tokens(merged: &AgentContextRecord, _: Timestamp, _: Timestamp) {
    assert_eq!(
        used_pct(merged),
        Some(10),
        "unknown local tokens keep the established sidebar meter",
    );
}

fn assert_established_codex_usage(merged: &AgentContextRecord, _: Timestamp, _: Timestamp) {
    let tokens = merged.context.tokens.as_ref().unwrap();
    assert_eq!(
        tokens
            .current_usage
            .as_ref()
            .and_then(|usage| usage.cache_read_input_tokens),
        Some(120_000),
        "a fresh zero placeholder must not blink an established Codex meter to empty",
    );
    assert_eq!(
        tokens.context_window_size,
        Some(258_400),
        "the established exact window stays paired with the preserved usage",
    );
    assert_eq!(
        merged.transcript_stat,
        Some(stat()),
        "the stat gate still advances past the incomplete read",
    );
}

fn assert_exact_window(merged: &AgentContextRecord, _: Timestamp, _: Timestamp) {
    let tokens = merged.context.tokens.as_ref().unwrap();
    assert_eq!(
        tokens.context_window_size,
        Some(258_400),
        "an exact rollout window cached in the sidecar beats a later fallback",
    );
    assert_eq!(
        tokens.used_percentage,
        Some(10),
        "fresh usage still updates while only the fallback window is replaced",
    );
}

fn tokens(
    context_window_size: u64,
    used_percentage: u8,
    remaining_percentage: u8,
    current_usage: Option<AgentCurrentUsage>,
) -> AgentTokenUsage {
    AgentTokenUsage {
        context_window_size: Some(context_window_size),
        used_percentage: Some(used_percentage),
        remaining_percentage: Some(remaining_percentage),
        current_usage,
    }
}

/// A Codex token record in its real shape: no baked percentage (the gauge
/// derives it downstream from `current_usage` over the window).
fn codex_tokens(
    context_window_size: u64,
    current_usage: Option<AgentCurrentUsage>,
) -> AgentTokenUsage {
    AgentTokenUsage {
        context_window_size: Some(context_window_size),
        used_percentage: None,
        remaining_percentage: None,
        current_usage,
    }
}

fn current_usage(
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
) -> AgentCurrentUsage {
    AgentCurrentUsage {
        input_tokens: Some(input_tokens),
        output_tokens: Some(output_tokens),
        cache_creation_input_tokens: Some(cache_creation_input_tokens),
        cache_read_input_tokens: Some(cache_read_input_tokens),
    }
}

fn cost(total_cost_usd: f64) -> AgentCost {
    AgentCost {
        total_cost_usd: Some(total_cost_usd),
        ..AgentCost::default()
    }
}

fn stat() -> TranscriptStat {
    TranscriptStat {
        mtime_secs: 123,
        mtime_nanos: 456,
        len: 789,
    }
}

fn used_pct(record: &AgentContextRecord) -> Option<u8> {
    record
        .context
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.used_percentage)
}

fn total_cost(record: &AgentContextRecord) -> Option<f64> {
    record
        .context
        .cost
        .as_ref()
        .and_then(|cost| cost.total_cost_usd)
}
