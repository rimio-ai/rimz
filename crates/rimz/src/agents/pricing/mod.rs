//! Per-model token pricing — the table that turns token counts into dollars.
//!
//! Token-only providers — Claude and Codex — need this: their spend
//! ([`super::spending`]) can only be totalled by multiplying each turn's tokens
//! by a price, since current Claude transcripts and every Codex rollout log token
//! counts rather than a `costUSD`. Pi normally reports `costUSD` directly and
//! consults the table only for token-bearing records where that value is absent.
//!
//! Ordered pricing passes feed one [`PriceBook`]:
//!
//! 1. **Embedded snapshot** ([`embedded`]) — the generated LiteLLM table
//!    `build.rs` gzips into release binaries. Fresh clones without the generated
//!    file embed an empty table.
//! 2. **Remote refresh** ([`source`]) — a weekly LiteLLM and models.dev
//!    projection overwrites older embedded rows; an unknown-model chase can run
//!    that same projection early.
//!
//! Lookups are pure and network-free: [`cached_book`] memoizes the merged
//! embedded and on-disk cache data by file stamp, and [`PriceBook::price`]
//! resolves a model by exact match then a boundary-aware fuzzy scan. The only
//! network is the gated refresh in [`load_for_spending`]: a weekly refresh,
//! plus an escalating unknown-model chase when a transcript names a priceable
//! model the current book cannot resolve.

mod embedded;
mod overrides;
#[doc(hidden)]
pub mod source;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::spending::is_priceable_model_name;

pub(crate) const CACHE_CREATE_1H_INPUT_MULTIPLIER: f64 = 2.0;
const DEFAULT_LONG_CONTEXT_THRESHOLD_TOKENS: u64 = 200_000;

/// The token counts one priced request consumed. Providers fill the fields
/// their wire exposes and leave the rest at zero; `cache_write_1h` and `fast`
/// carry Claude's cache-tier and priority-turn distinctions, which every other
/// provider leaves at the default.
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
    pub fn new(input: u64, output: u64) -> Self {
        Self {
            input,
            output,
            ..Self::default()
        }
    }

    /// Add 5-minute cache-creation and cache-read counts.
    pub fn cached(self, cache_write: u64, cache_read: u64) -> Self {
        Self {
            cache_write,
            cache_read,
            ..self
        }
    }

    /// Mark the turn as fast/priority, applying the model's fast multiplier.
    pub fn fast(self, fast: bool) -> Self {
        Self { fast, ..self }
    }

    /// The request consumed no tokens at all. The tier flags are not counts, so
    /// a `fast` turn with empty usage is still empty.
    pub fn is_empty(&self) -> bool {
        self.input == 0
            && self.output == 0
            && self.cache_write == 0
            && self.cache_write_1h == 0
            && self.cache_read == 0
    }
}

/// Per-token costs in USD for one model.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    /// Cost per (uncached) input token.
    pub input: f64,
    /// Cost per output token (output already includes reasoning tokens).
    pub output: f64,
    /// Cost per cache-read (prompt-cache-hit) input token.
    #[serde(default)]
    pub cache_read: f64,
    /// Cost per cache-creation input token.
    #[serde(default)]
    pub cache_create: f64,
    /// Whether `cache_read` came from source data rather than the input-rate
    /// default. Kept for cache compatibility with ccusage's pricing shape.
    #[serde(default)]
    pub cache_read_explicit: bool,
    /// Cost per uncached input token above the 200k tier threshold.
    #[serde(default)]
    pub input_above_200k: Option<f64>,
    /// Cost per output token above the 200k tier threshold.
    #[serde(default)]
    pub output_above_200k: Option<f64>,
    /// Cost per 5-minute cache-creation token above the 200k tier threshold.
    #[serde(default)]
    pub cache_create_above_200k: Option<f64>,
    /// Cost per cache-read token above the 200k tier threshold.
    #[serde(default)]
    pub cache_read_above_200k: Option<f64>,
    /// Request input boundary for an all-or-nothing long-context tier.
    /// `None` keeps LiteLLM's marginal 200k semantics.
    #[serde(default)]
    pub long_context_threshold: Option<u64>,
    /// Multiplier applied when the provider records a fast/priority turn.
    #[serde(default = "one")]
    pub fast_multiplier: f64,
    /// Maximum accepted input/context tokens when the pricing source publishes
    /// an exact positive integer capacity.
    #[serde(default)]
    pub max_input_tokens: Option<u64>,
}

impl Pricing {
    pub(crate) const fn empty() -> Self {
        Self {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_create: 0.0,
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

    pub(crate) const fn from_base_rates(
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

    pub(crate) fn cost_of(self, split: TokenSplit) -> f64 {
        let TokenSplit {
            input,
            output,
            cache_write: cache_5m,
            cache_write_1h: cache_1h,
            cache_read,
            fast,
        } = split;
        let cache_1h_cost = self.input * CACHE_CREATE_1H_INPUT_MULTIPLIER;
        let cache_1h_above = self
            .input_above_200k
            .map(|cost| cost * CACHE_CREATE_1H_INPUT_MULTIPLIER);
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
    pub(crate) fn session_cost(
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

impl Default for Pricing {
    fn default() -> Self {
        Self::empty()
    }
}

/// A resolved model → price table.
#[derive(Clone, Debug, Default)]
pub struct PriceBook {
    entries: HashMap<String, Pricing>,
    /// Memoizes [`PriceBook::fuzzy`]'s boundary-aware scan, keyed by the trimmed
    /// lookup name, so a model that misses the exact-match map pays the linear
    /// scan once rather than on every per-entry `price` call across a spend
    /// walk. Misses cache as `None` to short-circuit the unknown-model chase's
    /// repeated lookups. `Arc<Mutex<…>>` keeps the derived `Clone` and lets the
    /// spend walk's worker threads share one memo; entries never mutate after
    /// construction, so a cached result can never go stale, and a rebuilt book
    /// (the only way entries change) starts with a fresh memo.
    fuzzy_cache: Arc<Mutex<HashMap<String, Option<Pricing>>>>,
}

impl PriceBook {
    /// The no-network book from the embedded upstream snapshot.
    pub fn embedded() -> Self {
        let entries = embedded::load();
        Self {
            entries,
            fuzzy_cache: Arc::default(),
        }
    }

    /// Build a book from an arbitrary LiteLLM-shaped document (tests, tooling).
    pub fn from_litellm_json(json: &str) -> Self {
        let entries = embedded::parse(json);
        Self {
            entries,
            fuzzy_cache: Arc::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        Self::from_litellm_json(include_str!("tests/fixtures/prices.json"))
    }

    #[cfg(test)]
    pub(crate) fn write_fixture_cache(path: &Path) {
        let fixture = Self::fixture();
        write_cache(
            path,
            &PricingCache {
                models: fixture.entries.into_iter().collect(),
                ..PricingCache::default()
            },
        );
    }

    /// Assemble the merged book: embedded snapshot, then the latest projected
    /// upstream table.
    fn assembled(cache: &PricingCache) -> Self {
        let mut entries = embedded::load();
        for (model, price) in &cache.models {
            entries.insert(model.clone(), *price);
        }
        Self {
            entries,
            fuzzy_cache: Arc::default(),
        }
    }

    /// Resolve the price for `model`: exact match, then a longest-key fuzzy scan.
    pub fn price(&self, model: &str) -> Option<Pricing> {
        let key = model.trim();
        if let Some(price) = self.entries.get(key) {
            return Some(*price);
        }
        // The fuzzy scan is linear over every entry, so memoize it per lookup
        // name. Read under a short lock, release it for the scan so the spend
        // walk's worker threads never serialize on the expensive path, then
        // record the result — a miss included — for the next call.
        {
            let cache = self
                .fuzzy_cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(cached) = cache.get(key) {
                return *cached;
            }
        }
        let result = self.fuzzy(key);
        self.fuzzy_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_owned(), result);
        result
    }

    /// Resolve only an exact stored model id. Capacity and locally estimated
    /// spend use this conservative path so a related model cannot lend another
    /// selector its limits or rates.
    pub fn exact_price(&self, model: &str) -> Option<Pricing> {
        self.entries.get(model.trim()).copied()
    }

    /// Longest stored key that is a boundary-prefix of the normalized lookup.
    ///
    /// `claude-sonnet-4-20250514-via-bedrock` resolves to its base model rather
    /// than a shorter prefix; a purely numeric version bump (`gpt-5` ↛ a `-9`
    /// suffix) is rejected so a new version is never silently priced as the old.
    fn fuzzy(&self, model: &str) -> Option<Pricing> {
        let want = normalize(model);
        let mut best: Option<(usize, Pricing)> = None;
        for (key, price) in &self.entries {
            let key_norm = normalize(key);
            if prefix_at_boundary(&want, &key_norm)
                && best.is_none_or(|(len, _)| key_norm.len() > len)
            {
                best = Some((key_norm.len(), *price));
            }
        }
        best.map(|(_, price)| price)
    }
}

/// Load the price book for a spending pass, refreshing the on-disk cache at most
/// once per TTL. A second gated trigger chases model names recorded by spend
/// parsers when the assembled book still cannot price them, backing off from
/// 30 minutes to the 24-hour cap while the same names persist. Best-effort: a
/// failed or skipped fetch falls back to the cache, then to the embedded
/// snapshot — the returned book is always usable.
///
/// `cache_path` is the producer's persistent shared `pricing-cache.json`.
pub fn load_for_spending(cache_path: &Path, unknown_models: &BTreeSet<String>) -> PriceBook {
    let mut cache = read_cache(cache_path);
    let mut book = PriceBook::assembled(&cache);
    let pending = unpriced_subset(&book, unknown_models);
    let now = unix_secs_now();
    let mut write = false;

    if pending.is_empty() {
        write |= clear_unknown_chase(&mut cache);
    }

    if should_refresh(&cache, now, source::offline(), &pending) {
        book = refresh_cache(
            &mut cache,
            now,
            pending,
            unknown_models,
            source::fetch_litellm,
            source::fetch_models_dev,
        );
        write = true;
    }
    if write {
        write_cache(cache_path, &cache);
    }
    book
}

type CachedBookMemo = Option<(PathBuf, Option<(u64, u64)>, Arc<PriceBook>)>;

static CACHED_BOOK_MEMO: LazyLock<Mutex<CachedBookMemo>> = LazyLock::new(|| Mutex::new(None));

/// Load the current read-only price book from the embedded snapshot and
/// persistent shared cache, without refreshing or writing. Spending fallbacks,
/// agent-card costs, and hook reconciliation share this path.
pub fn cached_book(cache_path: &Path) -> Arc<PriceBook> {
    cached_book_with_fingerprint(cache_path).0
}

/// Load the memoized price book with the `(mtime, length)` fingerprint used to
/// validate it. Consumers that persist derived pricing can use the fingerprint
/// to retry only after the underlying cache changes.
pub fn cached_book_with_fingerprint(cache_path: &Path) -> (Arc<PriceBook>, Option<String>) {
    let stamp = cache_stamp(cache_path);
    let fingerprint = stamp.map(|(mtime_secs, len)| format!("{mtime_secs}:{len}"));
    let mut memo = CACHED_BOOK_MEMO
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some((memo_path, memo_stamp, book)) = memo.as_ref()
        && memo_path == cache_path
        && *memo_stamp == stamp
    {
        return (Arc::clone(book), fingerprint);
    }

    let book = Arc::new(PriceBook::assembled(&read_cache(cache_path)));
    *memo = Some((cache_path.to_owned(), stamp, Arc::clone(&book)));
    (book, fingerprint)
}

fn cache_stamp(path: &Path) -> Option<(u64, u64)> {
    let metadata = fs::metadata(path).ok()?;
    let mtime_secs = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((mtime_secs, metadata.len()))
}

fn refresh_cache(
    cache: &mut PricingCache,
    now: u64,
    pending: BTreeSet<String>,
    unknown_models: &BTreeSet<String>,
    fetch_litellm: impl FnOnce() -> Option<String>,
    fetch_models_dev: impl FnOnce() -> Option<String>,
) -> PriceBook {
    cache.last_attempt_secs = now;
    let chasing = !pending.is_empty();
    if chasing {
        note_chase_attempt(cache, now, pending);
    }
    // Both documents or none: a LiteLLM-only table drops the authoritative
    // families and tiers models.dev alone carries. Fetching lazily keeps a
    // LiteLLM outage from also paying for the 3MB models.dev download.
    if let Some(litellm) = fetch_litellm()
        && let Some(models_dev) = fetch_models_dev()
        && let Ok((snapshot, _)) = source::project_sources(&litellm, Some(&models_dev))
        && let Ok(json) = serde_json::to_string(&snapshot)
    {
        let models = embedded::parse(&json);
        if !models.is_empty() {
            cache.models = models.into_iter().collect();
            cache.fetched_at_secs = now;
        }
    }
    let book = PriceBook::assembled(cache);
    if unpriced_subset(&book, unknown_models).is_empty() {
        clear_unknown_chase(cache);
    }
    book
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
const PRICING_CACHE_SCHEMA: u32 = 4;

/// On-disk pricing cache at persistent shared `pricing-cache.json`. Sorted maps
/// keep the file diff-stable.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PricingCache {
    #[serde(default)]
    schema: u32,
    #[serde(default)]
    fetched_at_secs: u64,
    #[serde(default)]
    last_attempt_secs: u64,
    #[serde(default)]
    models: BTreeMap<String, Pricing>,
    #[serde(default)]
    unknown_attempt_secs: u64,
    #[serde(default)]
    unknown_backoff_secs: u64,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    unknown_seen: BTreeSet<String>,
}

impl Default for PricingCache {
    fn default() -> Self {
        Self {
            schema: PRICING_CACHE_SCHEMA,
            fetched_at_secs: 0,
            last_attempt_secs: 0,
            models: BTreeMap::new(),
            unknown_attempt_secs: 0,
            unknown_backoff_secs: 0,
            unknown_seen: BTreeSet::new(),
        }
    }
}

pub(crate) fn tiered_cost(tokens: u64, base: f64, above: Option<f64>) -> f64 {
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

fn one() -> f64 {
    1.0
}

fn should_refresh(
    cache: &PricingCache,
    now: u64,
    offline: bool,
    pending: &BTreeSet<String>,
) -> bool {
    if offline {
        return false;
    }
    baseline_refresh_due(cache, now) || unknown_refresh_due(cache, now, pending)
}

fn baseline_refresh_due(cache: &PricingCache, now: u64) -> bool {
    now.saturating_sub(cache.fetched_at_secs) > REFRESH_TTL_SECS
        && now.saturating_sub(cache.last_attempt_secs) > RETRY_BACKOFF_SECS
}

fn unknown_refresh_due(cache: &PricingCache, now: u64, pending: &BTreeSet<String>) -> bool {
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

fn note_chase_attempt(cache: &mut PricingCache, now: u64, pending: BTreeSet<String>) {
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

fn chase_gate(cache: &PricingCache, pending: &BTreeSet<String>) -> (bool, u64) {
    let new_sighting = !pending.is_subset(&cache.unknown_seen);
    let gate = if new_sighting {
        UNKNOWN_REFRESH_TTL_SECS
    } else {
        cache.unknown_backoff_secs.max(UNKNOWN_REFRESH_TTL_SECS)
    };
    (new_sighting, gate)
}

fn clear_unknown_chase(cache: &mut PricingCache) -> bool {
    let changed = cache.unknown_attempt_secs != 0
        || cache.unknown_backoff_secs != 0
        || !cache.unknown_seen.is_empty();
    cache.unknown_attempt_secs = 0;
    cache.unknown_backoff_secs = 0;
    cache.unknown_seen.clear();
    changed
}

fn read_cache(path: &Path) -> PricingCache {
    let Ok(bytes) = fs::read(path) else {
        return PricingCache::default();
    };
    let cache = serde_json::from_slice::<PricingCache>(&bytes).unwrap_or_default();
    if cache.schema == PRICING_CACHE_SCHEMA {
        cache
    } else {
        PricingCache::default()
    }
}

/// Atomic write: temp file + rename, matching the store durability contract.
fn write_cache(path: &Path, cache: &PricingCache) {
    let mut cache = cache.clone();
    cache.schema = PRICING_CACHE_SCHEMA;
    let Ok(bytes) = serde_json::to_vec_pretty(&cache) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, &bytes).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
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
mod tests {
    use super::*;

    fn set(models: &[&str]) -> BTreeSet<String> {
        models.iter().map(|model| (*model).to_owned()).collect()
    }

    fn book() -> PriceBook {
        PriceBook::from_litellm_json(
            r#"{
                "gpt-5": {"input_cost_per_token": 1.25e-6, "output_cost_per_token": 1e-5},
                "claude-sonnet-4-20250514": {"input_cost_per_token": 3e-6, "output_cost_per_token": 1.5e-5},
                "claude-sonnet-4": {"input_cost_per_token": 9e-9, "output_cost_per_token": 9e-9}
            }"#,
        )
    }

    #[test]
    fn base_rates_own_cache_defaults_and_explicitness() {
        let defaulted = Pricing::from_base_rates(2.0, 4.0, None, None);
        assert_eq!(defaulted.cache_create, 2.5);
        assert_eq!(defaulted.cache_read, 0.2);
        assert!(!defaulted.cache_read_explicit);

        let explicit = Pricing::from_base_rates(2.0, 4.0, Some(3.0), Some(0.5));
        assert_eq!(explicit.cache_create, 3.0);
        assert_eq!(explicit.cache_read, 0.5);
        assert!(explicit.cache_read_explicit);
    }

    #[test]
    fn price_lookup_handles_exact_fuzzy_embedded_and_unknown_boundaries() {
        assert!((book().price("gpt-5").unwrap().input - 1.25e-6).abs() < 1e-18);

        // The dated key is longer than the bare key, so it wins the scan.
        let p = book()
            .price("claude-sonnet-4-20250514-via-bedrock")
            .unwrap();
        assert!((p.input - 3e-6).abs() < 1e-18);

        let b = PriceBook::from_litellm_json(
            r#"{"gpt-5-codex": {"input_cost_per_token": 2e-6, "output_cost_per_token": 4e-6}}"#,
        );
        // `gpt-5.codex@v1` normalizes to `gpt-5-codex-v1`, matching `gpt-5-codex`.
        assert!(b.price("gpt-5.codex@v1").is_some());

        // `gpt-5-9` must not be priced as `gpt-5`.
        assert!(book().price("gpt-5-9").is_none());
        assert!(book().price("totally-unknown-model").is_none());
    }

    #[test]
    fn fuzzy_memo_is_transparent_and_survives_a_clone() {
        let book = book();

        // Repeated fuzzy lookups (hit then cached) stay identical, and a cached
        // miss keeps reading as a miss rather than flipping to a stale hit.
        let first = book.price("claude-sonnet-4-20250514-via-bedrock").unwrap();
        let second = book.price("claude-sonnet-4-20250514-via-bedrock").unwrap();
        assert_eq!(first, second);
        assert!((second.input - 3e-6).abs() < 1e-18);
        assert!(book.price("totally-unknown-model").is_none());
        assert!(book.price("totally-unknown-model").is_none());

        // A clone shares entries, so a fuzzy lookup on it resolves the same way
        // whether or not the original primed the memo first.
        let clone = book.clone();
        assert_eq!(
            clone.price("claude-sonnet-4-20250514-via-bedrock").unwrap(),
            first
        );
        assert!(clone.price("gpt-5-9").is_none());
    }

    #[test]
    fn openai_long_context_pricing_switches_the_whole_request_above_272k() {
        let price = PriceBook::fixture().price("gpt-5.6-sol").unwrap();
        assert_eq!(price.long_context_threshold, Some(272_000));

        let short = price.cost_of(TokenSplit::new(100_000, 1_000).cached(0, 100));
        assert!((short - 0.53005).abs() < 1e-9, "short cost was {short}");

        let long = price.cost_of(TokenSplit::new(300_000, 1_000).cached(0, 100));
        assert!((long - 3.0451).abs() < 1e-9, "long cost was {long}");
    }

    #[test]
    fn session_cost_uses_base_rates_above_request_tier_boundaries() {
        let price = PriceBook::fixture().price("gpt-5.6-sol").unwrap();
        let cost = price.session_cost(500_000, 10_000, 20_000, 400_000);
        let expected = 500_000.0 * price.input
            + 10_000.0 * price.output
            + 20_000.0 * price.cache_create
            + 400_000.0 * price.cache_read;
        assert!((cost - expected).abs() < f64::EPSILON);
        let split = TokenSplit::new(500_000, 10_000).cached(20_000, 400_000);
        assert_ne!(cost, price.cost_of(split.fast(true)));
    }

    #[test]
    fn refresh_policy_covers_offline_empty_fresh_and_recent_failures() {
        let cache = PricingCache::default();
        assert!(!should_refresh(
            &cache,
            unix_secs_now(),
            true,
            &BTreeSet::new()
        ));

        let cache = PricingCache::default();
        assert!(should_refresh(
            &cache,
            unix_secs_now(),
            false,
            &BTreeSet::new()
        ));

        let now = unix_secs_now();
        let cache = PricingCache {
            fetched_at_secs: now,
            last_attempt_secs: now,
            ..Default::default()
        };
        assert!(!should_refresh(&cache, now, false, &BTreeSet::new()));

        // Data is stale (never fetched) but we just attempted: back off.
        let cache = PricingCache {
            fetched_at_secs: 0,
            last_attempt_secs: now,
            ..Default::default()
        };
        assert!(!should_refresh(&cache, now, false, &BTreeSet::new()));
    }

    #[test]
    fn unknown_chase_refresh_policy_handles_seen_sets_and_offline_mode() {
        let now = 10 * UNKNOWN_REFRESH_TTL_SECS;
        let cache = PricingCache {
            fetched_at_secs: now,
            ..Default::default()
        };
        assert!(should_refresh(&cache, now, false, &set(&["new-model"])));
        assert!(!should_refresh(&cache, now, true, &set(&["new-model"])));

        let cache = PricingCache {
            fetched_at_secs: now,
            last_attempt_secs: now,
            ..Default::default()
        };
        assert!(!should_refresh(&cache, now, false, &set(&["new-model"])));

        let cache = PricingCache {
            fetched_at_secs: now,
            unknown_attempt_secs: now - UNKNOWN_REFRESH_TTL_SECS - 1,
            unknown_backoff_secs: 2 * UNKNOWN_REFRESH_TTL_SECS,
            unknown_seen: set(&["new-model"]),
            ..Default::default()
        };

        assert!(!should_refresh(&cache, now, false, &set(&["new-model"])));
        assert!(should_refresh(
            &cache,
            now + UNKNOWN_REFRESH_TTL_SECS,
            false,
            &set(&["new-model"])
        ));

        let cache = PricingCache {
            fetched_at_secs: now,
            unknown_attempt_secs: now - UNKNOWN_REFRESH_TTL_SECS - 1,
            unknown_backoff_secs: 4 * UNKNOWN_REFRESH_TTL_SECS,
            unknown_seen: set(&["old-model"]),
            ..Default::default()
        };

        assert!(should_refresh(
            &cache,
            now,
            false,
            &set(&["old-model", "new-model"])
        ));
    }

    #[test]
    fn chase_attempts_double_until_the_cap_and_clear_when_healed() {
        let mut cache = PricingCache::default();

        note_chase_attempt(&mut cache, 1, set(&["new-model"]));
        assert_eq!(cache.unknown_backoff_secs, UNKNOWN_REFRESH_TTL_SECS);

        note_chase_attempt(&mut cache, 2, set(&["new-model"]));
        assert_eq!(cache.unknown_backoff_secs, 2 * UNKNOWN_REFRESH_TTL_SECS);

        cache.unknown_backoff_secs = UNKNOWN_BACKOFF_CAP_SECS;
        note_chase_attempt(&mut cache, 3, set(&["new-model"]));
        assert_eq!(cache.unknown_backoff_secs, UNKNOWN_BACKOFF_CAP_SECS);

        let mut cache = PricingCache {
            unknown_attempt_secs: 10,
            unknown_backoff_secs: UNKNOWN_REFRESH_TTL_SECS,
            unknown_seen: set(&["new-model"]),
            ..Default::default()
        };

        note_chase_attempt(&mut cache, 20, BTreeSet::new());

        assert_eq!(cache.unknown_attempt_secs, 0);
        assert_eq!(cache.unknown_backoff_secs, 0);
        assert!(cache.unknown_seen.is_empty());
    }

    #[test]
    fn runtime_refresh_projects_both_sources_and_keeps_the_last_good_table() {
        use std::cell::Cell;

        let now = REFRESH_TTL_SECS + RETRY_BACKOFF_SECS + 1;
        let litellm_fetches = Cell::new(0);
        let models_dev_fetches = Cell::new(0);
        let mut cache = PricingCache::default();
        let book = refresh_cache(
            &mut cache,
            now,
            BTreeSet::new(),
            &BTreeSet::new(),
            || {
                litellm_fetches.set(litellm_fetches.get() + 1);
                Some(include_str!("tests/fixtures/litellm.json").to_owned())
            },
            || {
                models_dev_fetches.set(models_dev_fetches.get() + 1);
                Some(include_str!("tests/fixtures/models-dev.json").to_owned())
            },
        );

        assert_eq!(litellm_fetches.get(), 1);
        assert_eq!(models_dev_fetches.get(), 1);
        let grok = book.price("grok-4.5").unwrap();
        assert_eq!(grok.long_context_threshold, Some(200_000));
        assert_eq!(grok.max_input_tokens, Some(500_000));

        let previous_models = cache.models.clone();
        let previous_fetched_at = cache.fetched_at_secs;
        let failed_at = now + RETRY_BACKOFF_SECS + 1;
        let book = refresh_cache(
            &mut cache,
            failed_at,
            BTreeSet::new(),
            &BTreeSet::new(),
            || Some(include_str!("tests/fixtures/litellm.json").to_owned()),
            || None,
        );

        assert_eq!(cache.models, previous_models);
        assert_eq!(cache.fetched_at_secs, previous_fetched_at);
        assert_eq!(cache.last_attempt_secs, failed_at);
        assert_eq!(
            book.price("grok-4.5").unwrap().long_context_threshold,
            Some(200_000)
        );

        // A LiteLLM outage already rules the attempt out, so the 3MB
        // models.dev download never starts.
        let skipped = Cell::new(0);
        refresh_cache(
            &mut cache,
            failed_at + RETRY_BACKOFF_SECS + 1,
            BTreeSet::new(),
            &BTreeSet::new(),
            || None,
            || {
                skipped.set(skipped.get() + 1);
                Some(include_str!("tests/fixtures/models-dev.json").to_owned())
            },
        );

        assert_eq!(skipped.get(), 0);
        assert_eq!(cache.models, previous_models);
    }

    #[test]
    fn unpriced_subset_filters_exact_and_fuzzy_priced_unknowns() {
        let b = PriceBook::from_litellm_json(
            r#"{"new-model": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6}}"#,
        );

        assert!(unpriced_subset(&b, &set(&["new-model"])).is_empty());
        assert!(unpriced_subset(&b, &set(&["new-model-via-provider"])).is_empty());
    }

    #[test]
    fn exact_lookup_does_not_borrow_a_boundary_prefix() {
        let book = PriceBook::from_litellm_json(
            r#"{"exact-model":{"input_cost_per_token":1e-6,"output_cost_per_token":2e-6,"max_input_tokens":123456}}"#,
        );

        assert_eq!(
            book.exact_price(" exact-model ").unwrap().max_input_tokens,
            Some(123_456)
        );
        assert!(book.price("exact-model-via-gateway").is_some());
        assert!(book.exact_price("exact-model-via-gateway").is_none());
    }

    #[test]
    fn assembly_overwrites_embedded_rows_with_the_projected_cache() {
        let cache = PricingCache {
            models: BTreeMap::from([
                (
                    "gpt-5".to_owned(),
                    Pricing {
                        input: 9e-6,
                        output: 9e-6,
                        ..Pricing::empty()
                    },
                ),
                (
                    "models-dev-only".to_owned(),
                    Pricing {
                        input: 7e-6,
                        output: 7e-6,
                        ..Pricing::empty()
                    },
                ),
            ]),
            ..Default::default()
        };

        let book = PriceBook::assembled(&cache);

        assert!((book.price("gpt-5").unwrap().input - 9e-6).abs() < 1e-18);
        assert!((book.price("models-dev-only").unwrap().input - 7e-6).abs() < 1e-18);
    }

    #[test]
    fn cached_book_reads_shared_cache_without_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pricing-cache.json");
        let model = "rimz-test-cached-model";
        let cache = PricingCache {
            models: BTreeMap::from([(
                model.to_owned(),
                Pricing {
                    input: 3e-6,
                    output: 15e-6,
                    cache_read: 3e-7,
                    cache_create: 3.75e-6,
                    max_input_tokens: Some(777_000),
                    ..Pricing::empty()
                },
            )]),
            ..Default::default()
        };
        write_cache(&path, &cache);

        let book = cached_book(&path);
        let price = book.price(model).expect("cached price");

        assert!((price.input - 3e-6).abs() < f64::EPSILON);
        assert!((price.output - 15e-6).abs() < f64::EPSILON);
        assert_eq!(price.max_input_tokens, Some(777_000));
    }

    #[test]
    fn cached_book_invalidates_when_shared_cache_stamp_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pricing-cache.json");
        let model = "rimz-test-changing-model";
        let mut cache = PricingCache {
            models: BTreeMap::from([(
                model.to_owned(),
                Pricing {
                    input: 3e-6,
                    output: 15e-6,
                    ..Pricing::empty()
                },
            )]),
            ..Default::default()
        };
        write_cache(&path, &cache);

        let (first, first_fingerprint) = cached_book_with_fingerprint(&path);
        assert!((first.price(model).unwrap().input - 3e-6).abs() < f64::EPSILON);

        cache.models.get_mut(model).unwrap().input = 30e-6;
        cache.models.insert(
            "rimz-test-length-bump".to_owned(),
            Pricing {
                input: 1e-6,
                output: 1e-6,
                ..Pricing::empty()
            },
        );
        write_cache(&path, &cache);

        let (second, second_fingerprint) = cached_book_with_fingerprint(&path);
        assert!((second.price(model).unwrap().input - 30e-6).abs() < f64::EPSILON);
        assert_ne!(first_fingerprint, second_fingerprint);
    }

    #[test]
    fn schema_three_pricing_cache_drops_the_split_source_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pricing-cache.json");
        std::fs::write(
            &path,
            r#"{"schema":3,"litellm":{"rimz-test-stale-model":{"input":999.0,"output":999.0}}}"#,
        )
        .unwrap();

        let cache = read_cache(&path);

        assert_eq!(cache.schema, PRICING_CACHE_SCHEMA);
        assert!(cache.models.is_empty());
    }
}
