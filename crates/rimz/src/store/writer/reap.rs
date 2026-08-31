use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use tracing::warn;

use crate::agents::{AgentLifecycleObservation, AgentState, AgentStatus, LifecycleSignal};
use crate::store::event::EventEnvelope;
use crate::store::paths::RuntimePaths;
use crate::store::runtime::{self, AgentLiveness, RuntimeScope};
use crate::store::{live_roster, session_death};

use super::super::{Result, StatePaths, Store, workspace_record};
use super::debounce;

const REAP_INTERVAL: Duration = Duration::from_secs(60);

fn dead_reap_stamp(paths: &StatePaths) -> std::path::PathBuf {
    paths.locks_dir.join("dead-reap.stamp")
}

pub(super) fn reap_due(paths: &StatePaths) -> bool {
    debounce::stamp_due(&dead_reap_stamp(paths), REAP_INTERVAL)
}

fn reap_session_name(paths: &StatePaths) -> String {
    workspace_record::read(&paths.workspace_record)
        .map(|record| record.session_name)
        .unwrap_or_else(|_| "rimz-reap".to_owned())
}

type EndedSession = (
    crate::ids::AgentKind,
    crate::ids::AgentSessionId,
    &'static str,
);

fn attach_rest_certificates(agents: &mut [AgentState], runtime: &RuntimePaths) {
    for agent in agents {
        if agent.ended_at.is_some()
            || !matches!(agent.status, AgentStatus::Running | AgentStatus::Waiting)
        {
            continue;
        }
        agent.context = crate::store::agent_context::read_one(
            runtime,
            agent.kind.as_str(),
            agent.agent_id.as_str(),
        )
        .map(|record| record.context);
    }
}

impl Store {
    fn append_ended_sessions(&self, victims: &[EndedSession]) -> Result<usize> {
        if victims.is_empty() {
            return Ok(0);
        }
        let session_name = reap_session_name(&self.inner.paths);
        self.commit(|txn| {
            for (kind, agent_id, event_name) in victims {
                let observation =
                    AgentLifecycleObservation::new(Some(agent_id.clone()), LifecycleSignal::Ended);
                txn.append(&EventEnvelope::agent_lifecycle(
                    txn.paths.workspace_id.clone(),
                    session_name.as_str(),
                    kind.as_str(),
                    *event_name,
                    &observation,
                ))?;
            }
            Ok(victims.len())
        })
    }

    pub fn retire_worktree_sessions(
        &self,
        worktree_path: &Path,
        worktree_branch: Option<&str>,
    ) -> Result<usize> {
        let projection = self.runtime_projection(RuntimeScope::Audit)?;
        let target_path = crate::worktree::normalize_path_lexical(worktree_path);
        let victims = projection
            .agents
            .iter()
            .filter(|agent| !agent.is_provider_subagent())
            .filter(|agent| agent.ended_at.is_none())
            .filter(|agent| {
                agent.worktree_path.as_deref().is_some_and(|path| {
                    crate::worktree::normalize_path_lexical(Path::new(path)) == target_path
                }) || worktree_branch
                    .is_some_and(|branch| agent.worktree_branch.as_deref() == Some(branch))
            })
            .filter(|agent| !matches!(runtime::agent_liveness(agent), AgentLiveness::Live { .. }))
            .map(|agent| {
                (
                    agent.kind.clone(),
                    agent.agent_id.clone(),
                    "WorktreeRemoved",
                )
            })
            .collect::<Vec<_>>();
        self.append_ended_sessions(&victims)
    }

    fn reap_dead_sessions(&self) -> Result<usize> {
        // The persisted roster protects crash-recovery candidates until room
        // rebirth consumes it. The remaining scan stays lock-free: a live
        // same-id session that races the append clears its end stamp on its
        // next lifecycle event.
        let mut projection = self.runtime_projection(RuntimeScope::Audit)?;
        // Rest certificates are cache-class sidecars. Reading only raw-active
        // roots lets a provider-rested owner yield; a missing sidecar leaves
        // the lifecycle status in charge.
        attach_rest_certificates(&mut projection.agents, &self.inner.runtime);
        let protected = live_roster::read(&self.inner.paths.live_roster)
            .map(|roster| roster.agents)
            .unwrap_or_default();
        let now = Timestamp::now();
        let victims = projection
            .agents
            .iter()
            .filter(|agent| !agent.is_provider_subagent())
            .filter(|agent| agent.ended_at.is_none())
            .filter_map(|agent| {
                let superseded = projection.agents.iter().any(|newer| {
                    !newer.is_provider_subagent()
                        && newer.ended_at.is_none()
                        && session_death::supersedes(agent, newer)
                });
                let event_name = if superseded {
                    "ReapedSuperseded"
                } else if protected.contains(&(agent.kind.clone(), agent.agent_id.clone())) {
                    return None;
                } else if runtime::agent_liveness(agent) == AgentLiveness::Dead {
                    "ReapedDead"
                } else if session_death::agent_is_pidless(agent)
                    && session_death::session_age_secs(now, agent)
                        > session_death::GHOST_SESSION_TTL_SECS
                {
                    "ReapedStale"
                } else {
                    return None;
                };
                Some((agent.kind.clone(), agent.agent_id.clone(), event_name))
            })
            .collect::<Vec<_>>();
        self.append_ended_sessions(&victims)
    }

    pub(super) fn reap_dead_sessions_if_due(&self) {
        if !reap_due(&self.inner.paths) {
            return;
        }
        debounce::touch_stamp(&dead_reap_stamp(&self.inner.paths));
        if let Err(err) = self.reap_dead_sessions() {
            warn!(error = %err, "dead session reap failed after store commit");
        }
    }
}

#[cfg(test)]
#[path = "reap/tests.rs"]
mod tests;
