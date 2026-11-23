//! Antigravity spend from the token rows of each conversation's `transcript_full.jsonl`.
//!
//! Every model response is one `PLANNER_RESPONSE` row carrying `input_tokens`
//! (cache reads excluded), `cache_read_tokens` and `output_tokens` (thinking
//! included). `transcript.jsonl` repeats the same rows, so it is never a source.
//! Rows are priced at list price by exact lookup only: a subscription's quota is
//! work-metered, so the dollars are an estimate, and a related model's rates are
//! never borrowed.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, SpendingSource, iso_to_unix_secs, record_unknown_model,
};
use crate::agents::transcript_fs::try_read_transcript_lines;
use crate::agents::{
    AgentCost, FieldPatch, LocalContextPatch, LocalContextRefresh, LocalContextRefreshCtx,
    LocalSpendFold, PriceBook, TokenSplit, TranscriptStat, pricing,
};

const EFFORT_SUFFIXES: [&str; 5] = ["-low", "-medium", "-high", "-xhigh", "-max"];

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

#[derive(Deserialize)]
struct TokenRow {
    step_index: u64,
    created_at: String,
    model: Option<String>,
    input_tokens: u64,
    #[serde(default)]
    cache_read_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

pub(super) fn parse(path: &Path, resume: Option<&SpendCursor>, prices: &PriceBook) -> SpendParse {
    let from = resume.map_or(0, |cursor| cursor.offset);
    let (bytes, next) = match try_read_transcript_lines(path, from) {
        Ok(Some(read)) => read,
        Ok(None) => return SpendParse::stalled(resume),
        Err(error) => {
            return SpendParse {
                read_error: Some(error.kind()),
                ..SpendParse::stalled(resume)
            };
        }
    };
    let cursor = SpendCursor::new(next);
    // Every transcript has the same file name; the conversation is its directory.
    let Some(conversation) = path
        .ancestors()
        .nth(3)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|id| super::session::valid_conversation_id(id))
    else {
        return SpendParse {
            cursor,
            ..SpendParse::default()
        };
    };
    let mut unknown_models = BTreeMap::new();
    let entries = String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|line| line.contains(r#""input_tokens""#))
        .filter_map(|line| serde_json::from_str::<TokenRow>(line).ok())
        .filter_map(|row| entry(conversation, &row, prices, &mut unknown_models))
        .collect();
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
    prices: &PriceBook,
    unknown_models: &mut BTreeMap<String, u64>,
) -> Option<CachedEntry> {
    let ts_secs = iso_to_unix_secs(&row.created_at)?;
    let split =
        TokenSplit::new(row.input_tokens, row.output_tokens).cached(0, row.cache_read_tokens);
    if split == TokenSplit::default() {
        return None;
    }
    let model = row
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty());
    let base = model.map(base_model);
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
        ..CachedEntry::new(ts_secs, cost, &split)
    })
}

/// The model slug without the effort agy appends (`gemini-4-neon-high`).
fn base_model(model: &str) -> &str {
    EFFORT_SUFFIXES
        .iter()
        .find_map(|suffix| model.strip_suffix(suffix))
        .filter(|base| !base.is_empty())
        .unwrap_or(model)
}
