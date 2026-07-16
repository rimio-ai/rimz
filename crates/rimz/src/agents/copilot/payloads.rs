//! Typed GitHub Copilot CLI command-hook payloads.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::agents::transcript_fs::deserialize_optional_string_lossy;

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
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    #[serde(alias = "tool_name")]
    pub tool_name: Option<String>,
    #[serde(
        alias = "tool_args",
        alias = "tool_input",
        deserialize_with = "deserialize_optional_tool_args",
        default
    )]
    pub tool_args: Option<Value>,
    #[serde(default, alias = "tool_calls")]
    pub tool_calls: Vec<CopilotToolCall>,
    pub source: Option<String>,
    pub recoverable: Option<bool>,
    pub error: Option<CopilotHookError>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct CopilotToolCall {
    #[serde(
        alias = "toolName",
        alias = "tool_name",
        deserialize_with = "deserialize_optional_string_lossy"
    )]
    name: Option<String>,
    #[serde(
        alias = "toolArgs",
        alias = "tool_args",
        alias = "toolInput",
        alias = "tool_input",
        deserialize_with = "deserialize_optional_tool_args"
    )]
    args: Option<Value>,
}

#[derive(Clone, Copy)]
pub(crate) struct NormalizedToolCall<'a> {
    pub name: Option<&'a str>,
    pub args: Option<&'a Value>,
}

pub(crate) struct NormalizedToolCalls<'a> {
    calls: Vec<NormalizedToolCall<'a>>,
}

impl CopilotHookPayload {
    /// Present both Copilot hook wire shapes through one adapter-local view.
    /// A batched ask wins selection so lifecycle and detail extraction agree
    /// even when another call appears first.
    pub(crate) fn normalized_tool_calls(&self) -> NormalizedToolCalls<'_> {
        let legacy =
            (self.tool_name.is_some() || self.tool_args.is_some()).then_some(NormalizedToolCall {
                name: self.tool_name.as_deref(),
                args: self.tool_args.as_ref(),
            });
        let calls = legacy
            .into_iter()
            .chain(self.tool_calls.iter().map(|call| NormalizedToolCall {
                name: call.name.as_deref(),
                args: call.args.as_ref(),
            }))
            .collect();
        NormalizedToolCalls { calls }
    }
}

impl<'a> NormalizedToolCalls<'a> {
    pub(crate) fn selected(&self) -> Option<NormalizedToolCall<'a>> {
        self.calls
            .iter()
            .copied()
            .find(|call| call.name == Some("ask_user"))
            .or_else(|| self.calls.first().copied())
    }

    pub(crate) fn any_named(&self, names: &[&str]) -> bool {
        self.calls
            .iter()
            .any(|call| call.name.is_some_and(|name| names.contains(&name)))
    }
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

    #[test]
    fn normalizes_singular_batched_and_mixed_tool_calls() {
        let singular = parse_payload(&json!({
            "toolName": "bash",
            "toolArgs": "{\"command\":\"true\"}"
        }));
        let selected = singular.normalized_tool_calls().selected().unwrap();
        assert_eq!(selected.name, Some("bash"));
        assert_eq!(selected.args, Some(&json!({"command":"true"})));

        let batched = parse_payload(&json!({
            "toolCalls": [
                {"name":"edit","args":{"path":"a.rs"}},
                {"toolName":"ask_user","toolArgs":"{\"question\":\"Ship?\"}"}
            ]
        }));
        let calls = batched.normalized_tool_calls();
        let selected = calls.selected().unwrap();
        assert_eq!(selected.name, Some("ask_user"));
        assert_eq!(selected.args, Some(&json!({"question":"Ship?"})));
        assert!(calls.any_named(&["edit"]));

        let mixed = parse_payload(&json!({
            "toolName":"view",
            "toolCalls":[{"name":"create","args":"{\"path\":\"b.rs\"}"}]
        }));
        let calls = mixed.normalized_tool_calls();
        assert_eq!(calls.selected().unwrap().name, Some("view"));
        assert!(calls.any_named(&["create", "edit"]));
    }
}
