//! Who occupies a checkout for loop takeover and workflow admission, and
//! whether a takeover may stop them.
//!
//! The caller supplies a runtime-scoped snapshot and card-status reader before
//! this reads owned worktrees and then the clock. It checks candidate owners
//! again because a cached row may outlive its process. An occupant needs a
//! pane stamp and an owner not known dead; no multiplexer is read. The rows stay
//! unchanged for the caller's stop. The read-to-close window includes discovery,
//! park evidence and owner probes: an agent starting a turn after the row read
//! is still stopped.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::agents::{AgentState, AgentStatus};
use crate::harness::auto_continue::CardStatus;
use crate::store::runtime::{AgentLiveness, agent_liveness};
use crate::store::snapshot::SidebarSnapshot;
use crate::utils::path::normalize_path_lexical;
use crate::worktree::{self, WorktreeErr};

/// The open turn that keeps a takeover from stopping an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenTurn {
    Working,
    WaitingOnUser,
    Paused,
    Compacting,
}

/// What a takeover of one checkout does with the agents already in it.
#[derive(Debug)]
pub enum Takeover<'a> {
    /// Every occupant is settled. Roots come before launched children, which
    /// a parent's stop already takes with it.
    Stop(Vec<&'a AgentState>),
    /// An occupant, or a live child one launched, holds an open turn; nothing
    /// is stopped.
    Blocked(Vec<(&'a AgentState, OpenTurn)>),
}

/// Read checkout occupancy from the caller's runtime-scoped snapshot.
/// The caller must read its snapshot, including rest certificates and
/// local-session bindings, before this call and supply its card-status reader.
/// Candidate owners are checked again because the cached roster can outlive
/// them; known-dead owners do not occupy. Returned agents borrow those unchanged
/// rows; the clock is sampled after owned-worktree discovery. No multiplexer is
/// consulted.
pub fn read<'a>(
    snapshot: &'a SidebarSnapshot,
    card_status: &CardStatus,
    checkout: &Path,
    project_root: &Path,
) -> Result<Takeover<'a>, WorktreeErr> {
    let checkouts = path_forms(checkout.to_path_buf()).collect::<Vec<_>>();
    let owned = match worktree::discover_owned(project_root) {
        Ok(owned) => owned
            .into_iter()
            .flat_map(|tree| path_forms(tree.marker.worktree_path))
            .collect(),
        Err(WorktreeErr::NotRepo) => Vec::new(),
        Err(error) => return Err(error),
    };
    Ok(plan(
        snapshot,
        card_status,
        &checkouts,
        &owned,
        Timestamp::now(),
        agent_liveness,
    ))
}

fn path_forms(path: PathBuf) -> impl Iterator<Item = PathBuf> {
    let physical = path.canonicalize().ok();
    std::iter::once(path).chain(physical)
}

/// Decide the takeover of one checkout. An occupant is a launch with a pane
/// stamp and owner liveness other than `Dead`, recorded at or under the
/// checkout, read through the one row that currently is that launch; `owned`
/// worktrees nested inside it keep their own agents. `checkouts` and `owned`
/// carry every path form `read` resolved (as launched, and physical where a
/// symlink leads there), since a row may record either. The snapshot must
/// carry rest certificates.
fn plan<'a>(
    snapshot: &'a SidebarSnapshot,
    card_status: &CardStatus,
    checkouts: &[PathBuf],
    owned: &[PathBuf],
    now: Timestamp,
    liveness: impl Fn(&AgentState) -> AgentLiveness,
) -> Takeover<'a> {
    let checkouts: Vec<PathBuf> = checkouts
        .iter()
        .map(|path| normalize_path_lexical(path))
        .collect();
    let nested: Vec<PathBuf> = owned
        .iter()
        .map(|path| normalize_path_lexical(path))
        .filter(|path| !checkouts.contains(path) && is_under_any(path, &checkouts))
        .collect();
    let mut occupants: Vec<&AgentState> = crate::address::launch_occupants(snapshot.agents.iter())
        .into_iter()
        .filter(|agent| {
            occupies(agent, &checkouts, &nested) && liveness(agent) != AgentLiveness::Dead
        })
        .collect();
    occupants.sort_by_key(|agent| agent.is_launched_child());
    let mut blockers = Vec::new();
    for occupant in &occupants {
        collect_open_turns(snapshot, card_status, occupant, now, &mut blockers);
    }
    if blockers.is_empty() {
        Takeover::Stop(occupants)
    } else {
        Takeover::Blocked(blockers)
    }
}

fn is_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

fn occupies(agent: &AgentState, checkouts: &[PathBuf], nested: &[PathBuf]) -> bool {
    if agent.ended_at.is_some() || agent.pane.is_none() || agent.is_provider_subagent() {
        return false;
    }
    let Some(path) = agent.working_path() else {
        return false;
    };
    let path = normalize_path_lexical(Path::new(path));
    is_under_any(&path, checkouts) && !is_under_any(&path, nested)
}

fn collect_open_turns<'a>(
    snapshot: &'a SidebarSnapshot,
    card_status: &CardStatus,
    agent: &'a AgentState,
    now: Timestamp,
    blockers: &mut Vec<(&'a AgentState, OpenTurn)>,
) {
    if blockers.iter().any(|(seen, _)| std::ptr::eq(*seen, agent)) {
        return;
    }
    if let Some(turn) = open_turn(snapshot, card_status, agent, now) {
        blockers.push((agent, turn));
    }
    for child in crate::address::launched_children(&snapshot.agents, agent)
        .into_iter()
        .filter(|child| child.ended_at.is_none())
    {
        collect_open_turns(snapshot, card_status, child, now, blockers);
    }
}

fn open_turn(
    snapshot: &SidebarSnapshot,
    card_status: &CardStatus,
    agent: &AgentState,
    now: Timestamp,
) -> Option<OpenTurn> {
    if agent.is_awaiting_input() {
        return Some(OpenTurn::WaitingOnUser);
    }
    match card_status.status(snapshot, agent, now) {
        AgentStatus::Running => Some(OpenTurn::Working),
        AgentStatus::Waiting => Some(OpenTurn::WaitingOnUser),
        AgentStatus::Paused => Some(OpenTurn::Paused),
        AgentStatus::Idle | AgentStatus::Success | AgentStatus::Failed | AgentStatus::Sleeping => {
            agent.is_compacting(now).then_some(OpenTurn::Compacting)
        }
    }
}

#[cfg(test)]
mod tests;
