//! An agent's lane, decided once at a fresh launch.
//!
//! [`decide`] is the one constructor of a fresh launch's lane; relaunches copy
//! the row's lane through [`row_lane`] and never decide again. [`own_lane`]
//! classifies a directory into its checkout's own lane without spawning Git, so
//! the constructor, registration, and every reader agree on it. The lane rules
//! live beside them: a typed name is flat, a name a checkout owns (trunk's
//! `main` and directory, a managed worktree's directory) means that checkout's
//! own lane, and one named lane is held at one checkout while live agents hold it.

use std::path::{Path, PathBuf};
use std::str::FromStr;

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
/// team at trunk, the caller's named lane, the target checkout's own lane. Only
/// the caller's lane homed at the target checkout carries over; a pin that
/// fails a typed name's checks is ignored.
pub fn decide(request: &LaneRequest<'_>) -> Result<Lane, LaneErr> {
    let own = own_lane(request.project_root, request.target);
    if let Some(typed) = request.typed.filter(|typed| !typed.is_empty()) {
        if owned_names(&own, request.project_root).contains(&typed) {
            return Ok(own);
        }
        let name = admit_named(request, &own, typed)?;
        return Ok(own.named(name));
    }
    let caller = request.caller.as_ref().and_then(|caller| {
        let (CallerLane::Agent(lane) | CallerLane::Pin(lane)) = caller;
        let name = lane.name()?;
        let admissible = || match caller {
            CallerLane::Agent(_) => true,
            CallerLane::Pin(_) => admit_named(request, &own, name.as_str()).is_ok(),
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
        return Ok(own.named(in_place_team_name(request.project_root, team)));
    }
    Ok(caller.cloned().unwrap_or(own))
}

/// The checkout's own lane for `dir`: the deepest of trunk and a managed
/// worktree containing it, else a foreign checkout homed at its repository
/// root, else at `dir` itself. A non-repository room's project root is its
/// trunk; a managed worktree whose directory is `main` is foreign. A repository
/// at or above the user's home, compared with links resolved, homes no foreign
/// lane, so every directory under a dotfiles home keeps its own.
pub fn own_lane(project_root: &Path, dir: &Path) -> Lane {
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty());
    own_lane_at_home(project_root, dir, home.as_deref().map(Path::new))
}

fn own_lane_at_home(project_root: &Path, dir: &Path, home: Option<&Path>) -> Lane {
    let dir = crate::utils::path::normalize_path_lexical(dir);
    for ancestor in dir.ancestors() {
        if ancestor == project_root {
            return Lane::own(LaneHome::Trunk, path_text(project_root));
        }
        if is_managed_worktree(project_root, ancestor) {
            let home = if ancestor.file_name().is_some_and(|name| name == "main") {
                LaneHome::Foreign
            } else {
                LaneHome::Worktree
            };
            return Lane::own(home, path_text(ancestor));
        }
    }
    let root = dir
        .ancestors()
        .find(|ancestor| is_repository_root(ancestor))
        .filter(|root| !home.is_some_and(|home| resolved(home).starts_with(resolved(root))))
        .unwrap_or(&dir);
    Lane::own(LaneHome::Foreign, path_text(root))
}

/// `path` with every link resolved, or as written when it cannot be resolved,
/// so a home and a root named through different links still compare equal.
fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| crate::utils::path::normalize_path_lexical(path))
}

/// A `.git` directory holding `HEAD`, or a `.git` file naming its `gitdir:`;
/// read without Git, since a bare `.git` entry proves nothing.
fn is_repository_root(dir: &Path) -> bool {
    use std::io::Read;
    let git = dir.join(".git");
    if git.is_dir() {
        return git.join("HEAD").is_file();
    }
    let mut prefix = [0; 7];
    std::fs::File::open(&git)
        .and_then(|mut file| file.read_exact(&mut prefix))
        .is_ok_and(|()| &prefix == b"gitdir:")
}

/// A row's lane: its stored lane, else its old stamp adopted at the checkout
/// its lane path classifies into.
pub fn row_lane(agent: &AgentState, project_root: &Path) -> Option<Lane> {
    if let Some(lane) = &agent.lane {
        return Some(lane.clone());
    }
    let path = agent.lane_path()?;
    Some(own_lane(project_root, Path::new(path)).adopt(agent.channel.as_deref()))
}

/// The named lane an in-place team starts at trunk: `<project dir>/<team>`, or
/// the team name alone when the root has no basename. Each whitespace
/// character and `#` becomes `-`, so the name is always a lane name.
pub fn in_place_team_name(project_root: &Path, team: &str) -> LaneName {
    let raw = match project_root.file_name().and_then(|name| name.to_str()) {
        Some(dir) if !dir.is_empty() => format!("{dir}/{team}"),
        _ => team.to_owned(),
    };
    let dashed = raw.replace(|ch: char| ch.is_whitespace() || ch == '#', "-");
    // Every character a lane name refuses is mapped, so only an empty team at a
    // root without a basename is left, and a configured team name is never empty.
    dashed
        .parse()
        .unwrap_or_else(|_| LaneName::from_str("-").expect("`-` is a lane name"))
}

/// The routing name of `dir`'s own lane; trunk's own lane has none.
pub fn directory_channel(project_root: &Path, dir: &Path) -> Option<String> {
    let lane = own_lane(project_root, dir);
    if lane.is_trunk() {
        return None;
    }
    lane.routing_name().map(ToOwned::to_owned)
}

/// Admit a new managed worktree's name: no live agent holds a named lane under
/// it, apart from agents whose lane path is a directory of that name, which is
/// the worktree itself before it exists.
pub fn admit_worktree_name(
    agents: &[AgentState],
    project_root: &Path,
    name: &str,
) -> Result<(), LaneErr> {
    let in_worktree = |agent: &AgentState| {
        agent
            .lane_path()
            .and_then(|path| Path::new(path).file_name())
            .is_some_and(|dir| dir == name)
    };
    let occupied = agents
        .iter()
        .filter(|agent| held_live(agent) && !in_worktree(agent))
        .any(|agent| {
            row_lane(agent, project_root)
                .is_some_and(|lane| lane.name().is_some_and(|held| held.as_str() == name))
        });
    if occupied {
        return Err(LaneErr::LaneOccupied {
            name: name.to_owned(),
        });
    }
    Ok(())
}

/// A typed name's checks, for a named lane at `own`'s checkout: no other
/// checkout owns it, it is flat, and no live agents hold it at another checkout.
fn admit_named(request: &LaneRequest<'_>, own: &Lane, name: &str) -> Result<LaneName, LaneErr> {
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
    let invalid = || LaneErr::InvalidName {
        name: name.to_owned(),
    };
    if !LaneName::is_flat(name) {
        return Err(invalid());
    }
    if !own.is_trunk() && owned_names(&trunk, project_root).contains(&name) {
        return Err(LaneErr::TrunkCollision {
            name: name.to_owned(),
        });
    }
    let held_at = request
        .agents
        .iter()
        .filter(|agent| held_live(agent))
        .filter_map(|agent| row_lane(agent, project_root))
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
    name.parse().map_err(|_| invalid())
}

/// The names a checkout's own lane owns: its directory, and for trunk also `main`.
fn owned_names<'a>(own: &'a Lane, project_root: &'a Path) -> Vec<&'a str> {
    let mut names = Vec::new();
    match own.home() {
        LaneHome::Trunk => {
            names.push("main");
            names.extend(project_root.file_name().and_then(|name| name.to_str()));
        }
        LaneHome::Worktree => names.extend(own.routing_name()),
        LaneHome::Foreign => {}
    }
    names
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

fn is_managed_worktree(project_root: &Path, dir: &Path) -> bool {
    dir.join(".git").is_file()
        && matches!(
            crate::worktree::classify_checkout(dir, project_root),
            crate::worktree::CheckoutKind::Linked(_)
        )
        && crate::worktree::read_marker_from_checkout_metadata(dir)
            .is_ok_and(|marker| marker.is_some())
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
