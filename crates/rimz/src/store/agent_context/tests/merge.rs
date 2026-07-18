use super::*;
use crate::agents::context::WindowSource;
use crate::agents::{
    AgentCost, AgentCurrentUsage, AgentRateLimits, AgentSessionUsage, AgentTokenUsage,
    AgentTurnError, FieldPatch, LocalContextPatch, LocalContextRefresh, LocalSpendFold,
    LocalTokenPatch, RateLimitWindow, TranscriptStat, TurnErrorClass,
};

#[test]
fn droid_local_merge_replaces_current_call_and_keeps_session_usage_monotonic() {
    let (_dir, runtime) = runtime();
    let observed_at = observed_at();
    let mut prior = new_record("droid", "sess-1", AgentContext::new("droid", observed_at));
    prior.context.model_id = Some("deepseek-v4-pro".to_owned());
    prior.context.model_display_name = Some("DeepSeek V4 Pro".to_owned());
    prior.context.cost = Some(AgentCost {
        total_cost_usd: Some(1.25),
        ..AgentCost::default()
    });
    prior.context.tokens = Some(AgentTokenUsage {
        context_window_size: Some(128_000),
        used_percentage: Some(42),
        remaining_percentage: Some(58),
        current_context_tokens: None,
        current_usage: Some(current_usage(10, 2, 3, 40)),
        session_usage: Some(AgentSessionUsage {
            input_tokens: Some(100),
            output_tokens: Some(20),
            cache_creation_input_tokens: Some(30),
            cache_read_input_tokens: Some(400),
            thinking_tokens: Some(5),
        }),
    });
    let refresh = LocalContextRefresh {
        context: LocalContextPatch {
            model_id: FieldPatch::Set("deepseek-v4-pro".to_owned()),
            model_display_name: FieldPatch::Set("DeepSeek V4 Pro".to_owned()),
            effort: FieldPatch::Set("high".to_owned()),
            tokens: LocalTokenPatch::ReplaceCurrentPreservingSession(Some(AgentTokenUsage {
                context_window_size: Some(200_000),
                used_percentage: None,
                remaining_percentage: None,
                current_context_tokens: None,
                current_usage: None,
                session_usage: Some(AgentSessionUsage {
                    input_tokens: Some(90),
                    output_tokens: Some(25),
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: Some(390),
                    thinking_tokens: Some(7),
                }),
            })),
            cost: FieldPatch::Clear,
            native_permission_wait: FieldPatch::Set(Timestamp::from_second(1_700_000_001).unwrap()),
            ..LocalContextPatch::authoritative_current()
        },
        transcript_path: Some("/tmp/sess-1.settings.json".to_owned()),
        transcript_stat: Some(stat()),
        spend_fold: FieldPatch::Keep,
    };
    write_record(&runtime, &prior).unwrap();

    merge_local_context(
        &runtime,
        descriptor("droid"),
        "sess-1",
        refresh,
        observed_at,
    )
    .unwrap();
    let merged = read_one(&runtime, "droid", "sess-1").unwrap();
    let tokens = merged.context.tokens.as_ref().unwrap();
    assert_eq!(tokens.context_window_size, Some(200_000));
    assert_eq!(tokens.used_percentage, None);
    assert_eq!(tokens.current_usage, None);
    assert_eq!(
        merged.context.native_permission_wait,
        Some(Timestamp::from_second(1_700_000_001).unwrap())
    );
    let session = tokens.session_usage.as_ref().unwrap();
    assert_eq!(session.input_tokens, Some(100));
    assert_eq!(session.output_tokens, Some(25));
    assert_eq!(session.cache_creation_input_tokens, Some(30));
    assert_eq!(session.cache_read_input_tokens, Some(400));
    assert_eq!(session.thinking_tokens, Some(7));
    assert!(
        merged.context.cost.is_none(),
        "missing exact pricing clears the prior cost"
    );

    let unresolved_model = LocalContextRefresh {
        context: LocalContextPatch {
            model_id: FieldPatch::Clear,
            model_display_name: FieldPatch::Set("Other Model".to_owned()),
            tokens: LocalTokenPatch::ReplaceCurrentPreservingSession(None),
            cost: FieldPatch::Clear,
            ..LocalContextPatch::authoritative_current()
        },
        transcript_path: Some("/tmp/sess-1.settings.json".to_owned()),
        transcript_stat: Some(stat()),
        spend_fold: FieldPatch::Keep,
    };
    merge_local_context(
        &runtime,
        descriptor("droid"),
        "sess-1",
        unresolved_model,
        observed_at,
    )
    .unwrap();
    let unresolved = read_one(&runtime, "droid", "sess-1").unwrap();
    let tokens = unresolved.context.tokens.unwrap();
    assert_eq!(tokens.context_window_size, None);
    assert_eq!(tokens.used_percentage, None);
    assert_eq!(tokens.current_usage, None);
    assert!(tokens.session_usage.is_some());
    assert!(unresolved.context.native_permission_wait.is_none());
}

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
            name: "unknown local effort keeps prior effort",
            prior: prior_effort,
            refresh: unknown_effort_refresh,
            assert: assert_prior_effort,
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
            descriptor("codex"),
            "sess-1",
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
fn stale_provider_observation_updates_latest_locked_record() {
    let (_dir, runtime) = runtime();
    let provider_at = Timestamp::from_second(1_700_000_000).unwrap();
    assert!(
        update_record(
            &runtime,
            "codex",
            "sess-1",
            provider_at,
            |record, existed| {
                assert!(!existed);
                record.context.session_name = Some("Initial provider name".to_owned());
                record.rate_limits_observed_at = Some(provider_at);
                true
            },
        )
        .unwrap()
    );

    let local_at = Timestamp::from_second(1_700_000_100).unwrap();
    let mut refresh = full_local_refresh();
    refresh.spend_fold = FieldPatch::Set(LocalSpendFold {
        cursor: crate::agents::spending::SpendCursor {
            offset: 42,
            state: None,
        },
        total_usd: 0.12,
    });
    merge_local_context(&runtime, descriptor("codex"), "sess-1", refresh, local_at).unwrap();
    let opener = crate::ids::MessageId::parse("msg_0123456789abcdef").unwrap();
    merge_turn_opened_by(&runtime, "codex", "sess-1", vec![opener.clone()]).unwrap();

    assert!(
        update_record(
            &runtime,
            "codex",
            "sess-1",
            provider_at,
            |record, existed| {
                assert!(existed);
                record.context.session_name = Some("Refined provider name".to_owned());
                record.context.observed_at = provider_at;
                record.rate_limits_observed_at = Some(provider_at);
                true
            },
        )
        .unwrap()
    );

    let merged = read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(
        merged.context.session_name.as_deref(),
        Some("Refined provider name")
    );
    assert_eq!(used_pct(&merged), Some(40));
    assert_eq!(total_cost(&merged), Some(0.12));
    assert_eq!(merged.context.turn_opened_by, vec![opener]);
    assert_eq!(
        merged.transcript_path.as_deref(),
        Some("/tmp/rollout.jsonl")
    );
    assert_eq!(merged.transcript_stat, Some(stat()));
    assert_eq!(merged.spend_fold.unwrap().total_usd, 0.12);
    assert_eq!(merged.rate_limits_observed_at, Some(provider_at));
    assert_eq!(merged.context.observed_at, provider_at);
}

#[test]
fn foldless_local_refresh_preserves_prior_spend_fold() {
    let (_dir, runtime) = runtime();
    let observed_at = observed_at();
    let mut prior = codex_record(observed_at);
    prior.spend_fold = Some(LocalSpendFold {
        cursor: crate::agents::spending::SpendCursor {
            offset: 42,
            state: None,
        },
        total_usd: 1.25,
    });
    write_record(&runtime, &prior).unwrap();

    merge_local_context(
        &runtime,
        descriptor("codex"),
        "sess-1",
        unpriced_refresh(),
        observed_at,
    )
    .unwrap();

    assert_eq!(
        read_one(&runtime, "codex", "sess-1").unwrap().spend_fold,
        prior.spend_fold
    );
}

#[test]
fn local_session_preview_updates_only_when_the_refresh_has_one() {
    let (_dir, runtime) = runtime();
    let observed_at = observed_at();
    let mut prior = codex_record(observed_at);
    prior.context.session_preview = Some("Old title".to_owned());
    write_record(&runtime, &prior).unwrap();

    let mut refresh = unpriced_refresh();
    refresh.context.session_preview = FieldPatch::Set("New title".to_owned());
    merge_local_context(
        &runtime,
        descriptor("codex"),
        "sess-1",
        refresh,
        observed_at,
    )
    .unwrap();
    assert_eq!(
        read_one(&runtime, "codex", "sess-1")
            .unwrap()
            .context
            .session_preview
            .as_deref(),
        Some("New title")
    );

    merge_local_context(
        &runtime,
        descriptor("codex"),
        "sess-1",
        unpriced_refresh(),
        observed_at,
    )
    .unwrap();
    assert_eq!(
        read_one(&runtime, "codex", "sess-1")
            .unwrap()
            .context
            .session_preview
            .as_deref(),
        Some("New title")
    );
}

#[test]
fn codex_local_refresh_overwrites_turn_error_marker() {
    let (_dir, runtime) = runtime();
    let observed_at = Timestamp::from_second(1_700_000_000).unwrap();
    let prior_marker = turn_error(TurnErrorClass::PausedRateLimit, "old limit", 1_700_000_000);
    let next_marker = turn_error(
        TurnErrorClass::Unknown,
        "turn ended with no final message",
        1_700_000_030,
    );
    let mut prior = codex_record(observed_at);
    prior.context.turn_error = Some(prior_marker);
    write_record(&runtime, &prior).unwrap();

    let mut refresh = unpriced_refresh();
    refresh.context.turn_error = FieldPatch::Set(next_marker.clone());
    merge_local_context(
        &runtime,
        descriptor("codex"),
        "sess-1",
        refresh,
        observed_at,
    )
    .unwrap();
    let merged = read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(merged.context.turn_error, Some(next_marker));

    let mut clear = unpriced_refresh();
    clear.context.turn_error = FieldPatch::Clear;
    merge_local_context(&runtime, descriptor("codex"), "sess-1", clear, observed_at).unwrap();
    let merged = read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(
        merged.context.turn_error, None,
        "Codex detector clears stale turn errors when the tail advances"
    );
}

#[test]
fn local_refresh_overwrites_turn_settle_markers() {
    let (_dir, runtime) = runtime();
    let observed_at = Timestamp::from_second(1_700_000_000).unwrap();
    let old = Timestamp::from_second(1_700_000_000).unwrap();
    let new = Timestamp::from_second(1_700_000_030).unwrap();
    let mut prior = codex_record(observed_at);
    prior.context.plan_proposed = Some(old);
    prior.context.turn_interrupted = Some(old);
    write_record(&runtime, &prior).unwrap();

    let mut refresh = unpriced_refresh();
    refresh.context.plan_proposed = FieldPatch::Set(new);
    refresh.context.turn_interrupted = FieldPatch::Set(new);
    merge_local_context(
        &runtime,
        descriptor("codex"),
        "sess-1",
        refresh,
        observed_at,
    )
    .unwrap();
    let merged = read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(merged.context.plan_proposed, Some(new));
    assert_eq!(merged.context.turn_interrupted, Some(new));

    merge_local_context(
        &runtime,
        descriptor("codex"),
        "sess-1",
        unpriced_refresh(),
        observed_at,
    )
    .unwrap();
    let merged = read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(merged.context.plan_proposed, None);
    assert_eq!(
        merged.context.turn_interrupted, None,
        "local detector clears stale interrupted markers when the tail advances"
    );
}

#[test]
fn non_codex_local_refresh_preserves_turn_error_marker() {
    let (_dir, runtime) = runtime();
    let observed_at = Timestamp::from_second(1_700_000_000).unwrap();
    let marker = turn_error(
        TurnErrorClass::PausedOverloaded,
        "provider parked",
        1_700_000_000,
    );
    let mut prior = new_record("pi", "sess-1", ctx(observed_at));
    prior.context.source = "pi".to_owned();
    prior.context.turn_error = Some(marker.clone());
    write_record(&runtime, &prior).unwrap();

    let mut refresh = unpriced_refresh();
    refresh.context.turn_error = FieldPatch::Keep;
    merge_local_context(&runtime, descriptor("pi"), "sess-1", refresh, observed_at).unwrap();

    let merged = read_one(&runtime, "pi", "sess-1").unwrap();
    assert_eq!(merged.context.turn_error, Some(marker));
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

fn turn_error(class: TurnErrorClass, label: &str, at: i64) -> AgentTurnError {
    AgentTurnError {
        class,
        at: Timestamp::from_second(at).unwrap(),
        label: Some(label.to_owned()),
    }
}

#[test]
fn observed_context_merge_preserves_fields_cost_coverage_and_monotonicity() {
    let (_dir, runtime) = runtime();

    assert!(merge_observed(&runtime, "pi", "sess-1", observed_context()).unwrap());
    let first = read_one(&runtime, "pi", "sess-1").unwrap();
    assert_eq!(first.context.model_id.as_deref(), Some("gpt-5.5"));
    assert_eq!(
        first.context.session_name.as_deref(),
        Some("fixture session")
    );
    assert_eq!(
        first.context.model_display_name.as_deref(),
        Some("Fixture 5.5")
    );
    assert_eq!(first.context.agent_version.as_deref(), Some("1.2.3"));
    assert_eq!(first.context.effort.as_deref(), Some("high"));
    assert_eq!(total_cost(&first), Some(0.5));
    assert_eq!(
        first.context.cost.as_ref().map(|cost| cost.coverage),
        Some(crate::agents::CostCoverage::CurrentUsage)
    );
    assert_eq!(
        first
            .context
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.current_usage.as_ref())
            .and_then(|usage| usage.cache_read_input_tokens),
        Some(30)
    );
    assert_eq!(
        first
            .context
            .rate_limits
            .as_ref()
            .map(|limits| limits.windows.len()),
        Some(1)
    );

    assert!(
        !merge_observed(&runtime, "pi", "sess-1", observed_context()).unwrap(),
        "an identical envelope is a no-op"
    );
    let after_repeat = read_one(&runtime, "pi", "sess-1").unwrap();
    assert_eq!(after_repeat, first);

    let mut lower_cost = observed_context();
    lower_cost.model_id = None;
    lower_cost.effort = None;
    lower_cost.tokens = None;
    lower_cost.rate_limits = None;
    lower_cost.cost = Some(cost(0.25));
    assert!(
        !merge_observed(&runtime, "pi", "sess-1", lower_cost).unwrap(),
        "a resume-reset extension accumulator must not lower displayed cost"
    );
    let after_lower = read_one(&runtime, "pi", "sess-1").unwrap();
    assert_eq!(after_lower, first);

    let mut partial_tokens = observed_context();
    partial_tokens.cost = None;
    partial_tokens.rate_limits = None;
    partial_tokens.tokens = Some(AgentTokenUsage {
        context_window_size: Some(300_000),
        used_percentage: None,
        remaining_percentage: None,
        current_context_tokens: None,
        current_usage: None,
        session_usage: None,
    });
    assert!(merge_observed(&runtime, "pi", "sess-1", partial_tokens).unwrap());
    let merged = read_one(&runtime, "pi", "sess-1").unwrap();
    let tokens = merged.context.tokens.as_ref().unwrap();
    assert_eq!(tokens.context_window_size, Some(300_000));
    assert_eq!(tokens.used_percentage, Some(42));
    assert_eq!(
        tokens
            .current_usage
            .as_ref()
            .and_then(|usage| usage.input_tokens),
        Some(10),
        "missing token subfields preserve the last known values"
    );
}

#[test]
fn context_merge_accepts_model_and_effort_only_enrichment() {
    let (_dir, runtime) = runtime();
    let mut model_context = AgentContext::new("pi", observed_at());
    model_context.model_id = Some("gpt-5.5".to_owned());
    let mut effort_context = AgentContext::new("pi", observed_at());
    effort_context.effort = Some("high".to_owned());

    assert!(merge_observed(&runtime, "pi", "sess-1", model_context).unwrap());
    assert!(merge_observed(&runtime, "pi", "sess-1", effort_context).unwrap());
    let merged = read_one(&runtime, "pi", "sess-1").unwrap();
    assert_eq!(merged.context.model_id.as_deref(), Some("gpt-5.5"));
    assert_eq!(merged.context.effort.as_deref(), Some("high"));
}

#[test]
fn observed_context_merge_replaces_authoritative_scalar_including_zero() {
    let (_dir, runtime) = runtime();
    let observed = |current_context_tokens| {
        let mut context = AgentContext::new("pi", observed_at());
        context.tokens = Some(AgentTokenUsage {
            current_context_tokens: Some(current_context_tokens),
            ..AgentTokenUsage::default()
        });
        context
    };

    assert!(merge_observed(&runtime, "pi", "sess-1", observed(42)).unwrap());
    assert!(merge_observed(&runtime, "pi", "sess-1", observed(0)).unwrap());
    assert_eq!(
        read_one(&runtime, "pi", "sess-1")
            .unwrap()
            .context
            .tokens
            .unwrap()
            .current_context_tokens,
        Some(0)
    );
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

fn prior_effort(observed_at: Timestamp) -> AgentContextRecord {
    let mut record = codex_record(observed_at);
    record.context.effort = Some("xhigh".to_owned());
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
        context: LocalContextPatch {
            model_id: FieldPatch::Set("gpt-5.5".to_owned()),
            effort: FieldPatch::Set("xhigh".to_owned()),
            tokens: LocalTokenPatch::PreserveEstablished(Some(tokens(
                1_000,
                40,
                60,
                Some(AgentCurrentUsage {
                    input_tokens: Some(30),
                    output_tokens: Some(4),
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: Some(10),
                }),
            ))),
            cost: FieldPatch::Set(cost(0.12)),
            turn_error: FieldPatch::Clear,
            ..LocalContextPatch::authoritative_current()
        },
        transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
        transcript_stat: Some(stat()),
        spend_fold: FieldPatch::Keep,
    }
}

fn unpriced_refresh() -> LocalContextRefresh {
    LocalContextRefresh {
        context: LocalContextPatch {
            model_id: FieldPatch::Set("gpt-5".to_owned()),
            effort: FieldPatch::Set("high".to_owned()),
            tokens: LocalTokenPatch::PreserveEstablished(Some(tokens(1_000, 10, 90, None))),
            turn_error: FieldPatch::Clear,
            ..LocalContextPatch::authoritative_current()
        },
        transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
        transcript_stat: Some(stat()),
        spend_fold: FieldPatch::Keep,
    }
}

fn unknown_tokens_refresh() -> LocalContextRefresh {
    let mut refresh = unpriced_refresh();
    refresh.context.tokens = LocalTokenPatch::PreserveEstablished(None);
    refresh
}

fn unknown_effort_refresh() -> LocalContextRefresh {
    let mut refresh = unpriced_refresh();
    refresh.context.effort = FieldPatch::Keep;
    refresh
}

fn fresh_zero_codex_refresh() -> LocalContextRefresh {
    LocalContextRefresh {
        context: LocalContextPatch {
            effort: FieldPatch::Set("high".to_owned()),
            tokens: LocalTokenPatch::PreserveEstablished(Some(codex_tokens(
                codex_default_window(),
                Some(current_usage(0, 0, 0, 0)),
            ))),
            turn_error: FieldPatch::Clear,
            ..LocalContextPatch::authoritative_current()
        },
        transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
        transcript_stat: Some(stat()),
        spend_fold: FieldPatch::Keep,
    }
}

fn fallback_window_refresh() -> LocalContextRefresh {
    LocalContextRefresh {
        context: LocalContextPatch {
            effort: FieldPatch::Set("high".to_owned()),
            tokens: LocalTokenPatch::PreserveEstablished(Some(tokens(
                codex_default_window(),
                10,
                90,
                None,
            ))),
            turn_error: FieldPatch::Clear,
            ..LocalContextPatch::authoritative_current()
        },
        transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
        transcript_stat: Some(stat()),
        spend_fold: FieldPatch::Keep,
    }
}

fn descriptor(kind: &str) -> &'static crate::agents::AgentDescriptor {
    crate::agents::descriptor_by_kind(kind).expect("fixture adapter is registered")
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

fn assert_prior_effort(merged: &AgentContextRecord, _: Timestamp, _: Timestamp) {
    assert_eq!(
        merged.context.effort.as_deref(),
        Some("xhigh"),
        "missing rollout effort preserves the last observed effort",
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
        current_context_tokens: None,
        current_usage,
        session_usage: None,
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
        current_context_tokens: None,
        current_usage,
        session_usage: None,
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
        companion: None,
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

fn observed_at() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

fn observed_context() -> AgentContext {
    AgentContext {
        source: "pi".to_owned(),
        session_name: Some("fixture session".to_owned()),
        session_preview: None,
        model_id: Some("gpt-5.5".to_owned()),
        model_display_name: Some("Fixture 5.5".to_owned()),
        effort: Some("high".to_owned()),
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: Some("1.2.3".to_owned()),
        exceeds_200k_tokens: None,
        cost: Some(AgentCost {
            coverage: crate::agents::CostCoverage::CurrentUsage,
            ..cost(0.5)
        }),
        tokens: Some(AgentTokenUsage {
            context_window_size: Some(272_000),
            used_percentage: Some(42),
            remaining_percentage: None,
            current_context_tokens: None,
            current_usage: Some(AgentCurrentUsage {
                input_tokens: Some(10),
                output_tokens: Some(2),
                cache_creation_input_tokens: Some(4),
                cache_read_input_tokens: Some(30),
            }),
            session_usage: None,
        }),
        rate_limits: Some(AgentRateLimits {
            windows: vec![RateLimitWindow {
                used_percentage: Some(72),
                resets_at: None,
                duration_mins: Some(300),
                observed_at: Some(observed_at()),
                source: WindowSource::BestEffort,
                ..Default::default()
            }],
        }),
        pr: None,
        account: None,
        turn_opened_by: Vec::new(),
        turn_error: None,
        turn_complete: None,
        plan_proposed: None,
        native_permission_wait: None,
        turn_interrupted: None,
        observed_at: observed_at(),
    }
}
