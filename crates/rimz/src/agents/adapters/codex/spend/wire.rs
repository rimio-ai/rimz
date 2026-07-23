//! Lossy typed JSON wire model for headless Codex spend logs.
//!
//! Interactive rollouts normalize in `codex::rollout`; this module owns the
//! distinct exec-only wrapper shapes.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::Deserialize;

use crate::agents::transcript_fs::deserialize_optional_object_lossy;

use super::super::rollout::{CodexModelMetadata, CodexRawUsage, CodexTimestamp};

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
    /// Reasoning tokens, already folded into `output_tokens` for pricing. Kept
    /// here only to strengthen the cross-file dedup fingerprint so two distinct
    /// same-second events with identical input/output but differing reasoning
    /// stay separate.
    pub reasoning_output_tokens: u64,
    /// Total tokens as reported (or summed) for the event. Fingerprint-only, for
    /// the same reason as `reasoning_output_tokens`.
    pub total_tokens: u64,
    pub tool_calls: BTreeMap<String, u32>,
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
