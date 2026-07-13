//! OpenCode SQLite spend parser.
//!
//! OpenCode stores sessions and messages in one WAL-mode SQLite database under
//! the XDG data root. The dashboard reads it directly through a read-only
//! connection: positive stored `cost` values are authoritative, while zero-cost
//! token rows are priced through Rimz's [`PriceBook`]. Older flat JSON disk_usage
//! is intentionally skipped; OpenCode 1.15's SQLite store is the current source
//! of truth.
//!
//! Rows are mutable — a streaming assistant message is updated in place when it
//! completes — so each refresh cold-folds the whole table and replaces this
//! database's cache set rather than resuming from a cursor. See
//! [`parse_opencode_spend`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;

use crate::agents::pricing::PriceBook;
use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, origin_path, record_unknown_model,
};
use crate::agents::transcript_fs::{
    deserialize_optional_f64_lossy, deserialize_optional_object_lossy,
    deserialize_optional_string_lossy, deserialize_optional_u64_lossy,
};

// ── Discovery ────────────────────────────────────────────────────────────────

pub(crate) fn opencode_db_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in opencode_data_dirs() {
        if let Some(path) = db_file_in_dir(&dir) {
            files.push(path);
        }
    }
    files.sort();
    files.dedup();
    files
}

pub(crate) fn opencode_data_dirs() -> Vec<PathBuf> {
    if let Ok(env_val) = std::env::var("RIMZ_OPENCODE_DATA_DIR") {
        return env_val
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .collect();
    }

    let data_home = std::env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".local/share"))
        })
        .unwrap_or_else(|| PathBuf::from("/").join(".local/share"));
    vec![data_home.join("opencode")]
}

fn db_file_in_dir(dir: &Path) -> Option<PathBuf> {
    let primary = dir.join("opencode.db");
    if primary.is_file() {
        return Some(primary);
    }

    let mut candidates = Vec::new();
    let rd = std::fs::read_dir(dir).ok()?;
    for entry in rd.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_channel_db_name)
        {
            candidates.push(path);
        }
    }
    candidates.sort();
    candidates.into_iter().next()
}

fn is_channel_db_name(name: &str) -> bool {
    let Some(channel) = name
        .strip_prefix("opencode-")
        .and_then(|rest| rest.strip_suffix(".db"))
    else {
        return false;
    };
    !channel.is_empty()
        && channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(crate) fn open_readonly(path: &Path) -> Option<Connection> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .ok()
}

// ── Parser ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct MessageData {
    #[serde(default, deserialize_with = "deserialize_optional_f64_lossy")]
    cost: Option<f64>,
    #[serde(
        rename = "modelID",
        default,
        deserialize_with = "deserialize_optional_string_lossy"
    )]
    model_id: Option<String>,
    #[serde(
        rename = "providerID",
        default,
        deserialize_with = "deserialize_optional_string_lossy"
    )]
    provider_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    path: Option<MessagePath>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    time: Option<MessageTime>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    tokens: Option<MessageTokens>,
}

#[derive(Deserialize)]
struct MessagePath {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    cwd: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    root: Option<String>,
}

#[derive(Deserialize)]
struct MessageTime {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    created: Option<u64>,
}

#[derive(Deserialize)]
struct MessageTokens {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    input: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    output: Option<u64>,
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

/// Cold-fold the whole `message` table each refresh and atomically replace this
/// database's cache set.
///
/// OpenCode rows are mutable: an assistant message is inserted before its
/// response streams, then updated **in place** (same rowid) with tokens and cost
/// when the turn completes. A monotonic rowid/byte resume cursor would advance
/// past such a row while it was still incomplete — priced as nothing — and never
/// revisit it once finalized, silently dropping that turn's spend whenever the
/// database later grew. So, like gemini's rewind-prone transcripts, every
/// refresh reparses the full table and returns `replace_entries`, trading the
/// append-only O(delta) read for correctness against an in-place store. The
/// `resume` cursor is ignored.
pub(crate) fn parse_opencode_spend(
    path: &Path,
    _resume: Option<&SpendCursor>,
    prices: &PriceBook,
) -> SpendParse {
    let Some(conn) = open_readonly(path) else {
        return empty_parse(0);
    };
    let mut stmt = match conn
        .prepare("SELECT session_id, data FROM message ORDER BY rowid")
        .or_else(|_| conn.prepare("SELECT NULL, data FROM message ORDER BY rowid"))
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
        if let Some((entry, entry_origin)) =
            parse_message_entry(&data, thread_id, prices, &mut unknown_models)
        {
            if origin.is_none() {
                origin = entry_origin;
            }
            entries.push(entry);
        }
    }

    SpendParse {
        entries,
        origin,
        cursor: SpendCursor::default(),
        unknown_models,
        replace_entries: true,
    }
}

fn empty_parse(offset: u64) -> SpendParse {
    SpendParse {
        entries: Vec::new(),
        origin: None,
        cursor: SpendCursor {
            offset,
            state: None,
        },
        unknown_models: BTreeMap::new(),
        replace_entries: false,
    }
}

fn parse_message_entry(
    data: &str,
    thread_id: Option<String>,
    prices: &PriceBook,
    unknown_models: &mut BTreeMap<String, u64>,
) -> Option<(CachedEntry, Option<PathBuf>)> {
    let message: MessageData = serde_json::from_str(data).ok()?;
    let tokens = message.tokens.as_ref()?;
    let model = non_empty(message.model_id.as_deref())?;
    let provider = non_empty(message.provider_id.as_deref())?;

    let input = tokens.input.unwrap_or(0);
    let mut output = tokens.output.unwrap_or(0);
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
    if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 {
        return None;
    }

    let ts_secs = message
        .time
        .as_ref()
        .and_then(|time| time.created)
        .map(|millis| millis / 1000)
        .unwrap_or(0);
    let mut cost = message.cost.unwrap_or(0.0);
    if cost <= 0.0 {
        cost = match price_tokens(
            prices,
            model,
            provider,
            input,
            output,
            cache_read,
            cache_write,
        ) {
            Some(cost) => cost,
            None => {
                record_unknown_model(unknown_models, model, ts_secs);
                0.0
            }
        };
    }

    let origin = message
        .path
        .as_ref()
        .and_then(|path| origin_path(path.cwd.as_deref().or(path.root.as_deref())));

    Some((
        CachedEntry {
            ts_secs,
            cost_usd: cost,
            input,
            output,
            cache_write,
            cache_read,
            message_id: None,
            request_id: None,
            dedup_key: None,
            thread_id,
            is_sidechain: false,
            has_speed: false,
            model: Some(model.to_owned()),
            rolled: false,
        },
        origin,
    ))
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn price_tokens(
    prices: &PriceBook,
    model: &str,
    provider: &str,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
) -> Option<f64> {
    for candidate in model_candidates(model, provider) {
        let Some(price) = prices.price(&candidate) else {
            continue;
        };
        let cost = price.cost(input, output, cache_write, 0, cache_read, false);
        return Some(cost);
    }
    None
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

// ── Account helper ───────────────────────────────────────────────────────────

pub(crate) fn latest_message_provider() -> Option<String> {
    opencode_db_files()
        .into_iter()
        .filter_map(|path| latest_provider_in_db(&path))
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, provider)| provider)
}

fn latest_provider_in_db(path: &Path) -> Option<(u64, String)> {
    let conn = open_readonly(path)?;
    let mut stmt = conn
        .prepare("SELECT data FROM message ORDER BY rowid DESC LIMIT 100")
        .ok()?;
    let mut rows = stmt.query([]).ok()?;
    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(_) => break,
        };
        let Ok(data) = row.get::<_, String>(0) else {
            continue;
        };
        let Ok(message) = serde_json::from_str::<MessageData>(&data) else {
            continue;
        };
        let Some(provider) = non_empty(message.provider_id.as_deref()) else {
            continue;
        };
        let provider = provider.to_owned();
        let ts = message
            .time
            .as_ref()
            .and_then(|time| time.created)
            .unwrap_or(0);
        return Some((ts, provider));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_db(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)",
            [],
        )
        .unwrap();
        path
    }

    fn insert_message(path: &Path, data: &str) {
        insert_message_for_session(path, "ses", data);
    }

    fn insert_message_for_session(path: &Path, session_id: &str, data: &str) {
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('msg', ?1, ?2)",
            (session_id, data),
        )
        .unwrap();
    }

    /// Rewrite the newest row's `data` in place, as OpenCode does when a
    /// streaming assistant message completes.
    fn update_last_message(path: &Path, data: &str) {
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "UPDATE message SET data = ?1 WHERE rowid = (SELECT MAX(rowid) FROM message)",
            [data],
        )
        .unwrap();
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
    fn discovery_prefers_primary_then_sorted_channel_db() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("opencode-beta.db"), "").unwrap();
        std::fs::write(dir.path().join("opencode-alpha.db"), "").unwrap();
        assert_eq!(
            db_file_in_dir(dir.path()).unwrap().file_name().unwrap(),
            "opencode-alpha.db"
        );
        std::fs::write(dir.path().join("opencode.db"), "").unwrap();
        assert_eq!(
            db_file_in_dir(dir.path()).unwrap().file_name().unwrap(),
            "opencode.db"
        );
        std::fs::write(dir.path().join("opencode-bad!.db"), "").unwrap();
        assert!(!is_channel_db_name("opencode-bad!.db"));
    }

    #[test]
    fn parses_stored_cost_tokens_timestamp_and_origin() {
        let dir = TempDir::new().unwrap();
        let cwd = dir.path().join("repo");
        let path = create_db(dir.path(), "opencode.db");
        insert_message(
            &path,
            &format!(
                r#"{{
                    "cost": 0.42,
                    "modelID": "gpt-priced",
                    "providerID": "openai",
                    "path": {{ "cwd": "{}" }},
                    "time": {{ "created": 1780590149011 }},
                    "tokens": {{
                        "input": 100,
                        "output": 20,
                        "cache": {{ "read": 30, "write": 40 }}
                    }}
                }}"#,
                cwd.display()
            ),
        );

        let parsed = parse_opencode_spend(&path, None, &prices());
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
    fn prices_zero_cost_rows_and_records_unknown_models() {
        let dir = TempDir::new().unwrap();
        let path = create_db(dir.path(), "opencode.db");
        insert_message(
            &path,
            r#"{
                "cost": 0,
                "modelID": "gpt-priced",
                "providerID": "openai",
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
                "modelID": "unknown-future",
                "providerID": "openai",
                "time": { "created": 3000 },
                "tokens": { "input": 10, "output": 20 }
            }"#,
        );

        let parsed = parse_opencode_spend(&path, None, &prices());
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
                "modelID": "gpt-provider",
                "providerID": "openai",
                "tokens": { "total": 50 }
            }"#,
        );
        insert_message(
            &path,
            r#"{
                "modelID": "claude-sonnet-4.5",
                "providerID": "anthropic",
                "tokens": { "input": 10, "output": 10 }
            }"#,
        );

        let parsed = parse_opencode_spend(&path, None, &prices());
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
                "modelID": "gpt-priced",
                "providerID": "openai",
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
                "modelID": "gpt-priced",
                "providerID": "openai",
                "path": { "cwd": 42 },
                "time": "unknown",
                "tokens": {
                    "input": true,
                    "output": 5,
                    "cache": 0
                }
            }"#,
        );

        let parsed = parse_opencode_spend(&path, None, &prices());
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
            r#"{"modelID":"gpt-priced","providerID":"openai"}"#,
            r#"{"modelID":"gpt-priced","providerID":"openai","tokens":{"input":0}}"#,
            r#"{"providerID":"openai","tokens":{"input":1}}"#,
            r#"{"modelID":"gpt-priced","tokens":{"input":1}}"#,
        ] {
            insert_message(&path, data);
        }
        let first = parse_opencode_spend(&path, None, &prices());
        assert!(first.entries.is_empty());
        // A mutable store is cold-folded whole: no resume cursor, and the fold is
        // authoritative for the file so it replaces the cache set.
        assert!(first.replace_entries);
        assert_eq!(first.cursor, SpendCursor::default());

        insert_message(
            &path,
            r#"{"modelID":"gpt-priced","providerID":"openai","tokens":{"input":1,"output":1}}"#,
        );
        let second = parse_opencode_spend(&path, Some(&first.cursor), &prices());
        assert_eq!(second.entries.len(), 1);
        assert!(second.replace_entries);
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
            r#"{"role":"assistant","modelID":"gpt-priced","providerID":"openai"}"#,
        );
        let mid_stream = parse_opencode_spend(&path, None, &prices());
        assert!(mid_stream.entries.is_empty());

        // The turn completes: the same row is rewritten with tokens+cost, and a
        // later row lands so the database grows — the condition that used to pick
        // the lossy resume path.
        update_last_message(
            &path,
            r#"{"role":"assistant","modelID":"gpt-priced","providerID":"openai","cost":0.5,"tokens":{"input":10,"output":5}}"#,
        );
        insert_message(
            &path,
            r#"{"role":"assistant","modelID":"gpt-priced","providerID":"openai","tokens":{"input":1,"output":1}}"#,
        );
        let completed = parse_opencode_spend(&path, Some(&mid_stream.cursor), &prices());
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
            r#"{"modelID":"gpt-priced","providerID":"openai","tokens":{"input":100,"output":20,"cache":{"read":30},"total":200}}"#,
        );
        let parsed = parse_opencode_spend(&path, None, &prices());
        assert_eq!(parsed.entries.len(), 1);
        let entry = &parsed.entries[0];
        // missing = 200 - (100 + 20 + 30) = 50 → output = 20 + 50 = 70.
        assert_eq!((entry.input, entry.output, entry.cache_read), (100, 70, 30));
        let expected = 100.0 * 0.000001 + 70.0 * 0.000002 + 30.0 * 0.0000001;
        assert!((entry.cost_usd - expected).abs() < 1e-12);
    }

    #[test]
    fn latest_provider_reads_newest_message() {
        let dir = TempDir::new().unwrap();
        let path = create_db(dir.path(), "opencode.db");
        insert_message(
            &path,
            r#"{"providerID":"anthropic","modelID":"claude","tokens":{"input":1},"time":{"created":1000}}"#,
        );
        insert_message(
            &path,
            r#"{"providerID":"openai","modelID":"gpt","tokens":{"input":1},"time":{"created":2000}}"#,
        );
        assert_eq!(
            latest_provider_in_db(&path).map(|(_, provider)| provider),
            Some("openai".to_owned())
        );
    }
}
