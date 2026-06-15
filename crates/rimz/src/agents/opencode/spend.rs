//! OpenCode SQLite spend parser.
//!
//! OpenCode stores sessions and messages in one WAL-mode SQLite database under
//! the XDG data root. The dashboard reads it directly through a read-only
//! connection: positive stored `cost` values are authoritative, while zero-cost
//! token rows are priced through Rimz's [`PriceBook`]. Older flat JSON storage
//! is intentionally skipped; OpenCode 1.15's SQLite store is the current source
//! of truth.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;

use crate::agents::pricing::PriceBook;
use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, origin_path, record_unknown_model,
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
    cost: Option<f64>,
    #[serde(rename = "modelID")]
    model_id: Option<String>,
    #[serde(rename = "providerID")]
    provider_id: Option<String>,
    path: Option<MessagePath>,
    time: Option<MessageTime>,
    tokens: Option<MessageTokens>,
}

#[derive(Deserialize)]
struct MessagePath {
    cwd: Option<String>,
    root: Option<String>,
}

#[derive(Deserialize)]
struct MessageTime {
    created: Option<u64>,
}

#[derive(Deserialize)]
struct MessageTokens {
    total: Option<u64>,
    input: Option<u64>,
    output: Option<u64>,
    cache: Option<MessageCache>,
}

#[derive(Deserialize)]
struct MessageCache {
    read: Option<u64>,
    write: Option<u64>,
}

pub(crate) fn parse_opencode_spend(
    path: &Path,
    resume: Option<&SpendCursor>,
    prices: &PriceBook,
) -> SpendParse {
    let from_offset = resume.map_or(0, |cursor| cursor.offset);
    let Some(conn) = open_readonly(path) else {
        return empty_parse(from_offset);
    };
    let sql = if resume.is_some() {
        "SELECT rowid, data FROM message WHERE rowid > ?1 ORDER BY rowid"
    } else {
        "SELECT rowid, data FROM message ORDER BY rowid"
    };
    let Ok(mut stmt) = conn.prepare(sql) else {
        return empty_parse(from_offset);
    };
    let rows_result = match resume {
        Some(_) => stmt.query([from_offset as i64]),
        None => stmt.query([]),
    };
    let Ok(mut rows) = rows_result else {
        return empty_parse(from_offset);
    };

    let mut entries = Vec::new();
    let mut unknown_models = BTreeMap::new();
    let mut max_rowid = from_offset;
    loop {
        let row = match rows.next() {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(_) => break,
        };
        let rowid = row.get::<_, i64>(0).unwrap_or(0);
        if rowid > 0 {
            max_rowid = max_rowid.max(rowid as u64);
        }
        let Ok(data) = row.get::<_, String>(1) else {
            continue;
        };
        if let Some(entry) = parse_message_entry(&data, prices, &mut unknown_models) {
            entries.push(entry);
        }
    }

    SpendParse {
        entries,
        cursor: SpendCursor {
            offset: max_rowid,
            state: None,
        },
        unknown_models,
    }
}

fn empty_parse(offset: u64) -> SpendParse {
    SpendParse {
        entries: Vec::new(),
        cursor: SpendCursor {
            offset,
            state: None,
        },
        unknown_models: BTreeMap::new(),
    }
}

fn parse_message_entry(
    data: &str,
    prices: &PriceBook,
    unknown_models: &mut BTreeMap<String, u64>,
) -> Option<CachedEntry> {
    let message: MessageData = serde_json::from_str(data).ok()?;
    let tokens = message.tokens.as_ref()?;
    let model = non_empty(message.model_id.as_deref())?;
    let provider = non_empty(message.provider_id.as_deref())?;

    let mut input = tokens.input.unwrap_or(0);
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
    if input == 0 && output == 0 && cache_read == 0 && cache_write == 0 {
        output = tokens.total.unwrap_or(0);
        if output == 0 {
            return None;
        }
        input = 0;
    }

    let ts_secs = message
        .time
        .as_ref()
        .and_then(|time| time.created)
        .map(|millis| millis / 1000)
        .unwrap_or(0);
    let mut cost = message.cost.unwrap_or(0.0);
    if cost <= 0.0 {
        cost = price_tokens(
            prices,
            model,
            provider,
            input,
            output,
            cache_read,
            cache_write,
        )
        .unwrap_or_else(|| {
            record_unknown_model(unknown_models, model, ts_secs);
            0.0
        });
    }
    if cost <= 0.0 {
        return None;
    }

    let origin = message
        .path
        .as_ref()
        .and_then(|path| origin_path(path.cwd.as_deref().or(path.root.as_deref())));

    Some(CachedEntry {
        ts_secs,
        cost_usd: cost,
        input,
        output,
        cache_write,
        cache_read,
        message_id: None,
        request_id: None,
        is_sidechain: false,
        model: Some(model.to_owned()),
        origin_path: origin,
    })
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
        let cost = input as f64 * price.input
            + cache_read as f64 * price.cache_read
            + cache_write as f64 * price.cache_create
            + output as f64 * price.output;
        if cost > 0.0 {
            return Some(cost);
        }
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
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, data) VALUES ('msg', 'ses', ?1)",
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
        assert_eq!(entry.origin_path.as_deref(), Some(cwd.as_path()));
        assert_eq!(parsed.cursor.offset, 1);
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
        assert_eq!(parsed.entries.len(), 1);
        let expected = 10.0 * 0.000001 + 30.0 * 0.0000001 + 40.0 * 0.0000005 + 20.0 * 0.000002;
        assert!((parsed.entries[0].cost_usd - expected).abs() < 1e-12);
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
    fn drop_rules_and_rowid_resume_are_stable() {
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
        assert_eq!(first.cursor.offset, 4);

        insert_message(
            &path,
            r#"{"modelID":"gpt-priced","providerID":"openai","tokens":{"input":1,"output":1}}"#,
        );
        let second = parse_opencode_spend(&path, Some(&first.cursor), &prices());
        assert_eq!(second.entries.len(), 1);
        assert_eq!(second.cursor.offset, 5);
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
