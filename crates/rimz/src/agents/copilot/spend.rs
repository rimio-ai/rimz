//! Read-only Copilot CLI shutdown-history spend parser.
//!
//! Copilot appends cumulative per-model counters to `session.shutdown`
//! records. The cursor retains each model's last observed categories so a
//! resumed parse emits only the growth at that shutdown's timestamp.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::paths;
use crate::agents::spending::{
    CachedEntry, SpendCursor, SpendParse, iso_to_unix_secs, origin_path, record_unknown_model,
};
use crate::agents::transcript_fs::{
    deserialize_optional_object_lossy, deserialize_optional_string_lossy,
    deserialize_optional_u64_lossy,
};
use crate::agents::{PriceBook, read_transcript_lines};

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum HistoryRecord {
    #[serde(rename = "session.start")]
    Start { data: StartData },
    #[serde(rename = "session.shutdown")]
    Shutdown {
        #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
        id: Option<String>,
        #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
        timestamp: Option<String>,
        data: Box<ShutdownData>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct StartData {
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    context: Option<StartContext>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct StartContext {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    cwd: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ShutdownData {
    #[serde(default, deserialize_with = "deserialize_model_metrics_lossy")]
    model_metrics: BTreeMap<String, ModelMetric>,
    #[serde(default, deserialize_with = "deserialize_optional_model_lossy")]
    model: Option<ModelIdentity>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    model_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    usage: Option<TokenUsage>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    token_details: Option<TokenDetails>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    output_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    cache_read_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    cache_write_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ModelIdentity {
    Name(String),
    Detail(ModelDetail),
}

impl ModelIdentity {
    fn into_name(self) -> Option<String> {
        let raw = match self {
            Self::Name(name) => Some(name),
            Self::Detail(detail) => detail.id.or(detail.name),
        }?;
        non_empty(raw)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ModelDetail {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ModelMetric {
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    usage: Option<TokenUsage>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    token_details: Option<TokenDetails>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct TokenUsage {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    output_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    cache_read_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    cache_write_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
struct TokenDetails {
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    input: Option<TokenCount>,
    #[serde(
        default,
        alias = "cacheRead",
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    cache_read: Option<TokenCount>,
    #[serde(
        default,
        alias = "cacheWrite",
        deserialize_with = "deserialize_optional_object_lossy"
    )]
    cache_write: Option<TokenCount>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    output: Option<TokenCount>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct TokenCount {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    token_count: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ModelCounters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_write: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_read: Option<u64>,
}

impl ModelCounters {
    fn from_parts(usage: Option<TokenUsage>, details: Option<TokenDetails>) -> Self {
        let usage = usage.unwrap_or_default();
        let details = details.unwrap_or_default();
        let cache_read = details
            .cache_read
            .and_then(|count| count.token_count)
            .or(usage.cache_read_tokens);
        let cache_write = details
            .cache_write
            .and_then(|count| count.token_count)
            .or(usage.cache_write_tokens);
        let input = details
            .input
            .and_then(|count| count.token_count)
            .or_else(|| {
                usage.input_tokens.map(|total| {
                    total
                        .saturating_sub(cache_read.unwrap_or(0))
                        .saturating_sub(cache_write.unwrap_or(0))
                })
            });
        let output = details
            .output
            .and_then(|count| count.token_count)
            .or(usage.output_tokens);
        Self {
            input,
            output,
            cache_write,
            cache_read,
        }
    }

    fn total(self) -> u64 {
        self.input
            .unwrap_or(0)
            .saturating_add(self.output.unwrap_or(0))
            .saturating_add(self.cache_write.unwrap_or(0))
            .saturating_add(self.cache_read.unwrap_or(0))
    }

    fn delta_from(self, baseline: &mut Self) -> Self {
        Self {
            input: counter_delta(self.input, &mut baseline.input),
            output: counter_delta(self.output, &mut baseline.output),
            cache_write: counter_delta(self.cache_write, &mut baseline.cache_write),
            cache_read: counter_delta(self.cache_read, &mut baseline.cache_read),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CopilotSpendState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    models: BTreeMap<String, ModelCounters>,
}

pub(super) fn parse(path: &Path, resume: Option<&SpendCursor>, prices: &PriceBook) -> SpendParse {
    let from = resume.map_or(0, |cursor| cursor.offset);
    let mut state: CopilotSpendState = resume
        .and_then(|cursor| cursor.state.clone())
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let Some(session_id) = native_session_id(path) else {
        return result(state, from, Vec::new(), BTreeMap::new());
    };
    let Some((bytes, next)) = read_transcript_lines(path, from) else {
        return result(state, from, Vec::new(), BTreeMap::new());
    };
    let mut entries = Vec::new();
    let mut unknown_models = BTreeMap::new();
    for line in String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        let Ok(record) = serde_json::from_str::<HistoryRecord>(line) else {
            continue;
        };
        match record {
            HistoryRecord::Start { data } => {
                if let Some(cwd) = data.context.and_then(|context| context.cwd)
                    && let Some(cwd) = origin_path(Some(&cwd))
                {
                    state.cwd = Some(cwd);
                }
            }
            HistoryRecord::Shutdown {
                id,
                timestamp,
                data,
            } => {
                let Some((timestamp, ts_secs)) = timestamp
                    .as_deref()
                    .and_then(|timestamp| iso_to_unix_secs(timestamp).map(|ts| (timestamp, ts)))
                else {
                    continue;
                };
                for (model, current) in shutdown_models(*data) {
                    let baseline = state.models.entry(model.clone()).or_default();
                    let delta = current.delta_from(baseline);
                    if delta.total() == 0 {
                        continue;
                    }
                    let cost_usd = match prices.price(&model) {
                        Some(price) => price.cost(
                            delta.input.unwrap_or(0),
                            delta.output.unwrap_or(0),
                            delta.cache_write.unwrap_or(0),
                            0,
                            delta.cache_read.unwrap_or(0),
                            false,
                        ),
                        None => {
                            record_unknown_model(&mut unknown_models, &model, ts_secs);
                            0.0
                        }
                    };
                    entries.push(CachedEntry {
                        ts_secs,
                        cost_usd,
                        input: delta.input.unwrap_or(0),
                        output: delta.output.unwrap_or(0),
                        cache_write: delta.cache_write.unwrap_or(0),
                        cache_read: delta.cache_read.unwrap_or(0),
                        message_id: None,
                        request_id: None,
                        dedup_key: Some(dedup_key(
                            &session_id,
                            id.as_deref(),
                            timestamp,
                            &model,
                            current,
                        )),
                        thread_id: Some(session_id.clone()),
                        is_sidechain: false,
                        has_speed: false,
                        model: Some(model),
                        rolled: false,
                    });
                }
            }
            HistoryRecord::Other => {}
        }
    }
    result(state, next, entries, unknown_models)
}

fn shutdown_models(data: ShutdownData) -> Vec<(String, ModelCounters)> {
    if !data.model_metrics.is_empty() {
        return data
            .model_metrics
            .into_iter()
            .filter_map(|(model, metric)| {
                let model = non_empty(model)?;
                Some((
                    model,
                    ModelCounters::from_parts(metric.usage, metric.token_details),
                ))
            })
            .collect();
    }
    let model = data
        .model
        .and_then(ModelIdentity::into_name)
        .or_else(|| data.model_id.and_then(non_empty));
    let usage = data.usage.or_else(|| {
        (data.input_tokens.is_some()
            || data.output_tokens.is_some()
            || data.cache_read_tokens.is_some()
            || data.cache_write_tokens.is_some())
        .then_some(TokenUsage {
            input_tokens: data.input_tokens,
            output_tokens: data.output_tokens,
            cache_read_tokens: data.cache_read_tokens,
            cache_write_tokens: data.cache_write_tokens,
        })
    });
    model
        .map(|model| vec![(model, ModelCounters::from_parts(usage, data.token_details))])
        .unwrap_or_default()
}

fn native_session_id(path: &Path) -> Option<String> {
    let session_id = path.parent()?.file_name()?.to_str()?;
    paths::validated_transcript_path(path, session_id)?;
    Some(session_id.to_owned())
}

fn result(
    state: CopilotSpendState,
    offset: u64,
    entries: Vec<CachedEntry>,
    unknown_models: BTreeMap<String, u64>,
) -> SpendParse {
    SpendParse {
        entries,
        origin: state.cwd.clone(),
        cursor: SpendCursor {
            offset,
            state: serde_json::to_value(state).ok(),
        },
        unknown_models,
        replace_entries: false,
    }
}

fn counter_delta(current: Option<u64>, baseline: &mut Option<u64>) -> Option<u64> {
    let current = current?;
    let delta = baseline.map_or(current, |prior| current.saturating_sub(prior));
    *baseline = Some(current);
    Some(delta)
}

fn dedup_key(
    session_id: &str,
    record_id: Option<&str>,
    timestamp: &str,
    model: &str,
    counters: ModelCounters,
) -> String {
    let record = record_id.filter(|id| !id.trim().is_empty()).map_or_else(
        || {
            format!(
                "{timestamp}:{}:{}:{}:{}",
                counters.input.unwrap_or(0),
                counters.cache_read.unwrap_or(0),
                counters.cache_write.unwrap_or(0),
                counters.output.unwrap_or(0),
            )
        },
        str::to_owned,
    );
    format!("copilot:{session_id}:{record}:{model}")
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn deserialize_model_metrics_lossy<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, ModelMetric>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(model, metric)| {
            serde_json::from_value(metric.clone())
                .ok()
                .map(|metric| (model.clone(), metric))
        })
        .collect())
}

fn deserialize_optional_model_lossy<'de, D>(
    deserializer: D,
) -> Result<Option<ModelIdentity>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    const FIXTURE: &str = include_str!("tests/fixtures/shutdown-history.jsonl");

    fn transcript(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session-fixture");
        std::fs::create_dir(&session).unwrap();
        let path = session.join("events.jsonl");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    #[test]
    fn captured_shutdowns_emit_exact_non_overlapping_deltas() {
        let (_dir, path) = transcript(FIXTURE);
        let parsed = parse(&path, None, &PriceBook::embedded());

        assert!(!parsed.replace_entries);
        assert_eq!(
            parsed.origin.as_deref(),
            Some(Path::new("/home/example/project"))
        );
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(
            parsed
                .entries
                .iter()
                .map(|entry| (
                    entry.input,
                    entry.cache_read,
                    entry.cache_write,
                    entry.output
                ))
                .collect::<Vec<_>>(),
            vec![
                (9_560, 1_664, 0, 127),
                (160, 11_136, 0, 58),
                (9_705, 1_664, 0, 100)
            ]
        );
        assert_eq!(
            parsed.entries.iter().map(|entry| entry.input).sum::<u64>(),
            19_425
        );
        assert_eq!(
            parsed
                .entries
                .iter()
                .map(|entry| entry.cache_read)
                .sum::<u64>(),
            14_464
        );
        assert_eq!(
            parsed.entries.iter().map(|entry| entry.output).sum::<u64>(),
            285,
            "reasoning is already included in generated output"
        );
        assert!(parsed.entries.iter().all(|entry| {
            entry.thread_id.as_deref() == Some("session-fixture")
                && entry.model.as_deref() == Some("gpt-5-mini")
                && entry
                    .dedup_key
                    .as_deref()
                    .is_some_and(|key| key.starts_with("copilot:"))
                && entry.cost_usd > 0.0
        }));
    }

    #[test]
    fn cold_and_incremental_parses_assign_identical_shutdown_windows() {
        let lines = FIXTURE.lines().collect::<Vec<_>>();
        let initial = format!("{}\n{}\n", lines[0], lines[1]);
        let (_dir, path) = transcript(&initial);
        let first = parse(&path, None, &PriceBook::embedded());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{}", lines[2]).unwrap();
        writeln!(file, "{}", lines[3]).unwrap();
        let second = parse(&path, Some(&first.cursor), &PriceBook::embedded());
        let mut incremental = first.entries;
        incremental.extend(second.entries);
        let cold = parse(&path, None, &PriceBook::embedded());

        assert_eq!(incremental, cold.entries);
        assert!(!second.replace_entries);
        assert_eq!(
            incremental
                .iter()
                .map(|entry| entry.ts_secs)
                .collect::<Vec<_>>(),
            vec![
                iso_to_unix_secs("2026-07-14T06:46:59.337Z").unwrap(),
                iso_to_unix_secs("2026-07-14T06:51:19.303Z").unwrap(),
                iso_to_unix_secs("2026-07-14T06:51:31.605Z").unwrap(),
            ]
        );
    }

    #[test]
    fn multiple_models_and_top_level_fallback_keep_model_attribution() {
        let (_dir, path) = transcript(
            r#"{"type":"session.shutdown","id":"multi","timestamp":"2026-07-14T06:00:00Z","data":{"modelMetrics":{"known-model":{"usage":{"inputTokens":100,"cacheReadTokens":20,"outputTokens":10}},"unknown-model":{"tokenDetails":{"input":{"tokenCount":5},"output":{"tokenCount":2}}}}}}
{"type":"session.shutdown","id":"fallback","timestamp":"2026-07-14T07:00:00Z","data":{"model":{"id":"known-model"},"inputTokens":140,"cacheReadTokens":25,"cacheWriteTokens":5,"outputTokens":14,"reasoningTokens":3,"totalNanoAiu":999}}
"#,
        );
        let prices = PriceBook::from_litellm_json(
            r#"{"known-model":{"input_cost_per_token":0.000001,"output_cost_per_token":0.000002,"cache_read_input_token_cost":0.0000001,"cache_creation_input_token_cost":0.00000125}}"#,
        );
        let parsed = parse(&path, None, &prices);

        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(parsed.entries[0].model.as_deref(), Some("known-model"));
        assert_eq!(parsed.entries[0].input, 80);
        assert_eq!(parsed.entries[1].model.as_deref(), Some("unknown-model"));
        assert_eq!(parsed.entries[1].cost_usd, 0.0);
        assert!(parsed.unknown_models.contains_key("unknown-model"));
        let fallback = &parsed.entries[2];
        assert_eq!(
            (
                fallback.input,
                fallback.cache_read,
                fallback.cache_write,
                fallback.output
            ),
            (30, 5, 5, 4)
        );
    }

    #[test]
    fn regressions_reset_only_the_present_field_and_torn_rows_resume() {
        let contents = concat!(
            "not-json\n",
            r#"{"type":"session.shutdown","timestamp":"2026-07-14T06:00:00Z","data":{"modelMetrics":{"m":{"usage":{"inputTokens":100,"outputTokens":50,"cacheReadTokens":20}}}}}"#,
            "\n",
            r#"{"type":"session.shutdown","timestamp":"2026-07-14T07:00:00Z","data":{"modelMetrics":{"m":{"tokenDetails":{"input":{"tokenCount":70}}}}}}"#,
            "\n",
            r#"{"type":"session.shutdown","timestamp":"2026-07-14T08:00:00Z","data":{"modelMetrics":{"m":{"tokenDetails":{"input":{"tokenCount":75},"output":{"tokenCount":60}}}}}}"#,
            "\n",
            r#"{"type":"session.shutdown","timestamp":"2026-07-14T09:00:00Z","data":{"modelMetrics":{"m":{"usage":{"inputTokens":999"#,
        );
        let (_dir, path) = transcript(contents);
        let first = parse(&path, None, &PriceBook::embedded());
        assert_eq!(first.entries.len(), 2, "the pure regression emits no entry");
        assert_eq!(
            (
                first.entries[0].input,
                first.entries[0].cache_read,
                first.entries[0].output
            ),
            (80, 20, 50)
        );
        assert_eq!((first.entries[1].input, first.entries[1].output), (5, 10));

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b",\"outputTokens\":61}}}}}\n").unwrap();
        let resumed = parse(&path, Some(&first.cursor), &PriceBook::embedded());
        assert_eq!(resumed.entries.len(), 1);
        assert_eq!(resumed.entries[0].output, 1);
    }
}
