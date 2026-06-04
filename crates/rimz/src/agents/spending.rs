//! JSONL-based spending aggregation over agent transcript history.
//!
//! Per-provider typed parsers live in each adapter's `spend.rs`; this module
//! owns the on-disk cache types and the [`compute_spending`] aggregation loop
//! with cross-file Claude dedup. Discovery and parsing dispatch through the
//! adapter ([`AgentAdapter::transcript_files`] /
//! [`AgentAdapter::parse_spend`]): a dollar-logging provider (Claude's legacy
//! `costUSD`, Pi) reads its figures verbatim, a token-only provider (Codex,
//! current Claude) multiplies counts through the
//! [`PriceBook`](super::pricing) — either way every file yields
//! [`CachedEntry`] values and buckets under its adapter's kind.
//!
//! [`compute_spending`] returns a [`Spending`]: one fleet-wide trailing
//! 24h / 7d / 30d / 365d [`SpendTally`] plus a per-provider breakdown, so the
//! fleet ledger and cockpit read the fleet pile and each dashboard panel reads
//! its own.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::AgentAdapter;
use super::descriptor::ThreadKey;
use super::pricing::PriceBook;

// ── Public types ──────────────────────────────────────────────────────────────

/// Spend (USD) and token throughput accumulated over one time window. `tokens`
/// is `input + output` — the same `◇` total the rest of the sidebar reads, so
/// the figure stays consistent with the cockpit and per-card token lines. The
/// four-way split (`input` / `output` / `cache_write` / `cache_read`) feeds the
/// `◇ ↘ ↗ ◍ ◌` breakdown on the cockpit, the provider dashboard, and the W/M
/// ledger rows; `sessions` counts the distinct threads (transcript files, with a
/// Claude session's subagent files folded under it) that ran in the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpendWindow {
    pub usd: f64,
    /// The `◇` total: `input + output`. A maintained field (not derived on read)
    /// so the many `.tokens` read sites need no change.
    pub tokens: u64,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_write: u64,
    #[serde(default)]
    pub cache_read: u64,
    /// Distinct sessions (threads) with activity in this window.
    #[serde(default)]
    pub sessions: u32,
}

impl SpendWindow {
    /// Fold one priced entry's spend and token split into the window. `tokens`
    /// stays `input + output` (the `◇` total); cache tokens ride their own
    /// fields, never the total.
    fn add(&mut self, usd: f64, entry: &CachedEntry) {
        self.usd += usd;
        self.tokens += entry.input + entry.output;
        self.input += entry.input;
        self.output += entry.output;
        self.cache_write += entry.cache_write;
        self.cache_read += entry.cache_read;
    }
}

/// Rolling spend and token tally over four trailing windows: the last 24 hours,
/// 7 days, 30 days, and 365 days. The windows nest — `year` (365 days) is the
/// widest and subsumes the rest — so a recent entry lands in all four. Spend
/// older than a year falls out of every window.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpendTally {
    /// Trailing 24 hours.
    pub today: SpendWindow,
    /// Trailing 7 days.
    pub week: SpendWindow,
    /// Trailing 30 days.
    pub month: SpendWindow,
    /// Trailing 365 days — the widest window, so it subsumes the other three.
    /// `#[serde(default)]` keeps an older `provider-spending.json` (which carried
    /// an `all_time` field and no `year`) readable: the stale field is ignored and
    /// `year` defaults to zero until the producer rewrites the cache next tick.
    #[serde(default)]
    pub year: SpendWindow,
}

impl SpendTally {
    /// True when nothing has been recorded in the trailing year. `year` is the
    /// widest window, so a zero year means every window is zero.
    pub fn is_zero(&self) -> bool {
        self.year.usd == 0.0 && self.year.tokens == 0
    }
}

/// The result of a spending pass: the fleet-wide total plus a per-provider
/// breakdown keyed by agent kind (`"claude"`, `"codex"`, `"pi"`). The cockpit and
/// the fleet ledger read [`Spending::total`]; each provider dashboard panel reads
/// its own entry from [`Spending::by_provider`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Spending {
    pub total: SpendTally,
    pub by_provider: BTreeMap<String, SpendTally>,
}

/// Bumped whenever the cached parse shape *or values* change, so an upgrade
/// re-reads every file once. A finalized session's stable mtime otherwise pins
/// its entries in the cache forever — a field added to or reshaped in
/// [`CachedEntry`] (the `date` → `ts_secs` switch in v3; the `tokens` → four-way
/// split in v4), or a change in how a kept cost is computed (v4 also prices Claude
/// turns from token usage now that transcripts omit `costUSD`, so sessions cached
/// as zero entries must re-parse), would otherwise stay frozen for that session
/// and never heal. v5 makes the parse incremental: an entry without a real
/// `len`/`cursor` would read as "grown from offset 0" and append a duplicate
/// full parse, so the pre-cursor shape must cold-rebuild. A cache stamped with
/// an older version is discarded on read, forcing a clean re-parse under the
/// current shape. `0` is the implicit pre-versioning shape (no `version` field).
const SPENDING_CACHE_VERSION: u32 = 5;

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
    /// File length at the last parse — the growth/truncation detector: a
    /// longer file parses only its suffix, a shorter (rotated/truncated) one
    /// re-parses cold, an equal length with a new mtime re-parses cold (an
    /// in-place rewrite).
    #[serde(default)]
    pub len: u64,
    /// Where the last parse left off — the next incremental parse resumes here.
    #[serde(default)]
    pub cursor: SpendCursor,
    /// One entry per JSONL line with a positive cost. Duplicates within a file
    /// (retry writes) are kept raw here; the aggregation pass owns all dedup.
    pub entries: Vec<CachedEntry>,
}

/// Where an incremental spend parse left off: the byte offset just past the
/// last consumed line, plus the adapter's opaque cross-line state (Codex
/// carries its cumulative token totals and tracked model so a resumed delta
/// subtraction stays exact). Stored per file in the spending cache; a state
/// shape change bumps [`SPENDING_CACHE_VERSION`].
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpendCursor {
    pub offset: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<serde_json::Value>,
}

/// One spend parse: the entries read past the resume point and the cursor the
/// cache stores for the next pass.
#[derive(Debug, Default)]
pub struct SpendParse {
    pub entries: Vec<CachedEntry>,
    pub cursor: SpendCursor,
}

/// A single cost entry with dedup keys for cross-file deduplication.
///
/// `message_id` and `request_id` are present for Claude entries and absent for
/// Codex and Pi entries.  `is_sidechain` drives the sidechain-replay suppression
/// logic in `compute_spending`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedEntry {
    /// Unix timestamp (seconds) the entry was recorded, parsed from the JSONL
    /// `timestamp` via [`iso_to_unix_secs`]. Drives the trailing-window bucketing
    /// in [`accum`].
    pub ts_secs: u64,
    pub cost_usd: f64,
    /// Fresh input tokens (Claude `input_tokens`; Codex uncached input). The `◇`
    /// total is `input + output`; cache tokens are tracked apart, never folded in.
    /// `#[serde(default)]` keeps an older cache parseable; `SPENDING_CACHE_VERSION`
    /// is what actually heals it — a pre-split cache is discarded on read so every
    /// file re-parses, since a finalized session's stable mtime would otherwise pin
    /// these at `0`.
    #[serde(default)]
    pub input: u64,
    /// Output tokens (Codex `output_tokens` already includes reasoning).
    #[serde(default)]
    pub output: u64,
    /// Cache-write (Claude `cache_creation_input_tokens`); `0` for providers with
    /// no cache-creation concept (Codex, Pi).
    #[serde(default)]
    pub cache_write: u64,
    /// Cache-read (Claude `cache_read_input_tokens`; Codex `cached_input_tokens`).
    #[serde(default)]
    pub cache_read: u64,
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

// ── Spending computation ──────────────────────────────────────────────────────

/// Compute the fleet and per-provider trailing 24h / 7d / 30d / 365d spend and
/// token tally for the given adapter-tagged JSONL files, pricing token-only
/// providers (Codex) through `prices`.
///
/// IO is O(delta), not O(history): an unchanged file (same mtime and length)
/// is a pure cache hit, a grown file parses only its appended suffix from the
/// stored [`SpendCursor`], and only a truncated/rotated or rewritten-in-place
/// file re-parses cold. Sets `cache.dirty = true` when any entry was updated;
/// finished sessions have stable stats and are permanently served from cache
/// after the first parse.
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
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    cache: &mut SpendingDiskCache,
    prices: &PriceBook,
) -> Spending {
    let now_secs = unix_secs_now();

    // First pass: refresh stale cache entries — pure hit, suffix parse, or
    // cold parse, decided from one stat per file.
    for (adapter, file) in files {
        let (mtime, len) = file_stat(file);
        let key = file.to_string_lossy().into_owned();
        match cache.files.get_mut(&key) {
            // Unchanged: nothing to read.
            Some(entry) if entry.mtime_secs == mtime && entry.len == len => {}
            // Grown in place: parse only the appended suffix and extend.
            Some(entry) if len > entry.len => {
                let parsed = adapter.parse_spend(file, Some(&entry.cursor), prices);
                entry.entries.extend(parsed.entries);
                entry.cursor = parsed.cursor;
                entry.mtime_secs = mtime;
                entry.len = len;
                cache.dirty = true;
            }
            // New, truncated/rotated, or rewritten in place: parse cold.
            _ => {
                let parsed = adapter.parse_spend(file, None, prices);
                cache.files.insert(
                    key,
                    FileCacheEntry {
                        mtime_secs: mtime,
                        len,
                        cursor: parsed.cursor,
                        entries: parsed.entries,
                    },
                );
                cache.dirty = true;
            }
        }
    }

    // Second pass: aggregate with cross-file Claude deduplication.
    //
    // Claude entries carry message IDs and dedup on exact_key = (message_id,
    // request_id); msg_has_non_sidechain tracks whether each message_id has a
    // main-chain entry anywhere across all files, so sidechain replays can be
    // suppressed.  ID-free entries (Codex, Pi) carry their file's provider so
    // they bucket under the right kind.
    let mut by_exact_key: HashMap<(String, Option<String>), (&'static str, CachedEntry)> =
        HashMap::new();
    let mut msg_has_non_sidechain: HashMap<String, bool> = HashMap::new();
    let mut free_entries: Vec<(&'static str, CachedEntry)> = Vec::new();

    for (adapter, file) in files {
        let kind = adapter.descriptor().kind;
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
                    .and_modify(|(_, existing)| {
                        if existing.is_sidechain && !e.is_sidechain {
                            *existing = e.clone();
                        }
                    })
                    .or_insert_with(|| (kind, e.clone()));
            } else {
                free_entries.push((kind, e.clone()));
            }
        }
    }

    let mut spending = Spending::default();
    let mut add = |provider: &str, entry: &CachedEntry| {
        accum(&mut spending.total, entry, now_secs);
        accum(
            spending.by_provider.entry(provider.to_owned()).or_default(),
            entry,
            now_secs,
        );
    };

    // Message-ID entries bucket under the kind whose file they were kept from.
    for ((msg_id, _), (kind, entry)) in &by_exact_key {
        let is_sidechain_replay = entry.is_sidechain
            && msg_has_non_sidechain
                .get(msg_id.as_str())
                .copied()
                .unwrap_or(false);
        if !is_sidechain_replay {
            add(kind, entry);
        }
    }
    for (provider, entry) in &free_entries {
        add(provider, entry);
    }

    // Session counts, keyed by thread rather than file: a Claude session's
    // subagent files fold under its `session_id` directory so one thread counts
    // once. Each thread is single-provider; we track its youngest entry and bump
    // every window that youngest reading still falls within. Counted from the raw
    // cached entries (not the deduped set) since a thread that ran is a thread,
    // regardless of which file a duplicated turn was kept in.
    let mut threads: HashMap<String, (&'static str, u64)> = HashMap::new();
    for (adapter, file) in files {
        let cache_key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get(&cache_key) else {
            continue;
        };
        let Some(youngest) = cached_file.entries.iter().map(|entry| entry.ts_secs).max() else {
            continue;
        };
        threads
            .entry(session_key(*adapter, file))
            .and_modify(|(_, ts)| *ts = (*ts).max(youngest))
            .or_insert((adapter.descriptor().kind, youngest));
    }
    for (provider, youngest) in threads.values() {
        bump_sessions(&mut spending.total, *youngest, now_secs);
        bump_sessions(
            spending
                .by_provider
                .entry((*provider).to_owned())
                .or_default(),
            *youngest,
            now_secs,
        );
    }

    spending
}

fn accum(tally: &mut SpendTally, entry: &CachedEntry, now_secs: u64) {
    let usd = entry.cost_usd;
    // Trailing-window bucketing: an entry counts toward each window whose span it
    // still falls within. The windows nest (24h ⊂ 7d ⊂ 30d ⊂ 365d), so a recent
    // entry lands in all four; one older than a year lands in none.
    let age = now_secs.saturating_sub(entry.ts_secs);
    if age >= 365 * 86_400 {
        return;
    }
    tally.year.add(usd, entry);
    if age < 30 * 86_400 {
        tally.month.add(usd, entry);
    }
    if age < 7 * 86_400 {
        tally.week.add(usd, entry);
    }
    if age < 86_400 {
        tally.today.add(usd, entry);
    }
}

/// Count one session (thread) toward each trailing window its youngest entry
/// still falls within. The windows nest, so a thread young enough for `today` is
/// counted in `week`/`month`/`year` too; one whose last activity is older than a
/// year counts nowhere.
fn bump_sessions(tally: &mut SpendTally, youngest_ts: u64, now_secs: u64) {
    let age = now_secs.saturating_sub(youngest_ts);
    if age >= 365 * 86_400 {
        return;
    }
    tally.year.sessions += 1;
    if age < 30 * 86_400 {
        tally.month.sessions += 1;
    }
    if age < 7 * 86_400 {
        tally.week.sessions += 1;
    }
    if age < 86_400 {
        tally.today.sessions += 1;
    }
}

/// The thread a transcript file belongs to, per the adapter's declared
/// [`ThreadKey`]: a session-dir provider (Claude) spreads one session across a
/// main `…/<session_id>/chat.jsonl` plus subagent
/// `…/<session_id>/subagents/*.jsonl` files, so both fold under the
/// `<session_id>` directory and one thread counts once; a per-file provider
/// (Codex, Pi) keys on the file path.
fn session_key(adapter: &dyn AgentAdapter, path: &Path) -> String {
    let dir = match adapter.descriptor().thread_key {
        ThreadKey::SessionDir => {
            let parent = path.parent();
            match parent {
                Some(p) if p.file_name().and_then(|name| name.to_str()) == Some("subagents") => {
                    p.parent()
                }
                other => other,
            }
        }
        ThreadKey::PerFile => None,
    };
    dir.unwrap_or(path).to_string_lossy().into_owned()
}

/// `(mtime_secs, len)` from one stat. `(0, 0)` on any error — the file then
/// reads as an empty cold parse rather than failing the pass.
fn file_stat(path: &Path) -> (u64, u64) {
    let Ok(meta) = fs::metadata(path) else {
        return (0, 0);
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (mtime, meta.len())
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

/// Parse an ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SS…`, e.g. a JSONL
/// `timestamp`) to Unix seconds. Reads fixed offsets; the time-of-day is optional
/// (a bare `YYYY-MM-DD` parses to midnight). Returns `None` when the date prefix
/// is malformed — the same guard the parsers applied to the old date slice.
pub(crate) fn iso_to_unix_secs(ts: &str) -> Option<u64> {
    let bytes = ts.as_bytes();
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    let year: i64 = ts.get(0..4)?.parse().ok()?;
    let month: i64 = ts.get(5..7)?.parse().ok()?;
    let day: i64 = ts.get(8..10)?.parse().ok()?;
    let hour: i64 = ts.get(11..13).and_then(|s| s.parse().ok()).unwrap_or(0);
    let min: i64 = ts.get(14..16).and_then(|s| s.parse().ok()).unwrap_or(0);
    let sec: i64 = ts.get(17..19).and_then(|s| s.parse().ok()).unwrap_or(0);
    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + min * 60 + sec;
    u64::try_from(secs).ok()
}

/// Days since the Unix epoch (1970-01-01 = 0) for a civil date — the inverse of
/// [`civil_date_from_epoch_days`]. Howard Hinnant's algorithm:
/// <http://howardhinnant.github.io/date_algorithms.html>
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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

    fn claude_adapter() -> &'static dyn AgentAdapter {
        &crate::agents::ClaudeAdapter
    }

    fn codex_adapter() -> &'static dyn AgentAdapter {
        &crate::agents::CodexAdapter
    }

    /// Claude tests don't need pricing — tag the files Claude, sum with an empty
    /// book, and take the fleet total, matching the pre-per-provider assertions.
    fn compute_total(files: &[PathBuf], cache: &mut SpendingDiskCache) -> SpendTally {
        let tagged: Vec<(&'static dyn AgentAdapter, PathBuf)> = files
            .iter()
            .map(|file| (claude_adapter(), file.clone()))
            .collect();
        compute_spending(&tagged, cache, &PriceBook::default()).total
    }

    /// ISO-8601 UTC timestamp for a Unix-seconds instant — round-trips through
    /// [`iso_to_unix_secs`] back to that same whole second.
    fn iso_at(secs: u64) -> String {
        let date = utc_date(secs);
        let tod = secs % 86_400;
        format!(
            "{date}T{:02}:{:02}:{:02}.000Z",
            tod / 3_600,
            (tod % 3_600) / 60,
            tod % 60
        )
    }

    // Full Claude-format line including "usage":{ to pass the fast pre-filter.
    fn claude_line_ts(ts: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
        format!(
            r#"{{"timestamp":"{ts}","costUSD":{cost},"requestId":"{req_id}","message":{{"id":"{msg_id}","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
        )
    }

    fn claude_line(date: &str, cost: f64, msg_id: &str, req_id: &str) -> String {
        claude_line_ts(&format!("{date}T10:00:00.000Z"), cost, msg_id, req_id)
    }

    /// A Claude line stamped `secs_ago` before now — for trailing-window tests.
    fn claude_line_ago(secs_ago: u64, cost: f64, msg_id: &str, req_id: &str) -> String {
        claude_line_ts(
            &iso_at(unix_secs_now().saturating_sub(secs_ago)),
            cost,
            msg_id,
            req_id,
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
    fn iso_to_unix_secs_parses_known_instants() {
        assert_eq!(
            iso_to_unix_secs("2000-01-01T00:00:00.000Z"),
            Some(946_684_800)
        );
        // 2025-06-01T00:00:00Z = 1748736000; + 12h = +43200.
        assert_eq!(
            iso_to_unix_secs("2025-06-01T12:00:00Z"),
            Some(1_748_779_200)
        );
        // A bare date parses to midnight UTC.
        assert_eq!(iso_to_unix_secs("1970-01-02"), Some(86_400));
        // Round-trips with the test formatter.
        assert_eq!(
            iso_to_unix_secs(&iso_at(1_700_000_123)),
            Some(1_700_000_123)
        );
        // Malformed prefixes are rejected.
        assert_eq!(iso_to_unix_secs("not-a-date"), None);
        assert_eq!(iso_to_unix_secs(""), None);
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
                    len: 0,
                    cursor: SpendCursor::default(),
                    entries: vec![CachedEntry {
                        ts_secs: 1_767_225_600,
                        cost_usd: 9.0,
                        input: 0,
                        output: 0,
                        cache_write: 0,
                        cache_read: 0,
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
                len: 0,
                cursor: SpendCursor::default(),
                entries: vec![CachedEntry {
                    ts_secs: 1_770_000_000,
                    cost_usd: 1.0,
                    input: 30,
                    output: 12,
                    cache_write: 0,
                    cache_read: 0,
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
            kept.files["/new/chat.jsonl"].entries[0].input, 30,
            "a same-version cache keeps its entries"
        );
    }

    #[test]
    fn token_split_and_session_counts_populate_windows() {
        let dir = TempDir::new().unwrap();
        let now_secs = unix_secs_now();
        let today = utc_date(now_secs);
        // One Claude thread spread across its `session_id` dir: a main chat file
        // plus a subagent file. Both fold under the one thread for session counts.
        let session = dir.path().join("sess-1");
        std::fs::create_dir_all(session.join("subagents")).unwrap();
        let main_line = format!(
            r#"{{"timestamp":"{today}T10:00:00.000Z","costUSD":0.5,"requestId":"req-1","message":{{"id":"msg-1","usage":{{"input_tokens":12000,"output_tokens":64000,"cache_creation_input_tokens":12000,"cache_read_input_tokens":68000}}}}}}"#
        );
        let main = write_jsonl(&session, "chat.jsonl", &[&main_line]);
        let sub_line = format!(
            r#"{{"timestamp":"{today}T10:01:00.000Z","costUSD":0.1,"requestId":"req-2","isSidechain":true,"message":{{"id":"msg-2","usage":{{"input_tokens":1000,"output_tokens":500,"cache_creation_input_tokens":0,"cache_read_input_tokens":2000}}}}}}"#
        );
        let subfile = write_jsonl(&session.join("subagents"), "worker.jsonl", &[&sub_line]);

        let mut cache = SpendingDiskCache::default();
        let total = compute_total(&[main, subfile], &mut cache);

        // `◇` is input + output only; the cache split rides its own fields.
        assert_eq!(total.today.input, 13_000, "12000 + 1000");
        assert_eq!(total.today.output, 64_500, "64000 + 500");
        assert_eq!(total.today.tokens, 77_500, "◇ = input + output");
        assert_eq!(total.today.cache_write, 12_000);
        assert_eq!(total.today.cache_read, 70_000, "68000 + 2000");
        // The main + subagent files fold under one `session_id` directory, so the
        // thread counts once across every window its activity falls within.
        assert_eq!(total.today.sessions, 1, "main + subagent = one thread");
        assert_eq!(total.year.sessions, 1);
    }

    #[test]
    fn trailing_windows_bucket_by_age() {
        let dir = TempDir::new().unwrap();
        const HOUR: u64 = 3_600;
        const DAY: u64 = 86_400;

        // One entry seated inside each successive window, plus one past the year.
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[
                &claude_line_ago(2 * HOUR, 1.0, "msg-1", "req-1"), // within 24h
                &claude_line_ago(3 * DAY, 0.5, "msg-2", "req-2"),  // within 7d, not 24h
                &claude_line_ago(20 * DAY, 0.25, "msg-3", "req-3"), // within 30d, not 7d
                &claude_line_ago(100 * DAY, 0.1, "msg-4", "req-4"), // within 365d, not 30d
                &claude_line_ago(400 * DAY, 9.0, "msg-5", "req-5"), // older than a year — dropped
            ],
        );

        let mut cache = SpendingDiskCache::default();
        let totals = compute_total(&[file], &mut cache);

        // The windows nest, so each wider one adds the next entry.
        assert!(
            (totals.today.usd - 1.0).abs() < 1e-9,
            "today (24h) = {}",
            totals.today.usd
        );
        assert_eq!(totals.today.tokens, 15, "one entry inside 24h");
        assert!(
            (totals.week.usd - 1.5).abs() < 1e-9,
            "week (7d) = {}",
            totals.week.usd
        );
        assert_eq!(totals.week.tokens, 30);
        assert!(
            (totals.month.usd - 1.75).abs() < 1e-9,
            "month (30d) = {}",
            totals.month.usd
        );
        assert_eq!(totals.month.tokens, 45);
        // year (365d) adds the 100-day entry; the 400-day entry falls out entirely.
        assert!(
            (totals.year.usd - 1.85).abs() < 1e-9,
            "year (365d) = {}",
            totals.year.usd
        );
        assert_eq!(
            totals.year.tokens, 60,
            "four entries inside the year; the 400-day one is dropped"
        );
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

        // Same (message_id, request_id) twice within one file (the parser
        // returns raw entries — this pass owns all dedup) and again in a
        // second file.
        let file1 = write_jsonl(dir.path(), "session1.jsonl", &[&line, &line]);
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

    fn append_line(path: &Path, line: &str) {
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(f, "{line}").unwrap();
    }

    #[test]
    fn grown_file_parses_only_the_appended_suffix() {
        let dir = TempDir::new().unwrap();
        let today = utc_date(unix_secs_now());
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[&claude_line(&today, 1.0, "msg-1", "req-1")],
        );
        let mut cache = SpendingDiskCache::default();
        let first = compute_spending(
            &[(claude_adapter(), file.clone())],
            &mut cache,
            &PriceBook::default(),
        );
        assert!((first.total.today.usd - 1.0).abs() < 1e-9);

        // Corrupt the already-parsed prefix in place (length unchanged, the
        // trailing newline kept), then append a second line. The incremental
        // pass must read only past its cursor, so the corruption is invisible
        // and the cached first entry still counts.
        let prefix_len = std::fs::metadata(&file).unwrap().len() as usize;
        {
            use std::io::{Seek as _, SeekFrom};
            let mut f = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
            f.seek(SeekFrom::Start(0)).unwrap();
            f.write_all(&vec![b'x'; prefix_len - 1]).unwrap();
        }
        append_line(&file, &claude_line(&today, 0.25, "msg-2", "req-2"));

        let second = compute_spending(
            &[(claude_adapter(), file)],
            &mut cache,
            &PriceBook::default(),
        );
        assert!(
            (second.total.today.usd - 1.25).abs() < 1e-9,
            "suffix-only read: the cached prefix entry survives its corruption (got {})",
            second.total.today.usd
        );
    }

    #[test]
    fn truncated_file_reparses_cold() {
        let dir = TempDir::new().unwrap();
        let today = utc_date(unix_secs_now());
        let line_a = claude_line(&today, 1.0, "msg-a", "req-a");
        let line_b = claude_line(&today, 0.5, "msg-b", "req-b");
        let file = write_jsonl(dir.path(), "chat.jsonl", &[&line_a, &line_b]);
        let mut cache = SpendingDiskCache::default();
        let first = compute_spending(
            &[(claude_adapter(), file.clone())],
            &mut cache,
            &PriceBook::default(),
        );
        assert!((first.total.today.usd - 1.5).abs() < 1e-9);

        // Rotation/truncation: the file shrinks. The stale tail entries must
        // drop with the cold re-parse, never lingering from the old cache.
        write_jsonl(dir.path(), "chat.jsonl", &[&line_a]);
        let second = compute_spending(
            &[(claude_adapter(), file)],
            &mut cache,
            &PriceBook::default(),
        );
        assert!(
            (second.total.today.usd - 1.0).abs() < 1e-9,
            "a shorter file re-parses cold (got {})",
            second.total.today.usd
        );
    }

    #[test]
    fn same_length_rewrite_with_a_new_mtime_reparses_cold() {
        let dir = TempDir::new().unwrap();
        let today = utc_date(unix_secs_now());
        // `1.0` and `3.0` format to the same byte length, so the rewrite
        // changes content but not size — only the mtime can reveal it.
        let file = write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[&claude_line(&today, 1.0, "msg-a", "req-a")],
        );
        let mut cache = SpendingDiskCache::default();
        compute_spending(
            &[(claude_adapter(), file.clone())],
            &mut cache,
            &PriceBook::default(),
        );

        write_jsonl(
            dir.path(),
            "chat.jsonl",
            &[&claude_line(&today, 3.0, "msg-a", "req-a")],
        );
        let f = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
            .unwrap();

        let second = compute_spending(
            &[(claude_adapter(), file)],
            &mut cache,
            &PriceBook::default(),
        );
        assert!(
            (second.total.today.usd - 3.0).abs() < 1e-9,
            "an in-place rewrite (same length, new mtime) re-parses cold (got {})",
            second.total.today.usd
        );
    }

    #[test]
    fn codex_resume_state_survives_the_suffix_parse() {
        let dir = TempDir::new().unwrap();
        let today = utc_date(unix_secs_now());
        // Cumulative-only token counts plus a model declared once up front:
        // both halves of the resume state are exercised — the appended event
        // must subtract the stored totals AND price under the remembered
        // model (a fresh fold would record the full cumulative as one
        // inflated delta; a lost model would drop the entry as unpriced).
        let file = write_codex(
            dir.path(),
            &[
                r#"{"type":"turn_context","payload":{"model":"gpt-4o"}}"#,
                &codex_total_line(&today, 1000, 500),
            ],
        );
        let mut cache = SpendingDiskCache::default();
        let first = compute_spending(
            &[(codex_adapter(), file.clone())],
            &mut cache,
            &gpt4o_book(),
        );
        assert_eq!(first.total.today.input, 1000);
        assert_eq!(first.total.today.output, 500);

        append_line(&file, &codex_total_line(&today, 1600, 800));
        let second = compute_spending(&[(codex_adapter(), file)], &mut cache, &gpt4o_book());
        assert_eq!(
            second.total.today.input, 1600,
            "the resumed fold subtracts the stored cumulative totals"
        );
        assert_eq!(second.total.today.output, 800);
    }

    fn codex_total_line(date: &str, input: u64, output: u64) -> String {
        format!(
            r#"{{"type":"event_msg","timestamp":"{date}T10:00:00.000Z","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"output_tokens":{output}}}}}}}}}"#
        )
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
        let spending = compute_spending(&[(codex_adapter(), file)], &mut cache, &gpt4o_book());

        // uncached 600 * 1e-6 + cached 400 * 1e-7 + output 500 * 2e-6
        //   = 0.0006 + 0.00004 + 0.001 = 0.00164
        let codex = &spending.by_provider["codex"];
        assert!(
            (codex.today.usd - 0.00164).abs() < 1e-9,
            "got {}",
            codex.today.usd
        );
        // `◇` is fresh input + output: Codex's `input_tokens` includes the cached
        // slice, so the uncached 600 + output 500 = 1100, with the 400 cached
        // riding `cache_read` (never the total).
        assert_eq!(codex.today.tokens, 1100, "uncached input 600 + output 500");
        assert_eq!(codex.today.input, 600);
        assert_eq!(codex.today.output, 500);
        assert_eq!(codex.today.cache_read, 400);
        assert_eq!(codex.today.cache_write, 0, "Codex has no cache-creation");
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
            &[(codex_adapter(), file)],
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
                (claude_adapter(), claude_file),
                (codex_adapter(), codex_file),
            ],
            &mut cache,
            &gpt4o_book(),
        );

        assert!((spending.by_provider["claude"].today.usd - 0.5).abs() < 1e-9);
        assert!((spending.by_provider["codex"].today.usd - 0.001).abs() < 1e-9);
        assert!((spending.total.today.usd - 0.501).abs() < 1e-9);
    }
}
