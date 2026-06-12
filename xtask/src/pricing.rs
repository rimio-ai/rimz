use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::files::write_atomically;

const LITELLM_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/refs/heads/main/model_prices_and_context_window.json";
const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const GENERATED_SNAPSHOT: &str = "crates/rimz/pricing/litellm-pricing.json";
const KEPT_FIELDS: [&str; 4] = [
    "input_cost_per_token",
    "output_cost_per_token",
    "cache_read_input_token_cost",
    "cache_creation_input_token_cost",
];

/// Regenerate the ignored pricing snapshot that `crates/rimz/build.rs` embeds
/// as the tier-1 table when present. Fetches LiteLLM first, fills missing
/// models from authoritative models.dev provider catalogues, compacts to the
/// fields Rimz reads, and writes a sorted JSON document. `RIMZ_PRICING_JSON_PATH`
/// overrides the LiteLLM network fetch with a local raw document;
/// `RIMZ_PRICING_MODELS_DEV_JSON_PATH` supplies a local models.dev document.
/// Without the models.dev override, a local LiteLLM override keeps the task
/// network-free and skips models.dev.
///
/// The compaction mirrors `crates/rimz/build.rs::compact`; keep the two in step.
pub(crate) fn pricing_refresh(root: &Path) -> Result<()> {
    let litellm_override = env::var_os("RIMZ_PRICING_JSON_PATH");
    let raw = if let Some(path) = &litellm_override {
        fs::read_to_string(PathBuf::from(path)).context("reading RIMZ_PRICING_JSON_PATH")?
    } else {
        fetch_url(LITELLM_URL).context("fetching LiteLLM pricing JSON")?
    };
    let mut snapshot = compact_litellm(&raw).context("compacting LiteLLM pricing JSON")?;
    if let Some(models_dev) = resolve_models_dev_json(litellm_override.is_some())
        .context("reading models.dev pricing JSON")?
    {
        for (model, pricing) in
            compact_models_dev(&models_dev).context("compacting models.dev pricing JSON")?
        {
            snapshot.entry(model).or_insert(pricing);
        }
    }
    let snapshot = compact_json(&snapshot).context("serializing pricing JSON")?;
    let dest = root.join(GENERATED_SNAPSHOT);
    write_atomically(&dest, snapshot.as_bytes())?;
    Ok(())
}

fn resolve_models_dev_json(skip_remote: bool) -> Result<Option<String>> {
    if let Some(path) = env::var_os("RIMZ_PRICING_MODELS_DEV_JSON_PATH") {
        return fs::read_to_string(PathBuf::from(path))
            .map(Some)
            .context("reading RIMZ_PRICING_MODELS_DEV_JSON_PATH");
    }
    if skip_remote {
        return Ok(None);
    }
    fetch_url(MODELS_DEV_URL)
        .map(Some)
        .context("fetching models.dev pricing JSON")
}

fn fetch_url(url: &str) -> Result<String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .new_agent();
    let mut response = agent.get(url).call().context("HTTP GET")?;
    if response.status().as_u16() != 200 {
        bail!("fetch returned HTTP {}", response.status().as_u16());
    }
    response
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()
        .context("reading response body")
}

fn compact_litellm(json: &str) -> Result<BTreeMap<String, Value>> {
    let Value::Object(raw) = serde_json::from_str::<Value>(json).context("parsing JSON")? else {
        bail!("pricing JSON is not an object");
    };
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    for (model, pricing) in raw {
        let Value::Object(fields) = pricing else {
            continue;
        };
        let mut kept = Map::new();
        for field in KEPT_FIELDS {
            if let Some(value) = fields.get(field)
                && !value.is_null()
            {
                kept.insert(field.to_owned(), value.clone());
            }
        }
        if kept.contains_key("input_cost_per_token") && kept.contains_key("output_cost_per_token") {
            out.insert(model, Value::Object(kept));
        }
    }
    Ok(out)
}

fn compact_models_dev(json: &str) -> Result<BTreeMap<String, Value>> {
    let Value::Object(providers) = serde_json::from_str::<Value>(json).context("parsing JSON")?
    else {
        bail!("models.dev JSON is not an object");
    };
    let mut out = BTreeMap::new();
    for (provider_id, provider) in providers {
        if !is_authoritative_models_dev_provider(&provider_id) {
            continue;
        }
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (model, details) in models {
            let Some(cost) = details.get("cost").and_then(Value::as_object) else {
                continue;
            };
            let per_million = |key: &str| cost.get(key).and_then(Value::as_f64);
            let (Some(input), Some(output)) = (per_million("input"), per_million("output")) else {
                continue;
            };
            let Some(input) = per_token_value(input) else {
                continue;
            };
            let Some(output) = per_token_value(output) else {
                continue;
            };

            let mut kept = Map::new();
            kept.insert("input_cost_per_token".to_owned(), input);
            kept.insert("output_cost_per_token".to_owned(), output);
            if let Some(cache_read) = per_million("cache_read").and_then(per_token_value) {
                kept.insert("cache_read_input_token_cost".to_owned(), cache_read);
            }
            if let Some(cache_write) = per_million("cache_write").and_then(per_token_value) {
                kept.insert("cache_creation_input_token_cost".to_owned(), cache_write);
            }
            out.insert(model.clone(), Value::Object(kept));
        }
    }
    Ok(out)
}

fn is_authoritative_models_dev_provider(provider_id: &str) -> bool {
    matches!(provider_id, "anthropic" | "openai")
}

fn per_token_value(per_million: f64) -> Option<Value> {
    serde_json::Number::from_f64(per_million / 1e6).map(Value::Number)
}

fn compact_json(out: &BTreeMap<String, Value>) -> Result<String> {
    let mut json = serde_json::to_string(&out).context("serializing snapshot")?;
    json.push('\n');
    Ok(json)
}

#[cfg(test)]
mod tests;
