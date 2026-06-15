//! Codex agent JSONL transcript parser.
//!
//! Codex JSONL records **token usage events** and carries **no** `costUSD`
//! field, so [`parse_codex_spend`] multiplies each [`wire::CodexTokenEvent`]
//! through the [`pricing`](crate::agents::pricing) table to a USD cost.
//! Discovery and parsing stay pure and network-free.
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
//! [`wire::CodexRawUsage`] normalizes field-name variants across providers
//! that embed Codex-compatible usage: `prompt_tokens`/`completion_tokens`
//! (OpenAI), `input`/`output` (compact),
//! `cached_tokens`/`cached_input_tokens` (cache).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::pricing::PriceBook;
use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, iso_to_unix_secs, record_unknown_model,
};
use crate::agents::transcript_fs::{collect_jsonl, home_dir};

mod parse;
#[cfg(test)]
mod tests;
mod wire;

use parse::{CodexSpendState, parse_codex_session};
#[cfg(test)]
use parse::{codex_line_kind, millis_to_rfc3339};
#[cfg(test)]
use wire::{CodexLogEntry, CodexRawUsage};

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

/// Turn a Codex session's token events into priced [`CachedEntry`] values,
/// resuming from `resume` when given (the cursor's `state` restores the
/// cumulative-total and tracked-model fold exactly where it left off).
///
/// Codex logs token counts, not dollars, so each event is multiplied through
/// the price book: uncached input at the input rate, the cached slice at the
/// cache-read rate, and output (which already includes reasoning tokens) at the
/// output rate. Codex records `cache_write: 0`, so its aggregate `◇` total stays
/// input + output. Events whose model has no known price, or that price to
/// zero, are dropped. Codex entries carry no message/request IDs, so they bypass
/// the Claude dedup and bucket directly under the `codex` provider.
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
    let mut unknown_models = BTreeMap::new();
    for event in events {
        let Some(model) = event.model.as_deref() else {
            continue;
        };
        let Some(price) = prices.price(model) else {
            if let Some(ts_secs) = iso_to_unix_secs(&event.timestamp) {
                record_unknown_model(&mut unknown_models, model, ts_secs);
            }
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
            model: Some(model.to_owned()),
            // The session's durable origin, parsed from the rollout's
            // `session_meta` cwd and carried in `state` across resume cursors,
            // so a closed Codex session still scopes to its workspace. A trusted
            // snapshot override (`codex_origin_overrides`) can still supersede it
            // for live or headless sessions whose rollout omits the header.
            origin_path: state.cwd.clone(),
        });
    }
    SpendParse {
        entries: out,
        cursor: SpendCursor {
            offset: next_offset,
            state: serde_json::to_value(&state).ok(),
        },
        unknown_models,
    }
}
