//! Shared projection of provider parks that can no longer resume.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;

use super::model_refusal::{CONFIRM_WINDOW, parsed_model_key};
use super::{AgentState, AgentStatus, AgentTurnError, ProviderCapacity, TurnErrorClass};
use crate::ids::{AgentKind, AgentSessionId, LoginKey};

/// Durable retry and provider-window evidence demoting a paused card to failed, and the
/// login's model refusals that turn a resting card failed and hold RimZ's typing.
#[derive(Default)]
pub struct ParkDemotion {
    exhausted_resumes: BTreeSet<(AgentKind, AgentSessionId)>,
    windows: RateLimitKindSummary,
    capacities: BTreeMap<LoginKey, ProviderCapacity>,
}

/// Why RimZ types no message into an agent.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "hold", rename_all = "snake_case")]
pub enum ModelHold {
    /// The provider refuses the agent's current model on its account.
    Refused {
        /// The model's display name.
        model: String,
        login: LoginKey,
        /// The provider's own sentence.
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// The provider refused the agent's last request on its current model and account, and no
    /// record stands for it: a success on that model has since cleared the record, or a usage
    /// read has confirmed the refusal before the record lands. The agent itself waits for its
    /// own next turn.
    RefusedLast {
        /// The model's display name.
        model: String,
        login: LoginKey,
        /// The sentence of the marker that raised the refusal.
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// The provider refused the agent's last request and no usage read has seen the account
    /// since; the hold lapses at `until` without one.
    Unconfirmed { until: Timestamp },
}

impl std::fmt::Display for ModelHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused {
                model,
                login,
                label,
            } => {
                write!(f, "the provider refuses {model} on {login}")?;
                if let Some(label) = label {
                    write!(f, ": \"{label}\"")?;
                }
                f.write_str(
                    "; it goes in once the agent's model changes or a turn on that model and \
                     account succeeds",
                )
            }
            Self::RefusedLast {
                model,
                login,
                label,
            } => {
                write!(
                    f,
                    "the provider refused the receiver's last request on {model} on {login}"
                )?;
                if let Some(label) = label {
                    write!(f, ": \"{label}\"")?;
                }
                f.write_str(
                    "; it goes in once the agent's model changes or after the agent's own next \
                     turn",
                )
            }
            Self::Unconfirmed { .. } => f.write_str(
                "the provider refused the receiver's last request; RimZ is waiting for the \
                 account's usage read",
            ),
        }
    }
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
            capacities: provider_capacities.clone(),
        }
    }

    /// The label to show when the provider refuses `agent`, resting at its effective `status`
    /// (idle, success, sleeping, or a provider pause that is no budget park): a record stands
    /// for its login and current model (the record's label), or its newest input is a limit
    /// marker on that model that is confirmed (the marker's): no later than the record's
    /// refusal, standing or cleared, so the refused agent stays failed until its own next turn,
    /// or confirmed by the login's usage read now (`ProviderCapacity::confirms_refusal`, never
    /// for a marker naming its refill or from a window renewed after the marker), the read the
    /// next heavy pass turns into the record, so the agent is refused from that read rather
    /// than from the record's write.
    pub fn refused<'a>(
        &'a self,
        agent: &'a AgentState,
        status: AgentStatus,
        now: Timestamp,
    ) -> Option<Option<&'a str>> {
        let resting = match status {
            AgentStatus::Idle | AgentStatus::Success | AgentStatus::Sleeping => true,
            AgentStatus::Paused => agent.budget_park.is_none(),
            _ => false,
        };
        if !resting || agent.ended_at.is_some() {
            return None;
        }
        let model = agent.current_model()?;
        let capacity = self.capacities.get(&agent.login_key())?;
        let record = capacity.refusal(Some(model));
        if let Some(record) = record.filter(|record| record.stands()) {
            return Some(record.label.as_deref());
        }
        let marker = agent.newest_limit_marker(now)?;
        let confirmed = record.is_some_and(|record| marker.at <= record.refused_at)
            || capacity.confirms_refusal(model, marker);
        (marker_on(marker, model) && confirmed).then_some(marker.label.as_deref())
    }

    /// Whether live `agent`'s displayed limit marker landed on a model the provider refused and
    /// the agent has since moved off it: the card rests as if the marker were gone. An ended row
    /// keeps its reading, since a session that ended mid-turn is raw failed too.
    pub fn left_model(&self, agent: &AgentState, now: Timestamp) -> bool {
        if agent.ended_at.is_some() {
            return false;
        }
        let Some((class, marker)) = agent.displayed_turn_error(now) else {
            return false;
        };
        let (Some(current), Some(refused)) = (agent.current_model(), marker.model.as_deref())
        else {
            return false;
        };
        class.is_limit()
            && !marker_on(marker, current)
            && self
                .capacities
                .get(&agent.login_key())
                .and_then(|capacity| capacity.refusal(Some(refused)))
                .is_some_and(|record| marker.at <= record.refused_at)
    }

    /// Why RimZ may type nothing into `agent` at its effective `status`: it is
    /// [refused](Self::refused) (on a standing record, or on a confirmed marker whose record a
    /// success has since cleared or the next heavy pass has yet to write), or its newest input is
    /// a limit marker on its current model under ten minutes old (`CONFIRM_WINDOW`) that no
    /// usage read has seen the account since. A login with no usage windows can never confirm,
    /// and no read confirms a marker naming its refill, so neither marker holds anything.
    pub fn held(
        &self,
        agent: &AgentState,
        status: AgentStatus,
        now: Timestamp,
    ) -> Option<ModelHold> {
        if let Some(label) = self.refused(agent, status, now) {
            let current = agent.current_model()?;
            let model = super::model_display::display_model_for_kind(
                super::account::launch_model_id(current.trim()),
                Some(agent.kind.as_str()),
            );
            let login = agent.login_key();
            let label = label.map(str::to_owned);
            let stands = self
                .capacities
                .get(&login)
                .and_then(|capacity| capacity.refusal(Some(current)))
                .is_some_and(super::model_refusal::ModelRefusal::stands);
            return Some(if stands {
                ModelHold::Refused {
                    model,
                    login,
                    label,
                }
            } else {
                ModelHold::RefusedLast {
                    model,
                    login,
                    label,
                }
            });
        }
        if agent.ended_at.is_some()
            || !matches!(
                status,
                AgentStatus::Idle
                    | AgentStatus::Success
                    | AgentStatus::Sleeping
                    | AgentStatus::Paused
            )
        {
            return None;
        }
        let marker = agent.newest_limit_marker(now)?;
        let current = agent.current_model()?;
        let until = marker.at.checked_add(CONFIRM_WINDOW).ok()?;
        let read_since = self
            .capacities
            .get(&agent.login_key())?
            .read_since(Some(current), marker.at)?;
        (marker.refill_at.is_none() && marker_on(marker, current) && now < until && !read_since)
            .then_some(ModelHold::Unconfirmed { until })
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

    pub(super) fn window_spent(&self, login: &LoginKey) -> bool {
        self.windows.spent.contains(login)
    }
}

/// Whether `marker` landed on `model`: its stamp, else the agent's current model, shares the
/// key.
fn marker_on(marker: &AgentTurnError, model: &str) -> bool {
    marker
        .model
        .as_deref()
        .is_none_or(|stamp| parsed_model_key(stamp) == parsed_model_key(model))
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
