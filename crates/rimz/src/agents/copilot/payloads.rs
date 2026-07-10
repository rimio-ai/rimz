//! Typed GitHub Copilot CLI command-hook payloads.

use serde::Deserialize;
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
    #[serde(alias = "tool_args")]
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

#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)] // `name` is retained for forward-compatible error classification.
pub(crate) struct CopilotHookError {
    pub message: Option<String>,
    pub name: Option<String>,
}

pub(crate) fn parse_payload(payload: &Value) -> CopilotHookPayload {
    serde_json::from_value(payload.clone()).unwrap_or_default()
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
            "tool_args": { "command": "true" },
            "error_context": "tool_execution"
        }));
        assert_eq!(compatible.session_id.as_deref(), Some("compatible"));
        assert_eq!(compatible.tool_name.as_deref(), Some("bash"));
        assert_eq!(compatible.error_context.as_deref(), Some("tool_execution"));
        assert!(parse_payload(&json!(null)).session_id.is_none());
    }
}
