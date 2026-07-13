//! Tolerant decoder for Amp's private rewritten thread cache.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::agents::{TranscriptMessage, TranscriptRole, sanitize_user_prompt};

#[derive(Clone, Debug)]
pub(super) struct AmpThread {
    pub(super) id: String,
    pub(super) messages: Vec<AmpMessage>,
    pub(super) usage: Vec<AmpUsage>,
}

#[derive(Clone, Debug)]
pub(super) struct AmpMessage {
    pub(super) role: TranscriptRole,
    pub(super) at: Option<Timestamp>,
    pub(super) text: String,
    complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AmpUsage {
    pub(super) at: Timestamp,
    pub(super) model: String,
    pub(super) native_id: Option<String>,
    pub(super) input: u64,
    pub(super) output: u64,
    pub(super) cache_write: u64,
    pub(super) cache_read: u64,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadRoot {
    id: Option<Value>,
    #[serde(default)]
    messages: Value,
    #[serde(default)]
    usage_ledger: Value,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMessage {
    role: Option<String>,
    content: Option<Value>,
    timestamp: Option<Value>,
    model: Option<Value>,
    #[serde(alias = "id")]
    message_id: Option<Value>,
    usage: Option<Value>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLedgerEvent {
    id: Option<Value>,
    timestamp: Option<Value>,
    model: Option<Value>,
    tokens: Option<Value>,
    to_message_id: Option<Value>,
}

impl AmpThread {
    pub(super) fn read(path: &Path) -> io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text)
    }

    pub(super) fn parse(text: &str) -> io::Result<Self> {
        let root = serde_json::from_str::<ThreadRoot>(text).map_err(invalid_data)?;
        let id = normalized_id(root.id.as_ref()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "Amp thread is missing its id")
        })?;

        let raw_messages = root
            .messages
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| serde_json::from_value::<RawMessage>(value.clone()).ok())
            .collect::<Vec<_>>();
        let message_usage = parse_message_usage(&raw_messages);
        let ledger_usage = parse_ledger_usage(&root.usage_ledger, &raw_messages);
        let (usage, completed_ids, completed_indices) = if ledger_usage.is_empty() {
            let completed_ids = message_usage
                .iter()
                .filter_map(|usage| usage.message_id.clone())
                .collect();
            let completed_indices = message_usage
                .iter()
                .filter_map(|usage| usage.message_index)
                .collect();
            (
                message_usage.into_iter().map(|usage| usage.usage).collect(),
                completed_ids,
                completed_indices,
            )
        } else {
            let completed = ledger_usage
                .iter()
                .filter_map(|usage| usage.message_id.clone())
                .collect();
            (
                ledger_usage.into_iter().map(|usage| usage.usage).collect(),
                completed,
                HashSet::new(),
            )
        };

        let messages = raw_messages
            .into_iter()
            .enumerate()
            .filter_map(|(index, message)| {
                normalize_message(message, index, &completed_ids, &completed_indices)
            })
            .collect();
        Ok(Self {
            id,
            messages,
            usage,
        })
    }

    pub(super) fn transcript_messages(&self) -> Vec<TranscriptMessage> {
        self.messages
            .iter()
            .map(|message| TranscriptMessage {
                role: message.role,
                at: message.at,
                text: message.text.clone(),
            })
            .collect()
    }

    pub(super) fn completed_assistant_messages(&self) -> Vec<String> {
        self.messages
            .iter()
            .filter(|message| message.role == TranscriptRole::Assistant && message.complete)
            .map(|message| message.text.clone())
            .collect()
    }
}

struct UsageWithMessage {
    usage: AmpUsage,
    message_id: Option<String>,
    message_index: Option<usize>,
}

fn parse_message_usage(messages: &[RawMessage]) -> Vec<UsageWithMessage> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role.as_deref() == Some("assistant"))
        .filter_map(|(message_index, message)| {
            let usage = message.usage.as_ref()?.as_object()?;
            let at = string_value(usage.get("timestamp"))
                .or_else(|| string_value(message.timestamp.as_ref()))?
                .parse::<Timestamp>()
                .ok()?;
            let model = string_value(usage.get("model"))
                .or_else(|| string_value(message.model.as_ref()))?;
            let (input, output, cache_write, cache_read) = token_parts(
                usage.get("inputTokens"),
                usage.get("outputTokens"),
                usage.get("cacheCreationInputTokens"),
                usage.get("cacheReadInputTokens"),
                usage.get("totalTokens"),
            )?;
            let message_id = normalized_id(message.message_id.as_ref());
            Some(UsageWithMessage {
                usage: AmpUsage {
                    at,
                    model,
                    native_id: message_id.clone(),
                    input,
                    output,
                    cache_write,
                    cache_read,
                },
                message_id,
                message_index: Some(message_index),
            })
        })
        .collect()
}

fn parse_ledger_usage(ledger: &Value, messages: &[RawMessage]) -> Vec<UsageWithMessage> {
    let cache_by_message = messages
        .iter()
        .filter(|message| message.role.as_deref() == Some("assistant"))
        .filter_map(|message| {
            let id = normalized_id(message.message_id.as_ref())?;
            let usage = message.usage.as_ref()?.as_object()?;
            Some((
                id,
                (
                    u64_value(usage.get("cacheCreationInputTokens")).unwrap_or(0),
                    u64_value(usage.get("cacheReadInputTokens")).unwrap_or(0),
                ),
            ))
        })
        .collect::<HashMap<_, _>>();
    let events = ledger
        .as_object()
        .and_then(|ledger| ledger.get("events"))
        .and_then(Value::as_array);
    events
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::from_value::<RawLedgerEvent>(value.clone()).ok())
        .filter_map(|event| {
            let at = string_value(event.timestamp.as_ref())?
                .parse::<Timestamp>()
                .ok()?;
            let model = string_value(event.model.as_ref())?;
            let tokens = event.tokens.as_ref()?.as_object()?;
            let message_id = normalized_id(event.to_message_id.as_ref());
            let (cache_write, cache_read) = message_id
                .as_ref()
                .and_then(|id| cache_by_message.get(id).copied())
                .unwrap_or_default();
            let cache_write_value = Value::from(cache_write);
            let cache_read_value = Value::from(cache_read);
            let (input, output, cache_write, cache_read) = token_parts(
                tokens.get("input"),
                tokens.get("output"),
                (cache_write > 0).then_some(&cache_write_value),
                (cache_read > 0).then_some(&cache_read_value),
                tokens.get("total"),
            )?;
            Some(UsageWithMessage {
                usage: AmpUsage {
                    at,
                    model,
                    native_id: normalized_id(event.id.as_ref()),
                    input,
                    output,
                    cache_write,
                    cache_read,
                },
                message_id,
                message_index: None,
            })
        })
        .collect()
}

fn normalize_message(
    message: RawMessage,
    message_index: usize,
    completed_ids: &HashSet<String>,
    completed_indices: &HashSet<usize>,
) -> Option<AmpMessage> {
    let role = match message.role.as_deref() {
        Some("user") => TranscriptRole::User,
        Some("assistant") => TranscriptRole::Assistant,
        _ => return None,
    };
    let visible = visible_text(message.content.as_ref());
    let text = match role {
        TranscriptRole::User => sanitize_user_prompt(visible.as_deref()),
        TranscriptRole::Assistant => visible,
    }?;
    let native_id = normalized_id(message.message_id.as_ref());
    let usage_at = message
        .usage
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|usage| string_value(usage.get("timestamp")))
        .and_then(|timestamp| timestamp.parse::<Timestamp>().ok());
    let message_at = string_value(message.timestamp.as_ref())
        .and_then(|timestamp| timestamp.parse::<Timestamp>().ok());
    Some(AmpMessage {
        role,
        at: if role == TranscriptRole::Assistant {
            usage_at.or(message_at)
        } else {
            message_at
        },
        text,
        complete: native_id
            .as_ref()
            .is_some_and(|id| completed_ids.contains(id))
            || completed_indices.contains(&message_index),
    })
}

fn visible_text(content: Option<&Value>) -> Option<String> {
    let parts = match content? {
        Value::String(text) => vec![text.trim()],
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                let block = block.as_object()?;
                (block.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| block.get("text").and_then(Value::as_str))
                    .flatten()
                    .map(str::trim)
            })
            .collect(),
        _ => return None,
    };
    let text = parts
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn token_parts(
    input: Option<&Value>,
    output: Option<&Value>,
    cache_write: Option<&Value>,
    cache_read: Option<&Value>,
    total: Option<&Value>,
) -> Option<(u64, u64, u64, u64)> {
    let split_present = [input, output, cache_write, cache_read]
        .into_iter()
        .any(|value| value.is_some());
    let input = u64_value(input).unwrap_or(0);
    let mut output = u64_value(output).unwrap_or(0);
    let cache_write = u64_value(cache_write).unwrap_or(0);
    let cache_read = u64_value(cache_read).unwrap_or(0);
    if !split_present {
        output = u64_value(total).unwrap_or(0);
    }
    (input > 0 || output > 0 || cache_write > 0 || cache_read > 0).then_some((
        input,
        output,
        cache_write,
        cache_read,
    ))
}

fn normalized_id(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        }
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn string_value(value: Option<&Value>) -> Option<String> {
    value?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn u64_value(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(value) => value.as_u64(),
        Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}

fn invalid_data(error: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_schema_filters_content_sanitizes_prompts_and_certifies_usage() {
        let thread = AmpThread::parse(
            r#"{
                "id":"T-current",
                "messages":[
                    {"role":"user","messageId":"1","timestamp":"2026-01-01T00:00:00Z","content":[{"type":"text","text":"  fix it  "},{"type":"tool_result","output":"secret"}]},
                    {"role":"user","messageId":"control","content":"<system-reminder>hidden</system-reminder>"},
                    {"role":"info","content":"hidden"},
                    {"role":"assistant","messageId":2,"timestamp":"2026-01-01T00:00:02Z","content":[{"type":"thinking","thinking":"hidden"},{"type":"text","text":" done "}],"usage":{"timestamp":"2026-01-01T00:00:03Z","model":"claude-sonnet-4-20250514","inputTokens":"10","outputTokens":2,"cacheCreationInputTokens":3,"cacheReadInputTokens":4}},
                    {"role":"assistant","messageId":3,"content":"still running"},
                    "malformed"
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(thread.id, "T-current");
        assert_eq!(thread.messages.len(), 3);
        assert_eq!(thread.messages[0].text, "fix it");
        assert_eq!(thread.messages[1].text, "done");
        assert_eq!(thread.messages[1].at.unwrap().as_second(), 1_767_225_603);
        assert_eq!(thread.completed_assistant_messages(), vec!["done"]);
        assert_eq!(thread.usage[0].input, 10);
        assert_eq!(thread.usage[0].cache_write, 3);
    }

    #[test]
    fn usable_ledger_wins_and_correlates_mixed_ids_and_cache_tokens() {
        let thread = AmpThread::parse(
            r#"{
                "id":"T-legacy",
                "messages":[
                    {"role":"assistant","messageId":7,"content":"legacy answer","usage":{"model":"ignored","timestamp":"2026-01-01T00:00:01Z","inputTokens":99,"cacheCreationInputTokens":30,"cacheReadInputTokens":40}}
                ],
                "usageLedger":{"events":[
                    {"id":8,"timestamp":"2026-01-01T00:00:02Z","model":"gpt-5","toMessageId":"7","tokens":{"input":"10","output":20}},
                    {"id":"bad","timestamp":"not-a-date","model":"gpt-5","tokens":{"input":100}}
                ]}
            }"#,
        )
        .unwrap();

        assert_eq!(thread.usage.len(), 1);
        assert_eq!(thread.usage[0].model, "gpt-5");
        assert_eq!(thread.usage[0].native_id.as_deref(), Some("8"));
        assert_eq!(thread.usage[0].cache_write, 30);
        assert_eq!(thread.usage[0].cache_read, 40);
        assert_eq!(thread.completed_assistant_messages(), vec!["legacy answer"]);
    }

    #[test]
    fn unusable_ledger_falls_back_and_total_only_stays_visible() {
        for ledger in [
            r#"{"events":[]}"#,
            r#"{"events":[{"model":"gpt-5"}]}"#,
            r#""bad""#,
        ] {
            let body = format!(
                r#"{{"id":"T-a","usageLedger":{ledger},"messages":[{{"role":"assistant","messageId":"a","content":"answer","usage":{{"timestamp":"2026-01-01T00:00:00Z","model":"gpt-5","totalTokens":"345"}}}}]}}"#
            );
            let thread = AmpThread::parse(&body).unwrap();
            assert_eq!(thread.usage[0].output, 345);
            assert_eq!(thread.completed_assistant_messages(), vec!["answer"]);
        }
    }

    #[test]
    fn malformed_root_and_missing_id_are_errors() {
        assert_eq!(
            AmpThread::parse("{").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            AmpThread::parse(r#"{"messages":[]}"#).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
