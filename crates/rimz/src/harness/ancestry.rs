//! Durable caller identity and launch-chain policy.

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

/// Stable identity exported to a RimZ-launched agent process.
///
/// A launch id is authoritative when present. Agents launched before that id
/// was exported retain the legacy unambiguous-pane fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchCallerEnv {
    pub kind: AgentKind,
    pub launch_id: Option<AgentSessionId>,
    pub pane_id: Option<crate::ids::PaneId>,
}

impl LaunchCallerEnv {
    pub fn from_env() -> Option<Self> {
        let kind = std::env::var(crate::harness::launch::ENV_AGENT_KIND)
            .ok()
            .filter(|value| !value.is_empty())
            .map(AgentKind::new_unchecked)?;
        let launch_id = std::env::var(crate::harness::launch::ENV_AGENT_ID)
            .ok()
            .filter(|value| !value.is_empty())
            .map(AgentSessionId::from);
        let pane_id = launch_id
            .is_none()
            .then(crate::mux::ambient_pane_id)
            .flatten();
        Some(Self {
            kind,
            launch_id,
            pane_id,
        })
    }
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

/// Whether this process identifies itself as an agent caller. Human launches
/// can skip the audit projection entirely.
pub fn launch_ancestry_required() -> bool {
    std::env::var(crate::harness::launch::ENV_AGENT_KIND)
        .ok()
        .is_some_and(|value| !value.is_empty())
}

/// Resolve the launching process through its stable launch id. Kind
/// corroborates the match so stale cross-provider environment cannot attach a
/// child to the wrong durable row. An agent process already running across an
/// upgrade has no launch id; only that missing-id case may use an unambiguous
/// live pane stamp as legacy identity.
pub fn resolve_launch_ancestry_from_env(
    agents: &[crate::agents::AgentState],
    subagent: bool,
    max_chain_length: u8,
) -> Result<Option<LaunchAncestry>, LaunchAncestryError> {
    if !launch_ancestry_required() {
        return Ok(None);
    }
    let caller = resolve_launch_caller_from_env(agents)?;
    resolve_launch_ancestry(Some(caller), subagent, max_chain_length)
}

/// Resolve the pane-backed agent that owns the current process environment.
pub fn resolve_launch_caller_from_env(
    agents: &[crate::agents::AgentState],
) -> Result<&crate::agents::AgentState, LaunchAncestryError> {
    let caller = LaunchCallerEnv::from_env().ok_or(LaunchAncestryError::UnresolvedCaller)?;
    resolve_launch_caller(agents, &caller)
}

pub fn resolve_launch_caller<'a>(
    agents: &'a [crate::agents::AgentState],
    caller: &LaunchCallerEnv,
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
        let durable = LaunchCallerEnv {
            kind: AgentKind::new_unchecked("claude"),
            launch_id: Some(AgentSessionId::from("launch-stable")),
            pane_id: None,
        };
        let legacy = LaunchCallerEnv {
            kind: AgentKind::new_unchecked("claude"),
            launch_id: None,
            pane_id: Some(pane_id.clone()),
        };

        assert_eq!(
            resolve_launch_caller(&agents, &durable).unwrap().agent_id,
            "provider-session"
        );
        assert_eq!(
            resolve_launch_caller(&agents, &legacy).unwrap().agent_id,
            "provider-session"
        );
        let stale = LaunchCallerEnv {
            launch_id: Some(AgentSessionId::from("stale-launch")),
            pane_id: Some(pane_id.clone()),
            ..legacy.clone()
        };
        assert!(resolve_launch_caller(&agents, &stale).is_err());

        let mut duplicate = agents[0].clone();
        duplicate.agent_id = AgentSessionId::from("duplicate");
        assert!(resolve_launch_caller(&[agents[0].clone(), duplicate], &legacy).is_err());
    }
}
