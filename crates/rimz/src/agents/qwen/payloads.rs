//! Typed, drift-tolerant Qwen Code hook payloads.

#![allow(dead_code)]

use serde::Deserialize;
use serde_json::Value;

use crate::agents::hook_types::{BackgroundTask, CompactTrigger, HookEventCommon, SessionSource};

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
}
