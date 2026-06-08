use std::path::Path;

use super::*;

use crate::agents::lifecycle;
use crate::feed::AgentStatus;
use crate::ids::WorkspaceId;
use crate::ledger::snapshot::testkit::*;

/// Test-local shorthand over [`merge_agent_rollups_with_tombstones`]
/// with no tombstones in play.
fn merge_agent_rollups(base: &[AgentState], live: &[AgentState]) -> Vec<AgentState> {
    merge_agent_rollups_with_tombstones(base, live, &BTreeSet::new())
}

#[test]
fn merge_carryover_prefers_newer_observation() {
    let mut older = agent("claude", "agent-1", AgentStatus::Idle, 1_000);
    older.worktree_branch = Some("main".into());
    let mut newer = agent("claude", "agent-1", AgentStatus::Running, 2_000);
    newer.worktree_branch = Some("feature".into());
    let merged = merge_agent_rollups(std::slice::from_ref(&older), std::slice::from_ref(&newer));
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].status, AgentStatus::Running);
    assert_eq!(merged[0].worktree_branch.as_deref(), Some("feature"));
}

#[test]
fn merge_carryover_preserves_orphaned_entries() {
    let only_in_carryover = agent("claude", "agent-1", AgentStatus::Idle, 1_000);
    let only_live = agent("codex", "agent-2", AgentStatus::Running, 2_000);
    let merged = merge_agent_rollups(
        std::slice::from_ref(&only_in_carryover),
        std::slice::from_ref(&only_live),
    );
    assert_eq!(merged.len(), 2);
}

#[test]
fn carryover_session_end_tombstones_older_agent_state() {
    let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
    let carried = agent("claude", "agent-1", AgentStatus::Idle, 1_000);
    let ended = lifecycle_at(
        &workspace,
        "claude",
        "SessionEnd",
        "agent-1",
        lifecycle::LifecycleSignal::Ended,
    );

    let merged = agent_rollup_with_carryover(&[ended], vec![carried]);

    assert!(
        merged.is_empty(),
        "active-log SessionEnd must tombstone older carryover state"
    );
}

#[test]
fn carryover_round_trips_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agents.carryover.json");
    assert_eq!(
        read_carryover(&path).unwrap(),
        EventCarryover::default(),
        "missing file yields empty carryover"
    );

    let carryover = EventCarryover {
        agents: vec![{
            let mut agent = agent("claude", "agent-1", AgentStatus::Success, 3_000);
            agent.worktree_branch = Some("main".into());
            agent
        }],
    };
    write_carryover(&path, &carryover).unwrap();
    let loaded = read_carryover(&path).unwrap();
    assert_eq!(loaded, carryover);
}

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
        tombstones: Vec::new(),
    };
    let assert_cold = |label: &str| {
        let (cache, agents) = catch_up_rollup(&paths).unwrap();
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
        tombstones: Vec::new(),
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
    let (first_extent, first) = cursor.fold(&paths).unwrap();

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
            tombstones: Vec::new(),
        },
    )
    .unwrap();

    let (held_extent, held) = cursor.fold(&paths).unwrap();
    assert_eq!(held_extent, first_extent);
    assert_eq!(sorted_value(held.clone()), sorted_value(first));
    assert!(
        held.iter().all(|a| a.agent_id != "ghost"),
        "an unchanged log serves the in-memory base, not the disk base"
    );
}

#[test]
fn cursor_reloads_across_a_rotation() {
    // Rotation renames the log away and recreates it; a regrown log can pass
    // the held offset, so the cursor's reload guard is the file identity, not
    // the length. After the swap the cursor must drop its in-memory base and
    // reload `rollup.json`, whose bumped generation rotation reseeded.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let lifecycle = |kind: &str, id: &str| {
        lifecycle_at(
            &workspace,
            kind,
            "SessionStart",
            id,
            lifecycle::LifecycleSignal::Registered,
        )
    };
    event_log::append(&paths.events_log, &lifecycle("claude", "a")).unwrap();

    let mut cursor = RollupCursor::new();
    let (warm_extent, _) = cursor.fold(&paths).unwrap();
    assert_eq!(warm_extent.generation, 0);

    // Rotate: carryover the rollup, swap the log file, reseed the base —
    // then regrow the new log *past* the held offset, the case a
    // length-only guard would misread as appended frames.
    let (cache, _) = catch_up_rollup(&paths).unwrap();
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: cache.raw_agents.clone(),
        },
    )
    .unwrap();
    std::fs::remove_file(&paths.events_log).unwrap();
    reseed_rollup_cache_for_rotation(&paths).unwrap();
    event_log::append(&paths.events_log, &lifecycle("codex", "b")).unwrap();
    event_log::append(&paths.events_log, &lifecycle("codex", "c")).unwrap();
    assert!(
        std::fs::metadata(&paths.events_log).unwrap().len() > warm_extent.offset,
        "the regrown log must outgrow the held offset for this test to bite"
    );

    let (extent, merged) = cursor.fold(&paths).unwrap();
    std::fs::remove_file(&paths.rollup_cache).unwrap();
    let (cold_cache, cold) = catch_up_rollup(&paths).unwrap();
    assert_eq!(extent.generation, 1, "the reloaded base carries the bump");
    assert_eq!(extent.offset, cold_cache.extent.offset);
    assert_eq!(
        sorted_value(merged),
        sorted_value(cold),
        "the post-rotation cursor fold equals the cold fold"
    );
}

#[test]
fn cursor_reloads_on_an_offset_regression() {
    // Same file identity, shorter log — a truncation the cursor's held offset
    // now overruns. The reload path falls back to the cold fold (the planted
    // base is past the log too) and the cursor re-folds what remains.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let mut frame_ends = Vec::new();
    for id in ["a", "b"] {
        event_log::append(
            &paths.events_log,
            &lifecycle_at(
                &workspace,
                "claude",
                "SessionStart",
                id,
                lifecycle::LifecycleSignal::Registered,
            ),
        )
        .unwrap();
        frame_ends.push(std::fs::metadata(&paths.events_log).unwrap().len());
    }

    let mut cursor = RollupCursor::new();
    let (warm_extent, _) = cursor.fold(&paths).unwrap();
    assert_eq!(warm_extent.offset, frame_ends[1]);

    // Truncate in place: identity unchanged, length regressed.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&paths.events_log)
        .unwrap()
        .set_len(frame_ends[0])
        .unwrap();

    let (extent, merged) = cursor.fold(&paths).unwrap();
    assert_eq!(extent.offset, frame_ends[0]);
    let ids: Vec<&str> = merged.iter().map(|a| a.agent_id.as_str()).collect();
    assert_eq!(
        ids,
        ["a"],
        "the regressed fold reflects only the surviving frames"
    );
}

#[test]
fn reseed_for_rotation_bumps_generation_and_starts_an_empty_fold() {
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
            "a",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();
    let (cache, _) = catch_up_rollup(&paths).unwrap();
    write_rollup_cache(&paths.rollup_cache, &cache).unwrap();
    assert_eq!(cache.extent.generation, 0);
    assert!(cache.extent.offset > 0);

    // Rotation: the old log's rollup moves into the carryover, the active
    // log is renamed away, and the fold base reseeds for the new generation.
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents: cache.raw_agents.clone(),
        },
    )
    .unwrap();
    std::fs::remove_file(&paths.events_log).unwrap();
    reseed_rollup_cache_for_rotation(&paths).unwrap();

    let (fresh, agents) = catch_up_rollup(&paths).unwrap();
    assert_eq!(
        fresh.extent,
        event_log::LogExtent {
            generation: 1,
            offset: 0,
        },
        "the new generation starts with an empty fold at offset zero"
    );
    assert!(fresh.raw_agents.is_empty());
    assert!(
        agents.iter().any(|a| a.agent_id == "a"),
        "the pre-rotation agent survives via the carryover merge"
    );

    // Appends to the fresh log fold under the bumped generation.
    event_log::append(
        &paths.events_log,
        &lifecycle_at(
            &workspace,
            "codex",
            "SessionStart",
            "b",
            lifecycle::LifecycleSignal::Registered,
        ),
    )
    .unwrap();
    let (next, agents) = catch_up_rollup(&paths).unwrap();
    assert_eq!(next.extent.generation, 1);
    assert!(next.extent.offset > 0);
    let ids: Vec<&str> = {
        let mut ids: Vec<&str> = agents.iter().map(|a| a.agent_id.as_str()).collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(ids, ["a", "b"], "carryover and fresh-log agents merge");
}

#[test]
fn merge_carryover_prefers_live_on_a_last_seen_tie() {
    // The tie rule the strictly-newer guard implies: a base observation
    // survives only when *strictly* newer, so on an equal `last_seen` the
    // live in-log entry wins and a rotation boundary can never freeze a
    // stale carryover field forever.
    let mut carried = agent("claude", "agent-1", AgentStatus::Idle, 2_000);
    carried.worktree_branch = Some("main".into());
    let mut live = agent("claude", "agent-1", AgentStatus::Running, 2_000);
    live.worktree_branch = Some("feature".into());

    let merged = merge_agent_rollups(std::slice::from_ref(&carried), std::slice::from_ref(&live));

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].status, AgentStatus::Running, "live wins the tie");
    assert_eq!(merged[0].worktree_branch.as_deref(), Some("feature"));
}

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
    /// the whole log cold — agents, tombstones, and extent alike.
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
        let (prefix_cache, _) = catch_up_rollup(&paths).unwrap();
        write_rollup_cache(&paths.rollup_cache, &prefix_cache).unwrap();
        let mut cursor = RollupCursor::new();
        cursor.fold(&paths).unwrap();
        for event in &events[at..] {
            event_log::append(&paths.events_log, event).unwrap();
        }
        let (incremental_cache, incremental) = catch_up_rollup(&paths).unwrap();
        let (cursor_extent, cursor_merged) = cursor.fold(&paths).unwrap();

        std::fs::remove_file(&paths.rollup_cache).unwrap();
        let (cold_cache, cold) = catch_up_rollup(&paths).unwrap();

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
        prop_assert_eq!(
            incremental_cache
                .tombstones
                .iter()
                .collect::<std::collections::BTreeSet<_>>(),
            cold_cache
                .tombstones
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
        );
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
        "the boundary unstamps, it never tombstones"
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
