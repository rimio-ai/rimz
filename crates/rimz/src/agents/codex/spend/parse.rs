//! Stateful Codex spend parser.
//!
//! This module classifies JSONL lines, tracks cumulative totals and current model across resume cursors, and emits raw token events for pricing.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agents::spending::origin_path;
use crate::agents::transcript_fs::read_spend_lines;

use super::wire::{
    CodexInfo, CodexLogEntry, CodexModelMetadata, CodexPayload, CodexRawUsage, CodexResultFields,
    CodexSessionEntry, CodexSessionMeta, CodexTimestamp, CodexTokenEvent,
};

// ── Line-kind detection ───────────────────────────────────────────────────────

/// Which JSONL format a line belongs to.
#[derive(Clone, Copy)]
pub(super) enum CodexLineKind {
    /// Rollout header: `"type":"session_meta"`, carrying the session `cwd`.
    SessionMeta,
    /// Structured session log: `turn_context` or `event_msg` + `token_count`.
    Session,
    /// Headless/exec log: flat `usage`, `input_tokens`, or `prompt_tokens` field.
    Headless,
}

/// Classify a Codex JSONL line for parsing.
///
/// Returns `None` for lines that contain no relevant token-usage information.
pub(super) fn codex_line_kind(line: &[u8]) -> Option<CodexLineKind> {
    fn has(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    if has(line, br#""type":"session_meta""#) {
        return Some(CodexLineKind::SessionMeta);
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
pub(super) struct CodexSpendState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) previous_totals: Option<CodexRawUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current_model: Option<String>,
    /// The session `cwd` from the rollout's `session_meta` header, captured
    /// once and carried across resume cursors so every appended usage entry
    /// keeps its workspace origin without re-reading the header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cwd: Option<PathBuf>,
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
pub(super) fn parse_codex_session(
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
            Some(CodexLineKind::SessionMeta) => {
                if state.cwd.is_none()
                    && let Ok(meta) = serde_json::from_slice::<CodexSessionMeta<'_>>(line)
                    && let Some(cwd) = meta.payload.and_then(|payload| payload.cwd)
                {
                    state.cwd = origin_path(Some(cwd.as_ref()));
                }
            }
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
pub(super) fn subtract_raw_usage(
    current: &CodexRawUsage,
    previous: Option<&CodexRawUsage>,
) -> CodexRawUsage {
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

pub(super) fn millis_to_rfc3339(millis: u64) -> String {
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
