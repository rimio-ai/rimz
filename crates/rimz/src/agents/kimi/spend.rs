//! Read-only Kimi Wire token spend parser.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::pricing::PriceBook;
use crate::agents::spending::{CachedEntry, SpendCursor, SpendParse, record_unknown_model};

use super::wire;

const FALLBACK_MODEL: &str = "moonshot/kimi-k2.5";

pub fn files() -> Vec<PathBuf> {
    wire::transcript_files()
}

pub fn parse(path: &Path, resume: Option<&SpendCursor>, prices: &PriceBook) -> SpendParse {
    let from = resume.map_or(0, |cursor| cursor.offset);
    let Some((records, next)) = wire::read_records(path, from) else {
        return SpendParse {
            cursor: SpendCursor {
                offset: from,
                state: None,
            },
            ..SpendParse::default()
        };
    };
    let configured = configured_model();
    let mut entries = Vec::new();
    let mut unknown_models = BTreeMap::new();
    for (timestamp, record) in wire::usage_records(&records) {
        let model = (!record.model.trim().is_empty())
            .then_some(record.model.as_str())
            .filter(|model| prices.price(model).is_some())
            .or_else(|| {
                configured
                    .as_deref()
                    .filter(|model| prices.price(model).is_some())
            })
            .unwrap_or(FALLBACK_MODEL);
        let usage = record.usage;
        let fresh = usage.input_other.unwrap_or(0);
        let cache_read = usage.input_cache_read.unwrap_or(0);
        let cache_write = usage.input_cache_creation.unwrap_or(0);
        let output = usage.output.unwrap_or(0);
        let ts_secs = if timestamp > 100_000_000_000.0 {
            (timestamp / 1_000.0).max(0.0) as u64
        } else {
            timestamp.max(0.0) as u64
        };
        let cost_usd = match prices.price(model) {
            Some(price) => price.cost(fresh, output, cache_write, 0, cache_read, false),
            None => {
                record_unknown_model(&mut unknown_models, model, ts_secs);
                0.0
            }
        };
        entries.push(CachedEntry {
            ts_secs,
            cost_usd,
            input: fresh,
            output,
            cache_write,
            cache_read,
            message_id: None,
            request_id: None,
            thread_id: path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned),
            is_sidechain: false,
            model: Some(model.to_owned()),
            rolled: false,
        });
    }
    SpendParse {
        entries,
        origin: None,
        cursor: SpendCursor {
            offset: next,
            state: None,
        },
        unknown_models,
        replace_entries: false,
    }
}

pub fn configured_model() -> Option<String> {
    let path = super::install::config_path().ok()?;
    configured_model_at(&path)
}

pub(crate) fn configured_model_at(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let root: toml::Table = toml::from_str(&text).ok()?;
    root.get("default_model")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
}
