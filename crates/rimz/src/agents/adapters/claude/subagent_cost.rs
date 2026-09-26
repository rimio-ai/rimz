//! Exact incremental cost fold for one Claude child transcript.

use std::path::Path;

use crate::agents::AgentUsageSummary;
use crate::agents::context::{PricedRequest, SubagentUsageCursor};
use crate::agents::pricing::{PriceBook, TokenSplit};
use crate::agents::spending::{SplitPrice, lookup_split_price, should_replace_usage_duplicate};
use crate::agents::transcript_fs::read_transcript_lines;

use super::spend::{ClaudeEntry, ClaudeUsage, priced_entry, request_split};
use super::subagents::subagents_dir;

/// Advance one child's cumulative cost through complete records appended since
/// `prior`. An unreadable child transcript returns `None` so the caller can
/// retain its last published figure.
pub(super) fn advance_cursor(
    parent_transcript: &Path,
    child_id: &str,
    prior: Option<&SubagentUsageCursor>,
    prices: &PriceBook,
    book_fingerprint: Option<&str>,
) -> Option<SubagentUsageCursor> {
    let filename = format!("agent-{child_id}.jsonl");
    if Path::new(&filename).file_name()?.to_str()? != filename {
        return None;
    }
    let path = subagents_dir(parent_transcript)?.join(filename);
    let len = std::fs::metadata(&path).ok()?.len();
    let transcript_path = path.to_string_lossy().into_owned();
    let pricing_changed =
        prior.is_some_and(|cursor| cursor.book_fingerprint.as_deref() != book_fingerprint);
    let mut cursor = prior
        .filter(|cursor| {
            cursor.transcript_path == transcript_path && cursor.offset <= len && !pricing_changed
        })
        .cloned()
        .unwrap_or(SubagentUsageCursor {
            last_call: None,
            transcript_path,
            offset: 0,
            model: None,
            cost_usd: 0.0,
            unpriced: false,
            book_fingerprint: None,
            last_request: None,
        });

    let Some((content, next_offset)) = read_transcript_lines(&path, cursor.offset) else {
        return Some(cursor);
    };
    for line in content.split(|byte| *byte == b'\n') {
        let Some(entry) = priced_entry(line) else {
            continue;
        };
        if entry.agent_id.as_deref() != Some(child_id) {
            continue;
        }
        if let Some(model) = entry
            .message
            .model
            .as_deref()
            .filter(|model| !model.is_empty() && !model.starts_with('<'))
            && cursor.model.as_deref() != Some(model)
        {
            cursor.model = Some(model.to_owned());
        }
        fold_entry(&mut cursor, &entry, prices);
    }
    cursor.offset = next_offset;
    cursor.book_fingerprint = book_fingerprint.map(str::to_owned);
    Some(cursor)
}

fn fold_entry(cursor: &mut SubagentUsageCursor, entry: &ClaudeEntry, prices: &PriceBook) {
    let main = request_split(&entry.message.usage);
    if main.is_empty() {
        return;
    }
    cursor.last_call = Some(AgentUsageSummary {
        fresh_input_tokens: Some(main.input),
        cache_read_input_tokens: Some(main.cache_read),
        cache_write_input_tokens: Some(main.cache_write + main.cache_write_1h),
        output_tokens: Some(main.output),
        ..Default::default()
    });
    let Some(mut request) = price_usage(
        &entry.message.usage,
        entry.message.model.as_deref(),
        entry.cost_usd,
        prices,
        &mut cursor.unpriced,
    ) else {
        cursor.last_request = None;
        return;
    };

    for iteration in entry
        .message
        .usage
        .iterations
        .iter()
        .filter(|iteration| iteration.kind == "advisor_message")
    {
        let Some(advisor) = price_usage(
            &iteration.usage,
            iteration.model.as_deref(),
            None,
            prices,
            &mut cursor.unpriced,
        ) else {
            continue;
        };
        request.cost_usd += advisor.cost_usd;
        request.token_total = request.token_total.saturating_add(advisor.token_total);
        request.has_speed |= advisor.has_speed;
    }

    let key = entry
        .message
        .id
        .as_deref()
        .filter(|id| !id.is_empty())
        .map(|message_id| {
            format!(
                "{message_id}\0{}",
                entry.request_id.as_deref().unwrap_or_default()
            )
        });
    let Some(key) = key else {
        cursor.cost_usd += request.cost_usd;
        cursor.last_request = None;
        return;
    };
    let request = PricedRequest {
        key,
        cost_usd: request.cost_usd,
        token_total: request.token_total,
        has_speed: request.has_speed,
    };

    if let Some(previous) = cursor
        .last_request
        .as_ref()
        .filter(|previous| previous.key == request.key)
    {
        if should_replace_usage_duplicate(
            request.token_total,
            request.has_speed,
            previous.token_total,
            previous.has_speed,
        ) {
            cursor.cost_usd = cursor.cost_usd - previous.cost_usd + request.cost_usd;
            cursor.last_request = Some(request);
        }
        return;
    }

    cursor.cost_usd += request.cost_usd;
    cursor.last_request = Some(request);
}

struct PricedUsage {
    cost_usd: f64,
    token_total: u64,
    has_speed: bool,
}

fn price_usage(
    usage: &ClaudeUsage,
    model: Option<&str>,
    logged_cost: Option<f64>,
    prices: &PriceBook,
    unpriced: &mut bool,
) -> Option<PricedUsage> {
    let split = request_split(usage);
    if split.is_empty() {
        return None;
    }
    let cost_usd = match logged_cost {
        Some(cost) if cost > 0.0 => cost,
        _ => match lookup_split_price(prices, model.unwrap_or_default(), split) {
            SplitPrice::Priced(cost) => cost,
            SplitPrice::Unpriced => {
                *unpriced = true;
                return None;
            }
            SplitPrice::NotPriceable => return None,
        },
    };
    Some(PricedUsage {
        cost_usd,
        token_total: token_total(split),
        has_speed: usage.speed.is_some(),
    })
}

fn token_total(split: TokenSplit) -> u64 {
    split
        .input
        .saturating_add(split.output)
        .saturating_add(split.cache_write)
        .saturating_add(split.cache_write_1h)
        .saturating_add(split.cache_read)
}

#[cfg(test)]
mod tests;
