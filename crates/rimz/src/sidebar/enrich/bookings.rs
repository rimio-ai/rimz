//! The bookings fold: joins the published bookings cache to this fold's rows. Which card
//! owns each booking (the nearest living agent up its chain), what its launch has alive,
//! its band and its place in the card's list are row facts, so they are decided here
//! from the same rows the card is drawn from. Reads only; a booking reaches no status,
//! attention, count or sort.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use jiff::Timestamp;

use crate::agents::{AgentState, AgentStatus};
use crate::store::snapshot::{
    BookingBand, BookingBlocker, BookingFire, BookingLive, BookingPr, BookingSibling, BookingStage,
    BookingState, BookingWait, SiblingState, SidebarBooking, SidebarRow, SidebarSnapshot,
};

use super::super::refresh::bookings::{BookingFacts, BookingsCache, FireFacts, launched_by};

#[cfg(test)]
mod tests;

/// A row's place in the fold: group index, row index.
type RowAt = (usize, usize);

/// Attach every booking of `cache` to the card that owns it, in draw order, and clear
/// the bookings of every other card. A row is copied only when its list changes.
pub(super) fn attach(snapshot: &mut SidebarSnapshot, cache: &BookingsCache) {
    let rows = row_index(snapshot);
    let mut cards: BTreeMap<RowAt, Vec<Entry<'_>>> = BTreeMap::new();
    for facts in &cache.bookings {
        let Some(owner) = owner(snapshot, &rows, facts) else {
            continue;
        };
        let adopted = !answers_link(snapshot, owner, facts.booked_by.first());
        cards.entry(owner).or_default().push(Entry {
            facts,
            fire: facts
                .fire
                .as_ref()
                .map(|fire| (fire, launched(snapshot, &rows, facts, fire))),
            adopted,
        });
    }
    let mut lists: BTreeMap<RowAt, Vec<SidebarBooking>> = cards
        .into_iter()
        .map(|(at, entries)| (at, card_list(snapshot, &rows, entries)))
        .collect();
    let groups = Arc::make_mut(&mut snapshot.worktree_groups);
    for (group_at, group) in groups.iter_mut().enumerate() {
        let wanted: Vec<_> = (0..group.rows.len())
            .map(|row_at| lists.remove(&(group_at, row_at)).unwrap_or_default())
            .collect();
        let changed = group
            .rows
            .iter()
            .zip(&wanted)
            .any(|(row, wanted)| row.as_agent().is_some_and(|card| card.bookings != *wanted));
        if !changed {
            continue;
        }
        for (row, wanted) in Arc::make_mut(&mut group.rows).iter_mut().zip(wanted) {
            if row.as_agent().is_some_and(|card| card.bookings != wanted)
                && let Some(card) = row.as_agent_mut()
            {
                card.bookings = wanted;
            }
        }
    }
}

/// Every agent row of the fold by its kind and session id.
fn row_index(snapshot: &SidebarSnapshot) -> HashMap<(&str, &str), RowAt> {
    snapshot
        .worktree_groups
        .iter()
        .enumerate()
        .flat_map(|(group_at, group)| {
            group
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.is_agent())
                .map(move |(row_at, row)| {
                    ((row.name.as_str(), row.id.as_str()), (group_at, row_at))
                })
        })
        .collect()
}

fn row_of(snapshot: &SidebarSnapshot, at: RowAt) -> &SidebarRow {
    &snapshot.worktree_groups[at.0].rows[at.1]
}

fn agent_row(rows: &HashMap<(&str, &str), RowAt>, agent: &AgentState) -> Option<RowAt> {
    rows.get(&(agent.kind.as_str(), agent.agent_id.as_str()))
        .copied()
}

/// D2: the first link of the chain, nearest first, whose living launch has a row here.
fn owner(
    snapshot: &SidebarSnapshot,
    rows: &HashMap<(&str, &str), RowAt>,
    facts: &BookingFacts,
) -> Option<RowAt> {
    facts
        .booked_by
        .iter()
        .find_map(|link| living_row(snapshot, rows, link))
}

/// The row of a chain link's launch while it lives: not ended, and with a row here.
fn living_row(
    snapshot: &SidebarSnapshot,
    rows: &HashMap<(&str, &str), RowAt>,
    link: &crate::agents::LaunchedBy,
) -> Option<RowAt> {
    crate::address::launch_row(snapshot.agents.iter(), &link.kind, &link.agent_id)
        .filter(|agent| agent.ended_at.is_none())
        .and_then(|agent| agent_row(rows, agent))
}

/// Whether the card at `at` answers to `link`: an entry it does not is adopted.
fn answers_link(
    snapshot: &SidebarSnapshot,
    at: RowAt,
    link: Option<&crate::agents::LaunchedBy>,
) -> bool {
    let row = row_of(snapshot, at);
    link.is_some_and(|link| {
        crate::address::launch_row(snapshot.agents.iter(), &link.kind, &link.agent_id).is_some_and(
            |agent| agent.kind.as_str() == row.name && agent.agent_id.as_str() == row.id,
        )
    })
}

/// The living rows a fire launched: its living leader with a row here, then its team
/// cohort.
fn launched<'s>(
    snapshot: &'s SidebarSnapshot,
    rows: &HashMap<(&str, &str), RowAt>,
    facts: &BookingFacts,
    fire: &FireFacts,
) -> Vec<&'s AgentState> {
    let Some(leader) = snapshot.agents.iter().find(|agent| {
        agent.ended_at.is_none()
            && agent_row(rows, agent).is_some()
            && launched_by(agent, &facts.task, fire.run.as_ref(), fire.at)
    }) else {
        return Vec::new();
    };
    let Some(instance) = leader.team_instance() else {
        return vec![leader];
    };
    let mut members = vec![leader];
    members.extend(snapshot.agents.iter().filter(|agent| {
        !std::ptr::eq(*agent, leader)
            && agent.ended_at.is_none()
            && agent.team_instance().as_ref() == Some(&instance)
            && agent_row(rows, agent).is_some()
    }));
    members
}

struct Entry<'c> {
    facts: &'c BookingFacts,
    fire: Option<(&'c FireFacts, Vec<&'c AgentState>)>,
    adopted: bool,
}

impl Entry<'_> {
    fn sibling_state(&self, snapshot: &SidebarSnapshot) -> SiblingState {
        match &self.fire {
            None => SiblingState::Waiting,
            Some((_, live)) if !live.is_empty() => SiblingState::Running,
            Some((fire, _)) if starting(snapshot, self.facts, fire, false) => {
                SiblingState::Starting
            }
            Some(_) => SiblingState::Ended,
        }
    }
}

/// A fire is starting while nothing it launched is alive or has a row, and the records hold
/// no end of its agent step.
fn starting(
    snapshot: &SidebarSnapshot,
    facts: &BookingFacts,
    fire: &FireFacts,
    alive: bool,
) -> bool {
    !alive
        && fire.ended_at.is_none()
        && !snapshot
            .agents
            .iter()
            .any(|agent| launched_by(agent, &facts.task, fire.run.as_ref(), fire.at))
}

/// The sibling on the card whose destination branch the forge term `key` reads.
fn sibling_for<'e, 'c>(entries: &'e [Entry<'c>], own: usize, key: &str) -> Option<usize> {
    let branch = crate::harness::when::forge_branch(key)?;
    (0..entries.len())
        .find(|&index| index != own && entries[index].facts.branch.as_deref() == Some(branch))
}

/// The fired sibling whose fire launched an adopted entry's booker: up the chain to the
/// first living link, the first link a fired sibling recorded among its `launches` names
/// that sibling. A booker no sibling recorded (a direct peer, or one that ended before any
/// pass saw it) leaves the entry in its ordinary place.
fn parent(
    snapshot: &SidebarSnapshot,
    rows: &HashMap<(&str, &str), RowAt>,
    entries: &[Entry<'_>],
    own: usize,
) -> Option<usize> {
    let entry = &entries[own];
    if !entry.adopted {
        return None;
    }
    for link in &entry.facts.booked_by {
        if living_row(snapshot, rows, link).is_some() {
            return None;
        }
        let launcher = (0..entries.len()).find(|&index| {
            index != own
                && entries[index].fire.as_ref().is_some_and(|(fire, _)| {
                    fire.launches.iter().any(|id| id == link.agent_id.as_str())
                })
        });
        if launcher.is_some() {
            return launcher;
        }
    }
    None
}

/// One card's entries as rows, in draw order with their bands (D6, D7, D8, D11).
fn card_list(
    snapshot: &SidebarSnapshot,
    rows: &HashMap<(&str, &str), RowAt>,
    entries: Vec<Entry<'_>>,
) -> Vec<SidebarBooking> {
    let blockers: Vec<Option<usize>> = (0..entries.len())
        .map(|index| {
            let entry = &entries[index];
            entry
                .fire
                .is_none()
                .then_some(entry.facts.blocking.as_ref())
                .flatten()
                .and_then(|term| sibling_for(&entries, index, &term.key))
        })
        .collect();
    let depended_on = |index: usize| {
        entries.iter().enumerate().any(|(waiter, entry)| {
            entry.fire.is_none()
                && entry
                    .facts
                    .unmet
                    .iter()
                    .chain(entry.facts.blocking.as_ref().map(|term| &term.key))
                    .any(|key| sibling_for(&entries, waiter, key) == Some(index))
        })
    };
    let built: Vec<(SidebarBooking, Option<usize>)> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let state = match &entry.fire {
                None => BookingState::Waiting(BookingWait {
                    blocker: entry.facts.blocking.as_ref().map(|term| BookingBlocker {
                        after: blockers[index].map(|sibling| BookingSibling {
                            task: entries[sibling].facts.task.clone(),
                            state: entries[sibling].sibling_state(snapshot),
                        }),
                        key: term.key.clone(),
                        reading: term.reading.clone(),
                        pr: term.pr,
                        more: term.more,
                    }),
                    timing: entry.facts.timing.clone(),
                }),
                Some((fire, members)) => {
                    let depended_on = depended_on(index);
                    BookingState::Fired(fired(
                        snapshot,
                        rows,
                        entry.facts,
                        fire,
                        members,
                        depended_on,
                    ))
                }
            };
            let band = band(&entries, index, &state);
            let booking = SidebarBooking {
                task: entry.facts.task.clone(),
                starts: entry.facts.starts.clone(),
                band,
                needs_fix: entry.facts.needs_fix.clone(),
                state,
            };
            (booking, parent(snapshot, rows, &entries, index))
        })
        .collect();
    order(fold_older(built))
}

/// A fired entry's state (D9, D11). It is starting as `starting` rules; finished once its
/// pull request merged, or, with nothing waiting on it, once a team's board reads `Done` or
/// a launch with no worktree has nothing alive.
fn fired(
    snapshot: &SidebarSnapshot,
    rows: &HashMap<(&str, &str), RowAt>,
    facts: &BookingFacts,
    fire: &FireFacts,
    members: &[&AgentState],
    depended_on: bool,
) -> BookingFire {
    let live = live(snapshot, rows, facts, members);
    let starting = starting(snapshot, facts, fire, live.is_some());
    let done = fire.team_done == Some(true)
        || live
            .as_ref()
            .and_then(|live| live.stage.as_ref())
            .is_some_and(|stage| stage.done);
    let ended_treeless = live.is_none() && !starting && !facts.worktree;
    BookingFire {
        at: fire.at,
        ended_at: fire.ended_at,
        pr: fire.pr,
        finished: matches!(fire.pr, BookingPr::Merged { .. })
            || ((done || ended_treeless) && !depended_on),
        depended_on,
        starting,
        live,
    }
}

/// The band before the ended cap: needs a fix, fired, later when every unmet term
/// joins an unfired sibling, else next.
fn band(entries: &[Entry<'_>], index: usize, state: &BookingState) -> BookingBand {
    let entry = &entries[index];
    if entry.facts.needs_fix.is_some() {
        return BookingBand::NeedsFix;
    }
    if matches!(state, BookingState::Fired(_)) {
        return BookingBand::Fired;
    }
    let later = !entry.facts.unmet.is_empty()
        && entry.facts.unmet.iter().all(|key| {
            sibling_for(entries, index, key).is_some_and(|sibling| entries[sibling].fire.is_none())
        });
    if later {
        BookingBand::Later
    } else {
        BookingBand::Next
    }
}

/// Ended entries a card lists before the rest fold into `Older`: the renderer's cap on
/// finished subagents (`MAX_FINISHED` in the agent card's bands), which the data plane
/// cannot import.
const MAX_LISTED_ENDED: usize = 5;

/// The section's finished rule over ended entries: an entry nothing alive, not starting,
/// nothing waiting on it, and no unfinished adopted entry below it at any depth, folds
/// into `Older` past the newest `MAX_LISTED_ENDED` by end time.
fn fold_older(
    mut built: Vec<(SidebarBooking, Option<usize>)>,
) -> Vec<(SidebarBooking, Option<usize>)> {
    let mut parents = vec![false; built.len()];
    for (booking, parent) in &built {
        if booking.is_finished() {
            continue;
        }
        let mut at = *parent;
        for _ in 0..built.len() {
            let Some(index) = at.filter(|index| !parents[*index]) else {
                break;
            };
            parents[index] = true;
            at = built[index].1;
        }
    }
    let mut ended: Vec<(Option<Timestamp>, usize)> = built
        .iter()
        .enumerate()
        .filter(|(_, (booking, _))| booking.band == BookingBand::Fired)
        .filter_map(|(index, (booking, _))| match &booking.state {
            BookingState::Fired(fire)
                if fire.live.is_none()
                    && !fire.starting
                    && !fire.depended_on
                    && !parents[index] =>
            {
                Some((fire.ended_at, index))
            }
            _ => None,
        })
        .collect();
    ended.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    for (_, index) in ended.into_iter().skip(MAX_LISTED_ENDED) {
        built[index].0.band = BookingBand::Older;
    }
    built
}

/// Bands in order: fired in fire order, next and later by name, older newest first. Each
/// fired entry is followed, depth first, by the adopted fired and next entries its launch
/// booked; an adopted entry whose parent is not listed fired, or that sits on a cycle of
/// parents, takes its ordinary place. Every entry is drawn once.
fn order(built: Vec<(SidebarBooking, Option<usize>)>) -> Vec<SidebarBooking> {
    let followed = |index: usize| {
        let (booking, parent) = &built[index];
        parent.filter(|parent| {
            matches!(booking.band, BookingBand::Fired | BookingBand::Next)
                && built[*parent].0.band == BookingBand::Fired
        })
    };
    let mut parents: Vec<Option<usize>> = (0..built.len()).map(followed).collect();
    let on_cycle: Vec<bool> = (0..built.len())
        .map(|start| {
            let mut at = parents[start];
            for _ in 0..built.len() {
                match at {
                    Some(index) if index == start => return true,
                    Some(index) => at = parents[index],
                    None => return false,
                }
            }
            false
        })
        .collect();
    for (parent, cyclic) in parents.iter_mut().zip(on_cycle) {
        if cyclic {
            *parent = None;
        }
    }
    let rank = |band: BookingBand| match band {
        BookingBand::NeedsFix => 0,
        BookingBand::Fired => 1,
        BookingBand::Next => 2,
        BookingBand::Later => 3,
        BookingBand::Older => 4,
    };
    let key = |index: &usize| {
        let booking = &built[*index].0;
        let fired = match &booking.state {
            BookingState::Fired(fire) => Some(fire.at),
            BookingState::Waiting(_) => None,
        };
        let older = (booking.band == BookingBand::Older).then_some(std::cmp::Reverse(fired));
        (rank(booking.band), older, fired, booking.task.clone())
    };
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); built.len()];
    let mut roots = Vec::new();
    for (index, parent) in parents.into_iter().enumerate() {
        match parent {
            Some(parent) => children[parent].push(index),
            None => roots.push(index),
        }
    }
    roots.sort_by_key(key);
    for list in &mut children {
        list.sort_by_key(key);
    }
    let mut stack: Vec<usize> = roots.into_iter().rev().collect();
    let mut drawn = Vec::with_capacity(built.len());
    while let Some(index) = stack.pop() {
        drawn.push(index);
        stack.extend(children[index].iter().rev());
    }
    let mut slots: Vec<Option<SidebarBooking>> = built
        .into_iter()
        .map(|(booking, _)| Some(booking))
        .collect();
    drawn
        .into_iter()
        .filter_map(|index| slots[index].take())
        .collect()
}

/// What a fire has alive: the lead's status (a team's most urgent member), the focus
/// pane, and a team's board stage. `None` once nothing is.
fn live(
    snapshot: &SidebarSnapshot,
    rows: &HashMap<(&str, &str), RowAt>,
    facts: &BookingFacts,
    members: &[&AgentState],
) -> Option<BookingLive> {
    let leader = *members.first()?;
    let leader_at = agent_row(rows, leader)?;
    let leader_row = row_of(snapshot, leader_at);
    let leader_card = leader_row.as_agent()?;
    let statuses: Vec<AgentStatus> = members
        .iter()
        .filter_map(|member| agent_row(rows, member))
        .filter_map(|at| row_of(snapshot, at).as_agent().map(|card| card.status))
        .collect();
    let status = [
        AgentStatus::Waiting,
        AgentStatus::Failed,
        AgentStatus::Running,
    ]
    .into_iter()
    .find(|urgent| members.len() > 1 && statuses.contains(urgent))
    .unwrap_or(leader_card.status);
    let focus = match leader.team_instance() {
        Some(instance) => {
            let scores: HashMap<&str, u32> = members
                .iter()
                .filter_map(|member| {
                    let at = agent_row(rows, member)?;
                    Some((
                        member.agent_id.as_str(),
                        row_of(snapshot, at).attention_score,
                    ))
                })
                .collect();
            // The cohort in `rimz teams focus` order, so a tie lands where the CLI does.
            let cohort = crate::address::team_cohorts(snapshot.agents.iter())
                .into_iter()
                .find(|cohort| cohort.instance == instance)
                .map(|cohort| crate::address::TeamCohort {
                    members: cohort
                        .members
                        .into_iter()
                        .filter(|member| members.iter().any(|live| std::ptr::eq(*live, *member)))
                        .collect(),
                    ..cohort
                })
                .filter(|cohort| !cohort.members.is_empty())
                .unwrap_or_else(|| crate::address::TeamCohort {
                    instance,
                    members: members.to_vec(),
                });
            cohort.focus_member(facts.leader.as_deref(), &scores, snapshot.now)
        }
        None => leader,
    };
    let pane = agent_row(rows, focus)
        .and_then(|at| row_of(snapshot, at).pane.as_ref())
        .map(|pane| pane.pane_id.clone());
    let stage = snapshot.worktree_groups[leader_at.0]
        .pipeline
        .as_ref()
        .filter(|_| leader.team_instance().is_some())
        .map(|pipeline| BookingStage {
            stage: pipeline.stage.clone(),
            owner: pipeline.owner.clone(),
            done: pipeline.done_at.is_some(),
        });
    Some(BookingLive {
        status,
        phase: leader_card.phase,
        handle: leader_card.handle.clone(),
        stage,
        pane,
    })
}
