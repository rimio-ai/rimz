//! Tier 1: the build-time pricing snapshot embedded into the binary.
//!
//! `build.rs` compacts the generated, LiteLLM-shaped snapshot into
//! `OUT_DIR/litellm-pricing.json.gz`; it is included as compressed bytes here.
//! The same [`parse`] turns a LiteLLM-shaped document into a price table,
//! including 200k tiers, cache defaults, and fast multipliers, so the runtime
//! refresh and the embedded snapshot share one parser.

use std::collections::HashMap;
use std::io::Read;

use flate2::read::GzDecoder;
use serde_json::Value;

use super::{Pricing, overrides};

/// The compacted LiteLLM snapshot, embedded at build time by `build.rs`.
const BUILD_TIME_PRICING_JSON_GZ: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/litellm-pricing.json.gz"));

/// Parse the embedded snapshot into a model→price table.
pub(super) fn load() -> HashMap<String, Pricing> {
    let mut json = String::new();
    if GzDecoder::new(BUILD_TIME_PRICING_JSON_GZ)
        .read_to_string(&mut json)
        .is_err()
    {
        return HashMap::new();
    }
    parse(&json)
}

/// Parse a LiteLLM-shaped pricing document.
///
/// Each top-level key is a model id; its object carries per-token costs. An
/// entry is kept only when both input and output per-token costs are present,
/// mirroring the upstream schema's required fields. Missing cache-create and
/// cache-read costs follow ccusage defaults: 1.25× input and 0.1× input.
pub(super) fn parse(json: &str) -> HashMap<String, Pricing> {
    let mut out = HashMap::new();
    let Ok(Value::Object(models)) = serde_json::from_str::<Value>(json) else {
        return out;
    };
    for (model, value) in models {
        let Value::Object(fields) = value else {
            continue;
        };
        let num = |key: &str| fields.get(key).and_then(Value::as_f64);
        let (Some(input), Some(output)) =
            (num("input_cost_per_token"), num("output_cost_per_token"))
        else {
            continue;
        };
        let cache_read = num("cache_read_input_token_cost");
        let fast = fields
            .get("provider_specific_entry")
            .and_then(Value::as_object)
            .and_then(|entry| entry.get("fast"))
            .and_then(Value::as_f64)
            .or_else(|| overrides::multiplier_for(&model))
            .unwrap_or(1.0);
        out.insert(
            model,
            Pricing {
                input_above_200k: num("input_cost_per_token_above_200k_tokens"),
                output_above_200k: num("output_cost_per_token_above_200k_tokens"),
                cache_create_above_200k: num("cache_creation_input_token_cost_above_200k_tokens"),
                cache_read_above_200k: num("cache_read_input_token_cost_above_200k_tokens"),
                long_context_threshold: None,
                fast_multiplier: fast,
                max_input_tokens: fields
                    .get("max_input_tokens")
                    .and_then(Value::as_u64)
                    .filter(|tokens| *tokens > 0),
                ..Pricing::from_base_rates(
                    input,
                    output,
                    num("cache_creation_input_token_cost"),
                    cache_read,
                )
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_snapshot_decompresses() {
        // A fresh clone may have no generated snapshot yet, in which case
        // build.rs embeds `{}`. The assertion here guards the gzip boundary.
        let table = load();
        assert!(table.values().all(|price| {
            price.input.is_finite()
                && price.output.is_finite()
                && price.cache_read.is_finite()
                && price.cache_create.is_finite()
                && price.fast_multiplier.is_finite()
                && price.input >= 0.0
                && price.output >= 0.0
                && price.cache_read >= 0.0
                && price.cache_create >= 0.0
                && price.fast_multiplier >= 0.0
        }));
    }

    #[test]
    fn parse_keeps_only_priced_entries() {
        let json = r#"{
            "gpt-x": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
                       "cache_read_input_token_cost": 1e-7,
                       "input_cost_per_token_above_200k_tokens": 3e-6,
                       "max_input_tokens": 128000,
                       "provider_specific_entry": {"fast": 2.5}},
            "no-output": {"input_cost_per_token": 1e-6},
            "default-cache": {"input_cost_per_token": 4e-6, "output_cost_per_token": 5e-6},
            "not-an-object": 5
        }"#;
        let table = parse(json);
        assert_eq!(table.len(), 2);
        let p = table.get("gpt-x").unwrap();
        assert!((p.input - 1e-6).abs() < 1e-18);
        assert!((p.output - 2e-6).abs() < 1e-18);
        assert!((p.cache_read - 1e-7).abs() < 1e-18);
        assert!((p.cache_create - 1.25e-6).abs() < 1e-18);
        assert_eq!(p.input_above_200k, Some(3e-6));
        assert_eq!(p.max_input_tokens, Some(128_000));
        assert!(p.cache_read_explicit);
        assert_eq!(p.fast_multiplier, 2.5);

        let p = table.get("default-cache").unwrap();
        assert!((p.cache_read - 4e-7).abs() < 1e-18);
        assert!((p.cache_create - 5e-6).abs() < 1e-18);
        assert!(!p.cache_read_explicit);
        assert_eq!(p.fast_multiplier, 1.0);
        assert_eq!(p.max_input_tokens, None);
    }

    #[test]
    fn parse_ignores_zero_fractional_and_non_numeric_capacity() {
        let table = parse(
            r#"{
              "zero":{"input_cost_per_token":1e-6,"output_cost_per_token":2e-6,"max_input_tokens":0},
              "fractional":{"input_cost_per_token":1e-6,"output_cost_per_token":2e-6,"max_input_tokens":12.5},
              "string":{"input_cost_per_token":1e-6,"output_cost_per_token":2e-6,"max_input_tokens":"128000"}
            }"#,
        );
        assert!(
            table
                .values()
                .all(|pricing| pricing.max_input_tokens.is_none())
        );
    }

    #[test]
    fn parse_fills_fast_multiplier_from_overrides() {
        let table = parse(
            r#"{"claude-opus-4.8-20260528": {"input_cost_per_token": 5e-6, "output_cost_per_token": 25e-6}}"#,
        );

        assert_eq!(table["claude-opus-4.8-20260528"].fast_multiplier, 2.0);
    }
}
