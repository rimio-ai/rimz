use super::*;

fn generations(paths: &StatePaths) -> [u64; 2] {
    [
        lifecycle_log_generation(paths),
        lifecycle_log_generation_constant_memory(paths),
    ]
}

fn fixture() -> (tempfile::TempDir, StatePaths, RollupCache) {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let cache = RollupCache {
        version: ROLLUP_CACHE_VERSION,
        extent: event_log::LogExtent {
            generation: 7,
            offset: 123,
        },
        raw_agents: Vec::new(),
        resume_outcomes: Vec::new(),
        agent_identity: AgentIdentityState::default(),
        saw_session_rebirth: false,
    };
    (dir, paths, cache)
}

#[test]
fn reads_the_published_generation() {
    let (_dir, paths, cache) = fixture();
    write_rollup_cache(&paths, &cache).unwrap();
    assert_eq!(generations(&paths), [7; 2]);
}

#[test]
fn same_size_replacement_is_visible_with_the_same_mtime() {
    let (_dir, paths, mut cache) = fixture();
    write_rollup_cache(&paths, &cache).unwrap();
    let original = fs::metadata(&paths.rollup_cache).unwrap();
    assert_eq!(generations(&paths), [7; 2]);

    cache.extent.generation = 8;
    write_rollup_cache(&paths, &cache).unwrap();
    fs::File::options()
        .write(true)
        .open(&paths.rollup_cache)
        .unwrap()
        .set_modified(original.modified().unwrap())
        .unwrap();
    let replacement = fs::metadata(&paths.rollup_cache).unwrap();
    assert_eq!(replacement.len(), original.len());
    assert_eq!(
        replacement.modified().unwrap(),
        original.modified().unwrap()
    );
    assert_eq!(generations(&paths), [8; 2]);
}

#[test]
fn rejects_both_older_and_newer_versions() {
    let (_dir, paths, mut cache) = fixture();
    for version in [ROLLUP_CACHE_VERSION - 1, ROLLUP_CACHE_VERSION + 1] {
        cache.version = version;
        write_rollup_cache(&paths, &cache).unwrap();
        assert_eq!(generations(&paths), [0; 2], "version {version}");
    }
}

#[test]
fn missing_empty_and_truncated_documents_return_zero() {
    let (_dir, paths, cache) = fixture();
    assert_eq!(generations(&paths), [0; 2]);
    for tail in ["", ",", r#", "raw_agents": ["unfinished"#] {
        let bytes = if tail.is_empty() {
            Vec::new()
        } else {
            format!(
                r#"{{"version":{},"extent":{{"generation":7,"offset":123}}{tail}"#,
                cache.version
            )
            .into_bytes()
        };
        fs::write(&paths.rollup_cache, bytes).unwrap();
        assert_eq!(generations(&paths), [0; 2], "tail {tail:?}");
    }
}

#[test]
fn trailing_data_is_rejected_but_whitespace_is_allowed() {
    let (_dir, paths, cache) = fixture();
    for (suffix, expected) in [("invalid", 0), ("{}", 0), (" \t\r", 7), ("\n", 7)] {
        let mut bytes = serde_json::to_vec(&cache).unwrap();
        bytes.extend_from_slice(suffix.as_bytes());
        fs::write(&paths.rollup_cache, bytes).unwrap();
        assert_eq!(generations(&paths), [expected; 2], "{suffix:?}");
    }
}

#[test]
fn malformed_ignored_fields_are_rejected() {
    let (_dir, paths, cache) = fixture();
    for value in ["\"raw\u{1}control\"", "invalid"] {
        let bytes = format!(
            r#"{{"version":{},"extent":{{"generation":7,"offset":123}},"raw_agents":{value}}}"#,
            cache.version
        );
        fs::write(&paths.rollup_cache, bytes).unwrap();
        assert_eq!(generations(&paths), [0; 2], "{value:?}");
    }
}

#[test]
fn multi_megabyte_ignored_payload_is_accepted() {
    let (_dir, paths, mut cache) = fixture();
    let mut row = agent("claude", "large", AgentStatus::Idle, 0);
    row.first_prompt = Some("x".repeat(3 * 1024 * 1024));
    cache.raw_agents.push(row);
    write_rollup_cache(&paths, &cache).unwrap();
    assert_eq!(generations(&paths), [7; 2]);
}

#[test]
fn rotation_exposes_the_next_generation() {
    let (_dir, paths, cache) = fixture();
    write_rollup_cache(&paths, &cache).unwrap();
    assert_eq!(generations(&paths), [7; 2]);
    reseed_rollup_cache_for_rotation(&paths).unwrap();
    assert_eq!(generations(&paths), [8; 2]);
}

#[cfg(target_os = "linux")]
fn stamp_path(paths: &StatePaths) -> PathBuf {
    paths.rollup_generation.clone()
}

#[test]
#[cfg(target_os = "linux")]
fn every_publication_is_stamped_and_hits_without_parsing() {
    let (_dir, paths, cache) = fixture();
    write_rollup_cache(&paths, &cache).unwrap();
    assert!(
        stamp_path(&paths).is_file(),
        "publish the generation mirror"
    );
    let full: RollupGenerationHeader =
        serde_json::from_slice(&fs::read(&paths.rollup_cache).unwrap()).unwrap();
    let before = testkit::generation_parse_count();
    assert_eq!(generations(&paths), [full.extent.generation; 2]);
    assert_eq!(testkit::generation_parse_count() - before, 0);

    let old: serde_json::Value =
        serde_json::from_slice(&fs::read(stamp_path(&paths)).unwrap()).unwrap();
    write_rollup_cache(&paths, &cache).unwrap();
    let stamp: serde_json::Value =
        serde_json::from_slice(&fs::read(stamp_path(&paths)).unwrap()).unwrap();
    let identity = FileStamp::from_metadata(&fs::metadata(&paths.rollup_cache).unwrap());
    assert_eq!(stamp["rollup"], serde_json::to_value(identity).unwrap());
    assert_ne!(old["rollup"], stamp["rollup"]);
    assert_eq!(generations(&paths), [7; 2]);
    assert_eq!(testkit::generation_parse_count() - before, 0);

    fs::remove_file(stamp_path(&paths)).unwrap();
    assert_eq!(generations(&paths), [7; 2]);
    assert_eq!(testkit::generation_parse_count() - before, 2);
}

#[test]
#[cfg(target_os = "linux")]
fn unstamped_replacements_including_equal_size_and_mtime_are_visible() {
    let (_dir, paths, mut cache) = fixture();
    write_rollup_cache(&paths, &cache).unwrap();
    assert!(
        stamp_path(&paths).is_file(),
        "publish the generation mirror"
    );
    let original = fs::metadata(&paths.rollup_cache).unwrap();
    let stamp = fs::read(stamp_path(&paths)).unwrap();
    // Each replaced rollup stays open so ext4 cannot hand its freed inode to the next
    // replacement: a recycled inode with forged size and mtime is the accepted stamp gap.
    let mut replaced = Vec::new();
    for generation in [8, 9] {
        replaced.push(fs::File::open(&paths.rollup_cache).unwrap());
        cache.extent.generation = generation;
        atomic::write_temp_then_rename_cache_compact(&paths.rollup_cache, &cache).unwrap();
        fs::File::open(&paths.rollup_cache)
            .unwrap()
            .set_modified(original.modified().unwrap())
            .unwrap();
        assert_eq!(
            fs::metadata(&paths.rollup_cache).unwrap().len(),
            original.len()
        );
        assert_eq!(generations(&paths), [generation; 2]);
        assert_eq!(fs::read(stamp_path(&paths)).unwrap(), stamp);
    }
    write_rollup_cache(&paths, &cache).unwrap();
    let before = testkit::generation_parse_count();
    assert_eq!(generations(&paths), [9; 2]);
    assert_eq!(testkit::generation_parse_count() - before, 0);
}

#[test]
#[cfg(target_os = "linux")]
fn missing_malformed_oversized_and_foreign_stamps_fall_back() {
    let (_dir, paths, cache) = fixture();
    write_rollup_cache(&paths, &cache).unwrap();
    let stamp = serde_json::json!({
        "version": ROLLUP_CACHE_VERSION,
        "generation": 99,
        "rollup": FileStamp::from_metadata(&fs::metadata(&paths.rollup_cache).unwrap()),
        "boot_id": crate::proc::boot_id().unwrap(),
    });
    let mut observed = Vec::new();
    for case in [
        "missing",
        "malformed",
        "oversized",
        "older",
        "newer",
        "foreign",
    ] {
        let mut stamp = stamp.clone();
        match case {
            "older" => stamp["version"] = (ROLLUP_CACHE_VERSION - 1).into(),
            "newer" => stamp["version"] = (ROLLUP_CACHE_VERSION + 1).into(),
            "foreign" => stamp["boot_id"] = "another-boot".into(),
            _ => {}
        }
        let mut bytes = serde_json::to_vec(&stamp).unwrap();
        match case {
            "missing" => {
                let _ = fs::remove_file(stamp_path(&paths));
            }
            "malformed" => fs::write(stamp_path(&paths), b"{").unwrap(),
            "oversized" => {
                bytes.resize(4096, b' ');
                fs::write(stamp_path(&paths), bytes).unwrap();
            }
            _ => fs::write(stamp_path(&paths), bytes).unwrap(),
        }
        observed.push(generations(&paths));
    }
    assert_eq!(observed, vec![[7; 2]; 6]);
    fs::write(stamp_path(&paths), serde_json::to_vec(&stamp).unwrap()).unwrap();
    fs::remove_file(&paths.rollup_cache).unwrap();
    assert_eq!(generations(&paths), [0; 2]);
}

#[test]
#[cfg(target_os = "linux")]
fn generation_reads_leave_every_cache_file_unchanged() {
    let (_dir, paths, cache) = fixture();
    write_rollup_cache(&paths, &cache).unwrap();
    assert!(
        stamp_path(&paths).is_file(),
        "publish the generation mirror"
    );
    let files = || {
        let mut files: Vec<_> = fs::read_dir(&paths.snapshots_dir)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let bytes = fs::read(&path).unwrap();
                let stamp = FileStamp::from_metadata(&fs::metadata(&path).unwrap());
                (path, bytes, stamp)
            })
            .collect();
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    };
    let before = files();
    assert_eq!(generations(&paths), [7; 2]);
    assert_eq!(files(), before);
}

#[test]
#[cfg(target_os = "linux")]
fn a_failed_stamp_publication_does_not_fail_the_rollup() {
    let (_dir, paths, cache) = fixture();
    fs::create_dir(stamp_path(&paths)).unwrap();
    assert!(write_rollup_cache(&paths, &cache).is_ok());
    assert_eq!(generations(&paths), [7; 2]);
}
