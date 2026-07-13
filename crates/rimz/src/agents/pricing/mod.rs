//! Per-model token pricing — the table that turns token counts into dollars.
//!
//! Token-only providers — Claude and Codex — need this: their spend
//! ([`super::spending`]) can only be totalled by multiplying each turn's tokens
//! by a price, since current Claude transcripts and every Codex rollout log token
//! counts rather than a `costUSD`. Pi reports `costUSD` directly and never
//! consults the table.
//!
//! Ordered pricing passes feed one [`PriceBook`], with builtins acting as a
//! fallback below the live LiteLLM refresh:
//!
//! 1. **Embedded snapshot** ([`embedded`]) — the generated LiteLLM table
//!    `build.rs` compacts and gzips into release binaries. Fresh clones without
//!    the generated file embed an empty table.
//! 2. **Builtins** ([`builtins`]) — hardcoded fallback prices for models that
//!    must price before a refresh.
//! 3. **Remote refresh** ([`remote`]) — a weekly LiteLLM pull overwrites older
//!    fallback rows; the models.dev catalogue is fetched only during an
//!    unknown-model chase and fills models LiteLLM lacks.
//!
//! Lookups are pure and network-free: [`cached_book`] memoizes the merged
//! embedded, builtin, and on-disk cache data by file stamp, and
//! [`PriceBook::price`] resolves a model by exact match then a boundary-aware
//! fuzzy scan. The only network is the gated refresh in [`load_for_spending`]:
//! a weekly refresh, plus an escalating unknown-model chase when a transcript
//! names a priceable model the current book cannot resolve.

mod builtins;
mod embedded;
mod overrides;
mod remote;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::spending::is_priceable_model_name;

pub(crate) const CACHE_CREATE_1H_INPUT_MULTIPLIER: f64 = 2.0;
const DEFAULT_LONG_CONTEXT_THRESHOLD_TOKENS: u64 = 200_000;
const OPENAI_LONG_CONTEXT_THRESHOLD_TOKENS: u64 = 272_000;

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
        }
    }

    pub(crate) fn cost(
        self,
        input: u64,
        output: u64,
        cache_5m: u64,
        cache_1h: u64,
        cache_read: u64,
        fast: bool,
    ) -> f64 {
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
}

impl PriceBook {
    /// The no-network book: embedded snapshot overlaid with builtins.
    pub fn embedded() -> Self {
        let mut entries = embedded::load();
        builtins::put_builtins(&mut entries);
        apply_builtin_long_context_rates(&mut entries);
        Self { entries }
    }

    /// Build a book from an arbitrary LiteLLM-shaped document (tests, tooling).
    /// Builtins still win here, matching the embedded no-cache path.
    pub fn from_litellm_json(json: &str) -> Self {
        let mut entries = embedded::parse(json);
        builtins::put_builtins(&mut entries);
        apply_builtin_long_context_rates(&mut entries);
        Self { entries }
    }

    /// Assemble the merged book: embedded snapshot, then builtins as ccusage's
    /// fallback layer, then the LiteLLM refresh (overwriting), then models.dev
    /// for models both sources lack.
    fn assembled(cache: &PricingCache) -> Self {
        let mut entries = embedded::load();
        builtins::put_builtins(&mut entries);
        for (model, price) in &cache.litellm {
            entries.insert(model.clone(), *price);
        }
        for (model, price) in &cache.models_dev {
            entries.entry(model.clone()).or_insert(*price);
        }
        apply_builtin_long_context_rates(&mut entries);
        Self { entries }
    }

    /// Resolve the price for `model`: exact match, then a longest-key fuzzy scan.
    pub fn price(&self, model: &str) -> Option<Pricing> {
        if let Some(price) = self.entries.get(model.trim()) {
            return Some(*price);
        }
        self.fuzzy(model)
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

    if should_refresh(&cache, now, remote::offline(), &pending) {
        book = refresh_cache(
            &mut cache,
            now,
            pending,
            unknown_models,
            remote::fetch_litellm,
            remote::fetch_models_dev,
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

/// Load the current read-only price book from the embedded snapshot, builtins,
/// and persistent shared cache, without refreshing or writing. Spending
/// fallbacks, agent-card costs, and hook reconciliation share this path.
pub fn cached_book(cache_path: &Path) -> Arc<PriceBook> {
    let stamp = cache_stamp(cache_path);
    let mut memo = CACHED_BOOK_MEMO
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some((memo_path, memo_stamp, book)) = memo.as_ref()
        && memo_path == cache_path
        && *memo_stamp == stamp
    {
        return Arc::clone(book);
    }

    let book = Arc::new(PriceBook::assembled(&read_cache(cache_path)));
    *memo = Some((cache_path.to_owned(), stamp, Arc::clone(&book)));
    book
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
    if let Some(json) = fetch_litellm() {
        let table = embedded::parse(&json);
        if !table.is_empty() {
            cache.litellm = table.into_iter().collect();
            cache.fetched_at_secs = now;
        }
    }
    if chasing {
        let book = PriceBook::assembled(cache);
        if !unpriced_subset(&book, unknown_models).is_empty()
            && let Some(json) = fetch_models_dev()
        {
            let table = remote::parse_models_dev(&json);
            if !table.is_empty() {
                cache.models_dev = table;
            }
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
const PRICING_CACHE_SCHEMA: u32 = 2;

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
    litellm: BTreeMap<String, Pricing>,
    #[serde(default)]
    models_dev: BTreeMap<String, Pricing>,
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
            litellm: BTreeMap::new(),
            models_dev: BTreeMap::new(),
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

#[derive(Clone, Copy)]
struct LongContextRates {
    input: f64,
    output: f64,
    cache_create: Option<f64>,
    cache_read: Option<f64>,
}

fn apply_builtin_long_context_rates(entries: &mut HashMap<String, Pricing>) {
    for (model, pricing) in entries {
        if pricing.input_above_200k.is_some()
            || pricing.output_above_200k.is_some()
            || pricing.cache_create_above_200k.is_some()
            || pricing.cache_read_above_200k.is_some()
        {
            continue;
        }
        let Some(rates) = builtin_long_context_rates(model_without_date_suffix(model)) else {
            continue;
        };
        pricing.input_above_200k = Some(rates.input);
        pricing.output_above_200k = Some(rates.output);
        pricing.cache_create_above_200k = rates.cache_create;
        pricing.cache_read_above_200k = rates.cache_read;
        pricing.long_context_threshold = Some(OPENAI_LONG_CONTEXT_THRESHOLD_TOKENS);
    }
}

fn builtin_long_context_rates(model: &str) -> Option<LongContextRates> {
    let rates = |input, output, cache_create, cache_read| LongContextRates {
        input,
        output,
        cache_create,
        cache_read,
    };
    match model {
        "gpt-5.6" | "gpt-5.6-sol" => Some(rates(10e-6, 45e-6, Some(12.5e-6), Some(1e-6))),
        "gpt-5.6-terra" => Some(rates(5e-6, 22.5e-6, Some(6.25e-6), Some(0.5e-6))),
        "gpt-5.6-luna" => Some(rates(2e-6, 9e-6, Some(2.5e-6), Some(0.2e-6))),
        "gpt-5.5" => Some(rates(10e-6, 45e-6, Some(10e-6), Some(1e-6))),
        "gpt-5.4" => Some(rates(5e-6, 22.5e-6, Some(5e-6), Some(0.5e-6))),
        "gpt-5.5-pro" | "gpt-5.4-pro" => Some(rates(60e-6, 270e-6, None, None)),
        _ => None,
    }
}

fn model_without_date_suffix(model: &str) -> &str {
    let bytes = model.as_bytes();
    if bytes.len() >= 11 {
        let start = bytes.len() - 11;
        let suffix = &bytes[start..];
        if suffix[0] == b'-'
            && suffix[1..5].iter().all(u8::is_ascii_digit)
            && suffix[5] == b'-'
            && suffix[6..8].iter().all(u8::is_ascii_digit)
            && suffix[8] == b'-'
            && suffix[9..11].iter().all(u8::is_ascii_digit)
        {
            return &model[..start];
        }
    }
    if bytes.len() >= 9 {
        let start = bytes.len() - 9;
        if bytes[start] == b'-' && bytes[start + 1..].iter().all(u8::is_ascii_digit) {
            return &model[..start];
        }
    }
    model
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
        assert!(PriceBook::embedded().price("gpt-5").is_some());
        assert!(PriceBook::embedded().price("gpt-5.5-codex").is_some());
        assert!(PriceBook::embedded().price("gpt-5.6").is_some());
        assert!(PriceBook::embedded().price("gpt-5.6-sol").is_some());
    }

    #[test]
    fn openai_long_context_pricing_switches_the_whole_request_above_272k() {
        let price = PriceBook::embedded().price("gpt-5.6-sol").unwrap();
        assert_eq!(
            price.long_context_threshold,
            Some(OPENAI_LONG_CONTEXT_THRESHOLD_TOKENS)
        );

        let short = price.cost(100_000, 1_000, 0, 0, 100, false);
        assert!((short - 0.53005).abs() < 1e-9, "short cost was {short}");

        let long = price.cost(300_000, 1_000, 0, 0, 100, false);
        assert!((long - 3.0451).abs() < 1e-9, "long cost was {long}");
    }

    #[test]
    fn openai_long_context_overlay_covers_date_pins_and_defers_to_upstream_tiers() {
        let mut entries = HashMap::from([
            (
                "gpt-5.5-2026-04-23".to_owned(),
                Pricing {
                    input: 6e-6,
                    output: 31e-6,
                    ..Pricing::empty()
                },
            ),
            (
                "gpt-5.4".to_owned(),
                Pricing {
                    input: 3e-6,
                    output: 18e-6,
                    input_above_200k: Some(12e-6),
                    ..Pricing::empty()
                },
            ),
        ]);

        apply_builtin_long_context_rates(&mut entries);

        let dated = entries["gpt-5.5-2026-04-23"];
        assert_eq!(dated.input_above_200k, Some(10e-6));
        assert_eq!(dated.long_context_threshold, Some(272_000));
        let upstream = entries["gpt-5.4"];
        assert_eq!(upstream.input_above_200k, Some(12e-6));
        assert_eq!(upstream.long_context_threshold, None);
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
    fn refresh_fetches_models_dev_only_for_unknowns_litellm_still_lacks() {
        use std::cell::Cell;

        fn litellm_json(model: &str) -> String {
            format!(
                r#"{{
                    "{model}": {{
                        "input_cost_per_token": 1e-6,
                        "output_cost_per_token": 2e-6
                    }}
                }}"#
            )
        }

        fn models_dev_json(model: &str) -> String {
            format!(
                r#"{{
                    "openai": {{
                        "models": {{
                            "{model}": {{
                                "cost": {{
                                    "input": 1.0,
                                    "output": 2.0
                                }}
                            }}
                        }}
                    }}
                }}"#
            )
        }

        let now = REFRESH_TTL_SECS + RETRY_BACKOFF_SECS + 1;

        let litellm_fetches = Cell::new(0);
        let models_dev_fetches = Cell::new(0);
        let mut cache = PricingCache::default();
        refresh_cache(
            &mut cache,
            now,
            BTreeSet::new(),
            &BTreeSet::new(),
            || {
                litellm_fetches.set(litellm_fetches.get() + 1);
                Some(litellm_json("rimz-test-baseline-model"))
            },
            || {
                models_dev_fetches.set(models_dev_fetches.get() + 1);
                Some(models_dev_json("rimz-test-baseline-model"))
            },
        );

        assert_eq!(litellm_fetches.get(), 1);
        assert_eq!(models_dev_fetches.get(), 0);

        let litellm_fetches = Cell::new(0);
        let models_dev_fetches = Cell::new(0);
        let mut cache = PricingCache::default();
        let unknowns = set(&["rimz-test-litellm-chase-model"]);
        let book = refresh_cache(
            &mut cache,
            now,
            unknowns.clone(),
            &unknowns,
            || {
                litellm_fetches.set(litellm_fetches.get() + 1);
                Some(litellm_json("rimz-test-litellm-chase-model"))
            },
            || {
                models_dev_fetches.set(models_dev_fetches.get() + 1);
                Some(models_dev_json("rimz-test-litellm-chase-model"))
            },
        );

        assert_eq!(litellm_fetches.get(), 1);
        assert_eq!(models_dev_fetches.get(), 0);
        assert!(book.price("rimz-test-litellm-chase-model").is_some());
        assert!(cache.unknown_seen.is_empty());

        let litellm_fetches = Cell::new(0);
        let models_dev_fetches = Cell::new(0);
        let mut cache = PricingCache::default();
        let unknowns = set(&["rimz-test-models-dev-chase-model"]);
        let book = refresh_cache(
            &mut cache,
            now,
            unknowns.clone(),
            &unknowns,
            || {
                litellm_fetches.set(litellm_fetches.get() + 1);
                Some(litellm_json("rimz-test-other-model"))
            },
            || {
                models_dev_fetches.set(models_dev_fetches.get() + 1);
                Some(models_dev_json("rimz-test-models-dev-chase-model"))
            },
        );

        assert_eq!(litellm_fetches.get(), 1);
        assert_eq!(models_dev_fetches.get(), 1);
        assert!(book.price("rimz-test-models-dev-chase-model").is_some());
        assert!(cache.unknown_seen.is_empty());
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
    fn assembly_uses_builtins_as_fallback_under_litellm_and_models_dev_fill() {
        let cache = PricingCache {
            litellm: BTreeMap::from([(
                "gpt-5".to_owned(),
                Pricing {
                    input: 9e-6,
                    output: 9e-6,
                    ..Pricing::empty()
                },
            )]),
            models_dev: BTreeMap::from([
                (
                    "gpt-5".to_owned(),
                    Pricing {
                        input: 8e-6,
                        output: 8e-6,
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
        assert!(book.price("claude-opus-4-8").is_some());
    }

    #[test]
    fn cached_book_reads_shared_cache_without_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pricing-cache.json");
        let model = "rimz-test-cached-model";
        let cache = PricingCache {
            models_dev: BTreeMap::from([(
                model.to_owned(),
                Pricing {
                    input: 3e-6,
                    output: 15e-6,
                    cache_read: 3e-7,
                    cache_create: 3.75e-6,
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
    }

    #[test]
    fn cached_book_invalidates_when_shared_cache_stamp_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pricing-cache.json");
        let model = "rimz-test-changing-model";
        let mut cache = PricingCache {
            litellm: BTreeMap::from([(
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

        let first = cached_book(&path);
        assert!((first.price(model).unwrap().input - 3e-6).abs() < f64::EPSILON);

        cache.litellm.get_mut(model).unwrap().input = 30e-6;
        cache.litellm.insert(
            "rimz-test-length-bump".to_owned(),
            Pricing {
                input: 1e-6,
                output: 1e-6,
                ..Pricing::empty()
            },
        );
        write_cache(&path, &cache);

        let second = cached_book(&path);
        assert!((second.price(model).unwrap().input - 30e-6).abs() < f64::EPSILON);
    }

    #[test]
    fn stale_pricing_cache_schema_drops_cached_tables() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pricing-cache.json");
        std::fs::write(
            &path,
            r#"{"schema":1,"litellm":{"rimz-test-stale-model":{"input":999.0,"output":999.0}}}"#,
        )
        .unwrap();

        let cache = read_cache(&path);

        assert_eq!(cache.schema, PRICING_CACHE_SCHEMA);
        assert!(cache.litellm.is_empty());
        assert!(cache.models_dev.is_empty());
    }
}
