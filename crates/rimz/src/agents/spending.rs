//! JSONL-based spending aggregation over agent transcript history.
//!
//! Per-provider typed parsers live in each adapter's `spend.rs`; this module
//! owns the on-disk cache types and the [`compute_spending`] aggregation loop
//! with cross-file Claude dedup. Discovery and parsing dispatch through the
//! adapter ([`AgentAdapter::transcript_files`] /
//! [`AgentAdapter::parse_spend`]): a dollar-logging provider (Claude's legacy
//! `costUSD`, Pi) reads its figures verbatim, a token-only provider (Codex,
//! current Claude) multiplies counts through the
//! [`PriceBook`](super::pricing) — either way every file yields
//! [`CachedEntry`] values and buckets under its adapter's kind.
//!
//! [`compute_spending`] returns a [`Spending`]: one fleet-wide headline / 7d /
//! 30d / 365d [`SpendTally`] plus a per-provider breakdown, so the fleet ledger
//! and each dashboard panel read account-global piles. The cockpit reads a
//! workspace-scoped [`SpendTally`] derived from the same cached entries.

#[cfg(test)]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::descriptor::ThreadKey;
use super::pricing::PriceBook;
use super::{AgentAdapter, AgentCost};
pub use crate::sidebar::timing::SPENDING_TTL;

// ── Public types ──────────────────────────────────────────────────────────────

/// Spend (USD) and token throughput accumulated over one time window. `tokens`
/// is the `◇` total: input with cache-write folded in, plus output. The split
/// fields stay available (`input` / `output` / `cache_write` / `cache_read`);
/// the fleet lines read `◇ ↘ ↗ ◌`, while `cache_write` remains separate for
/// debug/cache compatibility. `sessions` counts distinct billing threads,
/// preferring provider-native thread ids and falling back to transcript-file
/// grouping.
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
const WIDEST_SPEND_WINDOW_SECS: u64 = 365 * 86_400;

/// The headline spend window shown in the cockpit and provider dashboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpendWindowMode {
    /// Trailing 24 hours, matching Rimz's original "today" row behaviour.
    #[default]
    #[serde(rename = "24h")]
    Trailing24h,
    /// The local calendar day, using `[sidebar] spend_timezone` when set.
    Today,
    /// The current activity burst since the last five-hour idle gap.
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
    /// Configured headline window (`[sidebar] spend_window`): trailing 24 hours
    /// by default, the local calendar day, or the current session.
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

/// Progress from the transcript-history spending pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpendProgress {
    pub finished_files: usize,
    pub total_files: usize,
}

/// Spending data published for one sidebar enrichment fold: account-global
/// provider totals plus the room-local cockpit tally.
#[derive(Clone, Debug, Default)]
pub struct SpendingCaches {
    pub provider: ProviderSpendingCache,
    pub workspace: WorkspaceSpendingCache,
}

/// One UTC day's deduplicated, account-global spend and token total — a cell of
/// the [`compute_daily_spend`] contribution heatmap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DaySpend {
    pub usd: f64,
    /// `input` (cache-write folded in) + `output`, matching [`SpendWindow`]'s
    /// `◇` token total so a day's tokens agree with the trailing windows.
    pub tokens: u64,
}

/// Bucket the full spend history by UTC day (days since the Unix epoch),
/// account-global, for the token-usage heatmap. Reuses the same cross-file
/// Claude `(message_id, request_id)` dedup and sidechain-replay suppression as
/// the trailing-window tally, so a day's tokens match what [`aggregate_spending`]
/// would fold for that span. Pure over an already-refreshed cache — the caller
/// owns the walk that fills it.
pub fn compute_daily_spend(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
) -> BTreeMap<i64, DaySpend> {
    let deduped = dedup_cached_entries(files, cache);
    let mut by_day: BTreeMap<i64, DaySpend> = BTreeMap::new();
    for_each_counted_entry(&deduped, |entry| {
        let day = (entry.ts_secs / 86_400) as i64;
        let cell = by_day.entry(day).or_default();
        cell.usd += entry.cost_usd;
        cell.tokens += entry.input + entry.cache_write + entry.output;
    });
    by_day
}

/// One model's deduplicated, account-global throughput across the full history —
/// a row of the `rimz stats` model breakdown. `input` folds cache-write in to
/// match the `◇` convention, so `tokens` stays `input + output`; cache-read rides
/// alongside for the model detail split and all-time cached total.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelSpend {
    pub usd: f64,
    pub input: u64,
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    pub tokens: u64,
}

/// Bucket the full spend history by model id, account-global, reusing the same
/// cross-file Claude dedup and sidechain-replay suppression as
/// [`compute_daily_spend`] so a model's tokens agree with the trailing windows.
/// An entry whose transcript named no model buckets under the empty key, which
/// the caller folds into an "Other" row. Pure over an already-refreshed cache.
pub fn compute_model_breakdown(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
) -> BTreeMap<String, ModelSpend> {
    let deduped = dedup_cached_entries(files, cache);
    let mut by_model: BTreeMap<String, ModelSpend> = BTreeMap::new();
    for_each_counted_entry(&deduped, |entry| {
        let cell = by_model
            .entry(entry.model.clone().unwrap_or_default())
            .or_default();
        cell.usd += entry.cost_usd;
        cell.input += entry.input + entry.cache_write;
        cell.output += entry.output;
        cell.cache_read += entry.cache_read;
        cell.tokens += entry.input + entry.cache_write + entry.output;
    });
    by_model
}

/// Visit every deduplicated entry that counts toward totals: each kept
/// message-ID entry that is not a suppressed sidechain replay, then every ID-free
/// entry. The shared spine of [`compute_daily_spend`] and
/// [`compute_model_breakdown`].
fn for_each_counted_entry(deduped: &DedupedCachedEntries, mut visit: impl FnMut(&CachedEntry)) {
    for ((msg_id, _), (_, entry)) in &deduped.by_exact_key {
        let is_sidechain_replay = entry.is_sidechain
            && deduped
                .msg_has_non_sidechain
                .get(msg_id.as_str())
                .copied()
                .unwrap_or(false);
        if !is_sidechain_replay {
            visit(entry);
        }
    }
    for (_, entry) in &deduped.free_entries {
        visit(entry);
    }
}

/// Bumped whenever the cached parse shape *or values* change, so an upgrade
/// re-reads every file once. A finalized session's stable mtime otherwise pins
/// its entries in the cache forever — a field added to or reshaped in
/// [`CachedEntry`] (the `date` → `ts_secs` switch in v3; the `tokens` → four-way
/// split in v4), or a change in how a kept cost is computed (v4 also prices Claude
/// turns from token usage now that transcripts omit `costUSD`, so sessions cached
/// as zero entries must re-parse), would otherwise stay frozen for that session
/// and never heal. v5 makes the parse incremental: an entry without a real
/// `len`/`cursor` would read as "grown from offset 0" and append a duplicate
/// full parse, so the pre-cursor shape must cold-rebuild. v6 records per-file
/// unknown model names; the cold rebuild also heals entries silently dropped
/// while unpriced under v5. v7 records per-entry origin paths so workspace
/// scoped cockpit tallies can be exact; old entries would otherwise read as
/// unknown-origin and disappear from the cockpit. v8 stores Codex
/// `session_meta.cwd` in the parser cursor and stamps Codex entries from it;
/// old finalized Codex files would otherwise sit at EOF with no origin and
/// remain invisible to workspace-scoped cockpit tallies. v9 records the per-entry
/// model id for the `rimz stats` per-model breakdown; old finalized files would
/// otherwise sit at EOF with no model and never attribute their tokens. v10
/// records provider-native thread ids and keeps unpriced token usage rows, so
/// multi-session stores count sessions correctly and unknown prices hide only
/// dollars. A cache
/// stamped with an older version is discarded on read, forcing a clean re-parse
/// under the current shape. `0` is the implicit pre-versioning shape (no
/// `version` field).
const SPENDING_CACHE_VERSION: u32 = 10;

/// Gates the aggregate meaning in provider-spending.json, independent of the
/// raw per-file [`SPENDING_CACHE_VERSION`]. An older stamp reads as stale, so
/// the producer recomputes once from the still-current entry cache. `0` is the
/// implicit pre-versioning shape. v1: cache-write folds into `◇`/`↘`. v2:
/// live-session baselines moved to a per-workspace sidecar. v3: the published
/// aggregate carries per-day and per-model rollups for `rimz stats`. v4:
/// unpriced token usage contributes tokens/sessions with zero dollars, and
/// provider-native thread ids count multi-session stores correctly. v5: GPT-5.5
/// built-in prices heal previously zero-dollar token rows. v6: per-model
/// cache-read is published for `rimz stats`.
pub(crate) const PROVIDER_SPENDING_VERSION: u32 = 6;

/// Aggregate version for the per-workspace cockpit tally cache. This is
/// independent of the shared raw-entry cache version: a semantic change here
/// can force a cheap re-aggregate without re-reading transcripts.
pub(crate) const WORKSPACE_SPENDING_VERSION: u32 = 1;

/// On-disk cache persisted at shared state `spending.json`.
///
/// Keyed by canonical file path string.  `dirty` is excluded from
/// serialization — callers set it and flush when true. `version` gates the
/// whole cache: [`read_spending_cache`] discards a stale-shape cache and stamps
/// the current version, so a write always carries it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpendingDiskCache {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub files: HashMap<String, FileCacheEntry>,
    #[serde(skip)]
    pub dirty: bool,
}

/// Cached parse of one JSONL file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileCacheEntry {
    pub mtime_secs: u64,
    /// File length at the last parse — the growth/truncation detector: a
    /// longer file parses only its suffix, a shorter (rotated/truncated) one
    /// re-parses cold, an equal length with a new mtime re-parses cold (an
    /// in-place rewrite).
    #[serde(default)]
    pub len: u64,
    /// Where the last parse left off — the next incremental parse resumes here.
    #[serde(default)]
    pub cursor: SpendCursor,
    /// Durable per-file origin learned outside the parser. Codex rollout paths
    /// do not encode a workspace, so Rimz stamps the file once from live
    /// snapshot metadata and reuses that origin across cold re-parses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_path: Option<PathBuf>,
    /// One nonzero token-usage entry per parsed transcript record. Duplicates
    /// within a file (retry writes) are kept raw here; the aggregation pass owns
    /// all dedup.
    pub entries: Vec<CachedEntry>,
    /// Price lookup misses observed while parsing this file, keyed by model and
    /// carrying the youngest timestamp seen for that model. The pricing refresh
    /// chase unions active unknowns across currently discovered files. When one
    /// later resolves while still inside the widest spend window, this file cold
    /// re-parses so zero-dollar token entries recover their spend.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown_models: BTreeMap<String, u64>,
}

/// Where an incremental spend parse left off: the byte offset just past the
/// last consumed line, plus the adapter's opaque cross-line state (Codex
/// carries its cumulative token totals and tracked model so a resumed delta
/// subtraction stays exact). Stored per file in the spending cache; a state
/// shape change bumps [`SPENDING_CACHE_VERSION`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpendCursor {
    pub offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<serde_json::Value>,
}

/// One spend parse: the entries read past the resume point and the cursor the
/// cache stores for the next pass.
#[derive(Debug, Default)]
pub struct SpendParse {
    pub entries: Vec<CachedEntry>,
    pub cursor: SpendCursor,
    pub unknown_models: BTreeMap<String, u64>,
}

/// A single cost entry with dedup keys for cross-file deduplication.
///
/// `message_id` and `request_id` are present for Claude entries and absent for
/// Codex and Pi entries. `thread_id` is present when the provider's durable
/// transcript store exposes a native session id. `is_sidechain` drives the
/// sidechain-replay suppression logic in `compute_spending`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedEntry {
    /// Unix timestamp (seconds) the entry was recorded, parsed from the JSONL
    /// `timestamp` via [`iso_to_unix_secs`]. Drives the trailing-window bucketing
    /// in [`accum`].
    pub ts_secs: u64,
    pub cost_usd: f64,
    /// Fresh input tokens (Claude `input_tokens`; Codex uncached input).
    /// Per-entry components stay raw; [`SpendWindow::add`] folds `cache_write`
    /// into aggregate input/total. `#[serde(default)]` keeps an older cache
    /// parseable; `SPENDING_CACHE_VERSION` is what actually heals it — a
    /// pre-split cache is discarded on read so every file re-parses, since a
    /// finalized session's stable mtime would otherwise pin these at `0`.
    #[serde(default)]
    pub input: u64,
    /// Output tokens (Codex `output_tokens` already includes reasoning).
    #[serde(default)]
    pub output: u64,
    /// Cache-write tokens (Claude `cache_creation_input_tokens`, Pi
    /// `cacheWrite`); `0` for providers with no cache-creation concept (Codex).
    #[serde(default)]
    pub cache_write: u64,
    /// Cache-read (Claude `cache_read_input_tokens`; Codex `cached_input_tokens`).
    #[serde(default)]
    pub cache_read: u64,
    /// `message.id` from Claude entries; `None` for Codex and Pi entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// `requestId` from Claude entries; `None` for Codex and Pi entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Provider-native billing thread/session id, used for stores where many
    /// sessions live in one transcript file or database.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// `isSidechain` flag from Claude entries.
    #[serde(default)]
    pub is_sidechain: bool,
    /// Model id as the transcript named it (`claude-opus-4-8`, `gpt-5-codex`, …),
    /// kept for the per-model token breakdown. `None` for an entry whose
    /// transcript named no model. Carried through dedup so a kept turn keeps its
    /// model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Working-directory origin for workspace-scoped tallies. `None` means the
    /// parser could not prove the transcript's workspace; scoped aggregation
    /// omits it rather than guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_path: Option<PathBuf>,
}

#[cfg(test)]
type DiscoveredSpendingFiles = Vec<(&'static dyn AgentAdapter, PathBuf)>;

#[cfg(test)]
thread_local! {
    static DISCOVER_SPENDING_FILES_OVERRIDE: RefCell<Option<DiscoveredSpendingFiles>> =
        const { RefCell::new(None) };
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

// ── Spending computation ──────────────────────────────────────────────────────

/// Every registered agent transcript file, tagged with the adapter that owns
/// its native spending parser.
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

/// Compute the fleet and per-provider default headline / 7d / 30d / 365d spend
/// and token tally for the given adapter-tagged JSONL files, pricing token-only
/// providers (Codex) through `prices`.
///
/// IO is O(delta), not O(history): an unchanged file (same mtime and length)
/// is a pure cache hit, a grown file parses only its appended suffix from the
/// stored [`SpendCursor`], and only a truncated/rotated or rewritten-in-place
/// file re-parses cold. Sets `cache.dirty = true` when any entry was updated;
/// finished sessions have stable stats and are permanently served from cache
/// after the first parse.
///
/// ### Cross-file Claude deduplication
///
/// When a Claude session spawns subagents, the btw tool replays the parent
/// message into the subagent file with the full reconstructed context (50k+
/// cache-read tokens) and a `costUSD` that would double-count the turn if
/// summed naively.  Dedup by `(message.id, requestId)` across all files
/// prevents this: if `message.id` M appears as both `isSidechain:false`
/// (main-chain entry) and `isSidechain:true` (sidechain replay), the sidechain
/// entry is suppressed.  Only Claude entries carry message IDs; Codex and Pi
/// entries are ID-free and bucket directly under their file's tagged provider.
pub fn compute_spending(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &mut SpendingDiskCache,
    prices: &PriceBook,
    now_secs: u64,
) -> Spending {
    compute_spending_with_origins(files, cache, prices, now_secs, &HashMap::new())
}

/// Compute fleet spending while reporting file-level progress for user-facing
/// cold walks such as `rimz stats`.
pub fn compute_spending_with_progress(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &mut SpendingDiskCache,
    prices: &PriceBook,
    now_secs: u64,
    progress: &mut dyn FnMut(SpendProgress),
) -> Spending {
    let spec = HeadlineSpec::default();
    compute_spending_with_origins_and_scope_progress(
        files,
        cache,
        prices,
        now_secs,
        &HashMap::new(),
        None,
        &spec,
        Some(progress),
    )
    .0
}

/// Compute fleet spending, applying trusted transcript-path → origin overrides
/// before aggregation. The overrides are currently used for Codex rollout files,
/// whose path does not encode the workspace; Claude and Pi parsers stamp their
/// own origins from transcript contents.
pub fn compute_spending_with_origins(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &mut SpendingDiskCache,
    prices: &PriceBook,
    now_secs: u64,
    origin_overrides: &HashMap<PathBuf, PathBuf>,
) -> Spending {
    let spec = HeadlineSpec::default();
    compute_spending_with_origins_and_scope(
        files,
        cache,
        prices,
        now_secs,
        origin_overrides,
        None,
        &spec,
    )
    .0
}

/// Compute account-global spending and, when `scope` is present, the cockpit's
/// workspace-scoped tally from the same refreshed cache and the same dedup pass.
pub fn compute_spending_with_origins_and_scope(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &mut SpendingDiskCache,
    prices: &PriceBook,
    now_secs: u64,
    origin_overrides: &HashMap<PathBuf, PathBuf>,
    scope: Option<&SpendScope>,
    spec: &HeadlineSpec,
) -> (Spending, SpendTally) {
    compute_spending_with_origins_and_scope_progress(
        files,
        cache,
        prices,
        now_secs,
        origin_overrides,
        scope,
        spec,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn compute_spending_with_origins_and_scope_progress(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &mut SpendingDiskCache,
    prices: &PriceBook,
    now_secs: u64,
    origin_overrides: &HashMap<PathBuf, PathBuf>,
    scope: Option<&SpendScope>,
    spec: &HeadlineSpec,
    mut progress: Option<&mut dyn FnMut(SpendProgress)>,
) -> (Spending, SpendTally) {
    // First pass: refresh stale cache entries — pure hit, suffix parse, or
    // cold parse, decided from one stat per file.
    let total_files = files.len();
    for (index, (adapter, file)) in files.iter().enumerate() {
        let (mtime, len) = file_stat(file);
        let key = file.to_string_lossy().into_owned();
        let override_origin = origin_overrides
            .get(file)
            .and_then(|origin| normalized_absolute_path(origin));
        let prior_origin = cache
            .files
            .get(&key)
            .and_then(|entry| file_cache_origin(entry, adapter.descriptor().kind == "codex"));
        let file_origin = override_origin.or(prior_origin);
        let heals = cache
            .files
            .get(&key)
            .is_some_and(|entry| has_healed_unknown(entry, prices, now_secs));
        match cache.files.get_mut(&key) {
            // Unchanged: nothing to read.
            Some(entry) if !heals && entry.mtime_secs == mtime && entry.len == len => {
                if let Some(origin) = file_origin.as_deref()
                    && stamp_file_origin(entry, origin)
                {
                    cache.dirty = true;
                }
            }
            // Grown in place: parse only the appended suffix and extend.
            Some(entry) if !heals && len > entry.len => {
                let mut parsed = adapter.parse_spend(file, Some(&entry.cursor), prices);
                if let Some(origin) = file_origin.as_deref() {
                    stamp_entries_origin(&mut parsed.entries, origin);
                }
                entry.entries.extend(parsed.entries);
                entry.unknown_models.extend(parsed.unknown_models);
                entry.cursor = parsed.cursor;
                entry.mtime_secs = mtime;
                entry.len = len;
                if let Some(origin) = file_origin.as_deref() {
                    stamp_file_origin(entry, origin);
                }
                cache.dirty = true;
            }
            // New, truncated/rotated, or rewritten in place: parse cold.
            _ => {
                let mut parsed = adapter.parse_spend(file, None, prices);
                if let Some(origin) = file_origin.as_deref() {
                    stamp_entries_origin(&mut parsed.entries, origin);
                }
                cache.files.insert(
                    key.clone(),
                    FileCacheEntry {
                        mtime_secs: mtime,
                        len,
                        cursor: parsed.cursor,
                        origin_path: file_origin,
                        entries: parsed.entries,
                        unknown_models: parsed.unknown_models,
                    },
                );
                cache.dirty = true;
            }
        }
        if let Some(progress) = progress.as_deref_mut() {
            progress(SpendProgress {
                finished_files: index + 1,
                total_files,
            });
        }
    }
    if prune_spending_cache(files, cache, now_secs) {
        cache.dirty = true;
    }

    // Second pass: aggregate with cross-file Claude deduplication.
    //
    // Claude entries carry message IDs and dedup on exact_key = (message_id,
    // request_id); msg_has_non_sidechain tracks whether each message_id has a
    // main-chain entry anywhere across all files, so sidechain replays can be
    // suppressed.  ID-free entries (Codex, Pi) carry their file's provider so
    // they bucket under the right kind.
    let deduped = dedup_cached_entries(files, cache);
    let spending = aggregate_spending(files, cache, &deduped, now_secs, spec);
    let workspace = scope
        .map(|scope| aggregate_scoped_tally(files, cache, &deduped, scope, now_secs, spec))
        .unwrap_or_default();

    (spending, workspace)
}

fn aggregate_spending(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    deduped: &DedupedCachedEntries,
    now_secs: u64,
    spec: &HeadlineSpec,
) -> Spending {
    let mut spending = Spending::default();
    let counted = counted_entries(deduped);
    let uniform = uniform_headline_cutoff(spec, now_secs);
    let all_timestamps: Vec<u64> = counted.iter().map(|(_, entry)| entry.ts_secs).collect();
    let total_cutoff =
        uniform.unwrap_or_else(|| session_cutoff_secs(all_timestamps.as_slice(), now_secs));
    let provider_cutoffs = if uniform.is_none() {
        let mut timestamps: HashMap<&'static str, Vec<u64>> = HashMap::new();
        for (provider, entry) in &counted {
            timestamps.entry(*provider).or_default().push(entry.ts_secs);
        }
        timestamps
            .into_iter()
            .map(|(provider, timestamps)| {
                (
                    provider,
                    session_cutoff_secs(timestamps.as_slice(), now_secs),
                )
            })
            .collect::<HashMap<_, _>>()
    } else {
        HashMap::new()
    };
    let cutoff_for = |provider: &'static str| {
        uniform
            .or_else(|| provider_cutoffs.get(provider).copied())
            .unwrap_or_else(|| session_cutoff_secs(&[], now_secs))
    };

    let mut add = |provider: &'static str, entry: &CachedEntry| {
        accum(&mut spending.total, entry, now_secs, total_cutoff);
        accum(
            spending.by_provider.entry(provider.to_owned()).or_default(),
            entry,
            now_secs,
            cutoff_for(provider),
        );
    };

    for (provider, entry) in counted {
        add(provider, entry);
    }

    // Session counts, keyed by provider-native thread id when one is available
    // and by transcript file grouping otherwise. A Claude session's subagent
    // files fold under its `session_id` directory so one thread counts once.
    // Each thread is single-provider; we track its youngest entry and bump every
    // window that youngest reading still falls within. Counted from the raw
    // cached entries (not the deduped set) since a thread that ran is a thread,
    // regardless of which file a duplicated turn was kept in.
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
        bump_sessions(&mut spending.total, *youngest, now_secs, total_cutoff);
        bump_sessions(
            spending
                .by_provider
                .entry((*provider).to_owned())
                .or_default(),
            *youngest,
            now_secs,
            cutoff_for(provider),
        );
    }

    spending
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

/// Compute the cockpit's workspace-scoped tally from an already-refreshed
/// spending cache. Unknown-origin entries are skipped.
pub fn compute_scoped_tally(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    scope: &SpendScope,
    now_secs: u64,
    spec: &HeadlineSpec,
) -> SpendTally {
    if scope.is_empty() {
        return SpendTally::default();
    }
    let deduped = dedup_cached_entries(files, cache);
    aggregate_scoped_tally(files, cache, &deduped, scope, now_secs, spec)
}

fn aggregate_scoped_tally(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    deduped: &DedupedCachedEntries,
    scope: &SpendScope,
    now_secs: u64,
    spec: &HeadlineSpec,
) -> SpendTally {
    let mut tally = SpendTally::default();
    let counted = counted_entries(deduped)
        .into_iter()
        .filter(|(_, entry)| entry_in_scope(entry, scope))
        .collect::<Vec<_>>();
    let timestamps: Vec<u64> = counted.iter().map(|(_, entry)| entry.ts_secs).collect();
    let headline_cutoff = uniform_headline_cutoff(spec, now_secs)
        .unwrap_or_else(|| session_cutoff_secs(timestamps.as_slice(), now_secs));

    for (_, entry) in counted {
        accum(&mut tally, entry, now_secs, headline_cutoff);
    }

    let mut threads: HashMap<String, u64> = HashMap::new();
    for (adapter, file) in files {
        let cache_key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get(&cache_key) else {
            continue;
        };
        for entry in cached_file
            .entries
            .iter()
            .filter(|entry| entry_in_scope(entry, scope))
        {
            threads
                .entry(session_key(*adapter, file, entry))
                .and_modify(|ts| *ts = (*ts).max(entry.ts_secs))
                .or_insert(entry.ts_secs);
        }
    }
    for youngest in threads.values() {
        bump_sessions(&mut tally, *youngest, now_secs, headline_cutoff);
    }

    tally
}

fn stamp_file_origin(entry: &mut FileCacheEntry, origin: &Path) -> bool {
    let origin = crate::worktree::normalize_path_lexical(origin);
    let mut changed = false;
    if entry.origin_path.as_ref() != Some(&origin) {
        entry.origin_path = Some(origin.clone());
        changed = true;
    }
    changed | stamp_entries_origin(&mut entry.entries, &origin)
}

fn stamp_entries_origin(entries: &mut [CachedEntry], origin: &Path) -> bool {
    let origin = crate::worktree::normalize_path_lexical(origin);
    let mut changed = false;
    for cached in entries {
        if cached.origin_path.as_ref() != Some(&origin) {
            cached.origin_path = Some(origin.clone());
            changed = true;
        }
    }
    changed
}

fn file_cache_origin(entry: &FileCacheEntry, infer_from_entries: bool) -> Option<PathBuf> {
    entry.origin_path.clone().or_else(|| {
        infer_from_entries
            .then(|| single_cached_origin(entry))
            .flatten()
    })
}

fn single_cached_origin(entry: &FileCacheEntry) -> Option<PathBuf> {
    let mut origins = entry
        .entries
        .iter()
        .filter_map(|entry| entry.origin_path.as_deref());
    let first = origins.next()?;
    origins
        .all(|origin| origin == first)
        .then(|| first.to_path_buf())
}

fn entry_in_scope(entry: &CachedEntry, scope: &SpendScope) -> bool {
    entry
        .origin_path
        .as_deref()
        .is_some_and(|origin| scope.contains(origin))
}

pub(crate) fn origin_path(raw: Option<&str>) -> Option<PathBuf> {
    normalized_absolute_path(&PathBuf::from(raw?.trim()))
}

fn normalized_absolute_path(path: &Path) -> Option<PathBuf> {
    let normalized = crate::worktree::normalize_path_lexical(path);
    normalized.is_absolute().then_some(normalized)
}

/// Whether a model name from a transcript should feed the pricing refresh
/// chase. Claude sentinel turns such as `<synthetic>` are not API model names
/// and would otherwise keep the chase pending forever.
pub(crate) fn is_priceable_model_name(model: &str) -> bool {
    let model = model.trim();
    !model.is_empty() && !model.starts_with('<')
}

/// Record a priceable model lookup miss at `ts_secs`, keeping the youngest
/// timestamp so the chase stops once every occurrence ages out of the widest
/// spend window.
pub(crate) fn record_unknown_model(
    unknowns: &mut BTreeMap<String, u64>,
    model: &str,
    ts_secs: u64,
) {
    let model = model.trim();
    if !is_priceable_model_name(model) {
        return;
    }
    unknowns
        .entry(model.to_owned())
        .and_modify(|seen| *seen = (*seen).max(ts_secs))
        .or_insert(ts_secs);
}

/// Unknown models recorded by files still discovered in this spending pass.
/// Deleted or moved transcripts do not keep a never-resolving name alive.
pub fn recorded_unknown_models(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
    now_secs: u64,
) -> BTreeSet<String> {
    let mut unknowns = BTreeSet::new();
    for (_, file) in files {
        let key = file.to_string_lossy().into_owned();
        if let Some(entry) = cache.files.get(&key) {
            unknowns.extend(
                entry
                    .unknown_models
                    .iter()
                    .filter(|(_, ts_secs)| within_widest_window(**ts_secs, now_secs))
                    .map(|(model, _)| model.clone()),
            );
        }
    }
    unknowns
}

fn has_healed_unknown(entry: &FileCacheEntry, prices: &PriceBook, now_secs: u64) -> bool {
    entry.unknown_models.iter().any(|(model, ts_secs)| {
        within_widest_window(*ts_secs, now_secs) && prices.price(model).is_some()
    })
}

fn prune_spending_cache(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &mut SpendingDiskCache,
    now_secs: u64,
) -> bool {
    let discovered: BTreeSet<String> = files
        .iter()
        .map(|(_, file)| file.to_string_lossy().into_owned())
        .collect();
    let before_files = cache.files.len();
    cache
        .files
        .retain(|key, _| discovered.contains(key) || Path::new(key.as_str()).exists());
    let mut changed = cache.files.len() != before_files;
    for entry in cache.files.values_mut() {
        let before_entries = entry.entries.len();
        entry
            .entries
            .retain(|entry| within_widest_window(entry.ts_secs, now_secs));
        changed |= entry.entries.len() != before_entries;
        let before_unknowns = entry.unknown_models.len();
        entry
            .unknown_models
            .retain(|_, ts_secs| within_widest_window(*ts_secs, now_secs));
        changed |= entry.unknown_models.len() != before_unknowns;
    }
    changed
}

struct DedupedCachedEntries {
    by_exact_key: HashMap<(String, Option<String>), (&'static str, CachedEntry)>,
    msg_has_non_sidechain: HashMap<String, bool>,
    free_entries: Vec<(&'static str, CachedEntry)>,
}

fn dedup_cached_entries(
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &SpendingDiskCache,
) -> DedupedCachedEntries {
    let mut deduped = DedupedCachedEntries {
        by_exact_key: HashMap::new(),
        msg_has_non_sidechain: HashMap::new(),
        free_entries: Vec::new(),
    };
    for (adapter, file) in files {
        let kind = adapter.descriptor().kind;
        let key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get(&key) else {
            continue;
        };
        for entry in &cached_file.entries {
            insert_dedup_entry(&mut deduped, kind, entry);
        }
    }
    deduped
}

fn insert_dedup_entry(deduped: &mut DedupedCachedEntries, kind: &'static str, entry: &CachedEntry) {
    let Some(ref msg_id) = entry.message_id else {
        deduped.free_entries.push((kind, entry.clone()));
        return;
    };
    let has_non_sidechain = deduped
        .msg_has_non_sidechain
        .entry(msg_id.clone())
        .or_insert(false);
    if !entry.is_sidechain {
        *has_non_sidechain = true;
    }
    let exact_key = (msg_id.clone(), entry.request_id.clone());
    deduped
        .by_exact_key
        .entry(exact_key)
        .and_modify(|(_, existing)| {
            if existing.is_sidechain && !entry.is_sidechain {
                *existing = entry.clone();
            }
        })
        .or_insert_with(|| (kind, entry.clone()));
}

fn counted_entries(deduped: &DedupedCachedEntries) -> Vec<(&'static str, &CachedEntry)> {
    let mut counted = Vec::new();
    for ((msg_id, _), (kind, entry)) in &deduped.by_exact_key {
        let is_sidechain_replay = entry.is_sidechain
            && deduped
                .msg_has_non_sidechain
                .get(msg_id.as_str())
                .copied()
                .unwrap_or(false);
        if !is_sidechain_replay {
            counted.push((*kind, entry));
        }
    }
    counted.extend(
        deduped
            .free_entries
            .iter()
            .map(|(provider, entry)| (*provider, entry)),
    );
    counted
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
    let zone = tz
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .and_then(|name| TimeZone::get(name).ok())
        .unwrap_or_else(TimeZone::system);
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

fn within_widest_window(ts_secs: u64, now_secs: u64) -> bool {
    now_secs.saturating_sub(ts_secs) < WIDEST_SPEND_WINDOW_SECS
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

/// `(mtime_secs, len)` from one stat. `(0, 0)` on any error — the file then
/// reads as an empty cold parse rather than failing the pass.
fn file_stat(path: &Path) -> (u64, u64) {
    let Ok(meta) = fs::metadata(path) else {
        return (0, 0);
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (mtime, meta.len())
}

// ── Cache I/O ─────────────────────────────────────────────────────────────────

/// Load the cache, discarding any cache written under an older parse shape so
/// every file re-parses once under the current one. The returned cache always
/// carries the current version, so the next [`write_spending_cache`] stamps it.
pub fn read_spending_cache(path: &Path) -> SpendingDiskCache {
    let mut cache = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SpendingDiskCache>(&bytes).ok())
        .filter(|cache| cache.version == SPENDING_CACHE_VERSION)
        .unwrap_or_default();
    cache.version = SPENDING_CACHE_VERSION;
    cache
}

/// Atomic write: temp file + rename, matching the project's ledger durability
/// contract.
pub fn write_spending_cache(path: &Path, cache: &SpendingDiskCache) {
    let _ = crate::ledger::atomic::sweep_stale_temp_siblings(
        path,
        std::time::Duration::from_secs(3_600),
    );
    let _ = crate::ledger::atomic::write_temp_then_rename_cache(path, cache);
}

// ── Provider-spending cache ───────────────────────────────────────────────────

/// The published provider-spending cache: the aggregated [`Spending`] plus the
/// stamp the producer's [`SPENDING_TTL`] gate reads. A wrapper rather than a
/// field on [`Spending`] keeps the in-memory value the fold path threads
/// stamp-free; `#[serde(flatten)]` keeps a pre-stamp file (a bare `Spending`)
/// readable — its values survive, with `version` and `refreshed_at_ms`
/// defaulting to 0 so it reads as stale and refreshes once. A later aggregate
/// semantic change bumps `version` without forcing raw JSONL re-parse.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderSpendingCache {
    /// Aggregate semantic version for the TTL gate.
    #[serde(default)]
    pub version: u32,
    /// When the producer last walked and published, for the TTL gate.
    #[serde(default)]
    pub refreshed_at_ms: u64,
    /// Account-global UTC-day buckets keyed by epoch day, published so the stats
    /// command can render its heatmap without walking transcripts.
    #[serde(default)]
    pub days: BTreeMap<i64, DaySpend>,
    /// Account-global model buckets, published so `rimz stats` can render its
    /// model breakdown without walking transcripts.
    #[serde(default)]
    pub models: BTreeMap<String, ModelSpend>,
    #[serde(flatten)]
    pub spending: Spending,
}

impl ProviderSpendingCache {
    /// Whether this cache carries the current published aggregate shape.
    pub fn is_current_version(&self) -> bool {
        self.version == PROVIDER_SPENDING_VERSION
    }

    /// Whether the published walk is young enough that the producer skips the
    /// transcript walk this tick. Saturating, so a clock that ran backwards
    /// reads fresh rather than re-walking every tick.
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        self.is_current_version()
            && now_ms.saturating_sub(self.refreshed_at_ms) <= SPENDING_TTL.as_millis() as u64
    }
}

/// Atomic write of the aggregated `Spending`, stamped `refreshed_at_ms`, so
/// consumer sidebar tabs read the fleet and per-provider totals — and the
/// producer its own [`SPENDING_TTL`] gate — without re-walking the JSONL
/// transcript history. Follows the same temp-then-rename durability contract
/// as [`write_spending_cache`].
pub fn write_provider_spending_cache(path: &Path, refreshed_at_ms: u64, spending: &Spending) {
    let days = BTreeMap::new();
    let models = BTreeMap::new();
    write_provider_spending_cache_with_rollups(path, refreshed_at_ms, spending, &days, &models);
}

/// Atomic write of the provider aggregate plus the rollups consumed by
/// `rimz stats`.
pub fn write_provider_spending_cache_with_rollups(
    path: &Path,
    refreshed_at_ms: u64,
    spending: &Spending,
    days: &BTreeMap<i64, DaySpend>,
    models: &BTreeMap<String, ModelSpend>,
) {
    let cache = ProviderSpendingCache {
        version: PROVIDER_SPENDING_VERSION,
        refreshed_at_ms,
        days: days.clone(),
        models: models.clone(),
        spending: spending.clone(),
    };
    let _ = crate::ledger::atomic::sweep_stale_temp_siblings(
        path,
        std::time::Duration::from_secs(3_600),
    );
    let _ = crate::ledger::atomic::write_temp_then_rename_cache(path, &cache);
}

/// Read the provider-spending cache written by [`write_provider_spending_cache`].
/// Returns a default (stamp 0, so it reads as stale) on any error so callers
/// always get a usable value; a pre-stamp file deserializes with its spending
/// values intact.
pub fn read_provider_spending_cache(path: &Path) -> ProviderSpendingCache {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ProviderSpendingCache>(&bytes).ok())
        .unwrap_or_default()
}

/// The published per-workspace cockpit spending cache. The filename is keyed by
/// the scope hash; the hash rides in the file too so a stale or renamed file
/// cannot satisfy a different scope.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSpendingCache {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub refreshed_at_ms: u64,
    #[serde(default)]
    pub scope_hash: String,
    #[serde(default)]
    pub tally: SpendTally,
}

impl WorkspaceSpendingCache {
    pub fn is_fresh(&self, now_ms: u64, scope_hash: &str) -> bool {
        self.version == WORKSPACE_SPENDING_VERSION
            && self.scope_hash == scope_hash
            && now_ms.saturating_sub(self.refreshed_at_ms) <= SPENDING_TTL.as_millis() as u64
    }
}

pub fn write_workspace_spending_cache(
    path: &Path,
    refreshed_at_ms: u64,
    scope_hash: &str,
    tally: &SpendTally,
) {
    let cache = WorkspaceSpendingCache {
        version: WORKSPACE_SPENDING_VERSION,
        refreshed_at_ms,
        scope_hash: scope_hash.to_owned(),
        tally: tally.clone(),
    };
    let _ = crate::ledger::atomic::write_temp_then_rename_cache(path, &cache);
}

pub fn read_workspace_spending_cache(path: &Path) -> WorkspaceSpendingCache {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<WorkspaceSpendingCache>(&bytes).ok())
        .unwrap_or_default()
}

/// Per-workspace live-cost baselines for the cockpit's between-walk count-up.
/// The shared provider-spending cache is account-global; these baselines are
/// room-local because row ids and live statusline costs belong to one rendered
/// workspace.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveSpendBaselines {
    #[serde(default)]
    pub observed_walk_ms: u64,
    #[serde(default)]
    pub baselines: BTreeMap<String, f64>,
}

pub fn read_live_spend_baselines(path: &Path) -> LiveSpendBaselines {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<LiveSpendBaselines>(&bytes).ok())
        .unwrap_or_default()
}

pub fn write_live_spend_baselines(path: &Path, baselines: &LiveSpendBaselines) {
    let _ = crate::ledger::atomic::write_temp_then_rename_cache(path, baselines);
}

/// Headline spend as the cockpit paints it: the walked tally's exact figure plus
/// each live session's overshoot over the baseline captured when the walk
/// published — so the headline climbs the instant a session's statusline cost
/// moves, while the walk stays the truth it reconciles to on the next publish.
/// Pure presentation over `(session id, cost now, registered-at ms)` triples: a
/// baselined session adds `max(0, cost_now − baseline)` (a resumed or reset
/// session clamps to zero rather than rolling the headline backwards); a session
/// absent from the baselines adds its whole cost when it registered after the
/// publish stamp — the walk never saw it — and nothing otherwise, the fail-safe
/// undercount that heals on the next walk.
pub fn today_spend_live_usd<'a>(
    walked_headline_usd: f64,
    live_costs: impl Iterator<Item = (&'a str, f64, Option<u64>)>,
    baselines: &BTreeMap<String, f64>,
    published_at_ms: u64,
) -> f64 {
    let overshoot: f64 = live_costs
        .map(|(id, cost_now, registered_at_ms)| match baselines.get(id) {
            Some(baseline) => (cost_now - baseline).max(0.0),
            None if registered_at_ms.is_some_and(|at| at > published_at_ms) => cost_now.max(0.0),
            None => 0.0,
        })
        .sum();
    walked_headline_usd + overshoot
}

// ── Date utilities ────────────────────────────────────────────────────────────

/// The wall clock as Unix seconds — the `now_secs` a caller captures once and
/// threads into [`compute_spending`], which stays pure over it.
pub fn unix_secs_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Format a Unix timestamp in seconds as a UTC date: `"YYYY-MM-DD"`.
pub fn utc_date(secs: u64) -> String {
    civil_date_from_epoch_days((secs / 86_400) as i64)
}

/// Parse an ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SS…`, e.g. a JSONL
/// `timestamp`) to Unix seconds. Reads fixed offsets; the time-of-day is optional
/// (a bare `YYYY-MM-DD` parses to midnight). Returns `None` when the date prefix
/// is malformed — the same guard the parsers applied to the old date slice.
pub(crate) fn iso_to_unix_secs(ts: &str) -> Option<u64> {
    let bytes = ts.as_bytes();
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    let year: i64 = ts.get(0..4)?.parse().ok()?;
    let month: i64 = ts.get(5..7)?.parse().ok()?;
    let day: i64 = ts.get(8..10)?.parse().ok()?;
    let hour: i64 = ts.get(11..13).and_then(|s| s.parse().ok()).unwrap_or(0);
    let min: i64 = ts.get(14..16).and_then(|s| s.parse().ok()).unwrap_or(0);
    let sec: i64 = ts.get(17..19).and_then(|s| s.parse().ok()).unwrap_or(0);
    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + min * 60 + sec;
    u64::try_from(secs).ok()
}

/// Days since the Unix epoch (1970-01-01 = 0) for a civil date — the inverse of
/// [`civil_date_from_epoch_days`]. Howard Hinnant's algorithm:
/// <http://howardhinnant.github.io/date_algorithms.html>
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Convert days since the Unix epoch (1970-01-01 = 0) to `"YYYY-MM-DD"`.
///
/// Uses Howard Hinnant's civil-from-days algorithm:
/// <http://howardhinnant.github.io/date_algorithms.html>
fn civil_date_from_epoch_days(z: i64) -> String {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
