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
use crate::agents::pricing::{PriceBook, TokenSplit};
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
    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(_) => break,
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
    }
}

fn empty_parse(offset: u64) -> SpendParse {
    SpendParse {
        entries: Vec::new(),
        origin: None,
        cursor: SpendCursor::new(offset),
        unknown_models: BTreeMap::new(),
        update: SpendUpdate::Extend,
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
                record_unknown_model(unknown_models, model, ts_secs);
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
    model_candidates(model, provider)
        .into_iter()
        .find_map(|candidate| prices.price(&candidate))
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
    if !provider.is_empty() && provider != "unknown" {
        let prefix = provider.replace('-', "_");
        for base in out.clone() {
            push_unique(&mut out, format!("{prefix}/{base}"));
        }
    }
    out
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
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn compaction_rows_contribute_usage_with_own_or_latest_assistant_model() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("opencode.db");
        let db = crate::testkit::opencode::OpenCodeDb::new(&path, false, false);
        db.session("ses", dir.path());
        db.message(
            "a",
            "ses",
            "assistant",
            1000,
            r#"{"model":{"id":"gpt-priced","providerID":"openai"}}"#,
        );
        db.message("c", "ses", "compaction", 2000, r#"{"status":"completed","cost":0.5,"model":{"id":"gpt-provider","providerID":"openai"},"tokens":{"input":10,"output":2,"reasoning":3}}"#);
        db.message(
            "f",
            "ses",
            "compaction",
            3000,
            r#"{"status":"failed","cost":0,"tokens":{"input":20,"output":4}}"#,
        );
        db.message(
            "cost",
            "ses",
            "compaction",
            4000,
            r#"{"status":"failed","cost":0.25}"#,
        );
        db.message(
            "running",
            "ses",
            "compaction",
            5000,
            r#"{"status":"running"}"#,
        );
        let parsed = parse(&path, None, &prices());
        assert_eq!(parsed.entries.len(), 3);
        let entry = &parsed.entries[0];
        assert_eq!((entry.input, entry.output, entry.ts_secs), (10, 5, 2));
        assert_eq!(entry.cost_usd, 0.5);
        assert_eq!(entry.model.as_deref(), Some("gpt-provider"));
        assert_eq!(entry.thread_id.as_deref(), Some("ses"));
        assert_eq!(parsed.entries[1].model.as_deref(), Some("gpt-priced"));
        assert!((parsed.entries[1].cost_usd - 0.000028).abs() < 1e-12);
        assert_eq!(parsed.entries[2].cost_usd, 0.25);
        assert!(
            super::super::transcript::read_messages(
                &path,
                Some(&crate::ids::AgentSessionId::from("ses"))
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn v2_rows_price_reasoning_and_count_inline_tools() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("opencode.db");
        let db = crate::testkit::opencode::OpenCodeDb::new(&path, false, false);
        db.session("ses", dir.path());
        let data = r#"{"model":{"id":"gpt-priced","providerID":"openai"},"cost":0,"time":{"created":2000},"tokens":{"input":10,"output":20,"reasoning":5,"cache":{"read":30,"write":40}},"content":[{"type":"tool","name":"read"},{"type":"tool","name":"read"},{"type":"text","text":"answer"}]}"#;
        db.message("assistant", "ses", "assistant", 2000, data);
        db.message("user", "ses", "user", 3000, data);
        let parsed = parse(&path, None, &prices());
        assert_eq!(parsed.entries.len(), 1);
        let entry = &parsed.entries[0];
        assert_eq!(
            (
                entry.input,
                entry.output,
                entry.cache_read,
                entry.cache_write
            ),
            (10, 25, 30, 40)
        );
        assert!(
            (entry.cost_usd
                - (10.0 * 0.000001 + 25.0 * 0.000002 + 30.0 * 0.0000001 + 40.0 * 0.0000005))
                .abs()
                < 1e-12
        );
        assert_eq!(entry.tool_calls.get("read"), Some(&2));
        assert_eq!(entry.ts_secs, 2);
        assert_eq!(parsed.origin.as_deref(), Some(dir.path()));
    }

    fn create_db(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        crate::testkit::opencode::OpenCodeDb::new(&path, false, false);
        path
    }

    fn insert_message(path: &Path, data: &str) {
        insert_message_for_session(path, "ses", data);
    }

    fn insert_message_for_session(path: &Path, session_id: &str, data: &str) {
        let db = crate::testkit::opencode::OpenCodeDb::new(path, false, false);
        let index: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM session_message", [], |row| row.get(0))
            .unwrap();
        db.message(
            &format!("msg-{index}"),
            session_id,
            "assistant",
            index,
            data,
        );
    }

    /// Rewrite the newest row's `data` in place, as OpenCode does when a
    /// streaming assistant message completes.
    fn update_last_message(path: &Path, data: &str) {
        let db = crate::testkit::opencode::OpenCodeDb::new(path, false, false);
        let id: String = db
            .connection()
            .query_row(
                "SELECT id FROM session_message ORDER BY rowid DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        db.update_message(&id, 0, data);
    }

    fn prices() -> PriceBook {
        PriceBook::from_litellm_json(
            r#"{
                "gpt-priced": {
                    "input_cost_per_token": 0.000001,
                    "output_cost_per_token": 0.000002,
                    "cache_read_input_token_cost": 0.0000001,
                    "cache_creation_input_token_cost": 0.0000005
                },
                "openai/gpt-provider": {
                    "input_cost_per_token": 0.000003,
                    "output_cost_per_token": 0.000004
                },
                "claude-sonnet-4-5": {
                    "input_cost_per_token": 0.000005,
                    "output_cost_per_token": 0.000006
                }
            }"#,
        )
    }

    #[test]
    fn parses_stored_cost_tokens_timestamp_and_origin() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("repo");
        let path = create_db(dir.path(), "opencode.db");
        crate::testkit::opencode::OpenCodeDb::new(&path, false, false).session("ses", &cwd);
        insert_message(
            &path,
            r#"{
                    "cost": 0.42,
                    "model":{"id":"gpt-priced","providerID":"openai"},
                    "time": { "created": 1780590149011 },
                    "tokens": {
                        "input": 100,
                        "output": 20,
                        "cache": { "read": 30, "write": 40 }
                    }
                }"#,
        );

        let parsed = parse(&path, None, &prices());
        assert_eq!(parsed.entries.len(), 1);
        let entry = &parsed.entries[0];
        assert!((entry.cost_usd - 0.42).abs() < 1e-9);
        assert_eq!(entry.input, 100);
        assert_eq!(entry.output, 20);
        assert_eq!(entry.cache_read, 30);
        assert_eq!(entry.cache_write, 40);
        assert_eq!(entry.ts_secs, 1_780_590_149);
        assert_eq!(entry.thread_id.as_deref(), Some("ses"));
        assert_eq!(parsed.origin.as_deref(), Some(cwd.as_path()));
        assert_eq!(parsed.cursor, SpendCursor::default());
    }

    #[test]
    fn attaches_tool_content_to_its_message_entry() {
        let dir = TempDir::new().unwrap();
        let path = create_db(dir.path(), "opencode.db");
        insert_message(
            &path,
            r#"{
                "cost": 0.42,
                "model":{"id":"gpt-priced","providerID":"openai"},
                "time": { "created": 1780590149011 },
                "tokens": { "input": 100, "output": 20 },
                "content": [{"type":"tool","name":"bash"},{"type":"tool","name":"read"},{"type":"tool","name":"read"}]
            }"#,
        );

        let parsed = parse(&path, None, &prices());

        assert_eq!(
            parsed.entries[0].tool_calls,
            ToolCounts::from(BTreeMap::from([
                ("bash".to_owned(), 1),
                ("read".to_owned(), 2)
            ]))
        );
    }

    #[test]
    fn prices_zero_cost_rows_and_records_unknown_models() {
        let dir = TempDir::new().unwrap();
        let path = create_db(dir.path(), "opencode.db");
        insert_message(
            &path,
            r#"{
                "cost": 0,
                "model":{"id":"gpt-priced","providerID":"openai"},
                "time": { "created": 2000 },
                "tokens": {
                    "input": 10,
                    "output": 20,
                    "cache": { "read": 30, "write": 40 }
                }
            }"#,
        );
        insert_message(
            &path,
            r#"{
                "cost": 0,
                "model":{"id":"unknown-future","providerID":"openai"},
                "time": { "created": 3000 },
                "tokens": { "input": 10, "output": 20 }
            }"#,
        );

        let parsed = parse(&path, None, &prices());
        assert_eq!(parsed.entries.len(), 2);
        let expected = 10.0 * 0.000001 + 30.0 * 0.0000001 + 40.0 * 0.0000005 + 20.0 * 0.000002;
        assert!((parsed.entries[0].cost_usd - expected).abs() < 1e-12);
        assert_eq!(parsed.entries[1].cost_usd, 0.0);
        assert_eq!(parsed.entries[1].input, 10);
        assert_eq!(parsed.entries[1].output, 20);
        assert_eq!(parsed.unknown_models.get("unknown-future"), Some(&3));
    }

    #[test]
    fn total_fallback_and_candidate_resolution_work() {
        let dir = TempDir::new().unwrap();
        let path = create_db(dir.path(), "opencode.db");
        insert_message(
            &path,
            r#"{
                "model":{"id":"gpt-provider","providerID":"openai"},
                "tokens": { "total": 50 }
            }"#,
        );
        insert_message(
            &path,
            r#"{
                "model":{"id":"claude-sonnet-4.5","providerID":"anthropic"},
                "tokens": { "input": 10, "output": 10 }
            }"#,
        );

        let parsed = parse(&path, None, &prices());
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].output, 50);
        assert!((parsed.entries[0].cost_usd - 50.0 * 0.000004).abs() < 1e-12);
        assert!((parsed.entries[1].cost_usd - (10.0 * 0.000005 + 10.0 * 0.000006)).abs() < 1e-12);
    }

    #[test]
    fn parses_numeric_strings_and_ignores_malformed_nested_values() {
        let dir = TempDir::new().unwrap();
        let path = create_db(dir.path(), "opencode.db");
        insert_message(
            &path,
            r#"{
                "cost": "0.42",
                "model":{"id":"gpt-priced","providerID":"openai"},
                "time": { "created": "1780590149011" },
                "tokens": {
                    "input": "100",
                    "output": "20",
                    "cache": { "read": "30", "write": "40" }
                }
            }"#,
        );
        insert_message(
            &path,
            r#"{
                "cost": 0.21,
                "model":{"id":"gpt-priced","providerID":"openai"},
                "time": "unknown",
                "tokens": {
                    "input": true,
                    "output": 5,
                    "cache": 0
                }
            }"#,
        );

        let parsed = parse(&path, None, &prices());
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(
            (
                parsed.entries[0].input,
                parsed.entries[0].output,
                parsed.entries[0].cache_read,
                parsed.entries[0].cache_write,
            ),
            (100, 20, 30, 40)
        );
        assert_eq!(parsed.entries[0].ts_secs, 1_780_590_149);
        assert!((parsed.entries[0].cost_usd - 0.42).abs() < 1e-9);
        assert_eq!((parsed.entries[1].input, parsed.entries[1].output), (0, 5));
        assert_eq!(parsed.entries[1].ts_secs, 0);
        assert!((parsed.entries[1].cost_usd - 0.21).abs() < 1e-9);
    }

    #[test]
    fn drop_rules_hold_and_each_refresh_replaces_the_cache_set() {
        let dir = TempDir::new().unwrap();
        let path = create_db(dir.path(), "opencode.db");
        for data in [
            r#"{"model":{"id":"gpt-priced","providerID":"openai"}}"#,
            r#"{"model":{"id":"gpt-priced","providerID":"openai"},"tokens":{"input":0}}"#,
            r#"{"model":{"providerID":"openai"},"tokens":{"input":1}}"#,
            r#"{"model":{"id":"gpt-priced"},"tokens":{"input":1}}"#,
        ] {
            insert_message(&path, data);
        }
        let first = parse(&path, None, &prices());
        assert!(first.entries.is_empty());
        // A mutable store is cold-folded whole: no resume cursor, and the fold is
        // authoritative for the file so it replaces the cache set.
        assert_eq!(first.update, SpendUpdate::Replace);
        assert_eq!(first.cursor, SpendCursor::default());

        insert_message(
            &path,
            r#"{"model":{"id":"gpt-priced","providerID":"openai"},"tokens":{"input":1,"output":1}}"#,
        );
        let second = parse(&path, Some(&first.cursor), &prices());
        assert_eq!(second.entries.len(), 1);
        assert_eq!(second.update, SpendUpdate::Replace);
    }

    #[test]
    fn in_place_completion_is_counted_after_the_row_updates() {
        // Guards the resumed-spend data loss: an assistant row is inserted while
        // streaming (no tokens yet), then updated in place with tokens and cost
        // when the turn completes. A rowid resume cursor would have advanced past
        // the incomplete row and skipped its finalized cost forever; the full
        // reparse counts it.
        let dir = TempDir::new().unwrap();
        let path = create_db(dir.path(), "opencode.db");
        insert_message(
            &path,
            r#"{"model":{"id":"gpt-priced","providerID":"openai"}}"#,
        );
        let mid_stream = parse(&path, None, &prices());
        assert!(mid_stream.entries.is_empty());

        // The turn completes: the same row is rewritten with tokens+cost, and a
        // later row lands so the database grows — the condition that used to pick
        // the lossy resume path.
        update_last_message(
            &path,
            r#"{"model":{"id":"gpt-priced","providerID":"openai"},"cost":0.5,"tokens":{"input":10,"output":5}}"#,
        );
        insert_message(
            &path,
            r#"{"model":{"id":"gpt-priced","providerID":"openai"},"tokens":{"input":1,"output":1}}"#,
        );
        let completed = parse(&path, Some(&mid_stream.cursor), &prices());
        assert_eq!(completed.entries.len(), 2);
        assert!((completed.entries[0].cost_usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn total_token_gap_folds_into_output_when_parts_are_nonzero() {
        // `total` exceeds the itemized parts while output is already nonzero: the
        // excess is counted and priced as output rather than silently dropped.
        let dir = TempDir::new().unwrap();
        let path = create_db(dir.path(), "opencode.db");
        insert_message(
            &path,
            r#"{"model":{"id":"gpt-priced","providerID":"openai"},"tokens":{"input":100,"output":20,"cache":{"read":30},"total":200}}"#,
        );
        let parsed = parse(&path, None, &prices());
        assert_eq!(parsed.entries.len(), 1);
        let entry = &parsed.entries[0];
        // missing = 200 - (100 + 20 + 30) = 50 → output = 20 + 50 = 70.
        assert_eq!((entry.input, entry.output, entry.cache_read), (100, 70, 30));
        let expected = 100.0 * 0.000001 + 70.0 * 0.000002 + 30.0 * 0.0000001;
        assert!((entry.cost_usd - expected).abs() < 1e-12);
    }
}
