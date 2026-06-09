use std::collections::{BTreeMap, BTreeSet, HashMap};

use jiff::Timestamp;

use crate::agents::lifecycle::TurnPhase;
use crate::feed::{AgentState, AgentStatus, FeedItem, PaneRef};
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::ledger::snapshot::panes::{
    AgentPaneRow, LazyAgentPairingResult, agent_for_pane, agent_pane_for_pane,
    compute_lazy_agent_pairings, pane_start_matches,
};
use crate::ledger::snapshot::process::{pane_command_is_known, row_from_process};
use crate::ledger::snapshot::row::SidebarRow;

use super::super::rows::{
    active_resolver_state, agent_id_from_item, row_from_agent, row_from_standalone_item,
};

pub(super) struct LazyAgentPaneProjection<'a> {
    pub(super) wired_kinds: &'a [String],
    pub(super) default_models: &'a BTreeMap<String, String>,
    pub(super) pairings: Option<&'a LazyAgentPairingResult>,
}

pub(super) fn rows_from_panes(
    agents: &[AgentState],
    needs_attention: &[FeedItem],
    resolver_working: &[FeedItem],
    panes: &[PaneRef],
    lazy_agents: LazyAgentPaneProjection<'_>,
    now: Timestamp,
) -> Vec<SidebarRow> {
    let mut rows = Vec::new();
    let mut bound_agents: BTreeSet<(AgentKind, AgentSessionId)> = BTreeSet::new();
    let standalone_items = standalone_items_by_pane(needs_attention, resolver_working, panes);
    let computed_pairings;
    let lazy_pairings = if let Some(pairings) = lazy_agents.pairings {
        pairings
    } else {
        computed_pairings = compute_lazy_agent_pairings(panes, agents);
        &computed_pairings
    };

    for pane in panes {
        let standalone_ask = standalone_items.get(&pane.pane_id).copied();
        if let Some(agent) = agent_for_pane(pane, agents, &bound_agents) {
            push_agent_row(
                &mut rows,
                &mut bound_agents,
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
            }
        } else if let Some(item) = standalone_ask {
            rows.push(row_from_standalone_item(item, pane));
        } else if pane_command_is_known(pane) {
            rows.push(row_from_process(pane, now));
        }
    }

    rows
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
    agent: &AgentState,
    pane: &PaneRef,
    ask: Option<&FeedItem>,
    now: Timestamp,
) {
    bound.insert((agent.kind.clone(), agent.agent_id.clone()));
    let mut row = row_from_agent(agent, now);
    row.worktree_path = row.worktree_path.or_else(|| pane.cwd.clone());
    row.pane = Some(pane.clone());
    if let Some(ask) = ask {
        fold_ask_onto_row(&mut row, ask);
    }
    rows.push(row);
}

fn most_relevant_ask<'a>(
    agent: &AgentState,
    needs_attention: &'a [FeedItem],
    resolver_working: &'a [FeedItem],
) -> Option<&'a FeedItem> {
    needs_attention
        .iter()
        .chain(resolver_working.iter())
        .find(|item| {
            item.source_kind == "agent-hook"
                && item.source == agent.kind
                && agent_id_from_item(item).as_deref() == Some(agent.agent_id.as_str())
                && !agent_moved_past_ask(agent, item)
        })
}

fn agent_moved_past_ask(agent: &AgentState, ask: &FeedItem) -> bool {
    agent.last_activity > ask.updated_at
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
