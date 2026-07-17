//! Tolerant projection of Copilot CLI's command-statusline JSON.

use jiff::Timestamp;
use serde::Deserialize;
use serde_json::Value;

use crate::agents::context::{
    AgentContext, AgentCost, AgentCurrentUsage, AgentSessionUsage, AgentTokenUsage, CostCoverage,
    clamp_pct,
};
use crate::agents::pricing::PriceBook;
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
        let model = resolve_model(&model);
        let tokens = self.context_window.and_then(ContextWindow::into_usage);
        let cost = self.cost.and_then(Cost::into_cost);
        let context = AgentContext {
            session_name: self.session_name,
            model_id: model.id,
            model_display_name: model.display_name,
            effort: model.effort,
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

    pub(super) fn cost(&self, prices: &PriceBook) -> Option<AgentCost> {
        let model = resolve_model(self.model.as_ref()?);
        let model = model.id.as_deref()?;
        let usage = self.context_window.as_ref()?.session_usage()?;
        let price = prices.price(model)?;
        let total_cost_usd = price.cost(
            usage.input_tokens.unwrap_or(0),
            usage
                .output_tokens
                .unwrap_or(0)
                .saturating_add(usage.thinking_tokens.unwrap_or(0)),
            usage.cache_creation_input_tokens.unwrap_or(0),
            0,
            usage.cache_read_input_tokens.unwrap_or(0),
            false,
        );
        (total_cost_usd.is_finite() && total_cost_usd > 0.0).then(|| AgentCost {
            total_cost_usd: Some(total_cost_usd),
            coverage: CostCoverage::Session,
            ..AgentCost::default()
        })
    }
}

impl ContextWindow {
    fn session_usage(&self) -> Option<AgentSessionUsage> {
        let cached_input = self
            .total_cache_write_tokens
            .unwrap_or(0)
            .saturating_add(self.total_cache_read_tokens.unwrap_or(0));
        let usage = AgentSessionUsage {
            input_tokens: self
                .total_input_tokens
                .map(|total| total.saturating_sub(cached_input)),
            output_tokens: self
                .total_output_tokens
                .map(|output| output.saturating_sub(self.total_reasoning_tokens.unwrap_or(0))),
            cache_creation_input_tokens: self.total_cache_write_tokens,
            cache_read_input_tokens: self.total_cache_read_tokens,
            thinking_tokens: self.total_reasoning_tokens,
        };
        (self.total_input_tokens.is_some()
            || self.total_output_tokens.is_some()
            || self.total_cache_write_tokens.is_some()
            || self.total_cache_read_tokens.is_some()
            || self.total_reasoning_tokens.is_some())
        .then_some(usage)
    }

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
        let current_context_tokens = self.current_context_tokens;
        let session_usage = self.session_usage();
        let current_usage = self.current_usage.and_then(CurrentUsage::into_usage);
        let usage = AgentTokenUsage {
            context_window_size,
            used_percentage,
            remaining_percentage: clamp_pct(self.remaining_percentage),
            current_context_tokens,
            current_usage,
            session_usage,
        };
        (usage.context_window_size.is_some()
            || usage.used_percentage.is_some()
            || usage.remaining_percentage.is_some()
            || usage.current_context_tokens.is_some()
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

struct ResolvedModel {
    id: Option<String>,
    display_name: Option<String>,
    effort: Option<String>,
}

fn resolve_model(model: &Model) -> ResolvedModel {
    if model
        .id
        .as_deref()
        .is_some_and(|id| id.eq_ignore_ascii_case("auto"))
        && let Some((target, effort)) = model.display_name.as_deref().and_then(auto_target)
    {
        return ResolvedModel {
            id: Some(target.clone()),
            display_name: Some(target),
            effort,
        };
    }

    let (display_name, effort) = if model
        .id
        .as_deref()
        .is_some_and(|id| id.eq_ignore_ascii_case("auto"))
    {
        // An unresolved selector is still useful provider data. Keep it byte
        // for byte instead of publishing part of a malformed target.
        (model.display_name.clone(), None)
    } else {
        split_effort(model.display_name.clone())
    };
    ResolvedModel {
        id: model.id.clone(),
        display_name,
        effort,
    }
}

fn auto_target(display_name: &str) -> Option<(String, Option<String>)> {
    let unicode = display_name.rfind('→').map(|index| (index, '→'.len_utf8()));
    let ascii = display_name.rfind("->").map(|index| (index, 2));
    let (index, arrow_len) = match (unicode, ascii) {
        (Some(unicode), Some(ascii)) => unicode.max(ascii),
        (Some(unicode), None) => unicode,
        (None, Some(ascii)) => ascii,
        (None, None) => return None,
    };
    let mut target = display_name[index + arrow_len..].trim();
    let mut effort = None;
    while let Some((label, qualifier)) = terminal_qualifier(target) {
        if is_effort(qualifier) {
            effort.get_or_insert_with(|| qualifier.to_ascii_lowercase());
        } else if !is_multiplier(qualifier) {
            break;
        }
        target = label.trim_end();
    }
    (!target.is_empty()).then(|| (target.to_owned(), effort))
}

fn terminal_qualifier(value: &str) -> Option<(&str, &str)> {
    let prefix = value.strip_suffix(')')?;
    let (label, qualifier) = prefix
        .rsplit_once(" (")
        .or_else(|| prefix.strip_prefix('(').map(|qualifier| ("", qualifier)))?;
    (!qualifier.is_empty()).then_some((label, qualifier))
}

fn is_effort(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
    )
}

fn is_multiplier(value: &str) -> bool {
    value
        .strip_suffix('x')
        .or_else(|| value.strip_suffix('X'))
        .is_some_and(|value| !value.is_empty() && value.parse::<f64>().is_ok())
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
        let prices = PriceBook::embedded();
        let estimated_cost = StatuslinePayload::parse(&payload)
            .unwrap()
            .cost(&prices)
            .unwrap();

        let context = context(payload).unwrap();

        assert_eq!(context.session_name.as_deref(), Some("Fixing auth retry"));
        assert_eq!(context.model_id.as_deref(), Some("claude-sonnet-4.6"));
        assert_eq!(
            context.model_display_name.as_deref(),
            Some("claude-sonnet-4.6")
        );
        assert_eq!(context.effort.as_deref(), Some("medium"));
        assert_eq!(context.agent_version.as_deref(), Some("1.0.71"));
        let tokens = context.tokens.unwrap();
        assert_eq!(tokens.context_window_size, Some(128_000));
        assert_eq!(tokens.used_percentage, Some(38));
        assert_eq!(tokens.remaining_percentage, Some(65));
        assert_eq!(tokens.used_tokens(), Some(48_600));
        assert_eq!(tokens.current_usage.unwrap().output_tokens, Some(900));
        assert_eq!(
            tokens.session_usage,
            Some(AgentSessionUsage {
                input_tokens: Some(6_000),
                output_tokens: Some(77),
                cache_creation_input_tokens: Some(7_000),
                cache_read_input_tokens: Some(69_000),
                thinking_tokens: Some(128),
            })
        );
        assert_eq!(context.cost.unwrap().total_cost_usd, None);
        let expected = prices
            .price("claude-sonnet-4.6")
            .unwrap()
            .cost(6_000, 205, 7_000, 0, 69_000, false);
        assert_eq!(estimated_cost.total_cost_usd, Some(expected));
        assert_eq!(estimated_cost.coverage, CostCoverage::Session);
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
        assert_eq!(derived.current_context_tokens, Some(51_000));
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

    #[test]
    fn cost_prefers_concrete_ids_and_resolves_auto_display_targets() {
        let prices = PriceBook::embedded();
        for model in [
            json!({"id":"claude-haiku-4.5","display_name":"ignored"}),
            json!({"id":"auto","display_name":"Auto → claude-haiku-4.5 (1x) (medium)"}),
        ] {
            let payload = json!({
                "model": model,
                "context_window": {
                    "total_input_tokens": 100,
                    "total_output_tokens": 20,
                    "total_cache_write_tokens": 30,
                    "total_cache_read_tokens": 40,
                    "total_reasoning_tokens": 5
                }
            });
            assert!(
                StatuslinePayload::parse(&payload)
                    .unwrap()
                    .cost(&prices)
                    .and_then(|cost| cost.total_cost_usd)
                    .is_some_and(|cost| cost > 0.0),
                "{model}"
            );
        }
    }

    #[test]
    fn auto_targets_resolve_across_arrow_and_qualifier_forms() {
        for (raw, expected, effort) in [
            ("Auto → gpt-5-mini", "gpt-5-mini", None),
            (
                "Auto -> claude-haiku-4.5 (2x) (HIGH)",
                "claude-haiku-4.5",
                Some("high"),
            ),
            (
                "selector -> stale → gpt-5-mini (medium) (1.5X)",
                "gpt-5-mini",
                Some("medium"),
            ),
        ] {
            let context = context(json!({"model":{"id":"AUTO","display_name":raw}})).unwrap();
            assert_eq!(context.model_id.as_deref(), Some(expected), "{raw}");
            assert_eq!(
                context.model_display_name.as_deref(),
                Some(expected),
                "{raw}"
            );
            assert_eq!(context.effort.as_deref(), effort, "{raw}");
        }
    }

    #[test]
    fn malformed_auto_and_concrete_models_preserve_provider_identity() {
        for raw in ["Auto", "Auto ->", "Auto → (medium)"] {
            let context = context(json!({"model":{"id":"auto","display_name":raw}})).unwrap();
            assert_eq!(context.model_id.as_deref(), Some("auto"), "{raw}");
            assert_eq!(context.model_display_name.as_deref(), Some(raw), "{raw}");
            assert_eq!(context.effort, None, "{raw}");
        }

        let concrete = context(json!({
            "model":{"id":"gpt-5-mini","display_name":"GPT 5 Mini (high)"}
        }))
        .unwrap();
        assert_eq!(concrete.model_id.as_deref(), Some("gpt-5-mini"));
        assert_eq!(concrete.model_display_name.as_deref(), Some("GPT 5 Mini"));
        assert_eq!(concrete.effort.as_deref(), Some("high"));
    }
}
