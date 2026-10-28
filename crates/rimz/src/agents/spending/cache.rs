//! Cursor-cache types, disk I/O, file stamps, and raw-row compaction for spending walks.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

use serde::ser::SerializeMap as _;
use serde::{Deserialize, Serialize};
use serde_json::value::{RawValue, to_raw_value};
use tracing::{debug, warn};

use super::SpendingFile;
use crate::agents::TranscriptStat;
use crate::agents::pricing::TokenSplit;

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
/// OpenAI long-context pricing; finalized files need one cold reprice. v16
/// bills Codex cached input at the full input rate when a model carries no
/// explicit cache-read rate (ccusage parity), widens the Codex dedup
/// fingerprint with reasoning and total tokens, and applies explicit `±HH:MM`
/// timezone offsets when reading entry timestamps; each reshapes a stored cost,
/// dedup key, or timestamp, so finalized files need one cold reprice. v17 keeps
/// Pi token-only records, applies `totalTokens` fallback, and prices an absent
/// direct cost, which also requires a cold parse of finalized Pi sessions.
/// v18 corrects candidate/thought overlap in finalized Qwen transcripts, so
/// their historical output tokens and cost receive one cold reprice. v19
/// stores nanosecond-precise logical source stamps, including provider-owned
/// companions such as OpenCode's WAL, so main-file-only cursors rebuild once.
/// v20 locally prices Grok completions whose current wire omits native cost
/// ticks, so finalized sessions previously cached with no entries rebuild once.
/// v21 records per-tool call counts, so finalized supported-provider sessions
/// rebuild once to backfill their transcript history.
/// v22 removes locally fabricated pricing fallbacks, so finalized token-priced
/// sessions rebuild once against the upstream-only book. v23 applies Claude's
/// managed contracted rates, so finalized Claude sessions need one cold reprice.
/// v24 rejects Claude null fields only at their schema paths, so finalized
/// Claude sessions whose nested iteration `model` is null rebuild once.
/// v25 drops Grok child-session transcripts whose usage the parent already
/// folds, so finalized child sessions cached with priced entries rebuild once.
/// v26: `g` response timing (`m` ms, `o` output tokens) for the per-model tok/s figure.
/// v27 prices Codex turns after a priority `thread_settings_applied` at the
/// model's fast multiplier, so finalized Codex sessions reprice once.
/// Optional content anchors do not bump v27: legacy cursors retain their resume rule.
/// v28 records per-entry fast mode; finalized sessions rebuild once to backfill the flag.
pub(crate) const SPENDING_CACHE_VERSION: u32 = 28;

/// Output tokens and elapsed milliseconds for timed responses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseTiming {
    #[serde(rename = "m")]
    pub ms: u64,
    #[serde(rename = "o")]
    pub output: u64,
}

const MIN_TIMED_OUTPUT_TOKENS: u64 = 100;
const MIN_RESPONSE_MS: u64 = 200;
const MAX_RESPONSE_MS: u64 = 300_000;

pub(in crate::agents) fn response_timing(output: u64, ms: u64) -> Option<ResponseTiming> {
    (output >= MIN_TIMED_OUTPUT_TOKENS && ms > MIN_RESPONSE_MS && ms <= MAX_RESPONSE_MS)
        .then_some(ResponseTiming { ms, output })
}

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

/// Cached parse of one logical transcript or provider store.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileCacheEntry {
    /// Stat gate captured before the last parse. Companion-free changes can
    /// resume an anchored cursor; legacy cursors resume only on primary growth.
    #[serde(default, rename = "s")]
    pub stat: TranscriptStat,
    /// Where the last parse left off — the next incremental parse resumes here.
    #[serde(default, rename = "c", skip_serializing_if = "is_default_cursor")]
    pub cursor: SpendCursor,
    /// Durable per-file origin learned from the parser or a trusted override.
    /// Codex rollout paths do not encode a workspace, so RimZ can stamp the
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
/// last consumed line, plus the adapter's cross-line state, held as JSON text
/// and decoded on use (Codex carries its cumulative token totals and tracked
/// model so a resumed delta subtraction stays exact; Claude carries its
/// pricing fingerprint and the input and in-flight message timestamps).
/// Stored per file in the spending cache; a state shape change bumps
/// `SPENDING_CACHE_VERSION`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpendCursor {
    #[serde(default, rename = "o", skip_serializing_if = "is_zero")]
    pub offset: u64,
    /// CRC-32 of up to 4 KiB before the offset. Absent on legacy cursors and
    /// adapters without rewrite detection; a mismatched anchor forces a cold read.
    #[serde(default, rename = "a", skip_serializing_if = "Option::is_none")]
    pub anchor: Option<u32>,
    #[serde(default, rename = "s", skip_serializing_if = "Option::is_none")]
    state: Option<Box<RawValue>>,
}

impl SpendCursor {
    /// A stateless cursor at the next byte offset.
    pub fn new(offset: u64) -> Self {
        Self {
            offset,
            ..Self::default()
        }
    }

    /// Decode the adapter's cross-line state. The state was serialized by the
    /// same adapter under the current `SPENDING_CACHE_VERSION` (a shape change
    /// bumps it and cold-rebuilds), so a missing or odd value degrades to a
    /// fresh fold rather than failing the pass.
    pub fn state_as<T: serde::de::DeserializeOwned + Default>(&self) -> T {
        self.state
            .as_ref()
            .and_then(|value| serde_json::from_str(value.get()).ok())
            .unwrap_or_default()
    }

    /// Build the cursor an adapter hands back after folding to `offset`. The
    /// state passes through a `serde_json::Value` first, so its text keeps the
    /// sorted keys the cache has always written.
    pub fn with_state<T: Serialize>(offset: u64, state: &T) -> Self {
        Self {
            offset,
            anchor: None,
            state: serde_json::to_value(state)
                .ok()
                .and_then(|value| to_raw_value(&value).ok()),
        }
    }

    pub(in crate::agents) fn state(&self) -> Option<&RawValue> {
        self.state.as_deref()
    }
}

impl PartialEq for SpendCursor {
    fn eq(&self, other: &Self) -> bool {
        if self.offset != other.offset || self.anchor != other.anchor {
            return false;
        }
        match (&self.state, &other.state) {
            (None, None) => true,
            (Some(left), Some(right)) if left.get() == right.get() => true,
            (Some(left), Some(right)) => serde_json::from_str::<serde_json::Value>(left.get())
                .ok()
                .zip(serde_json::from_str::<serde_json::Value>(right.get()).ok())
                .is_some_and(|(left, right)| left == right),
            _ => false,
        }
    }
}

/// One spend parse: the entries read past the resume point, the single
/// workspace origin observed for that parsed slice, and the cursor the cache
/// stores for the next pass. A cold fold can request authoritative replacement
/// or a rewrite merge that preserves previously counted spend.
#[derive(Debug, Default)]
pub struct SpendParse {
    pub entries: Vec<CachedEntry>,
    pub origin: Option<PathBuf>,
    pub cursor: SpendCursor,
    pub unknown_models: BTreeMap<String, u64>,
    pub update: SpendUpdate,
}

/// How a parsed slice updates a previously cached file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpendUpdate {
    #[default]
    Extend,
    /// The source is authoritative, including removal of prior entries.
    Replace,
    /// Keep prior spend and merge unseen recent keys after a content-anchor mismatch.
    Rewritten,
}

// Before each insertion, evict names held only by the table: at most the distinct
// names live at the last insertion plus the new name remain retained.
static SHARED_NAMES: LazyLock<Mutex<HashSet<Arc<str>>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// A model or tool name shared process-wide across cache rows and parser results.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SharedName(Arc<str>);

impl From<&str> for SharedName {
    fn from(name: &str) -> Self {
        let mut names = SHARED_NAMES
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(shared) = names.get(name) {
            return Self(Arc::clone(shared));
        }
        names.retain(|shared| Arc::strong_count(shared) > 1);
        let shared = Arc::from(name);
        names.insert(Arc::clone(&shared));
        Self(shared)
    }
}

impl<'de> Deserialize<'de> for SharedName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NameVisitor;

        impl serde::de::Visitor<'_> for NameVisitor {
            type Value = SharedName;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a model or tool name")
            }

            fn visit_str<E: serde::de::Error>(self, name: &str) -> Result<Self::Value, E> {
                Ok(SharedName::from(name))
            }
        }

        deserializer.deserialize_str(NameVisitor)
    }
}

impl std::ops::Deref for SharedName {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for SharedName {
    fn as_ref(&self) -> &str {
        self
    }
}

impl fmt::Debug for SharedName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, formatter)
    }
}

/// Tool calls in byte order, with one name inline and multiple names in a boxed slice.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ToolCounts(ToolCountStorage);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum ToolCountStorage {
    #[default]
    Empty,
    One((SharedName, u32)),
    Many(Box<[(SharedName, u32)]>),
}

impl ToolCounts {
    pub fn is_empty(&self) -> bool {
        matches!(self.0, ToolCountStorage::Empty)
    }

    /// Count one more call, saturating at `u32::MAX`.
    pub fn increment(&mut self, name: &str) {
        self.add(name, 1);
    }

    /// Add calls to a name, saturating at `u32::MAX`.
    pub fn add(&mut self, name: &str, count: u32) {
        match self
            .as_slice()
            .binary_search_by(|(stored, _)| stored.as_ref().cmp(name))
        {
            Ok(index) => {
                let total = &mut self.as_mut_slice()[index].1;
                *total = total.saturating_add(count);
            }
            Err(index) => self.insert_at(index, (name.into(), count)),
        }
    }

    pub fn get(&self, name: &str) -> Option<&u32> {
        self.as_slice()
            .binary_search_by(|(stored, _)| stored.as_ref().cmp(name))
            .ok()
            .map(|index| &self.as_slice()[index].1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, u32)> {
        self.as_slice()
            .iter()
            .map(|(name, count)| (&**name, *count))
    }

    fn as_slice(&self) -> &[(SharedName, u32)] {
        match &self.0 {
            ToolCountStorage::Empty => &[],
            ToolCountStorage::One(entry) => std::slice::from_ref(entry),
            ToolCountStorage::Many(entries) => entries,
        }
    }

    fn as_mut_slice(&mut self) -> &mut [(SharedName, u32)] {
        match &mut self.0 {
            ToolCountStorage::Empty => &mut [],
            ToolCountStorage::One(entry) => std::slice::from_mut(entry),
            ToolCountStorage::Many(entries) => entries,
        }
    }

    fn insert(&mut self, name: SharedName, count: u32) {
        match self
            .as_slice()
            .binary_search_by(|(stored, _)| stored.cmp(&name))
        {
            Ok(index) => self.as_mut_slice()[index].1 = count,
            Err(index) => self.insert_at(index, (name, count)),
        }
    }

    fn insert_at(&mut self, index: usize, entry: (SharedName, u32)) {
        self.0 = match std::mem::take(&mut self.0) {
            ToolCountStorage::Empty => ToolCountStorage::One(entry),
            ToolCountStorage::One(previous) => ToolCountStorage::Many(if index == 0 {
                Box::new([entry, previous])
            } else {
                Box::new([previous, entry])
            }),
            ToolCountStorage::Many(entries) => {
                let mut entries = entries.into_vec();
                entries.insert(index, entry);
                ToolCountStorage::Many(entries.into_boxed_slice())
            }
        };
    }
}

impl<N: AsRef<str>> FromIterator<(N, u32)> for ToolCounts {
    fn from_iter<I: IntoIterator<Item = (N, u32)>>(iter: I) -> Self {
        let mut counts = Self::default();
        for (name, count) in iter {
            counts.insert(name.as_ref().into(), count);
        }
        counts
    }
}

impl From<BTreeMap<String, u32>> for ToolCounts {
    fn from(counts: BTreeMap<String, u32>) -> Self {
        counts.into_iter().collect()
    }
}

impl Serialize for ToolCounts {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.as_slice().len()))?;
        for (name, count) in self.iter() {
            map.serialize_entry(name, &count)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ToolCounts {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CountsVisitor;

        impl<'de> serde::de::Visitor<'de> for CountsVisitor {
            type Value = ToolCounts;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("tool names mapped to counts")
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut counts = ToolCounts::default();
                while let Some((name, count)) = map.next_entry()? {
                    counts.insert(name, count);
                }
                Ok(counts)
            }
        }

        deserializer.deserialize_map(CountsVisitor)
    }
}

/// A single cost entry with dedup keys for cross-file deduplication.
///
/// `message_id` and `request_id` identify Claude responses. `dedup_key`
/// identifies provider-namespaced records such as Codex events whose wire has
/// no message ID. `thread_id` is present when the provider's durable transcript
/// store exposes a native session id. `is_sidechain` drives the sidechain-replay
/// suppression logic in the spending walk.
/// Model and tool names share storage across rows; provider IDs are boxed strings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CachedEntry {
    /// Unix timestamp (seconds) the entry was recorded, parsed from the JSONL
    /// `timestamp` via `super::time::iso_to_unix_secs`. Drives local-day window bucketing
    /// in `super::aggregate::accum`.
    #[serde(rename = "t")]
    pub ts_secs: u64,
    #[serde(rename = "u")]
    pub cost_usd: f64,
    /// Fresh input tokens (Claude `input_tokens`; Codex uncached input).
    /// Per-entry components stay raw; `SpendWindow::add` folds `cache_write`
    /// into aggregate input/total. `#[serde(default)]` keeps an older cache
    /// parseable; `SPENDING_CACHE_VERSION` is what actually heals it — a
    /// pre-split cache is discarded on read so every file re-parses, since a
    /// finalized session's stable mtime would otherwise pin these at `0`.
    #[serde(default, rename = "i", skip_serializing_if = "is_zero")]
    pub input: u64,
    /// Output tokens (Codex `output_tokens` already includes reasoning).
    #[serde(default, rename = "o", skip_serializing_if = "is_zero")]
    pub output: u64,
    /// Timed-response counters, summed together when old rows compact.
    #[serde(default, rename = "g", skip_serializing_if = "Option::is_none")]
    pub timing: Option<ResponseTiming>,
    /// Cache-write tokens (Claude `cache_creation_input_tokens`, Codex
    /// `cache_write_input_tokens`, Pi `cacheWrite`).
    #[serde(default, rename = "w", skip_serializing_if = "is_zero")]
    pub cache_write: u64,
    /// Cache-read (Claude `cache_read_input_tokens`; Codex `cached_input_tokens`).
    #[serde(default, rename = "r", skip_serializing_if = "is_zero")]
    pub cache_read: u64,
    /// Named tool calls attributed to this provider response.
    #[serde(default, rename = "n", skip_serializing_if = "ToolCounts::is_empty")]
    pub tool_calls: ToolCounts,
    /// `message.id` from Claude entries; `None` for Codex and Pi entries.
    #[serde(default, rename = "m", skip_serializing_if = "Option::is_none")]
    pub message_id: Option<Box<str>>,
    /// `requestId` from Claude entries; `None` for Codex and Pi entries.
    #[serde(default, rename = "q", skip_serializing_if = "Option::is_none")]
    pub request_id: Option<Box<str>>,
    /// Provider-namespaced exact-record key for wires without message IDs.
    #[serde(default, rename = "k", skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<Box<str>>,
    /// Provider-native billing thread/session id, used for stores where many
    /// sessions live in one transcript file or database.
    #[serde(default, rename = "h", skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<Box<str>>,
    /// `isSidechain` flag from Claude entries.
    #[serde(default, rename = "s", skip_serializing_if = "is_false")]
    pub is_sidechain: bool,
    /// The source usage carried speed metadata, used to keep the richest Claude duplicate.
    #[serde(default, rename = "v", skip_serializing_if = "is_false")]
    pub has_speed: bool,
    /// Provider fast mode; compacted rows retain whether any counted turn was fast.
    #[serde(default, rename = "f", skip_serializing_if = "is_false")]
    pub fast: bool,
    /// Model id as the transcript named it (`claude-opus-4-8`, `gpt-5-codex`, …),
    /// kept for the per-model token breakdown. `None` for an entry whose
    /// transcript named no model. Carried through dedup so a kept turn keeps its
    /// model.
    #[serde(default, rename = "l", skip_serializing_if = "Option::is_none")]
    pub model: Option<SharedName>,
    /// Synthetic per-day rollup produced by recency compaction. Rolled rows
    /// carry no dedup IDs because compaction runs after cross-file dedup.
    /// Native thread ids are retained so old multi-session stores keep session
    /// counts exact.
    #[serde(default, rename = "d", skip_serializing_if = "is_false")]
    pub rolled: bool,
}

impl SpendParse {
    /// The no-progress result for a file that could not be read: hold the
    /// resume point and its state so the next pass resumes the fold rather than
    /// restarting it from a default at an offset already past the lines that
    /// produced it.
    pub fn stalled(resume: Option<&SpendCursor>) -> Self {
        Self {
            cursor: resume.cloned().unwrap_or_default(),
            ..Self::default()
        }
    }
}

impl CachedEntry {
    /// One priced entry from a token split, retaining its fast flag. Cache-creation
    /// tokens are stored summed: the 5-minute/1-hour distinction is a pricing input,
    /// never a stored fact. Provider-specific identity — model, dedup keys, thread id,
    /// sidechain and speed-metadata presence — is filled by the adapter through struct
    /// update.
    pub fn new(ts_secs: u64, cost_usd: f64, split: &TokenSplit) -> Self {
        Self {
            ts_secs,
            cost_usd,
            input: split.input,
            output: split.output,
            cache_write: split.cache_write.saturating_add(split.cache_write_1h),
            cache_read: split.cache_read,
            fast: split.fast,
            ..Self::default()
        }
    }
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
    files: &[SpendingFile],
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
    for SpendingFile { path: file, .. } in files {
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
    // Parsed entry and unknown-model timestamps are not newer than the logical
    // source mtime in real transcript stores; once its newest participant is
    // past the widest window plus skew margin, the record can no longer affect
    // totals, sessions, or price chases.
    cache
        .files
        .retain(|_, file| !cold_parse_out_of_window(file.stat.newest_mtime_secs(), now_secs));
    changed |= cache.files.len() != before;
    for file in cache.files.values_mut() {
        file.entries.shrink_to_fit();
    }
    cache.files.shrink_to_fit();
    changed
}

fn retained_raw_message_ids(
    cache: &SpendingDiskCache,
    files: &[SpendingFile],
    now_secs: u64,
) -> BTreeSet<Box<str>> {
    let mut message_ids = BTreeSet::new();
    for SpendingFile { path: file, .. } in files {
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
    retained_message_ids: &BTreeSet<Box<str>>,
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
    model: Option<SharedName>,
    thread_id: Option<Box<str>>,
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
    files: &[SpendingFile],
    now_secs: u64,
    retained_message_ids: &BTreeSet<Box<str>>,
) -> Vec<CompactionSourceEntry> {
    let mut deduped = SidechainDedup::default();
    for SpendingFile { path: file, .. } in files {
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
        timing: entry.timing,
        cache_write: entry.cache_write,
        cache_read: entry.cache_read,
        tool_calls: entry.tool_calls.clone(),
        message_id: None,
        request_id: None,
        dedup_key: None,
        thread_id: entry.thread_id.clone(),
        is_sidechain: false,
        has_speed: false,
        fast: entry.fast,
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
            rolled.fast |= entry.fast;
            if let Some(timing) = entry.timing {
                let total = rolled
                    .timing
                    .get_or_insert(ResponseTiming { ms: 0, output: 0 });
                total.ms += timing.ms;
                total.output += timing.output;
            }
            for (name, count) in entry.tool_calls.iter() {
                rolled.tool_calls.add(name, count);
            }
        })
        .or_insert(entry);
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
    let _ =
        crate::disk::atomic::sweep_stale_temp_siblings(path, std::time::Duration::from_secs(3_600));
    match crate::disk::atomic::write_temp_then_rename_cache_compact(path, cache) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_name_construction_shares_allocation_and_text_semantics() {
        let first = SharedName::from("Read");
        let second = SharedName::from("Read");
        assert!(Arc::ptr_eq(&first.0, &second.0));
        assert_eq!(serde_json::to_string(&first).unwrap(), r#""Read""#);
        assert_eq!(format!("{first:?}"), r#""Read""#);
        assert_eq!(HashSet::from([first, second]).len(), 1);
        let mut names = ["bash", "検索", "Bash"].map(SharedName::from);
        names.sort();
        assert_eq!(
            names.each_ref().map(|name| &**name),
            ["Bash", "bash", "検索"]
        );
    }

    #[test]
    fn decoded_models_share_allocation_including_escaped_names() {
        let rows: Vec<CachedEntry> = serde_json::from_str(concat!(
            r#"[{"t":1,"u":0.0,"l":"modèle"},"#,
            r#"{"t":2,"u":0.0,"l":"mod\u00e8le"}]"#,
        ))
        .unwrap();
        let first = rows[0].model.as_ref().unwrap();
        let second = rows[1].model.as_ref().unwrap();
        assert!(Arc::ptr_eq(&first.0, &second.0));
    }

    #[test]
    fn shared_name_table_prunes_unreferenced_names_on_the_next_miss() {
        let unused = SharedName::from("unused");
        let active = SharedName::from("active");
        let weak = Arc::downgrade(&unused.0);
        assert!(SHARED_NAMES.lock().unwrap().contains("unused"));
        drop(unused);
        let hit = SharedName::from("active");
        assert!(weak.upgrade().is_some());
        assert!(Arc::ptr_eq(&active.0, &hit.0));
        let _next = SharedName::from("next");
        assert!(!SHARED_NAMES.lock().unwrap().contains("unused"));
        assert!(weak.upgrade().is_none());
        assert!(SHARED_NAMES.lock().unwrap().contains("active"));
    }

    #[test]
    fn tool_counts_decode_one_inline_and_skip_empty_rows() {
        let row: CachedEntry = serde_json::from_str(r#"{"t":1,"u":0.0,"n":{"Read":2}}"#).unwrap();
        assert!(matches!(row.tool_calls.0, ToolCountStorage::One((_, _))));
        assert_eq!(row.tool_calls.iter().collect::<Vec<_>>(), [("Read", 2)]);
        let empty: CachedEntry = serde_json::from_str(r#"{"t":1,"u":0.0}"#).unwrap();
        assert!(matches!(empty.tool_calls.0, ToolCountStorage::Empty));
        assert!(empty.tool_calls.is_empty());
        assert!(serde_json::to_value(empty).unwrap().get("n").is_none());
    }

    #[test]
    fn tool_counts_decode_many_in_byte_order() {
        let tools: ToolCounts = serde_json::from_str(r#"{"bash":2,"検索":3,"Bash":1}"#).unwrap();
        assert!(matches!(tools.0, ToolCountStorage::Many(_)));
        assert_eq!(
            tools.iter().collect::<Vec<_>>(),
            [("Bash", 1), ("bash", 2), ("検索", 3)]
        );
        assert_eq!(
            serde_json::to_string(&tools).unwrap(),
            r#"{"Bash":1,"bash":2,"検索":3}"#
        );
    }

    #[test]
    fn tool_counts_duplicate_keys_keep_the_last_value_in_canonical_storage() {
        let tools: ToolCounts = serde_json::from_str(r#"{"Read":7,"Read":0}"#).unwrap();
        assert!(matches!(tools.0, ToolCountStorage::One((_, _))));
        assert_eq!(tools.get("Read"), Some(&0));
        assert_eq!(serde_json::to_string(&tools).unwrap(), r#"{"Read":0}"#);
        assert_eq!(
            tools,
            [("Read", 7), ("Read", 0)]
                .into_iter()
                .collect::<ToolCounts>()
        );
        assert_eq!(
            tools,
            ToolCounts::from(BTreeMap::from([("Read".to_owned(), 0)]))
        );
    }

    #[test]
    fn tool_counts_increment_and_add_preserve_sorted_union() {
        let mut tools: ToolCounts = [("Read", 1)].into_iter().collect();
        tools.increment("Read");
        tools.add("Read", 3);
        tools.increment("Bash");
        assert_eq!(tools.iter().collect::<Vec<_>>(), [("Bash", 1), ("Read", 5)]);
    }

    #[test]
    fn tool_counts_increment_and_add_saturate() {
        let mut incremented: ToolCounts = [("Read", u32::MAX - 1)].into_iter().collect();
        incremented.increment("Read");
        incremented.increment("Read");
        let mut added: ToolCounts = [("Read", u32::MAX - 1)].into_iter().collect();
        added.add("Read", 2);
        assert_eq!(
            [incremented.get("Read").copied(), added.get("Read").copied()],
            [Some(u32::MAX), Some(u32::MAX)]
        );
    }

    #[test]
    fn rollup_tool_counts_sum_shared_names_and_sorted_union() {
        let mut rollups = BTreeMap::new();
        for tools in [
            [("Bash", 1), ("Read", u32::MAX - 1), ("Zed", 4)],
            [("Bash", 2), ("Read", 2), ("bash", 3)],
        ] {
            merge_rollup(
                &mut rollups,
                CachedEntry {
                    tool_calls: tools.into_iter().collect(),
                    rolled: true,
                    ..CachedEntry::default()
                },
            );
        }
        let rolled = rollups.into_values().next().unwrap();
        assert_eq!(
            rolled.tool_calls.iter().collect::<Vec<_>>(),
            [("Bash", 3), ("Read", u32::MAX), ("Zed", 4), ("bash", 3)]
        );
    }

    #[test]
    fn cursor_decode_retains_state_text_verbatim() {
        let source = r#"{"o":7,"s":{ "z":2, "a":1 }}"#;
        let cursor: SpendCursor = serde_json::from_str(source).unwrap();
        assert_eq!(serde_json::to_string(&cursor).unwrap(), source);
    }

    #[test]
    fn cursor_with_state_canonicalizes_struct_fields_and_handles_serialization_failure() {
        #[derive(Serialize)]
        struct State {
            z: u32,
            a: u32,
        }
        let cursor = SpendCursor::with_state(7, &State { z: 2, a: 1 });
        assert_eq!(
            serde_json::to_string(&cursor).unwrap(),
            r#"{"o":7,"s":{"a":1,"z":2}}"#
        );
        assert_eq!(
            cursor.state_as::<serde_json::Value>(),
            serde_json::json!({"a":1,"z":2})
        );
        let failed = SpendCursor::with_state(7, &BTreeMap::from([((1, 2), 3)]));
        assert_eq!(serde_json::to_string(&failed).unwrap(), r#"{"o":7}"#);
        assert!(failed.state_as::<BTreeMap<String, u32>>().is_empty());
        let wrong_shape = SpendCursor::with_state(7, &"not an object");
        assert!(wrong_shape.state_as::<BTreeMap<String, u32>>().is_empty());
    }

    #[test]
    fn cursor_equality_compares_json_values_offset_and_anchor() {
        let mut canonical = SpendCursor::with_state(7, &serde_json::json!({"a":1,"z":2}));
        canonical.anchor = Some(123);
        let pretty: SpendCursor =
            serde_json::from_str(r#"{"o":7,"a":123,"s":{ "z":2, "a":1 }}"#).unwrap();
        assert_eq!(canonical, pretty);
        assert_eq!(pretty, pretty.clone());
        let mut different = pretty.clone();
        different.offset = 8;
        assert_ne!(canonical, different);
        different.offset = 7;
        different.anchor = None;
        assert_ne!(canonical, different);
        let changed: SpendCursor =
            serde_json::from_str(r#"{"o":7,"a":123,"s":{"a":2,"z":2}}"#).unwrap();
        assert_ne!(canonical, changed);
        let absent: SpendCursor = serde_json::from_str(r#"{"o":7,"a":123}"#).unwrap();
        assert_ne!(canonical, absent);
    }

    #[test]
    fn cursor_default_skip_distinguishes_offset_anchor_and_state() {
        let skipped: Vec<_> = [r#"{}"#, r#"{"o":1}"#, r#"{"a":0}"#, r#"{"s":{}}"#]
            .into_iter()
            .map(|source| {
                let file = FileCacheEntry {
                    stat: TranscriptStat::default(),
                    cursor: serde_json::from_str(source).unwrap(),
                    origin_path: None,
                    entries: Vec::new(),
                    unknown_models: BTreeMap::new(),
                };
                serde_json::to_value(file).unwrap().get("c").is_none()
            })
            .collect();
        assert_eq!(skipped, [true, false, false, false]);
    }
}
