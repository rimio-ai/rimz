//! The agent-lifecycle reducer: folds `agent.lifecycle` events into
//! [`AgentState`] rollups, carrying turn, phase, subagent, and model
//! state forward.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use jiff::Timestamp;
use tracing::debug;

use crate::agents::lifecycle::{self, Transition};
use crate::agents::petname::valid_agent_name;
use crate::agents::state::{OwedContinue, SoftCompactMark, TransitionMode};
use crate::agents::{AgentLifecycleObservation, AgentUsageSummary, LaunchParams, SessionOrigin};
use crate::agents::{AgentState, AgentStatus, KeptVerdict};
use crate::ids::{AgentKind, AgentSessionId};
use crate::pane::{PaneRef, RuntimeOwner, RuntimeOwnerKind};
use crate::store::event::{
    self, AgentAttachPayload, AgentLaunchPayload, AgentLaunchState, AgentLifecyclePayload,
    EventEnvelope, EventKind, MessageEventMethod, MessageEventPayload,
};
use crate::store::message::{MessageBody, MessageStatus, SoftCompactStep};

mod identity;

pub(crate) use identity::AgentIdentityState;
pub(super) use identity::backfill_agent_identities;
use identity::{CardIdentity, CardIdentityAllocator};

/// The launch and epoch a lifecycle event's process ran under, from the
/// observation's captured launch id. The fold's fences read it alone: it
/// never assigns a row's launch identity.
#[derive(Clone, Copy)]
struct EventStamp<'a> {
    launch: &'a AgentSessionId,
    epoch: u32,
}

impl<'a> EventStamp<'a> {
    fn of(observation: &'a AgentLifecycleObservation) -> Option<Self> {
        observation.launch_id.as_ref().map(|launch| Self {
            launch,
            epoch: observation.launch.epoch.unwrap_or(0),
        })
    }

    /// A row of this launch that a later swap moved past: the event is stale.
    fn stale_on(self, row: &AgentState) -> bool {
        self.stale_at(row.launch_id.as_ref(), row.epoch)
    }

    /// A row of `launch` at `epoch` that a later swap of this event's launch
    /// moved past.
    fn stale_at(self, launch: Option<&AgentSessionId>, epoch: u32) -> bool {
        launch == Some(self.launch) && epoch > self.epoch
    }
}

/// Whether the fold keeps `observation` without effect on `row`: a root event
/// stamped with an epoch a later swap of `row`'s launch moved past.
pub(in crate::store) fn observation_stale_on(
    observation: &AgentLifecycleObservation,
    row: &AgentState,
) -> bool {
    observation_stale_at(observation, row.launch_id.as_ref(), row.epoch)
}

/// [`observation_stale_on`] for a row known only by its `launch` and
/// `epoch`, as the lifecycle follower tracks it.
pub(in crate::store) fn observation_stale_at(
    observation: &AgentLifecycleObservation,
    launch: Option<&AgentSessionId>,
    epoch: u32,
) -> bool {
    observation.parent_agent_id.is_none()
        && EventStamp::of(observation).is_some_and(|stamp| stamp.stale_at(launch, epoch))
}

/// Whether an event may adopt or release `provisional`: a stamped event only
/// the reservation of its own launch and epoch, an unstamped one only an
/// epoch-0 launch.
fn stamp_takes(stamp: Option<EventStamp<'_>>, provisional: &AgentState) -> bool {
    match stamp {
        Some(stamp) => {
            provisional.launch_id.as_ref() == Some(stamp.launch) && provisional.epoch == stamp.epoch
        }
        None => provisional.epoch == 0,
    }
}

type AgentKey = (AgentKind, AgentSessionId);
type LaunchInstanceKey = (AgentKind, crate::ids::PaneId, u32);

pub(super) fn agent_mut(agent: &mut Arc<AgentState>) -> &mut AgentState {
    if Arc::strong_count(agent) > 1 {
        super::fold::testkit::count_row_copies(1);
    }
    Arc::make_mut(agent)
}

#[derive(Default)]
struct LaunchIdentityIndex {
    by_instance: HashMap<LaunchInstanceKey, BTreeSet<AgentKey>>,
    by_agent: HashMap<AgentKey, LaunchInstanceKey>,
}

impl LaunchIdentityIndex {
    fn from_map(map: &BTreeMap<AgentKey, Arc<AgentState>>) -> Self {
        let mut index = Self::default();
        for (key, state) in map {
            index.replace(key, state);
        }
        index
    }

    fn replace(&mut self, key: &AgentKey, state: &AgentState) {
        self.remove(key);
        if state.launch_id.is_none() || state.is_provider_subagent() {
            return;
        }
        let Some(instance) = launch_instance_key(state) else {
            return;
        };
        self.by_instance
            .entry(instance.clone())
            .or_default()
            .insert(key.clone());
        self.by_agent.insert(key.clone(), instance);
    }

    fn remove(&mut self, key: &AgentKey) {
        let Some(instance) = self.by_agent.remove(key) else {
            return;
        };
        let Some(keys) = self.by_instance.get_mut(&instance) else {
            return;
        };
        keys.remove(key);
        if keys.is_empty() {
            self.by_instance.remove(&instance);
        }
    }

    fn predecessors<'a>(
        &self,
        map: &'a BTreeMap<AgentKey, Arc<AgentState>>,
        successor: &AgentState,
    ) -> Option<(&'a AgentState, &'a AgentState)> {
        let instance = launch_instance_key(successor)?;
        self.by_instance
            .get(&instance)?
            .iter()
            .filter_map(|key| {
                #[cfg(test)]
                launch_identity_testkit::record_candidate_inspection();
                map.get(key)
            })
            .filter(|candidate| {
                candidate.kind == successor.kind
                    && candidate.agent_id != successor.agent_id
                    && candidate.launch_id.is_some()
                    && !candidate.is_provider_subagent()
                    && crate::store::session_death::same_agent_instance(candidate, successor)
            })
            .map(Arc::as_ref)
            .fold(None, |predecessors, candidate| {
                let Some((first, latest)) = predecessors else {
                    return Some((candidate, candidate));
                };
                Some((
                    std::cmp::min_by(first, candidate, |a, b| {
                        (a.registered_at.is_none(), a.registered_at, &a.agent_id).cmp(&(
                            b.registered_at.is_none(),
                            b.registered_at,
                            &b.agent_id,
                        ))
                    }),
                    std::cmp::max_by(latest, candidate, |a, b| {
                        (a.registered_at, &a.agent_id).cmp(&(b.registered_at, &b.agent_id))
                    }),
                ))
            })
    }
}

fn launch_instance_key(state: &AgentState) -> Option<LaunchInstanceKey> {
    let pane_id = state.pane.as_ref()?.pane_id.clone();
    let owner = state.runtime_owner.as_ref()?;
    (owner.kind == RuntimeOwnerKind::Agent).then(|| (state.kind.clone(), pane_id, owner.pid))
}

#[cfg(test)]
mod launch_identity_testkit {
    use std::cell::Cell;

    thread_local! {
        static CANDIDATE_INSPECTIONS: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn record_candidate_inspection() {
        CANDIDATE_INSPECTIONS.set(CANDIDATE_INSPECTIONS.get() + 1);
    }

    pub(super) fn reset_candidate_inspections() {
        CANDIDATE_INSPECTIONS.set(0);
    }

    pub(super) fn candidate_inspections() -> usize {
        CANDIDATE_INSPECTIONS.get()
    }
}

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
    .map(Arc::unwrap_or_clone)
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
/// reuse it. Sessions stay alive across the boundary; pane, on-screen and
/// ordinal stamps are retired.
pub(super) fn unstamp_for_rebirth<'a>(agents: impl IntoIterator<Item = &'a mut Arc<AgentState>>) {
    for agent in agents {
        if agent.pane.is_none() && agent.on_screen_at.is_none() && agent.kind_ordinal.is_none() {
            continue;
        }
        let agent = agent_mut(agent);
        agent.pane = None;
        agent.on_screen_at = None;
        agent.kind_ordinal = None;
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
    let seed = seed
        .into_iter()
        .map(|(key, row)| (key, Arc::new(row)))
        .collect();
    reduce_agent_states_seeded_with_identity(seed, AgentIdentityState::default(), &events)
        .0
        .into_iter()
        .map(|(key, row)| (key, Arc::unwrap_or_clone(row)))
        .collect()
}

/// The agent key for every event kind that needs its carried row hydrated
/// before reduction, whether the event creates the row or only updates it.
pub(super) fn agent_event_key(event: &FoldEvent<'_>) -> Option<(AgentKind, AgentSessionId)> {
    let agent_id = match &event.kind {
        EventKind::AgentLifecycle(payload) => payload.observation.agent_id.clone()?,
        EventKind::AgentLaunch(payload) => payload.agent_id.clone(),
        EventKind::AgentAttach(payload) => payload.agent_id.clone(),
        EventKind::AgentLaunchWarnings(payload) => payload.agent_id.clone(),
        EventKind::AgentWorkflowCleared(payload) => payload.agent_id.clone(),
        EventKind::AgentTune(payload) => payload.agent_id.clone(),
        EventKind::AgentSoftCompactClaimed(claim) => claim.agent_id.clone(),
        _ => return None,
    };
    Some((
        AgentKind::new_unchecked(event.envelope.source.clone()),
        agent_id,
    ))
}

struct ReducerState {
    map: BTreeMap<AgentKey, Arc<AgentState>>,
    identity: CardIdentityAllocator,
    launch_identity: LaunchIdentityIndex,
}

pub(super) fn reduce_agent_states_seeded_with_identity(
    seed: BTreeMap<(AgentKind, AgentSessionId), Arc<AgentState>>,
    identity_state: AgentIdentityState,
    events: &[FoldEvent<'_>],
) -> (
    BTreeMap<(AgentKind, AgentSessionId), Arc<AgentState>>,
    AgentIdentityState,
) {
    let mut state = ReducerState {
        identity: CardIdentityAllocator::from_map_and_state(&seed, identity_state),
        launch_identity: LaunchIdentityIndex::from_map(&seed),
        map: seed,
    };
    for event in events {
        let envelope = event.envelope;
        match &event.kind {
            EventKind::SessionRebirth => {
                unstamp_for_rebirth(state.map.values_mut());
                state.identity.reset_ordinals();
                state.launch_identity = LaunchIdentityIndex::from_map(&state.map);
            }
            EventKind::AgentLaunch(payload) => {
                let kind = AgentKind::new_unchecked(envelope.source.clone());
                state.reduce_agent_launch(envelope, &kind, payload);
            }
            EventKind::AgentAttach(payload) => {
                let kind = AgentKind::new_unchecked(envelope.source.clone());
                state.reduce_agent_attach(envelope, &kind, payload);
            }
            EventKind::AgentLaunchWarnings(payload) => {
                let key = (
                    AgentKind::new_unchecked(envelope.source.clone()),
                    payload.agent_id.clone(),
                );
                let Some(agent) = state.map.get_mut(&key) else {
                    debug!(
                        target: "rimz::agent::binding",
                        kind = %key.0,
                        agent_id = %key.1,
                        "agent.launch_warnings event for unknown session ignored",
                    );
                    continue;
                };
                let agent = agent_mut(agent);
                agent.launch_warnings.clone_from(&payload.warnings);
                state.launch_identity.replace(&key, agent);
            }
            EventKind::AgentTune(payload) => {
                let key = (
                    AgentKind::new_unchecked(envelope.source.clone()),
                    payload.agent_id.clone(),
                );
                let Some(agent) = state.map.get_mut(&key) else {
                    continue;
                };
                let _ = agent_mut(agent).tune.apply(
                    payload.setting,
                    payload.value.as_deref(),
                    envelope.timestamp,
                    payload.by.as_deref(),
                );
            }
            EventKind::AgentSoftCompactClaimed(claim) => {
                let key = (
                    AgentKind::new_unchecked(envelope.source.clone()),
                    claim.agent_id.clone(),
                );
                if let Some(agent) = state.map.get_mut(&key) {
                    stamp_reminder(agent_mut(agent), claim.compactions, Some(claim.rung));
                }
            }
            EventKind::AgentLifecycle(payload) => {
                state.reduce_lifecycle_event(envelope, payload);
            }
            EventKind::AgentWorkflowCleared(payload) => {
                let key = (
                    AgentKind::new_unchecked(envelope.source.clone()),
                    payload.agent_id.clone(),
                );
                if let Some(agent) = state.map.get_mut(&key)
                    && agent.workflow.as_ref() == Some(&payload.workflow)
                {
                    agent_mut(agent).workflow = None;
                }
            }
            EventKind::Message { method, payload } => {
                stamp_message(&mut state.map, *method, payload, envelope.timestamp);
            }
            EventKind::SessionDeath(_) => {}
            EventKind::Signal(_) => {}
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
    (state.map, state.identity.state())
}

impl ReducerState {
    fn insert(&mut self, key: AgentKey, state: AgentState) {
        self.launch_identity.replace(&key, &state);
        self.map.insert(key, Arc::new(state));
    }

    fn reduce_agent_attach(
        &mut self,
        event: &EventEnvelope,
        kind: &AgentKind,
        payload: &AgentAttachPayload,
    ) {
        let key = (kind.clone(), payload.agent_id.clone());
        if !self.map.contains_key(&key) && payload.launch_id.is_none() {
            debug!(
                target: "rimz::agent::binding",
                kind = %kind,
                agent_id = %payload.agent_id,
                pane_id = %payload.pane_id,
                "legacy agent.attached event for unknown session ignored",
            );
            return;
        }
        if let (Some(epoch), Some(row)) = (payload.epoch, self.map.get(&key))
            && row.launch_id == payload.launch_id
            && row.epoch > epoch
        {
            debug!(
                target: "rimz::agent::binding",
                kind = %kind,
                agent_id = %payload.agent_id,
                epoch,
                "stale agent.attached event from a replaced wrapper ignored",
            );
            return;
        }
        // A discovered provider session may have no prior RimZ event. The resume
        // wrapper knows enough durable identity to seed it before the provider
        // process starts, so the process can launch children immediately.
        self.map.entry(key.clone()).or_insert_with(|| {
            Arc::new(AgentState::seed(
                kind.clone(),
                payload.agent_id.clone(),
                AgentStatus::Idle,
                event.timestamp,
            ))
        });
        // A resume wrapper's attach at a swap's epoch makes this session the
        // reservation's: the session row takes it here, before its stamped
        // hooks arrive at an epoch the row already holds.
        if !payload.agent_id.is_provisional()
            && let (Some(launch), Some(epoch)) = (&payload.launch_id, payload.epoch)
            && let Some(provisional_key) =
                self.stamped_provisional(kind, EventStamp { launch, epoch })
        {
            self.absorb_provisional(&key, &provisional_key);
        }
        let Some(state) = self.map.get_mut(&key) else {
            return;
        };
        let state = agent_mut(state);
        state.launch_bound |= state.agent_id.is_provisional();
        if let Some(launch_id) = &payload.launch_id {
            state.set_launch_id(launch_id.clone());
        }
        if let Some(epoch) = payload.epoch {
            state.epoch = state.epoch.max(epoch);
        }
        if let Some(login) = &payload.login {
            state.login = (!login.is_default()).then(|| login.clone());
        }
        if let Some(isolation) = payload.isolation {
            state.isolation = Some(isolation);
        }
        if let Some(isolation) = payload.effective_isolation {
            state.effective_isolation = Some(isolation);
        }
        if payload.record.is_some() {
            state.record = payload.record.clone().map(Box::new);
            state.tier = payload.tier.clone().map(Box::new);
        }
        if payload.record.is_some() || payload.mode.is_some() {
            state.mode = payload.mode;
        }
        let prior_pane = state.pane.replace(PaneRef {
            pane_pid: payload.pane_pid,
            ..PaneRef::from_id(payload.pane_id.clone())
        });
        let prior_owner = state.runtime_owner.replace(payload.runtime_owner.clone());
        if !crate::store::session_death::same_instance_placement(
            (prior_pane.as_ref(), prior_owner.as_ref()),
            (state.pane.as_ref(), state.runtime_owner.as_ref()),
        ) {
            state.on_screen_at = None;
        }
        self.launch_identity.replace(&key, state);
    }

    fn reduce_lifecycle_event(&mut self, event: &EventEnvelope, payload: &AgentLifecyclePayload) {
        let kind = AgentKind::new_unchecked(event.source.clone());
        // The agent-agnostic lifecycle intent this event carries. The status
        // and the phase/compacting heads are all derived from it through the
        // shared `AgentState::transition` decision — never taken verbatim — so an
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
        if observation.origin == Some(SessionOrigin::SideConversation) {
            self.identity.mark_side_session(agent_id.clone());
            self.map.remove(&key);
            self.launch_identity.remove(&key);
            self.identity.release_key(&key);
        }
        if self.identity.is_side_session(&agent_id) {
            debug!(
                target: "rimz::agent::lifecycle",
                event_id = %event.event_id,
                workspace = %event.workspace_id,
                kind = %kind,
                agent_id = %agent_id,
                reason = "side conversation",
                "agent.lifecycle event quarantined",
            );
            return;
        }
        let event_is_child = observation.parent_agent_id.is_some();
        let stamp = (!event_is_child)
            .then(|| EventStamp::of(observation))
            .flatten();
        if let Some(stamp) = stamp
            && self.map.get(&key).is_some_and(|row| stamp.stale_on(row))
        {
            debug!(
                target: "rimz::agent::lifecycle",
                event_id = %event.event_id,
                kind = %kind,
                agent_id = %agent_id,
                epoch = stamp.epoch,
                "stale agent.lifecycle event from a replaced process kept without effect",
            );
            return;
        }
        let provisional_prior = if event_is_child {
            // Exact child IDs are authoritative. A child may share its type label
            // and pane with siblings or its parent, so neither is an adoption or
            // provisional-release key.
            None
        } else if self.map.contains_key(&key) {
            self.release_stamped_provisional_for_existing(&kind, &key, observation, stamp);
            None
        } else {
            self.adopt_provisional(&kind, &key, observation, stamp)
        };
        let event_name = payload.event_name.as_deref();
        let event_parent_agent_id =
            non_empty_string(observation.parent_agent_id.as_deref()).map(AgentSessionId::from);
        let event_task = non_empty_string(observation.task.as_deref());
        let prior = self
            .map
            .get(&key)
            .or(provisional_prior.as_ref())
            .map(Arc::as_ref);
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
        let adopting_root = adopts_root(event_name, prior.map(|prior| &prior.parent_agent_id));
        let card_identity = self.identity.assign(
            &kind,
            &agent_id,
            observation,
            prior,
            adopting_root,
            &self.map,
        );
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
        if prior.is_none()
            && matches!(
                observation.signal,
                lifecycle::LifecycleSignal::CompactionEnded { .. }
            )
        {
            state.compacted_awaiting_prompt = state.compacted_awaiting_prompt.or_else(|| {
                let predecessor = observation.compacted_from.as_ref()?;
                self.map
                    .get(&(kind.clone(), predecessor.clone()))?
                    .compacted_awaiting_prompt
            });
            state.soft_compact = observation
                .compacted_from
                .as_ref()
                .and_then(|predecessor| {
                    self.map
                        .get(&(kind.clone(), predecessor.clone()))?
                        .soft_compact
                        .clone()
                })
                .map(|mark| SoftCompactMark {
                    compactions: None,
                    low_water_tokens: None,
                    low_water_pct: None,
                    ..mark
                });
        }
        inherit_compaction_registration(&self.map, &mut state);
        inherit_launch_identity(&self.map, &self.launch_identity, &mut state);
        self.insert(key, state);
    }

    fn adopt_provisional(
        &mut self,
        kind: &AgentKind,
        key: &(AgentKind, AgentSessionId),
        observation: &AgentLifecycleObservation,
        stamp: Option<EventStamp<'_>>,
    ) -> Option<Arc<AgentState>> {
        if let Some(stamp) = stamp {
            let provisional_key = self.stamped_provisional(kind, stamp)?;
            return self.retire_provisional(&provisional_key);
        }
        if let Some(provisional_key) = observation.agent_name.as_deref().and_then(|name| {
            self.identity
                .adoptable_owner_for_name(&self.map, kind, name, key)
        }) && self
            .map
            .get(&provisional_key)
            .is_none_or(|provisional| stamp_takes(None, provisional))
            && let Some(prior) = self.retire_provisional(&provisional_key)
        {
            return Some(prior);
        }
        let provisional_key = observation.pane_id.as_ref().and_then(|pane_id| {
            self.identity
                .adoptable_owner_for_pane(&self.map, kind, pane_id, key, |provisional| {
                    stamp_takes(None, provisional)
                })
        })?;
        self.retire_provisional(&provisional_key)
    }

    /// The reservation a stamped event speaks for: the provisional row of its
    /// kind, launch, and epoch.
    fn stamped_provisional(&self, kind: &AgentKind, stamp: EventStamp<'_>) -> Option<AgentKey> {
        self.map
            .iter()
            .find(|(owner, state)| {
                owner.0 == *kind && owner.1.is_provisional() && stamp_takes(Some(stamp), state)
            })
            .map(|(owner, _)| owner.clone())
    }

    /// Give a provisional launch to the existing session row it registers as.
    /// A stamped event of the row's own launch at a later epoch (a resumed
    /// swap) releases its reservation whatever the row's pane or end; any
    /// other event releases only into a paneless or ended row, by pane, and
    /// only a provisional its stamp may take.
    fn release_stamped_provisional_for_existing(
        &mut self,
        kind: &AgentKind,
        key: &(AgentKind, AgentSessionId),
        observation: &AgentLifecycleObservation,
        stamp: Option<EventStamp<'_>>,
    ) {
        let Some(row) = self.map.get(key) else {
            return;
        };
        if row.is_provider_subagent() {
            return;
        }
        let continuation = stamp.filter(|stamp| {
            row.launch_id.as_ref() == Some(stamp.launch) && row.epoch < stamp.epoch
        });
        let provisional_key = if let Some(stamp) = continuation {
            self.stamped_provisional(kind, stamp)
        } else if row.pane.is_some() && row.ended_at.is_none() {
            None
        } else {
            observation.pane_id.as_ref().and_then(|pane_id| {
                self.identity.adoptable_owner_for_pane(
                    &self.map,
                    kind,
                    pane_id,
                    key,
                    |provisional| stamp_takes(stamp, provisional),
                )
            })
        };
        if let Some(provisional_key) = provisional_key {
            self.absorb_provisional(key, &provisional_key);
        }
    }

    /// Retire `provisional_key` into the existing row `key`, which takes its
    /// epoch, launch, and launch settings.
    fn absorb_provisional(&mut self, key: &AgentKey, provisional_key: &AgentKey) {
        if let Some(provisional) = self.retire_provisional(provisional_key)
            && let Some(existing) = self.map.get_mut(key)
        {
            let existing = agent_mut(existing);
            existing.epoch = existing.epoch.max(provisional.epoch);
            if let Some(launch_id) = &provisional.launch_id {
                existing.set_launch_id(launch_id.clone());
            }
            existing.record.clone_from(&provisional.record);
            existing.tier.clone_from(&provisional.tier);
            existing.mode = provisional.mode;
            existing.isolation = provisional.isolation;
            existing.effective_isolation = provisional
                .effective_isolation
                .or(existing.effective_isolation);
            self.launch_identity.replace(key, existing);
        }
    }

    fn retire_provisional(&mut self, key: &AgentKey) -> Option<Arc<AgentState>> {
        let prior = self.map.remove(key);
        self.launch_identity.remove(key);
        self.identity.release_key(key);
        self.identity.consume_launch_key(key);
        prior
    }

    fn reduce_agent_launch(
        &mut self,
        event: &EventEnvelope,
        kind: &AgentKind,
        payload: &AgentLaunchPayload,
    ) {
        if !valid_agent_name(&payload.agent_name) {
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
        if self.identity.launch_consumed(&payload.agent_id) {
            return;
        }
        // A swap's reservation keeps its agent's handle: it is the owner's
        // launch at a later epoch. Any other launch under an owned handle is
        // consumed.
        let launch = payload.launch_id.as_ref().unwrap_or(&payload.agent_id);
        let epoch = payload.launch.epoch.unwrap_or(0);
        if payload.agent_id.is_provisional()
            && let Some(owner) = self.identity.owner_for_name(&payload.agent_name)
            && owner.0 == *kind
            && owner != key
            && self
                .map
                .get(&owner)
                .is_some_and(|row| row.launch_key() != launch || row.epoch >= epoch)
        {
            self.identity.consume_launch_key(&key);
            return;
        }
        if matches!(payload.state, AgentLaunchState::Failed)
            && !self.map.contains_key(&key)
            && self.identity.owner_for_name(&payload.agent_name).is_none()
        {
            return;
        }
        let prior = self.map.get(&key).map(Arc::as_ref);
        let card_identity =
            self.identity
                .assign_launch(kind, &payload.agent_id, payload, prior, &self.map);
        let state = assemble_launch_state(kind, event, payload, prior, card_identity);
        self.insert(key, state);
    }
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

pub(super) fn message_agent_key<'a>(
    agents: impl Iterator<Item = &'a AgentState> + Clone,
    payload: &MessageEventPayload,
) -> Option<(AgentKind, AgentSessionId)> {
    let compact_command = payload.body == MessageBody::Command
        && matches!(
            payload.status,
            MessageStatus::Sent | MessageStatus::Delivered
        );
    let soft_compact = match payload.soft_compact {
        Some(SoftCompactStep::WrapUp) => payload.status == MessageStatus::Sent,
        Some(SoftCompactStep::Continue) => {
            payload.status == MessageStatus::Queued || payload.status.is_terminal()
        }
        None => false,
    };
    if !compact_command && !soft_compact {
        return None;
    }
    let mut agents = agents;
    let agent = agents
        .clone()
        .find(|agent| agent.kind == payload.kind && agent.agent_id == payload.agent_id)
        .or_else(|| {
            let agent_name = payload.agent_name.as_deref()?;
            agents.find(|agent| {
                agent.kind == payload.kind && agent.name.as_deref() == Some(agent_name)
            })
        })?;
    Some((agent.kind.clone(), agent.agent_id.clone()))
}

/// Spends the reminder rung claimed in epoch `compactions` (`None`: a legacy wrap-up, rung 1). A
/// claim armed before the row's latest compaction spends nothing; a later rung in the same epoch
/// raises the spent one.
fn stamp_reminder(agent: &mut AgentState, compactions: u32, rung: Option<u8>) {
    if compactions < agent.compaction_count {
        return;
    }
    let mark = agent.soft_compact.get_or_insert_default();
    if mark.compactions == Some(compactions) {
        mark.rung = mark.rung.max(rung);
    } else {
        mark.compactions = Some(compactions);
        mark.rung = rung;
    }
    mark.low_water_tokens = None;
    mark.low_water_pct = None;
}

fn stamp_message(
    agents: &mut BTreeMap<AgentKey, Arc<AgentState>>,
    method: MessageEventMethod,
    payload: &MessageEventPayload,
    timestamp: Timestamp,
) {
    let Some(key) = message_agent_key(agents.values().map(Arc::as_ref), payload) else {
        return;
    };
    let Some(agent) = agents.get_mut(&key) else {
        return;
    };
    let agent = agent_mut(agent);
    // Only the enqueue owes a continue. A rewrite back to queued (a released
    // claim, a requeue, an edit, the Codex rebind) carries a reason and must
    // not re-owe one an interrupt or a user prompt already cleared.
    let enqueued = method == MessageEventMethod::Queued && payload.reason.is_none();
    match (payload.soft_compact, payload.status) {
        // Only older binaries sharing the store still write a wrap-up message.
        (Some(SoftCompactStep::WrapUp), MessageStatus::Sent) => {
            stamp_reminder(agent, agent.compaction_count, None);
        }
        (Some(SoftCompactStep::Continue), MessageStatus::Queued) if enqueued => {
            agent.soft_compact.get_or_insert_default().continue_owed = Some(OwedContinue {
                message_id: payload.message_id.clone(),
                queued_at: timestamp,
                past_stop: payload.past_stop,
            });
        }
        // Only the owed continue's own ending settles it: an older continue, its
        // command canceled, can end after a newer compaction queued the one owed.
        (Some(SoftCompactStep::Continue), status) if status.is_terminal() => {
            if let Some(mark) = &mut agent.soft_compact
                && mark
                    .continue_owed
                    .as_ref()
                    .is_some_and(|owed| owed.message_id == payload.message_id)
            {
                mark.continue_owed = None;
            }
        }
        _ => {}
    }
    if payload.body != MessageBody::Command {
        return;
    }
    if let Some(tokens) = payload.compacted_context_tokens {
        agent.last_compact_command_tokens = Some(tokens);
    }
    // The lifecycle hook is recorded before its acknowledgement. A delayed
    // Delivered event must not undo a newer prompt's clear.
    if payload.status == MessageStatus::Sent {
        agent.compacted_awaiting_prompt = Some(timestamp);
    }
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
        state.prior_launch_ids.clone_from(&prior.prior_launch_ids);
        state.epoch = prior.epoch;
        state.launch_bound = prior.launch_bound;
        state
            .system_prompt_digest
            .clone_from(&prior.system_prompt_digest);
        state.launch_warnings.clone_from(&prior.launch_warnings);
        state.mode = prior.mode;
        state.isolation = prior.isolation;
        state.effective_isolation = prior.effective_isolation;
        state.launch_group = prior.launch_group.clone();
        state.launch_ordinal = prior.launch_ordinal;
        state.molted_from.clone_from(&prior.molted_from);
        state.pane = prior.pane.clone();
        state.runtime_owner = prior.runtime_owner.clone();
        state.parent_agent_id = prior.parent_agent_id.clone();
        state.parent_agent_kind = prior.parent_agent_kind.clone();
        state.launch_depth = prior.launch_depth;
        state.launched_by = prior.launched_by.clone();
        state.profile_scope = prior.profile_scope;
        state.task = prior.task.clone();
        state.first_prompt = prior.first_prompt.clone();
        state.prompt = prior.prompt.clone();
        state.prompt_wake_only = prior.prompt_wake_only;
        state.description = prior.description.clone();
        event::carry_lifetime_fields(&mut state, prior);
        state.origin = prior.origin;
        state.compacted_from = prior.compacted_from.clone();
        state.model = prior.model.clone();
        state.effort = prior.effort.clone();
        state.budget = prior.budget.clone();
        state.tune.clone_from(&prior.tune);
        state.usage = prior.usage.clone();
        state.compaction_count = prior.compaction_count;
        state.compacted_at = prior.compacted_at;
        state.soft_compact = prior.soft_compact.clone();
        state.tool_calls = prior.tool_calls.clone();
        state.background_shells = prior.background_shells.clone();
        state.background_scripts = prior.background_scripts.clone();
        state.last_compact_command_tokens = prior.last_compact_command_tokens;
        state.retired_context_tokens = prior.retired_context_tokens;
        state.compacted_unmeasured = prior.compacted_unmeasured;
        state.compacted_awaiting_prompt = prior.compacted_awaiting_prompt;
        state.turn_started_at = prior.turn_started_at;
        state.user_turn_started_at = prior.user_turn_started_at;
        state.last_prompt_at = prior.last_prompt_at;
        state.keepalive_since = prior.keepalive_since;
        state.pinged_at = prior.pinged_at;
        state.ping_opened_at = prior.ping_opened_at;
        state.prior_turn = prior.prior_turn;
        state.registered_at = prior.registered_at.or(Some(event_ts));
        state.on_screen_at = prior.on_screen_at;
    }
    state
}

/// A registration's lane is the checkout its process reports, so it never
/// lands on a lane-less row displaced into an empty directory: that row keeps
/// its old reading until a launch event brings its lane.
fn fold_launch_params(state: &mut AgentState, launch: &LaunchParams, registration: bool) {
    if let Some(automation) = &launch.automation {
        state.automation = Some(automation.clone());
    }
    if let Some(workflow) = &launch.workflow {
        state.workflow = Some(workflow.clone());
    }
    if let Some(predecessor) = &launch.molted_from {
        state.molted_from = Some(predecessor.clone());
    }
    if let Some(record) = &launch.record {
        state.record = Some(record.clone());
    }
    if let Some(parent_agent_id) = &launch.parent_agent_id {
        state.parent_agent_id = Some(parent_agent_id.clone());
    }
    if let Some(parent_agent_kind) = &launch.parent_agent_kind {
        state.parent_agent_kind = Some(parent_agent_kind.clone());
    }
    if let Some(launch_depth) = launch.launch_depth {
        state.launch_depth = Some(launch_depth);
    }
    if let Some(launched_by) = &launch.launched_by {
        state.launched_by = Some(launched_by.as_ref().clone());
    }
    if let Some(profile) = &launch.profile {
        state.profile = Some(profile.clone());
    }
    if let Some(scope) = launch.profile_scope {
        state.profile_scope = Some(scope);
    }
    if let Some(login) = &launch.login {
        state.login = Some(login.clone());
    }
    if let Some(tier) = &launch.tier {
        state.tier = Some(tier.clone());
    }
    if let Some(mode) = launch.mode {
        state.mode = Some(mode);
    }
    if let Some(isolation) = launch.isolation {
        state.isolation = Some(isolation);
    }
    if let Some(checkout) = &launch.checkout {
        state.checkout = Some(checkout.clone());
    }
    if !launch.add_dirs.is_empty() {
        state.add_dirs.clone_from(&launch.add_dirs);
    }
    state.untrusted_checkout |= launch.untrusted_checkout;
    if let Some(role) = &launch.role {
        state.role = Some(role.clone());
    }
    if let Some(team) = &launch.team {
        state.team = Some(team.clone());
    }
    if let Some(task) = &launch.loop_task {
        state.loop_task = Some(task.clone());
    }
    if !launch.loop_booked_by.is_empty() {
        state.loop_booked_by.clone_from(&launch.loop_booked_by);
    }
    if let Some(fire) = &launch.loop_fire {
        state.loop_fire = Some(fire.clone());
    }
    if let Some(launch_group) = &launch.launch_group {
        state.launch_group = Some(launch_group.clone());
    }
    if let Some(launch_ordinal) = launch.launch_ordinal {
        state.launch_ordinal = Some(launch_ordinal);
    }
    if state.lane.is_none()
        && !(registration && state.checkout.is_some())
        && let Some(lane) = &launch.lane
    {
        state.lane = Some(lane.clone());
    }
    if state.lane.is_none()
        && let Some(channel) = &launch.channel
    {
        state.channel = Some(channel.clone());
    }
    state.project_lane();
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

/// Drop a reading's current-window figures, keeping the window/capacity pair and the
/// whole-run total. A completed compaction retires the carried reading this
/// way, and a later re-report of the retired occupancy contributes nothing.
fn clear_current_window(usage: &mut AgentUsageSummary) {
    *usage = AgentUsageSummary {
        context_window: usage.context_window,
        context_capacity: usage.context_capacity,
        run_total_tokens: usage.run_total_tokens,
        ..AgentUsageSummary::default()
    };
}

/// Whether `event_name` attaches a row whose prior parent is `prior_parent` (`None`
/// for an unseen row) to a parent as an adopted root, which drops the launch
/// identity the root copied from its parent's process environment
/// ([`strip_root_launch_identity`]). The reducer and the lifecycle follower both
/// decide it here.
pub(in crate::store) fn adopts_root(
    event_name: Option<&str>,
    prior_parent: Option<&Option<AgentSessionId>>,
) -> bool {
    event_name == Some("SubagentAdopted") && prior_parent.is_some_and(Option::is_none)
}

/// The parent a row holds after `event_name` carrying `event_parent`, given its prior parent
/// (`None` for an unseen row): an established parent stays, and the event's parent takes on
/// an unseen row or a root the event adopts ([`adopts_root`]).
pub(in crate::store) fn parent_after(
    prior_parent: Option<&Option<AgentSessionId>>,
    event_name: Option<&str>,
    event_parent: Option<AgentSessionId>,
) -> Option<AgentSessionId> {
    match prior_parent {
        Some(Some(parent)) => Some(parent.clone()),
        Some(None) if adopts_root(event_name, prior_parent) => event_parent,
        Some(None) => None,
        None => event_parent,
    }
}

/// Drop launch-scoped identity that an unjoined provider subagent copied from
/// its parent's process environment or inherited as a provisional root before
/// `SubagentAdopted` attached it to its parent.
fn strip_root_launch_identity(state: &mut AgentState) {
    state.launch_id = None;
    state.prior_launch_ids.clear();
    state.epoch = 0;
    state.launch_bound = false;
    state.system_prompt_digest = None;
    state.launch_warnings.clear();
    state.profile = None;
    state.profile_scope = None;
    state.login = None;
    state.tier = None;
    state.record = None;
    state.mode = None;
    state.isolation = None;
    state.effective_isolation = None;
    state.checkout = None;
    state.add_dirs.clear();
    state.untrusted_checkout = false;
    state.role = None;
    state.team = None;
    state.loop_task = None;
    state.loop_booked_by.clear();
    state.loop_fire = None;
    state.workflow = None;
    state.tune = Default::default();
    state.automation = None;
    state.launch_group = None;
    state.launch_ordinal = None;
    state.molted_from = None;
    state.lane = None;
    state.channel = None;
    state.root_lane = false;
    state.launch_depth = None;
    state.launched_by = None;
    state.task = None;
}

fn assemble_agent_state(input: AgentStateInput<'_>) -> AgentState {
    let mut state = carried_base(
        input.kind,
        input.agent_id,
        input.prior,
        input.event.timestamp,
    );
    let adopting_root = adopts_root(
        input.event_name,
        input.prior.map(|prior| &prior.parent_agent_id),
    );
    if adopting_root {
        strip_root_launch_identity(&mut state);
    }
    fold_launch_params(&mut state, &input.observation.launch, true);
    state.resumed_at =
        (input.event_name == Some("rimz.agent-resumed")).then_some(input.event.timestamp);
    let ended_at =
        matches!(&input.signal, lifecycle::LifecycleSignal::Ended).then_some(input.event.timestamp);
    let mode = crate::store::message::transition_mode(input.observation);
    let snapshot = mode == TransitionMode::Snapshot;
    let ends_session_shells = !snapshot
        && matches!(
            &input.signal,
            lifecycle::LifecycleSignal::Ended
                | lifecycle::LifecycleSignal::Registered
                | lifecycle::LifecycleSignal::SubagentStopped { .. }
        );
    let linked_continuation = !snapshot
        && input.prior.is_none()
        && input.observation.compacted_from.is_some()
        && matches!(
            input.signal,
            lifecycle::LifecycleSignal::CompactionEnded { failed: false, .. }
        );
    let retires_usage = fold_lifecycle(
        &mut state,
        input.prior,
        input.event.timestamp,
        input.signal,
        input.observation.prompt.as_deref(),
        input.observation.ask_queue.as_ref(),
        mode,
    );
    if linked_continuation {
        state.compacted_at = Some(input.event.timestamp);
        state.compaction_retired = true;
    }
    if ended_at.is_some()
        && crate::store::session_death::reaped_end_rests_at_idle(
            input.event_name,
            input.prior.map(|prior| prior.status),
        )
    {
        state.status = AgentStatus::Idle;
    }
    state.finished_at = finish_clock(
        input.prior,
        state.status,
        state.phase,
        input.event.timestamp,
    );
    state.kept_verdict = kept_verdict(input.prior, state.status);
    let default_window = crate::agents::spec_by_kind(input.kind.as_str())
        .and_then(|definition| definition.default_context_window);
    let mut carried_usage = input.prior.map(|prior| prior.usage.clone());
    if retires_usage {
        let mut retired_from_prior = false;
        if let Some(carried) = &mut carried_usage {
            // An estimate carries no per-call split. Retiring it again before a
            // model call keeps the earlier count: the sidecar still holds that
            // pre-compaction reading, and the attach must keep withholding it.
            let measured = carried
                .input_context_tokens()
                .is_some_and(|input| input > 0);
            if let Some(occupancy) = carried.context_used_tokens() {
                retired_from_prior = true;
                if measured || state.retired_context_tokens.is_none() {
                    state.retired_context_tokens = Some(occupancy);
                }
            }
            clear_current_window(carried);
        }
        if !retired_from_prior
            && let Some(occupancy) = input
                .observation
                .retired_context_tokens
                .filter(|used| *used > 0)
        {
            state.retired_context_tokens = Some(occupancy);
        }
    }
    let mut observed_usage = input.observation.usage.clone();
    if state.retired_context_tokens.is_some()
        && observed_usage.context_used_tokens() == state.retired_context_tokens
    {
        clear_current_window(&mut observed_usage);
    }
    if let Some(mark) = &mut state.soft_compact
        && mark.compactions.is_some()
    {
        if let Some(tokens) = observed_usage.context_used_tokens() {
            mark.low_water_tokens =
                Some(mark.low_water_tokens.map_or(tokens, |low| low.min(tokens)));
        }
        if let Some(pct) = observed_usage.context_pct {
            mark.low_water_pct = Some(mark.low_water_pct.map_or(pct, |low| low.min(pct)));
        }
    }
    state.compacted_unmeasured |= retires_usage || linked_continuation;
    if observed_usage
        .input_context_tokens()
        .is_some_and(|input| input > 0)
    {
        state.compacted_unmeasured = false;
    }
    let usage = observed_usage.merge(carried_usage.as_ref(), default_window);
    // Established lineage stays authoritative. The explicit adoption event is
    // the one path that converts a provisional root after provider evidence
    // became readable later than the child's own hooks.
    let parent_agent_id = parent_after(
        input.prior.map(|prior| &prior.parent_agent_id),
        input.event_name,
        input.event_parent_agent_id,
    );
    let worktree = worktree_projection(
        input.observation,
        input.prior,
        input.establishes_identity,
        input.event_name,
    );
    let runtime_owner = runtime_projection(input.observation, input.prior, input.agent_id);
    let provider_subagent = parent_agent_id.is_some()
        && (adopting_root || input.prior.is_none_or(|prior| prior.launch_depth.is_none()));
    let task = task_projection(
        input.observation,
        (!adopting_root).then_some(input.prior).flatten(),
        provider_subagent,
        input.event_task,
    );
    state.name = Some(input.card_identity.name);
    state.name_explicit = input.card_identity.name_explicit;
    state.kind_ordinal = Some(input.card_identity.kind_ordinal);
    state.ended_at = ended_at;
    state.pane = pane_projection(input.observation, input.prior);
    state.runtime_owner = runtime_owner;
    if input.observation.on_screen == Some(true) {
        state.on_screen_at = Some(input.event.timestamp);
    } else if input
        .prior
        .is_some_and(|prior| !crate::store::session_death::same_agent_instance(prior, &state))
    {
        state.on_screen_at = None;
    }
    state.parent_agent_id = parent_agent_id;
    state.worktree_path = worktree.path;
    if let Some(branch) = worktree.observed_branch {
        state.worktree_branches.insert(branch.clone());
        state.worktree_branch = Some(branch);
    }
    state.task = task;
    // An open ping's prompt is the harness's, not the agent's latest ask.
    if let Some(prompt) = input.observation.prompt.as_deref()
        && !state.ping_turn
    {
        state.observe_prompt(prompt, crate::store::message::prompt_is_wake_only(prompt));
    }
    if let Some(description) = &input.observation.description {
        state.description = Some(description.clone());
    }
    if let Some(transcript_path) = &input.observation.transcript_path {
        state.transcript_path = Some(transcript_path.clone());
    }
    if let Some(origin) = input.observation.origin {
        state.origin = Some(origin);
    }
    if let Some(account_key) = &input.observation.account_key {
        state.account_key = Some(account_key.clone());
    }
    if let Some(compacted_from) = &input.observation.compacted_from {
        state.compacted_from = Some(compacted_from.clone());
    }
    state.usage = usage;
    // A shell or script never outlives its session or native subagent: an end,
    // a subagent stop, or a real registration drops the lists before this
    // event's own reports land. Snapshots keep them.
    if ends_session_shells {
        state.background_shells.clear();
        state.background_scripts.clear();
    }
    if !snapshot && let Some(report) = &input.observation.background_shells {
        report.apply(&mut state.background_shells);
    }
    if !snapshot && let Some(report) = &input.observation.background_scripts {
        report.apply(&mut state.background_scripts);
    }
    if !snapshot
        && input.observation.script_ask
        && let Some(key) = state
            .open_ask
            .as_ref()
            .and_then(|ask| ask.native_key.as_ref())
    {
        for script in &mut state.background_scripts {
            script.held_by = Some(key.clone());
        }
    }
    state.trim_ended_text();
    state
}

fn inherit_compaction_registration(
    map: &BTreeMap<(AgentKind, AgentSessionId), Arc<AgentState>>,
    continuation: &mut AgentState,
) {
    let Some(compacted_from) = continuation.compacted_from.as_ref() else {
        return;
    };
    let predecessor_key = (continuation.kind.clone(), compacted_from.clone());
    let Some(predecessor) = map.get(&predecessor_key) else {
        return;
    };
    continuation.last_prompt_at = continuation.last_prompt_at.max(predecessor.last_prompt_at);
    let Some(predecessor_registered_at) = predecessor.registered_at else {
        return;
    };
    if continuation
        .registered_at
        .is_some_and(|registered_at| predecessor_registered_at < registered_at)
    {
        continuation.registered_at = Some(predecessor_registered_at);
    }
}

/// Carry state across an in-place provider conversation change. Launch-scoped
/// routing and ancestry copy from the chain's first predecessor: those fields
/// are equal along the chain. Per-conversation state (`tune`) copies from the
/// latest predecessor instead. Card naming stays session-scoped, and only an
/// explicit compaction edge inherits registration primacy. A field that pins a
/// row to its launch (team, loop task, workflow attempt, lane, channel) is copied here;
/// a field missing from this list is lost when `/clear` gives the pane a new
/// conversation row.
fn inherit_launch_identity(
    map: &BTreeMap<AgentKey, Arc<AgentState>>,
    launch_identity: &LaunchIdentityIndex,
    successor: &mut AgentState,
) {
    if successor.launch_id.is_some()
        || successor.pane.is_none()
        || successor.is_provider_subagent()
        || successor.runtime_owner.as_ref().map(|owner| owner.kind) != Some(RuntimeOwnerKind::Agent)
    {
        return;
    }
    let Some((predecessor, latest)) = launch_identity.predecessors(map, successor) else {
        return;
    };

    successor.launch_id.clone_from(&predecessor.launch_id);
    successor
        .prior_launch_ids
        .clone_from(&predecessor.prior_launch_ids);
    successor.epoch = predecessor.epoch;
    successor
        .launch_warnings
        .clone_from(&predecessor.launch_warnings);
    successor.profile.clone_from(&predecessor.profile);
    successor.profile_scope = predecessor.profile_scope;
    successor.login.clone_from(&predecessor.login);
    successor.tier.clone_from(&predecessor.tier);
    successor.record.clone_from(&predecessor.record);
    successor.mode = predecessor.mode;
    successor.isolation = predecessor.isolation;
    successor.checkout.clone_from(&predecessor.checkout);
    successor.add_dirs.clone_from(&predecessor.add_dirs);
    successor.untrusted_checkout = predecessor.untrusted_checkout;
    successor.effective_isolation = predecessor.effective_isolation;
    successor.role.clone_from(&predecessor.role);
    successor.team.clone_from(&predecessor.team);
    successor.loop_task.clone_from(&predecessor.loop_task);
    successor
        .loop_booked_by
        .clone_from(&predecessor.loop_booked_by);
    successor.loop_fire.clone_from(&predecessor.loop_fire);
    successor.workflow.clone_from(&predecessor.workflow);
    successor.tune.clone_from(&latest.tune);
    successor.automation.clone_from(&predecessor.automation);
    successor.launch_group.clone_from(&predecessor.launch_group);
    successor.launch_ordinal = predecessor.launch_ordinal;
    successor.molted_from.clone_from(&predecessor.molted_from);
    // A displaced lane-less predecessor keeps reading through its checkout, so
    // its successor takes no lane from the directory it registered in.
    successor.lane = match &predecessor.lane {
        Some(lane) => Some(lane.clone()),
        None if predecessor.checkout.is_some() => None,
        None => successor.lane.take(),
    };
    successor.channel.clone_from(&predecessor.channel);
    successor.project_lane();
    successor.launch_depth = predecessor.launch_depth;
    successor.launched_by.clone_from(&predecessor.launched_by);
    successor
        .parent_agent_id
        .clone_from(&predecessor.parent_agent_id);
    successor
        .parent_agent_kind
        .clone_from(&predecessor.parent_agent_kind);
}

/// The instant a row first came to rest on a verdict, kept while it rests there: a
/// terminal status, or a clean turn end parked on background work, which reads as
/// success. Any other status clears it.
fn finish_clock(
    prior: Option<&AgentState>,
    status: AgentStatus,
    phase: lifecycle::TurnPhase,
    at: Timestamp,
) -> Option<Timestamp> {
    let rests_on_verdict = matches!(status, AgentStatus::Success | AgentStatus::Failed)
        || (status == AgentStatus::Running && phase == lifecycle::TurnPhase::Parked);
    rests_on_verdict.then(|| prior.and_then(|prior| prior.finished_at).unwrap_or(at))
}

/// The verdict a row resting at `Idle` keeps: the one `prior` rests on, or the one a `prior`
/// already at `Idle` keeps. A turn always leaves `Idle`, so one that opened since the verdict
/// shows as a `prior` in another status, and drops it.
fn kept_verdict(prior: Option<&AgentState>, status: AgentStatus) -> Option<KeptVerdict> {
    let prior = prior.filter(|_| status == AgentStatus::Idle)?;
    match prior.status {
        AgentStatus::Idle => prior.kept_verdict,
        AgentStatus::Success | AgentStatus::Failed => Some(KeptVerdict {
            status: prior.status,
            finished_at: prior.finished_at,
        }),
        _ => None,
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
    if let Some(digest) = &payload.system_prompt_digest {
        state.system_prompt_digest = Some(digest.clone());
    }
    if let Some(launch_id) = &payload.launch_id {
        state.set_launch_id(launch_id.clone());
    }
    if let Some(epoch) = payload.launch.epoch {
        state.epoch = epoch;
    }
    state.launch_bound |= payload.state == AgentLaunchState::Bound;
    fold_launch_params(&mut state, &payload.launch, false);
    if payload.state == AgentLaunchState::Bound
        && let Some(prior) = prior
    {
        state.workflow.clone_from(&prior.workflow);
    }
    state.mode = payload.launch.mode;
    if let Some(pane_id) = &payload.pane_id {
        state.pane = Some(PaneRef::from_id(pane_id.clone()));
    }
    if let Some(runtime_owner) = &payload.runtime_owner {
        state.runtime_owner = Some(runtime_owner.clone());
    }
    if let Some(prompt) = payload.prompt.as_deref() {
        state.observe_prompt(prompt, crate::store::message::prompt_is_wake_only(prompt));
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
    state.task = state.prompt.clone();
    if let Some(description) = &payload.description {
        state.description = Some(description.clone());
    }
    if let Some(worktree_path) = &payload.worktree_path {
        state.worktree_path = Some(worktree_path.clone());
    }
    if let Some(worktree_branch) = &payload.worktree_branch {
        state.worktree_branch = Some(worktree_branch.clone());
        if !worktree_branch.is_empty() {
            state.worktree_branches.insert(worktree_branch.clone());
        }
    }
    state.status = status;
    state.finished_at = finish_clock(prior, status, phase, event.timestamp);
    state.kept_verdict = kept_verdict(prior, status);
    state.phase = phase;
    if status == AgentStatus::Running {
        state.prior_turn = prior.and_then(AgentState::prior_turn_for_next);
        state.turn_started_at = Some(event.timestamp);
        state.user_turn_started_at = Some(event.timestamp);
        state.keepalive_since = None;
    }
    if prior.is_some_and(|prior| !crate::store::session_death::same_agent_instance(prior, &state)) {
        state.on_screen_at = None;
    }
    state
}

/// Drop the continue a compaction command owes this row: the queue withdraws it at delivery
/// (`MessageRecord::broken_row_premise`).
fn clear_owed_continue(state: &mut AgentState) {
    if let Some(mark) = &mut state.soft_compact {
        mark.continue_owed = None;
    }
}

/// Whether folding `signal` with `transition` over `prior` completes a
/// compaction (incrementing `compaction_count`, stamping `compacted_at`, and
/// retiring prior context occupancy).
///
/// A first event with no prior row (a linked continuation seeded by
/// `compacted_from`) returns false: the predecessor already counted that
/// compaction and the new row has no prior usage to retire.
///
/// For a kind whose `compaction_ended` annotation is `Native`, only an
/// unfailed, non-echo `CompactionEnded` that does not follow an already-retired
/// end (`compaction_retired`) completes a compaction, whether or not a bracket
/// is open; any other signal after `Compacting` only lowers the head. Every
/// other kind, a kind with no installed spec included, also completes an open
/// bracket on any signal other than a failed or echo `CompactionEnded`.
///
/// Shared by the fold and the lifecycle writer's lagging-sidecar stamp so both
/// agree on every close edge.
pub(in crate::store) fn compaction_retires_usage(
    kind: &str,
    prior: Option<&AgentState>,
    signal: &lifecycle::LifecycleSignal,
    mode: TransitionMode,
    transition: Transition,
) -> bool {
    let Some(prior) = prior else {
        return false;
    };
    if transition.ping.is_some() || mode == TransitionMode::Snapshot {
        return false;
    }
    let is_compaction_ended = matches!(signal, lifecycle::LifecycleSignal::CompactionEnded { .. });
    if !transition.compaction_closed && !is_compaction_ended {
        return false;
    }
    if !prior.compaction_retired
        && matches!(
            signal,
            lifecycle::LifecycleSignal::CompactionEnded {
                failed: false,
                echo: false,
                ..
            }
        )
    {
        return true;
    }
    if !transition.compaction_closed {
        return false;
    }
    let native = crate::agents::spec_by_kind(kind)
        .is_some_and(|spec| spec.lifecycle_hooks.compaction_ended.is_native());
    if native {
        return false;
    }
    !matches!(
        signal,
        lifecycle::LifecycleSignal::CompactionEnded { failed: true, .. }
            | lifecycle::LifecycleSignal::CompactionEnded { echo: true, .. }
    )
}

/// Returns whether this event completes a compaction (retiring prior usage,
/// incrementing `compaction_count`, and stamping `compacted_at`), decided by
/// [`compaction_retires_usage`].
fn fold_lifecycle(
    state: &mut AgentState,
    prior: Option<&AgentState>,
    timestamp: Timestamp,
    signal: lifecycle::LifecycleSignal,
    prompt: Option<&str>,
    ask_queue: Option<&crate::agents::AskQueueEdit>,
    mode: TransitionMode,
) -> bool {
    let turn_ids = prior.map(AgentState::turn_ids).unwrap_or_default();
    let transition = AgentState::transition(prior, &signal, mode);
    let Transition {
        next,
        kind,
        compaction_closed,
        opened_turn,
        ping,
        ..
    } = transition;
    if let (Some(edge), Some(prior)) = (ping, prior) {
        carry_across_ping(state, prior, timestamp, edge);
        return false;
    }
    state.status = next.status;
    state.phase = next.phase;
    if mode == TransitionMode::Snapshot {
        if let Some(prior) = prior {
            state.registered_at = prior.registered_at;
            state.turn_ended_at = prior.turn_ended_at;
            state.last_tool_at = prior.last_tool_at;
            state.started_turn_id.clone_from(&prior.started_turn_id);
            state
                .superseded_turn_id
                .clone_from(&prior.superseded_turn_id);
            state.closed_turn_id.clone_from(&prior.closed_turn_id);
            state.compacting_since = prior.compacting_since;
            state.compaction_retired = prior.compaction_retired;
            state.ping_turn = prior.ping_turn;
        }
        if opened_turn {
            state.prior_turn = prior.and_then(AgentState::prior_turn_for_next);
            state.turn_started_at = Some(timestamp);
        }
        return false;
    }
    state.compacting_since = next.compacting.then_some(timestamp);
    let completed_compaction =
        compaction_retires_usage(state.kind.as_str(), prior, &signal, mode, transition);
    state.compaction_count += u32::from(completed_compaction);
    // A registration (`/clear`) drops the whole mark, so the reminder re-arms;
    // an interrupt drops only the owed continue and leaves the epoch spent.
    match signal {
        lifecycle::LifecycleSignal::Registered => state.soft_compact = None,
        lifecycle::LifecycleSignal::TurnInterrupted { .. } => clear_owed_continue(state),
        _ => {}
    }
    match &signal {
        // An unfailed manual close, or an unfailed close landing at rest that
        // carries a known trigger, closes an open bracket (echo included), or
        // completes a compaction, compacted a context no prompt has used since.
        // An unfailed non-echo automatic close landing in an open non-parked
        // turn continues work and clears the marker. A failed close, or an
        // unbracketed triggerless echo or linked continuation (which inherits
        // its predecessor's marker instead), leaves the marker as it was.
        lifecycle::LifecycleSignal::CompactionEnded {
            auto,
            failed: false,
            ..
        } if *auto == Some(false)
            || (!matches!(next.status, AgentStatus::Running | AgentStatus::Waiting)
                && (auto.is_some() || compaction_closed || completed_compaction)) =>
        {
            state.compacted_awaiting_prompt = Some(timestamp);
        }
        lifecycle::LifecycleSignal::CompactionEnded {
            auto: Some(true),
            failed: false,
            echo: false,
        } if matches!(next.status, AgentStatus::Running | AgentStatus::Waiting)
            && next.phase != lifecycle::TurnPhase::Parked =>
        {
            state.compacted_awaiting_prompt = None;
        }
        lifecycle::LifecycleSignal::TurnStarted { .. } => state.compacted_awaiting_prompt = None,
        _ => {}
    }
    if let lifecycle::LifecycleSignal::ToolUsed {
        name: Some(name), ..
    } = &signal
    {
        let name = name.trim();
        if !name.is_empty() {
            let total = state.tool_calls.entry(name.to_owned()).or_default();
            *total = total.saturating_add(1);
        }
    }
    // A context reset that lands the agent at rest retires the prior turn's
    // subagents: a manual `/compact` summarizes away the turn the children
    // belonged to, and a `/clear` (`Registered`) drops it outright. Advancing
    // the subagent boundary here makes a user-typed reset behave like automatic
    // compaction from a rested state, which already opens a turn. The rest gate
    // is load-bearing: automatic compaction *mid-turn* resumes the same turn
    // (stays `running`), so its in-flight children stay listed.
    // Checking `completed_compaction` handles completions whose bracket was
    // missed or already lowered while keeping echoes and duplicates inert. A
    // first-event `Registered` leaves `turn_started_at` unset because the
    // session has never opened a turn; pane recovery reads that absence as
    // first-turn-start eligibility. A reset before any turn has nothing to
    // retire.
    let resets_context = prior.is_some_and(|prior| prior.turn_started_at.is_some())
        && !matches!(next.status, AgentStatus::Running | AgentStatus::Waiting)
        && (matches!(&signal, lifecycle::LifecycleSignal::Registered)
            || (completed_compaction
                && matches!(&signal, lifecycle::LifecycleSignal::CompactionEnded { .. })));
    if opened_turn || resets_context {
        state.turn_started_at = Some(timestamp);
    }
    // The cache clock reads the stamps a turn opened over while that turn reads as a
    // limit refusal; a context reset leaves no request to read back to.
    if resets_context {
        state.prior_turn = None;
    } else if opened_turn {
        state.prior_turn = prior.and_then(AgentState::prior_turn_for_next);
    }
    let closes_request = (!matches!(kind, lifecycle::TransitionKind::Ignored { .. })
        && matches!(
            signal,
            lifecycle::LifecycleSignal::TurnEnded { .. }
                | lifecycle::LifecycleSignal::TurnInterrupted { .. }
        ))
        || (completed_compaction
            && matches!(signal, lifecycle::LifecycleSignal::CompactionEnded { .. }));
    state.turn_ended_at = if closes_request {
        Some(timestamp)
    } else {
        prior.and_then(|p| p.turn_ended_at)
    };
    // A ping stays open across a report the machine ignored; anything else it
    // grew into folds as that work.
    state.ping_turn = prior.is_some_and(|prior| prior.ping_turn)
        && matches!(kind, lifecycle::TransitionKind::Ignored { .. });
    // A delivered prompt opens a provider turn but continues the user's task.
    let harness_prompt = matches!(signal, lifecycle::LifecycleSignal::TurnStarted { .. })
        && prompt.is_some_and(crate::store::message::prompt_is_harness_delivered);
    if (opened_turn && !harness_prompt) || resets_context {
        state.user_turn_started_at = Some(timestamp);
    }
    // A run of keepalive-only turns keeps the last real request it followed.
    // Any other prompt ends the run even when it resumes a parked row. An inert
    // ping opens its run in `carry_across_ping`, including on a parked row.
    let turn_started = matches!(signal, lifecycle::LifecycleSignal::TurnStarted { .. });
    let keepalive_turn = turn_started && mode == TransitionMode::KeepaliveEdge;
    if turn_started && !keepalive_turn {
        state.last_prompt_at = Some(timestamp);
    }
    // A turn the user opened takes over from the compaction's owed continue, and so does any
    // turn that starts after a compaction or a typed compaction command later than the continue's
    // enqueue: the agent is working again. A harness notice landing before that leaves it owed.
    let compacted_since = state
        .compacted_at
        .max(prior.and_then(|prior| prior.compacted_awaiting_prompt));
    let compacted_since_owed = state
        .soft_compact
        .as_ref()
        .and_then(|mark| mark.continue_owed.as_ref())
        .zip(compacted_since)
        .is_some_and(|(owed, compacted_at)| compacted_at > owed.queued_at);
    if turn_started
        && !keepalive_turn
        && (!harness_prompt || compacted_since_owed)
        && !matches!(kind, lifecycle::TransitionKind::Ignored { .. })
    {
        clear_owed_continue(state);
    }
    if resets_context || ((opened_turn || turn_started) && !keepalive_turn) {
        state.keepalive_since = None;
    } else if opened_turn {
        state.keepalive_since = Some(
            prior
                .and_then(|prior| prior.keepalive_since.or(prior.last_request_at()))
                .unwrap_or(timestamp),
        );
    }
    state.waiting_since = if matches!(&signal, lifecycle::LifecycleSignal::AwaitingInput { .. }) {
        Some(timestamp)
    } else if next.status == AgentStatus::Waiting {
        prior.and_then(|p| p.waiting_since)
    } else {
        None
    };
    state.open_ask = match &signal {
        lifecycle::LifecycleSignal::AwaitingInput {
            kind,
            ask_id: Some(id),
            detail,
            extra_questions,
            native_key,
        } => Some(crate::agents::OpenAsk {
            extra_questions: *extra_questions,
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
    state.queued_asks = prior
        .map(|state| state.queued_asks.clone())
        .unwrap_or_default();
    if let Some(edit) = ask_queue {
        if edit.clear_all {
            state.queued_asks.clear();
        }
        state.queued_asks.retain(|ask| {
            !edit
                .answered
                .iter()
                .any(|answer| answer.native_key == ask.native_key)
        });
        for question in &edit.queued {
            let Some(id) = &question.ask_id else { continue };
            if state
                .queued_asks
                .iter()
                .any(|ask| ask.native_key == question.native_key)
            {
                continue;
            }
            state.queued_asks.push(crate::agents::QueuedAsk {
                id: id.clone(),
                detail: question.detail.clone(),
                native_key: question.native_key.clone(),
                since: timestamp,
            });
        }
    }
    if !matches!(next.status, AgentStatus::Running | AgentStatus::Waiting) {
        state.queued_asks.clear();
    }
    (
        state.started_turn_id,
        state.superseded_turn_id,
        state.closed_turn_id,
    ) = lifecycle::turn_ids_after(turn_ids, &signal, opened_turn);
    // Keep `compaction_retired` set across consecutive `CompactionEnded`
    // signals so unflagged legacy records (written before `echo` existed)
    // still count and retire once per compaction in either logged order.
    let already_retired = prior.is_some_and(|prior| prior.compaction_retired);
    state.compaction_retired = completed_compaction
        || (already_retired
            && matches!(signal, lifecycle::LifecycleSignal::CompactionEnded { .. }));
    if completed_compaction {
        state.compacted_at = Some(timestamp);
        // An automatic compaction that ends inside the open turn leaves the agent working on by
        // itself: the continue owed from before it has nothing left to bring about.
        let automatic = matches!(
            signal,
            lifecycle::LifecycleSignal::CompactionEnded { auto, .. } if auto != Some(false)
        );
        if automatic && matches!(next.status, AgentStatus::Running | AgentStatus::Waiting) {
            clear_owed_continue(state);
        }
    }
    completed_compaction
}

/// A ping-only turn's edge: the agent reads exactly as `prior` left it, and
/// only the open-ping flag and the ping stamps move.
fn carry_across_ping(
    state: &mut AgentState,
    prior: &AgentState,
    timestamp: Timestamp,
    edge: lifecycle::PingEdge,
) {
    state.ping_turn = edge != lifecycle::PingEdge::Close;
    state.pinged_at = if edge == lifecycle::PingEdge::Close {
        Some(timestamp)
    } else {
        prior.pinged_at
    };
    // A run of pings keeps the last real request it followed: the clock the keepalive cap reads.
    if edge == lifecycle::PingEdge::Open {
        state.keepalive_since = prior.keepalive_since.or(prior.last_request_at());
        state.ping_opened_at = Some(timestamp);
    } else {
        state.ping_opened_at = prior.ping_opened_at;
    }
    state.status = prior.status;
    state.phase = prior.phase;
    state.last_activity = prior.last_activity;
    state.compacting_since = prior.compacting_since;
    state.compacted_awaiting_prompt = prior.compacted_awaiting_prompt;
    state.turn_started_at = prior.turn_started_at;
    state.turn_ended_at = prior.turn_ended_at;
    state.user_turn_started_at = prior.user_turn_started_at;
    state.last_prompt_at = prior.last_prompt_at;
    state.waiting_since = prior.waiting_since;
    state.open_ask.clone_from(&prior.open_ask);
    state.queued_asks.clone_from(&prior.queued_asks);
    state.started_turn_id.clone_from(&prior.started_turn_id);
    state
        .superseded_turn_id
        .clone_from(&prior.superseded_turn_id);
    state.closed_turn_id.clone_from(&prior.closed_turn_id);
    state.compaction_retired = prior.compaction_retired;
}

struct WorktreeProjection {
    path: Option<String>,
    observed_branch: Option<String>,
}

fn worktree_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
    establishes_identity: bool,
    event_name: Option<&str>,
) -> WorktreeProjection {
    let event_path = observation.worktree_path.clone();
    let prior_path = prior.and_then(|p| p.worktree_path.clone());
    let event_first = establishes_identity || event_name.is_none();
    let path = if event_first {
        event_path.or(prior_path)
    } else {
        prior_path.or(event_path)
    };
    let observed_branch = observation.worktree_branch.as_ref().filter(|branch| {
        !branch.is_empty()
            && observation
                .worktree_path
                .as_ref()
                .is_none_or(|event_path| Some(event_path) == path.as_ref())
    });
    WorktreeProjection {
        path,
        observed_branch: observed_branch.cloned(),
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

fn task_projection(
    observation: &AgentLifecycleObservation,
    prior: Option<&AgentState>,
    is_subagent: bool,
    event_task: Option<String>,
) -> Option<String> {
    if is_subagent {
        event_task.or_else(|| prior.and_then(|p| p.task.clone()))
    } else {
        non_empty_string(observation.task.as_deref())
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
            if observed.pane_id == prior.pane_id && observed.pane_pid.is_none() =>
        {
            Some(prior)
        }
        (Some(observed), _) => Some(observed),
        (None, prior) => prior,
    }
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests;
