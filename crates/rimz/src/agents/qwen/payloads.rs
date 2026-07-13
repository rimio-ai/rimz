//! Typed, drift-tolerant Qwen Code hook payloads.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

use crate::agents::hook_types::{BackgroundTask, CompactTrigger, HookEventCommon, SessionSource};
use crate::agents::transcript_fs::{
    deserialize_optional_object_lossy, deserialize_optional_u64_lossy,
};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QwenCommon {
    #[serde(flatten)]
    pub common: HookEventCommon,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QwenSessionStart {
    #[serde(flatten)]
    pub common: QwenCommon,
    pub source: SessionSource,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QwenUserPromptSubmit {
    #[serde(flatten)]
    pub common: QwenCommon,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QwenToolUse {
    #[serde(flatten)]
    pub common: QwenCommon,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub tool_response: Option<Value>,
    pub error: Option<String>,
    pub is_interrupt: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QwenStop {
    #[serde(flatten)]
    pub common: QwenCommon,
    pub last_assistant_message: Option<String>,
    pub background_tasks: Vec<BackgroundTask>,
    pub crons: Vec<QwenCron>,
    pub context_usage: Option<f64>,
    pub context_limit: Option<u64>,
    pub input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QwenCron {
    pub id: Option<String>,
    pub status: Option<String>,
    pub prompt: Option<String>,
    pub schedule: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QwenStopFailure {
    #[serde(flatten)]
    pub common: QwenCommon,
    pub error: QwenStopError,
    pub error_details: Option<Value>,
    pub last_assistant_message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QwenStopError {
    RateLimit,
    AuthenticationFailed,
    BillingError,
    InvalidRequest,
    ServerError,
    MaxOutputTokens,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QwenSessionEnd {
    #[serde(flatten)]
    pub common: QwenCommon,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QwenNotification {
    #[serde(flatten)]
    pub common: QwenCommon,
    pub message: Option<String>,
    pub title: Option<String>,
    pub notification_type: Option<String>,
}

pub type QwenPreToolUse = QwenToolUse;
pub type QwenPostToolUse = QwenToolUse;
pub type QwenPostToolUseFailure = QwenToolUse;
pub type QwenPermissionRequest = QwenToolUse;
pub type QwenSubagentStart = QwenSubagent;
pub type QwenSubagentStop = QwenSubagent;
pub type QwenPreCompact = QwenCompact;
pub type QwenPostCompact = QwenCompact;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QwenSubagent {
    #[serde(flatten)]
    pub common: QwenCommon,
    pub agent_transcript_path: Option<String>,
    pub last_assistant_message: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct QwenCompact {
    #[serde(flatten)]
    pub common: QwenCommon,
    pub trigger: CompactTrigger,
}

/// One Qwen session-JSONL record, in the Google `Content` shape Qwen persists.
/// Only the fields RimZ reads to reconstruct the main-thread conversation are
/// typed; every other key is tolerated and ignored.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TranscriptRecord {
    pub uuid: Option<String>,
    pub parent_uuid: Option<String>,
    pub session_id: Option<String>,
    pub r#type: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub context_window_size: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    pub usage_metadata: Option<TranscriptUsage>,
    pub message: TranscriptContent,
    pub is_sidechain: Option<bool>,
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TranscriptUsage {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub prompt_token_count: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub cached_content_token_count: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub candidates_token_count: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub thoughts_token_count: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub tool_use_prompt_token_count: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    pub total_token_count: Option<u64>,
}

impl TranscriptUsage {
    pub fn uncached_prompt(&self) -> u64 {
        self.prompt_token_count
            .unwrap_or(0)
            .saturating_sub(self.cache_read())
    }

    pub fn cache_read(&self) -> u64 {
        self.cached_content_token_count.unwrap_or(0)
    }

    pub fn output(&self) -> u64 {
        self.candidates_token_count
            .unwrap_or(0)
            .saturating_add(self.thoughts_token_count.unwrap_or(0))
    }

    pub fn live_total(&self) -> Option<u64> {
        self.total_token_count.or_else(|| {
            (self.prompt_token_count.is_some()
                || self.candidates_token_count.is_some()
                || self.thoughts_token_count.is_some()
                || self.tool_use_prompt_token_count.is_some())
            .then(|| {
                self.prompt_token_count
                    .unwrap_or(0)
                    .saturating_add(self.candidates_token_count.unwrap_or(0))
                    .saturating_add(self.thoughts_token_count.unwrap_or(0))
                    .saturating_add(self.tool_use_prompt_token_count.unwrap_or(0))
            })
        })
    }
}

#[derive(Debug, Default)]
pub struct FoldedTranscript {
    pub physical: Vec<TranscriptRecord>,
    active_root: Vec<usize>,
}

impl FoldedTranscript {
    pub fn active_root(&self) -> impl DoubleEndedIterator<Item = &TranscriptRecord> {
        self.active_root.iter().map(|index| &self.physical[*index])
    }

    pub fn latest_active_assistant_with_usage(&self) -> Option<&TranscriptRecord> {
        self.active_root().rev().find(|record| {
            record.r#type.as_deref() == Some("assistant")
                && record.agent_id.is_none()
                && record.is_sidechain != Some(true)
                && record.usage_metadata.is_some()
        })
    }
}

/// Parse complete JSONL and select the latest root record's UUID ancestry.
/// Legacy transcripts without a usable root UUID retain physical ordering.
pub fn fold_transcript(text: &str) -> FoldedTranscript {
    let physical = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<TranscriptRecord>(line).ok())
        .collect::<Vec<_>>();
    let mut by_uuid = HashMap::new();
    let mut tail = None;
    for (index, record) in physical.iter().enumerate() {
        if let Some(uuid) = record.uuid.as_deref().filter(|uuid| !uuid.is_empty()) {
            by_uuid.insert(uuid, index);
            if record.agent_id.is_none() && record.is_sidechain != Some(true) {
                tail = Some(index);
            }
        }
    }
    let active_root = if let Some(mut index) = tail {
        let mut reversed = Vec::new();
        let mut visited = HashSet::new();
        loop {
            let record = &physical[index];
            let Some(uuid) = record.uuid.as_deref() else {
                break;
            };
            if !visited.insert(uuid) {
                break;
            }
            reversed.push(index);
            let Some(parent) = record.parent_uuid.as_deref() else {
                break;
            };
            let Some(parent_index) = by_uuid.get(parent).copied() else {
                break;
            };
            index = parent_index;
        }
        reversed.reverse();
        reversed
    } else {
        (0..physical.len()).collect()
    };
    FoldedTranscript {
        physical,
        active_root,
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TranscriptContent {
    pub parts: Vec<TranscriptPart>,
}

impl TranscriptContent {
    /// Join the record's visible text, newest model thinking excluded. Thought
    /// parts (`thought: true`) and `functionCall`/`functionResponse` parts carry
    /// no user-visible prose, so only non-thought `text` parts contribute.
    pub fn visible_text(&self) -> String {
        self.parts
            .iter()
            .filter(|part| part.thought != Some(true))
            .filter_map(|part| {
                let text = part.text.as_deref()?.trim();
                (!text.is_empty()).then_some(text)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TranscriptPart {
    pub text: Option<String>,
    pub thought: Option<bool>,
}

macro_rules! parse_fn {
    ($name:ident, $ty:ty) => {
        pub fn $name(payload: &Value) -> $ty {
            serde_json::from_value(payload.clone()).unwrap_or_default()
        }
    };
}

parse_fn!(parse_session_start, QwenSessionStart);
parse_fn!(parse_user_prompt_submit, QwenUserPromptSubmit);
parse_fn!(parse_tool_use, QwenToolUse);
parse_fn!(parse_stop, QwenStop);
parse_fn!(parse_stop_failure, QwenStopFailure);
parse_fn!(parse_subagent, QwenSubagent);
parse_fn!(parse_compact, QwenCompact);

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn sparse_and_future_payloads_parse() {
        assert_eq!(
            parse_session_start(&json!({"source": "branch"})).source,
            SessionSource::Unknown
        );
        let stop = parse_stop(&json!({
            "context_usage": 0.5,
            "background_tasks": [{"id": "job-1", "status": "running"}],
            "future": true
        }));
        assert_eq!(stop.context_usage, Some(0.5));
        assert_eq!(stop.background_tasks.len(), 1);
    }

    #[test]
    fn transcript_fold_follows_latest_root_ancestry() {
        let folded = fold_transcript(
            r#"{"uuid":"u1","type":"user"}
{"uuid":"a1","parentUuid":"u1","type":"assistant"}
{"uuid":"u2","parentUuid":"a1","type":"user"}
{"uuid":"a2","parentUuid":"u2","type":"assistant"}
{"uuid":"rewind","parentUuid":"a1","type":"system"}
{"uuid":"u3","parentUuid":"rewind","type":"user"}
{"uuid":"a3","parentUuid":"u3","type":"assistant","usageMetadata":{"totalTokenCount":"42"}}
{"uuid":"child","parentUuid":"a3","type":"assistant","agentId":"child"}"#,
        );
        let ids = folded
            .active_root()
            .filter_map(|record| record.uuid.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["u1", "a1", "rewind", "u3", "a3"]);
        assert_eq!(
            folded
                .latest_active_assistant_with_usage()
                .and_then(|record| record.usage_metadata.as_ref())
                .and_then(TranscriptUsage::live_total),
            Some(42)
        );
    }

    #[test]
    fn transcript_fold_is_last_wins_and_stops_at_missing_or_cycles() {
        let duplicate = fold_transcript(
            r#"{"uuid":"root","type":"user","message":{"parts":[{"text":"old"}]}}
{"uuid":"root","type":"user","message":{"parts":[{"text":"new"}]}}
{"uuid":"tail","parentUuid":"root","type":"assistant"}"#,
        );
        assert_eq!(duplicate.active_root().count(), 2);
        assert_eq!(
            duplicate
                .active_root()
                .next()
                .unwrap()
                .message
                .visible_text(),
            "new"
        );

        let missing =
            fold_transcript(r#"{"uuid":"tail","parentUuid":"missing","type":"assistant"}"#);
        assert_eq!(missing.active_root().count(), 1);

        let cycle = fold_transcript(
            r#"{"uuid":"a","parentUuid":"b","type":"user"}
{"uuid":"b","parentUuid":"a","type":"assistant"}"#,
        );
        assert_eq!(cycle.active_root().count(), 2);
    }

    #[test]
    fn transcript_fold_preserves_legacy_order_and_fails_soft() {
        let folded = fold_transcript("{\"type\":\"user\"}\nnot json\n{\"type\":\"assistant\"}\n");
        assert_eq!(folded.physical.len(), 2);
        assert_eq!(folded.active_root().count(), 2);
    }

    #[test]
    fn usage_is_lossy_and_keeps_known_categories_distinct() {
        let record = serde_json::from_value::<TranscriptRecord>(json!({
            "type": "assistant",
            "contextWindowSize": "131072",
            "usageMetadata": {
                "promptTokenCount": "100",
                "cachedContentTokenCount": 25,
                "candidatesTokenCount": "10",
                "thoughtsTokenCount": false,
                "toolUsePromptTokenCount": 5,
                "totalTokenCount": []
            }
        }))
        .unwrap();
        let usage = record.usage_metadata.unwrap();
        assert_eq!(record.context_window_size, Some(131_072));
        assert_eq!(usage.uncached_prompt(), 75);
        assert_eq!(usage.cache_read(), 25);
        assert_eq!(usage.output(), 10);
        assert_eq!(usage.live_total(), Some(115));
    }
}
