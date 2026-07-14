//! Tolerant projection of Antigravity CLI's custom-statusline JSON.

use jiff::Timestamp;
use serde::Deserialize;

use crate::agents::context::{AgentAccount, AgentContext, AgentCurrentUsage, AgentTokenUsage};

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

impl StatuslinePayload {
    pub(crate) fn into_context(self, source: &str, observed_at: Timestamp) -> AgentContext {
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
            model_id: non_empty(self.model.id),
            model_display_name: non_empty(self.model.display_name),
            effort: None,
            thinking_enabled: None,
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
