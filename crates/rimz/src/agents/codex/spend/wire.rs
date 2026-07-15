//! Lossy typed JSON wire model for Codex spend logs.
//!
//! Session and headless log variants, timestamp forms, and token-count aliases normalize here so the parser can stay structural and state-focused.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::agents::transcript_fs::deserialize_optional_object_lossy;

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
}

// ── Typed structs — session format ───────────────────────────────────────────

/// Rollout `session_meta` header — the file's first line. Spend reads `cwd`;
/// local-session discovery also reads the provider session id and creation
/// timestamp without scanning the rollout body.
#[derive(Deserialize)]
pub(crate) struct CodexSessionMeta<'a> {
    #[serde(rename = "type", borrow, default)]
    pub(crate) entry_type: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    pub(crate) timestamp: Option<CodexTimestamp<'a>>,
    #[serde(
        borrow,
        default,
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    pub(crate) payload: Option<CodexSessionMetaPayload<'a>>,
}

/// The slice of the `session_meta` payload Rimz reads: session identity and cwd.
#[derive(Default, Deserialize)]
pub(crate) struct CodexSessionMetaPayload<'a> {
    #[serde(borrow, default)]
    pub(crate) id: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    pub(crate) cwd: Option<Cow<'a, str>>,
}

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
