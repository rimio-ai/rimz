use super::*;
use crate::ids::AgentKind;

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
    assert_eq!(
        serde_json::to_string(&legacy).unwrap(),
        r#"{"probed_at_ms":1,"ok":true,"account":null}"#,
        "an unrecorded login leaves the bytes an older build reads"
    );
    let recorded = ProviderRecord {
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
    crate::disk::atomic::write_temp_then_rename_cache(&runtime.shared_rate_limits_path(), cache)
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
fn sub_provider_windows_require_an_exact_binding_for_launch_controls() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let scope = ProviderAccountScope::sub_provider("alibaba", "international");
    let binding = ProviderAccountBinding::new(scope.clone(), "owner".to_owned()).unwrap();
    let other = ProviderAccountBinding::new(scope.clone(), "other".to_owned()).unwrap();
    let mut cache = RateLimitsCache {
        entries: BTreeMap::from([(
            login_key("qwen"),
            RateLimitCacheEntry {
                scope,
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
    assert!(capacity.longest_window_surplus(now).is_some());
    assert!(capacity.spent_window(now).is_some());
    assert!(ProviderCapacity::read_bound(&runtime, &login_key("qwen"), &other).is_none());
    let reason = provider_budget_gate(&runtime, &login_key("qwen"), &binding, now).unwrap();
    assert!(reason.contains("Qwen Alibaba International 30d window exhausted"));
    assert!(!reason.contains("owner"));

    cache.entries.get_mut(&login_key("qwen")).unwrap().scope = ProviderAccountScope::KindWide;
    cache
        .entries
        .get_mut(&login_key("qwen"))
        .unwrap()
        .account_key = None;
    write_cache(&runtime, &cache);
    assert!(ProviderCapacity::read(&runtime, &login_key("qwen")).is_some());
    assert!(
        ProviderCapacity::read_all(&runtime, &RoomLoginSet::native())
            .contains_key(&login_key("qwen"))
    );
}

#[test]
fn managed_launch_state_selects_only_applicable_capacity() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let kind_wide = |kind: &str| {
        (
            login_key(kind),
            RateLimitCacheEntry {
                scope: ProviderAccountScope::KindWide,
                limits: AgentRateLimits {
                    windows: vec![window(now, Some(20), 3_600, Some(300))],
                },
                ..Default::default()
            },
        )
    };
    let mut cache = RateLimitsCache {
        entries: BTreeMap::from([kind_wide("claude"), kind_wide("qwen")]),
        ..Default::default()
    };
    let (_dir, runtime) = runtime();
    write_cache(&runtime, &cache);

    assert!(
        ManagedLaunchState::Unsupported
            .capacity(&runtime, &login_key("claude"))
            .is_some()
    );
    assert!(
        ManagedLaunchState::Unresolved
            .capacity(&runtime, &login_key("qwen"))
            .is_none()
    );

    let scope = ProviderAccountScope::sub_provider("alibaba", "international");
    cache.entries.insert(
        login_key("qwen"),
        RateLimitCacheEntry {
            scope: scope.clone(),
            account_key: Some("cached".to_owned()),
            limits: AgentRateLimits {
                windows: vec![window(now, Some(20), 3_600, Some(300))],
            },
            ..Default::default()
        },
    );
    write_cache(&runtime, &cache);
    let other = ProviderAccountBinding::new(scope, "other".to_owned()).unwrap();
    assert!(
        ManagedLaunchState::Bound(other)
            .capacity(&runtime, &login_key("qwen"))
            .is_none()
    );
}

#[test]
fn capacity_selects_temporal_windows_and_measures_surplus() {
    let now = Timestamp::from_second(1_000_000).unwrap();
    let five_hours = 5 * 60;
    let duration_mins = 7 * 24 * 60;
    let capacity = ProviderCapacity::from_windows(vec![
        window(now, Some(10), 2 * 3_600, Some(five_hours)),
        window(now, Some(50), 2 * 86_400, Some(duration_mins)),
    ]);
    let reading = capacity.longest_window_surplus(now).unwrap();
    assert_eq!(reading.duration_mins, duration_mins);
    assert_eq!(reading.elapsed, SignedDuration::from_secs(5 * 86_400));
    assert!((reading.headroom - 1.75).abs() < f64::EPSILON);
}

#[test]
fn temporal_policy_fails_closed_for_incomplete_or_durationless_readings() {
    let now = Timestamp::from_second(1_000_000).unwrap();
    let duration_mins = 7 * 24 * 60;
    let readings = [
        window(
            now,
            Some(1),
            i64::from(duration_mins) * 60,
            Some(duration_mins),
        ),
        window(now, Some(60), -60, Some(duration_mins)),
        window(now, None, 2 * 86_400, Some(duration_mins)),
        RateLimitWindow {
            resets_at: None,
            ..window(now, Some(50), 2 * 86_400, Some(duration_mins))
        },
        window(now, Some(50), 2 * 86_400, None),
    ];
    for reading in readings {
        let capacity = ProviderCapacity::from_windows(vec![reading]);
        assert_eq!(capacity.longest_window_surplus(now), None);
    }

    let mut named = window(now, Some(100), 86_400, Some(60));
    named.scope = Some(crate::agents::RateLimitWindowScope {
        id: "build_minutes".to_owned(),
        label: "bld".to_owned(),
    });
    let capacity = ProviderCapacity::from_windows(vec![named]);
    assert_eq!(capacity.longest_window_surplus(now), None);
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
        read_rate_limits_cache(&runtime.shared_rate_limits_path())
            .entries
            .is_empty()
    );
    std::fs::write(runtime.shared_rate_limits_path(), b"not-json").unwrap();
    assert!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path())
            .entries
            .is_empty()
    );
    let cache = RateLimitsCache {
        version: RateLimitsCache::default().version + 1,
        entries: BTreeMap::from([(login_key("claude"), Default::default())]),
        ..Default::default()
    };
    write_cache(&runtime, &cache);
    assert!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path())
            .entries
            .is_empty()
    );
    write_cache(
        &runtime,
        &RateLimitsCache {
            version: 5,
            entries: BTreeMap::from([(login_key("qwen"), Default::default())]),
            ..Default::default()
        },
    );
    assert!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path())
            .entries
            .is_empty(),
        "v5 caches are kind-keyed, so their schema must cold-drop"
    );
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
