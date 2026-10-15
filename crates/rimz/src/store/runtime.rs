//! Runtime projection for durable store records.
//!
//! Expel is read-time filtering: default live views keep only records whose
//! recorded owner process is still the same live process. Audit views bypass
//! this filter and read durable history as written.

use crate::agents::AgentState;
use crate::ids::{AgentKind, AgentSessionId};
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
use std::collections::{BTreeSet, HashSet};

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
    /// Every terminal [`super::snapshot::ResumeOutcome`] within retention.
    ///
    /// Independent of liveness scope, for durable retry/demotion decisions.
    pub resume_outcomes: Vec<super::snapshot::ResumeOutcome>,
}

/// Read the durable agent rollup without opening a writer-capable [`crate::Store`].
///
/// The one-shot signal emitter (`harness::schedule::signal::audit_agents`)
/// retains ended rows through this read-only entry point when retiring
/// subscriptions.
pub(crate) fn audit_projection(
    paths: &crate::StatePaths,
) -> crate::store::snapshot::Result<RuntimeProjection> {
    let (_, agents, resume_outcomes) = super::snapshot::catch_up_rollup(paths)?;
    Ok(RuntimeProjection::from_parts(
        &agents,
        RuntimeScope::Audit,
        resume_outcomes,
    ))
}

impl RuntimeProjection {
    /// Audit keeps every row, so it owns a copy of each; runtime owns only
    /// the rows it keeps.
    pub(super) fn from_parts(
        agents: &super::snapshot::AgentRollup,
        scope: RuntimeScope,
        resume_outcomes: Vec<super::snapshot::ResumeOutcome>,
    ) -> Self {
        match scope {
            RuntimeScope::Audit => Self {
                ended: ended_keys(agents.iter()),
                expelled: BTreeSet::new(),
                agents: agents.to_vec(),
                resume_outcomes,
            },
            RuntimeScope::Runtime => Self::runtime_from_refs(agents.iter(), resume_outcomes),
        }
    }

    /// The runtime-scoped projection over borrowed rows, cloning only the
    /// rows it keeps — a long-lived reader's rollup stays shared.
    pub(super) fn runtime_from_refs<'a>(
        agents: impl IntoIterator<Item = &'a AgentState>,
        resume_outcomes: Vec<super::snapshot::ResumeOutcome>,
    ) -> Self {
        let agents = agents
            .into_iter()
            .map(|agent| (agent, agent_is_runtime_visible(agent)))
            .collect::<Vec<_>>();
        let visible_parents = agents
            .iter()
            .filter(|(agent, visible)| agent.parent_agent_id.is_none() && *visible)
            .flat_map(|(agent, _)| agent.identity_ids().map(move |id| (&agent.kind, id)))
            .collect::<HashSet<_>>();
        let mut expelled = BTreeSet::new();
        let kept = agents
            .iter()
            .filter_map(|(agent, independently_visible)| {
                let visible = *independently_visible
                    || (agent.is_launched_child()
                        && agent.parent_agent_id.as_ref().is_some_and(|id| {
                            visible_parents.contains(&(
                                agent.parent_agent_kind.as_ref().unwrap_or(&agent.kind),
                                id,
                            ))
                        }));
                if !visible && agent.ended_at.is_none() {
                    expelled.insert((agent.kind.clone(), agent.agent_id.clone()));
                }
                visible.then(|| (*agent).clone())
            })
            .collect();
        Self {
            ended: ended_keys(agents.iter().map(|(agent, _)| *agent)),
            expelled,
            agents: kept,
            resume_outcomes,
        }
    }
}

fn ended_keys<'a>(
    agents: impl IntoIterator<Item = &'a AgentState>,
) -> BTreeSet<(AgentKind, AgentSessionId)> {
    agents
        .into_iter()
        .filter_map(|agent| {
            agent
                .ended_at
                .map(|_| (agent.kind.clone(), agent.agent_id.clone()))
        })
        .collect()
}

/// Runtime visibility for an agent. Liveness suppresses; it never gates an
/// agent in. An unknown pid abstains (foreground/pane corroboration carries
/// liveness — see `docs/internals/agents/model.md`); a known owner that is known-dead
/// suppresses the stale overlay. A launched child's liveness follows its visible
/// parent, matching a native child whose runtime owner is the parent's process.
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

pub(super) fn owner_is_live(owner: &RuntimeOwner) -> bool {
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
    use crate::store::snapshot::AgentRollup;
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

        let projection =
            RuntimeProjection::from_parts(&agents.into(), RuntimeScope::Runtime, Vec::new());

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
    fn runtime_projection_keeps_ended_launched_child_only_with_visible_parent() {
        let parent = agent(None);
        let mut child = agent(Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "sess-child",
            u32::MAX,
            None,
        )));
        child.kind = AgentKind::new_unchecked("codex");
        child.agent_id = AgentSessionId::from("sess-child");
        child.parent_agent_kind = Some(parent.kind.clone());
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.launch_depth = Some(1);
        child.ended_at = Some(Timestamp::UNIX_EPOCH);
        child.status = AgentStatus::Success;

        let projection = RuntimeProjection::from_parts(
            &AgentRollup::from(vec![parent.clone(), child.clone()]),
            RuntimeScope::Runtime,
            Vec::new(),
        );
        assert_eq!(projection.agents, vec![parent.clone(), child.clone()]);

        let without_parent = RuntimeProjection::from_parts(
            &AgentRollup::from(vec![child.clone()]),
            RuntimeScope::Runtime,
            Vec::new(),
        );
        assert!(without_parent.agents.is_empty());

        let mut ended_parent = parent;
        ended_parent.ended_at = Some(Timestamp::UNIX_EPOCH);
        let ended_parent_projection = RuntimeProjection::from_parts(
            &AgentRollup::from(vec![ended_parent, child]),
            RuntimeScope::Runtime,
            Vec::new(),
        );
        assert!(ended_parent_projection.agents.is_empty());
    }

    #[test]
    fn runtime_projection_keeps_child_through_parent_launch() {
        for parent_kind in [None, Some(AgentKind::new_unchecked("claude"))] {
            let mut old = agent(None);
            old.launch_id = Some("L".into());
            old.ended_at = Some(Timestamp::UNIX_EPOCH);
            let mut successor = agent(None);
            successor.agent_id = "NEW".into();
            successor.launch_id = old.launch_id.clone();
            let mut child = agent(None);
            child.agent_id = "child".into();
            child.parent_agent_id = Some("L".into());
            child.parent_agent_kind = parent_kind;
            child.launch_depth = Some(1);
            child.ended_at = Some(Timestamp::UNIX_EPOCH);

            let projection = RuntimeProjection::from_parts(
                &AgentRollup::from(vec![old.clone(), successor.clone(), child.clone()]),
                RuntimeScope::Runtime,
                Vec::new(),
            );
            assert_eq!(projection.agents, vec![successor, child.clone()]);
            let without_successor = RuntimeProjection::from_parts(
                &AgentRollup::from(vec![old, child]),
                RuntimeScope::Runtime,
                Vec::new(),
            );
            assert!(without_successor.agents.is_empty());
        }
    }

    #[test]
    fn runtime_projection_parent_aliases_are_kind_scoped() {
        let mut parent = agent(None);
        parent.launch_id = Some("launch".into());
        parent.prior_launch_ids = vec!["prior".into()];
        for id in ["sess-1", "launch", "prior"] {
            for kind in ["claude", "codex"] {
                let mut child = agent(None);
                child.agent_id = "child".into();
                child.parent_agent_id = Some(id.into());
                child.parent_agent_kind = Some(AgentKind::new_unchecked(kind));
                child.launch_depth = Some(1);
                child.ended_at = Some(Timestamp::UNIX_EPOCH);
                let projection =
                    RuntimeProjection::runtime_from_refs([&child, &parent], Vec::new());
                let expected = if kind == "claude" {
                    vec![child.clone(), parent.clone()]
                } else {
                    vec![parent.clone()]
                };
                assert_eq!(projection.agents, expected, "{id}, {kind}");
                assert_eq!(projection.ended, [(child.kind, child.agent_id)].into());
                assert!(projection.expelled.is_empty());
            }
        }
    }

    #[test]
    fn runtime_projection_dead_parent_does_not_hide_a_visible_child() {
        let mut parent = agent(Some(RuntimeOwner::new(
            RuntimeOwnerKind::Agent,
            "parent",
            u32::MAX,
            None,
        )));
        parent.agent_id = "parent".into();
        let mut child = agent(None);
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.launch_depth = Some(1);
        for visible in [true, false] {
            child.runtime_owner = if visible {
                None
            } else {
                parent.runtime_owner.clone()
            };
            let projection = RuntimeProjection::runtime_from_refs([&parent, &child], Vec::new());
            assert_eq!(
                projection.agents,
                if visible { vec![child.clone()] } else { vec![] }
            );
            let mut expelled = BTreeSet::from([(parent.kind.clone(), parent.agent_id.clone())]);
            if !visible {
                expelled.insert((child.kind.clone(), child.agent_id.clone()));
            }
            assert_eq!(projection.expelled, expelled);
        }
    }

    #[test]
    fn runtime_projection_hides_ended_rows_while_audit_retains_them() {
        let active = agent(None);
        let mut ended = agent(None);
        ended.agent_id = AgentSessionId::from("sess-ended");
        ended.ended_at = Some(Timestamp::UNIX_EPOCH);
        let key = (ended.kind.clone(), ended.agent_id.clone());

        let runtime = RuntimeProjection::from_parts(
            &AgentRollup::from(vec![active.clone(), ended.clone()]),
            RuntimeScope::Runtime,
            Vec::new(),
        );
        assert_eq!(runtime.agents, vec![active]);
        assert_eq!(runtime.ended, [key.clone()].into_iter().collect());
        assert!(runtime.expelled.is_empty());

        let audit = RuntimeProjection::from_parts(
            &AgentRollup::from(vec![ended.clone()]),
            RuntimeScope::Audit,
            Vec::new(),
        );
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
