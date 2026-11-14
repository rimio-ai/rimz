use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use tracing::warn;

use crate::agents::{AgentLifecycleObservation, AgentState, LifecycleSignal};
use crate::store::event::EventEnvelope;
use crate::store::runtime::{self, AgentLiveness, RuntimeScope};
use crate::store::{live_roster, pending_recovery, session_death};
use crate::workspace::record;

use crate::disk::paths::StatePaths;

use super::super::{Result, Store};
use super::debounce;

const REAP_INTERVAL: Duration = Duration::from_secs(60);
const DEAD_REAP_STAMP: &str = "dead-reap.stamp";

fn reap_session_name(paths: &StatePaths) -> String {
    record::read(&paths.workspace_record)
        .map(|record| record.session_name)
        .unwrap_or_else(|_| "rimz-reap".to_owned())
}

/// A derived end, stamped with the launch and epoch observed on its row so a
/// swap that lands before the append leaves the row alone.
struct EndedSession {
    kind: crate::ids::AgentKind,
    agent_id: crate::ids::AgentSessionId,
    launch_id: Option<crate::ids::AgentSessionId>,
    epoch: u32,
    event_name: &'static str,
}

impl EndedSession {
    fn of(agent: &AgentState, event_name: &'static str) -> Self {
        Self {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            launch_id: agent.launch_id.clone(),
            epoch: agent.epoch,
            event_name,
        }
    }
}

impl Store {
    fn append_ended_sessions(&self, victims: &[EndedSession]) -> Result<usize> {
        if victims.is_empty() {
            return Ok(0);
        }
        let session_name = reap_session_name(&self.inner.paths);
        self.commit(|txn| {
            for victim in victims {
                let mut observation = AgentLifecycleObservation::new(
                    Some(victim.agent_id.clone()),
                    LifecycleSignal::Ended,
                );
                observation.launch_id = victim.launch_id.clone();
                observation.launch.epoch = (victim.epoch > 0).then_some(victim.epoch);
                txn.append(&EventEnvelope::agent_lifecycle(
                    txn.paths.workspace_id.clone(),
                    session_name.as_str(),
                    victim.kind.as_str(),
                    victim.event_name,
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
        let target_path = crate::utils::path::normalize_path_lexical(worktree_path);
        let victims = projection
            .agents
            .iter()
            .filter(|agent| !agent.is_provider_subagent())
            .filter(|agent| agent.ended_at.is_none())
            .filter(|agent| {
                agent.worktree_path.as_deref().is_some_and(|path| {
                    crate::utils::path::normalize_path_lexical(Path::new(path)) == target_path
                }) || worktree_branch
                    .is_some_and(|branch| agent.worktree_branch.as_deref() == Some(branch))
            })
            .filter(|agent| !matches!(runtime::agent_liveness(agent), AgentLiveness::Live { .. }))
            .map(|agent| EndedSession::of(agent, "WorktreeRemoved"))
            .collect::<Vec<_>>();
        self.append_ended_sessions(&victims)
    }

    fn reap_dead_sessions(&self) -> Result<usize> {
        // The persisted roster protects crash-recovery candidates until a
        // rebirth boundary parks them in the pending-recovery record, which
        // shields pending roots from every reap reason until the user decides;
        // they cannot supersede another owner either. Pending launched
        // children get no shield: no settlement asks about them. The scan
        // stays lock-free: a live same-id session that races the append
        // clears its end stamp on its next lifecycle event.
        let mut projection = self.runtime_projection(RuntimeScope::Audit)?;
        // Rest certificates are cache-class sidecars. Reading only raw-active
        // roots lets a provider-rested owner yield; a missing sidecar leaves
        // the lifecycle status in charge.
        crate::store::agent_context::attach_rest_certificates(
            &self.inner.runtime,
            projection.agents.iter_mut(),
        );
        // Order is load-bearing: a boundary writes pending before deleting the
        // roster, so reading roster first cannot miss an agent in transit.
        let roster = live_roster::read(&self.inner.paths.live_roster)
            .map(|roster| roster.agents)
            .unwrap_or_default();
        let pending = pending_recovery::read(&self.inner.paths.pending_recovery);
        let shielded = |agent: &AgentState| {
            pending.contains(&(agent.kind.clone(), agent.agent_id.clone()))
                && !agent.is_launched_child()
        };
        let now = Timestamp::now();
        let victims = projection
            .agents
            .iter()
            .filter(|agent| !agent.is_provider_subagent())
            .filter(|agent| agent.ended_at.is_none())
            .filter(|agent| !shielded(agent))
            .filter_map(|agent| {
                let superseded = projection.agents.iter().any(|newer| {
                    !newer.is_provider_subagent()
                        && newer.ended_at.is_none()
                        && !shielded(newer)
                        && session_death::supersedes(agent, newer)
                });
                let event_name = if superseded {
                    "ReapedSuperseded"
                } else if roster.contains(&(agent.kind.clone(), agent.agent_id.clone())) {
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
                Some(EndedSession::of(agent, event_name))
            })
            .collect::<Vec<_>>();
        self.append_ended_sessions(&victims)
    }

    pub(super) fn reap_dead_sessions_if_due(&self) {
        if !debounce::claim(
            &self.inner.paths.cache_dir.join(DEAD_REAP_STAMP),
            REAP_INTERVAL,
        ) {
            return;
        }
        let store = Self {
            ingress: None,
            ..self.clone()
        };
        if let Err(err) = store.reap_dead_sessions() {
            warn!(error = %err, "dead session reap failed after store commit");
        }
    }
}

#[cfg(test)]
#[path = "reap/tests.rs"]
mod tests;
