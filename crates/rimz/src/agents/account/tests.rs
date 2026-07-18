use super::*;

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
    crate::store::atomic::write_temp_then_rename_cache(&runtime.shared_rate_limits_path(), cache)
        .unwrap();
}

#[test]
fn sub_provider_windows_require_an_exact_binding_for_launch_controls() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let pacing_reset = now + SignedDuration::from_secs(2 * 86_400);
    let scope = ProviderAccountScope::sub_provider("alibaba", "international");
    let binding = ProviderAccountBinding::new(scope.clone(), "owner".to_owned()).unwrap();
    let other = ProviderAccountBinding::new(scope.clone(), "other".to_owned()).unwrap();
    let mut cache = RateLimitsCache {
        entries: BTreeMap::from([(
            "qwen".to_owned(),
            RateLimitCacheEntry {
                scope,
                account_key: Some("owner".to_owned()),
                limits: AgentRateLimits {
                    windows: vec![
                        window(now, Some(20), 3_600, Some(300)),
                        window(now, Some(50), 2 * 86_400, Some(7 * 24 * 60)),
                        window(now, Some(100), 20 * 86_400, Some(30 * 24 * 60)),
                    ],
                },
                pending: Vec::new(),
            },
        )]),
        ..Default::default()
    };
    let (_dir, runtime) = runtime();
    write_cache(&runtime, &cache);
    assert!(ProviderCapacity::read(&runtime, "qwen").is_none());
    assert!(ProviderCapacity::read_all(&runtime).is_empty());
    let capacity = ProviderCapacity::read_bound(&runtime, "qwen", &binding).unwrap();
    assert_eq!(capacity.shortest_window_running(now), Some(true));
    assert_eq!(capacity.longest_window_running(now), Some(true));
    assert_eq!(
        capacity.longest_window_signal(now),
        LongestWindowSignal::At(pacing_reset)
    );
    assert!(capacity.longest_window_surplus(now).is_some());
    assert!(capacity.spent_window(now).is_some());
    assert!(ProviderCapacity::read_bound(&runtime, "qwen", &other).is_none());
    assert_eq!(
        ProviderCapacity::binding_cache_matches(&runtime, "qwen", &binding),
        Some(true)
    );
    assert_eq!(
        ProviderCapacity::binding_cache_matches(&runtime, "qwen", &other),
        Some(false)
    );
    let reason = provider_budget_gate(&runtime, "qwen", &binding, now).unwrap();
    assert!(reason.contains("Qwen Alibaba International 30d window exhausted"));
    assert!(!reason.contains("owner"));

    cache.entries.get_mut("qwen").unwrap().scope = ProviderAccountScope::KindWide;
    cache.entries.get_mut("qwen").unwrap().account_key = None;
    write_cache(&runtime, &cache);
    let capacity = ProviderCapacity::read(&runtime, "qwen").unwrap();
    assert_eq!(
        capacity.longest_window_signal(now),
        LongestWindowSignal::At(now + SignedDuration::from_secs(20 * 86_400))
    );
    assert!(ProviderCapacity::read_all(&runtime).contains_key("qwen"));
}

#[test]
fn managed_launch_state_selects_only_applicable_capacity() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let kind_wide = |kind: &str| {
        (
            kind.to_owned(),
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
            .capacity(&runtime, "claude")
            .is_some()
    );
    assert!(
        ManagedLaunchState::Unresolved
            .capacity(&runtime, "qwen")
            .is_none()
    );

    let scope = ProviderAccountScope::sub_provider("alibaba", "international");
    cache.entries.insert(
        "qwen".to_owned(),
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
            .capacity(&runtime, "qwen")
            .is_none()
    );
}

#[test]
fn capacity_selects_temporal_windows_and_measures_surplus() {
    let now = Timestamp::from_second(1_000_000).unwrap();
    let five_hours = 5 * 60;
    let duration_mins = 7 * 24 * 60;
    let full_five_hours = i64::from(five_hours) * 60;
    let selected = ProviderCapacity::from_windows(vec![
        window(now, Some(1), full_five_hours, Some(five_hours)),
        window(now, Some(40), 3_600, Some(duration_mins)),
    ]);
    assert_eq!(selected.shortest_window_running(now), Some(false));
    assert_eq!(selected.longest_window_running(now), Some(true));

    let longest_not_started = ProviderCapacity::from_windows(vec![
        window(now, Some(40), 3_600, Some(five_hours)),
        window(
            now,
            Some(1),
            i64::from(duration_mins) * 60,
            Some(duration_mins),
        ),
    ]);
    assert_eq!(longest_not_started.shortest_window_running(now), Some(true));
    assert_eq!(longest_not_started.longest_window_running(now), Some(false));

    let capacity = ProviderCapacity::from_windows(vec![
        window(now, Some(10), 2 * 3_600, Some(five_hours)),
        window(now, Some(50), 2 * 86_400, Some(duration_mins)),
    ]);
    assert_eq!(capacity.shortest_window_running(now), Some(true));
    assert_eq!(capacity.longest_window_running(now), Some(true));
    let reading = capacity.longest_window_surplus(now).unwrap();
    assert_eq!(reading.duration_mins, duration_mins);
    assert_eq!(reading.elapsed, SignedDuration::from_secs(5 * 86_400));
    assert!((reading.headroom - 1.75).abs() < f64::EPSILON);
}

#[test]
fn temporal_policy_fails_closed_for_incomplete_or_durationless_readings() {
    let now = Timestamp::from_second(1_000_000).unwrap();
    let five_hours = 5 * 60;
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

    let past = now - SignedDuration::from_secs(60);
    let projected = ProviderCapacity::from_windows(vec![
        window(now, Some(40), 3_600, Some(five_hours)),
        window(now, Some(90), -60, Some(duration_mins)),
    ]);
    assert_eq!(projected.longest_window_running(now), Some(false));
    assert_eq!(
        projected.longest_window_signal(now),
        LongestWindowSignal::At(past)
    );

    let unknown =
        ProviderCapacity::from_windows(vec![window(now, None, 3_600, Some(duration_mins))]);
    assert_eq!(unknown.shortest_window_running(now), None);
    assert_eq!(unknown.longest_window_running(now), None);

    let undated = ProviderCapacity::from_windows(vec![
        window(now, Some(40), 3_600, Some(five_hours)),
        RateLimitWindow {
            resets_at: None,
            ..window(now, Some(80), 3_600, Some(duration_mins))
        },
    ]);
    assert_eq!(
        undated.longest_window_signal(now),
        LongestWindowSignal::Unknown
    );
    assert_eq!(projected.shortest_window_running(now), Some(true));

    let mut named = window(now, Some(100), 86_400, Some(60));
    named.scope = Some(crate::agents::RateLimitWindowScope {
        id: "build_minutes".to_owned(),
        label: "bld".to_owned(),
    });
    let capacity = ProviderCapacity::from_windows(vec![named]);
    assert_eq!(capacity.shortest_window_running(now), None);
    assert_eq!(capacity.longest_window_running(now), None);
    assert_eq!(
        capacity.longest_window_signal(now),
        LongestWindowSignal::Unknown
    );
    assert_eq!(capacity.longest_window_surplus(now), None);
    assert_eq!(capacity.latest_spent_window_reset(now), None);
    assert!(!capacity.subscription_budget_available(now));

    let durationless = ProviderCapacity::from_windows(vec![window(now, Some(100), 86_400, None)]);
    assert_eq!(
        durationless.longest_window_signal(now),
        LongestWindowSignal::Unknown
    );
    assert_eq!(durationless.latest_spent_window_reset(now), None);
    assert!(!durationless.subscription_budget_available(now));
}

#[test]
fn longest_window_signal_distinguishes_reset_down_and_unknown_truth() {
    let now = Timestamp::from_second(1_000_000).unwrap();
    let duration_mins = 7 * 24 * 60;
    let full_window_secs = i64::from(duration_mins) * 60;
    let capacity = |window| ProviderCapacity::from_windows(vec![window]);

    assert_eq!(
        ProviderCapacity::default().longest_window_signal(now),
        LongestWindowSignal::Unknown
    );

    let lifted = RateLimitWindow {
        lifted: true,
        source: crate::agents::context::WindowSource::Authoritative,
        ..window(now, Some(20), 3_600, Some(duration_mins))
    };
    assert_eq!(
        capacity(lifted).longest_window_signal(now),
        LongestWindowSignal::Unknown
    );

    let authoritative_not_started = RateLimitWindow {
        source: crate::agents::context::WindowSource::Authoritative,
        ..window(now, Some(1), full_window_secs, Some(duration_mins))
    };
    assert_eq!(
        capacity(authoritative_not_started).longest_window_signal(now),
        LongestWindowSignal::ConfirmedDown
    );
    assert_eq!(
        capacity(window(now, Some(1), full_window_secs, Some(duration_mins)))
            .longest_window_signal(now),
        LongestWindowSignal::Unknown
    );

    for resets_in_secs in [3_600, -60] {
        let dated = window(now, Some(20), resets_in_secs, Some(duration_mins));
        assert_eq!(
            capacity(dated).longest_window_signal(now),
            LongestWindowSignal::At(now + SignedDuration::from_secs(resets_in_secs))
        );
    }

    for used_percentage in [0, 1] {
        let authoritative_undated = RateLimitWindow {
            resets_at: None,
            source: crate::agents::context::WindowSource::Authoritative,
            ..window(now, Some(used_percentage), 3_600, Some(duration_mins))
        };
        assert_eq!(
            capacity(authoritative_undated).longest_window_signal(now),
            LongestWindowSignal::ConfirmedDown
        );
    }

    for unknown in [
        RateLimitWindow {
            resets_at: None,
            ..window(now, Some(20), 3_600, Some(duration_mins))
        },
        RateLimitWindow {
            resets_at: None,
            used_percentage: None,
            source: crate::agents::context::WindowSource::Authoritative,
            ..window(now, Some(20), 3_600, Some(duration_mins))
        },
        RateLimitWindow {
            resets_at: None,
            source: crate::agents::context::WindowSource::Authoritative,
            ..window(now, Some(45), 3_600, Some(duration_mins))
        },
        window(now, Some(20), 3_600, None),
    ] {
        assert_eq!(
            capacity(unknown).longest_window_signal(now),
            LongestWindowSignal::Unknown
        );
    }
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
        entries: BTreeMap::from([("claude".to_owned(), Default::default())]),
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
            version: 3,
            entries: BTreeMap::from([("qwen".to_owned(), Default::default())]),
            ..Default::default()
        },
    );
    assert!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path())
            .entries
            .is_empty(),
        "the pre-binding v3 schema must cold-drop"
    );
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
