use super::*;

// ── Metrics sampling cadence ───────────────────────────────────────────────────

fn metrics_runtime(name: &str) -> (tempfile::TempDir, crate::RuntimePaths) {
    let dir = tempfile::TempDir::new().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new(name)),
        dir.path(),
    )
    .unwrap();
    std::fs::create_dir_all(&runtime.root).unwrap();
    (dir, runtime)
}

#[test]
fn metric_entry_due_samples_missing_and_changed_entries_immediately() {
    let command = Some("zsh".to_owned());
    let entry = fresh_entry(42, 700, "zsh", 1_000);

    assert!(metric_entry_due(None, &command, 1_000));
    assert!(metric_entry_due(
        Some(&entry),
        &Some("cargo build".to_owned()),
        1_001
    ));
    assert!(
        !metric_entry_due(Some(&entry), &command, 500),
        "a clock that ran backwards reads fresh rather than busy-looping"
    );
}

#[test]
fn metric_entry_due_warms_legacy_sample_versions() {
    let command = Some("cargo build".to_owned());
    let mut entry = fresh_entry(42, 700, "cargo build", 1_000);
    entry.sample_version = 1;

    assert!(
        metric_entry_due(Some(&entry), &command, 1_001),
        "single-process cache entries cannot seed pane-tree rates"
    );
}

#[test]
fn metric_entry_due_uses_idle_or_hot_ttl() {
    let idle = (
        Some("zsh".to_owned()),
        fresh_entry(42, 700, "zsh", 1_000),
        METRICS_SAMPLE_TTL.as_millis() as u64,
    );
    let active = (
        Some("cargo build".to_owned()),
        fresh_entry(42, 700, "cargo build", 1_000),
        METRICS_HOT_SAMPLE_TTL.as_millis() as u64,
    );
    let mut child_entry = fresh_entry(42, 700, "htop", 1_000);
    child_entry.tree_process_count = 2;
    assert!(metrics_entry_hot(&child_entry));
    let child = (
        Some("htop".to_owned()),
        child_entry,
        METRICS_HOT_SAMPLE_TTL.as_millis() as u64,
    );

    for (command, entry, ttl_ms) in [idle, active, child] {
        assert!(!metric_entry_due(Some(&entry), &command, 1_000 + ttl_ms));
        assert!(metric_entry_due(Some(&entry), &command, 1_001 + ttl_ms));
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
    let (_dir, runtime) = metrics_runtime("/tmp/metrics-unbound");

    let mut frame = frame_from_panes(vec![pane("terminal_1", Some("zsh"), Some("/repo"))]);
    assert!(enrich_pane_metrics(
        &mut frame,
        "rimz-test-no-such-session-for-metrics",
        &runtime
    ));
    assert!(
        !pane_metrics_due(&frame, &runtime),
        "an unbound idle pane records a sample attempt instead of retrying the \
         table walk every produce"
    );

    let cache_path = runtime.root.join("metrics-sample.json");
    let cache: MetricsSampleCache =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
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
    let (_dir, runtime) = metrics_runtime("/tmp/metrics-carry");
    let mut panes = vec![
        pane("terminal_1", Some("zsh"), Some("/repo")),
        pane("terminal_4", Some("zsh"), Some("/repo")),
    ];
    panes[1].pane_pid = Some(7);
    let now_ms = unix_now_ms();
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
    let cache_path = runtime.root.join("metrics-sample.json");
    std::fs::write(&cache_path, serde_json::to_vec(&cache).unwrap()).unwrap();
    let mut frame = frame_from_panes(panes);

    assert!(!enrich_pane_metrics(
        &mut frame,
        "rimz-query-engine",
        &runtime
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
    let (_dir, runtime) = metrics_runtime("/tmp/metrics-warmup");
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
    let cache_path = runtime.root.join("metrics-sample.json");
    std::fs::write(&cache_path, serde_json::to_vec(&cache).unwrap()).unwrap();
    let mut frame = frame_from_panes(panes);

    assert!(enrich_pane_metrics(
        &mut frame,
        "rimz-query-engine",
        &runtime
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
    let rewritten: MetricsSampleCache =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
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
    let (_dir, runtime) = metrics_runtime("/tmp/metrics-due");

    let stale_ms = unix_now_ms() - METRICS_SAMPLE_TTL.as_millis() as u64 - 1_000;
    let cache = MetricsSampleCache {
        sampled_at_ms: stale_ms,
        entries: HashMap::new(),
    };
    let cache_path = runtime.root.join("metrics-sample.json");
    std::fs::write(&cache_path, serde_json::to_vec(&cache).unwrap()).unwrap();

    let mut pidded = pane("terminal_1", Some("zsh"), Some("/repo"));
    pidded.pane_pid = Some(std::process::id());
    let mut frame = frame_from_panes(vec![pidded]);
    assert!(enrich_pane_metrics(
        &mut frame,
        "rimz-query-engine",
        &runtime
    ));

    let rewritten: MetricsSampleCache =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    assert!(
        rewritten.sampled_at_ms > stale_ms,
        "a due produce re-samples and re-stamps the cache"
    );
}
