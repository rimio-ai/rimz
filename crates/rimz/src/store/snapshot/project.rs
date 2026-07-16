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
    AgentLaunchPayload, AgentLaunchState, AgentLifecyclePayload, EventEnvelope, EventKind,
    MessageEventPayload,
};

use super::row::derive_percent;

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
    let state = assemble_agent_state(AgentStateInput {
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
    map.insert(key, state);
}

fn quarantine_reason(
    signal: &lifecycle::LifecycleSignal,
    prior: Option<&AgentState>,
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
        | lifecycle::LifecycleSignal::CompactionEnded { .. } => {
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
    if map
        .get(key)
        .is_some_and(|state| state.parent_agent_id.is_some())
    {
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
    AgentState {
        agent_id: agent_id.clone(),
        kind: kind.clone(),
        name: None,
        name_explicit: false,
        kind_ordinal: None,
        profile: prior.and_then(|state| state.profile.clone()),
        mode: prior.and_then(|state| state.mode),
        role: prior.and_then(|state| state.role.clone()),
        team: prior.and_then(|state| state.team.clone()),
        launch_group: prior.and_then(|state| state.launch_group.clone()),
        launch_ordinal: prior.and_then(|state| state.launch_ordinal),
        channel: prior.and_then(|state| state.channel.clone()),
        ended_at: None,
        status: AgentStatus::Idle,
        phase: lifecycle::TurnPhase::Idle,
        pane: prior.and_then(|state| state.pane.clone()),
        runtime_owner: prior.and_then(|state| state.runtime_owner.clone()),
        parent_agent_id: prior.and_then(|state| state.parent_agent_id.clone()),
        worktree_path: prior.and_then(|state| state.worktree_path.clone()),
        worktree_branch: prior.and_then(|state| state.worktree_branch.clone()),
        task: prior.and_then(|state| state.task.clone()),
        first_prompt: prior.and_then(|state| state.first_prompt.clone()),
        prompt: prior.and_then(|state| state.prompt.clone()),
        description: prior.and_then(|state| state.description.clone()),
        transcript_path: prior.and_then(|state| state.transcript_path.clone()),
        origin: prior.and_then(|state| state.origin),
        recent_prompts: prior
            .map(|state| state.recent_prompts.clone())
            .unwrap_or_default(),
        model: prior.and_then(|state| state.model.clone()),
        effort: prior.and_then(|state| state.effort.clone()),
        budget: prior.and_then(|state| state.budget.clone()),
        context_pct: prior.and_then(|state| state.context_pct),
        context_window: prior.and_then(|state| state.context_window),
        total_tokens: prior.and_then(|state| state.total_tokens),
        cache_read_input_tokens: prior.and_then(|state| state.cache_read_input_tokens),
        cache_write_input_tokens: prior.and_then(|state| state.cache_write_input_tokens),
        fresh_input_tokens: prior.and_then(|state| state.fresh_input_tokens),
        output_tokens: prior.and_then(|state| state.output_tokens),
        context: None,
        budget_park: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        waiting_since: None,
        open_ask: None,
        compacting_since: None,
        compaction_count: prior.map_or(0, |state| state.compaction_count),
        last_compact_command_tokens: prior.and_then(|state| state.last_compact_command_tokens),
        last_seen: event_ts,
        last_activity: event_ts,
        registered_at: prior
            .and_then(|state| state.registered_at)
            .or(Some(event_ts)),
    }
}

fn fold_launch_params(state: &mut AgentState, launch: &LaunchParams) {
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
    let enrichment = enrichment_projection(input.observation, input.prior, input.kind);
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
    let prompt = prompt_projection(
        input.observation,
        input.prior,
        parent_agent_id.is_some(),
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
    state.context_pct = enrichment.context_pct;
    state.context_window = enrichment.context_window;
    state.total_tokens = enrichment.total_tokens;
    state.cache_read_input_tokens = enrichment.cache_read_input_tokens;
    state.cache_write_input_tokens = enrichment.cache_write_input_tokens;
    state.fresh_input_tokens = enrichment.fresh_input_tokens;
    state.output_tokens = enrichment.output_tokens;
    state.turn_started_at = lifecycle.turn_started_at;
    state.waiting_since = lifecycle.waiting_since;
    state.open_ask = lifecycle.open_ask;
    state.compacting_since = lifecycle.compacting_since;
    state.compaction_count = lifecycle.compaction_count;
    state
}

fn assemble_launch_state(
    kind: &AgentKind,
    event: &EventEnvelope,
    payload: &AgentLaunchPayload,
    prior: Option<&AgentState>,
    card_identity: CardIdentity,
) -> AgentState {
    let mut state = carried_base(kind, &payload.agent_id, prior, event.timestamp);
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
    state.parent_agent_id = None;
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
        turn_started_at,
        waiting_since,
        open_ask,
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
    derive_percent(used, window)
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
