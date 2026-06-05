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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    now_secs: u64,
) -> Spending {
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

/// How long the producer trusts a published fleet-spending walk before
/// re-walking every provider's transcript tree. Spend is display-only (the
/// eased odometer roll absorbs the step) and the walk — discovery readdirs,
/// per-file stats, the cursor-map parse, the price-book load — is the
/// producer's largest steady cost, so a coarse TTL pays for itself. One TTL,
/// no retry split like `ACCOUNTS_RETRY_TTL`: the walk is per-file best-effort
/// and an empty fleet prices to zero cheaply, so there is no
/// infrastructure-failure state to re-probe fast — a partial read is a
/// smaller-than-true figure that heals on the next due walk.
pub const SPENDING_TTL: Duration = Duration::from_secs(15);

/// The published provider-spending cache: the aggregated [`Spending`] plus the
/// stamp the producer's [`SPENDING_TTL`] gate reads. A wrapper rather than a
/// field on [`Spending`] keeps the in-memory value the fold path threads
/// stamp-free; `#[serde(flatten)]` keeps a pre-stamp file (a bare `Spending`)
/// readable — its values survive, with `refreshed_at_ms` defaulting to 0 so it
/// reads as stale and refreshes once.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderSpendingCache {
    /// When the producer last walked and published, for the TTL gate.
    #[serde(default)]
    pub refreshed_at_ms: u64,
    /// Each live session's statusline cost (`total_cost_usd`, keyed by agent
    /// id == sidebar row id) captured at the instant the walk published — the
    /// baseline the cockpit's live overlay measures per-session overshoot
    /// against until the next walk re-stamps it ([`today_spend_live_usd`]). A
    /// pre-baseline file reads an empty map, so the overlay degrades to the
    /// exact walked figure, never a double count.
    #[serde(default)]
    pub live_cost_baselines: BTreeMap<String, f64>,
    #[serde(flatten)]
    pub spending: Spending,
}

impl ProviderSpendingCache {
    /// Whether the published walk is young enough that the producer skips the
    /// transcript walk this tick. Saturating, so a clock that ran backwards
    /// reads fresh rather than re-walking every tick.
    pub fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.refreshed_at_ms) <= SPENDING_TTL.as_millis() as u64
    }
}

/// Atomic write of the aggregated `Spending`, stamped `refreshed_at_ms` and
/// carrying the live-session cost baselines captured at this publish, so
/// consumer sidebar tabs read the fleet and per-provider totals — and the
/// producer its own [`SPENDING_TTL`] gate — without re-walking the JSONL
/// transcript history. Follows the same temp-then-rename durability contract
/// as [`write_spending_cache`].
pub fn write_provider_spending_cache(
    path: &Path,
    refreshed_at_ms: u64,
    spending: &Spending,
    live_cost_baselines: BTreeMap<String, f64>,
) {
    let cache = ProviderSpendingCache {
        refreshed_at_ms,
        live_cost_baselines,
        spending: spending.clone(),
    };
    let Ok(bytes) = serde_json::to_vec(&cache) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, &bytes).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

/// Read the provider-spending cache written by [`write_provider_spending_cache`].
/// Returns a default (stamp 0, so it reads as stale) on any error so callers
/// always get a usable value; a pre-stamp file deserializes with its spending
/// values intact.
pub fn read_provider_spending_cache(path: &Path) -> ProviderSpendingCache {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ProviderSpendingCache>(&bytes).ok())
        .unwrap_or_default()
}

/// Today's spend as the cockpit paints it: the walked tally's exact figure
/// plus each live session's overshoot over the baseline captured when the
/// walk published — so the headline climbs the instant a session's statusline
/// cost moves, while the walk stays the truth it reconciles to on the next
/// publish. Pure presentation over `(session id, cost now, registered-at ms)`
/// triples: a baselined session adds `max(0, cost_now − baseline)` (a resumed
/// or reset session clamps to zero rather than rolling the headline
/// backwards); a session absent from the baselines adds its whole cost when
/// it registered after the publish stamp — the walk never saw it — and
/// nothing otherwise, the fail-safe undercount that heals on the next walk.
pub fn today_spend_live_usd<'a>(
    walked_today_usd: f64,
    live_costs: impl Iterator<Item = (&'a str, f64, Option<u64>)>,
    baselines: &BTreeMap<String, f64>,
    published_at_ms: u64,
) -> f64 {
    let overshoot: f64 = live_costs
        .map(|(id, cost_now, registered_at_ms)| match baselines.get(id) {
            Some(baseline) => (cost_now - baseline).max(0.0),
            None if registered_at_ms.is_some_and(|at| at > published_at_ms) => cost_now.max(0.0),
            None => 0.0,
        })
        .sum();
    walked_today_usd + overshoot
}

// ── Date utilities ────────────────────────────────────────────────────────────

/// The wall clock as Unix seconds — the `now_secs` a caller captures once and
/// threads into [`compute_spending`], which stays pure over it.
pub fn unix_secs_now() -> u64 {
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
mod tests;
