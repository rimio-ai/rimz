//! Read-only lifecycle and signal follower over the durable event log.

use std::collections::BTreeMap;
use std::fs;
use std::io;

use crate::agents::AgentState;
use crate::agents::lifecycle::{LifecycleEvent, LifecycleSignal, LifecycleState, step};
use crate::disk::paths::StatePaths;
use crate::harness::schedule::signal::{SignalName, SignalSource};
use crate::ids::{AgentKind, AgentSessionId};
use crate::ids::{EventId, WorkspaceId};
use crate::store::event::EventKind;
use crate::store::{event_log, snapshot};
use jiff::Timestamp;
use serde::Serialize;
use serde_json::{Map, Value};

type AgentKey = (AgentKind, AgentSessionId);

#[derive(Clone, Debug)]
struct FollowState {
    lifecycle: LifecycleState,
    open_ask_key: Option<String>,
    interrupted_turn_id: Option<String>,
}

/// Events and non-fatal archive-gap warnings observed in one poll.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventFollowBatch {
    pub events: Vec<FollowEvent>,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum EventFollowErr {
    #[error(transparent)]
    Snapshot(#[from] crate::store::snapshot::SnapshotErr),
    #[error(transparent)]
    EventLog(#[from] crate::store::event_log::EventLogErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Cursor that projects durable `agent.lifecycle` frames into public envelopes.
pub struct EventFollower {
    paths: StatePaths,
    cursor: event_log::LogExtent,
    states: BTreeMap<AgentKey, FollowState>,
}

impl EventFollower {
    /// Start at the live edge, or replay the current active generation from zero.
    pub fn open(paths: StatePaths, replay: bool) -> Result<Self, EventFollowErr> {
        if replay {
            return Ok(Self {
                cursor: event_log::LogExtent {
                    generation: snapshot::lifecycle_log_generation(&paths),
                    offset: 0,
                },
                paths,
                states: BTreeMap::new(),
            });
        }
        let (cursor, agents) = snapshot::lifecycle_follow_seed(&paths)?;
        let states = agents
            .into_iter()
            .map(|agent| {
                let key = (agent.kind.clone(), agent.agent_id.clone());
                let state = FollowState {
                    lifecycle: agent.lifecycle(),
                    open_ask_key: open_ask_key(&agent),
                    interrupted_turn_id: agent.interrupted_turn_id,
                };
                (key, state)
            })
            .collect();
        Ok(Self {
            paths,
            cursor,
            states,
        })
    }

    /// Read every lifecycle frame appended since the prior poll.
    pub fn poll(&mut self) -> Result<EventFollowBatch, EventFollowErr> {
        let generation = snapshot::lifecycle_log_generation(&self.paths);
        let mut batch = EventFollowBatch::default();
        if generation > self.cursor.generation {
            self.drain_rotated(generation, &mut batch)?;
        } else if generation < self.cursor.generation {
            batch.warnings.push(format!(
                "lifecycle event-log generation moved backward from {} to {}; resuming at the active log",
                self.cursor.generation, generation
            ));
            self.cursor = event_log::LogExtent {
                generation,
                offset: 0,
            };
        }

        let active_len = file_len(&self.paths.events_log)?;
        if active_len < self.cursor.offset {
            // Rotation publishes the archive before it bumps the generation.
            // Wait for that durable generation marker rather than aliasing the
            // old cursor into the shorter active file.
            return Ok(batch);
        }
        let (events, end) =
            event_log::read_from_offset(&self.paths.events_log, self.cursor.offset)?;
        self.cursor.offset = end;
        batch.events.extend(self.fold(events));
        Ok(batch)
    }

    fn drain_rotated(
        &mut self,
        generation: u64,
        batch: &mut EventFollowBatch,
    ) -> Result<(), EventFollowErr> {
        let delta = generation.saturating_sub(self.cursor.generation);
        let needed = usize::try_from(delta).unwrap_or(usize::MAX);
        let archives = event_log::newest_archives(&self.paths.events_archive_dir, needed)?;
        let complete = archives.len() == needed;
        if !complete {
            batch.warnings.push(format!(
                "lifecycle event-log archive gap: needed {needed} generation(s), found {}",
                archives.len()
            ));
        }
        for (index, archive) in archives.iter().enumerate() {
            if !archive.is_file() {
                batch.warnings.push(format!(
                    "lifecycle event-log archive disappeared before it could be read: {}",
                    archive.display()
                ));
                continue;
            }
            let start = if complete && index == 0 {
                self.cursor.offset
            } else {
                0
            };
            let (events, _end) = event_log::read_from_offset(archive, start)?;
            batch.events.extend(self.fold(events));
        }
        self.cursor = event_log::LogExtent {
            generation,
            offset: 0,
        };
        Ok(())
    }

    fn fold(&mut self, envelopes: Vec<crate::store::event::EventEnvelope>) -> Vec<FollowEvent> {
        let mut events = Vec::new();
        for envelope in envelopes {
            if let EventKind::Signal(payload) = envelope.kind() {
                events.push(FollowEvent::Signal(SignalEvent {
                    v: 1,
                    event_id: envelope.event_id.clone(),
                    at: envelope.timestamp,
                    workspace_id: envelope.workspace_id.clone(),
                    name: payload.name,
                    payload: payload.payload,
                    source: payload.source,
                }));
                continue;
            }
            let EventKind::AgentLifecycle(payload) = envelope.kind() else {
                continue;
            };
            let observation = &payload.observation;
            let Some(agent_id) = observation.agent_id.clone() else {
                continue;
            };
            let kind = AgentKind::new_unchecked(envelope.source.clone());
            let key = (kind.clone(), agent_id.clone());
            let prior = self.states.get(&key);
            let transition = step(
                prior.map(|state| &state.lifecycle),
                prior.and_then(|state| state.open_ask_key.as_deref()),
                prior.and_then(|state| state.interrupted_turn_id.as_deref()),
                &observation.signal,
            );
            events.push(FollowEvent::Lifecycle(LifecycleEvent::new(
                envelope.event_id.clone(),
                envelope.timestamp,
                envelope.workspace_id.clone(),
                kind,
                agent_id,
                observation.agent_name.clone(),
                observation.parent_agent_id.clone(),
                observation.signal.clone(),
                prior.map(|state| state.lifecycle.status),
                transition,
            )));
            let open_ask_key = match &observation.signal {
                LifecycleSignal::AwaitingInput {
                    ask_id: Some(_),
                    native_key,
                    ..
                } => native_key.clone(),
                LifecycleSignal::AwaitingInput { ask_id: None, .. } => None,
                _ if transition.next.status != crate::agents::AgentStatus::Waiting => None,
                _ => prior.and_then(|state| state.open_ask_key.clone()),
            };
            let interrupted_turn_id = match &observation.signal {
                LifecycleSignal::TurnInterrupted { turn_id } => turn_id.clone(),
                LifecycleSignal::Registered => None,
                _ if transition.opened_turn => None,
                _ => prior.and_then(|state| state.interrupted_turn_id.clone()),
            };
            self.states.insert(
                key,
                FollowState {
                    lifecycle: transition.next,
                    open_ask_key,
                    interrupted_turn_id,
                },
            );
        }
        events
    }
}

fn open_ask_key(agent: &AgentState) -> Option<String> {
    agent
        .open_ask
        .as_ref()
        .and_then(|ask| ask.native_key.clone())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum FollowEvent {
    Lifecycle(LifecycleEvent),
    Signal(SignalEvent),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SignalEvent {
    pub v: u8,
    pub event_id: EventId,
    pub at: Timestamp,
    pub workspace_id: WorkspaceId,
    pub name: SignalName,
    pub payload: Map<String, Value>,
    pub source: SignalSource,
}

fn file_len(path: &std::path::Path) -> Result<u64, EventFollowErr> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(source) => Err(EventFollowErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::paths::RuntimePaths;
    use crate::ids::WorkspaceId;
    use crate::store::{Store, writer::AgentLifecycleIntent};

    fn fixture() -> (tempfile::TempDir, Store, StatePaths) {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace_id, dir.path()).unwrap();
        let store = Store::open(paths.clone(), runtime).unwrap();
        (dir, store, paths)
    }

    fn append(store: &Store, signal: LifecycleSignal) {
        let observation = crate::agents::AgentLifecycleObservation::new(
            Some(AgentSessionId::from("session-1")),
            signal,
        );
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "room",
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    }

    #[test]
    fn replay_folds_the_active_generation_and_live_mode_starts_at_the_edge() {
        let (_dir, store, paths) = fixture();
        append(&store, LifecycleSignal::Registered);
        let mut live = EventFollower::open(paths.clone(), false).unwrap();
        assert!(live.poll().unwrap().events.is_empty());

        let mut replay = EventFollower::open(paths, true).unwrap();
        let batch = replay.poll().unwrap();
        assert_eq!(batch.events.len(), 1);
        let FollowEvent::Lifecycle(event) = &batch.events[0] else {
            panic!("lifecycle")
        };
        assert_eq!(event.status, crate::agents::AgentStatus::Idle);
    }

    #[test]
    fn follower_drains_the_rotated_tail_before_the_new_active_log() {
        let (_dir, store, paths) = fixture();
        append(&store, LifecycleSignal::Registered);
        let mut follower = EventFollower::open(paths, false).unwrap();
        append(&store, LifecycleSignal::TurnStarted);
        assert_eq!(follower.poll().unwrap().events.len(), 1);

        store.rotate_event_log(1, None).unwrap();
        append(
            &store,
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        );
        let batch = follower.poll().unwrap();
        assert!(batch.warnings.is_empty());
        assert_eq!(batch.events.len(), 1);
        let FollowEvent::Lifecycle(event) = &batch.events[0] else {
            panic!("lifecycle")
        };
        assert_eq!(
            event.prior_status,
            Some(crate::agents::AgentStatus::Running)
        );
        assert_eq!(event.status, crate::agents::AgentStatus::Success);
    }

    #[test]
    fn adapter_ask_without_id_does_not_hold_a_sibling_tool_waiting() {
        let (_dir, store, paths) = fixture();
        append(&store, LifecycleSignal::Registered);
        append(&store, LifecycleSignal::TurnStarted);
        append(
            &store,
            LifecycleSignal::AwaitingInput {
                kind: crate::agents::AskKind::Permission,
                ask_id: None,
                detail: None,
                native_key: Some("ask-call".to_owned()),
            },
        );
        append(
            &store,
            LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
                name: None,
                native_key: Some("sibling-call".to_owned()),
                turn_id: None,
            },
        );

        let mut follower = EventFollower::open(paths, true).unwrap();
        let events = follower.poll().unwrap().events;
        let FollowEvent::Lifecycle(tool) = events.last().unwrap() else {
            panic!("lifecycle")
        };
        assert_eq!(tool.prior_status, Some(crate::agents::AgentStatus::Waiting));
        assert_eq!(tool.status, crate::agents::AgentStatus::Running);
        assert_eq!(tool.transition, crate::agents::LifecycleTransition::Normal);
        assert!(tool.waiting_cleared);
    }
}
