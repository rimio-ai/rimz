use super::*;

#[test]
fn snapshot_cache_serves_only_fresh_same_session_readable_entries() {
    let dir = tempfile::tempdir().unwrap();
    let stale = unix_now_ms().saturating_sub(SNAPSHOT_CACHE_TTL.as_millis() as u64 + 1);

    for (name, publish, requested, expected_hit) in [
        (
            "fresh same-session",
            Some(("rimz-query-engine", unix_now_ms())),
            "rimz-query-engine",
            true,
        ),
        // One session's panes must never be served to a sidebar pinned to
        // another — the Zellij backend stamps PaneRef.session_name from the
        // requested session, so a cross-session hit would mislabel panes.
        (
            "different session",
            Some(("rimz-query-engine", unix_now_ms())),
            "rimz-other",
            false,
        ),
        (
            "stale entry",
            Some(("rimz-query-engine", stale)),
            "rimz-query-engine",
            false,
        ),
        ("absent cache", None, "rimz-query-engine", false),
    ] {
        let path = dir.path().join(format!("{name}.json"));
        if let Some((session, produced_at_ms)) = publish {
            write_snapshot_cache(&path, session, produced_at_ms);
        }
        assert_eq!(
            fresh_snapshot_cache(&path, requested, None, SNAPSHOT_CACHE_TTL).is_some(),
            expected_hit,
            "{name}"
        );
    }

    let unreadable = dir.path().join("unreadable.json");
    std::fs::write(&unreadable, b"{ not json").unwrap();
    assert!(
        fresh_snapshot_cache(&unreadable, "rimz-query-engine", None, SNAPSHOT_CACHE_TTL).is_none()
    );
}

#[test]
fn snapshot_cache_misses_before_requested_pane_freshness_floor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    let produced_at_ms = unix_now_ms();
    write_snapshot_cache(&path, "rimz-query-engine", produced_at_ms);

    assert!(
        fresh_snapshot_cache(
            &path,
            "rimz-query-engine",
            Some(produced_at_ms),
            SNAPSHOT_CACHE_TTL
        )
        .is_some(),
        "a cache produced at the requested floor is usable"
    );
    assert!(
        fresh_snapshot_cache(
            &path,
            "rimz-query-engine",
            Some(produced_at_ms.saturating_add(1)),
            SNAPSHOT_CACHE_TTL,
        )
        .is_none(),
        "a pane-sensitive wakeup rejects the pre-signal pane cache"
    );
}

#[test]
fn read_only_consumer_serves_a_stale_same_session_base() {
    // A `--no-produce` renderer holds the producer's last published base even
    // past the freshness TTL — it renders the last good frame rather than
    // forking its own `list-panes`. The fresh-only read (the producer's fast
    // path) misses the stale entry; the TTL-agnostic read still serves it.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snapshot.json");
    let stale = unix_now_ms().saturating_sub(SNAPSHOT_CACHE_TTL.as_millis() as u64 + 1);
    write_snapshot_cache(&path, "rimz-query-engine", stale);
    assert!(
        fresh_snapshot_cache(&path, "rimz-query-engine", None, SNAPSHOT_CACHE_TTL).is_none(),
        "the producer's fresh-only fast path skips a stale entry"
    );
    assert!(
        read_snapshot_cache(&path, "rimz-query-engine").is_some(),
        "the consumer's read serves the stale entry as last-good"
    );
}

#[test]
fn metrics_only_refresh_preserves_the_pane_frame_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/metrics-frame")),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();

    let mut pidded = pane("terminal_1", Some("zsh"), Some("/repo"));
    pidded.pane_pid = Some(std::process::id());
    let produced_at_ms = unix_now_ms();
    let frame = crate::sidebar::frame::assemble_frame(vec![pidded], produced_at_ms, "s");
    let cache_path = runtime.root.join("snapshot.json");
    atomic::write_temp_then_rename_cache(&cache_path, &frame).unwrap();

    let refreshed = refresh_cached_metrics(
        frame,
        &runtime,
        &cache_path,
        &runtime.root.join("snapshot.lock"),
        "s",
        None,
        SNAPSHOT_CACHE_TTL,
    );

    assert_eq!(refreshed.produced_at_ms, produced_at_ms);
    let published = read_snapshot_cache(&cache_path, "s").unwrap();
    assert_eq!(
        published.produced_at_ms, produced_at_ms,
        "a metrics-only publish must not masquerade as a fresh pane listing"
    );
    assert!(
        runtime.root.join("metrics-sample.json").exists(),
        "metrics refresh samples /proc and writes its own cache"
    );
}
