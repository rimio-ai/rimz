//! Typed Gemini hook and transcript wire shapes.
//!
//! Hook payloads are sparse by event, so one tolerant envelope owns the common
//! and event-specific fields. Transcript lines use the same tolerant approach:
//! message records, `$set.messages` checkpoints, and `$rewindTo` markers fold
//! into one active ordered message list.

use serde::Deserialize;
use serde_json::Value;

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
#[allow(dead_code)] // plan_path is retained for the native plan-approval payload.
pub(super) struct GeminiToolInput {
    pub questions: Option<Vec<GeminiQuestion>>,
    pub plan_path: Option<String>,
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

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub(super) struct GeminiTokens {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cached: Option<u64>,
    pub thoughts: Option<u64>,
    pub tool: Option<u64>,
    pub total: Option<u64>,
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
        self.messages
            .iter()
            .rev()
            .find(|message| message.kind.as_deref() == Some("gemini") && message.tokens.is_some())
    }
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
    for line in trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(mut record) = serde_json::from_str::<TranscriptRecord>(line) else {
            continue;
        };
        if folded.session_id.is_none() {
            folded.session_id = record.session_id.take();
        }
        if let Some(messages) = record.set.take().and_then(|set| set.messages) {
            folded.messages = dedup_messages(messages);
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
            continue;
        }
        if let Some(message) = record.into_message() {
            replace_message(&mut folded.messages, message);
        }
    }
    folded
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
}
