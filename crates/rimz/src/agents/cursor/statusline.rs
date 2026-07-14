//! Tolerant projection of Cursor Agent's command-statusline JSON.

use jiff::Timestamp;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::agents::context::{AgentContext, AgentCurrentUsage, AgentTokenUsage};
use crate::agents::transcript_fs::{
    deserialize_optional_f64_lossy, deserialize_optional_string_lossy,
    deserialize_optional_u64_lossy,
};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct StatuslinePayload {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    pub session_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    session_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_model_lossy")]
    model: Option<Model>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    version: Option<String>,
    #[serde(default, deserialize_with = "deserialize_named_field_lossy")]
    output_style: Option<String>,
    #[serde(default, deserialize_with = "deserialize_named_field_lossy")]
    vim: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_context_lossy")]
    context_window: Option<ContextWindow>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Model {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    display_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    param_summary: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_bool_lossy")]
    max_mode: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ContextWindow {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    context_window_size: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_f64_lossy")]
    used_percentage: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_f64_lossy")]
    remaining_percentage: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_usage_lossy")]
    current_usage: Option<CurrentUsage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CurrentUsage {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    output_tokens: Option<u64>,
    #[serde(
        default,
        alias = "cache_write_tokens",
        deserialize_with = "deserialize_optional_u64_lossy"
    )]
    cache_creation_input_tokens: Option<u64>,
    #[serde(
        default,
        alias = "cache_read_tokens",
        deserialize_with = "deserialize_optional_u64_lossy"
    )]
    cache_read_input_tokens: Option<u64>,
}

impl StatuslinePayload {
    pub(super) fn into_context(self, source: &str, observed_at: Timestamp) -> AgentContext {
        let model = self.model.unwrap_or_default();
        let model_id = model.id.map(normalize_model);
        let effort = model
            .param_summary
            .filter(|summary| !summary.trim().is_empty())
            .or_else(|| (model.max_mode == Some(true)).then(|| "Max".to_owned()));
        let tokens = self.context_window.and_then(ContextWindow::into_usage);
        AgentContext {
            source: source.to_owned(),
            session_name: self.session_name,
            session_preview: None,
            model_id,
            model_display_name: model.display_name,
            effort,
            thinking_enabled: None,
            output_style: self.output_style,
            vim_mode: self.vim,
            agent_version: self.version,
            exceeds_200k_tokens: None,
            cost: None,
            tokens,
            rate_limits: None,
            pr: None,
            account: None,
            native_permission_wait: None,
            turn_opened_by: Vec::new(),
            turn_error: None,
            turn_complete: None,
            plan_proposed: None,
            turn_interrupted: None,
            observed_at,
        }
    }
}

impl ContextWindow {
    fn into_usage(self) -> Option<AgentTokenUsage> {
        let current_usage = self.current_usage.and_then(CurrentUsage::into_usage);
        let usage = AgentTokenUsage {
            context_window_size: self.context_window_size,
            used_percentage: clamp_pct(self.used_percentage),
            remaining_percentage: clamp_pct(self.remaining_percentage),
            current_usage,
        };
        (usage.context_window_size.is_some()
            || usage.used_percentage.is_some()
            || usage.remaining_percentage.is_some()
            || usage.current_usage.is_some())
        .then_some(usage)
    }
}

impl CurrentUsage {
    fn into_usage(self) -> Option<AgentCurrentUsage> {
        let any = self.input_tokens.is_some()
            || self.output_tokens.is_some()
            || self.cache_creation_input_tokens.is_some()
            || self.cache_read_input_tokens.is_some();
        any.then_some(AgentCurrentUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
        })
    }
}

pub(super) fn normalize_model(model: String) -> String {
    if model.trim().eq_ignore_ascii_case("default") {
        "auto".to_owned()
    } else {
        model
    }
}

fn clamp_pct(value: Option<f64>) -> Option<u8> {
    value
        .filter(|value| value.is_finite())
        .map(|value| value.round().clamp(0.0, 100.0) as u8)
}

fn deserialize_optional_model_lossy<'de, D>(deserializer: D) -> Result<Option<Model>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_optional_object_lossy(deserializer)
}

fn deserialize_optional_context_lossy<'de, D>(
    deserializer: D,
) -> Result<Option<ContextWindow>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_optional_object_lossy(deserializer)
}

fn deserialize_optional_usage_lossy<'de, D>(
    deserializer: D,
) -> Result<Option<CurrentUsage>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_optional_object_lossy(deserializer)
}

fn deserialize_optional_object_lossy<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

fn deserialize_optional_bool_lossy<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(value.as_bool())
}

fn deserialize_named_field_lossy<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(match value {
        Value::String(value) => Some(value),
        Value::Object(object) => object
            .get("name")
            .or_else(|| object.get("mode"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn context(value: Value) -> AgentContext {
        serde_json::from_value::<StatuslinePayload>(value)
            .unwrap()
            .into_context("cursor", Timestamp::from_second(1_700_000_000).unwrap())
    }

    #[test]
    fn full_payload_projects_cursor_context() {
        let context = context(json!({
            "session_id": "sess-1",
            "session_name": "card work",
            "model": {
                "id": "default",
                "display_name": "Auto",
                "param_summary": "High",
                "max_mode": false
            },
            "version": "2026.07.09-a3815c0",
            "output_style": {"name": "default"},
            "vim": {"mode": "NORMAL"},
            "context_window": {
                "context_window_size": 256000,
                "used_percentage": 8.9,
                "remaining_percentage": 91.1,
                "current_usage": {
                    "input_tokens": 14021,
                    "output_tokens": 26,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 8704
                }
            }
        }));

        assert_eq!(context.model_id.as_deref(), Some("auto"));
        assert_eq!(context.model_display_name.as_deref(), Some("Auto"));
        assert_eq!(context.effort.as_deref(), Some("High"));
        let tokens = context.tokens.unwrap();
        assert_eq!(tokens.context_window_size, Some(256_000));
        assert_eq!(tokens.used_percentage, Some(9));
        assert_eq!(tokens.remaining_percentage, Some(91));
        assert_eq!(tokens.current_usage.unwrap().input_tokens, Some(14_021));
    }

    #[test]
    fn malformed_siblings_degrade_field_locally_without_synthesizing_usage() {
        let context = context(json!({
            "model": {"id": "default", "display_name": 9, "max_mode": "yes"},
            "version": false,
            "output_style": [],
            "context_window": {
                "context_window_size": "256000",
                "used_percentage": "NaN",
                "remaining_percentage": 75,
                "current_usage": "missing"
            },
            "future": true
        }));

        assert_eq!(context.model_id.as_deref(), Some("auto"));
        assert!(context.model_display_name.is_none());
        let tokens = context.tokens.unwrap();
        assert_eq!(tokens.context_window_size, Some(256_000));
        assert_eq!(tokens.remaining_percentage, Some(75));
        assert!(tokens.used_percentage.is_none());
        assert!(tokens.current_usage.is_none());
    }
}
