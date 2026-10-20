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
    write_rollup_cache(&paths.rollup_cache, &cache).unwrap();
    assert_eq!(generations(&paths), [7; 2]);
}

#[test]
fn same_size_replacement_is_visible_with_the_same_mtime() {
    let (_dir, paths, mut cache) = fixture();
    write_rollup_cache(&paths.rollup_cache, &cache).unwrap();
    let original = fs::metadata(&paths.rollup_cache).unwrap();
    assert_eq!(generations(&paths), [7; 2]);

    cache.extent.generation = 8;
    write_rollup_cache(&paths.rollup_cache, &cache).unwrap();
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
        write_rollup_cache(&paths.rollup_cache, &cache).unwrap();
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
    write_rollup_cache(&paths.rollup_cache, &cache).unwrap();
    assert_eq!(generations(&paths), [7; 2]);
}

#[test]
fn rotation_exposes_the_next_generation() {
    let (_dir, paths, cache) = fixture();
    write_rollup_cache(&paths.rollup_cache, &cache).unwrap();
    assert_eq!(generations(&paths), [7; 2]);
    reseed_rollup_cache_for_rotation(&paths).unwrap();
    assert_eq!(generations(&paths), [8; 2]);
}
