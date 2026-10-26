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
        sub_provider: None,
        effort: None,
        fast: None,
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
        turn_opened_by: Vec::new(),
        turn_error: None,
        settle: None,
        observed_at,
    }
}

#[test]
fn write_then_read_round_trips() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    write(&runtime, "claude", "sess-1", &ctx(now), None).unwrap();
    let all = read_all(&runtime);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].kind, "claude");
    assert_eq!(all[0].agent_id, "sess-1");
    assert_eq!(all[0].context.model_id.as_deref(), Some("claude-opus-4-8"));
}

#[test]
fn statusline_windows_keep_content_stamps_per_key() {
    use crate::agents::{AgentRateLimits, RateLimitWindow};
    use jiff::SignedDuration;

    let (_dir, runtime) = runtime();
    let start = Timestamp::now();
    let reset = start + SignedDuration::from_secs(86_400);
    let windows = vec![
        RateLimitWindow {
            duration_mins: Some(300),
            used_percentage: Some(20),
            resets_at: Some(reset),
            ..Default::default()
        },
        RateLimitWindow {
            duration_mins: Some(10_080),
            used_percentage: Some(95),
            resets_at: Some(reset),
            ..Default::default()
        },
    ];
    for (seconds, change_weekly, drop_short, expected) in [
        (0, false, false, vec![0, 0]),
        (20, false, false, vec![0, 0]),
        (30, true, false, vec![0, 30]),
        (40, true, true, vec![30]),
    ] {
        let at = start + SignedDuration::from_secs(seconds);
        let mut context = ctx(at);
        let mut windows = windows.clone();
        if change_weekly {
            windows[1].used_percentage = Some(5);
        }
        if drop_short {
            windows.remove(0);
        }
        context.rate_limits = Some(AgentRateLimits { windows }.stamped_at(at));
        write(&runtime, "claude", "sess-1", &context, None).unwrap();
        let stored = read_one(&runtime, "claude", "sess-1").unwrap().context;
        assert_eq!(stored.observed_at, at);
        assert_eq!(
            stored
                .rate_limits
                .unwrap()
                .windows
                .iter()
                .map(|window| window.observed_at)
                .collect::<Vec<_>>(),
            expected
                .into_iter()
                .map(|seconds| Some(start + SignedDuration::from_secs(seconds)))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn statusline_window_stamp_changes_only_for_new_content() {
    use crate::agents::context::WindowSource;
    use crate::agents::{AgentRateLimits, RateLimitWindow};
    use jiff::SignedDuration;

    let (_dir, runtime) = runtime();
    let start = Timestamp::now();
    let at = start + SignedDuration::from_secs(20);
    let reset = start + SignedDuration::from_secs(86_400);
    for (name, prior_stamp, source, reset_change, expected) in [
        ("unstamped", None, WindowSource::BestEffort, 0, at),
        (
            "authoritative",
            Some(start),
            WindowSource::Authoritative,
            0,
            at,
        ),
        ("new-reset", Some(start), WindowSource::BestEffort, 1, at),
        ("unchanged", Some(start), WindowSource::BestEffort, 0, start),
    ] {
        let mut context = ctx(start);
        context.rate_limits = Some(AgentRateLimits {
            windows: vec![RateLimitWindow {
                used_percentage: Some(5),
                duration_mins: Some(10_080),
                resets_at: Some(reset),
                observed_at: prior_stamp,
                source,
                ..Default::default()
            }],
        });
        write(&runtime, "claude", name, &context, None).unwrap();
        context.observed_at = at;
        let window = &mut context.rate_limits.as_mut().unwrap().windows[0];
        window.observed_at = Some(at);
        window.resets_at = Some(reset + SignedDuration::from_secs(reset_change));
        write(&runtime, "claude", name, &context, None).unwrap();
        assert_eq!(
            read_one(&runtime, "claude", name)
                .unwrap()
                .context
                .rate_limits
                .unwrap()
                .windows[0]
                .observed_at,
            Some(expected),
            "{name}"
        );
    }
}

#[test]
fn first_statusline_windows_use_the_owner_start_floor() {
    use crate::agents::context::WindowSource;
    use crate::agents::{AgentRateLimits, RateLimitWindow};
    use jiff::SignedDuration;

    let (_dir, runtime) = runtime();
    let capture = Timestamp::from_second(1_700_000_100).unwrap();
    let older = capture - SignedDuration::from_secs(60);
    for prior in ["missing", "hook", "empty"] {
        for (index, owner) in [
            None,
            Some(capture + SignedDuration::from_secs(1)),
            Some(older),
        ]
        .into_iter()
        .enumerate()
        {
            let session = format!("{prior}-{index}");
            if prior != "missing" {
                let mut context = ctx(capture);
                if prior == "empty" {
                    context.rate_limits = Some(AgentRateLimits::default());
                }
                write(&runtime, "claude", &session, &context, None).unwrap();
            }
            let mut context = ctx(capture);
            context.rate_limits = Some(AgentRateLimits {
                windows: [WindowSource::BestEffort, WindowSource::Authoritative]
                    .into_iter()
                    .map(|source| RateLimitWindow {
                        duration_mins: Some(if source.is_authoritative() {
                            300
                        } else {
                            10_080
                        }),
                        used_percentage: Some(95),
                        observed_at: Some(capture),
                        source,
                        ..Default::default()
                    })
                    .collect(),
            });
            write(&runtime, "claude", &session, &context, owner).unwrap();
            let windows = read_one(&runtime, "claude", &session)
                .unwrap()
                .context
                .rate_limits
                .unwrap()
                .windows;
            assert_eq!(
                windows[0].observed_at,
                Some(owner.map_or(capture, |start| capture.min(start))),
                "{session}"
            );
            assert_eq!(
                windows[1].observed_at,
                Some(capture),
                "authoritative {session}"
            );
        }
    }
}

#[test]
fn a_new_window_in_a_populated_sidecar_keeps_capture_time() {
    use crate::agents::{AgentRateLimits, RateLimitWindow};
    use jiff::SignedDuration;

    let (_dir, runtime) = runtime();
    let capture = Timestamp::from_second(1_700_000_100).unwrap();
    let owner = capture - SignedDuration::from_secs(60);
    let mut context = ctx(capture);
    context.rate_limits = Some(AgentRateLimits {
        windows: vec![RateLimitWindow {
            duration_mins: Some(10_080),
            used_percentage: Some(95),
            observed_at: Some(capture),
            ..Default::default()
        }],
    });
    write(&runtime, "claude", "session", &context, Some(owner)).unwrap();
    let later = capture + SignedDuration::from_secs(20);
    context.observed_at = later;
    let windows = &mut context.rate_limits.as_mut().unwrap().windows;
    windows[0].observed_at = Some(later);
    windows.push(RateLimitWindow {
        duration_mins: Some(300),
        used_percentage: Some(2),
        observed_at: Some(later),
        ..Default::default()
    });
    write(&runtime, "claude", "session", &context, Some(owner)).unwrap();
    let windows = read_one(&runtime, "claude", "session")
        .unwrap()
        .context
        .rate_limits
        .unwrap()
        .windows;
    assert_eq!(windows[0].observed_at, Some(owner));
    assert_eq!(windows[1].observed_at, Some(later));
}

#[test]
fn old_context_record_without_current_scalar_still_decodes() {
    let now = Timestamp::now();
    let mut record = AgentContextRecord::new("qwen", "sess-1", ctx(now));
    record.context.tokens = Some(crate::agents::AgentTokenUsage {
        context_window_size: Some(1_000_000),
        current_context_tokens: Some(42),
        ..Default::default()
    });
    let mut value = serde_json::to_value(record).unwrap();
    value
        .pointer_mut("/context/tokens")
        .and_then(serde_json::Value::as_object_mut)
        .unwrap()
        .remove("current_context_tokens");

    let decoded: AgentContextRecord = serde_json::from_value(value).unwrap();
    assert_eq!(decoded.context.tokens.unwrap().current_context_tokens, None);
}

#[test]
fn turn_openers_replace_and_survive_statusline_refresh() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    let first = crate::ids::MessageId::parse("msg_0123456789abcdef").unwrap();
    let second = crate::ids::MessageId::parse("msg_123456789abcdef0").unwrap();

    assert!(merge_turn_opened_by(&runtime, "claude", "sess-1", vec![first.clone()]).unwrap());
    write(&runtime, "claude", "sess-1", &ctx(now), None).unwrap();
    assert_eq!(
        read_one(&runtime, "claude", "sess-1")
            .unwrap()
            .context
            .turn_opened_by,
        vec![first]
    );

    assert!(merge_turn_opened_by(&runtime, "claude", "sess-1", vec![second.clone()]).unwrap());
    assert_eq!(
        read_one(&runtime, "claude", "sess-1")
            .unwrap()
            .context
            .turn_opened_by,
        vec![second]
    );
    assert!(merge_turn_opened_by(&runtime, "claude", "sess-1", Vec::new()).unwrap());
    assert!(
        read_one(&runtime, "claude", "sess-1")
            .unwrap()
            .context
            .turn_opened_by
            .is_empty()
    );
}

#[test]
fn statusline_refresh_replaces_the_live_fast_reading() {
    let (_dir, runtime) = runtime();
    for fast in [Some(true), Some(false), None] {
        let mut context = ctx(Timestamp::now());
        context.fast = fast;
        write(&runtime, "claude", "sess-1", &context, None).unwrap();
        assert_eq!(
            read_one(&runtime, "claude", "sess-1").unwrap().context.fast,
            fast
        );
    }
}

#[test]
fn session_usage_survives_whole_context_statusline_refresh() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    let mut folded = ctx(now);
    folded.tokens = Some(crate::agents::AgentTokenUsage {
        session_usage: Some(crate::agents::AgentSessionUsage {
            input_tokens: Some(10),
            cache_read_input_tokens: Some(90),
            ..Default::default()
        }),
        ..Default::default()
    });
    write(&runtime, "claude", "sess-1", &folded, None).unwrap();

    let mut statusline = ctx(now);
    statusline.tokens = Some(crate::agents::AgentTokenUsage {
        context_window_size: Some(200_000),
        used_percentage: Some(25),
        ..Default::default()
    });
    write(&runtime, "claude", "sess-1", &statusline, None).unwrap();
    let tokens = read_one(&runtime, "claude", "sess-1")
        .unwrap()
        .context
        .tokens
        .unwrap();
    assert_eq!(tokens.used_percentage, Some(25));
    assert_eq!(
        tokens
            .session_usage
            .and_then(|usage| usage.cache_hit_percent()),
        Some(90)
    );

    write(&runtime, "claude", "sess-1", &ctx(now), None).unwrap();
    assert_eq!(
        read_one(&runtime, "claude", "sess-1")
            .unwrap()
            .context
            .tokens
            .and_then(|tokens| tokens.session_usage)
            .and_then(|usage| usage.cache_hit_percent()),
        Some(90),
        "a statusline without any token reading still retains the projection"
    );
}

#[test]
fn statusline_refresh_keeps_the_local_fold_bound_to_its_transcript() {
    let (_dir, runtime) = runtime();
    let now = Timestamp::now();
    let mut record = AgentContextRecord::new("claude", "sess-1", ctx(now));
    record.transcript_path = Some("/tmp/sess-1.jsonl".to_owned());
    record.transcript_stat = Some(crate::agents::TranscriptStat {
        mtime_secs: 1,
        mtime_nanos: 2,
        len: 3,
        companion: None,
    });
    let mut spend_fold = crate::agents::LocalSpendFold::default();
    spend_fold.input = 10;
    spend_fold.cache_read = 90;
    record.spend_fold = Some(spend_fold);
    write_record(&runtime, &record).unwrap();

    write(&runtime, "claude", "sess-1", &ctx(now), None).unwrap();
    let refreshed = read_one(&runtime, "claude", "sess-1").unwrap();
    assert_eq!(
        refreshed.transcript_path.as_deref(),
        Some("/tmp/sess-1.jsonl")
    );
    assert_eq!(refreshed.transcript_stat, None);
    assert_eq!(refreshed.spend_fold, record.spend_fold);
}

#[test]
fn locally_priced_cost_deduplicates_accumulates_and_survives_statusline_refresh() {
    let (_dir, runtime) = runtime();
    let first = crate::agents::LocallyPricedTurnCost {
        turn_id: "gen-1".to_owned(),
        cost_usd: 0.25,
    };
    assert!(merge_locally_priced_cost(&runtime, "cursor", "sess-1", &first).unwrap());
    assert!(!merge_locally_priced_cost(&runtime, "cursor", "sess-1", &first).unwrap());

    let mut context = ctx(Timestamp::now());
    context.source = "cursor".to_owned();
    context.model_id = Some("auto".to_owned());
    write(&runtime, "cursor", "sess-1", &context, None).unwrap();
    let second = crate::agents::LocallyPricedTurnCost {
        turn_id: "gen-2".to_owned(),
        cost_usd: 0.5,
    };
    assert!(merge_locally_priced_cost(&runtime, "cursor", "sess-1", &second).unwrap());

    let record = read_one(&runtime, "cursor", "sess-1").unwrap();
    assert_eq!(record.context.model_id.as_deref(), Some("auto"));
    assert_eq!(
        record
            .context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_cost_usd),
        Some(0.75)
    );
    assert_eq!(
        record.context.cost.as_ref().map(|cost| cost.coverage),
        Some(crate::agents::CostCoverage::Session)
    );
}

#[test]
fn observed_cost_ownership_wins_over_later_local_accumulation() {
    let (_dir, runtime) = runtime();
    let first = crate::agents::LocallyPricedTurnCost {
        turn_id: "gen-1".to_owned(),
        cost_usd: 0.25,
    };
    merge_locally_priced_cost(&runtime, "cursor", "sess-1", &first).unwrap();

    let mut context = ctx(Timestamp::now());
    context.source = "cursor".to_owned();
    context.cost = Some(crate::agents::AgentCost {
        total_cost_usd: Some(10.0),
        ..Default::default()
    });
    write(&runtime, "cursor", "sess-1", &context, None).unwrap();
    let second = crate::agents::LocallyPricedTurnCost {
        turn_id: "gen-2".to_owned(),
        cost_usd: 0.5,
    };
    assert!(merge_locally_priced_cost(&runtime, "cursor", "sess-1", &second).unwrap());

    let record = read_one(&runtime, "cursor", "sess-1").unwrap();
    assert_eq!(
        record
            .context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_cost_usd),
        Some(10.0)
    );
    assert_eq!(
        record.context.cost.as_ref().map(|cost| cost.coverage),
        Some(crate::agents::CostCoverage::Session)
    );
}

#[test]
fn statusline_and_locally_priced_cost_writers_serialize_without_lost_fields() {
    let (_dir, runtime) = runtime();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let statusline_runtime = runtime.clone();
    let statusline_barrier = barrier.clone();
    let statusline = std::thread::spawn(move || {
        let mut context = ctx(Timestamp::now());
        context.source = "cursor".to_owned();
        context.model_display_name = Some("Auto".to_owned());
        statusline_barrier.wait();
        write(&statusline_runtime, "cursor", "sess-race", &context, None).unwrap();
    });
    let cost_runtime = runtime.clone();
    let cost_barrier = barrier.clone();
    let cost = std::thread::spawn(move || {
        cost_barrier.wait();
        merge_locally_priced_cost(
            &cost_runtime,
            "cursor",
            "sess-race",
            &crate::agents::LocallyPricedTurnCost {
                turn_id: "gen-1".to_owned(),
                cost_usd: 0.25,
            },
        )
        .unwrap();
    });
    barrier.wait();
    statusline.join().unwrap();
    cost.join().unwrap();

    let record = read_one(&runtime, "cursor", "sess-race").unwrap();
    assert_eq!(record.context.model_display_name.as_deref(), Some("Auto"));
    assert_eq!(
        record
            .context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_cost_usd),
        Some(0.25)
    );
}

#[test]
fn old_record_is_read_liveness_gating_is_the_rollups_job() {
    let (_dir, runtime) = runtime();
    let old = Timestamp::from_second(0).unwrap();
    write(&runtime, "claude", "sess-old", &ctx(old), None).unwrap();

    let all = read_all(&runtime);

    assert_eq!(all.len(), 1);
    assert_eq!(all[0].agent_id, "sess-old");
    assert_eq!(all[0].context.observed_at, old);
}

mod merge;
