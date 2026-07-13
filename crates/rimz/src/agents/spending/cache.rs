//! Cursor-cache types, disk I/O, file stamps, and raw-row compaction for spending walks.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::agents::AgentAdapter;

use super::aggregate::{
    DedupPayload, SidechainDedup, cold_parse_out_of_window, within_raw_retain_window,
    within_widest_window,
};

/// Bumped whenever the cached parse shape *or values* change, so an upgrade
/// re-reads every file once. A finalized session's stable mtime otherwise pins
/// its entries in the cache forever — a field added to or reshaped in
/// [`CachedEntry`] (the `date` → `ts_secs` switch in v3; the `tokens` → four-way
/// split in v4), or a change in how a kept cost is computed (v4 also prices Claude
/// turns from token usage now that transcripts omit `costUSD`, so sessions cached
/// as zero entries must re-parse), would otherwise stay frozen for that session
/// and never heal. v5 makes the parse incremental: an entry without a real
/// `len`/`cursor` would read as "grown from offset 0" and append a duplicate
/// full parse, so the pre-cursor shape must cold-rebuild. v6 records per-file
/// unknown model names; the cold rebuild also heals entries silently dropped
/// while unpriced under v5. v7 records per-entry origin paths so workspace
/// scoped cockpit tallies can be exact; old entries would otherwise read as
/// unknown-origin and disappear from the cockpit. v8 stores Codex
/// `session_meta.cwd` in the parser cursor and stamps Codex entries from it;
/// old finalized Codex files would otherwise sit at EOF with no origin and
/// remain invisible to workspace-scoped cockpit tallies. v9 records the per-entry
/// model id for the `rimz stats` per-model breakdown; old finalized files would
/// otherwise sit at EOF with no model and never attribute their tokens. v10
/// records provider-native thread ids and keeps unpriced token usage rows, so
/// multi-session stores count sessions correctly and unknown prices hide only
/// dollars. A cache
/// stamped with an older version is discarded on read, forcing a clean re-parse
/// under the current shape. `0` is the implicit pre-versioning shape (no
/// `version` field). v11 drops per-entry origin in favour of per-file origin,
/// dedups retry writes within each parsed chunk before storing, and reshapes
/// compaction rollup keys around that per-file origin. v12 shortens on-disk
/// field keys and skips default values; a v11 cache would otherwise read under
/// the new keys as zeroed entries, so it cold-rebuilds. v13 aligns token-priced
/// costs with ccusage: Claude 1h cache creation prices at 2x input, 200k tiers
/// apply per token class, and fast-mode turns apply the model multiplier. v14
/// adds Claude advisor calls, Codex replay suppression, and request-selected
/// OpenAI long-context pricing; finalized files need one cold reprice.
pub(crate) const SPENDING_CACHE_VERSION: u32 = 14;

/// On-disk cache persisted at shared state `spending.json`.
///
/// Keyed by canonical file path string.  `dirty` is excluded from
/// serialization — callers set it and flush when true. `version` gates the
/// whole cache: [`read_spending_cache`] discards a stale-shape cache and stamps
/// the current version, so a write always carries it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpendingDiskCache {
    /// Version must stay the first field; `peek_cache_version` reads it from
    /// the file prefix before large cache writes.
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub files: HashMap<String, FileCacheEntry>,
    #[serde(skip)]
    pub dirty: bool,
    #[serde(skip)]
    pub generation: u64,
}

impl SpendingDiskCache {
    pub(crate) fn mark_changed(&mut self) {
        self.dirty = true;
        self.generation = self.generation.wrapping_add(1);
    }
}

/// Cached parse of one JSONL file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileCacheEntry {
    #[serde(default, rename = "m", skip_serializing_if = "is_zero")]
    pub mtime_secs: u64,
    /// File length at the last parse — the growth/truncation detector: a
    /// longer file parses only its suffix, a shorter (rotated/truncated) one
    /// re-parses cold, an equal length with a new mtime re-parses cold (an
    /// in-place rewrite).
    #[serde(default, rename = "n", skip_serializing_if = "is_zero")]
    pub len: u64,
    /// Where the last parse left off — the next incremental parse resumes here.
    #[serde(default, rename = "c", skip_serializing_if = "is_default_cursor")]
    pub cursor: SpendCursor,
    /// Durable per-file origin learned from the parser or a trusted override.
    /// Codex rollout paths do not encode a workspace, so Rimz can stamp the
    /// file once from live snapshot metadata and reuse that origin across cold
    /// re-parses.
    #[serde(default, rename = "p", skip_serializing_if = "Option::is_none")]
    pub origin_path: Option<PathBuf>,
    /// One nonzero token-usage entry per parsed transcript record. Retry-write
    /// duplicates within each parsed chunk collapse before disk_usage; aggregation
    /// still owns cross-file dedup.
    #[serde(default, rename = "e", skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<CachedEntry>,
    /// Price lookup misses observed while parsing this file, keyed by model and
    /// carrying the youngest timestamp seen for that model. The pricing refresh
    /// chase unions active unknowns across currently discovered files. When one
    /// later resolves while still inside the widest spend window, this file cold
    /// re-parses so zero-dollar token entries recover their spend.
    #[serde(default, rename = "u", skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown_models: BTreeMap<String, u64>,
}

/// Where an incremental spend parse left off: the byte offset just past the
/// last consumed line, plus the adapter's opaque cross-line state (Codex
/// carries its cumulative token totals and tracked model so a resumed delta
/// subtraction stays exact). Stored per file in the spending cache; a state
/// shape change bumps [`SPENDING_CACHE_VERSION`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpendCursor {
    #[serde(default, rename = "o", skip_serializing_if = "is_zero")]
    pub offset: u64,
    #[serde(default, rename = "s", skip_serializing_if = "Option::is_none")]
    pub state: Option<serde_json::Value>,
}

/// One spend parse: the entries read past the resume point, the single
/// workspace origin observed for that parsed slice, and the cursor the cache
/// stores for the next pass. A parser whose append-only log contains
/// invalidation markers can request authoritative replacement after a cold
/// fold instead of appending a suffix to stale cached entries.
#[derive(Debug, Default)]
pub struct SpendParse {
    pub entries: Vec<CachedEntry>,
    pub origin: Option<PathBuf>,
    pub cursor: SpendCursor,
    pub unknown_models: BTreeMap<String, u64>,
    /// Replace this file's cached entries and unknown-model set instead of
    /// appending. Rewindable transcripts use this after an authoritative cold fold.
    pub replace_entries: bool,
}

/// A single cost entry with dedup keys for cross-file deduplication.
///
/// `message_id` and `request_id` are present for Claude entries and absent for
/// Codex and Pi entries. `thread_id` is present when the provider's durable
/// transcript store exposes a native session id. `is_sidechain` drives the
/// sidechain-replay suppression logic in the spending walk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CachedEntry {
    /// Unix timestamp (seconds) the entry was recorded, parsed from the JSONL
    /// `timestamp` via [`iso_to_unix_secs`]. Drives the trailing-window bucketing
    /// in [`accum`].
    #[serde(rename = "t")]
    pub ts_secs: u64,
    #[serde(rename = "u")]
    pub cost_usd: f64,
    /// Fresh input tokens (Claude `input_tokens`; Codex uncached input).
    /// Per-entry components stay raw; [`SpendWindow::add`] folds `cache_write`
    /// into aggregate input/total. `#[serde(default)]` keeps an older cache
    /// parseable; `SPENDING_CACHE_VERSION` is what actually heals it — a
    /// pre-split cache is discarded on read so every file re-parses, since a
    /// finalized session's stable mtime would otherwise pin these at `0`.
    #[serde(default, rename = "i", skip_serializing_if = "is_zero")]
    pub input: u64,
    /// Output tokens (Codex `output_tokens` already includes reasoning).
    #[serde(default, rename = "o", skip_serializing_if = "is_zero")]
    pub output: u64,
    /// Cache-write tokens (Claude `cache_creation_input_tokens`, Pi
    /// `cacheWrite`); `0` for providers with no cache-creation concept (Codex).
    #[serde(default, rename = "w", skip_serializing_if = "is_zero")]
    pub cache_write: u64,
    /// Cache-read (Claude `cache_read_input_tokens`; Codex `cached_input_tokens`).
    #[serde(default, rename = "r", skip_serializing_if = "is_zero")]
    pub cache_read: u64,
    /// `message.id` from Claude entries; `None` for Codex and Pi entries.
    #[serde(default, rename = "m", skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// `requestId` from Claude entries; `None` for Codex and Pi entries.
    #[serde(default, rename = "q", skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Provider-native billing thread/session id, used for stores where many
    /// sessions live in one transcript file or database.
    #[serde(default, rename = "h", skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// `isSidechain` flag from Claude entries.
    #[serde(default, rename = "s", skip_serializing_if = "is_false")]
    pub is_sidechain: bool,
    /// Model id as the transcript named it (`claude-opus-4-8`, `gpt-5-codex`, …),
    /// kept for the per-model token breakdown. `None` for an entry whose
    /// transcript named no model. Carried through dedup so a kept turn keeps its
    /// model.
    #[serde(default, rename = "l", skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Synthetic per-day rollup produced by recency compaction. Rolled rows
    /// carry no dedup IDs because compaction runs after cross-file dedup.
    /// Native thread ids are retained so old multi-session stores keep session
    /// counts exact.
    #[serde(default, rename = "d", skip_serializing_if = "is_false")]
    pub rolled: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn is_default_cursor(cursor: &SpendCursor) -> bool {
    cursor == &SpendCursor::default()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CacheStamp {
    mtime: SystemTime,
    len: u64,
}

pub(crate) fn compact_spending_cache(
    cache: &mut SpendingDiskCache,
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    now_secs: u64,
) -> bool {
    let retained_message_ids = retained_raw_message_ids(cache, files, now_secs);
    let mut rolled_by_file = BTreeMap::<String, BTreeMap<RollupKey, CachedEntry>>::new();
    for source in old_counted_entries_for_compaction(cache, files, now_secs, &retained_message_ids)
    {
        if within_widest_window(source.entry.ts_secs, now_secs) {
            merge_rollup(
                rolled_by_file.entry(source.file_key).or_default(),
                rolled_entry_from(&source.entry),
            );
        }
    }

    let mut changed = false;
    for (_, file) in files {
        let file_key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get_mut(&file_key) else {
            continue;
        };
        let before_len = cached_file.entries.len();
        let mut retained = Vec::with_capacity(cached_file.entries.len());
        let mut retained_rollups = BTreeMap::<RollupKey, CachedEntry>::new();
        for entry in cached_file.entries.drain(..) {
            if entry.rolled {
                if within_widest_window(entry.ts_secs, now_secs) {
                    merge_rollup(&mut retained_rollups, entry);
                } else {
                    changed = true;
                }
            } else if raw_entry_ready_for_compaction(&entry, now_secs, &retained_message_ids) {
                changed = true;
            } else {
                retained.push(entry);
            }
        }
        if let Some(new_rollups) = rolled_by_file.remove(&file_key) {
            for rollup in new_rollups.into_values() {
                merge_rollup(&mut retained_rollups, rollup);
            }
        }
        retained.extend(retained_rollups.into_values());
        changed |= retained.len() != before_len;
        cached_file.entries = retained;
    }
    let before = cache.files.len();
    // Parsed entry and unknown-model timestamps are not newer than the file mtime
    // in real transcript stores; once mtime is past the widest window plus skew
    // margin, the record can no longer affect totals, sessions, or price chases.
    cache
        .files
        .retain(|_, file| !cold_parse_out_of_window(file.mtime_secs, now_secs));
    changed |= cache.files.len() != before;
    changed
}

fn retained_raw_message_ids(
    cache: &SpendingDiskCache,
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    now_secs: u64,
) -> BTreeSet<String> {
    let mut message_ids = BTreeSet::new();
    for (_, file) in files {
        let file_key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get(&file_key) else {
            continue;
        };
        message_ids.extend(
            cached_file
                .entries
                .iter()
                .filter(|entry| !entry.rolled && within_raw_retain_window(entry.ts_secs, now_secs))
                .filter_map(|entry| entry.message_id.clone()),
        );
    }
    message_ids
}

fn raw_entry_ready_for_compaction(
    entry: &CachedEntry,
    now_secs: u64,
    retained_message_ids: &BTreeSet<String>,
) -> bool {
    !entry.rolled
        && !within_raw_retain_window(entry.ts_secs, now_secs)
        && entry
            .message_id
            .as_ref()
            .is_none_or(|message_id| !retained_message_ids.contains(message_id))
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RollupKey {
    day: u64,
    model: Option<String>,
    thread_id: Option<String>,
}

impl RollupKey {
    fn from_entry(entry: &CachedEntry) -> Self {
        Self {
            day: entry.ts_secs / 86_400,
            model: entry.model.clone(),
            thread_id: entry.thread_id.clone(),
        }
    }
}

#[derive(Clone)]
struct CompactionSourceEntry {
    file_key: String,
    entry: CachedEntry,
}

impl DedupPayload for CompactionSourceEntry {
    fn entry(&self) -> &CachedEntry {
        &self.entry
    }
}

fn old_counted_entries_for_compaction(
    cache: &SpendingDiskCache,
    files: &[(&'static dyn AgentAdapter, PathBuf)],
    now_secs: u64,
    retained_message_ids: &BTreeSet<String>,
) -> Vec<CompactionSourceEntry> {
    let mut deduped = SidechainDedup::default();
    for (_, file) in files {
        let file_key = file.to_string_lossy().into_owned();
        let Some(cached_file) = cache.files.get(&file_key) else {
            continue;
        };
        for entry in cached_file
            .entries
            .iter()
            .filter(|entry| raw_entry_ready_for_compaction(entry, now_secs, retained_message_ids))
        {
            deduped.insert(CompactionSourceEntry {
                file_key: file_key.clone(),
                entry: entry.clone(),
            });
        }
    }
    deduped.into_counted()
}

fn rolled_entry_from(entry: &CachedEntry) -> CachedEntry {
    CachedEntry {
        ts_secs: entry.ts_secs,
        cost_usd: entry.cost_usd,
        input: entry.input,
        output: entry.output,
        cache_write: entry.cache_write,
        cache_read: entry.cache_read,
        message_id: None,
        request_id: None,
        thread_id: entry.thread_id.clone(),
        is_sidechain: false,
        model: entry.model.clone(),
        rolled: true,
    }
}

fn merge_rollup(rollups: &mut BTreeMap<RollupKey, CachedEntry>, entry: CachedEntry) {
    let key = RollupKey::from_entry(&entry);
    rollups
        .entry(key)
        .and_modify(|rolled| {
            rolled.ts_secs = rolled.ts_secs.max(entry.ts_secs);
            rolled.cost_usd += entry.cost_usd;
            rolled.input += entry.input;
            rolled.output += entry.output;
            rolled.cache_write += entry.cache_write;
            rolled.cache_read += entry.cache_read;
        })
        .or_insert(entry);
}

pub(crate) fn file_stat(path: &Path) -> (u64, u64) {
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

pub(crate) fn cache_stamp(path: &Path) -> Option<CacheStamp> {
    let meta = fs::metadata(path).ok()?;
    Some(CacheStamp {
        mtime: meta.modified().ok()?,
        len: meta.len(),
    })
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

/// The leading `version` of an on-disk cache without parsing the body.
///
/// Every shared-cache struct serializes `version` first, so a short prefix read
/// settles it even for the multi-megabyte cursor cache. `None` means the file
/// is absent, empty, or carries no readable leading version.
pub(crate) fn peek_cache_version(path: &Path) -> Option<u32> {
    let mut file = fs::File::open(path).ok()?;
    let mut buf = [0_u8; 512];
    let read = file.read(&mut buf).ok()?;
    parse_cache_version_prefix(&buf[..read])
}

fn parse_cache_version_prefix(bytes: &[u8]) -> Option<u32> {
    let key = b"\"version\"";
    let mut rest = skip_json_ws(bytes);
    rest = rest.strip_prefix(b"{")?;
    rest = skip_json_ws(rest);
    rest = rest.strip_prefix(key)?;
    rest = rest.strip_prefix(b":")?;
    rest = skip_json_ws(rest);
    let end = rest
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    std::str::from_utf8(&rest[..end]).ok()?.parse().ok()
}

fn skip_json_ws(bytes: &[u8]) -> &[u8] {
    let skipped = bytes
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        .unwrap_or(bytes.len());
    &bytes[skipped..]
}

/// Atomic write: temp file + rename, matching the project's store durability
/// contract.
pub fn write_spending_cache(path: &Path, cache: &SpendingDiskCache) -> bool {
    if let Some(on_disk) = peek_cache_version(path)
        && on_disk > cache.version
    {
        debug!(
            path = %path.display(),
            on_disk,
            ours = cache.version,
            "skip spending cursor downgrade"
        );
        return true;
    }
    let _ = crate::store::atomic::sweep_stale_temp_siblings(
        path,
        std::time::Duration::from_secs(3_600),
    );
    match crate::store::atomic::write_temp_then_rename_cache_compact(path, cache) {
        Ok(()) => true,
        Err(err) => {
            warn!(
                path = %path.display(),
                error = %err,
                "spending cursor cache write failed"
            );
            false
        }
    }
}
