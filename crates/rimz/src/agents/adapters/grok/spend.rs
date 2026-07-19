//! Exact-cost Grok Build completed-turn spend parsing.

use std::path::Path;

use crate::agents::spending::{CachedEntry, SpendCursor, SpendParse, origin_path};
use crate::agents::{PriceBook, TokenSplit, read_transcript_lines};

use super::transcript::{self, ModelUsage, TurnCompletion};

const USD_TICKS_PER_USD: f64 = 10_000_000_000.0;

pub(super) fn parse(path: &Path, resume: Option<&SpendCursor>, _prices: &PriceBook) -> SpendParse {
    let from = resume.map_or(0, |cursor| cursor.offset);
    let Some((bytes, next)) = read_transcript_lines(path, from) else {
        return SpendParse::stalled(resume);
    };
    let suffix = String::from_utf8_lossy(&bytes);
    let rewound = resume.is_some() && transcript::contains_rewind(&suffix);
    // A rewind invalidates the suffix, so re-fold the whole transcript from the
    // top; a plain resume trusts the physical suffix. Grok carries no cross-line
    // state either way — the fold is rebuilt from the bytes it reads.
    let (completions, offset) = if rewound {
        let Some((bytes, next)) = read_transcript_lines(path, 0) else {
            return SpendParse::default();
        };
        (folded_completions(&String::from_utf8_lossy(&bytes)), next)
    } else if resume.is_some() {
        (transcript::physical_completions(&suffix), next)
    } else {
        (folded_completions(&suffix), next)
    };
    let cursor = SpendCursor {
        offset,
        state: None,
    };
    let fallback_session_id = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str());
    let entries = entries_for_completions(&completions, fallback_session_id);
    let origin =
        transcript::read_summary(path).and_then(|summary| origin_path(summary.info.cwd.as_deref()));
    SpendParse {
        entries,
        origin,
        cursor,
        unknown_models: Default::default(),
        replace_entries: rewound,
    }
}

/// Price an already authoritative rewind-aware fold without reopening either
/// the transcript or its summary companion.
pub(super) fn cost_from_folded(
    path: &Path,
    folded: &transcript::FoldedSession,
    session_id: &str,
) -> Option<crate::agents::AgentCost> {
    let fallback_session_id = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str());
    let completions = folded.completions().cloned().collect::<Vec<_>>();
    let entries = entries_for_completions(&completions, fallback_session_id);
    crate::agents::spending::session_cost_from_entries(&entries, session_id)
}

fn folded_completions(text: &str) -> Vec<TurnCompletion> {
    transcript::fold(text).completions().cloned().collect()
}

fn entries_for_completions(
    completions: &[TurnCompletion],
    fallback_session_id: Option<&str>,
) -> Vec<CachedEntry> {
    completions
        .iter()
        .flat_map(|completion| entries_for_completion(completion, fallback_session_id))
        .collect()
}

fn entries_for_completion(
    completion: &TurnCompletion,
    fallback_session_id: Option<&str>,
) -> Vec<CachedEntry> {
    let Some(usage) = completion.usage.as_ref().filter(|usage| {
        !usage.usage_is_incomplete
            && !usage.cost_is_partial
            && usage.cost_usd_ticks.is_some_and(|ticks| ticks >= 0)
    }) else {
        return Vec::new();
    };
    let session_id = completion
        .session_id
        .as_deref()
        .or(fallback_session_id)
        .unwrap_or("unknown");
    let timestamp = completion.at_secs;
    if usage.model_usage.is_empty() {
        return vec![entry(
            session_id,
            &completion.prompt_id,
            None,
            timestamp,
            Totals {
                input: usage.input_tokens,
                cache_read: usage.cached_read_tokens,
                output: usage.output_tokens,
                cost_ticks: usage.cost_usd_ticks.unwrap_or_default(),
            },
        )];
    }

    let mut entries = Vec::new();
    let mut attributed = Totals::default();
    for (model, model_usage) in &usage.model_usage {
        let Some(cost_ticks) = trusted_model_ticks(model_usage) else {
            return vec![entry(
                session_id,
                &completion.prompt_id,
                None,
                timestamp,
                Totals {
                    input: usage.input_tokens,
                    cache_read: usage.cached_read_tokens,
                    output: usage.output_tokens,
                    cost_ticks: usage.cost_usd_ticks.unwrap_or_default(),
                },
            )];
        };
        attributed.add(model_usage, cost_ticks);
        entries.push(entry(
            session_id,
            &completion.prompt_id,
            Some(model),
            timestamp,
            Totals {
                input: model_usage.input_tokens,
                cache_read: model_usage.cached_read_tokens,
                output: model_usage.output_tokens,
                cost_ticks,
            },
        ));
    }
    let aggregate_ticks = usage.cost_usd_ticks.unwrap_or_default();
    if attributed.input > usage.input_tokens
        || attributed.cache_read > usage.cached_read_tokens
        || attributed.output > usage.output_tokens
        || attributed.cost_ticks > aggregate_ticks
    {
        return vec![entry(
            session_id,
            &completion.prompt_id,
            None,
            timestamp,
            Totals {
                input: usage.input_tokens,
                cache_read: usage.cached_read_tokens,
                output: usage.output_tokens,
                cost_ticks: aggregate_ticks,
            },
        )];
    }
    let residual = Totals {
        input: usage.input_tokens.saturating_sub(attributed.input),
        cache_read: usage
            .cached_read_tokens
            .saturating_sub(attributed.cache_read),
        output: usage.output_tokens.saturating_sub(attributed.output),
        cost_ticks: aggregate_ticks.saturating_sub(attributed.cost_ticks),
    };
    if residual.input > 0
        || residual.cache_read > 0
        || residual.output > 0
        || residual.cost_ticks > 0
        || entries.is_empty()
    {
        entries.push(entry(
            session_id,
            &completion.prompt_id,
            None,
            timestamp,
            residual,
        ));
    }
    entries
}

fn trusted_model_ticks(usage: &ModelUsage) -> Option<i64> {
    (!usage.cost_is_partial)
        .then_some(usage.cost_usd_ticks?)
        .filter(|ticks| *ticks >= 0)
}

#[derive(Default)]
struct Totals {
    input: u64,
    cache_read: u64,
    output: u64,
    cost_ticks: i64,
}

impl Totals {
    fn add(&mut self, usage: &ModelUsage, cost_ticks: i64) {
        self.input = self.input.saturating_add(usage.input_tokens);
        self.cache_read = self.cache_read.saturating_add(usage.cached_read_tokens);
        self.output = self.output.saturating_add(usage.output_tokens);
        self.cost_ticks = self.cost_ticks.saturating_add(cost_ticks);
    }
}

fn entry(
    session_id: &str,
    prompt_id: &str,
    model: Option<&str>,
    ts_secs: u64,
    totals: Totals,
) -> CachedEntry {
    let model_key = model.unwrap_or("aggregate");
    // Grok reports `input_tokens` inclusive of the cached slice, and bills in
    // exact ticks — the price book is never consulted.
    let split = TokenSplit::new(
        totals.input.saturating_sub(totals.cache_read),
        totals.output,
    )
    .cached(0, totals.cache_read);
    let cost_usd = totals.cost_ticks as f64 / USD_TICKS_PER_USD;
    CachedEntry {
        dedup_key: Some(format!("grok:{session_id}:{prompt_id}:{model_key}")),
        thread_id: Some(session_id.to_owned()),
        model: model.map(ToOwned::to_owned),
        ..CachedEntry::new(ts_secs, cost_usd, &split)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn row(prompt: &str, ticks: i64) -> String {
        serde_json::json!({
            "timestamp": 1_700_000_000_u64,
            "method": "_x.ai/session/update",
            "params": {
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "turn_completed",
                    "prompt_id": prompt,
                    "stop_reason": "end_turn",
                    "usage": {
                        "inputTokens": 100,
                        "cachedReadTokens": 40,
                        "outputTokens": 10,
                        "reasoningTokens": 7,
                        "totalTokens": 110,
                        "costUsdTicks": ticks,
                    }
                }
            }
        })
        .to_string()
    }

    fn with_prompt(completion: &str) -> String {
        format!(
            "{}\n{completion}\n",
            serde_json::json!({
                "timestamp": 1_699_999_999_u64,
                "method": "session/update",
                "params": {
                    "sessionId": "s1",
                    "update": {
                        "sessionUpdate": "user_message_chunk",
                        "content": {"type": "text", "text": "hello"},
                        "_meta": {"promptIndex": 0}
                    }
                }
            })
        )
    }

    #[test]
    fn exact_cost_subtracts_cache_and_does_not_duplicate_reasoning() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("s1");
        std::fs::create_dir(&session).unwrap();
        let path = session.join("updates.jsonl");
        std::fs::write(&path, with_prompt(&row("p1", 2_500_000_000))).unwrap();
        let parsed = parse(&path, None, &PriceBook::embedded());
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].input, 60);
        assert_eq!(parsed.entries[0].cache_read, 40);
        assert_eq!(parsed.entries[0].output, 10);
        assert_eq!(parsed.entries[0].cost_usd, 0.25);
        let folded = transcript::read(&path).unwrap();
        assert_eq!(
            cost_from_folded(&path, &folded, "s1")
                .unwrap()
                .total_cost_usd,
            Some(0.25)
        );
    }

    #[test]
    fn suffix_resume_and_rewind_request_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("s1");
        std::fs::create_dir(&session).unwrap();
        let path = session.join("updates.jsonl");
        let first = format!(
            "{{\"timestamp\":1,\"method\":\"session/update\",\"params\":{{\"update\":{{\"sessionUpdate\":\"user_message_chunk\",\"content\":{{\"type\":\"text\",\"text\":\"one\"}}}}}}}}\n{}\n",
            row("p1", 1_000_000_000)
        );
        std::fs::write(&path, &first).unwrap();
        let cold = parse(&path, None, &PriceBook::embedded());
        assert_eq!(cold.entries.len(), 1);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(format!("{}\n", row("p2", 2_000_000_000)).as_bytes())
            .unwrap();
        let suffix = parse(&path, Some(&cold.cursor), &PriceBook::embedded());
        assert_eq!(suffix.entries.len(), 1);
        assert!(!suffix.replace_entries);

        file.write_all(b"{\"timestamp\":2,\"method\":\"_x.ai/session/update\",\"params\":{\"update\":{\"sessionUpdate\":\"rewind_marker\",\"target_prompt_index\":0}}}\n")
            .unwrap();
        let rewound = parse(&path, Some(&suffix.cursor), &PriceBook::embedded());
        assert!(rewound.replace_entries);
        assert!(rewound.entries.is_empty());
    }

    #[test]
    fn partial_or_incomplete_cost_is_absent() {
        let base = row("p", 1_000_000_000);
        for modified in [
            base.replace(
                "\"costUsdTicks\":1000000000",
                "\"costUsdTicks\":1000000000,\"costIsPartial\":true",
            ),
            base.replace(
                "\"costUsdTicks\":1000000000",
                "\"costUsdTicks\":1000000000,\"usageIsIncomplete\":true",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("updates.jsonl");
            std::fs::write(&path, with_prompt(&modified)).unwrap();
            assert!(
                parse(&path, None, &PriceBook::embedded())
                    .entries
                    .is_empty()
            );
        }
    }

    #[test]
    fn model_rows_and_aggregate_residual_sum_once() {
        let completion = row("p1", 3_000_000_000).replace(
            "\"costUsdTicks\":3000000000",
            r#""costUsdTicks":3000000000,"modelUsage":{"grok-a":{"inputTokens":60,"cachedReadTokens":20,"outputTokens":7,"costUsdTicks":2000000000}}"#,
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, with_prompt(&completion)).unwrap();
        let parsed = parse(&path, None, &PriceBook::embedded());
        assert_eq!(parsed.entries.len(), 2);
        let cost = parsed
            .entries
            .iter()
            .map(|entry| entry.cost_usd)
            .sum::<f64>();
        assert!((cost - 0.3).abs() < 1e-12, "{cost}");
        assert_eq!(
            parsed.entries.iter().map(|entry| entry.output).sum::<u64>(),
            10
        );
    }

    #[test]
    fn terminal_message_uses_certified_tail_then_branch_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        let completion = row("p1", 1_000_000_000).replace(
            "\"stop_reason\":\"end_turn\"",
            "\"stop_reason\":\"end_turn\",\"agent_result\":\"done\"",
        );
        let padding = serde_json::json!({ "padding": "x".repeat(70_000) }).to_string();
        std::fs::write(&path, format!("{padding}\n{}", with_prompt(&completion))).unwrap();
        assert_eq!(
            transcript::last_assistant_message(&path).as_deref(),
            Some("done")
        );
        std::fs::write(&path, format!("{}{padding}\n", with_prompt(&completion))).unwrap();
        assert_eq!(
            transcript::last_assistant_message(&path).as_deref(),
            Some("done")
        );
    }
}
