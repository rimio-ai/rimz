//! Tolerant projection of Cursor Agent's command-statusline JSON.

use jiff::Timestamp;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::agents::context::{AgentContext, AgentCurrentUsage, AgentTokenUsage, clamp_pct};
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
        let (model_display_name, effort) = normalize_model_metadata(
            model.display_name,
            model.param_summary,
            model.max_mode == Some(true),
        );
        let tokens = self.context_window.and_then(ContextWindow::into_usage);
        AgentContext {
            session_name: self.session_name,
            model_id,
            model_display_name,
            effort,
            output_style: self.output_style,
            vim_mode: self.vim,
            agent_version: self.version,
            tokens,
            ..AgentContext::new(source, observed_at)
        }
    }
}

fn normalize_model_metadata(
    display_name: Option<String>,
    param_summary: Option<String>,
    max_mode: bool,
) -> (Option<String>, Option<String>) {
    let display_name = display_name.filter(|display| !display.trim().is_empty());
    let param_summary = param_summary.filter(|summary| !summary.trim().is_empty());
    let Some(summary) = param_summary.as_deref() else {
        let effort = max_mode.then(|| "max".to_owned());
        return (display_name, effort);
    };

    let summary_tokens = summary.split_whitespace().collect::<Vec<_>>();
    let mut cursor = usize::from(
        summary_tokens
            .first()
            .is_some_and(|token| is_context_magnitude(token)),
    );
    let (effort, effort_tokens) = parse_effort(&summary_tokens[cursor..]);
    cursor += effort_tokens;
    let qualifiers = &summary_tokens[cursor..];

    let normalized_display = display_name.as_deref().and_then(|display| {
        let display_tokens = display.split_whitespace().collect::<Vec<_>>();
        if display_tokens.len() <= summary_tokens.len()
            || !display_tokens.ends_with(&summary_tokens)
        {
            return None;
        }

        let base_tokens = display_tokens.len() - summary_tokens.len();
        let mut normalized = display_tokens[..base_tokens].join(" ");
        if !qualifiers.is_empty() {
            normalized.push(' ');
            normalized.push_str(&qualifiers.join(" "));
        }
        Some(normalized)
    });

    (
        normalized_display.or(display_name),
        effort
            .map(str::to_owned)
            .or_else(|| max_mode.then(|| "max".to_owned())),
    )
}

fn is_context_magnitude(token: &str) -> bool {
    let Some((unit, digits)) = token.as_bytes().split_last() else {
        return false;
    };
    !digits.is_empty()
        && digits.iter().all(u8::is_ascii_digit)
        && matches!(unit.to_ascii_lowercase(), b'k' | b'm')
}

fn parse_effort(tokens: &[&str]) -> (Option<&'static str>, usize) {
    let Some(first) = tokens.first() else {
        return (None, 0);
    };
    if first.eq_ignore_ascii_case("extra")
        && tokens
            .get(1)
            .is_some_and(|second| second.eq_ignore_ascii_case("high"))
    {
        return (Some("xhigh"), 2);
    }

    let effort = ["none", "low", "medium", "high", "xhigh", "max"]
        .into_iter()
        .find(|effort| first.eq_ignore_ascii_case(effort));
    (effort, usize::from(effort.is_some()))
}

impl ContextWindow {
    fn into_usage(self) -> Option<AgentTokenUsage> {
        let current_usage = self.current_usage.and_then(CurrentUsage::into_usage);
        let usage = AgentTokenUsage {
            context_window_size: self.context_window_size,
            used_percentage: clamp_pct(self.used_percentage),
            remaining_percentage: clamp_pct(self.remaining_percentage),
            current_usage,
            session_usage: None,
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
                "display_name": "GPT-5.6 Sol 272K Medium",
                "param_summary": "272K Medium",
                "max_mode": false
            },
            "version": "2026.07.09-a3815c0",
            "output_style": {"name": "default"},
            "vim": {"mode": "NORMAL"},
            "context_window": {
                "context_window_size": 200000,
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
        assert_eq!(context.model_display_name.as_deref(), Some("GPT-5.6 Sol"));
        assert_eq!(context.effort.as_deref(), Some("medium"));
        let tokens = context.tokens.unwrap();
        assert_eq!(tokens.context_window_size, Some(200_000));
        assert_eq!(tokens.used_percentage, Some(9));
        assert_eq!(tokens.remaining_percentage, Some(91));
        assert_eq!(tokens.current_usage.unwrap().input_tokens, Some(14_021));
    }

    #[test]
    fn model_metadata_normalizes_effort_and_preserves_ambiguous_identity() {
        let cases = [
            (
                Some("Model 272K Low"),
                Some("272K Low"),
                false,
                Some("Model"),
                Some("low"),
            ),
            (
                Some("Model 272K Medium"),
                Some("272K Medium"),
                true,
                Some("Model"),
                Some("medium"),
            ),
            (
                Some("Model 1M HIGH"),
                Some("1M HIGH"),
                false,
                Some("Model"),
                Some("high"),
            ),
            (
                Some("Model 272K Extra High"),
                Some("272K Extra High"),
                false,
                Some("Model"),
                Some("xhigh"),
            ),
            (
                Some("Model 272K xhigh"),
                Some("272K xhigh"),
                false,
                Some("Model"),
                Some("xhigh"),
            ),
            (
                Some("Model 272K None"),
                Some("272K None"),
                true,
                Some("Model"),
                Some("none"),
            ),
            (
                Some("Model Max"),
                Some("Max"),
                false,
                Some("Model"),
                Some("max"),
            ),
            (
                Some("Model 272K Medium Fast"),
                Some("272K Medium Fast"),
                false,
                Some("Model Fast"),
                Some("medium"),
            ),
            (
                Some("Model 272K High Thinking"),
                Some("272K High Thinking"),
                false,
                Some("Model Thinking"),
                Some("high"),
            ),
            (
                Some("Model 272K Turbo"),
                Some("272K Turbo"),
                false,
                Some("Model Turbo"),
                None,
            ),
            (
                Some("Model Fast Medium"),
                Some("Fast Medium"),
                true,
                Some("Model Fast Medium"),
                Some("max"),
            ),
            (
                Some("Model 272K Medium  "),
                Some("272K Medium"),
                false,
                Some("Model"),
                Some("medium"),
            ),
            (
                Some("Model 272K Medium"),
                Some("272k Medium"),
                false,
                Some("Model 272K Medium"),
                Some("medium"),
            ),
            (
                Some("272K Medium"),
                Some("272K Medium"),
                false,
                Some("272K Medium"),
                Some("medium"),
            ),
            (Some("  "), Some("272K Medium"), false, None, Some("medium")),
            (Some("Model"), Some("  "), true, Some("Model"), Some("max")),
            (None, Some("272K Medium"), false, None, Some("medium")),
        ];

        for (display, summary, max_mode, expected_display, expected_effort) in cases {
            let (display, effort) = normalize_model_metadata(
                display.map(str::to_owned),
                summary.map(str::to_owned),
                max_mode,
            );
            assert_eq!(display.as_deref(), expected_display, "display");
            assert_eq!(effort.as_deref(), expected_effort, "effort");
        }
    }

    #[test]
    fn malformed_siblings_degrade_field_locally_without_synthesizing_usage() {
        let context = context(json!({
            "model": {
                "id": "default",
                "display_name": 9,
                "param_summary": [],
                "max_mode": "yes"
            },
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
