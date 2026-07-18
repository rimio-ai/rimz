//! Bounded, metadata-only Copilot OpenTelemetry chat-span enrichment.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use super::super::{
    AgentCurrentUsage, AgentTokenUsage, FieldPatch, LocalContextPatch, LocalContextRefresh,
    LocalContextRefreshCtx, LocalTokenPatch, TranscriptStat, read_transcript_tail,
};
use super::paths;

#[derive(Debug, Deserialize)]
struct OtelRecord {
    #[serde(rename = "type")]
    record_type: Option<String>,
    name: Option<String>,
    #[serde(rename = "startTime")]
    start_time: Option<Value>,
    #[serde(rename = "endTime")]
    end_time: Option<Value>,
    #[serde(rename = "hrTime")]
    hr_time: Option<Value>,
    #[serde(rename = "_hrTime")]
    underscore_hr_time: Option<Value>,
    time: Option<Value>,
    timestamp: Option<Value>,
    #[serde(rename = "observedTimestamp")]
    observed_timestamp: Option<Value>,
    #[serde(rename = "timeUnixNano")]
    time_unix_nano: Option<Value>,
    attributes: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, PartialEq)]
struct ChatUsage {
    model_id: Option<String>,
    current_usage: Option<AgentCurrentUsage>,
}

pub(super) fn refresh(ctx: &LocalContextRefreshCtx<'_>) -> Option<LocalContextRefresh> {
    // Hook refreshes can carry the provider transcript before a context
    // sidecar exists. Keep that conversation file out of the telemetry slot;
    // subsequent refreshes carry the sidecar's OTel path here instead.
    let prior_otel_path = ctx
        .prior_transcript_path
        .map(Path::new)
        .filter(|path| paths::validated_transcript_path(path, ctx.agent_id).is_none());
    let path = paths::otel_source(prior_otel_path)?;
    let stat = TranscriptStat::from_path(&path)?;
    if ctx.prior_transcript_stat == Some(&stat) {
        return None;
    }
    let usage = latest_chat_usage(&read_transcript_tail(&path)?, ctx.agent_id);
    let model_id = usage
        .as_ref()
        .and_then(|usage| usage.model_id.clone())
        .or_else(|| ctx.model_hint.map(ToOwned::to_owned));
    let tokens = usage
        .and_then(|usage| usage.current_usage)
        .map(|current_usage| AgentTokenUsage {
            current_usage: Some(current_usage),
            ..AgentTokenUsage::default()
        });
    Some(LocalContextRefresh {
        context: LocalContextPatch {
            model_id: model_id.map_or(FieldPatch::Keep, FieldPatch::Set),
            tokens: LocalTokenPatch::PreserveEstablished(tokens),
            ..LocalContextPatch::authoritative_current()
        },
        transcript_path: Some(path.to_string_lossy().into_owned()),
        transcript_stat: Some(stat),
        ..LocalContextRefresh::authoritative_current()
    })
}

fn latest_chat_usage(lines: &str, session_id: &str) -> Option<ChatUsage> {
    lines
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let record = serde_json::from_str::<OtelRecord>(line).ok()?;
            let timestamp = record_timestamp(&record)?;
            chat_usage(record, session_id).map(|usage| ((timestamp, index), usage))
        })
        .max_by_key(|(order, _)| *order)
        .map(|(_, usage)| usage)
}

fn chat_usage(record: OtelRecord, session_id: &str) -> Option<ChatUsage> {
    if record.record_type.as_deref() != Some("span") {
        return None;
    }
    let attributes = record.attributes.as_ref()?;
    let operation = attr_string(attributes, "gen_ai.operation.name");
    let name_is_chat = record
        .name
        .as_deref()
        .is_some_and(|name| name == "chat" || name.starts_with("chat "));
    if operation.as_deref() != Some("chat") && !name_is_chat {
        return None;
    }
    if attr_string(attributes, "gen_ai.conversation.id").as_deref() != Some(session_id) {
        return None;
    }

    let input_total = attr_u64(attributes, "gen_ai.usage.input_tokens");
    let cache_read = attr_u64(attributes, "gen_ai.usage.cache_read.input_tokens");
    let cache_creation = attr_u64(attributes, "gen_ai.usage.cache_write.input_tokens")
        .or_else(|| attr_u64(attributes, "gen_ai.usage.cache_creation.input_tokens"));
    let output = attr_u64(attributes, "gen_ai.usage.output_tokens");
    let has_usage = [input_total, cache_read, cache_creation, output]
        .into_iter()
        .any(|value| value.is_some());
    Some(ChatUsage {
        model_id: attr_string(attributes, "gen_ai.response.model")
            .or_else(|| attr_string(attributes, "gen_ai.request.model")),
        current_usage: has_usage.then(|| AgentCurrentUsage {
            input_tokens: input_total.map(|input| input.saturating_sub(cache_read.unwrap_or(0))),
            output_tokens: output,
            cache_creation_input_tokens: cache_creation,
            cache_read_input_tokens: cache_read,
        }),
    })
}

fn attr_string(attributes: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    attributes
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn attr_u64(attributes: &BTreeMap<String, Value>, key: &str) -> Option<u64> {
    match attributes.get(key)? {
        Value::Number(value) => value.as_u64(),
        Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}

fn record_timestamp(record: &OtelRecord) -> Option<i128> {
    timestamp_parts(record.end_time.as_ref())
        .or_else(|| timestamp_parts(record.start_time.as_ref()))
        .or_else(|| timestamp_parts(record.hr_time.as_ref()))
        .or_else(|| timestamp_parts(record.underscore_hr_time.as_ref()))
        .or_else(|| timestamp_parts(record.time.as_ref()))
        .or_else(|| timestamp_scalar(record.timestamp.as_ref()))
        .or_else(|| timestamp_scalar(record.observed_timestamp.as_ref()))
        .or_else(|| unix_nanos(record.time_unix_nano.as_ref()))
}

fn timestamp_parts(value: Option<&Value>) -> Option<i128> {
    let parts = value?.as_array()?;
    let seconds = value_u64(parts.first()?)? as i128;
    let nanos = value_u64(parts.get(1)?)? as i128;
    seconds.checked_mul(1_000_000_000)?.checked_add(nanos)
}

fn timestamp_scalar(value: Option<&Value>) -> Option<i128> {
    if let Some(text) = value?.as_str()
        && let Ok(timestamp) = text.parse::<jiff::Timestamp>()
    {
        return Some(timestamp.as_millisecond() as i128 * 1_000_000);
    }
    let raw = value_u64(value?)? as i128;
    Some(if raw >= 100_000_000_000_000_000 {
        raw
    } else if raw >= 100_000_000_000_000 {
        raw * 1_000
    } else if raw >= 100_000_000_000 {
        raw * 1_000_000
    } else {
        raw * 1_000_000_000
    })
}

fn unix_nanos(value: Option<&Value>) -> Option<i128> {
    Some(value_u64(value?)? as i128)
}

fn value_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(value) => value.as_u64(),
        Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    const FIXTURE: &str = include_str!("tests/fixtures/otel.jsonl");
    const INTERLEAVED_FIXTURE: &str = include_str!("tests/fixtures/otel-interleaved.jsonl");

    #[test]
    fn captured_chat_span_maps_model_and_latest_call_tokens() {
        assert_eq!(
            latest_chat_usage(FIXTURE, "session-fixture"),
            Some(ChatUsage {
                model_id: Some("gpt-5-mini".to_owned()),
                current_usage: Some(AgentCurrentUsage {
                    input_tokens: Some(10_596),
                    output_tokens: Some(140),
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: Some(1_664),
                }),
            })
        );
        assert!(latest_chat_usage(FIXTURE, "other-session").is_none());
        assert!(!format!("{:?}", latest_chat_usage(FIXTURE, "session-fixture")).contains("secret"));
    }

    #[test]
    fn newest_timestamp_wins_with_sparse_numeric_drift() {
        let lines = r#"
{"type":"span","name":"chat requested","timestamp":"2026-07-13T15:13:20Z","attributes":{"gen_ai.operation.name":"chat","gen_ai.conversation.id":"s","gen_ai.request.model":"requested","gen_ai.usage.input_tokens":"20","gen_ai.usage.cache_read.input_tokens":"25","gen_ai.usage.cache_creation.input_tokens":"3","gen_ai.usage.output_tokens":"4","gen_ai.input.messages":"secret"}}
{"type":"span","name":"chat resolved","endTime":[1783955603,940841369],"attributes":{"gen_ai.operation.name":"chat","gen_ai.conversation.id":"s","gen_ai.request.model":"requested","gen_ai.response.model":"resolved","gen_ai.usage.input_tokens":40,"gen_ai.usage.cache_read.input_tokens":10,"gen_ai.usage.cache_write.input_tokens":"5","gen_ai.usage.output_tokens":6}}
"#;
        assert_eq!(
            latest_chat_usage(lines, "s"),
            Some(ChatUsage {
                model_id: Some("resolved".to_owned()),
                current_usage: Some(AgentCurrentUsage {
                    input_tokens: Some(30),
                    output_tokens: Some(6),
                    cache_creation_input_tokens: Some(5),
                    cache_read_input_tokens: Some(10),
                }),
            })
        );
    }

    #[test]
    fn interleaved_shared_file_isolates_sessions_and_tolerates_noise() {
        assert_eq!(
            latest_chat_usage(INTERLEAVED_FIXTURE, "session-a"),
            Some(ChatUsage {
                model_id: Some("gpt-a-new".to_owned()),
                current_usage: Some(AgentCurrentUsage {
                    input_tokens: Some(90),
                    output_tokens: Some(11),
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: Some(10),
                }),
            })
        );
        assert_eq!(
            latest_chat_usage(INTERLEAVED_FIXTURE, "session-b"),
            Some(ChatUsage {
                model_id: Some("gpt-b".to_owned()),
                current_usage: Some(AgentCurrentUsage {
                    input_tokens: Some(35),
                    output_tokens: Some(7),
                    cache_creation_input_tokens: Some(2),
                    cache_read_input_tokens: Some(5),
                }),
            })
        );
    }

    #[test]
    fn rejects_unattributed_and_non_span_records() {
        for line in [
            r#"{"type":"metric","name":"chat x","timestamp":1,"attributes":{"gen_ai.operation.name":"chat","gen_ai.conversation.id":"s"}}"#,
            r#"{"type":"log","name":"chat x","timestamp":1,"attributes":{"gen_ai.operation.name":"chat","gen_ai.conversation.id":"s"}}"#,
            r#"{"type":"span","name":"invoke_agent","timestamp":1,"attributes":{"gen_ai.operation.name":"invoke_agent","gen_ai.conversation.id":"s"}}"#,
            r#"{"type":"span","name":"chat x","attributes":{"gen_ai.operation.name":"chat","gen_ai.conversation.id":"s"}}"#,
        ] {
            assert!(latest_chat_usage(line, "s").is_none(), "{line}");
        }
    }

    #[test]
    fn local_refresh_stat_gates_then_reads_an_appended_matching_span() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.jsonl");
        std::fs::write(&path, FIXTURE).unwrap();
        let pricing = dir.path().join("pricing.json");
        let first = refresh(&LocalContextRefreshCtx {
            agent_id: "session-fixture",
            model_hint: None,
            current_transcript_path: None,
            prior_transcript_path: Some(path.to_str().unwrap()),
            prior_transcript_stat: None,
            prior_spend_fold: None,
            shared_pricing_cache_path: &pricing,
        })
        .unwrap();
        assert_eq!(
            first.context.model_id.as_set().map(String::as_str),
            Some("gpt-5-mini")
        );
        let stat = first.transcript_stat.unwrap();
        assert!(
            refresh(&LocalContextRefreshCtx {
                agent_id: "session-fixture",
                model_hint: None,
                current_transcript_path: None,
                prior_transcript_path: first.transcript_path.as_deref(),
                prior_transcript_stat: Some(&stat),
                prior_spend_fold: None,
                shared_pricing_cache_path: &pricing,
            })
            .is_none()
        );

        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"span\",\"name\":\"chat next\",\"timeUnixNano\":\"1783955606000000000\",\"attributes\":{\"gen_ai.operation.name\":\"chat\",\"gen_ai.conversation.id\":\"session-fixture\",\"gen_ai.response.model\":\"next-model\",\"gen_ai.usage.output_tokens\":1}}\n")
            .unwrap();
        let next = refresh(&LocalContextRefreshCtx {
            agent_id: "session-fixture",
            model_hint: None,
            current_transcript_path: None,
            prior_transcript_path: first.transcript_path.as_deref(),
            prior_transcript_stat: Some(&stat),
            prior_spend_fold: None,
            shared_pricing_cache_path: &pricing,
        })
        .unwrap();
        assert_eq!(
            next.context.model_id.as_set().map(String::as_str),
            Some("next-model")
        );
        assert_ne!(next.transcript_stat, Some(stat));
    }

    #[test]
    fn local_refresh_anchors_an_empty_managed_file_then_sees_a_bounded_tail_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.jsonl");
        std::fs::write(&path, "").unwrap();
        let pricing = dir.path().join("pricing.json");
        let anchored = refresh(&LocalContextRefreshCtx {
            agent_id: "session-a",
            model_hint: None,
            current_transcript_path: None,
            prior_transcript_path: Some(path.to_str().unwrap()),
            prior_transcript_stat: None,
            prior_spend_fold: None,
            shared_pricing_cache_path: &pricing,
        })
        .unwrap();
        assert_eq!(anchored.transcript_path.as_deref(), path.to_str());
        assert!(anchored.context.tokens.as_value().is_none());

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        for _ in 0..2_500 {
            writeln!(file, r#"{{"type":"metric","name":"padding"}}"#).unwrap();
        }
        file.write_all(INTERLEAVED_FIXTURE.as_bytes()).unwrap();

        let refreshed = refresh(&LocalContextRefreshCtx {
            agent_id: "session-a",
            model_hint: None,
            current_transcript_path: None,
            prior_transcript_path: anchored.transcript_path.as_deref(),
            prior_transcript_stat: anchored.transcript_stat.as_ref(),
            prior_spend_fold: None,
            shared_pricing_cache_path: &pricing,
        })
        .unwrap();
        assert_eq!(
            refreshed.context.model_id.as_set().map(String::as_str),
            Some("gpt-a-new")
        );
        assert!(refreshed.context.tokens.as_value().is_some());
    }
}
