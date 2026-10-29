use super::*;

#[test]
fn follower_seed_uses_one_record_generation_across_concurrent_moves() {
    let generation = std::cell::Cell::new(0);
    let mut reads = 0;
    let seed = record_generation(
        || Some(SystemTime::UNIX_EPOCH + Duration::from_secs(generation.get())),
        || {
            reads += 1;
            generation.set((generation.get() + 1).min(2));
            format!("build-{}", generation.get())
        },
    );
    assert_eq!(
        seed,
        (
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(2)),
            "build-2".to_owned(),
        ),
        "the seed must not absorb a build move paired with an older mtime",
    );
    assert_eq!(reads, 3);
}

#[test]
fn record_change_requires_a_new_mtime_and_preserves_verification() {
    let prior = SystemTime::UNIX_EPOCH;
    let next = prior + Duration::from_secs(1);
    let target = crate::reload::StagedBuild {
        path: PathBuf::from("/state/builds/next/rimz"),
        build: "next".to_owned(),
    };
    assert_eq!(
        record_change(
            Some(prior),
            Some(prior),
            crate::reload::WorkspaceReexecTarget::Verified(target.clone()),
        ),
        None,
    );
    assert_eq!(
        record_change(
            Some(prior),
            Some(next),
            crate::reload::WorkspaceReexecTarget::Verified(target.clone()),
        ),
        Some(RecordChange::Verified(target)),
    );
    assert_eq!(
        record_change(
            Some(prior),
            Some(next),
            crate::reload::WorkspaceReexecTarget::Invalid,
        ),
        Some(RecordChange::Unavailable),
    );
}

#[test]
fn reexec_target_resolves_the_replacement_after_an_install() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("rimz");
    std::fs::write(&real, b"x").unwrap();
    let deleted = PathBuf::from(format!("{} (deleted)", real.display()));
    assert!(!deleted.is_file(), "the annotated path must not exist");
    assert_eq!(resolve_reexec_target(deleted), Some(real.clone()));
    assert_eq!(resolve_reexec_target(real.clone()), Some(real));
}

#[test]
fn reexec_target_is_none_when_nothing_exists_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("rimz");
    let deleted = PathBuf::from(format!("{} (deleted)", missing.display()));
    assert_eq!(resolve_reexec_target(deleted), None);
    assert_eq!(resolve_reexec_target(missing), None);
}

fn staging_elf(note_offset: usize) -> Vec<u8> {
    let mut image = vec![0_u8; note_offset.max(1 << 20) + 24];
    image[..6].copy_from_slice(b"\x7fELF\x02\x01");
    image[0x20..0x28].copy_from_slice(&64_u64.to_le_bytes());
    image[0x36..0x38].copy_from_slice(&56_u16.to_le_bytes());
    image[0x38..0x3a].copy_from_slice(&1_u16.to_le_bytes());
    image[64..68].copy_from_slice(&4_u32.to_le_bytes());
    image[72..80].copy_from_slice(&(note_offset as u64).to_le_bytes());
    image[96..104].copy_from_slice(&24_u64.to_le_bytes());
    image[note_offset..note_offset + 4].copy_from_slice(&4_u32.to_le_bytes());
    image[note_offset + 4..note_offset + 8].copy_from_slice(&8_u32.to_le_bytes());
    image[note_offset + 8..note_offset + 12].copy_from_slice(&3_u32.to_le_bytes());
    image[note_offset + 12..note_offset + 24].copy_from_slice(b"GNU\0build-id");
    image
}

#[test]
fn staging_copies_and_verifies_file_identity() {
    for note_offset in [0x100, (1 << 20) + 0x100] {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source-rimz");
        let image = staging_elf(note_offset);
        fs::write(&source, &image).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&source, fs::Permissions::from_mode(0o644)).unwrap();
        let expected = crate::build_id::of_file(&source).unwrap();

        let staged = stage_build_under(&source, dir.path()).unwrap();

        assert_eq!(staged.build, expected);
        assert_eq!(
            staged.path,
            dir.path().join("builds").join(&expected).join("rimz")
        );
        assert_eq!(
            crate::build_id::of_file(&staged.path).unwrap(),
            staged.build
        );
        assert_eq!(fs::read(&staged.path).unwrap(), image);
        assert_eq!(
            fs::read_dir(staged.path.parent().unwrap()).unwrap().count(),
            1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::metadata(&staged.path).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o755);
            assert_ne!(metadata.ino(), fs::metadata(&source).unwrap().ino());
        }
    }
}

#[test]
fn staging_reuses_header_identity_without_reading_sparse_image() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source-rimz");
    fs::write(&source, staging_elf(0x100)).unwrap();
    let first = stage_build_under(&source, dir.path()).unwrap();
    let before = fs::metadata(&first.path).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .unwrap()
        .set_len(1 << 40)
        .unwrap();
    #[cfg(unix)]
    fs::set_permissions(&first.path, fs::Permissions::from_mode(0o644)).unwrap();

    let reused = stage_build_under(&source, dir.path());

    assert!(
        reused.is_ok(),
        "a header-identical sparse image must reuse: {reused:?}"
    );
    assert_eq!(reused.unwrap(), first);
    let after = fs::metadata(&first.path).unwrap();
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    assert_eq!(before.len(), after.len());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(before.ino(), after.ino());
        assert_eq!(after.permissions().mode() & 0o777, 0o755);
    }
}

#[test]
fn staging_is_idempotent_and_executable() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source-rimz");
    std::fs::write(&source, b"one immutable build").unwrap();

    let first = stage_build_under(&source, dir.path()).unwrap();
    let second = stage_build_under(&source, dir.path()).unwrap();

    assert_eq!(first, second);
    assert_eq!(std::fs::read(&first.path).unwrap(), b"one immutable build");
    assert_eq!(crate::build_id::of_file(&first.path).unwrap(), first.build);
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(&first.path).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn staging_sweeps_only_unreferenced_builds() {
    let dir = tempfile::tempdir().unwrap();
    let first_source = dir.path().join("first");
    let second_source = dir.path().join("second");
    std::fs::write(&first_source, b"first build").unwrap();
    std::fs::write(&second_source, b"second build").unwrap();
    let first = stage_build_under(&first_source, dir.path()).unwrap();

    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let workspace = crate::workspace::WorkspaceResolver::resolve(&project, None).unwrap();
    let paths = StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
    let mut record = crate::workspace::record::WorkspaceRecord::from_resolved(&workspace);
    record.rimz_bin = Some(first.path.clone());
    record.rimz_build = Some(first.build.clone());
    record::write(&paths, &record).unwrap();
    let known = KnownWorkspace {
        dir_name: paths.dir_name.clone(),
        workspace_id: workspace.workspace_id.clone(),
        project_root: workspace.project_root.clone(),
        session_name: workspace.session_name.clone(),
        root_class: workspace.root_class,
        rimz_bin: record.rimz_bin.clone(),
        updated_at: record.updated_at,
    };

    let unreferenced = crate::disk::paths::builds_dir_under(dir.path()).join("unused");
    std::fs::create_dir_all(&unreferenced).unwrap();
    std::fs::write(unreferenced.join("rimz"), b"unused").unwrap();
    std::fs::File::open(&unreferenced)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - STAGED_BUILD_GC_GRACE - Duration::from_secs(1))
        .unwrap();
    let second = stage_build_under(&second_source, dir.path()).unwrap();
    let room_bin = record_live_room_bin_at(&known, &second, &paths, &runtime).unwrap();

    assert!(
        first.path.is_file(),
        "previously recorded build remains staged"
    );
    assert!(second.path.is_file(), "new build remains staged");
    assert!(!unreferenced.exists(), "unreferenced build is swept");
    assert_eq!(std::fs::read(room_bin).unwrap(), b"second build");
    let updated = record::read(&paths.workspace_record).unwrap();
    assert_eq!(updated.rimz_bin.as_deref(), Some(second.path.as_path()));
    assert_eq!(updated.rimz_build.as_deref(), Some(second.build.as_str()));
}

#[test]
fn staging_sweeps_builds_only_referenced_by_legacy_rooms() {
    let home = tempfile::tempdir().unwrap();
    let project = home.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let workspace = crate::workspace::WorkspaceResolver::resolve(&project, None).unwrap();
    let paths = StatePaths::under(workspace.workspace_id.clone(), home.path()).unwrap();
    let mut record = record::WorkspaceRecord::from_resolved(&workspace);
    record.layout = 1;
    record.rimz_build = Some("legacy".to_owned());
    std::fs::create_dir_all(&paths.root).unwrap();
    std::fs::write(
        &paths.workspace_record,
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let builds = crate::disk::paths::builds_dir_under(home.path());
    let legacy = builds.join("legacy");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::File::open(&legacy)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - STAGED_BUILD_GC_GRACE - Duration::from_secs(1))
        .unwrap();
    sweep_unreferenced_builds(home.path(), &builds, "new");
    assert!(!legacy.exists());
}

#[test]
fn recorded_target_requires_the_recorded_digest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rimz");
    std::fs::write(&path, b"verified build").unwrap();
    let build = crate::build_id::of_file(&path).unwrap();

    assert_eq!(
        resolve_recorded_target(Some(path.clone()), Some(build.clone())),
        WorkspaceReexecTarget::Verified(StagedBuild {
            path: path.clone(),
            build,
        })
    );
    assert_eq!(
        resolve_recorded_target(Some(path.clone()), Some("wrong".to_owned())),
        WorkspaceReexecTarget::Invalid
    );
    assert_eq!(
        resolve_recorded_target(Some(path), None),
        WorkspaceReexecTarget::Absent
    );
    assert_eq!(
        resolve_recorded_target(None, Some("orphan".to_owned())),
        WorkspaceReexecTarget::Invalid
    );
}

#[test]
fn born_recently_holds_inside_the_grace_and_for_clock_fuzz() {
    let now = jiff::Timestamp::from_second(1_000_000).unwrap();
    let grace = crate::sidebar::FRESH_PANE_GRACE;
    let at = |secs_ago: i64| jiff::Timestamp::from_second(1_000_000 - secs_ago).unwrap();
    assert!(crate::sidebar::born_recently(at(0), now, grace));
    assert!(crate::sidebar::born_recently(
        at(i64::try_from(grace.as_secs()).unwrap()),
        now,
        grace
    ));
    assert!(
        !crate::sidebar::born_recently(at(i64::try_from(grace.as_secs()).unwrap() + 1), now, grace,),
        "one second past the grace is old",
    );
    assert!(
        crate::sidebar::born_recently(at(-3), now, grace),
        "a start after `now` is a just-spawned process under clock fuzz",
    );
}

#[test]
fn live_session_claim_is_once_per_mux_session() {
    let mut seen = HashSet::new();
    assert!(claim_live_session(
        &mut seen,
        MuxName::Zellij,
        "rimz-query-engine"
    ));
    assert!(!claim_live_session(
        &mut seen,
        MuxName::Zellij,
        "rimz-query-engine"
    ));
    assert!(
        claim_live_session(&mut seen, MuxName::Tmux, "rimz-query-engine"),
        "the same name on a different mux is a different live session",
    );
}

#[test]
fn reload_outcome_merge_sums_every_field() {
    let mut base = ReloadOutcome {
        sessions: 1,
        presence_dead: 2,
        busy: 4,
        plugin_current: 3,
        plugin_upgraded: 4,
        plugin_reconciled: 18,
        already_current: 5,
        reexeced: 6,
        unconverged: 7,
        unverified: 8,
        recovered: 9,
        closed: 10,
        reaped: 11,
        dead_swept: 12,
        stats_reloaded: 13,
        brokers_retired: 1,
        brokers_marked: 2,
        brokers_kept: 3,
        brokers_unchecked: 4,
        drainers_restarted: 2,
        drainers_stop_failed: 3,
        failed: 14,
        deferred: 15,
        redocked: 16,
        misdocked: 17,
        sidebar_missing: 36,
        daemon_reopened: 2,
        daemon_reopen_deferred: 1,
        daemon_reopen_failed: 3,
    };
    base.merge(ReloadOutcome {
        sessions: 18,
        presence_dead: 19,
        busy: 5,
        plugin_current: 20,
        plugin_upgraded: 21,
        plugin_reconciled: 35,
        already_current: 22,
        reexeced: 23,
        unconverged: 24,
        unverified: 25,
        recovered: 26,
        closed: 27,
        reaped: 28,
        dead_swept: 29,
        stats_reloaded: 30,
        brokers_retired: 5,
        brokers_marked: 6,
        brokers_kept: 7,
        brokers_unchecked: 8,
        drainers_restarted: 4,
        drainers_stop_failed: 5,
        failed: 31,
        deferred: 32,
        redocked: 33,
        misdocked: 34,
        sidebar_missing: 37,
        daemon_reopened: 4,
        daemon_reopen_deferred: 3,
        daemon_reopen_failed: 5,
    });

    assert_eq!(
        base,
        ReloadOutcome {
            sessions: 19,
            presence_dead: 21,
            busy: 9,
            plugin_current: 23,
            plugin_upgraded: 25,
            plugin_reconciled: 53,
            already_current: 27,
            reexeced: 29,
            unconverged: 31,
            unverified: 33,
            recovered: 35,
            closed: 37,
            reaped: 39,
            dead_swept: 41,
            stats_reloaded: 43,
            brokers_retired: 6,
            brokers_marked: 8,
            brokers_kept: 10,
            brokers_unchecked: 12,
            drainers_restarted: 6,
            drainers_stop_failed: 8,
            failed: 45,
            deferred: 47,
            redocked: 49,
            misdocked: 51,
            sidebar_missing: 73,
            daemon_reopened: 6,
            daemon_reopen_deferred: 4,
            daemon_reopen_failed: 8,
        }
    );
}

#[test]
fn presence_health_never_gates_structural_repair() {
    use crate::mux::zellij::PresenceProbe;
    for presence in [
        PresenceProbe::Silent,
        PresenceProbe::Stale {
            ghost_panes: vec![],
            produced_at_ms: 200,
            observed_at_ms: 100,
            writer: None,
        },
        PresenceProbe::Current,
    ] {
        let calls = std::cell::Cell::new(0);
        let outcome = reconcile_repair("room", Some(&presence), || {
            calls.set(calls.get() + 1);
            Ok(Some(crate::mux::SidebarRecovery {
                recovered: 2,
                ..Default::default()
            }))
        });
        assert_eq!(calls.get(), 1, "{presence:?} must still reconcile");
        assert_eq!(outcome.recovered, 2);
        assert_eq!(
            outcome.presence_dead,
            usize::from(presence != PresenceProbe::Current)
        );
    }
}

#[test]
fn repair_counts_lock_contention_separately_from_failure() {
    let busy = reconcile_repair("room", None, || Ok(None));
    assert_eq!(busy.busy, 1);
    assert_eq!(busy.failed, 0);
    let failed = reconcile_repair("room", None, || {
        Err(crate::mux::MuxErr::SessionNotFound {
            session: "room".to_owned(),
        }
        .into())
    });
    assert_eq!(failed.busy, 0);
    assert_eq!(failed.failed, 1);
}

fn heartbeat(raw: &str, build: Option<&str>) -> SidebarHeartbeat {
    let pane = PaneId::from_parts(MuxName::Tmux, raw);
    let mut hb = SidebarHeartbeat::new(
        crate::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
        crate::SidebarInstanceId::new(),
        MuxName::Tmux,
        "rimz-test",
        std::path::PathBuf::from(format!("/tmp/{raw}.sock")),
        Some(pane),
    );
    hb.build = build.map(str::to_owned);
    hb
}

#[test]
fn build_partition_marks_missing_or_stale_builds_as_awaiting() {
    let heartbeats = vec![
        heartbeat("%1", Some("current")),
        heartbeat("%2", Some("old")),
        heartbeat("%3", None),
    ];

    assert_eq!(current_located_count(&heartbeats, "current"), 1);
    let awaiting = awaiting_panes(&heartbeats, "current");
    assert!(!awaiting.contains(&PaneId::from_parts(MuxName::Tmux, "%1")));
    assert!(awaiting.contains(&PaneId::from_parts(MuxName::Tmux, "%2")));
    assert!(awaiting.contains(&PaneId::from_parts(MuxName::Tmux, "%3")));
}

#[test]
fn current_located_count_deduplicates_pane_claims() {
    let heartbeats = vec![
        heartbeat("%1", Some("current")),
        heartbeat("%1", Some("current")),
    ];

    assert_eq!(current_located_count(&heartbeats, "current"), 1);
}

#[test]
fn convergence_wait_settles_when_awaiting_pane_flips_current() {
    let awaiting = [PaneId::from_parts(MuxName::Tmux, "%1")].into();
    let mut reads = 0;

    let heartbeats = wait_for_convergence_with(
        &awaiting,
        "current",
        Duration::from_secs(1),
        Duration::ZERO,
        || {
            reads += 1;
            if reads == 1 {
                vec![heartbeat("%1", Some("old"))]
            } else {
                vec![heartbeat("%1", Some("current"))]
            }
        },
    );

    assert_eq!(reads, 2);
    assert_eq!(
        current_build_claims(&heartbeats, "current"),
        [PaneId::from_parts(MuxName::Tmux, "%1")].into(),
    );
}

#[test]
fn convergence_wait_returns_latest_heartbeat_on_timeout() {
    let awaiting = [PaneId::from_parts(MuxName::Tmux, "%1")].into();

    let heartbeats =
        wait_for_convergence_with(&awaiting, "current", Duration::ZERO, Duration::ZERO, || {
            vec![heartbeat("%1", Some("old"))]
        });

    assert_eq!(heartbeats.len(), 1);
    assert_eq!(heartbeats[0].build.as_deref(), Some("old"));
}
