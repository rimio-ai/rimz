//! Read-only lifecycle and signal follower over the durable event log.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io;

use crate::agents::lifecycle::{
    LifecycleEvent, LifecycleSignal, LifecycleState, PriorTurnIds, step, turn_ids_after,
};
use crate::agents::{AgentState, AgentStatus, SessionOrigin};
use crate::disk::paths::StatePaths;
use crate::ids::{AgentKind, AgentSessionId};
use crate::ids::{EventId, WorkspaceId};
use crate::store::event::{EventKind, SignalName, SignalSource};
use crate::store::{event_log, session_death, snapshot};
use jiff::Timestamp;
use serde::Serialize;
use serde_json::{Map, Value};

type AgentKey = (AgentKind, AgentSessionId);
const ENDED_STATES_RETAINED: usize = 256;

/// Fold-free raw event tail starting at a caller's saved extent.
pub struct LaunchTail {
    paths: StatePaths,
    cursor: event_log::LogExtent,
}

impl LaunchTail {
    /// Preserve the startup cursor, including rotations before the first poll.
    pub fn from_cursor(paths: StatePaths, cursor: event_log::LogExtent) -> Self {
        Self { paths, cursor }
    }

    /// Visit new frames in archive-to-active order and return archive warnings.
    pub fn poll(
        &mut self,
        mut visit: impl FnMut(crate::store::event::EventEnvelope),
    ) -> Result<Vec<String>, EventFollowErr> {
        let generation = snapshot::lifecycle_log_generation(&self.paths);
        let mut warnings = Vec::new();
        if generation > self.cursor.generation {
            let needed = usize::try_from(generation - self.cursor.generation).unwrap_or(usize::MAX);
            let archives = event_log::newest_archives(&self.paths.events_archive_dir, needed)?;
            let complete = archives.len() == needed;
            if !complete {
                warnings.push(format!(
                    "lifecycle event-log archive gap: needed {needed} generation(s), found {}",
                    archives.len()
                ));
            }
            for (index, archive) in archives.iter().enumerate() {
                if !archive.is_file() {
                    warnings.push(format!(
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
                let end = event_log::visit_from_offset(archive, start, &mut visit)?;
                let length = fs::metadata(archive)
                    .map_err(|source| EventFollowErr::Io {
                        path: archive.clone(),
                        source,
                    })?
                    .len();
                if end != length {
                    warnings.push(format!(
                        "lifecycle event-log archive tail could not be read: {}",
                        archive.display()
                    ));
                }
            }
            self.cursor = event_log::LogExtent {
                generation,
                offset: 0,
            };
        } else if generation < self.cursor.generation {
            warnings.push(format!(
                "lifecycle event-log generation moved backward from {} to {}; resuming at the active log",
                self.cursor.generation, generation
            ));
            self.cursor = event_log::LogExtent {
                generation,
                offset: 0,
            };
        }
        // The archive precedes the generation publish. Do not alias an old
        // offset into the shorter active log during that window.
        if file_len(&self.paths.events_log)? >= self.cursor.offset {
            self.cursor.offset =
                event_log::visit_from_offset(&self.paths.events_log, self.cursor.offset, visit)?;
        }
        Ok(warnings)
    }
}

#[derive(Clone, Debug)]
struct FollowState {
    lifecycle: LifecycleState,
    open_ask_key: Option<String>,
    started_turn_id: Option<String>,
    superseded_turn_id: Option<String>,
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
    // Only ended states are evicted; live agents retain their turn history.
    ended: VecDeque<AgentKey>,
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
                ended: VecDeque::new(),
            });
        }
        let (cursor, agents) = snapshot::lifecycle_follow_seed(&paths)?;
        let mut ended = Vec::new();
        let states = agents
            .iter()
            .map(|agent| {
                let key = (agent.kind.clone(), agent.agent_id.clone());
                if let Some(ended_at) = agent.ended_at {
                    ended.push((ended_at, key.clone()));
                }
                let state = FollowState {
                    lifecycle: agent.lifecycle(),
                    open_ask_key: open_ask_key(agent),
                    started_turn_id: agent.started_turn_id.clone(),
                    superseded_turn_id: agent.superseded_turn_id.clone(),
                    interrupted_turn_id: agent.interrupted_turn_id.clone(),
                };
                (key, state)
            })
            .collect();
        // The rollup is key-sorted; eviction must drop the oldest endings first.
        ended.sort();
        let mut follower = Self {
            paths,
            cursor,
            states,
            ended: ended.into_iter().map(|(_, key)| key).collect(),
        };
        follower.prune_ended();
        Ok(follower)
    }

    fn prune_ended(&mut self) {
        while self.ended.len() > ENDED_STATES_RETAINED {
            if let Some(key) = self.ended.pop_front() {
                self.states.remove(&key);
            }
        }
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
            if let Some(signal) = SignalEvent::from_envelope(&envelope) {
                events.push(FollowEvent::Signal(signal));
                continue;
            }
            let EventKind::AgentLifecycle(payload) = envelope.kind() else {
                continue;
            };
            let observation = &payload.observation;
            if observation.origin == Some(SessionOrigin::SideConversation) {
                continue;
            }
            let Some(agent_id) = observation.agent_id.clone() else {
                continue;
            };
            let kind = AgentKind::new_unchecked(envelope.source.clone());
            let key = (kind.clone(), agent_id.clone());
            let prior = self.states.get(&key);
            if observation.state_snapshot == Some(true) {
                let next = AgentState::snapshot_transition(
                    prior.map(|state| &state.lifecycle),
                    &observation.signal,
                )
                .next;
                let state = self.states.entry(key.clone()).or_insert(FollowState {
                    lifecycle: next,
                    open_ask_key: None,
                    started_turn_id: None,
                    superseded_turn_id: None,
                    interrupted_turn_id: None,
                });
                state.lifecycle = next;
                state.open_ask_key = None;
                self.ended.retain(|ended| ended != &key);
                continue;
            }
            let turn_ids = PriorTurnIds {
                started: prior.and_then(|state| state.started_turn_id.as_deref()),
                superseded: prior.and_then(|state| state.superseded_turn_id.as_deref()),
                interrupted: prior.and_then(|state| state.interrupted_turn_id.as_deref()),
            };
            let mut transition = step(
                prior.map(|state| &state.lifecycle),
                prior.and_then(|state| state.open_ask_key.as_deref()),
                turn_ids,
                &observation.signal,
            );
            if matches!(observation.signal, LifecycleSignal::Ended)
                && session_death::reaped_end_rests_at_idle(
                    payload.event_name.as_deref(),
                    prior.map(|state| state.lifecycle.status),
                )
            {
                transition.next.status = AgentStatus::Idle;
            }
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
            let (started_turn_id, superseded_turn_id, interrupted_turn_id) =
                turn_ids_after(turn_ids, &observation.signal, transition.opened_turn);
            self.states.insert(
                key.clone(),
                FollowState {
                    lifecycle: transition.next,
                    open_ask_key,
                    started_turn_id,
                    superseded_turn_id,
                    interrupted_turn_id,
                },
            );
            self.ended.retain(|ended| ended != &key);
            if matches!(observation.signal, LifecycleSignal::Ended) {
                self.ended.push_back(key);
            }
            self.prune_ended();
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

impl SignalEvent {
    pub fn from_envelope(envelope: &crate::store::event::EventEnvelope) -> Option<Self> {
        let EventKind::Signal(payload) = envelope.kind() else {
            return None;
        };
        Some(Self {
            v: 1,
            event_id: envelope.event_id.clone(),
            at: envelope.timestamp,
            workspace_id: envelope.workspace_id.clone(),
            name: payload.name,
            payload: payload.payload,
            source: payload.source,
        })
    }
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
mod tests;
