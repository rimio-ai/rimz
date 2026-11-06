//! Per-model token pricing — the table that turns token counts into dollars.
//!
//! Token-only providers — Claude and Codex — need this: their spend
//! ([`super::spending`]) can only be totalled by multiplying each turn's tokens
//! by a price, since current Claude transcripts and every Codex rollout log token
//! counts rather than a `costUSD`. Pi normally reports `costUSD` directly and
//! consults the table only for token-bearing records where that value is absent.
//!
//! Readers open only the shared table once it is usable. The embedded snapshot
//! is a fresh-install seed and a read-only fallback until the spending walker
//! builds the first nonempty table, even offline. Fresh clones without a generated
//! snapshot embed an empty seed.
//!
//! The walker alone refreshes LiteLLM and models.dev under its spending lock,
//! carrying forward previous rows before applying the projection. Weekly refresh
//! and an escalating unknown-model chase share a separate small state file.
//! Failed fetches never rewrite a usable table.
//!
//! Exact and boundary-aware prefix lookups are network-free. Books share a
//! mutexed SQLite connection and per-name result memos across workers, and the
//! process memo keeps a book until the file's identity changes. Managed rates
//! layer exact overrides and a lazy transform over the same backing.

mod embedded;
mod overrides;
#[doc(hidden)]
pub mod source;
mod sqlite;

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::context::CacheWriteRetention;
use super::spending::is_priceable_model_name;

const CACHE_CREATE_1H_INPUT_MULTIPLIER: f64 = 2.0;
const DEFAULT_LONG_CONTEXT_THRESHOLD_TOKENS: u64 = 200_000;

pub(super) fn scoped_model_key(provider: &str, model: &str) -> String {
    format!("{}/{model}", provider.replace('-', "_"))
}

/// Estimate rebuilding a context through the same cache-write pricing as spending.
/// Only an observed one-hour write selects that tier, not a configured lifetime.
pub fn cache_rewarm_cost_usd(
    book: &PriceBook,
    model: &str,
    context_tokens: u64,
    last_cache_write: Option<CacheWriteRetention>,
) -> Option<f64> {
    let split = if last_cache_write == Some(CacheWriteRetention::OneHour) {
        TokenSplit {
            cache_write_1h: context_tokens,
            ..TokenSplit::default()
        }
    } else {
        TokenSplit {
            cache_write: context_tokens,
            ..TokenSplit::default()
        }
    };
    match super::spending::lookup_split_price(book, model, split) {
        super::spending::SplitPrice::Priced(cost) => Some(cost),
        _ => None,
    }
}

/// The token counts one priced request consumed. Providers fill the fields
/// their wire exposes and leave the rest at zero. `cache_write_1h` carries
/// Claude's cache tier; `fast` carries Claude, Codex, and Cursor priority turns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenSplit {
    /// Fresh (uncached) input tokens.
    pub input: u64,
    /// Output tokens, already including reasoning tokens.
    pub output: u64,
    /// Cache-creation tokens billed at the 5-minute rate.
    pub cache_write: u64,
    /// Cache-creation tokens billed at the 1-hour rate.
    pub cache_write_1h: u64,
    /// Cache-read (prompt-cache-hit) input tokens.
    pub cache_read: u64,
    /// The provider recorded a fast/priority turn.
    pub fast: bool,
}

impl TokenSplit {
    /// The uncached case: fresh input and output only.
    pub(super) fn new(input: u64, output: u64) -> Self {
        Self {
            input,
            output,
            ..Self::default()
        }
    }

    /// Add 5-minute cache-creation and cache-read counts.
    pub(super) fn cached(self, cache_write: u64, cache_read: u64) -> Self {
        Self {
            cache_write,
            cache_read,
            ..self
        }
    }

    /// Mark the turn as fast/priority; the selected rates supply its multiplier.
    pub(super) fn fast(self, fast: bool) -> Self {
        Self { fast, ..self }
    }

    /// The request consumed no tokens at all. The tier flags are not counts, so
    /// a `fast` turn with empty usage is still empty.
    pub(super) fn is_empty(&self) -> bool {
        self.input == 0
            && self.output == 0
            && self.cache_write == 0
            && self.cache_write_1h == 0
            && self.cache_read == 0
    }
}

/// Per-token costs in USD for one model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Pricing {
    /// Cost per (uncached) input token.
    pub input: f64,
    /// Cost per output token (output already includes reasoning tokens).
    pub output: f64,
    /// Cost per cache-read (prompt-cache-hit) input token.
    pub cache_read: f64,
    /// Cost per cache-creation input token.
    pub cache_create: f64,
    /// Optional cost per 1-hour cache-creation token. When absent, Anthropic's
    /// standard 2x-input rate applies.
    pub cache_create_1h: Option<f64>,
    /// Whether `cache_read` came from source data rather than the input-rate
    /// default.
    pub cache_read_explicit: bool,
    /// Cost per uncached input token above the 200k tier threshold.
    pub input_above_200k: Option<f64>,
    /// Cost per output token above the 200k tier threshold.
    pub output_above_200k: Option<f64>,
    /// Cost per 5-minute cache-creation token above the 200k tier threshold.
    pub cache_create_above_200k: Option<f64>,
    /// Cost per cache-read token above the 200k tier threshold.
    pub cache_read_above_200k: Option<f64>,
    /// Request input boundary for an all-or-nothing long-context tier.
    /// `None` keeps LiteLLM's marginal 200k semantics.
    pub long_context_threshold: Option<u64>,
    /// List-price multiplier when the provider records a fast/priority turn.
    pub fast_multiplier: f64,
    /// Maximum accepted input/context tokens when the pricing source publishes
    /// an exact positive integer capacity.
    pub max_input_tokens: Option<u64>,
}

/// Select list tiers or base rates with a caller-supplied Fast multiplier.
#[derive(Clone, Copy, Debug)]
pub(super) enum PricingRates {
    List,
    Base { fast_multiplier: f64 },
}

impl Pricing {
    pub(super) const fn empty() -> Self {
        Self {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_create: 0.0,
            cache_create_1h: None,
            cache_read_explicit: false,
            input_above_200k: None,
            output_above_200k: None,
            cache_create_above_200k: None,
            cache_read_above_200k: None,
            long_context_threshold: None,
            fast_multiplier: 1.0,
            max_input_tokens: None,
        }
    }

    pub(super) const fn from_base_rates(
        input: f64,
        output: f64,
        cache_create: Option<f64>,
        cache_read: Option<f64>,
    ) -> Self {
        Self {
            input,
            output,
            cache_create: match cache_create {
                Some(rate) => rate,
                None => input * 1.25,
            },
            cache_read: match cache_read {
                Some(rate) => rate,
                None => input * 0.1,
            },
            cache_read_explicit: cache_read.is_some(),
            ..Self::empty()
        }
    }

    pub(super) fn cost_of_with_rates(self, split: TokenSplit, rates: PricingRates) -> f64 {
        let price = match rates {
            PricingRates::List => self,
            PricingRates::Base { fast_multiplier } => Self {
                long_context_threshold: None,
                input_above_200k: None,
                output_above_200k: None,
                cache_create_above_200k: None,
                cache_read_above_200k: None,
                fast_multiplier,
                ..self
            },
        };
        price.cost_of(split)
    }

    pub(super) fn cost_of(self, split: TokenSplit) -> f64 {
        let TokenSplit {
            input,
            output,
            cache_write: cache_5m,
            cache_write_1h: cache_1h,
            cache_read,
            fast,
        } = split;
        let cache_1h_cost = self
            .cache_create_1h
            .unwrap_or(self.input * CACHE_CREATE_1H_INPUT_MULTIPLIER);
        let cache_1h_above = if self.cache_create_1h.is_some() {
            None
        } else {
            self.input_above_200k
                .map(|cost| cost * CACHE_CREATE_1H_INPUT_MULTIPLIER)
        };
        let multiplier = if fast { self.fast_multiplier } else { 1.0 };
        let cost = if let Some(threshold) = self.long_context_threshold {
            let request_input = input
                .saturating_add(cache_5m)
                .saturating_add(cache_1h)
                .saturating_add(cache_read);
            let long = request_input > threshold;
            let rate = |base: f64, above: Option<f64>| {
                if long { above.unwrap_or(base) } else { base }
            };
            input as f64 * rate(self.input, self.input_above_200k)
                + output as f64 * rate(self.output, self.output_above_200k)
                + cache_5m as f64 * rate(self.cache_create, self.cache_create_above_200k)
                + cache_1h as f64 * rate(cache_1h_cost, cache_1h_above)
                + cache_read as f64 * rate(self.cache_read, self.cache_read_above_200k)
        } else {
            tiered_cost(input, self.input, self.input_above_200k)
                + tiered_cost(output, self.output, self.output_above_200k)
                + tiered_cost(cache_5m, self.cache_create, self.cache_create_above_200k)
                + tiered_cost(cache_1h, cache_1h_cost, cache_1h_above)
                + tiered_cost(cache_read, self.cache_read, self.cache_read_above_200k)
        };
        cost * multiplier
    }

    /// Price session-cumulative token totals at base rates. A session sum loses
    /// the per-request boundaries needed to apply long-context tiers exactly.
    pub(super) fn session_cost(
        self,
        input: u64,
        output: u64,
        cache_create: u64,
        cache_read: u64,
    ) -> f64 {
        input as f64 * self.input
            + output as f64 * self.output
            + cache_create as f64 * self.cache_create
            + cache_read as f64 * self.cache_read
    }
}

/// A resolved model → price table. Clones share the backing and result memos.
#[derive(Clone)]
pub struct PriceBook {
    backing: Arc<Backing>,
    layer: Option<Arc<PriceLayer>>,
    identity: Arc<()>,
    // File replacement retires these per-name memos with the immutable book.
    exact_cache: Arc<Mutex<HashMap<String, Option<Pricing>>>>,
    fuzzy_cache: Arc<Mutex<HashMap<String, Option<Pricing>>>>,
}

#[derive(Debug)]
enum Backing {
    Memory(HashMap<String, Pricing>),
    Sqlite(sqlite::Table),
}

struct PriceLayer {
    base: PriceBook,
    transform: Box<dyn Fn(Pricing) -> Pricing + Send + Sync>,
    overrides: HashMap<String, Pricing>,
}

impl std::fmt::Debug for PriceBook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PriceBook")
            .field("backing", &self.backing)
            .field("layered", &self.layer.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for PriceBook {
    fn default() -> Self {
        Self::from_entries(HashMap::new())
    }
}

impl PriceBook {
    /// Build a book from an arbitrary LiteLLM-shaped document (tests, tooling).
    pub fn from_litellm_json(json: &str) -> Self {
        Self::from_entries(embedded::parse(json))
    }

    fn from_entries(entries: HashMap<String, Pricing>) -> Self {
        Self::new(Backing::Memory(entries))
    }

    fn new(backing: Backing) -> Self {
        Self {
            backing: Arc::new(backing),
            layer: None,
            identity: Arc::default(),
            exact_cache: Arc::default(),
            fuzzy_cache: Arc::default(),
        }
    }

    pub(super) fn derive_with_exact_overrides(
        &self,
        map: impl Fn(Pricing) -> Pricing + Send + Sync + 'static,
        overrides: impl IntoIterator<Item = (String, Pricing)>,
    ) -> Self {
        Self {
            backing: Arc::clone(&self.backing),
            layer: Some(Arc::new(PriceLayer {
                base: self.clone(),
                transform: Box::new(map),
                overrides: overrides.into_iter().collect(),
            })),
            identity: Arc::default(),
            exact_cache: Arc::default(),
            fuzzy_cache: Arc::default(),
        }
    }

    pub(super) fn identity(&self) -> Arc<()> {
        Arc::clone(&self.identity)
    }

    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        Self::from_litellm_json(include_str!("tests/fixtures/prices.json"))
    }

    #[cfg(test)]
    pub(crate) fn write_fixture_cache(path: &Path) {
        testkit::seed_cache_at(path, include_str!("tests/fixtures/prices.json"));
    }

    /// Resolve the price for `model`: exact match, then longest normalized prefix.
    pub(super) fn price(&self, model: &str) -> Option<Pricing> {
        let key = model.trim();
        if let Some(price) = self.exact_price(key) {
            return Some(price);
        }
        if let Some(cached) = self
            .fuzzy_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
        {
            return *cached;
        }
        let result = if let Some(layer) = &self.layer {
            layer.base.price(key).map(&layer.transform)
        } else {
            let want = normalize(key);
            match self.backing.as_ref() {
                Backing::Memory(entries) => entries
                    .iter()
                    .filter_map(|(key, price)| {
                        let norm = normalize(key);
                        prefix_at_boundary(&want, &norm).then_some((norm.len(), key, price))
                    })
                    .min_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(right.1)))
                    .map(|(_, _, price)| *price),
                Backing::Sqlite(table) => table.fuzzy(&want).ok().flatten(),
            }
        };
        self.fuzzy_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_owned(), result);
        result
    }

    /// Resolve only an exact stored model id. Capacity and locally estimated
    /// spend use this conservative path so a related model cannot lend another
    /// selector its limits or rates.
    pub(super) fn exact_price(&self, model: &str) -> Option<Pricing> {
        let key = model.trim();
        if let Some(cached) = self
            .exact_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
        {
            return *cached;
        }
        let result = if let Some(layer) = &self.layer {
            layer
                .overrides
                .get(key)
                .copied()
                .or_else(|| layer.base.exact_price(key).map(&layer.transform))
        } else {
            match self.backing.as_ref() {
                Backing::Memory(entries) => entries.get(key).copied(),
                Backing::Sqlite(table) => table.exact(key).ok().flatten(),
            }
        };
        self.exact_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_owned(), result);
        result
    }

    fn fetched_at_secs(&self) -> u64 {
        match self.backing.as_ref() {
            Backing::Memory(_) => 0,
            Backing::Sqlite(table) => table.fetched_at_secs,
        }
    }

    fn fingerprint(&self) -> Option<String> {
        match self.backing.as_ref() {
            Backing::Memory(_) => None,
            Backing::Sqlite(table) => Some(table.build_id.clone()),
        }
    }
}

/// Load the shared book under the spending lock, building a missing table from
/// a nonempty seed even offline. Weekly refresh and the chase share separate retry state.
pub(super) fn load_for_spending(
    runtime: &crate::RuntimePaths,
    unknown_models: &BTreeSet<String>,
) -> Arc<PriceBook> {
    load_for_spending_with(
        runtime,
        unknown_models,
        unix_secs_now,
        source::offline(),
        embedded::load,
        source::fetch_litellm,
        source::fetch_models_dev,
    )
}

fn load_for_spending_with(
    runtime: &crate::RuntimePaths,
    unknown_models: &BTreeSet<String>,
    now: impl FnOnce() -> u64,
    offline: bool,
    seed: impl FnOnce() -> HashMap<String, Pricing>,
    fetch_litellm: impl FnOnce() -> Option<String>,
    fetch_models_dev: impl FnOnce() -> Option<String>,
) -> Arc<PriceBook> {
    let path = runtime.shared_pricing_table_path();
    let state_path = runtime.shared_pricing_refresh_path();
    let mut book = cached_book(&path);
    let mut state: RefreshState = crate::disk::atomic::read_json_cache(&state_path);
    let previous_state = state.clone();
    let pending = unpriced_subset(&book, unknown_models);
    let now = now();
    if pending.is_empty() {
        clear_unknown_chase(&mut state);
    }
    let projection = if should_refresh(&state, book.fetched_at_secs(), now, offline, &pending) {
        state.last_attempt_secs = now;
        if !pending.is_empty() {
            note_chase_attempt(&mut state, now, pending);
        }
        fetch_projection(fetch_litellm, fetch_models_dev)
    } else {
        None
    };
    if book.fingerprint().is_none() || projection.is_some() {
        let fetched_at = if projection.is_some() { now } else { 0 };
        match rebuild_table(&path, &book, seed, projection, fetched_at) {
            Ok(()) => book = cached_book(&path),
            Err(error) => {
                tracing::debug!(%error, path = %path.display(), "price table build failed")
            }
        }
    }
    if unpriced_subset(&book, unknown_models).is_empty() {
        clear_unknown_chase(&mut state);
    }
    if state != previous_state
        && let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(&state_path, &state)
    {
        tracing::debug!(%error, path = %state_path.display(), "pricing refresh state write failed");
    }
    book
}

fn rebuild_table(
    path: &Path,
    book: &PriceBook,
    seed: impl FnOnce() -> HashMap<String, Pricing>,
    projection: Option<HashMap<String, Pricing>>,
    fetched_at: u64,
) -> Result<(), sqlite::TableErr> {
    let mut rows = match book.backing.as_ref() {
        Backing::Memory(_) => seed(),
        Backing::Sqlite(table) => table.rows()?,
    };
    if let Some(projection) = projection {
        rows.extend(projection);
    }
    if rows.is_empty() {
        return Ok(());
    }
    sqlite::build(path, &rows, fetched_at)
}

type CacheStamp = (u64, u64, SystemTime, u64);
type CachedBookMemo = Option<(PathBuf, Option<CacheStamp>, Arc<PriceBook>)>;

static CACHED_BOOK_MEMO: LazyLock<Mutex<CachedBookMemo>> = LazyLock::new(|| Mutex::new(None));

#[cfg(feature = "testkit")]
#[doc(hidden)]
pub mod testkit {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{CACHED_BOOK_MEMO, PoisonError, embedded, sqlite};

    static BOOK_LOADS: AtomicU64 = AtomicU64::new(0);

    pub fn book_loads() -> u64 {
        BOOK_LOADS.load(Ordering::Relaxed)
    }

    /// Seed the shared price cache from a LiteLLM-shaped document and return its path.
    #[track_caller]
    pub fn seed_cache(runtime: &crate::RuntimePaths, json: &str) -> PathBuf {
        let path = runtime.shared_pricing_table_path();
        seed_cache_at(&path, json);
        path
    }

    /// Seed an explicit price-cache path using the upstream projection parser.
    #[track_caller]
    pub fn seed_cache_at(path: &Path, json: &str) {
        sqlite::build(path, &embedded::parse(json), 0).expect("pricing fixture table");
    }

    /// Forget the process-wide book without changing the cumulative load counter.
    pub fn reset_cached_book() {
        *CACHED_BOOK_MEMO
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    pub(super) fn count_load() {
        BOOK_LOADS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Open the current read-only shared table, falling back to the fresh-install
/// snapshot only when the file is unusable. Never refreshes or writes.
pub fn cached_book(cache_path: &Path) -> Arc<PriceBook> {
    cached_book_with_fingerprint(cache_path).0
}

/// Load the memoized book and its build id, read through the connection serving
/// its prices. A snapshot fallback has no fingerprint.
pub fn cached_book_with_fingerprint(cache_path: &Path) -> (Arc<PriceBook>, Option<String>) {
    let stamp = cache_stamp(cache_path);
    let mut memo = CACHED_BOOK_MEMO
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some((memo_path, memo_stamp, book)) = memo.as_ref()
        && memo_path == cache_path
        && *memo_stamp == stamp
    {
        return (Arc::clone(book), book.fingerprint());
    }
    #[cfg(feature = "testkit")]
    testkit::count_load();
    let book = Arc::new(match sqlite::Table::open(cache_path) {
        Some(table) => PriceBook::new(Backing::Sqlite(table)),
        None => PriceBook::from_entries(embedded::load()),
    });
    let fingerprint = book.fingerprint();
    *memo = Some((cache_path.to_owned(), stamp, Arc::clone(&book)));
    (book, fingerprint)
}

fn cache_stamp(path: &Path) -> Option<CacheStamp> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(path).ok()?;
    Some((
        metadata.dev(),
        metadata.ino(),
        metadata.modified().ok()?,
        metadata.len(),
    ))
}

fn fetch_projection(
    fetch_litellm: impl FnOnce() -> Option<String>,
    fetch_models_dev: impl FnOnce() -> Option<String>,
) -> Option<HashMap<String, Pricing>> {
    // Both documents or none; a first-source outage skips the second download.
    let litellm = fetch_litellm()?;
    let models_dev = fetch_models_dev()?;
    let (snapshot, _) = source::project_sources(&litellm, Some(&models_dev)).ok()?;
    let models = embedded::parse(&serde_json::to_string(&snapshot).ok()?);
    (!models.is_empty()).then_some(models)
}

// ── Disk cache ──────────────────────────────────────────────────────────────

/// Refetch once a week; on failure, back off an hour before retrying so a
/// persistent network outage never fetches on every snapshot.
const REFRESH_TTL_SECS: u64 = 7 * 24 * 60 * 60;
const RETRY_BACKOFF_SECS: u64 = 60 * 60;
/// Chase a newly observed unpriced model after 30 minutes, then double up to the
/// 24-hour cap while the same unknown set persists.
const UNKNOWN_REFRESH_TTL_SECS: u64 = 30 * 60;
const UNKNOWN_BACKOFF_CAP_SECS: u64 = 24 * 60 * 60;
/// Walker-only refresh and unknown-model chase state, separate from price rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct RefreshState {
    last_attempt_secs: u64,
    unknown_attempt_secs: u64,
    unknown_backoff_secs: u64,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    unknown_seen: BTreeSet<String>,
}

fn tiered_cost(tokens: u64, base: f64, above: Option<f64>) -> f64 {
    if tokens == 0 {
        return 0.0;
    }
    if let Some(above) = above
        && tokens > DEFAULT_LONG_CONTEXT_THRESHOLD_TOKENS
    {
        return DEFAULT_LONG_CONTEXT_THRESHOLD_TOKENS as f64 * base
            + (tokens - DEFAULT_LONG_CONTEXT_THRESHOLD_TOKENS) as f64 * above;
    }
    tokens as f64 * base
}

fn should_refresh(
    cache: &RefreshState,
    fetched_at_secs: u64,
    now: u64,
    offline: bool,
    pending: &BTreeSet<String>,
) -> bool {
    if offline {
        return false;
    }
    baseline_refresh_due(cache, fetched_at_secs, now) || unknown_refresh_due(cache, now, pending)
}

fn baseline_refresh_due(cache: &RefreshState, fetched_at_secs: u64, now: u64) -> bool {
    now.saturating_sub(fetched_at_secs) > REFRESH_TTL_SECS
        && now.saturating_sub(cache.last_attempt_secs) > RETRY_BACKOFF_SECS
}

fn unknown_refresh_due(cache: &RefreshState, now: u64, pending: &BTreeSet<String>) -> bool {
    if pending.is_empty() || now.saturating_sub(cache.last_attempt_secs) <= UNKNOWN_REFRESH_TTL_SECS
    {
        return false;
    }
    let (_, gate) = chase_gate(cache, pending);
    now.saturating_sub(cache.unknown_attempt_secs) > gate
}

fn unpriced_subset(book: &PriceBook, unknowns: &BTreeSet<String>) -> BTreeSet<String> {
    unknowns
        .iter()
        .filter(|model| is_priceable_model_name(model) && book.price(model).is_none())
        .cloned()
        .collect()
}

fn note_chase_attempt(cache: &mut RefreshState, now: u64, pending: BTreeSet<String>) {
    if pending.is_empty() {
        clear_unknown_chase(cache);
        return;
    }
    let (new_sighting, gate) = chase_gate(cache, &pending);
    cache.unknown_attempt_secs = now;
    cache.unknown_seen = pending;
    cache.unknown_backoff_secs = if new_sighting {
        UNKNOWN_REFRESH_TTL_SECS
    } else {
        gate.saturating_mul(2).min(UNKNOWN_BACKOFF_CAP_SECS)
    };
}

fn chase_gate(cache: &RefreshState, pending: &BTreeSet<String>) -> (bool, u64) {
    let new_sighting = !pending.is_subset(&cache.unknown_seen);
    let gate = if new_sighting {
        UNKNOWN_REFRESH_TTL_SECS
    } else {
        cache.unknown_backoff_secs.max(UNKNOWN_REFRESH_TTL_SECS)
    };
    (new_sighting, gate)
}

fn clear_unknown_chase(cache: &mut RefreshState) -> bool {
    let changed = cache.unknown_attempt_secs != 0
        || cache.unknown_backoff_secs != 0
        || !cache.unknown_seen.is_empty();
    cache.unknown_attempt_secs = 0;
    cache.unknown_backoff_secs = 0;
    cache.unknown_seen.clear();
    changed
}

fn unix_secs_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Fuzzy matching ────────────────────────────────────────────────────────────

/// Normalize a model id for comparison: trimmed, lowercased, `.`/`@` → `-`.
fn normalize(model: &str) -> String {
    model.trim().to_ascii_lowercase().replace(['.', '@'], "-")
}

fn fuzzy_candidates(want: &str) -> impl Iterator<Item = &str> {
    std::iter::once(want.len())
        .chain(want.char_indices().rev().map(|(index, _)| index))
        .filter_map(|end| {
            let prefix = &want[..end];
            prefix_at_boundary(want, prefix).then_some(prefix)
        })
}

/// `true` when `key` is a prefix of `want` ending at a word boundary, excluding a
/// purely numeric (non-date) suffix that would be a version bump.
fn prefix_at_boundary(want: &str, key: &str) -> bool {
    if key.is_empty() || !want.starts_with(key) {
        return false;
    }
    let rest = &want[key.len()..];
    let Some(sep) = rest.chars().next() else {
        return true; // exact match
    };
    if sep.is_alphanumeric() {
        return false; // key ended mid-token
    }
    let next_segment: &str = rest[sep.len_utf8()..]
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or("");
    let is_numeric = !next_segment.is_empty() && next_segment.bytes().all(|b| b.is_ascii_digit());
    // An 8-digit date suffix (e.g. `20250514`) is allowed; any other pure-number
    // segment is a version bump and must not collapse onto the base model.
    !(is_numeric && next_segment.len() != 8)
}

#[cfg(test)]
mod tests;
