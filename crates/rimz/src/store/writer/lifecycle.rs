//! Agent lifecycle ingestion policy and automatic event-log rotation gate.

use std::time::Duration;

use crate::agents::lifecycle::{self, LifecycleEvent, LifecycleSignal, Transition, TransitionKind};
use crate::agents::{
    AgentLifecycleObservation, AgentState, AgentStatus, SessionOrigin, SpawnedSubagent,
};
use crate::disk::paths::StatePaths;
use crate::ids::{AgentKind, AgentSessionId, EventId, LoginName, WorkspaceId};
use crate::pane::{PaneRef, RuntimeOwner};
use crate::store::event::{self, EventEnvelope};
use crate::store::{
    session_death,
    snapshot::{self, find_agent},
};
use crate::workspace::record;

use super::{Store, debounce};
use crate::store::Result;

pub use crate::disk::retention::DEFAULT_EVENT_LOG_ROTATE_BYTES;
const AUTO_ROTATE_DEBOUNCE: Duration = Duration::from_secs(60);
const AUTO_ROTATE_STAMP: &str = "auto-rotate.stamp";

pub struct AgentLifecycleIntent<'a> {
    pub session_name: &'a str,
    pub agent_kind: AgentKind,
    pub event_name: &'a str,
    pub observation: &'a AgentLifecycleObservation,
    pub spawned_subagents: &'a [SpawnedSubagent],
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
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

/// A side conversation's receipt: the card owner hosting it, resolved at this
/// hook when a live root proves the instance, and whether that host's own turn
/// is running.
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
                .is_some_and(|agent_id| cache.agent_identity.is_side_session(agent_id));
            let mut staged = Vec::new();
            let mut receipt = if known_side
                || intent.observation.origin == Some(SessionOrigin::SideConversation)
            {
                let host = session_death::side_conversation_host(
                    &agents,
                    &intent.agent_kind,
                    intent.observation,
                );
                let mut receipt = AgentLifecycleReceipt {
                    prior_status: None,
                    transition: None,
                    waiting_cleared: false,
                    primary_event_id: None,
                    events: Vec::new(),
                    rotation_due: false,
                    side_conversation: Some(SideConversation {
                        host: host.map(|host| host.agent_id.clone()),
                        host_running: host.is_some_and(|host| host.status == AgentStatus::Running),
                    }),
                };
                if known_side {
                    return Ok(receipt);
                }
                receipt.primary_event_id = Some(stage(
                    &self.inner.paths.workspace_id,
                    &intent,
                    intent.event_name,
                    &event::observation_for_event(intent.observation),
                    None,
                    None,
                    &mut staged,
                ));
                receipt
            } else {
                let prior_status = intent
                    .observation
                    .agent_id
                    .as_ref()
                    .and_then(|agent_id| find_agent(&agents, &intent.agent_kind, agent_id))
                    .map(|agent| agent.status);
                let transition =
                    lifecycle_transition(&agents, &intent.agent_kind, intent.observation);
                // Only hook-first roots take the room default; launched rows and children keep their account.
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
                let primary_event_id = if append_primary {
                    let mut observation = event::observation_for_event(intent.observation);
                    if prior_status.is_none() {
                        observation.launch.login = ingress_login(
                            &agents,
                            &intent.agent_kind,
                            &observation,
                            login.as_ref(),
                        );
                    }
                    Some(stage(
                        &self.inner.paths.workspace_id,
                        &intent,
                        intent.event_name,
                        &observation,
                        prior_status,
                        transition,
                        &mut staged,
                    ))
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
                AgentLifecycleReceipt {
                    prior_status,
                    transition,
                    waiting_cleared: transition
                        .is_some_and(|transition| transition.waiting_cleared),
                    primary_event_id,
                    events: Vec::new(),
                    rotation_due: false,
                    side_conversation: None,
                }
            };
            let envelopes = staged
                .iter()
                .map(|staged| staged.envelope.clone())
                .collect::<Vec<_>>();
            txn.append_batch(&envelopes)?;
            receipt.rotation_due =
                !staged.is_empty() && claim_rotation(txn.paths, rotation_threshold);
            receipt.events = staged
                .into_iter()
                .filter_map(|staged| staged.event)
                .collect();

            Ok(receipt)
        })
    }
}

/// Whether the event log crossed the rotation threshold, claiming the debounce
/// stamp when it did.
fn claim_rotation(paths: &StatePaths, rotation_threshold: u64) -> bool {
    let stamp = paths.cache_dir.join(AUTO_ROTATE_STAMP);
    std::fs::metadata(&paths.events_log).is_ok_and(|metadata| metadata.len() >= rotation_threshold)
        && debounce::claim(&stamp, AUTO_ROTATE_DEBOUNCE)
}

fn lifecycle_transition(
    agents: &[AgentState],
    kind: &AgentKind,
    observation: &AgentLifecycleObservation,
) -> Option<Transition> {
    let agent_id = observation.agent_id.as_ref()?;
    let prior = find_agent(agents, kind, agent_id);
    Some(AgentState::transition(
        prior,
        &observation.signal,
        observation
            .prompt
            .as_deref()
            .is_some_and(crate::store::message::prompt_is_keepalive_only),
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
        stage(
            workspace_id,
            intent,
            "SubagentAskAnswered",
            &observation,
            Some(parent.status),
            Some(transition),
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
            observation.prompt = crate::agents::SanitizedPrompt::new(child.prompt.as_deref());
            observation.usage = child.usage.clone();
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

    if intent.observation.parent_agent_id.is_none()
        && !matches!(intent.observation.signal, LifecycleSignal::Ended)
        && let Some(agent_id) = intent.observation.agent_id.as_ref()
    {
        let (pane, owner) = session_death::observed_placement(intent.observation);
        for forked in agents.iter().filter(|agent| {
            forked_from_resumed(
                agent,
                (agent.pane.as_ref(), agent.runtime_owner.as_ref()),
                &intent.agent_kind,
                agent_id,
                (pane.as_ref(), owner.as_ref()),
            )
        }) {
            stage(
                workspace_id,
                intent,
                "ReapedSuperseded",
                &AgentLifecycleObservation::new(
                    Some(forked.agent_id.clone()),
                    LifecycleSignal::Ended,
                ),
                None,
                None,
                staged,
            );
        }
    }
}

/// The supersession an attach completes for the card it binds to `pane` and `owner`. The exec
/// wrapper attaches a resumed card's spawned provider after the spawn, so a provider that forked
/// and registered first spoke before its process named the card's instance.
pub(super) fn superseded_on_attach(
    workspace_id: &WorkspaceId,
    session_name: &str,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    pane: &PaneRef,
    owner: &RuntimeOwner,
    agents: &[AgentState],
) -> Option<EventEnvelope> {
    let card = find_agent(agents, kind, agent_id)?;
    agents
        .iter()
        .any(|speaker| {
            speaker.parent_agent_id.is_none()
                && speaker.ended_at.is_none()
                && speaker.resumed_at.is_none()
                && forked_from_resumed(
                    card,
                    (Some(pane), Some(owner)),
                    &speaker.kind,
                    &speaker.agent_id,
                    (speaker.pane.as_ref(), speaker.runtime_owner.as_ref()),
                )
        })
        .then(|| {
            EventEnvelope::agent_lifecycle(
                workspace_id.clone(),
                session_name,
                kind.as_str(),
                "ReapedSuperseded",
                &AgentLifecycleObservation::new(Some(agent_id.clone()), LifecycleSignal::Ended),
            )
        })
}

/// Whether root session `speaker` of `kind`, placed at `speaker_at`, ends `resumed`, placed at
/// `resumed_at`. The exec wrapper's resume stamp revives the card it resumed until that card's
/// provider speaks; a provider that forks on resume speaks as another session on the same pane
/// and agent process instead, and that ends the revival rather than the next debounced reap.
fn forked_from_resumed(
    resumed: &AgentState,
    resumed_at: (Option<&PaneRef>, Option<&RuntimeOwner>),
    kind: &AgentKind,
    speaker: &AgentSessionId,
    speaker_at: (Option<&PaneRef>, Option<&RuntimeOwner>),
) -> bool {
    resumed.kind == *kind
        && resumed.agent_id != *speaker
        && resumed.parent_agent_id.is_none()
        && resumed.ended_at.is_none()
        && resumed.resumed_at.is_some()
        && session_death::same_instance_placement(resumed_at, speaker_at)
}

fn root_parent(
    agents: &[AgentState],
    kind: &AgentKind,
    parent_id: &AgentSessionId,
) -> (AgentSessionId, AgentKind) {
    let parent = find_agent(agents, kind, parent_id);
    (
        parent
            .and_then(|state| state.parent_agent_id.clone())
            .unwrap_or_else(|| parent_id.clone()),
        parent
            .and_then(|state| state.parent_agent_kind.clone())
            .unwrap_or_else(|| kind.clone()),
    )
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
        observation.launch.login = find_agent(agents, &intent.agent_kind, parent_id)
            .map_or_else(|| login.cloned(), |parent| parent.login.clone());
    }
    let (parent_id, parent_kind) = root_parent(agents, &intent.agent_kind, parent_id);
    observation.parent_agent_id = Some(parent_id);
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
    stage(
        workspace_id,
        intent,
        "SubagentAdopted",
        &observation,
        prior_status,
        Some(transition),
        staged,
    );
}

fn ingress_login(
    agents: &[AgentState],
    kind: &AgentKind,
    observation: &AgentLifecycleObservation,
    default: Option<&LoginName>,
) -> Option<LoginName> {
    if let Some(parent_id) = observation.parent_agent_id.as_ref() {
        let parent_kind = observation
            .launch
            .parent_agent_kind
            .as_ref()
            .unwrap_or(kind);
        if let Some(parent) = find_agent(agents, parent_kind.as_str(), parent_id)
            .filter(|parent| parent.kind == *kind)
        {
            return parent.login.clone();
        }
        return default.cloned();
    }
    let adopts_launch = agents.iter().any(|agent| {
        agent.kind == *kind
            && agent.agent_id.is_provisional()
            && (observation
                .agent_name
                .as_ref()
                .is_some_and(|name| agent.name.as_ref() == Some(name))
                || observation.pane_id.as_ref().is_some_and(|pane| {
                    agent
                        .pane
                        .as_ref()
                        .is_some_and(|bound| &bound.pane_id == pane)
                }))
    });
    if adopts_launch {
        None
    } else {
        default.cloned()
    }
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
    let (root_parent_id, parent_kind) = root_parent(agents, &intent.agent_kind, parent_id);
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
    let mut merged_usage = observation.usage.merge(Some(&child_state.usage), None);
    // A recomputed gauge percentage is not new provider token metadata.
    merged_usage.context_pct = child_state.usage.context_pct;
    let tokens_changed = merged_usage != child_state.usage;
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
    if parent_kind != intent.agent_kind {
        observation.launch.parent_agent_kind = Some(parent_kind);
    }
    let errored = child_state.status == AgentStatus::Failed;
    observation.signal = LifecycleSignal::SubagentStopped { errored };
    let transition = lifecycle_transition(agents, &intent.agent_kind, &observation)
        .expect("derived reconciliation has child identity");
    let prior_status = Some(child_state.status);
    stage(
        workspace_id,
        intent,
        "SubagentReconciled",
        &observation,
        prior_status,
        Some(transition),
        staged,
    );
}

fn stage(
    workspace_id: &WorkspaceId,
    intent: &AgentLifecycleIntent<'_>,
    event_name: &str,
    observation: &AgentLifecycleObservation,
    prior_status: Option<AgentStatus>,
    transition: Option<Transition>,
    staged: &mut Vec<StagedLifecycleEvent>,
) -> EventId {
    let envelope = EventEnvelope::agent_lifecycle(
        workspace_id.clone(),
        intent.session_name,
        intent.agent_kind.as_str(),
        event_name,
        observation,
    );
    let event_id = envelope.event_id.clone();
    let event = observation
        .agent_id
        .as_ref()
        .zip(transition)
        .map(|(agent_id, transition)| {
            LifecycleEvent::new(
                event_id.clone(),
                envelope.timestamp,
                envelope.workspace_id.clone(),
                intent.agent_kind.clone(),
                agent_id.clone(),
                observation.agent_name.clone(),
                observation.parent_agent_id.clone(),
                observation.signal.clone(),
                prior_status,
                transition,
            )
        });
    staged.push(StagedLifecycleEvent { envelope, event });
    event_id
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
