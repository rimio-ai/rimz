//! Agent target parsing and snapshot resolution.

use crate::feed::AgentState;
use crate::ids::PaneId;
use crate::ledger::snapshot::SidebarSnapshot;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TargetErr {
    #[error("{0}")]
    InvalidPaneId(String),
    #[error("target `{target}` names worktree `{scope}` but --worktree names `{flag}`")]
    WorktreeMismatch {
        target: String,
        scope: String,
        flag: String,
    },
    #[error("no agent matches target `{target}`; live agents: {candidates}")]
    NoMatch { target: String, candidates: String },
    #[error("target `{target}` matched multiple agents: {candidates}")]
    Ambiguous { target: String, candidates: String },
    #[error("pane `{pane_id}` is not bound to a known agent")]
    PaneUnbound { pane_id: PaneId },
}

pub fn resolve_card<'a>(
    snapshot: &'a SidebarSnapshot,
    raw: &str,
    worktree_filter: Option<&str>,
) -> Result<&'a AgentState, TargetErr> {
    if raw.contains(':') {
        let pane = PaneId::parse(raw).map_err(|err| TargetErr::InvalidPaneId(err.to_string()))?;
        return resolve_by_pane(snapshot, raw, &pane, worktree_filter);
    }
    let (selector, scoped_worktree) = split_scoped_selector(raw);
    let worktree_filter = merge_worktree_filter(raw, scoped_worktree, worktree_filter)?;
    let live_agents = live_root_agents(snapshot, worktree_filter);

    let exact_name: Vec<&AgentState> = live_agents
        .iter()
        .copied()
        .filter(|agent| agent.name.as_deref() == Some(selector))
        .collect();
    if !exact_name.is_empty() {
        return one_or_ambiguous(raw, &live_agents, exact_name);
    }

    if let Some((kind, ordinal)) = parse_ordinal_selector(selector) {
        let matches: Vec<&AgentState> = live_agents
            .iter()
            .copied()
            .filter(|agent| agent.kind.as_str() == kind && agent.kind_ordinal == Some(ordinal))
            .collect();
        return one_or_ambiguous(raw, &live_agents, matches);
    }

    if crate::agents::known_kinds().any(|kind| kind == selector) {
        let matches: Vec<&AgentState> = live_agents
            .iter()
            .copied()
            .filter(|agent| agent.kind.as_str() == selector)
            .collect();
        return one_or_ambiguous(raw, &live_agents, matches);
    }

    let matches: Vec<&AgentState> = live_agents
        .iter()
        .copied()
        .filter(|agent| agent.agent_id.as_str().starts_with(selector))
        .collect();
    one_or_ambiguous(
        raw,
        &live_agents,
        prefer_exact_session_raw(selector, matches),
    )
}

fn resolve_by_pane<'a>(
    snapshot: &'a SidebarSnapshot,
    raw: &str,
    pane_id: &PaneId,
    worktree_filter: Option<&str>,
) -> Result<&'a AgentState, TargetErr> {
    let live_agents = live_root_agents(snapshot, worktree_filter);
    let matches: Vec<&AgentState> = live_agents
        .iter()
        .copied()
        .filter(|agent| {
            agent
                .pane
                .as_ref()
                .is_some_and(|pane| pane.pane_id == *pane_id)
        })
        .collect();
    match matches.as_slice() {
        [agent] => Ok(agent),
        [] => Err(TargetErr::PaneUnbound {
            pane_id: pane_id.clone(),
        }),
        many => Err(TargetErr::Ambiguous {
            target: raw.to_owned(),
            candidates: render_candidates(many),
        }),
    }
}

fn live_root_agents<'a>(
    snapshot: &'a SidebarSnapshot,
    worktree_filter: Option<&str>,
) -> Vec<&'a AgentState> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| worktree_filter.is_none_or(|filter| agent_in_worktree(agent, filter)))
        .collect()
}

fn split_scoped_selector(raw: &str) -> (&str, Option<&str>) {
    match raw.split_once('@') {
        Some((selector, scope)) if !selector.is_empty() && !scope.is_empty() => {
            (selector, Some(scope))
        }
        _ => (raw, None),
    }
}

fn merge_worktree_filter<'a>(
    raw: &str,
    scoped: Option<&'a str>,
    flag: Option<&'a str>,
) -> Result<Option<&'a str>, TargetErr> {
    match (scoped, flag) {
        (Some(scope), Some(flag)) if scope != flag => Err(TargetErr::WorktreeMismatch {
            target: raw.to_owned(),
            scope: scope.to_owned(),
            flag: flag.to_owned(),
        }),
        (Some(scope), _) => Ok(Some(scope)),
        (_, flag) => Ok(flag),
    }
}

fn parse_ordinal_selector(selector: &str) -> Option<(&str, u32)> {
    let (kind, raw_ordinal) = selector.rsplit_once('-')?;
    if !crate::agents::known_kinds().any(|known| known == kind) {
        return None;
    }
    let ordinal = raw_ordinal.parse::<u32>().ok()?;
    (ordinal > 0).then_some((kind, ordinal))
}

fn one_or_ambiguous<'a>(
    raw: &str,
    live_agents: &[&AgentState],
    candidates: Vec<&'a AgentState>,
) -> Result<&'a AgentState, TargetErr> {
    match candidates.as_slice() {
        [agent] => Ok(agent),
        [] => Err(TargetErr::NoMatch {
            target: raw.to_owned(),
            candidates: render_candidates_or_none(live_agents),
        }),
        many => Err(TargetErr::Ambiguous {
            target: raw.to_owned(),
            candidates: render_candidates(many),
        }),
    }
}

fn prefer_exact_session_raw<'a>(
    selector: &str,
    candidates: Vec<&'a AgentState>,
) -> Vec<&'a AgentState> {
    let exact: Vec<&AgentState> = candidates
        .iter()
        .copied()
        .filter(|agent| agent.agent_id.as_str() == selector)
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
            let name = agent.name.as_deref().unwrap_or("unnamed");
            let kind = match agent.kind_ordinal {
                Some(ordinal) => format!("{}-{}", agent.kind, ordinal),
                None => agent.kind.to_string(),
            };
            let worktree = agent
                .worktree_branch
                .as_deref()
                .or_else(|| {
                    agent
                        .worktree_path
                        .as_deref()
                        .and_then(|path| path.rsplit('/').next())
                })
                .unwrap_or("no-worktree");
            let pane = agent
                .pane
                .as_ref()
                .map(|pane| pane.pane_id.to_string())
                .unwrap_or_else(|| "no-pane".to_owned());
            format!("{name} {kind} {worktree} {pane}")
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
    use crate::ids::{AgentKind, AgentSessionId, MuxName, WorkspaceId};

    #[test]
    fn resolve_card_prefers_name_ordinal_kind_then_session_prefix() {
        let mut snapshot = empty_snapshot();
        let mut alpha = agent("claude", "session-alpha", Some("main"), "terminal_1");
        alpha.name = Some("lucid-atlas".to_owned());
        alpha.kind_ordinal = Some(1);
        let mut beta = agent("claude", "session-beta", Some("feature/x.y"), "terminal_2");
        beta.name = Some("bright-beacon".to_owned());
        beta.kind_ordinal = Some(2);
        snapshot.agents = vec![alpha, beta];

        assert_eq!(
            resolve_card(&snapshot, "lucid-atlas", None)
                .unwrap()
                .agent_id
                .as_str(),
            "session-alpha"
        );
        assert_eq!(
            resolve_card(&snapshot, "claude-2", None)
                .unwrap()
                .agent_id
                .as_str(),
            "session-beta"
        );
        assert!(matches!(
            resolve_card(&snapshot, "claude", None),
            Err(TargetErr::Ambiguous { .. })
        ));
        assert_eq!(
            resolve_card(&snapshot, "claude@feature/x.y", None)
                .unwrap()
                .agent_id
                .as_str(),
            "session-beta"
        );
        assert_eq!(
            resolve_card(&snapshot, "session-a", None)
                .unwrap()
                .agent_id
                .as_str(),
            "session-alpha"
        );
    }

    #[test]
    fn resolve_card_kind_ordinal_never_falls_through_to_session_prefix() {
        let mut snapshot = empty_snapshot();
        let mut agent = agent("codex", "claude-1-session", Some("main"), "terminal_1");
        agent.name = Some("solid-lumen".to_owned());
        snapshot.agents = vec![agent];

        assert!(matches!(
            resolve_card(&snapshot, "claude-1", None),
            Err(TargetErr::NoMatch { .. })
        ));
    }

    #[test]
    fn resolve_card_rejects_conflicting_worktree_scopes() {
        let snapshot = empty_snapshot();
        assert!(matches!(
            resolve_card(&snapshot, "claude@main", Some("docs")),
            Err(TargetErr::WorktreeMismatch { .. })
        ));
    }

    #[test]
    fn resolve_card_splits_scope_at_first_at_sign() {
        let mut snapshot = empty_snapshot();
        let mut agent = agent("claude", "session-alpha", Some("feat@v2"), "terminal_1");
        agent.name = Some("lucid-atlas".to_owned());
        snapshot.agents = vec![agent];

        assert_eq!(
            resolve_card(&snapshot, "lucid-atlas@feat@v2", None)
                .unwrap()
                .agent_id
                .as_str(),
            "session-alpha"
        );
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
            name: None,
            kind_ordinal: None,
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
