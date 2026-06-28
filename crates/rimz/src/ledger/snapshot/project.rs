//! The agent-lifecycle reducer: folds `agent.lifecycle` events into
//! [`AgentState`] rollups, carrying turn, phase, subagent, and model
//! state forward.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agents::AgentLifecycleObservation;
use crate::agents::lifecycle::{self, Transition};
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::pane::{PaneRef, RuntimeOwner, RuntimeOwnerKind};
use crate::schema::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope, EventKind};

/// How many user prompts a session's rollup keeps (`AgentState::recent_prompts`,
/// newest last). The events are durable, so the cap bounds only the projected
/// history, not the record; 16 covers a long working session without growing
/// the rollup cache unbounded.
const RECENT_PROMPTS_LIMIT: usize = 16;

/// Strip a trailing capability tag (`claude-opus-4-8[1m]` → `claude-opus-4-8`)
/// so the sidebar shows one stable model id per agent. The tag rides only on a
/// fresh-launch SessionStart payload — it is absent after `/clear`, the
/// transcript records the bare id, and no model env var exposes it — so it can
/// never be shown reliably. Idempotent on an already-bare id.
fn canonical_model(model: &str) -> String {
    match model.split_once('[') {
        Some((base, _)) => base.trim_end().to_owned(),
        None => model.to_owned(),
    }
}

/// Fold `agent.lifecycle` events into the latest [`AgentState`] per
/// agent_id, keyed by `(agent_kind, agent_id)`. A session-less event is
/// quarantined (logged, folded to nothing) — identity is required. Events
/// are walked in log order, so the newest observation wins.
///
/// Each event is a *partial* update: `status` always comes from the event,
/// but the stable capability/identity fields (`model`, `effort`,
/// `context_window`, worktree, pane) carry forward from the prior state when
/// the event omits them. A `UserPromptSubmit` therefore moves the agent to
/// running without erasing its model line.
#[cfg(test)]
pub(super) fn reduce_agent_states(events: &[EventEnvelope]) -> Vec<AgentState> {
    reduce_agent_states_seeded_with_identity(BTreeMap::new(), AgentIdentityState::default(), events)
        .0
        .into_values()
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct AgentIdentityState {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    names: BTreeMap<String, (AgentKind, AgentSessionId)>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    next_ordinal: BTreeMap<AgentKind, u32>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    consumed_launches: BTreeSet<AgentSessionId>,
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

pub(super) fn backfill_agent_identities(
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
        agents[index].name = Some(identity.name);
        agents[index].kind_ordinal = Some(identity.kind_ordinal);
        map.insert(key, agents[index].clone());
    }
    allocator.state()
}

fn has_card_identity(agent: &AgentState) -> bool {
    agent.name.as_deref().is_some_and(usable_name) && agent.kind_ordinal.is_some()
}

/// [`reduce_agent_states`] resuming from a prior fold map. Each lifecycle
/// event reads only its own key's prior state, and the rebirth boundary is a
/// pointwise transform of the whole map at its log position — either way,
/// folding a delta onto the map the earlier prefix produced equals folding
/// the whole log from scratch, the property the incremental
/// [`catch_up_rollup`] and the rotation carryover both stand on.
#[cfg(test)]
pub(super) fn reduce_agent_states_seeded(
    seed: BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    events: &[EventEnvelope],
) -> BTreeMap<(AgentKind, AgentSessionId), AgentState> {
    reduce_agent_states_seeded_with_identity(seed, AgentIdentityState::default(), events).0
}

pub(super) fn reduce_agent_states_seeded_with_identity(
    seed: BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    identity_state: AgentIdentityState,
    events: &[EventEnvelope],
) -> (
    BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    AgentIdentityState,
) {
    let mut map = seed;
    let mut identity = CardIdentityAllocator::from_map_and_state(&map, identity_state);
    for event in events {
        // A mux rebirth renumbers panes from zero, so every stamp recorded
        // before the boundary names a pane that no longer exists — and the
        // reborn session reuses those ids for new panes. Clear them all here,
        // in log order, so a prior incarnation's session can never bind (or
        // block recovery of) a reused pane id; a stamp recorded by a later
        // event is the new incarnation's and stays. Sessions themselves are
        // kept — the boundary unstamps, it never tombstones.
        let payload = match event.kind() {
            EventKind::SessionRebirth => {
                for state in map.values_mut() {
                    state.pane = None;
                    state.kind_ordinal = None;
                }
                identity.reset_ordinals();
                continue;
            }
            EventKind::AgentLaunch(payload) => {
                let kind = AgentKind::new_unchecked(event.source.clone());
                reduce_agent_launch(&mut map, &mut identity, event, &kind, payload);
                continue;
            }
            EventKind::AgentLifecycle(payload) => *payload,
            EventKind::Message { .. } => continue,
            EventKind::Other {
                method: "agent.lifecycle",
                ..
            } => {
                debug!(
                    target: "rimz::agent::lifecycle",
                    event_id = %event.event_id,
                    "non-conforming agent.lifecycle event ignored",
                );
                continue;
            }
            EventKind::Other { .. } => continue,
        };
        let kind = AgentKind::new_unchecked(event.source.clone());
        // The agent-agnostic lifecycle intent this event carries. The status
        // and the phase/compacting heads are all derived from it through the
        // one shared `lifecycle::step` table — never taken verbatim — so an
        // illegal jump can't slip through unvalidated. Replay is silent here;
        // the ingestion path logs anomalies once per fresh event.
        let observation = payload.observation;
        let signal = observation.signal;
        // Identity is required: a session-less event is quarantined (folded
        // to nothing), mirroring the malformed-subagent-identity rule —
        // never silently merged into a shared per-kind bucket where two
        // distinct instances would collapse into one row. The ingestion path
        // warns once with the event in hand; here a `debug!` keeps every
        // cold rebuild's re-fold quiet.
        let Some(agent_id) = observation.agent_id.clone() else {
            debug!(
                target: "rimz::agent::lifecycle",
                event_id = %event.event_id,
                workspace = %event.workspace_id,
                kind = %kind,
                "session-less agent.lifecycle event quarantined",
            );
            continue;
        };
        let key = (kind.clone(), agent_id.clone());
        let mut provisional_prior = None;
        if let Some(agent_name) = observation.agent_name.as_deref()
            && !map.contains_key(&key)
            && let Some(provisional_key) =
                identity.adoptable_owner_for_name(&map, &kind, agent_name, &key)
        {
            provisional_prior = map.remove(&provisional_key);
            identity.release_key(&provisional_key);
            identity.consume_launch_key(&provisional_key);
        }
        if provisional_prior.is_none()
            && !map.contains_key(&key)
            && let Some(pane_id) = observation.pane_id.as_ref()
            && let Some(provisional_key) =
                identity.adoptable_owner_for_pane(&map, &kind, pane_id, &key)
        {
            provisional_prior = map.remove(&provisional_key);
            identity.release_key(&provisional_key);
            identity.consume_launch_key(&provisional_key);
        }
        let event_name = payload.event_name.as_deref();
        let event_parent_agent_id =
            non_empty_string(observation.parent_agent_id.as_deref()).map(AgentSessionId::from);
        let event_task = non_empty_string(observation.task.as_deref());
        if matches!(signal, lifecycle::LifecycleSignal::Ended) {
            identity.release_key(&key);
            map.remove(&key);
            continue;
        }
        let prior = map.get(&key).or(provisional_prior.as_ref());
        if matches!(signal, lifecycle::LifecycleSignal::SubagentStopped { .. })
            && prior.is_none()
            && event_parent_agent_id.is_some()
            && event_task.is_none()
        {
            debug!(
                target: "rimz::agent::lifecycle",
                event_id = %event.event_id,
                workspace = %event.workspace_id,
                session = %event.session_name,
                kind = %kind,
                source_kind = %event.source_kind,
                timestamp = %event.timestamp,
                event_name = event_name.unwrap_or(""),
                parent = event_parent_agent_id.as_deref().unwrap_or(""),
                child = %agent_id,
                "typeless SubagentStop for unknown child — ignored",
            );
            continue;
        }
        let establishes_identity = signal.establishes_identity();
        if prior.is_none() && !establishes_identity {
            debug!(
                target: "rimz::agent::binding",
                kind = %kind,
                agent_id = %agent_id,
                signal = ?signal,
                event_name = event_name.unwrap_or(""),
                "non-start lifecycle event created an unseen session in the reducer",
            );
        }
        let card_identity = identity.assign(&kind, &agent_id, &observation, prior);
        let state = assemble_agent_state(AgentStateInput {
            kind: &kind,
            agent_id: &agent_id,
            event,
            event_name,
            observation: &observation,
            signal,
            prior,
            event_parent_agent_id,
            event_task,
            establishes_identity,
            card_identity,
        });
        map.insert(key, state);
    }
    (map, identity.state())
}

#[derive(Clone, Debug)]
struct CardIdentity {
    name: String,
    kind_ordinal: u32,
}

fn reduce_agent_launch(
    map: &mut BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    identity: &mut CardIdentityAllocator,
    event: &EventEnvelope,
    kind: &AgentKind,
    payload: AgentLaunchPayload,
) {
    if !usable_name(&payload.agent_name) {
        debug!(
            target: "rimz::agent::launch",
            event_id = %event.event_id,
            workspace = %event.workspace_id,
            kind = %kind,
            agent_name = %payload.agent_name,
            "agent.launched event with invalid name ignored",
        );
        return;
    }
    let key = (kind.clone(), payload.agent_id.clone());
    if identity.launch_consumed(&payload.agent_id) {
        return;
    }
    if is_provisional_agent_id(&payload.agent_id)
        && let Some(owner) = identity.owner_for_name(&payload.agent_name)
        && owner.0 == *kind
        && owner != key
        && map.contains_key(&owner)
    {
        identity.consume_launch_key(&key);
        return;
    }
    if matches!(payload.state, AgentLaunchState::Failed)
        && !map.contains_key(&key)
        && identity.owner_for_name(&payload.agent_name).is_none()
    {
        return;
    }
    if let Some(owner) = identity.owner_for_name(&payload.agent_name)
        && owner != key
        && !map.contains_key(&owner)
    {
        identity.release_key(&owner);
    }
    let prior = map.get(&key);
    let card_identity = identity.assign_launch(kind, &payload.agent_id, &payload, prior);
    let state = assemble_launch_state(kind, event, payload, prior, card_identity);
    map.insert(key, state);
}

#[derive(Default)]
struct CardIdentityAllocator {
    names: BTreeMap<String, (AgentKind, AgentSessionId)>,
    ordinals: BTreeMap<(AgentKind, u32), AgentSessionId>,
    next_ordinal: BTreeMap<AgentKind, u32>,
    consumed_launches: BTreeSet<AgentSessionId>,
}

impl CardIdentityAllocator {
    fn from_map_and_state(
        map: &BTreeMap<(AgentKind, AgentSessionId), AgentState>,
        state: AgentIdentityState,
    ) -> Self {
        let mut allocator = Self {
            names: state.names,
            next_ordinal: state.next_ordinal,
            consumed_launches: state.consumed_launches,
            ordinals: BTreeMap::new(),
        };
        allocator.names.retain(|_, owner| map.contains_key(owner));
        for ((kind, agent_id), state) in map {
            if let Some(name) = state.name.as_deref().filter(|name| usable_name(name)) {
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

    fn state(&mut self) -> AgentIdentityState {
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

    fn reset_ordinals(&mut self) {
        self.ordinals.clear();
        self.next_ordinal.clear();
    }

    fn owner_for_name(&self, name: &str) -> Option<(AgentKind, AgentSessionId)> {
        self.names.get(name).cloned()
    }

    fn adoptable_owner_for_name(
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
        if is_provisional_agent_id(&owner.1) {
            return Some(owner.clone());
        }
        let prior = map.get(owner)?;
        (prior.kind_ordinal.is_none() || prior.pane.is_none()).then(|| owner.clone())
    }

    fn adoptable_owner_for_pane(
        &self,
        map: &BTreeMap<(AgentKind, AgentSessionId), AgentState>,
        kind: &AgentKind,
        pane_id: &PaneId,
        new_key: &(AgentKind, AgentSessionId),
    ) -> Option<(AgentKind, AgentSessionId)> {
        map.iter().find_map(|(owner, state)| {
            if owner == new_key || owner.0 != *kind || !is_provisional_agent_id(&owner.1) {
                return None;
            }
            let pane = state.pane.as_ref()?;
            if pane.pane_id.as_str() != pane_id.as_str() {
                return None;
            }
            Some(owner.clone())
        })
    }

    fn release_key(&mut self, key: &(AgentKind, AgentSessionId)) {
        self.names.retain(|_, owner| owner != key);
        self.ordinals.retain(|_, owner| owner != &key.1);
    }

    fn consume_launch_key(&mut self, key: &(AgentKind, AgentSessionId)) {
        if is_provisional_agent_id(&key.1) {
            self.consumed_launches.insert(key.1.clone());
        }
    }

    fn launch_consumed(&self, agent_id: &AgentSessionId) -> bool {
        self.consumed_launches.contains(agent_id)
    }

    fn assign(
        &mut self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        observation: &AgentLifecycleObservation,
        prior: Option<&AgentState>,
    ) -> CardIdentity {
        let key = (kind.clone(), agent_id.clone());
        let name = self.assign_name(&key, observation, prior);
        let kind_ordinal = self.assign_ordinal(kind, agent_id, observation, prior);
        CardIdentity { name, kind_ordinal }
    }

    fn assign_launch(
        &mut self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        payload: &AgentLaunchPayload,
        prior: Option<&AgentState>,
    ) -> CardIdentity {
        let key = (kind.clone(), agent_id.clone());
        let name =
            self.assign_name_candidate(&key, Some(payload.agent_name.as_str()), prior, agent_id);
        let candidate = payload
            .kind_ordinal
            .or_else(|| prior.and_then(|state| state.kind_ordinal));
        let kind_ordinal = self.assign_ordinal_candidate(kind, agent_id, candidate, prior);
        CardIdentity { name, kind_ordinal }
    }

    fn assign_existing(
        &mut self,
        kind: &AgentKind,
        agent_id: &AgentSessionId,
        prior: Option<&AgentState>,
    ) -> CardIdentity {
        let key = (kind.clone(), agent_id.clone());
        let name = self.assign_name_candidate(&key, None, prior, agent_id);
        let candidate = prior.and_then(|state| state.kind_ordinal);
        let kind_ordinal = self.assign_ordinal_candidate(kind, agent_id, candidate, prior);
        CardIdentity { name, kind_ordinal }
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
        let name = crate::petname::mint_for_session(fallback_id, taken);
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

fn usable_name(name: &str) -> bool {
    crate::petname::valid_name(name)
        && !crate::petname::collides_with_reserved_prefix(name, crate::agents::known_kinds())
}

fn is_provisional_agent_id(agent_id: &AgentSessionId) -> bool {
    agent_id.is_provisional()
}

struct AgentStateInput<'a> {
    kind: &'a AgentKind,
    agent_id: &'a AgentSessionId,
    event: &'a EventEnvelope,
    event_name: Option<&'a str>,
    observation: &'a AgentLifecycleObservation,
    signal: lifecycle::LifecycleSignal,
    prior: Option<&'a AgentState>,
    event_parent_agent_id: Option<AgentSessionId>,
    event_task: Option<String>,
    establishes_identity: bool,
    card_identity: CardIdentity,
}

fn assemble_agent_state(input: AgentStateInput<'_>) -> AgentState {
    let lifecycle = lifecycle_projection(input.prior, input.event.timestamp, input.signal);
    let enrichment = enrichment_projection(input.observation, input.prior, input.kind);
    let parent_agent_id = input
        .event_parent_agent_id
        .or_else(|| input.prior.and_then(|p| p.parent_agent_id.clone()));
    let worktree = worktree_projection(
        input.observation,
        input.prior,
        input.establishes_identity,
        input.event_name,
    );
    let runtime = runtime_projection(input.observation, input.prior, input.agent_id);
    let prompt = prompt_projection(
        input.observation,
        input.prior,
        parent_agent_id.is_some(),
        input.event_task,
    );
    AgentState {
        agent_id: input.agent_id.clone(),
        kind: input.kind.clone(),
        name: Some(input.card_identity.name),
        kind_ordinal: Some(input.card_identity.kind_ordinal),
        profile: input
            .observation
            .profile
            .clone()
            .or_else(|| input.prior.and_then(|state| state.profile.clone())),
        role: input
            .observation
            .role
            .clone()
            .or_else(|| input.prior.and_then(|state| state.role.clone())),
        team: input
            .observation
            .team
            .clone()
            .or_else(|| input.prior.and_then(|state| state.team.clone())),
        channel: input
            .observation
            .channel
            .clone()
            .or_else(|| input.prior.and_then(|state| state.channel.clone())),
        status: lifecycle.status,
        phase: lifecycle.phase,
        pane: pane_projection(input.observation, input.prior),
        agent_pid: runtime.agent_pid,
        agent_process_start: runtime.agent_process_start,
        runtime_owner: runtime.runtime_owner,
        parent_agent_id,
        worktree_path: worktree.path,
        worktree_branch: worktree.branch,
        task: prompt.task,
        prompt: prompt.prompt,
        description: input.prior.and_then(|state| state.description.clone()),
        transcript_path: transcript_path_projection(input.observation, input.prior),
        origin: origin_projection(input.observation, input.prior),
        recent_prompts: prompt.recent_prompts,
        model: model_projection(input.observation, input.prior),
        effort: effort_projection(input.observation, input.prior),
        context_pct: enrichment.context_pct,
        context_window: enrichment.context_window,
        total_tokens: enrichment.total_tokens,
        cache_read_input_tokens: enrichment.cache_read_input_tokens,
        cache_write_input_tokens: enrichment.cache_write_input_tokens,
        fresh_input_tokens: enrichment.fresh_input_tokens,
        output_tokens: enrichment.output_tokens,
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: lifecycle.turn_started_at,
        compacting_since: lifecycle.compacting_since,
        compaction_count: lifecycle.compaction_count,
        last_seen: input.event.timestamp,
        last_activity: input.event.timestamp,
        registered_at: lifecycle.registered_at,
    }
}

fn assemble_launch_state(
    kind: &AgentKind,
    event: &EventEnvelope,
    payload: AgentLaunchPayload,
    prior: Option<&AgentState>,
    card_identity: CardIdentity,
) -> AgentState {
    let pane = payload
        .pane_id
        .clone()
        .map(PaneRef::from_id)
        .or_else(|| prior.and_then(|state| state.pane.clone()));
    let runtime_owner = payload
        .runtime_owner
        .clone()
        .or_else(|| prior.and_then(|state| state.runtime_owner.clone()));
    let agent_pid = runtime_owner.as_ref().map(|owner| owner.pid);
    let agent_process_start = runtime_owner
        .as_ref()
        .and_then(|owner| owner.process_start.clone());
    let prompt = payload
        .prompt
        .clone()
        .or_else(|| prior.and_then(|state| state.prompt.clone()));
    let description = payload
        .description
        .clone()
        .or_else(|| prior.and_then(|state| state.description.clone()));
    let recent_prompts = match prompt.as_ref() {
        Some(prompt) => vec![prompt.clone()],
        None => prior
            .map(|state| state.recent_prompts.clone())
            .unwrap_or_default(),
    };
    let (status, phase) = match payload.state {
        AgentLaunchState::Failed => (AgentStatus::Failed, lifecycle::TurnPhase::Idle),
        AgentLaunchState::Starting | AgentLaunchState::Bound => {
            if payload.prompt.is_some() {
                (AgentStatus::Running, lifecycle::TurnPhase::Reasoning)
            } else {
                (AgentStatus::Idle, lifecycle::TurnPhase::Idle)
            }
        }
    };
    AgentState {
        agent_id: payload.agent_id,
        kind: kind.clone(),
        name: Some(card_identity.name),
        kind_ordinal: Some(card_identity.kind_ordinal),
        profile: payload
            .profile
            .clone()
            .or_else(|| prior.and_then(|state| state.profile.clone())),
        role: payload
            .role
            .clone()
            .or_else(|| prior.and_then(|state| state.role.clone())),
        team: payload
            .team
            .clone()
            .or_else(|| prior.and_then(|state| state.team.clone())),
        channel: payload
            .channel
            .clone()
            .or_else(|| prior.and_then(|state| state.channel.clone())),
        status,
        phase,
        pane,
        agent_pid,
        agent_process_start,
        runtime_owner,
        parent_agent_id: None,
        worktree_path: payload
            .worktree_path
            .or_else(|| prior.and_then(|state| state.worktree_path.clone())),
        worktree_branch: payload
            .worktree_branch
            .or_else(|| prior.and_then(|state| state.worktree_branch.clone())),
        task: prompt.clone(),
        prompt,
        description,
        transcript_path: prior.and_then(|state| state.transcript_path.clone()),
        origin: prior.and_then(|state| state.origin),
        recent_prompts,
        model: prior.and_then(|state| state.model.clone()),
        effort: prior.and_then(|state| state.effort.clone()),
        context_pct: prior.and_then(|state| state.context_pct),
        context_window: prior.and_then(|state| state.context_window),
        total_tokens: prior.and_then(|state| state.total_tokens),
        cache_read_input_tokens: prior.and_then(|state| state.cache_read_input_tokens),
        cache_write_input_tokens: prior.and_then(|state| state.cache_write_input_tokens),
        fresh_input_tokens: prior.and_then(|state| state.fresh_input_tokens),
        output_tokens: prior.and_then(|state| state.output_tokens),
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: Some(event.timestamp),
        compacting_since: None,
        compaction_count: prior.map_or(0, |state| state.compaction_count),
        last_seen: event.timestamp,
        last_activity: event.timestamp,
        registered_at: prior.and_then(|state| state.registered_at),
    }
}

struct LifecycleProjection {
    status: AgentStatus,
    phase: lifecycle::TurnPhase,
    compacting_since: Option<Timestamp>,
    compaction_count: u32,
    turn_started_at: Option<Timestamp>,
    registered_at: Option<Timestamp>,
}

fn lifecycle_projection(
    prior: Option<&AgentState>,
    timestamp: Timestamp,
    signal: lifecycle::LifecycleSignal,
) -> LifecycleProjection {
    let prev_state = prior.map(AgentState::lifecycle);
    let Transition {
        next,
        compaction_closed,
        opened_turn,
        ..
    } = lifecycle::step(prev_state.as_ref(), &signal);
    let compacting_since = if next.compacting {
        Some(timestamp)
    } else {
        None
    };
    let compaction_count = prior.map_or(0, |p| p.compaction_count) + u32::from(compaction_closed);
    // A context reset that rests the agent retires the prior turn's subagents: a
    // manual `/compact` (`CompactionEnded` resting to idle) summarizes away the
    // turn the children belonged to, and a `/clear` (`Registered`) drops it
    // outright. Advancing the subagent boundary here makes a user-typed reset
    // behave like automatic compaction from a rested state, which already opens a
    // turn. The rest gate is load-bearing: automatic compaction *mid-turn* resumes
    // the same turn (stays `running`), so its in-flight children stay listed.
    // Matching the signal — not `compaction_closed` — still fires when the
    // `PreCompact` bracket open was missed; on a fresh-launch `Registered` it is a
    // no-op (no children yet).
    let resets_context = next.status == AgentStatus::Idle
        && matches!(
            signal,
            lifecycle::LifecycleSignal::CompactionEnded { .. }
                | lifecycle::LifecycleSignal::Registered
        );
    let turn_started_at = if opened_turn || resets_context {
        Some(timestamp)
    } else {
        prior.and_then(|p| p.turn_started_at)
    };
    LifecycleProjection {
        status: next.status,
        phase: next.phase,
        compacting_since,
        compaction_count,
        turn_started_at,
        registered_at: prior.and_then(|p| p.registered_at).or(Some(timestamp)),
    }
}

struct EnrichmentProjection {
    context_pct: Option<u8>,
    context_window: Option<u64>,
    total_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_write_input_tokens: Option<u64>,
    fresh_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

fn enrichment_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
    kind: &AgentKind,
) -> EnrichmentProjection {
    let context_window = observation
        .context_window
        .or_else(|| prior.and_then(|p| p.context_window));
    let total_tokens = observation
        .total_tokens
        .or_else(|| prior.and_then(|p| p.total_tokens));
    let cache_read_input_tokens = observation
        .cache_read_input_tokens
        .or_else(|| prior.and_then(|p| p.cache_read_input_tokens));
    let cache_write_input_tokens = observation
        .cache_write_input_tokens
        .or_else(|| prior.and_then(|p| p.cache_write_input_tokens));
    let fresh_input_tokens = observation
        .fresh_input_tokens
        .or_else(|| prior.and_then(|p| p.fresh_input_tokens));
    let output_tokens = observation
        .output_tokens
        .or_else(|| prior.and_then(|p| p.output_tokens));
    // One denominator for the gauge: the percentage is derived from the same
    // window that is stored and displayed, so the bar can never disagree with
    // the window label. An adapter that stamps an authoritative percentage (pi,
    // from its in-process gauge) overrides; otherwise derive from the resolved
    // window (folded, else the kind's descriptor default). Carry the prior
    // value only when neither the explicit stamp nor a numerator exists.
    let resolved_window = context_window.or_else(|| {
        crate::agents::descriptor_by_kind(kind.as_str())
            .and_then(|descriptor| descriptor.default_context_window)
    });
    let used_tokens = context_used_tokens(
        cache_read_input_tokens,
        cache_write_input_tokens,
        fresh_input_tokens,
        total_tokens,
    );
    let context_pct = observation
        .context_pct
        .or_else(|| derive_context_pct(used_tokens, resolved_window))
        .or_else(|| prior.and_then(|p| p.context_pct));
    EnrichmentProjection {
        context_pct,
        context_window,
        total_tokens,
        cache_read_input_tokens,
        cache_write_input_tokens,
        fresh_input_tokens,
        output_tokens,
    }
}

/// Tokens currently occupying the window: the per-call context split
/// (`cache_read + cache_write + fresh_input`) when the adapter persists it,
/// else the latest turn's token total. This is the gauge numerator the
/// percentage scales.
fn context_used_tokens(
    cache_read_input_tokens: Option<u64>,
    cache_write_input_tokens: Option<u64>,
    fresh_input_tokens: Option<u64>,
    total_tokens: Option<u64>,
) -> Option<u64> {
    match fresh_input_tokens {
        Some(fresh) => Some(
            cache_read_input_tokens.unwrap_or(0) + cache_write_input_tokens.unwrap_or(0) + fresh,
        ),
        None => total_tokens,
    }
}

/// The integer context-fill percentage (0..=100) of `used` tokens over the
/// resolved `window`. `None` when either input is unknown so the gauge falls
/// back rather than rendering a fabricated 0%.
fn derive_context_pct(used: Option<u64>, window: Option<u64>) -> Option<u8> {
    let (used, window) = (used?, window?);
    (window > 0).then(|| (used.saturating_mul(100) / window).min(100) as u8)
}

struct WorktreeProjection {
    path: Option<String>,
    branch: Option<String>,
}

fn worktree_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
    establishes_identity: bool,
    event_name: Option<&str>,
) -> WorktreeProjection {
    let event_path = observation.worktree_path.clone();
    let event_branch = observation.worktree_branch.clone();
    let prior_path = prior.and_then(|p| p.worktree_path.clone());
    let prior_branch = prior.and_then(|p| p.worktree_branch.clone());
    let event_first = establishes_identity || event_name.is_none();
    WorktreeProjection {
        path: if event_first {
            event_path.or(prior_path)
        } else {
            prior_path.or(event_path)
        },
        branch: if event_first {
            event_branch.or(prior_branch)
        } else {
            prior_branch.or(event_branch)
        },
    }
}

struct RuntimeProjection {
    agent_pid: Option<u32>,
    agent_process_start: Option<String>,
    runtime_owner: Option<RuntimeOwner>,
}

fn runtime_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
    agent_id: &AgentSessionId,
) -> RuntimeProjection {
    let agent_pid = observation
        .agent_pid
        .or_else(|| prior.and_then(|p| p.agent_pid));
    let agent_process_start = observation
        .agent_process_start
        .clone()
        .or_else(|| prior.and_then(|p| p.agent_process_start.clone()));
    let runtime_owner = observation
        .runtime_owner
        .clone()
        .or_else(|| {
            agent_pid.map(|pid| {
                RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    agent_id.to_string(),
                    pid,
                    agent_process_start.clone(),
                )
            })
        })
        .or_else(|| prior.and_then(|p| p.runtime_owner.clone()));
    RuntimeProjection {
        agent_pid,
        agent_process_start,
        runtime_owner,
    }
}

struct PromptProjection {
    task: Option<String>,
    prompt: Option<String>,
    recent_prompts: Vec<String>,
}

fn prompt_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
    is_subagent: bool,
    event_task: Option<String>,
) -> PromptProjection {
    let task = if is_subagent {
        event_task.or_else(|| prior.and_then(|p| p.task.clone()))
    } else {
        non_empty_string(observation.task.as_deref())
    };
    let event_prompt = observation.prompt.clone();
    let mut recent_prompts = prior.map(|p| p.recent_prompts.clone()).unwrap_or_default();
    if let Some(prompt) = event_prompt.as_deref().filter(|prompt| !prompt.is_empty()) {
        recent_prompts.push(prompt.to_owned());
        let excess = recent_prompts.len().saturating_sub(RECENT_PROMPTS_LIMIT);
        if excess > 0 {
            recent_prompts.drain(0..excess);
        }
    }
    PromptProjection {
        task,
        prompt: event_prompt.or_else(|| prior.and_then(|p| p.prompt.clone())),
        recent_prompts,
    }
}

fn transcript_path_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
) -> Option<String> {
    observation
        .transcript_path
        .clone()
        .or_else(|| prior.and_then(|p| p.transcript_path.clone()))
}

fn origin_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
) -> Option<crate::agents::codex::SessionOrigin> {
    observation.origin.or_else(|| prior.and_then(|p| p.origin))
}

fn model_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
) -> Option<String> {
    observation
        .model
        .clone()
        .map(|raw| canonical_model(&raw))
        .or_else(|| prior.and_then(|p| p.model.clone()))
}

fn effort_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
) -> Option<String> {
    observation
        .effort
        .clone()
        .or_else(|| prior.and_then(|p| p.effort.clone()))
}

fn pane_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
) -> Option<PaneRef> {
    observation
        .pane_id
        .clone()
        .map(PaneRef::from_id)
        .or_else(|| prior.and_then(|p| p.pane.clone()))
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests;
