//! The resumable event-log fold: the persisted rollup cache, its extent
//! stamp, and the carryover that survives log rotation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::project::{reduce_agent_states, reduce_agent_states_seeded};
use super::{Result, SnapshotErr};
use crate::agents::lifecycle;
use crate::feed::AgentState;
use crate::ids::{AgentKind, AgentSessionId};
use crate::ledger::atomic::{self, write_temp_then_rename};
use crate::ledger::event_log::{self};
use crate::ledger::paths::StatePaths;
use crate::schema::event::EventEnvelope;

/// Carryover state preserved across event-log rotation. Today this is the
/// agent rollup; other reductions can join when they appear.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct EventCarryover {
    #[serde(default)]
    pub agents: Vec<AgentState>,
}

pub(crate) fn read_carryover(path: &Path) -> Result<EventCarryover> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| SnapshotErr::Json {
            path: path.to_path_buf(),
            source,
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(EventCarryover::default()),
        Err(source) => Err(SnapshotErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[must_use = "durability barrier; check the result"]
pub(crate) fn write_carryover(path: &Path, carryover: &EventCarryover) -> Result<()> {
    write_temp_then_rename(path, carryover)?;
    Ok(())
}

pub(crate) fn agent_rollup_with_carryover(
    events: &[EventEnvelope],
    carryover_agents: Vec<AgentState>,
) -> Vec<AgentState> {
    let live = reduce_agent_states(events);
    let tombstones = agent_tombstones_for_events(events);
    merge_agent_rollups_with_tombstones(&carryover_agents, &live, &tombstones)
}

pub(super) fn merge_agent_rollups_with_tombstones(
    base: &[AgentState],
    live: &[AgentState],
    tombstones: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Vec<AgentState> {
    let mut map: BTreeMap<(AgentKind, AgentSessionId), AgentState> = BTreeMap::new();
    for entry in base {
        let key = (entry.kind.clone(), entry.agent_id.clone());
        if !tombstones.contains(&key) {
            map.insert(key, entry.clone());
        }
    }
    for entry in live {
        let key = (entry.kind.clone(), entry.agent_id.clone());
        match map.get(&key) {
            Some(existing) if existing.last_seen > entry.last_seen => {}
            _ => {
                map.insert(key, entry.clone());
            }
        }
    }
    map.into_values().collect()
}

/// The `(kind, agent_id)` set whose sessions ended in `events` — an `Ended`
/// lifecycle signal. Exposed so resume-on-rebirth can drop a cleanly-ended
/// agent from the audit rollup (which, unlike the carryover merge, keeps a
/// within-log `SessionEnd` row), never re-spawning a session the user closed.
pub fn agent_tombstones_for_events(
    events: &[EventEnvelope],
) -> BTreeSet<(AgentKind, AgentSessionId)> {
    let mut tombstones = BTreeSet::new();
    for event in events {
        if event.method != "agent.lifecycle" {
            continue;
        }
        if !matches!(
            lifecycle::signal_from_event_params(&event.params),
            Some(lifecycle::LifecycleSignal::Ended)
        ) {
            continue;
        }
        let Some(agent_id) = event
            .params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(AgentSessionId::from)
        else {
            continue;
        };
        tombstones.insert((AgentKind::new_unchecked(event.source.clone()), agent_id));
    }
    tombstones
}

/// Bump when [`RollupCache`]'s shape changes — a mismatched cache reads as
/// absent and cold-rebuilds.
const ROLLUP_CACHE_VERSION: u32 = 2;

/// The resumable agent-rollup fold base persisted in `snapshots/rollup.json`:
/// the raw pre-projection fold map and this generation's tombstones, stamped
/// with the log extent folded so far. Cache-class — reconstructible from the
/// event log and the carryover at any time, so it renames atomically without
/// fsync and any read failure falls back to the full fold.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RollupCache {
    pub version: u32,
    pub extent: event_log::LogExtent,
    pub raw_agents: Vec<AgentState>,
    pub tombstones: Vec<(AgentKind, AgentSessionId)>,
}

fn read_rollup_cache(path: &Path) -> Option<RollupCache> {
    let bytes = fs::read(path).ok()?;
    let cache: RollupCache = serde_json::from_slice(&bytes).ok()?;
    (cache.version == ROLLUP_CACHE_VERSION).then_some(cache)
}

#[must_use = "atomicity barrier; check the result"]
pub(super) fn write_rollup_cache(path: &Path, cache: &RollupCache) -> Result<()> {
    atomic::write_temp_then_rename_cache(path, cache)?;
    Ok(())
}

/// Catch the rollup up to the live log: resume the fold from
/// `snapshots/rollup.json`, fold only the frames appended since its extent,
/// and return the refreshed cache beside the carryover-merged rollup.
///
/// O(delta bytes) on the common path. Any miss — an absent or
/// shape-mismatched cache, or an extent past the live log (a rotation this
/// cache predates) — falls back to the full fold from offset zero, the
/// universal recovery path. Read-only: the caller that owns a write
/// serialization point (a locked rebuild, the single-flighted publisher)
/// persists the returned cache; a plain reader just uses it.
pub(crate) fn catch_up_rollup(paths: &StatePaths) -> Result<(RollupCache, Vec<AgentState>)> {
    let log_len = fs::metadata(&paths.events_log)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let cache =
        read_rollup_cache(&paths.rollup_cache).filter(|cache| cache.extent.offset <= log_len);
    let (seed, mut tombstones, generation, base) = match cache {
        Some(RollupCache {
            extent,
            raw_agents,
            tombstones,
            ..
        }) => {
            let seed: BTreeMap<(AgentKind, AgentSessionId), AgentState> = raw_agents
                .into_iter()
                .map(|agent| ((agent.kind.clone(), agent.agent_id.clone()), agent))
                .collect();
            let tombstones: BTreeSet<(AgentKind, AgentSessionId)> =
                tombstones.into_iter().collect();
            (seed, tombstones, extent.generation, extent.offset)
        }
        None => (BTreeMap::new(), BTreeSet::new(), 0, 0),
    };
    let (delta, end) = event_log::read_from_offset(&paths.events_log, base)?;
    let map = reduce_agent_states_seeded(seed, &delta);
    tombstones.extend(agent_tombstones_for_events(&delta));
    let raw_agents: Vec<AgentState> = map.into_values().collect();
    let carryover = read_carryover(&paths.agents_carryover)?;
    let merged = merge_agent_rollups_with_tombstones(&carryover.agents, &raw_agents, &tombstones);
    let refreshed = RollupCache {
        version: ROLLUP_CACHE_VERSION,
        extent: event_log::LogExtent {
            generation,
            offset: end,
        },
        raw_agents,
        tombstones: tombstones.into_iter().collect(),
    };
    Ok((refreshed, merged))
}

/// Reseed `snapshots/rollup.json` for the next log generation. Called by
/// rotation under the workspace lock, right after the old log's rollup is
/// merged into the carryover: the new generation starts with an empty fold
/// at offset zero, and the bumped generation keeps any in-flight reader's
/// pre-rotation extent from aliasing the fresh log.
#[must_use = "atomicity barrier; check the result"]
pub(crate) fn reseed_rollup_cache_for_rotation(paths: &StatePaths) -> Result<()> {
    let generation = read_rollup_cache(&paths.rollup_cache)
        .map(|cache| cache.extent.generation)
        .unwrap_or(0);
    write_rollup_cache(
        &paths.rollup_cache,
        &RollupCache {
            version: ROLLUP_CACHE_VERSION,
            extent: event_log::LogExtent {
                generation: generation + 1,
                offset: 0,
            },
            raw_agents: Vec::new(),
            tombstones: Vec::new(),
        },
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

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
        let merged =
            merge_agent_rollups(std::slice::from_ref(&older), std::slice::from_ref(&newer));
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
        let ended = EventEnvelope::new(
            workspace,
            "session",
            "claude",
            "agent-hook",
            "agent.lifecycle",
            serde_json::json!({
                "event_name": "SessionEnd",
                "agent_id": "agent-1",
                "signal": { "signal": "ended" },
            }),
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

        // Cold path: drop the base; the same call folds the whole log from zero.
        std::fs::remove_file(&paths.rollup_cache).unwrap();
        let (cold_cache, cold) = catch_up_rollup(&paths).unwrap();

        assert_eq!(
            sorted_value(incremental),
            sorted_value(cold),
            "fold(seed, delta) == fold(empty, all)"
        );
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

        let merged =
            merge_agent_rollups(std::slice::from_ref(&carried), std::slice::from_ref(&live));

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
        file.sync_all().unwrap();
        drop(file);

        let err = catch_up_rollup(&paths).expect_err("a zeroed middle frame must fail the fold");
        assert!(
            matches!(
                err,
                SnapshotErr::EventLog(
                    event_log::EventLogErr::Torn { .. }
                        | event_log::EventLogErr::FrameLength { .. }
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
            for event in &events[at..] {
                event_log::append(&paths.events_log, event).unwrap();
            }
            let (incremental_cache, incremental) = catch_up_rollup(&paths).unwrap();

            std::fs::remove_file(&paths.rollup_cache).unwrap();
            let (cold_cache, cold) = catch_up_rollup(&paths).unwrap();

            prop_assert_eq!(
                sorted_value(incremental),
                sorted_value(cold),
                "fold(seed, delta) == fold(empty, all)"
            );
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
}
