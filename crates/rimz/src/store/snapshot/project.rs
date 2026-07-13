//! The agent-lifecycle reducer: folds `agent.lifecycle` events into
//! [`AgentState`] rollups, carrying turn, phase, subagent, and model
//! state forward.

use std::collections::BTreeMap;

use jiff::Timestamp;
use tracing::debug;

use crate::agents::AgentLifecycleObservation;
use crate::agents::lifecycle::{self, Transition};
use crate::agents::{AgentState, AgentStatus};
use crate::ids::{AgentKind, AgentSessionId};
use crate::message::{MessageBody, MessageStatus};
use crate::pane::{PaneRef, RuntimeOwner, RuntimeOwnerKind};
use crate::store::event::{
    AgentLaunchPayload, AgentLaunchState, EventEnvelope, EventKind, MessageEventPayload,
};

mod identity;

pub(crate) use identity::AgentIdentityState;
pub(super) use identity::backfill_agent_identities;
use identity::{CardIdentity, CardIdentityAllocator, usable_name};

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
        // A mux rebirth renumbers panes from zero, so every stamp recorded
        // before the boundary names a pane that no longer exists — and the
        // reborn session reuses those ids for new panes. Clear them all here,
        // in log order, so a prior incarnation's session can never bind (or
        // block recovery of) a reused pane id; a stamp recorded by a later
        // event is the new incarnation's and stays. Sessions themselves are
        // kept — the boundary unstamps, it never tombstones.
        let payload = match &event.kind {
            EventKind::SessionRebirth => {
                for state in map.values_mut() {
                    state.pane = None;
                    state.kind_ordinal = None;
                }
                identity.reset_ordinals();
                continue;
            }
            EventKind::AgentLaunch(payload) => {
                let kind = AgentKind::new_unchecked(envelope.source.clone());
                reduce_agent_launch(&mut map, &mut identity, envelope, &kind, payload);
                continue;
            }
            EventKind::AgentLifecycle(payload) => payload,
            EventKind::Message { payload, .. } => {
                stamp_compact_command(map.values_mut(), payload);
                continue;
            }
            EventKind::SessionDeath(_) => continue,
            EventKind::Other {
                method: "agent.lifecycle",
                ..
            } => {
                debug!(
                    target: "rimz::agent::lifecycle",
                    event_id = %envelope.event_id,
                    "non-conforming agent.lifecycle event ignored",
                );
                continue;
            }
            EventKind::Other { .. } => continue,
        };
        let kind = AgentKind::new_unchecked(envelope.source.clone());
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
                event_id = %envelope.event_id,
                workspace = %envelope.workspace_id,
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
        if matches!(&signal, lifecycle::LifecycleSignal::Ended) {
            identity.release_key(&key);
            map.remove(&key);
            continue;
        }
        let prior = map.get(&key).or(provisional_prior.as_ref());
        if matches!(&signal, lifecycle::LifecycleSignal::Lost) && prior.is_none() {
            debug!(
                target: "rimz::agent::lifecycle",
                event_id = %envelope.event_id,
                workspace = %envelope.workspace_id,
                kind = %kind,
                agent_id = %agent_id,
                "lost marker for unknown session ignored by agent-state reducer",
            );
            continue;
        }
        if matches!(
            &signal,
            lifecycle::LifecycleSignal::Compacting
                | lifecycle::LifecycleSignal::CompactionEnded { .. }
        ) && prior.is_none()
        {
            debug!(
                target: "rimz::agent::lifecycle",
                event_id = %envelope.event_id,
                workspace = %envelope.workspace_id,
                kind = %kind,
                agent_id = %agent_id,
                "compaction signal for unknown session ignored by agent-state reducer",
            );
            continue;
        }
        if matches!(&signal, lifecycle::LifecycleSignal::SubagentStopped { .. })
            && prior.is_none()
            && event_parent_agent_id.is_some()
            && event_task.is_none()
        {
            debug!(
                target: "rimz::agent::lifecycle",
                event_id = %envelope.event_id,
                workspace = %envelope.workspace_id,
                session = %envelope.session_name,
                kind = %kind,
                source_kind = %envelope.source_kind,
                timestamp = %envelope.timestamp,
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
        let card_identity = identity.assign(&kind, &agent_id, observation, prior);
        let state = assemble_agent_state(AgentStateInput {
            kind: &kind,
            agent_id: &agent_id,
            event: envelope,
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
    (map, identity.state())
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

struct CarriedFields {
    profile: Option<String>,
    mode: Option<crate::harness::run::PermissionMode>,
    role: Option<String>,
    team: Option<String>,
    launch_group: Option<String>,
    launch_ordinal: Option<u32>,
    channel: Option<String>,
    description: Option<String>,
    transcript_path: Option<String>,
    origin: Option<crate::agents::SessionOrigin>,
    recent_prompts: Vec<String>,
    model: Option<String>,
    effort: Option<String>,
    budget: Option<String>,
    context_pct: Option<u8>,
    context_window: Option<u64>,
    total_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_write_input_tokens: Option<u64>,
    fresh_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    compaction_count: u32,
    last_compact_command_tokens: Option<u64>,
    registered_at: Option<Timestamp>,
}

/// The carried baseline: every carry-forward and identity field cloned from
/// `prior`, activity fields cleared, enrichment sidecars left for projection.
/// This is the lifetime table's code home; see docs/internals/agents/model.md
/// § The rollup.
fn carried_state(prior: Option<&AgentState>) -> CarriedFields {
    CarriedFields {
        profile: prior.and_then(|state| state.profile.clone()),
        mode: prior.and_then(|state| state.mode),
        role: prior.and_then(|state| state.role.clone()),
        team: prior.and_then(|state| state.team.clone()),
        launch_group: prior.and_then(|state| state.launch_group.clone()),
        launch_ordinal: prior.and_then(|state| state.launch_ordinal),
        channel: prior.and_then(|state| state.channel.clone()),
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
        compaction_count: prior.map_or(0, |state| state.compaction_count),
        last_compact_command_tokens: prior.and_then(|state| state.last_compact_command_tokens),
        registered_at: prior.and_then(|state| state.registered_at),
    }
}

fn assemble_agent_state(input: AgentStateInput<'_>) -> AgentState {
    let carried = carried_state(input.prior);
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
        name_explicit: input.card_identity.name_explicit,
        kind_ordinal: Some(input.card_identity.kind_ordinal),
        profile: input.observation.launch.profile.clone().or(carried.profile),
        mode: input.observation.launch.mode.or(carried.mode),
        role: input.observation.launch.role.clone().or(carried.role),
        team: input.observation.launch.team.clone().or(carried.team),
        launch_group: input
            .observation
            .launch
            .launch_group
            .clone()
            .or(carried.launch_group),
        launch_ordinal: input
            .observation
            .launch
            .launch_ordinal
            .or(carried.launch_ordinal),
        channel: input.observation.launch.channel.clone().or(carried.channel),
        status: lifecycle.status,
        phase: lifecycle.phase,
        pane: pane_projection(input.observation, input.prior),
        runtime_owner: runtime.runtime_owner,
        parent_agent_id,
        worktree_path: worktree.path,
        worktree_branch: worktree.branch,
        task: prompt.task,
        prompt: prompt.prompt,
        description: carried.description,
        transcript_path: transcript_path_projection(input.observation, input.prior),
        origin: origin_projection(input.observation, input.prior),
        recent_prompts: prompt.recent_prompts,
        model: model_projection(input.observation, input.prior),
        effort: effort_projection(input.observation, input.prior),
        budget: input.observation.launch.budget.clone().or(carried.budget),
        context_pct: enrichment.context_pct,
        context_window: enrichment.context_window,
        total_tokens: enrichment.total_tokens,
        cache_read_input_tokens: enrichment.cache_read_input_tokens,
        cache_write_input_tokens: enrichment.cache_write_input_tokens,
        fresh_input_tokens: enrichment.fresh_input_tokens,
        output_tokens: enrichment.output_tokens,
        context: None,
        budget_park: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: lifecycle.turn_started_at,
        waiting_since: lifecycle.waiting_since,
        open_ask: lifecycle.open_ask,
        compacting_since: lifecycle.compacting_since,
        compaction_count: lifecycle.compaction_count,
        last_compact_command_tokens: carried.last_compact_command_tokens,
        last_seen: input.event.timestamp,
        last_activity: input.event.timestamp,
        registered_at: lifecycle.registered_at,
    }
}

fn assemble_launch_state(
    kind: &AgentKind,
    event: &EventEnvelope,
    payload: &AgentLaunchPayload,
    prior: Option<&AgentState>,
    card_identity: CardIdentity,
) -> AgentState {
    let carried = carried_state(prior);
    let pane = payload
        .pane_id
        .clone()
        .map(PaneRef::from_id)
        .or_else(|| prior.and_then(|state| state.pane.clone()));
    let runtime_owner = payload
        .runtime_owner
        .clone()
        .or_else(|| prior.and_then(|state| state.runtime_owner.clone()));
    let prompt = payload
        .prompt
        .clone()
        .or_else(|| prior.and_then(|state| state.prompt.clone()));
    let description = payload.description.clone().or(carried.description);
    let mut recent_prompts = carried.recent_prompts;
    if let Some(prompt) = payload
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.is_empty())
    {
        append_recent_prompt(&mut recent_prompts, prompt);
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
    AgentState {
        agent_id: payload.agent_id.clone(),
        kind: kind.clone(),
        name: Some(card_identity.name),
        name_explicit: card_identity.name_explicit,
        kind_ordinal: Some(card_identity.kind_ordinal),
        profile: payload.launch.profile.clone().or(carried.profile),
        mode: payload.launch.mode.or(carried.mode),
        role: payload.launch.role.clone().or(carried.role),
        team: payload.launch.team.clone().or(carried.team),
        launch_group: payload.launch.launch_group.clone().or(carried.launch_group),
        launch_ordinal: payload.launch.launch_ordinal.or(carried.launch_ordinal),
        channel: payload.launch.channel.clone().or(carried.channel),
        status,
        phase,
        pane,
        runtime_owner,
        parent_agent_id: None,
        worktree_path: payload
            .worktree_path
            .clone()
            .or_else(|| prior.and_then(|state| state.worktree_path.clone())),
        worktree_branch: payload
            .worktree_branch
            .clone()
            .or_else(|| prior.and_then(|state| state.worktree_branch.clone())),
        task: prompt.clone(),
        prompt,
        description,
        transcript_path: carried.transcript_path,
        origin: carried.origin,
        recent_prompts,
        model: payload
            .launch
            .model
            .as_deref()
            .map(canonical_model)
            .or(carried.model),
        effort: payload.launch.effort.clone().or(carried.effort),
        budget: payload.launch.budget.clone().or(carried.budget),
        context_pct: carried.context_pct,
        context_window: carried.context_window,
        total_tokens: carried.total_tokens,
        cache_read_input_tokens: carried.cache_read_input_tokens,
        cache_write_input_tokens: carried.cache_write_input_tokens,
        fresh_input_tokens: carried.fresh_input_tokens,
        output_tokens: carried.output_tokens,
        context: None,
        budget_park: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: Some(event.timestamp),
        waiting_since: None,
        open_ask: None,
        compacting_since: None,
        compaction_count: carried.compaction_count,
        last_compact_command_tokens: carried.last_compact_command_tokens,
        last_seen: event.timestamp,
        last_activity: event.timestamp,
        registered_at: carried.registered_at.or(Some(event.timestamp)),
    }
}

struct LifecycleProjection {
    status: AgentStatus,
    phase: lifecycle::TurnPhase,
    compacting_since: Option<Timestamp>,
    compaction_count: u32,
    turn_started_at: Option<Timestamp>,
    waiting_since: Option<Timestamp>,
    open_ask: Option<crate::agents::OpenAsk>,
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
        } => Some(crate::agents::OpenAsk {
            id: id.clone(),
            kind: *kind,
            detail: detail.clone(),
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
    runtime_owner: Option<RuntimeOwner>,
}

fn runtime_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
    agent_id: &AgentSessionId,
) -> RuntimeProjection {
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
    let runtime_owner = match (event_owner, prior_owner) {
        (Some(event), Some(prior))
            if event.kind == RuntimeOwnerKind::Daemon
                && prior.kind == RuntimeOwnerKind::Agent
                && prior.subject_id == agent_id.as_str() =>
        {
            Some(prior)
        }
        (Some(event), _) => Some(event),
        (None, prior) => prior,
    };
    RuntimeProjection { runtime_owner }
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
        append_recent_prompt(&mut recent_prompts, prompt);
    }
    PromptProjection {
        task,
        prompt: event_prompt.or_else(|| prior.and_then(|p| p.prompt.clone())),
        recent_prompts,
    }
}

fn append_recent_prompt(recent_prompts: &mut Vec<String>, prompt: &str) {
    recent_prompts.push(prompt.to_owned());
    let excess = recent_prompts.len().saturating_sub(RECENT_PROMPTS_LIMIT);
    if excess > 0 {
        recent_prompts.drain(0..excess);
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
) -> Option<crate::agents::SessionOrigin> {
    observation.origin.or_else(|| prior.and_then(|p| p.origin))
}

fn model_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
) -> Option<String> {
    observation
        .launch
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
        .launch
        .effort
        .clone()
        .or_else(|| prior.and_then(|p| p.effort.clone()))
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
