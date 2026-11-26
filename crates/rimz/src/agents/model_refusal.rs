//! Models a provider refuses on one account with no reset coming.
//!
//! A refusal record is keyed by login and model and lives in the machine-wide
//! `model_refusals.v1.json` provider cache beside the rate-limit windows. The
//! room host's heavy pass writes it (`sidebar::refresh`) once a usage read
//! confirms a limit marker ([`super::ProviderCapacity::confirms_refusal`]: the
//! account unspent in the window the marker ran in, for a marker naming no
//! refill), raises it on any limit marker while it stands, and raises its
//! success stamp on the first clean turn on that model and account after it.
//! [`super::ProviderCapacity`] loads a login's records, so every capacity
//! reader (pings, compaction, launch routing, restart, auto-continue) vetoes
//! the model while its record stands.
//!
//! Every stamp only rises, so writers that each read, raise, and write under
//! the file's lock end equal in either order. A missing or unreadable file
//! reads as no records, and entries are never deleted.

use std::collections::BTreeMap;
use std::path::Path;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::{AgentState, AgentStatus, AgentTurnError, ProviderCapacity};
use crate::ids::LoginKey;

/// How long after a limit marker a usage read may first see the account and still confirm the
/// refusal; a read past it may postdate a reset that explains the marker. A read inside it whose
/// window renewed after the marker postdates such a reset too, and confirms nothing
/// ([`ProviderCapacity::confirms_refusal`]).
pub(crate) const CONFIRM_WINDOW: jiff::SignedDuration = jiff::SignedDuration::from_mins(10);

/// One model's refusal on one login.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRefusal {
    /// The newest limit marker the refusal was recorded or raised from.
    pub refused_at: Timestamp,
    /// The start of the newest clean turn on this model and login after a refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub succeeded_at: Option<Timestamp>,
    /// The provider's own sentence, from the marker that last raised `refused_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl ModelRefusal {
    /// Whether no clean turn has started since the newest refusal.
    pub fn stands(&self) -> bool {
        self.succeeded_at
            .is_none_or(|succeeded| self.refused_at >= succeeded)
    }
}

/// Every recorded refusal, by login and then by [`model_key`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelRefusals {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) logins: BTreeMap<LoginKey, BTreeMap<String, ModelRefusal>>,
}

impl ModelRefusals {
    pub(crate) fn read(path: &Path) -> Self {
        crate::disk::atomic::read_json_cache(path)
    }

    pub(crate) fn login(&self, login: &LoginKey) -> BTreeMap<String, ModelRefusal> {
        self.logins.get(login).cloned().unwrap_or_default()
    }

    /// Raise the records `agents` give evidence for at `now`, and return whether any rose:
    /// a refusal for each agent whose newest input is a limit marker on a model its login's
    /// record already refuses, or whose `capacities` reading confirms it
    /// ([`ProviderCapacity::confirms_refusal`]); then a success for each row, ended or live,
    /// whose latest request opened after its model's refusal and closed clean. Only live rows
    /// raise a refusal. Provider-native subagent rows give neither: their status and turn stamps
    /// derive from the parent's transcript, whose own row is the evidence.
    pub(crate) fn observe(
        &mut self,
        agents: &[AgentState],
        capacities: &BTreeMap<LoginKey, ProviderCapacity>,
        now: Timestamp,
    ) -> bool {
        let agents = agents.iter().filter(|agent| !agent.is_provider_subagent());
        let mut raised = false;
        for agent in agents.clone().filter(|agent| agent.ended_at.is_none()) {
            let Some(marker) = agent.newest_limit_marker(now) else {
                continue;
            };
            let Some(model) = marker.model.as_deref().or(agent.current_model()) else {
                continue;
            };
            let login = agent.login_key();
            let parsed_key = parsed_model_key(model);
            let key = model_key(model);
            let stands = self.logins.get(&login).is_some_and(|models| {
                models.iter().any(|(recorded, refusal)| {
                    refusal.stands()
                        && parsed_key.as_ref().is_some_and(|key| {
                            super::model_display::ParsedModel::parse(recorded)
                                .match_key()
                                .is_some_and(|recorded| recorded == *key)
                        })
                })
            });
            let confirmed = stands
                || capacities
                    .get(&login)
                    .is_some_and(|capacity| capacity.confirms_refusal(model, marker));
            if confirmed {
                raised |= self.raise_refused(&login, &key, marker.at, marker.label.as_deref());
            }
        }
        for agent in agents {
            let (Some(opened), Some(model)) =
                (clean_request_opened_at(agent), agent.model.as_deref())
            else {
                continue;
            };
            raised |= self.raise_succeeded(&agent.login_key(), &model_key(model), opened);
        }
        raised
    }

    /// Raise `login`'s refusal of `model_key` to `at` with the provider's
    /// `label`. Returns whether anything changed: an `at` no later than the
    /// stored one writes nothing. Matches an existing record sharing the same
    /// parsed matching key so two spellings (`claude 4.8 opus`, `opus 4.8`)
    /// stay one record.
    pub(crate) fn raise_refused(
        &mut self,
        login: &LoginKey,
        model_key: &str,
        at: Timestamp,
        label: Option<&str>,
    ) -> bool {
        let models = self.logins.entry(login.clone()).or_default();
        let parsed = parsed_model_key(model_key);
        let existing_key = if models.contains_key(model_key) {
            Some(model_key.to_owned())
        } else {
            parsed.as_ref().and_then(|key| {
                models.keys().find_map(|recorded| {
                    super::model_display::ParsedModel::parse(recorded)
                        .match_key()
                        .filter(|recorded_key| recorded_key == key)
                        .map(|_| recorded.clone())
                })
            })
        };
        if let Some(existing_key) = existing_key
            && let Some(refusal) = models.get_mut(&existing_key)
        {
            if refusal.refused_at >= at {
                return false;
            }
            refusal.refused_at = at;
            refusal.label = label.map(str::to_owned);
            return true;
        }
        models.insert(
            model_key.to_owned(),
            ModelRefusal {
                refused_at: at,
                succeeded_at: None,
                label: label.map(str::to_owned),
            },
        );
        true
    }

    /// Raise the success stamp of `login`'s refusals of `model_key`, and of
    /// its family's version-less key (a record a bare alias raised), to
    /// `started`, a clean turn's start after the refusal. Returns whether
    /// anything changed.
    pub(crate) fn raise_succeeded(
        &mut self,
        login: &LoginKey,
        model_key: &str,
        started: Timestamp,
    ) -> bool {
        let Some(models) = self.logins.get_mut(login) else {
            return false;
        };
        let parsed = parsed_model_key(model_key);
        let mut raised = false;
        for (_, refusal) in models.iter_mut().filter(|(recorded, _)| {
            *recorded == model_key
                || parsed.as_ref().is_some_and(|key| {
                    super::model_display::ParsedModel::parse(recorded)
                        .match_key()
                        .is_some_and(|recorded| key.clears_refusal(&recorded))
                })
        }) {
            if started <= refusal.refused_at || refusal.succeeded_at.is_some_and(|at| at >= started)
            {
                continue;
            }
            refusal.succeeded_at = Some(started);
            raised = true;
        }
        raised
    }
}

/// When `agent`'s latest request opened, if that request closed clean: the row reads raw
/// Success, or raw Running parked on background work (what a clean Stop with background work
/// pending folds to), with its turn end at or after the open. The open is the later of
/// `turn_started_at` and `last_prompt_at`, since a prompt into a row already parked opens no turn
/// and moves `last_prompt_at` alone, while a provider with no prompt hook opens its turn on a tool
/// and stamps `turn_started_at` alone.
fn clean_request_opened_at(agent: &AgentState) -> Option<Timestamp> {
    let clean = match agent.status {
        AgentStatus::Success => true,
        AgentStatus::Running => agent.phase == super::TurnPhase::Parked,
        _ => false,
    };
    let opened = agent.turn_started_at.max(agent.last_prompt_at)?;
    (clean && agent.turn_ended_at? >= opened).then_some(opened)
}

/// The parsed `(name, version)` matching key for a model or launch alias.
pub(super) fn parsed_model_key(model: &str) -> Option<super::model_display::ModelMatchKey> {
    super::model_display::ParsedModel::parse(super::account::launch_model_id(model.trim()))
        .match_key()
}

/// The key a refusal is recorded under: a launch alias resolved to its model
/// id, then the lowercased `series version family` (or family/verbatim name
/// when unseriesed) from [`super::model_display::ParsedModel::refusal_key`],
/// which re-parses to the same matching key.
pub fn model_key(model: &str) -> String {
    super::model_display::ParsedModel::parse(super::account::launch_model_id(model.trim()))
        .refusal_key()
}

impl AgentState {
    /// The limit marker that is this agent's newest input at `now`: the displayed turn error
    /// when it is a limit, or, on a row at rest (raw idle, success or sleeping), a limit marker
    /// at or after the row's last ping open and after its last activity, as a refused keep-warm
    /// ping leaves it.
    pub(crate) fn newest_limit_marker(&self, now: Timestamp) -> Option<&AgentTurnError> {
        if let Some((class, marker)) = self.displayed_turn_error(now) {
            return class.is_limit().then_some(marker);
        }
        if !matches!(
            self.status,
            AgentStatus::Idle | AgentStatus::Success | AgentStatus::Sleeping
        ) {
            return None;
        }
        let marker = self.context.as_ref()?.turn_error.as_ref()?;
        let after_ping = self
            .ping_opened_at
            .is_some_and(|opened| marker.at >= opened);
        (super::state::effective_turn_error_class(marker).is_limit()
            && after_ping
            && marker.at > self.last_activity)
            .then_some(marker)
    }
}
