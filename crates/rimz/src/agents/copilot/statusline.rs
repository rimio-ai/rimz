//! Tolerant projection of Copilot CLI's command-statusline JSON.

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::agents::context::{
    AgentContext, AgentCost, AgentCurrentUsage, AgentSessionUsage, AgentTokenUsage, clamp_pct,
};
use crate::agents::transcript_fs::{
    deserialize_optional_f64_lossy, deserialize_optional_object_lossy,
    deserialize_optional_string_lossy, deserialize_optional_u64_lossy,
};

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct StatuslinePayload {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    session_name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    version: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    model: Option<Model>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    context_window: Option<ContextWindow>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    cost: Option<Cost>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Model {
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_string_lossy")]
    display_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ContextWindow {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    context_window_size: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    displayed_context_limit: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    current_context_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_f64_lossy")]
    current_context_used_percentage: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_f64_lossy")]
    used_percentage: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_f64_lossy")]
    remaining_percentage: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_object_lossy")]
    current_usage: Option<CurrentUsage>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total_input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total_output_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total_cache_write_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total_cache_read_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total_reasoning_tokens: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CurrentUsage {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    output_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Cost {
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total_duration_ms: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total_api_duration_ms: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total_lines_added: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional_u64_lossy")]
    total_lines_removed: Option<u64>,
}

impl StatuslinePayload {
    pub(super) fn parse(payload: &Value) -> Option<Self> {
        payload
            .is_object()
            .then(|| serde_json::from_value(payload.clone()).ok())
            .flatten()
    }

    pub(super) fn into_context(self, source: &str, observed_at: Timestamp) -> Option<AgentContext> {
        let model = self.model.unwrap_or_default();
        let (model_display_name, effort) = split_effort(model.display_name);
        let tokens = self.context_window.and_then(ContextWindow::into_usage);
        let cost = self.cost.and_then(Cost::into_cost);
        let context = AgentContext {
            session_name: self.session_name,
            model_id: model.id,
            model_display_name,
            effort,
            agent_version: self.version,
            tokens,
            cost,
            ..AgentContext::new(source, observed_at)
        };
        (context.session_name.is_some()
            || context.model_id.is_some()
            || context.model_display_name.is_some()
            || context.effort.is_some()
            || context.agent_version.is_some()
            || context.tokens.is_some()
            || context.cost.is_some())
        .then_some(context)
    }
}

impl ContextWindow {
    fn into_usage(self) -> Option<AgentTokenUsage> {
        let context_window_size = self.displayed_context_limit.or(self.context_window_size);
        let used_percentage = clamp_pct(
            self.current_context_used_percentage
                .or(self.used_percentage)
                .or_else(|| {
                    let denominator = context_window_size.filter(|limit| *limit > 0)?;
                    let used = self.current_context_tokens?;
                    Some(used as f64 * 100.0 / denominator as f64)
                }),
        );
        let current_usage = self.current_usage.and_then(CurrentUsage::into_usage);
        let session_usage = AgentSessionUsage {
            input_tokens: self.total_input_tokens,
            output_tokens: self.total_output_tokens,
            cache_creation_input_tokens: self.total_cache_write_tokens,
            cache_read_input_tokens: self.total_cache_read_tokens,
            thinking_tokens: self.total_reasoning_tokens,
        };
        let session_usage = (!session_usage.is_zero()
            || self.total_input_tokens.is_some()
            || self.total_output_tokens.is_some()
            || self.total_cache_write_tokens.is_some()
            || self.total_cache_read_tokens.is_some()
            || self.total_reasoning_tokens.is_some())
        .then_some(session_usage);
        let usage = AgentTokenUsage {
            context_window_size,
            used_percentage,
            remaining_percentage: clamp_pct(self.remaining_percentage),
            current_context_tokens: None,
            current_usage,
            session_usage,
        };
        (usage.context_window_size.is_some()
            || usage.used_percentage.is_some()
            || usage.remaining_percentage.is_some()
            || usage.current_usage.is_some()
            || usage.session_usage.is_some())
        .then_some(usage)
    }
}

impl CurrentUsage {
    fn into_usage(self) -> Option<AgentCurrentUsage> {
        let usage = AgentCurrentUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_input_tokens: self.cache_creation_input_tokens,
            cache_read_input_tokens: self.cache_read_input_tokens,
        };
        [
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
        ]
        .into_iter()
        .any(|count| count.is_some())
        .then_some(usage)
    }
}

impl Cost {
    fn into_cost(self) -> Option<AgentCost> {
        let cost = AgentCost {
            total_duration_ms: self.total_duration_ms,
            total_api_duration_ms: self.total_api_duration_ms,
            total_lines_added: self.total_lines_added,
            total_lines_removed: self.total_lines_removed,
            ..AgentCost::default()
        };
        (cost.total_duration_ms.is_some()
            || cost.total_api_duration_ms.is_some()
            || cost.total_lines_added.is_some()
            || cost.total_lines_removed.is_some())
        .then_some(cost)
    }
}

fn split_effort(display_name: Option<String>) -> (Option<String>, Option<String>) {
    let Some(display_name) = display_name else {
        return (None, None);
    };
    let Some(prefix) = display_name.strip_suffix(')') else {
        return (Some(display_name), None);
    };
    let Some((label, qualifier)) = prefix.rsplit_once(" (") else {
        return (Some(display_name), None);
    };
    let effort = match qualifier.to_ascii_lowercase().as_str() {
        "none" => "none",
        "minimal" => "minimal",
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        "xhigh" => "xhigh",
        "max" => "max",
        _ => return (Some(display_name), None),
    };
    (
        (!label.is_empty()).then(|| label.to_owned()),
        Some(effort.to_owned()),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn context(value: Value) -> Option<AgentContext> {
        StatuslinePayload::parse(&value)?
            .into_context("copilot", Timestamp::from_second(1_700_000_000).unwrap())
    }

    #[test]
    fn modern_fixture_separates_live_window_and_session_totals() {
        let payload: Value =
            serde_json::from_str(include_str!("tests/fixtures/statusline-modern.json")).unwrap();

        let context = context(payload).unwrap();

        assert_eq!(context.session_name.as_deref(), Some("Fixing auth retry"));
        assert_eq!(context.model_id.as_deref(), Some("auto"));
        assert_eq!(
            context.model_display_name.as_deref(),
            Some("Auto → claude-sonnet-4.6 (1x)")
        );
        assert_eq!(context.effort.as_deref(), Some("medium"));
        assert_eq!(context.agent_version.as_deref(), Some("1.0.71"));
        let tokens = context.tokens.unwrap();
        assert_eq!(tokens.context_window_size, Some(128_000));
        assert_eq!(tokens.used_percentage, Some(38));
        assert_eq!(tokens.remaining_percentage, Some(65));
        assert_eq!(tokens.used_tokens(), Some(48_000));
        assert_eq!(tokens.current_usage.unwrap().output_tokens, Some(900));
        assert_eq!(
            tokens.session_usage,
            Some(AgentSessionUsage {
                input_tokens: Some(82_000),
                output_tokens: Some(6_100),
                cache_creation_input_tokens: Some(7_000),
                cache_read_input_tokens: Some(69_000),
                thinking_tokens: Some(1_200),
            })
        );
        assert_eq!(context.cost.unwrap().total_cost_usd, None);
    }

    #[test]
    fn derives_only_from_selected_live_occupancy_and_clamps_percentages() {
        let derived = context(json!({
            "context_window": {
                "displayed_context_limit": "200000",
                "context_window_size": 1,
                "current_context_tokens": "51000",
                "total_tokens": 199999,
                "last_call_input_tokens": 99999
            }
        }))
        .unwrap()
        .tokens
        .unwrap();
        assert_eq!(derived.context_window_size, Some(200_000));
        assert_eq!(derived.used_percentage, Some(26));
        assert_eq!(derived.current_usage, None);

        let clamped = context(json!({
            "context_window": {
                "current_context_used_percentage": 500,
                "used_percentage": 20,
                "remaining_percentage": -5
            }
        }))
        .unwrap()
        .tokens
        .unwrap();
        assert_eq!(clamped.used_percentage, Some(100));
        assert_eq!(clamped.remaining_percentage, Some(0));
    }

    #[test]
    fn malformed_fields_are_local_and_ambiguous_fields_stay_ignored() {
        let context = context(json!({
            "session_name": [],
            "version": " 1.0.71 ",
            "model": {"id": false, "display_name": "GPT selector (2x) (turbo)"},
            "context_window": {
                "displayed_context_limit": {},
                "context_window_size": "64000",
                "current_context_used_percentage": "bad",
                "used_percentage": "12.4",
                "current_usage": {
                    "input_tokens": "500",
                    "output_tokens": -1,
                    "cache_read_input_tokens": null
                },
                "total_tokens": 999999,
                "last_call_input_tokens": 123456
            },
            "cost": {"total_duration_ms": "800", "total_premium_requests": 99},
            "ai_used": {"formatted": "8.20"},
            "remote": {"connected": true}
        }))
        .unwrap();
        assert_eq!(context.session_name, None);
        assert_eq!(context.agent_version.as_deref(), Some("1.0.71"));
        assert_eq!(
            context.model_display_name.as_deref(),
            Some("GPT selector (2x) (turbo)")
        );
        assert_eq!(context.effort, None);
        let tokens = context.tokens.unwrap();
        assert_eq!(tokens.context_window_size, Some(64_000));
        assert_eq!(tokens.used_percentage, Some(12));
        assert_eq!(tokens.used_tokens(), Some(500));
        assert_eq!(tokens.session_usage, None);
        let cost = context.cost.unwrap();
        assert_eq!(cost.total_duration_ms, Some(800));
        assert_eq!(cost.total_cost_usd, None);
    }

    #[test]
    fn sparse_or_non_object_payload_without_enrichment_is_ignored() {
        for payload in [
            Value::Null,
            json!([]),
            json!({}),
            json!({"session_id":"session-only"}),
            json!({"model": null, "context_window": "bad", "cost": {}}),
        ] {
            assert!(context(payload).is_none());
        }
    }

    #[test]
    fn recognizes_only_documented_terminal_effort_suffixes() {
        for (raw, display, effort) in [
            ("GPT (none)", Some("GPT"), Some("none")),
            ("GPT (minimal)", Some("GPT"), Some("minimal")),
            (
                "GPT selector (3x) (xhigh)",
                Some("GPT selector (3x)"),
                Some("xhigh"),
            ),
            (
                "GPT selector (3x) (MAX)",
                Some("GPT selector (3x)"),
                Some("max"),
            ),
            (
                "GPT selector (3x) (extreme)",
                Some("GPT selector (3x) (extreme)"),
                None,
            ),
        ] {
            let context = context(json!({"model":{"display_name":raw}})).unwrap();
            assert_eq!(context.model_display_name.as_deref(), display, "{raw}");
            assert_eq!(context.effort.as_deref(), effort, "{raw}");
        }
    }
}
