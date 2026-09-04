//! Durable caller identity and launch-chain policy.
//!
//! RimZ-launched providers identify themselves through their launch
//! environment. A provider running without that environment is identified by
//! matching its process ancestry against a live durable runtime owner.

use crate::ids::{AgentKind, AgentSessionId};

/// Durable launch stamp for an agent started by another agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchAncestry {
    /// A top-level peer that participates in an agent-launch chain.
    Peer { launch_generation: u8 },
    /// A pane-backed child created through `rimz subagents`.
    Subagent {
        parent_agent_id: AgentSessionId,
        parent_agent_kind: AgentKind,
        launch_generation: u8,
    },
}

/// Stable identity for the agent calling a RimZ command.
///
/// A launch id is authoritative when present. Agents launched before that id
/// was exported retain the legacy unambiguous-pane fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallerIdentity {
    pub kind: AgentKind,
    pub launch_id: Option<AgentSessionId>,
    pub pane_id: Option<crate::ids::PaneId>,
    pub name: Option<String>,
    pub profile: Option<String>,
    pub role: Option<String>,
}

impl CallerIdentity {
    pub fn from_env() -> Option<Self> {
        let kind =
            env_string(crate::harness::launch::ENV_AGENT_KIND).map(AgentKind::new_unchecked)?;
        let launch_id = env_string(crate::harness::launch::ENV_AGENT_ID).map(AgentSessionId::from);
        let pane_id = launch_id
            .is_none()
            .then(crate::mux::ambient_pane_id)
            .flatten();
        Some(Self {
            kind,
            launch_id,
            pane_id,
            name: env_string(crate::harness::launch::ENV_AGENT_NAME),
            profile: env_string(crate::harness::launch::ENV_AGENT_PROFILE),
            role: env_string(crate::harness::launch::ENV_AGENT_ROLE),
        })
    }

    fn from_agent(
        agent: &crate::agents::AgentState,
        ambient_pane: Option<&crate::ids::PaneId>,
    ) -> Self {
        Self {
            kind: agent.kind.clone(),
            launch_id: agent.launch_id.clone(),
            pane_id: agent
                .launch_id
                .is_none()
                .then(|| ambient_pane.cloned())
                .flatten(),
            name: agent.name.clone(),
            profile: agent.profile.clone(),
            role: agent.role.clone(),
        }
    }

    pub fn from_process_ancestry(agents: &[crate::agents::AgentState]) -> Option<Self> {
        let ambient_pane = crate::mux::ambient_pane_id();
        from_ancestors(
            agents,
            &crate::proc::ancestor_pids(),
            ambient_pane.as_ref(),
            crate::proc::process_start_token,
        )
    }
}

fn env_string(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

fn from_ancestors(
    agents: &[crate::agents::AgentState],
    ancestors: &[u32],
    ambient_pane: Option<&crate::ids::PaneId>,
    start_token: impl Fn(u32) -> Option<String>,
) -> Option<CallerIdentity> {
    for &pid in ancestors {
        let actual_start = start_token(pid);
        let matches = agents
            .iter()
            .filter(|agent| agent.ended_at.is_none() && !agent.is_provider_subagent())
            .filter(|agent| {
                agent.runtime_owner.as_ref().is_some_and(|owner| {
                    owner.kind == crate::pane::RuntimeOwnerKind::Agent
                        && owner.pid == pid
                        && owner.process_start.as_ref().is_none_or(|expected| {
                            actual_start.as_deref() == Some(expected.as_str())
                        })
                })
            })
            .collect::<Vec<_>>();
        let agent = matches
            .iter()
            .copied()
            .find(|agent| {
                ambient_pane.is_some_and(|pane_id| {
                    agent
                        .pane
                        .as_ref()
                        .is_some_and(|pane| &pane.pane_id == pane_id)
                })
            })
            .or_else(|| matches.first().copied());
        if let Some(agent) = agent {
            return Some(CallerIdentity::from_agent(agent, ambient_pane));
        }
    }
    None
}

pub fn resolve_caller(agents: &[crate::agents::AgentState]) -> Option<CallerIdentity> {
    CallerIdentity::from_env().or_else(|| CallerIdentity::from_process_ancestry(agents))
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LaunchAncestryError {
    #[error(
        "launch refused: RimZ could not resolve the calling agent's durable launch identity, so it cannot safely verify the configured chain limit. Launching another agent from here is not permitted; do not retry this command."
    )]
    UnresolvedCaller,
    #[error(
        "launch refused: this agent is {current} launches deep in an agent chain, and another launch would exceed this workspace's maximum chain length of {max}. Launching another agent from here is not permitted; do not retry this command."
    )]
    ChainExceeded { current: u8, max: u8 },
    #[error(
        "launch refused: subagents cannot launch agents or subagents. Do the work yourself and report the result to your caller; do not retry this command."
    )]
    SubagentCaller,
}

/// Resolve the launch generation and optional direct subagent parent.
pub fn resolve_launch_ancestry(
    caller: Option<&crate::agents::AgentState>,
    subagent: bool,
    max_chain_length: u8,
) -> Result<Option<LaunchAncestry>, LaunchAncestryError> {
    let Some(caller) = caller else {
        return Ok(None);
    };
    if caller.is_launched_child() {
        return Err(LaunchAncestryError::SubagentCaller);
    }
    let generation = caller.launch_depth.unwrap_or(0);
    if subagent {
        return Ok(Some(LaunchAncestry::Subagent {
            parent_agent_id: caller.agent_id.clone(),
            parent_agent_kind: caller.kind.clone(),
            launch_generation: generation.saturating_add(1),
        }));
    }
    if generation >= max_chain_length {
        return Err(LaunchAncestryError::ChainExceeded {
            current: generation,
            max: max_chain_length,
        });
    }
    Ok(Some(LaunchAncestry::Peer {
        launch_generation: generation.saturating_add(1),
    }))
}

/// Resolve the launching process through its stable launch id. Kind
/// corroborates the match so stale cross-provider environment cannot attach a
/// child to the wrong durable row. An agent process already running across an
/// upgrade has no launch id; only that missing-id case may use an unambiguous
/// live pane stamp as legacy identity.
pub fn resolve_launch_ancestry_here(
    agents: &[crate::agents::AgentState],
    subagent: bool,
    max_chain_length: u8,
) -> Result<Option<LaunchAncestry>, LaunchAncestryError> {
    let Some(caller) = resolve_caller(agents) else {
        return Ok(None);
    };
    let caller = resolve_launch_caller(agents, &caller)?;
    resolve_launch_ancestry(Some(caller), subagent, max_chain_length)
}

/// Resolve the pane-backed agent that owns the current process environment.
///
/// Command doorways that operate on the caller's descendants use the same
/// stable launch-id rules as ancestry stamping, so a stale pane environment
/// cannot select another agent's children.
pub fn resolve_calling_agent(
    agents: &[crate::agents::AgentState],
) -> Result<&crate::agents::AgentState, LaunchAncestryError> {
    let caller = resolve_caller(agents).ok_or(LaunchAncestryError::UnresolvedCaller)?;
    resolve_launch_caller(agents, &caller)
}

pub fn resolve_launch_caller<'a>(
    agents: &'a [crate::agents::AgentState],
    caller: &CallerIdentity,
) -> Result<&'a crate::agents::AgentState, LaunchAncestryError> {
    let resolved = if let Some(launch_id) = caller.launch_id.as_ref() {
        agents.iter().find(|agent| {
            agent.kind == caller.kind
                && agent
                    .launch_id
                    .as_ref()
                    .is_some_and(|candidate| candidate == launch_id)
        })
    } else {
        let pane_id = caller
            .pane_id
            .as_ref()
            .ok_or(LaunchAncestryError::UnresolvedCaller)?;
        let mut matches = agents.iter().filter(|agent| {
            agent.kind == caller.kind
                && !agent.is_provider_subagent()
                && agent.ended_at.is_none()
                && agent
                    .pane
                    .as_ref()
                    .is_some_and(|pane| &pane.pane_id == pane_id)
        });
        let caller = matches.next();
        if matches.next().is_some() {
            return Err(LaunchAncestryError::UnresolvedCaller);
        }
        caller
    }
    .ok_or(LaunchAncestryError::UnresolvedCaller)?;
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;

    #[test]
    fn distinguishes_peers_subagents_and_chain_limits() {
        assert_eq!(resolve_launch_ancestry(None, false, 3).unwrap(), None);

        let root = crate::agents::AgentState::stub("claude", "root", AgentStatus::Running);
        assert_eq!(
            resolve_launch_ancestry(Some(&root), false, 3).unwrap(),
            Some(LaunchAncestry::Peer {
                launch_generation: 1,
            })
        );
        assert_eq!(
            resolve_launch_ancestry(Some(&root), true, 0).unwrap(),
            Some(LaunchAncestry::Subagent {
                parent_agent_id: AgentSessionId::from("root"),
                parent_agent_kind: AgentKind::new_unchecked("claude"),
                launch_generation: 1,
            })
        );

        let mut peer = crate::agents::AgentState::stub("codex", "peer", AgentStatus::Running);
        peer.launch_depth = Some(3);
        let error = resolve_launch_ancestry(Some(&peer), false, 3).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("3 launches deep"));
        assert!(message.contains("maximum chain length of 3"));
        assert!(message.contains("do not retry"));

        assert_eq!(
            resolve_launch_ancestry(Some(&peer), true, 3).unwrap(),
            Some(LaunchAncestry::Subagent {
                parent_agent_id: AgentSessionId::from("peer"),
                parent_agent_kind: AgentKind::new_unchecked("codex"),
                launch_generation: 4,
            })
        );

        let mut child = crate::agents::AgentState::stub("codex", "child", AgentStatus::Running);
        child.parent_agent_id = Some(AgentSessionId::from("root"));
        child.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
        child.launch_depth = Some(1);
        for subagent in [false, true] {
            assert_eq!(
                resolve_launch_ancestry(Some(&child), subagent, 3).unwrap_err(),
                LaunchAncestryError::SubagentCaller
            );
        }
    }

    #[test]
    fn caller_uses_durable_identity_and_only_legacy_missing_ids_use_the_pane() {
        let pane_id = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%7");
        let mut caller =
            crate::agents::AgentState::stub("claude", "provider-session", AgentStatus::Running);
        caller.launch_id = Some(AgentSessionId::from("launch-stable"));
        caller.pane = Some(crate::pane::PaneRef::from_id(pane_id.clone()));
        let agents = vec![caller];
        let durable = CallerIdentity {
            kind: AgentKind::new_unchecked("claude"),
            launch_id: Some(AgentSessionId::from("launch-stable")),
            pane_id: None,
            name: None,
            profile: None,
            role: None,
        };
        let legacy = CallerIdentity {
            kind: AgentKind::new_unchecked("claude"),
            launch_id: None,
            pane_id: Some(pane_id.clone()),
            name: None,
            profile: None,
            role: None,
        };

        assert_eq!(
            resolve_launch_caller(&agents, &durable).unwrap().agent_id,
            "provider-session"
        );
        assert_eq!(
            resolve_launch_caller(&agents, &legacy).unwrap().agent_id,
            "provider-session"
        );
        let stale = CallerIdentity {
            launch_id: Some(AgentSessionId::from("stale-launch")),
            pane_id: Some(pane_id.clone()),
            ..legacy.clone()
        };
        assert!(resolve_launch_caller(&agents, &stale).is_err());

        let mut duplicate = agents[0].clone();
        duplicate.agent_id = AgentSessionId::from("duplicate");
        assert!(resolve_launch_caller(&[agents[0].clone(), duplicate], &legacy).is_err());
    }

    #[test]
    fn caller_identity_matches_live_agent_owner_and_start_token() {
        let mut agent = owned_agent("claude", "provider-session", 42, Some("start-42"));
        agent.launch_id = Some(AgentSessionId::from("launch-stable"));
        agent.name = Some("architect".to_owned());
        agent.profile = Some("planner".to_owned());
        agent.role = Some("lead".to_owned());

        let identity = from_ancestors(&[agent], &[42], None, |pid| Some(format!("start-{pid}")))
            .expect("matching owner");

        assert_eq!(identity.kind, AgentKind::new_unchecked("claude"));
        assert_eq!(
            identity.launch_id,
            Some(AgentSessionId::from("launch-stable"))
        );
        assert_eq!(identity.name.as_deref(), Some("architect"));
        assert_eq!(identity.profile.as_deref(), Some("planner"));
        assert_eq!(identity.role.as_deref(), Some("lead"));
    }

    #[test]
    fn caller_identity_rejects_non_agent_and_stale_owners() {
        let mut ended = owned_agent("claude", "ended", 10, None);
        ended.ended_at = Some(jiff::Timestamp::now());

        let mut provider_subagent = owned_agent("claude", "child", 11, None);
        provider_subagent.parent_agent_id = Some(AgentSessionId::from("parent"));

        let mut daemon = owned_agent("codex", "daemon", 12, None);
        daemon.runtime_owner.as_mut().unwrap().kind = crate::pane::RuntimeOwnerKind::Daemon;

        let mismatched = owned_agent("claude", "reused", 13, Some("old-start"));
        let agents = [ended, provider_subagent, daemon, mismatched];

        assert!(from_ancestors(&agents, &[99], None, |_| None).is_none());
        assert!(
            from_ancestors(&agents, &[10, 11, 12, 13], None, |pid| {
                (pid == 13).then(|| "new-start".to_owned())
            })
            .is_none()
        );
    }

    #[test]
    fn caller_identity_prefers_the_nearest_matching_ancestor() {
        let farther = owned_agent("claude", "farther", 20, None);
        let nearest = owned_agent("codex", "nearest", 10, None);

        let identity = from_ancestors(&[farther, nearest], &[10, 20], None, |_| None)
            .expect("matching ancestor");

        assert_eq!(identity.kind, AgentKind::new_unchecked("codex"));
    }

    #[test]
    fn caller_identity_prefers_the_ambient_pane_for_a_shared_owner() {
        let first_pane = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%1");
        let ambient_pane = crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, "%2");
        let mut first = owned_agent("claude", "first", 42, None);
        first.name = Some("first".to_owned());
        first.pane = Some(crate::pane::PaneRef::from_id(first_pane));
        let mut ambient = owned_agent("claude", "ambient", 42, None);
        ambient.name = Some("ambient".to_owned());
        ambient.pane = Some(crate::pane::PaneRef::from_id(ambient_pane.clone()));

        let identity = from_ancestors(&[first, ambient], &[42], Some(&ambient_pane), |_| None)
            .expect("shared owner");

        assert_eq!(identity.name.as_deref(), Some("ambient"));
        assert_eq!(identity.pane_id.as_ref(), Some(&ambient_pane));
    }

    fn owned_agent(
        kind: &str,
        id: &str,
        pid: u32,
        process_start: Option<&str>,
    ) -> crate::agents::AgentState {
        let mut agent = crate::agents::AgentState::stub(kind, id, AgentStatus::Running);
        agent.runtime_owner = Some(crate::pane::RuntimeOwner::new(
            crate::pane::RuntimeOwnerKind::Agent,
            id,
            pid,
            process_start.map(ToOwned::to_owned),
        ));
        agent
    }
}
