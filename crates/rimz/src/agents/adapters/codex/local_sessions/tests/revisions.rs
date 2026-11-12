use super::*;

#[test]
fn rollout_filename_derivation_and_matching_share_thread_identity() {
    let thread = "11111111-1111-4111-8111-111111111111";
    let rollout = "22222222-2222-4222-8222-222222222222";
    for (name, expected) in [
        (
            format!("rollout-2026-01-01T00-00-00-{thread}.jsonl"),
            Some(thread),
        ),
        (
            format!("rollout-2026-01-01T00-00-00-{thread}_{rollout}.jsonl"),
            Some(thread),
        ),
        (
            format!("rollout-2026-01-01T00-00-00-sess-abc_{rollout}.jsonl"),
            None,
        ),
        (format!("history-{thread}.jsonl"), None),
        (format!("rollout-2026-01-01-{thread}.jsonl"), None),
        (format!("rollout-2026-01-01T00-00-00_{thread}.jsonl"), None),
        (format!("rollout-2026-01-01T00-00-00-{thread}_.jsonl"), None),
        (
            format!("rollout-2026-01-01T00-00-00-{thread}.jsonl.zst"),
            None,
        ),
    ] {
        let derived = session_id_from_filename(Path::new(&name));
        assert_eq!(derived, expected, "{name}");
        for id in [thread, rollout] {
            assert_eq!(
                rollout_filename_matches_session(&name, id),
                derived == Some(id),
                "{name}: {id}"
            );
        }
    }
    assert!(rollout_filename_matches_session(
        "rollout-2026-01-01T00-00-00-sess-abc.jsonl",
        "sess-abc"
    ));
    assert!(rollout_filename_matches_session(
        &format!("rollout-2026-01-01T00-00-00-sess-abc_{rollout}.jsonl"),
        "sess-abc"
    ));
}

#[test]
fn discovers_newer_archived_revision_over_older_active_rollout() {
    let temp = tempfile::tempdir().unwrap();
    let thread = "11111111-1111-4111-8111-111111111111";
    let base = write_rollout(
        &date_path(temp.path(), false, 0),
        thread,
        "/workspace/project",
        "2025-01-01T00:00:00Z",
        1,
    );
    let archive = date_path(temp.path(), true, 0);
    fs::create_dir_all(&archive).unwrap();
    let revision = archive.join(format!(
        "{}_22222222-2222-4222-8222-222222222222.jsonl",
        base.file_stem().unwrap().to_str().unwrap()
    ));
    fs::copy(base, &revision).unwrap();

    let observations = discover_under(temp.path(), &[Path::new("/workspace/project")]);

    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].session_id.as_str(), thread);
    assert_eq!(observations[0].transcript_path, revision);
}

#[test]
fn cold_discovery_reads_one_header_for_all_thread_revisions() {
    let temp = tempfile::tempdir().unwrap();
    let thread = "11111111-1111-4111-8111-111111111111";
    let base = write_rollout(
        &date_path(temp.path(), false, 0),
        thread,
        "/workspace/project",
        "2025-01-01T00:00:00Z",
        1,
    );
    let mut newest = base.clone();
    for rollout in [
        "22222222-2222-4222-8222-222222222222",
        "33333333-3333-4333-8333-333333333333",
    ] {
        newest = base.with_file_name(format!(
            "{}_{rollout}.jsonl",
            base.file_stem().unwrap().to_str().unwrap()
        ));
        fs::copy(&base, &newest).unwrap();
    }
    let key = DiscoveryKey {
        home: temp.path().to_path_buf(),
        workspaces: vec![PathBuf::from("/workspace/project")],
        today: Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date(),
    };
    let mut snapshot = CodexDiscoverySnapshot::default();

    let observations = snapshot.refresh(key, Instant::now());

    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].session_id.as_str(), thread);
    assert_eq!(observations[0].transcript_path, newest);
    assert_eq!(snapshot.work.header_reads, 1);
}

fn full_scan(home: &Path) -> (Vec<LocalSessionObservation>, usize) {
    let key = DiscoveryKey {
        home: home.to_path_buf(),
        workspaces: vec![PathBuf::from("/workspace/project")],
        today: Timestamp::now().to_zoned(jiff::tz::TimeZone::UTC).date(),
    };
    let mut snapshot = CodexDiscoverySnapshot::default();
    let observations = snapshot.refresh(key, Instant::now());
    (observations, snapshot.work.header_reads)
}

fn revisions(base: &Path, count: usize) -> Vec<PathBuf> {
    (0..count)
        .map(|index| {
            let revision = base.with_file_name(format!(
                "{}_{index:08x}-bbbb-4bbb-8bbb-{index:012x}.jsonl",
                base.file_stem().unwrap().to_str().unwrap()
            ));
            fs::copy(base, &revision).unwrap();
            revision
        })
        .collect()
}

#[test]
fn full_scan_cap_counts_skipped_and_invalid_revisions() {
    let higher = "22222222-2222-4222-8222-222222222222";
    let lower = "11111111-1111-4111-8111-111111111111";
    let modified = Timestamp::now().as_second() as u64;
    for valid in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let dir = date_path(temp.path(), false, 0);
        let base = write_rollout(
            &dir,
            higher,
            "/workspace/project",
            "2025-01-01T00:00:00Z",
            modified,
        );
        if !valid {
            fs::write(&base, "not a header").unwrap();
        }
        revisions(&base, MAX_EXAMINED_FILES);
        fs::remove_file(base).unwrap();
        write_rollout(
            &dir,
            lower,
            "/workspace/project",
            "2025-01-01T00:00:00Z",
            modified,
        );
        let (observations, reads) = full_scan(temp.path());
        let ids = observations
            .iter()
            .map(|row| row.session_id.as_str())
            .collect::<Vec<_>>();
        if valid {
            assert_eq!((ids, reads), (vec![higher], 1));
        } else {
            assert_eq!((ids, reads), (vec![], MAX_EXAMINED_FILES));
        }
    }
}

#[test]
fn full_scan_filters_archived_twins_before_the_cap() {
    let modified = Timestamp::now().as_second() as u64;
    let temp = tempfile::tempdir().unwrap();
    let dir = date_path(temp.path(), false, 0);
    let archive = date_path(temp.path(), true, 0);
    fs::create_dir_all(&archive).unwrap();
    for index in 1..MAX_EXAMINED_FILES {
        let active = write_rollout(
            &dir,
            &format!("{index:08x}-aaaa-4aaa-8aaa-{index:012x}"),
            "/workspace/project",
            "2025-01-01T00:00:00Z",
            modified,
        );
        fs::copy(&active, archive.join(active.file_name().unwrap())).unwrap();
    }
    write_rollout(
        &archive,
        "00000000-aaaa-4aaa-8aaa-000000000000",
        "/workspace/project",
        "2025-01-01T00:00:00Z",
        modified,
    );
    let (observations, reads) = full_scan(temp.path());
    assert_eq!(
        (observations.len(), reads),
        (MAX_EXAMINED_FILES, MAX_EXAMINED_FILES)
    );
    assert!(
        observations
            .iter()
            .any(|row| row.session_id == "00000000-aaaa-4aaa-8aaa-000000000000")
    );

    let temp = tempfile::tempdir().unwrap();
    let thread = "11111111-1111-4111-8111-111111111111";
    let active = write_rollout(
        &date_path(temp.path(), false, 0),
        thread,
        "/workspace/project",
        "2025-01-01T00:00:00Z",
        modified,
    );
    let archive = date_path(temp.path(), true, 0);
    fs::create_dir_all(&archive).unwrap();
    fs::copy(&active, archive.join(active.file_name().unwrap())).unwrap();
    fs::write(&active, "not a header").unwrap();
    assert!(full_scan(temp.path()).0.is_empty());
}
