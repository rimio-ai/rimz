//! Read-only Kimi Wire token spend parser.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agents::pricing::PriceBook;
use crate::agents::spending::{CachedEntry, SpendCursor, SpendParse, record_unknown_model};

use super::wire;

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct KimiSpendState {
    request: Option<wire::RequestAttribution>,
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
    fold_records(path, &records, next, resume, prices)
}

pub fn parse_snapshot(
    path: &Path,
    snapshot: &wire::WireSnapshot,
    prices: &PriceBook,
) -> SpendParse {
    fold_records(
        path,
        snapshot.records(),
        snapshot.consumed_offset(),
        None,
        prices,
    )
}

fn fold_records(
    path: &Path,
    records: &[wire::WireRecord],
    next: u64,
    resume: Option<&SpendCursor>,
    prices: &PriceBook,
) -> SpendParse {
    let mut state: KimiSpendState = resume
        .and_then(|cursor| cursor.state.clone())
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let mut entries = Vec::new();
    let mut unknown_models = BTreeMap::new();
    for wire_record in records {
        let record = match &wire_record.event {
            wire::WireEvent::LlmRequest(request) => {
                state.request = Some(request.clone());
                continue;
            }
            wire::WireEvent::Usage(record) => record,
            _ => continue,
        };
        let Some(timestamp) = wire_record.time else {
            continue;
        };
        let request_key = state.request.as_ref().and_then(request_model_key);
        let model = non_empty(&record.model)
            .filter(|model| prices.price(model).is_some())
            .or_else(|| {
                request_key
                    .as_deref()
                    .filter(|model| prices.price(model).is_some())
                    .map(ToOwned::to_owned)
            })
            .or_else(|| {
                state
                    .request
                    .as_ref()
                    .and_then(|request| request.model.as_deref())
                    .filter(|model| prices.price(model).is_some())
                    .map(ToOwned::to_owned)
            });
        let unknown_label = state
            .request
            .as_ref()
            .and_then(|request| request.model_alias.as_deref())
            .map(wire::normalize_model_alias)
            .or_else(|| non_empty(&record.model));
        let Some(model_label) = model.clone().or(unknown_label) else {
            continue;
        };
        let usage = &record.usage;
        let fresh = usage.input_other.unwrap_or(0);
        let cache_read = usage.input_cache_read.unwrap_or(0);
        let cache_write = usage.input_cache_creation.unwrap_or(0);
        let output = usage.output.unwrap_or(0);
        let ts_secs = (timestamp / 1_000.0) as u64;
        let cost_usd = match model.as_deref().and_then(|model| prices.price(model)) {
            Some(price) => price.cost(fresh, output, cache_write, 0, cache_read, false),
            None => {
                record_unknown_model(&mut unknown_models, &model_label, ts_secs);
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
            dedup_key: None,
            thread_id: path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned),
            is_sidechain: false,
            has_speed: false,
            model: Some(model_label),
            rolled: false,
        });
    }
    SpendParse {
        entries,
        origin: None,
        cursor: SpendCursor {
            offset: next,
            state: serde_json::to_value(state).ok(),
        },
        unknown_models,
        replace_entries: false,
    }
}

fn request_model_key(request: &wire::RequestAttribution) -> Option<String> {
    let provider = request.provider.as_deref().and_then(non_empty)?;
    let model = request.model.as_deref().and_then(non_empty)?;
    if model.starts_with(&format!("{provider}/")) {
        Some(model)
    } else {
        Some(format!("{provider}/{model}"))
    }
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
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
