//! Tier 3a: hardcoded prices for the models Rimz must get right.
//!
//! Applied **last**, after the embedded snapshot and any remote refresh, so the
//! team's values always win — a stale or missing remote entry never mis-prices
//! a model we ship support for. The set is deliberately the OpenAI / Codex
//! family: Codex needs a guaranteed fallback model; Claude resolves through the
//! generated or runtime LiteLLM table when available, and Pi reports `costUSD`
//! directly.
//!
//! `gpt-5` is mandatory: it is the Codex parser's fallback model
//! (`codex::spend`), so a Codex event with no resolvable model still prices.
//!
//! Values are USD per token, taken from the upstream LiteLLM table; refresh them
//! alongside `cargo xtask pricing-refresh` when OpenAI changes prices.

use std::collections::HashMap;

use super::Pricing;

/// Overlay the guaranteed model prices, overwriting any embedded/remote entry.
pub(super) fn put_builtins(entries: &mut HashMap<String, Pricing>) {
    // gpt-5 tier: $1.25 / $10 per 1M, cache-read $0.125 per 1M.
    let gpt_5 = Pricing {
        input: 1.25e-6,
        output: 1.0e-5,
        cache_read: 1.25e-7,
        cache_create: 0.0,
    };
    for key in [
        "gpt-5",
        "gpt-5-codex",
        "gpt-5.1",
        "gpt-5.1-codex",
        "gpt-5.1-codex-max",
    ] {
        entries.insert(key.to_owned(), gpt_5);
    }

    // 5.2 / 5.3 codex tier: $1.75 / $14 per 1M, cache-read $0.175 per 1M.
    let codex_5_2 = Pricing {
        input: 1.75e-6,
        output: 1.4e-5,
        cache_read: 1.75e-7,
        cache_create: 0.0,
    };
    for key in ["gpt-5.2-codex", "gpt-5.3-codex"] {
        entries.insert(key.to_owned(), codex_5_2);
    }

    // mini tier: $0.25 / $2 per 1M, cache-read $0.025 per 1M.
    let mini = Pricing {
        input: 2.5e-7,
        output: 2.0e-6,
        cache_read: 2.5e-8,
        cache_create: 0.0,
    };
    for key in ["gpt-5-mini", "gpt-5.1-codex-mini"] {
        entries.insert(key.to_owned(), mini);
    }

    // nano tier: $0.05 / $0.40 per 1M, cache-read $0.005 per 1M.
    entries.insert(
        "gpt-5-nano".to_owned(),
        Pricing {
            input: 5.0e-8,
            output: 4.0e-7,
            cache_read: 5.0e-9,
            cache_create: 0.0,
        },
    );

    // reasoning models.
    entries.insert(
        "o3".to_owned(),
        Pricing {
            input: 2.0e-6,
            output: 8.0e-6,
            cache_read: 5.0e-7,
            cache_create: 0.0,
        },
    );
    entries.insert(
        "o4-mini".to_owned(),
        Pricing {
            input: 1.1e-6,
            output: 4.4e-6,
            cache_read: 2.75e-7,
            cache_create: 0.0,
        },
    );
    entries.insert(
        "codex-mini-latest".to_owned(),
        Pricing {
            input: 1.5e-6,
            output: 6.0e-6,
            cache_read: 3.75e-7,
            cache_create: 0.0,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_model_is_present() {
        let mut entries = HashMap::new();
        put_builtins(&mut entries);
        assert!(entries.contains_key("gpt-5"), "gpt-5 fallback must exist");
    }

    #[test]
    fn builtins_overwrite_existing() {
        let mut entries = HashMap::new();
        entries.insert(
            "gpt-5".to_owned(),
            Pricing {
                input: 999.0,
                output: 999.0,
                cache_read: 999.0,
                cache_create: 999.0,
            },
        );
        put_builtins(&mut entries);
        assert!((entries["gpt-5"].input - 1.25e-6).abs() < 1e-18);
    }
}
