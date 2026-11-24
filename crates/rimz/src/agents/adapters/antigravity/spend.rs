//! Antigravity spend and response timing from each conversation's `transcript_full.jsonl`.
//!
//! Every model response is one `MODEL` / `PLANNER_RESPONSE` row carrying
//! `input_tokens` (cache reads excluded), `cache_read_tokens` and `output_tokens`
//! (thinking included). Compaction `SYSTEM` / `CHECKPOINT` rows carry tokens too
//! and are priced without timing. `transcript.jsonl` repeats the same rows, so it
//! is never a source. Rows are priced at list price by exact lookup only: a
//! subscription's quota is work-metered, so the dollars are an estimate, and a
//! related model's rates are never borrowed.
//!
//! A `MODEL` / `PLANNER_RESPONSE` row followed immediately by a `MODEL` /
//! `GENERIC` row (any status) is timed from the whole-second gap between their
//! `created_at` timestamps through the shared `response_timing` filter. A
//! response followed by anything else or nothing (plain-text turn endings
//! included) stays untimed. When a parse ends on a model row, the row is emitted
//! untimed at once and the cursor records its byte start so the next resumed
//! parse can re-emit it timed once a `GENERIC` follower lands.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agents::spending::{
    CachedEntry, ResponseTiming, SpendCursor, SpendParse, SpendingSource, iso_to_unix_secs,
    record_unknown_model, response_timing,
};
use crate::agents::transcript_fs::try_read_transcript_lines;
use crate::agents::{
    AgentCost, FieldPatch, LocalContextPatch, LocalContextRefresh, LocalContextRefreshCtx,
    LocalSpendFold, PriceBook, TokenSplit, TranscriptStat, pricing,
};

pub(super) fn sources(provider_home: &Path) -> Vec<SpendingSource> {
    SpendingSource::tree(
        provider_home.join("brain"),
        "*/.system_generated/logs/transcript_full.jsonl",
    )
}

/// The one transcript spend reads: `transcript.jsonl` repeats its rows.
pub(super) const SOURCE_TRANSCRIPT: &str = "transcript_full.jsonl";

/// The card's live cost: the conversation's cumulative list-price total, folded from the
/// bytes written since the last refresh. It patches cost only, since the statusline owns
/// model, tokens, and settle, and a transcript with no new bytes costs one stat.
pub(super) fn refresh(
    path: &Path,
    ctx: &LocalContextRefreshCtx<'_>,
) -> Option<LocalContextRefresh> {
    let mut stat = TranscriptStat::from_path(path)?;
    let same_path = ctx.prior_transcript_path.map(Path::new) == Some(path);
    if same_path {
        stat = ctx.changed_transcript(stat)?;
    }
    let prior_fold = same_path.then_some(ctx.prior_spend_fold).flatten();
    let mut fold = LocalSpendFold::resume(prior_fold, stat.len);
    let from = fold.cursor.offset;
    let resumed = prior_fold.is_some_and(|prior| prior.cursor.offset == from);
    if resumed && from == stat.len {
        return None;
    }
    let parsed = parse(
        path,
        Some(&fold.cursor),
        &pricing::cached_book(ctx.shared_pricing_cache_path),
    );
    if resumed && parsed.cursor.offset == from {
        return None;
    }
    fold.absorb(&parsed.entries);
    fold.cursor = parsed.cursor;
    // A transcript cut shorter folds again from byte zero, and its total can come out
    // below the figure shown. That money was spent, so the fresh fold carries the prior
    // totals as its floor, and rows written after the cut add to them.
    if let Some(prior) = prior_fold.filter(|prior| fold.total_usd < prior.total_usd) {
        fold.total_usd = prior.total_usd;
        fold.input = fold.input.max(prior.input);
        fold.output = fold.output.max(prior.output);
        fold.cache_write = fold.cache_write.max(prior.cache_write);
        fold.cache_read = fold.cache_read.max(prior.cache_read);
    }
    let cost = (fold.total_usd > 0.0).then(|| AgentCost {
        total_cost_usd: Some(fold.total_usd),
        ..AgentCost::default()
    });
    Some(LocalContextRefresh {
        context: LocalContextPatch {
            cost: cost.map_or(FieldPatch::Keep, FieldPatch::Set),
            ..LocalContextPatch::default()
        },
        transcript_path: Some(path.to_string_lossy().into_owned()),
        transcript_stat: Some(stat),
        model_context_limits_key: None,
        spend_fold: FieldPatch::Set(fold),
    })
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AntigravitySpendState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_start: Option<u64>,
}

#[derive(Deserialize)]
struct TokenRow {
    step_index: u64,
    #[serde(default)]
    source: Option<String>,
    #[serde(default, rename = "type")]
    record_type: Option<String>,
    created_at: String,
    model: Option<String>,
    input_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

impl TokenRow {
    fn is_model_response(&self) -> bool {
        self.source.as_deref() == Some("MODEL")
            && self.record_type.as_deref() == Some("PLANNER_RESPONSE")
    }

    fn token_split(&self) -> TokenSplit {
        TokenSplit::new(self.input_tokens, self.output_tokens).cached(0, self.cache_read_tokens)
    }
}

#[derive(Deserialize)]
struct FollowerRow {
    #[serde(default)]
    source: Option<String>,
    #[serde(default, rename = "type")]
    record_type: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

impl FollowerRow {
    fn end_secs(&self) -> Option<u64> {
        (self.source.as_deref() == Some("MODEL") && self.record_type.as_deref() == Some("GENERIC"))
            .then_some(self.created_at.as_deref())
            .flatten()
            .and_then(iso_to_unix_secs)
    }
}

struct PendingModel {
    row: TokenRow,
    ts_secs: u64,
    start: u64,
    emitted: bool,
}

fn parse_token_row(line: &str) -> Option<TokenRow> {
    if !line.contains(r#""input_tokens""#) {
        return None;
    }
    serde_json::from_str(line).ok()
}

fn follower_end_secs(line: &str) -> Option<u64> {
    if !line.contains(r#""GENERIC""#) {
        return None;
    }
    serde_json::from_str::<FollowerRow>(line)
        .ok()
        .and_then(|row| row.end_secs())
}

pub(super) fn parse(path: &Path, resume: Option<&SpendCursor>, prices: &PriceBook) -> SpendParse {
    let from = resume.map_or(0, |cursor| cursor.offset);
    let carried_start = resume
        .and_then(|cursor| cursor.state_as::<AntigravitySpendState>().pending_start)
        .filter(|&start| start < from);
    let read_from = carried_start.unwrap_or(from);
    let (bytes, next) = match try_read_transcript_lines(path, read_from) {
        Ok(Some(read)) => read,
        Ok(None) => return SpendParse::stalled(resume),
        Err(error) => {
            return SpendParse {
                read_error: Some(error.kind()),
                ..SpendParse::stalled(resume)
            };
        }
    };
    if next <= from {
        return SpendParse::stalled(resume);
    }
    // Every transcript has the same file name; the conversation is its directory.
    let Some(conversation) = path
        .ancestors()
        .nth(3)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|id| super::session::valid_conversation_id(id))
    else {
        return SpendParse {
            cursor: SpendCursor::new(next),
            ..SpendParse::default()
        };
    };
    let mut unknown_models = BTreeMap::new();
    let mut entries = Vec::new();
    let mut pending: Option<PendingModel> = None;
    let mut pos = 0;
    while pos < bytes.len() {
        let line_start = read_from + pos as u64;
        let (raw_line, line_end, next_pos) = match bytes[pos..].iter().position(|&b| b == b'\n') {
            Some(len) => (
                &bytes[pos..pos + len],
                read_from + (pos + len + 1) as u64,
                pos + len + 1,
            ),
            None => (&bytes[pos..], read_from + bytes.len() as u64, bytes.len()),
        };
        pos = next_pos;
        let raw_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        let content_end = line_start + raw_line.len() as u64;
        if raw_line.is_empty() {
            continue;
        }
        let line = String::from_utf8_lossy(raw_line);

        if line_start < from {
            // Re-seed pending only from the exact trailing model row the prior parse ended on;
            // a rewritten prefix whose line bounds or row kind changed is ignored.
            if carried_start == Some(line_start)
                && (line_end == from || content_end == from)
                && let Some(row) = parse_token_row(&line)
                && row.is_model_response()
                && let Some(ts_secs) = iso_to_unix_secs(&row.created_at)
                && row.token_split() != TokenSplit::default()
            {
                pending = Some(PendingModel {
                    row,
                    ts_secs,
                    start: line_start,
                    emitted: true,
                });
            }
            continue;
        }

        if let Some(prev) = pending.take() {
            let timing = follower_end_secs(&line)
                .and_then(|end| end.checked_sub(prev.ts_secs))
                .and_then(|secs| secs.checked_mul(1_000))
                .and_then(|ms| response_timing(prev.row.output_tokens, ms));
            if (!prev.emitted || timing.is_some())
                && let Some(built) =
                    entry(conversation, &prev.row, timing, prices, &mut unknown_models)
            {
                entries.push(built);
            }
        }

        let Some(row) = parse_token_row(&line) else {
            continue;
        };
        if row.is_model_response()
            && let Some(ts_secs) = iso_to_unix_secs(&row.created_at)
            && row.token_split() != TokenSplit::default()
        {
            pending = Some(PendingModel {
                row,
                ts_secs,
                start: line_start,
                emitted: false,
            });
        } else if let Some(built) = entry(conversation, &row, None, prices, &mut unknown_models) {
            entries.push(built);
        }
    }

    let cursor = if let Some(last) = pending {
        if !last.emitted
            && let Some(built) = entry(conversation, &last.row, None, prices, &mut unknown_models)
        {
            entries.push(built);
        }
        SpendCursor::with_state(
            next,
            &AntigravitySpendState {
                pending_start: Some(last.start),
            },
        )
    } else {
        SpendCursor::new(next)
    };

    SpendParse {
        entries,
        cursor,
        unknown_models,
        ..SpendParse::default()
    }
}

fn entry(
    conversation: &str,
    row: &TokenRow,
    timing: Option<ResponseTiming>,
    prices: &PriceBook,
    unknown_models: &mut BTreeMap<String, u64>,
) -> Option<CachedEntry> {
    let ts_secs = iso_to_unix_secs(&row.created_at)?;
    let split = row.token_split();
    if split == TokenSplit::default() {
        return None;
    }
    let model = row
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty());
    let base = model.map(|model| super::split_model_effort(model).0);
    let cost = match (model, base) {
        (Some(raw), Some(base)) => {
            match prices.exact_price(raw).or_else(|| prices.exact_price(base)) {
                Some(price) => price.cost_of(split),
                None => {
                    record_unknown_model(unknown_models, base, ts_secs);
                    0.0
                }
            }
        }
        _ => 0.0,
    };
    Some(CachedEntry {
        message_id: Some(format!("{conversation}:{}", row.step_index).into_boxed_str()),
        thread_id: Some(conversation.into()),
        model: base.map(Into::into),
        timing,
        ..CachedEntry::new(ts_secs, cost, &split)
    })
}
