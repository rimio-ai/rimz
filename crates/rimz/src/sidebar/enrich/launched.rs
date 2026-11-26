//! The launched fold: joins the published launched cache to this fold's rows. Each entry
//! lands on the card of its launcher's living launch (following a swap, as a booking's owner
//! does) with what it has alive in this fold. Reads only; a launched entry reaches no status,
//! attention, count or sort.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::agents::AgentState;
use crate::store::snapshot::{SidebarLaunched, SidebarSnapshot};

use super::super::refresh::launched::{LaunchedCache, LaunchedFacts};
use super::bookings::{RowAt, agent_row, live, living_row, row_index, row_of};

#[cfg(test)]
mod tests;

/// Attach every launched entry of `cache` to its launcher's card, in launch order, and
/// clear the launched entries of every other card. A row is copied only when its list
/// changes.
pub(super) fn attach(snapshot: &mut SidebarSnapshot, cache: &LaunchedCache) {
    let rows = row_index(snapshot);
    let mut lists: BTreeMap<RowAt, Vec<SidebarLaunched>> = BTreeMap::new();
    for facts in &cache.launched {
        let Some(owner) = living_row(snapshot, &rows, &facts.launched_by) else {
            continue;
        };
        if is_own_launch(snapshot, owner, facts) {
            continue;
        }
        let members: Vec<&AgentState> = snapshot
            .agents
            .iter()
            .filter(|agent| {
                agent.ended_at.is_none()
                    && agent_row(&rows, agent).is_some()
                    && facts
                        .launches
                        .iter()
                        .any(|id| id == agent.launch_key().as_str())
            })
            .collect();
        lists.entry(owner).or_default().push(SidebarLaunched {
            starts: facts.starts.clone(),
            lane: facts.lane.clone(),
            description: facts.description.clone(),
            launches: facts.launches.clone(),
            at: facts.at,
            live: live(
                snapshot,
                &rows,
                facts.leader.as_deref(),
                &facts.starts,
                &members,
            ),
            starting: members.is_empty() && starting(snapshot, facts),
            ended_at: facts.ended_at,
            pr: facts.pr,
            team: facts.team.clone(),
        });
    }
    let groups = Arc::make_mut(&mut snapshot.worktree_groups);
    for (group_at, group) in groups.iter_mut().enumerate() {
        let wanted: Vec<_> = (0..group.rows.len())
            .map(|row_at| lists.remove(&(group_at, row_at)).unwrap_or_default())
            .collect();
        let changed = group
            .rows
            .iter()
            .zip(&wanted)
            .any(|(row, wanted)| row.as_agent().is_some_and(|card| card.launched != *wanted));
        if !changed {
            continue;
        }
        for (row, wanted) in Arc::make_mut(&mut group.rows).iter_mut().zip(wanted) {
            if row.as_agent().is_some_and(|card| card.launched != wanted)
                && let Some(card) = row.as_agent_mut()
            {
                card.launched = wanted;
            }
        }
    }
}

/// Whether an entry with no card alive is starting: the launch lane read a row of it alive,
/// and that row is still in this fold, its card not landed yet. One that left the fold's
/// rows since the lane's pass has ended.
fn starting(snapshot: &SidebarSnapshot, facts: &LaunchedFacts) -> bool {
    facts.ended_at.is_none()
        && snapshot.agents.iter().any(|agent| {
            agent.ended_at.is_none()
                && facts
                    .launches
                    .iter()
                    .any(|id| id == agent.launch_key().as_str())
        })
}

/// Whether the card at `at` is a row of the entry's own launch: a successor carries its
/// predecessor's link, so its launch never lists on its own card.
fn is_own_launch(snapshot: &SidebarSnapshot, at: RowAt, facts: &LaunchedFacts) -> bool {
    let row = row_of(snapshot, at);
    snapshot
        .agents
        .iter()
        .find(|agent| agent.kind.as_str() == row.name && agent.agent_id.as_str() == row.id)
        .is_some_and(|agent| {
            facts
                .launches
                .iter()
                .any(|id| id == agent.launch_key().as_str())
        })
}
