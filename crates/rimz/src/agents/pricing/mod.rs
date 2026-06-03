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
//! 1. **Embedded snapshot** ([`embedded`]) — the LiteLLM table `build.rs` compacts
//!    into the binary, available the instant the process starts.
//! 2. **Remote refresh** ([`remote`]) — a fresh LiteLLM pull plus the models.dev
//!    catalogue (filling models the snapshot lacks), cached on disk with a TTL so
//!    the one-shot `rimz sidebar snapshot` process fetches at most once per day.
//! 3. **Builtins** ([`builtins`]) — hardcoded prices applied last, so the team's
//!    values always win over a stale or missing remote entry.
//!
//! Lookups are pure and network-free: the merged book is assembled once per
//! process from the embedded data and the on-disk cache, and
//! [`PriceBook::price`] resolves a model by exact match then a boundary-aware
//! fuzzy scan. The only network is the gated refresh in [`load_for_spending`].

mod builtins;
mod embedded;
mod remote;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

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
/// once per TTL. Best-effort: a failed or skipped fetch falls back to the cache,
/// then to the embedded snapshot — the returned book is always usable.
///
/// `cache_path` is the producer's `{runtime_root}/pricing-cache.json`.
pub fn load_for_spending(cache_path: &Path) -> PriceBook {
    let mut cache = read_cache(cache_path);
    let now = unix_secs_now();
    if should_refresh(&cache, now, remote::offline()) {
        cache.last_attempt_secs = now;
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
        write_cache(cache_path, &cache);
    }
    PriceBook::assembled(&cache)
}

// ── Disk cache ──────────────────────────────────────────────────────────────

/// Refetch once a day; on failure, back off an hour before retrying so a
/// persistent network outage never fetches on every snapshot.
const REFRESH_TTL_SECS: u64 = 24 * 60 * 60;
const RETRY_BACKOFF_SECS: u64 = 60 * 60;

/// On-disk pricing cache at `{runtime_root}/pricing-cache.json`. Sorted maps keep
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
}

fn should_refresh(cache: &PricingCache, now: u64, offline: bool) -> bool {
    if offline {
        return false;
    }
    now.saturating_sub(cache.fetched_at_secs) > REFRESH_TTL_SECS
        && now.saturating_sub(cache.last_attempt_secs) > RETRY_BACKOFF_SECS
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
    fn exact_match_wins() {
        assert!((book().price("gpt-5").unwrap().input - 1.25e-6).abs() < 1e-18);
    }

    #[test]
    fn fuzzy_strips_provider_suffix_longest_key_wins() {
        // The dated key is longer than the bare key, so it wins the scan.
        let p = book()
            .price("claude-sonnet-4-20250514-via-bedrock")
            .unwrap();
        assert!((p.input - 3e-6).abs() < 1e-18);
    }

    #[test]
    fn fuzzy_normalizes_dot_and_at() {
        let b = PriceBook::from_litellm_json(
            r#"{"gpt-5-codex": {"input_cost_per_token": 2e-6, "output_cost_per_token": 4e-6}}"#,
        );
        // `gpt-5.codex@v1` normalizes to `gpt-5-codex-v1`, matching `gpt-5-codex`.
        assert!(b.price("gpt-5.codex@v1").is_some());
    }

    #[test]
    fn numeric_version_bump_is_not_collapsed() {
        // `gpt-5-9` must not be priced as `gpt-5`.
        assert!(book().price("gpt-5-9").is_none());
    }

    #[test]
    fn unknown_model_is_none() {
        assert!(book().price("totally-unknown-model").is_none());
    }

    #[test]
    fn embedded_book_prices_the_fallback_model() {
        assert!(PriceBook::embedded().price("gpt-5").is_some());
    }

    #[test]
    fn offline_never_refreshes() {
        let cache = PricingCache::default();
        assert!(!should_refresh(&cache, unix_secs_now(), true));
    }

    #[test]
    fn empty_cache_refreshes_when_online() {
        let cache = PricingCache::default();
        assert!(should_refresh(&cache, unix_secs_now(), false));
    }

    #[test]
    fn fresh_cache_does_not_refresh() {
        let now = unix_secs_now();
        let cache = PricingCache {
            fetched_at_secs: now,
            last_attempt_secs: now,
            ..Default::default()
        };
        assert!(!should_refresh(&cache, now, false));
    }

    #[test]
    fn recent_failed_attempt_backs_off() {
        let now = unix_secs_now();
        // Data is stale (never fetched) but we just attempted: back off.
        let cache = PricingCache {
            fetched_at_secs: 0,
            last_attempt_secs: now,
            ..Default::default()
        };
        assert!(!should_refresh(&cache, now, false));
    }
}
