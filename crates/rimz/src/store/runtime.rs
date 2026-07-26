//! Runtime projection for durable store records.
//!
//! Expel is read-time filtering: default live views keep only records whose
//! recorded owner process is still the same live process. Audit views bypass
//! this filter and read durable history as written.

use crate::agents::AgentState;
use crate::ids::{AgentKind, AgentSessionId};
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeScope {
    Runtime,
    Audit,
}

/// Tri-state process liveness for an agent session record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentLiveness {
    Live { pid: u32 },
    Dead,
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuntimeProjection {
    pub ended: BTreeSet<(AgentKind, AgentSessionId)>,
    pub expelled: BTreeSet<(AgentKind, AgentSessionId)>,
    pub agents: Vec<AgentState>,
}

impl RuntimeProjection {
    pub fn from_parts(agents: Vec<AgentState>, scope: RuntimeScope) -> Self {
        let ended = agents
            .iter()
            .filter_map(|agent| {
                agent
                    .ended_at
                    .map(|_| (agent.kind.clone(), agent.agent_id.clone()))
            })
            .collect();
        match scope {
            RuntimeScope::Audit => Self {
                ended,
                expelled: BTreeSet::new(),
                agents,
            },
            RuntimeScope::Runtime => {
                let mut expelled = BTreeSet::new();
                let agents = agents
                    .into_iter()
                    .filter(|agent| {
                        let visible = agent_is_runtime_visible(agent);
                        if !visible && agent.ended_at.is_none() {
                            expelled.insert((agent.kind.clone(), agent.agent_id.clone()));
                        }
                        visible
                    })
                    .collect();
                Self {
                    ended,
                    expelled,
                    agents,
                }
            }
        }
    }
}

/// Runtime visibility for an agent. Liveness suppresses; it never gates an
/// agent in. An unknown pid abstains (foreground/pane corroboration carries
/// liveness — see `docs/internals/agents/model.md`); a known owner that is known-dead
/// suppresses the stale overlay.
fn agent_is_runtime_visible(agent: &AgentState) -> bool {
    agent.ended_at.is_none() && agent.runtime_owner.as_ref().is_none_or(owner_is_live)
}

pub fn current_process_owner(
    kind: RuntimeOwnerKind,
    subject_id: impl Into<String>,
) -> RuntimeOwner {
    let pid = std::process::id();
    RuntimeOwner::new(kind, subject_id, pid, crate::proc::process_start_token(pid))
}

pub fn process_owner(
    kind: RuntimeOwnerKind,
    subject_id: impl Into<String>,
    pid: u32,
) -> RuntimeOwner {
    RuntimeOwner::new(kind, subject_id, pid, crate::proc::process_start_token(pid))
}

pub fn owner_is_live(owner: &RuntimeOwner) -> bool {
    crate::proc::process_is_live(owner.pid, owner.process_start.as_deref())
}

pub fn agent_liveness(agent: &AgentState) -> AgentLiveness {
    if let Some(owner) = &agent.runtime_owner {
        if owner.kind == RuntimeOwnerKind::Daemon {
            return if owner_is_live(owner) {
                AgentLiveness::Unknown
            } else {
                AgentLiveness::Dead
            };
        }
        return if owner_is_live(owner) {
            AgentLiveness::Live { pid: owner.pid }
        } else {
            AgentLiveness::Dead
        };
    }
    AgentLiveness::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use jiff::Timestamp;

    fn agent(owner: Option<RuntimeOwner>) -> AgentState {
        AgentState {
            status: AgentStatus::Idle,
            runtime_owner: owner,
            ..crate::testkit::agent_state("claude", "sess-1", Timestamp::UNIX_EPOCH)
        }
    }

    #[test]
    fn runtime_projection_keeps_unknown_agents_and_drops_known_dead_ones() {
        let agents = vec![agent(None)];
        #[cfg(unix)]
        let agents = {
            let mut agents = agents;
            let mut dead = agent(Some(RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                "sess-dead",
                u32::MAX,
                None,
            )));
            dead.agent_id = AgentSessionId::from("sess-dead");
            agents.push(dead);
            agents
        };

        let projection = RuntimeProjection::from_parts(agents, RuntimeScope::Runtime);

        assert_eq!(
            projection.agents.len(),
            1,
            "unknown pid abstains while known-dead owners suppress stale overlays"
        );
        #[cfg(unix)]
        assert_eq!(
            projection.expelled,
            [(
                AgentKind::new_unchecked("claude"),
                AgentSessionId::from("sess-dead"),
            )]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn runtime_projection_hides_ended_rows_while_audit_retains_them() {
        let active = agent(None);
        let mut ended = agent(None);
        ended.agent_id = AgentSessionId::from("sess-ended");
        ended.ended_at = Some(Timestamp::UNIX_EPOCH);
        let key = (ended.kind.clone(), ended.agent_id.clone());

        let runtime = RuntimeProjection::from_parts(
            vec![active.clone(), ended.clone()],
            RuntimeScope::Runtime,
        );
        assert_eq!(runtime.agents, vec![active]);
        assert_eq!(runtime.ended, [key.clone()].into_iter().collect());
        assert!(runtime.expelled.is_empty());

        let audit = RuntimeProjection::from_parts(vec![ended.clone()], RuntimeScope::Audit);
        assert_eq!(audit.agents, vec![ended]);
        assert_eq!(audit.ended, [key].into_iter().collect());
        assert!(audit.expelled.is_empty());
    }

    #[test]
    fn agent_liveness_reports_live_runtime_owner() {
        let owner = current_process_owner(RuntimeOwnerKind::Agent, "sess-live");
        assert_eq!(
            agent_liveness(&agent(Some(owner))),
            AgentLiveness::Live {
                pid: std::process::id()
            }
        );
    }

    #[test]
    fn agent_liveness_daemon_owner_abstains_while_process_lives() {
        let owner = current_process_owner(RuntimeOwnerKind::Daemon, "sess-daemon");
        assert_eq!(agent_liveness(&agent(Some(owner))), AgentLiveness::Unknown);
    }

    #[test]
    fn agent_liveness_reports_unknown_without_process_identity() {
        assert_eq!(agent_liveness(&agent(None)), AgentLiveness::Unknown);
    }

    #[cfg(unix)]
    #[test]
    fn agent_liveness_reports_dead_for_missing_or_wrong_process() {
        let missing = RuntimeOwner::new(RuntimeOwnerKind::Agent, "sess-missing", u32::MAX, None);
        assert_eq!(agent_liveness(&agent(Some(missing))), AgentLiveness::Dead);

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let wrong_start = RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                "sess-wrong-start",
                std::process::id(),
                Some("definitely-not-this-process".to_owned()),
            );
            assert_eq!(
                agent_liveness(&agent(Some(wrong_start))),
                AgentLiveness::Dead
            );
        }

        let daemon = RuntimeOwner::new(RuntimeOwnerKind::Daemon, "sess-daemon", u32::MAX, None);
        assert_eq!(agent_liveness(&agent(Some(daemon))), AgentLiveness::Dead);
    }
}
