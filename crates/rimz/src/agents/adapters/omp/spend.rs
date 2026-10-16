//! OMP main-session JSONL accounting.

use std::collections::BTreeMap;
use std::io::BufRead as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::pricing::{PriceBook, TokenSplit};
use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, SpendUpdate, origin_path, price_split,
};
use crate::agents::transcript_fs::read_transcript_lines;

#[derive(Default, Deserialize, Serialize)]
struct State {
    id: Option<String>,
    cwd: Option<PathBuf>,
}

pub(super) fn parse(path: &Path, resume: Option<&SpendCursor>, prices: &PriceBook) -> SpendParse {
    let Ok(file) = std::fs::File::open(path) else {
        return SpendParse::stalled(resume);
    };
    let mut state = State::default();
    for line in std::io::BufReader::new(file)
        .lines()
        .take(2)
        .map_while(std::result::Result::ok)
    {
        let Ok(header) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if header.get("type").and_then(Value::as_str) == Some("session") {
            state.id = header
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            state.cwd = origin_path(header.get("cwd").and_then(Value::as_str));
            break;
        }
    }
    let old: State = resume.map(SpendCursor::state_as).unwrap_or_default();
    let replaced = resume.is_some_and(|cursor| {
        old.id != state.id || std::fs::metadata(path).is_ok_and(|m| m.len() < cursor.offset)
    });
    let from = if replaced {
        0
    } else {
        resume.map_or(0, |c| c.offset)
    };
    let Some((content, next_offset)) = read_transcript_lines(path, from) else {
        if replaced && std::fs::metadata(path).is_ok_and(|m| m.len() == 0) {
            return SpendParse {
                origin: state.cwd.clone(),
                cursor: SpendCursor::with_state(0, &state),
                update: SpendUpdate::Replace,
                ..SpendParse::stalled(None)
            };
        }
        return SpendParse {
            origin: state.cwd,
            ..SpendParse::stalled(resume)
        };
    };
    let mut entries = Vec::new();
    let mut unknown_models = BTreeMap::new();
    for line in String::from_utf8_lossy(&content).lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let (usage, model, blocks) = match entry.get("type").and_then(Value::as_str) {
            Some("message") => {
                let Some(message) = entry.get("message") else {
                    continue;
                };
                if message.get("role").and_then(Value::as_str) != Some("assistant") {
                    continue;
                }
                (
                    message.get("usage"),
                    message.get("model"),
                    message.get("content"),
                )
            }
            Some("model_usage") => (entry.get("usage"), entry.get("model"), None),
            _ => continue,
        };
        let Some(usage) = usage.and_then(Value::as_object) else {
            continue;
        };
        let Some(ts_secs) = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(crate::agents::spending::iso_to_unix_secs)
        else {
            continue;
        };
        let count = |key| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        let input = count("input");
        let cache_write = count("cacheWrite");
        let cache_read = count("cacheRead");
        let output = count("output");
        let known = input
            .saturating_add(output)
            .saturating_add(cache_write)
            .saturating_add(cache_read);
        let output = output.saturating_add(count("totalTokens").saturating_sub(known));
        let split = TokenSplit::new(input, output).cached(cache_write, cache_read);
        let model = model.and_then(Value::as_str);
        let direct_cost = usage
            .get("cost")
            .and_then(|c| c.get("total"))
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= 0.0);
        let cost = direct_cost.unwrap_or_else(|| {
            model
                .filter(|_| {
                    input
                        .saturating_add(output)
                        .saturating_add(cache_read)
                        .saturating_add(cache_write)
                        > 0
                })
                .and_then(|m| price_split(prices, m, split, ts_secs, &mut unknown_models))
                .unwrap_or(0.0)
        });
        if input == 0 && output == 0 && cache_write == 0 && cache_read == 0 && cost <= 0.0 {
            continue;
        }
        let mut cached = CachedEntry {
            model: model.map(ToOwned::to_owned),
            ..CachedEntry::new(ts_secs, cost, &split)
        };
        for block in blocks.and_then(Value::as_array).into_iter().flatten() {
            if block.get("type").and_then(Value::as_str) != Some("toolCall") {
                continue;
            }
            if let Some(name) = block
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|n| !n.is_empty())
            {
                let count = cached.tool_calls.entry(name.to_owned()).or_default();
                *count = count.saturating_add(1);
            }
        }
        entries.push(cached);
    }
    SpendParse {
        entries,
        origin: state.cwd.clone(),
        cursor: SpendCursor::with_state(next_offset, &state),
        unknown_models,
        update: if replaced {
            SpendUpdate::Replace
        } else {
            SpendUpdate::Extend
        },
    }
}
