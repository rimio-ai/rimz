//! Read-only lifecycle-event follower over the durable event log.

use std::collections::BTreeMap;
use std::fs;
use std::io;

use super::{LifecycleEvent, LifecycleSignal, LifecycleState, step};
use crate::agents::AgentState;
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::event::EventKind;
use crate::store::{StatePaths, event_log, snapshot};

type AgentKey = (AgentKind, AgentSessionId);

#[derive(Clone, Debug)]
struct FollowState {
    lifecycle: LifecycleState,
    open_ask_key: Option<String>,
}

/// Events and non-fatal archive-gap warnings observed in one poll.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LifecycleFollowBatch {
    pub events: Vec<LifecycleEvent>,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum LifecycleFollowErr {
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
pub struct LifecycleFollower {
    paths: StatePaths,
    cursor: event_log::LogExtent,
    states: BTreeMap<AgentKey, FollowState>,
}

impl LifecycleFollower {
    /// Start at the live edge, or replay the current active generation from zero.
    pub fn open(paths: StatePaths, replay: bool) -> Result<Self, LifecycleFollowErr> {
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
    pub fn poll(&mut self) -> Result<LifecycleFollowBatch, LifecycleFollowErr> {
        let generation = snapshot::lifecycle_log_generation(&self.paths);
        let mut batch = LifecycleFollowBatch::default();
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
        batch: &mut LifecycleFollowBatch,
    ) -> Result<(), LifecycleFollowErr> {
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

    fn fold(&mut self, envelopes: Vec<crate::store::event::EventEnvelope>) -> Vec<LifecycleEvent> {
        let mut events = Vec::new();
        for envelope in envelopes {
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
                &observation.signal,
            );
            events.push(LifecycleEvent::new(
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
            ));
            let open_ask_key = match &observation.signal {
                LifecycleSignal::AwaitingInput { native_key, .. } => native_key.clone(),
                _ if transition.next.status != crate::agents::AgentStatus::Waiting => None,
                _ => prior.and_then(|state| state.open_ask_key.clone()),
            };
            self.states.insert(
                key,
                FollowState {
                    lifecycle: transition.next,
                    open_ask_key,
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

fn file_len(path: &std::path::Path) -> Result<u64, LifecycleFollowErr> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(source) => Err(LifecycleFollowErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::LifecycleSignal;
    use crate::ids::WorkspaceId;
    use crate::store::{AgentLifecycleIntent, RuntimePaths, Store};

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
        let mut live = LifecycleFollower::open(paths.clone(), false).unwrap();
        assert!(live.poll().unwrap().events.is_empty());

        let mut replay = LifecycleFollower::open(paths, true).unwrap();
        let batch = replay.poll().unwrap();
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].status, crate::agents::AgentStatus::Idle);
    }

    #[test]
    fn follower_drains_the_rotated_tail_before_the_new_active_log() {
        let (_dir, store, paths) = fixture();
        append(&store, LifecycleSignal::Registered);
        let mut follower = LifecycleFollower::open(paths, false).unwrap();
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
        assert_eq!(
            batch.events[0].prior_status,
            Some(crate::agents::AgentStatus::Running)
        );
        assert_eq!(batch.events[0].status, crate::agents::AgentStatus::Success);
    }
}
