use super::*;

#[test]
fn catch_up_rollup_equals_the_full_fold() {
    // The correctness keystone for incremental folding: resuming from a
    // persisted prefix base and folding only the delta must equal folding
    // the whole log from scratch — including a tombstone inside the delta.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();

    // Prefix: two agents come up.
    for event in [
        lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            "a",
            lifecycle::LifecycleSignal::Registered,
        ),
        lifecycle_at(
            &workspace,
            "claude",
            "UserPromptSubmit",
            "a",
            lifecycle::LifecycleSignal::TurnStarted,
        ),
        lifecycle_at(
            &workspace,
            "codex",
            "SessionStart",
            "b",
            lifecycle::LifecycleSignal::Registered,
        ),
    ] {
        event_log::append(&paths.events_log, &event).unwrap();
    }
    // Persist the fold base at the prefix — the seed the delta resumes from.
    let (prefix_cache, _) = catch_up_rollup(&paths).unwrap();
    write_rollup_cache(&paths.rollup_cache, &prefix_cache).unwrap();
    // A cursor warms its in-memory base at the same prefix point.
    let mut cursor = RollupCursor::new();
    cursor.fold(&paths).unwrap();

    // Delta: a third agent appears, `a` stops, and `b`'s session ends —
    // a tombstone the incremental fold must carry exactly like the full one.
    for event in [
        lifecycle_at(
            &workspace,
            "claude",
            "SessionStart",
            "c",
            lifecycle::LifecycleSignal::Registered,
        ),
        lifecycle_at(
            &workspace,
            "claude",
            "Stop",
            "a",
            lifecycle::LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        ),
        lifecycle_at(
            &workspace,
            "codex",
            "SessionEnd",
            "b",
            lifecycle::LifecycleSignal::Ended,
        ),
    ] {
        event_log::append(&paths.events_log, &event).unwrap();
    }
    let (incremental_cache, incremental) = catch_up_rollup(&paths).unwrap();
    let (cursor_extent, cursor_merged) = cursor.fold(&paths).unwrap();

    // Cold path: drop the base; the same call folds the whole log from zero.
    std::fs::remove_file(&paths.rollup_cache).unwrap();
    let (cold_cache, cold) = catch_up_rollup(&paths).unwrap();

    assert_eq!(
        sorted_value(incremental),
        sorted_value(cold.clone()),
        "fold(seed, delta) == fold(empty, all)"
    );
    assert_eq!(
        sorted_value(cursor_merged),
        sorted_value(cold),
        "the warm in-memory cursor fold equals the cold fold too"
    );
    assert_eq!(cursor_extent, cold_cache.extent);
    assert_eq!(
        sorted_value(incremental_cache.raw_agents),
        sorted_value(cold_cache.raw_agents),
        "the refreshed fold bases agree too"
    );
    assert_eq!(incremental_cache.extent, cold_cache.extent);
    let full_len = std::fs::metadata(&paths.events_log).unwrap().len();
    assert_eq!(
        incremental_cache.extent,
        event_log::LogExtent {
            generation: 0,
            offset: full_len,
        },
        "the extent claims exactly the folded bytes"
    );
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
