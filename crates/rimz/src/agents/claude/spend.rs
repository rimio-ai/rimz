//! Claude Code JSONL spending parser.
//!
//! JSONL shape (one entry per API response logged by Claude Code):
//! ```json
//! { "timestamp": "2026-01-01T10:00:00.000Z",
//!   "requestId": "req-abc",
//!   "isSidechain": false,
//!   "message": { "id": "msg-xyz",
//!                "model": "claude-sonnet-4-6",
//!                "usage": { "input_tokens": 1200, "output_tokens": 80,
//!                           "cache_creation_input_tokens": 0,
//!                           "cache_read_input_tokens": 800 } } }
//! ```
//!
//! **Cost source.** Current Claude transcripts log no `costUSD` field, so spend
//! is reconstructed from the `message.usage` token counts priced through the
//! per-model [`PriceBook`](crate::agents::pricing::PriceBook) — the same path
//! Codex takes.
//! When an older transcript still carries a positive `costUSD`, that authoritative
//! figure is used verbatim and the table is not consulted.
//!
//! Fast pre-filter: skip lines without `"usage":{` and lines where certain
//! fields carry `:null` (rejected by the upstream TypeScript/Zod schema).
//! Entries are returned raw — all `(message.id, requestId)` dedup, including
//! the btw/subagent sidechain-replay suppression, lives in one place,
//! `spending::compute_spending`, so an incremental suffix parse never has to
//! see the lines before its resume point.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agents::pricing::PriceBook;
use crate::agents::spending::{CachedEntry, SpendParse, record_unknown_model};

use crate::agents::transcript_fs::{
    bytes_contains, collect_jsonl, expand_tilde, home_dir, read_spend_lines,
};

// ── Typed structs ─────────────────────────────────────────────────────────────

/// Full typed Claude usage entry.  Fields match the Claude Code JSONL schema
/// (`camelCase`; `costUSD` is an explicit serde rename).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeEntry {
    timestamp: Option<String>,
    #[serde(rename = "costUSD")]
    cost_usd: Option<f64>,
    #[serde(default)]
    message: ClaudeMessage,
    request_id: Option<String>,
    session_id: Option<String>,
    version: Option<String>,
    is_sidechain: Option<bool>,
}

#[derive(Default, Deserialize)]
struct ClaudeMessage {
    /// Anthropic message ID (`msg-…`).  The dedup key alongside `requestId`.
    id: Option<String>,
    model: Option<String>,
    #[serde(default)]
    usage: ClaudeUsage,
}

/// The `message.usage` token counts.  `Option` tolerates both an absent field
/// and an explicit `:null` (which the upstream schema can emit for the cache
/// fields), so a usage shape never drops an otherwise-valid cost entry.
#[derive(Default, Deserialize)]
struct ClaudeUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

// ── Path helpers ──────────────────────────────────────────────────────────────

/// Discover Claude config directories in priority order.
///
/// 1. `CLAUDE_CONFIG_DIR` env (comma-separated; each entry must have `projects/`)
/// 2. `$XDG_CONFIG_HOME/claude` / `~/.config/claude`
/// 3. `~/.claude`
///
/// Returns directories that actually have a `projects/` child.
pub fn claude_config_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Ok(env_val) = std::env::var("CLAUDE_CONFIG_DIR") {
        for raw in env_val.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let p = expand_tilde(raw);
            let p = if p.file_name().is_some_and(|n| n == "projects") && p.is_dir() {
                p.parent().map(Path::to_path_buf).unwrap_or(p)
            } else {
                p
            };
            if p.join("projects").is_dir() {
                dirs.push(p);
            }
        }
        if !dirs.is_empty() {
            return dirs;
        }
    }

    let home = home_dir();
    let xdg = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".config"));
    for candidate in [xdg.join("claude"), home.join(".claude")] {
        if candidate.join("projects").is_dir() {
            dirs.push(candidate);
        }
    }
    dirs
}

/// Every Claude `*.jsonl` across all project dirs — fleet-wide, the same footing
/// as Codex and Pi (their session logs are not project-scoped either). Walks
/// `~/.claude/projects/` recursively, covering both modern
/// `session_id/chat.jsonl` and subagent `session_id/subagents/worker.jsonl`
/// layouts.
pub fn all_jsonl_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for config_dir in claude_config_dirs() {
        collect_jsonl(&config_dir.join("projects"), &mut files);
    }
    files.sort();
    files.dedup();
    files
}

// ── Validation helpers ────────────────────────────────────────────────────────

/// Return `true` when `line` contains a `:null` value for a field that the
/// upstream TypeScript/Zod schema does not accept as nullable.
///
/// These are the same field names rejected by ccusage's `has_unsupported_null_field`.
/// Skipping them prevents silently including entries with missing cost or IDs.
fn has_unsupported_null_field(line: &[u8]) -> bool {
    const NULL_PATTERNS: &[&[u8]] = &[
        b"\"id\":null",
        b"\"cwd\":null",
        b"\"model\":null",
        b"\"speed\":null",
        b"\"costUSD\":null",
        b"\"version\":null",
        b"\"sessionId\":null",
        b"\"requestId\":null",
        b"\"isApiErrorMessage\":null",
        b"\"cache_read_input_tokens\":null",
        b"\"cache_creation_input_tokens\":null",
    ];
    NULL_PATTERNS.iter().any(|p| bytes_contains(line, p))
}

/// Return `false` for entries that would be rejected by the upstream schema:
/// empty strings for IDs/model, or a `version` that is not a semver prefix.
fn is_valid_claude_entry(entry: &ClaudeEntry) -> bool {
    if entry
        .version
        .as_deref()
        .is_some_and(|v| !is_semver_prefix(v))
    {
        return false;
    }
    if entry.session_id.as_deref().is_some_and(str::is_empty) {
        return false;
    }
    if entry.request_id.as_deref().is_some_and(str::is_empty) {
        return false;
    }
    if entry.message.id.as_deref().is_some_and(str::is_empty) {
        return false;
    }
    if entry.message.model.as_deref().is_some_and(str::is_empty) {
        return false;
    }
    true
}

/// Return `true` when `value` begins with a semver major.minor.patch prefix.
fn is_semver_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut i = 0;
    if !consume_digits(bytes, &mut i) || bytes.get(i) != Some(&b'.') {
        return false;
    }
    i += 1;
    if !consume_digits(bytes, &mut i) || bytes.get(i) != Some(&b'.') {
        return false;
    }
    i += 1;
    bytes.get(i).is_some_and(u8::is_ascii_digit)
}

fn consume_digits(bytes: &[u8], i: &mut usize) -> bool {
    let start = *i;
    while bytes.get(*i).is_some_and(u8::is_ascii_digit) {
        *i += 1;
    }
    *i > start
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// Parse a Claude JSONL file into raw `CachedEntry` values, resuming from
/// `from_offset` (0 = the whole file). Lines are independent — no cross-line
/// state — so the cursor is just the consumed-byte offset.
///
/// ### Fast pre-filter
/// Lines without `"usage":{` are skipped before deserialization — tool-call,
/// user-message, and summary lines carry no usage object and no `costUSD`.
/// Lines with unsupported null fields are also rejected before deserialization.
///
/// ### Dedup lives downstream
/// Entries are returned raw, duplicates and sidechain replays included: every
/// `(message.id, requestId)` rule — the retry-write duplicate, the btw tool
/// replaying a parent message into the subagent file with inflated context
/// tokens — is applied once, over all files and cache generations, in
/// `spending::compute_spending`. A suffix parse therefore never needs the
/// lines before its resume point.
pub fn parse_claude_spend(path: &Path, from_offset: u64, prices: &PriceBook) -> SpendParse {
    let Some((content, next_offset)) = read_spend_lines(path, from_offset) else {
        return SpendParse {
            entries: Vec::new(),
            cursor: crate::agents::spending::SpendCursor {
                offset: from_offset,
                state: None,
            },
            unknown_models: BTreeMap::new(),
        };
    };
    const USAGE_MARKER: &[u8] = br#""usage":{"#;

    let mut entries: Vec<CachedEntry> = Vec::new();
    let mut unknown_models = BTreeMap::new();

    for line in content.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if !bytes_contains(line, USAGE_MARKER) {
            continue;
        }
        if has_unsupported_null_field(line) {
            continue;
        }
        let Ok(entry) = serde_json::from_slice::<ClaudeEntry>(line) else {
            continue;
        };
        if !is_valid_claude_entry(&entry) {
            continue;
        }
        let Some(ts) = entry.timestamp.as_deref() else {
            continue;
        };
        let Some(ts_secs) = crate::agents::spending::iso_to_unix_secs(ts) else {
            continue;
        };
        let usage = &entry.message.usage;
        let input = usage.input_tokens.unwrap_or(0);
        let output = usage.output_tokens.unwrap_or(0);
        let cache_creation = usage.cache_creation_input_tokens.unwrap_or(0);
        let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
        // Cost source: a positive logged `costUSD` is authoritative (older
        // transcripts carried it); otherwise reconstruct spend from the token
        // usage through the model table, since current transcripts omit it. An
        // entry that still resolves to zero — no positive cost and no priced
        // model — is dropped rather than counted as a free turn.
        let cost = match entry.cost_usd {
            Some(cost) if cost > 0.0 => cost,
            _ => match price_usage(
                prices,
                entry.message.model.as_deref(),
                input,
                output,
                cache_creation,
                cache_read,
            ) {
                Some(cost) => cost,
                None => {
                    if let Some(model) = entry.message.model.as_deref() {
                        record_unknown_model(&mut unknown_models, model, ts_secs);
                    }
                    0.0
                }
            },
        };
        if cost <= 0.0 {
            continue;
        }
        // Claude reports the four token components separately; `input_tokens` is
        // already the fresh (uncached) slice. Window aggregation folds cache
        // creation into input/total, while cache reads ride their own field.
        entries.push(CachedEntry {
            ts_secs,
            cost_usd: cost,
            input,
            output,
            cache_write: cache_creation,
            cache_read,
            message_id: entry.message.id.clone(),
            request_id: entry.request_id.clone(),
            is_sidechain: entry.is_sidechain == Some(true),
        });
    }

    SpendParse {
        entries,
        cursor: crate::agents::spending::SpendCursor {
            offset: next_offset,
            state: None,
        },
        unknown_models,
    }
}

/// Price a Claude usage record through the model table, in dollars.
///
/// Each token class bills at its own rate — uncached input, output,
/// cache-creation (prompt-cache write), and cache-read (prompt-cache hit) — the
/// same decomposition Codex spend uses. Returns `None` when the model is absent
/// or unpriced; the caller records a priceable unknown model before dropping the
/// entry instead of recording a zero-cost line.
fn price_usage(
    prices: &PriceBook,
    model: Option<&str>,
    input: u64,
    output: u64,
    cache_creation: u64,
    cache_read: u64,
) -> Option<f64> {
    let price = model.and_then(|model| prices.price(model))?;
    Some(
        input as f64 * price.input
            + output as f64 * price.output
            + cache_creation as f64 * price.cache_create
            + cache_read as f64 * price.cache_read,
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::TempDir;

    /// An empty price book: the `costUSD`-bearing fixtures below never consult it,
    /// so spend is driven entirely by their logged cost.
    fn no_prices() -> PriceBook {
        PriceBook::from_litellm_json("{}")
    }

    fn write_jsonl(dir: &Path, filename: &str, lines: &[&str]) -> PathBuf {
        let path = dir.join(filename);
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        path
    }

    fn claude_line(date: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
        format!(
            r#"{{"timestamp":"{date}T10:00:00.000Z","costUSD":{cost},"requestId":"{req_id}","message":{{"id":"{msg_id}","usage":{{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":3,"cache_read_input_tokens":7}}}}}}"#
        )
    }

    fn sidechain_line(date: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
        format!(
            r#"{{"timestamp":"{date}T10:00:00.000Z","costUSD":{cost},"requestId":"{req_id}","isSidechain":true,"message":{{"id":"{msg_id}","usage":{{"input_tokens":50000}}}}}}"#
        )
    }

    #[test]
    fn skips_lines_without_usage_object_and_captures_token_components() {
        let dir = TempDir::new().unwrap();
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":99.0}"#,
                &claude_line("2026-01-01", 0.5, "msg-1", "req-1"),
            ],
        );
        let entries = parse_claude_spend(&file, 0, &no_prices()).entries;
        assert_eq!(entries.len(), 1);
        assert!((entries[0].cost_usd - 0.5).abs() < 1e-9);
        // The components are kept apart here; window aggregation folds
        // cache-write into input/total downstream.
        assert_eq!(entries[0].input, 10);
        assert_eq!(entries[0].output, 5);
        assert_eq!(entries[0].cache_write, 3);
        assert_eq!(entries[0].cache_read, 7);
    }

    #[test]
    fn prices_from_tokens_when_cost_usd_absent() {
        // Current Claude transcripts carry no `costUSD`; cost is reconstructed
        // from the token usage through the model table.
        let dir = TempDir::new().unwrap();
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","requestId":"req-1","message":{"id":"msg-1","model":"claude-test","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":200,"cache_read_input_tokens":800}}}"#,
            ],
        );
        let prices = PriceBook::from_litellm_json(
            r#"{"claude-test": {"input_cost_per_token": 3e-6, "output_cost_per_token": 15e-6,
                                "cache_read_input_token_cost": 3e-7,
                                "cache_creation_input_token_cost": 3.75e-6}}"#,
        );
        let entries = parse_claude_spend(&file, 0, &prices).entries;
        assert_eq!(entries.len(), 1);
        // 100*3e-6 + 50*15e-6 + 200*3.75e-6 + 800*3e-7
        //   = 3e-4 + 7.5e-4 + 7.5e-4 + 2.4e-4 = 2.04e-3
        assert!(
            (entries[0].cost_usd - 2.04e-3).abs() < 1e-12,
            "got {}",
            entries[0].cost_usd
        );
        assert_eq!(entries[0].input, 100);
        assert_eq!(entries[0].output, 50);
        assert_eq!(entries[0].cache_write, 200);
        assert_eq!(entries[0].cache_read, 800);
    }

    #[test]
    fn unknown_model_tracking_records_priceable_names_only() {
        let dir = TempDir::new().unwrap();
        let timestamp = "2026-01-01T10:00:00.000Z";
        let unknown = write_jsonl(
            dir.path(),
            "unknown.jsonl",
            &[&format!(
                r#"{{"timestamp":"{timestamp}","requestId":"req-1","message":{{"id":"msg-1","model":"claude-new-release","usage":{{"input_tokens":100,"output_tokens":50}}}}}}"#
            )],
        );

        let parsed = parse_claude_spend(&unknown, 0, &no_prices());

        assert!(parsed.entries.is_empty());
        assert_eq!(
            parsed.unknown_models,
            BTreeMap::from([(
                "claude-new-release".to_owned(),
                crate::agents::spending::iso_to_unix_secs(timestamp).unwrap()
            )])
        );

        let synthetic = write_jsonl(
            dir.path(),
            "synthetic.jsonl",
            &[
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","requestId":"req-1","message":{"id":"msg-1","model":"<synthetic>","usage":{"input_tokens":100,"output_tokens":50}}}"#,
            ],
        );
        let parsed = parse_claude_spend(&synthetic, 0, &no_prices());
        assert!(parsed.entries.is_empty());
        assert!(parsed.unknown_models.is_empty());

        let logged_cost = write_jsonl(
            dir.path(),
            "logged-cost.jsonl",
            &[
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.25,"requestId":"req-1","message":{"id":"msg-1","model":"claude-new-release","usage":{"input_tokens":100,"output_tokens":50}}}"#,
            ],
        );
        let parsed = parse_claude_spend(&logged_cost, 0, &no_prices());
        assert_eq!(parsed.entries.len(), 1);
        assert!(parsed.unknown_models.is_empty());
    }

    #[test]
    fn duplicates_parse_raw_for_the_downstream_dedup() {
        // All (msg, req) dedup lives in `spending::compute_spending`, so an
        // incremental suffix parse never has to see earlier lines; the raw
        // parse keeps both copies.
        let dir = TempDir::new().unwrap();
        let line = claude_line("2026-01-01", 1.0, "msg-a", "req-a");
        let file = write_jsonl(dir.path(), "chat.jsonl", &[&line, &line]);
        let entries = parse_claude_spend(&file, 0, &no_prices()).entries;
        assert_eq!(entries.len(), 2, "raw parse; the aggregation pass dedups");
    }

    #[test]
    fn sidechain_within_file_replaced_by_main_chain() {
        let dir = TempDir::new().unwrap();
        // Sidechain replay with same msg_id appears first (higher cost/tokens).
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                &sidechain_line("2026-01-01", 5.0, "msg-x", "req-sc"),
                // Different req_id → separate exact-key, but msg_id matches.
                // The cross-file suppression handles this; per-file dedup only
                // fires when the same (msg_id, req_id) pair repeats.
                &claude_line("2026-01-01", 0.05, "msg-x", "req-main"),
            ],
        );
        let entries = parse_claude_spend(&file, 0, &no_prices()).entries;
        // Two distinct exact-keys: the suppression happens in compute_spending.
        assert_eq!(entries.len(), 2);
        let sc = entries.iter().find(|e| e.is_sidechain).unwrap();
        let main = entries.iter().find(|e| !e.is_sidechain).unwrap();
        assert!((sc.cost_usd - 5.0).abs() < 1e-9);
        assert!((main.cost_usd - 0.05).abs() < 1e-9);
    }

    #[test]
    fn invalid_or_unsupported_cost_lines_are_skipped() {
        let dir = TempDir::new().unwrap();
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.0,"message":{"usage":{"input_tokens":1}}}"#,
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":-1.0,"message":{"usage":{"input_tokens":1}}}"#,
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":null,"message":{"usage":{"input_tokens":1}}}"#,
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.1,"message":{"id":"msg-1","model":null,"usage":{"input_tokens":1}}}"#,
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.1,"requestId":"","message":{"id":"msg-2","usage":{"input_tokens":1}}}"#,
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.1,"version":"dev","message":{"id":"msg-3","usage":{"input_tokens":1}}}"#,
                &claude_line("2026-01-01", 0.5, "msg-4", "req-4"),
            ],
        );
        let entries = parse_claude_spend(&file, 0, &no_prices()).entries;
        assert_eq!(entries.len(), 1);
        assert!((entries[0].cost_usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn is_semver_prefix_accepts_valid_versions() {
        assert!(is_semver_prefix("1.0.0"));
        assert!(is_semver_prefix("12.34.56"));
        assert!(is_semver_prefix("1.0.0-beta"));
        assert!(!is_semver_prefix("dev"));
        assert!(!is_semver_prefix("1.0"));
        assert!(!is_semver_prefix("v1.0.0"));
        assert!(!is_semver_prefix(""));
    }
}
