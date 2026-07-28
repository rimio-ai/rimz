//! JSONL-based spending aggregation over agent transcript history.
//!
//! Per-provider typed parsers live in each adapter's `spend.rs`; this module
//! discovers transcript stores, folds per-slot lifetime effort directly from
//! session transcripts, refreshes the shared cursor cache through the
//! elected [`service`] owner of one warm [`SpendingWalker`], aggregates
//! account-global and workspace-scoped windows, and publishes stamped
//! provider/workspace caches. Discovery and parsing
//! dispatch through the adapter ([`AgentDefinition::spending_sources`] /
//! [`AgentDefinition::parse_spend`]): a dollar-logging provider (Claude's legacy
//! `costUSD`, Pi) reads its figures verbatim, a token-only provider (Codex,
//! current Claude) multiplies counts through the
//! [`PriceBook`](super::pricing) — either way every file yields
//! [`CachedEntry`] values with one per-file origin and buckets under its
//! adapter's kind.

mod aggregate;
mod cache;
mod discovery;
mod effort;
mod engine;
mod publish;
mod refresh;
pub mod service;
mod time;
pub mod user_input;

use std::cell::Cell;
#[cfg(any(test, feature = "testkit"))]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::pricing::PriceBook;
use super::{AgentCost, AgentDefinition};

/// How long a published fleet-spending walk remains fresh.
pub const SPENDING_TTL: Duration = Duration::from_secs(15);

/// Maximum age served while another producer owns the global walk.
pub(crate) const SPENDING_STALE_GRACE: Duration = Duration::from_secs(90);

pub(crate) fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

#[cfg(test)]
pub(crate) use aggregate::CountedPayload;
pub(crate) use aggregate::{
    CountedLocation, HeadlineContext, NO_BURST_CUTOFF, SESSION_GAP_SECS, aggregate_counted_rollups,
    dedup_cached_entries, dedup_cached_entry_locations, indexed_counted_entries, live_session_keys,
    origin_path, should_replace_usage_duplicate, spending_files_signature,
};
pub use aggregate::{
    DaySpend, HeadlineSpec, SpendScope, SpendTally, SpendWindow, SpendWindowMode, Spending,
};
#[cfg(test)]
pub(crate) use aggregate::{RAW_RETAIN_SECS, cold_parse_out_of_window};
#[cfg(test)]
pub(crate) use aggregate::{SKIP_PARSE_MARGIN_SECS, WIDEST_SPEND_WINDOW_SECS};
pub(crate) use cache::{CacheStamp, SPENDING_CACHE_VERSION, cache_stamp};
pub use cache::{
    CachedEntry, FileCacheEntry, SpendCursor, SpendParse, SpendingDiskCache, read_spending_cache,
    write_spending_cache,
};
#[cfg(test)]
pub(crate) use cache::{compact_spending_cache, peek_cache_version};
pub use discovery::{SpendingSource, SpendingSourceGroup, SpendingSourceTree};
pub use effort::{
    EffortParseMemo, EffortSessionRef, EffortTokens, SlotEffort, SlotEffortBreakdown, slot_effort,
    slot_effort_breakdown, slot_effort_with_memo, sum_optional_cost,
};
#[doc(hidden)]
pub use engine::refresh_global_spending_direct;
pub(crate) use publish::{
    PROVIDER_SPENDING_VERSION, WORKSPACE_SPENDING_VERSION, write_provider_spending_cache_value,
};
pub use publish::{
    ProviderSpendingCache, WorkspaceSpendingCache, read_provider_spending_cache,
    read_workspace_spending_cache, write_provider_spending_cache,
    write_provider_spending_cache_with_day, write_provider_spending_cache_with_rollups,
    write_workspace_spending_cache,
};
pub(crate) use refresh::{
    RefreshCallbacks, SplitPrice, is_priceable_model_name, lookup_split_price, price_split,
    record_unknown_model, recorded_unknown_models, refresh_spending_cache,
};
pub(crate) use time::iso_to_unix_secs;
pub use time::{unix_secs_now, utc_date};

/// Cadence for cursor-cache checkpoints and partial aggregate publishes during
/// a cold spending-history walk.
pub(crate) const WALK_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);

const SPENDING_PERSIST_MIN_INTERVAL: u64 = 5 * 60;
const SPENDING_PERSIST_PARSE_BYTES: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpendProgress {
    pub finished_files: usize,
    pub total_files: usize,
}

/// Periodic side effects during a long cold walk.
pub trait WalkObserver {
    fn on_file(&mut self, _progress: SpendProgress) {}
    fn on_interval(&mut self, _cache: &SpendingDiskCache) {}
}

pub struct SilentWalk;

impl WalkObserver for SilentWalk {}

/// Spending data published for one sidebar enrichment fold: account-global
/// provider totals plus the room-local cockpit tally.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SpendingCaches {
    pub provider: ProviderSpendingCache,
    pub workspace: WorkspaceSpendingCache,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpendingWalkResult {
    pub spending: Spending,
    pub workspace_tally: SpendTally,
    pub workspace_headline_cutoff_secs: u64,
    pub workspace_live_baselines: BTreeMap<String, f64>,
    pub workspace_day: SpendWindow,
    pub provider_day: BTreeMap<String, SpendWindow>,
    pub day_cutoff_secs: u64,
    pub days: BTreeMap<i64, DaySpend>,
    pub models: BTreeMap<String, SpendTally>,
    pub stats: WalkStats,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WalkStats {
    pub dedup_passes: u32,
    pub cache_parsed: bool,
    pub cache_written: bool,
    /// Transcript parse jobs scheduled by this walk. An unchanged file or a
    /// memo-only recompute contributes zero; a grown file contributes one
    /// suffix job.
    pub parse_jobs: u32,
    /// Transcript bytes scheduled for parsing by this walk. For grown files
    /// this is the appended suffix, not the full file length.
    pub parse_bytes: u64,
}

#[cfg(any(test, feature = "testkit"))]
type DiscoveredSpendingFiles = Vec<(&'static AgentDefinition, PathBuf)>;

#[cfg(any(test, feature = "testkit"))]
thread_local! {
    static DISCOVER_SPENDING_FILES_OVERRIDE: RefCell<Option<DiscoveredSpendingFiles>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    static PANIC_AFTER_REFRESH_FOR_TEST: Cell<bool> = const { Cell::new(false) };
}

#[cfg(any(test, feature = "testkit"))]
#[doc(hidden)]
pub struct DiscoverSpendingFilesOverride {
    prior: Option<DiscoveredSpendingFiles>,
}

#[cfg(any(test, feature = "testkit"))]
impl Drop for DiscoverSpendingFilesOverride {
    fn drop(&mut self) {
        let prior = self.prior.take();
        DISCOVER_SPENDING_FILES_OVERRIDE.with(|slot| {
            *slot.borrow_mut() = prior;
        });
    }
}

#[cfg(any(test, feature = "testkit"))]
#[doc(hidden)]
pub fn override_discovered_spending_files_for_test(
    files: DiscoveredSpendingFiles,
) -> DiscoverSpendingFilesOverride {
    let prior = DISCOVER_SPENDING_FILES_OVERRIDE.with(|slot| slot.replace(Some(files)));
    DiscoverSpendingFilesOverride { prior }
}

#[cfg(test)]
fn panic_after_refresh_for_test() {
    if PANIC_AFTER_REFRESH_FOR_TEST.with(|slot| slot.replace(false)) {
        panic!("spending walk aggregate test panic");
    }
}

#[cfg(test)]
fn panic_after_next_refresh_for_test() {
    PANIC_AFTER_REFRESH_FOR_TEST.with(|slot| slot.set(true));
}

pub struct SpendingWalker {
    cache: SpendingDiskCache,
    cache_stamp: Option<CacheStamp>,
    last_persisted_now_secs: Option<u64>,
    memo: Option<SpendingMemo>,
    discovery: discovery::SpendingDiscoveryIndex,
}

#[derive(Clone, Debug)]
struct SpendingMemo {
    key: SpendingMemoKey,
    counted: Box<[CountedLocation]>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SpendingMemoKey {
    generation: u64,
    files_signature: u64,
}

pub struct WalkRequest<'a> {
    pub files: &'a [(&'static AgentDefinition, PathBuf)],
    pub prices: &'a PriceBook,
    pub now_secs: u64,
    pub origin_overrides: &'a HashMap<PathBuf, PathBuf>,
    pub user_inputs: &'a [user_input::UserInputRecord],
    pub scope: Option<&'a SpendScope>,
    pub spec: &'a HeadlineSpec,
}

impl SpendingWalker {
    pub fn new() -> Self {
        Self {
            cache: SpendingDiskCache {
                version: SPENDING_CACHE_VERSION,
                ..Default::default()
            },
            cache_stamp: None,
            last_persisted_now_secs: None,
            memo: None,
            discovery: discovery::SpendingDiscoveryIndex::default(),
        }
    }

    /// Discover the historical spend stores through this walker's warm,
    /// process-local directory frontier.
    pub fn discover_spending_files(
        &mut self,
        now_secs: u64,
    ) -> Vec<(&'static AgentDefinition, PathBuf)> {
        #[cfg(any(test, feature = "testkit"))]
        if let Some(files) = DISCOVER_SPENDING_FILES_OVERRIDE.with(|slot| slot.borrow().clone()) {
            if files.is_empty() {
                self.discovery.mark_non_authoritative_for_test();
            }
            return files;
        }

        self.discovery
            .discover(crate::agents::all_definitions(), now_secs)
    }

    pub(crate) fn spending_discovery_is_authoritative(&self) -> bool {
        self.discovery.last_scan_authoritative()
    }

    /// Exercise the production discovery index with an explicit source set.
    /// Performance fixtures use this without mutating provider-home globals.
    #[cfg(feature = "testkit")]
    #[doc(hidden)]
    pub fn discover_declared_spending_files(
        &mut self,
        adapter: &'static AgentDefinition,
        sources: Vec<SpendingSource>,
        now_secs: u64,
    ) -> Vec<(&'static AgentDefinition, PathBuf)> {
        self.discovery
            .discover_sources_for_testkit(sources, now_secs)
            .into_iter()
            .map(|path| (adapter, path))
            .collect()
    }

    pub fn recorded_unknown_models(
        &mut self,
        cache_path: &Path,
        files: &[(&'static AgentDefinition, PathBuf)],
        now_secs: u64,
    ) -> BTreeSet<String> {
        let mut stats = WalkStats::default();
        self.sync_from_disk(cache_path, &mut stats);
        recorded_unknown_models(files, &self.cache, now_secs)
    }

    pub fn walk(
        &mut self,
        cache_path: &Path,
        req: &WalkRequest<'_>,
        observer: &mut dyn WalkObserver,
    ) -> SpendingWalkResult {
        self.walk_inner(cache_path, true, req, observer)
    }

    pub fn walk_local(
        &mut self,
        cache_path: &Path,
        req: &WalkRequest<'_>,
        observer: &mut dyn WalkObserver,
    ) -> SpendingWalkResult {
        self.walk_inner(cache_path, false, req, observer)
    }

    fn walk_inner(
        &mut self,
        cache_path: &Path,
        persist: bool,
        req: &WalkRequest<'_>,
        observer: &mut dyn WalkObserver,
    ) -> SpendingWalkResult {
        let mut stats = WalkStats::default();
        self.sync_from_disk(cache_path, &mut stats);
        let prior_generation = self.cache.generation;
        let persist_worthy = Cell::new(
            persist
                && self.last_persisted_now_secs.is_none_or(|last| {
                    req.now_secs.saturating_sub(last) >= SPENDING_PERSIST_MIN_INTERVAL
                }),
        );
        let mut checkpoint_written = false;
        let mut checkpoint_generation = None;
        {
            let mut last_checkpoint = Instant::now()
                .checked_sub(WALK_CHECKPOINT_INTERVAL)
                .unwrap_or_else(Instant::now);
            let mut on_jobs_scheduled = |stats: &WalkStats| {
                if stats.parse_bytes >= SPENDING_PERSIST_PARSE_BYTES {
                    persist_worthy.set(true);
                }
            };
            let mut tick = |cache: &SpendingDiskCache, progress: SpendProgress| {
                observer.on_file(progress);
                if last_checkpoint.elapsed() >= WALK_CHECKPOINT_INTERVAL {
                    if persist
                        && persist_worthy.get()
                        && cache.dirty
                        && write_spending_cache(cache_path, cache)
                    {
                        checkpoint_written = true;
                        checkpoint_generation = Some(cache.generation);
                    }
                    observer.on_interval(cache);
                    last_checkpoint = Instant::now();
                }
            };
            refresh_spending_cache(
                req.files,
                &mut self.cache,
                req.prices,
                req.now_secs,
                req.origin_overrides,
                &mut stats,
                &mut RefreshCallbacks {
                    on_jobs_scheduled: &mut on_jobs_scheduled,
                    tick: &mut tick,
                },
            );
        }
        self.discovery.reconcile(&self.cache, req.now_secs);
        if checkpoint_written {
            stats.cache_written = true;
            self.last_persisted_now_secs = Some(req.now_secs);
            if checkpoint_generation == Some(self.cache.generation) {
                self.cache.dirty = false;
                self.cache_stamp = cache_stamp(cache_path);
            }
        }
        if persist
            && persist_worthy.get()
            && self.cache.dirty
            && write_spending_cache(cache_path, &self.cache)
        {
            self.cache.dirty = false;
            self.cache_stamp = cache_stamp(cache_path);
            self.last_persisted_now_secs = Some(req.now_secs);
            stats.cache_written = true;
        }
        #[cfg(test)]
        panic_after_refresh_for_test();
        if self.cache.generation != prior_generation {
            self.memo = None;
        }

        self.ensure_memo(req.files, &mut stats);
        let locations = &self.memo.as_ref().expect("memo seeded above").counted;
        let counted = indexed_counted_entries(req.files, &self.cache, locations);
        let mut result = aggregate_counted_rollups(
            req.files,
            &self.cache,
            &counted,
            req.scope,
            HeadlineContext {
                user_inputs: req.user_inputs,
                now_secs: req.now_secs,
                spec: req.spec,
            },
            true,
        );
        result.stats = stats;
        result
    }

    fn sync_from_disk(&mut self, cache_path: &Path, stats: &mut WalkStats) {
        let stamp = cache_stamp(cache_path);
        if self.cache_stamp == stamp {
            return;
        }
        self.cache = read_spending_cache(cache_path);
        self.cache_stamp = stamp;
        self.memo = None;
        stats.cache_parsed = stamp.is_some();
    }

    fn ensure_memo(
        &mut self,
        files: &[(&'static AgentDefinition, PathBuf)],
        stats: &mut WalkStats,
    ) {
        let key = SpendingMemoKey {
            generation: self.cache.generation,
            files_signature: spending_files_signature(files),
        };
        if self.memo.as_ref().is_none_or(|memo| memo.key != key) {
            self.memo = Some(SpendingMemo {
                key,
                counted: dedup_cached_entry_locations(files, &self.cache).into_boxed_slice(),
            });
            stats.dedup_passes = 1;
        }
    }

    /// Compute one scope from the synchronized cursor cache without refreshing
    /// transcripts or persisting shared state. The producer uses this when the
    /// provider publication is fresh but its room sidecar is missing, retaining
    /// the same parsed cache and dedup memo for the next due global walk.
    pub(crate) fn scoped_from_cache(
        &mut self,
        cache_path: &Path,
        files: &[(&'static AgentDefinition, PathBuf)],
        user_inputs: &[user_input::UserInputRecord],
        scope: &SpendScope,
        now_secs: u64,
        spec: &HeadlineSpec,
    ) -> CachedScopedSpending {
        let mut stats = WalkStats::default();
        self.sync_from_disk(cache_path, &mut stats);
        self.ensure_memo(files, &mut stats);
        let locations = &self.memo.as_ref().expect("memo seeded above").counted;
        let counted = indexed_counted_entries(files, &self.cache, locations);
        let aggregate = aggregate_counted_rollups(
            files,
            &self.cache,
            &counted,
            Some(scope),
            HeadlineContext {
                user_inputs,
                now_secs,
                spec,
            },
            false,
        );
        CachedScopedSpending {
            has_discovered_file: files.iter().any(|(_, file)| {
                self.cache
                    .files
                    .contains_key(&file.to_string_lossy().into_owned())
            }),
            scoped: ScopedSpending {
                tally: aggregate.workspace_tally,
                headline_cutoff_secs: aggregate.workspace_headline_cutoff_secs,
                live_baselines: aggregate.workspace_live_baselines,
                day: aggregate.workspace_day,
                day_cutoff_secs: aggregate.day_cutoff_secs,
            },
        }
    }

    /// Apply trusted live transcript origins before a scope-only derivation.
    /// A changed origin invalidates the compact location memo exactly once. The
    /// same five-minute gate as a walk bounds full cursor rewrites when newly
    /// live transcripts reveal their origins between global refreshes.
    pub(crate) fn apply_origin_overrides(
        &mut self,
        cache_path: &Path,
        origin_overrides: &HashMap<PathBuf, PathBuf>,
        persist: bool,
        now_secs: u64,
    ) {
        let mut stats = WalkStats::default();
        self.sync_from_disk(cache_path, &mut stats);
        let mut changed = false;
        for (transcript, origin) in origin_overrides {
            let key = transcript.to_string_lossy();
            if let Some(file) = self.cache.files.get_mut(key.as_ref()) {
                changed |= aggregate::stamp_file_origin(file, origin);
            }
        }
        if changed {
            self.cache.mark_changed();
            self.memo = None;
        }
        if self.cache.dirty
            && persist
            && self
                .last_persisted_now_secs
                .is_none_or(|last| now_secs.saturating_sub(last) >= SPENDING_PERSIST_MIN_INTERVAL)
            && write_spending_cache(cache_path, &self.cache)
        {
            self.cache.dirty = false;
            self.cache_stamp = cache_stamp(cache_path);
            self.last_persisted_now_secs = Some(now_secs);
        }
    }
}

impl Default for SpendingWalker {
    fn default() -> Self {
        Self::new()
    }
}

// ── Spending computation ──────────────────────────────────────────────────────

/// Compute one live session's cumulative USD cost from the transcript/store the
/// adapter already parses for historical spending. Native thread ids select the
/// requested session in multi-session stores; id-free entries are included only
/// when the parsed file has no thread ids at all, which is the one-file-per-session
/// shape used by JSONL transcript providers. This fallback intentionally reads
/// only the supplied primary file; Claude's statusline self-report remains the
/// source that includes subagent companions for live session-scoped cost.
pub fn session_cost_usd(
    adapter: &AgentDefinition,
    session_id: &str,
    transcript_path: &Path,
    prices: &PriceBook,
) -> Option<AgentCost> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return None;
    }
    let parsed = adapter.parse_spend(transcript_path, None, prices);
    session_cost_from_entries(&parsed.entries, session_id)
}

/// Compute one live session's cumulative cost from an already parsed provider
/// projection without reopening its backing store.
pub fn session_cost_from_entries(entries: &[CachedEntry], session_id: &str) -> Option<AgentCost> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return None;
    }
    let total = session_entries(entries, session_id)
        .into_iter()
        .map(|entry| entry.cost_usd)
        .filter(|cost| cost.is_finite() && *cost > 0.0)
        .sum::<f64>();
    (total > 0.0).then_some(AgentCost {
        total_cost_usd: Some(total),
        ..AgentCost::default()
    })
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionTokenTotals {
    pub input: u64,
    pub output: u64,
}

/// Compute one live session's cumulative fresh input and output tokens from
/// the transcript/store the adapter already parses for historical spending.
/// Cache reads and writes stay outside these headline totals, matching the
/// token figures rendered by `rimz agents history`.
pub fn session_token_totals(
    adapter: &AgentDefinition,
    session_id: &str,
    transcript_path: &Path,
    prices: &PriceBook,
) -> Option<SessionTokenTotals> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return None;
    }
    let parsed = adapter.parse_spend(transcript_path, None, prices);
    let totals = session_entries(&parsed.entries, session_id)
        .into_iter()
        .fold(SessionTokenTotals::default(), |mut totals, entry| {
            totals.input = totals.input.saturating_add(entry.input);
            totals.output = totals.output.saturating_add(entry.output);
            totals
        });
    (totals.input > 0 || totals.output > 0).then_some(totals)
}

/// Select one provider session from parsed spend rows. A file with no native
/// thread ids is already session-scoped, so every row belongs to the requested
/// session.
pub fn session_entries<'a>(entries: &'a [CachedEntry], session_id: &str) -> Vec<&'a CachedEntry> {
    let session_id = session_id.trim();
    let has_thread_ids = entries.iter().any(|entry| {
        entry
            .thread_id
            .as_deref()
            .map(str::trim)
            .is_some_and(|thread_id| !thread_id.is_empty())
    });
    entries
        .iter()
        .filter(|entry| {
            entry
                .thread_id
                .as_deref()
                .map(str::trim)
                .filter(|thread_id| !thread_id.is_empty())
                .map_or(!has_thread_ids, |thread_id| thread_id == session_id)
        })
        .collect()
}

pub(crate) fn aggregate_walk_publish(
    files: &[(&'static AgentDefinition, PathBuf)],
    cache: &SpendingDiskCache,
    user_inputs: &[user_input::UserInputRecord],
    now_secs: u64,
    scope: Option<&SpendScope>,
    spec: &HeadlineSpec,
) -> SpendingWalkResult {
    let counted = dedup_cached_entries(files, cache).into_counted();
    aggregate_counted_rollups(
        files,
        cache,
        &counted,
        scope,
        HeadlineContext {
            user_inputs,
            now_secs,
            spec,
        },
        true,
    )
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScopedSpending {
    pub tally: SpendTally,
    pub headline_cutoff_secs: u64,
    pub live_baselines: BTreeMap<String, f64>,
    pub day: SpendWindow,
    pub day_cutoff_secs: u64,
}

pub(crate) struct CachedScopedSpending {
    pub(crate) has_discovered_file: bool,
    pub(crate) scoped: ScopedSpending,
}

/// Compute the cockpit's workspace-scoped tally plus the headline epoch cutoff
/// that resets presentation ratchets at window boundaries.
pub fn compute_scoped_spending(
    files: &[(&'static AgentDefinition, PathBuf)],
    cache: &SpendingDiskCache,
    user_inputs: &[user_input::UserInputRecord],
    scope: &SpendScope,
    now_secs: u64,
    spec: &HeadlineSpec,
) -> ScopedSpending {
    if scope.is_empty() {
        return ScopedSpending::default();
    }
    let deduped = dedup_cached_entries(files, cache);
    let counted = deduped.into_counted();
    let aggregate = aggregate_counted_rollups(
        files,
        cache,
        &counted,
        Some(scope),
        HeadlineContext {
            user_inputs,
            now_secs,
            spec,
        },
        false,
    );
    ScopedSpending {
        tally: aggregate.workspace_tally,
        headline_cutoff_secs: aggregate.workspace_headline_cutoff_secs,
        live_baselines: aggregate.workspace_live_baselines,
        day: aggregate.workspace_day,
        day_cutoff_secs: aggregate.day_cutoff_secs,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
