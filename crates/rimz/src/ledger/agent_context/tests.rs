use super::*;
use crate::ids::WorkspaceId;

fn runtime() -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let id = WorkspaceId::from_project_root(std::path::Path::new("/tmp/ctx-test"));
    let runtime = RuntimePaths::under(id, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, runtime)
}

fn ctx(observed_at: Timestamp) -> AgentContext {
    AgentContext {
        source: "claude".to_owned(),
        session_name: None,
        session_preview: None,
        model_id: Some("claude-opus-4-8".to_owned()),
        model_display_name: None,
        effort: None,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: None,
        exceeds_200k_tokens: None,
        cost: None,
        tokens: None,
        rate_limits: None,
        pr: None,
        account: None,
        turn_error: None,
        observed_at,
    }
}

#[test]
fn write_then_read_round_trips() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
    let all = read_all(&runtime);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].kind, "claude");
    assert_eq!(all[0].agent_id, "sess-1");
    assert_eq!(all[0].context.model_id.as_deref(), Some("claude-opus-4-8"));
}

#[test]
fn absent_merge_fields_round_trip_as_none() {
    let now = Timestamp::now();
    let record = new_record("codex", "sess-1", ctx(now));
    let mut value = serde_json::to_value(&record).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .remove("rate_limits_observed_at");
    value.as_object_mut().unwrap().remove("transcript_path");
    value.as_object_mut().unwrap().remove("transcript_stat");

    let parsed: AgentContextRecord = serde_json::from_value(value).unwrap();
    assert_eq!(parsed.kind, "codex");
    assert_eq!(parsed.agent_id, "sess-1");
    assert_eq!(parsed.rate_limits_observed_at, None);
    assert_eq!(parsed.transcript_path, None);
    assert_eq!(parsed.transcript_stat, None);

    let serialized = serde_json::to_string(&record).unwrap();
    assert!(!serialized.contains("rate_limits_observed_at"));
    assert!(!serialized.contains("transcript_path"));
    assert!(!serialized.contains("transcript_stat"));
}

#[test]
fn read_one_bypasses_the_parse_cache() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
    assert_eq!(
        read_all(&runtime)[0].context.model_id.as_deref(),
        Some("claude-opus-4-8")
    );

    let mut changed = ctx(now);
    changed.model_id = Some("claude-sonnet-4-5".to_owned());
    write(&runtime, "claude", "sess-1", &changed).unwrap();

    let fresh = read_one(&runtime, "claude", "sess-1").expect("fresh direct read");
    assert_eq!(fresh.context.model_id.as_deref(), Some("claude-sonnet-4-5"));
}

#[test]
fn merge_local_context_preserves_app_server_fields() {
    let (_dir, runtime) = runtime();
    let app_at = Timestamp::from_second(1_700_000_000).unwrap();
    let local_at = Timestamp::from_second(1_700_000_030).unwrap();
    let mut prior_context = ctx(app_at);
    prior_context.model_id = Some("gpt-5".to_owned());
    prior_context.model_display_name = Some("GPT-5".to_owned());
    prior_context.rate_limits = Some(crate::agents::AgentRateLimits {
        windows: vec![crate::agents::RateLimitWindow {
            used_percentage: Some(12),
            resets_at: None,
            duration_mins: Some(300),
        }],
    });
    let mut prior = new_record("codex", "sess-1", prior_context);
    prior.rate_limits_observed_at = Some(app_at);
    write_record(&runtime, &prior).unwrap();

    merge_local_context(
        &runtime,
        "codex",
        "sess-1",
        read_one(&runtime, "codex", "sess-1"),
        crate::agents::LocalContextRefresh {
            model_id: Some("gpt-5.5".to_owned()),
            effort: Some("xhigh".to_owned()),
            tokens: Some(crate::agents::AgentTokenUsage {
                context_window_size: Some(1_000),
                used_percentage: Some(40),
                remaining_percentage: Some(60),
                current_usage: Some(crate::agents::AgentCurrentUsage {
                    input_tokens: Some(30),
                    output_tokens: Some(4),
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: Some(10),
                }),
            }),
            cost: Some(crate::agents::AgentCost {
                total_cost_usd: Some(0.12),
                ..crate::agents::AgentCost::default()
            }),
            transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
            transcript_stat: Some(crate::agents::TranscriptStat {
                mtime_secs: 123,
                mtime_nanos: 456,
                len: 789,
            }),
        },
        local_at,
    )
    .unwrap();

    let merged = read_one(&runtime, "codex", "sess-1").unwrap();
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
        Some(12)
    );
    assert_eq!(merged.rate_limits_observed_at, Some(app_at));
    assert_eq!(merged.context.observed_at, local_at);
    assert_eq!(
        merged
            .context
            .tokens
            .as_ref()
            .and_then(|t| t.used_percentage),
        Some(40)
    );
    assert_eq!(
        merged
            .context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_cost_usd),
        Some(0.12)
    );
    assert_eq!(
        merged.transcript_path.as_deref(),
        Some("/tmp/rollout.jsonl")
    );
}

#[test]
fn merge_local_context_preserves_prior_cost_when_refresh_is_unpriced() {
    let (_dir, runtime) = runtime();
    let prior_at = Timestamp::from_second(1_700_000_000).unwrap();
    let local_at = Timestamp::from_second(1_700_000_030).unwrap();
    let mut prior_context = ctx(prior_at);
    prior_context.cost = Some(crate::agents::AgentCost {
        total_cost_usd: Some(0.42),
        ..crate::agents::AgentCost::default()
    });
    write_record(&runtime, &new_record("codex", "sess-1", prior_context)).unwrap();

    merge_local_context(
        &runtime,
        "codex",
        "sess-1",
        read_one(&runtime, "codex", "sess-1"),
        crate::agents::LocalContextRefresh {
            model_id: Some("gpt-5".to_owned()),
            effort: Some("high".to_owned()),
            tokens: Some(crate::agents::AgentTokenUsage {
                context_window_size: Some(1_000),
                used_percentage: Some(10),
                remaining_percentage: Some(90),
                current_usage: None,
            }),
            cost: None,
            transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
            transcript_stat: Some(crate::agents::TranscriptStat {
                mtime_secs: 123,
                mtime_nanos: 456,
                len: 789,
            }),
        },
        local_at,
    )
    .unwrap();

    let merged = read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(
        merged
            .context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_cost_usd),
        Some(0.42),
        "an unpriced refresh keeps the last known cumulative session cost"
    );
    assert_eq!(
        merged
            .context
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.used_percentage),
        Some(10),
        "window tokens still update independently of cost pricing"
    );
}

#[test]
fn merge_local_context_preserves_prior_tokens_when_refresh_is_unknown() {
    let (_dir, runtime) = runtime();
    let prior_at = Timestamp::from_second(1_700_000_000).unwrap();
    let local_at = Timestamp::from_second(1_700_000_030).unwrap();
    let mut prior_context = ctx(prior_at);
    prior_context.tokens = Some(crate::agents::AgentTokenUsage {
        context_window_size: Some(1_000),
        used_percentage: Some(10),
        remaining_percentage: Some(90),
        current_usage: None,
    });
    write_record(&runtime, &new_record("codex", "sess-1", prior_context)).unwrap();

    merge_local_context(
        &runtime,
        "codex",
        "sess-1",
        read_one(&runtime, "codex", "sess-1"),
        crate::agents::LocalContextRefresh {
            model_id: Some("gpt-5".to_owned()),
            effort: Some("high".to_owned()),
            tokens: None,
            cost: None,
            transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
            transcript_stat: Some(crate::agents::TranscriptStat {
                mtime_secs: 123,
                mtime_nanos: 456,
                len: 789,
            }),
        },
        local_at,
    )
    .unwrap();

    let merged = read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(
        merged
            .context
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.used_percentage),
        Some(10),
        "unknown local tokens keep the established sidebar meter"
    );
}

#[test]
fn merge_local_context_preserves_established_codex_usage_over_fresh_zero() {
    let (_dir, runtime) = runtime();
    let prior_at = Timestamp::from_second(1_700_000_000).unwrap();
    let local_at = Timestamp::from_second(1_700_000_030).unwrap();
    let mut prior_context = ctx(prior_at);
    prior_context.source = "codex".to_owned();
    prior_context.tokens = Some(crate::agents::AgentTokenUsage {
        context_window_size: Some(258_400),
        used_percentage: Some(50),
        remaining_percentage: Some(50),
        current_usage: Some(crate::agents::AgentCurrentUsage {
            input_tokens: Some(9_200),
            output_tokens: Some(800),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(120_000),
        }),
    });
    write_record(&runtime, &new_record("codex", "sess-1", prior_context)).unwrap();

    merge_local_context(
        &runtime,
        "codex",
        "sess-1",
        read_one(&runtime, "codex", "sess-1"),
        crate::agents::LocalContextRefresh {
            model_id: None,
            effort: Some("high".to_owned()),
            tokens: Some(crate::agents::AgentTokenUsage {
                context_window_size: Some(258_000),
                used_percentage: Some(0),
                remaining_percentage: Some(100),
                current_usage: Some(crate::agents::AgentCurrentUsage {
                    input_tokens: Some(0),
                    output_tokens: Some(0),
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: Some(0),
                }),
            }),
            cost: None,
            transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
            transcript_stat: Some(crate::agents::TranscriptStat {
                mtime_secs: 123,
                mtime_nanos: 456,
                len: 789,
            }),
        },
        local_at,
    )
    .unwrap();

    let merged = read_one(&runtime, "codex", "sess-1").unwrap();
    let tokens = merged.context.tokens.as_ref().unwrap();
    assert_eq!(
        tokens.used_percentage,
        Some(50),
        "a fresh placeholder must not blink an established Codex meter to 0"
    );
    assert_eq!(
        tokens.context_window_size,
        Some(258_400),
        "the established exact window stays paired with the preserved usage"
    );
    assert_eq!(
        tokens
            .current_usage
            .as_ref()
            .and_then(|usage| usage.cache_read_input_tokens),
        Some(120_000)
    );
    assert_eq!(
        merged.transcript_stat,
        Some(crate::agents::TranscriptStat {
            mtime_secs: 123,
            mtime_nanos: 456,
            len: 789,
        }),
        "the stat gate still advances past the incomplete read"
    );
}

#[test]
fn merge_local_context_preserves_cached_exact_window_over_default_fallback() {
    let (_dir, runtime) = runtime();
    let prior_at = Timestamp::from_second(1_700_000_000).unwrap();
    let local_at = Timestamp::from_second(1_700_000_030).unwrap();
    let mut prior_context = ctx(prior_at);
    prior_context.source = "codex".to_owned();
    prior_context.model_id = Some("gpt-5.5".to_owned());
    prior_context.tokens = Some(crate::agents::AgentTokenUsage {
        context_window_size: Some(258_400),
        used_percentage: Some(50),
        remaining_percentage: Some(50),
        current_usage: None,
    });
    write_record(&runtime, &new_record("codex", "sess-1", prior_context)).unwrap();

    merge_local_context(
        &runtime,
        "codex",
        "sess-1",
        read_one(&runtime, "codex", "sess-1"),
        crate::agents::LocalContextRefresh {
            model_id: None,
            effort: Some("high".to_owned()),
            tokens: Some(crate::agents::AgentTokenUsage {
                context_window_size: Some(258_000),
                used_percentage: Some(10),
                remaining_percentage: Some(90),
                current_usage: None,
            }),
            cost: None,
            transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
            transcript_stat: Some(crate::agents::TranscriptStat {
                mtime_secs: 123,
                mtime_nanos: 456,
                len: 789,
            }),
        },
        local_at,
    )
    .unwrap();

    let merged = read_one(&runtime, "codex", "sess-1").unwrap();
    let tokens = merged.context.tokens.as_ref().unwrap();
    assert_eq!(
        tokens.context_window_size,
        Some(258_400),
        "an exact rollout window cached in the sidecar beats a later fallback"
    );
    assert_eq!(
        tokens.used_percentage,
        Some(10),
        "fresh usage still updates while only the fallback window is replaced"
    );
}

#[test]
fn distinct_sessions_get_distinct_files() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
    write(&runtime, "claude", "sess-2", &ctx(now)).unwrap();
    let mut ids: Vec<_> = read_all(&runtime).into_iter().map(|r| r.agent_id).collect();
    ids.sort();
    assert_eq!(ids, vec!["sess-1".to_owned(), "sess-2".to_owned()]);
}

#[test]
fn corrupt_file_is_skipped() {
    let (_dir, runtime) = runtime();
    std::fs::write(
        runtime.agent_context_dir.join("ctx.bogus.json"),
        b"not json",
    )
    .unwrap();
    assert!(read_all(&runtime).is_empty());
}

#[test]
fn past_ttl_record_is_skipped() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let stale = Timestamp::from_second(1_700_000_000 - CONTEXT_TTL_SECS - 60).unwrap();
    write(&runtime, "claude", "sess-1", &ctx(stale)).unwrap();
    assert!(read_all_at(&runtime, now).is_empty());
}

#[test]
fn ttl_cutoff_is_boundary_exact() {
    // A missed tombstone ages out on the TTL exactly: a record *at* the
    // cutoff is still served, one second past it is gone — an off-by-one
    // in either direction fails one arm.
    let (_dir, runtime) = runtime();
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let at_cutoff = Timestamp::from_second(1_700_000_000 - CONTEXT_TTL_SECS).unwrap();
    let past_cutoff = Timestamp::from_second(1_700_000_000 - CONTEXT_TTL_SECS - 1).unwrap();
    write(&runtime, "claude", "sess-at", &ctx(at_cutoff)).unwrap();
    write(&runtime, "claude", "sess-past", &ctx(past_cutoff)).unwrap();
    let ids: Vec<_> = read_all_at(&runtime, now)
        .into_iter()
        .map(|r| r.agent_id)
        .collect();
    assert_eq!(ids, vec!["sess-at".to_owned()]);
}

#[test]
fn unchanged_stat_skips_the_reparse() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
    let first = read_all(&runtime);
    assert_eq!(first[0].agent_id, "sess-1");

    // Rewrite the file in place with a different identity but identical
    // length, restoring the original mtime: the stat gate cannot tell it
    // changed, so the cached parse is served — which is exactly the
    // contract (every real update is an atomic rename of a fresh temp
    // file, so a same-stat file is byte-identical in production).
    let path = runtime.agent_context_path("claude", "sess-1");
    let original = std::fs::read(&path).unwrap();
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let swapped = String::from_utf8(original)
        .unwrap()
        .replace("sess-1", "sess-9");
    std::fs::write(&path, swapped).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_modified(mtime).unwrap();
    drop(f);
    assert_eq!(
        read_all(&runtime)[0].agent_id,
        "sess-1",
        "same (mtime, len) serves the cached parse — one stat, no read"
    );

    // A moved mtime invalidates: the rewrite is now visible.
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_modified(mtime + std::time::Duration::from_secs(3))
        .unwrap();
    drop(f);
    assert_eq!(read_all(&runtime)[0].agent_id, "sess-9");
}

#[test]
fn remove_targets_one_session() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    write(&runtime, "claude", "sess-1", &ctx(now)).unwrap();
    write(&runtime, "claude", "sess-2", &ctx(now)).unwrap();
    remove(&runtime, "claude", "sess-1").unwrap();
    let ids: Vec<_> = read_all(&runtime).into_iter().map(|r| r.agent_id).collect();
    assert_eq!(ids, vec!["sess-2".to_owned()]);
    // Removing an absent session is success.
    remove(&runtime, "claude", "sess-1").unwrap();
}
