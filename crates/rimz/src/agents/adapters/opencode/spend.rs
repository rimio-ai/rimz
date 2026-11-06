//! OpenCode SQLite spend parser.
//!
//! OpenCode stores sessions and messages in one WAL-mode SQLite database under
//! the XDG data root. The dashboard reads it directly through a read-only
//! connection: positive stored `cost` values are authoritative, while zero-cost
//! token rows are priced through RimZ's [`PriceBook`]. Only OpenCode 2's
//! session-message store is read.
//!
//! Rows are mutable — a streaming assistant message is updated in place when it
//! completes — so each refresh cold-folds the whole table and replaces this
//! database's cache set rather than resuming from a cursor. See [`parse`].

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
#[cfg(test)]
use std::path::PathBuf;

use super::database::{MessageTime, open_readonly};
use crate::agents::pricing::{PriceBook, TokenSplit, scoped_model_key};
use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, SpendUpdate, ToolCounts, origin_path,
    record_unknown_model,
};
use crate::agents::transcript_fs::{
    deserialize_optional_f64_lossy, deserialize_optional_object_lossy,
    deserialize_optional_string_lossy, deserialize_optional_u64_lossy,
};

// ── Parser ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct MessageData {
    #[serde(default, deserialize_with = "deserialize_optional_f64_lossy")]
    cost: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    model: Option<MessageModel>,
    #[serde(default)]
    content: Vec<MessageContent>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    time: Option<MessageTime>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    tokens: Option<MessageTokens>,
}

#[derive(Deserialize)]
struct MessageModel {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    id: Option<String>,
    #[serde(
        rename = "providerID",
        default,
        deserialize_with = "deserialize_optional_string_lossy"
    )]
    provider_id: Option<String>,
}

#[derive(Deserialize)]
struct MessageContent {
    #[serde(
        rename = "type",
        default,
        deserialize_with = "deserialize_optional_string_lossy"
    )]
    kind: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    name: Option<String>,
}

#[derive(Default, Deserialize)]
struct MessageTokens {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    input: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    output: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    reasoning: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    cache: Option<MessageCache>,
}

#[derive(Deserialize)]
struct MessageCache {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    read: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    write: Option<u64>,
}

/// Cold-fold assistant and compaction `session_message` rows each refresh and atomically replace this
/// database's cache set.
///
/// OpenCode rows are mutable: an assistant message is inserted before its
/// response streams, then updated **in place** (same rowid) with tokens and cost
/// when the turn completes. A monotonic rowid/byte resume cursor would advance
/// past such a row while it was still incomplete — priced as nothing — and never
/// revisit it once finalized, silently dropping that turn's spend whenever the
/// database later grew. Like other rewind-prone transcript stores, every
/// refresh reparses the full table and returns `SpendUpdate::Replace`, trading the
/// append-only O(delta) read for correctness against an in-place store. The
/// `resume` cursor is ignored.
pub(super) fn parse(path: &Path, _resume: Option<&SpendCursor>, prices: &PriceBook) -> SpendParse {
    let Some(conn) = open_readonly(path) else {
        return empty_parse(0);
    };
    let mut stmt = match conn
        .prepare("SELECT m.session_id, m.data, s.directory, m.type, m.time_created,
            CASE WHEN m.type = 'compaction' THEN (SELECT json_extract(a.data, '$.model') FROM session_message a
             WHERE a.session_id = m.session_id AND a.type = 'assistant' AND json_valid(a.data)
               AND json_type(a.data, '$.model') = 'object'
             ORDER BY a.time_created DESC, a.rowid DESC LIMIT 1) END
            FROM session_message m LEFT JOIN session_v2 s ON s.id = m.session_id
            WHERE m.type IN ('assistant', 'compaction') ORDER BY m.rowid")
    {
        Ok(stmt) => stmt,
        Err(_) => return empty_parse(0),
    };
    let Ok(mut rows) = stmt.query([]) else {
        return empty_parse(0);
    };

    let mut entries = Vec::new();
    let mut origin = None;
    let mut unknown_models = BTreeMap::new();
    let mut read_error = None;
    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(_) => {
                read_error = Some(std::io::ErrorKind::Other);
                break;
            }
        };
        let thread_id = row
            .get::<_, Option<String>>(0)
            .ok()
            .flatten()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let Ok(data) = row.get::<_, String>(1) else {
            continue;
        };
        let Ok(mut message) = serde_json::from_str::<MessageData>(&data) else {
            continue;
        };
        let compaction = row.get::<_, String>(3).ok().as_deref() == Some("compaction");
        if compaction {
            if message.tokens.is_none() && message.cost.unwrap_or(0.0) <= 0.0 {
                continue;
            }
            if message.model.is_none() {
                message.model = row
                    .get::<_, String>(5)
                    .ok()
                    .and_then(|model| serde_json::from_str(&model).ok());
            }
            message.tokens.get_or_insert_with(MessageTokens::default);
            message.time = Some(MessageTime {
                created: row
                    .get::<_, i64>(4)
                    .ok()
                    .and_then(|value| u64::try_from(value).ok()),
            });
        }
        if let Some(entry) =
            parse_message_entry(message, thread_id, compaction, prices, &mut unknown_models)
        {
            if origin.is_none() {
                origin = row
                    .get::<_, Option<String>>(2)
                    .ok()
                    .flatten()
                    .and_then(|directory| origin_path(Some(&directory)));
            }
            entries.push(entry);
        }
    }

    SpendParse {
        entries,
        origin,
        cursor: SpendCursor::default(),
        unknown_models,
        update: SpendUpdate::Replace,
        read_error,
    }
}

fn empty_parse(offset: u64) -> SpendParse {
    SpendParse {
        entries: Vec::new(),
        origin: None,
        cursor: SpendCursor::new(offset),
        unknown_models: BTreeMap::new(),
        update: SpendUpdate::Extend,
        read_error: Some(std::io::ErrorKind::Other),
    }
}

fn parse_message_entry(
    message: MessageData,
    thread_id: Option<String>,
    allow_cost_only: bool,
    prices: &PriceBook,
    unknown_models: &mut BTreeMap<String, u64>,
) -> Option<CachedEntry> {
    let tokens = message.tokens.as_ref()?;
    let model_info = message.model.as_ref()?;
    let model = non_empty(model_info.id.as_deref())?;
    let provider = non_empty(model_info.provider_id.as_deref())?;

    let input = tokens.input.unwrap_or(0);
    let mut output = tokens
        .output
        .unwrap_or(0)
        .saturating_add(tokens.reasoning.unwrap_or(0));
    let cache_read = tokens
        .cache
        .as_ref()
        .and_then(|cache| cache.read)
        .unwrap_or(0);
    let cache_write = tokens
        .cache
        .as_ref()
        .and_then(|cache| cache.write)
        .unwrap_or(0);
    // `total` is a reported grand total. Fold any excess over the itemized parts
    // into output so it is both counted and priced as output — ccusage's
    // `apply_total_token_fallback` behavior (rimz has no separate extra-total
    // bucket, so the gap rides output rather than an unpriced side counter).
    let known = input
        .saturating_add(output)
        .saturating_add(cache_read)
        .saturating_add(cache_write);
    output = output.saturating_add(tokens.total.unwrap_or(0).saturating_sub(known));
    if input == 0
        && output == 0
        && cache_read == 0
        && cache_write == 0
        && (!allow_cost_only || message.cost.unwrap_or(0.0) <= 0.0)
    {
        return None;
    }

    let ts_secs = message
        .time
        .as_ref()
        .and_then(|time| time.created)
        .map(|millis| millis / 1000)
        .unwrap_or(0);
    let split = TokenSplit::new(input, output).cached(cache_write, cache_read);
    let mut cost = message.cost.unwrap_or(0.0);
    if cost <= 0.0 {
        cost = match price_tokens(prices, model, provider, split) {
            Some(cost) => cost,
            None => {
                for candidate in model_candidates(model, provider) {
                    record_unknown_model(unknown_models, &candidate, ts_secs);
                }
                0.0
            }
        };
    }

    let mut tool_calls = ToolCounts::default();
    for part in message.content {
        if part.kind.as_deref() != Some("tool") {
            continue;
        }
        if let Some(name) = non_empty(part.name.as_deref()) {
            tool_calls.increment(name);
        }
    }
    Some(CachedEntry {
        thread_id: thread_id.map(String::into_boxed_str),
        model: Some(model.into()),
        tool_calls,
        ..CachedEntry::new(ts_secs, cost, &split)
    })
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn price_tokens(prices: &PriceBook, model: &str, provider: &str, split: TokenSplit) -> Option<f64> {
    let candidates = model_candidates(model, provider);
    candidates
        .iter()
        .find_map(|candidate| prices.exact_price(candidate))
        .or_else(|| {
            candidates
                .iter()
                .find_map(|candidate| prices.price(candidate))
        })
        .map(|price| price.cost_of(split))
}

fn model_candidates(model: &str, provider: &str) -> Vec<String> {
    let mut out = Vec::new();
    push_unique(&mut out, model.trim().to_owned());
    let lower = model.trim().to_ascii_lowercase();
    match lower.as_str() {
        "gemini-3-pro-high" => push_unique(&mut out, "gemini-3-pro-preview".to_owned()),
        "k2p6" => push_unique(&mut out, "kimi-k2.6".to_owned()),
        _ => {}
    }
    if let Some(normalized) = claude_dotted_normalized(&lower) {
        push_unique(&mut out, normalized);
    }

    let provider = provider.trim();
    if provider.is_empty() || provider == "unknown" {
        return out;
    }
    let mut scoped = out
        .iter()
        .map(|base| scoped_model_key(provider, base))
        .collect::<Vec<_>>();
    scoped.extend(out);
    scoped
}

fn claude_dotted_normalized(lower: &str) -> Option<String> {
    if !(lower.starts_with("claude-haiku-")
        || lower.starts_with("claude-opus-")
        || lower.starts_with("claude-sonnet-"))
    {
        return None;
    }
    lower.contains('.').then(|| lower.replace('.', "-"))
}

fn push_unique(out: &mut Vec<String>, value: String) {
    if !value.is_empty() && !out.contains(&value) {
        out.push(value);
    }
}

#[cfg(test)]
#[path = "spend/tests.rs"]
mod tests;
