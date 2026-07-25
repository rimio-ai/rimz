//! Declared gap-fillers for rates the pricing sources leave unpublished.
//!
//! Both tables state a *ratio*, never a price, so upstream stays the only
//! source of absolute numbers and a rate a provider revises still lands. A row
//! applies only where the source document is silent about that field, so an
//! upstream that starts publishing the value silently retires the entry.
//!
//! - `fast-multiplier-overrides.json` fills the priority-turn multiplier that
//!   LiteLLM's `provider_specific_entry.fast` covers for only some models.
//! - `cache-rate-ratios.json` fills cache rates against the model's uncached
//!   input rate. Neither source publishes a cache-write rate for GPT-5 through
//!   GPT-5.5, where OpenAI bills a cache write as plain input (GPT-5.6 onward
//!   does carry the 1.25× premium upstream, so no entry covers it); neither
//!   publishes a cache-read rate for the Qwen 3 coder models, where Alibaba
//!   discounts a cached token to a fifth of input. Without these the shared
//!   ccusage defaults — Anthropic's 1.25× write and 0.1× read — apply, which
//!   over-charges OpenAI cache writes by 25% and halves Qwen cached input.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

const FAST_MULTIPLIER_OVERRIDES_JSON: &str = include_str!("fast-multiplier-overrides.json");
const CACHE_RATE_RATIOS_JSON: &str = include_str!("cache-rate-ratios.json");

/// Ratios against a model's uncached input rate.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub(super) struct CacheRatios {
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct Overrides<T> {
    #[serde(default)]
    exact: HashMap<String, T>,
    #[serde(default)]
    normalized_prefix: HashMap<String, T>,
}

pub(super) fn multiplier_for(model: &str) -> Option<f64> {
    static OVERRIDES: OnceLock<Overrides<f64>> = OnceLock::new();
    OVERRIDES
        .get_or_init(|| {
            parse(
                FAST_MULTIPLIER_OVERRIDES_JSON,
                "fast-multiplier-overrides.json",
            )
        })
        .lookup(model)
}

pub(super) fn cache_ratios_for(model: &str) -> CacheRatios {
    static RATIOS: OnceLock<Overrides<CacheRatios>> = OnceLock::new();
    RATIOS
        .get_or_init(|| parse(CACHE_RATE_RATIOS_JSON, "cache-rate-ratios.json"))
        .lookup(model)
        .unwrap_or_default()
}

fn parse<T: Default + for<'de> Deserialize<'de>>(json: &str, name: &str) -> Overrides<T> {
    serde_json::from_str(json).unwrap_or_else(|err| panic!("parse embedded {name}: {err}"))
}

impl<T: Copy> Overrides<T> {
    fn lookup(&self, model: &str) -> Option<T> {
        if let Some(value) = self.exact.get(model) {
            return Some(*value);
        }
        let normalized = model.replace(['.', '@'], "-");
        normalized.split(['/', ':']).find_map(|part| {
            self.normalized_prefix
                .iter()
                .find_map(|(base, value)| matches_model_suffix(part, base).then_some(*value))
        })
    }
}

fn matches_model_suffix(part: &str, base: &str) -> bool {
    let Some(index) = part.rfind(base) else {
        return false;
    };
    let suffix = &part[index..];
    suffix == base || suffix.as_bytes().get(base.len()) == Some(&b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiplier_matches_exact_and_normalized_suffixes() {
        assert_eq!(multiplier_for("gpt-5.5"), Some(2.5));
        assert_eq!(
            multiplier_for("openrouter/anthropic/claude-opus-4.7"),
            Some(6.0)
        );
        assert_eq!(multiplier_for("claude-opus-4-70"), None);
    }

    #[test]
    fn cache_ratios_cover_the_families_upstream_leaves_silent() {
        assert_eq!(cache_ratios_for("gpt-5").cache_write, Some(1.0));
        assert_eq!(cache_ratios_for("gpt-5.4-mini").cache_write, Some(1.0));
        assert_eq!(cache_ratios_for("gpt-5").cache_read, None);
        assert_eq!(cache_ratios_for("qwen3-coder-plus").cache_read, Some(0.2));
        assert_eq!(cache_ratios_for("qwen3-coder-flash").cache_read, Some(0.2));
        assert_eq!(cache_ratios_for("qwen3-coder-plus").cache_write, None);

        // Open-weight coder builds and unrelated families keep the defaults.
        assert_eq!(
            cache_ratios_for("qwen3-coder-30b-a3b-instruct").cache_read,
            None
        );
        assert_eq!(cache_ratios_for("claude-sonnet-4-6").cache_write, None);
    }
}
