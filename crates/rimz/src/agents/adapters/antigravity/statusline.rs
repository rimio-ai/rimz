//! Tolerant projection of Antigravity CLI's custom-statusline JSON.

use jiff::Timestamp;
use serde::Deserialize;

use serde_json::Value;

use crate::agents::context::{
    AgentAccount, AgentContext, AgentCurrentUsage, AgentTokenUsage, TurnSettle, TurnSettleOutcome,
    clamp_pct,
};
use crate::agents::hook_types::ProviderRest;
use crate::agents::payload::non_empty_trimmed;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct StatuslinePayload {
    #[serde(alias = "conversationId")]
    pub(super) conversation_id: Option<String>,
    version: Option<String>,
    model: Model,
    context_window: ContextWindow,
    /// Quota buckets by id, each kept raw until its id selects it. Kept raw
    /// whole, so a `quota` that is not a map loses only the windows.
    quota: Option<Value>,
    plan_tier: Option<String>,
    email: Option<String>,
    tool_confirmation_pending: Option<bool>,
    agent_state: Option<String>,
    task_count: Option<u64>,
    /// Native subagent list, kept raw whole so an unexpected shape never drops
    /// the rest of the payload and counts as live when gating a parked turn end.
    subagents: Option<Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Model {
    id: Option<String>,
    display_name: Option<String>,
    /// Native selector effort, first captured from 1.2.2; older releases only
    /// carry it as the display label's parenthesized qualifier.
    effort: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ContextWindow {
    context_window_size: Option<u64>,
    used_percentage: Option<f64>,
    remaining_percentage: Option<f64>,
    /// agy 1.5.12 sends `null` before the first model turn.
    current_usage: Option<CurrentUsage>,
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
    XHigh,
    Max,
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
        } else if value.eq_ignore_ascii_case("xhigh") || value.eq_ignore_ascii_case("extra high") {
            Some(Self::XHigh)
        } else if value.eq_ignore_ascii_case("max") {
            Some(Self::Max)
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
            Self::XHigh => Some("xhigh"),
            Self::Max => Some("max"),
            Self::Thinking => None,
        }
    }
}

/// A selector label split from its recognized trailing qualifier (`Gemini 4 Neon (High)`).
/// A model variant (`Orchestrator (Neon)`) is no qualifier.
fn split_qualifier(label: &str) -> Option<(&str, ReasoningQualifier)> {
    let without_close = label.trim().strip_suffix(')')?;
    let open = without_close.rfind('(')?;
    let qualifier = ReasoningQualifier::parse(&without_close[open + 1..])?;
    let base = without_close[..open].trim();
    (!base.is_empty()).then_some((base, qualifier))
}

fn normalize_model_display(
    display_name: Option<String>,
) -> (Option<String>, Option<String>, Option<bool>) {
    let Some(display_name) = display_name.as_deref().and_then(non_empty_trimmed) else {
        return (None, None, None);
    };
    let Some((base, qualifier)) = split_qualifier(&display_name) else {
        return (Some(display_name), None, None);
    };
    (
        Some(base.to_owned()),
        qualifier.effort().map(ToOwned::to_owned),
        (qualifier == ReasoningQualifier::Thinking).then_some(true),
    )
}

impl StatuslinePayload {
    /// agy reads `idle` with no background task and no pending tool confirmation.
    /// `task_count` is absent after an Escape with nothing in the background.
    pub(super) fn at_rest(&self) -> bool {
        self.agent_state
            .as_deref()
            .is_some_and(|state| state.trim().eq_ignore_ascii_case("idle"))
            && self.task_count.unwrap_or(0) == 0
            && self.tool_confirmation_pending != Some(true)
    }

    /// Whether the payload lists any native subagent whose status is anything
    /// other than `completed` (an unknown status, missing status, or non-array
    /// value counts as live).
    fn has_live_subagent(&self) -> bool {
        match &self.subagents {
            None | Some(Value::Null) => false,
            Some(Value::Array(entries)) => entries.iter().any(|entry| {
                !entry
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|status| status.trim().eq_ignore_ascii_case("completed"))
            }),
            Some(_) => true,
        }
    }

    /// What this statusline payload establishes about its conversation resting:
    /// `None` unless [`Self::at_rest`] holds; `Complete` when no native subagent
    /// is live either, and `Foreground` while any native subagent may still run.
    pub(super) fn provider_rest(&self) -> Option<ProviderRest> {
        if !self.at_rest() {
            return None;
        }
        Some(if self.has_live_subagent() {
            ProviderRest::Foreground
        } else {
            ProviderRest::Complete
        })
    }

    pub(super) fn into_context(self, source: &str, observed_at: Timestamp) -> AgentContext {
        let (model_display_name, mut label_effort, mut thinking_enabled) =
            normalize_model_display(self.model.display_name);
        let model_id = self.model.id.filter(|id| !id.trim().is_empty());
        // agy sends the selector label as `model.id`; its qualifier is the effort, and stands in
        // for the display name's when that carries none.
        let id_split = model_id.as_deref().and_then(split_qualifier);
        if label_effort.is_none()
            && thinking_enabled.is_none()
            && let Some((_, qualifier)) = id_split
        {
            label_effort = qualifier.effort().map(ToOwned::to_owned);
            thinking_enabled = (qualifier == ReasoningQualifier::Thinking).then_some(true);
        }
        let base_id = id_split.map(|(base, _)| base.to_owned());
        let effort = self
            .model
            .effort
            .as_deref()
            .and_then(non_empty_trimmed)
            .map(|effort| effort.to_ascii_lowercase())
            .or(label_effort);
        let settle = (self.tool_confirmation_pending == Some(true))
            .then(|| TurnSettle::new(observed_at, TurnSettleOutcome::NativeWait));
        let usage = self.context_window.current_usage.unwrap_or_default();
        let has_current_usage = usage.input_tokens.is_some()
            || usage.output_tokens.is_some()
            || usage.cache_creation_input_tokens.is_some()
            || usage.cache_read_input_tokens.is_some();
        let current_usage = has_current_usage.then_some(AgentCurrentUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
        });
        let tokens = (self.context_window.context_window_size.is_some()
            || self.context_window.used_percentage.is_some()
            || self.context_window.remaining_percentage.is_some()
            || current_usage.is_some())
        .then(|| AgentTokenUsage {
            context_window_size: self.context_window.context_window_size,
            used_percentage: clamp_pct(self.context_window.used_percentage),
            remaining_percentage: clamp_pct(self.context_window.remaining_percentage),
            current_context_tokens: None,
            current_usage,
            session_usage: None,
        });
        let plan = self.plan_tier.as_deref().and_then(non_empty_trimmed);
        let account_id = self.email.as_deref().and_then(non_empty_trimmed);
        let account = (plan.is_some() || account_id.is_some()).then(|| AgentAccount {
            plan,
            account_id,
            metered: Some(true),
            ..AgentAccount::default()
        });
        AgentContext {
            model_id: base_id.or(model_id),
            model_display_name,
            effort,
            thinking_enabled,
            agent_version: self.version.as_deref().and_then(non_empty_trimmed),
            tokens,
            account,
            rate_limits: self
                .quota
                .and_then(|quota| super::usage::statusline_windows(quota, observed_at)),
            settle,
            ..AgentContext::new(source, observed_at)
        }
    }
}
