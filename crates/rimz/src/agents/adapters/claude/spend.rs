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
//! Fast pre-filter: only usage lines and top-level user lines (the response
//! timing starts) are deserialized; other lines are skipped, as are lines where
//! certain schema paths carry `null` (rejected by the upstream TypeScript/Zod
//! schema).
//! Entries are returned raw. Batch consumers apply the shared
//! `SidechainDedup` policy in the fleet walk, seat fold, or per-session
//! selector; live folds retain a contiguous-duplicate window. An incremental
//! suffix parse only rereads the content anchor before its resume point.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agents::pricing::{PriceBook, TokenSplit};
use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, SpendUpdate, iso_to_unix_ms, iso_to_unix_secs,
    origin_path, price_split, response_timing,
};

use crate::agents::transcript_fs::{bytes_contains, read_anchored_transcript};

// ── Typed structs ─────────────────────────────────────────────────────────────

/// Full typed Claude usage entry.  Fields match the Claude Code JSONL schema
/// (`camelCase`; `costUSD` is an explicit serde rename).
///
/// Fields marked `reject_null` fail the whole entry on an explicit `null`, the
/// upstream TypeScript/Zod schema rejecting them as non-nullable (the same set
/// as ccusage's `has_unsupported_null_field`). The check binds to the schema
/// path, so a same-named nullable field nested elsewhere, such as
/// `message.usage.iterations[].model`, never drops the entry.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClaudeEntry {
    timestamp: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "costUSD", default, deserialize_with = "reject_null")]
    pub(super) cost_usd: Option<f64>,
    #[serde(default)]
    pub(super) message: ClaudeMessage,
    #[serde(default, deserialize_with = "reject_null")]
    pub(super) request_id: Option<String>,
    #[serde(default, deserialize_with = "reject_null")]
    session_id: Option<String>,
    #[serde(default, deserialize_with = "reject_null")]
    version: Option<String>,
    is_sidechain: Option<bool>,
    pub(super) agent_id: Option<String>,
    #[serde(
        rename = "isApiErrorMessage",
        default,
        deserialize_with = "reject_null_any"
    )]
    _is_api_error_message: (),
}

#[derive(Default, Deserialize)]
pub(super) struct ClaudeMessage {
    /// Anthropic message ID (`msg-…`).  The dedup key alongside `requestId`.
    #[serde(default, deserialize_with = "reject_null")]
    pub(super) id: Option<String>,
    #[serde(default, deserialize_with = "reject_null")]
    pub(super) model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_claude_content")]
    content: Vec<ClaudeContentBlock>,
    #[serde(default, deserialize_with = "deserialize_message_usage")]
    pub(super) usage: ClaudeUsage,
}

#[derive(Deserialize)]
struct ClaudeContentBlock {
    #[serde(rename = "type")]
    kind: Option<String>,
    name: Option<String>,
}

fn deserialize_claude_content<'de, D>(deserializer: D) -> Result<Vec<ClaudeContentBlock>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    let Some(blocks) = value.as_array() else {
        return Ok(Vec::new());
    };
    Ok(blocks
        .iter()
        .filter_map(|block| serde_json::from_value(block.clone()).ok())
        .collect())
}

/// `message.usage`, rejecting the schema's non-nullable usage fields there only:
/// `ClaudeUsage` itself stays lenient because iterations flatten it. The usage
/// object is a handful of counts, so buffering it costs little.
fn deserialize_message_usage<'de, D>(deserializer: D) -> Result<ClaudeUsage, D::Error>
where
    D: serde::Deserializer<'de>,
{
    const NON_NULLABLE: &[&str] = &[
        "speed",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ];
    let usage = serde_json::Value::deserialize(deserializer)?;
    if NON_NULLABLE
        .iter()
        .any(|field| usage.get(field).is_some_and(serde_json::Value::is_null))
    {
        return Err(serde::de::Error::custom("unsupported null usage field"));
    }
    ClaudeUsage::deserialize(usage).map_err(serde::de::Error::custom)
}

/// The `message.usage` token counts.  `Option` tolerates both an absent field
/// and an explicit `:null` (which the upstream schema can emit for the cache
/// fields), so a usage shape never drops an otherwise-valid cost entry.
#[derive(Default, Deserialize)]
pub(super) struct ClaudeUsage {
    #[serde(default, deserialize_with = "lenient_opt_u64")]
    input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "lenient_opt_u64")]
    output_tokens: Option<u64>,
    #[serde(default, deserialize_with = "lenient_opt_u64")]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "lenient_opt_u64")]
    cache_read_input_tokens: Option<u64>,
    pub(super) speed: Option<String>,
    #[serde(default)]
    cache_creation: Option<CacheCreation>,
    /// Separately billed nested calls such as Claude's advisor model. The
    /// top-level usage remains attributable to the main model.
    #[serde(default)]
    pub(super) iterations: Vec<ClaudeUsageIteration>,
}

#[derive(Default, Deserialize)]
pub(super) struct ClaudeUsageIteration {
    #[serde(rename = "type")]
    pub(super) kind: String,
    pub(super) model: Option<String>,
    #[serde(flatten)]
    pub(super) usage: ClaudeUsage,
}

#[derive(Default, Deserialize)]
pub(super) struct CacheCreation {
    #[serde(default, deserialize_with = "lenient_u64")]
    ephemeral_5m_input_tokens: u64,
    #[serde(default, deserialize_with = "lenient_u64")]
    ephemeral_1h_input_tokens: u64,
}

/// Deserialize a present field, failing on an explicit `null`; pair it with
/// `#[serde(default)]` so an absent field stays `None`.
fn reject_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)?
        .map(Some)
        .ok_or_else(|| serde::de::Error::custom("unsupported null field"))
}

/// [`reject_null`] for a field whose value is otherwise unused.
fn reject_null_any<'de, D>(deserializer: D) -> Result<(), D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<serde::de::IgnoredAny>::deserialize(deserializer)?
        .map(drop)
        .ok_or_else(|| serde::de::Error::custom("unsupported null field"))
}

/// Deserialize a token count leniently: a mistyped value (float, string, `null`,
/// or negative) becomes `None` rather than failing the whole record, so one odd
/// field never drops an otherwise-valid usage entry. Mirrors ccusage's lenient
/// JSONL deserializers (`Value::as_u64` coercion).
fn lenient_opt_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.as_ref().and_then(serde_json::Value::as_u64))
}

/// [`lenient_opt_u64`] for a non-optional count field, defaulting a mistyped or
/// absent value to `0`.
fn lenient_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(lenient_opt_u64(deserializer)?.unwrap_or(0))
}

// ── Path helpers ──────────────────────────────────────────────────────────────

/// Discover Claude config directories in priority order.
///
/// 1. `CLAUDE_CONFIG_DIR` env (comma-separated; each entry must have `projects/`)
/// 2. `$XDG_CONFIG_HOME/claude` / `~/.config/claude`
/// 3. `~/.claude`
///
/// Returns directories that actually have a `projects/` child.
pub(super) fn claude_config_dirs_from(login_env: &BTreeMap<String, String>) -> Vec<PathBuf> {
    let has_projects = |path: &PathBuf| path.join("projects").is_dir();
    let dirs = env_config_roots(login_env)
        .into_iter()
        .filter(has_projects)
        .collect::<Vec<_>>();
    if !dirs.is_empty() {
        return dirs;
    }
    default_config_roots(login_env)
        .into_iter()
        .filter(has_projects)
        .collect()
}

/// Config roots without filesystem filtering, for the process-local discovery
/// index to retain across a transiently unavailable provider home.
pub(super) fn claude_config_roots(login_env: &BTreeMap<String, String>) -> Vec<PathBuf> {
    let roots = env_config_roots(login_env);
    if roots.is_empty() {
        return default_config_roots(login_env);
    }
    roots
}

fn env_config_roots(login_env: &BTreeMap<String, String>) -> Vec<PathBuf> {
    login_env
        .get("CLAUDE_CONFIG_DIR")
        .into_iter()
        .flat_map(|env_val| env_val.split(','))
        .filter_map(|raw| env_config_dir(raw, login_env))
        .collect()
}

fn default_config_roots(login_env: &BTreeMap<String, String>) -> Vec<PathBuf> {
    let home = login_env
        .get("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let xdg = login_env
        .get("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    vec![xdg.join("claude"), home.join(".claude")]
}

fn env_config_dir(raw: &str, login_env: &BTreeMap<String, String>) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let home = login_env.get("HOME").map(String::as_str).unwrap_or("/");
    let path = if raw == "~" {
        PathBuf::from(home)
    } else if let Some(rest) = raw.strip_prefix("~/") {
        PathBuf::from(home).join(rest)
    } else {
        PathBuf::from(raw)
    };
    Some(if path.file_name().is_some_and(|name| name == "projects") {
        path.parent().map(Path::to_path_buf).unwrap_or(path)
    } else {
        path
    })
}

// ── Validation helpers ────────────────────────────────────────────────────────

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
    let mut segments = value.splitn(3, '.');
    let is_digits =
        |segment: &str| !segment.is_empty() && segment.as_bytes().iter().all(u8::is_ascii_digit);
    segments.next().is_some_and(is_digits)
        && segments.next().is_some_and(is_digits)
        && segments
            .next()
            .and_then(|segment| segment.as_bytes().first())
            .is_some_and(u8::is_ascii_digit)
}

// ── Parser ────────────────────────────────────────────────────────────────────

pub(super) fn priced_entry(line: &[u8]) -> Option<ClaudeEntry> {
    const USAGE_MARKER: &[u8] = br#""usage":{"#;

    if line.is_empty() || !bytes_contains(line, USAGE_MARKER) {
        return None;
    }
    serde_json::from_slice(line).ok()
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct ClaudeSpendState {
    pub(super) pricing_fingerprint: Option<String>,
    latest_user_ms: Option<u64>,
    current_message: Option<ClaudeResponseStart>,
}

#[derive(Serialize, Deserialize)]
struct ClaudeResponseStart {
    message_id: String,
    start_ms: Option<u64>,
}

#[derive(Deserialize)]
struct ClaudeInput {
    #[serde(rename = "type")]
    kind: String,
    timestamp: Option<String>,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<bool>,
}

/// Parse a Claude JSONL file into raw `CachedEntry` values, resuming from
/// the cursor's offset and timing state (`None` = the whole file). The latest
/// user timestamp and the in-flight message's fixed start survive suffix parses.
///
/// ### Fast pre-filter
/// Usage lines are priced; top-level user lines advance the latest input
/// timestamp, except sidechain (`isSidechain: true`) lines, which are skipped.
/// Other lines are skipped before deserialization.
/// Lines carrying `null` at a non-nullable schema path fail deserialization.
///
/// ### Dedup lives downstream
/// Entries are returned raw, duplicates and sidechain replays included. Batch
/// consumers apply the shared `(message.id, requestId)` policy in the fleet
/// walk, seat fold, or per-session selector; live folds retain a
/// contiguous-duplicate window. A suffix parse checks its content anchor;
/// a mismatch resets timing state and reports a rewrite for the consumer to merge.
pub fn parse_claude_spend(
    path: &Path,
    resume: Option<&SpendCursor>,
    prices: &PriceBook,
) -> SpendParse {
    let from_offset = resume.map_or(0, |cursor| cursor.offset);
    let Some(read) =
        read_anchored_transcript(path, from_offset, resume.and_then(|cursor| cursor.anchor))
    else {
        return SpendParse::stalled(resume);
    };
    if read.content.is_empty() && !read.rewritten && resume.is_some() {
        return SpendParse::stalled(resume);
    }
    let mut state: ClaudeSpendState = resume
        .filter(|_| !read.rewritten)
        .map(SpendCursor::state_as)
        .unwrap_or_default();
    state.pricing_fingerprint = super::managed_pricing::fingerprint_current();
    let mut entries: Vec<CachedEntry> = Vec::new();
    let mut origin = None;
    let mut unknown_models = BTreeMap::new();

    for line in read.content.split(|&b| b == b'\n') {
        if bytes_contains(line, br#""user""#)
            && let Ok(input) = serde_json::from_slice::<ClaudeInput>(line)
            && input.kind == "user"
        {
            if input.is_sidechain == Some(true) {
                continue;
            }
            if let Some(ms) = input.timestamp.as_deref().and_then(iso_to_unix_ms) {
                state.latest_user_ms = Some(state.latest_user_ms.map_or(ms, |prior| prior.max(ms)));
            }
            continue;
        }
        let Some(entry) = priced_entry(line) else {
            continue;
        };
        if !is_valid_claude_entry(&entry) {
            continue;
        }
        let Some(ts) = entry.timestamp.as_deref() else {
            continue;
        };
        let Some(ts_secs) = iso_to_unix_secs(ts) else {
            continue;
        };
        match entry.message.id.as_ref() {
            Some(message_id)
                if state
                    .current_message
                    .as_ref()
                    .is_none_or(|current| current.message_id != *message_id) =>
            {
                state.current_message = Some(ClaudeResponseStart {
                    message_id: message_id.clone(),
                    start_ms: state.latest_user_ms,
                });
            }
            None => state.current_message = None,
            _ => {}
        }
        let usage = &entry.message.usage;
        let Some(mut main_entry) = usage_entry(
            usage,
            entry.message.model.as_deref(),
            entry.cost_usd,
            UsageEntryMeta {
                ts_secs,
                message_id: entry.message.id.clone(),
                request_id: entry.request_id.clone(),
                is_sidechain: entry.is_sidechain == Some(true),
            },
            prices,
            &mut unknown_models,
        ) else {
            continue;
        };
        main_entry.timing = state
            .current_message
            .as_ref()
            .and_then(|current| current.start_ms)
            .and_then(|start| iso_to_unix_ms(ts)?.checked_sub(start))
            .and_then(|ms| response_timing(main_entry.output, ms));
        for name in entry
            .message
            .content
            .iter()
            .filter(|block| block.kind.as_deref() == Some("tool_use"))
            .filter_map(|block| block.name.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            main_entry.tool_calls.increment(name);
        }
        if origin.is_none() {
            origin = origin_path(entry.cwd.as_deref());
        }
        entries.push(main_entry);

        // Advisor calls are additional billable requests nested under the main
        // response's usage. They carry their own model and tokens but no
        // `costUSD`, so price them independently and give each a stable child
        // key under the parent response for cross-file replay deduplication.
        for (index, iteration) in usage
            .iterations
            .iter()
            .enumerate()
            .filter(|(_, iteration)| iteration.kind == "advisor_message")
        {
            let Some(model) = iteration
                .model
                .as_deref()
                .map(str::trim)
                .filter(|model| !model.is_empty())
            else {
                continue;
            };
            if let Some(advisor_entry) = usage_entry(
                &iteration.usage,
                Some(model),
                None,
                UsageEntryMeta {
                    ts_secs,
                    message_id: entry
                        .message
                        .id
                        .as_ref()
                        .map(|message_id| format!("{message_id}:advisor:{index}")),
                    request_id: entry.request_id.clone(),
                    is_sidechain: entry.is_sidechain == Some(true),
                },
                prices,
                &mut unknown_models,
            ) {
                entries.push(advisor_entry);
            }
        }
    }

    let mut cursor = SpendCursor::with_state(read.offset, &state);
    cursor.anchor = read.anchor;
    SpendParse {
        entries,
        origin,
        cursor,
        unknown_models,
        update: if read.rewritten {
            SpendUpdate::Rewritten
        } else {
            SpendUpdate::Extend
        },
    }
}

struct UsageEntryMeta {
    ts_secs: u64,
    message_id: Option<String>,
    request_id: Option<String>,
    is_sidechain: bool,
}

/// Price one usage object into a raw `CachedEntry`, or `None` when the entry
/// is skipped (all-zero tokens, or an unpriceable model without a logged cost).
fn usage_entry(
    usage: &ClaudeUsage,
    model: Option<&str>,
    logged_cost: Option<f64>,
    meta: UsageEntryMeta,
    prices: &PriceBook,
    unknown_models: &mut BTreeMap<String, u64>,
) -> Option<CachedEntry> {
    let split = request_split(usage);
    if split.is_empty() {
        return None;
    }

    // Cost source: a positive logged `costUSD` is authoritative (older
    // transcripts carried it); otherwise reconstruct spend from the token
    // usage through the model table, since current transcripts omit it. An
    // entry that has usage but no known model price still contributes tokens
    // and sessions with zero dollars while the unknown-model chase refreshes
    // pricing for the next producer pass; a name that is no model at all drops
    // the entry outright.
    let cost_usd = match logged_cost {
        Some(cost) if cost > 0.0 => cost,
        _ => price_split(prices, model?, split, meta.ts_secs, unknown_models)?,
    };

    Some(CachedEntry {
        message_id: meta.message_id.map(String::into_boxed_str),
        request_id: meta.request_id.map(String::into_boxed_str),
        is_sidechain: meta.is_sidechain,
        has_speed: usage.speed.is_some(),
        model: model.map(Into::into),
        ..CachedEntry::new(meta.ts_secs, cost_usd, &split)
    })
}

/// Normalize Claude's per-request usage into the pricing split shared by spend
/// aggregation and exact child-transcript pricing.
pub(super) fn request_split(usage: &ClaudeUsage) -> TokenSplit {
    let (cache_5m, cache_1h) = usage
        .cache_creation
        .as_ref()
        .map(|breakdown| {
            (
                breakdown.ephemeral_5m_input_tokens,
                breakdown.ephemeral_1h_input_tokens,
            )
        })
        .unwrap_or((usage.cache_creation_input_tokens.unwrap_or(0), 0));
    // Claude reports the four token components separately, and prices the two
    // cache-creation tiers at different rates; `input_tokens` is already the
    // fresh (uncached) slice. The stored entry carries the tiers summed.
    TokenSplit {
        input: usage.input_tokens.unwrap_or(0),
        output: usage.output_tokens.unwrap_or(0),
        cache_write: cache_5m,
        cache_write_1h: cache_1h,
        cache_read: usage.cache_read_input_tokens.unwrap_or(0),
        fast: usage.speed.as_deref() == Some("fast"),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
