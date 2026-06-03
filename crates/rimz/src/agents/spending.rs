//! JSONL-based spending aggregation over agent transcript history.
//!
//! Per-provider typed parsers live in the sibling [`transcript`](super::transcript)
//! modules; this module owns the on-disk cache types and the
//! [`compute_spending`] aggregation loop with cross-file Claude dedup. Parsing
//! is provider-dispatched in `parse_for`, keyed by the [`ProviderKind`] the
//! caller tags each file with at discovery:
//!
//! - **Claude** and **Pi** log `costUSD` directly — each entry becomes a
//!   [`CachedEntry`] verbatim, with `input + output` tokens alongside.
//! - **Codex** logs only token counts, so its events are multiplied through a
//!   [`PriceBook`](super::pricing) to a USD cost before bucketing.
//!
//! [`compute_spending`] returns a [`Spending`]: one fleet-wide today / week /
//! month / all-time [`SpendTally`] plus a per-provider breakdown, so the value
//! corner and cockpit read the fleet pile and each dashboard panel reads its own.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::pricing::PriceBook;
use super::transcript::{claude, codex, pi};

// ── Public types ──────────────────────────────────────────────────────────────

/// Spend (USD) and token throughput accumulated over one time window. `tokens`
/// is `input + output` — the same `◇` total the rest of the sidebar reads, so
/// the figure stays consistent with the cockpit and per-card token lines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpendWindow {
    pub usd: f64,
    pub tokens: u64,
}

impl SpendWindow {
    fn add(&mut self, usd: f64, tokens: u64) {
        self.usd += usd;
        self.tokens += tokens;
    }
}

/// Today / rolling-7-day / calendar-month / all-time spend and token tally for a
/// set of sessions. `all_time` is the unfiltered sum — the figure that only ever
/// grows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpendTally {
    pub today: SpendWindow,
    /// Rolling 7-day window: today plus the 6 prior UTC days.
    pub week: SpendWindow,
    /// Calendar month: the current UTC `YYYY-MM`.
    pub month: SpendWindow,
    /// Every entry regardless of date — the all-time pile.
    pub all_time: SpendWindow,
}

impl SpendTally {
    /// True when nothing has ever been recorded. `all_time` subsumes every
    /// window, so a zero all-time means every window is zero.
    pub fn is_zero(&self) -> bool {
        self.all_time.usd == 0.0 && self.all_time.tokens == 0
    }
}

/// The result of a spending pass: the fleet-wide total plus a per-provider
/// breakdown keyed by agent kind (`"claude"`, `"codex"`, `"pi"`). The value
/// corner and cockpit read [`Spending::total`]; each provider dashboard panel
/// reads its own entry from [`Spending::by_provider`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Spending {
    pub total: SpendTally,
    pub by_provider: BTreeMap<String, SpendTally>,
}

/// Which provider a discovered transcript file belongs to. The caller knows this
/// from the discovery source (`project_jsonl_files`, `codex_session_files`,
/// `pi_session_files`), so it is passed in rather than guessed from the path —
/// a non-default `CODEX_HOME` / `CLAUDE_CONFIG_DIR` would defeat a path guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    Claude,
    Codex,
    Pi,
}

impl ProviderKind {
    /// The agent `kind` string used as the per-provider bucket key.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Claude => "claude",
            ProviderKind::Codex => "codex",
            ProviderKind::Pi => "pi",
        }
    }
}

/// Bumped whenever the cached parse shape changes, so an upgrade re-reads every
/// file once. A finalized session's stable mtime otherwise pins its entries in
/// the cache forever — a field added to [`CachedEntry`] (such as `tokens`) would
/// stay at its `serde` default for that session and never heal. A cache stamped
/// with an older version is discarded on read, forcing a clean re-parse under the
/// current shape. `0` is the implicit pre-versioning shape (no `version` field).
const SPENDING_CACHE_VERSION: u32 = 2;

/// On-disk cache persisted at `{runtime_root}/spending.json`.
///
/// Keyed by canonical file path string.  `dirty` is excluded from
/// serialization — callers set it and flush when true. `version` gates the
/// whole cache: [`read_spending_cache`] discards a stale-shape cache and stamps
/// the current version, so a write always carries it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpendingDiskCache {
    #[serde(default)]
    pub version: u32,
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
/// Codex and Pi entries.  `is_sidechain` drives the sidechain-replay suppression
/// logic in `compute_spending`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedEntry {
    pub date: String,
    pub cost_usd: f64,
    /// Input + output tokens for this entry. `#[serde(default)]` keeps an older
    /// cache parseable; `SPENDING_CACHE_VERSION` is what actually heals it — a
    /// pre-token cache is discarded on read so every file re-parses, since a
    /// finalized session's stable mtime would otherwise pin `tokens` at `0`.
    #[serde(default)]
    pub tokens: u64,
    /// `message.id` from Claude entries; `None` for Codex and Pi entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// `requestId` from Claude entries; `None` for Codex and Pi entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// `isSidechain` flag from Claude entries.
    #[serde(default)]
    pub is_sidechain: bool,
}

// ── Re-exports from transcript parser modules ─────────────────────────────────

pub use claude::{claude_config_dirs, encode_project_dir, project_jsonl_files};
pub use codex::codex_session_files;
pub use pi::pi_session_files;

// ── Spending computation ──────────────────────────────────────────────────────

/// Compute the fleet and per-provider today / week / month / all-time spend and
/// token tally for the given JSONL files, pricing token-only providers (Codex)
/// through `prices`.
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
/// entry is suppressed.  Only Claude entries carry message IDs; Codex and Pi
/// entries are ID-free and bucket directly under their file's tagged provider.
pub fn compute_spending(
    files: &[(ProviderKind, PathBuf)],
    cache: &mut SpendingDiskCache,
    prices: &PriceBook,
) -> Spending {
    let now_secs = unix_secs_now();
    let today = utc_date(now_secs);
    let week_start = utc_date(now_secs.saturating_sub(6 * 86_400));
    let month_prefix = today[..7].to_string(); // "YYYY-MM"

    // First pass: refresh stale cache entries.
    for (kind, file) in files {
        let mtime = file_mtime_secs(file);
        let key = file.to_string_lossy().into_owned();
        let stale = cache.files.get(&key).is_none_or(|e| e.mtime_secs != mtime);
        if stale {
            let parsed = parse_for(*kind, file, prices);
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
    // Claude entries carry message IDs and dedup on exact_key = (message_id,
    // request_id); msg_has_non_sidechain tracks whether each message_id has a
    // main-chain entry anywhere across all files, so sidechain replays can be
    // suppressed.  ID-free entries (Codex, Pi) carry their file's provider so
    // they bucket under the right kind.
    let mut by_exact_key: HashMap<(String, Option<String>), CachedEntry> = HashMap::new();
    let mut msg_has_non_sidechain: HashMap<String, bool> = HashMap::new();
    let mut free_entries: Vec<(&'static str, CachedEntry)> = Vec::new();

    for (kind, file) in files {
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
                free_entries.push((kind.as_str(), e.clone()));
            }
        }
    }

    let mut spending = Spending::default();
    let mut add = |provider: &str, entry: &CachedEntry| {
        accum(
            &mut spending.total,
            entry,
            &today,
            &week_start,
            &month_prefix,
        );
        accum(
            spending.by_provider.entry(provider.to_owned()).or_default(),
            entry,
            &today,
            &week_start,
            &month_prefix,
        );
    };

    // Message-ID entries are Claude's.
    for ((msg_id, _), entry) in &by_exact_key {
        let is_sidechain_replay = entry.is_sidechain
            && msg_has_non_sidechain
                .get(msg_id.as_str())
                .copied()
                .unwrap_or(false);
        if !is_sidechain_replay {
            add("claude", entry);
        }
    }
    for (provider, entry) in &free_entries {
        add(provider, entry);
    }
    spending
}

fn accum(
    tally: &mut SpendTally,
    entry: &CachedEntry,
    today: &str,
    week_start: &str,
    month_prefix: &str,
) {
    let (usd, tokens) = (entry.cost_usd, entry.tokens);
    tally.all_time.add(usd, tokens);
    if entry.date == today {
        tally.today.add(usd, tokens);
    }
    if entry.date.as_str() >= week_start {
        tally.week.add(usd, tokens);
    }
    if entry.date.starts_with(month_prefix) {
        tally.month.add(usd, tokens);
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

fn parse_for(kind: ProviderKind, path: &Path, prices: &PriceBook) -> Vec<CachedEntry> {
    match kind {
        ProviderKind::Codex => codex_entries(path, prices),
        ProviderKind::Pi => pi::parse_pi_jsonl(path),
        ProviderKind::Claude => claude::parse_claude_jsonl(path),
    }
}

/// Turn a Codex session's token events into priced [`CachedEntry`] values.
///
/// Codex logs token counts, not dollars, so each event is multiplied through
/// the price book: uncached input at the input rate, the cached slice at the
/// cache-read rate, and output (which already includes reasoning tokens) at the
/// output rate. The recorded `tokens` is `input + output` — the same `◇` total
/// the rest of the sidebar reads. Events whose model has no known price, or that
/// price to zero, are dropped. Codex entries carry no message/request IDs, so
/// they bypass the Claude dedup and bucket directly under the `codex` provider.
fn codex_entries(path: &Path, prices: &PriceBook) -> Vec<CachedEntry> {
    let sessions_dir = path.parent().unwrap_or(path);
    let events = codex::parse_codex_session(sessions_dir, path);
    let mut out = Vec::with_capacity(events.len());
    for event in events {
        let Some(model) = event.model.as_deref() else {
            continue;
        };
        let Some(price) = prices.price(model) else {
            continue;
        };
        let uncached_input = event.input_tokens.saturating_sub(event.cached_input_tokens);
        let cost = uncached_input as f64 * price.input
            + event.cached_input_tokens as f64 * price.cache_read
            + event.output_tokens as f64 * price.output;
        if cost <= 0.0 {
            continue;
        }
        let date = match event.timestamp.get(..10) {
            Some(d) if d.as_bytes().get(4) == Some(&b'-') => d.to_owned(),
            _ => continue,
        };
        out.push(CachedEntry {
            date,
            cost_usd: cost,
            tokens: event.input_tokens + event.output_tokens,
            message_id: None,
            request_id: None,
            is_sidechain: false,
        });
    }
    out
}

// ── Cache I/O ─────────────────────────────────────────────────────────────────

/// Load the cache, discarding any cache written under an older parse shape so
/// every file re-parses once under the current one. The returned cache always
/// carries the current version, so the next [`write_spending_cache`] stamps it.
pub fn read_spending_cache(path: &Path) -> SpendingDiskCache {
    let mut cache = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SpendingDiskCache>(&bytes).ok())
        .filter(|cache| cache.version == SPENDING_CACHE_VERSION)
        .unwrap_or_default();
    cache.version = SPENDING_CACHE_VERSION;
    cache
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

// ── Provider-spending cache ───────────────────────────────────────────────────

/// Atomic write of the aggregated `Spending` to a small JSON cache so consumer
/// sidebar tabs can read the fleet and per-provider totals without re-walking
/// the JSONL transcript history. Follows the same temp-then-rename durability
/// contract as [`write_spending_cache`].
pub fn write_provider_spending_cache(path: &Path, spending: &Spending) {
    let Ok(bytes) = serde_json::to_vec(spending) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, &bytes).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

/// Read the provider-spending cache written by [`write_provider_spending_cache`].
/// Returns [`Spending::default`] on any error so callers always get a usable value.
pub fn read_provider_spending_cache(path: &Path) -> Spending {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Spending>(&bytes).ok())
        .unwrap_or_default()
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

    /// Claude tests don't need pricing — tag the files Claude, sum with an empty
    /// book, and take the fleet total, matching the pre-per-provider assertions.
    fn compute_total(files: &[PathBuf], cache: &mut SpendingDiskCache) -> SpendTally {
        let tagged: Vec<(ProviderKind, PathBuf)> = files
            .iter()
            .map(|file| (ProviderKind::Claude, file.clone()))
            .collect();
        compute_spending(&tagged, cache, &PriceBook::default()).total
    }

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
        let t1 = compute_total(std::slice::from_ref(&file), &mut cache);
        assert!((t1.today.usd - 0.5).abs() < 1e-9);
        assert_eq!(t1.today.tokens, 15, "input 10 + output 5");
        assert!(cache.dirty);

        cache.dirty = false;
        let t2 = compute_total(&[file], &mut cache);
        assert_eq!(t2.today.usd, t1.today.usd);
        assert_eq!(t2.today.tokens, t1.today.tokens);
        assert!(
            !cache.dirty,
            "cache should not be marked dirty on a cache hit"
        );
    }

    #[test]
    fn stale_version_cache_is_discarded_so_files_reparse() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("spending.json");

        // A cache from an older parse shape: a file entry whose `tokens` predate
        // the field, under the implicit pre-versioning `version: 0`.
        let stale = SpendingDiskCache {
            version: 0,
            files: HashMap::from([(
                "/old/chat.jsonl".to_string(),
                FileCacheEntry {
                    mtime_secs: 123,
                    entries: vec![CachedEntry {
                        date: "2026-01-01".to_string(),
                        cost_usd: 9.0,
                        tokens: 0,
                        message_id: Some("msg-old".to_string()),
                        request_id: Some("req-old".to_string()),
                        is_sidechain: false,
                    }],
                },
            )]),
            dirty: false,
        };
        write_spending_cache(&path, &stale);

        // Read drops the stale-shape cache entirely and stamps the current
        // version, so the finalized session re-parses instead of serving `0`
        // tokens from a mtime that will never change again.
        let healed = read_spending_cache(&path);
        assert_eq!(healed.version, SPENDING_CACHE_VERSION);
        assert!(
            healed.files.is_empty(),
            "a stale-version cache is discarded, not served"
        );

        // A current-version cache round-trips with its files intact — only a
        // version mismatch discards.
        let mut current = healed;
        current.files.insert(
            "/new/chat.jsonl".to_string(),
            FileCacheEntry {
                mtime_secs: 456,
                entries: vec![CachedEntry {
                    date: "2026-02-02".to_string(),
                    cost_usd: 1.0,
                    tokens: 42,
                    message_id: None,
                    request_id: None,
                    is_sidechain: false,
                }],
            },
        );
        write_spending_cache(&path, &current);
        let kept = read_spending_cache(&path);
        assert_eq!(kept.version, SPENDING_CACHE_VERSION);
        assert_eq!(
            kept.files["/new/chat.jsonl"].entries[0].tokens, 42,
            "a same-version cache keeps its entries"
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
        let totals = compute_total(&[file], &mut cache);

        assert!(
            (totals.today.usd - 1.0).abs() < 1e-9,
            "today = {}",
            totals.today.usd
        );
        assert_eq!(totals.today.tokens, 15, "one entry today, 15 tok each");
        // week = today + yesterday = 1.5 (old is 8 days ago, outside the 7-day window)
        assert!(
            (totals.week.usd - 1.5).abs() < 1e-9,
            "week = {}",
            totals.week.usd
        );
        assert_eq!(totals.week.tokens, 30, "two entries in the rolling week");
        // all-time sums every entry regardless of date — the figure that only grows.
        assert!(
            (totals.all_time.usd - 1.75).abs() < 1e-9,
            "all_time = {}",
            totals.all_time.usd
        );
        assert_eq!(totals.all_time.tokens, 45, "three entries, 15 tok each");
    }

    #[test]
    fn empty_file_list_returns_zero() {
        let mut cache = SpendingDiskCache::default();
        assert!(compute_total(&[], &mut cache).is_zero());
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
        let totals = compute_total(&[file], &mut cache);
        assert!((totals.today.usd - 0.3).abs() < 1e-9);
        assert_eq!(
            totals.today.tokens, 15,
            "only the kept entry: input 10 + output 5"
        );
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
        let totals = compute_total(&[file1, file2], &mut cache);
        assert!(
            (totals.today.usd - 1.0).abs() < 1e-9,
            "got {}",
            totals.today.usd
        );
        assert_eq!(totals.today.tokens, 15, "the duplicate pair counts once");
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
        let totals = compute_total(&[main_file, side_file], &mut cache);
        assert!(
            (totals.today.usd - 0.05).abs() < 1e-9,
            "today.usd = {} (expected 0.05)",
            totals.today.usd
        );
        assert_eq!(
            totals.today.tokens, 15,
            "main-chain tokens kept, the 50k sidechain replay suppressed"
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
        let totals = compute_total(&[file], &mut cache);
        assert!(
            (totals.today.usd - 0.20).abs() < 1e-9,
            "got {}",
            totals.today.usd
        );
        assert_eq!(
            totals.today.tokens, 50_005,
            "a lone sidechain keeps its tokens: input 50000 + output 5"
        );
    }

    /// A price book with a single non-builtin model so the asserted cost is
    /// independent of the hardcoded builtin values.
    fn gpt4o_book() -> PriceBook {
        PriceBook::from_litellm_json(
            r#"{"gpt-4o": {"input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6,
                           "cache_read_input_token_cost": 1e-7}}"#,
        )
    }

    /// Write a Codex session file. The path is irrelevant — the provider is
    /// tagged explicitly at the `compute_spending` call.
    fn write_codex(dir: &Path, lines: &[&str]) -> PathBuf {
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        write_jsonl(&sessions, "sess.jsonl", lines)
    }

    fn codex_token_line(date: &str, input: u64, cached: u64, output: u64) -> String {
        format!(
            r#"{{"type":"event_msg","timestamp":"{date}T10:00:00.000Z","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output}}}}}}}}}"#
        )
    }

    #[test]
    fn codex_tokens_priced_through_book() {
        let dir = TempDir::new().unwrap();
        let today = utc_date(unix_secs_now());
        let file = write_codex(
            dir.path(),
            &[
                r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#,
                &codex_token_line(&today, 1000, 400, 500),
            ],
        );

        let mut cache = SpendingDiskCache::default();
        let spending = compute_spending(&[(ProviderKind::Codex, file)], &mut cache, &gpt4o_book());

        // uncached 600 * 1e-6 + cached 400 * 1e-7 + output 500 * 2e-6
        //   = 0.0006 + 0.00004 + 0.001 = 0.00164
        let codex = &spending.by_provider["codex"];
        assert!(
            (codex.today.usd - 0.00164).abs() < 1e-9,
            "got {}",
            codex.today.usd
        );
        assert_eq!(codex.today.tokens, 1500, "input 1000 + output 500");
        assert!((spending.total.today.usd - 0.00164).abs() < 1e-9);
    }

    #[test]
    fn unpriced_codex_model_contributes_nothing() {
        let dir = TempDir::new().unwrap();
        let today = utc_date(unix_secs_now());
        let file = write_codex(
            dir.path(),
            &[
                r#"{"type":"turn_context","payload":{"model":"some-unknown-model-xyz"}}"#,
                &codex_token_line(&today, 1000, 0, 500),
            ],
        );

        let mut cache = SpendingDiskCache::default();
        // Empty json → only builtins; the unknown model has no price.
        let spending = compute_spending(
            &[(ProviderKind::Codex, file)],
            &mut cache,
            &PriceBook::from_litellm_json("{}"),
        );
        assert!(spending.total.is_zero());
    }

    #[test]
    fn per_provider_breakdown_splits_claude_and_codex() {
        let dir = TempDir::new().unwrap();
        let today = utc_date(unix_secs_now());

        let claude_file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[&claude_line(&today, 0.5, "msg-1", "req-1")],
        );
        let codex_file = write_codex(
            dir.path(),
            &[
                r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#,
                &codex_token_line(&today, 1000, 0, 0),
            ],
        );

        let mut cache = SpendingDiskCache::default();
        let spending = compute_spending(
            &[
                (ProviderKind::Claude, claude_file),
                (ProviderKind::Codex, codex_file),
            ],
            &mut cache,
            &gpt4o_book(),
        );

        assert!((spending.by_provider["claude"].today.usd - 0.5).abs() < 1e-9);
        assert!((spending.by_provider["codex"].today.usd - 0.001).abs() < 1e-9);
        assert!((spending.total.today.usd - 0.501).abs() < 1e-9);
    }
}
