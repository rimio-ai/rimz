//! Agent lifecycle ingestion policy and automatic event-log rotation gate.

use std::time::Duration;

use crate::agents::lifecycle::{self, LifecycleEvent, LifecycleSignal, Transition, TransitionKind};
use crate::agents::{
    AgentLifecycleObservation, AgentState, AgentStatus, SessionOrigin, SpawnedSubagent,
};
use crate::disk::paths::StatePaths;
use crate::ids::{AgentKind, AgentSessionId, EventId, LoginName, WorkspaceId};
use crate::store::event::{self, EventEnvelope};
use crate::store::snapshot;
use crate::workspace::record;

use super::{Store, debounce};
use crate::store::Result;

const MIB: u64 = 1024 * 1024;
pub const DEFAULT_EVENT_LOG_ROTATE_BYTES: u64 = 64 * MIB;
const AUTO_ROTATE_DEBOUNCE: Duration = Duration::from_secs(60);
const AUTO_ROTATE_STAMP: &str = "auto-rotate.stamp";

pub struct AgentLifecycleIntent<'a> {
    pub session_name: &'a str,
    pub agent_kind: AgentKind,
    pub event_name: &'a str,
    pub observation: &'a AgentLifecycleObservation,
    pub spawned_subagents: &'a [SpawnedSubagent],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLifecycleReceipt {
    pub prior_status: Option<AgentStatus>,
    pub transition: Option<Transition>,
    pub waiting_cleared: bool,
    pub primary_event_id: Option<EventId>,
    pub events: Vec<LifecycleEvent>,
    pub rotation_due: bool,
    /// Set when the observation belongs to an ephemeral side conversation,
    /// which is never an agent session.
    pub side_conversation: Option<SideConversation>,
}

/// A side conversation's receipt: the root session hosting it, once the
/// side registration has folded and a root proved the instance, and whether
/// that host's own turn is running.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SideConversation {
    pub host: Option<AgentSessionId>,
    pub host_running: bool,
}

struct StagedLifecycleEvent {
    envelope: EventEnvelope,
    event: Option<LifecycleEvent>,
}

impl Store {
    /// Apply lifecycle append policy and report whether the CLI should launch
    /// the existing detached event-log rotation command.
    #[must_use = "durability barrier; check the result"]
    pub fn append_agent_lifecycle(
        &self,
        intent: AgentLifecycleIntent<'_>,
    ) -> Result<AgentLifecycleReceipt> {
        self.append_agent_lifecycle_with_threshold(intent, DEFAULT_EVENT_LOG_ROTATE_BYTES)
    }

    fn append_agent_lifecycle_with_threshold(
        &self,
        intent: AgentLifecycleIntent<'_>,
        rotation_threshold: u64,
    ) -> Result<AgentLifecycleReceipt> {
        self.commit(|txn| {
            let (cache, agents, _resume_outcomes) = snapshot::catch_up_rollup(txn.paths)?;
            let known_side = intent
                .observation
                .agent_id
                .as_ref()
                .and_then(|agent_id| cache.agent_identity.side_session_host(agent_id))
                .map(|host| SideConversation {
                    host_running: host
                        .and_then(|host| find_agent(&agents, &intent.agent_kind, host))
                        .is_some_and(|host| host.status == AgentStatus::Running),
                    host: host.cloned(),
                });
            if known_side.is_some()
                || intent.observation.origin == Some(SessionOrigin::SideConversation)
            {
                let mut receipt = AgentLifecycleReceipt {
                    prior_status: None,
                    transition: None,
                    waiting_cleared: false,
                    primary_event_id: None,
                    events: Vec::new(),
                    rotation_due: false,
                    side_conversation: Some(known_side.clone().unwrap_or_default()),
                };
                if known_side.is_some() {
                    return Ok(receipt);
                }
                let envelope = EventEnvelope::agent_lifecycle(
                    self.inner.paths.workspace_id.clone(),
                    intent.session_name,
                    intent.agent_kind.as_str(),
                    intent.event_name,
                    &event::observation_for_event(intent.observation),
                );
                receipt.primary_event_id = Some(envelope.event_id.clone());
                txn.append_batch(&[envelope])?;
                receipt.rotation_due = claim_rotation(txn.paths, rotation_threshold);
                return Ok(receipt);
            }
            let prior_status = intent
                .observation
                .agent_id
                .as_ref()
                .and_then(|agent_id| find_agent(&agents, &intent.agent_kind, agent_id))
                .map(|agent| agent.status);
            let transition = lifecycle_transition(&agents, &intent.agent_kind, intent.observation);
            // A row this ingress creates carries the room's account, as a launch batch stamps it.
            let creates_row = |agent_id: &AgentSessionId| {
                find_agent(&agents, &intent.agent_kind, agent_id).is_none()
            };
            let login = if intent
                .observation
                .agent_id
                .as_ref()
                .is_some_and(creates_row)
                || intent
                    .spawned_subagents
                    .iter()
                    .any(|child| creates_row(&child.child_agent_id))
            {
                record::read_optional(&txn.paths.workspace_record)?
                    .and_then(|record| record.logins)
                    .and_then(|mut logins| logins.remove(&intent.agent_kind))
                    .filter(|name| !name.is_default())
            } else {
                None
            };
            let append_primary = append_lifecycle_event(
                &intent.observation.signal,
                transition,
                intent.observation.parent_agent_id.is_some(),
            );
            let mut staged = Vec::new();
            let primary_event_id = if append_primary {
                let mut observation = event::observation_for_event(intent.observation);
                if prior_status.is_none() {
                    observation.launch.login.clone_from(&login);
                }
                let envelope = EventEnvelope::agent_lifecycle(
                    self.inner.paths.workspace_id.clone(),
                    intent.session_name,
                    intent.agent_kind.as_str(),
                    intent.event_name,
                    &observation,
                );
                let event_id = envelope.event_id.clone();
                let event = intent.observation.agent_id.as_ref().zip(transition).map(
                    |(agent_id, transition)| {
                        LifecycleEvent::new(
                            event_id.clone(),
                            envelope.timestamp,
                            envelope.workspace_id.clone(),
                            intent.agent_kind.clone(),
                            agent_id.clone(),
                            intent.observation.agent_name.clone(),
                            intent.observation.parent_agent_id.clone(),
                            intent.observation.signal.clone(),
                            prior_status,
                            transition,
                        )
                    },
                );
                staged.push(StagedLifecycleEvent { envelope, event });
                Some(event_id)
            } else {
                None
            };
            derive_lifecycle_events(
                &self.inner.paths.workspace_id,
                &intent,
                &agents,
                transition,
                login.as_ref(),
                &mut staged,
            );
            let envelopes = staged
                .iter()
                .map(|staged| staged.envelope.clone())
                .collect::<Vec<_>>();
            txn.append_batch(&envelopes)?;
            let events = staged
                .into_iter()
                .filter_map(|staged| staged.event)
                .collect();

            let waiting_cleared = transition.is_some_and(|transition| transition.waiting_cleared);
            let rotation_due =
                !envelopes.is_empty() && claim_rotation(txn.paths, rotation_threshold);
            Ok(AgentLifecycleReceipt {
                prior_status,
                transition,
                waiting_cleared,
                primary_event_id,
                events,
                rotation_due,
                side_conversation: None,
            })
        })
    }
}

/// Whether the event log crossed the rotation threshold, claiming the debounce
/// stamp when it did.
fn claim_rotation(paths: &StatePaths, rotation_threshold: u64) -> bool {
    let stamp = paths.locks_dir.join(AUTO_ROTATE_STAMP);
    let due = std::fs::metadata(&paths.events_log)
        .is_ok_and(|metadata| metadata.len() >= rotation_threshold)
        && debounce::stamp_due(&stamp, AUTO_ROTATE_DEBOUNCE);
    if due {
        debounce::touch_stamp(&stamp);
    }
    due
}

fn lifecycle_transition(
    agents: &[AgentState],
    kind: &AgentKind,
    observation: &AgentLifecycleObservation,
) -> Option<Transition> {
    let agent_id = observation.agent_id.as_ref()?;
    let prior = agents
        .iter()
        .find(|agent| agent.kind == *kind && agent.agent_id == *agent_id);
    let previous = prior.map(AgentState::lifecycle);
    Some(lifecycle::step(
        previous.as_ref(),
        prior
            .and_then(|agent| agent.open_ask.as_ref())
            .and_then(|ask| ask.native_key.as_deref()),
        lifecycle::PriorTurnIds {
            started: prior.and_then(|agent| agent.started_turn_id.as_deref()),
            superseded: prior.and_then(|agent| agent.superseded_turn_id.as_deref()),
            interrupted: prior.and_then(|agent| agent.interrupted_turn_id.as_deref()),
        },
        &observation.signal,
    ))
}

fn derive_lifecycle_events(
    workspace_id: &WorkspaceId,
    intent: &AgentLifecycleIntent<'_>,
    agents: &[AgentState],
    primary_transition: Option<Transition>,
    login: Option<&LoginName>,
    staged: &mut Vec<StagedLifecycleEvent>,
) {
    if matches!(
        intent.observation.signal,
        LifecycleSignal::SubagentStarted | LifecycleSignal::SubagentStopped { .. }
    ) && let (Some(child_id), Some(parent_id)) = (
        intent.observation.agent_id.as_ref(),
        intent.observation.parent_agent_id.as_ref(),
    ) && agents.iter().any(|state| {
        state.kind == intent.agent_kind
            && state.agent_id == *child_id
            && state.parent_agent_id.is_none()
    }) {
        append_adoption(
            workspace_id,
            intent,
            agents,
            parent_id,
            intent.observation.clone(),
            intent.observation.signal.clone(),
            primary_transition,
            login,
            staged,
        );
    }

    // A provider can raise a child's native prompt on the parent session. The
    // child's completion of the keyed call is the answer edge, so it clears the
    // parent's wait; any other child tool leaves a real parent ask open.
    if let LifecycleSignal::ToolUsed {
        native_key: Some(key),
        ..
    } = &intent.observation.signal
        && let Some(parent_id) = intent.observation.parent_agent_id.as_ref()
        && let Some(parent) = find_agent(agents, &intent.agent_kind, parent_id)
        && parent.status == AgentStatus::Waiting
        && parent
            .open_ask
            .as_ref()
            .and_then(|ask| ask.native_key.as_ref())
            == Some(key)
    {
        let observation = AgentLifecycleObservation::new(
            Some(parent_id.clone()),
            LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
                name: None,
                native_key: Some(key.clone()),
                turn_id: None,
            },
        );
        let transition = lifecycle_transition(agents, &intent.agent_kind, &observation)
            .expect("derived answer has parent identity");
        push_derived(
            workspace_id,
            intent,
            "SubagentAskAnswered",
            observation,
            Some(parent.status),
            transition,
            staged,
        );
    }

    if intent.observation.parent_agent_id.is_none()
        && matches!(
            intent.observation.signal,
            LifecycleSignal::ToolUsed { .. } | LifecycleSignal::TurnEnded { .. }
        )
        && let Some(parent_id) = intent.observation.agent_id.as_ref()
    {
        for child in intent.spawned_subagents {
            let child_state = find_agent(agents, &intent.agent_kind, &child.child_agent_id);
            let errored = child_state.is_some_and(|state| state.status == AgentStatus::Failed);
            let mut observation = AgentLifecycleObservation::new(
                Some(child.child_agent_id.clone()),
                LifecycleSignal::SubagentStopped { errored },
            );
            observation.agent_name = child.agent_name.clone();
            observation.launch.role = child.role.clone();
            observation.launch.model = child.model.clone();
            observation.task = child.role.clone().or_else(|| child.prompt.clone());
            observation.prompt = child.prompt.clone();
            observation.usage.total_tokens = child.total_tokens;
            observation.pane_id = intent.observation.pane_id.clone();
            if child_state.is_some_and(|state| state.parent_agent_id.is_some()) {
                append_reconciliation(workspace_id, intent, agents, parent_id, observation, staged);
            } else {
                append_adoption(
                    workspace_id,
                    intent,
                    agents,
                    parent_id,
                    observation,
                    LifecycleSignal::SubagentStopped { errored },
                    None,
                    login,
                    staged,
                );
            }
        }
    }
}

fn find_agent<'a>(
    agents: &'a [AgentState],
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> Option<&'a AgentState> {
    agents
        .iter()
        .find(|state| state.kind == *kind && state.agent_id == *agent_id)
}

fn root_parent_id(
    agents: &[AgentState],
    kind: &AgentKind,
    parent_id: &AgentSessionId,
) -> AgentSessionId {
    find_agent(agents, kind, parent_id)
        .and_then(|state| state.parent_agent_id.clone())
        .unwrap_or_else(|| parent_id.clone())
}

fn root_parent_kind(
    agents: &[AgentState],
    kind: &AgentKind,
    parent_id: &AgentSessionId,
) -> AgentKind {
    find_agent(agents, kind, parent_id)
        .and_then(|state| state.parent_agent_kind.clone())
        .unwrap_or_else(|| kind.clone())
}

#[allow(clippy::too_many_arguments)]
fn append_adoption(
    workspace_id: &WorkspaceId,
    intent: &AgentLifecycleIntent<'_>,
    agents: &[AgentState],
    parent_id: &AgentSessionId,
    mut observation: AgentLifecycleObservation,
    signal: LifecycleSignal,
    primary_transition: Option<Transition>,
    login: Option<&LoginName>,
    staged: &mut Vec<StagedLifecycleEvent>,
) {
    let Some(child_id) = observation.agent_id.clone() else {
        return;
    };
    if child_id == *parent_id {
        return;
    }
    let child_state = find_agent(agents, &intent.agent_kind, &child_id);
    if child_state.is_some_and(|state| state.parent_agent_id.is_some())
        || child_state
            .and_then(|state| state.pane.as_ref())
            .is_some_and(|pane| observation.pane_id.as_ref() != Some(&pane.pane_id))
    {
        return;
    }
    observation.signal = signal;
    if child_state.is_none() {
        observation.launch.login = login.cloned();
    }
    let parent_kind = root_parent_kind(agents, &intent.agent_kind, parent_id);
    observation.parent_agent_id = Some(root_parent_id(agents, &intent.agent_kind, parent_id));
    if parent_kind != intent.agent_kind {
        observation.launch.parent_agent_kind = Some(parent_kind);
    }
    let transition = primary_transition.map_or_else(
        || {
            lifecycle_transition(agents, &intent.agent_kind, &observation)
                .expect("derived adoption has child identity")
        },
        |primary| {
            lifecycle::step(
                Some(&primary.next),
                None,
                lifecycle::PriorTurnIds::default(),
                &observation.signal,
            )
        },
    );
    let prior_status = primary_transition
        .map(|primary| primary.next.status)
        .or_else(|| child_state.map(|state| state.status));
    push_derived(
        workspace_id,
        intent,
        "SubagentAdopted",
        observation,
        prior_status,
        transition,
        staged,
    );
}

fn append_reconciliation(
    workspace_id: &WorkspaceId,
    intent: &AgentLifecycleIntent<'_>,
    agents: &[AgentState],
    parent_id: &AgentSessionId,
    mut observation: AgentLifecycleObservation,
    staged: &mut Vec<StagedLifecycleEvent>,
) {
    let Some(child_id) = observation.agent_id.clone() else {
        return;
    };
    let root_parent_id = root_parent_id(agents, &intent.agent_kind, parent_id);
    let Some(child_state) = find_agent(agents, &intent.agent_kind, &child_id) else {
        return;
    };
    if child_state.parent_agent_id.as_ref() != Some(&root_parent_id)
        || child_state
            .pane
            .as_ref()
            .is_some_and(|pane| observation.pane_id.as_ref() != Some(&pane.pane_id))
    {
        return;
    }
    let model_changed = observation
        .launch
        .model
        .as_ref()
        .is_some_and(|model| child_state.model.as_ref() != Some(model));
    let tokens_changed = observation
        .usage
        .total_tokens
        .is_some_and(|tokens| child_state.usage.total_tokens != Some(tokens));
    // Provider-settled truth closes a child the rollup still holds running,
    // even when the provider has no new metadata to carry with the close.
    // Once terminal, the metadata delta remains the reconciliation dedupe.
    if child_state.status != AgentStatus::Running && !model_changed && !tokens_changed {
        return;
    }
    observation.agent_name = None;
    observation.launch.role = None;
    observation.task = None;
    observation.prompt = None;
    observation.parent_agent_id = Some(root_parent_id);
    let parent_kind = root_parent_kind(agents, &intent.agent_kind, parent_id);
    if parent_kind != intent.agent_kind {
        observation.launch.parent_agent_kind = Some(parent_kind);
    }
    let errored = child_state.status == AgentStatus::Failed;
    observation.signal = LifecycleSignal::SubagentStopped { errored };
    let transition = lifecycle_transition(agents, &intent.agent_kind, &observation)
        .expect("derived reconciliation has child identity");
    let prior_status = Some(child_state.status);
    push_derived(
        workspace_id,
        intent,
        "SubagentReconciled",
        observation,
        prior_status,
        transition,
        staged,
    );
}

#[allow(clippy::too_many_arguments)]
fn push_derived(
    workspace_id: &WorkspaceId,
    intent: &AgentLifecycleIntent<'_>,
    event_name: &'static str,
    observation: AgentLifecycleObservation,
    prior_status: Option<AgentStatus>,
    transition: Transition,
    staged: &mut Vec<StagedLifecycleEvent>,
) {
    let agent_id = observation
        .agent_id
        .clone()
        .expect("derived lifecycle event has child identity");
    let parent_agent_id = observation.parent_agent_id.clone();
    let agent_name = observation.agent_name.clone();
    let signal = observation.signal.clone();
    let envelope = EventEnvelope::agent_lifecycle(
        workspace_id.clone(),
        intent.session_name,
        intent.agent_kind.as_str(),
        event_name,
        &observation,
    );
    let event = LifecycleEvent::new(
        envelope.event_id.clone(),
        envelope.timestamp,
        envelope.workspace_id.clone(),
        intent.agent_kind.clone(),
        agent_id,
        agent_name,
        parent_agent_id,
        signal,
        prior_status,
        transition,
    );
    staged.push(StagedLifecycleEvent {
        envelope,
        event: Some(event),
    });
}

fn proof_of_work_tool(signal: &LifecycleSignal) -> bool {
    matches!(
        signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            name: None,
            ..
        }
    )
}

fn append_lifecycle_event(
    signal: &LifecycleSignal,
    transition: Option<Transition>,
    child_owned: bool,
) -> bool {
    child_owned
        || !proof_of_work_tool(signal)
        || transition.is_some_and(|transition| {
            transition.compaction_closed
                || transition.waiting_cleared
                || matches!(transition.kind, TransitionKind::Reconciled { .. })
        })
}

#[cfg(test)]
#[path = "lifecycle/rotation_tests.rs"]
mod rotation_tests;

#[cfg(test)]
#[path = "lifecycle/tests.rs"]
mod tests;
