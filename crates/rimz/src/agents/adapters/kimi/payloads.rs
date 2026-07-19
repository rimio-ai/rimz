//! Tolerant typed Kimi command-hook payload.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct KimiHookPayload {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub agent_name: Option<String>,
    #[serde(deserialize_with = "deserialize_prompt")]
    pub prompt: Option<String>,
    pub response: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub error_type: Option<String>,
    pub error_message: Option<String>,
    pub trigger: Option<String>,
    pub action: Option<String>,
}

impl KimiHookPayload {
    pub fn question_background(&self) -> bool {
        self.tool_input
            .as_ref()
            .and_then(|input| input.get("background"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

pub fn parse(payload: &Value) -> KimiHookPayload {
    serde_json::from_value(payload.clone()).unwrap_or_default()
}

fn deserialize_prompt<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.as_ref().and_then(flatten_prompt))
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn prompt_accepts_string_or_text_parts_and_ignores_unknown_fields() {
        assert_eq!(
            parse(&json!({"prompt": "hello", "future": true}))
                .prompt
                .as_deref(),
            Some("hello")
        );
        assert_eq!(
            parse(&json!({"prompt": [
                {"type": "text", "text": "one"},
                {"type": "image", "text": "skip"},
                {"type": "text", "text": "two"}
            ]}))
            .prompt
            .as_deref(),
            Some("one\ntwo")
        );
        assert!(parse(&json!(null)).prompt.is_none());
    }

    #[test]
    fn question_background_comes_from_tool_input() {
        assert!(parse(&json!({"tool_input": {"background": true}})).question_background());
        assert!(!parse(&json!({"tool_input": {}})).question_background());
    }
}
