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
fn fresh_publishable_snapshot_cache_rejects_invalid_cached_frames() {
    let dir = tempfile::tempdir().unwrap();
    let own = crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_1");

    let empty_path = dir.path().join("empty.json");
    let empty = crate::sidebar::frame::assemble_frame(Vec::new(), unix_now_ms(), "s");
    atomic::write_temp_then_rename_cache(&empty_path, &empty).unwrap();
    assert!(
        fresh_publishable_snapshot_cache(&empty_path, "s", None, SNAPSHOT_CACHE_TTL, None, None)
            .is_none(),
        "a fresh empty cache is still implausible"
    );

    let missing_own_path = dir.path().join("missing-own.json");
    let missing_own = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_2", Some("zsh"), Some("/repo"))],
        unix_now_ms(),
        "s",
    );
    atomic::write_temp_then_rename_cache(&missing_own_path, &missing_own).unwrap();
    assert!(
        fresh_publishable_snapshot_cache(
            &missing_own_path,
            "s",
            None,
            SNAPSHOT_CACHE_TTL,
            Some(&own),
            None,
        )
        .is_none(),
        "a cache missing the renderer's own pane must not short-circuit produce"
    );

    let valid_path = dir.path().join("valid.json");
    let valid = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", Some("zsh"), Some("/repo"))],
        unix_now_ms(),
        "s",
    );
    atomic::write_temp_then_rename_cache(&valid_path, &valid).unwrap();
    assert!(
        fresh_publishable_snapshot_cache(
            &valid_path,
            "s",
            None,
            SNAPSHOT_CACHE_TTL,
            Some(&own),
            None,
        )
        .is_some()
    );
}

#[test]
fn rejected_frame_holds_only_a_publishable_prior() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = crate::ids::WorkspaceId::from_project_root(std::path::Path::new(
        "/tmp/rejected-frame-prior",
    ));
    let runtime = crate::RuntimePaths::under(workspace_id, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let cache_path = runtime.root.join("snapshot.json");

    let valid_prior = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", Some("zsh"), Some("/repo"))],
        unix_now_ms(),
        "s",
    );
    let empty_fresh = crate::sidebar::frame::assemble_frame(Vec::new(), unix_now_ms(), "s");
    let held = validate_frame_for_publish(
        empty_fresh,
        Some(valid_prior),
        None,
        None,
        false,
        &runtime,
        &cache_path,
    )
    .expect("valid prior is held over rejected fresh frame");
    assert_eq!(pane_count(&held), 1);

    let invalid_prior = crate::sidebar::frame::assemble_frame(Vec::new(), unix_now_ms(), "s");
    let empty_fresh = crate::sidebar::frame::assemble_frame(Vec::new(), unix_now_ms(), "s");
    let err = validate_frame_for_publish(
        empty_fresh,
        Some(invalid_prior),
        None,
        None,
        false,
        &runtime,
        &cache_path,
    )
    .expect_err("invalid prior must not be returned as last good");
    assert!(
        matches!(
            err,
            crate::sidebar::produce::ProduceErr::FrameRejected(
                crate::schema::diag::FrameRejectReason::Empty
            )
        ),
        "unexpected error: {err:?}"
    );
}

#[test]
fn missing_own_rejection_captures_prior_and_offending_frames() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = crate::ids::WorkspaceId::from_project_root(std::path::Path::new(
        "/tmp/missing-own-frame-rejected",
    ));
    let runtime = crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let cache_path = runtime.root.join("snapshot.json");
    let sink = crate::diag::DiagSink::under(dir.path().join("state"), workspace_id, "s", None);
    let own = crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_1");
    let now = unix_now_ms();
    let prior = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", Some("zsh"), Some("/repo"))],
        now,
        "s",
    );
    let fresh = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_2", Some("zsh"), Some("/repo"))],
        now.saturating_add(1),
        "s",
    );

    let held = validate_frame_for_publish(
        fresh,
        Some(prior),
        Some(&own),
        Some(&sink),
        false,
        &runtime,
        &cache_path,
    )
    .expect("missing-own rejection holds the prior frame before escape");

    assert_eq!(pane_count(&held), 1);
    let event = diagnostic_events(&sink).pop().expect("diagnostic event");
    let crate::schema::diag::DiagEvent::FrameRejected {
        reason: crate::schema::diag::FrameRejectReason::MissingOwnPane,
        frames_ref: Some(frames_ref),
        ..
    } = event
    else {
        panic!("expected missing-own frame rejection with frame capture");
    };
    assert!(
        sink.frame_capture_path(&frames_ref).exists(),
        "the frame reference points at a captured prior/offending pair"
    );
}

#[test]
fn verified_shrink_repull_result_is_published() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id =
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/verified-shrink"));
    let runtime = crate::RuntimePaths::under(workspace_id, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let cache_path = runtime.root.join("snapshot.json");
    let prior = frame(vec![
        pane("terminal_1", Some("zsh"), Some("/repo")),
        pane("terminal_2", Some("zsh"), Some("/repo")),
        pane("terminal_3", Some("zsh"), Some("/repo")),
    ]);
    let raced = frame(vec![pane("terminal_1", Some("zsh"), Some("/repo"))]);
    let verified = frame(vec![
        pane("terminal_1", Some("zsh"), Some("/repo")),
        pane("terminal_2", Some("zsh"), Some("/repo")),
    ]);
    let calls = std::cell::Cell::new(0);

    let repulled = verify_shrink(
        raced,
        Some(&prior),
        &|enrich_metrics| {
            assert!(enrich_metrics);
            calls.set(calls.get() + 1);
            Ok(verified.clone())
        },
        None,
        true,
    )
    .expect("re-pull succeeds");

    assert_eq!(calls.get(), 1);
    let published = validate_frame_for_publish(
        repulled,
        Some(prior),
        None,
        None,
        true,
        &runtime,
        &cache_path,
    )
    .expect("verified frame publishes");
    assert_eq!(pane_count(&published), 2);
    let cached = read_snapshot_cache(&cache_path, "s").expect("published frame");
    assert_eq!(
        pane_count(&cached),
        2,
        "the verified re-pull, not the first shrunken read, is published"
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

fn diagnostic_events(sink: &crate::diag::DiagSink) -> Vec<crate::schema::diag::DiagEvent> {
    std::fs::read_to_string(sink.log_path())
        .expect("diagnostic log")
        .lines()
        .map(|line| {
            serde_json::from_str::<crate::schema::diag::DiagEnvelope>(line)
                .expect("diagnostic envelope")
                .event
        })
        .collect()
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
        None,
        None,
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
