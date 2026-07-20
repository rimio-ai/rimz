//! Exact-or-locally-priced Grok Build completed-turn spend parsing.

use std::collections::BTreeMap;
use std::path::Path;

use crate::agents::spending::{CachedEntry, SpendCursor, SpendParse, origin_path, price_split};
use crate::agents::{PriceBook, TokenSplit, read_transcript_lines};

use super::transcript::{self, ModelUsage, TurnCompletion};

const USD_TICKS_PER_USD: f64 = 10_000_000_000.0;

pub(super) fn parse(path: &Path, resume: Option<&SpendCursor>, prices: &PriceBook) -> SpendParse {
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
    let mut unknown_models = BTreeMap::new();
    let entries = entries_for_completions(
        &completions,
        fallback_session_id,
        prices,
        &mut unknown_models,
    );
    let origin =
        transcript::read_summary(path).and_then(|summary| origin_path(summary.info.cwd.as_deref()));
    SpendParse {
        entries,
        origin,
        cursor,
        unknown_models,
        replace_entries: rewound,
    }
}

/// Price an already authoritative rewind-aware fold without reopening either
/// the transcript or its summary companion.
pub(super) fn cost_from_folded(
    path: &Path,
    folded: &transcript::FoldedSession,
    session_id: &str,
    prices: &PriceBook,
) -> Option<crate::agents::AgentCost> {
    let fallback_session_id = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str());
    let completions = folded.completions().cloned().collect::<Vec<_>>();
    let entries = entries_for_completions(
        &completions,
        fallback_session_id,
        prices,
        &mut BTreeMap::new(),
    );
    crate::agents::spending::session_cost_from_entries(&entries, session_id)
}

fn folded_completions(text: &str) -> Vec<TurnCompletion> {
    transcript::fold(text).completions().cloned().collect()
}

fn entries_for_completions(
    completions: &[TurnCompletion],
    fallback_session_id: Option<&str>,
    prices: &PriceBook,
    unknown_models: &mut BTreeMap<String, u64>,
) -> Vec<CachedEntry> {
    completions
        .iter()
        .flat_map(|completion| {
            entries_for_completion(completion, fallback_session_id, prices, unknown_models)
        })
        .collect()
}

fn entries_for_completion(
    completion: &TurnCompletion,
    fallback_session_id: Option<&str>,
    prices: &PriceBook,
    unknown_models: &mut BTreeMap<String, u64>,
) -> Vec<CachedEntry> {
    let Some(usage) = completion
        .usage
        .as_ref()
        .filter(|usage| !usage.usage_is_incomplete)
    else {
        return Vec::new();
    };
    let session_id = completion
        .session_id
        .as_deref()
        .or(fallback_session_id)
        .unwrap_or("unknown");
    let timestamp = completion.at_secs;
    if let Some(aggregate_ticks) = usage.cost_usd_ticks {
        if usage.cost_is_partial || aggregate_ticks < 0 {
            return Vec::new();
        }
        return exact_entries(
            usage,
            session_id,
            &completion.prompt_id,
            timestamp,
            aggregate_ticks,
        );
    }
    if usage.cost_is_partial {
        return Vec::new();
    }
    estimated_entries(
        usage,
        session_id,
        &completion.prompt_id,
        timestamp,
        prices,
        unknown_models,
    )
}

fn exact_entries(
    usage: &transcript::PromptUsage,
    session_id: &str,
    prompt_id: &str,
    timestamp: u64,
    aggregate_ticks: i64,
) -> Vec<CachedEntry> {
    if usage.model_usage.is_empty() {
        return vec![entry(
            session_id,
            prompt_id,
            None,
            timestamp,
            Totals {
                input: usage.input_tokens,
                cache_read: usage.cached_read_tokens,
                output: usage.output_tokens,
            },
            ticks_to_usd(aggregate_ticks),
        )];
    }

    let mut entries = Vec::new();
    let mut attributed = Totals::default();
    let mut attributed_ticks = 0_i64;
    for (model, model_usage) in &usage.model_usage {
        let Some(cost_ticks) = trusted_model_ticks(model_usage) else {
            return vec![entry(
                session_id,
                prompt_id,
                None,
                timestamp,
                Totals {
                    input: usage.input_tokens,
                    cache_read: usage.cached_read_tokens,
                    output: usage.output_tokens,
                },
                ticks_to_usd(aggregate_ticks),
            )];
        };
        attributed.add(model_usage);
        attributed_ticks = attributed_ticks.saturating_add(cost_ticks);
        entries.push(entry(
            session_id,
            prompt_id,
            Some(model),
            timestamp,
            Totals {
                input: model_usage.input_tokens,
                cache_read: model_usage.cached_read_tokens,
                output: model_usage.output_tokens,
            },
            ticks_to_usd(cost_ticks),
        ));
    }
    if attributed.input > usage.input_tokens
        || attributed.cache_read > usage.cached_read_tokens
        || attributed.output > usage.output_tokens
        || attributed_ticks > aggregate_ticks
    {
        return vec![entry(
            session_id,
            prompt_id,
            None,
            timestamp,
            Totals {
                input: usage.input_tokens,
                cache_read: usage.cached_read_tokens,
                output: usage.output_tokens,
            },
            ticks_to_usd(aggregate_ticks),
        )];
    }
    let residual = Totals {
        input: usage.input_tokens.saturating_sub(attributed.input),
        cache_read: usage
            .cached_read_tokens
            .saturating_sub(attributed.cache_read),
        output: usage.output_tokens.saturating_sub(attributed.output),
    };
    let residual_ticks = aggregate_ticks.saturating_sub(attributed_ticks);
    if residual.input > 0
        || residual.cache_read > 0
        || residual.output > 0
        || residual_ticks > 0
        || entries.is_empty()
    {
        entries.push(entry(
            session_id,
            prompt_id,
            None,
            timestamp,
            residual,
            ticks_to_usd(residual_ticks),
        ));
    }
    entries
}

fn estimated_entries(
    usage: &transcript::PromptUsage,
    session_id: &str,
    prompt_id: &str,
    timestamp: u64,
    prices: &PriceBook,
    unknown_models: &mut BTreeMap<String, u64>,
) -> Vec<CachedEntry> {
    if usage.model_usage.is_empty() {
        return Vec::new();
    }
    if usage.model_usage.len() == 1 {
        let (model, model_usage) = usage.model_usage.first_key_value().expect("length checked");
        if model_usage.cost_is_partial {
            return Vec::new();
        }
        let totals = Totals {
            input: usage.input_tokens,
            cache_read: usage.cached_read_tokens,
            output: usage.output_tokens,
        };
        return estimated_entry(
            session_id,
            prompt_id,
            model,
            timestamp,
            totals,
            prices,
            unknown_models,
        )
        .into_iter()
        .collect();
    }

    let mut entries = Vec::new();
    let mut attributed = Totals::default();
    for (model, model_usage) in &usage.model_usage {
        if model_usage.cost_is_partial {
            return Vec::new();
        }
        let totals = Totals {
            input: model_usage.input_tokens,
            cache_read: model_usage.cached_read_tokens,
            output: model_usage.output_tokens,
        };
        attributed.add(model_usage);
        if let Some(entry) = estimated_entry(
            session_id,
            prompt_id,
            model,
            timestamp,
            totals,
            prices,
            unknown_models,
        ) {
            entries.push(entry);
        }
    }
    if attributed.input > usage.input_tokens
        || attributed.cache_read > usage.cached_read_tokens
        || attributed.output > usage.output_tokens
    {
        return Vec::new();
    }
    let residual = Totals {
        input: usage.input_tokens.saturating_sub(attributed.input),
        cache_read: usage
            .cached_read_tokens
            .saturating_sub(attributed.cache_read),
        output: usage.output_tokens.saturating_sub(attributed.output),
    };
    if residual.input > 0 || residual.cache_read > 0 || residual.output > 0 {
        entries.push(entry(session_id, prompt_id, None, timestamp, residual, 0.0));
    }
    entries
}

#[allow(clippy::too_many_arguments)]
fn estimated_entry(
    session_id: &str,
    prompt_id: &str,
    model: &str,
    timestamp: u64,
    totals: Totals,
    prices: &PriceBook,
    unknown_models: &mut BTreeMap<String, u64>,
) -> Option<CachedEntry> {
    let split = token_split(&totals);
    let cost_usd = price_split(prices, model, split, timestamp, unknown_models)?;
    Some(entry(
        session_id,
        prompt_id,
        Some(model),
        timestamp,
        totals,
        cost_usd,
    ))
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
}

impl Totals {
    fn add(&mut self, usage: &ModelUsage) {
        self.input = self.input.saturating_add(usage.input_tokens);
        self.cache_read = self.cache_read.saturating_add(usage.cached_read_tokens);
        self.output = self.output.saturating_add(usage.output_tokens);
    }
}

fn entry(
    session_id: &str,
    prompt_id: &str,
    model: Option<&str>,
    ts_secs: u64,
    totals: Totals,
    cost_usd: f64,
) -> CachedEntry {
    let model_key = model.unwrap_or("aggregate");
    let split = token_split(&totals);
    CachedEntry {
        dedup_key: Some(format!("grok:{session_id}:{prompt_id}:{model_key}")),
        thread_id: Some(session_id.to_owned()),
        model: model.map(ToOwned::to_owned),
        ..CachedEntry::new(ts_secs, cost_usd, &split)
    }
}

fn token_split(totals: &Totals) -> TokenSplit {
    // Grok reports `input_tokens` inclusive of the cached slice.
    TokenSplit::new(
        totals.input.saturating_sub(totals.cache_read),
        totals.output,
    )
    .cached(0, totals.cache_read)
}

fn ticks_to_usd(ticks: i64) -> f64 {
    ticks as f64 / USD_TICKS_PER_USD
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
            cost_from_folded(&path, &folded, "s1", &PriceBook::embedded())
                .unwrap()
                .total_cost_usd,
            Some(0.25)
        );
    }

    #[test]
    fn missing_native_cost_uses_model_pricing_and_tracks_unknowns() {
        let completion = serde_json::json!({
            "timestamp": 1_700_000_000_u64,
            "method": "_x.ai/session/update",
            "params": {
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "turn_completed",
                    "prompt_id": "p1",
                    "stop_reason": "end_turn",
                    "usage": {
                        "inputTokens": 17_869,
                        "cachedReadTokens": 0,
                        "outputTokens": 32,
                        "totalTokens": 17_901,
                        "modelUsage": {
                            "grok-4.5-build-free": {
                                "inputTokens": 17_869,
                                "cachedReadTokens": 0,
                                "outputTokens": 32,
                                "totalTokens": 17_901
                            }
                        }
                    }
                }
            }
        })
        .to_string();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.jsonl");
        std::fs::write(&path, with_prompt(&completion)).unwrap();

        let parsed = parse(&path, None, &PriceBook::embedded());
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(
            parsed.entries[0].model.as_deref(),
            Some("grok-4.5-build-free")
        );
        let expected = 17_869.0 * 2e-6 + 32.0 * 6e-6;
        assert!((parsed.entries[0].cost_usd - expected).abs() < 1e-12);
        assert!(parsed.unknown_models.is_empty());

        let unknown = completion.replace("grok-4.5-build-free", "grok-9-build-free");
        std::fs::write(&path, with_prompt(&unknown)).unwrap();
        let parsed = parse(&path, None, &PriceBook::embedded());
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].cost_usd, 0.0);
        assert_eq!(
            parsed.unknown_models.get("grok-9-build-free"),
            Some(&1_700_000_000)
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
