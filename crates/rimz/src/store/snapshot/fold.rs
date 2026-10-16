//! The resumable event-log fold: the persisted rollup cache, its extent
//! stamp, and the carryover that survives log rotation.

use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::project::{
    AgentIdentityState, FoldEvent, agent_event_key, backfill_agent_identities,
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
    static DERIVED_CARRYOVER_BYTES_READ: AtomicU64 = AtomicU64::new(0);
    static ROLLUP_ROWS_MATERIALIZED: AtomicU64 = AtomicU64::new(0);
    static ROLLUP_BASE_COPIES: AtomicU64 = AtomicU64::new(0);

    /// Rollup rows flattened since process start; borrowed readers add nothing.
    #[cfg(any(test, feature = "testkit"))]
    pub fn rollup_rows_materialized() -> u64 {
        ROLLUP_ROWS_MATERIALIZED.load(Ordering::Relaxed)
    }

    pub(super) fn count_rows_materialized(n: u64) {
        ROLLUP_ROWS_MATERIALIZED.fetch_add(n, Ordering::Relaxed);
    }

    /// Deep copies of a shared fold base since process start: a fold resumed
    /// from a base another holder still shares.
    #[cfg(any(test, feature = "testkit"))]
    pub fn rollup_base_copies() -> u64 {
        ROLLUP_BASE_COPIES.load(Ordering::Relaxed)
    }

    pub(super) fn count_base_copy() {
        ROLLUP_BASE_COPIES.fetch_add(1, Ordering::Relaxed);
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

    /// Trimmed carryover cache bytes read since process start, including misses.
    #[cfg(feature = "testkit")]
    pub fn derived_carryover_bytes_read() -> u64 {
        DERIVED_CARRYOVER_BYTES_READ.load(Ordering::Relaxed)
    }

    pub(super) fn count_derived_bytes(n: u64) {
        DERIVED_CARRYOVER_BYTES_READ.fetch_add(n, Ordering::Relaxed);
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
/// form keeps a bounded first-line prompt label on ended rows and drops their
/// other prompt and pane-command text; prune and reseed read
/// the raw file through [`read_carryover`], and rotation staging folds against
/// its own untrimmed copy and writes the merged rows, so an identity backfilled
/// here is persisted at the next rotation.
#[derive(Clone, Debug, Default)]
struct FoldCarryover {
    agents: Vec<AgentState>,
    project_root: Option<PathBuf>,
    /// The file's identity state as written — the cold fold's seed.
    agent_identity: AgentIdentityState,
    resume_outcomes: Vec<ResumeOutcome>,
    source_path: Option<PathBuf>,
    raw_source: Option<FileStamp>,
    reborn: OnceLock<Arc<Self>>,
}

const CARRIED_FIRST_PROMPT_MAX_CHARS: usize = 160;

impl FoldCarryover {
    fn from_raw(mut raw: EventCarryover) -> Self {
        backfill_agent_identities(&mut raw.agents, raw.agent_identity.clone());
        let agents = raw
            .agents
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
            source_path: None,
            raw_source: None,
            reborn: OnceLock::new(),
        }
    }

    fn trimmed(mut self, source: &Path) -> Self {
        self.source_path = Some(source.to_path_buf());
        for agent in &mut self.agents {
            if agent.ended_at.is_none() {
                continue;
            }
            if let Some(prompt) = &mut agent.first_prompt {
                *prompt = prompt
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or_default()
                    .chars()
                    .take(CARRIED_FIRST_PROMPT_MAX_CHARS)
                    .collect();
            }
            agent.prompt = None;
            agent.recent_prompts = Vec::new();
            if let Some(pane) = &mut agent.pane {
                pane.foreground_cmdline = None;
                pane.spawn_command = None;
            }
        }
        self
    }

    fn stamp_root_lane(&mut self, project_root: Option<&Path>) {
        self.project_root = project_root.map(Path::to_path_buf);
        self.reborn = OnceLock::new();
        for agent in &mut self.agents {
            agent.root_lane = AgentState::is_root_lane(
                agent.channel.as_deref(),
                agent.lane_path().map(Path::new),
                project_root,
            );
        }
    }

    fn get(&self, kind: &AgentKind, agent_id: &AgentSessionId) -> Option<&AgentState> {
        self.agents
            .binary_search_by(|agent| (&agent.kind, &agent.agent_id).cmp(&(kind, agent_id)))
            .ok()
            .map(|index| &self.agents[index])
    }

    /// The copy a session rebirth folds against: panes and ordinals unstamped.
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
                source_path: self.source_path.clone(),
                raw_source: self.raw_source,
                reborn: OnceLock::new(),
            })
        }))
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
fn fold_carryover(
    path: &Path,
    project_root: Option<&Path>,
    derived_path: Option<&Path>,
) -> Result<Arc<FoldCarryover>> {
    let stamped = StampedPath::of(path);
    let (mut carryover, cached) =
        match CARRYOVER_PARSE_CACHE.with(|cache| cache.get_stamped(&stamped)) {
            Some(carryover) => (carryover, true),
            None => {
                let derived = derived_path
                    .filter(|_| path.exists())
                    .and_then(|derived| read_derived_carryover(derived, stamped.stamp));
                let carryover = match derived {
                    Some(raw) => FoldCarryover {
                        agents: raw.agents,
                        agent_identity: raw.agent_identity,
                        resume_outcomes: raw.resume_outcomes,
                        source_path: Some(path.to_path_buf()),
                        ..FoldCarryover::default()
                    },
                    None => {
                        let mut carryover =
                            FoldCarryover::from_raw(read_carryover(path)?).trimmed(path);
                        carryover.raw_source = path.exists().then_some(stamped.stamp);
                        carryover
                    }
                };
                (Arc::new(carryover), false)
            }
        };
    if !cached || carryover.project_root.as_deref() != project_root {
        Arc::make_mut(&mut carryover).stamp_root_lane(project_root);
    }
    CARRYOVER_PARSE_CACHE.with(|cache| cache.store_stamped(&stamped, Arc::clone(&carryover)));
    Ok(carryover)
}

fn workspace_carryover(paths: &StatePaths) -> Result<Arc<FoldCarryover>> {
    let identity = super::assemble::WorkspaceSnapshotIdentity::from_paths(paths);
    fold_carryover(
        &paths.agents_carryover,
        identity.project_root.as_deref(),
        Some(&paths.carryover_cache),
    )
}

#[derive(Serialize, Deserialize)]
struct DerivedCarryover {
    version: u32,
    source: FileStamp,
    agents: Vec<AgentState>,
    agent_identity: AgentIdentityState,
    resume_outcomes: Vec<ResumeOutcome>,
}

fn read_derived_carryover(path: &Path, source: FileStamp) -> Option<EventCarryover> {
    let bytes = fs::read(path).ok()?;
    testkit::count_derived_bytes(bytes.len() as u64);
    let cached: DerivedCarryover = serde_json::from_slice(&bytes).ok()?;
    (cached.version == ROLLUP_CACHE_VERSION && cached.source == source).then_some(EventCarryover {
        agents: cached.agents,
        agent_identity: cached.agent_identity,
        resume_outcomes: cached.resume_outcomes,
    })
}

pub(super) fn write_carryover_cache(paths: &StatePaths) -> Result<()> {
    static PUBLISHED: OnceLock<Mutex<HashSet<StampedPath>>> = OnceLock::new();
    let carryover = workspace_carryover(paths)?;
    let Some(source) = carryover.raw_source else {
        return Ok(());
    };
    let stamped = StampedPath {
        path: paths.agents_carryover.clone(),
        stamp: source,
    };
    let mut published = PUBLISHED
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if published.contains(&stamped) {
        return Ok(());
    }
    let cached = DerivedCarryover {
        version: ROLLUP_CACHE_VERSION,
        source,
        agents: carryover.agents.clone(),
        agent_identity: carryover.agent_identity.clone(),
        resume_outcomes: carryover.resume_outcomes.clone(),
    };
    let _ = atomic::sweep_stale_temp_siblings(&paths.carryover_cache, Duration::from_secs(3_600));
    atomic::write_temp_then_rename_cache_compact(&paths.carryover_cache, &cached)?;
    published.insert(stamped);
    Ok(())
}

/// The carryover-merged agent rollup, layered so a fold never copies history:
/// the shared fold-ready carryover beneath the live rows that win the merge
/// (no carried row, or one observed no later than the live row). Iterates in
/// `(kind, agent_id)` order, as the flat merge did; cloning shares both layers.
#[derive(Clone, Debug, Default)]
pub struct AgentRollup {
    base: Arc<FoldCarryover>,
    live: Arc<[AgentState]>,
}

impl AgentRollup {
    fn layered(base: Arc<FoldCarryover>, raw_agents: &[AgentState]) -> Self {
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
            live: rows.into(),
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
/// [`FoldCarryover::trimmed`]'s rules change — a
/// mismatched cache reads as absent and cold-rebuilds.
const ROLLUP_CACHE_VERSION: u32 = 30;

/// The resumable agent-rollup fold base persisted in `snapshots/rollup.json`:
/// the raw pre-projection fold map stamped with the log extent folded so far.
/// Cache-class — reconstructible from the event log and the carryover at any
/// time, so it renames atomically without fsync and any read failure falls back
/// to the full fold.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RollupCache {
    pub version: u32,
    pub extent: event_log::LogExtent,
    pub raw_agents: Vec<AgentState>,
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
    // Only the *parse* is cached; every caller still folds against the live
    // log from the cached extent, so even a stale serve costs a larger fold,
    // never a wrong rollup (the [`ParseCache`] contract).
    if let Some(cache) = ROLLUP_PARSE_CACHE.with(|cache| cache.get(path, mtime, len)) {
        return Some(cache);
    }
    let bytes = fs::read(path).ok()?;
    let cache: RollupCache = serde_json::from_slice(&bytes).ok()?;
    let cache = (cache.version == ROLLUP_CACHE_VERSION).then_some(cache)?;
    let cache = Arc::new(cache);
    ROLLUP_PARSE_CACHE.with(|slot| slot.store(path, mtime, len, Arc::clone(&cache)));
    Some(cache)
}

/// The fold base a fold may consume: moved out when this is its only holder,
/// copied (and counted) when the parse cache or a reader still shares it.
fn owned_base(base: Arc<RollupCache>) -> RollupCache {
    Arc::try_unwrap(base).unwrap_or_else(|shared| {
        testkit::count_base_copy();
        RollupCache::clone(&shared)
    })
}

/// Seed the lifecycle stream at the exact active-log extent its states reflect.
pub(crate) fn lifecycle_follow_seed(
    paths: &StatePaths,
) -> Result<(event_log::LogExtent, AgentRollup)> {
    let (cache, agents, _resume_outcomes) = catch_up_rollup(paths)?;
    Ok((cache.extent, agents))
}

/// Read the active log generation without using the long-lived parse cache.
///
/// A follower polls this across rotations, where a same-size cache rewrite must
/// still become visible even on filesystems with coarse modification times.
pub fn lifecycle_log_generation(paths: &StatePaths) -> u64 {
    #[derive(Deserialize)]
    struct Header {
        version: u32,
        extent: event_log::LogExtent,
    }

    fs::File::open(&paths.rollup_cache)
        .ok()
        .and_then(|file| serde_json::from_reader::<_, Header>(io::BufReader::new(file)).ok())
        .filter(|cache| cache.version == ROLLUP_CACHE_VERSION)
        .map(|cache| cache.extent.generation)
        .unwrap_or(0)
}

thread_local! {
    /// This thread's last `rollup.json` parse — the fold base a cold
    /// catch-up fold resumes from.
    static ROLLUP_PARSE_CACHE: ParseCache<RollupCache> = const { ParseCache::new() };
}

#[must_use = "atomicity barrier; check the result"]
pub(super) fn write_rollup_cache(path: &Path, cache: &RollupCache) -> Result<()> {
    let _ = atomic::sweep_stale_temp_siblings(path, Duration::from_secs(3_600));
    atomic::write_temp_then_rename_cache_compact(path, cache)?;
    Ok(())
}

struct FoldedDelta {
    raw_agents: Vec<AgentState>,
    agent_identity: AgentIdentityState,
    saw_session_rebirth: bool,
    merged: AgentRollup,
    resume_outcomes: Vec<ResumeOutcome>,
    merged_resume_outcomes: Vec<ResumeOutcome>,
}

#[derive(Default)]
struct FoldDeltaSeed {
    agents: BTreeMap<(AgentKind, AgentSessionId), AgentState>,
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
    let mut full_rows: Option<BTreeMap<_, _>> = None;
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
                        .or_insert_with(|| carried.clone());
                }
            }
        }
        for event in delta {
            let key = match &event.kind {
                EventKind::Message { payload, .. } => compact_command_agent_key(
                    seed.agents.values().chain(carryover.agents.iter()),
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
                    let mut carried = carried.clone();
                    if carried.ended_at.is_some()
                        && let Some(source) = &carryover.source_path
                    {
                        if full_rows.is_none() {
                            full_rows = Some(
                                read_carryover(source)?
                                    .agents
                                    .into_iter()
                                    .map(|agent| {
                                        ((agent.kind.clone(), agent.agent_id.clone()), agent)
                                    })
                                    .collect(),
                            );
                        }
                        if let Some(full) = full_rows.as_ref().and_then(|rows| rows.get(&key)) {
                            restore_agent_text(&mut carried, full);
                        }
                    }
                    seed.agents.insert(key, carried);
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
    let mut raw_agents: Vec<AgentState> = map.into_values().collect();
    let provisional: Vec<_> = raw_agents
        .iter()
        .enumerate()
        .filter(|(_, agent)| seed.saw_session_rebirth && agent.kind_ordinal.is_none())
        .map(|(index, agent)| (index, agent.name.clone(), agent.name_explicit))
        .collect();
    let reducer_identity = (!provisional.is_empty()).then(|| agent_identity.clone());
    let merged_resume_outcomes =
        merge_resume_outcomes(&carryover.resume_outcomes, &resume_outcomes, now);
    let merged = if seed.saw_session_rebirth {
        let carryover = if rebirth_precedes_delta {
            carryover
        } else {
            carryover.unstamped_for_rebirth()
        };
        agent_identity = backfill_agent_identities(&mut raw_agents, agent_identity);
        AgentRollup::layered(carryover, &raw_agents)
    } else {
        AgentRollup::layered(carryover, &raw_agents)
    };

    // Reducer-unassigned identities belong to the projection, not the next delta's claims.
    for (index, name, name_explicit) in provisional {
        raw_agents[index].name = name;
        raw_agents[index].name_explicit = name_explicit;
        raw_agents[index].kind_ordinal = None;
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

fn restore_agent_text(agent: &mut AgentState, full: &AgentState) {
    agent.first_prompt.clone_from(&full.first_prompt);
    agent.prompt.clone_from(&full.prompt);
    agent.recent_prompts.clone_from(&full.recent_prompts);
    if let Some(pane) = &mut agent.pane {
        pane.foreground_cmdline = full
            .pane
            .as_ref()
            .and_then(|pane| pane.foreground_cmdline.clone());
        pane.spawn_command = full
            .pane
            .as_ref()
            .and_then(|pane| pane.spawn_command.clone());
    }
}

pub(crate) fn load_full_agent(paths: &StatePaths, agent: &AgentState) -> Result<AgentState> {
    let mut loaded = agent.clone();
    if agent.ended_at.is_none() {
        return Ok(loaded);
    }
    if let Some(full) = read_carryover(&paths.agents_carryover)?
        .agents
        .into_iter()
        .rev()
        .find(|full| full.kind == agent.kind && full.agent_id == agent.agent_id)
        && full.ended_at == agent.ended_at
        && full.last_seen == agent.last_seen
    {
        let events = event_log::read_all(&paths.events_log)?;
        let observed = decode_events(&events)
            .iter()
            .filter_map(agent_event_key)
            .any(|(kind, agent_id)| kind == agent.kind && agent_id == agent.agent_id);
        if !observed {
            restore_agent_text(&mut loaded, &full);
        }
    }
    Ok(loaded)
}

/// Catch the rollup up to the live log and return the fold base beside the
/// carryover-merged rollup, both shared rather than copied.
///
/// Served by this thread's held [`RollupCursor`]: the first read parses
/// `snapshots/rollup.json` and the carryover, a read after an append folds only
/// the new frames, and a read at an unchanged log and carryover opens no file.
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
    rollup_cache: PathBuf,
    agents_carryover: PathBuf,
}

impl FoldInputs {
    fn of(paths: &StatePaths) -> Self {
        Self {
            events_log: paths.events_log.clone(),
            rollup_cache: paths.rollup_cache.clone(),
            agents_carryover: paths.agents_carryover.clone(),
        }
    }
}

/// [`catch_up_rollup`] with a hook transaction's staged, unappended events
/// folded on top. With nothing staged the shared rollup passes through; with
/// events staged the reducer folds them over its own copy of the rows.
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
    let seed = agents
        .iter()
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
        agents: agents.into_values().collect::<Vec<_>>().into(),
        resume_outcomes,
    })
}

/// The rollup a store read sees inside a hook transaction: the shared fold
/// base, plus the identity state its staged events folded to.
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
    let log_len = fs::metadata(&paths.events_log)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let base = read_rollup_cache(&paths.rollup_cache)
        .filter(|cache| cache.extent.offset <= log_len)
        .map(owned_base);
    catch_up_from(base, paths, workspace_carryover(paths)?)
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
    carryover: Arc<FoldCarryover>,
) -> Result<(RollupCache, AgentRollup, Vec<ResumeOutcome>)> {
    let now = Timestamp::now();
    let project_root = carryover.project_root.clone();
    let project_root = project_root.as_deref();
    let (seed, generation, start) = match base {
        Some(RollupCache {
            extent,
            raw_agents,
            resume_outcomes,
            agent_identity,
            saw_session_rebirth,
            ..
        }) => {
            let seed: BTreeMap<(AgentKind, AgentSessionId), AgentState> = raw_agents
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
                extent.generation,
                extent.offset,
            )
        }
        None => (
            FoldDeltaSeed {
                identity: carryover.agent_identity.clone(),
                ..FoldDeltaSeed::default()
            },
            0,
            0,
        ),
    };
    let (delta, end) = event_log::read_from_offset(&paths.events_log, start)?;
    let decoded_delta = decode_events(&delta);
    let mut folded = fold_delta(seed, carryover, &decoded_delta, now)?;
    for agent in folded
        .raw_agents
        .iter_mut()
        .chain(Arc::make_mut(&mut folded.merged.live))
    {
        agent.root_lane = AgentState::is_root_lane(
            agent.channel.as_deref(),
            agent.lane_path().map(Path::new),
            project_root,
        );
    }
    let refreshed = RollupCache {
        version: ROLLUP_CACHE_VERSION,
        extent: event_log::LogExtent {
            generation,
            offset: end,
        },
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
    let current_bytes = match fs::metadata(&paths.events_log) {
        Ok(meta) => meta.len(),
        Err(source) if source.kind() == io::ErrorKind::NotFound => 0,
        Err(source) => {
            return Err(event_log::EventLogErr::Io {
                path: paths.events_log.clone(),
                source,
            }
            .into());
        }
    };
    if current_bytes == 0 || current_bytes < min_bytes {
        return Ok(read_carryover(&paths.agents_carryover)?.agents.len());
    }

    let base = read_rollup_cache(&paths.rollup_cache)
        .filter(|cache| cache.extent.offset <= current_bytes)
        .map(owned_base);
    let identity = super::assemble::WorkspaceSnapshotIdentity::from_paths(paths);
    let mut carryover = FoldCarryover::from_raw(read_carryover(&paths.agents_carryover)?);
    carryover.stamp_root_lane(identity.project_root.as_deref());
    let (cache, agents, resume_outcomes) = catch_up_from(base, paths, Arc::new(carryover))?;
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
    let carryover = read_carryover(&paths.agents_carryover)?;
    write_rollup_cache(
        &paths.rollup_cache,
        &RollupCache {
            version: ROLLUP_CACHE_VERSION,
            extent: event_log::LogExtent {
                generation: generation + 1,
                offset: 0,
            },
            raw_agents: Vec::new(),
            resume_outcomes: Vec::new(),
            agent_identity: carryover.agent_identity,
            saw_session_rebirth: false,
        },
    )
}

/// A long-lived reader's in-memory fold base: the last `RollupCache` this
/// cursor folded to, the merged rollup it produced, and the identity of the
/// log file it folded. A cursor folds each delta from memory — O(new bytes)
/// per wakeup with one `stat` each of the log and the carryover, parsing
/// neither base — and an unchanged log and carryover return the held rollup
/// without opening a file. A long-lived reader (the sidebar fetch worker)
/// owns its cursor across its loop; every other store read goes through the
/// thread's one held cursor in `catch_up_rollup`.
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
    cache: Arc<RollupCache>,
    merged: AgentRollup,
    resume_outcomes: Vec<ResumeOutcome>,
    file_id: Option<LogFileId>,
    /// The carryover stamp observed before this base was folded: GC can
    /// prune the carryover while the log stands still, so an unchanged log
    /// alone does not make the held merge current.
    carryover: FileStamp,
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
        let meta = fs::metadata(&paths.events_log).ok();
        let file_id = meta.as_ref().and_then(LogFileId::of);
        let log_len = meta.map(|meta| meta.len()).unwrap_or(0);
        // Stamped before any read, so a carryover replaced mid-fold reads as
        // changed on the next call rather than hiding behind this one.
        let carryover_stamp = StampedPath::of(&paths.agents_carryover).stamp;
        if let Some(held) = self.held.take() {
            // The held base serves warm only when the live log carries a real
            // identity that matches the one it folded; without one (a missing
            // log, a target with no dev+ino) the fold reloads `rollup.json`
            // rather than risk folding a swapped file's bytes onto it.
            let same_file = file_id.is_some() && held.file_id == file_id;
            if same_file && held.cache.extent.offset == log_len && held.carryover == carryover_stamp
            {
                // Nothing appended and the carryover unchanged: serve the
                // held fold without opening a file.
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
            if same_file
                && held.cache.extent.offset < log_len
                && let Ok(carryover) = workspace_carryover(paths)
                && let Ok((cache, merged, resume_outcomes)) =
                    catch_up_from(Some(owned_base(held.cache)), paths, carryover)
            {
                return Ok(self.hold(cache, merged, resume_outcomes, file_id, carryover_stamp));
            }
            // Identity changed, the offset regressed, or only the carryover
            // moved: the in-memory merge is stale, so reload from disk.
        }
        let (cache, merged, resume_outcomes) = catch_up_rollup_layered(paths)?;
        Ok(self.hold(cache, merged, resume_outcomes, file_id, carryover_stamp))
    }

    fn hold(
        &mut self,
        cache: RollupCache,
        merged: AgentRollup,
        resume_outcomes: Vec<ResumeOutcome>,
        file_id: Option<LogFileId>,
        carryover: FileStamp,
    ) -> (Arc<RollupCache>, AgentRollup, Vec<ResumeOutcome>) {
        let cache = Arc::new(cache);
        self.held = Some(CursorState {
            cache: Arc::clone(&cache),
            merged: merged.clone(),
            resume_outcomes: resume_outcomes.clone(),
            file_id,
            carryover,
        });
        (cache, merged, resume_outcomes)
    }
}

#[cfg(test)]
mod tests;
