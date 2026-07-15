//! Typed GitHub Copilot CLI command-hook payloads.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CopilotHookPayload {
    #[serde(alias = "session_id")]
    pub session_id: Option<String>,
    #[serde(alias = "transcript_path")]
    pub transcript_path: Option<String>,
    pub timestamp: Option<Value>,
    #[serde(alias = "initial_prompt")]
    pub initial_prompt: Option<String>,
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
    pub recoverable: Option<bool>,
    pub error: Option<CopilotHookError>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum CopilotHookError {
    Detail { message: Option<String> },
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
            "initialPrompt": "native prompt",
            "transcriptPath": "/tmp/events.jsonl",
            "errorContext": "model_call"
        }));
        assert_eq!(native.session_id.as_deref(), Some("native"));
        assert_eq!(native.tool_name.as_deref(), Some("edit"));
        assert_eq!(native.transcript_path.as_deref(), Some("/tmp/events.jsonl"));

        let compatible = parse_payload(&json!({
            "session_id": "compatible",
            "tool_name": "bash",
            "tool_input": "{\"command\":\"true\"}",
            "transcript_path": "/tmp/compatible/events.jsonl",
            "error_context": "tool_execution"
        }));
        assert_eq!(compatible.session_id.as_deref(), Some("compatible"));
        assert_eq!(compatible.tool_name.as_deref(), Some("bash"));
        assert_eq!(
            compatible.transcript_path.as_deref(),
            Some("/tmp/compatible/events.jsonl")
        );
        assert_eq!(compatible.tool_args, Some(json!({ "command": "true" })));
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
