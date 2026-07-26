//! Window aggregation, scoped tallies, and cross-file deduplication for spending walks.

use std::collections::{BTreeMap, HashMap, HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agents::AgentDefinition;
use crate::agents::definition::ThreadKey;

use super::cache::{CachedEntry, FileCacheEntry, SpendingDiskCache};
use super::user_input::UserInputRecord;

type FastHashMap<K, V> = HashMap<K, V, foldhash::fast::RandomState>;
type FastHashSet<K> = HashSet<K, foldhash::fast::RandomState>;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpendWindow {
    pub usd: f64,
    /// The `◇` total: `input` (cache-write folded in) + `output`. A maintained
    /// field (not derived on read) so the many `.tokens` read sites need no
    /// change.
    pub tokens: u64,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_write: u64,
    #[serde(default)]
    pub cache_read: u64,
    /// Total named tool calls observed in this window.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub tool_calls: u64,
    /// Named tool call counts in this window.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, u64>,
    /// Distinct sessions (threads) with activity in this window.
    #[serde(default)]
    pub sessions: u32,
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

impl SpendWindow {
    /// Fold one priced entry's spend and token split into the window. `input`
    /// includes cache-write at the window level, so `tokens` stays
    /// `input + output` for the `◇` total; cache-read rides its own field.
    fn add(&mut self, usd: f64, entry: &CachedEntry) {
        self.usd += usd;
        self.tokens += entry.input + entry.cache_write + entry.output;
        self.input += entry.input + entry.cache_write;
        self.output += entry.output;
        self.cache_write += entry.cache_write;
        self.cache_read += entry.cache_read;
        for (name, count) in &entry.tool_calls {
            let count = u64::from(*count);
            self.tool_calls = self.tool_calls.saturating_add(count);
            let total = self.tools.entry(name.clone()).or_default();
            *total = total.saturating_add(count);
        }
    }

    pub fn merge(&mut self, other: &Self) {
        self.usd += other.usd;
        self.tokens += other.tokens;
        self.input += other.input;
        self.output += other.output;
        self.cache_write += other.cache_write;
        self.cache_read += other.cache_read;
        self.tool_calls = self.tool_calls.saturating_add(other.tool_calls);
        for (name, count) in &other.tools {
            let total = self.tools.entry(name.clone()).or_default();
            *total = total.saturating_add(*count);
        }
        self.sessions += other.sessions;
    }

    pub fn display_tokens(&self) -> u64 {
        self.tokens.saturating_add(self.cache_read)
    }

    pub fn cache_hit_percent(&self) -> Option<u8> {
        crate::agents::context::cache_hit_percent(self.cache_read, self.input)
    }
}

/// The widest spend window. Entries at or beyond this age never contribute to
/// totals, sessions, or the unknown-model pricing chase.
pub(crate) const WIDEST_SPEND_WINDOW_SECS: u64 = 365 * 86_400;

/// Margin for the mtime-based cold-parse skip, covering filesystem timestamp
/// skew and transcript rows written just before the widest window.
pub(crate) const SKIP_PARSE_MARGIN_SECS: u64 = 2 * 86_400;

/// Raw rows newer than this stay verbatim; older rows fold into
/// day/model/thread rollups so the shared cache stays bounded by recent
/// activity instead of lifetime history. Eight days keeps the 7d window
/// second-exact with a day of margin.
pub(crate) const RAW_RETAIN_SECS: u64 = 8 * 86_400;

/// The headline spend window shown in the cockpit and provider dashboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpendWindowMode {
    /// Trailing 24 hours, matching RimZ's original "today" row behaviour.
    #[serde(rename = "24h")]
    Trailing24h,
    /// The local calendar day, using the global `timezone` when set.
    Today,
    /// The current activity burst since the last five-hour idle gap. For
    /// account-global tallies, a user prompt to any agent kind opens one shared
    /// burst for every provider; a workspace tally applies the same all-kind
    /// rule within its scope. Priced automation keeps an open burst alive.
    #[default]
    Session,
}

/// Resolved headline spend-window settings threaded into aggregation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadlineSpec {
    pub mode: SpendWindowMode,
    pub timezone: Option<String>,
}

pub(crate) const SESSION_GAP_SECS: u64 = 5 * 3_600;

/// Stable sentinel for a session headline with no user-opened burst.
pub(crate) const NO_BURST_CUTOFF: u64 = u64::MAX;

/// Rolling spend and token tally over the configured headline window plus three
/// trailing store windows: 7 days, 30 days, and 365 days. The store windows
/// nest — `year` (365 days) is the widest and subsumes the rest — while the
/// headline window is independent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpendTally {
    /// Configured headline window (`[sidebar] spend_window`): the current
    /// session by default, trailing 24 hours, or the local calendar day.
    #[serde(rename = "today")]
    pub headline: SpendWindow,
    /// Trailing 7 days.
    pub week: SpendWindow,
    /// Trailing 30 days.
    pub month: SpendWindow,
    /// Trailing 365 days — the widest window, so it subsumes the other three.
    /// `#[serde(default)]` keeps an older `provider-spending.json` (which carried
    /// an `all_time` field and no `year`) readable: the stale field is ignored and
    /// `year` defaults to zero until the producer rewrites the cache next tick.
    #[serde(default)]
    pub year: SpendWindow,
}

impl SpendTally {
    /// True when nothing has been recorded in the trailing year. `year` is the
    /// widest window, so a zero year means every window is zero.
    pub fn is_zero(&self) -> bool {
        self.year.usd == 0.0
            && self.year.tokens == 0
            && self.year.tool_calls == 0
            && self.year.tools.is_empty()
    }
}

/// The result of a spending pass: the fleet-wide total plus a per-provider
/// breakdown keyed by agent kind (`"claude"`, `"codex"`, `"pi"`). The fleet
/// store reads [`Spending::total`]; each provider dashboard panel reads its own
/// entry from [`Spending::by_provider`]. The cockpit uses a separate
/// workspace-scoped tally.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Spending {
    pub total: SpendTally,
    pub by_provider: BTreeMap<String, SpendTally>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DaySpend {
    pub usd: f64,
    /// `input` (cache-write folded in) + `output` + `cache_read`, matching
    /// `rimz stats`'s cache-inclusive `◇` token total.
    pub tokens: u64,
}

pub(crate) struct CountedRollups {
    pub(crate) spending: Spending,
    pub(crate) workspace_tally: SpendTally,
    pub(crate) workspace_headline_cutoff_secs: u64,
    pub(crate) workspace_live_baselines: BTreeMap<String, f64>,
    pub(crate) workspace_day: SpendWindow,
    pub(crate) provider_day: BTreeMap<String, SpendWindow>,
    pub(crate) day_cutoff_secs: u64,
    pub(crate) days: BTreeMap<i64, DaySpend>,
    pub(crate) models: BTreeMap<String, SpendTally>,
}

#[derive(Clone, Copy)]
pub(crate) struct HeadlineContext<'a> {
    pub(crate) user_inputs: &'a [UserInputRecord],
    pub(crate) now_secs: u64,
    pub(crate) spec: &'a HeadlineSpec,
}

pub(crate) fn aggregate_counted_rollups(
    files: &[(&'static AgentDefinition, PathBuf)],
    cache: &SpendingDiskCache,
    counted: &[impl CountedPayload],
    workspace: Option<&SpendScope>,
    headline: HeadlineContext<'_>,
    include_history_rollups: bool,
) -> CountedRollups {
    let HeadlineContext {
        user_inputs,
        now_secs,
        spec,
    } = headline;
    let workspace = workspace.filter(|scope| !scope.is_empty());
    let cutoffs = CountedCutoffs::from_events(counted, user_inputs, workspace, now_secs, spec);
    let mut spending = Spending::default();
    let mut workspace_tally = SpendTally::default();
    let day_cutoff_secs = local_day_start_secs(now_secs, spec.timezone.as_deref())
        .unwrap_or_else(|| trailing_window_cutoff(now_secs, 86_400));
    let mut workspace_day = SpendWindow::default();
    let mut provider_day = BTreeMap::<String, SpendWindow>::new();
    // These collections only establish session uniqueness. Hashing avoids
    // repeatedly ordering transcript paths while the final public maps retain
    // their deterministic `BTreeMap` representation.
    let mut workspace_day_sessions = FastHashSet::default();
    let mut workspace_session_totals = FastHashMap::<SessionKey<'_>, (u64, f64)>::default();
    let mut provider_day_sessions = BTreeMap::<String, FastHashSet<SessionKey<'_>>>::new();
    let mut days = BTreeMap::<i64, DaySpend>::new();
    let mut models = BTreeMap::<&str, SpendTally>::new();

    for counted in counted {
        let provider = counted.kind();
        let entry = counted.entry();
        accum(&mut spending.total, entry, now_secs, cutoffs.total);
        accum(
            spending.by_provider.entry(provider.to_owned()).or_default(),
            entry,
            now_secs,
            cutoffs.total,
        );
        if entry.ts_secs >= day_cutoff_secs && within_widest_window(entry.ts_secs, now_secs) {
            provider_day
                .entry(provider.to_owned())
                .or_default()
                .add(entry.cost_usd, entry);
            provider_day_sessions
                .entry(provider.to_owned())
                .or_default()
                .insert(counted.session_key());
        }
        if let Some(scope) = workspace
            && counted
                .origin()
                .is_some_and(|origin| scope.contains(origin))
        {
            accum(&mut workspace_tally, entry, now_secs, cutoffs.scoped());
            if entry.ts_secs >= day_cutoff_secs && within_widest_window(entry.ts_secs, now_secs) {
                workspace_day.add(entry.cost_usd, entry);
                workspace_day_sessions.insert(counted.session_key());
            }
            if within_widest_window(entry.ts_secs, now_secs) {
                workspace_session_totals
                    .entry(counted.session_key())
                    .and_modify(|(youngest, usd)| {
                        *youngest = (*youngest).max(entry.ts_secs);
                        *usd += entry.cost_usd;
                    })
                    .or_insert((entry.ts_secs, entry.cost_usd));
            }
        }

        if include_history_rollups {
            let day = (entry.ts_secs / 86_400) as i64;
            let cell = days.entry(day).or_default();
            cell.usd += entry.cost_usd;
            cell.tokens += entry.input + entry.cache_write + entry.output + entry.cache_read;

            accum(
                models
                    .entry(entry.model.as_deref().unwrap_or_default())
                    .or_default(),
                entry,
                now_secs,
                u64::MAX,
            );
        }
    }

    add_spending_sessions(&mut spending, files, cache, now_secs, cutoffs.total);
    if let Some(scope) = workspace {
        add_scoped_sessions(
            &mut workspace_tally,
            files,
            cache,
            scope,
            now_secs,
            cutoffs.scoped(),
        );
    }
    workspace_day.sessions = workspace_day_sessions.len().try_into().unwrap_or(u32::MAX);
    for (provider, sessions) in provider_day_sessions {
        provider_day.entry(provider).or_default().sessions =
            sessions.len().try_into().unwrap_or(u32::MAX);
    }

    CountedRollups {
        spending,
        workspace_tally,
        workspace_headline_cutoff_secs: workspace.map(|_| cutoffs.scoped()).unwrap_or_default(),
        workspace_live_baselines: workspace_session_totals
            .into_iter()
            .filter_map(|(session, (youngest, usd))| {
                within_raw_retain_window(youngest, now_secs).then_some((session.materialize(), usd))
            })
            .collect(),
        workspace_day,
        provider_day,
        day_cutoff_secs,
        days,
        models: if include_history_rollups {
            models
                .into_iter()
                .map(|(model, tally)| (model.to_owned(), tally))
                .collect()
        } else {
            BTreeMap::new()
        },
    }
}

struct CountedCutoffs {
    total: u64,
    scoped: Option<u64>,
}

impl CountedCutoffs {
    fn from_events(
        counted: &[impl CountedPayload],
        user_inputs: &[UserInputRecord],
        scope: Option<&SpendScope>,
        now_secs: u64,
        spec: &HeadlineSpec,
    ) -> Self {
        if let Some(cutoff) = uniform_headline_cutoff(spec, now_secs) {
            return Self {
                total: cutoff,
                scoped: scope.map(|_| cutoff),
            };
        }

        let mut total_activity = Vec::new();
        let mut scoped_activity = Vec::new();
        for counted in counted {
            let ts_secs = counted.entry().ts_secs;
            total_activity.push(ts_secs);
            if let Some(scope) = scope
                && counted
                    .origin()
                    .is_some_and(|origin| scope.contains(origin))
            {
                scoped_activity.push(ts_secs);
            }
        }

        let mut total_prompts = Vec::new();
        let mut scoped_prompts = Vec::new();
        for record in user_inputs {
            let Ok(ts_secs) = u64::try_from(record.at.as_second()) else {
                continue;
            };
            total_prompts.push(ts_secs);
            if let Some(scope) = scope
                && record
                    .origin
                    .as_deref()
                    .is_some_and(|origin| scope.contains(origin))
            {
                scoped_prompts.push(ts_secs);
            }
        }

        Self {
            total: burst_cutoff(&total_prompts, &total_activity, now_secs),
            scoped: scope.map(|_| burst_cutoff(&scoped_prompts, &scoped_activity, now_secs)),
        }
    }

    fn scoped(&self) -> u64 {
        self.scoped.unwrap_or(NO_BURST_CUTOFF)
    }
}

/// Session counts, keyed by provider-native thread id when one is available
/// and by transcript file grouping otherwise. A Claude session's subagent
/// files fold under its `session_id` directory so one thread counts once.
/// Each thread is single-provider; we track its youngest entry and bump every
/// window that youngest reading still falls within. Counted from the raw
/// cached entries (not the deduped set) since a thread that ran is a thread,
/// regardless of which file a duplicated turn was kept in.
fn add_spending_sessions(
    spending: &mut Spending,
    files: &[(&'static AgentDefinition, PathBuf)],
    cache: &SpendingDiskCache,
    now_secs: u64,
    headline_cutoff: u64,
) {
    let mut threads = FastHashMap::<SessionKey<'_>, (&'static str, u64)>::with_capacity_and_hasher(
        files.len(),
        foldhash::fast::RandomState::default(),
    );
    for (adapter, file) in files {
        let cache_key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get(&cache_key) else {
            continue;
        };
        for entry in &cached_file.entries {
            threads
                .entry(session_key(adapter, file, entry))
                .and_modify(|(_, ts)| *ts = (*ts).max(entry.ts_secs))
                .or_insert((adapter.spec().kind, entry.ts_secs));
        }
    }
    for (provider, youngest) in threads.values() {
        bump_sessions(&mut spending.total, *youngest, now_secs, headline_cutoff);
        bump_sessions(
            spending
                .by_provider
                .entry((*provider).to_owned())
                .or_default(),
            *youngest,
            now_secs,
            headline_cutoff,
        );
    }
}

fn add_scoped_sessions(
    tally: &mut SpendTally,
    files: &[(&'static AgentDefinition, PathBuf)],
    cache: &SpendingDiskCache,
    scope: &SpendScope,
    now_secs: u64,
    headline_cutoff: u64,
) {
    let mut threads = FastHashMap::<SessionKey<'_>, u64>::default();
    for (adapter, file) in files {
        let cache_key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get(&cache_key) else {
            continue;
        };
        if !cached_file
            .origin_path
            .as_deref()
            .is_some_and(|origin| scope.contains(origin))
        {
            continue;
        }
        for entry in &cached_file.entries {
            threads
                .entry(session_key(adapter, file, entry))
                .and_modify(|ts| *ts = (*ts).max(entry.ts_secs))
                .or_insert(entry.ts_secs);
        }
    }
    for youngest in threads.values() {
        bump_sessions(tally, *youngest, now_secs, headline_cutoff);
    }
}

/// The roots that define one cockpit scope: the project root plus grouped
/// worktree roots. Roots are lexical absolute paths; unreadable or relative
/// origins do not enter the scope.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpendScope {
    roots: Vec<PathBuf>,
}

impl SpendScope {
    pub fn from_roots(project_root: Option<&Path>, worktree_roots: &[PathBuf]) -> Self {
        Self::for_workspace(project_root, worktree_roots, None)
    }

    /// The cockpit scope for a room: its project root, the live `git worktree
    /// list` checkout roots, and — the durable part — the repo's worktree-home
    /// directory (the resolved `[agents.worktree] dir` template, e.g.
    /// `…/<repo>-worktrees`). The home is a path prefix, so a session recorded
    /// under a worktree that has since been removed still scopes in, where the
    /// live worktree list alone would drop it the moment cleanup ran.
    pub fn for_workspace(
        project_root: Option<&Path>,
        worktree_roots: &[PathBuf],
        worktree_home: Option<&Path>,
    ) -> Self {
        let mut roots: Vec<PathBuf> = project_root
            .into_iter()
            .chain(worktree_roots.iter().map(PathBuf::as_path))
            .chain(worktree_home)
            .map(crate::worktree::normalize_path_lexical)
            .filter(|root| root.is_absolute())
            .collect();
        roots.sort();
        roots.dedup();
        Self { roots }
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    pub fn hash(&self) -> String {
        let mut hasher = Sha256::new();
        for root in &self.roots {
            hasher.update(root.to_string_lossy().as_bytes());
            hasher.update([0]);
        }
        hex::encode(hasher.finalize())
    }

    pub(crate) fn contains(&self, origin: &Path) -> bool {
        if !origin.is_absolute() {
            return false;
        }
        // Parser and trusted-override origins are normalized when cached. Keep
        // a defensive slow path for a hand-edited or older cache without
        // allocating a fresh `PathBuf` for every entry in the common case.
        let normalized;
        let origin = if origin
            .components()
            .any(|component| component == std::path::Component::ParentDir)
        {
            normalized = crate::worktree::normalize_path_lexical(origin);
            normalized.as_path()
        } else {
            origin
        };
        if !origin.is_absolute() {
            return false;
        }
        self.roots.iter().any(|root| origin.starts_with(root))
    }
}

pub(crate) fn stamp_file_origin(entry: &mut FileCacheEntry, origin: &Path) -> bool {
    let origin = crate::worktree::normalize_path_lexical(origin);
    if entry.origin_path.as_ref() != Some(&origin) {
        entry.origin_path = Some(origin.clone());
        return true;
    }
    false
}

pub(crate) fn origin_path(raw: Option<&str>) -> Option<PathBuf> {
    normalized_absolute_path(&PathBuf::from(raw?.trim()))
}

pub(crate) fn normalized_absolute_path(path: &Path) -> Option<PathBuf> {
    let normalized = crate::worktree::normalize_path_lexical(path);
    normalized.is_absolute().then_some(normalized)
}

pub(crate) trait DedupPayload {
    fn entry(&self) -> &CachedEntry;
}

pub(crate) trait CountedPayload: DedupPayload {
    fn kind(&self) -> &'static str;
    fn origin(&self) -> Option<&Path>;
    fn session_key(&self) -> SessionKey<'_>;
}

pub(crate) struct SidechainDedup<P> {
    by_exact_key: FastHashMap<(String, Option<String>), P>,
    by_provider_key: FastHashMap<String, P>,
    msg_has_non_sidechain: FastHashMap<String, bool>,
    free: Vec<P>,
}

impl<P> Default for SidechainDedup<P> {
    fn default() -> Self {
        Self {
            by_exact_key: FastHashMap::default(),
            by_provider_key: FastHashMap::default(),
            msg_has_non_sidechain: FastHashMap::default(),
            free: Vec::new(),
        }
    }
}

impl<P: DedupPayload> SidechainDedup<P> {
    pub(crate) fn insert(&mut self, payload: P) {
        let Some(msg_id) = payload.entry().message_id.clone() else {
            if let Some(key) = payload.entry().dedup_key.clone() {
                self.by_provider_key.entry(key).or_insert(payload);
                return;
            }
            self.free.push(payload);
            return;
        };
        let request_id = payload.entry().request_id.clone();
        let is_sidechain = payload.entry().is_sidechain;
        let has_non_sidechain = self
            .msg_has_non_sidechain
            .entry(msg_id.clone())
            .or_insert(false);
        if !is_sidechain {
            *has_non_sidechain = true;
        }
        let exact_key = (msg_id, request_id);
        match self.by_exact_key.entry(exact_key) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if should_replace_duplicate(payload.entry(), entry.get().entry()) {
                    entry.insert(payload);
                }
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(payload);
            }
        }
    }

    pub(crate) fn into_counted(self) -> Vec<P> {
        let mut counted = Vec::new();
        let mut keyed = self.by_exact_key.into_iter().collect::<Vec<_>>();
        keyed.sort_by(|(left, _), (right, _)| left.cmp(right));
        for ((msg_id, _), payload) in keyed {
            let is_sidechain_replay = payload.entry().is_sidechain
                && self
                    .msg_has_non_sidechain
                    .get(msg_id.as_str())
                    .copied()
                    .unwrap_or(false);
            if !is_sidechain_replay {
                counted.push(payload);
            }
        }
        let mut provider_keyed = self.by_provider_key.into_iter().collect::<Vec<_>>();
        provider_keyed.sort_by(|(left, _), (right, _)| left.cmp(right));
        counted.extend(provider_keyed.into_iter().map(|(_, payload)| payload));
        counted.extend(self.free);
        counted
    }
}

pub(crate) fn should_replace_duplicate(candidate: &CachedEntry, existing: &CachedEntry) -> bool {
    if candidate.is_sidechain != existing.is_sidechain {
        return existing.is_sidechain;
    }

    should_replace_usage_duplicate(
        entry_token_total(candidate),
        candidate.has_speed,
        entry_token_total(existing),
        existing.has_speed,
    )
}

/// Prefer the more complete form of one provider request. Kept independent of
/// [`CachedEntry`] so incremental consumers can apply the same replacement
/// policy without materializing a historical spending row.
pub(crate) fn should_replace_usage_duplicate(
    candidate_tokens: u64,
    candidate_has_speed: bool,
    existing_tokens: u64,
    existing_has_speed: bool,
) -> bool {
    if candidate_tokens != existing_tokens {
        return candidate_tokens > existing_tokens;
    }

    candidate_has_speed && !existing_has_speed
}

fn entry_token_total(entry: &CachedEntry) -> u64 {
    entry
        .input
        .saturating_add(entry.output)
        .saturating_add(entry.cache_write)
        .saturating_add(entry.cache_read)
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum SessionKey<'a> {
    Native {
        kind: &'static str,
        thread_id: &'a str,
    },
    File(&'a Path),
}

impl SessionKey<'_> {
    fn materialize(self) -> String {
        match self {
            Self::Native { kind, thread_id } => format!("{kind}:{thread_id}"),
            Self::File(path) => path.to_string_lossy().into_owned(),
        }
    }
}

pub(crate) struct Counted<'a> {
    kind: &'static str,
    origin: Option<&'a Path>,
    session_key: SessionKey<'a>,
    entry: &'a CachedEntry,
}

impl DedupPayload for Counted<'_> {
    fn entry(&self) -> &CachedEntry {
        self.entry
    }
}

impl CountedPayload for Counted<'_> {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn origin(&self) -> Option<&Path> {
        self.origin
    }

    fn session_key(&self) -> SessionKey<'_> {
        self.session_key
    }
}

/// Exact position of one dedup winner in the file table keyed by the walker's
/// cache generation and spending-file signature.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CountedLocation {
    pub(crate) file_index: usize,
    pub(crate) entry_index: usize,
}

struct LocatedCounted<'a> {
    location: CountedLocation,
    entry: &'a CachedEntry,
}

impl DedupPayload for LocatedCounted<'_> {
    fn entry(&self) -> &CachedEntry {
        self.entry
    }
}

pub(crate) struct IndexedCounted<'a> {
    kind: &'static str,
    origin: Option<&'a Path>,
    session_key: SessionKey<'a>,
    entry: &'a CachedEntry,
}

impl DedupPayload for IndexedCounted<'_> {
    fn entry(&self) -> &CachedEntry {
        self.entry
    }
}

impl CountedPayload for IndexedCounted<'_> {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn origin(&self) -> Option<&Path> {
        self.origin
    }

    fn session_key(&self) -> SessionKey<'_> {
        self.session_key
    }
}

pub(crate) fn dedup_cached_entries<'a>(
    files: &'a [(&'static AgentDefinition, PathBuf)],
    cache: &'a SpendingDiskCache,
) -> SidechainDedup<Counted<'a>> {
    dedup_cached_entries_with(files, cache, |adapter, file, kind, cached_file, entry| {
        Counted {
            kind,
            origin: cached_file.origin_path.as_deref(),
            session_key: session_key(adapter, file, entry),
            entry,
        }
    })
}

pub(crate) fn dedup_cached_entry_locations(
    files: &[(&'static AgentDefinition, PathBuf)],
    cache: &SpendingDiskCache,
) -> Vec<CountedLocation> {
    let mut deduped = SidechainDedup::default();
    for (file_index, (_, file)) in files.iter().enumerate() {
        let key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get(&key) else {
            continue;
        };
        for (entry_index, entry) in cached_file.entries.iter().enumerate() {
            deduped.insert(LocatedCounted {
                location: CountedLocation {
                    file_index,
                    entry_index,
                },
                entry,
            });
        }
    }
    deduped
        .into_counted()
        .into_iter()
        .map(|counted| counted.location)
        .collect()
}

pub(crate) fn indexed_counted_entries<'a>(
    files: &'a [(&'static AgentDefinition, PathBuf)],
    cache: &'a SpendingDiskCache,
    locations: &[CountedLocation],
) -> Vec<IndexedCounted<'a>> {
    struct IndexedFile<'a> {
        adapter: &'static AgentDefinition,
        path: &'a Path,
        cache: &'a FileCacheEntry,
    }

    let table = files
        .iter()
        .map(|(adapter, file)| {
            let key = file.to_string_lossy().into_owned();
            cache.files.get(&key).map(|cached| IndexedFile {
                adapter,
                path: file,
                cache: cached,
            })
        })
        .collect::<Vec<_>>();

    locations
        .iter()
        .map(|location| {
            // The memo key includes the exact file signature and cache
            // generation that built these locations, so invalid indices are
            // an impossible internal state rather than recoverable input.
            let file = table
                .get(location.file_index)
                .and_then(Option::as_ref)
                .expect("counted location must resolve under its memo key");
            let entry = file
                .cache
                .entries
                .get(location.entry_index)
                .expect("counted entry must resolve under its memo key");
            IndexedCounted {
                kind: file.adapter.spec().kind,
                origin: file.cache.origin_path.as_deref(),
                session_key: session_key(file.adapter, file.path, entry),
                entry,
            }
        })
        .collect()
}

fn dedup_cached_entries_with<'a, P: DedupPayload>(
    files: &'a [(&'static AgentDefinition, PathBuf)],
    cache: &'a SpendingDiskCache,
    make: impl Fn(
        &'static AgentDefinition,
        &'a Path,
        &'static str,
        &'a FileCacheEntry,
        &'a CachedEntry,
    ) -> P,
) -> SidechainDedup<P> {
    let mut deduped = SidechainDedup::default();
    for (adapter, file) in files {
        let kind = adapter.spec().kind;
        let key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get(&key) else {
            continue;
        };
        for entry in &cached_file.entries {
            deduped.insert(make(adapter, file, kind, cached_file, entry));
        }
    }
    deduped
}

fn uniform_headline_cutoff(spec: &HeadlineSpec, now_secs: u64) -> Option<u64> {
    match spec.mode {
        SpendWindowMode::Trailing24h => Some(trailing_window_cutoff(now_secs, 86_400)),
        SpendWindowMode::Today => Some(
            local_day_start_secs(now_secs, spec.timezone.as_deref())
                .unwrap_or_else(|| trailing_window_cutoff(now_secs, 86_400)),
        ),
        SpendWindowMode::Session => None,
    }
}

fn trailing_window_cutoff(now_secs: u64, span_secs: u64) -> u64 {
    now_secs
        .checked_sub(span_secs)
        .map_or(0, |cutoff| cutoff.saturating_add(1))
}

fn local_day_start_secs(now_secs: u64, tz: Option<&str>) -> Option<u64> {
    let now = Timestamp::from_second(i64::try_from(now_secs).ok()?).ok()?;
    let zone = crate::config::resolve_time_zone(tz);
    let start = now.to_zoned(zone).start_of_day().ok()?.timestamp();
    u64::try_from(start.as_second()).ok()
}

fn burst_cutoff(prompts: &[u64], activity: &[u64], now_secs: u64) -> u64 {
    let mut events = prompts
        .iter()
        .copied()
        .map(|ts| (ts, true))
        .chain(activity.iter().copied().map(|ts| (ts, false)))
        .collect::<Vec<_>>();
    events.sort_unstable();

    let mut newer = now_secs;
    let mut oldest_prompt = None;
    for (ts_secs, is_prompt) in events.into_iter().rev() {
        if newer.saturating_sub(ts_secs) >= SESSION_GAP_SECS {
            break;
        }
        if is_prompt {
            oldest_prompt = Some(ts_secs);
        }
        newer = ts_secs;
    }
    oldest_prompt.unwrap_or(NO_BURST_CUTOFF)
}

fn accum(tally: &mut SpendTally, entry: &CachedEntry, now_secs: u64, headline_cutoff: u64) {
    let usd = entry.cost_usd;
    // Store-window bucketing: an entry counts toward each trailing window whose
    // span it still falls within. The configured headline window is independent
    // and may be calendar-day or session scoped.
    if !within_widest_window(entry.ts_secs, now_secs) {
        return;
    }
    let age = now_secs.saturating_sub(entry.ts_secs);
    tally.year.add(usd, entry);
    if age < 30 * 86_400 {
        tally.month.add(usd, entry);
    }
    if age < 7 * 86_400 {
        tally.week.add(usd, entry);
    }
    if entry.ts_secs >= headline_cutoff {
        tally.headline.add(usd, entry);
    }
}

/// Count one session (thread) toward each trailing store window its youngest
/// entry still falls within, plus the configured headline window.
fn bump_sessions(tally: &mut SpendTally, youngest_ts: u64, now_secs: u64, headline_cutoff: u64) {
    let age = now_secs.saturating_sub(youngest_ts);
    if age >= WIDEST_SPEND_WINDOW_SECS {
        return;
    }
    tally.year.sessions += 1;
    if age < 30 * 86_400 {
        tally.month.sessions += 1;
    }
    if age < 7 * 86_400 {
        tally.week.sessions += 1;
    }
    if youngest_ts >= headline_cutoff {
        tally.headline.sessions += 1;
    }
}

pub(crate) fn within_widest_window(ts_secs: u64, now_secs: u64) -> bool {
    now_secs.saturating_sub(ts_secs) < WIDEST_SPEND_WINDOW_SECS
}

/// A file last modified before the widest spend window (plus a clock-skew
/// margin) holds no in-window entries, so cold-parsing or retaining it is pure
/// waste.
pub(crate) fn cold_parse_out_of_window(mtime_secs: u64, now_secs: u64) -> bool {
    mtime_secs.saturating_add(WIDEST_SPEND_WINDOW_SECS + SKIP_PARSE_MARGIN_SECS) < now_secs
}

pub(crate) fn within_raw_retain_window(ts_secs: u64, now_secs: u64) -> bool {
    now_secs.saturating_sub(ts_secs) < RAW_RETAIN_SECS
}

/// The thread a priced entry belongs to. Providers with native thread ids use
/// those ids; otherwise the adapter's declared [`ThreadKey`] maps transcript
/// paths to threads. A session-dir provider (Claude) spreads one session across
/// a main `…/<session_id>/chat.jsonl` plus subagent
/// `…/<session_id>/subagents/*.jsonl` files, so both fold under the
/// `<session_id>` directory and one thread counts once; a per-file provider
/// (Codex, Pi) keys on the file path.
fn session_key<'a>(
    adapter: &'static AgentDefinition,
    path: &'a Path,
    entry: &'a CachedEntry,
) -> SessionKey<'a> {
    if let Some(thread_id) = entry.thread_id.as_deref().map(str::trim)
        && !thread_id.is_empty()
    {
        return SessionKey::Native {
            kind: adapter.spec().kind,
            thread_id,
        };
    }

    SessionKey::File(file_grouping_session_path(adapter, path))
}

pub(crate) fn live_session_keys(
    adapter: &AgentDefinition,
    session_id: &str,
    transcript_path: &Path,
) -> Vec<String> {
    let mut keys = Vec::new();
    let session_id = session_id.trim();
    if !session_id.is_empty() {
        keys.push(
            SessionKey::Native {
                kind: adapter.spec().kind,
                thread_id: session_id,
            }
            .materialize(),
        );
    }
    keys.push(SessionKey::File(file_grouping_session_path(adapter, transcript_path)).materialize());
    keys.sort();
    keys.dedup();
    keys
}

fn file_grouping_session_path<'a>(adapter: &AgentDefinition, path: &'a Path) -> &'a Path {
    let dir = match adapter.spec().thread_key {
        ThreadKey::SessionDir => {
            let parent = path.parent();
            match parent {
                Some(p) if p.file_name().and_then(|name| name.to_str()) == Some("subagents") => {
                    p.parent()
                }
                other => other,
            }
        }
        ThreadKey::PerFile => None,
    };
    dir.unwrap_or(path)
}

pub(crate) fn spending_files_signature(files: &[(&'static AgentDefinition, PathBuf)]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for (adapter, file) in files {
        adapter.spec().kind.hash(&mut hasher);
        file.hash(&mut hasher);
    }
    hasher.finish()
}
