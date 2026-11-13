//! An agent's lane, decided once at a fresh launch.
//!
//! [`decide`] is the one constructor of a fresh launch's lane; relaunches copy
//! the row's stored lane and never decide again.
//! [`crate::worktree::own_lane`] classifies a directory into its checkout's own
//! lane without spawning Git, so the constructor, registration, and every
//! reader agree on it. The lane rules
//! live beside them: a typed name is flat, a name a checkout owns (trunk's
//! `main`, a managed worktree's directory) means that checkout's own lane, an
//! in-place team at trunk is the named lane [`team_lane`] gives it, admitted like a
//! typed name but never refused as unflat, and one named lane is held at one
//! checkout while live agents hold it.

use std::path::{Path, PathBuf};

use crate::agents::AgentState;
use crate::ids::{Lane, LaneHome, LaneName};
use crate::store::runtime::{AgentLiveness, agent_liveness};

#[derive(Debug, thiserror::Error)]
pub enum LaneErr {
    #[error("invalid channel name `{name}`; use ASCII letters, numbers, `_`, or `-`")]
    InvalidName { name: String },
    #[error("channel `{name}` is backed by a worktree; use `--worktree {name}`")]
    WorktreeCollision { name: String },
    #[error(
        "channel `{name}` is the project checkout's own lane; launch at the project root, or pick another name"
    )]
    TrunkCollision { name: String },
    #[error(
        "channel `{name}` is held by live agents at `{}`; launch there, or pick another name",
        checkout.display()
    )]
    HeldElsewhere { name: String, checkout: PathBuf },
    #[error(
        "channel `{name}` is held by live agents; pick another name, or stop them with `rimz agents stop`"
    )]
    LaneOccupied { name: String },
    #[error(
        "team `{team}` cannot run in place: `{team}` is the project checkout's own lane; launch it in a worktree with `--worktree <name>`"
    )]
    TeamOwnLane { team: String },
}

/// The lane of the launch's caller, when it has one.
#[derive(Clone, Debug)]
pub enum CallerLane {
    /// The calling agent's row lane.
    Agent(Lane),
    /// A caller-less shell pane's pin: `RIMZ_CHANNEL` homed at `RIMZ_WORKTREE_PATH`.
    Pin(Lane),
}

/// Everything a fresh launch's lane is decided from.
pub struct LaneRequest<'a> {
    pub project_root: &'a Path,
    /// The target directory before empty-directory displacement.
    pub target: &'a Path,
    /// The typed `--channel`.
    pub typed: Option<&'a str>,
    /// The in-place team the launch starts.
    pub team: Option<&'a str>,
    /// A bare role was qualified into a live cohort of the current lane.
    pub joins_cohort: bool,
    pub caller: Option<CallerLane>,
    pub agents: &'a [AgentState],
}

/// Decide a fresh launch's lane, first match: a typed name the target checkout
/// owns, a typed name, a bare role joining the caller's named lane, an in-place
/// team at trunk (the named lane of its dashed name, under a typed name's checks
/// apart from flatness), the caller's named lane, the target checkout's own
/// lane. Only the caller's lane homed at the target checkout carries over; a pin
/// that fails a typed name's checks is ignored.
pub fn decide(request: &LaneRequest<'_>) -> Result<Lane, LaneErr> {
    let own = crate::worktree::own_lane(request.project_root, request.target);
    if let Some(typed) = request.typed.filter(|typed| !typed.is_empty()) {
        if owned_names(&own).iter().any(|owned| owned == typed) {
            return Ok(own);
        }
        let name = admit_typed(request, &own, typed)?;
        return Ok(own.named(name));
    }
    let caller = request.caller.as_ref().and_then(|caller| {
        let (CallerLane::Agent(lane) | CallerLane::Pin(lane)) = caller;
        let name = lane.name()?;
        let admissible = || match caller {
            CallerLane::Agent(_) => true,
            CallerLane::Pin(_) => admit_typed(request, &own, name.as_str()).is_ok(),
        };
        (same_checkout(lane.path(), own.path()) && admissible()).then_some(lane)
    });
    if request.joins_cohort
        && let Some(lane) = caller
    {
        return Ok(lane.clone());
    }
    if let Some(team) = request.team.filter(|team| !team.is_empty())
        && own.is_trunk()
    {
        let name = team_lane(team);
        if owned_names(&own).iter().any(|owned| owned == name.as_str()) {
            return Err(LaneErr::TeamOwnLane {
                team: team.to_owned(),
            });
        }
        admit_named(request, &own, &name)?;
        return Ok(own.named(name));
    }
    Ok(caller.cloned().unwrap_or(own))
}

/// The name of the lane an in-place team runs in at trunk: the team's name,
/// dashed so a name that is not flat still names a lane.
pub fn team_lane(team: &str) -> LaneName {
    LaneName::dashed(team)
}

/// The routing name of `dir`'s own lane; trunk's own lane has none.
pub fn directory_channel(project_root: &Path, dir: &Path) -> Option<LaneName> {
    let lane = crate::worktree::own_lane(project_root, dir);
    if lane.is_trunk() {
        return None;
    }
    Some(lane.routing_name())
}

/// Admit a new managed worktree's name: no live agent holds a named lane under
/// it, apart from agents whose lane path is `dir`, the worktree's own
/// directory before it exists. A holder homed anywhere else refuses it.
pub fn admit_worktree_name(agents: &[AgentState], name: &str, dir: &Path) -> Result<(), LaneErr> {
    let dir = crate::utils::path::normalize_path_lexical(dir);
    let in_worktree = |agent: &AgentState| {
        agent
            .lane_path()
            .is_some_and(|path| crate::utils::path::normalize_path_lexical(Path::new(path)) == dir)
    };
    let occupied = agents
        .iter()
        .filter(|agent| held_live(agent) && !in_worktree(agent))
        .any(|agent| {
            agent
                .lane
                .as_ref()
                .is_some_and(|lane| lane.name().is_some_and(|held| held.as_str() == name))
        });
    if occupied {
        return Err(LaneErr::LaneOccupied {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// A typed name's checks: it is flat, and admitted as a named lane at `own`'s
/// checkout.
fn admit_typed(request: &LaneRequest<'_>, own: &Lane, name: &str) -> Result<LaneName, LaneErr> {
    let invalid = || LaneErr::InvalidName {
        name: name.to_owned(),
    };
    if !LaneName::is_flat(name) {
        return Err(invalid());
    }
    let name = name.parse().map_err(|_| invalid())?;
    admit_named(request, own, &name)?;
    Ok(name)
}

/// A named lane's checks at `own`'s checkout: no other checkout owns it, and no
/// live agents hold it at another checkout.
fn admit_named(request: &LaneRequest<'_>, own: &Lane, name: &LaneName) -> Result<(), LaneErr> {
    let name = name.as_str();
    let project_root = request.project_root;
    let trunk = Lane::own(LaneHome::Trunk, path_text(project_root));
    if managed_worktree_names(project_root)
        .iter()
        .any(|owned| owned == name)
    {
        return Err(LaneErr::WorktreeCollision {
            name: name.to_owned(),
        });
    }
    if !own.is_trunk() && owned_names(&trunk).iter().any(|owned| owned == name) {
        return Err(LaneErr::TrunkCollision {
            name: name.to_owned(),
        });
    }
    let held_at = request
        .agents
        .iter()
        .filter(|agent| held_live(agent))
        .filter_map(|agent| agent.lane.as_ref())
        .find(|lane| {
            lane.name().is_some_and(|held| held.as_str() == name)
                && !same_checkout(lane.path(), own.path())
        });
    if let Some(lane) = held_at {
        return Err(LaneErr::HeldElsewhere {
            name: name.to_owned(),
            checkout: PathBuf::from(lane.path()),
        });
    }
    Ok(())
}

/// The names a checkout's own lane owns: trunk's `main`, a managed worktree's
/// directory.
fn owned_names(own: &Lane) -> Vec<String> {
    match own.home() {
        LaneHome::Trunk | LaneHome::Worktree => vec![own.routing_name().into()],
        LaneHome::Foreign => Vec::new(),
    }
}

/// Every managed worktree's directory name; an unreadable list claims none.
fn managed_worktree_names(project_root: &Path) -> Vec<String> {
    crate::worktree::discover_owned_cached(project_root)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|worktree| {
            let name = worktree.path.file_name()?.to_str()?.to_owned();
            (name != "main").then_some(name)
        })
        .collect()
}

fn held_live(agent: &AgentState) -> bool {
    agent.ended_at.is_none() && agent_liveness(agent) != AgentLiveness::Dead
}

fn same_checkout(a: &str, b: &str) -> bool {
    let comparable = |path: &str| {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| crate::utils::path::normalize_path_lexical(Path::new(path)))
    };
    a == b || comparable(a) == comparable(b)
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
#[path = "lanes/tests.rs"]
pub(crate) mod tests;
