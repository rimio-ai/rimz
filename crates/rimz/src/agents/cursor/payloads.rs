//! Tolerant Cursor hook payloads.

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize)]
pub(super) struct ModelParam {
    pub id: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[expect(
    dead_code,
    reason = "typed common and per-event fields pin Cursor's wire for future diagnostics"
)]
pub(super) struct CursorHookPayload {
    pub conversation_id: Option<String>,
    pub generation_id: Option<String>,
    pub model: Option<String>,
    pub model_id: Option<String>,
    #[serde(default)]
    pub model_params: Vec<ModelParam>,
    pub hook_event_name: Option<String>,
    pub cursor_version: Option<String>,
    #[serde(default)]
    pub workspace_roots: Vec<String>,
    pub transcript_path: Option<String>,
    pub prompt: Option<String>,
    pub tool_name: Option<String>,
    pub status: Option<String>,
    pub trigger: Option<String>,
    pub context_usage_percent: Option<f64>,
    pub context_tokens: Option<u64>,
    pub context_window_size: Option<u64>,
    pub reason: Option<String>,
}

pub(super) fn parse_payload(payload: &Value) -> CursorHookPayload {
    serde_json::from_value(payload.clone()).unwrap_or_default()
}

impl CursorHookPayload {
    pub(super) fn model_param(&self, id: &str) -> Option<&str> {
        self.model_params
            .iter()
            .find(|param| param.id == id)
            .map(|param| param.value.trim())
            .filter(|value| !value.is_empty())
    }
}
