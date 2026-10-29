//! The resumable event-log fold: the persisted rollup cache, its extent
//! stamp, and the carryover that survives log rotation.

use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::project::{
    AgentIdentityState, FoldEvent, agent_event_key, agent_mut, backfill_agent_identities,
    compact_command_agent_key, decode_events, reduce_agent_states_seeded_with_identity,
    unstamp_for_rebirth,
};
use super::{Result, SnapshotErr};
use crate::agents::AgentState;
use crate::disk::atomic::{self, write_temp_then_rename};
use crate::disk::parse_cache::{FileStamp, ParseCache, StampedPath};
use crate::disk::paths::StatePaths;
use crate::disk::retention::RESUME_OUTCOME_RETENTION_SECS;
use crate::ids::{AgentKind, AgentSessionId, MessageId};
use crate::store::event::EventEnvelope;
use crate::store::event::EventKind;
use crate::store::event_log::{self};
use crate::store::message::{DeliveryGate, MessageBody, MessageStatus};
use crate::store::runtime;

/// Carryover state preserved across event-log rotation. Today this is the
/// agent rollup and terminal resume-message outcomes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct EventCarryover {
    #[serde(default)]
    agents: Vec<AgentState>,
    #[serde(default)]
    agent_identity: AgentIdentityState,
    #[serde(default)]
    resume_outcomes: Vec<ResumeOutcome>,
}

/// One terminal resume-gated prompt outcome, deduplicated by message id.
///
/// The rollup retains every terminal outcome within the seven-day retention window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeOutcome {
    pub message_id: MessageId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub agent_name: Option<String>,
    pub status: MessageStatus,
    pub enqueued_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Always-on observability seam beside the carryover read, the twin of
/// [`event_log::testkit`]: carryover bytes parsed, so the performance tier
/// can prove a warm fold re-parses no history and the sidebar tick meter
/// counts a re-parse storm as fold bytes.
#[doc(hidden)]
pub mod testkit {
    use std::sync::atomic::{AtomicU64, Ordering};

    static CARRYOVER_BYTES_PARSED: AtomicU64 = AtomicU64::new(0);
    static ROLLUP_ROWS_MATERIALIZED: AtomicU64 = AtomicU64::new(0);
    static FOLD_ROW_COPIES: AtomicU64 = AtomicU64::new(0);

    /// Row copies from flattening, runtime projections, shared context
    /// vectors, own-row reads, live launch members read with context and
    /// dispatch views; borrowed readers add nothing.
    #[cfg(any(test, feature = "testkit"))]
    pub fn rollup_rows_materialized() -> u64 {
        ROLLUP_ROWS_MATERIALIZED.load(Ordering::Relaxed)
    }

    pub(crate) fn count_rows_materialized(n: u64) {
        ROLLUP_ROWS_MATERIALIZED.fetch_add(n, Ordering::Relaxed);
    }

    /// Deep `AgentState` copies inside the fold since process start.
    #[cfg(any(test, feature = "testkit"))]
    pub fn fold_row_copies() -> u64 {
        FOLD_ROW_COPIES.load(Ordering::Relaxed)
    }

    pub(in crate::store::snapshot) fn count_row_copies(n: u64) {
        FOLD_ROW_COPIES.fetch_add(n, Ordering::Relaxed);
    }

    /// `agents.carryover.json` bytes parsed since process start.
    #[cfg(feature = "testkit")]
    pub fn carryover_bytes_parsed() -> u64 {
        CARRYOVER_BYTES_PARSED.load(Ordering::Relaxed)
    }

    pub(super) fn count_bytes_parsed(n: u64) {
        CARRYOVER_BYTES_PARSED.fetch_add(n, Ordering::Relaxed);
        crate::lane::count_event_log_bytes_read(n);
    }
}

fn read_carryover(path: &Path) -> Result<EventCarryover> {
    match fs::read(path) {
        Ok(bytes) => {
            testkit::count_bytes_parsed(bytes.len() as u64);
            serde_json::from_slice(&bytes).map_err(|source| SnapshotErr::Json {
                path: path.to_path_buf(),
                source,
            })
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(EventCarryover::default()),
        Err(source) => Err(SnapshotErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[must_use = "durability barrier; check the result"]
fn write_carryover(path: &Path, carryover: &EventCarryover) -> Result<()> {
    write_temp_then_rename(path, carryover)?;
    Ok(())
}

/// `agents.carryover.json` made fold-ready once per file identity: card
/// identities backfilled and rows key-sorted with unique keys (the last row
/// for a duplicated key wins, as the fold's keyed maps always did). Shared
/// read-only by every fold on this thread until the file changes. The cached
/// form normalizes legacy prompt labels and trims ended rows' pane commands.
/// Rotation staging writes these labels and backfilled identities into the
/// durable carryover.
#[derive(Clone, Debug, Default)]
struct FoldCarryover {
    agents: Vec<Arc<AgentState>>,
    project_root: Option<PathBuf>,
    /// The file's identity state as written — the cold fold's seed.
    agent_identity: AgentIdentityState,
    resume_outcomes: Vec<ResumeOutcome>,
    reborn: OnceLock<Arc<Self>>,
}

impl FoldCarryover {
    fn from_raw(mut raw: EventCarryover) -> Self {
        for agent in &mut raw.agents {
            let prompt = agent.prompt.clone();
            agent.normalize_prompt_labels();
            if agent.prompt != prompt {
                agent.prompt_wake_only |= prompt
                    .as_deref()
                    .is_some_and(crate::store::message::prompt_is_wake_only);
            }
            agent.trim_ended_text();
        }
        let mut agents: Vec<_> = raw.agents.into_iter().map(Arc::new).collect();
        backfill_agent_identities(&mut agents, raw.agent_identity.clone());
        let agents = agents
            .into_iter()
            .map(|agent| ((agent.kind.clone(), agent.agent_id.clone()), agent))
            .collect::<BTreeMap<_, _>>()
            .into_values()
            .collect();
        Self {
            agents,
            project_root: None,
            agent_identity: raw.agent_identity,
            resume_outcomes: raw.resume_outcomes,
            reborn: OnceLock::new(),
        }
    }

    fn stamp_root_lane(&mut self, project_root: Option<&Path>) {
        self.project_root = project_root.map(Path::to_path_buf);
        self.reborn = OnceLock::new();
        for agent in &mut self.agents {
            stamp_root_lane(agent, project_root);
        }
    }

    fn get(&self, kind: &AgentKind, agent_id: &AgentSessionId) -> Option<&Arc<AgentState>> {
        self.agents
            .binary_search_by(|agent| (&agent.kind, &agent.agent_id).cmp(&(kind, agent_id)))
            .ok()
            .map(|index| &self.agents[index])
    }

    /// The copy a session rebirth folds against: pane, on-screen and ordinal
    /// stamps retired.
    /// Keys are untouched, so the sort order holds.
    fn unstamped_for_rebirth(&self) -> Arc<Self> {
        Arc::clone(self.reborn.get_or_init(|| {
            let mut agents = self.agents.clone();
            unstamp_for_rebirth(&mut agents);
            Arc::new(Self {
                agents,
                project_root: self.project_root.clone(),
                agent_identity: self.agent_identity.clone(),
                resume_outcomes: self.resume_outcomes.clone(),
                reborn: OnceLock::new(),
            })
        }))
    }
}

fn stamp_root_lane(agent: &mut Arc<AgentState>, project_root: Option<&Path>) {
    let root_lane = AgentState::is_root_lane(
        agent.channel.as_deref(),
        agent.lane_path().map(Path::new),
        project_root,
    );
    if agent.root_lane != root_lane {
        agent_mut(agent).root_lane = root_lane;
    }
}

thread_local! {
    /// This thread's last fold-ready carryover — parsed and backfilled once
    /// per file identity instead of once per fold.
    static CARRYOVER_PARSE_CACHE: ParseCache<FoldCarryover> = const { ParseCache::new() };
}

/// The fold-ready carryover under the file identity observed before the read.
/// Rotation and prune replace the file by atomic rename, so a changed payload
/// changes `(len, mtime, dev, ino)` and re-parses exactly once. An error is
/// never cached: a corrupt replacement fails every fold until it is replaced.
fn fold_carryover(path: &Path, project_root: Option<&Path>) -> Result<Arc<FoldCarryover>> {
    let stamped = StampedPath::of(path);
    let (mut carryover, cached) =
        match CARRYOVER_PARSE_CACHE.with(|cache| cache.get_stamped(&stamped)) {
            Some(carryover) => (carryover, true),
            None => (
                Arc::new(FoldCarryover::from_raw(read_carryover(path)?)),
                false,
            ),
        };
    if !cached || carryover.project_root.as_deref() != project_root {
        Arc::make_mut(&mut carryover).stamp_root_lane(project_root);
    }
    CARRYOVER_PARSE_CACHE.with(|cache| cache.store_stamped(&stamped, Arc::clone(&carryover)));
    Ok(carryover)
}

fn workspace_carryover(paths: &StatePaths) -> Result<Arc<FoldCarryover>> {
    let identity = super::assemble::WorkspaceSnapshotIdentity::from_paths(paths);
    fold_carryover(&paths.agents_carryover, identity.project_root.as_deref())
}

/// The carryover-merged rollup: fold-ready carryover beneath the winning
/// active-log rows, in `(kind, agent_id)` order. Both layers share individual
/// rows; folds deep-copy only rows they change. A cold reader still decodes
/// every persisted row before sharing it.
#[derive(Clone, Debug, Default)]
pub struct AgentRollup {
    base: Arc<FoldCarryover>,
    live: Arc<[Arc<AgentState>]>,
}

impl AgentRollup {
    fn layered(base: Arc<FoldCarryover>, raw_agents: &[Arc<AgentState>]) -> Self {
        let live = raw_agents
            .iter()
            .filter(|live| {
                base.get(&live.kind, &live.agent_id)
                    .is_none_or(|carried| carried.last_seen <= live.last_seen)
            })
            .cloned()
            .collect();
        Self { base, live }
    }

    pub fn iter(&self) -> impl Iterator<Item = &AgentState> + Clone {
        self.iter_shared().map(Arc::as_ref)
    }

    fn iter_shared(&self) -> impl Iterator<Item = &Arc<AgentState>> + Clone {
        let mut base = self.base.agents.iter().peekable();
        let mut live = self.live.iter().peekable();
        std::iter::from_fn(move || match (base.peek(), live.peek()) {
            (Some(carried), Some(fresh)) => {
                match (&carried.kind, &carried.agent_id).cmp(&(&fresh.kind, &fresh.agent_id)) {
                    Ordering::Less => base.next(),
                    Ordering::Greater => live.next(),
                    Ordering::Equal => {
                        base.next();
                        live.next()
                    }
                }
            }
            (Some(_), None) => base.next(),
            (None, _) => live.next(),
        })
    }

    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// The row for an exact provider and session identity.
    pub fn find(&self, kind: &str, agent_id: &AgentSessionId) -> Option<&AgentState> {
        self.iter()
            .find(|agent| agent.kind == kind && agent.agent_id == *agent_id)
    }

    pub fn is_empty(&self) -> bool {
        self.base.agents.is_empty() && self.live.is_empty()
    }

    pub fn to_vec(&self) -> Vec<AgentState> {
        let agents: Vec<_> = self.iter().cloned().collect();
        testkit::count_rows_materialized(agents.len() as u64);
        agents
    }
}

/// Rows a fold already owns, with no carryover beneath them.
impl From<Vec<AgentState>> for AgentRollup {
    fn from(rows: Vec<AgentState>) -> Self {
        Self {
            base: Arc::default(),
            live: rows.into_iter().map(Arc::new).collect(),
        }
    }
}

#[cfg(test)]
pub(crate) fn agent_rollup_with_carryover(
    events: &[EventEnvelope],
    carryover_agents: Vec<AgentState>,
) -> Vec<AgentState> {
    let events = decode_events(events);
    fold_delta(
        FoldDeltaSeed::default(),
        Arc::new(FoldCarryover::from_raw(EventCarryover {
            agents: carryover_agents,
            ..EventCarryover::default()
        })),
        &events,
        Timestamp::now(),
    )
    .expect("fold fixture")
    .merged
    .to_vec()
}

/// Bump when [`RollupCache`]'s shape, a reducer's semantics, or
/// [`AgentState::normalize_prompt_labels`]'s rules change — a
/// mismatched cache reads as absent and cold-rebuilds.
const ROLLUP_CACHE_VERSION: u32 = 48;

/// The resumable agent-rollup fold base persisted in `snapshots/rollup.json`:
/// the raw pre-projection fold map stamped with the log extent folded so far.
/// Cache-class — reconstructible from the event log and the carryover at any
/// time, so it renames atomically without fsync and any read failure falls back
/// to the full fold.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RollupCache {
    pub version: u32,
    pub extent: event_log::LogExtent,
    #[serde(default)]
    file_id: Option<LogFileId>,
    pub raw_agents: Vec<Arc<AgentState>>,
    #[serde(default)]
    pub resume_outcomes: Vec<ResumeOutcome>,
    #[serde(default)]
    pub agent_identity: AgentIdentityState,
    #[serde(default)]
    pub saw_session_rebirth: bool,
}

fn read_rollup_cache(path: &Path) -> Option<Arc<RollupCache>> {
    let meta = fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let len = meta.len();
    // Only the parse is cached; callers revalidate the generation, log file
    // identity and extent against the live log before resuming from it.
    if let Some(cache) = ROLLUP_PARSE_CACHE.with(|cache| cache.get(path, mtime, len)) {
        return Some(cache);
    }
    let bytes = fs::read(path).ok()?;
    #[cfg(feature = "testkit")]
    crate::testkit::WORK_COUNTERS
        .rollup_bytes_parsed
        .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
    let cache: RollupCache = serde_json::from_slice(&bytes).ok()?;
    let cache = (cache.version == ROLLUP_CACHE_VERSION).then_some(cache)?;
    let cache = Arc::new(cache);
    ROLLUP_PARSE_CACHE.with(|slot| slot.store(path, mtime, len, Arc::clone(&cache)));
    Some(cache)
}

/// The fold base a fold may consume, sharing rows with any remaining holders.
fn owned_base(base: Arc<RollupCache>) -> RollupCache {
    Arc::unwrap_or_clone(base)
}

/// Seed the lifecycle stream at the exact active-log extent its states reflect.
pub(crate) fn lifecycle_follow_seed(
    paths: &StatePaths,
) -> Result<(event_log::LogExtent, AgentRollup)> {
    let (cache, agents, _resume_outcomes) = catch_up_rollup(paths)?;
    Ok((cache.extent, agents))
}

#[derive(Serialize, Deserialize)]
struct LogGeneration {
    generation: u64,
}

/// Read the durable active-log generation without caching or loading the fold.
/// Missing, undecodable or oversized records read as zero. The bounded read
/// also serves the parked wrapper without a log- or fold-sized allocation.
pub fn lifecycle_log_generation(paths: &StatePaths) -> u64 {
    read_log_generation(paths).unwrap_or(0)
}

fn read_log_generation(paths: &StatePaths) -> Option<u64> {
    use std::io::Read;

    const MAX_RECORD_BYTES: u64 = 1_024;
    let mut bytes = Vec::new();
    fs::File::open(&paths.log_generation)
        .ok()?
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return None;
    }
    let record: LogGeneration = serde_json::from_slice(&bytes).ok()?;
    Some(record.generation)
}

/// Durably add one to the active-log generation. Only `Store::commit_boundary`
/// calls this, under the workspace lock, after a mutation replaced the log.
pub(crate) fn advance_log_generation(paths: &StatePaths) -> Result<()> {
    let generation = lifecycle_log_generation(paths).saturating_add(1);
    write_temp_then_rename(&paths.log_generation, &LogGeneration { generation })?;
    Ok(())
}

thread_local! {
    /// This thread's last `rollup.json` parse — the fold base a cold
    /// catch-up fold resumes from.
    static ROLLUP_PARSE_CACHE: ParseCache<RollupCache> = const { ParseCache::new() };
}

#[must_use = "atomicity barrier; check the result"]
pub(super) fn write_rollup_cache(paths: &StatePaths, cache: &RollupCache) -> Result<()> {
    let Some(path) = paths.rollup_cache() else {
        return Ok(());
    };
    let _ = atomic::sweep_stale_temp_siblings(&path, Duration::from_secs(3_600));
    atomic::write_checkpoint(&path, cache)?;
    Ok(())
}

struct FoldedDelta {
    raw_agents: Vec<Arc<AgentState>>,
    agent_identity: AgentIdentityState,
    saw_session_rebirth: bool,
    merged: AgentRollup,
    resume_outcomes: Vec<ResumeOutcome>,
    merged_resume_outcomes: Vec<ResumeOutcome>,
}

#[derive(Default)]
struct FoldDeltaSeed {
    agents: BTreeMap<(AgentKind, AgentSessionId), Arc<AgentState>>,
    identity: AgentIdentityState,
    resume_outcomes: BTreeMap<(AgentKind, AgentSessionId, MessageId), ResumeOutcome>,
    saw_session_rebirth: bool,
}

fn fold_delta(
    mut seed: FoldDeltaSeed,
    carryover: Arc<FoldCarryover>,
    events: &[FoldEvent<'_>],
    now: Timestamp,
) -> Result<FoldedDelta> {
    let rebirth_precedes_delta = seed.saw_session_rebirth;
    let rebirth = events
        .iter()
        .position(|event| matches!(event.kind, EventKind::SessionRebirth));
    seed.saw_session_rebirth |= rebirth.is_some();
    // The shared parse is never mutated: a rebirth folds against its own copy.
    let carryover = if rebirth_precedes_delta {
        carryover.unstamped_for_rebirth()
    } else {
        carryover
    };

    // Hydrate continuing rows at rebirth, plus observed keys, linked predecessors,
    // and command receivers. Stamps persist in raw_agents, not the shared carryover.
    let (before_rebirth, after_rebirth) = events.split_at(rebirth.unwrap_or(0));
    for delta in std::iter::once(before_rebirth)
        .filter(|delta| !delta.is_empty())
        .chain(std::iter::once(after_rebirth))
    {
        if delta
            .first()
            .is_some_and(|event| matches!(event.kind, EventKind::SessionRebirth))
        {
            for carried in &carryover.agents {
                if carried.ended_at.is_none() {
                    seed.agents
                        .entry((carried.kind.clone(), carried.agent_id.clone()))
                        .or_insert_with(|| Arc::clone(carried));
                }
            }
        }
        for event in delta {
            let key = match &event.kind {
                EventKind::Message { payload, .. } => compact_command_agent_key(
                    seed.agents
                        .values()
                        .chain(carryover.agents.iter())
                        .map(Arc::as_ref),
                    payload,
                ),
                _ => agent_event_key(event),
            };
            let Some(key) = key else {
                continue;
            };
            let predecessor = match &event.kind {
                EventKind::AgentLifecycle(payload) => payload
                    .observation
                    .compacted_from
                    .as_ref()
                    .map(|id| (key.0.clone(), id.clone())),
                _ => None,
            };
            for key in std::iter::once(key).chain(predecessor) {
                if seed.agents.contains_key(&key) {
                    continue;
                }
                if let Some(carried) = carryover.get(&key.0, &key.1) {
                    seed.agents.insert(key, Arc::clone(carried));
                }
            }
        }
        (seed.agents, seed.identity) =
            reduce_agent_states_seeded_with_identity(seed.agents, seed.identity, delta);
    }

    let (map, mut agent_identity) = (seed.agents, seed.identity);
    let resume_outcomes: Vec<ResumeOutcome> =
        reduce_resume_outcomes_seeded(seed.resume_outcomes, events, now)
            .into_values()
            .collect();
    let mut raw_agents: Vec<_> = map.into_values().collect();
    let provisional: Vec<_> = raw_agents
        .iter()
        .enumerate()
        .filter(|(_, agent)| seed.saw_session_rebirth && agent.kind_ordinal.is_none())
        .map(|(index, agent)| (index, agent.name.clone(), agent.name_explicit))
        .collect();
    let reducer_identity = (!provisional.is_empty()).then(|| agent_identity.clone());
    let merged_resume_outcomes =
        merge_resume_outcomes(&carryover.resume_outcomes, &resume_outcomes, now);
    let carryover = if seed.saw_session_rebirth && !rebirth_precedes_delta {
        carryover.unstamped_for_rebirth()
    } else {
        carryover
    };
    if seed.saw_session_rebirth {
        agent_identity = backfill_agent_identities(&mut raw_agents, agent_identity);
    }
    for agent in &mut raw_agents {
        stamp_root_lane(agent, carryover.project_root.as_deref());
    }
    let merged = AgentRollup::layered(carryover, &raw_agents);

    // Reducer-unassigned identities belong to the projection, not the next delta's claims.
    for (index, name, name_explicit) in provisional {
        let agent = agent_mut(&mut raw_agents[index]);
        agent.name = name;
        agent.name_explicit = name_explicit;
        agent.kind_ordinal = None;
    }
    if let Some(identity) = reducer_identity {
        agent_identity = identity;
    }

    Ok(FoldedDelta {
        raw_agents,
        agent_identity,
        saw_session_rebirth: seed.saw_session_rebirth,
        merged,
        resume_outcomes,
        merged_resume_outcomes,
    })
}

/// Catch the rollup up to the live log and return the fold base beside the
/// carryover-merged rollup, both shared rather than copied.
///
/// Served by this thread's held [`RollupCursor`]: the first read parses
/// `snapshots/rollup.json` and the carryover, a read after an append folds only
/// the new frames, and an unchanged log and carryover need only the bounded
/// generation read and metadata checks, without reopening either fold input.
/// The slot holds one workspace, keyed by the fold's input paths; a read of
/// another workspace replaces it, so a thread holds at most one rollup.
/// Read-only: the caller that owns a write serialization point (a locked
/// rebuild, the single-flighted publisher) persists the returned cache; a
/// plain reader just uses it.
pub(crate) fn catch_up_rollup(
    paths: &StatePaths,
) -> Result<(Arc<RollupCache>, AgentRollup, Vec<ResumeOutcome>)> {
    thread_local! {
        static THREAD_ROLLUP: Cell<Option<ThreadRollup>> = const { Cell::new(None) };
    }
    THREAD_ROLLUP.with(|slot| {
        let mut held = slot
            .take()
            .filter(|held| held.inputs == FoldInputs::of(paths))
            .unwrap_or_else(|| ThreadRollup {
                inputs: FoldInputs::of(paths),
                cursor: RollupCursor::new(),
            });
        let read = held.cursor.fold_shared(paths);
        slot.set(Some(held));
        read
    })
}

/// The thread's one held fold and the workspace files it folded.
struct ThreadRollup {
    inputs: FoldInputs,
    cursor: RollupCursor,
}

#[derive(PartialEq, Eq)]
struct FoldInputs {
    events_log: PathBuf,
    rollup_cache: Option<PathBuf>,
    agents_carryover: PathBuf,
}

impl FoldInputs {
    fn of(paths: &StatePaths) -> Self {
        Self {
            events_log: paths.events_log.clone(),
            rollup_cache: paths.rollup_cache(),
            agents_carryover: paths.agents_carryover.clone(),
        }
    }
}

/// [`catch_up_rollup`] with a hook transaction's staged, unappended events
/// folded on top. With nothing staged the shared rollup passes through; with
/// events staged the reducer shares the rows and copies only those it changes.
pub(in crate::store) fn catch_up_with_pending(
    paths: &StatePaths,
    pending: &[EventEnvelope],
) -> Result<PendingRollup> {
    let (base, agents, resume_outcomes) = catch_up_rollup(paths)?;
    if pending.is_empty() {
        return Ok(PendingRollup {
            base,
            pending_identity: None,
            agents,
            resume_outcomes,
        });
    }
    let seed: BTreeMap<_, _> = agents
        .iter_shared()
        .map(|agent| ((agent.kind.clone(), agent.agent_id.clone()), agent.clone()))
        .collect();
    let (agents, identity) = reduce_agent_states_seeded_with_identity(
        seed,
        base.agent_identity.clone(),
        &decode_events(pending),
    );
    Ok(PendingRollup {
        base,
        pending_identity: Some(identity),
        agents: AgentRollup {
            base: Arc::default(),
            live: agents.into_values().collect(),
        },
        resume_outcomes,
    })
}

/// The rollup a store read sees inside a hook transaction: the shared fold
/// base, plus the identity state its staged events folded to.
#[derive(Debug)]
pub(in crate::store) struct PendingRollup {
    base: Arc<RollupCache>,
    pending_identity: Option<AgentIdentityState>,
    pub agents: AgentRollup,
    pub resume_outcomes: Vec<ResumeOutcome>,
}

impl PendingRollup {
    pub fn extent(&self) -> event_log::LogExtent {
        self.base.extent
    }

    pub fn agent_identity(&self) -> &AgentIdentityState {
        self.pending_identity
            .as_ref()
            .unwrap_or(&self.base.agent_identity)
    }
}

fn catch_up_rollup_layered(
    paths: &StatePaths,
) -> Result<(RollupCache, AgentRollup, Vec<ResumeOutcome>)> {
    let generation = lifecycle_log_generation(paths);
    let log = LogHead::new(generation, fs::metadata(&paths.events_log).ok());
    let base = paths
        .rollup_cache()
        .as_deref()
        .and_then(read_rollup_cache)
        .filter(|cache| log.accepts(cache))
        .map(owned_base);
    catch_up_from(base, paths, workspace_carryover(paths)?, log)
}

/// The base-parameterized fold core every entry point shares: resume from
/// `base` (a fold cold from offset zero when `None`), fold only the frames
/// appended past it, and return the refreshed cache beside the
/// carryover-merged rollup. The disk-backed wrapper ([`catch_up_rollup`]) and
/// the in-memory [`RollupCursor`] both delegate here — one implementation, so
/// the keystone fold equivalence holds for every reader by construction.
/// Callers validate the base against `log`, stamped before reading any bytes.
fn catch_up_from(
    base: Option<RollupCache>,
    paths: &StatePaths,
    carryover: Arc<FoldCarryover>,
    log: LogHead,
) -> Result<(RollupCache, AgentRollup, Vec<ResumeOutcome>)> {
    let now = Timestamp::now();
    let (seed, start) = match base {
        Some(RollupCache {
            extent,
            raw_agents,
            resume_outcomes,
            agent_identity,
            saw_session_rebirth,
            ..
        }) => {
            let seed: BTreeMap<_, _> = raw_agents
                .into_iter()
                .map(|agent| ((agent.kind.clone(), agent.agent_id.clone()), agent))
                .collect();
            let resume_seed = resume_outcomes
                .into_iter()
                .map(|outcome| {
                    (
                        (
                            outcome.kind.clone(),
                            outcome.agent_id.clone(),
                            outcome.message_id.clone(),
                        ),
                        outcome,
                    )
                })
                .collect();
            (
                FoldDeltaSeed {
                    agents: seed,
                    identity: agent_identity,
                    resume_outcomes: resume_seed,
                    saw_session_rebirth,
                },
                extent.offset,
            )
        }
        None => (
            FoldDeltaSeed {
                identity: carryover.agent_identity.clone(),
                ..FoldDeltaSeed::default()
            },
            0,
        ),
    };
    let (delta, end) = event_log::read_from_offset(&paths.events_log, start)?;
    let decoded_delta = decode_events(&delta);
    let folded = fold_delta(seed, carryover, &decoded_delta, now)?;
    let refreshed = RollupCache {
        version: ROLLUP_CACHE_VERSION,
        extent: event_log::LogExtent {
            generation: log.generation,
            offset: end,
        },
        file_id: log.file_id,
        raw_agents: folded.raw_agents,
        resume_outcomes: folded.resume_outcomes,
        agent_identity: folded.agent_identity,
        saw_session_rebirth: folded.saw_session_rebirth,
    };
    Ok((refreshed, folded.merged, folded.merged_resume_outcomes))
}

fn reduce_resume_outcomes_seeded(
    mut seed: BTreeMap<(AgentKind, AgentSessionId, MessageId), ResumeOutcome>,
    events: &[FoldEvent<'_>],
    now: Timestamp,
) -> BTreeMap<(AgentKind, AgentSessionId, MessageId), ResumeOutcome> {
    for event in events {
        let Some(outcome) = resume_outcome_for_event(event) else {
            continue;
        };
        upsert_resume_outcome(&mut seed, outcome);
    }
    prune_resume_outcomes(&mut seed, now);
    seed
}

fn resume_outcome_for_event(event: &FoldEvent<'_>) -> Option<ResumeOutcome> {
    let EventKind::Message { payload, .. } = &event.kind else {
        return None;
    };
    if payload.gate != DeliveryGate::Resume
        || payload.body != MessageBody::Prompt
        || !payload.status.is_terminal()
    {
        return None;
    }
    Some(ResumeOutcome {
        message_id: payload.message_id.clone(),
        kind: payload.kind.clone(),
        agent_id: payload.agent_id.clone(),
        agent_name: payload.agent_name.clone(),
        status: payload.status,
        enqueued_at: payload.enqueued_at.unwrap_or(event.envelope.timestamp),
        updated_at: event.envelope.timestamp,
    })
}

fn merge_resume_outcomes(
    carryover: &[ResumeOutcome],
    live: &[ResumeOutcome],
    now: Timestamp,
) -> Vec<ResumeOutcome> {
    let mut map = BTreeMap::new();
    for outcome in carryover.iter().chain(live) {
        upsert_resume_outcome(&mut map, outcome.clone());
    }
    prune_resume_outcomes(&mut map, now);
    map.into_values().collect()
}

fn upsert_resume_outcome(
    map: &mut BTreeMap<(AgentKind, AgentSessionId, MessageId), ResumeOutcome>,
    outcome: ResumeOutcome,
) {
    let key = (
        outcome.kind.clone(),
        outcome.agent_id.clone(),
        outcome.message_id.clone(),
    );
    match map.get(&key) {
        Some(existing) if resume_outcome_cmp(existing, &outcome).is_ge() => {}
        _ => {
            map.insert(key, outcome);
        }
    }
}

fn resume_outcome_cmp(left: &ResumeOutcome, right: &ResumeOutcome) -> std::cmp::Ordering {
    left.enqueued_at
        .cmp(&right.enqueued_at)
        .then_with(|| left.updated_at.cmp(&right.updated_at))
        .then_with(|| left.message_id.as_str().cmp(right.message_id.as_str()))
}

fn prune_resume_outcomes(
    map: &mut BTreeMap<(AgentKind, AgentSessionId, MessageId), ResumeOutcome>,
    now: Timestamp,
) {
    map.retain(|_, outcome| {
        now.duration_since(outcome.updated_at).as_secs() <= RESUME_OUTCOME_RETENTION_SECS
    });
}

/// Preserve retained audit identity before the active event log rotates.
pub(crate) fn stage_carryover_for_rotation(paths: &StatePaths, min_bytes: u64) -> Result<usize> {
    let generation = lifecycle_log_generation(paths);
    let meta = match fs::metadata(&paths.events_log) {
        Ok(meta) => Some(meta),
        Err(source) if source.kind() == io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(event_log::EventLogErr::Io {
                path: paths.events_log.clone(),
                source,
            }
            .into());
        }
    };
    let log = LogHead::new(generation, meta);
    let current_bytes = log.len;
    if current_bytes == 0 || current_bytes < min_bytes {
        return Ok(read_carryover(&paths.agents_carryover)?.agents.len());
    }

    let base = paths
        .rollup_cache()
        .as_deref()
        .and_then(read_rollup_cache)
        .filter(|cache| log.accepts(cache))
        .map(owned_base);
    let identity = super::assemble::WorkspaceSnapshotIdentity::from_paths(paths);
    let mut carryover = FoldCarryover::from_raw(read_carryover(&paths.agents_carryover)?);
    carryover.stamp_root_lane(identity.project_root.as_deref());
    let (cache, agents, resume_outcomes) = catch_up_from(base, paths, Arc::new(carryover), log)?;
    let agents = retain_carryover_agents(agents.to_vec(), event_log::DEFAULT_RETENTION);
    let carryover_agents = agents.len();
    let agent_identity = cache
        .agent_identity
        .with_agent_ordinals(&agents)
        .without_consumed_launches();
    write_carryover(
        &paths.agents_carryover,
        &EventCarryover {
            agents,
            agent_identity,
            resume_outcomes,
        },
    )?;
    Ok(carryover_agents)
}

/// Prune expired carryover rows without changing the active-log fold extent.
pub(crate) fn prune_carryover(paths: &StatePaths, older_than: Duration) -> Result<usize> {
    let mut carryover = read_carryover(&paths.agents_carryover)?;
    let before = carryover.agents.len();
    carryover.agents = retain_carryover_agents(carryover.agents, older_than);
    let removed = before.saturating_sub(carryover.agents.len());
    if removed > 0 {
        write_carryover(&paths.agents_carryover, &carryover)?;
    }
    Ok(removed)
}

fn retain_carryover_agents(agents: Vec<AgentState>, older_than: Duration) -> Vec<AgentState> {
    let cutoff = Timestamp::now() - older_than;
    agents
        .into_iter()
        .filter(|agent| {
            agent.last_seen >= cutoff
                || agent
                    .runtime_owner
                    .as_ref()
                    .is_some_and(runtime::owner_is_live)
        })
        .collect()
}

/// A long-lived reader's in-memory fold base: the last `RollupCache` this
/// cursor folded to, the merged rollup it produced, and the identity of the
/// log file it folded. A cursor reads only new log bytes per wakeup, with a
/// bounded generation read and one `stat` each of the log and carryover,
/// parsing neither base. An unchanged log and carryover return
/// the held rollup without opening either fold input. A long-lived reader
/// (the sidebar fetch worker) owns its cursor across its loop; every other
/// store read goes through the thread's one held cursor in `catch_up_rollup`.
/// Deltas still walk retained keys and names, sharing rows and copying only
/// those they change. A cold reload decodes every persisted row once.
///
/// Staleness is structural, not best-effort: a swapped log file — rotation's
/// rename-and-recreate, the identity rewrite's rename-over — changes the
/// `(dev, ino)` identity captured by the same `stat`, and an offset past the
/// live length or a changed durable generation means the same, so any of
/// these drops the in-memory base and reloads a validated `rollup.json`.
/// A warm fold that errors retries cold once before propagating, so a stale
/// base can never surface as corruption; an error out of the cold fold is
/// the same real corruption every reader reports.
#[derive(Debug, Default)]
pub struct RollupCursor {
    held: Option<CursorState>,
}

#[derive(Debug)]
struct CursorState {
    cache: Arc<RollupCache>,
    merged: AgentRollup,
    resume_outcomes: Vec<ResumeOutcome>,
    /// The carryover stamp observed before this base was folded: GC can
    /// prune the carryover while the log stands still, so an unchanged log
    /// alone does not make the held merge current.
    carryover: FileStamp,
}

/// Identity of the log file a base folded: device + inode on unix, so a
/// recreated or renamed-over log reads as a different file even when the
/// regrown log is longer than the held offset. Targets without that identity
/// get `None` and retain the generation and offset guards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Copy)]
struct LogHead {
    generation: u64,
    len: u64,
    file_id: Option<LogFileId>,
}

impl LogHead {
    fn new(generation: u64, meta: Option<fs::Metadata>) -> Self {
        Self {
            generation,
            len: meta.as_ref().map_or(0, fs::Metadata::len),
            file_id: meta.as_ref().and_then(LogFileId::of),
        }
    }

    fn accepts(self, cache: &RollupCache) -> bool {
        cache.extent.generation == self.generation
            && cache.file_id == self.file_id
            && cache.extent.offset <= self.len
    }
}

impl RollupCursor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Catch the held rollup up to the live log and return the extent it
    /// reflects beside the carryover-merged agents. The cursor twin of
    /// `catch_up_rollup`; see the type docs for the staleness guards.
    pub fn fold(
        &mut self,
        paths: &StatePaths,
    ) -> Result<(event_log::LogExtent, AgentRollup, Vec<ResumeOutcome>)> {
        let (cache, merged, resume_outcomes) = self.fold_shared(paths)?;
        Ok((cache.extent, merged, resume_outcomes))
    }

    /// [`Self::fold`] returning the whole fold base, shared with the cursor:
    /// a caller that drops it before the next fold leaves the cursor the only
    /// holder, so that fold resumes without copying it.
    fn fold_shared(
        &mut self,
        paths: &StatePaths,
    ) -> Result<(Arc<RollupCache>, AgentRollup, Vec<ResumeOutcome>)> {
        let generation = lifecycle_log_generation(paths);
        let log = LogHead::new(generation, fs::metadata(&paths.events_log).ok());
        // Stamped before any read, so a carryover replaced mid-fold reads as
        // changed on the next call rather than hiding behind this one.
        let carryover_stamp = StampedPath::of(&paths.agents_carryover).stamp;
        if let Some(held) = self.held.take() {
            let current = log.accepts(&held.cache);
            if current && held.cache.extent.offset == log.len && held.carryover == carryover_stamp {
                // Nothing appended and the carryover unchanged: serve the
                // held fold without reopening the log or carryover.
                let out = (
                    Arc::clone(&held.cache),
                    held.merged.clone(),
                    held.resume_outcomes.clone(),
                );
                self.held = Some(held);
                return Ok(out);
            }
            // A warm fold that errors falls through to the cold reload: if
            // the log is genuinely corrupt the cold fold errors the same way,
            // so nothing is masked — only a stale base heals.
            if current
                && held.cache.extent.offset < log.len
                && let Ok(carryover) = workspace_carryover(paths)
                && let Ok((cache, merged, resume_outcomes)) =
                    catch_up_from(Some(owned_base(held.cache)), paths, carryover, log)
            {
                return Ok(self.hold(cache, merged, resume_outcomes, carryover_stamp));
            }
            // Identity changed, the offset regressed, or only the carryover
            // moved: the in-memory merge is stale, so reload from disk.
        }
        let (cache, merged, resume_outcomes) = catch_up_rollup_layered(paths)?;
        Ok(self.hold(cache, merged, resume_outcomes, carryover_stamp))
    }

    fn hold(
        &mut self,
        cache: RollupCache,
        merged: AgentRollup,
        resume_outcomes: Vec<ResumeOutcome>,
        carryover: FileStamp,
    ) -> (Arc<RollupCache>, AgentRollup, Vec<ResumeOutcome>) {
        let cache = Arc::new(cache);
        self.held = Some(CursorState {
            cache: Arc::clone(&cache),
            merged: merged.clone(),
            resume_outcomes: resume_outcomes.clone(),
            carryover,
        });
        (cache, merged, resume_outcomes)
    }
}

#[cfg(test)]
mod tests;
