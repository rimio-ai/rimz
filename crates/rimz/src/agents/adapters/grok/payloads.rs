//! Tolerant Grok Build hook payloads.

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct HookPayload {
    pub hook_event_name: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub workspace_root: Option<String>,
    /// RFC3339 on the hook wire. Persisted update rows use numeric Unix seconds.
    pub timestamp: Option<String>,
    pub transcript_path: Option<String>,
    pub prompt_id: Option<String>,
    pub source: Option<String>,
    pub model_id: Option<String>,
    pub agent_type: Option<String>,
    pub prompt: Option<String>,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub notification_type: Option<String>,
    pub message: Option<String>,
    pub title: Option<String>,
    pub level: Option<String>,
    pub error: Option<String>,
    pub reason: Option<String>,
    pub subagent_id: Option<String>,
    pub subagent_type: Option<String>,
    pub description: Option<String>,
    pub exit_code: Option<i32>,
}

pub(super) fn parse(payload: &Value) -> HookPayload {
    serde_json::from_value(payload.clone()).unwrap_or_default()
}

pub(super) fn notification_label(payload: &HookPayload) -> Option<&str> {
    payload
        .message
        .as_deref()
        .or(payload.title.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}
