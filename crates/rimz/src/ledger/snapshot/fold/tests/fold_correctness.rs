use super::*;

#[test]
fn catch_up_rollup_equals_the_full_fold() {
    // The correctness keystone for incremental folding: resuming from a
    // persisted prefix base and folding only the delta must equal folding
    // the whole log from scratch — including a tombstone inside the delta.
    let (dir, workspace, paths) = fold_fixture();

    seed_prefix(&paths, &workspace);
    // Persist the fold base at the prefix — the seed the delta resumes from.
    let (prefix_cache, _) = catch_up_rollup(&paths).unwrap();
    write_rollup_cache(&paths.rollup_cache, &prefix_cache).unwrap();
    // A cursor warms its in-memory base at the same prefix point.
    let mut cursor = RollupCursor::new();
    cursor.fold(&paths).unwrap();

    append_delta(&paths, &workspace);
    assert_incremental_fold_matches_cold(&paths, cursor);
    drop(dir);
}

fn fold_fixture() -> (tempfile::TempDir, WorkspaceId, StatePaths) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    (dir, workspace, paths)
}

fn seed_prefix(paths: &StatePaths, workspace: &WorkspaceId) {
    for event in [
        lifecycle_at(
            workspace,
            "claude",
            "SessionStart",
            "a",
            lifecycle::LifecycleSignal::Registered,
        ),
        lifecycle_at(
            workspace,
            "claude",
            "UserPromptSubmit",
            "a",
            lifecycle::LifecycleSignal::TurnStarted,
        ),
        lifecycle_at(
            workspace,
            "codex",
            "SessionStart",
            "b",
            lifecycle::LifecycleSignal::Registered,
        ),
    ] {
        event_log::append(&paths.events_log, &event).unwrap();
    }
}

fn append_delta(paths: &StatePaths, workspace: &WorkspaceId) {
    for event in [
        lifecycle_at(
            workspace,
            "claude",
            "SessionStart",
            "c",
            lifecycle::LifecycleSignal::Registered,
        ),
        lifecycle_at(
            workspace,
            "claude",
            "Stop",
            "a",
            lifecycle::LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        ),
        lifecycle_at(
            workspace,
            "codex",
            "SessionEnd",
            "b",
            lifecycle::LifecycleSignal::Ended,
        ),
    ] {
        event_log::append(&paths.events_log, &event).unwrap();
    }
}

fn assert_incremental_fold_matches_cold(paths: &StatePaths, mut cursor: RollupCursor) {
    let (incremental_cache, incremental) = catch_up_rollup(paths).unwrap();
    let (cursor_extent, cursor_merged) = cursor.fold(paths).unwrap();

    std::fs::remove_file(&paths.rollup_cache).unwrap();
    let (cold_cache, cold) = catch_up_rollup(paths).unwrap();

    assert_eq!(sorted_value(incremental), sorted_value(cold.clone()));
    assert_eq!(sorted_value(cursor_merged), sorted_value(cold));
    assert_eq!(cursor_extent, cold_cache.extent);
    assert_eq!(
        sorted_value(incremental_cache.raw_agents.clone()),
        sorted_value(cold_cache.raw_agents.clone())
    );
    assert_eq!(incremental_cache.extent, cold_cache.extent);
    assert_extent_claims_full_log(paths, &incremental_cache);
    assert_eq!(
        incremental_cache
            .tombstones
            .iter()
            .collect::<std::collections::BTreeSet<_>>(),
        cold_cache
            .tombstones
            .iter()
            .collect::<std::collections::BTreeSet<_>>(),
    );
}

fn assert_extent_claims_full_log(paths: &StatePaths, cache: &RollupCache) {
    let full_len = std::fs::metadata(&paths.events_log).unwrap().len();
    assert_eq!(
        cache.extent,
        event_log::LogExtent {
            generation: 0,
            offset: full_len,
        }
    );
}
