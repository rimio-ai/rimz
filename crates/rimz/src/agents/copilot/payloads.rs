//! Typed GitHub Copilot CLI command-hook payloads.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)] // Sparse event variants share this one typed wire shape.
pub(crate) struct CopilotHookPayload {
    #[serde(alias = "session_id")]
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub timestamp: Option<Value>,
    pub prompt: Option<String>,
    #[serde(alias = "tool_name")]
    pub tool_name: Option<String>,
    #[serde(
        alias = "tool_args",
        alias = "tool_input",
        deserialize_with = "deserialize_optional_tool_args",
        default
    )]
    pub tool_args: Option<Value>,
    pub source: Option<String>,
    #[serde(alias = "stop_reason")]
    pub stop_reason: Option<String>,
    pub trigger: Option<String>,
    pub reason: Option<String>,
    pub recoverable: Option<bool>,
    pub error: Option<CopilotHookError>,
    #[serde(alias = "error_context")]
    pub error_context: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum CopilotHookError {
    Detail {
        message: Option<String>,
        #[allow(dead_code)] // Retained for forward-compatible error classification.
        name: Option<String>,
    },
    Message(String),
}

impl CopilotHookError {
    pub(crate) fn into_message(self) -> Option<String> {
        match self {
            Self::Detail { message, .. } => message,
            Self::Message(message) => Some(message),
        }
    }
}

pub(crate) fn parse_payload(payload: &Value) -> CopilotHookPayload {
    serde_json::from_value(payload.clone()).unwrap_or_default()
}

fn deserialize_optional_tool_args<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.map(|value| match value {
        // Copilot's hook reference types this field as `unknown`, while its
        // policy tutorial documents the native CLI form as a JSON string.
        // Normalize either wire shape before the adapter inspects ask details.
        Value::String(raw) => serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
        value => value,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_native_and_compatible_field_names() {
        let native = parse_payload(&json!({
            "sessionId": "native",
            "toolName": "edit",
            "toolArgs": { "path": "a.rs" },
            "errorContext": "model_call"
        }));
        assert_eq!(native.session_id.as_deref(), Some("native"));
        assert_eq!(native.tool_name.as_deref(), Some("edit"));
        assert_eq!(native.error_context.as_deref(), Some("model_call"));

        let compatible = parse_payload(&json!({
            "session_id": "compatible",
            "tool_name": "bash",
            "tool_input": "{\"command\":\"true\"}",
            "error_context": "tool_execution"
        }));
        assert_eq!(compatible.session_id.as_deref(), Some("compatible"));
        assert_eq!(compatible.tool_name.as_deref(), Some("bash"));
        assert_eq!(compatible.tool_args, Some(json!({ "command": "true" })));
        assert_eq!(compatible.error_context.as_deref(), Some("tool_execution"));
        assert!(parse_payload(&json!(null)).session_id.is_none());
    }

    #[test]
    fn a_failure_error_string_does_not_discard_the_rest_of_the_payload() {
        let payload = parse_payload(&json!({
            "sessionId": "session",
            "toolName": "bash",
            "error": "command failed"
        }));
        assert_eq!(payload.session_id.as_deref(), Some("session"));
        assert_eq!(payload.tool_name.as_deref(), Some("bash"));
        assert_eq!(
            payload
                .error
                .and_then(CopilotHookError::into_message)
                .as_deref(),
            Some("command failed")
        );
    }
}
