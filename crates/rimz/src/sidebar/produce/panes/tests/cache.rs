use super::*;
use crate::sidebar::produce::metrics;

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
fn cached_pid_restoration_does_not_rotate_a_pidless_fresh_pane() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = crate::RuntimePaths::under(
        crate::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let now = unix_now_ms();
    let topology = serde_json::from_value(serde_json::json!({
        "session_name": "s",
        "produced_at_ms": now.saturating_add(60_000),
        "focused_pane": 7,
        "clients": { "human_clients": 1, "viewed_panes": [7] },
        "panes": [{
            "id": 7,
            "tab_position": 0,
            "pane_command": "zsh",
            "pane_cwd": "/repo",
            "terminal_command": "zsh"
        }]
    }))
    .unwrap();
    crate::mux::zellij::pane_topology::write_pane_topology_cache(&runtime, &topology).unwrap();
    let mut prior = frame(vec![pane("terminal_7", Some("zsh"), Some("/repo"))]);
    first_mut(&mut prior).current.pid = Some(u32::MAX - 1);
    first_mut(&mut prior).first_seen_at_ms = Some(7);
    let mut history = serde_json::from_value(serde_json::json!({
        "entries": {
            "zellij:terminal_7": {
                "sample_version": 3,
                "sampled_at_ms": now.saturating_add(60_000),
                "stats_pid": u32::MAX,
                "pane_pid": u32::MAX,
                "cpu_ticks": 0,
                "io_bytes": 0,
                "command": "zsh"
            }
        }
    }))
    .unwrap();
    let mut context = FrameContext {
        runtime: &runtime,
        mux: MuxName::Zellij,
        session: "s",
        own_pane: None,
        diag: &crate::diag::DiagSink::disabled(),
        metrics: &mut history,
        sampled: false,
    };
    let (fresh, _) = context.candidate(Some(&prior), true, None, false).unwrap();

    assert_eq!(first(&fresh).current.pid, Some(u32::MAX));
    assert_eq!(
        first(&fresh).previous,
        None,
        "a cached pid restored after rotation is not a mux-reported tenant change"
    );
    assert_eq!(first(&fresh).first_seen_at_ms, Some(7));
}

#[test]
fn unwatched_prior_selects_event_ttl_and_absent_prior_stays_watched() {
    for (prior_session, viewed, expected_calls) in [
        (None, false, 1),
        (Some("s"), false, 0),
        (Some("s"), true, 1),
        (Some("other"), false, 1),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = crate::RuntimePaths::under(
            crate::WorkspaceId::from_project_root(dir.path()),
            dir.path(),
        )
        .unwrap();
        runtime.ensure_dirs().unwrap();
        if let Some(session) = prior_session {
            let mut prior = crate::sidebar::frame::assemble_frame(
                vec![pane("terminal_1", None, Some("/repo"))],
                unix_now_ms().saturating_sub(5_000),
                session,
            );
            if viewed {
                prior.viewed_panes = vec![PaneId::from_parts(MuxName::Zellij, "terminal_1")];
            }
            atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &prior).unwrap();
        }
        let calls = std::cell::Cell::new(0);
        PaneProducer::seeded(&runtime, "s")
            .tick_with(
                &runtime,
                MuxName::Zellij,
                "s",
                None,
                None,
                &crate::diag::DiagSink::disabled(),
                &mut |_, _| {
                    calls.set(calls.get() + 1);
                    Ok(Accepted::Fresh(frame(vec![pane(
                        "terminal_1",
                        None,
                        Some("/repo"),
                    )])))
                },
                &|_, _| None,
            )
            .unwrap();
        assert_eq!(
            calls.get(),
            expected_calls,
            "session={prior_session:?}, viewed={viewed}"
        );
    }
}

#[test]
fn published_focus_trace_tracks_roster_edges_and_keeps_frame_stamps() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = crate::ids::WorkspaceId::from_project_root(Path::new("/focus-publication"));
    let runtime = crate::RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    let diag = crate::diag::DiagSink::under(dir.path().to_owned(), workspace, "s", None);
    let own = pane("terminal_1", Some("rimz sidebar serve"), Some("/repo"));
    let sibling = pane("terminal_2", Some("zsh"), Some("/repo"));
    let mut published = frame(vec![own.clone(), sibling]);
    published.focused_pane = Some(PaneId::from_parts(MuxName::Zellij, "terminal_2"));
    publish_frame(
        &runtime.pane_frame_path(),
        &mut published,
        PaneFramePublicationKind::Topology,
        Some(&own.pane_id),
        &diag,
    );
    let mut repeated = published.clone();
    repeated.produced_at_ms += 1;
    publish_frame(
        &runtime.pane_frame_path(),
        &mut repeated,
        PaneFramePublicationKind::Topology,
        Some(&own.pane_id),
        &diag,
    );
    let mut changed = frame(vec![
        own.clone(),
        pane("terminal_3", Some("zsh"), Some("/repo")),
    ]);
    changed.focused_pane = published.focused_pane.clone();
    changed.produced_at_ms = 3;
    publish_frame(
        &runtime.pane_frame_path(),
        &mut changed,
        PaneFramePublicationKind::Topology,
        Some(&own.pane_id),
        &diag,
    );
    let rows: Vec<serde_json::Value> =
        std::fs::read_to_string(crate::diag::focus_trace::log_path(dir.path()))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert_eq!(
        rows.len(),
        2,
        "identical newer publications are suppressed, not roster edges"
    );
    assert_eq!(rows[0]["event"]["produced_at_ms"], published.produced_at_ms);
    assert_eq!(rows[0]["event"]["observed_at_ms"], published.observed_at_ms);
    assert_eq!(rows[0]["event"]["own_pane_listed"], true);
    assert_eq!(rows[0]["event"]["pane_count"], 2);
    assert_eq!(rows[0]["event"]["publication"], "topology");
    assert_eq!(rows[1]["event"]["produced_at_ms"], changed.produced_at_ms);
    assert_eq!(
        rows[1]["event"]["focused_pane"],
        rows[0]["event"]["focused_pane"]
    );
}

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
            read_snapshot_cache(&path, requested)
                .filter(|frame| snapshot_cache_is_fresh(
                    frame,
                    unix_now_ms(),
                    min_produced_at_ms,
                    ttl
                ))
                .is_some(),
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
        read_snapshot_cache(&consumer_path, "rimz-query-engine")
            .filter(|frame| snapshot_cache_is_fresh(frame, unix_now_ms(), None, SNAPSHOT_CACHE_TTL))
            .is_none(),
        "the producer's fresh-only fast path skips a stale entry"
    );
    assert!(
        read_snapshot_cache(&consumer_path, "rimz-query-engine").is_some(),
        "the consumer's read serves the stale entry as last-good"
    );
}

#[test]
fn carried_prior_expires_at_poll_ttl_in_event_mode() {
    for carried in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let runtime = crate::RuntimePaths::under(
            crate::ids::WorkspaceId::from_project_root(Path::new("/carried-event-ttl")),
            dir.path(),
        )
        .unwrap();
        runtime.ensure_dirs().unwrap();
        let now = unix_now_ms();
        let produced_at_ms = now.saturating_sub(5_000);
        let topology_at_ms = now.saturating_add(60_000);
        let pane_id = PaneId::from_parts(MuxName::Zellij, "terminal_7");
        let mut prior = crate::sidebar::frame::assemble_frame(
            vec![pane("terminal_7", None, Some("/repo"))],
            produced_at_ms,
            "s",
        );
        prior.viewed_panes = vec![pane_id.clone()];
        if carried {
            prior.carried_panes = vec![crate::sidebar::frame::CarriedPane {
                pane_id,
                pid: None,
                start_ticks: None,
                carried_since_ms: produced_at_ms,
            }];
        }
        atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &prior).unwrap();
        atomic::write_temp_then_rename_cache(
            &crate::sidebar::cache::presence_stamp_path(&runtime),
            &crate::sidebar::cache::PresenceStamp {
                written_at_ms: topology_at_ms,
                mux: Some(MuxName::Zellij),
                session_name: Some("s".to_owned()),
            },
        )
        .unwrap();
        let topology = serde_json::from_value(serde_json::json!({
            "session_name": "s",
            "produced_at_ms": topology_at_ms,
            "focused_pane": 7,
            "clients": { "human_clients": 1, "viewed_panes": [7] },
            "panes": [{
                "id": 7,
                "tab_position": 0,
                "pane_command": "zsh",
                "pane_cwd": "/repo",
                "terminal_command": "zsh"
            }]
        }))
        .unwrap();
        crate::mux::zellij::pane_topology::write_pane_topology_cache(&runtime, &topology).unwrap();

        let calls = std::cell::Cell::new(0);
        let returned = PaneProducer::seeded(&runtime, "s")
            .tick_with(
                &runtime,
                MuxName::Zellij,
                "s",
                None,
                None,
                &crate::diag::DiagSink::disabled(),
                &mut |ctx, prior| {
                    calls.set(calls.get() + 1);
                    ctx.produce(prior)
                },
                &|_, _| None,
            )
            .expect("event-mode produce uses pushed topology after a carried prior expires");
        assert_eq!(
            calls.get(),
            usize::from(carried),
            "an ordinary event-mode prior lists no panes"
        );

        assert_eq!(
            returned.observed_at_ms,
            if carried {
                topology_at_ms
            } else {
                produced_at_ms
            },
            "only a carried prior expires before the event TTL: carried={carried}",
        );
        assert!(returned.carried_panes.is_empty());
    }
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
    crate::mux::zellij::pane_topology::write_pane_topology_cache(
        &runtime,
        &crate::mux::zellij::pane_topology::PaneTopologyCache {
            session_name: "s".to_owned(),
            produced_at_ms: now.saturating_sub(1),
            writer: None,
            focused_pane: Some(7),
            clients_withheld: false,
            clients: Some(crate::mux::zellij::pane_topology::TopologyClients {
                human_clients: Some(1),
                viewed_panes: Some(vec![7]),
                views: Vec::new(),
            }),
            panes: vec![crate::mux::zellij::pane_topology::PaneTopologyPane {
                id: 7,
                is_plugin: false,
                is_fullscreen: false,
                is_held: false,
                exited: false,
                is_suppressed: false,
                is_floating: false,
                tab_position: 0,
                tab_viewed: false,
                stable_tab_id: None,
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

    let frame = PaneProducer::seeded(&runtime, "s")
        .tick(
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
        PaneProducer::seeded(&runtime, "s")
            .tick_with(
                &runtime,
                MuxName::Zellij,
                "s",
                None,
                Some(&own),
                &diag,
                &mut |_, _| Err(crate::sidebar::produce::ProduceErr::FrameRejected(
                    FrameRejectReason::MissingOwnPane
                )),
                &|_, _| None
            )
            .is_err(),
        "a cache missing the renderer's own pane must not short-circuit produce"
    );

    let valid = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", Some("zsh"), Some("/repo"))],
        stamp,
        "s",
    );
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &valid).unwrap();
    assert!(
        PaneProducer::seeded(&runtime, "s")
            .tick_with(
                &runtime,
                MuxName::Zellij,
                "s",
                None,
                Some(&own),
                &diag,
                &mut |_, _| panic!("valid fresh frame must not produce"),
                &|_, _| None
            )
            .is_ok()
    );
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
    let valid_prior = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_1", Some("zsh"), Some("/repo"))],
        unix_now_ms(),
        "s",
    );
    let empty_fresh = crate::sidebar::frame::assemble_frame(Vec::new(), unix_now_ms(), "s");
    let held = accept(
        empty_fresh,
        Some(valid_prior),
        None,
        &crate::diag::DiagSink::disabled(),
    )
    .expect("valid prior is held over rejected fresh frame");
    assert!(matches!(held, Accepted::Held(ref frame) if pane_count(frame) == 1));

    let invalid_prior = crate::sidebar::frame::assemble_frame(Vec::new(), unix_now_ms(), "s");
    let empty_fresh = crate::sidebar::frame::assemble_frame(Vec::new(), unix_now_ms(), "s");
    let err = accept(
        empty_fresh,
        Some(invalid_prior),
        None,
        &crate::diag::DiagSink::disabled(),
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
    let state_root = dir.path().join("state");
    let sink = crate::diag::DiagSink::under(state_root.clone(), workspace_id, "s", None);
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

    let held = accept(fresh, Some(prior), Some(&own), &sink)
        .expect("missing-own rejection holds the prior frame");

    assert!(matches!(held, Accepted::Held(ref frame) if pane_count(frame) == 1));
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
        crate::diag::frames_dir_under(&state_root)
            .join(frames_ref)
            .exists(),
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

    let published = PaneProducer::seeded(&runtime, "s")
        .tick_with(
            &runtime,
            MuxName::Zellij,
            "s",
            None,
            Some(&own),
            &crate::diag::DiagSink::disabled(),
            &mut |_, _| {
                accept(
                    fresh.clone(),
                    None,
                    Some(&own),
                    &crate::diag::DiagSink::disabled(),
                )
            },
            &|_, _| None,
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
            (raced, false),
            Some(&prior),
            None,
            &mut |enrich_metrics, min_topology_produced_at_ms, authoritative| {
                assert!(enrich_metrics, "{name}");
                assert!(min_topology_produced_at_ms.is_some(), "{name}");
                assert!(authoritative, "{name}");
                calls.set(calls.get() + 1);
                Ok((verified.clone(), true))
            },
            &crate::diag::DiagSink::disabled(),
            true,
            &metrics::pane_root_bindings(&read_metrics_sample_cache(
                &runtime.lane_path("metrics-sample.json"),
            )),
        )
        .expect("re-pull succeeds");

        assert_eq!(calls.get(), 1, "{name}");
        assert_eq!(pane_count(&repulled), expected_count, "{name}");
        assert!(repulled.carried_panes.is_empty(), "{name}");
        let published = PaneProducer::seeded(&runtime, "s")
            .tick_with(
                &runtime,
                MuxName::Zellij,
                "s",
                None,
                None,
                &crate::diag::DiagSink::disabled(),
                &mut |_, _| {
                    accept(
                        repulled.clone(),
                        Some(prior.clone()),
                        None,
                        &crate::diag::DiagSink::disabled(),
                    )
                },
                &|_, _| None,
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
fn missing_own_pane_controls_authoritative_repull() {
    for (name, initial_raws, expected_calls, expected_count) in [
        ("own pane missing", vec!["terminal_2"], 1, 2),
        ("own pane present", vec!["terminal_9"], 0, 1),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = crate::ids::WorkspaceId::from_project_root(std::path::Path::new(
            &format!("/tmp/{name}"),
        ));
        let runtime = crate::RuntimePaths::under(workspace_id, dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let own = crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, "terminal_9");
        let initial = frame(
            initial_raws
                .into_iter()
                .map(|raw| pane(raw, Some("zsh"), Some("/repo")))
                .collect(),
        );
        let verified = frame(vec![
            pane("terminal_9", Some("zsh"), Some("/repo")),
            pane("terminal_2", Some("zsh"), Some("/repo")),
        ]);
        let calls = std::cell::Cell::new(0);

        let repulled = confirm_and_carry_with(
            (initial, false),
            None,
            Some(&own),
            &mut |enrich_metrics, min_topology_produced_at_ms, authoritative| {
                assert!(enrich_metrics, "{name}");
                assert!(min_topology_produced_at_ms.is_some(), "{name}");
                assert!(authoritative, "{name}");
                calls.set(calls.get() + 1);
                Ok((verified.clone(), true))
            },
            &crate::diag::DiagSink::disabled(),
            true,
            &metrics::pane_root_bindings(&read_metrics_sample_cache(
                &runtime.lane_path("metrics-sample.json"),
            )),
        )
        .expect("pane frame confirmation");

        assert_eq!(calls.get(), expected_calls, "{name}");
        assert_eq!(pane_count(&repulled), expected_count, "{name}");
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
            (fresh, false),
            Some(&prior),
            None,
            &mut |enrich_metrics, min_topology_produced_at_ms, authoritative| {
                assert!(enrich_metrics, "{name}");
                assert!(min_topology_produced_at_ms.is_some(), "{name}");
                assert!(authoritative, "{name}");
                calls.set(calls.get() + 1);
                Ok((verified.clone(), true))
            },
            &crate::diag::DiagSink::disabled(),
            true,
            &metrics::pane_root_bindings(&read_metrics_sample_cache(
                &runtime.lane_path("metrics-sample.json"),
            )),
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
        (fresh, false),
        Some(&prior),
        None,
        &mut |enrich_metrics, min_topology_produced_at_ms, authoritative| {
            assert!(enrich_metrics);
            assert!(min_topology_produced_at_ms.is_some());
            assert!(authoritative);
            Ok((verified.clone(), true))
        },
        &sink,
        true,
        &metrics::pane_root_bindings(&read_metrics_sample_cache(
            &runtime.lane_path("metrics-sample.json"),
        )),
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
    check_partial_frame_carry(false, false);
}

#[test]
fn authoritative_partial_frame_refutes_live_dropped_pane() {
    check_partial_frame_carry(false, true);
    check_partial_frame_carry(true, true);
}

#[test]
fn authoritative_whole_tab_omission_records_confirmed_carry() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let sink = crate::diag::DiagSink::under(dir.path().join("state"), workspace_id, "s", None);
    let mut sibling = pane("terminal_2", Some("zsh"), Some("/repo"));
    sibling.view_id = Some("tab_9".to_owned());
    sibling.pane_pid = Some(std::process::id());
    let prior = frame(vec![
        pane("terminal_1", Some("zsh"), Some("/repo")),
        sibling,
    ]);
    let fresh = frame(vec![pane("terminal_1", Some("zsh"), Some("/repo"))]);
    let carried = confirm_and_carry_with(
        (fresh.clone(), false),
        Some(&prior),
        None,
        &mut |_, _, authoritative| {
            assert!(authoritative);
            Ok((fresh.clone(), true))
        },
        &sink,
        true,
        &std::collections::HashMap::new(),
    )
    .expect("carry confirmation succeeds");

    assert_eq!(pane_count(&carried), 2);
    assert_eq!(carried.tabs.len(), 2);
    assert_eq!(carried.carried_panes.len(), 1);
    assert_eq!(carried.carried_panes[0].pane_id.raw(), "terminal_2");
    let events = diagnostic_events(&sink);
    assert!(
        matches!(events.as_slice(), [crate::diag::record::DiagEvent::PaneCarryForward {
        carried, cli_confirmed: true, prior: 2, fresh: 1, ..
    }] if carried.len() == 1 && carried[0].raw() == "terminal_2"),
        "{events:?}"
    );
}

fn check_partial_frame_carry(initial_authoritative: bool, verified_authoritative: bool) {
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

    let same_tab_pane = |id| {
        let mut pane = pane(id, Some("zsh"), Some("/repo"));
        pane.view_id = Some("tab_0".to_owned());
        pane
    };
    let mut prior = frame(vec![
        same_tab_pane("terminal_1"),
        same_tab_pane("terminal_2"),
    ]);
    prior
        .pane_states_mut()
        .find(|pane| pane.pane_id.raw() == "terminal_2")
        .expect("terminal_2 present")
        .current
        .pid = Some(std::process::id());
    let fresh = frame(vec![same_tab_pane("terminal_1")]);
    let calls = std::cell::Cell::new(0);

    let carried = confirm_and_carry_with(
        (fresh.clone(), initial_authoritative),
        Some(&prior),
        None,
        &mut |enrich_metrics, min_topology_produced_at_ms, authoritative| {
            assert!(enrich_metrics);
            assert!(min_topology_produced_at_ms.is_some());
            assert!(authoritative);
            calls.set(calls.get() + 1);
            Ok((fresh.clone(), verified_authoritative))
        },
        &sink,
        true,
        &metrics::pane_root_bindings(&read_metrics_sample_cache(
            &runtime.lane_path("metrics-sample.json"),
        )),
    )
    .expect("carry confirmation succeeds");

    // tmux's initial server listing needs no carry confirmation.
    assert_eq!(calls.get(), usize::from(!initial_authoritative));
    let events = if initial_authoritative {
        assert!(!sink.log_path().unwrap().exists());
        Vec::new()
    } else {
        diagnostic_events(&sink)
    };
    if verified_authoritative {
        assert_eq!(pane_count(&carried), 1);
        assert!(carried.carried_panes.is_empty());
        if initial_authoritative {
            assert!(events.is_empty(), "{events:?}");
        } else {
            assert!(
                matches!(events.as_slice(), [crate::diag::record::DiagEvent::PaneCarryRefuted {
                carried, pids, prior: 2, fresh: 1, verified: 1, frames_ref: None,
            }] if carried.len() == 1 && carried[0].raw() == "terminal_2"
                && pids == &vec![std::process::id()]),
                "{events:?}"
            );
        }
        return;
    }
    assert_eq!(pane_count(&carried), 2);
    assert_eq!(carried.carried_panes.len(), 1);
    assert_eq!(
        carried.carried_panes[0].pane_id.raw(),
        "terminal_2",
        "the omitted live pane is carried"
    );
    assert_eq!(carried.carried_panes[0].start_ticks, Some(root_ticks));

    assert!(matches!(
        events.as_slice(),
        [crate::diag::record::DiagEvent::PaneCarryForward {
            carried,
            pids,
            prior: 2,
            fresh: 1,
            cli_confirmed: false,
            frames_ref: Some(_),
        }] if carried.len() == 1 && carried[0].raw() == "terminal_2"
            && pids == &vec![std::process::id()]
    ));
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
    let refreshed = PaneProducer::seeded(&runtime, "s")
        .tick(&runtime, MuxName::Zellij, "s", None, None, &diag)
        .unwrap();

    assert_eq!(refreshed.produced_at_ms, produced_at_ms);
    let published = read_snapshot_cache(&cache_path, "s").unwrap();
    assert_eq!(
        published.produced_at_ms, produced_at_ms,
        "a metrics-only publish must not masquerade as a fresh pane listing"
    );
    assert!(
        runtime.lane_path("metrics-sample.json").exists(),
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

    let returned = PaneProducer::seeded(&runtime, "s")
        .tick(
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
