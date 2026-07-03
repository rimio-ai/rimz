//! Window aggregation, scoped tallies, and cross-file deduplication for spending walks.

use std::collections::{BTreeMap, HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agents::AgentAdapter;
use crate::agents::descriptor::ThreadKey;

use super::cache::{CachedEntry, FileCacheEntry, SpendingDiskCache};

type FastHashMap<K, V> = HashMap<K, V, foldhash::fast::RandomState>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
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
    /// Distinct sessions (threads) with activity in this window.
    #[serde(default)]
    pub sessions: u32,
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
    /// Trailing 24 hours, matching Rimz's original "today" row behaviour.
    #[serde(rename = "24h")]
    Trailing24h,
    /// The local calendar day, using the global `timezone` when set.
    Today,
    /// The current activity burst since the last five-hour idle gap.
    #[default]
    Session,
}

/// Resolved headline spend-window settings threaded into aggregation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeadlineSpec {
    pub mode: SpendWindowMode,
    pub timezone: Option<String>,
}

const SESSION_GAP_SECS: u64 = 5 * 3_600;

/// Rolling spend and token tally over the configured headline window plus three
/// trailing ledger windows: 7 days, 30 days, and 365 days. The ledger windows
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
        self.year.usd == 0.0 && self.year.tokens == 0
    }
}

/// The result of a spending pass: the fleet-wide total plus a per-provider
/// breakdown keyed by agent kind (`"claude"`, `"codex"`, `"pi"`). The fleet
/// ledger reads [`Spending::total`]; each provider dashboard panel reads its own
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
    pub(crate) days: BTreeMap<i64, DaySpend>,
    pub(crate) models: BTreeMap<String, SpendTally>,
}

pub(crate) fn aggregate_counted_rollups(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    counted: &[impl CountedPayload],
    scope: Option<&SpendScope>,
    now_secs: u64,
    spec: &HeadlineSpec,
    include_history_rollups: bool,
) -> CountedRollups {
    let scope = scope.filter(|scope| !scope.is_empty());
    let cutoffs = CountedCutoffs::from_counted(counted, scope, now_secs, spec);
    let mut spending = Spending::default();
    let mut workspace_tally = SpendTally::default();
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
            cutoffs.provider(provider),
        );
        if let Some(scope) = scope
            && counted
                .origin()
                .is_some_and(|origin| scope.contains(origin))
        {
            accum(&mut workspace_tally, entry, now_secs, cutoffs.scoped());
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

    add_spending_sessions(&mut spending, files, cache, now_secs, &cutoffs);
    if let Some(scope) = scope {
        add_scoped_sessions(
            &mut workspace_tally,
            files,
            cache,
            scope,
            now_secs,
            cutoffs.scoped(),
        );
    }

    CountedRollups {
        spending,
        workspace_tally,
        workspace_headline_cutoff_secs: scope.map(|_| cutoffs.scoped()).unwrap_or_default(),
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
    uniform: Option<u64>,
    total: u64,
    provider: HashMap<&'static str, u64>,
    scoped: Option<u64>,
    empty_session_cutoff: u64,
}

impl CountedCutoffs {
    fn from_counted(
        counted: &[impl CountedPayload],
        scope: Option<&SpendScope>,
        now_secs: u64,
        spec: &HeadlineSpec,
    ) -> Self {
        let uniform = uniform_headline_cutoff(spec, now_secs);
        let empty_session_cutoff = session_cutoff_secs(&[], now_secs);
        if let Some(cutoff) = uniform {
            return Self {
                uniform,
                total: cutoff,
                provider: HashMap::new(),
                scoped: scope.map(|_| cutoff),
                empty_session_cutoff,
            };
        }

        let mut total_timestamps = Vec::new();
        let mut provider_timestamps: HashMap<&'static str, Vec<u64>> = HashMap::new();
        let mut scoped_timestamps = Vec::new();
        for counted in counted {
            let ts_secs = counted.entry().ts_secs;
            total_timestamps.push(ts_secs);
            provider_timestamps
                .entry(counted.kind())
                .or_default()
                .push(ts_secs);
            if let Some(scope) = scope
                && counted
                    .origin()
                    .is_some_and(|origin| scope.contains(origin))
            {
                scoped_timestamps.push(ts_secs);
            }
        }

        Self {
            uniform: None,
            total: session_cutoff_secs(total_timestamps.as_slice(), now_secs),
            provider: provider_timestamps
                .into_iter()
                .map(|(provider, timestamps)| {
                    (
                        provider,
                        session_cutoff_secs(timestamps.as_slice(), now_secs),
                    )
                })
                .collect(),
            scoped: scope.map(|_| session_cutoff_secs(scoped_timestamps.as_slice(), now_secs)),
            empty_session_cutoff,
        }
    }

    fn provider(&self, provider: &'static str) -> u64 {
        self.uniform
            .or_else(|| self.provider.get(provider).copied())
            .unwrap_or(self.empty_session_cutoff)
    }

    fn scoped(&self) -> u64 {
        self.scoped.unwrap_or(self.empty_session_cutoff)
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
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    now_secs: u64,
    cutoffs: &CountedCutoffs,
) {
    let mut threads: HashMap<String, (&'static str, u64)> = HashMap::new();
    for (adapter, file) in files {
        let cache_key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get(&cache_key) else {
            continue;
        };
        for entry in &cached_file.entries {
            threads
                .entry(session_key(*adapter, file, entry))
                .and_modify(|(_, ts)| *ts = (*ts).max(entry.ts_secs))
                .or_insert((adapter.descriptor().kind, entry.ts_secs));
        }
    }
    for (provider, youngest) in threads.values() {
        bump_sessions(&mut spending.total, *youngest, now_secs, cutoffs.total);
        bump_sessions(
            spending
                .by_provider
                .entry((*provider).to_owned())
                .or_default(),
            *youngest,
            now_secs,
            cutoffs.provider(provider),
        );
    }
}

fn add_scoped_sessions(
    tally: &mut SpendTally,
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    scope: &SpendScope,
    now_secs: u64,
    headline_cutoff: u64,
) {
    let mut threads: HashMap<String, u64> = HashMap::new();
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
                .entry(session_key(*adapter, file, entry))
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
        let origin = crate::worktree::normalize_path_lexical(origin);
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
}

pub(crate) struct SidechainDedup<P> {
    by_exact_key: FastHashMap<(String, Option<String>), P>,
    msg_has_non_sidechain: FastHashMap<String, bool>,
    free: Vec<P>,
}

impl<P> Default for SidechainDedup<P> {
    fn default() -> Self {
        Self {
            by_exact_key: FastHashMap::default(),
            msg_has_non_sidechain: FastHashMap::default(),
            free: Vec::new(),
        }
    }
}

impl<P: DedupPayload> SidechainDedup<P> {
    pub(crate) fn insert(&mut self, payload: P) {
        let Some(msg_id) = payload.entry().message_id.clone() else {
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
                if entry.get().entry().is_sidechain && !is_sidechain {
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
        counted.extend(self.free);
        counted
    }
}

pub(crate) struct Counted<'a> {
    kind: &'static str,
    origin: Option<&'a Path>,
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
}

#[derive(Clone, Debug)]
pub(crate) struct OwnedCounted {
    kind: &'static str,
    origin: Option<PathBuf>,
    entry: CachedEntry,
}

impl DedupPayload for OwnedCounted {
    fn entry(&self) -> &CachedEntry {
        &self.entry
    }
}

impl CountedPayload for OwnedCounted {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn origin(&self) -> Option<&Path> {
        self.origin.as_deref()
    }
}

pub(crate) fn dedup_cached_entries<'a>(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &'a SpendingDiskCache,
) -> SidechainDedup<Counted<'a>> {
    let mut deduped = SidechainDedup::default();
    for (adapter, file) in files {
        let kind = adapter.descriptor().kind;
        let key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get(&key) else {
            continue;
        };
        for entry in &cached_file.entries {
            deduped.insert(Counted {
                kind,
                origin: cached_file.origin_path.as_deref(),
                entry,
            });
        }
    }
    deduped
}

pub(crate) fn dedup_cached_entries_owned(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
) -> SidechainDedup<OwnedCounted> {
    let mut deduped = SidechainDedup::default();
    for (adapter, file) in files {
        let kind = adapter.descriptor().kind;
        let key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get(&key) else {
            continue;
        };
        for entry in &cached_file.entries {
            deduped.insert(OwnedCounted {
                kind,
                origin: cached_file.origin_path.clone(),
                entry: entry.clone(),
            });
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

fn session_cutoff_secs(timestamps: &[u64], now_secs: u64) -> u64 {
    let mut sorted = timestamps.to_vec();
    sorted.sort_unstable();
    let Some(&newest) = sorted.last() else {
        return now_secs.saturating_add(1);
    };
    if now_secs.saturating_sub(newest) >= SESSION_GAP_SECS {
        return now_secs.saturating_add(1);
    }

    let mut oldest = newest;
    let mut newer = newest;
    for &ts_secs in sorted[..sorted.len() - 1].iter().rev() {
        if newer.saturating_sub(ts_secs) >= SESSION_GAP_SECS {
            break;
        }
        oldest = ts_secs;
        newer = ts_secs;
    }
    oldest
}

fn accum(tally: &mut SpendTally, entry: &CachedEntry, now_secs: u64, headline_cutoff: u64) {
    let usd = entry.cost_usd;
    // Ledger-window bucketing: an entry counts toward each trailing window whose
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

/// Count one session (thread) toward each trailing ledger window its youngest
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
fn session_key(adapter: &dyn AgentAdapter, path: &Path, entry: &CachedEntry) -> String {
    if let Some(thread_id) = entry.thread_id.as_deref().map(str::trim)
        && !thread_id.is_empty()
    {
        return format!("{}:{thread_id}", adapter.descriptor().kind);
    }

    let dir = match adapter.descriptor().thread_key {
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
    dir.unwrap_or(path).to_string_lossy().into_owned()
}

pub(crate) fn spending_files_signature(files: &[(&'static dyn AgentAdapter, PathBuf)]) -> u64 {
    let mut hasher = DefaultHasher::new();
    for (adapter, file) in files {
        adapter.descriptor().kind.hash(&mut hasher);
        file.hash(&mut hasher);
    }
    hasher.finish()
}
