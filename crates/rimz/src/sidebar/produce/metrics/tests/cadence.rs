use super::*;

// ── Metrics sampling cadence ───────────────────────────────────────────────────

#[test]
fn metric_entry_due_samples_missing_changed_and_legacy_entries_immediately() {
    let command = Some("zsh".to_owned());
    let entry = fresh_entry(42, 700, "zsh", 1_000);

    assert!(metric_entry_due(None, &command, 1_000, false));
    assert!(metric_entry_due(
        Some(&entry),
        &Some("cargo build".to_owned()),
        1_001,
        false,
    ));
    assert!(
        !metric_entry_due(Some(&entry), &command, 500, false),
        "a clock that ran backwards reads fresh rather than busy-looping"
    );

    let mut entry = fresh_entry(42, 700, "cargo build", 1_000);
    entry.sample_version = 1;
    assert!(
        metric_entry_due(Some(&entry), &Some("cargo build".to_owned()), 1_001, false),
        "single-process cache entries cannot seed pane-tree rates"
    );
}

#[test]
fn metric_entry_due_uses_viewed_or_background_ttl() {
    let idle = (Some("zsh".to_owned()), fresh_entry(42, 700, "zsh", 1_000));
    let active = (
        Some("cargo build".to_owned()),
        fresh_entry(42, 700, "cargo build", 1_000),
    );
    let child = (Some("htop".to_owned()), fresh_entry(42, 700, "htop", 1_000));

    let focused_ttl = METRICS_FOCUSED_SAMPLE_TTL.as_millis() as u64;
    let background_ttl = METRICS_BACKGROUND_SAMPLE_TTL.as_millis() as u64;
    for (command, entry) in [idle, active, child] {
        assert!(!metric_entry_due(
            Some(&entry),
            &command,
            1_000 + focused_ttl,
            true,
        ));
        assert!(metric_entry_due(
            Some(&entry),
            &command,
            1_001 + focused_ttl,
            true,
        ));
        assert!(!metric_entry_due(
            Some(&entry),
            &command,
            1_000 + background_ttl,
            false,
        ));
        assert!(metric_entry_due(
            Some(&entry),
            &command,
            1_001 + background_ttl,
            false,
        ));
    }
}

#[test]
fn hibernated_metrics_follow_the_shared_check_not_sample_age() {
    let mut pidded = pane("terminal_1", Some("claude"), Some("/repo"));
    pidded.pane_pid = Some(42);
    let held = HashSet::from([pidded.pane_id.clone()]);
    let mut frame = frame_from_panes(vec![pidded]);
    let mut cache = MetricsSampleCache::default();
    cache.entries.insert(
        state(&frame, "terminal_1").pane_id.to_string(),
        fresh_entry(42, 700, "claude", unix_now_ms().saturating_add(60_000)),
    );
    for viewed in [false, true] {
        frame.viewed_panes = if viewed {
            held.iter().cloned().collect()
        } else {
            Vec::new()
        };
        assert!(
            pane_metrics_due(&frame, &cache, &held, &HashSet::new()),
            "a due held check samples even when its wall-clock entry is fresh"
        );
        assert!(!pane_metrics_due(&frame, &cache, &held, &held));
        assert!(!pane_metrics_due(
            &frame,
            &cache,
            &HashSet::new(),
            &HashSet::new()
        ));
        assert!(!enrich_pane_metrics(
            &mut frame, SESSION, &mut cache, &held, &held
        ));
        assert!(enrich_pane_metrics(
            &mut frame,
            SESSION,
            &mut cache,
            &held,
            &HashSet::new()
        ));
    }
}

#[test]
fn cached_entry_publishes_resource_stats_only_when_complete() {
    let mut frame = frame_from_panes(vec![pane("terminal_1", Some("zsh"), Some("/repo"))]);
    let mut partial = fresh_entry(42, 700, "zsh", 1_000);
    partial.rss_kb = Some(2_048);
    partial.cpu_pct = Some(3);

    apply_cached_entry(frame.pane_states_mut().next().unwrap(), &partial);
    assert_eq!(state(&frame, "terminal_1").metrics.rss_kb, None);
    assert_eq!(state(&frame, "terminal_1").metrics.cpu_pct, None);
    assert_eq!(state(&frame, "terminal_1").metrics.io_bps, None);

    let mut complete = partial;
    complete.io_bps = Some(512);
    apply_cached_entry(frame.pane_states_mut().next().unwrap(), &complete);
    assert_eq!(state(&frame, "terminal_1").metrics.rss_kb, Some(2_048));
    assert_eq!(state(&frame, "terminal_1").metrics.cpu_pct, Some(3));
    assert_eq!(state(&frame, "terminal_1").metrics.io_bps, Some(512));
}

#[test]
fn unbound_idle_panes_back_off_on_the_idle_ttl() {
    let mut cache = MetricsSampleCache::default();
    let mut frame = frame_from_panes(vec![pane("terminal_1", Some("zsh"), Some("/repo"))]);
    assert!(enrich_pane_metrics(
        &mut frame,
        "rimz-test-no-such-session-for-metrics",
        &mut cache,
        &HashSet::new(),
        &HashSet::new(),
    ));
    assert!(
        !pane_metrics_due(&frame, &cache, &HashSet::new(), &HashSet::new()),
        "an unbound idle pane records a sample attempt instead of retrying the \
         table walk every produce"
    );

    let entry = cache
        .entries
        .get(&state(&frame, "terminal_1").pane_id.to_string())
        .unwrap();
    assert_eq!(entry.stats_pid, 0);
    assert_eq!(entry.pane_pid, None);
}

/// The within-TTL skip path: stored display values and the root-pid binding the
/// process-row name anchors on carry forward onto the matching pane. A
/// natively-pidded pane keeps its live pid.
#[test]
fn metrics_within_ttl_carries_matching_display_values_and_binding() {
    let mut panes = vec![
        pane("terminal_1", Some("zsh"), Some("/repo")),
        pane("terminal_4", Some("zsh"), Some("/repo")),
    ];
    panes[1].pane_pid = Some(7);
    let now_ms = unix_now_ms().saturating_add(METRICS_BACKGROUND_SAMPLE_TTL.as_millis() as u64);
    let mut cache = MetricsSampleCache {
        sampled_at_ms: now_ms,
        entries: HashMap::new(),
    };
    cache.entries.insert(
        panes[0].pane_id.to_string(),
        MetricsSampleEntry {
            cpu_pct: Some(42),
            io_bps: Some(1_024),
            rss_kb: Some(2_048),
            ..fresh_entry(42, 700, "zsh", now_ms)
        },
    );
    cache.entries.insert(
        panes[1].pane_id.to_string(),
        fresh_entry(44, 702, "zsh", now_ms),
    );
    let mut frame = frame_from_panes(panes);

    assert!(!enrich_pane_metrics(
        &mut frame,
        "rimz-query-engine",
        &mut cache,
        &HashSet::new(),
        &HashSet::new(),
    ));

    assert_eq!(
        state(&frame, "terminal_1").metrics.cpu_pct,
        Some(42),
        "matching pane carries forward"
    );
    assert_eq!(state(&frame, "terminal_1").metrics.io_bps, Some(1_024));
    assert_eq!(state(&frame, "terminal_1").metrics.rss_kb, Some(2_048));
    assert_eq!(
        state(&frame, "terminal_1").current.pid,
        Some(42),
        "the root-pid binding rides with the values — the process-row name \
         anchor must not flip between windows"
    );
    assert_eq!(
        state(&frame, "terminal_4").current.pid,
        Some(7),
        "a natively-reported pid is never overwritten by the cached binding"
    );
}

/// A changed foreground on the same root pid goes straight to warmup because
/// the prior values belong to the old tenant; an uncached pane also warms up.
#[test]
fn metrics_within_ttl_warms_changed_or_uncached_panes() {
    let mut panes = vec![
        pane("terminal_2", Some("cargo build"), Some("/repo")),
        pane("terminal_3", Some("zsh"), Some("/repo")),
    ];
    panes[0].pane_pid = Some(43);
    let now_ms = unix_now_ms();
    let mut cache = MetricsSampleCache {
        sampled_at_ms: now_ms,
        entries: HashMap::new(),
    };
    cache.entries.insert(
        panes[0].pane_id.to_string(),
        MetricsSampleEntry {
            cpu_pct: Some(99),
            io_bps: Some(9_999),
            rss_kb: Some(9_999),
            ..fresh_entry(43, 701, "zsh", now_ms)
        },
    );
    let mut frame = frame_from_panes(panes);

    assert!(enrich_pane_metrics(
        &mut frame,
        "rimz-query-engine",
        &mut cache,
        &HashSet::new(),
        &HashSet::new(),
    ));

    assert_eq!(
        state(&frame, "terminal_2").metrics.cpu_pct,
        None,
        "a changed foreground on the same root pid publishes no partial stats"
    );
    assert_eq!(
        state(&frame, "terminal_3").metrics.cpu_pct,
        None,
        "uncached pane warms up next window"
    );
    let rewritten = cache;
    assert_eq!(
        rewritten
            .entries
            .get(&state(&frame, "terminal_2").pane_id.to_string())
            .and_then(|entry| entry.command.as_deref()),
        Some("cargo build"),
        "changed commands get a fresh raw-counter entry immediately"
    );
}

/// A due cache (stamp older than the TTL) re-samples and re-stamps, so the
/// next produce inside the new window skips again.
#[test]
fn metrics_due_path_resamples_and_restamps() {
    let stale_ms = unix_now_ms() - METRICS_BACKGROUND_SAMPLE_TTL.as_millis() as u64 - 1_000;
    let mut cache = MetricsSampleCache {
        sampled_at_ms: stale_ms,
        entries: HashMap::new(),
    };

    let mut pidded = pane("terminal_1", Some("zsh"), Some("/repo"));
    pidded.pane_pid = Some(std::process::id());
    let mut frame = frame_from_panes(vec![pidded]);
    assert!(enrich_pane_metrics(
        &mut frame,
        "rimz-query-engine",
        &mut cache,
        &HashSet::new(),
        &HashSet::new(),
    ));

    let rewritten = cache;
    assert!(
        rewritten.sampled_at_ms > stale_ms,
        "a due produce re-samples and re-stamps the cache"
    );
}
