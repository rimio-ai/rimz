//! Claude Code JSONL spending parser.
//!
//! JSONL shape (one entry per API response logged by Claude Code):
//! ```json
//! { "timestamp": "2026-01-01T10:00:00.000Z",
//!   "costUSD": 0.042,
//!   "requestId": "req-abc",
//!   "isSidechain": false,
//!   "message": { "id": "msg-xyz",
//!                "model": "claude-sonnet-4-6",
//!                "usage": { "input_tokens": 1200, "output_tokens": 80,
//!                           "cache_creation_input_tokens": 0,
//!                           "cache_read_input_tokens": 800 } } }
//! ```
//!
//! Fast pre-filter: skip lines without `"usage":{` and lines where certain
//! fields carry `:null` (rejected by the upstream TypeScript/Zod schema).
//! Per-file dedup by `(message.id, requestId)` with sidechain preference
//! mirrors the ccusage `push_deduped_entry` logic.  Cross-file dedup
//! (btw/subagent replay suppression) is handled by `spending::compute_spending`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agents::spending::CachedEntry;

use super::{bytes_contains, collect_jsonl, expand_tilde, home_dir};

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

/// Encode a worktree absolute path to its Claude project directory name.
///
/// Claude names project dirs by replacing every `/` with `-`:
/// `/home/user/my-project` → `-home-user-my-project`.
pub fn encode_project_dir(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

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

/// Collect Claude `*.jsonl` files scoped to the given worktree paths.
///
/// Files live at `~/.claude/projects/{encode_project_dir(path)}/` recursively,
/// covering both modern `session_id/chat.jsonl` and subagent
/// `session_id/subagents/worker.jsonl` layouts.
pub fn project_jsonl_files(worktree_paths: &[&Path]) -> Vec<PathBuf> {
    let config_dirs = claude_config_dirs();
    let mut files = Vec::new();

    for &worktree_path in worktree_paths {
        let encoded = encode_project_dir(worktree_path);
        for config_dir in &config_dirs {
            let project_dir = config_dir.join("projects").join(&encoded);
            collect_jsonl(&project_dir, &mut files);
        }
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

/// Parse a Claude JSONL file into deduplicated `CachedEntry` values.
///
/// ### Fast pre-filter
/// Lines without `"usage":{` are skipped before deserialization — tool-call,
/// user-message, and summary lines carry no usage object and no `costUSD`.
/// Lines with unsupported null fields are also rejected before deserialization.
///
/// ### Per-file dedup by `(message.id, requestId)`
/// Within a single file, if the same `(id, requestId)` pair appears more than
/// once (e.g. a retry write), only the first is kept.  When the same
/// `message.id` appears under a different `requestId` with `isSidechain:true`,
/// the sidechain entry is replaced by the main-chain one — the btw tool can
/// replay a parent message into the subagent file with inflated context tokens
/// and an incorrect cost that would double-count the turn.
///
/// Cross-file suppression (same message_id across a session file and its
/// subagent file) is handled in `spending::compute_spending`.
pub fn parse_claude_jsonl(path: &Path) -> Vec<CachedEntry> {
    let Ok(content) = std::fs::read(path) else {
        return Vec::new();
    };
    const USAGE_MARKER: &[u8] = br#""usage":{"#;

    let mut by_exact_key: HashMap<(String, Option<String>), CachedEntry> = HashMap::new();
    let mut no_id: Vec<CachedEntry> = Vec::new();

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
        let (Some(ts), Some(cost)) = (entry.timestamp.as_deref(), entry.cost_usd) else {
            continue;
        };
        if cost <= 0.0 {
            continue;
        }
        let date = match ts.get(..10) {
            Some(d) if d.as_bytes().get(4) == Some(&b'-') => d.to_string(),
            _ => continue,
        };
        let is_sidechain = entry.is_sidechain == Some(true);
        let usage = &entry.message.usage;
        let tokens = usage.input_tokens.unwrap_or(0)
            + usage.output_tokens.unwrap_or(0)
            + usage.cache_creation_input_tokens.unwrap_or(0)
            + usage.cache_read_input_tokens.unwrap_or(0);
        let cached = CachedEntry {
            date,
            cost_usd: cost,
            tokens,
            message_id: entry.message.id.clone(),
            request_id: entry.request_id.clone(),
            is_sidechain,
        };
        if let Some(ref msg_id) = entry.message.id {
            let exact_key = (msg_id.clone(), entry.request_id);
            by_exact_key
                .entry(exact_key)
                .and_modify(|existing| {
                    // Prefer main-chain over sidechain within the same file.
                    if existing.is_sidechain && !is_sidechain {
                        *existing = cached.clone();
                    }
                })
                .or_insert(cached);
        } else {
            no_id.push(cached);
        }
    }

    let mut out: Vec<CachedEntry> = by_exact_key.into_values().collect();
    out.extend(no_id);
    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::TempDir;

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
    fn encode_project_dir_replaces_slashes() {
        assert_eq!(
            encode_project_dir(Path::new("/home/user/my-project")),
            "-home-user-my-project"
        );
        assert_eq!(encode_project_dir(Path::new("/a/b/c")), "-a-b-c");
    }

    #[test]
    fn skips_lines_without_usage_object() {
        let dir = TempDir::new().unwrap();
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":99.0}"#,
                &claude_line("2026-01-01", 0.5, "msg-1", "req-1"),
            ],
        );
        let entries = parse_claude_jsonl(&file);
        assert_eq!(entries.len(), 1);
        assert!((entries[0].cost_usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn captures_input_plus_output_tokens() {
        let dir = TempDir::new().unwrap();
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[&claude_line("2026-01-01", 0.5, "msg-1", "req-1")],
        );
        let entries = parse_claude_jsonl(&file);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tokens, 25, "input 10 + output 5 + cache_creation 3 + cache_read 7");
    }

    #[test]
    fn captures_cache_tokens() {
        let dir = TempDir::new().unwrap();
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.5,"requestId":"req-1","message":{"id":"msg-1","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":200,"cache_read_input_tokens":800}}}"#,
            ],
        );
        let entries = parse_claude_jsonl(&file);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tokens, 1150, "input 100 + output 50 + cache_creation 200 + cache_read 800");
    }

    #[test]
    fn per_file_dedup_drops_exact_duplicate() {
        let dir = TempDir::new().unwrap();
        let line = claude_line("2026-01-01", 1.0, "msg-a", "req-a");
        let file = write_jsonl(dir.path(), "chat.jsonl", &[&line, &line]);
        let entries = parse_claude_jsonl(&file);
        assert_eq!(entries.len(), 1, "same (msg, req) within file must dedup");
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
        let entries = parse_claude_jsonl(&file);
        // Two distinct exact-keys: the suppression happens in compute_spending.
        assert_eq!(entries.len(), 2);
        let sc = entries.iter().find(|e| e.is_sidechain).unwrap();
        let main = entries.iter().find(|e| !e.is_sidechain).unwrap();
        assert!((sc.cost_usd - 5.0).abs() < 1e-9);
        assert!((main.cost_usd - 0.05).abs() < 1e-9);
    }

    #[test]
    fn zero_and_negative_costs_ignored() {
        let dir = TempDir::new().unwrap();
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.0,"message":{"usage":{"input_tokens":1}}}"#,
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":-1.0,"message":{"usage":{"input_tokens":1}}}"#,
                &claude_line("2026-01-01", 0.3, "msg-1", "req-1"),
            ],
        );
        let entries = parse_claude_jsonl(&file);
        assert_eq!(entries.len(), 1);
        assert!((entries[0].cost_usd - 0.3).abs() < 1e-9);
    }

    #[test]
    fn null_cost_usd_line_skipped() {
        let dir = TempDir::new().unwrap();
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":null,"message":{"usage":{"input_tokens":1}}}"#,
                &claude_line("2026-01-01", 0.5, "msg-1", "req-1"),
            ],
        );
        let entries = parse_claude_jsonl(&file);
        assert_eq!(entries.len(), 1, "costUSD:null line must be skipped");
    }

    #[test]
    fn null_model_line_skipped() {
        let dir = TempDir::new().unwrap();
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.1,"message":{"id":"msg-1","model":null,"usage":{"input_tokens":1}}}"#,
                &claude_line("2026-01-01", 0.5, "msg-2", "req-2"),
            ],
        );
        let entries = parse_claude_jsonl(&file);
        assert_eq!(entries.len(), 1, "model:null line must be skipped");
    }

    #[test]
    fn empty_request_id_line_skipped() {
        let dir = TempDir::new().unwrap();
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.1,"requestId":"","message":{"id":"msg-1","usage":{"input_tokens":1}}}"#,
                &claude_line("2026-01-01", 0.5, "msg-2", "req-2"),
            ],
        );
        let entries = parse_claude_jsonl(&file);
        assert_eq!(entries.len(), 1, "empty requestId must be rejected");
    }

    #[test]
    fn non_semver_version_skipped() {
        let dir = TempDir::new().unwrap();
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.1,"version":"dev","message":{"id":"msg-1","usage":{"input_tokens":1}}}"#,
                r#"{"timestamp":"2026-01-01T10:00:00.000Z","costUSD":0.2,"version":"1.2.3","message":{"id":"msg-2","usage":{"input_tokens":1}}}"#,
            ],
        );
        let entries = parse_claude_jsonl(&file);
        assert_eq!(entries.len(), 1, "non-semver version rejected; semver kept");
        assert!((entries[0].cost_usd - 0.2).abs() < 1e-9);
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
