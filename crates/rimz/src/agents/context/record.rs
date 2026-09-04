use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::{
    AgentCost, AgentSpec, AgentTokenUsage, LocalContextRefresh, LocalSpendFold,
    LocallyPricedTurnCost, TranscriptStat,
};
use crate::ids::{AgentKind, AgentSessionId};

use super::AgentContext;

#[cfg(any(test, feature = "testkit"))]
mod fixture;

/// A session's context sidecar: the normalized record plus the
/// `(kind, agent_id)` it is filed under, so a read can confirm the key — and
/// shrug off a digest collision — instead of trusting the filename.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentContextRecord {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub context: AgentContext,
    /// When app-server/account-scoped context was last observed. Local transcript
    /// pushes bump `context.observed_at`, so app-server throttles use this stamp
    /// instead of the whole-record freshness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limits_observed_at: Option<Timestamp>,
    /// When a rich-context transport last wrote display-only metadata that is
    /// not rate-limit/account data. Local token/cost pushes bump
    /// `context.observed_at`, so rich-context throttles use this stamp instead
    /// of whole-record freshness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rich_observed_at: Option<Timestamp>,
    /// Transcript, rollout, or telemetry file used for the latest local context refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// Stat gate for [`Self::transcript_path`], letting high-frequency hooks skip
    /// an unchanged tail without parsing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_stat: Option<TranscriptStat>,
    /// Resumable per-request pricing state for the local transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend_fold: Option<LocalSpendFold>,
    /// Hook-priced live-session state. Private so only the idempotent merge can
    /// advance the accumulator.
    #[serde(default, skip_serializing_if = "LocallyPricedCostState::is_empty")]
    locally_priced_cost: LocallyPricedCostState,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct LocallyPricedCostState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    cumulative_usd: f64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    owns_context_cost: bool,
}

impl LocallyPricedCostState {
    fn is_empty(&self) -> bool {
        self.last_turn_id.is_none() && self.cumulative_usd == 0.0 && !self.owns_context_cost
    }
}

fn is_zero_f64(value: &f64) -> bool {
    *value == 0.0
}

impl AgentContextRecord {
    pub fn new(kind: &str, agent_id: &str, context: AgentContext) -> Self {
        Self {
            kind: AgentKind::new_unchecked(kind),
            agent_id: agent_id.into(),
            context,
            rate_limits_observed_at: None,
            rich_observed_at: None,
            transcript_path: None,
            transcript_stat: None,
            spend_fold: None,
            locally_priced_cost: LocallyPricedCostState::default(),
        }
    }

    pub(crate) fn apply_context_refresh(
        &mut self,
        kind: &str,
        agent_id: &str,
        context: AgentContext,
    ) -> bool {
        let mut context = context;
        let observed_cost = context.cost.is_some();
        context.turn_opened_by = self.context.turn_opened_by.clone();
        if context.cost.is_none() {
            context.cost.clone_from(&self.context.cost);
        }
        let prior_session_usage = self
            .context
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.session_usage.clone());
        if let Some(prior_session_usage) = prior_session_usage {
            let tokens = context.tokens.get_or_insert_with(AgentTokenUsage::default);
            super::merge_session_usage(&mut tokens.session_usage, Some(prior_session_usage));
        }
        let mut locally_priced_cost = self.locally_priced_cost.clone();
        if observed_cost {
            locally_priced_cost.owns_context_cost = false;
        }
        let mut next = Self::new(kind, agent_id, context);
        next.transcript_path.clone_from(&self.transcript_path);
        next.spend_fold.clone_from(&self.spend_fold);
        next.locally_priced_cost = locally_priced_cost;
        *self = next;
        true
    }

    pub fn apply_local_refresh(
        &mut self,
        definition: &AgentSpec,
        refresh: LocalContextRefresh,
        observed_at: Timestamp,
    ) -> bool {
        self.context.source = definition.kind.to_owned();
        let cost_replaced = !refresh.context.cost.is_keep();
        refresh.context.apply(&mut self.context, definition);
        if cost_replaced {
            self.locally_priced_cost.owns_context_cost = false;
        }
        self.context.observed_at = observed_at;
        refresh.spend_fold.apply(&mut self.spend_fold);
        self.transcript_path = refresh.transcript_path;
        self.transcript_stat = refresh.transcript_stat;
        true
    }

    pub fn merge_observed(
        &mut self,
        kind: &str,
        context: AgentContext,
        observed_at: Timestamp,
    ) -> bool {
        let mut changed = false;
        macro_rules! merge_optional {
            ($field:ident) => {
                if let Some(value) = context.$field
                    && self.context.$field.as_ref() != Some(&value)
                {
                    self.context.$field = Some(value);
                    changed = true;
                }
            };
        }
        merge_optional!(session_name);
        merge_optional!(session_preview);
        merge_optional!(model_display_name);
        merge_optional!(thinking_enabled);
        merge_optional!(output_style);
        merge_optional!(vim_mode);
        merge_optional!(agent_version);
        merge_optional!(exceeds_200k_tokens);
        merge_optional!(pr);
        merge_optional!(account);
        merge_optional!(turn_error);
        merge_optional!(settle);
        merge_optional!(model_id);
        merge_optional!(effort);
        if let Some(rate_limits) = context.rate_limits
            && self.context.rate_limits.as_ref() != Some(&rate_limits)
        {
            self.context.rate_limits = Some(rate_limits);
            self.rate_limits_observed_at = Some(observed_at);
            changed = true;
        }
        if let Some(tokens) = context.tokens {
            changed |= merge_observed_tokens(&mut self.context.tokens, tokens);
        }
        if let Some(cost) = context.cost
            && let Some(total_cost_usd) = cost.total_cost_usd
        {
            let prior_total_cost = self
                .context
                .cost
                .as_ref()
                .and_then(|cost| cost.total_cost_usd);
            if prior_total_cost.is_none_or(|prior| total_cost_usd >= prior) {
                changed |= merge_observed_cost(&mut self.context.cost, cost, total_cost_usd);
                self.locally_priced_cost.owns_context_cost = false;
            }
        }
        if changed {
            self.context.source = kind.to_owned();
            self.context.observed_at = observed_at;
        }
        changed
    }

    pub fn apply_locally_priced_turn(
        &mut self,
        kind: &str,
        priced: &LocallyPricedTurnCost,
        observed_at: Timestamp,
    ) -> bool {
        if self.locally_priced_cost.last_turn_id.as_deref() == Some(priced.turn_id.as_str()) {
            return false;
        }
        let cumulative = self.locally_priced_cost.cumulative_usd + priced.cost_usd;
        if !cumulative.is_finite() || cumulative < 0.0 {
            return false;
        }
        self.locally_priced_cost.last_turn_id = Some(priced.turn_id.clone());
        self.locally_priced_cost.cumulative_usd = cumulative;
        self.context.source = kind.to_owned();
        self.context.observed_at = observed_at;
        if self.locally_priced_cost.owns_context_cost || self.context.cost.is_none() {
            self.locally_priced_cost.owns_context_cost = true;
            let cost = self.context.cost.get_or_insert_with(AgentCost::default);
            cost.total_cost_usd = Some(cumulative);
        }
        true
    }
}

fn merge_observed_tokens(prior: &mut Option<AgentTokenUsage>, incoming: AgentTokenUsage) -> bool {
    let target = prior.get_or_insert_with(AgentTokenUsage::default);
    let before = target.clone();
    if incoming.context_window_size.is_some() {
        target.context_window_size = incoming.context_window_size;
    }
    if incoming.used_percentage.is_some() {
        target.used_percentage = incoming.used_percentage;
    }
    if incoming.remaining_percentage.is_some() {
        target.remaining_percentage = incoming.remaining_percentage;
    }
    if incoming.current_context_tokens.is_some() {
        target.current_context_tokens = incoming.current_context_tokens;
    }
    if let Some(current_usage) = incoming.current_usage {
        target.current_usage = Some(current_usage);
    }
    super::merge_session_usage(&mut target.session_usage, incoming.session_usage);
    *target != before
}

fn merge_observed_cost(
    prior: &mut Option<AgentCost>,
    incoming: AgentCost,
    total_cost_usd: f64,
) -> bool {
    let target = prior.get_or_insert_with(AgentCost::default);
    let before = target.clone();
    target.total_cost_usd = Some(total_cost_usd);
    target.coverage = incoming.coverage;
    if incoming.total_duration_ms.is_some() {
        target.total_duration_ms = incoming.total_duration_ms;
    }
    if incoming.total_api_duration_ms.is_some() {
        target.total_api_duration_ms = incoming.total_api_duration_ms;
    }
    if incoming.total_lines_added.is_some() {
        target.total_lines_added = incoming.total_lines_added;
    }
    if incoming.total_lines_removed.is_some() {
        target.total_lines_removed = incoming.total_lines_removed;
    }
    *target != before
}
