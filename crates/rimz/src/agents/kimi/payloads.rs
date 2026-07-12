//! Typed Kimi command-hook payloads.

#![allow(dead_code)]

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Common {
    pub hook_event_name: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SessionStart {
    #[serde(flatten)]
    pub common: Common,
    pub source: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct UserPromptSubmit {
    #[serde(flatten)]
    pub common: Common,
    pub prompt: Option<Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ToolHook {
    #[serde(flatten)]
    pub common: Common,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub tool_call_id: Option<String>,
    pub tool_output: Option<Value>,
    pub error: Option<Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct StopFailure {
    #[serde(flatten)]
    pub common: Common,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Compact {
    #[serde(flatten)]
    pub common: Common,
    pub trigger: Option<String>,
    pub token_count: Option<u64>,
    pub estimated_token_count: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Subagent {
    #[serde(flatten)]
    pub common: Common,
    pub agent_name: Option<String>,
    pub prompt: Option<String>,
    pub response: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Permission {
    #[serde(flatten)]
    pub common: Common,
    pub turn_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub action: Option<String>,
    pub tool_input: Option<Value>,
    pub decision: Option<String>,
    pub scope: Option<Value>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Interrupt {
    #[serde(flatten)]
    pub common: Common,
    pub turn_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct HookPayload {
    pub common: Common,
    pub prompt: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub tool_call_id: Option<String>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    pub trigger: Option<String>,
    pub action: Option<String>,
    pub decision: Option<String>,
    pub reason: Option<String>,
    pub question_background: bool,
}

pub fn parse(event: &str, payload: &Value) -> HookPayload {
    match event {
        "SessionStart" => {
            let value: SessionStart = serde_json::from_value(payload.clone()).unwrap_or_default();
            HookPayload {
                common: value.common,
                ..HookPayload::default()
            }
        }
        "UserPromptSubmit" => {
            let value: UserPromptSubmit =
                serde_json::from_value(payload.clone()).unwrap_or_default();
            HookPayload {
                common: value.common,
                prompt: value.prompt.as_ref().and_then(flatten_prompt),
                ..HookPayload::default()
            }
        }
        "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => {
            let value: ToolHook = serde_json::from_value(payload.clone()).unwrap_or_default();
            let question_background = value
                .tool_input
                .as_ref()
                .and_then(|input| input.get("background"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            HookPayload {
                common: value.common,
                tool_name: value.tool_name,
                tool_input: value.tool_input,
                tool_call_id: value.tool_call_id,
                question_background,
                ..HookPayload::default()
            }
        }
        "StopFailure" => {
            let value: StopFailure = serde_json::from_value(payload.clone()).unwrap_or_default();
            HookPayload {
                common: value.common,
                error_type: value.error_type,
                error_message: value.error_message,
                ..HookPayload::default()
            }
        }
        "PreCompact" | "PostCompact" => {
            let value: Compact = serde_json::from_value(payload.clone()).unwrap_or_default();
            HookPayload {
                common: value.common,
                trigger: value.trigger,
                ..HookPayload::default()
            }
        }
        "SubagentStart" | "SubagentStop" => {
            let value: Subagent = serde_json::from_value(payload.clone()).unwrap_or_default();
            HookPayload {
                common: value.common,
                prompt: value.prompt,
                ..HookPayload::default()
            }
        }
        "PermissionRequest" | "PermissionResult" => {
            let value: Permission = serde_json::from_value(payload.clone()).unwrap_or_default();
            HookPayload {
                common: value.common,
                tool_name: value.tool_name,
                tool_input: value.tool_input,
                tool_call_id: value.tool_call_id,
                action: value.action,
                decision: value.decision,
                ..HookPayload::default()
            }
        }
        "Interrupt" => {
            let value: Interrupt = serde_json::from_value(payload.clone()).unwrap_or_default();
            HookPayload {
                common: value.common,
                reason: value.reason,
                ..HookPayload::default()
            }
        }
        _ => HookPayload {
            common: serde_json::from_value(payload.clone()).unwrap_or_default(),
            ..HookPayload::default()
        },
    }
}

fn flatten_prompt(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    let text = value
        .as_array()?
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}
