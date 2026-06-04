//! Codex agent JSONL transcript parser.
//!
//! Codex JSONL records **token usage events** and carries **no** `costUSD`
//! field, so [`parse_codex_spend`] multiplies each [`CodexTokenEvent`] through
//! the [`pricing`](crate::agents::pricing) table to a USD cost. Discovery and
//! parsing stay pure and network-free.
//!
//! Codex session files live at `~/.codex/sessions/` (or `CODEX_HOME` env).
//!
//! Two log formats are handled:
//!
//! **Session format** — structured interactive-session log:
//! ```json
//! {"type":"event_msg","timestamp":"2026-01-01T10:00:00.000Z",
//!  "payload":{"type":"token_count","info":{
//!    "last_token_usage":{"input_tokens":100,"output_tokens":50},
//!    "total_token_usage":{"input_tokens":500,"output_tokens":200}
//!  }}}
//! {"type":"turn_context","payload":{"model":"gpt-5"}}
//! ```
//!
//! **Headless format** — exec/non-interactive log with a flat usage object:
//! ```json
//! {"usage":{"input_tokens":200,"output_tokens":80},"model":"gpt-5",
//!  "timestamp":"2026-01-01T10:00:00.000Z"}
//! ```
//!
//! `CodexRawUsage` normalizes field-name variants across providers that embed
//! Codex-compatible usage: `prompt_tokens`/`completion_tokens` (OpenAI),
//! `input`/`output` (compact), `cached_tokens`/`cached_input_tokens` (cache).

use std::borrow::Cow;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agents::pricing::PriceBook;
use crate::agents::spending::{CachedEntry, SpendCursor, SpendParse, iso_to_unix_secs};
use crate::agents::transcript_fs::{collect_jsonl, home_dir, read_spend_lines};

// ── Public output type ────────────────────────────────────────────────────────

/// A single Codex token-usage event, ready for pricing multiplication.
///
/// Produced by `parse_codex_session`.  Cost computation requires a pricing
/// table keyed on `model`.
#[derive(Debug, Clone)]
pub struct CodexTokenEvent {
    /// ISO-8601 / RFC-3339 timestamp string from the event.
    pub timestamp: String,
    /// Model name, resolved from the event payload and tracked `turn_context`.
    /// `None` only when the file contained no model hint at all.
    pub model: Option<String>,
    pub input_tokens: u64,
    /// Cached (prompt-cache-hit) input tokens, capped to `input_tokens`.
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
}

// ── Typed structs — session format ───────────────────────────────────────────

/// Codex session log entry — structural detection before deeper parsing.
///
/// Matches both `"type":"event_msg"` (carries `payload`) and
/// `"type":"turn_context"` (carries model identity).
#[derive(Deserialize)]
pub(crate) struct CodexSessionEntry<'a> {
    #[serde(rename = "type", borrow, default)]
    pub(crate) entry_type: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    pub(crate) timestamp: Option<CodexTimestamp<'a>>,
    #[serde(
        borrow,
        default,
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    pub(crate) payload: Option<CodexPayload<'a>>,
}

/// `event_msg` payload — `"type":"token_count"` entries carry usage info.
#[derive(Default, Deserialize)]
pub(crate) struct CodexPayload<'a> {
    #[serde(rename = "type", borrow, default)]
    pub(crate) payload_type: Option<Cow<'a, str>>,
    #[serde(
        borrow,
        default,
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    pub(crate) info: Option<CodexInfo<'a>>,
    #[serde(borrow, default)]
    pub(crate) model: Option<Cow<'a, str>>,
    #[serde(rename = "model_name", borrow, default)]
    pub(crate) model_name: Option<Cow<'a, str>>,
    #[serde(
        borrow,
        default,
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    pub(crate) metadata: Option<CodexModelMetadata<'a>>,
}

/// Token usage info nested inside the `token_count` payload.
#[derive(Default, Deserialize)]
pub(crate) struct CodexInfo<'a> {
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    pub(crate) last_token_usage: Option<CodexRawUsage>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    pub(crate) total_token_usage: Option<CodexRawUsage>,
    #[serde(borrow, default)]
    pub(crate) model: Option<Cow<'a, str>>,
    #[serde(rename = "model_name", borrow, default)]
    pub(crate) model_name: Option<Cow<'a, str>>,
    #[serde(
        borrow,
        default,
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    pub(crate) metadata: Option<CodexModelMetadata<'a>>,
}

/// Model metadata object that may appear at multiple nesting depths.
#[derive(Default, Deserialize)]
pub(crate) struct CodexModelMetadata<'a> {
    #[serde(borrow, default)]
    pub(crate) model: Option<Cow<'a, str>>,
}

// ── Typed structs — headless format ──────────────────────────────────────────

/// Headless / exec Codex log entry.
///
/// These entries appear in non-interactive runs and have usage at the top level
/// or nested under `data`, `result`, or `response`.  Multiple field-name
/// aliases exist across different executor versions.
#[derive(Deserialize)]
pub(crate) struct CodexLogEntry<'a> {
    #[serde(borrow, default)]
    pub(crate) timestamp: Option<CodexTimestamp<'a>>,
    #[serde(rename = "created_at", borrow, default)]
    pub(crate) created_at: Option<CodexTimestamp<'a>>,
    #[serde(rename = "createdAt", borrow, default)]
    pub(crate) created_at_camel: Option<CodexTimestamp<'a>>,
    #[serde(
        borrow,
        default,
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    pub(crate) data: Option<CodexResultFields<'a>>,
    #[serde(
        borrow,
        default,
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    pub(crate) result: Option<CodexResultFields<'a>>,
    #[serde(
        borrow,
        default,
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    pub(crate) response: Option<CodexResultFields<'a>>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    pub(crate) usage: Option<CodexRawUsage>,
    #[serde(borrow, default)]
    pub(crate) model: Option<Cow<'a, str>>,
    #[serde(rename = "model_name", borrow, default)]
    pub(crate) model_name: Option<Cow<'a, str>>,
    #[serde(
        borrow,
        default,
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    pub(crate) metadata: Option<CodexModelMetadata<'a>>,
}

/// Nested fields shared by `data`, `result`, and `response` wrappers.
#[derive(Default, Deserialize)]
pub(crate) struct CodexResultFields<'a> {
    #[serde(borrow, default)]
    pub(crate) timestamp: Option<CodexTimestamp<'a>>,
    #[serde(rename = "created_at", borrow, default)]
    pub(crate) created_at: Option<CodexTimestamp<'a>>,
    #[serde(rename = "createdAt", borrow, default)]
    pub(crate) created_at_camel: Option<CodexTimestamp<'a>>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    pub(crate) usage: Option<CodexRawUsage>,
    #[serde(borrow, default)]
    pub(crate) model: Option<Cow<'a, str>>,
    #[serde(rename = "model_name", borrow, default)]
    pub(crate) model_name: Option<Cow<'a, str>>,
    #[serde(
        borrow,
        default,
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    pub(crate) metadata: Option<CodexModelMetadata<'a>>,
}

// ── Timestamp ─────────────────────────────────────────────────────────────────

/// Codex timestamps appear as ISO-8601 strings or as Unix integers (seconds or
/// milliseconds).  The untagged enum deserializes either form.
#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum CodexTimestamp<'a> {
    String(Cow<'a, str>),
    Number(u64),
}

// ── Raw usage — custom Deserialize ───────────────────────────────────────────

/// Raw token counts from a Codex usage event.
///
/// Multiple field-name aliases are normalized at deserialization time:
/// - input:  `input_tokens` / `prompt_tokens` / `input`
/// - cached: `cached_input_tokens` / `cache_read_input_tokens` / `cached_tokens`
/// - output: `output_tokens` / `completion_tokens` / `output`
/// - reason: `reasoning_output_tokens` / `reasoning_tokens`
/// - total:  `total_tokens` (auto-summed if absent)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CodexRawUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
}

/// Internal helper struct that collects all field-name aliases before combining.
#[derive(Default, Deserialize)]
struct CodexRawUsageFields {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    prompt_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    input: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    cached_input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    cache_read_input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    cached_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    output_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    completion_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    output: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    reasoning_output_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    reasoning_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total_tokens: Option<u64>,
}

impl<'de> Deserialize<'de> for CodexRawUsage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let f = CodexRawUsageFields::deserialize(deserializer)?;
        let input = f.input_tokens.or(f.prompt_tokens).or(f.input).unwrap_or(0);
        let cached = f
            .cached_input_tokens
            .or(f.cache_read_input_tokens)
            .or(f.cached_tokens)
            .unwrap_or(0);
        let output = f
            .output_tokens
            .or(f.completion_tokens)
            .or(f.output)
            .unwrap_or(0);
        let reasoning = f
            .reasoning_output_tokens
            .or(f.reasoning_tokens)
            .unwrap_or(0);
        let computed = input + output + reasoning;
        Ok(Self {
            input_tokens: input,
            cached_input_tokens: cached,
            output_tokens: output,
            reasoning_output_tokens: reasoning,
            // Prefer explicit total_tokens when positive; fall back to computed sum.
            total_tokens: f
                .total_tokens
                .filter(|t| *t > 0 || computed == 0)
                .unwrap_or(computed),
        })
    }
}

// ── Serde helpers ─────────────────────────────────────────────────────────────

/// Deserialize `Option<T>` where the field may carry a non-object value.
///
/// JSON fields that are expected to be objects sometimes carry `null`, `true`,
/// an integer, or a string in Codex log variants.  This deserializer maps all
/// non-object values to `None` rather than returning an error, matching the
/// ccusage `deserialize_optional_object_lossy` pattern.
fn deserialize_optional_object_lossy<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    struct Visitor<T>(PhantomData<T>);

    impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
        type Value = Option<T>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an optional object")
        }

        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
            Ok(None)
        }
        fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            deserialize_optional_object_lossy(d)
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
            T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(Some)
        }
    }

    deserializer.deserialize_any(Visitor(PhantomData))
}

/// Deserialize `Option<u64>` where the field may carry a string, negative
/// number, or float.  Non-u64 values become `None`.
fn deserialize_optional_u64_lossy<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct U64Visitor;

    impl<'de> serde::de::Visitor<'de> for U64Visitor {
        type Value = Option<u64>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an optional unsigned integer")
        }

        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
        fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Ok(v.trim().parse::<u64>().ok())
        }
        fn visit_borrowed_str<E: serde::de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
            self.visit_str(v)
        }
        fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
            self.visit_str(&v)
        }
        fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            deserialize_optional_u64_lossy(d)
        }
    }

    deserializer.deserialize_any(U64Visitor)
}

// ── Path discovery ────────────────────────────────────────────────────────────

/// Collect all Codex session `*.jsonl` files from `~/.codex/sessions/`.
///
/// Respects `CODEX_HOME` (comma-separated) when set; appends `sessions/` when
/// the resolved path contains that subdirectory.
///
/// **Note:** Codex files are not scoped to a project directory — all sessions
/// are returned.  Computing USD cost from these files requires a pricing table.
pub fn codex_session_files() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(env_val) = std::env::var("CODEX_HOME") {
        for raw in env_val.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let p = PathBuf::from(raw);
            let sessions = p.join("sessions");
            if sessions.is_dir() {
                roots.push(sessions);
            } else if p.is_dir() {
                roots.push(p);
            }
        }
    } else {
        let candidate = home_dir().join(".codex/sessions");
        if candidate.is_dir() {
            roots.push(candidate);
        }
    }

    let mut files = Vec::new();
    for dir in &roots {
        collect_jsonl(dir, &mut files);
    }
    files.sort();
    files.dedup();
    files
}

// ── Line-kind detection ───────────────────────────────────────────────────────

/// Which JSONL format a line belongs to.
#[derive(Clone, Copy)]
enum CodexLineKind {
    /// Structured session log: `turn_context` or `event_msg` + `token_count`.
    Session,
    /// Headless/exec log: flat `usage`, `input_tokens`, or `prompt_tokens` field.
    Headless,
}

/// Classify a Codex JSONL line for parsing.
///
/// Returns `None` for lines that contain no relevant token-usage information.
fn codex_line_kind(line: &[u8]) -> Option<CodexLineKind> {
    fn has(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    let has_turn_ctx = has(line, br#""type":"turn_context""#);
    let has_event_msg = has(line, br#""type":"event_msg""#);
    let has_token_count = has(line, br#""type":"token_count""#);

    if has_turn_ctx || (has_event_msg && has_token_count) {
        return Some(CodexLineKind::Session);
    }

    // Headless format: usage object or individual token-count fields with no
    // event_msg wrapper.
    if !has_event_msg
        && (has(line, br#""usage":"#)
            || has(line, br#""input_tokens":"#)
            || has(line, br#""prompt_tokens":"#))
    {
        return Some(CodexLineKind::Headless);
    }

    None
}

// ── Parser ────────────────────────────────────────────────────────────────────

/// The cross-line parse state a resumed Codex parse restores, riding the
/// spending cache as the cursor's opaque `state`. Without it, a `token_count`
/// event carrying only the cumulative total would subtract against nothing
/// and record the whole session as one inflated delta.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CodexSpendState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_totals: Option<CodexRawUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_model: Option<String>,
}

/// Parse a Codex session JSONL file into `CodexTokenEvent` values from
/// `from_offset`, folding into (and advancing) `state`.
///
/// Handles both the **session format** (`event_msg` + `token_count` payload,
/// `turn_context` for model tracking) and the **headless format** (flat usage
/// fields).
///
/// ### Cumulative totals
/// Session format `token_count` payloads often carry both `last_token_usage`
/// (the delta) and `total_token_usage` (cumulative).  When only the cumulative
/// value is present, the delta is computed by subtracting the previous
/// cumulative total — the same strategy as ccusage's `subtract_codex_raw_usage`.
///
/// ### Model resolution
/// Checked in order: `payload.model` → `payload.model_name` →
/// `payload.metadata.model` → `info.model` → `info.model_name` →
/// `info.metadata.model` → remembered `current_model` → fallback `"gpt-5"`.
fn parse_codex_session(
    path: &Path,
    from_offset: u64,
    state: &mut CodexSpendState,
) -> (Vec<CodexTokenEvent>, u64) {
    let Some((content, next_offset)) = read_spend_lines(path, from_offset) else {
        return (Vec::new(), from_offset);
    };
    let fallback_timestamp = file_mtime_rfc3339(path);
    let mut out = Vec::new();

    for line in content.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        match codex_line_kind(line) {
            Some(CodexLineKind::Session) => {
                let Ok(entry) = serde_json::from_slice::<CodexSessionEntry<'_>>(line) else {
                    continue;
                };
                visit_session_entry(
                    entry,
                    &mut state.previous_totals,
                    &mut state.current_model,
                    &mut out,
                );
            }
            Some(CodexLineKind::Headless) => {
                if let Ok(entry) = serde_json::from_slice::<CodexLogEntry<'_>>(line) {
                    visit_headless_entry(
                        &entry,
                        &fallback_timestamp,
                        &mut state.current_model,
                        &mut out,
                    );
                }
            }
            None => {}
        }
    }
    (out, next_offset)
}

fn visit_session_entry(
    entry: CodexSessionEntry<'_>,
    previous_totals: &mut Option<CodexRawUsage>,
    current_model: &mut Option<String>,
    out: &mut Vec<CodexTokenEvent>,
) {
    let entry_type = entry.entry_type.as_deref();
    if entry_type == Some("turn_context") {
        if let Some(model) = entry.payload.as_ref().and_then(model_from_payload) {
            *current_model = Some(model);
        }
        return;
    }
    if entry_type != Some("event_msg") {
        return;
    }
    let Some(ts) = normalize_timestamp(entry.timestamp.as_ref()) else {
        return;
    };
    let Some(payload) = entry.payload.as_ref() else {
        return;
    };
    if payload.payload_type.as_deref() != Some("token_count") {
        return;
    }

    let info = payload.info.as_ref();
    let total_usage = info.and_then(|i| i.total_token_usage);
    let raw_usage = info
        .and_then(|i| i.last_token_usage)
        .or_else(|| total_usage.map(|total| subtract_raw_usage(&total, previous_totals.as_ref())));
    if let Some(total) = total_usage {
        *previous_totals = Some(total);
    }
    let Some(raw) = raw_usage else { return };
    if is_zero_usage(&raw) {
        return;
    }

    let parsed_model = model_from_payload(payload).or_else(|| info.and_then(model_from_info));
    if let Some(ref m) = parsed_model {
        *current_model = Some(m.clone());
    }
    let model = resolve_model(parsed_model, current_model);

    out.push(CodexTokenEvent {
        timestamp: ts,
        model,
        input_tokens: raw.input_tokens,
        cached_input_tokens: raw.cached_input_tokens.min(raw.input_tokens),
        output_tokens: raw.output_tokens,
    });
}

fn visit_headless_entry(
    entry: &CodexLogEntry<'_>,
    fallback_timestamp: &str,
    current_model: &mut Option<String>,
    out: &mut Vec<CodexTokenEvent>,
) {
    let Some(raw) = headless_usage(entry) else {
        return;
    };
    if is_zero_usage(&raw) {
        return;
    }
    let ts = headless_timestamp(entry).unwrap_or_else(|| fallback_timestamp.to_string());
    let parsed_model = headless_model(entry);
    if let Some(ref m) = parsed_model {
        *current_model = Some(m.clone());
    }
    let model = resolve_model(parsed_model, current_model);

    out.push(CodexTokenEvent {
        timestamp: ts,
        model,
        input_tokens: raw.input_tokens,
        cached_input_tokens: raw.cached_input_tokens.min(raw.input_tokens),
        output_tokens: raw.output_tokens,
    });
}

// ── Model resolution ──────────────────────────────────────────────────────────

fn model_from_payload(p: &CodexPayload<'_>) -> Option<String> {
    model_from_parts(p.model.as_ref(), p.model_name.as_ref(), p.metadata.as_ref())
}

fn model_from_info(i: &CodexInfo<'_>) -> Option<String> {
    model_from_parts(i.model.as_ref(), i.model_name.as_ref(), i.metadata.as_ref())
}

fn headless_model(e: &CodexLogEntry<'_>) -> Option<String> {
    model_from_parts(e.model.as_ref(), e.model_name.as_ref(), e.metadata.as_ref())
        .or_else(|| e.data.as_ref().and_then(model_from_result_fields))
        .or_else(|| e.result.as_ref().and_then(model_from_result_fields))
        .or_else(|| e.response.as_ref().and_then(model_from_result_fields))
}

fn model_from_result_fields(r: &CodexResultFields<'_>) -> Option<String> {
    model_from_parts(r.model.as_ref(), r.model_name.as_ref(), r.metadata.as_ref())
}

fn model_from_parts(
    model: Option<&Cow<'_, str>>,
    model_name: Option<&Cow<'_, str>>,
    metadata: Option<&CodexModelMetadata<'_>>,
) -> Option<String> {
    non_empty_cow(model)
        .or_else(|| non_empty_cow(model_name))
        .or_else(|| metadata.and_then(|m| non_empty_cow(m.model.as_ref())))
}

fn non_empty_cow(v: Option<&Cow<'_, str>>) -> Option<String> {
    v.and_then(|s| {
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_string())
    })
}

/// The event's model — the parsed hint, else the file's remembered model,
/// else the `"gpt-5"` ultimate fallback (remembered for later entries).
fn resolve_model(parsed: Option<String>, current_model: &mut Option<String>) -> Option<String> {
    parsed.or_else(|| current_model.clone()).or_else(|| {
        *current_model = Some("gpt-5".to_string());
        current_model.clone()
    })
}

// ── Usage helpers ─────────────────────────────────────────────────────────────

fn headless_usage(e: &CodexLogEntry<'_>) -> Option<CodexRawUsage> {
    e.usage
        .or_else(|| e.data.as_ref().and_then(|d| d.usage))
        .or_else(|| e.result.as_ref().and_then(|r| r.usage))
        .or_else(|| e.response.as_ref().and_then(|r| r.usage))
}

fn is_zero_usage(u: &CodexRawUsage) -> bool {
    u.input_tokens == 0
        && u.cached_input_tokens == 0
        && u.output_tokens == 0
        && u.reasoning_output_tokens == 0
}

/// Compute the delta between a cumulative total and the previous cumulative total.
fn subtract_raw_usage(current: &CodexRawUsage, previous: Option<&CodexRawUsage>) -> CodexRawUsage {
    let prev = previous.copied().unwrap_or_default();
    CodexRawUsage {
        input_tokens: current.input_tokens.saturating_sub(prev.input_tokens),
        cached_input_tokens: current
            .cached_input_tokens
            .saturating_sub(prev.cached_input_tokens),
        output_tokens: current.output_tokens.saturating_sub(prev.output_tokens),
        reasoning_output_tokens: current
            .reasoning_output_tokens
            .saturating_sub(prev.reasoning_output_tokens),
        total_tokens: current.total_tokens.saturating_sub(prev.total_tokens),
    }
}

// ── Timestamp helpers ─────────────────────────────────────────────────────────

fn normalize_timestamp(ts: Option<&CodexTimestamp<'_>>) -> Option<String> {
    match ts? {
        CodexTimestamp::String(s) => {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        }
        CodexTimestamp::Number(n) => {
            let millis = if *n > 10_000_000_000 {
                *n
            } else {
                n.checked_mul(1_000)?
            };
            Some(millis_to_rfc3339(millis))
        }
    }
}

fn headless_timestamp(e: &CodexLogEntry<'_>) -> Option<String> {
    normalize_timestamp(e.timestamp.as_ref())
        .or_else(|| normalize_timestamp(e.created_at.as_ref()))
        .or_else(|| normalize_timestamp(e.created_at_camel.as_ref()))
        .or_else(|| result_fields_timestamp(e.data.as_ref()))
        .or_else(|| result_fields_timestamp(e.result.as_ref()))
        .or_else(|| result_fields_timestamp(e.response.as_ref()))
}

fn result_fields_timestamp(r: Option<&CodexResultFields<'_>>) -> Option<String> {
    let r = r?;
    normalize_timestamp(r.timestamp.as_ref())
        .or_else(|| normalize_timestamp(r.created_at.as_ref()))
        .or_else(|| normalize_timestamp(r.created_at_camel.as_ref()))
}

fn millis_to_rfc3339(millis: u64) -> String {
    let secs = millis / 1_000;
    let frac_ms = millis % 1_000;
    let days = secs / 86_400;
    let time = secs % 86_400;
    let h = time / 3_600;
    let m = (time % 3_600) / 60;
    let s = time % 60;
    // Howard Hinnant's civil-from-days algorithm.
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{frac_ms:03}Z")
}

fn file_mtime_rfc3339(path: &Path) -> String {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| millis_to_rfc3339(d.as_millis().min(u64::MAX as u128) as u64))
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

// ── Path utilities ────────────────────────────────────────────────────────────

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Turn a Codex session's token events into priced [`CachedEntry`] values,
/// resuming from `resume` when given (the cursor's `state` restores the
/// cumulative-total and tracked-model fold exactly where it left off).
///
/// Codex logs token counts, not dollars, so each event is multiplied through
/// the price book: uncached input at the input rate, the cached slice at the
/// cache-read rate, and output (which already includes reasoning tokens) at the
/// output rate. The recorded `tokens` is `input + output` — the same `◇` total
/// the rest of the sidebar reads. Events whose model has no known price, or that
/// price to zero, are dropped. Codex entries carry no message/request IDs, so
/// they bypass the Claude dedup and bucket directly under the `codex` provider.
pub(crate) fn parse_codex_spend(
    path: &Path,
    resume: Option<&SpendCursor>,
    prices: &PriceBook,
) -> SpendParse {
    let from_offset = resume.map_or(0, |cursor| cursor.offset);
    // The state was serialized by this same code under the current
    // SPENDING_CACHE_VERSION (a shape change bumps it and cold-rebuilds), so a
    // missing/odd value degrades to a fresh fold rather than failing the pass.
    let mut state: CodexSpendState = resume
        .and_then(|cursor| cursor.state.clone())
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let (events, next_offset) = parse_codex_session(path, from_offset, &mut state);
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
        let Some(ts_secs) = iso_to_unix_secs(&event.timestamp) else {
            continue;
        };
        // Codex has no cache-creation concept: its cached slice is a read. The `◇`
        // total is fresh input + output, so `input` is the uncached slice and the
        // cached slice rides `cache_read`.
        out.push(CachedEntry {
            ts_secs,
            cost_usd: cost,
            input: uncached_input,
            output: event.output_tokens,
            cache_write: 0,
            cache_read: event.cached_input_tokens,
            message_id: None,
            request_id: None,
            is_sidechain: false,
        });
    }
    SpendParse {
        entries: out,
        cursor: SpendCursor {
            offset: next_offset,
            state: serde_json::to_value(&state).ok(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line classifier feeding `parse_codex_session` — exercised here
    /// through the kind probe so each accepted/skipped shape stays pinned.
    fn token_line(line: &[u8]) -> bool {
        codex_line_kind(line).is_some()
    }

    #[test]
    fn token_line_accepts_each_known_shape() {
        assert!(token_line(
            br#"{"type":"event_msg","payload":{"type":"token_count","info":{}}}"#
        ));
        assert!(token_line(br#"{"type":"turn_context","payload":{}}"#));
        assert!(token_line(
            br#"{"usage":{"input_tokens":100,"output_tokens":50},"model":"gpt-5"}"#
        ));
        assert!(token_line(
            br#"{"prompt_tokens":100,"completion_tokens":50,"model":"gpt-5"}"#
        ));
    }

    #[test]
    fn token_line_skips_non_usage_shapes() {
        assert!(!token_line(
            br#"{"type":"event_msg","payload":{"type":"tool_call"}}"#
        ));
        assert!(!token_line(br#"{"type":"other","foo":"bar"}"#));
        assert!(!token_line(b"{}"));
    }

    #[test]
    fn millis_to_rfc3339_known_values() {
        // 2026-01-01 00:00:00.000 UTC = 1767225600000 ms
        assert_eq!(
            millis_to_rfc3339(1_767_225_600_000),
            "2026-01-01T00:00:00.000Z"
        );
        // 1970-01-01 00:00:01.000 UTC
        assert_eq!(millis_to_rfc3339(1_000), "1970-01-01T00:00:01.000Z");
        // fractional seconds
        assert_eq!(millis_to_rfc3339(1_000 + 42), "1970-01-01T00:00:01.042Z");
    }

    #[test]
    fn codex_raw_usage_field_aliases() {
        // OpenAI alias names
        let s = r#"{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}"#;
        let u: CodexRawUsage = serde_json::from_str(s).unwrap();
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.total_tokens, 150);
    }

    #[test]
    fn codex_raw_usage_cached_aliases() {
        let s = r#"{"input_tokens":200,"cached_tokens":80,"output_tokens":30}"#;
        let u: CodexRawUsage = serde_json::from_str(s).unwrap();
        assert_eq!(u.input_tokens, 200);
        assert_eq!(u.cached_input_tokens, 80);
        assert_eq!(u.output_tokens, 30);
        assert_eq!(u.total_tokens, 230);
    }

    #[test]
    fn codex_raw_usage_string_token_count() {
        // Some Codex log variants write counts as strings.
        let s = r#"{"input_tokens":"100","output_tokens":"50"}"#;
        let u: CodexRawUsage = serde_json::from_str(s).unwrap();
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
    }

    #[test]
    fn codex_raw_usage_non_object_field_is_none() {
        // CodexLogEntry.usage may be a boolean in malformed logs — skip gracefully.
        let s = r#"{"timestamp":"2026-01-01T00:00:00Z","usage":true}"#;
        let e: CodexLogEntry<'_> = serde_json::from_str(s).unwrap();
        assert!(e.usage.is_none());
    }

    #[test]
    fn subtract_raw_usage_computes_delta() {
        let prev = CodexRawUsage {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        };
        let current = CodexRawUsage {
            input_tokens: 300,
            output_tokens: 120,
            ..Default::default()
        };
        let delta = subtract_raw_usage(&current, Some(&prev));
        assert_eq!(delta.input_tokens, 200);
        assert_eq!(delta.output_tokens, 70);
    }

    #[test]
    fn parse_codex_session_event_msg() {
        use std::io::Write as _;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path();
        let path = sessions_dir.join("session-a.jsonl");

        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"turn_context","payload":{{"model":"gpt-5"}}}}"#
        )
        .unwrap();
        writeln!(
            f,
            r#"{{"type":"event_msg","timestamp":"2026-01-01T10:00:00.000Z","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":100,"output_tokens":50}}}}}}}}"#
        ).unwrap();

        let events = parse_codex_session(&path, 0, &mut CodexSpendState::default()).0;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 100);
        assert_eq!(events[0].output_tokens, 50);
        assert_eq!(events[0].model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn parse_codex_session_cumulative_total_subtracted() {
        use std::io::Write as _;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path();
        let path = sessions_dir.join("session-b.jsonl");

        let mut f = std::fs::File::create(&path).unwrap();
        // First event: total = 100/50
        writeln!(
            f,
            r#"{{"type":"event_msg","timestamp":"2026-01-01T10:00:00.000Z","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":100,"output_tokens":50}}}}}}}}"#
        ).unwrap();
        // Second event: total = 300/120 → delta = 200/70
        writeln!(
            f,
            r#"{{"type":"event_msg","timestamp":"2026-01-01T10:01:00.000Z","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":300,"output_tokens":120}}}}}}}}"#
        ).unwrap();

        let events = parse_codex_session(&path, 0, &mut CodexSpendState::default()).0;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].input_tokens, 100);
        assert_eq!(events[1].input_tokens, 200);
        assert_eq!(events[1].output_tokens, 70);
    }

    #[test]
    fn parse_codex_session_headless() {
        use std::io::Write as _;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path();
        let path = sessions_dir.join("exec.jsonl");

        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"model":"gpt-5","timestamp":"2026-01-01T10:00:00.000Z","usage":{{"input_tokens":200,"output_tokens":80}}}}"#
        ).unwrap();

        let events = parse_codex_session(&path, 0, &mut CodexSpendState::default()).0;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input_tokens, 200);
        assert_eq!(events[0].output_tokens, 80);
        assert_eq!(events[0].model.as_deref(), Some("gpt-5"));
    }
}
