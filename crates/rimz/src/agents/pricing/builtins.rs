//! Tier 2: hardcoded fallback prices for models Rimz must price offline.
//!
//! Builtins overwrite the embedded snapshot, then the live LiteLLM cache may
//! overwrite them. That matches ccusage precedence: local fallbacks cover stale
//! snapshots and absent rows, while fresh upstream pricing wins as it drifts.
//! `gpt-5` is mandatory because the Codex parser falls back to it when a log has
//! no model.

use std::collections::HashMap;

use super::{Pricing, overrides};

/// Overlay guaranteed fallback model prices, overwriting the embedded snapshot.
pub(super) fn put_builtins(entries: &mut HashMap<String, Pricing>) {
    entries.insert(
        "claude-opus-4-5".to_owned(),
        p(5e-6, 25e-6, 6.25e-6, 0.5e-6),
    );
    entries.insert(
        "claude-opus-4-6".to_owned(),
        with_fast("claude-opus-4-6", p(5e-6, 25e-6, 6.25e-6, 0.5e-6)),
    );
    entries.insert(
        "claude-opus-4-7".to_owned(),
        with_fast("claude-opus-4-7", p(5e-6, 25e-6, 6.25e-6, 0.5e-6)),
    );
    entries.insert(
        "claude-opus-4-8".to_owned(),
        with_fast("claude-opus-4-8", p(5e-6, 25e-6, 6.25e-6, 0.5e-6)),
    );
    entries.insert(
        "claude-haiku-4-5".to_owned(),
        p(1e-6, 5e-6, 1.25e-6, 0.1e-6),
    );
    entries.insert(
        "claude-opus-4".to_owned(),
        p(15e-6, 75e-6, 18.75e-6, 1.5e-6),
    );
    entries.insert(
        "claude-sonnet-4-6".to_owned(),
        p(3e-6, 15e-6, 3.75e-6, 0.3e-6),
    );
    entries.insert(
        "claude-sonnet-4".to_owned(),
        Pricing {
            input_above_200k: Some(6e-6),
            output_above_200k: Some(22.5e-6),
            cache_create_above_200k: Some(7.5e-6),
            cache_read_above_200k: Some(0.6e-6),
            ..p(3e-6, 15e-6, 3.75e-6, 0.3e-6)
        },
    );

    let claude_3_5_haiku = p(0.8e-6, 4e-6, 1.0e-6, 0.08e-6);
    entries.insert("claude-3-5-haiku".to_owned(), claude_3_5_haiku);
    entries.insert("claude-3-5-haiku-20241022".to_owned(), claude_3_5_haiku);
    entries.insert(
        "claude-3-opus".to_owned(),
        p(15e-6, 75e-6, 18.75e-6, 1.5e-6),
    );
    entries.insert(
        "claude-3-sonnet".to_owned(),
        p(3e-6, 15e-6, 3.75e-6, 0.3e-6),
    );
    entries.insert(
        "claude-3-haiku".to_owned(),
        p(0.25e-6, 1.25e-6, 0.3e-6, 0.03e-6),
    );

    entries.insert("gpt-5".to_owned(), p(1.25e-6, 10e-6, 1.25e-6, 0.125e-6));
    entries.insert(
        "gpt-5.5".to_owned(),
        with_fast("gpt-5.5", p(5e-6, 30e-6, 5e-6, 0.5e-6)),
    );
    entries.insert(
        "grok-4.3".to_owned(),
        Pricing {
            cache_read_explicit: false,
            ..p(1.25e-6, 2.5e-6, 1.25e-6, 0.125e-6)
        },
    );

    entries.insert(
        "moonshot/kimi-k2.5".to_owned(),
        p(0.6e-6, 3e-6, 0.75e-6, 0.1e-6),
    );
    entries.insert(
        "moonshot/kimi-k2.6".to_owned(),
        p(0.95e-6, 4e-6, 1.1875e-6, 0.16e-6),
    );

    let gpt_5_1 = p(1.25e-6, 10e-6, 1.25e-6, 0.125e-6);
    entries.insert("gpt-5.1".to_owned(), gpt_5_1);
    entries.insert("gpt-5.1-codex".to_owned(), gpt_5_1);

    let gpt_5_codex = p(1.75e-6, 14e-6, 1.75e-6, 0.175e-6);
    entries.insert("gpt-5.2-codex".to_owned(), gpt_5_codex);
    entries.insert(
        "gpt-5.3-codex".to_owned(),
        with_fast("gpt-5.3-codex", gpt_5_codex),
    );
    entries.insert("gpt-5.2".to_owned(), gpt_5_codex);
    entries.insert(
        "gpt-5.4".to_owned(),
        with_fast("gpt-5.4", p(2.5e-6, 15e-6, 2.5e-6, 0.25e-6)),
    );
    entries.insert(
        "gpt-5.4-mini".to_owned(),
        p(0.75e-6, 4.5e-6, 0.75e-6, 0.075e-6),
    );
    entries.insert(
        "gpt-5.4-nano".to_owned(),
        p(0.2e-6, 1.25e-6, 0.2e-6, 0.02e-6),
    );
    entries.insert("gpt-5.6-sol".to_owned(), p(5e-6, 30e-6, 5e-6, 0.5e-6));
    entries.insert(
        "gpt-5.6-terra".to_owned(),
        p(2.5e-6, 15e-6, 2.5e-6, 0.25e-6),
    );
    entries.insert("gpt-5.6-luna".to_owned(), p(1e-6, 6e-6, 1e-6, 0.1e-6));

    entries.insert(
        "gemini-3-pro-preview".to_owned(),
        p(2e-6, 12e-6, 4.5e-6, 0.2e-6),
    );
    entries.insert(
        "gemini-3-flash-preview".to_owned(),
        p(0.5e-6, 3e-6, 1e-6, 0.05e-6),
    );

    let glm = |input: f64, output: f64, cache_read: f64| Pricing {
        input,
        output,
        cache_create: 0.0,
        cache_read,
        cache_read_explicit: true,
        ..Pricing::empty()
    };
    let glm_base = glm(0.6e-6, 2.2e-6, 0.11e-6);
    entries.insert("glm-4.5".to_owned(), glm_base);
    entries.insert("zai/glm-4.5".to_owned(), glm_base);
    entries.insert("zai/glm-4.5-x".to_owned(), glm(2.2e-6, 8.9e-6, 0.45e-6));
    entries.insert("zai/glm-4.5-air".to_owned(), glm(0.2e-6, 1.1e-6, 0.03e-6));
    entries.insert("zai/glm-4.5-airx".to_owned(), glm(1.1e-6, 4.5e-6, 0.22e-6));
    entries.insert("zai/glm-4.5v".to_owned(), glm(0.6e-6, 1.8e-6, 0.11e-6));
    entries.insert(
        "zai/glm-4-32b-0414-128k".to_owned(),
        glm(0.1e-6, 0.1e-6, 0.0),
    );
    entries.insert("zai/glm-4.5-flash".to_owned(), glm(0.0, 0.0, 0.0));
    entries.insert("glm-4.6".to_owned(), glm_base);
    entries.insert("glm-4.7".to_owned(), glm_base);
    entries.insert(
        "glm-5".to_owned(),
        Pricing {
            input: 1.0e-6,
            output: 3.2e-6,
            cache_read: 0.2e-6,
            ..glm_base
        },
    );
    entries.insert(
        "glm-5-turbo".to_owned(),
        Pricing {
            input: 1.2e-6,
            output: 4.0e-6,
            cache_read: 0.24e-6,
            ..glm_base
        },
    );
    entries.insert(
        "glm-5.1".to_owned(),
        Pricing {
            input: 1.4e-6,
            output: 4.4e-6,
            cache_read: 0.26e-6,
            ..glm_base
        },
    );
}

fn p(input: f64, output: f64, cache_create: f64, cache_read: f64) -> Pricing {
    Pricing {
        input,
        output,
        cache_create,
        cache_read,
        cache_read_explicit: true,
        ..Pricing::empty()
    }
}

fn with_fast(model: &str, price: Pricing) -> Pricing {
    Pricing {
        fast_multiplier: overrides::multiplier_for(model).unwrap_or(1.0),
        ..price
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_overwrite_embedded_fallbacks_and_carry_ccusage_fields() {
        let mut entries = HashMap::from([(
            "gpt-5".to_owned(),
            Pricing {
                input: 999.0,
                output: 999.0,
                cache_read: 999.0,
                cache_create: 999.0,
                fast_multiplier: 999.0,
                ..Pricing::empty()
            },
        )]);
        put_builtins(&mut entries);

        assert!((entries["gpt-5"].input - 1.25e-6).abs() < 1e-18);
        assert!((entries["gpt-5"].cache_create - 1.25e-6).abs() < 1e-18);
        assert_eq!(entries["gpt-5.5"].fast_multiplier, 2.5);
        assert_eq!(entries["gpt-5.6-sol"].input, 5e-6);
        assert_eq!(entries["gpt-5.6-sol"].output, 30e-6);
        assert_eq!(entries["gpt-5.6-sol"].cache_create, 5e-6);
        assert_eq!(entries["gpt-5.6-sol"].cache_read, 0.5e-6);
        assert_eq!(entries["gemini-3-pro-preview"].output, 12e-6);
        assert_eq!(entries["gemini-3-flash-preview"].input, 0.5e-6);
        assert_eq!(entries["claude-opus-4-8"].fast_multiplier, 2.0);
        assert_eq!(entries["claude-sonnet-4"].input_above_200k, Some(6e-6));
        assert!((entries["moonshot/kimi-k2.6"].cache_read - 0.16e-6).abs() < 1e-18);
    }
}
