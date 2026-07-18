use super::*;

#[test]
fn snapshot_cache_freshness_matrix() {
    let dir = tempfile::tempdir().unwrap();
    // Keep the freshness cases independent from CI scheduler stalls. The
    // cache freshness check saturates future stamps to age 0, matching the
    // production clock-skew contract.
    let fresh = unix_now_ms().saturating_add(60_000);
    let stale = unix_now_ms().saturating_sub(SNAPSHOT_CACHE_TTL.as_millis() as u64 + 1);

    enum CacheEntry {
        Valid {
            session: &'static str,
            produced_at_ms: u64,
            carried: bool,
        },
        Unreadable,
        Absent,
    }

    for (name, entry, requested, min_produced_at_ms, ttl, expected_hit) in [
        (
            "fresh same-session",
            CacheEntry::Valid {
                session: "rimz-query-engine",
                produced_at_ms: fresh,
                carried: false,
            },
            "rimz-query-engine",
            None,
            SNAPSHOT_CACHE_TTL,
            true,
        ),
        // One session's panes must never be served to a sidebar pinned to
        // another — the Zellij backend stamps PaneRef.session_name from the
        // requested session, so a cross-session hit would mislabel panes.
        (
            "different session",
            CacheEntry::Valid {
                session: "rimz-query-engine",
                produced_at_ms: fresh,
                carried: false,
            },
            "rimz-other",
            None,
            SNAPSHOT_CACHE_TTL,
            false,
        ),
        (
            "stale entry",
            CacheEntry::Valid {
                session: "rimz-query-engine",
                produced_at_ms: stale,
                carried: false,
            },
            "rimz-query-engine",
            None,
            SNAPSHOT_CACHE_TTL,
            false,
        ),
        (
            "absent cache",
            CacheEntry::Absent,
            "rimz-query-engine",
            None,
            SNAPSHOT_CACHE_TTL,
            false,
        ),
        (
            "unreadable json",
            CacheEntry::Unreadable,
            "rimz-query-engine",
            None,
            SNAPSHOT_CACHE_TTL,
            false,
        ),
        (
            "floor at stamp",
            CacheEntry::Valid {
                session: "rimz-query-engine",
                produced_at_ms: fresh,
                carried: false,
            },
            "rimz-query-engine",
            Some(fresh),
            SNAPSHOT_CACHE_TTL,
            true,
        ),
        (
            "floor past stamp",
            CacheEntry::Valid {
                session: "rimz-query-engine",
                produced_at_ms: fresh,
                carried: false,
            },
            "rimz-query-engine",
            Some(fresh.saturating_add(1)),
            SNAPSHOT_CACHE_TTL,
            false,
        ),
        (
            "carried panes clamp event ttl",
            CacheEntry::Valid {
                session: "rimz-query-engine",
                produced_at_ms: stale,
                carried: true,
            },
            "rimz-query-engine",
            None,
            crate::sidebar::timing::EVENT_PANE_TTL,
            false,
        ),
    ] {
        let path = dir.path().join(format!("{name}.json"));
        match entry {
            CacheEntry::Valid {
                session,
                produced_at_ms,
                carried,
            } => write_snapshot_cache(&path, session, produced_at_ms, carried),
            CacheEntry::Unreadable => std::fs::write(&path, b"{ not json").unwrap(),
            CacheEntry::Absent => {}
        }
        assert_eq!(
            fresh_snapshot_cache(&path, requested, min_produced_at_ms, ttl).is_some(),
            expected_hit,
            "{name}"
        );
    }

    // A `--no-produce` renderer holds the producer's last published base even
    // past the freshness TTL — it renders the last good frame rather than
    // forking its own `list-panes`. The fresh-only read (the producer's fast
    // path) misses the stale entry; the TTL-agnostic read still serves it.
    let consumer_path = dir.path().join("consumer-stale.json");
    write_snapshot_cache(&consumer_path, "rimz-query-engine", stale, false);
    assert!(
        fresh_snapshot_cache(
            &consumer_path,
            "rimz-query-engine",
            None,
            SNAPSHOT_CACHE_TTL
        )
        .is_none(),
        "the producer's fresh-only fast path skips a stale entry"
    );
    assert!(
        read_snapshot_cache(&consumer_path, "rimz-query-engine").is_some(),
        "the consumer's read serves the stale entry as last-good"
    );
}

#[test]
fn producer_verification_trusts_event_carried_topology_without_topology_floor() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = crate::ids::WorkspaceId::from_project_root(std::path::Path::new(
        "/tmp/producer-topology-floor",
    ));
    let runtime = crate::RuntimePaths::under(workspace_id, dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    let now = unix_now_ms();
    write_snapshot_cache(
        &runtime.pane_frame_path(),
        "s",
        now.saturating_sub(crate::sidebar::timing::EVENT_PANE_TTL.as_millis() as u64 + 1),
        false,
    );
    crate::sidebar::cache::write_pane_topology_cache(
        &runtime,
        &crate::mux::zellij::pane_topology::PaneTopologyCache {
            session_name: "s".to_owned(),
            produced_at_ms: now.saturating_sub(1),
            writer: None,
            focused_pane: Some(7),
            clients: Some(crate::mux::zellij::pane_topology::TopologyClients {
                human_clients: 1,
                viewed_panes: vec![7],
                views: Vec::new(),
            }),
            panes: vec![crate::mux::zellij::pane_topology::PaneTopologyPane {
                id: 7,
                is_plugin: false,
                is_held: false,
                exited: false,
                is_suppressed: false,
                is_floating: false,
                tab_position: 0,
                tab_name: Some("main".to_owned()),
                pane_columns: Some(80),
                pane_x: Some(0),
                title: Some("zsh".to_owned()),
                pane_command: Some("zsh".to_owned()),
                pane_cwd: Some("/repo".to_owned()),
                pane_pid: None,
                terminal_command: Some("zsh".to_owned()),
            }],
        },
    )
    .expect("write topology cache");

    let frame = cached_panes_or_produce(
        &runtime,
        MuxName::Zellij,
        "s",
        Some(now),
        None,
        &crate::diag::DiagSink::disabled(),
    )
    .expect("event-carried topology satisfies verification");

    assert_eq!(pane_count(&frame), 1);
    assert_eq!(
        frame.viewed_panes,
        vec![crate::ids::PaneId::from_parts(
            crate::ids::MuxName::Zellij,
            "terminal_7"
        )]
    );
    assert_eq!(frame.presence.expect("pushed presence").human_clients, 1);
}

#[test]
fn pane_frame_cache_rejects_invalid_cached_frames() {
    let dir = tempfile::tempdir().unwrap();
    let own = crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_1");
    let runtime = crate::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/validated-cache")),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let diag = crate::diag::DiagSink::disabled();
    let cache = PaneFrameCache::new(&runtime, "s", None, Some(&own), &diag);
    // A future stamp keeps both frames inside the freshness TTL across CI
    // scheduler stalls (freshness saturates future stamps to age 0), so the
    // negative assertion below exercises the missing-own validation rather
    // than an accidental TTL expiry.
    let stamp = unix_now_ms().saturating_add(60_000);

    // Give the replacement a different serialized length so this validation
    // test does not also depend on the filesystem recording distinct mtimes
    // for two immediate atomic renames. The parse cache deliberately keys on
    // `(path, mtime, len)`.
    let missing_own = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_22", Some("zsh"), Some("/repo"))],
        stamp,
        "s",
    );
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &missing_own).unwrap();
    assert!(
        cache.fresh().is_none(),
        "a cache missing the renderer's own pane must not short-circuit produce"
    );

    let valid = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", Some("zsh"), Some("/repo"))],
        stamp,
        "s",
    );
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &valid).unwrap();
    assert!(cache.fresh().is_some());
}

#[test]
fn zombie_stat_metrics_do_not_prove_liveness() {
    let zombie = crate::proc::StatMetrics {
        state: 'Z',
        cpu_ticks: 1,
        child_cpu_ticks: 0,
        rss_kb: 0,
        start_ticks: 42,
    };
    let sleeping = crate::proc::StatMetrics {
        state: 'S',
        cpu_ticks: 1,
        child_cpu_ticks: 0,
        rss_kb: 4,
        start_ticks: 43,
    };

    assert_eq!(live_start_ticks(zombie), None);
    assert_eq!(live_start_ticks(sleeping), Some(43));
}

#[test]
fn rejected_frame_holds_only_a_publishable_prior() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = crate::ids::WorkspaceId::from_project_root(std::path::Path::new(
        "/tmp/rejected-frame-prior",
    ));
    let runtime = crate::RuntimePaths::under(workspace_id, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let cache_path = runtime.pane_frame_path();

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
        &crate::diag::DiagSink::disabled(),
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
        &crate::diag::DiagSink::disabled(),
        false,
        &runtime,
        &cache_path,
    )
    .expect_err("invalid prior must not be returned as last good");
    assert!(
        matches!(
            err,
            crate::sidebar::produce::ProduceErr::FrameRejected(
                crate::diag::record::FrameRejectReason::Empty
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
    let cache_path = runtime.pane_frame_path();
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
        &sink,
        false,
        &runtime,
        &cache_path,
    )
    .expect("missing-own rejection holds the prior frame");

    assert_eq!(pane_count(&held), 1);
    let event = diagnostic_events(&sink).pop().expect("diagnostic event");
    let crate::diag::record::DiagEvent::FrameRejected {
        reason: crate::diag::record::FrameRejectReason::MissingOwnPane,
        frames_ref: Some(frames_ref),
        ..
    } = event
    else {
        panic!("expected missing-own frame rejection with frame capture");
    };
    assert!(
        sink.frame_capture_path(&frames_ref).unwrap().exists(),
        "the frame reference points at a captured prior/offending pair"
    );
}

#[test]
fn missing_own_pane_without_prior_publishes() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = crate::ids::WorkspaceId::from_project_root(std::path::Path::new(
        "/tmp/missing-own-no-prior",
    ));
    let runtime = crate::RuntimePaths::under(workspace_id, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let cache_path = runtime.pane_frame_path();
    let own = crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_1");
    let fresh = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_2", Some("zsh"), Some("/repo"))],
        unix_now_ms(),
        "s",
    );

    let published = validate_frame_for_publish(
        fresh,
        None,
        Some(&own),
        &crate::diag::DiagSink::disabled(),
        true,
        &runtime,
        &cache_path,
    )
    .expect("missing-own frame without prior remains publishable");

    assert_eq!(pane_count(&published), 1);
    let cached = read_snapshot_cache(&cache_path, "s").expect("published frame");
    assert_eq!(pane_count(&cached), 1);
}

#[test]
fn degraded_first_read_publishes_verified_repull_result() {
    for (name, raced_raws, verified_raws, expected_count) in [
        (
            "verified shrink",
            vec!["terminal_1"],
            vec!["terminal_1", "terminal_2"],
            2,
        ),
        (
            "ambiguous loss",
            vec!["terminal_1", "terminal_2"],
            vec!["terminal_1", "terminal_2", "terminal_3"],
            3,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = crate::ids::WorkspaceId::from_project_root(std::path::Path::new(
            &format!("/tmp/{name}"),
        ));
        let runtime = crate::RuntimePaths::under(workspace_id, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let cache_path = runtime.pane_frame_path();
        let prior = frame(vec![
            pane("terminal_1", Some("zsh"), Some("/repo")),
            pane("terminal_2", Some("zsh"), Some("/repo")),
            pane("terminal_3", Some("zsh"), Some("/repo")),
        ]);
        let raced = frame(
            raced_raws
                .into_iter()
                .map(|raw| pane(raw, Some("zsh"), Some("/repo")))
                .collect(),
        );
        let verified = frame(
            verified_raws
                .into_iter()
                .map(|raw| pane(raw, Some("zsh"), Some("/repo")))
                .collect(),
        );
        let calls = std::cell::Cell::new(0);

        let repulled = confirm_and_carry_with(
            raced,
            Some(&prior),
            None,
            &|enrich_metrics, min_topology_produced_at_ms, authoritative| {
                assert!(enrich_metrics, "{name}");
                assert!(min_topology_produced_at_ms.is_some(), "{name}");
                assert!(authoritative, "{name}");
                calls.set(calls.get() + 1);
                Ok(verified.clone())
            },
            &crate::diag::DiagSink::disabled(),
            true,
            &runtime,
        )
        .expect("re-pull succeeds");

        assert_eq!(calls.get(), 1, "{name}");
        assert_eq!(pane_count(&repulled), expected_count, "{name}");
        assert!(repulled.carried_panes.is_empty(), "{name}");
        let published = validate_frame_for_publish(
            repulled,
            Some(prior),
            None,
            &crate::diag::DiagSink::disabled(),
            true,
            &runtime,
            &cache_path,
        )
        .expect("verified frame publishes");
        assert_eq!(pane_count(&published), expected_count, "{name}");
        let cached = read_snapshot_cache(&cache_path, "s").expect("published frame");
        assert_eq!(
            pane_count(&cached),
            expected_count,
            "the verified re-pull, not the first degraded read, is published: {name}"
        );
    }
}

#[test]
fn ambiguous_plain_process_absence_repull_matrix() {
    for (name, verified_keeps_row, expected_row_present) in [
        ("verified row survives", true, true),
        ("verified missing row drops", false, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = crate::ids::WorkspaceId::from_project_root(std::path::Path::new(
            &format!("/tmp/plain-process-{name}"),
        ));
        let runtime = crate::RuntimePaths::under(workspace_id, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let mut prior = frame(vec![
            pane("terminal_1", Some("zsh"), Some("/repo")),
            pane("terminal_5", Some("zsh"), Some("/repo")),
        ]);
        prior
            .pane_states_mut()
            .find(|pane| pane.pane_id.raw() == "terminal_5")
            .expect("terminal_5 present")
            .current
            .pid = Some(u32::MAX);
        let fresh = frame(vec![pane("terminal_1", Some("zsh"), Some("/repo"))]);
        let verified = if verified_keeps_row {
            prior.clone()
        } else {
            fresh.clone()
        };
        let calls = std::cell::Cell::new(0);

        let repulled = confirm_and_carry_with(
            fresh,
            Some(&prior),
            None,
            &|enrich_metrics, min_topology_produced_at_ms, authoritative| {
                assert!(enrich_metrics, "{name}");
                assert!(min_topology_produced_at_ms.is_some(), "{name}");
                assert!(authoritative, "{name}");
                calls.set(calls.get() + 1);
                Ok(verified.clone())
            },
            &crate::diag::DiagSink::disabled(),
            true,
            &runtime,
        )
        .expect("ambiguous process loss re-pulls");

        assert_eq!(calls.get(), 1, "{name}");
        assert!(repulled.carried_panes.is_empty(), "{name}");
        assert_eq!(
            live_row_ids(&repulled).contains(&"zellij:terminal_5".to_owned()),
            expected_row_present,
            "{name}"
        );
    }
}

#[test]
fn refuted_initial_carry_records_diagnostic() {
    if crate::proc::stat_metrics(std::process::id()).is_none() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let workspace_id =
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/refuted-carry"));
    let runtime = crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let sink = crate::diag::DiagSink::under(dir.path().join("state"), workspace_id, "s", None);

    let mut prior = frame(vec![
        pane("terminal_1", Some("zsh"), Some("/repo")),
        pane("terminal_2", Some("zsh"), Some("/repo")),
    ]);
    prior
        .pane_states_mut()
        .find(|pane| pane.pane_id.raw() == "terminal_2")
        .expect("terminal_2 present")
        .current
        .pid = Some(std::process::id());
    let fresh = frame(vec![pane("terminal_1", Some("zsh"), Some("/repo"))]);
    let verified = prior.clone();

    let repulled = confirm_and_carry_with(
        fresh,
        Some(&prior),
        None,
        &|enrich_metrics, min_topology_produced_at_ms, authoritative| {
            assert!(enrich_metrics);
            assert!(min_topology_produced_at_ms.is_some());
            assert!(authoritative);
            Ok(verified.clone())
        },
        &sink,
        true,
        &runtime,
    )
    .expect("confirm succeeds");

    assert_eq!(pane_count(&repulled), 2);
    assert!(repulled.carried_panes.is_empty());
    let events = diagnostic_events(&sink);
    assert!(matches!(
        events.as_slice(),
        [crate::diag::record::DiagEvent::PaneCarryRefuted {
            carried,
            pids,
            prior: 2,
            fresh: 1,
            verified: 2,
            frames_ref: None,
        }] if carried.len() == 1 && carried[0].raw() == "terminal_2"
            && pids == &vec![std::process::id()]
    ));
    let frames_dir = crate::diag::frames_dir_under(&dir.path().join("state"));
    assert!(
        std::fs::read_dir(&frames_dir)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true),
        "refuted carry records without capturing diagnostic frames"
    );
}

#[test]
fn confirmed_partial_frame_carries_live_dropped_pane_and_records_diagnostic() {
    let Some(root_ticks) =
        crate::proc::stat_metrics(std::process::id()).map(|stat| stat.start_ticks)
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let workspace_id =
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/confirmed-carry"));
    let runtime = crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let sink = crate::diag::DiagSink::under(dir.path().join("state"), workspace_id, "s", None);

    let mut prior = frame(vec![
        pane("terminal_1", Some("zsh"), Some("/repo")),
        pane("terminal_2", Some("zsh"), Some("/repo")),
    ]);
    prior
        .pane_states_mut()
        .find(|pane| pane.pane_id.raw() == "terminal_2")
        .expect("terminal_2 present")
        .current
        .pid = Some(std::process::id());
    let fresh = frame(vec![pane("terminal_1", Some("zsh"), Some("/repo"))]);
    let calls = std::cell::Cell::new(0);

    let carried = confirm_and_carry_with(
        fresh.clone(),
        Some(&prior),
        None,
        &|enrich_metrics, min_topology_produced_at_ms, authoritative| {
            assert!(enrich_metrics);
            assert!(min_topology_produced_at_ms.is_some());
            assert!(authoritative);
            calls.set(calls.get() + 1);
            Ok(fresh.clone())
        },
        &sink,
        true,
        &runtime,
    )
    .expect("carry confirmation succeeds");

    assert_eq!(calls.get(), 1);
    assert_eq!(pane_count(&carried), 2);
    assert_eq!(carried.carried_panes.len(), 1);
    assert_eq!(
        carried.carried_panes[0].pane_id.raw(),
        "terminal_2",
        "the omitted live pane is carried"
    );
    assert_eq!(carried.carried_panes[0].start_ticks, Some(root_ticks));

    let events = diagnostic_events(&sink);
    assert!(matches!(
        events.as_slice(),
        [crate::diag::record::DiagEvent::PaneCarryForward {
            carried,
            pids,
            prior: 2,
            fresh: 1,
            cli_confirmed: true,
            frames_ref: Some(_),
        }] if carried.len() == 1 && carried[0].raw() == "terminal_2"
            && pids == &vec![std::process::id()]
    ));
}

#[test]
fn prior_frame_from_another_build_records_mixed_writers() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id =
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/mixed-builds"));
    let runtime = crate::RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let cache_path = runtime.pane_frame_path();
    let sink = crate::diag::DiagSink::under(dir.path().join("state"), workspace_id, "s", None);
    let own = crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_1");
    let now = unix_now_ms();
    let mut prior = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", Some("zsh"), Some("/repo"))],
        now,
        "s",
    );
    prior.build = Some("0000aaaa0000".to_owned());
    let fresh = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", Some("zsh"), Some("/repo"))],
        now.saturating_add(1),
        "s",
    );

    validate_frame_for_publish(
        fresh.clone(),
        Some(prior),
        Some(&own),
        &sink,
        false,
        &runtime,
        &cache_path,
    )
    .expect("a valid fresh frame publishes despite the build mismatch");
    // A prior frame from this very build stays silent.
    validate_frame_for_publish(
        fresh.clone(),
        Some(fresh),
        Some(&own),
        &sink,
        false,
        &runtime,
        &cache_path,
    )
    .expect("same-build prior publishes");

    let mixed = diagnostic_events(&sink)
        .into_iter()
        .filter_map(|event| match event {
            crate::diag::record::DiagEvent::MixedBuildWriters {
                prior_build,
                own_build,
            } => Some((prior_build, own_build)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        mixed,
        vec![(
            "0000aaaa0000".to_owned(),
            crate::build_id::current()
                .expect("test binary build id")
                .to_owned()
        )]
    );
}

fn diagnostic_events(sink: &crate::diag::DiagSink) -> Vec<crate::diag::record::DiagEvent> {
    std::fs::read_to_string(sink.log_path().unwrap())
        .expect("diagnostic log")
        .lines()
        .map(|line| {
            serde_json::from_str::<crate::diag::record::DiagEnvelope>(line)
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
    let cache_path = runtime.pane_frame_path();
    atomic::write_temp_then_rename_cache(&cache_path, &frame).unwrap();
    let diag = crate::diag::DiagSink::disabled();
    let cache = PaneFrameCache::new(&runtime, "s", None, None, &diag);

    let refreshed = refresh_cached_metrics(frame, &cache);

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

#[test]
fn fresh_cache_hit_skips_election_mux_and_publication() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/fresh-frame")),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    // A future stamp keeps the cache inside the freshness TTL across CI
    // scheduler stalls (freshness saturates future stamps to age 0); a stale
    // miss here would fork a real mux produce and fail the expect below.
    // Leave the pane unsampleable so the independent metrics cadence cannot
    // legitimately republish the otherwise-fresh topology during this test.
    let cached = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", None, Some("/repo"))],
        unix_now_ms().saturating_add(60_000),
        "s",
    );
    let path = runtime.pane_frame_path();
    atomic::write_temp_then_rename_cache(&path, &cached).unwrap();
    let before = std::fs::read(&path).unwrap();

    let returned = cached_panes_or_produce(
        &runtime,
        MuxName::Zellij,
        "s",
        None,
        None,
        &crate::diag::DiagSink::disabled(),
    )
    .expect("fresh frame bypasses unavailable mux");

    assert_eq!(returned.produced_at_ms, cached.produced_at_ms);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "cache is not republished"
    );
}

#[test]
fn wedged_snapshot_producer_serves_prior_without_local_mux_fork() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/wedged-producer")),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();

    let prior = frame(vec![pane("terminal_1", Some("zsh"), Some("/repo"))]);
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &prior).unwrap();
    let lock_path = runtime.root.join("snapshot.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    lock.try_lock().unwrap();

    let held = cached_panes_or_produce(
        &runtime,
        crate::ids::MuxName::Zellij,
        "s",
        Some(unix_now_ms()),
        None,
        &crate::diag::DiagSink::disabled(),
    )
    .expect("stale prior should be held while producer is wedged");

    assert_eq!(
        held.to_pane_refs(),
        prior.to_pane_refs(),
        "a lock loser should not start another Zellij list-panes client when a prior frame is usable"
    );
}
