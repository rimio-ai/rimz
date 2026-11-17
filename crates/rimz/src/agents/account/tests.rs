use super::*;
use crate::ids::AgentKind;
use jiff::SignedDuration;

#[test]
fn capacity_requires_a_read_after_the_marker_and_confirms_through_the_deadline() {
    let model = "fable";
    let marker = Timestamp::from_second(2_000_000_000).unwrap();
    let deadline = marker + crate::agents::model_refusal::CONFIRM_WINDOW;
    for (observed, since, confirms) in [
        (marker, false, false),
        (deadline, true, true),
        (deadline + SignedDuration::from_nanos(1), true, false),
    ] {
        let capacity = ProviderCapacity::from_windows(vec![RateLimitWindow {
            duration_mins: Some(300),
            used_percentage: Some(99),
            observed_at: Some(observed),
            ..Default::default()
        }]);
        assert_eq!(
            capacity.read_since(Some(model), marker),
            Some(since),
            "{observed}"
        );
        assert_eq!(
            capacity.confirms_refusal(model, marker),
            confirms,
            "{observed}"
        );
    }
}

#[test]
fn usage_keys_round_trip_legacy_and_scoped_text_and_reject_ambiguous_shapes() {
    for text in [
        "claude@work",
        "codex@default",
        "pi@default/anthropic/oauth",
        "pi@default/openai/oauth",
        "qwen@work/alibaba/international",
    ] {
        let parsed = text.parse::<AccountUsageKey>();
        assert!(parsed.is_ok(), "valid usage key {text}: {parsed:?}");
        let key = parsed.unwrap();
        assert_eq!(key.to_string(), text);
        let json = serde_json::to_string(&key).unwrap();
        assert_eq!(serde_json::from_str::<AccountUsageKey>(&json).unwrap(), key);
    }
    for text in [
        "pi",
        "@default",
        "pi@",
        "pi@default/anthropic",
        "pi@default//oauth",
        "pi@default/anthropic/",
        "pi@default/anthropic/oauth/extra",
    ] {
        assert!(
            text.parse::<AccountUsageKey>().is_err(),
            "invalid usage key {text}"
        );
    }
}

#[test]
fn rate_limit_paths_follow_the_cache_format_version() {
    let (_dir, runtime) = runtime();
    let version = RateLimitsCache::default().version;
    assert_eq!(
        RateLimitsCache::shared_path(&runtime),
        runtime.shared_rate_limits_path(version)
    );
    assert_eq!(
        RateLimitsCache::shared_lock(&runtime),
        runtime.shared_rate_limits_lock(version)
    );
}

#[test]
fn rate_limits_cache_skips_invalid_usage_keys_without_losing_valid_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rate_limits.json");
    crate::disk::atomic::write_temp_then_rename_cache(
        &path,
        &serde_json::json!({
            "version": 7,
            "refreshed_at_ms": 123,
            "entries": {
                "pi@default/azure/openai/oauth": {"limits": {"windows": []}},
                "pi@default/anthropic/oauth": {"unknown_since_ms": 42}
            }
        }),
    )
    .unwrap();
    let cache = read_rate_limits_cache(&path);
    assert_eq!(cache.entries.len(), 1, "only the invalid key is discarded");
    assert_eq!(cache.refreshed_at_ms, 123);
    assert_eq!(
        cache.entries[&"pi@default/anthropic/oauth".parse().unwrap()].unknown_since_ms,
        Some(42)
    );
}

#[test]
fn version_six_rate_limits_are_cold_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rate_limits.json");
    crate::disk::atomic::write_temp_then_rename_cache(
        &path,
        &serde_json::json!({
            "version": 6,
            "refreshed_at_ms": 123,
            "entries": {"claude@default": {"limits": {"windows": []}}}
        }),
    )
    .unwrap();
    let cache = read_rate_limits_cache(&path);
    assert!(
        cache.entries.is_empty(),
        "version 6 must not survive the scoped-key change"
    );
    assert_eq!(cache.version, 7);
}

#[test]
fn cached_usage_timestamp_reads_the_newest_scope_for_one_login() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let key: LoginKey = "pi@default".parse().unwrap();
    let limits = serde_json::json!({
        "version": 7,
        "refreshed_at_ms": 0,
        "entries": {
            "pi@default/anthropic/oauth": {
                "limits": {"windows": [{"observed_at": "2026-10-25T00:00:00Z"}]}
            },
            "pi@default/openai/oauth": {
                "limits": {"windows": [{"observed_at": "2026-10-26T00:00:00Z"}]}
            },
            "pi@work/openai/oauth": {
                "limits": {"windows": [{"observed_at": "2026-10-27T00:00:00Z"}]}
            }
        }
    });
    crate::disk::atomic::write_temp_then_rename_cache(
        &RateLimitsCache::shared_path(&runtime),
        &limits,
    )
    .unwrap();
    assert_eq!(
        cached_usage_read_at(&runtime, &key),
        Some("2026-10-26T00:00:00Z".parse().unwrap())
    );
    std::fs::remove_file(RateLimitsCache::shared_path(&runtime)).unwrap();
    let entry = |observed_at_ms| serde_json::json!({"observed_at_ms": observed_at_ms, "ok": true});
    crate::disk::atomic::write_temp_then_rename_cache(
        &runtime.shared_credits_path(),
        &serde_json::json!({
            "refreshed_at_ms": 0,
            "logins": {"pi@default": entry(100)},
            "scoped": {
                "pi@default/anthropic/oauth": entry(200),
                "pi@default/openai/oauth": entry(300),
                "pi@work/openai/oauth": entry(400)
            }
        }),
    )
    .unwrap();
    assert_eq!(
        cached_usage_read_at(&runtime, &key),
        Some(Timestamp::from_millisecond(300).unwrap())
    );
}

#[test]
fn a_record_reads_its_recorded_login_and_an_ambiguous_legacy_shape_as_unknown() {
    use ProviderStatus::{LoggedIn, LoggedOut, Unavailable};
    use RecordedLogin as Login;
    let version_only = || {
        Some(AgentAccount {
            version: Some("2.1.0".to_owned()),
            ..Default::default()
        })
    };
    let planned = || {
        Some(AgentAccount {
            plan: Some("Max".to_owned()),
            version: Some("2.1.0".to_owned()),
            ..Default::default()
        })
    };
    for (ok, account, login, expected, why) in [
        (
            true,
            version_only(),
            Some(Login::LoggedOut),
            LoggedOut,
            "a logout that kept the version",
        ),
        (
            true,
            None,
            Some(Login::LoggedOut),
            LoggedOut,
            "a recorded logout",
        ),
        (
            true,
            version_only(),
            Some(Login::LoggedIn),
            LoggedIn,
            "a login with no facts but a version",
        ),
        (
            true,
            Some(AgentAccount::default()),
            Some(Login::LoggedIn),
            LoggedIn,
            "a login with no facts",
        ),
        (
            true,
            version_only(),
            None,
            Unavailable,
            "an older build's ambiguous record",
        ),
        (
            true,
            Some(AgentAccount::default()),
            None,
            LoggedIn,
            "an older build's login with no facts or version",
        ),
        (true, planned(), None, LoggedIn, "an older build's login"),
        (true, None, None, LoggedOut, "an older build's logout"),
        (false, planned(), None, Unavailable, "a failed probe"),
    ] {
        let record = ProviderRecord {
            consecutive_failures: 0,
            version_probed_at_ms: None,
            inputs_stamp: None,
            profile: None,
            probe_error: None,
            probed_at_ms: 1,
            ok,
            account,
            login,
        };
        assert_eq!(
            ProviderStatus::from_record(Some(&record)),
            expected,
            "{why}"
        );
        assert_eq!(
            record.login_is_ambiguous(),
            why == "an older build's ambiguous record",
            "{why}"
        );
    }
    assert_eq!(ProviderStatus::from_record(None), Unavailable);

    let legacy: ProviderRecord =
        serde_json::from_str(r#"{"probed_at_ms":1,"ok":true,"account":null}"#).unwrap();
    assert_eq!(legacy.login, None);
    assert_eq!(legacy.probe_error, None);
    assert_eq!(
        legacy.inputs_stamp, None,
        "an older build's record has no stamp"
    );
    assert_eq!(
        serde_json::to_string(&legacy).unwrap(),
        r#"{"probed_at_ms":1,"ok":true,"account":null}"#,
        "an unrecorded login leaves the bytes an older build reads"
    );
    let stamped = ProviderRecord {
        version_probed_at_ms: None,
        inputs_stamp: Some(7),
        ..legacy.clone()
    };
    let stamped_json = r#"{"probed_at_ms":1,"ok":true,"inputs_stamp":7,"account":null}"#;
    assert_eq!(serde_json::to_string(&stamped).unwrap(), stamped_json);
    assert_eq!(
        serde_json::from_str::<ProviderRecord>(stamped_json).unwrap(),
        stamped
    );
    let recorded = ProviderRecord {
        version_probed_at_ms: None,
        probe_error: None,
        login: Some(Login::LoggedOut),
        ..legacy
    };
    assert_eq!(
        serde_json::to_string(&recorded).unwrap(),
        r#"{"probed_at_ms":1,"ok":true,"account":null,"login":"logged_out"}"#
    );
}

#[test]
fn launch_exhaustion_requires_positive_matching_evidence() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    for (reading, reset, duration, expected) in [
        (None, 3600, Some(300), false),
        (Some(99), 3600, Some(300), false),
        (Some(100), -1, Some(300), false),
        (Some(100), 3600, None, false),
        (Some(100), 3600, Some(300), true),
    ] {
        let capacity = ProviderCapacity::from_windows(vec![window(now, reading, reset, duration)]);
        assert_eq!(
            capacity.subscription_exhausted_for_model(now, Some("opus")),
            expected
        );
    }
    assert!(!ProviderCapacity::default().subscription_exhausted_for_model(now, Some("opus")));
    let mut model = window(now, Some(100), 3600, Some(300));
    model.scope = Some(crate::agents::RateLimitWindowScope {
        id: "model:opus".into(),
        label: "Opus".into(),
    });
    let capacity = ProviderCapacity::from_windows(vec![model]);
    assert!(capacity.subscription_exhausted_for_model(now, Some("claude-opus-4-6")));
    assert!(!capacity.subscription_exhausted_for_model(now, Some("gpt-6-astra")));
}

fn login_key(kind: &str) -> LoginKey {
    LoginKey::default_for(AgentKind::new_unchecked(kind))
}

fn window(
    now: Timestamp,
    used_percentage: Option<u8>,
    resets_in_secs: i64,
    duration_mins: Option<u32>,
) -> RateLimitWindow {
    RateLimitWindow {
        used_percentage,
        resets_at: Some(now + SignedDuration::from_secs(resets_in_secs)),
        duration_mins,
        ..RateLimitWindow::default()
    }
}

fn runtime() -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, runtime)
}

fn write_cache(runtime: &RuntimePaths, cache: &RateLimitsCache) {
    crate::disk::atomic::write_temp_then_rename_cache(
        &RateLimitsCache::shared_path(runtime),
        cache,
    )
    .unwrap();
}

#[test]
fn capacity_keeps_both_live_logins_separate() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let (_dir, runtime) = runtime();
    let accounts = toml::from_str("[claude.work]\nhome = '/srv/work'\n").unwrap();
    let catalog = crate::agents::LoginCatalog::from_config(&accounts).unwrap();
    let mut agent = crate::agents::AgentState::seed(
        AgentKind::new_unchecked("claude"),
        "root".into(),
        crate::agents::AgentStatus::Idle,
        now,
    );
    agent.login = Some("work".parse().unwrap());
    let logins = RoomLoginSet::new(Some(Default::default()), Some(catalog), BTreeMap::new())
        .with_agents(&[agent]);
    write_cache(
        &runtime,
        &RateLimitsCache {
            entries: [("claude@default", 0), ("claude@work", 100)]
                .into_iter()
                .map(|(key, used)| {
                    (
                        key.parse().unwrap(),
                        RateLimitCacheEntry {
                            limits: AgentRateLimits {
                                windows: vec![window(now, Some(used), 3600, Some(300))],
                            },
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ..Default::default()
        },
    );
    let capacities = ProviderCapacity::read_all(&runtime, &logins);
    assert_eq!(capacities.len(), 2);
    let by_name: BTreeMap<_, _> = capacities
        .into_iter()
        .map(|(key, capacity)| {
            (
                key.to_string(),
                capacity.subscription_exhausted_for_model(now, None),
            )
        })
        .collect();
    assert!(!by_name["claude@default"]);
    assert!(by_name["claude@work"]);
}

#[test]
fn go_console_windows_do_not_feed_kind_wide_capacity() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let (_dir, runtime) = runtime();
    write_cache(
        &runtime,
        &RateLimitsCache {
            entries: BTreeMap::from([(
                AccountUsageKey::new(
                    login_key("opencode"),
                    ProviderAccountScope::sub_provider("opencode", "oauth"),
                ),
                RateLimitCacheEntry {
                    scope: ProviderAccountScope::sub_provider("opencode", "oauth"),
                    limits: AgentRateLimits {
                        windows: vec![window(now, Some(100), 3_600, Some(300))],
                    },
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    );
    assert!(ProviderCapacity::read(&runtime, &login_key("opencode")).is_none());
    assert!(ProviderCapacity::read_all(&runtime, &RoomLoginSet::native()).is_empty());
}

#[test]
fn display_label_asks_the_adapter_before_naming_raw_scope_parts() {
    let label = |kind: &str, scope: ProviderAccountScope| {
        ProviderAccountBinding::new(scope, "owner".to_owned())
            .unwrap()
            .display_label(kind)
    };
    let alibaba = |region| ProviderAccountScope::sub_provider("alibaba", region);
    assert_eq!(label("qwen", alibaba("china")), "Qwen Alibaba China");
    assert_eq!(
        label("qwen", alibaba("international")),
        "Qwen Alibaba International"
    );
    assert_eq!(
        label(
            "codex",
            ProviderAccountScope::sub_provider("openai", "oauth")
        ),
        "codex openai oauth"
    );
    assert_eq!(
        label("unregistered", ProviderAccountScope::KindWide),
        "unregistered"
    );
}

#[test]
fn sub_provider_windows_require_an_exact_binding_for_launch_controls() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let scope = ProviderAccountScope::sub_provider("alibaba", "international");
    let binding = ProviderAccountBinding::new(scope.clone(), "owner".to_owned()).unwrap();
    let other = ProviderAccountBinding::new(scope.clone(), "other".to_owned()).unwrap();
    let usage_key = AccountUsageKey::new(login_key("qwen"), scope.clone());
    let mut cache = RateLimitsCache {
        entries: BTreeMap::from([(
            usage_key.clone(),
            RateLimitCacheEntry {
                scope,
                reset_spotted_at_ms: None,
                account_key: Some("owner".to_owned()),
                limits: AgentRateLimits {
                    windows: vec![
                        window(now, Some(20), 3_600, Some(300)),
                        window(now, Some(50), 2 * 86_400, Some(7 * 24 * 60)),
                        window(now, Some(20), 20 * 86_400, Some(30 * 24 * 60)),
                    ],
                },
                bound_limits: Some(AgentRateLimits {
                    windows: vec![
                        window(now, Some(20), 3_600, Some(300)),
                        window(now, Some(50), 2 * 86_400, Some(7 * 24 * 60)),
                        window(now, Some(100), 20 * 86_400, Some(30 * 24 * 60)),
                    ],
                }),
                pending: Vec::new(),
                unknown_since_ms: None,
            },
        )]),
        ..Default::default()
    };
    let (_dir, runtime) = runtime();
    write_cache(&runtime, &cache);
    assert!(ProviderCapacity::read(&runtime, &login_key("qwen")).is_none());
    assert!(ProviderCapacity::read_all(&runtime, &RoomLoginSet::native()).is_empty());
    let capacity = ProviderCapacity::read_bound(&runtime, &login_key("qwen"), &binding).unwrap();
    assert!(capacity.spent_window(now).is_some());
    assert!(ProviderCapacity::read_bound(&runtime, &login_key("qwen"), &other).is_none());
    let reason = provider_budget_gate(&runtime, &login_key("qwen"), &binding, now).unwrap();
    assert!(reason.contains("Qwen Alibaba International mo window exhausted"));
    assert!(!reason.contains("owner"));

    let mut entry = cache.entries.remove(&usage_key).unwrap();
    entry.scope = ProviderAccountScope::KindWide;
    entry.account_key = None;
    cache.entries.insert(login_key("qwen").into(), entry);
    write_cache(&runtime, &cache);
    assert!(ProviderCapacity::read(&runtime, &login_key("qwen")).is_some());
    assert!(
        ProviderCapacity::read_all(&runtime, &RoomLoginSet::native())
            .contains_key(&login_key("qwen"))
    );
}

#[test]
fn temporal_policy_fails_closed_for_incomplete_or_durationless_readings() {
    let now = Timestamp::from_second(1_000_000).unwrap();
    let mut named = window(now, Some(100), 86_400, Some(60));
    named.scope = Some(crate::agents::RateLimitWindowScope {
        id: "build_minutes".to_owned(),
        label: "bld".to_owned(),
    });
    let capacity = ProviderCapacity::from_windows(vec![named]);
    assert_eq!(capacity.latest_spent_window_reset(now), None);
    assert!(!capacity.subscription_budget_available(now));

    let durationless = ProviderCapacity::from_windows(vec![window(now, Some(100), 86_400, None)]);
    assert_eq!(durationless.latest_spent_window_reset(now), None);
    assert!(!durationless.subscription_budget_available(now));
}

#[test]
fn cache_read_cold_drops_corrupt_and_unknown_versions() {
    let (_dir, runtime) = runtime();
    assert!(
        read_rate_limits_cache(&RateLimitsCache::shared_path(&runtime))
            .entries
            .is_empty()
    );
    std::fs::write(RateLimitsCache::shared_path(&runtime), b"not-json").unwrap();
    assert!(
        read_rate_limits_cache(&RateLimitsCache::shared_path(&runtime))
            .entries
            .is_empty()
    );
    let cache = RateLimitsCache {
        version: RateLimitsCache::default().version + 1,
        entries: BTreeMap::from([(login_key("claude").into(), Default::default())]),
        ..Default::default()
    };
    write_cache(&runtime, &cache);
    assert!(
        read_rate_limits_cache(&RateLimitsCache::shared_path(&runtime))
            .entries
            .is_empty()
    );
    write_cache(
        &runtime,
        &RateLimitsCache {
            version: 5,
            entries: BTreeMap::from([(login_key("qwen").into(), Default::default())]),
            ..Default::default()
        },
    );
    assert!(
        read_rate_limits_cache(&RateLimitsCache::shared_path(&runtime))
            .entries
            .is_empty(),
        "v5 caches are kind-keyed, so their schema must cold-drop"
    );
}

#[test]
fn usage_reading_age_uses_cache_readers_and_credits_fallback() {
    let (_dir, runtime) = runtime();
    let key = login_key("codex");
    let stamp = Timestamp::from_millisecond(2_000_000_000_000).unwrap();
    let credits = crate::agents::credits::CreditsCache {
        logins: BTreeMap::from([(
            key.clone(),
            crate::agents::credits::ProviderCreditsEntry {
                observed_at_ms: stamp.as_millisecond() as u64,
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    crate::disk::atomic::write_temp_then_rename_cache(&runtime.shared_credits_path(), &credits)
        .unwrap();
    assert_eq!(cached_usage_read_at(&runtime, &key), Some(stamp));
    let newer = stamp + SignedDuration::from_secs(60);
    let mut cache = RateLimitsCache {
        entries: BTreeMap::from([(
            key.clone().into(),
            RateLimitCacheEntry {
                limits: crate::agents::AgentRateLimits {
                    windows: vec![RateLimitWindow {
                        observed_at: Some(newer),
                        ..Default::default()
                    }],
                },
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    write_cache(&runtime, &cache);
    assert_eq!(cached_usage_read_at(&runtime, &key), Some(newer));
    cache.version += 1;
    write_cache(&runtime, &cache);
    assert_eq!(cached_usage_read_at(&runtime, &key), Some(stamp));
    assert_eq!(cached_usage_read_at(&runtime, &login_key("claude")), None);
}

#[test]
fn model_capacity_matches_only_the_model_family() {
    let now = Timestamp::from_second(1_000_000).unwrap();
    let reset = now + SignedDuration::from_secs(3_600);
    let mut scoped = window(now, Some(100), 3_600, Some(300));
    scoped.scope = Some(crate::agents::RateLimitWindowScope {
        id: "model:fable".to_owned(),
        label: "Fable".to_owned(),
    });
    let capacity =
        ProviderCapacity::from_windows(vec![window(now, Some(20), 3_600, Some(300)), scoped]);
    for (model, matches) in [
        (Some("claude-fable-5-1"), true),
        (Some("claude-fable-5-1-20260801"), true),
        (Some("claude-opus-4-8"), false),
        (Some("Fable 5.1@high"), true),
        (Some("fAbLe"), true),
        (Some("Fablework"), false),
        (Some("Opus 4.6"), false),
        (None, false),
    ] {
        assert_eq!(
            capacity.latest_spent_window_reset_for_model(now, model),
            matches.then_some(reset),
            "{model:?}"
        );
        assert_eq!(
            capacity.subscription_budget_available_for_model(now, model),
            !matches,
            "{model:?}"
        );
        assert!(capacity.subscription_budget_available_for_model(reset, model));
    }
    assert_eq!(capacity.latest_spent_window_reset(now), None);
    assert!(capacity.subscription_budget_available(now));
}

#[test]
fn known_unused_counts_lifted_windows_but_requires_unused_enforced_windows() {
    let now = Timestamp::from_second(1_000_000).unwrap();
    let lifted = RateLimitWindow {
        duration_mins: Some(300),
        source: crate::agents::context::WindowSource::Authoritative,
        lifted: true,
        ..Default::default()
    };
    let unused = ProviderCapacity::from_windows(vec![
        lifted.clone(),
        window(now, Some(0), 86_400, Some(10_080)),
    ]);
    assert!(
        unused.known_unused(now),
        "a lifted window has no clock to start"
    );
    for used in [Some(40), None] {
        let capacity = ProviderCapacity::from_windows(vec![
            lifted.clone(),
            window(now, used, 86_400, Some(10_080)),
        ]);
        assert!(!capacity.known_unused(now), "used or unknown 7d: {used:?}");
    }
    assert!(!ProviderCapacity::default().known_unused(now));
}

#[test]
fn spent_reset_and_available_capacity_use_projected_windows() {
    let now = Timestamp::from_second(1_000_000).unwrap();
    let spent = ProviderCapacity::from_windows(vec![window(now, Some(100), 3_600, Some(300))]);
    assert_eq!(
        spent.latest_spent_window_reset(now),
        Some(now + SignedDuration::from_secs(3_600))
    );
    assert!(!spent.subscription_budget_available(now));

    let available = ProviderCapacity::from_windows(vec![window(now, Some(20), 3_600, Some(300))]);
    assert_eq!(available.latest_spent_window_reset(now), None);
    assert!(available.subscription_budget_available(now));
}

#[test]
fn window_span_parses_displays_and_refuses_unknown_spans() {
    for (raw, span) in [("5h", WindowSpan::FiveHour), ("7d", WindowSpan::SevenDay)] {
        assert_eq!(raw.parse::<WindowSpan>(), Ok(span));
        assert_eq!(span.to_string(), raw);
    }
    assert_eq!(WindowSpan::FiveHour.minutes(), 5 * 60);
    assert_eq!(WindowSpan::SevenDay.minutes(), 7 * 24 * 60);
    for raw in ["1h", "5H", "", "300"] {
        let error = raw.parse::<WindowSpan>().unwrap_err();
        assert!(error.contains("use 5h or 7d"), "{raw}: {error}");
    }
}

#[test]
fn window_of_span_selects_the_unscoped_window_projected_at_now() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let five_hours = WindowSpan::FiveHour.minutes();
    let week = WindowSpan::SevenDay.minutes();
    let mut model = window(now, Some(90), 3_600, Some(five_hours));
    model.scope = Some(crate::agents::RateLimitWindowScope {
        id: "model:opus".into(),
        label: "Opus".into(),
    });
    let capacity = ProviderCapacity::from_windows(vec![
        model,
        window(now, Some(40), 3_600, Some(five_hours)),
        window(now, Some(70), -60, Some(week)),
    ]);
    let five = capacity.window_of_span(WindowSpan::FiveHour, now).unwrap();
    assert_eq!(
        five.used_percentage,
        Some(40),
        "the model sub-cap never matches"
    );
    assert_eq!(five.resets_at, Some(now + SignedDuration::from_secs(3_600)));
    let seven = capacity.window_of_span(WindowSpan::SevenDay, now).unwrap();
    assert_eq!(
        seven.used_percentage,
        Some(0),
        "a passed reset projects to a fresh window"
    );
    assert!(seven.not_started(now));
    let only_five =
        ProviderCapacity::from_windows(vec![window(now, Some(1), 60, Some(five_hours))]);
    assert_eq!(only_five.window_of_span(WindowSpan::SevenDay, now), None);
    assert_eq!(
        ProviderCapacity::default().window_of_span(WindowSpan::FiveHour, now),
        None
    );
    // Kimi reports two unscoped weekly rows; the span reads the one the
    // surplus gate's longest-window choice reads, the last.
    let twin_weeks = ProviderCapacity::from_windows(vec![
        window(now, Some(25), 3_600, Some(week)),
        window(now, Some(10), 3_600, Some(week)),
    ]);
    assert_eq!(
        twin_weeks
            .window_of_span(WindowSpan::SevenDay, now)
            .unwrap()
            .used_percentage,
        Some(10)
    );
}

#[test]
fn a_standing_refusal_vetoes_its_model_on_its_login_only() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let (_dir, runtime) = runtime();
    let unspent = |used| RateLimitCacheEntry {
        limits: AgentRateLimits {
            windows: vec![window(now, Some(used), 3_600, Some(300))],
        },
        ..Default::default()
    };
    write_cache(
        &runtime,
        &RateLimitsCache {
            entries: [("claude@default", 20), ("claude@work", 20)]
                .into_iter()
                .map(|(key, used)| (key.parse().unwrap(), unspent(used)))
                .collect(),
            ..Default::default()
        },
    );
    let refusals = |succeeded: &str| {
        format!(
            r#"{{"logins":{{"claude@default":{{"fable 5.2":{{"refused_at":"2033-05-18T03:00:00Z"{succeeded},"label":"Fable 5.2 requires usage credits."}}}}}}}}"#
        )
    };
    std::fs::write(runtime.shared_model_refusals_path(), refusals("")).unwrap();
    let default = login_key("claude");
    let capacity = ProviderCapacity::read(&runtime, &default).unwrap();
    for (model, refused) in [
        (Some("claude-fable-5-2"), true),
        (Some("claude-fable-5-2-20260801"), true),
        (Some("Fable 5.2 (1M context)"), true),
        (Some("fable"), true),
        (Some("claude-fable-5-5"), false),
        (Some("claude-opus-4-8"), false),
        (None, false),
    ] {
        assert_eq!(capacity.model_exhausted(now, model), refused, "{model:?}");
        assert_eq!(
            capacity.subscription_exhausted_for_model(now, model),
            refused,
            "{model:?}"
        );
        assert_eq!(
            capacity.subscription_budget_available_for_model(now, model),
            !refused,
            "{model:?}"
        );
        assert_eq!(
            capacity.latest_spent_window_reset_for_model(now, model),
            None
        );
    }
    assert_eq!(
        capacity
            .refusal(Some("claude-fable-5-2"))
            .and_then(|refusal| refusal.label.as_deref()),
        Some("Fable 5.2 requires usage credits.")
    );

    // A family alias matches a recorded version of its family, never a key that only starts
    // with the family's name.
    std::fs::write(
        runtime.shared_model_refusals_path(),
        r#"{"logins":{"claude@default":{"fable plan":{"refused_at":"2033-05-18T03:00:00Z"}}}}"#,
    )
    .unwrap();
    let unversioned = ProviderCapacity::read(&runtime, &default).unwrap();
    assert!(!unversioned.model_refused(Some("fable")));
    std::fs::write(runtime.shared_model_refusals_path(), refusals("")).unwrap();

    // The ping and compaction tests read the agent's current model: the context's id, which an
    // idle `/model` moves, ahead of the lifecycle model.
    let mut agent = crate::agents::AgentState::seed(
        AgentKind::new_unchecked("claude"),
        "root".into(),
        crate::agents::AgentStatus::Idle,
        now,
    );
    agent.model = Some("claude-opus-4-8".to_owned());
    agent.context = Some(crate::agents::AgentContext {
        model_id: Some("claude-fable-5-2".to_owned()),
        ..Default::default()
    });
    assert!(capacity.window_spent(&agent, now));
    assert!(capacity.compaction_blocked(&agent, now, || true));
    assert!(ProviderCapacity::login_window_spent(&runtime, &agent, now));
    assert!(ProviderCapacity::login_compaction_blocked(
        &runtime, &agent, now
    ));
    let mut moved = agent.clone();
    moved.context = None;
    assert!(!capacity.window_spent(&moved, now));
    assert!(!capacity.compaction_blocked(&moved, now, || false));

    // Another login of the same kind is not refused, through either reader.
    let mut work = agent.clone();
    work.login = Some("work".parse().unwrap());
    assert!(!ProviderCapacity::login_window_spent(&runtime, &work, now));
    let accounts = toml::from_str("[claude.work]\nhome = '/srv/work'\n").unwrap();
    let catalog = crate::agents::LoginCatalog::from_config(&accounts).unwrap();
    let logins = RoomLoginSet::new(Some(Default::default()), Some(catalog), BTreeMap::new())
        .with_agents(&[agent.clone(), work]);
    let all = ProviderCapacity::read_all(&runtime, &logins);
    assert!(all[&default].model_refused(Some("claude-fable-5-2")));
    assert!(!all[&"claude@work".parse().unwrap()].model_refused(Some("claude-fable-5-2")));

    // A success after the refusal clears it; an unreadable file reads as no records.
    std::fs::write(
        runtime.shared_model_refusals_path(),
        refusals(r#","succeeded_at":"2033-05-18T03:05:00Z""#),
    )
    .unwrap();
    let cleared = ProviderCapacity::read(&runtime, &default).unwrap();
    assert!(!cleared.model_refused(Some("claude-fable-5-2")));
    assert!(cleared.refusal(Some("claude-fable-5-2")).is_some());
    assert!(!cleared.window_spent(&agent, now));
    std::fs::write(runtime.shared_model_refusals_path(), "{not json").unwrap();
    let unreadable = ProviderCapacity::read(&runtime, &default).unwrap();
    assert!(!unreadable.model_refused(Some("claude-fable-5-2")));
    assert!(!unreadable.window_spent(&agent, now));
}

#[test]
fn a_bare_alias_reads_a_standing_version_ahead_of_its_own_cleared_record() {
    let (_dir, runtime) = runtime();
    write_cache(
        &runtime,
        &RateLimitsCache {
            entries: [(
                "claude@default".parse().unwrap(),
                RateLimitCacheEntry::default(),
            )]
            .into_iter()
            .collect(),
            ..Default::default()
        },
    );
    std::fs::write(
        runtime.shared_model_refusals_path(),
        r#"{"logins":{"claude@default":{
            "fable":{"refused_at":"2033-05-18T03:00:00Z","succeeded_at":"2033-05-18T03:05:00Z"},
            "fable 5.2":{"refused_at":"2033-05-18T04:00:00Z","label":"refused again"}}}}"#,
    )
    .unwrap();
    let capacity = ProviderCapacity::read(&runtime, &login_key("claude")).unwrap();
    assert!(
        capacity.model_refused(Some("fable")),
        "the standing version wins"
    );
    assert_eq!(
        capacity
            .refusal(Some("fable"))
            .and_then(|refusal| refusal.label.as_deref()),
        Some("refused again")
    );
    assert!(capacity.model_refused(Some("claude-fable-5-2")));
    assert!(
        !capacity.model_refused(Some("claude-fable-5-5")),
        "a sibling version with neither its own record nor a standing family alias"
    );

    // A standing record under the alias holds every version of the family.
    std::fs::write(
        runtime.shared_model_refusals_path(),
        r#"{"logins":{"claude@default":{
            "fable":{"refused_at":"2033-05-18T03:00:00Z","label":"alias refused"},
            "fable 5.2":{"refused_at":"2033-05-18T02:00:00Z","succeeded_at":"2033-05-18T02:30:00Z"}}}}"#,
    )
    .unwrap();
    let capacity = ProviderCapacity::read(&runtime, &login_key("claude")).unwrap();
    for model in ["claude-fable-5-2", "claude-fable-5-5"] {
        assert_eq!(
            capacity
                .refusal(Some(model))
                .filter(|refusal| refusal.stands())
                .and_then(|refusal| refusal.label.as_deref()),
            Some("alias refused"),
            "{model}"
        );
    }
}

#[test]
fn a_standing_refusal_vetoes_with_no_rate_limit_entry_for_its_login() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let (_dir, runtime) = runtime();
    std::fs::write(
        runtime.shared_model_refusals_path(),
        r#"{"logins":{"claude@default":{"fable 5.2":{"refused_at":"2033-05-18T03:00:00Z"}}}}"#,
    )
    .unwrap();
    let default = login_key("claude");
    let capacity = ProviderCapacity::read(&runtime, &default).expect("the record alone reads");
    assert!(capacity.model_exhausted(now, Some("claude-fable-5-2")));
    let agent = crate::agents::AgentState::seed(
        AgentKind::new_unchecked("claude"),
        "root".into(),
        crate::agents::AgentStatus::Idle,
        now,
    );
    let logins =
        RoomLoginSet::new(Some(Default::default()), None, BTreeMap::new()).with_agents(&[agent]);
    let all = ProviderCapacity::read_all(&runtime, &logins);
    assert!(all[&default].model_refused(Some("claude-fable-5-2")));
}
