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
fn sub_provider_windows_are_display_only_for_capacity_policy() {
    let now = Timestamp::from_second(2_000_000_000).unwrap();
    let reset = now + SignedDuration::from_secs(3_600);
    let mut cache = RateLimitsCache {
        entries: BTreeMap::from([(
            "qwen".to_owned(),
            RateLimitCacheEntry {
                scope: ProviderAccountScope::sub_provider("alibaba", "international"),
                limits: AgentRateLimits {
                    windows: vec![window(now, Some(100), 3_600, Some(300))],
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

    cache.entries.get_mut("qwen").unwrap().scope = ProviderAccountScope::KindWide;
    write_cache(&runtime, &cache);
    let capacity = ProviderCapacity::read(&runtime, "qwen").unwrap();
    assert_eq!(capacity.longest_window_reset_at(), Some(reset));
    assert!(ProviderCapacity::read_all(&runtime).contains_key("qwen"));
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
    assert_eq!(projected.longest_window_reset_at(), Some(past));

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
    assert_eq!(undated.longest_window_reset_at(), None);
    assert_eq!(projected.shortest_window_running(now), Some(true));

    let mut named = window(now, Some(100), 86_400, Some(60));
    named.scope = Some(crate::agents::RateLimitWindowScope {
        id: "build_minutes".to_owned(),
        label: "bld".to_owned(),
    });
    let capacity = ProviderCapacity::from_windows(vec![named]);
    assert_eq!(capacity.shortest_window_running(now), None);
    assert_eq!(capacity.longest_window_running(now), None);
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
        entries: BTreeMap::from([("claude".to_owned(), Default::default())]),
        ..Default::default()
    };
    write_cache(&runtime, &cache);
    assert!(
        read_rate_limits_cache(&runtime.shared_rate_limits_path())
            .entries
            .is_empty()
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
