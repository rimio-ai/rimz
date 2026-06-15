//! Per-model token pricing — the table that turns token counts into dollars.
//!
//! Token-only providers — Claude and Codex — need this: their spend
//! ([`super::spending`]) can only be totalled by multiplying each turn's tokens
//! by a price, since current Claude transcripts and every Codex rollout log token
//! counts rather than a `costUSD`. Pi reports `costUSD` directly and never
//! consults the table.
//!
//! Three layers feed one [`PriceBook`], the later ones winning:
//!
//! 1. **Embedded snapshot** ([`embedded`]) — the generated LiteLLM table
//!    `build.rs` compacts and gzips into release binaries. Fresh clones without
//!    the generated file embed an empty table.
//! 2. **Remote refresh** ([`remote`]) — a fresh LiteLLM pull plus the models.dev
//!    catalogue (filling models the snapshot lacks), cached on disk with a TTL so
//!    the one-shot `rimz sidebar snapshot` process fetches at most once per day.
//! 3. **Builtins** ([`builtins`]) — hardcoded prices applied last, so the team's
//!    values always win over a stale or missing remote entry.
//!
//! Lookups are pure and network-free: the merged book is assembled once per
//! process from the embedded data and the on-disk cache, and
//! [`PriceBook::price`] resolves a model by exact match then a boundary-aware
//! fuzzy scan. The only network is the gated refresh in [`load_for_spending`]:
//! a daily refresh, plus an escalating unknown-model chase when a transcript
//! names a priceable model the current book cannot resolve.

mod builtins;
mod embedded;
mod remote;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::spending::is_priceable_model_name;

/// Per-token costs in USD for one model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    /// Cost per (uncached) input token.
    pub input: f64,
    /// Cost per output token (output already includes reasoning tokens).
    pub output: f64,
    /// Cost per cache-read (prompt-cache-hit) input token.
    #[serde(default)]
    pub cache_read: f64,
    /// Cost per cache-creation input token (unused by OpenAI; kept for parity).
    #[serde(default)]
    pub cache_create: f64,
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
        Self { entries }
    }

    /// Build a book from an arbitrary LiteLLM-shaped document (tests, tooling).
    /// Builtins still win, matching the production assembly order.
    pub fn from_litellm_json(json: &str) -> Self {
        let mut entries = embedded::parse(json);
        builtins::put_builtins(&mut entries);
        Self { entries }
    }

    /// Assemble the merged book: embedded snapshot, then models.dev for models it
    /// lacks, then the LiteLLM refresh (overwriting), then builtins (winning).
    fn assembled(cache: &PricingCache) -> Self {
        let mut entries = embedded::load();
        for (model, price) in &cache.models_dev {
            entries.entry(model.clone()).or_insert(*price);
        }
        for (model, price) in &cache.litellm {
            entries.insert(model.clone(), *price);
        }
        builtins::put_builtins(&mut entries);
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
/// 30 minutes to the daily cap while the same names persist. Best-effort: a
/// failed or skipped fetch falls back to the cache, then to the embedded
/// snapshot — the returned book is always usable.
///
/// `cache_path` is the producer's shared runtime `pricing-cache.json`.
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
        cache.last_attempt_secs = now;
        if !pending.is_empty() {
            note_chase_attempt(&mut cache, now, pending);
        }
        if let Some(json) = remote::fetch_litellm() {
            let table = embedded::parse(&json);
            if !table.is_empty() {
                cache.litellm = table.into_iter().collect();
                cache.fetched_at_secs = now;
            }
        }
        if let Some(json) = remote::fetch_models_dev() {
            let table = remote::parse_models_dev(&json);
            if !table.is_empty() {
                cache.models_dev = table;
            }
        }
        book = PriceBook::assembled(&cache);
        if unpriced_subset(&book, unknown_models).is_empty() {
            clear_unknown_chase(&mut cache);
        }
        write = true;
    }
    if write {
        write_cache(cache_path, &cache);
    }
    book
}

/// Load the spending price book from the embedded snapshot plus the shared
/// runtime pricing cache, without refreshing or writing. Used by local fallback
/// spending reads that could not win the shared spending election but still need
/// the same cached prices the producer normally uses.
pub fn load_cached_for_spending(cache_path: &Path) -> PriceBook {
    PriceBook::assembled(&read_cache(cache_path))
}

// ── Disk cache ──────────────────────────────────────────────────────────────

/// Refetch once a day; on failure, back off an hour before retrying so a
/// persistent network outage never fetches on every snapshot.
const REFRESH_TTL_SECS: u64 = 24 * 60 * 60;
const RETRY_BACKOFF_SECS: u64 = 60 * 60;
/// Chase a newly observed unpriced model after 30 minutes, then double up to the
/// daily refresh cap while the same unknown set persists.
const UNKNOWN_REFRESH_TTL_SECS: u64 = 30 * 60;

/// On-disk pricing cache at the shared runtime `pricing-cache.json`. Sorted maps keep
/// the file diff-stable.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PricingCache {
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

fn should_refresh(
    cache: &PricingCache,
    now: u64,
    offline: bool,
    pending: &BTreeSet<String>,
) -> bool {
    if offline {
        return false;
    }
    daily_refresh_due(cache, now) || unknown_refresh_due(cache, now, pending)
}

fn daily_refresh_due(cache: &PricingCache, now: u64) -> bool {
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
        gate.saturating_mul(2).min(REFRESH_TTL_SECS)
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
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Atomic write: temp file + rename, matching the ledger durability contract.
fn write_cache(path: &Path, cache: &PricingCache) {
    let Ok(bytes) = serde_json::to_vec_pretty(cache) else {
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

        cache.unknown_backoff_secs = REFRESH_TTL_SECS;
        note_chase_attempt(&mut cache, 3, set(&["new-model"]));
        assert_eq!(cache.unknown_backoff_secs, REFRESH_TTL_SECS);

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
    fn unpriced_subset_filters_exact_and_fuzzy_priced_unknowns() {
        let b = PriceBook::from_litellm_json(
            r#"{"new-model": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6}}"#,
        );

        assert!(unpriced_subset(&b, &set(&["new-model"])).is_empty());
        assert!(unpriced_subset(&b, &set(&["new-model-via-provider"])).is_empty());
    }

    #[test]
    fn cached_spending_loader_reads_shared_cache_without_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pricing-cache.json");
        let cache = PricingCache {
            models_dev: BTreeMap::from([(
                "claude-opus-4-8".to_owned(),
                Pricing {
                    input: 3e-6,
                    output: 15e-6,
                    cache_read: 3e-7,
                    cache_create: 3.75e-6,
                },
            )]),
            ..Default::default()
        };
        write_cache(&path, &cache);

        let book = load_cached_for_spending(&path);
        let price = book.price("claude-opus-4-8").expect("cached price");

        assert!((price.input - 3e-6).abs() < f64::EPSILON);
        assert!((price.output - 15e-6).abs() < f64::EPSILON);
    }
}
