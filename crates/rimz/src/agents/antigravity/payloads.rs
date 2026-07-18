//! Typed projections of Antigravity's command-hook payloads.

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct CommonPayload {
    pub conversation_id: Option<String>,
    #[serde(rename = "conversationId")]
    pub conversation_id_camel: Option<String>,
    #[serde(rename = "workspacePaths")]
    pub workspace_paths: Vec<String>,
    #[serde(rename = "transcriptPath")]
    pub transcript_path: Option<String>,
    #[serde(rename = "modelName")]
    pub model_name: Option<String>,
}

impl CommonPayload {
    pub(super) fn conversation_id(&self) -> Option<&str> {
        self.conversation_id
            .as_deref()
            .or(self.conversation_id_camel.as_deref())
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct InvocationPayload {
    #[serde(flatten)]
    pub common: CommonPayload,
    #[serde(rename = "invocationNum")]
    pub invocation_num: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct PostToolPayload {
    #[serde(flatten)]
    pub common: CommonPayload,
    pub error: Option<Value>,
}

impl PostToolPayload {
    pub(super) fn failed(&self) -> bool {
        match self.error.as_ref() {
            None | Some(Value::Null) => false,
            Some(Value::String(value)) => !value.trim().is_empty(),
            Some(Value::Bool(false)) => false,
            Some(Value::Array(values)) => !values.is_empty(),
            Some(Value::Object(values)) => !values.is_empty(),
            Some(_) => true,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct StopPayload {
    #[serde(flatten)]
    pub common: CommonPayload,
    #[serde(rename = "terminationReason")]
    pub termination_reason: Option<String>,
    pub error: Option<Value>,
    #[serde(rename = "fullyIdle")]
    pub fully_idle: Option<bool>,
}

impl StopPayload {
    pub(super) fn failed(&self) -> bool {
        let reason = self
            .termination_reason
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();
        matches!(reason, "error" | "max_steps_exceeded") || value_is_error(self.error.as_ref())
    }
}

fn value_is_error(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::String(value)) => !value.trim().is_empty(),
        Some(Value::Bool(false)) => false,
        Some(Value::Array(values)) => !values.is_empty(),
        Some(Value::Object(values)) => !values.is_empty(),
        Some(_) => true,
    }
}

pub(super) fn parse_invocation(payload: &Value) -> Option<InvocationPayload> {
    serde_json::from_value(payload.clone()).ok()
}

pub(super) fn parse_common(payload: &Value) -> Option<CommonPayload> {
    serde_json::from_value(payload.clone()).ok()
}

pub(super) fn parse_post_tool(payload: &Value) -> Option<PostToolPayload> {
    serde_json::from_value(payload.clone()).ok()
}

pub(super) fn parse_stop(payload: &Value) -> Option<StopPayload> {
    serde_json::from_value(payload.clone()).ok()
}
