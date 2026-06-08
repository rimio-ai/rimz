//! The resumable event-log fold: the persisted rollup cache, its extent
//! stamp, and the carryover that survives log rotation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::project::{reduce_agent_states, reduce_agent_states_seeded};
use super::{Result, SnapshotErr};
use crate::agents::lifecycle::LifecycleSignal;
use crate::feed::AgentState;
use crate::ids::{AgentKind, AgentSessionId};
use crate::ledger::atomic::{self, write_temp_then_rename};
use crate::ledger::event_log::{self};
use crate::ledger::parse_cache::ParseCache;
use crate::ledger::paths::StatePaths;
use crate::schema::event::{EventEnvelope, EventKind};

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
    mut carryover_agents: Vec<AgentState>,
) -> Vec<AgentState> {
    // The carryover predates every event in the current log, so a rebirth
    // boundary anywhere in `events` postdates every carryover stamp — clear
    // them here, mirroring the in-order clear the seeded reducer applies to
    // within-log stamps (`reduce_agent_states_seeded`).
    if events
        .iter()
        .any(|event| matches!(event.kind(), EventKind::SessionRebirth))
    {
        for agent in &mut carryover_agents {
            agent.pane = None;
        }
    }
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
        if let EventKind::AgentLifecycle(payload) = event.kind() {
            let payload = *payload;
            if !matches!(payload.observation.signal, LifecycleSignal::Ended) {
                continue;
            }
            let Some(agent_id) = payload.observation.agent_id else {
                continue;
            };
            tombstones.insert((AgentKind::new_unchecked(event.source.clone()), agent_id));
        }
    }
    tombstones
}

/// Bump when [`RollupCache`]'s shape changes — a mismatched cache reads as
/// absent and cold-rebuilds.
const ROLLUP_CACHE_VERSION: u32 = 4;

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
    // log from the cached extent, so even a stale serve costs a larger fold,
    // never a wrong rollup (the [`ParseCache`] contract).
    if let Some(cache) = ROLLUP_PARSE_CACHE.with(|cache| cache.get(path, mtime, len)) {
        return Some(cache);
    }
    let bytes = fs::read(path).ok()?;
    let cache: RollupCache = serde_json::from_slice(&bytes).ok()?;
    let cache = (cache.version == ROLLUP_CACHE_VERSION).then_some(cache)?;
    ROLLUP_PARSE_CACHE.with(|slot| slot.store(path, mtime, len, cache.clone()));
    Some(cache)
}

thread_local! {
    /// This thread's last `rollup.json` parse — the fold base a long-lived
    /// reader re-reads on every catch-up fold.
    static ROLLUP_PARSE_CACHE: ParseCache<RollupCache> = const { ParseCache::new() };
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
    let base =
        read_rollup_cache(&paths.rollup_cache).filter(|cache| cache.extent.offset <= log_len);
    catch_up_from(base, paths)
}

/// The base-parameterized fold core every entry point shares: resume from
/// `base` (a fold cold from offset zero when `None`), fold only the frames
/// appended past it, and return the refreshed cache beside the
/// carryover-merged rollup. The disk-backed wrapper ([`catch_up_rollup`]) and
/// the in-memory [`RollupCursor`] both delegate here — one implementation, so
/// the keystone fold equivalence holds for every reader by construction.
/// `base.extent.offset` must not exceed the live log; callers filter.
fn catch_up_from(
    base: Option<RollupCache>,
    paths: &StatePaths,
) -> Result<(RollupCache, Vec<AgentState>)> {
    let (seed, mut tombstones, generation, start) = match base {
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
    let (delta, end) = event_log::read_from_offset(&paths.events_log, start)?;
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

/// A long-lived reader's in-memory fold base: the last [`RollupCache`] this
/// cursor folded to, the merged rollup it produced, and the identity of the
/// log file it folded. Where [`catch_up_rollup`] re-reads `rollup.json` per
/// call, a cursor folds each delta from memory — O(new bytes) per wakeup with
/// one `stat` of the log, no base parse — and an unchanged log returns the
/// held rollup without opening a file. One cursor per reader thread (the
/// sidebar fetch worker owns one across its loop); one-shot readers stay on
/// the disk-backed wrapper.
///
/// Staleness is structural, not best-effort: a swapped log file — rotation's
/// rename-and-recreate, the identity rewrite's rename-over — changes the
/// `(dev, ino)` identity captured by the same `stat`, and an offset past the
/// live length means the same, so either drops the in-memory base and
/// reloads `rollup.json`, whose generation rotation bumps unconditionally.
/// A warm fold that errors retries cold once before propagating, so a stale
/// base can never surface as corruption; an error out of the cold fold is
/// the same real corruption every reader reports.
#[derive(Debug, Default)]
pub struct RollupCursor {
    held: Option<CursorState>,
}

#[derive(Debug)]
struct CursorState {
    cache: RollupCache,
    merged: Vec<AgentState>,
    file_id: Option<LogFileId>,
}

/// Identity of the log file a cursor folded: device + inode on unix, so a
/// recreated or renamed-over log reads as a different file even when the
/// regrown log is longer than the held offset. Targets without that identity
/// get `None`, and the cursor reloads from `rollup.json` every fold — a
/// regrown swapped log would otherwise alias its bytes onto the stale base
/// (the offset-regression guard cannot see a *longer* new file, and a
/// frame-aligned offset folds cleanly rather than erroring), so the warm
/// fold is traded away there rather than served wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LogFileId {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl LogFileId {
    #[cfg(unix)]
    fn of(meta: &fs::Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        Some(Self {
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }

    #[cfg(not(unix))]
    fn of(_meta: &fs::Metadata) -> Option<Self> {
        None
    }
}

impl RollupCursor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Catch the held rollup up to the live log and return the extent it
    /// reflects beside the carryover-merged agents. The cursor twin of
    /// [`catch_up_rollup`]; see the type docs for the staleness guards.
    pub fn fold(&mut self, paths: &StatePaths) -> Result<(event_log::LogExtent, Vec<AgentState>)> {
        let meta = fs::metadata(&paths.events_log).ok();
        let file_id = meta.as_ref().and_then(LogFileId::of);
        let log_len = meta.map(|meta| meta.len()).unwrap_or(0);
        if let Some(held) = self.held.take() {
            // The held base serves warm only when the live log carries a real
            // identity that matches the one it folded; without one (a missing
            // log, a target with no dev+ino) the fold reloads `rollup.json`
            // rather than risk folding a swapped file's bytes onto it.
            let same_file = file_id.is_some() && held.file_id == file_id;
            if same_file && held.cache.extent.offset == log_len {
                // Nothing appended: serve the held fold without opening a file.
                let out = (held.cache.extent, held.merged.clone());
                self.held = Some(held);
                return Ok(out);
            }
            // A warm fold that errors falls through to the cold reload: if
            // the log is genuinely corrupt the cold fold errors the same way,
            // so nothing is masked — only a stale base heals.
            if same_file
                && held.cache.extent.offset < log_len
                && let Ok((cache, merged)) = catch_up_from(Some(held.cache), paths)
            {
                return Ok(self.hold(cache, merged, file_id));
            }
            // Identity changed or the offset regressed: the file underneath
            // was swapped, so the in-memory base describes a renamed-away log.
        }
        let (cache, merged) = catch_up_rollup(paths)?;
        Ok(self.hold(cache, merged, file_id))
    }

    fn hold(
        &mut self,
        cache: RollupCache,
        merged: Vec<AgentState>,
        file_id: Option<LogFileId>,
    ) -> (event_log::LogExtent, Vec<AgentState>) {
        let extent = cache.extent;
        self.held = Some(CursorState {
            cache,
            merged: merged.clone(),
            file_id,
        });
        (extent, merged)
    }
}

#[cfg(test)]
mod tests;
