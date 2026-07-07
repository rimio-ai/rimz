use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use jiff::Timestamp;

use crate::agents::AgentState;
use crate::diag::record::DiagEvent;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::pane::PaneRef;
use crate::store::snapshot::panes::{
    AgentPaneRow, LazyAgentPairingResult, agent_for_pane, agent_pane_for_pane,
    compute_lazy_agent_pairings, stamped_agent_for_pane,
};
use crate::store::snapshot::process::{
    pane_command_is_known, pane_worktree_path, row_from_process,
};
use crate::store::snapshot::row::{PaneAgent, SidebarRow};

use super::super::rows::row_from_agent;

pub(super) struct LazyAgentPaneProjection<'a> {
    pub(super) wired_kinds: &'a [String],
    pub(super) default_models: &'a BTreeMap<String, String>,
    pub(super) pairings: Option<&'a LazyAgentPairingResult>,
}

pub(super) struct RowProjection {
    pub(super) rows: Vec<SidebarRow>,
    /// Every live agent pane bound this fold, uncapped — the resolution source.
    /// Built here at the binding site so it never inherits row capping, ordering,
    /// or the standalone-ask rows that share the agent-card shape.
    pub(super) agent_panes: Vec<PaneAgent>,
    pub(super) diagnostics: Vec<DiagEvent>,
}

#[cfg(test)]
pub(crate) fn row_identity_violations<'a>(
    rows: impl IntoIterator<Item = &'a SidebarRow>,
) -> Vec<String> {
    let mut pane_ids = HashSet::new();
    let mut agent_ids = BTreeSet::new();
    let mut violations = Vec::new();
    for row in rows {
        if let Some(pane) = row.pane.as_ref()
            && !pane_ids.insert(pane.pane_id.clone())
        {
            violations.push(format!("duplicate pane row {}", pane.pane_id));
        }
        if row.is_agent() && !agent_ids.insert(row.id.clone()) {
            violations.push(format!("duplicate agent row {}", row.id));
        }
    }
    violations
}

pub(super) fn rows_from_panes(
    agents: &[AgentState],
    panes: &[PaneRef],
    lazy_agents: LazyAgentPaneProjection<'_>,
    panes_produced_at_ms: Option<u64>,
    now: Timestamp,
) -> RowProjection {
    let mut rows = Vec::new();
    let mut agent_panes = Vec::new();
    let mut diagnostics = Vec::new();
    let mut seen_panes = HashSet::new();
    let mut bound_agents: BTreeSet<(AgentKind, AgentSessionId)> = BTreeSet::new();
    let mut bound_agent_panes: HashMap<(AgentKind, AgentSessionId), PaneId> = HashMap::new();
    let computed_pairings;
    let lazy_pairings = if let Some(pairings) = lazy_agents.pairings {
        pairings
    } else {
        computed_pairings = compute_lazy_agent_pairings(panes, agents);
        &computed_pairings
    };

    for pane in panes {
        if !seen_panes.insert(pane.pane_id.clone()) {
            diagnostics.push(DiagEvent::DuplicatePaneId {
                pane_id: pane.pane_id.clone(),
            });
            continue;
        }
        if let Some(agent) = stamped_agent_for_pane(pane, agents) {
            let key = (agent.kind.clone(), agent.agent_id.clone());
            if let Some(bound_pane) = bound_agent_panes.get(&key) {
                diagnostics.push(DiagEvent::RowConflict {
                    agent_kind: agent.kind.clone(),
                    agent_session_id: agent.agent_id.clone(),
                    bound_pane: bound_pane.clone(),
                    conflicting_pane: pane.pane_id.clone(),
                });
                continue;
            }
        }
        if let Some(agent) = agent_for_pane(pane, agents, &bound_agents) {
            agent_panes.push(push_agent_row(
                &mut rows,
                &mut bound_agents,
                &mut bound_agent_panes,
                agent,
                pane,
                now,
            ));
        } else if let Some(bind) = agent_pane_for_pane(
            pane,
            agents,
            lazy_pairings,
            &bound_agents,
            lazy_agents.wired_kinds,
            lazy_agents.default_models,
            now,
        ) {
            match bind {
                AgentPaneRow::Agent(agent) => agent_panes.push(push_agent_row(
                    &mut rows,
                    &mut bound_agents,
                    &mut bound_agent_panes,
                    agent,
                    pane,
                    now,
                )),
                AgentPaneRow::Idle(row) => {
                    let row = *row;
                    // A wired pane carries only its kind and pane — no session,
                    // pet name, or ordinal until a lifecycle hook binds one.
                    agent_panes.push(PaneAgent {
                        kind: AgentKind::new_unchecked(row.name.clone()),
                        kind_ordinal: None,
                        name: None,
                        profile: None,
                        role: None,
                        channel: row.channel.clone(),
                        agent_id: None,
                        pane_id: pane.pane_id.clone(),
                        pane_pid: pane.pane_pid,
                        worktree_path: row.worktree_path.clone(),
                        worktree_branch: row.worktree_branch.clone(),
                    });
                    rows.push(row);
                }
                AgentPaneRow::SuppressedDuplicate { kind, agent_id } => {
                    if let Some(bound_pane) =
                        bound_agent_panes.get(&(kind.clone(), agent_id.clone()))
                    {
                        diagnostics.push(DiagEvent::RowConflict {
                            agent_kind: kind,
                            agent_session_id: agent_id,
                            bound_pane: bound_pane.clone(),
                            conflicting_pane: pane.pane_id.clone(),
                        });
                    }
                }
            }
        } else if newborn_unknown_cwd(pane, panes_produced_at_ms) {
            diagnostics.push(DiagEvent::NewbornQuarantined {
                pane_id: pane.pane_id.clone(),
            });
        } else if pane_command_is_known(pane) {
            rows.push(row_from_process(pane, now));
        }
    }
    // Floating panes stay in `agent_panes` so `@codex` still reaches them, but
    // they never render as sidebar room rows.
    rows.retain(|row| !row.pane.as_ref().is_some_and(|pane| pane.is_floating));

    RowProjection {
        rows,
        agent_panes,
        diagnostics,
    }
}

/// Push a bound agent's row and return the [`PaneAgent`] for `agent_panes`. The
/// pane comes from the live frame, not the session's own (often unstamped for a
/// daemon-routed agent) record — so resolution reaches the bound pane.
fn push_agent_row(
    rows: &mut Vec<SidebarRow>,
    bound: &mut BTreeSet<(AgentKind, AgentSessionId)>,
    bound_panes: &mut HashMap<(AgentKind, AgentSessionId), PaneId>,
    agent: &AgentState,
    pane: &PaneRef,
    now: Timestamp,
) -> PaneAgent {
    let key = (agent.kind.clone(), agent.agent_id.clone());
    bound.insert(key.clone());
    bound_panes.insert(key, pane.pane_id.clone());
    let worktree_path = agent.worktree_path.clone().or_else(|| pane.cwd.clone());
    let mut row = row_from_agent(agent, now);
    row.worktree_path = row.worktree_path.or_else(|| worktree_path.clone());
    row.pane = Some(pane.clone());
    rows.push(row);
    PaneAgent {
        kind: agent.kind.clone(),
        kind_ordinal: agent.kind_ordinal,
        name: agent.name.clone(),
        profile: agent.profile.clone(),
        role: agent.role.clone(),
        channel: agent.channel.clone(),
        agent_id: Some(agent.agent_id.clone()),
        pane_id: pane.pane_id.clone(),
        pane_pid: pane.pane_pid,
        worktree_path,
        worktree_branch: agent.worktree_branch.clone(),
    }
}

fn newborn_unknown_cwd(pane: &PaneRef, panes_produced_at_ms: Option<u64>) -> bool {
    panes_produced_at_ms.is_some()
        && pane.first_seen_at_ms == panes_produced_at_ms
        && pane_worktree_path(pane).is_none()
        && pane_command_is_known(pane)
}
