//! Tolerant projection of Antigravity CLI's custom-statusline JSON.

use jiff::Timestamp;
use serde::Deserialize;

use crate::agents::context::{
    AgentAccount, AgentContext, AgentCost, AgentCurrentUsage, AgentTokenUsage,
};
use crate::agents::pricing::PriceBook;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct StatuslinePayload {
    version: Option<String>,
    model: Model,
    context_window: ContextWindow,
    plan_tier: Option<String>,
    email: Option<String>,
    tool_confirmation_pending: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Model {
    id: Option<String>,
    display_name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ContextWindow {
    context_window_size: Option<u64>,
    used_percentage: Option<f64>,
    remaining_percentage: Option<f64>,
    current_usage: CurrentUsage,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CurrentUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReasoningQualifier {
    Low,
    Medium,
    High,
    Thinking,
}

impl ReasoningQualifier {
    fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("low") {
            Some(Self::Low)
        } else if value.eq_ignore_ascii_case("medium") {
            Some(Self::Medium)
        } else if value.eq_ignore_ascii_case("high") {
            Some(Self::High)
        } else if value.eq_ignore_ascii_case("thinking") {
            Some(Self::Thinking)
        } else {
            None
        }
    }

    const fn effort(self) -> Option<&'static str> {
        match self {
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::Thinking => None,
        }
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

fn clamp_pct(value: Option<f64>) -> Option<u8> {
    value
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value.round().clamp(0.0, 100.0) as u8)
}

fn normalize_model_display(
    display_name: Option<String>,
) -> (Option<String>, Option<String>, Option<bool>) {
    let Some(display_name) = non_empty(display_name) else {
        return (None, None, None);
    };
    let Some(without_close) = display_name.strip_suffix(')') else {
        return (Some(display_name), None, None);
    };
    let Some(open) = without_close.rfind('(') else {
        return (Some(display_name), None, None);
    };
    let Some(qualifier) = ReasoningQualifier::parse(&without_close[open + 1..]) else {
        return (Some(display_name), None, None);
    };
    let base = without_close[..open].trim();
    if base.is_empty() {
        return (Some(display_name), None, None);
    }
    (
        Some(base.to_owned()),
        qualifier.effort().map(ToOwned::to_owned),
        (qualifier == ReasoningQualifier::Thinking).then_some(true),
    )
}

impl StatuslinePayload {
    pub(crate) fn estimate_cost(&self, prices: &PriceBook) -> Option<AgentCost> {
        let model_id = self.model.id.as_deref()?.trim();
        (!model_id.is_empty()).then_some(())?;
        let usage = &self.context_window.current_usage;
        (usage.input_tokens.is_some()
            || usage.output_tokens.is_some()
            || usage.cache_creation_input_tokens.is_some()
            || usage.cache_read_input_tokens.is_some())
        .then_some(())?;
        let price = prices.price(model_id).or_else(|| {
            selector_price_keys(model_id)
                .into_iter()
                .flatten()
                .find_map(|key| prices.exact_price(&key))
        })?;
        let total_cost_usd = price.cost(
            usage.input_tokens.unwrap_or(0),
            usage.output_tokens.unwrap_or(0),
            usage.cache_creation_input_tokens.unwrap_or(0),
            0,
            usage.cache_read_input_tokens.unwrap_or(0),
            false,
        );
        (total_cost_usd.is_finite() && total_cost_usd > 0.0).then(|| AgentCost {
            total_cost_usd: Some(total_cost_usd),
            estimated: true,
            ..AgentCost::default()
        })
    }

    pub(crate) fn into_context(self, source: &str, observed_at: Timestamp) -> AgentContext {
        let (model_display_name, effort, thinking_enabled) =
            normalize_model_display(self.model.display_name);
        let native_permission_wait =
            (self.tool_confirmation_pending == Some(true)).then_some(observed_at);
        let has_current_usage = self.context_window.current_usage.input_tokens.is_some()
            || self.context_window.current_usage.output_tokens.is_some()
            || self
                .context_window
                .current_usage
                .cache_creation_input_tokens
                .is_some()
            || self
                .context_window
                .current_usage
                .cache_read_input_tokens
                .is_some();
        let current_usage = AgentCurrentUsage {
            input_tokens: self.context_window.current_usage.input_tokens,
            output_tokens: self.context_window.current_usage.output_tokens,
            cache_creation_input_tokens: self
                .context_window
                .current_usage
                .cache_creation_input_tokens,
            cache_read_input_tokens: self.context_window.current_usage.cache_read_input_tokens,
        };
        let current_usage = has_current_usage.then_some(current_usage);
        let tokens = (self.context_window.context_window_size.is_some()
            || self.context_window.used_percentage.is_some()
            || self.context_window.remaining_percentage.is_some()
            || current_usage.is_some())
        .then(|| AgentTokenUsage {
            context_window_size: self.context_window.context_window_size,
            used_percentage: clamp_pct(self.context_window.used_percentage),
            remaining_percentage: clamp_pct(self.context_window.remaining_percentage),
            current_usage,
            session_usage: None,
        });
        let plan = non_empty(self.plan_tier);
        let account_id = non_empty(self.email);
        let account = (plan.is_some() || account_id.is_some()).then(|| AgentAccount {
            plan,
            account_id,
            ..AgentAccount::default()
        });
        AgentContext {
            source: source.to_owned(),
            session_name: None,
            session_preview: None,
            model_id: self.model.id.filter(|id| !id.trim().is_empty()),
            model_display_name,
            effort,
            thinking_enabled,
            output_style: None,
            vim_mode: None,
            agent_version: non_empty(self.version),
            exceeds_200k_tokens: None,
            cost: None,
            tokens,
            rate_limits: None,
            pr: None,
            account,
            turn_opened_by: Vec::new(),
            turn_error: None,
            turn_complete: None,
            plan_proposed: None,
            native_permission_wait,
            turn_interrupted: None,
            observed_at,
        }
    }
}

/// Antigravity CLI 1.1.2 publishes the selected human label in `model.id`
/// (`Gemini 3.5 Flash (Medium)`) while hooks carry a canonical-shaped hint.
/// Keep that provider identity untouched in `AgentContext`, but let a captured
/// terminal reasoning qualifier expose conservative exact-table candidates for
/// this point-in-time estimate. Exact lookup keeps an unknown selector from
/// borrowing rates from a related model through the global fuzzy resolver.
fn selector_price_keys(model_id: &str) -> Option<[String; 2]> {
    let (base, effort, thinking_enabled) = normalize_model_display(Some(model_id.to_owned()));
    (effort.is_some() || thinking_enabled == Some(true)).then_some(())?;
    let dotted = base?
        .split_whitespace()
        .map(|segment| segment.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("-");
    (!dotted.is_empty()).then_some(())?;
    let hyphenated = dotted.replace('.', "-");
    Some([dotted, hyphenated])
}
