//! The agent-lifecycle reducer: folds `agent.lifecycle` events into
//! [`AgentState`] rollups, carrying turn, phase, subagent, and model
//! state forward.

use std::collections::BTreeMap;

use jiff::Timestamp;
use tracing::debug;

use crate::agents::lifecycle::{self, Transition};
use crate::agents::state::{append_recent_prompt, usable_description};
use crate::agents::{AgentLifecycleObservation, LaunchParams};
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, AgentSessionId};
use crate::message::{MessageBody, MessageStatus};
use crate::pane::{PaneRef, RuntimeOwner, RuntimeOwnerKind};
use crate::store::event::{
    AgentAttachPayload, AgentLaunchPayload, AgentLaunchState, AgentLifecyclePayload, EventEnvelope,
    EventKind, MessageEventPayload,
};

mod identity;

pub(crate) use identity::AgentIdentityState;
pub(super) use identity::backfill_agent_identities;
use identity::{CardIdentity, CardIdentityAllocator, usable_name};

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
    let events = decode_events(events);
    reduce_agent_states_seeded_with_identity(
        BTreeMap::new(),
        AgentIdentityState::default(),
        &events,
    )
    .0
    .into_values()
    .collect()
}

pub(super) struct FoldEvent<'a> {
    pub(super) envelope: &'a EventEnvelope,
    pub(super) kind: EventKind<'a>,
}

pub(super) fn decode_events(events: &[EventEnvelope]) -> Vec<FoldEvent<'_>> {
    events
        .iter()
        .map(|envelope| FoldEvent {
            envelope,
            kind: envelope.kind(),
        })
        .collect()
}

/// A mux rebirth renumbers panes from zero, so every stamp recorded before
/// the boundary names a pane that no longer exists and the reborn session can
/// reuse it. Sessions stay alive across the boundary; only pane and ordinal
/// stamps are retired.
pub(super) fn unstamp_for_rebirth<'a>(agents: impl IntoIterator<Item = &'a mut AgentState>) {
    for agent in agents {
        agent.pane = None;
        agent.kind_ordinal = None;
    }
}

pub(super) fn stamp_compact_commands_in_agents(
    agents: &mut [AgentState],
    events: &[FoldEvent<'_>],
) {
    for event in events {
        let EventKind::Message { payload, .. } = &event.kind else {
            continue;
        };
        stamp_compact_command(agents.iter_mut(), payload);
    }
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
    let events = decode_events(events);
    reduce_agent_states_seeded_with_identity(seed, AgentIdentityState::default(), &events).0
}

pub(super) fn reduce_agent_states_seeded_with_identity(
    seed: BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    identity_state: AgentIdentityState,
    events: &[FoldEvent<'_>],
) -> (
    BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    AgentIdentityState,
) {
    let mut map = seed;
    let mut identity = CardIdentityAllocator::from_map_and_state(&map, identity_state);
    for event in events {
        let envelope = event.envelope;
        match &event.kind {
            EventKind::SessionRebirth => {
                unstamp_for_rebirth(map.values_mut());
                identity.reset_ordinals();
            }
            EventKind::AgentLaunch(payload) => {
                let kind = AgentKind::new_unchecked(envelope.source.clone());
                reduce_agent_launch(&mut map, &mut identity, envelope, &kind, payload);
            }
            EventKind::AgentAttach(payload) => {
                let kind = AgentKind::new_unchecked(envelope.source.clone());
                reduce_agent_attach(&mut map, envelope, &kind, payload);
            }
            EventKind::AgentLifecycle(payload) => {
                reduce_lifecycle_event(&mut map, &mut identity, envelope, payload);
            }
            EventKind::Message { payload, .. } => {
                stamp_compact_command(map.values_mut(), payload);
            }
            EventKind::SessionDeath(_) => {}
            EventKind::Other {
                method: "agent.lifecycle",
                ..
            } => {
                debug!(
                    target: "rimz::agent::lifecycle",
                    event_id = %envelope.event_id,
                    "non-conforming agent.lifecycle event ignored",
                );
            }
            EventKind::Other { .. } => {}
        }
    }
    (map, identity.state())
}

fn reduce_agent_attach(
    map: &mut BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    event: &EventEnvelope,
    kind: &AgentKind,
    payload: &AgentAttachPayload,
) {
    let key = (kind.clone(), payload.agent_id.clone());
    if !map.contains_key(&key) && payload.launch_id.is_none() {
        debug!(
            target: "rimz::agent::binding",
            kind = %kind,
            agent_id = %payload.agent_id,
            pane_id = %payload.pane_id,
            "legacy agent.attached event for unknown session ignored",
        );
        return;
    }
    // A discovered provider session may have no prior RimZ event. The resume
    // wrapper knows enough durable identity to seed it before the provider
    // process starts, so the process can launch children immediately.
    let state = map.entry(key).or_insert_with(|| {
        AgentState::seed(
            kind.clone(),
            payload.agent_id.clone(),
            AgentStatus::Idle,
            event.timestamp,
        )
    });
    if let Some(launch_id) = &payload.launch_id {
        state.launch_id = Some(launch_id.clone());
    }
    state.pane = Some(PaneRef {
        pane_pid: payload.pane_pid,
        ..PaneRef::from_id(payload.pane_id.clone())
    });
    state.runtime_owner = Some(payload.runtime_owner.clone());
}

fn reduce_lifecycle_event(
    map: &mut BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    identity: &mut CardIdentityAllocator,
    event: &EventEnvelope,
    payload: &AgentLifecyclePayload,
) {
    let kind = AgentKind::new_unchecked(event.source.clone());
    // The agent-agnostic lifecycle intent this event carries. The status
    // and the phase/compacting heads are all derived from it through the
    // one shared `lifecycle::step` table — never taken verbatim — so an
    // illegal jump can't slip through unvalidated. Replay is silent here;
    // the ingestion path logs anomalies once per fresh event.
    let observation = &payload.observation;
    let signal = observation.signal.clone();
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
        return;
    };
    let key = (kind.clone(), agent_id.clone());
    let event_is_child = observation.parent_agent_id.is_some();
    let provisional_prior = if event_is_child {
        // Exact child IDs are authoritative. A child may share its type label
        // and pane with siblings or its parent, so neither is an adoption or
        // provisional-release key.
        None
    } else if map.contains_key(&key) {
        release_stamped_provisional_for_existing(map, identity, &kind, &key, observation);
        None
    } else {
        adopt_provisional(map, identity, &kind, &key, observation)
    };
    let event_name = payload.event_name.as_deref();
    let event_parent_agent_id =
        non_empty_string(observation.parent_agent_id.as_deref()).map(AgentSessionId::from);
    let event_task = non_empty_string(observation.task.as_deref());
    let prior = map.get(&key).or(provisional_prior.as_ref());
    if let Some(reason) = quarantine_reason(
        &signal,
        prior,
        observation.compacted_from.is_some(),
        event_parent_agent_id.as_ref(),
        event_task.as_deref(),
    ) {
        debug!(
            target: "rimz::agent::lifecycle",
            event_id = %event.event_id,
            workspace = %event.workspace_id,
            kind = %kind,
            agent_id = %agent_id,
            reason,
            "agent.lifecycle event quarantined",
        );
        return;
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
    let card_identity = identity.assign(&kind, &agent_id, observation, prior);
    let mut state = assemble_agent_state(AgentStateInput {
        kind: &kind,
        agent_id: &agent_id,
        event,
        event_name,
        observation,
        signal,
        prior,
        event_parent_agent_id,
        event_task,
        establishes_identity,
        card_identity,
    });
    inherit_compaction_registration(map, &mut state);
    map.insert(key, state);
}

fn quarantine_reason(
    signal: &lifecycle::LifecycleSignal,
    prior: Option<&AgentState>,
    has_compaction_predecessor: bool,
    event_parent_agent_id: Option<&AgentSessionId>,
    event_task: Option<&str>,
) -> Option<&'static str> {
    if prior.is_some() {
        return None;
    }
    match signal {
        lifecycle::LifecycleSignal::Lost => {
            Some("lost marker for unknown session ignored by agent-state reducer")
        }
        lifecycle::LifecycleSignal::Compacting
        | lifecycle::LifecycleSignal::CompactionEnded { .. }
            if !has_compaction_predecessor =>
        {
            Some("compaction signal for unknown session ignored by agent-state reducer")
        }
        lifecycle::LifecycleSignal::SubagentStopped { .. }
            if event_parent_agent_id.is_some() && event_task.is_none() =>
        {
            Some("typeless SubagentStop for unknown child — ignored")
        }
        _ => None,
    }
}

fn adopt_provisional(
    map: &mut BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    identity: &mut CardIdentityAllocator,
    kind: &AgentKind,
    key: &(AgentKind, AgentSessionId),
    observation: &AgentLifecycleObservation,
) -> Option<AgentState> {
    if let Some(provisional_key) = observation
        .agent_name
        .as_deref()
        .and_then(|name| identity.adoptable_owner_for_name(map, kind, name, key))
        && let Some(prior) = retire_provisional(map, identity, &provisional_key)
    {
        return Some(prior);
    }
    let provisional_key = observation
        .pane_id
        .as_ref()
        .and_then(|pane_id| identity.adoptable_owner_for_pane(map, kind, pane_id, key))?;
    retire_provisional(map, identity, &provisional_key)
}

fn release_stamped_provisional_for_existing(
    map: &mut BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    identity: &mut CardIdentityAllocator,
    kind: &AgentKind,
    key: &(AgentKind, AgentSessionId),
    observation: &AgentLifecycleObservation,
) {
    if map.get(key).is_some_and(AgentState::is_provider_subagent) {
        return;
    }
    if map.get(key).is_none_or(|state| state.pane.is_some()) {
        return;
    }
    let Some(provisional_key) = observation
        .pane_id
        .as_ref()
        .and_then(|pane_id| identity.adoptable_owner_for_pane(map, kind, pane_id, key))
    else {
        return;
    };
    let _ = retire_provisional(map, identity, &provisional_key);
}

fn retire_provisional(
    map: &mut BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    identity: &mut CardIdentityAllocator,
    key: &(AgentKind, AgentSessionId),
) -> Option<AgentState> {
    let prior = map.remove(key);
    identity.release_key(key);
    identity.consume_launch_key(key);
    prior
}

fn reduce_agent_launch(
    map: &mut BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    identity: &mut CardIdentityAllocator,
    event: &EventEnvelope,
    kind: &AgentKind,
    payload: &AgentLaunchPayload,
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
    if payload.agent_id.is_provisional()
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
    let card_identity = identity.assign_launch(kind, &payload.agent_id, payload, prior);
    let state = assemble_launch_state(kind, event, payload, prior, card_identity);
    map.insert(key, state);
}

fn stamp_compact_command<'a>(
    agents: impl IntoIterator<Item = &'a mut AgentState>,
    payload: &MessageEventPayload,
) {
    let Some(tokens) = compact_command_tokens(payload) else {
        return;
    };
    let mut agents = agents.into_iter().collect::<Vec<_>>();
    if let Some(agent) = agents
        .iter_mut()
        .find(|agent| agent.kind == payload.kind && agent.agent_id == payload.agent_id)
    {
        agent.last_compact_command_tokens = Some(tokens);
        return;
    }
    let Some(agent_name) = payload.agent_name.as_deref() else {
        return;
    };
    if let Some(agent) = agents
        .iter_mut()
        .find(|agent| agent.kind == payload.kind && agent.name.as_deref() == Some(agent_name))
    {
        agent.last_compact_command_tokens = Some(tokens);
    }
}

fn compact_command_tokens(payload: &MessageEventPayload) -> Option<u64> {
    if payload.body != MessageBody::Command
        || !matches!(
            payload.status,
            MessageStatus::Sent | MessageStatus::Delivered
        )
    {
        return None;
    }
    payload.compacted_context_tokens
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

/// The carried baseline: every carry-forward and identity field cloned from
/// `prior`, activity fields cleared, enrichment sidecars left for projection.
/// This is the lifetime table's code home; see docs/internals/agents/model.md
/// § The rollup.
fn carried_base(
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    prior: Option<&AgentState>,
    event_ts: Timestamp,
) -> AgentState {
    let mut state = AgentState::seed(kind.clone(), agent_id.clone(), AgentStatus::Idle, event_ts);
    if let Some(prior) = prior {
        state.launch_id = prior.launch_id.clone();
        state.profile = prior.profile.clone();
        state.mode = prior.mode;
        state.role = prior.role.clone();
        state.team = prior.team.clone();
        state.launch_group = prior.launch_group.clone();
        state.launch_ordinal = prior.launch_ordinal;
        state.channel = prior.channel.clone();
        state.pane = prior.pane.clone();
        state.runtime_owner = prior.runtime_owner.clone();
        state.parent_agent_id = prior.parent_agent_id.clone();
        state.parent_agent_kind = prior.parent_agent_kind.clone();
        state.launch_depth = prior.launch_depth;
        state.worktree_path = prior.worktree_path.clone();
        state.worktree_branch = prior.worktree_branch.clone();
        state.task = prior.task.clone();
        state.first_prompt = prior.first_prompt.clone();
        state.prompt = prior.prompt.clone();
        state.description = prior.description.clone();
        state.transcript_path = prior.transcript_path.clone();
        state.origin = prior.origin;
        state.compacted_from = prior.compacted_from.clone();
        state.recent_prompts = prior.recent_prompts.clone();
        state.model = prior.model.clone();
        state.effort = prior.effort.clone();
        state.budget = prior.budget.clone();
        state.usage = prior.usage.clone();
        state.compaction_count = prior.compaction_count;
        state.tool_calls = prior.tool_calls.clone();
        state.last_compact_command_tokens = prior.last_compact_command_tokens;
        state.registered_at = prior.registered_at.or(Some(event_ts));
    }
    state
}

fn fold_launch_params(state: &mut AgentState, launch: &LaunchParams) {
    if let Some(parent_agent_id) = &launch.parent_agent_id {
        state.parent_agent_id = Some(parent_agent_id.clone());
    }
    if let Some(parent_agent_kind) = &launch.parent_agent_kind {
        state.parent_agent_kind = Some(parent_agent_kind.clone());
    }
    if let Some(launch_depth) = launch.launch_depth {
        state.launch_depth = Some(launch_depth);
    }
    if let Some(profile) = &launch.profile {
        state.profile = Some(profile.clone());
    }
    if let Some(mode) = launch.mode {
        state.mode = Some(mode);
    }
    if let Some(role) = &launch.role {
        state.role = Some(role.clone());
    }
    if let Some(team) = &launch.team {
        state.team = Some(team.clone());
    }
    if let Some(launch_group) = &launch.launch_group {
        state.launch_group = Some(launch_group.clone());
    }
    if let Some(launch_ordinal) = launch.launch_ordinal {
        state.launch_ordinal = Some(launch_ordinal);
    }
    if let Some(channel) = &launch.channel {
        state.channel = Some(channel.clone());
    }
    if let Some(model) = &launch.model {
        state.model = Some(canonical_model(model));
    }
    if let Some(effort) = &launch.effort {
        state.effort = Some(effort.clone());
    }
    if let Some(budget) = &launch.budget {
        state.budget = Some(budget.clone());
    }
}

fn assemble_agent_state(input: AgentStateInput<'_>) -> AgentState {
    let mut state = carried_base(
        input.kind,
        input.agent_id,
        input.prior,
        input.event.timestamp,
    );
    fold_launch_params(&mut state, &input.observation.launch);
    let ended_at =
        matches!(&input.signal, lifecycle::LifecycleSignal::Ended).then_some(input.event.timestamp);
    let lifecycle = lifecycle_projection(input.prior, input.event.timestamp, input.signal);
    let default_window = crate::agents::spec_by_kind(input.kind.as_str())
        .and_then(|definition| definition.default_context_window);
    let usage = input
        .observation
        .usage
        .merge(input.prior.map(|prior| &prior.usage), default_window);
    // Established lineage stays authoritative. The explicit adoption event is
    // the one path that converts a provisional root after provider evidence
    // became readable later than the child's own hooks.
    let parent_agent_id = match input.prior {
        Some(prior) if prior.parent_agent_id.is_some() => prior.parent_agent_id.clone(),
        Some(_) if input.event_name == Some("SubagentAdopted") => input.event_parent_agent_id,
        Some(_) => None,
        None => input.event_parent_agent_id,
    };
    let worktree = worktree_projection(
        input.observation,
        input.prior,
        input.establishes_identity,
        input.event_name,
    );
    let runtime_owner = runtime_projection(input.observation, input.prior, input.agent_id);
    let provider_subagent =
        parent_agent_id.is_some() && input.prior.is_none_or(|prior| prior.launch_depth.is_none());
    let prompt = prompt_projection(
        input.observation,
        input.prior,
        provider_subagent,
        input.event_task,
    );
    state.name = Some(input.card_identity.name);
    state.name_explicit = input.card_identity.name_explicit;
    state.kind_ordinal = Some(input.card_identity.kind_ordinal);
    state.ended_at = ended_at;
    state.status = lifecycle.status;
    state.phase = lifecycle.phase;
    state.pane = pane_projection(input.observation, input.prior);
    state.runtime_owner = runtime_owner;
    state.parent_agent_id = parent_agent_id;
    state.worktree_path = worktree.path;
    state.worktree_branch = worktree.branch;
    state.task = prompt.task;
    state.first_prompt = prompt.first_prompt;
    state.prompt = prompt.prompt;
    state.recent_prompts = prompt.recent_prompts;
    if let Some(description) = &input.observation.description {
        state.description = Some(description.clone());
    }
    if let Some(transcript_path) = &input.observation.transcript_path {
        state.transcript_path = Some(transcript_path.clone());
    }
    if let Some(origin) = input.observation.origin {
        state.origin = Some(origin);
    }
    if let Some(compacted_from) = &input.observation.compacted_from {
        state.compacted_from = Some(compacted_from.clone());
    }
    state.usage = usage;
    state.turn_started_at = lifecycle.turn_started_at;
    state.waiting_since = lifecycle.waiting_since;
    state.open_ask = lifecycle.open_ask;
    state.compacting_since = lifecycle.compacting_since;
    state.compaction_count = lifecycle.compaction_count;
    state.tool_calls = lifecycle.tool_calls;
    state
}

fn inherit_compaction_registration(
    map: &BTreeMap<(AgentKind, AgentSessionId), AgentState>,
    continuation: &mut AgentState,
) {
    let Some(compacted_from) = continuation.compacted_from.as_ref() else {
        return;
    };
    let predecessor_key = (continuation.kind.clone(), compacted_from.clone());
    let Some(predecessor_registered_at) = map
        .get(&predecessor_key)
        .and_then(|predecessor| predecessor.registered_at)
    else {
        return;
    };
    if continuation
        .registered_at
        .is_some_and(|registered_at| predecessor_registered_at < registered_at)
    {
        continuation.registered_at = Some(predecessor_registered_at);
    }
}

fn assemble_launch_state(
    kind: &AgentKind,
    event: &EventEnvelope,
    payload: &AgentLaunchPayload,
    prior: Option<&AgentState>,
    card_identity: CardIdentity,
) -> AgentState {
    let mut state = carried_base(kind, &payload.agent_id, prior, event.timestamp);
    if let Some(launch_id) = &payload.launch_id {
        state.launch_id = Some(launch_id.clone());
    }
    fold_launch_params(&mut state, &payload.launch);
    if let Some(pane_id) = &payload.pane_id {
        state.pane = Some(PaneRef::from_id(pane_id.clone()));
    }
    if let Some(runtime_owner) = &payload.runtime_owner {
        state.runtime_owner = Some(runtime_owner.clone());
    }
    let prompt = payload
        .prompt
        .clone()
        .or_else(|| prior.and_then(|state| state.prompt.clone()));
    if state.first_prompt.is_none()
        && let Some(first_prompt) = payload
            .prompt
            .as_deref()
            .filter(|prompt| usable_description(prompt))
    {
        state.first_prompt = Some(first_prompt.to_owned());
    }
    if let Some(prompt) = payload
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.is_empty())
    {
        append_recent_prompt(&mut state.recent_prompts, prompt);
    }
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
    state.name = Some(card_identity.name);
    state.name_explicit = card_identity.name_explicit;
    state.kind_ordinal = Some(card_identity.kind_ordinal);
    state.task = prompt.clone();
    state.prompt = prompt;
    if let Some(description) = &payload.description {
        state.description = Some(description.clone());
    }
    if let Some(worktree_path) = &payload.worktree_path {
        state.worktree_path = Some(worktree_path.clone());
    }
    if let Some(worktree_branch) = &payload.worktree_branch {
        state.worktree_branch = Some(worktree_branch.clone());
    }
    state.status = status;
    state.phase = phase;
    state.turn_started_at = Some(event.timestamp);
    state
}

struct LifecycleProjection {
    status: AgentStatus,
    phase: lifecycle::TurnPhase,
    compacting_since: Option<Timestamp>,
    compaction_count: u32,
    tool_calls: BTreeMap<String, u32>,
    turn_started_at: Option<Timestamp>,
    waiting_since: Option<Timestamp>,
    open_ask: Option<crate::agents::OpenAsk>,
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
    } = lifecycle::step(
        prev_state.as_ref(),
        prior
            .and_then(|p| p.open_ask.as_ref())
            .and_then(|ask| ask.native_key.as_deref()),
        &signal,
    );
    let compacting_since = if next.compacting {
        Some(timestamp)
    } else {
        None
    };
    let compaction_count = prior.map_or(0, |p| p.compaction_count) + u32::from(compaction_closed);
    let mut tool_calls = prior.map_or_else(BTreeMap::new, |p| p.tool_calls.clone());
    if let lifecycle::LifecycleSignal::ToolUsed {
        name: Some(name), ..
    } = &signal
    {
        let name = name.trim();
        if !name.is_empty() {
            let total = tool_calls.entry(name.to_owned()).or_default();
            *total = total.saturating_add(1);
        }
    }
    // A context reset that rests the agent retires the prior turn's subagents: a
    // manual `/compact` (`CompactionEnded` resting to idle) summarizes away the
    // turn the children belonged to, and a `/clear` (`Registered`) drops it
    // outright. Advancing the subagent boundary here makes a user-typed reset
    // behave like automatic compaction from a rested state, which already opens a
    // turn. The rest gate is load-bearing: automatic compaction *mid-turn* resumes
    // the same turn (stays `running`), so its in-flight children stay listed.
    // Matching the signal — not `compaction_closed` — still fires when the
    // `PreCompact` bracket open was missed. A first-event `Registered` leaves
    // `turn_started_at` unset because the session has never opened a turn; pane
    // recovery reads that absence as first-turn-start eligibility.
    let resets_context = prior.is_some()
        && next.status == AgentStatus::Idle
        && matches!(
            &signal,
            lifecycle::LifecycleSignal::CompactionEnded { .. }
                | lifecycle::LifecycleSignal::Registered
        );
    let turn_started_at = if opened_turn || resets_context {
        Some(timestamp)
    } else {
        prior.and_then(|p| p.turn_started_at)
    };
    let waiting_since = if matches!(&signal, lifecycle::LifecycleSignal::AwaitingInput { .. }) {
        Some(timestamp)
    } else if next.status == AgentStatus::Waiting {
        prior.and_then(|p| p.waiting_since)
    } else {
        None
    };
    let open_ask = match &signal {
        lifecycle::LifecycleSignal::AwaitingInput {
            kind,
            ask_id: Some(id),
            detail,
            native_key,
        } => Some(crate::agents::OpenAsk {
            id: id.clone(),
            kind: *kind,
            detail: detail.clone(),
            native_key: native_key.clone(),
            since: timestamp,
        }),
        lifecycle::LifecycleSignal::AwaitingInput { ask_id: None, .. } => None,
        _ if next.status == AgentStatus::Waiting => prior.and_then(|p| p.open_ask.clone()),
        _ => None,
    };
    LifecycleProjection {
        status: next.status,
        phase: next.phase,
        compacting_since,
        compaction_count,
        tool_calls,
        turn_started_at,
        waiting_since,
        open_ask,
    }
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

fn runtime_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
    agent_id: &AgentSessionId,
) -> Option<RuntimeOwner> {
    let event_owner = observation.runtime_owner.clone().or_else(|| {
        observation.agent_pid.map(|pid| {
            RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                agent_id.to_string(),
                pid,
                observation.agent_process_start.clone(),
            )
        })
    });
    let prior_owner = prior.and_then(|p| p.runtime_owner.clone());
    match (event_owner, prior_owner) {
        (Some(event), Some(prior))
            if event.kind == RuntimeOwnerKind::Daemon
                && prior.kind == RuntimeOwnerKind::Agent
                && prior.subject_id == agent_id.as_str() =>
        {
            Some(prior)
        }
        (Some(event), _) => Some(event),
        (None, prior) => prior,
    }
}

struct PromptProjection {
    task: Option<String>,
    first_prompt: Option<String>,
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
    let first_prompt = prior
        .and_then(|state| state.first_prompt.clone())
        .or_else(|| {
            event_prompt
                .as_deref()
                .filter(|prompt| usable_description(prompt))
                .map(ToOwned::to_owned)
        });
    let mut recent_prompts = prior.map(|p| p.recent_prompts.clone()).unwrap_or_default();
    if let Some(prompt) = event_prompt.as_deref().filter(|prompt| !prompt.is_empty()) {
        append_recent_prompt(&mut recent_prompts, prompt);
    }
    PromptProjection {
        task,
        first_prompt,
        prompt: event_prompt.or_else(|| prior.and_then(|p| p.prompt.clone())),
        recent_prompts,
    }
}

fn pane_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
) -> Option<PaneRef> {
    let observation_pane = observation
        .pane_stamp
        .clone()
        .or_else(|| observation.pane_id.clone().map(PaneRef::from_id));
    match (observation_pane, prior.and_then(|p| p.pane.clone())) {
        (Some(observed), Some(prior))
            if observed.pane_id == prior.pane_id && !pane_stamp_is_enriched(&observed) =>
        {
            Some(prior)
        }
        (Some(observed), _) => Some(observed),
        (None, prior) => prior,
    }
}

fn pane_stamp_is_enriched(pane: &PaneRef) -> bool {
    pane.pane_pid.is_some()
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests;
