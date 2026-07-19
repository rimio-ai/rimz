//! Agent lifecycle ingestion policy and automatic event-log rotation gate.

use std::time::Duration;

use crate::agents::lifecycle::{self, LifecycleSignal, Transition, TransitionKind};
use crate::agents::{AgentLifecycleObservation, AgentState, AgentStatus, SpawnedSubagent};
use crate::ids::{AgentKind, AgentSessionId, EventId, WorkspaceId};
use crate::store::event::EventEnvelope;
use crate::store::snapshot;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleAppendOutcome {
    Suppressed,
    Appended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DerivedLifecycleKind {
    Adoption,
    Reconciliation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivedLifecycleOutcome {
    pub kind: DerivedLifecycleKind,
    pub agent_id: AgentSessionId,
    pub parent_agent_id: Option<AgentSessionId>,
    pub event_name: &'static str,
    pub signal: LifecycleSignal,
    pub event_id: EventId,
    pub transition: Transition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLifecycleReceipt {
    pub prior_status: Option<AgentStatus>,
    pub transition: Option<Transition>,
    pub waiting_cleared: bool,
    pub append: LifecycleAppendOutcome,
    pub primary_event_id: Option<EventId>,
    pub derived: Vec<DerivedLifecycleOutcome>,
    pub rotation_due: bool,
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
            let (_cache, agents, _resume_outcomes) = snapshot::catch_up_rollup(txn.paths)?;
            let prior_status = intent
                .observation
                .agent_id
                .as_ref()
                .and_then(|agent_id| find_agent(&agents, &intent.agent_kind, agent_id))
                .map(|agent| agent.status);
            let transition = lifecycle_transition(&agents, &intent.agent_kind, intent.observation);
            let append = if append_lifecycle_event(
                &intent.observation.signal,
                transition,
                intent.observation.parent_agent_id.is_some(),
            ) {
                LifecycleAppendOutcome::Appended
            } else {
                LifecycleAppendOutcome::Suppressed
            };
            let mut envelopes = Vec::new();
            let primary_event_id = if append == LifecycleAppendOutcome::Appended {
                let observation = event_lifecycle_observation(intent.observation);
                let envelope = EventEnvelope::agent_lifecycle(
                    self.inner.paths.workspace_id.clone(),
                    intent.session_name,
                    intent.agent_kind.as_str(),
                    intent.event_name,
                    &observation,
                );
                let event_id = envelope.event_id.clone();
                envelopes.push(envelope);
                Some(event_id)
            } else {
                None
            };
            let derived = derive_lifecycle_events(
                &self.inner.paths.workspace_id,
                &intent,
                &agents,
                transition,
                &mut envelopes,
            );
            txn.append_batch(&envelopes)?;

            let waiting_cleared = transition.is_some_and(|transition| transition.waiting_cleared);
            if envelopes.is_empty() {
                return Ok(AgentLifecycleReceipt {
                    prior_status,
                    transition,
                    waiting_cleared,
                    append,
                    primary_event_id,
                    derived,
                    rotation_due: false,
                });
            }
            let Ok(metadata) = std::fs::metadata(&txn.paths.events_log) else {
                return Ok(AgentLifecycleReceipt {
                    prior_status,
                    transition,
                    waiting_cleared,
                    append,
                    primary_event_id,
                    derived,
                    rotation_due: false,
                });
            };
            let stamp = txn.paths.locks_dir.join(AUTO_ROTATE_STAMP);
            let rotation_due = metadata.len() >= rotation_threshold
                && debounce::stamp_due(&stamp, AUTO_ROTATE_DEBOUNCE);
            if rotation_due {
                debounce::touch_stamp(&stamp);
            }
            Ok(AgentLifecycleReceipt {
                prior_status,
                transition,
                waiting_cleared,
                append,
                primary_event_id,
                derived,
                rotation_due,
            })
        })
    }
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
        &observation.signal,
    ))
}

fn derive_lifecycle_events(
    workspace_id: &WorkspaceId,
    intent: &AgentLifecycleIntent<'_>,
    agents: &[AgentState],
    primary_transition: Option<Transition>,
    envelopes: &mut Vec<EventEnvelope>,
) -> Vec<DerivedLifecycleOutcome> {
    let mut outcomes = Vec::new();
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
            envelopes,
            &mut outcomes,
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
                append_reconciliation(
                    workspace_id,
                    intent,
                    agents,
                    parent_id,
                    observation,
                    envelopes,
                    &mut outcomes,
                );
            } else {
                append_adoption(
                    workspace_id,
                    intent,
                    agents,
                    parent_id,
                    observation,
                    LifecycleSignal::SubagentStopped { errored },
                    None,
                    envelopes,
                    &mut outcomes,
                );
            }
        }
    }
    outcomes
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

#[allow(clippy::too_many_arguments)]
fn append_adoption(
    workspace_id: &WorkspaceId,
    intent: &AgentLifecycleIntent<'_>,
    agents: &[AgentState],
    parent_id: &AgentSessionId,
    mut observation: AgentLifecycleObservation,
    signal: LifecycleSignal,
    primary_transition: Option<Transition>,
    envelopes: &mut Vec<EventEnvelope>,
    outcomes: &mut Vec<DerivedLifecycleOutcome>,
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
    observation.parent_agent_id = Some(root_parent_id(agents, &intent.agent_kind, parent_id));
    let transition = primary_transition.map_or_else(
        || {
            lifecycle_transition(agents, &intent.agent_kind, &observation)
                .expect("derived adoption has child identity")
        },
        |primary| lifecycle::step(Some(&primary.next), None, &observation.signal),
    );
    push_derived(
        workspace_id,
        intent,
        "SubagentAdopted",
        observation,
        DerivedLifecycleKind::Adoption,
        transition,
        envelopes,
        outcomes,
    );
}

fn append_reconciliation(
    workspace_id: &WorkspaceId,
    intent: &AgentLifecycleIntent<'_>,
    agents: &[AgentState],
    parent_id: &AgentSessionId,
    mut observation: AgentLifecycleObservation,
    envelopes: &mut Vec<EventEnvelope>,
    outcomes: &mut Vec<DerivedLifecycleOutcome>,
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
    if !model_changed && !tokens_changed {
        return;
    }
    observation.agent_name = None;
    observation.launch.role = None;
    observation.task = None;
    observation.prompt = None;
    observation.parent_agent_id = Some(root_parent_id);
    let errored = child_state.status == AgentStatus::Failed;
    observation.signal = LifecycleSignal::SubagentStopped { errored };
    let transition = lifecycle_transition(agents, &intent.agent_kind, &observation)
        .expect("derived reconciliation has child identity");
    push_derived(
        workspace_id,
        intent,
        "SubagentReconciled",
        observation,
        DerivedLifecycleKind::Reconciliation,
        transition,
        envelopes,
        outcomes,
    );
}

#[allow(clippy::too_many_arguments)]
fn push_derived(
    workspace_id: &WorkspaceId,
    intent: &AgentLifecycleIntent<'_>,
    event_name: &'static str,
    observation: AgentLifecycleObservation,
    kind: DerivedLifecycleKind,
    transition: Transition,
    envelopes: &mut Vec<EventEnvelope>,
    outcomes: &mut Vec<DerivedLifecycleOutcome>,
) {
    let agent_id = observation
        .agent_id
        .clone()
        .expect("derived lifecycle event has child identity");
    let parent_agent_id = observation.parent_agent_id.clone();
    let signal = observation.signal.clone();
    let envelope = EventEnvelope::agent_lifecycle(
        workspace_id.clone(),
        intent.session_name,
        intent.agent_kind.as_str(),
        event_name,
        &observation,
    );
    let event_id = envelope.event_id.clone();
    envelopes.push(envelope);
    outcomes.push(DerivedLifecycleOutcome {
        kind,
        agent_id,
        parent_agent_id,
        event_name,
        signal,
        event_id,
        transition,
    });
}

fn proof_of_work_tool(signal: &LifecycleSignal) -> bool {
    matches!(
        signal,
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
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

fn event_lifecycle_observation(
    observation: &AgentLifecycleObservation,
) -> AgentLifecycleObservation {
    let mut trimmed = observation.clone();
    if observation.signal.establishes_identity() || observation.parent_agent_id.is_some() {
        return trimmed;
    }
    if !matches!(observation.signal, LifecycleSignal::TurnEnded { .. }) {
        trimmed.transcript_path = None;
    }
    trimmed.worktree_path = None;
    trimmed.worktree_branch = None;
    trimmed.launch.role = None;
    trimmed.launch.team = None;
    trimmed.launch.channel = None;
    trimmed.launch.profile = None;
    trimmed
}

#[cfg(test)]
fn auto_rotation_due(log_len: u64, stamp_age: Option<Duration>) -> bool {
    auto_rotation_due_at(log_len, DEFAULT_EVENT_LOG_ROTATE_BYTES, stamp_age)
}

#[cfg(test)]
fn auto_rotation_due_at(log_len: u64, threshold: u64, stamp_age: Option<Duration>) -> bool {
    log_len >= threshold && stamp_age.is_none_or(|age| age >= AUTO_ROTATE_DEBOUNCE)
}

#[cfg(test)]
#[path = "lifecycle/rotation_tests.rs"]
mod rotation_tests;

#[cfg(test)]
mod tests {
    use std::fs::{FileTimes, OpenOptions};
    use std::time::SystemTime;

    use super::*;
    use crate::agents::lifecycle::{LifecycleState, TurnPhase};
    use crate::agents::{AgentStatus, LaunchParams};
    use crate::ids::{AgentSessionId, MuxName, PaneId, WorkspaceId};
    use crate::store::event::EventKind;
    use crate::store::{RuntimePaths, StatePaths};

    fn transition(kind: TransitionKind, compaction_closed: bool) -> Transition {
        Transition {
            next: LifecycleState {
                status: AgentStatus::Running,
                phase: TurnPhase::Reasoning,
                compacting: false,
            },
            kind,
            compaction_closed,
            waiting_cleared: false,
            opened_turn: false,
        }
    }

    pub(super) fn test_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
        let runtime = RuntimePaths::under(workspace_id, dir.path()).expect("runtime paths");
        let store = Store::open(paths, runtime).expect("open store");
        (dir, store)
    }

    pub(super) fn observation(signal: LifecycleSignal) -> AgentLifecycleObservation {
        AgentLifecycleObservation::new(Some(AgentSessionId::from("sess-1")), signal)
    }

    #[test]
    fn lifecycle_append_gate_keeps_durable_truth_for_progress_signals() {
        let proof = LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            native_key: None,
        };
        let mutating = LifecycleSignal::ToolUsed {
            mutates: true,
            edits: false,
            native_key: None,
        };

        assert!(append_lifecycle_event(&mutating, None, false));
        assert!(!append_lifecycle_event(&proof, None, false));
        assert!(append_lifecycle_event(&proof, None, true));
        assert!(!append_lifecycle_event(
            &proof,
            Some(transition(TransitionKind::Normal, false)),
            false,
        ));
        assert!(append_lifecycle_event(
            &proof,
            Some(transition(
                TransitionKind::Reconciled {
                    from: AgentStatus::Idle,
                    reason: "tool used outside a running turn",
                },
                false,
            )),
            false,
        ));
        assert!(append_lifecycle_event(
            &proof,
            Some(transition(TransitionKind::Normal, true)),
            false,
        ));
        let mut clears_waiting = transition(TransitionKind::Normal, false);
        clears_waiting.waiting_cleared = true;
        assert!(append_lifecycle_event(&proof, Some(clears_waiting), false));
    }

    #[test]
    fn lifecycle_event_observation_trims_only_serialized_carry_forward_fields() {
        let mut full = observation(LifecycleSignal::Registered);
        full.transcript_path = Some("/tmp/transcript.jsonl".to_owned());
        full.worktree_path = Some("/tmp/project".to_owned());
        full.worktree_branch = Some("feature".to_owned());
        full.launch = LaunchParams {
            role: Some("coder".to_owned()),
            team: Some("forge".to_owned()),
            channel: Some("event-log".to_owned()),
            profile: Some("claude-coder".to_owned()),
            ..LaunchParams::default()
        };
        full.pane_id = Some(PaneId::from_parts(MuxName::Tmux, "%1"));
        let original = full.clone();

        assert_eq!(event_lifecycle_observation(&full), full);
        full.signal = LifecycleSignal::TurnStarted;
        let trimmed = event_lifecycle_observation(&full);
        assert!(trimmed.transcript_path.is_none());
        assert!(trimmed.worktree_path.is_none());
        assert!(trimmed.worktree_branch.is_none());
        assert!(trimmed.launch.role.is_none());
        assert!(trimmed.launch.team.is_none());
        assert!(trimmed.launch.channel.is_none());
        assert!(trimmed.launch.profile.is_none());
        assert_eq!(trimmed.pane_id.as_ref().map(PaneId::raw), Some("%1"));
        assert_eq!(
            full.transcript_path.as_deref(),
            Some("/tmp/transcript.jsonl")
        );

        full.signal = LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        };
        assert_eq!(
            event_lifecycle_observation(&full)
                .transcript_path
                .as_deref(),
            Some("/tmp/transcript.jsonl")
        );
        assert_eq!(original.signal, LifecycleSignal::Registered);
    }

    #[test]
    fn lifecycle_append_outcomes_cover_suppressed_and_appended() {
        let (_dir, store) = test_store();
        let started = observation(LifecycleSignal::TurnStarted);
        let appended = store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "UserPromptSubmit",
                observation: &started,
                spawned_subagents: &[],
            })
            .expect("append lifecycle event");
        assert_eq!(appended.append, LifecycleAppendOutcome::Appended);

        let proof = observation(LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            native_key: None,
        });
        let before = std::fs::metadata(&store.paths().events_log)
            .map(|meta| meta.len())
            .unwrap_or(0);
        let suppressed = store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "PreToolUse",
                observation: &proof,
                spawned_subagents: &[],
            })
            .expect("suppress proof-only event");
        assert_eq!(suppressed.append, LifecycleAppendOutcome::Suppressed);
        assert_eq!(
            std::fs::metadata(&store.paths().events_log)
                .map(|meta| meta.len())
                .unwrap_or(0),
            before
        );
        assert!(!store.paths().locks_dir.join(AUTO_ROTATE_STAMP).exists());
        let events = store.read_events().expect("read events");
        assert_eq!(events.len(), 1);
        let EventKind::AgentLifecycle(payload) = events[0].kind() else {
            panic!("agent lifecycle event")
        };
        assert_eq!(payload.event_name.as_deref(), Some("UserPromptSubmit"));
    }

    #[test]
    fn read_only_tool_uses_latest_durable_waiting_state() {
        let (_dir, store) = test_store();
        let kind = AgentKind::new_unchecked("claude");
        for (event_name, signal) in [
            ("UserPromptSubmit", LifecycleSignal::TurnStarted),
            (
                "PermissionRequest",
                LifecycleSignal::AwaitingInput {
                    ask_id: None,
                    kind: crate::agents::AskKind::Permission,
                    detail: None,
                    native_key: None,
                },
            ),
        ] {
            let observation = observation(signal);
            store
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "rimz-test",
                    agent_kind: kind.clone(),
                    event_name,
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .expect("seed lifecycle state");
        }

        let proof = observation(LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            native_key: None,
        });
        let receipt = store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: kind,
                event_name: "PostToolUse",
                observation: &proof,
                spawned_subagents: &[],
            })
            .expect("record waiting-clearing proof");

        assert_eq!(receipt.prior_status, Some(AgentStatus::Waiting));
        assert!(receipt.waiting_cleared);
        assert_eq!(receipt.append, LifecycleAppendOutcome::Appended);
        assert_eq!(
            store.snapshot_cached().unwrap().agents[0].status,
            AgentStatus::Running
        );
    }

    #[test]
    fn observed_child_adoption_is_guarded_flattened_and_ordered() {
        let (_dir, store) = test_store();
        let kind = AgentKind::new_unchecked("pi");
        let pane = PaneId::from_parts(MuxName::Tmux, "%1");
        let foreign_pane = PaneId::from_parts(MuxName::Tmux, "%2");
        let append = |event_name: &str, observation: &AgentLifecycleObservation| {
            store
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "rimz-test",
                    agent_kind: kind.clone(),
                    event_name,
                    observation,
                    spawned_subagents: &[],
                })
                .expect("append lifecycle")
        };

        let mut root = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("root")),
            LifecycleSignal::Registered,
        );
        root.pane_id = Some(pane.clone());
        append("SessionStart", &root);
        let mut nested = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("nested")),
            LifecycleSignal::SubagentStarted,
        );
        nested.parent_agent_id = Some(AgentSessionId::from("root"));
        nested.task = Some("nested parent".to_owned());
        nested.pane_id = Some(pane.clone());
        append("SubagentStart", &nested);

        let mut child = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("child")),
            LifecycleSignal::Registered,
        );
        child.pane_id = Some(pane.clone());
        append("SessionStart", &child);
        let mut observed = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("child")),
            LifecycleSignal::SubagentStarted,
        );
        observed.parent_agent_id = Some(AgentSessionId::from("nested"));
        observed.task = Some("adopt me".to_owned());
        observed.pane_id = Some(pane.clone());
        let receipt = append("SubagentStart", &observed);
        assert_eq!(receipt.derived.len(), 1);
        assert_eq!(receipt.derived[0].kind, DerivedLifecycleKind::Adoption);
        let events = store.read_events().unwrap();
        let names = events[events.len() - 2..]
            .iter()
            .map(|event| match event.kind() {
                EventKind::AgentLifecycle(payload) => payload.event_name.unwrap_or_default(),
                _ => String::new(),
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["SubagentStart", "SubagentAdopted"]);
        assert_eq!(
            store
                .snapshot_cached()
                .unwrap()
                .agents
                .iter()
                .find(|state| state.agent_id == "child")
                .and_then(|state| state.parent_agent_id.as_deref()),
            Some("root")
        );
        assert!(append("SubagentStart", &observed).derived.is_empty());

        let mut foreign = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("foreign")),
            LifecycleSignal::Registered,
        );
        foreign.pane_id = Some(foreign_pane);
        append("SessionStart", &foreign);
        foreign.signal = LifecycleSignal::SubagentStarted;
        foreign.parent_agent_id = Some(AgentSessionId::from("nested"));
        foreign.task = Some("wrong pane".to_owned());
        foreign.pane_id = Some(pane);
        assert!(append("SubagentStart", &foreign).derived.is_empty());
        assert_eq!(
            store
                .snapshot_cached()
                .unwrap()
                .agents
                .iter()
                .find(|state| state.agent_id == "foreign")
                .and_then(|state| state.parent_agent_id.as_ref()),
            None
        );
    }

    #[test]
    fn spawned_child_reconciliation_is_metadata_sensitive_and_idempotent() {
        let (_dir, store) = test_store();
        let kind = AgentKind::new_unchecked("copilot");
        let pane = PaneId::from_parts(MuxName::Tmux, "%1");
        let mut parent = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("parent")),
            LifecycleSignal::TurnStarted,
        );
        parent.pane_id = Some(pane.clone());
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: kind.clone(),
                event_name: "TurnStart",
                observation: &parent,
                spawned_subagents: &[],
            })
            .unwrap();
        let mut child = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("child")),
            LifecycleSignal::SubagentStopped { errored: false },
        );
        child.parent_agent_id = Some(AgentSessionId::from("parent"));
        child.task = Some("child task".to_owned());
        child.launch.model = Some("old-model".to_owned());
        child.usage.total_tokens = Some(10);
        child.pane_id = Some(pane);
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: kind.clone(),
                event_name: "SubagentStop",
                observation: &child,
                spawned_subagents: &[],
            })
            .unwrap();

        parent.signal = LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            native_key: None,
        };
        let spawned = |model: &str, total_tokens| SpawnedSubagent {
            child_agent_id: AgentSessionId::from("child"),
            agent_name: Some("child".to_owned()),
            role: Some("coder".to_owned()),
            prompt: Some("child task".to_owned()),
            model: Some(model.to_owned()),
            total_tokens: Some(total_tokens),
        };
        for (facts, expected) in [
            (spawned("old-model", 10), 0),
            (spawned("new-model", 20), 1),
            (spawned("new-model", 20), 0),
        ] {
            let receipt = store
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "rimz-test",
                    agent_kind: kind.clone(),
                    event_name: "PostToolUse",
                    observation: &parent,
                    spawned_subagents: std::slice::from_ref(&facts),
                })
                .unwrap();
            assert_eq!(receipt.derived.len(), expected);
        }
        let state = store
            .snapshot_cached()
            .unwrap()
            .agents
            .into_iter()
            .find(|state| state.agent_id == "child")
            .unwrap();
        assert_eq!(state.model.as_deref(), Some("new-model"));
        assert_eq!(state.usage.total_tokens, Some(20));
    }

    #[test]
    fn auto_rotation_decision_respects_exact_threshold_and_debounce() {
        assert!(!auto_rotation_due(DEFAULT_EVENT_LOG_ROTATE_BYTES - 1, None));
        assert!(auto_rotation_due(DEFAULT_EVENT_LOG_ROTATE_BYTES, None));
        assert!(!auto_rotation_due(
            DEFAULT_EVENT_LOG_ROTATE_BYTES,
            Some(AUTO_ROTATE_DEBOUNCE - Duration::from_secs(1))
        ));
        assert!(auto_rotation_due(
            DEFAULT_EVENT_LOG_ROTATE_BYTES,
            Some(AUTO_ROTATE_DEBOUNCE)
        ));
    }

    #[test]
    fn missing_unreadable_and_future_stamps_are_due() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("missing.stamp");
        assert!(debounce::stamp_due(&missing, AUTO_ROTATE_DEBOUNCE));
        assert!(auto_rotation_due(DEFAULT_EVENT_LOG_ROTATE_BYTES, None));

        #[cfg(unix)]
        {
            let unreadable = dir.path().join("unreadable.stamp");
            std::os::unix::fs::symlink("unreadable.stamp", &unreadable)
                .expect("create symlink loop");
            assert!(debounce::stamp_due(&unreadable, AUTO_ROTATE_DEBOUNCE));
            assert!(auto_rotation_due(DEFAULT_EVENT_LOG_ROTATE_BYTES, None));
        }

        let future = dir.path().join("future.stamp");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&future)
            .expect("create future stamp");
        file.set_times(FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(60)))
            .expect("set future stamp time");
        assert!(debounce::stamp_due(&future, AUTO_ROTATE_DEBOUNCE));
        assert!(auto_rotation_due(DEFAULT_EVENT_LOG_ROTATE_BYTES, None));
    }

    #[test]
    fn failed_lifecycle_append_does_not_touch_rotation_stamp() {
        let (_dir, store) = test_store();
        let stamp = store.paths().locks_dir.join(AUTO_ROTATE_STAMP);
        std::fs::create_dir(&store.paths().events_log).expect("block event log with directory");
        let registered = observation(LifecycleSignal::Registered);

        assert!(
            store
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "rimz-test",
                    agent_kind: AgentKind::new_unchecked("claude"),
                    event_name: "SessionStart",
                    observation: &registered,
                    spawned_subagents: &[],
                })
                .is_err()
        );
        assert!(!stamp.exists());
    }
}
