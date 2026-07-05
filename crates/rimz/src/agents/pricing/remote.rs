//! Tier 2 + 3b: the runtime fetches that keep the table fresh.
//!
//! [`fetch_litellm`] re-downloads the upstream LiteLLM table; [`fetch_models_dev`]
//! pulls chase-only authoritative models.dev provider catalogues used to fill
//! models neither the embedded snapshot nor the LiteLLM refresh knows. Both are
//! best-effort: a failure returns `None` and the caller keeps whatever it already
//! had. The decision of *when* to fetch (TTL + back-off, on-disk cache) lives in
//! the parent module — this file only knows how to GET and parse.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;

use super::Pricing;

/// LiteLLM's canonical pricing document (`main`).
const LITELLM_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/refs/heads/main/model_prices_and_context_window.json";
/// models.dev's aggregate model catalogue.
const MODELS_DEV_URL: &str = "https://models.dev/api.json";

/// Cap the wait so a wedged endpoint never stalls the producer for long.
const TIMEOUT_SECS: u64 = 5;
/// Cap the response so a malformed/huge body cannot exhaust memory.
const MAX_BYTES: u64 = 32 * 1024 * 1024;

/// `true` when network fetches are disabled (tests, CI, air-gapped runs).
pub(super) fn offline() -> bool {
    std::env::var_os("RIMZ_PRICING_OFFLINE").is_some()
}

/// Fetch the upstream LiteLLM pricing JSON, or `None` on any failure.
pub(super) fn fetch_litellm() -> Option<String> {
    fetch(LITELLM_URL)
}

/// Fetch the models.dev catalogue JSON, or `None` on any failure.
pub(super) fn fetch_models_dev() -> Option<String> {
    fetch(MODELS_DEV_URL)
}

fn fetch(url: &str) -> Option<String> {
    if offline() {
        return None;
    }
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(TIMEOUT_SECS)))
        .build()
        .new_agent();
    let mut response = match agent.get(url).call() {
        Ok(response) => response,
        Err(err) => {
            tracing::debug!(url, error = %err, "pricing fetch failed");
            return None;
        }
    };
    if response.status().as_u16() != 200 {
        tracing::debug!(
            url,
            status = response.status().as_u16(),
            "pricing fetch non-200"
        );
        return None;
    }
    match response
        .body_mut()
        .with_config()
        .limit(MAX_BYTES)
        .read_to_string()
    {
        Ok(body) => Some(body),
        Err(err) => {
            tracing::debug!(url, error = %err, "pricing body read failed");
            None
        }
    }
}

/// Parse the models.dev catalogue into a model→price table.
///
/// Shape: `{ provider: { models: { id: { cost: { input, output, cache_read,
/// cache_write } } } } }`, with costs quoted **per 1M tokens** — divided here to
/// per-token to match [`Pricing`]. Missing cache costs follow ccusage defaults:
/// cache-write is 1.25× input and cache-read is 0.1× input. Only authoritative
/// provider IDs are read, so gateway markups cannot overwrite direct
/// Anthropic/OpenAI prices. Defensive: any entry missing input/output is skipped
/// rather than failing the whole parse.
pub(super) fn parse_models_dev(json: &str) -> BTreeMap<String, Pricing> {
    let mut out = BTreeMap::new();
    let Ok(Value::Object(providers)) = serde_json::from_str::<Value>(json) else {
        return out;
    };
    for (provider_id, provider) in providers {
        if !is_authoritative_provider(&provider_id) {
            continue;
        }
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (model_id, model) in models {
            let Some(cost) = model.get("cost").and_then(Value::as_object) else {
                continue;
            };
            let per_million = |key: &str| cost.get(key).and_then(Value::as_f64);
            let (Some(input), Some(output)) = (per_million("input"), per_million("output")) else {
                continue;
            };
            let input = input / 1e6;
            let output = output / 1e6;
            let cache_read = per_million("cache_read").map(|cost| cost / 1e6);
            let cache_create = per_million("cache_write").map(|cost| cost / 1e6);
            out.insert(
                model_id.clone(),
                Pricing {
                    input,
                    output,
                    cache_read: cache_read.unwrap_or(input * 0.1),
                    cache_create: cache_create.unwrap_or(input * 1.25),
                    cache_read_explicit: cache_read.is_some(),
                    ..Pricing::empty()
                },
            );
        }
    }
    out
}

fn is_authoritative_provider(provider_id: &str) -> bool {
    matches!(provider_id, "anthropic" | "openai")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_dev_costs_convert_per_million_to_per_token() {
        let json = r#"{
            "openai": {"models": {
                "gpt-x": {"cost": {"input": 1.25, "output": 10.0, "cache_read": 0.125}}
            }},
            "anthropic": {"models": {
                "claude-x": {"cost": {"input": 3.0, "output": 15.0}}
            }},
            "gateway": {"models": {
                "gpt-x": {"cost": {"input": 99.0, "output": 99.0}}
            }},
            "bad": {"models": {"y": {"cost": {"input": 1.0}}}}
        }"#;
        let table = parse_models_dev(json);
        assert_eq!(table.len(), 2);
        let p = table.get("gpt-x").unwrap();
        assert!((p.input - 1.25e-6).abs() < 1e-18);
        assert!((p.output - 1.0e-5).abs() < 1e-18);
        assert!((p.cache_read - 1.25e-7).abs() < 1e-18);
        assert!(p.cache_read_explicit);
        let p = table.get("claude-x").unwrap();
        assert!((p.cache_read - 3e-7).abs() < 1e-18);
        assert!((p.cache_create - 3.75e-6).abs() < 1e-18);
        assert!(!p.cache_read_explicit);
    }
}
