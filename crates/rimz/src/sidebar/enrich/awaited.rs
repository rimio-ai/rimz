//! The awaited-agents fold: every `Subagent` wait on a card whose awaited row has no
//! entry among the card's current children becomes one entry of `AgentCard::awaited`,
//! carrying the awaited row's handle, display state and pane from this fold's rows.
//! Reads only; an awaited agent reaches no status, attention, count or sort.

use std::collections::HashMap;
use std::sync::Arc;

use crate::agents::{PendingWaitTarget, PendingWaitTrigger, TurnPhase};
use crate::store::snapshot::{AgentCard, SidebarAwaitedAgent, SidebarRow, SidebarSnapshot};

use super::bookings::{RowAt, row_index, row_of};

#[cfg(test)]
mod tests;

/// Set every card's awaited list from its waits. A row is copied only when its list
/// changes.
pub(super) fn attach(snapshot: &mut SidebarSnapshot) {
    let rows = row_index(snapshot);
    let wanted: Vec<Vec<Vec<SidebarAwaitedAgent>>> = snapshot
        .worktree_groups
        .iter()
        .map(|group| {
            group
                .rows
                .iter()
                .map(|row| {
                    row.as_agent()
                        .map(|card| awaited(snapshot, &rows, card))
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect();
    let differs = |row: &SidebarRow, wanted: &Vec<SidebarAwaitedAgent>| {
        row.as_agent().is_some_and(|card| card.awaited != *wanted)
    };
    let group_differs = |rows: &[SidebarRow], wanted: &[Vec<SidebarAwaitedAgent>]| {
        rows.iter()
            .zip(wanted)
            .any(|(row, wanted)| differs(row, wanted))
    };
    if !(snapshot.worktree_groups.iter().zip(&wanted))
        .any(|(group, wanted)| group_differs(&group.rows, wanted))
    {
        return;
    }
    let groups = Arc::make_mut(&mut snapshot.worktree_groups);
    for (group, wanted) in groups.iter_mut().zip(wanted) {
        if !group_differs(&group.rows, &wanted) {
            continue;
        }
        for (row, wanted) in Arc::make_mut(&mut group.rows).iter_mut().zip(wanted) {
            if differs(row, &wanted)
                && let Some(card) = row.as_agent_mut()
            {
                card.awaited = wanted;
            }
        }
    }
}

/// The card's `Subagent` waits whose target is not a current child, one per target in wait
/// order. A wait recorded without its target yields nothing. A peer can carry a settled turn
/// still owing its report beside the next open one, both naming its row: the open run's wait
/// stands for the agent.
fn awaited(
    snapshot: &SidebarSnapshot,
    rows: &HashMap<(&str, &str), RowAt>,
    card: &AgentCard,
) -> Vec<SidebarAwaitedAgent> {
    let mut awaited: Vec<(&PendingWaitTarget, SidebarAwaitedAgent)> = Vec::new();
    for wait in &card.pending_waits {
        let PendingWaitTrigger::Subagent {
            target: Some(target),
            settled,
            ..
        } = &wait.trigger
        else {
            continue;
        };
        if card
            .current_sub_agents()
            .any(|child| child.kind == target.kind && child.id == target.agent_id.as_str())
        {
            continue;
        }
        let row = rows
            .get(&(target.kind.as_str(), target.agent_id.as_str()))
            .map(|at| row_of(snapshot, *at));
        let awaited_card = row.and_then(SidebarRow::as_agent);
        let element = SidebarAwaitedAgent {
            handle: row.map_or_else(|| wait.name.clone(), |row| row.display_name().to_owned()),
            status: awaited_card.map(|card| card.status),
            phase: awaited_card.map_or(TurnPhase::Idle, |card| card.phase),
            compacting: awaited_card.is_some_and(|card| card.compacting),
            pane: row
                .and_then(|row| row.pane.as_ref())
                .map(|pane| pane.pane_id.clone()),
            since: wait.armed_at,
            settled: settled.clone(),
        };
        match awaited.iter_mut().find(|(seen, _)| *seen == target) {
            Some((_, kept)) if element.settled.is_none() => *kept = element,
            Some(_) => {}
            None => awaited.push((target, element)),
        }
    }
    awaited.into_iter().map(|(_, element)| element).collect()
}
