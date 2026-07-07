use std::time::{Duration, SystemTime};

use super::*;

#[test]
fn write_rollup_cache_emits_compact_json_and_sweeps_stale_temp_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollup.json");
    let nonce = "00000000000000000000000000000000";
    let stale = dir.path().join(format!("rollup.json.tmp.1.{nonce}"));
    let other = dir.path().join(format!("other.json.tmp.1.{nonce}"));
    std::fs::write(&stale, b"stale").unwrap();
    std::fs::write(&other, b"other").unwrap();
    let old = SystemTime::now() - Duration::from_secs(3_700);
    std::fs::File::open(&stale)
        .unwrap()
        .set_modified(old)
        .unwrap();
    std::fs::File::open(&other)
        .unwrap()
        .set_modified(old)
        .unwrap();

    write_rollup_cache(
        &path,
        &RollupCache {
            version: ROLLUP_CACHE_VERSION,
            extent: event_log::LogExtent {
                generation: 0,
                offset: 10,
            },
            raw_agents: vec![agent("claude", "real", AgentStatus::Running, 1_000)],
            resume_outcomes: Vec::new(),
            agent_identity: AgentIdentityState::default(),
            saw_session_rebirth: false,
            tombstones: Vec::new(),
            lost: Vec::new(),
        },
    )
    .unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        contents.lines().count(),
        1,
        "rollup cache should be compact single-line JSON"
    );
    assert!(
        !stale.exists(),
        "rollup write should sweep stale temp siblings"
    );
    assert!(other.exists(), "sweep should not touch other cache temps");
}

#[test]
fn mismatched_rollup_cache_falls_back_to_the_cold_fold() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            "real",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();
    let full_len = std::fs::metadata(&paths.events_log).unwrap().len();

    let ghost_cache = |version: u32, offset: u64| RollupCache {
        version,
        extent: event_log::LogExtent {
            generation: 7,
            offset,
        },
        raw_agents: vec![agent("claude", "ghost", AgentStatus::Running, 0)],
        resume_outcomes: Vec::new(),
        agent_identity: AgentIdentityState::default(),
        saw_session_rebirth: false,
        tombstones: Vec::new(),
        lost: Vec::new(),
    };
    let assert_cold = |label: &str| {
        let (cache, agents, _) = catch_up_rollup(&paths).unwrap();
        assert!(
            agents.iter().any(|a| a.agent_id == "real"),
            "{label}: the cold fold reads the log"
        );
        assert!(
            agents.iter().all(|a| a.agent_id != "ghost"),
            "{label}: the unusable cache contributes nothing"
        );
        assert_eq!(
            cache.extent,
            event_log::LogExtent {
                generation: 0,
                offset: full_len,
            },
            "{label}: the refreshed base restarts at generation zero"
        );
    };

    // A shape from a different version reads as absent.
    write_rollup_cache(
        &paths.rollup_cache,
        &ghost_cache(ROLLUP_CACHE_VERSION + 1, 0),
    )
    .unwrap();
    assert_cold("version mismatch");

    // An extent past the live log is a rotation this cache predates.
    write_rollup_cache(
        &paths.rollup_cache,
        &ghost_cache(ROLLUP_CACHE_VERSION, full_len + 999),
    )
    .unwrap();
    assert_cold("extent past the log");
}

#[test]
fn rollup_parse_cache_hits_on_identity_and_misses_on_republish() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollup.json");
    let cache_with = |id: &str| RollupCache {
        version: ROLLUP_CACHE_VERSION,
        extent: event_log::LogExtent {
            generation: 0,
            offset: 10,
        },
        raw_agents: vec![agent("claude", id, AgentStatus::Running, 1_000)],
        resume_outcomes: Vec::new(),
        agent_identity: AgentIdentityState::default(),
        saw_session_rebirth: false,
        tombstones: Vec::new(),
        lost: Vec::new(),
    };
    write_rollup_cache(&path, &cache_with("aaaa")).unwrap();
    let first = read_rollup_cache(&path).unwrap();
    assert_eq!(first.raw_agents[0].agent_id, "aaaa");

    // Identical (path, mtime, len): rewrite the bytes in place at equal
    // length and restore the mtime — the thread's parse cache must serve the
    // prior parse, proving the deserialize was skipped.
    let meta = std::fs::metadata(&path).unwrap();
    let mtime = meta.modified().unwrap();
    let swapped = std::fs::read_to_string(&path)
        .unwrap()
        .replace("aaaa", "bbbb");
    std::fs::write(&path, swapped).unwrap();
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), meta.len());
    let hit = read_rollup_cache(&path).unwrap();
    assert_eq!(
        hit.raw_agents[0].agent_id, "aaaa",
        "identical identity serves the cached parse"
    );

    // A republish renames a fresh temp file over the path (new mtime), so the
    // identity changes and the read re-parses.
    write_rollup_cache(&path, &cache_with("cccc")).unwrap();
    let miss = read_rollup_cache(&path).unwrap();
    assert_eq!(
        miss.raw_agents[0].agent_id, "cccc",
        "a republish changes the identity; the read re-parses"
    );
}

#[test]
fn cursor_serves_the_held_fold_while_the_log_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            "real",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();

    let mut cursor = RollupCursor::new();
    let (first_extent, first, _) = cursor.fold(&paths).unwrap();

    // Plant a ghost base on disk. A warm cursor over an unchanged log serves
    // its held fold — it never re-reads `rollup.json`, so the ghost cannot
    // leak into the merge.
    write_rollup_cache(
        &paths.rollup_cache,
        &RollupCache {
            version: ROLLUP_CACHE_VERSION,
            extent: event_log::LogExtent {
                generation: 0,
                offset: 0,
            },
            raw_agents: vec![agent("claude", "ghost", AgentStatus::Running, 0)],
            resume_outcomes: Vec::new(),
            agent_identity: AgentIdentityState::default(),
            saw_session_rebirth: false,
            tombstones: Vec::new(),
            lost: Vec::new(),
        },
    )
    .unwrap();

    let (held_extent, held, _) = cursor.fold(&paths).unwrap();
    assert_eq!(held_extent, first_extent);
    assert_eq!(sorted_value(held.clone()), sorted_value(first));
    assert!(
        held.iter().all(|a| a.agent_id != "ghost"),
        "an unchanged log serves the in-memory base, not the disk base"
    );
}

#[test]
fn cached_rebirth_continues_to_reset_carryover_identity_after_extent() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut carried =
        agent("claude", "old-session", AgentStatus::Idle, 1_000).in_pane("terminal_1");
    carried.name = Some("lucid-atlas".to_owned());
    carried.kind_ordinal = Some(1);
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: vec![carried],
            agent_identity: AgentIdentityState::default(),
            resume_outcomes: Vec::new(),
            lost: Vec::new(),
        },
    )
    .unwrap();
    event_log::append(
        &paths.events_log,
        &EventEnvelope::session_rebirth(workspace.clone(), "session"),
    )
    .unwrap();
    let (rebirth_cache, first, _) = catch_up_rollup(&paths).unwrap();
    assert!(rebirth_cache.saw_session_rebirth);
    assert!(
        first
            .iter()
            .all(|agent| agent.pane.is_none() && agent.kind_ordinal.is_some()),
        "the rebirth fold clears stale pane stamps and backfills card identity"
    );
    write_rollup_cache(&paths.rollup_cache, &rebirth_cache).unwrap();

    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            "new-session",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();
    let (next_cache, next, _) = catch_up_rollup(&paths).unwrap();
    let old = next
        .iter()
        .find(|agent| agent.agent_id.as_str() == "old-session")
        .expect("carryover survivor");
    let new = next
        .iter()
        .find(|agent| agent.agent_id.as_str() == "new-session")
        .expect("fresh active-log agent");

    assert!(next_cache.saw_session_rebirth);
    assert!(old.pane.is_none());
    assert_ne!(old.kind_ordinal, new.kind_ordinal);
    assert!(old.kind_ordinal.is_some());
    assert!(new.kind_ordinal.is_some());
}
