//! Agent target parsing and snapshot resolution.

use std::fmt;

use crate::feed::AgentState;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::ledger::snapshot::SidebarSnapshot;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentTarget {
    Pane(PaneId),
    Kind(AgentKind),
    Session(AgentSessionId),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TargetErr {
    #[error("{0}")]
    InvalidPaneId(String),
    #[error("no agent matches target `{target}`; live agents: {candidates}")]
    NoMatch { target: String, candidates: String },
    #[error("target `{target}` matched multiple agents: {candidates}")]
    Ambiguous { target: String, candidates: String },
    #[error("pane `{pane_id}` is not bound to a known agent")]
    PaneUnbound { pane_id: PaneId },
}

impl AgentTarget {
    pub fn parse<'a>(
        raw: &str,
        known_kinds: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, TargetErr> {
        if raw.contains(':') {
            return PaneId::parse(raw)
                .map(Self::Pane)
                .map_err(|err| TargetErr::InvalidPaneId(err.to_string()));
        }
        if known_kinds.into_iter().any(|kind| kind == raw) {
            return Ok(Self::Kind(AgentKind::new_unchecked(raw)));
        }
        Ok(Self::Session(AgentSessionId::from(raw)))
    }
}

impl fmt::Display for AgentTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pane(pane) => write!(f, "{pane}"),
            Self::Kind(kind) => write!(f, "{kind}"),
            Self::Session(session) => write!(f, "{session}"),
        }
    }
}

pub fn resolve_agent<'a>(
    snapshot: &'a SidebarSnapshot,
    target: &AgentTarget,
    worktree_filter: Option<&str>,
) -> Result<&'a AgentState, TargetErr> {
    let live_agents: Vec<&AgentState> = snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| worktree_filter.is_none_or(|filter| agent_in_worktree(agent, filter)))
        .collect();
    let candidates: Vec<&AgentState> = live_agents
        .iter()
        .copied()
        .filter(|agent| match target {
            AgentTarget::Pane(pane_id) => agent
                .pane
                .as_ref()
                .is_some_and(|pane| pane.pane_id == *pane_id),
            AgentTarget::Kind(kind) => agent.kind == *kind,
            AgentTarget::Session(session) => agent.agent_id.as_str().starts_with(session.as_str()),
        })
        .collect();
    let candidates = prefer_exact_session_match(target, candidates);
    match candidates.as_slice() {
        [agent] => Ok(agent),
        [] if let AgentTarget::Pane(pane_id) = target => Err(TargetErr::PaneUnbound {
            pane_id: pane_id.clone(),
        }),
        [] => Err(TargetErr::NoMatch {
            target: target.to_string(),
            candidates: render_candidates_or_none(&live_agents),
        }),
        many => Err(TargetErr::Ambiguous {
            target: target.to_string(),
            candidates: render_candidates(many),
        }),
    }
}

fn prefer_exact_session_match<'a>(
    target: &AgentTarget,
    candidates: Vec<&'a AgentState>,
) -> Vec<&'a AgentState> {
    let AgentTarget::Session(session) = target else {
        return candidates;
    };
    let exact: Vec<&AgentState> = candidates
        .iter()
        .copied()
        .filter(|agent| agent.agent_id == *session)
        .collect();
    if exact.is_empty() { candidates } else { exact }
}

fn agent_in_worktree(agent: &AgentState, filter: &str) -> bool {
    agent.worktree_branch.as_deref() == Some(filter)
        || agent
            .worktree_path
            .as_deref()
            .is_some_and(|path| path == filter || path.rsplit('/').next() == Some(filter))
}

fn render_candidates(candidates: &[&AgentState]) -> String {
    candidates
        .iter()
        .map(|agent| {
            let pane = agent
                .pane
                .as_ref()
                .map(|pane| pane.pane_id.to_string())
                .unwrap_or_else(|| "no-pane".to_owned());
            format!("{}:{}@{}", agent.kind, agent.agent_id, pane)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_candidates_or_none(candidates: &[&AgentState]) -> String {
    if candidates.is_empty() {
        "none".to_owned()
    } else {
        render_candidates(candidates)
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;

    use super::*;
    use crate::feed::{AgentStatus, PaneRef};
    use crate::ids::{MuxName, WorkspaceId};

    #[test]
    fn parse_prefers_pane_ids_then_known_kinds_then_session_ids() {
        assert!(matches!(
            AgentTarget::parse("tmux:%1", ["claude", "codex"]).unwrap(),
            AgentTarget::Pane(_)
        ));
        assert!(matches!(
            AgentTarget::parse("claude", ["claude", "codex"]).unwrap(),
            AgentTarget::Kind(_)
        ));
        assert!(matches!(
            AgentTarget::parse("sess-1", ["claude", "codex"]).unwrap(),
            AgentTarget::Session(_)
        ));
        assert!(AgentTarget::parse("bad:pane", ["claude"]).is_err());
    }

    #[test]
    fn resolve_agent_reports_ambiguity_and_worktree_filter() {
        let mut snapshot = empty_snapshot();
        snapshot.agents = vec![
            agent("claude", "session-alpha", Some("alpha"), "terminal_1"),
            agent("claude", "session-beta", Some("beta"), "terminal_2"),
        ];
        let target = AgentTarget::Kind(AgentKind::new_unchecked("claude"));
        assert!(matches!(
            resolve_agent(&snapshot, &target, None),
            Err(TargetErr::Ambiguous { .. })
        ));
        let resolved = resolve_agent(&snapshot, &target, Some("beta")).unwrap();
        assert_eq!(resolved.agent_id.as_str(), "session-beta");
    }

    #[test]
    fn session_target_accepts_unique_prefix_and_lists_candidates_on_miss() {
        let mut snapshot = empty_snapshot();
        snapshot.agents = vec![
            agent("claude", "session-alpha", Some("alpha"), "terminal_1"),
            agent("codex", "session-beta", Some("beta"), "terminal_2"),
        ];

        let resolved = resolve_agent(
            &snapshot,
            &AgentTarget::Session(AgentSessionId::from("session-a")),
            None,
        )
        .unwrap();
        assert_eq!(resolved.agent_id.as_str(), "session-alpha");

        assert!(matches!(
            resolve_agent(
                &snapshot,
                &AgentTarget::Session(AgentSessionId::from("session")),
                None,
            ),
            Err(TargetErr::Ambiguous { .. })
        ));

        let err = resolve_agent(
            &snapshot,
            &AgentTarget::Session(AgentSessionId::from("missing")),
            None,
        )
        .unwrap_err();
        let TargetErr::NoMatch { candidates, .. } = err else {
            panic!("expected no match");
        };
        assert!(candidates.contains("claude:session-alpha@"));
        assert!(candidates.contains("codex:session-beta@"));
    }

    #[test]
    fn pane_target_requires_a_bound_agent() {
        let snapshot = empty_snapshot();
        let target = AgentTarget::Pane(PaneId::from_parts(MuxName::Tmux, "%1"));
        assert!(matches!(
            resolve_agent(&snapshot, &target, None),
            Err(TargetErr::PaneUnbound { .. })
        ));
    }

    fn empty_snapshot() -> SidebarSnapshot {
        SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-target-test")),
            Vec::new(),
            Vec::new(),
            Timestamp::now(),
        )
    }

    fn agent(kind: &str, id: &str, branch: Option<&str>, raw_pane: &str) -> AgentState {
        let now = Timestamp::now();
        AgentState {
            agent_id: AgentSessionId::from(id),
            kind: AgentKind::new_unchecked(kind),
            status: AgentStatus::Idle,
            phase: crate::agents::TurnPhase::Idle,
            pane: Some(PaneRef::from_id(PaneId::from_parts(
                MuxName::Zellij,
                raw_pane,
            ))),
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: branch.map(|branch| format!("/repo/{branch}")),
            worktree_branch: branch.map(ToOwned::to_owned),
            task: None,
            prompt: None,
            transcript_path: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
