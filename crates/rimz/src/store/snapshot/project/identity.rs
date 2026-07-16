//! Stable card identity allocation across launches, lifecycle observations,
//! incremental folds, and log rotation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::agents::{AgentLifecycleObservation, AgentState};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::store::event::AgentLaunchPayload;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AgentIdentityState {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) names: BTreeMap<String, (AgentKind, AgentSessionId)>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) next_ordinal: BTreeMap<AgentKind, u32>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(super) consumed_launches: BTreeSet<AgentSessionId>,
}

impl AgentIdentityState {
    pub(crate) fn with_ordinals_reset(mut self) -> Self {
        self.next_ordinal.clear();
        self
    }

    pub(crate) fn without_consumed_launches(mut self) -> Self {
        self.consumed_launches.clear();
        self
    }
}

pub(crate) fn backfill_agent_identities(
    agents: &mut [AgentState],
    state: AgentIdentityState,
) -> AgentIdentityState {
    let mut map: BTreeMap<(AgentKind, AgentSessionId), AgentState> = agents
        .iter()
        .map(|agent| ((agent.kind.clone(), agent.agent_id.clone()), agent.clone()))
        .collect();
    let mut allocator = CardIdentityAllocator::from_map_and_state(&map, state);
    let mut order: Vec<_> = agents
        .iter()
        .enumerate()
        .map(|(index, agent)| (agent.kind.clone(), agent.agent_id.clone(), index))
        .collect();
    order.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));

    for (kind, agent_id, index) in order {
        if has_card_identity(&agents[index]) {
            continue;
        }
        let key = (kind.clone(), agent_id.clone());
        let identity = allocator.assign_existing(&kind, &agent_id, map.get(&key));
        agents[index].name_explicit = identity.name_explicit;
        agents[index].name = Some(identity.name);
        agents[index].kind_ordinal = Some(identity.kind_ordinal);
        map.insert(key, agents[index].clone());
    }
    allocator.state()
}

fn has_card_identity(agent: &AgentState) -> bool {
    agent.name.as_deref().is_some_and(usable_name) && agent.kind_ordinal.is_some()
}

#[derive(Clone, Debug)]
pub(super) struct CardIdentity {
    pub(super) name: String,
    pub(super) name_explicit: bool,
    pub(super) kind_ordinal: u32,
}

#[derive(Default)]
pub(super) struct CardIdentityAllocator {
    names: BTreeMap<String, (AgentKind, AgentSessionId)>,
    ordinals: BTreeMap<(AgentKind, u32), AgentSessionId>,
    next_ordinal: BTreeMap<AgentKind, u32>,
    consumed_launches: BTreeSet<AgentSessionId>,
}

impl CardIdentityAllocator {
    pub(super) fn from_map_and_state(
        map: &BTreeMap<(AgentKind, AgentSessionId), AgentState>,
        state: AgentIdentityState,
    ) -> Self {
        let mut allocator = Self {
            names: state.names,
            next_ordinal: state.next_ordinal,
            consumed_launches: state.consumed_launches,
            ordinals: BTreeMap::new(),
        };
        allocator.names.retain(|_, owner| {
            map.get(owner)
                .is_some_and(|state| state.parent_agent_id.is_none())
        });
        for ((kind, agent_id), state) in map {
            if state.parent_agent_id.is_none()
                && let Some(name) = state.name.as_deref().filter(|name| usable_name(name))
            {
                allocator
                    .names
                    .entry(name.to_owned())
                    .or_insert_with(|| (kind.clone(), agent_id.clone()));
            }
            if let Some(ordinal) = state.kind_ordinal {
                allocator
                    .ordinals
                    .entry((kind.clone(), ordinal))
                    .or_insert_with(|| agent_id.clone());
                allocator
                    .next_ordinal
                    .entry(kind.clone())
                    .and_modify(|next| *next = (*next).max(ordinal.saturating_add(1)))
                    .or_insert(ordinal.saturating_add(1));
            }
        }
        allocator
    }

    pub(super) fn state(&mut self) -> AgentIdentityState {
        for (kind, ordinal) in self.ordinals.keys() {
            self.next_ordinal
                .entry(kind.clone())
                .and_modify(|next| *next = (*next).max(ordinal.saturating_add(1)))
                .or_insert(ordinal.saturating_add(1));
        }
        AgentIdentityState {
            names: self.names.clone(),
            next_ordinal: self.next_ordinal.clone(),
            consumed_launches: BTreeSet::new(),
        }
    }

    pub(super) fn reset_ordinals(&mut self) {
        self.ordinals.clear();
        self.next_ordinal.clear();
    }

    pub(super) fn owner_for_name(&self, name: &str) -> Option<(AgentKind, AgentSessionId)> {
        self.names.get(name).cloned()
    }

    pub(super) fn adoptable_owner_for_name(
        &self,
        map: &BTreeMap<(AgentKind, AgentSessionId), AgentState>,
        kind: &AgentKind,
        name: &str,
        new_key: &(AgentKind, AgentSessionId),
    ) -> Option<(AgentKind, AgentSessionId)> {
        let owner = self.names.get(name)?;
        if owner.0 != *kind || owner == new_key {
            return None;
        }
        if owner.1.is_provisional() {
            return Some(owner.clone());
        }
        let prior = map.get(owner)?;
        (prior.kind_ordinal.is_none() || prior.pane.is_none()).then(|| owner.clone())
    }

    pub(super) fn adoptable_owner_for_pane(
        &self,
        map: &BTreeMap<(AgentKind, AgentSessionId), AgentState>,
        kind: &AgentKind,
        pane_id: &PaneId,
        new_key: &(AgentKind, AgentSessionId),
    ) -> Option<(AgentKind, AgentSessionId)> {
        map.iter().find_map(|(owner, state)| {
            if owner == new_key || owner.0 != *kind || !owner.1.is_provisional() {
                return None;
            }
            let pane = state.pane.as_ref()?;
            if pane.pane_id.as_str() != pane_id.as_str() {
                return None;
            }
            Some(owner.clone())
        })
    }

    pub(super) fn release_key(&mut self, key: &(AgentKind, AgentSessionId)) {
        self.names.retain(|_, owner| owner != key);
        self.ordinals.retain(|_, owner| owner != &key.1);
    }

    pub(super) fn consume_launch_key(&mut self, key: &(AgentKind, AgentSessionId)) {
        if key.1.is_provisional() {
            self.consumed_launches.insert(key.1.clone());
        }
    }

    pub(super) fn launch_consumed(&self, agent_id: &AgentSessionId) -> bool {
        self.consumed_launches.contains(agent_id)
    }

    pub(super) fn assign(
        &mut self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        observation: &AgentLifecycleObservation,
        prior: Option<&AgentState>,
    ) -> CardIdentity {
        let key = (kind.clone(), agent_id.clone());
        let child = observation.parent_agent_id.is_some()
            || prior.is_some_and(|state| state.parent_agent_id.is_some());
        let name = if child {
            self.names.retain(|_, owner| owner != &key);
            self.assign_child_name(observation.agent_name.as_deref(), prior, agent_id)
        } else {
            self.assign_name(&key, observation, prior)
        };
        let name_explicit = observation.parent_agent_id.is_some()
            && observation.agent_name.as_deref() == Some(name.as_str())
            || prior
                .and_then(|state| state.name.as_deref())
                .is_some_and(|prior_name| {
                    prior.is_some_and(|state| state.name_explicit) && name == prior_name
                });
        let kind_ordinal = self.assign_ordinal(kind, agent_id, observation, prior);
        CardIdentity {
            name,
            name_explicit,
            kind_ordinal,
        }
    }

    pub(super) fn assign_launch(
        &mut self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        payload: &AgentLaunchPayload,
        prior: Option<&AgentState>,
    ) -> CardIdentity {
        let key = (kind.clone(), agent_id.clone());
        let name =
            self.assign_name_candidate(&key, Some(payload.agent_name.as_str()), prior, agent_id);
        let name_explicit = payload.agent_name_explicit && name == payload.agent_name;
        let candidate = payload
            .launch
            .kind_ordinal
            .or_else(|| prior.and_then(|state| state.kind_ordinal));
        let kind_ordinal = self.assign_ordinal_candidate(kind, agent_id, candidate, prior);
        CardIdentity {
            name,
            name_explicit,
            kind_ordinal,
        }
    }

    fn assign_existing(
        &mut self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        prior: Option<&AgentState>,
    ) -> CardIdentity {
        let key = (kind.clone(), agent_id.clone());
        let name = if prior.is_some_and(|state| state.parent_agent_id.is_some()) {
            self.assign_child_name(None, prior, agent_id)
        } else {
            self.assign_name_candidate(&key, None, prior, agent_id)
        };
        let name_explicit =
            prior
                .and_then(|state| state.name.as_deref())
                .is_some_and(|prior_name| {
                    prior.is_some_and(|state| state.name_explicit) && name == prior_name
                });
        let candidate = prior.and_then(|state| state.kind_ordinal);
        let kind_ordinal = self.assign_ordinal_candidate(kind, agent_id, candidate, prior);
        CardIdentity {
            name,
            name_explicit,
            kind_ordinal,
        }
    }

    fn assign_name(
        &mut self,
        key: &(AgentKind, AgentSessionId),
        observation: &AgentLifecycleObservation,
        prior: Option<&AgentState>,
    ) -> String {
        let candidate = observation
            .agent_name
            .as_deref()
            .filter(|name| usable_name(name))
            .or_else(|| {
                prior
                    .and_then(|state| state.name.as_deref())
                    .filter(|name| usable_name(name))
            });
        self.assign_name_candidate(key, candidate, prior, &key.1)
    }

    fn assign_child_name(
        &self,
        candidate: Option<&str>,
        prior: Option<&AgentState>,
        fallback_id: &AgentSessionId,
    ) -> String {
        if let Some(name) = candidate.filter(|name| usable_name(name)) {
            return name.to_owned();
        }
        if let Some(name) = prior
            .and_then(|state| state.name.as_deref())
            .filter(|name| usable_name(name))
        {
            return name.to_owned();
        }
        crate::harness::petname::mint_for_session(
            fallback_id,
            self.names.keys().map(String::as_str),
        )
    }

    fn assign_name_candidate(
        &mut self,
        key: &(AgentKind, AgentSessionId),
        candidate: Option<&str>,
        prior: Option<&AgentState>,
        fallback_id: &AgentSessionId,
    ) -> String {
        if let Some(name) = candidate
            && self.name_available_for(name, key)
        {
            self.names.insert(name.to_owned(), key.clone());
            return name.to_owned();
        }
        if let Some(name) = prior
            .and_then(|state| state.name.as_deref())
            .filter(|name| usable_name(name))
            && self.name_available_for(name, key)
        {
            self.names.insert(name.to_owned(), key.clone());
            return name.to_owned();
        }
        let taken = self
            .names
            .iter()
            .filter(|(_name, owner)| *owner != key)
            .map(|(name, _owner)| name.as_str());
        let name = crate::harness::petname::mint_for_session(fallback_id, taken);
        self.names.insert(name.clone(), key.clone());
        name
    }

    fn name_available_for(&self, name: &str, key: &(AgentKind, AgentSessionId)) -> bool {
        self.names.get(name).is_none_or(|owner| owner == key)
    }

    fn assign_ordinal(
        &mut self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        observation: &AgentLifecycleObservation,
        prior: Option<&AgentState>,
    ) -> u32 {
        let candidate = observation
            .launch
            .kind_ordinal
            .or_else(|| prior.and_then(|state| state.kind_ordinal));
        self.assign_ordinal_candidate(kind, agent_id, candidate, prior)
    }

    fn assign_ordinal_candidate(
        &mut self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        candidate: Option<u32>,
        prior: Option<&AgentState>,
    ) -> u32 {
        if let Some(ordinal) = candidate
            && self.ordinal_candidate_allowed(kind, agent_id, ordinal, prior)
        {
            self.ordinals
                .insert((kind.clone(), ordinal), agent_id.clone());
            self.next_ordinal
                .entry(kind.clone())
                .and_modify(|next| *next = (*next).max(ordinal.saturating_add(1)))
                .or_insert(ordinal.saturating_add(1));
            return ordinal;
        }
        let mut next = self.next_ordinal.get(kind).copied().unwrap_or(1).max(1);
        while !self.ordinal_available_for(kind, agent_id, next) {
            next = next.saturating_add(1);
        }
        self.ordinals.insert((kind.clone(), next), agent_id.clone());
        self.next_ordinal
            .insert(kind.clone(), next.saturating_add(1));
        next
    }

    fn ordinal_available_for(
        &self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        ordinal: u32,
    ) -> bool {
        self.ordinals
            .get(&(kind.clone(), ordinal))
            .is_none_or(|owner| owner == agent_id)
    }

    fn ordinal_candidate_allowed(
        &self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        ordinal: u32,
        prior: Option<&AgentState>,
    ) -> bool {
        if !self.ordinal_available_for(kind, agent_id, ordinal) {
            return false;
        }
        if prior.and_then(|state| state.kind_ordinal) == Some(ordinal) {
            return true;
        }
        ordinal >= self.next_ordinal.get(kind).copied().unwrap_or(1)
    }
}

pub(super) fn usable_name(name: &str) -> bool {
    crate::harness::petname::valid_agent_name(name)
}
