//! Tier 1: the build-time pricing snapshot embedded into the binary.
//!
//! `build.rs` compacts the checked-in, LiteLLM-shaped snapshot into
//! `OUT_DIR/litellm-pricing.json`; it is included literally here. The same
//! [`parse`] turns a LiteLLM-shaped document into a price table, so the runtime
//! refresh and the embedded snapshot share one parser.

use std::collections::HashMap;

use serde_json::Value;

use super::Pricing;

/// The compacted LiteLLM snapshot, embedded at build time by `build.rs`.
const BUILD_TIME_PRICING_JSON: &str =
    include_str!(concat!(env!("OUT_DIR"), "/litellm-pricing.json"));

/// Parse the embedded snapshot into a model→price table.
pub(super) fn load() -> HashMap<String, Pricing> {
    parse(BUILD_TIME_PRICING_JSON)
}

/// Parse a LiteLLM-shaped pricing document.
///
/// Each top-level key is a model id; its object carries per-token costs. An
/// entry is kept only when both input and output per-token costs are present,
/// mirroring the upstream schema's required fields. Cache costs default to `0`.
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
        out.insert(
            model,
            Pricing {
                input,
                output,
                cache_read: num("cache_read_input_token_cost").unwrap_or(0.0),
                cache_create: num("cache_creation_input_token_cost").unwrap_or(0.0),
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_snapshot_has_a_known_model() {
        // Guards the build-time embed: if `build.rs` produced an empty or broken
        // snapshot, this fails loudly rather than silently pricing nothing.
        let table = load();
        assert!(!table.is_empty(), "embedded pricing snapshot is empty");
        assert!(
            table.keys().any(|k| k.starts_with("gpt-5")),
            "embedded snapshot is missing the gpt-5 family"
        );
    }

    #[test]
    fn embedded_snapshot_prices_claude_fable() {
        let table = load();
        let price = table
            .get("claude-fable-5")
            .expect("embedded snapshot is missing claude-fable-5");
        assert!((price.input - 1e-5).abs() < 1e-18);
        assert!((price.output - 5e-5).abs() < 1e-18);
        assert!((price.cache_read - 1e-6).abs() < 1e-18);
        assert!((price.cache_create - 1.25e-5).abs() < 1e-18);
    }

    #[test]
    fn parse_keeps_only_priced_entries() {
        let json = r#"{
            "gpt-x": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
                       "cache_read_input_token_cost": 1e-7},
            "no-output": {"input_cost_per_token": 1e-6},
            "not-an-object": 5
        }"#;
        let table = parse(json);
        assert_eq!(table.len(), 1);
        let p = table.get("gpt-x").unwrap();
        assert!((p.input - 1e-6).abs() < 1e-18);
        assert!((p.output - 2e-6).abs() < 1e-18);
        assert!((p.cache_read - 1e-7).abs() < 1e-18);
        assert_eq!(p.cache_create, 0.0);
    }
}
