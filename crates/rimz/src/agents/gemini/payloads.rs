//! Typed Gemini hook and transcript wire shapes.
//!
//! Hook payloads are sparse by event, so one tolerant envelope owns the common
//! and event-specific fields. Transcript lines use the same tolerant approach:
//! message records, `$set.messages` checkpoints, and `$rewindTo` markers fold
//! into one active ordered message list.

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

#[derive(Clone, Debug, Default, Deserialize)]
#[allow(dead_code)] // The typed wire intentionally mirrors every installed event field.
pub(super) struct GeminiHookPayload {
    pub session_id: Option<String>,
    pub transcript_path: Option<String>,
    pub cwd: Option<String>,
    pub hook_event_name: Option<String>,
    pub timestamp: Option<String>,
    pub source: Option<String>,
    pub reason: Option<String>,
    pub prompt: Option<String>,
    pub prompt_response: Option<String>,
    pub stop_hook_active: Option<bool>,
    pub tool_name: Option<String>,
    pub tool_input: Option<GeminiToolInput>,
    pub tool_response: Option<Value>,
    pub notification_type: Option<String>,
    pub message: Option<String>,
    pub details: Option<GeminiNotificationDetails>,
    pub trigger: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // Detail variants grow independently; keep the known wire typed.
pub(super) struct GeminiNotificationDetails {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub title: Option<String>,
    pub file_name: Option<String>,
    pub file_path: Option<String>,
    pub command: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[allow(dead_code)] // Both plan fields are retained across Gemini CLI wire versions.
pub(super) struct GeminiToolInput {
    pub questions: Option<Vec<GeminiQuestion>>,
    pub plan_path: Option<String>,
    pub plan_filename: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[allow(dead_code)] // Header/type/placeholder are preserved although AskQuestion is narrower.
pub(super) struct GeminiQuestion {
    pub question: Option<String>,
    pub header: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub options: Option<Vec<GeminiQuestionOption>>,
    #[serde(rename = "multiSelect")]
    pub multi_select: Option<bool>,
    pub placeholder: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct GeminiQuestionOption {
    pub label: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct GeminiTokens {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cached: Option<u64>,
    pub thoughts: Option<u64>,
    pub tool: Option<u64>,
    pub total: Option<u64>,
}

impl<'de> Deserialize<'de> for GeminiTokens {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = Map::<String, Value>::deserialize(deserializer)?;
        Ok(Self {
            input: first_token(
                &fields,
                &["input", "prompt", "input_tokens", "prompt_tokens"],
            ),
            output: first_token(
                &fields,
                &["output", "candidates", "output_tokens", "candidates_tokens"],
            ),
            cached: first_token(&fields, &["cached", "cached_tokens"]),
            thoughts: first_token(
                &fields,
                &[
                    "thoughts",
                    "reasoning",
                    "thoughts_tokens",
                    "reasoning_tokens",
                ],
            ),
            tool: first_token(&fields, &["tool", "tool_tokens"]),
            total: first_token(&fields, &["total", "total_tokens"]),
        })
    }
}

fn first_token(fields: &Map<String, Value>, names: &[&str]) -> Option<u64> {
    for name in names {
        if let Some(value) = fields.get(*name) {
            return token_scalar(value);
        }
    }
    None
}

fn token_scalar(value: &Value) -> Option<u64> {
    let number = match value {
        Value::Number(value) => value.as_f64(),
        Value::String(value) => value.trim().parse::<f64>().ok(),
        _ => None,
    }?;
    (number.is_finite() && number >= 0.0 && number <= u64::MAX as f64)
        .then(|| number.trunc() as u64)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NormalizedUsage {
    pub fresh_input: u64,
    pub cache_read: u64,
    pub billable_output: u64,
    pub context_total: u64,
}

impl GeminiTokens {
    pub fn normalized(&self) -> Option<NormalizedUsage> {
        if [
            self.input,
            self.output,
            self.cached,
            self.thoughts,
            self.tool,
            self.total,
        ]
        .iter()
        .all(Option::is_none)
        {
            return None;
        }
        let input = self.input.unwrap_or(0);
        let output = self.output.unwrap_or(0);
        let cache_read = self.cached.unwrap_or(0);
        let thoughts = self.thoughts.unwrap_or(0);
        let tool = self.tool.unwrap_or(0);
        let exclusive_cache = self.total.is_some_and(|total| {
            total
                == input
                    .saturating_add(cache_read)
                    .saturating_add(output)
                    .saturating_add(thoughts)
                    .saturating_add(tool)
        });
        Some(NormalizedUsage {
            fresh_input: if exclusive_cache {
                input
            } else {
                input.saturating_sub(cache_read)
            }
            .saturating_add(tool),
            cache_read,
            billable_output: output.saturating_add(thoughts),
            context_total: self.total.unwrap_or_else(|| {
                input
                    .saturating_add(output)
                    .saturating_add(thoughts)
                    .saturating_add(tool)
            }),
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct GeminiMessage {
    pub id: Option<String>,
    pub timestamp: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub content: Option<Value>,
    pub display_content: Option<Value>,
    pub model: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::agents::transcript_fs::deserialize_optional_object_lossy"
    )]
    pub tokens: Option<GeminiTokens>,
    pub tool_calls: Option<Vec<GeminiToolCall>>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct GeminiToolCall {
    pub id: Option<String>,
    pub name: Option<String>,
    pub args: Option<Value>,
    pub result: Option<Value>,
    pub status: Option<String>,
    pub timestamp: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptRecord {
    #[serde(rename = "$set")]
    set: Option<TranscriptSet>,
    #[serde(rename = "$rewindTo")]
    rewind_to: Option<String>,
    messages: Option<Vec<GeminiMessage>>,
    session_id: Option<String>,
    id: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    content: Option<Value>,
    display_content: Option<Value>,
    model: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::agents::transcript_fs::deserialize_optional_object_lossy"
    )]
    tokens: Option<GeminiTokens>,
    tool_calls: Option<Vec<GeminiToolCall>>,
}

#[derive(Debug, Default, Deserialize)]
struct TranscriptSet {
    messages: Option<Vec<GeminiMessage>>,
}

impl TranscriptRecord {
    fn into_message(self) -> Option<GeminiMessage> {
        self.kind.as_ref()?;
        Some(GeminiMessage {
            id: self.id,
            timestamp: self.timestamp,
            kind: self.kind,
            content: self.content,
            display_content: self.display_content,
            model: self.model,
            tokens: self.tokens,
            tool_calls: self.tool_calls,
        })
    }
}

#[derive(Debug, Default)]
pub(super) struct FoldedTranscript {
    pub session_id: Option<String>,
    pub messages: Vec<GeminiMessage>,
}

impl FoldedTranscript {
    pub fn latest_gemini(&self) -> Option<&GeminiMessage> {
        self.messages.iter().rev().find(|message| {
            message.kind.as_deref() == Some("gemini")
                && message
                    .tokens
                    .as_ref()
                    .and_then(GeminiTokens::normalized)
                    .is_some()
        })
    }

    pub fn apply(&mut self, text: &str) -> Vec<TranscriptChange> {
        apply_jsonl(self, text)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TranscriptChange {
    Ordinary {
        id: Option<String>,
        assistant: bool,
        introduced: bool,
    },
    Control,
}

pub(super) fn parse_hook(payload: &Value) -> GeminiHookPayload {
    serde_json::from_value(payload.clone()).unwrap_or_default()
}

/// Fold a JSONL transcript or a legacy whole-record JSON document.
pub(super) fn fold_transcript(text: &str) -> FoldedTranscript {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return FoldedTranscript::default();
    }

    if let Ok(record) = serde_json::from_str::<TranscriptRecord>(trimmed)
        && record.messages.is_some()
    {
        return FoldedTranscript {
            session_id: record.session_id,
            messages: record.messages.unwrap_or_default(),
        };
    }

    let mut folded = FoldedTranscript::default();
    folded.apply(trimmed);
    folded
}

fn apply_jsonl(folded: &mut FoldedTranscript, text: &str) -> Vec<TranscriptChange> {
    let mut changes = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Ok(mut record) = serde_json::from_str::<TranscriptRecord>(line) else {
            continue;
        };
        if folded.session_id.is_none() {
            folded.session_id = record.session_id.take();
        }
        if let Some(messages) = record.set.take().and_then(|set| set.messages) {
            folded.messages = dedup_messages(messages);
            changes.push(TranscriptChange::Control);
            continue;
        }
        if let Some(rewind_to) = record.rewind_to.as_deref() {
            if let Some(index) = folded
                .messages
                .iter()
                .position(|message| message.id.as_deref() == Some(rewind_to))
            {
                folded.messages.truncate(index);
            } else {
                // A bounded tail may begin after the target. Clearing avoids
                // presenting an abandoned branch as live context; subsequent
                // messages in the same tail still rebuild the active suffix.
                folded.messages.clear();
            }
            changes.push(TranscriptChange::Control);
            continue;
        }
        if let Some(message) = record.into_message() {
            let id = message.id.clone();
            let introduced = id.as_deref().is_none_or(|id| {
                !folded
                    .messages
                    .iter()
                    .any(|existing| existing.id.as_deref() == Some(id))
            });
            let assistant = message.kind.as_deref() == Some("gemini");
            replace_message(&mut folded.messages, message);
            changes.push(TranscriptChange::Ordinary {
                id,
                assistant,
                introduced,
            });
        }
    }
    changes
}

fn dedup_messages(messages: Vec<GeminiMessage>) -> Vec<GeminiMessage> {
    let mut deduped = Vec::with_capacity(messages.len());
    for message in messages {
        replace_message(&mut deduped, message);
    }
    deduped
}

fn replace_message(messages: &mut Vec<GeminiMessage>, message: GeminiMessage) {
    if let Some(id) = message.id.as_deref()
        && let Some(index) = messages
            .iter()
            .position(|existing| existing.id.as_deref() == Some(id))
    {
        messages[index] = message;
    } else {
        messages.push(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_fold_replaces_checkpoints_and_rewinds() {
        let folded = fold_transcript(
            r#"{"sessionId":"sess-1"}
{"id":"a","type":"gemini","model":"gemini-3-pro-preview","tokens":{"total":10}}
{"id":"a","type":"gemini","model":"gemini-3-pro-preview","tokens":{"total":20}}
{"id":"b","type":"gemini","tokens":{"total":30}}
{"$rewindTo":"b"}
{"$set":{"messages":[{"id":"c","type":"gemini","tokens":{"total":40}}]}}
{"id":"d","type":"gemini","tokens":{"total":50}}"#,
        );
        assert_eq!(folded.session_id.as_deref(), Some("sess-1"));
        assert_eq!(folded.messages.len(), 2);
        assert_eq!(folded.latest_gemini().unwrap().id.as_deref(), Some("d"));
    }

    #[test]
    fn unknown_tool_status_and_notification_kind_are_tolerated() {
        let payload = parse_hook(&serde_json::json!({
            "details": { "type": "future_permission", "title": "Future" },
            "tool_response": { "status": "future_status" }
        }));
        assert_eq!(
            payload.details.and_then(|details| details.kind).as_deref(),
            Some("future_permission")
        );
    }

    #[test]
    fn token_aliases_are_lossy_per_field_with_stable_precedence() {
        let message: GeminiMessage = serde_json::from_value(serde_json::json!({
            "tokens": {
                "input": "bad",
                "prompt": 99,
                "candidates_tokens": "12.9",
                "cached_tokens": 3.7,
                "reasoning": -1,
                "tool_tokens": "4",
                "total_tokens": 20
            }
        }))
        .unwrap();
        let tokens = message.tokens.unwrap();
        assert_eq!(tokens.input, None, "the first present alias owns the field");
        assert_eq!(tokens.output, Some(12));
        assert_eq!(tokens.cached, Some(3));
        assert_eq!(tokens.thoughts, None);
        assert_eq!(tokens.tool, Some(4));
        assert_eq!(tokens.total, Some(20));

        let folded = fold_transcript(
            r#"{"id":"a","type":"gemini","content":"kept","tokens":"future-shape"}"#,
        );
        assert_eq!(folded.messages.len(), 1);
        assert!(folded.messages[0].tokens.is_none());
    }

    #[test]
    fn normalized_usage_handles_current_and_proven_legacy_cache_shapes() {
        let current = GeminiTokens {
            input: Some(100),
            output: Some(20),
            cached: Some(40),
            thoughts: Some(5),
            tool: Some(7),
            total: Some(132),
        }
        .normalized()
        .unwrap();
        assert_eq!(current.fresh_input, 67);
        assert_eq!(current.cache_read, 40);
        assert_eq!(current.billable_output, 25);
        assert_eq!(current.context_total, 132);

        let legacy = GeminiTokens {
            input: Some(100),
            output: Some(20),
            cached: Some(40),
            thoughts: Some(5),
            tool: Some(7),
            total: Some(172),
        }
        .normalized()
        .unwrap();
        assert_eq!(legacy.fresh_input, 107);
        assert_eq!(legacy.context_total, 172);

        let derived = GeminiTokens {
            input: Some(100),
            output: Some(20),
            cached: Some(40),
            thoughts: Some(5),
            tool: Some(7),
            total: None,
        }
        .normalized()
        .unwrap();
        assert_eq!(derived.context_total, 132);
    }
}
