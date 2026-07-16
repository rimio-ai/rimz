use super::*;

#[test]
fn catch_up_rollup_rejects_a_zeroed_middle_frame() {
    // Crash-sim for writeback reordering: an earlier frame's data pages
    // zeroed while a later frame survived. Reachable only when the
    // single-writer flock contract is broken (concurrent `O_APPEND`
    // appenders), so the fold must fail loudly rather than silently drop
    // the surviving frames behind the hole — this is the recovery
    // boundary the `O_APPEND` rejection in
    // `docs/internals/performance.md` leans on.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();

    let mut frame_ends = Vec::new();
    for agent_id in ["a", "b", "c"] {
        event_log::append(
            &paths.events_log,
            &lifecycle_at(
                &workspace,
                "claude",
                "SessionStart",
                agent_id,
                lifecycle::LifecycleSignal::Registered,
            ),
        )
        .unwrap();
        frame_ends.push(std::fs::metadata(&paths.events_log).unwrap().len());
    }

    // Zero the middle frame's bytes but keep its newline terminator, so
    // the log still frames three entries with an unreadable second one.
    use std::io::{Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&paths.events_log)
        .unwrap();
    file.seek(SeekFrom::Start(frame_ends[0])).unwrap();
    let hole = usize::try_from(frame_ends[1] - frame_ends[0] - 1).unwrap();
    file.write_all(&vec![0u8; hole]).unwrap();
    drop(file);

    let err = catch_up_rollup(&paths).expect_err("a zeroed middle frame must fail the fold");
    assert!(
        matches!(
            err,
            SnapshotErr::EventLog(
                event_log::EventLogErr::Torn { .. } | event_log::EventLogErr::FrameLength { .. }
            )
        ),
        "torn middle frame surfaces as a hard event-log error, never a silent drop: {err:?}"
    );
}

proptest::proptest! {
    /// The keystone fold invariant, generalized from the hand-built case
    /// above: over arbitrary lifecycle sequences and an arbitrary
    /// prefix/delta split, resuming from a persisted base equals folding
    /// the whole log cold — agents and extent alike.
    #[test]
    fn fold_seed_delta_equals_fold_empty_all_over_arbitrary_sequences(
        seq in proptest::collection::vec((0usize..2, 0usize..4, 0usize..3), 1..12),
        split in proptest::prelude::any::<proptest::sample::Index>(),
    ) {
        use proptest::prelude::prop_assert_eq;

        const KINDS: [&str; 2] = ["claude", "codex"];
        const EVENTS: [&str; 4] = ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"];
        const AGENTS: [&str; 3] = ["a", "b", "c"];

        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();

        let events: Vec<_> = seq
            .iter()
            .map(|&(kind, event, agent_id)| {
                // The explicit signal each event name stamps in production.
                let signal = match event {
                    1 => lifecycle::LifecycleSignal::TurnStarted,
                    2 => lifecycle::LifecycleSignal::TurnEnded {
                        errored: false,
                        parked_on_background: false,
                    },
                    3 => lifecycle::LifecycleSignal::Ended,
                    _ => lifecycle::LifecycleSignal::Registered,
                };
                lifecycle_at(&workspace, KINDS[kind], EVENTS[event], AGENTS[agent_id], signal)
            })
            .collect();
        let at = split.index(events.len() + 1);

        for event in &events[..at] {
            event_log::append(&paths.events_log, event).unwrap();
        }
        let (prefix_cache, _, _) = catch_up_rollup(&paths).unwrap();
        write_rollup_cache(&paths.rollup_cache, &prefix_cache).unwrap();
        let mut cursor = RollupCursor::new();
        cursor.fold(&paths).unwrap();
        for event in &events[at..] {
            event_log::append(&paths.events_log, event).unwrap();
        }
        let (incremental_cache, incremental, _) = catch_up_rollup(&paths).unwrap();
        let (cursor_extent, cursor_merged, _) = cursor.fold(&paths).unwrap();

        std::fs::remove_file(&paths.rollup_cache).unwrap();
        let (cold_cache, cold, _) = catch_up_rollup(&paths).unwrap();

        prop_assert_eq!(
            sorted_value(incremental),
            sorted_value(cold.clone()),
            "fold(seed, delta) == fold(empty, all)"
        );
        prop_assert_eq!(
            sorted_value(cursor_merged),
            sorted_value(cold),
            "the warm in-memory cursor fold equals the cold fold"
        );
        prop_assert_eq!(cursor_extent, cold_cache.extent);
        prop_assert_eq!(
            sorted_value(incremental_cache.raw_agents),
            sorted_value(cold_cache.raw_agents)
        );
        prop_assert_eq!(incremental_cache.extent, cold_cache.extent);
    }
}

#[test]
fn rebirth_boundary_unstamps_carryover_agents() {
    // The carryover predates every event in the current log, so a rebirth
    // boundary anywhere in the window clears its stamps too — a rotated-out
    // session can no more own a reborn pane id than an in-log one.
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let carried = agent("codex", "sess-old", AgentStatus::Success, 1_000).in_pane("terminal_6");
    let boundary = EventEnvelope::session_rebirth(workspace, "session");

    let merged = agent_rollup_with_carryover(&[boundary], vec![carried.clone()]);
    assert_eq!(
        merged.len(),
        1,
        "the boundary unstamps, it never ends a session"
    );
    assert!(
        merged[0].pane.is_none(),
        "a carryover stamp predates the boundary and clears"
    );

    let untouched = agent_rollup_with_carryover(&[], vec![carried]);
    assert!(
        untouched[0].pane.is_some(),
        "no boundary in the window keeps the carryover stamp"
    );
}
