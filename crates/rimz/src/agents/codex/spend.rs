//! Codex agent JSONL transcript parser.
//!
//! Codex JSONL records **token usage events** and carries **no** `costUSD`
//! field, so [`parse_codex_spend`] multiplies each [`wire::CodexTokenEvent`]
//! through the [`pricing`](crate::agents::pricing) table to a USD cost.
//! Discovery and parsing stay pure and network-free.
//!
//! Codex session files live under `~/.codex/{sessions,archived_sessions}/`
//! (or `CODEX_HOME` env).
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
//! [`super::rollout::CodexRawUsage`] normalizes field-name variants across providers
//! that embed Codex-compatible usage: `prompt_tokens`/`completion_tokens`
//! (OpenAI), `input`/`output` (compact),
//! `cached_tokens`/`cached_input_tokens` (cache).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::LocalSpendFold;
use crate::agents::pricing::{PriceBook, Pricing};
use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, iso_to_unix_secs, record_unknown_model,
};
use crate::agents::transcript_fs::home_dir;

mod parse;
#[cfg(test)]
mod tests;
pub(super) mod wire;

#[cfg(test)]
use parse::codex_line_kind;
use parse::{CodexSpendState, parse_codex_session};
#[cfg(test)]
use wire::CodexLogEntry;

// ── Path discovery ────────────────────────────────────────────────────────────

pub(super) fn codex_homes() -> Vec<PathBuf> {
    if let Ok(env_val) = std::env::var("CODEX_HOME") {
        env_val
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .collect()
    } else {
        vec![home_dir().join(".codex")]
    }
}

pub(super) fn legacy_spend_relative(path: &Path) -> bool {
    path.components().next().is_none_or(|component| {
        !matches!(
            component.as_os_str().to_str(),
            Some("sessions" | "archived_sessions")
        )
    })
}

/// Turn a Codex session's token events into priced [`CachedEntry`] values,
/// resuming from `resume` when given (the cursor's `state` restores the
/// cumulative-total and tracked-model fold exactly where it left off).
///
/// Codex logs token counts, not dollars, so each event is multiplied through
/// the price book: uncached input at the input rate, the cached slice at the
/// model's cache-read rate when it is explicit (else at the input rate, per
/// [`codex_event_cost`]), and output (which already includes reasoning tokens)
/// at the output rate. Codex records `cache_write: 0`, so its aggregate `◇` total stays
/// input + output. Events whose model has no known price still contribute
/// tokens and sessions with zero dollars while recording an unknown-model chase.
/// Codex entries carry no message/request IDs, so a provider-namespaced event
/// fingerprint deduplicates copied rollout events across files.
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
        let uncached_input = event.input_tokens.saturating_sub(event.cached_input_tokens);
        let Some(ts_secs) = iso_to_unix_secs(&event.timestamp) else {
            continue;
        };
        let cost = match prices.price(model) {
            Some(price) => codex_event_cost(
                price,
                uncached_input,
                event.cached_input_tokens,
                event.output_tokens,
            ),
            None => {
                record_unknown_model(&mut unknown_models, model, ts_secs);
                0.0
            }
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
            dedup_key: Some(codex_event_dedup_key(&event.timestamp, model, &event)),
            thread_id: None,
            is_sidechain: false,
            has_speed: false,
            model: Some(model.to_owned()),
            rolled: false,
        });
    }
    SpendParse {
        entries: out,
        // The session's durable origin, parsed from the rollout's
        // `session_meta` cwd and carried in `state` across resume cursors, so a
        // closed Codex session still scopes to its workspace. A trusted snapshot
        // override (`codex_origin_overrides`) can still supersede it for live or
        // headless sessions whose rollout omits the header.
        origin: state.cwd.clone(),
        cursor: SpendCursor {
            offset: next_offset,
            state: serde_json::to_value(&state).ok(),
        },
        unknown_models,
        replace_entries: false,
    }
}

/// Resume the live card's exact per-request cost fold from its persisted cursor.
pub(crate) fn resume_live_fold(
    path: &Path,
    prior: Option<&LocalSpendFold>,
    file_len: u64,
    prices: &PriceBook,
) -> LocalSpendFold {
    let mut fold = prior.cloned().unwrap_or_default();
    if fold.cursor.offset > file_len {
        fold = LocalSpendFold::default();
    }
    let parsed = parse_codex_spend(path, Some(&fold.cursor), prices);
    fold.total_usd += parsed
        .entries
        .iter()
        .map(|entry| entry.cost_usd)
        .sum::<f64>();
    fold.cursor = parsed.cursor;
    fold
}

/// Price one Codex token event. Codex bills cached input at the model's
/// cache-read rate only when that rate is explicit in the pricing entry;
/// otherwise the cached slice is billed at the full input rate, matching
/// ccusage's Codex cost path (a Codex model without a discounted cache-read
/// rate does not discount cached tokens). rimz's shared [`Pricing::cost`] always
/// applies `cache_read`, so an implicit rate folds the cached slice into the
/// input argument to price it at the input rate — the long-context threshold
/// still sees the same total request size either way.
fn codex_event_cost(price: Pricing, uncached_input: u64, cached_input: u64, output: u64) -> f64 {
    if price.cache_read_explicit {
        price.cost(uncached_input, output, 0, 0, cached_input, false)
    } else {
        price.cost(
            uncached_input.saturating_add(cached_input),
            output,
            0,
            0,
            0,
            false,
        )
    }
}

fn codex_event_dedup_key(timestamp: &str, model: &str, event: &wire::CodexTokenEvent) -> String {
    format!(
        "codex:{}:{timestamp}:{}:{model}:{}:{}:{}:{}:{}",
        timestamp.len(),
        model.len(),
        event.input_tokens,
        event.cached_input_tokens,
        event.output_tokens,
        event.reasoning_output_tokens,
        event.total_tokens,
    )
}
