//! Read-only pricing of one Droid session settings snapshot.
//!
//! Factory publishes cumulative token categories but no authoritative USD.
//! This parser produces one replace-style live-session entry only when the raw
//! selector has a proven canonical identity and the price book has an exact
//! row. Fleet discovery remains disabled on the adapter, keeping values out
//! of historical/account spend.

use std::collections::BTreeMap;
use std::path::Path;

use crate::agents::pricing::PriceBook;
use crate::agents::spending::{CachedEntry, SpendCursor, SpendParse};
use crate::agents::{AgentCost, AgentSessionUsage};

use super::{config, droid_settings_path, transcript};

pub(super) fn parse(path: &Path, prices: &PriceBook) -> SpendParse {
    let Some(snapshot) = transcript::settings_snapshot(path, None) else {
        return SpendParse::default();
    };
    let Some(selector) = snapshot.telemetry.model.as_deref().map(str::trim) else {
        return SpendParse::default();
    };
    if selector.is_empty() {
        return SpendParse::default();
    }
    let canonical = if selector.starts_with("custom:") {
        let Some(user_settings) = droid_settings_path().ok() else {
            return SpendParse::default();
        };
        let Some(session_cwd) = snapshot.session_cwd.as_deref() else {
            return SpendParse::default();
        };
        let Some(resolved) =
            config::resolve_custom_model_from_cwd(selector, session_cwd, &user_settings)
        else {
            return SpendParse::default();
        };
        resolved.model_id
    } else {
        selector.to_owned()
    };
    let Some(usage) = snapshot.telemetry.session_usage else {
        return SpendParse::default();
    };
    let Some(priced) = price_exact_model(&canonical, &usage, prices) else {
        return SpendParse::default();
    };
    let Some(session_id) = session_id(&snapshot.settings_path) else {
        return SpendParse::default();
    };
    let ts_secs = snapshot.stat.mtime_secs.max(0) as u64;
    let entry = CachedEntry {
        ts_secs,
        cost_usd: priced.cost_usd,
        input: priced.input,
        output: priced.output,
        cache_write: priced.cache_write,
        cache_read: priced.cache_read,
        message_id: None,
        request_id: None,
        dedup_key: Some(format!("droid-settings:{session_id}")),
        thread_id: Some(session_id),
        is_sidechain: false,
        has_speed: false,
        model: Some(canonical),
        rolled: false,
    };
    SpendParse {
        entries: vec![entry],
        origin: snapshot.session_cwd,
        cursor: SpendCursor::default(),
        unknown_models: BTreeMap::new(),
        replace_entries: true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PricedUsage {
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
    cost_usd: f64,
}

fn price_exact_model(
    model: &str,
    usage: &AgentSessionUsage,
    prices: &PriceBook,
) -> Option<PricedUsage> {
    let price = prices.exact_price(model)?;
    let input = usage.input_tokens.unwrap_or(0);
    let output = usage
        .output_tokens
        .unwrap_or(0)
        .saturating_add(usage.thinking_tokens.unwrap_or(0));
    let cache_write = usage.cache_creation_input_tokens.unwrap_or(0);
    let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
    if input == 0 && output == 0 && cache_write == 0 && cache_read == 0 {
        return None;
    }
    Some(PricedUsage {
        input,
        output,
        cache_write,
        cache_read,
        cost_usd: price.cost(input, output, cache_write, 0, cache_read, false),
    })
}

pub(super) fn live_cost(
    model: Option<&str>,
    usage: Option<&AgentSessionUsage>,
    prices: &PriceBook,
) -> Option<AgentCost> {
    let priced = price_exact_model(model?, usage?, prices)?;
    Some(AgentCost {
        total_cost_usd: Some(priced.cost_usd),
        ..AgentCost::default()
    })
}

fn session_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let id = name.strip_suffix(".settings.json")?.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_price_counts_thinking_as_output_and_cache_creation_as_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess-1.settings.json");
        std::fs::write(
            &path,
            r#"{
              "model":"priced-model",
              "tokenUsage":{
                "inputTokens":100,
                "outputTokens":20,
                "cacheCreationTokens":30,
                "cacheReadTokens":40,
                "thinkingTokens":5
              }
            }"#,
        )
        .unwrap();
        let prices = PriceBook::from_litellm_json(
            r#"{"priced-model":{"input_cost_per_token":0.001,"output_cost_per_token":0.002,"cache_creation_input_token_cost":0.003,"cache_read_input_token_cost":0.0001}}"#,
        );

        let parsed = parse(&path, &prices);
        assert!(parsed.replace_entries);
        let entry = &parsed.entries[0];
        assert_eq!(
            (
                entry.input,
                entry.output,
                entry.cache_write,
                entry.cache_read
            ),
            (100, 25, 30, 40)
        );
        assert!((entry.cost_usd - 0.244).abs() < 1e-12);
        assert_eq!(entry.thread_id.as_deref(), Some("sess-1"));
    }

    #[test]
    fn fuzzy_or_unknown_models_produce_no_cost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sess.settings.json");
        std::fs::write(
            &path,
            r#"{"model":"priced-model-vendor-suffix","tokenUsage":{"inputTokens":100}}"#,
        )
        .unwrap();
        let prices = PriceBook::from_litellm_json(
            r#"{"priced-model":{"input_cost_per_token":0.001,"output_cost_per_token":0.002}}"#,
        );

        assert!(parse(&path, &prices).entries.is_empty());
    }

    #[test]
    fn pure_live_pricing_needs_no_backing_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "temporary").unwrap();
        let usage = AgentSessionUsage {
            input_tokens: Some(100),
            output_tokens: Some(20),
            cache_creation_input_tokens: Some(30),
            cache_read_input_tokens: Some(40),
            thinking_tokens: Some(5),
        };
        let prices = PriceBook::from_litellm_json(
            r#"{"priced-model":{"input_cost_per_token":0.001,"output_cost_per_token":0.002,"cache_creation_input_token_cost":0.003,"cache_read_input_token_cost":0.0001}}"#,
        );
        std::fs::remove_file(path).unwrap();
        let cost = live_cost(Some("priced-model"), Some(&usage), &prices)
            .unwrap()
            .total_cost_usd
            .unwrap();
        assert!((cost - 0.244).abs() < 1e-12);
    }
}
