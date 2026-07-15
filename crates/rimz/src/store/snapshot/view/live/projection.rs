use std::collections::{BTreeMap, BTreeSet, HashSet};

use jiff::Timestamp;

use crate::agents::AgentState;
use crate::diag::record::DiagEvent;
use crate::ids::AgentKind;
use crate::pane::PaneRef;
use crate::store::snapshot::panes::{
    LazyAgentPairingResult, PaneBinder, PaneBindingDisposition, PaneBindingIndex,
    compute_lazy_agent_pairings_with_index,
};
use crate::store::snapshot::process::row_from_process;
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
    let index = PaneBindingIndex::new(agents);
    let computed_pairings;
    let lazy_pairings = if let Some(pairings) = lazy_agents.pairings {
        pairings
    } else {
        computed_pairings = compute_lazy_agent_pairings_with_index(panes, &index);
        &computed_pairings
    };
    let mut binder = PaneBinder::new(
        index,
        lazy_pairings,
        lazy_agents.wired_kinds,
        lazy_agents.default_models,
        panes_produced_at_ms,
        now,
    );

    for pane in panes {
        match binder.resolve(pane) {
            PaneBindingDisposition::Agent(agent) => {
                agent_panes.push(push_agent_row(&mut rows, agent, pane, now));
            }
            PaneBindingDisposition::Idle(row) => {
                let row = *row;
                agent_panes.push(pane_agent_from_idle(&row, pane));
                if !pane.is_floating {
                    rows.push(row);
                }
            }
            PaneBindingDisposition::DuplicatePane => {
                diagnostics.push(DiagEvent::DuplicatePaneId {
                    pane_id: pane.pane_id.clone(),
                });
            }
            PaneBindingDisposition::Conflict {
                kind,
                agent_id,
                bound_pane,
            } => {
                diagnostics.push(DiagEvent::RowConflict {
                    agent_kind: kind,
                    agent_session_id: agent_id,
                    bound_pane,
                    conflicting_pane: pane.pane_id.clone(),
                });
            }
            PaneBindingDisposition::Quarantined => {
                diagnostics.push(DiagEvent::NewbornQuarantined {
                    pane_id: pane.pane_id.clone(),
                });
            }
            PaneBindingDisposition::Process => {
                if !pane.is_floating {
                    rows.push(row_from_process(pane, now));
                }
            }
            PaneBindingDisposition::Ignored => {}
        }
    }

    RowProjection {
        rows,
        agent_panes,
        diagnostics,
    }
}

fn pane_agent_from_idle(row: &SidebarRow, pane: &PaneRef) -> PaneAgent {
    // A wired pane carries only its kind and pane — no session, pet name, or
    // ordinal until a lifecycle hook binds one.
    PaneAgent {
        kind: AgentKind::new_unchecked(row.name.clone()),
        kind_ordinal: None,
        name: None,
        name_explicit: false,
        profile: None,
        role: None,
        channel: row.channel.clone(),
        agent_id: None,
        pane_id: pane.pane_id.clone(),
        pane_pid: pane.pane_pid,
        worktree_path: row.worktree_path.clone(),
        worktree_branch: row.worktree_branch.clone(),
    }
}

/// Push a bound agent's row and return the [`PaneAgent`] for `agent_panes`. The
/// pane comes from the live frame, not the session's own (often unstamped for a
/// daemon-routed agent) record — so resolution reaches the bound pane.
fn push_agent_row(
    rows: &mut Vec<SidebarRow>,
    agent: &AgentState,
    pane: &PaneRef,
    now: Timestamp,
) -> PaneAgent {
    let worktree_path = agent.worktree_path.clone().or_else(|| pane.cwd.clone());
    if !pane.is_floating {
        let mut row = row_from_agent(agent, now);
        row.worktree_path = row.worktree_path.or_else(|| worktree_path.clone());
        row.pane = Some(pane.clone());
        rows.push(row);
    }
    PaneAgent {
        kind: agent.kind.clone(),
        kind_ordinal: agent.kind_ordinal,
        name: agent.name.clone(),
        name_explicit: agent.name_explicit,
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
