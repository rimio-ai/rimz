use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use jiff::Timestamp;

use crate::agents::lifecycle::TurnPhase;
use crate::feed::{AgentState, AgentStatus, FeedItem, PaneRef, pending_ask_for};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::ledger::snapshot::panes::{
    AgentPaneRow, LazyAgentPairingResult, agent_for_pane, agent_pane_for_pane,
    compute_lazy_agent_pairings, pane_start_matches, stamped_agent_for_pane,
};
use crate::ledger::snapshot::process::{
    pane_command_is_known, pane_worktree_path, row_from_process,
};
use crate::ledger::snapshot::row::SidebarRow;
use crate::schema::diag::DiagEvent;

use super::super::rows::{active_resolver_state, row_from_agent, row_from_standalone_item};

pub(super) struct LazyAgentPaneProjection<'a> {
    pub(super) wired_kinds: &'a [String],
    pub(super) default_models: &'a BTreeMap<String, String>,
    pub(super) pairings: Option<&'a LazyAgentPairingResult>,
}

pub(super) struct RowProjection {
    pub(super) rows: Vec<SidebarRow>,
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
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
    panes: &[PaneRef],
    lazy_agents: LazyAgentPaneProjection<'_>,
    panes_produced_at_ms: Option<u64>,
    now: Timestamp,
) -> RowProjection {
    let mut rows = Vec::new();
    let mut diagnostics = Vec::new();
    let mut seen_panes = HashSet::new();
    let mut bound_agents: BTreeSet<(AgentKind, AgentSessionId)> = BTreeSet::new();
    let mut bound_agent_panes: HashMap<(AgentKind, AgentSessionId), PaneId> = HashMap::new();
    let standalone_items = standalone_items_by_pane(needs_attention, resolver_working, panes);
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
        let standalone_ask = standalone_items.get(&pane.pane_id).copied();
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
            push_agent_row(
                &mut rows,
                &mut bound_agents,
                &mut bound_agent_panes,
                agent,
                pane,
                pane_ask(agent, standalone_ask, needs_attention, resolver_working),
                now,
            );
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
                AgentPaneRow::Agent(agent) => push_agent_row(
                    &mut rows,
                    &mut bound_agents,
                    &mut bound_agent_panes,
                    agent,
                    pane,
                    pane_ask(agent, standalone_ask, needs_attention, resolver_working),
                    now,
                ),
                AgentPaneRow::Idle(row) => {
                    let mut row = *row;
                    if let Some(ask) = standalone_ask {
                        fold_ask_onto_row(&mut row, ask);
                    }
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
        } else if let Some(item) = standalone_ask {
            rows.push(row_from_standalone_item(item, pane));
        } else if newborn_unknown_cwd(pane, panes_produced_at_ms) {
            diagnostics.push(DiagEvent::NewbornQuarantined {
                pane_id: pane.pane_id.clone(),
            });
        } else if pane_command_is_known(pane) {
            rows.push(row_from_process(pane, now));
        }
    }

    RowProjection { rows, diagnostics }
}

/// The newest pending standalone (non-agent-hook) ask per frame-admitted pane.
fn standalone_items_by_pane<'a>(
    needs_attention: &'a [FeedItem],
    resolver_working: &'a [FeedItem],
    panes: &[PaneRef],
) -> HashMap<PaneId, &'a FeedItem> {
    let mut by_pane = HashMap::new();
    for item in needs_attention.iter().chain(resolver_working.iter()) {
        if item.source_kind == "agent-hook" {
            continue;
        }
        let Some(pane) = frame_pane_for_item(item, panes) else {
            continue;
        };
        by_pane
            .entry(pane.pane_id.clone())
            .and_modify(|current: &mut &'a FeedItem| {
                if item.updated_at > current.updated_at {
                    *current = item;
                }
            })
            .or_insert(item);
    }
    by_pane
}

fn frame_pane_for_item<'a>(item: &FeedItem, panes: &'a [PaneRef]) -> Option<&'a PaneRef> {
    let requested = item.pane.as_ref()?;
    panes
        .iter()
        .find(|pane| pane.pane_id == requested.pane_id && pane_start_matches(requested, pane))
}

fn pane_ask<'a>(
    agent: &AgentState,
    standalone_ask: Option<&'a FeedItem>,
    needs_attention: &'a [FeedItem],
    resolver_working: &'a [FeedItem],
) -> Option<&'a FeedItem> {
    standalone_ask.or_else(|| most_relevant_ask(agent, needs_attention, resolver_working))
}

fn push_agent_row(
    rows: &mut Vec<SidebarRow>,
    bound: &mut BTreeSet<(AgentKind, AgentSessionId)>,
    bound_panes: &mut HashMap<(AgentKind, AgentSessionId), PaneId>,
    agent: &AgentState,
    pane: &PaneRef,
    ask: Option<&FeedItem>,
    now: Timestamp,
) {
    let key = (agent.kind.clone(), agent.agent_id.clone());
    bound.insert(key.clone());
    bound_panes.insert(key, pane.pane_id.clone());
    let mut row = row_from_agent(agent, now);
    row.worktree_path = row.worktree_path.or_else(|| pane.cwd.clone());
    row.pane = Some(pane.clone());
    if let Some(ask) = ask {
        fold_ask_onto_row(&mut row, ask);
    }
    rows.push(row);
}

fn newborn_unknown_cwd(pane: &PaneRef, panes_produced_at_ms: Option<u64>) -> bool {
    panes_produced_at_ms.is_some()
        && pane.first_seen_at_ms == panes_produced_at_ms
        && pane_worktree_path(pane).is_none()
        && pane_command_is_known(pane)
}

fn most_relevant_ask<'a>(
    agent: &AgentState,
    needs_attention: &'a [FeedItem],
    resolver_working: &'a [FeedItem],
) -> Option<&'a FeedItem> {
    pending_ask_for(agent, needs_attention.iter().chain(resolver_working.iter()))
}

fn fold_ask_onto_row(row: &mut SidebarRow, ask: &FeedItem) {
    row.last_activity = ask.updated_at;
    let Some(agent) = row.as_agent_mut() else {
        return;
    };
    agent.status = Some(AgentStatus::Waiting);
    agent.phase = TurnPhase::Idle;
    agent.request_id = Some(ask.request_id.clone());
    agent.surface = Some(ask.surface);
    agent.resolver = active_resolver_state(ask);
    agent.options = ask.options.clone();
}
