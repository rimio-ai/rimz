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
    pub lost: BTreeSet<(AgentKind, AgentSessionId)>,
    pub agents: Vec<AgentState>,
}

impl RuntimeProjection {
    pub fn from_parts(
        ended: BTreeSet<(AgentKind, AgentSessionId)>,
        lost: BTreeSet<(AgentKind, AgentSessionId)>,
        agents: Vec<AgentState>,
        scope: RuntimeScope,
    ) -> Self {
        match scope {
            RuntimeScope::Audit => Self {
                ended,
                lost,
                agents,
            },
            RuntimeScope::Runtime => Self {
                ended,
                lost,
                agents: agents
                    .into_iter()
                    .filter(agent_is_runtime_visible)
                    .collect(),
            },
        }
    }
}

/// Runtime visibility for an agent. Liveness suppresses; it never gates an
/// agent in. An unknown pid abstains (foreground/pane corroboration carries
/// liveness — see `docs/internals/agents/agent.md`); a known owner that is known-dead
/// suppresses the stale overlay.
fn agent_is_runtime_visible(agent: &AgentState) -> bool {
    agent.runtime_owner.as_ref().is_none_or(owner_is_live)
}

pub fn current_process_owner(
    kind: RuntimeOwnerKind,
    subject_id: impl Into<String>,
) -> RuntimeOwner {
    let pid = std::process::id();
    RuntimeOwner::new(kind, subject_id, pid, process_start_token(pid))
}

pub fn process_owner(
    kind: RuntimeOwnerKind,
    subject_id: impl Into<String>,
    pid: u32,
) -> RuntimeOwner {
    RuntimeOwner::new(kind, subject_id, pid, process_start_token(pid))
}

pub fn owner_is_live(owner: &RuntimeOwner) -> bool {
    process_is_live(owner.pid, owner.process_start.as_deref())
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

#[cfg(target_os = "linux")]
pub fn process_start_token(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    linux_process_start_from_stat(&stat).map(ToOwned::to_owned)
}

#[cfg(target_os = "macos")]
pub fn process_start_token(pid: u32) -> Option<String> {
    crate::proc::process_start_token(pid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_start_token(_pid: u32) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn process_is_live(pid: u32, expected_start: Option<&str>) -> bool {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(_) => return false,
    };
    linux_process_stat_is_live(&stat, expected_start)
}

#[cfg(target_os = "macos")]
fn process_is_live(pid: u32, expected_start: Option<&str>) -> bool {
    let Some(metrics) = crate::proc::stat_metrics(pid) else {
        return unix_kill_probe(pid);
    };
    if metrics.state == 'Z' {
        return false;
    }
    match expected_start {
        Some(expected) => crate::proc::process_start_token(pid).as_deref() == Some(expected),
        None => true,
    }
}

#[cfg(target_os = "linux")]
fn linux_process_stat_is_live(stat: &str, expected_start: Option<&str>) -> bool {
    if matches!(linux_process_state_from_stat(stat), Some("Z" | "X")) {
        return false;
    }
    match expected_start {
        Some(expected) => linux_process_start_from_stat(stat) == Some(expected),
        None => true,
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn process_is_live(pid: u32, _expected_start: Option<&str>) -> bool {
    unix_kill_probe(pid)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn unix_kill_probe(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    matches!(kill(Pid::from_raw(pid), None), Ok(()) | Err(Errno::EPERM))
}

#[cfg(not(unix))]
fn process_is_live(_pid: u32, _expected_start: Option<&str>) -> bool {
    true
}

#[cfg(target_os = "linux")]
fn linux_process_start_from_stat(stat: &str) -> Option<&str> {
    let after_comm = stat.rsplit_once(") ")?.1;
    after_comm.split_whitespace().nth(19)
}

#[cfg(target_os = "linux")]
fn linux_process_state_from_stat(stat: &str) -> Option<&str> {
    let after_comm = stat.rsplit_once(") ")?.1;
    after_comm.split_whitespace().next()
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
            agents.push(agent(Some(RuntimeOwner::new(
                RuntimeOwnerKind::Agent,
                "sess-dead",
                u32::MAX,
                None,
            ))));
            agents
        };

        let projection = RuntimeProjection::from_parts(
            BTreeSet::new(),
            BTreeSet::new(),
            agents,
            RuntimeScope::Runtime,
        );

        assert_eq!(
            projection.agents.len(),
            1,
            "unknown pid abstains while known-dead owners suppress stale overlays"
        );
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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_stat_liveness_rejects_zombies() {
        let running = "123 (codex) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345";
        let zombie = "123 (codex) Z 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 12345";

        assert_eq!(linux_process_state_from_stat(zombie), Some("Z"));
        assert!(linux_process_stat_is_live(running, Some("12345")));
        assert!(!linux_process_stat_is_live(zombie, Some("12345")));
    }
}
