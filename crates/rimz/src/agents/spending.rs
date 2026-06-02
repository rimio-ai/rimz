//! JSONL-based spending aggregation over agent transcript history.
//!
//! Per-provider typed parsers live in the sibling [`transcript`](super::transcript)
//! modules; this module owns the on-disk cache types and the
//! [`compute_spending`] aggregation loop with cross-file Claude dedup.  Parsing
//! is provider-dispatched in `parse_jsonl`: a `costUSD`-bearing entry from
//! Claude or Pi becomes a [`CachedEntry`] and is bucketed into today / week /
//! month.
//!
//! The live consumer ([`crate::SidebarSnapshot`] spending enrichment) currently
//! feeds only Claude project files, so Claude is the sole provider reflected in
//! the sidebar today.  The Pi parser is wired into `parse_jsonl` and ready;
//! Codex JSONL carries token counts rather than `costUSD` and additionally
//! awaits a per-model pricing table.  This staging is deliberate — the typed
//! read-path lands ahead of the upcoming deeper transcript-history analysis;
//! see [`super::transcript`] for the full rationale.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::transcript::{self, claude, pi};

// ── Public types ──────────────────────────────────────────────────────────────

/// Today / rolling-7-day / calendar-month spending for a set of sessions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentSpending {
    pub today_usd: f64,
    /// Rolling 7-day window: today plus the 6 prior UTC days.
    pub week_usd: f64,
    /// Calendar month: the current UTC `YYYY-MM`.
    pub month_usd: f64,
}

impl AgentSpending {
    pub fn is_zero(&self) -> bool {
        self.today_usd == 0.0 && self.week_usd == 0.0 && self.month_usd == 0.0
    }
}

/// On-disk cache persisted at `{runtime_root}/spending.json`.
///
/// Keyed by canonical file path string.  `dirty` is excluded from
/// serialization — callers set it and flush when true.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpendingDiskCache {
    #[serde(default)]
    pub files: HashMap<String, FileCacheEntry>,
    #[serde(skip)]
    pub dirty: bool,
}

/// Cached parse of one JSONL file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileCacheEntry {
    pub mtime_secs: u64,
    /// One entry per deduplicated JSONL line with a positive cost.
    pub entries: Vec<CachedEntry>,
}

/// A single cost entry with dedup keys for cross-file deduplication.
///
/// `message_id` and `request_id` are present for Claude entries and absent for
/// Pi entries.  `is_sidechain` drives the sidechain-replay suppression logic
/// in `compute_spending`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedEntry {
    pub date: String,
    pub cost_usd: f64,
    /// `message.id` from Claude entries; `None` for Pi entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// `requestId` from Claude entries; `None` for Pi entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// `isSidechain` flag from Claude entries.
    #[serde(default)]
    pub is_sidechain: bool,
}

// ── Re-exports from transcript parser modules ─────────────────────────────────

pub use claude::{claude_config_dirs, encode_project_dir, project_jsonl_files};
pub use pi::pi_session_files;

// ── Spending computation ──────────────────────────────────────────────────────

/// Compute today / week / month spending totals for the given JSONL files.
///
/// Only re-reads a file when its mtime has changed; sets `cache.dirty = true`
/// when any entry was updated.  Finished sessions have stable mtimes and are
/// permanently served from cache after the first parse.
///
/// ### Cross-file Claude deduplication
///
/// When a Claude session spawns subagents, the btw tool replays the parent
/// message into the subagent file with the full reconstructed context (50k+
/// cache-read tokens) and a `costUSD` that would double-count the turn if
/// summed naively.  Dedup by `(message.id, requestId)` across all files
/// prevents this: if `message.id` M appears as both `isSidechain:false`
/// (main-chain entry) and `isSidechain:true` (sidechain replay), the sidechain
/// entry is suppressed.
pub fn compute_spending(files: &[PathBuf], cache: &mut SpendingDiskCache) -> AgentSpending {
    let now_secs = unix_secs_now();
    let today = utc_date(now_secs);
    let week_start = utc_date(now_secs.saturating_sub(6 * 86_400));
    let month_prefix = today[..7].to_string(); // "YYYY-MM"

    // First pass: refresh stale cache entries.
    for file in files {
        let mtime = file_mtime_secs(file);
        let key = file.to_string_lossy().into_owned();
        let stale = cache.files.get(&key).is_none_or(|e| e.mtime_secs != mtime);
        if stale {
            let parsed = parse_jsonl(file);
            cache.files.insert(
                key,
                FileCacheEntry {
                    mtime_secs: mtime,
                    entries: parsed,
                },
            );
            cache.dirty = true;
        }
    }

    // Second pass: aggregate with cross-file Claude deduplication.
    //
    // For entries with message IDs: exact_key = (message_id, request_id).
    // msg_has_non_sidechain tracks whether each message_id has a main-chain
    // entry anywhere across all files, so sidechain replays can be suppressed.
    let mut by_exact_key: HashMap<(String, Option<String>), CachedEntry> = HashMap::new();
    let mut msg_has_non_sidechain: HashMap<String, bool> = HashMap::new();
    let mut free_entries: Vec<CachedEntry> = Vec::new();

    for file in files {
        let key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get(&key) else {
            continue;
        };
        for e in &cached_file.entries {
            if let Some(ref msg_id) = e.message_id {
                let has_ns = msg_has_non_sidechain.entry(msg_id.clone()).or_insert(false);
                if !e.is_sidechain {
                    *has_ns = true;
                }
                let exact_key = (msg_id.clone(), e.request_id.clone());
                by_exact_key
                    .entry(exact_key)
                    .and_modify(|existing| {
                        if existing.is_sidechain && !e.is_sidechain {
                            *existing = e.clone();
                        }
                    })
                    .or_insert_with(|| e.clone());
            } else {
                free_entries.push(e.clone());
            }
        }
    }

    let mut totals = AgentSpending::default();
    for ((msg_id, _), entry) in &by_exact_key {
        let is_sidechain_replay = entry.is_sidechain
            && msg_has_non_sidechain
                .get(msg_id.as_str())
                .copied()
                .unwrap_or(false);
        if !is_sidechain_replay {
            accum(
                &mut totals,
                &entry.date,
                entry.cost_usd,
                &today,
                &week_start,
                &month_prefix,
            );
        }
    }
    for entry in &free_entries {
        accum(
            &mut totals,
            &entry.date,
            entry.cost_usd,
            &today,
            &week_start,
            &month_prefix,
        );
    }
    totals
}

fn accum(
    totals: &mut AgentSpending,
    date: &str,
    cost: f64,
    today: &str,
    week_start: &str,
    month_prefix: &str,
) {
    if date == today {
        totals.today_usd += cost;
    }
    if date >= week_start {
        totals.week_usd += cost;
    }
    if date.starts_with(month_prefix) {
        totals.month_usd += cost;
    }
}

fn file_mtime_secs(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn parse_jsonl(path: &Path) -> Vec<CachedEntry> {
    match transcript::detect_provider(path) {
        transcript::Provider::Pi => pi::parse_pi_jsonl(path),
        // Claude and Unknown (e.g. test temp-dir paths) both use Claude format.
        transcript::Provider::Claude | transcript::Provider::Unknown => {
            claude::parse_claude_jsonl(path)
        }
    }
}

// ── Cache I/O ─────────────────────────────────────────────────────────────────

pub fn read_spending_cache(path: &Path) -> SpendingDiskCache {
    let Ok(bytes) = fs::read(path) else {
        return SpendingDiskCache::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Atomic write: temp file + rename, matching the project's ledger durability
/// contract.
pub fn write_spending_cache(path: &Path, cache: &SpendingDiskCache) {
    let Ok(bytes) = serde_json::to_vec(cache) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, &bytes).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

// ── Date utilities ────────────────────────────────────────────────────────────

fn unix_secs_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Format a Unix timestamp in seconds as a UTC date: `"YYYY-MM-DD"`.
pub fn utc_date(secs: u64) -> String {
    civil_date_from_epoch_days((secs / 86_400) as i64)
}

/// Convert days since the Unix epoch (1970-01-01 = 0) to `"YYYY-MM-DD"`.
///
/// Uses Howard Hinnant's civil-from-days algorithm:
/// <http://howardhinnant.github.io/date_algorithms.html>
fn civil_date_from_epoch_days(z: i64) -> String {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::TempDir;

    // Full Claude-format line including "usage":{ to pass the fast pre-filter.
    fn claude_line(date: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
        format!(
            r#"{{"timestamp":"{date}T10:00:00.000Z","costUSD":{cost},"requestId":"{req_id}","message":{{"id":"{msg_id}","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
        )
    }

    fn claude_sidechain_line(date: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
        format!(
            r#"{{"timestamp":"{date}T10:00:00.000Z","costUSD":{cost},"requestId":"{req_id}","isSidechain":true,"message":{{"id":"{msg_id}","usage":{{"input_tokens":50000,"output_tokens":5}}}}}}"#
        )
    }

    fn write_jsonl(dir: &Path, filename: &str, lines: &[&str]) -> PathBuf {
        let path = dir.join(filename);
        let mut f = std::fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
        path
    }

    #[test]
    fn utc_date_known_epoch() {
        assert_eq!(utc_date(0), "1970-01-01");
        // 2000-01-01 00:00:00 UTC = 946684800
        assert_eq!(utc_date(946_684_800), "2000-01-01");
        // 2025-06-01 00:00:00 UTC = 1748736000
        assert_eq!(utc_date(1_748_736_000), "2025-06-01");
        assert_eq!(utc_date(1_748_822_399), "2025-06-01");
    }

    #[test]
    fn mtime_cache_hit_skips_io() {
        let dir = TempDir::new().unwrap();
        let now_secs = unix_secs_now();
        let today = utc_date(now_secs);

        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[&claude_line(&today, 0.5, "msg-1", "req-1")],
        );

        let mut cache = SpendingDiskCache::default();
        let t1 = compute_spending(std::slice::from_ref(&file), &mut cache);
        assert!((t1.today_usd - 0.5).abs() < 1e-9);
        assert!(cache.dirty);

        cache.dirty = false;
        let t2 = compute_spending(&[file], &mut cache);
        assert_eq!(t2.today_usd, t1.today_usd);
        assert!(
            !cache.dirty,
            "cache should not be marked dirty on a cache hit"
        );
    }

    #[test]
    fn today_week_month_bucketing() {
        let dir = TempDir::new().unwrap();
        let now_secs = unix_secs_now();
        let today = utc_date(now_secs);
        let yesterday = utc_date(now_secs - 86_400);
        let old = utc_date(now_secs - 8 * 86_400); // 8 days ago — outside rolling week

        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                &claude_line(&today, 1.0, "msg-1", "req-1"),
                &claude_line(&yesterday, 0.5, "msg-2", "req-2"),
                &claude_line(&old, 0.25, "msg-3", "req-3"),
            ],
        );

        let mut cache = SpendingDiskCache::default();
        let totals = compute_spending(&[file], &mut cache);

        assert!(
            (totals.today_usd - 1.0).abs() < 1e-9,
            "today = {}",
            totals.today_usd
        );
        // week = today + yesterday = 1.5 (old is 8 days ago, outside the 7-day window)
        assert!(
            (totals.week_usd - 1.5).abs() < 1e-9,
            "week = {}",
            totals.week_usd
        );
    }

    #[test]
    fn empty_file_list_returns_zero() {
        let mut cache = SpendingDiskCache::default();
        assert!(compute_spending(&[], &mut cache).is_zero());
    }

    #[test]
    fn zero_and_negative_costs_ignored() {
        let dir = TempDir::new().unwrap();
        let now_secs = unix_secs_now();
        let today = utc_date(now_secs);

        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                &format!(
                    r#"{{"timestamp":"{today}T10:00:00.000Z","costUSD":0.0,"message":{{"usage":{{"input_tokens":1}}}}}}"#
                ),
                &format!(
                    r#"{{"timestamp":"{today}T11:00:00.000Z","costUSD":-1.0,"message":{{"usage":{{"input_tokens":1}}}}}}"#
                ),
                &claude_line(&today, 0.3, "msg-1", "req-1"),
            ],
        );

        let mut cache = SpendingDiskCache::default();
        let totals = compute_spending(&[file], &mut cache);
        assert!((totals.today_usd - 0.3).abs() < 1e-9);
    }

    #[test]
    fn claude_exact_dedup_drops_repeated_message_request_pair() {
        let dir = TempDir::new().unwrap();
        let now_secs = unix_secs_now();
        let today = utc_date(now_secs);
        let line = claude_line(&today, 1.0, "msg-a", "req-a");

        // Same (message_id, request_id) in two separate files.
        let file1 = write_jsonl(dir.path(), "session1.jsonl", &[&line]);
        let file2 = write_jsonl(dir.path(), "session2.jsonl", &[&line]);

        let mut cache = SpendingDiskCache::default();
        let totals = compute_spending(&[file1, file2], &mut cache);
        assert!(
            (totals.today_usd - 1.0).abs() < 1e-9,
            "got {}",
            totals.today_usd
        );
    }

    #[test]
    fn sidechain_replay_does_not_double_count() {
        let dir = TempDir::new().unwrap();
        let now_secs = unix_secs_now();
        let today = utc_date(now_secs);

        // Main-chain entry for msg-parent in session file.
        let main_file = write_jsonl(
            dir.path(),
            "session.jsonl",
            &[&claude_line(&today, 0.05, "msg-parent", "req-parent")],
        );
        // Sidechain replay of the same message in subagent file — inflated cost.
        let side_file = write_jsonl(
            dir.path(),
            "subagent.jsonl",
            &[&claude_sidechain_line(
                &today,
                5.00,
                "msg-parent",
                "req-sidechain",
            )],
        );

        let mut cache = SpendingDiskCache::default();
        let totals = compute_spending(&[main_file, side_file], &mut cache);
        assert!(
            (totals.today_usd - 0.05).abs() < 1e-9,
            "today_usd = {} (expected 0.05)",
            totals.today_usd
        );
    }

    #[test]
    fn sidechain_only_kept_when_no_main_chain_exists() {
        let dir = TempDir::new().unwrap();
        let now_secs = unix_secs_now();
        let today = utc_date(now_secs);

        let file = write_jsonl(
            dir.path(),
            "sidechain.jsonl",
            &[&claude_sidechain_line(&today, 0.20, "msg-x", "req-x")],
        );

        let mut cache = SpendingDiskCache::default();
        let totals = compute_spending(&[file], &mut cache);
        assert!(
            (totals.today_usd - 0.20).abs() < 1e-9,
            "got {}",
            totals.today_usd
        );
    }
}
