//! Read-only Qwen Code JSONL token-spend parser.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agents::pricing::PriceBook;
use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, iso_to_unix_secs, origin_path, record_unknown_model,
};
use crate::agents::transcript_fs::{collect_jsonl, home_dir, read_spend_lines};

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Entry {
    uuid: Option<String>,
    session_id: Option<String>,
    timestamp: Option<String>,
    r#type: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    usage_metadata: Usage,
    agent_id: Option<String>,
    is_sidechain: Option<bool>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Usage {
    prompt_token_count: u64,
    candidates_token_count: u64,
    cached_content_token_count: u64,
    thoughts_token_count: u64,
}

fn runtime_base() -> PathBuf {
    std::env::var_os("QWEN_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("QWEN_HOME").filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".qwen"))
}

pub(crate) fn all_jsonl_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_jsonl(&runtime_base().join("projects"), &mut files);
    files.retain(|path| path.components().any(|part| part.as_os_str() == "chats"));
    files.sort();
    files.dedup();
    files
}

pub(crate) fn parse_qwen_spend(path: &Path, from_offset: u64, prices: &PriceBook) -> SpendParse {
    let Some((content, next_offset)) = read_spend_lines(path, from_offset) else {
        return SpendParse {
            cursor: SpendCursor {
                offset: from_offset,
                state: None,
            },
            ..SpendParse::default()
        };
    };
    let mut entries = Vec::new();
    let mut origin = None;
    let mut unknown_models = BTreeMap::new();
    for line in content
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
            continue;
        };
        if entry.r#type.as_deref() != Some("assistant") {
            continue;
        }
        let Some(model) = entry.model.as_deref().filter(|value| !value.is_empty()) else {
            continue;
        };
        let Some(timestamp) = entry.timestamp.as_deref().and_then(iso_to_unix_secs) else {
            continue;
        };
        let cached = entry.usage_metadata.cached_content_token_count;
        let input = entry
            .usage_metadata
            .prompt_token_count
            .saturating_sub(cached);
        let output =
            entry.usage_metadata.candidates_token_count + entry.usage_metadata.thoughts_token_count;
        let cost = match prices.price(model) {
            Some(price) => price.cost(input, output, 0, 0, cached, false),
            None => {
                record_unknown_model(&mut unknown_models, model, timestamp);
                0.0
            }
        };
        if origin.is_none() {
            origin = origin_path(entry.cwd.as_deref());
        }
        entries.push(CachedEntry {
            ts_secs: timestamp,
            cost_usd: cost,
            input,
            output,
            cache_write: 0,
            cache_read: cached,
            message_id: entry.uuid,
            request_id: None,
            dedup_key: None,
            thread_id: entry.session_id,
            is_sidechain: entry.is_sidechain == Some(true) || entry.agent_id.is_some(),
            has_speed: false,
            model: Some(model.to_owned()),
            rolled: false,
        });
    }
    SpendParse {
        entries,
        origin,
        cursor: SpendCursor {
            offset: next_offset,
            state: None,
        },
        unknown_models,
        replace_entries: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_and_preserves_dedup_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, r#"{"uuid":"msg-1","sessionId":"s1","timestamp":"2026-06-02T10:00:00Z","type":"assistant","model":"unknown-model","usageMetadata":{"promptTokenCount":100,"cachedContentTokenCount":25,"candidatesTokenCount":10,"thoughtsTokenCount":5}}"#).unwrap();
        let parsed = parse_qwen_spend(&path, 0, &PriceBook::default());
        assert_eq!(parsed.entries[0].input, 75);
        assert_eq!(parsed.entries[0].output, 15);
        assert_eq!(parsed.entries[0].message_id.as_deref(), Some("msg-1"));
    }
}
