//! JSONL-based spending aggregation over agent transcript history.
//!
//! Per-provider typed parsers live in each adapter's `spend.rs`; this module
//! discovers transcript stores, refreshes the shared cursor cache through
//! [`SpendingWalker`], aggregates account-global and workspace-scoped windows,
//! and publishes stamped provider/workspace caches. Discovery and parsing
//! dispatch through the adapter ([`AgentAdapter::transcript_files`] /
//! [`AgentAdapter::parse_spend`]): a dollar-logging provider (Claude's legacy
//! `costUSD`, Pi) reads its figures verbatim, a token-only provider (Codex,
//! current Claude) multiplies counts through the
//! [`PriceBook`](super::pricing) — either way every file yields
//! [`CachedEntry`] values with one per-file origin and buckets under its
//! adapter's kind.

mod aggregate;
mod cache;
mod publish;
mod refresh;
mod time;

use std::cell::Cell;
#[cfg(test)]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::pricing::PriceBook;
use super::{AgentAdapter, AgentCost};

pub use crate::sidebar::timing::SPENDING_TTL;

pub(crate) use aggregate::{
    CountedPayload, OwnedCounted, aggregate_counted_rollups, dedup_cached_entries,
    dedup_cached_entries_owned, origin_path, spending_files_signature,
};
pub use aggregate::{
    DaySpend, HeadlineSpec, SpendScope, SpendTally, SpendWindow, SpendWindowMode, Spending,
};
#[cfg(test)]
pub(crate) use aggregate::{
    RAW_RETAIN_SECS, SKIP_PARSE_MARGIN_SECS, WIDEST_SPEND_WINDOW_SECS, cold_parse_out_of_window,
};
pub(crate) use cache::{CacheStamp, SPENDING_CACHE_VERSION, cache_stamp};
pub use cache::{
    CachedEntry, FileCacheEntry, SpendCursor, SpendParse, SpendingDiskCache, read_spending_cache,
    write_spending_cache,
};
#[cfg(test)]
pub(crate) use cache::{compact_spending_cache, file_stat, peek_cache_version};
pub(crate) use publish::{PROVIDER_SPENDING_VERSION, WORKSPACE_SPENDING_VERSION};
pub use publish::{
    ProviderSpendingCache, WorkspaceSpendingCache, read_provider_spending_cache,
    read_workspace_spending_cache, reconcile_workspace_carry, today_spend_live_usd,
    write_provider_spending_cache, write_provider_spending_cache_with_rollups,
    write_workspace_spending_cache,
};
pub(crate) use refresh::{
    RefreshCallbacks, is_priceable_model_name, record_unknown_model, recorded_unknown_models,
    refresh_spending_cache,
};
pub(crate) use time::iso_to_unix_secs;
pub use time::{unix_secs_now, utc_date};

/// Cadence for cursor-cache checkpoints and partial aggregate publishes during
/// a cold spending-history walk.
pub(crate) const WALK_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(1);

const SPENDING_PERSIST_MIN_INTERVAL: u64 = 5 * 60;
const SPENDING_PERSIST_PARSE_BYTES: u64 = 1 << 20;

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
#[derive(Clone, Debug, Default)]
pub struct SpendingCaches {
    pub provider: ProviderSpendingCache,
    pub workspace: WorkspaceSpendingCache,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpendingWalkResult {
    pub spending: Spending,
    pub workspace_tally: SpendTally,
    pub workspace_headline_cutoff_secs: u64,
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

#[cfg(test)]
type DiscoveredSpendingFiles = Vec<(&'static dyn AgentAdapter, PathBuf)>;

#[cfg(test)]
thread_local! {
    static DISCOVER_SPENDING_FILES_OVERRIDE: RefCell<Option<DiscoveredSpendingFiles>> =
        const { RefCell::new(None) };
    static PANIC_AFTER_REFRESH_FOR_TEST: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
pub(crate) struct DiscoverSpendingFilesOverride {
    prior: Option<DiscoveredSpendingFiles>,
}

#[cfg(test)]
impl Drop for DiscoverSpendingFilesOverride {
    fn drop(&mut self) {
        let prior = self.prior.take();
        DISCOVER_SPENDING_FILES_OVERRIDE.with(|slot| {
            *slot.borrow_mut() = prior;
        });
    }
}

#[cfg(test)]
pub(crate) fn override_discovered_spending_files_for_test(
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
}

#[derive(Clone, Debug)]
struct SpendingMemo {
    key: SpendingMemoKey,
    counted: Vec<OwnedCounted>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SpendingMemoKey {
    generation: u64,
    files_signature: u64,
    automation_signature: u64,
}

pub struct WalkRequest<'a> {
    pub files: &'a [(&'static dyn AgentAdapter, PathBuf)],
    pub prices: &'a PriceBook,
    pub now_secs: u64,
    pub origin_overrides: &'a HashMap<PathBuf, PathBuf>,
    pub automation_files: &'a HashSet<PathBuf>,
    pub automation_signature: u64,
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
        }
    }

    pub fn recorded_unknown_models(
        &mut self,
        cache_path: &Path,
        files: &[(&'static dyn AgentAdapter, PathBuf)],
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

        let key = SpendingMemoKey {
            generation: self.cache.generation,
            files_signature: spending_files_signature(req.files),
            automation_signature: req.automation_signature,
        };
        if self.memo.as_ref().is_none_or(|memo| memo.key != key) {
            self.memo = Some(SpendingMemo {
                key,
                counted: dedup_cached_entries_owned(req.files, &self.cache, req.automation_files)
                    .into_counted(),
            });
            stats.dedup_passes = 1;
        }

        let counted = &self.memo.as_ref().expect("memo seeded above").counted;
        aggregate_walk_publish_from_counted(
            req.files,
            &self.cache,
            counted,
            req.now_secs,
            req.scope,
            req.spec,
            stats,
        )
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
}

impl Default for SpendingWalker {
    fn default() -> Self {
        Self::new()
    }
}

// ── Spending computation ──────────────────────────────────────────────────────

pub fn discover_spending_files() -> Vec<(&'static dyn AgentAdapter, PathBuf)> {
    #[cfg(test)]
    if let Some(files) = DISCOVER_SPENDING_FILES_OVERRIDE.with(|slot| slot.borrow().clone()) {
        return files;
    }

    crate::agents::ADAPTERS
        .iter()
        .flat_map(|adapter| {
            adapter
                .transcript_files()
                .into_iter()
                .map(move |file| (*adapter, file))
        })
        .collect()
}

/// Compute one live session's cumulative USD cost from the transcript/store the
/// adapter already parses for historical spending. Native thread ids select the
/// requested session in multi-session stores; id-free entries are included only
/// when the parsed file has no thread ids at all, which is the one-file-per-session
/// shape used by JSONL transcript providers.
pub fn session_cost_usd(
    adapter: &dyn AgentAdapter,
    session_id: &str,
    transcript_path: &Path,
    prices: &PriceBook,
) -> Option<AgentCost> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return None;
    }
    let parsed = adapter.parse_spend(transcript_path, None, prices);
    let has_thread_ids = parsed.entries.iter().any(|entry| {
        entry
            .thread_id
            .as_deref()
            .map(str::trim)
            .is_some_and(|thread_id| !thread_id.is_empty())
    });
    let total = parsed
        .entries
        .iter()
        .filter(|entry| {
            entry
                .thread_id
                .as_deref()
                .map(str::trim)
                .filter(|thread_id| !thread_id.is_empty())
                .map_or(!has_thread_ids, |thread_id| thread_id == session_id)
        })
        .map(|entry| entry.cost_usd)
        .filter(|cost| cost.is_finite() && *cost > 0.0)
        .sum::<f64>();
    (total > 0.0).then_some(AgentCost {
        total_cost_usd: Some(total),
        ..AgentCost::default()
    })
}

pub(crate) fn aggregate_walk_publish(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    automation_files: &HashSet<PathBuf>,
    now_secs: u64,
    scope: Option<&SpendScope>,
    spec: &HeadlineSpec,
) -> SpendingWalkResult {
    let counted = dedup_cached_entries(files, cache, automation_files).into_counted();
    aggregate_walk_publish_from_counted(
        files,
        cache,
        &counted,
        now_secs,
        scope,
        spec,
        WalkStats::default(),
    )
}

fn aggregate_walk_publish_from_counted<C: CountedPayload>(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    counted: &[C],
    now_secs: u64,
    scope: Option<&SpendScope>,
    spec: &HeadlineSpec,
    stats: WalkStats,
) -> SpendingWalkResult {
    let aggregate = aggregate_counted_rollups(files, cache, counted, scope, now_secs, spec, true);

    SpendingWalkResult {
        spending: aggregate.spending,
        workspace_tally: aggregate.workspace_tally,
        workspace_headline_cutoff_secs: aggregate.workspace_headline_cutoff_secs,
        days: aggregate.days,
        models: aggregate.models,
        stats,
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScopedSpending {
    pub tally: SpendTally,
    pub headline_cutoff_secs: u64,
}

/// Compute the cockpit's workspace-scoped tally plus the headline epoch cutoff
/// that makes live carry reset at window boundaries.
pub fn compute_scoped_spending(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    automation_files: &HashSet<PathBuf>,
    scope: &SpendScope,
    now_secs: u64,
    spec: &HeadlineSpec,
) -> ScopedSpending {
    if scope.is_empty() {
        return ScopedSpending::default();
    }
    let deduped = dedup_cached_entries(files, cache, automation_files);
    let counted = deduped.into_counted();
    let aggregate =
        aggregate_counted_rollups(files, cache, &counted, Some(scope), now_secs, spec, false);
    ScopedSpending {
        tally: aggregate.workspace_tally,
        headline_cutoff_secs: aggregate.workspace_headline_cutoff_secs,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
