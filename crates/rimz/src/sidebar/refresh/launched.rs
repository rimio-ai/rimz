//! Producer-published launched peers: each agent or team an agent in this room launched
//! directly (`AgentState::launched_by`), one entry per solo launch or team cohort, read on
//! the room's Audit rows so an entry outlives its rows' end for the lifetime a booked fire
//! keeps (`super::bookings::Lifetime`). A subagent, a booked fire's launch, and a workflow
//! step carry no such link. Which card owns an entry and what it has alive are joined in the
//! fold (`crate::sidebar::enrich::launched`).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::{AgentState, LaunchedBy};
use crate::config::{MachineConfig, TeamsConfig};
use crate::harness::cache_keepalive::SeatPolicy;
use crate::harness::when::CiSource;
use crate::store::snapshot::{BookingPr, BookingStarts, SidebarSnapshot};
use crate::{RuntimePaths, Store};

use super::bookings::{CONSUMED_TREELESS_SECS, Lifetime, agent_over, audit_rows, branch_pr};

#[cfg(test)]
mod tests;

const LAUNCHED_CACHE_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::sidebar) struct LaunchedCache {
    pub version: u32,
    pub launched: Vec<LaunchedFacts>,
}

/// One launch an agent made directly: a solo peer, or every seat of a team cohort.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::sidebar) struct LaunchedFacts {
    pub launched_by: LaunchedBy,
    /// A team by its name, else the peer's profile, else its kind.
    pub starts: BookingStarts,
    /// A team cohort's lane other than trunk's, else the peer's named lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// A team cohort's instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<crate::ids::TeamInstanceId>,
    /// A team's configured leader role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
    /// A solo peer's launch description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The launch ids of its rows, sorted, each once.
    pub launches: Vec<String>,
    /// Its first registration.
    pub at: Timestamp,
    /// Once none of its rows is alive, the newest of their end stamps (its first
    /// registration when none stamped one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Timestamp>,
    /// The branch in the RimZ marker of the tree its rows run in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// That tree; `None` for a launch that runs in no RimZ worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<PathBuf>,
    pub pr: BookingPr,
}

/// The launches found past their lifetime, by their launch ids, retained across passes.
#[derive(Debug, Default)]
pub(super) struct LaunchedState {
    expired: BTreeSet<Vec<String>>,
}

pub(in crate::sidebar) fn read_launched_cache(path: &Path) -> LaunchedCache {
    let cache: LaunchedCache = crate::disk::atomic::read_json_cache(path);
    if cache.version == LAUNCHED_CACHE_VERSION {
        cache
    } else {
        LaunchedCache::default()
    }
}

pub(super) fn refresh_launched_for(
    snapshot: &SidebarSnapshot,
    store: Option<&Store>,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    loaded_teams: Option<&TeamsConfig>,
    state: &mut LaunchedState,
) {
    let path = runtime.launched_path();
    let prior = read_launched_cache(&path);
    let launched = match snapshot.project_root.as_deref() {
        Some(root) if wanted(snapshot, &prior) => {
            let Some(rows) = audit_rows(store) else {
                return;
            };
            let room = Room {
                rows: &rows,
                source: &CiSource::read(runtime),
                root,
                now: snapshot.now,
                window_secs: CONSUMED_TREELESS_SECS,
            };
            let mut launched = collect(&room, &prior, &mut state.expired);
            if launched
                .iter()
                .any(|facts| matches!(facts.starts, BookingStarts::Team(_)))
            {
                match loaded_teams {
                    Some(teams) => name_leaders(&mut launched, teams),
                    None => {
                        name_leaders(&mut launched, &SeatPolicy::load(config, Some(root)).teams)
                    }
                }
            }
            launched
        }
        _ => Vec::new(),
    };
    let refreshed = LaunchedCache {
        version: LAUNCHED_CACHE_VERSION,
        launched,
    };
    if refreshed == prior {
        return;
    }
    if let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(&path, &refreshed) {
        tracing::debug!(path = %path.display(), %error, "sidebar launched cache write failed");
    }
}

/// Whether the pass reads the room's Audit rows: the room has an agent, or the last
/// publication holds an entry. A peer that launched and ended between two passes has no
/// row in the fold, so no row of the fold can gate the read.
fn wanted(snapshot: &SidebarSnapshot, prior: &LaunchedCache) -> bool {
    !prior.launched.is_empty() || !snapshot.agents.is_empty()
}

/// A row an agent launched directly: it carries the launcher's link, and is no
/// `rimz subagents` child and no booked fire's launch.
fn launched_peer(agent: &AgentState) -> bool {
    agent.launched_by.is_some() && !agent.is_launched_child() && agent.loop_fire.is_none()
}

/// The room one pass reads.
struct Room<'a> {
    /// The room's Audit rows, ended and replaced ones included.
    rows: &'a [AgentState],
    source: &'a CiSource,
    root: &'a Path,
    now: Timestamp,
    window_secs: u64,
}

/// Every launched entry of `room` in its lifetime, in launch order, with the entries of
/// `prior` whose rows are gone kept while their lifetime holds; the launches found past it
/// join `expired`, which no later pass reads again.
fn collect(
    room: &Room<'_>,
    prior: &LaunchedCache,
    expired: &mut BTreeSet<Vec<String>>,
) -> Vec<LaunchedFacts> {
    // A team groups by its cohort, a solo peer by its launch, so a molt's two rows on one
    // launch id are one entry; first-seen order breaks a tie on the launch time.
    let mut groups: Vec<(&LaunchedBy, Vec<&AgentState>)> = Vec::new();
    let mut at: HashMap<(&str, &str, String), usize> = HashMap::new();
    for agent in room.rows.iter().filter(|agent| launched_peer(agent)) {
        let Some(launcher) = agent.launched_by.as_ref() else {
            continue;
        };
        let key = agent.team_instance().map_or_else(
            || agent.launch_key().to_string(),
            |instance| format!("team {instance}"),
        );
        let index = *at
            .entry((launcher.kind.as_str(), launcher.agent_id.as_str(), key))
            .or_insert_with(|| {
                groups.push((launcher, Vec::new()));
                groups.len() - 1
            });
        groups[index].1.push(agent);
    }
    let mut facts: Vec<LaunchedFacts> = Vec::new();
    for (launcher, rows) in groups {
        let launches: BTreeSet<String> = rows
            .iter()
            .map(|agent| agent.launch_key().to_string())
            .collect();
        let launches: Vec<String> = launches.into_iter().collect();
        // A resumed peer keeps its launch id: a row of it alive lists it again, and its
        // lifetime starts over.
        if rows.iter().any(|agent| !agent_over(room.rows, agent)) {
            expired.remove(&launches);
        } else if expired.contains(&launches) {
            continue;
        }
        let entry = read_entry(room, launcher, &rows, launches);
        let before = prior
            .launched
            .iter()
            .find(|before| before.launches.iter().any(|id| entry.launches.contains(id)));
        facts.push(carry(room, entry, before));
    }
    let carried: Vec<LaunchedFacts> = prior
        .launched
        .iter()
        .filter(|before| {
            !room
                .rows
                .iter()
                .any(|agent| before.launches.contains(&agent.launch_key().to_string()))
        })
        .map(|before| {
            let pr = before
                .branch
                .as_deref()
                .map_or(BookingPr::Unread, |branch| {
                    branch_pr(room.source, branch, room.root)
                });
            carry(
                room,
                LaunchedFacts {
                    pr,
                    ..before.clone()
                },
                Some(before),
            )
        })
        .collect();
    facts.extend(carried);
    facts.sort_by_key(|facts| facts.at);
    let (kept, gone): (Vec<_>, Vec<_>) = facts.into_iter().partition(|facts| {
        let alive = facts.ended_at.is_none()
            && room.rows.iter().any(|agent| {
                facts.launches.contains(&agent.launch_key().to_string())
                    && !agent_over(room.rows, agent)
            });
        Lifetime {
            at: facts.at,
            ended_at: facts.ended_at,
            alive,
            checkout: facts.checkout.as_deref(),
            pr: facts.pr,
        }
        .holds(room.now, room.window_secs)
    });
    expired.extend(gone.into_iter().map(|facts| facts.launches));
    kept
}

/// One entry from its `rows`: what it starts, its lane and its text from its newest row
/// (by swap epoch, then registration; the Audit rows arrive by kind and agent id), its end
/// once none of them is alive, and the branch and checkout of the RimZ tree they run in.
fn read_entry(
    room: &Room<'_>,
    launcher: &LaunchedBy,
    rows: &[&AgentState],
    launches: Vec<String>,
) -> LaunchedFacts {
    let mut rows = rows.to_vec();
    rows.sort_by_key(|agent| std::cmp::Reverse((agent.epoch, agent.registered_at)));
    let newest = rows[0];
    let at = rows
        .iter()
        .map(|agent| agent.registered_at.unwrap_or(agent.last_activity))
        .min()
        .unwrap_or(newest.last_activity);
    let alive = rows.iter().any(|agent| !agent_over(room.rows, agent));
    let ended_at = (!alive).then(|| {
        rows.iter()
            .filter_map(|agent| agent.ended_at)
            .max()
            .unwrap_or(at)
    });
    let team = newest.team_instance();
    let (starts, lane) = match &team {
        Some(instance) => (
            BookingStarts::Team(instance.team().to_owned()),
            instance
                .lane()
                .filter(|lane| **lane != crate::ids::LaneName::trunk())
                .map(ToString::to_string),
        ),
        None => (
            BookingStarts::Profile(
                newest
                    .profile
                    .clone()
                    .unwrap_or_else(|| newest.kind.to_string()),
            ),
            newest
                .lane
                .as_ref()
                .and_then(crate::ids::Lane::name)
                .map(ToString::to_string),
        ),
    };
    let tree = rows
        .iter()
        .find_map(|agent| agent.worktree_path.as_deref())
        .map(PathBuf::from);
    let marked = tree.as_deref().and_then(|tree| {
        crate::worktree::read_marker_from_checkout_metadata(tree)
            .ok()
            .flatten()
    });
    let branch = marked.map(|marker| marker.branch);
    LaunchedFacts {
        launched_by: launcher.clone(),
        starts,
        lane,
        description: team
            .is_none()
            .then(|| rows.iter().find_map(|agent| agent.description.as_deref()))
            .flatten()
            .and_then(crate::agents::single_line_description),
        team,
        leader: None,
        launches,
        at,
        ended_at,
        pr: branch.as_deref().map_or(BookingPr::Unread, |branch| {
            branch_pr(room.source, branch, room.root)
        }),
        checkout: branch.as_ref().and(tree),
        branch,
    }
}

/// Names each team entry's configured leader role, the seat `rimz teams focus` falls back
/// to.
fn name_leaders(launched: &mut [LaunchedFacts], teams: &TeamsConfig) {
    for facts in launched {
        if let BookingStarts::Team(name) = &facts.starts {
            facts.leader = teams.0.get(name).and_then(|team| team.leader.clone());
        }
    }
}

/// Carries what this pass no longer reads from the last publication of the entry: once its
/// checkout's marker is unreadable (the tree removed), the branch and tree it read, so the
/// entry ends with its checkout; and a merged reading once the pull request lane no longer
/// reads its branch.
fn carry(
    room: &Room<'_>,
    mut facts: LaunchedFacts,
    before: Option<&LaunchedFacts>,
) -> LaunchedFacts {
    if facts.branch.is_none()
        && let Some(before) = before
        && let Some(branch) = &before.branch
    {
        facts.pr = branch_pr(room.source, branch, room.root);
        facts.branch = Some(branch.clone());
        facts.checkout.clone_from(&before.checkout);
    }
    if facts.branch.is_some()
        && facts.pr == BookingPr::Unread
        && let Some(before) = before
        && matches!(before.pr, BookingPr::Merged { .. })
    {
        facts.pr = before.pr;
    }
    facts
}

/// The pull request lane's demand: each launched entry's own branch until its pull request
/// merged.
pub(in crate::sidebar) fn branch_demand(cache: &LaunchedCache) -> BTreeSet<String> {
    cache
        .launched
        .iter()
        .filter(|facts| !matches!(facts.pr, BookingPr::Merged { .. }))
        .filter_map(|facts| facts.branch.clone())
        .collect()
}
