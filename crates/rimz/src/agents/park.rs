//! Shared projection of provider parks that can no longer resume.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;

use super::{AgentState, ProviderCapacity, TurnErrorClass};
use crate::ids::{AgentKind, AgentSessionId, LoginKey};

/// Durable retry and provider-window evidence demoting a paused card to failed.
#[derive(Default)]
pub struct ParkDemotion {
    exhausted_resumes: BTreeSet<(AgentKind, AgentSessionId)>,
    windows: RateLimitKindSummary,
}

impl ParkDemotion {
    /// Summarize exhausted retries and spent/reset windows at one projection time.
    pub fn new(
        provider_capacities: &BTreeMap<LoginKey, ProviderCapacity>,
        exhausted_resumes: BTreeSet<(AgentKind, AgentSessionId)>,
        now: Timestamp,
    ) -> Self {
        Self {
            exhausted_resumes,
            windows: rate_limit_window_kinds(provider_capacities, now),
        }
    }

    /// Whether retries are exhausted or a limit reset without a spent window left.
    pub fn is_spent(&self, agent: &AgentState, class: TurnErrorClass) -> bool {
        let resume_exhausted = self
            .exhausted_resumes
            .contains(&(agent.kind.clone(), agent.agent_id.clone()));
        let login = agent.login_key();
        let reset_without_budget = class.is_limit()
            && self.windows.reset.contains(&login)
            && !self.windows.spent.contains(&login);
        resume_exhausted || reset_without_budget
    }

    pub(crate) fn window_spent(&self, login: &LoginKey) -> bool {
        self.windows.spent.contains(login)
    }
}

#[derive(Default)]
struct RateLimitKindSummary {
    spent: BTreeSet<LoginKey>,
    reset: BTreeSet<LoginKey>,
}

fn rate_limit_window_kinds(
    provider_capacities: &BTreeMap<LoginKey, ProviderCapacity>,
    now: Timestamp,
) -> RateLimitKindSummary {
    let mut summary = RateLimitKindSummary::default();
    for (kind, capacity) in provider_capacities {
        let mut has_spent = false;
        let mut has_reset = false;
        for window in capacity.projected_windows(now) {
            if window.scope.is_some() && window.duration_mins.is_some() {
                continue;
            }
            if !window.is_spent() {
                continue;
            }
            if window.resets_at.is_none_or(|reset| reset > now) {
                has_spent = true;
            } else {
                has_reset = true;
            }
        }
        if has_spent {
            summary.spent.insert(kind.clone());
        }
        if has_reset {
            summary.reset.insert(kind.clone());
        }
    }
    summary
}
