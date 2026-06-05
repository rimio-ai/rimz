//! The resumable event-log fold: the persisted rollup cache, its extent
//! stamp, and the carryover that survives log rotation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let len = meta.len();
    // Only the *parse* is cached; every caller still folds against the live
    // log. The base is byte-stable between checkpoint publishes, so a
    // long-lived reader that races the checkpoint (`build_from` per delta)
    // skips the 100–500 KB deserialize when the file is byte-identical to this
    // thread's last read. Same (path, mtime, len) identity trade-off as the
    // `latest.json` parse cache; an atomic-rename republish changes both
    // mtime and len, so a stale parse cannot be served.
    let cached = ROLLUP_PARSE_CACHE.with_borrow(|slot| {
        slot.as_ref().and_then(|entry| {
            (entry.path == path && entry.mtime == mtime && entry.len == len)
                .then(|| entry.cache.clone())
        })
    });
    if let Some(cache) = cached {
        return Some(cache);
    }
    let bytes = fs::read(path).ok()?;
    let cache: RollupCache = serde_json::from_slice(&bytes).ok()?;
    let cache = (cache.version == ROLLUP_CACHE_VERSION).then_some(cache)?;
    ROLLUP_PARSE_CACHE.with_borrow_mut(|slot| {
        *slot = Some(ParsedRollup {
            path: path.to_path_buf(),
            mtime,
            len,
            cache: cache.clone(),
        });
    });
    Some(cache)
}

/// One thread's last parse of `rollup.json`, keyed by path + identity (mtime,
/// len) — the fold-base twin of `assemble`'s `latest.json` parse cache, for
/// the base a long-lived reader re-reads on every catch-up fold.
struct ParsedRollup {
    path: PathBuf,
    mtime: SystemTime,
    len: u64,
    cache: RollupCache,
}

thread_local! {
    static ROLLUP_PARSE_CACHE: std::cell::RefCell<Option<ParsedRollup>> =
        const { std::cell::RefCell::new(None) };
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
mod tests;
