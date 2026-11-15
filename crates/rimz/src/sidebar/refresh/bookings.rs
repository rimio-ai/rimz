//! Producer-published loop bookings: each one-off loop row an agent booked in this room whose
//! command is an agent or team launch, with the loop's own verdict on it, read once per pass through
//! `harness::schedule::attention::read_catalog`. The cache holds only what the room's
//! records say; which card owns a booking and what its launch has alive are joined in
//! the fold (`crate::sidebar::enrich::bookings`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::{AgentState, LaunchedBy};
use crate::config::{MachineConfig, TeamsConfig, WorktreeConfig};
use crate::disk::paths::WorkflowPaths;
use crate::harness::cache_keepalive::SeatPolicy;
use crate::harness::loop_task::{FireLaunch, LaunchTree, read_fire_launch};
use crate::harness::schedule::attention::{self, ReadContext, TaskReading};
use crate::harness::schedule::catalog::{LoadedTask, TaskCatalog, TaskSource};
use crate::harness::schedule::run_log::HeldStats;
use crate::harness::schedule::task_record::{self, FireMark};
use crate::harness::schedule::{TaskKind, Trigger};
use crate::harness::when::CiSource;
use crate::ids::WorkflowRunId;
use crate::store::snapshot::{BookingPr, BookingStarts, SidebarSnapshot};
use crate::store::workflow::{RunEnding, RunFire, RunOrigin};
use crate::{RuntimePaths, StatePaths};

#[cfg(test)]
mod tests;

const BOOKINGS_CACHE_VERSION: u32 = 3;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::sidebar) struct BookingsCache {
    pub version: u32,
    pub bookings: Vec<BookingFacts>,
}

/// One booking as the room's records state it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::sidebar) struct BookingFacts {
    pub task: String,
    pub starts: BookingStarts,
    /// A team's configured leader role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
    /// The agents behind the booking, nearest first.
    pub booked_by: Vec<LaunchedBy>,
    /// The launch's destination branch: the named worktree's branch, else its name; for a
    /// fresh tree per fire, the branch its fire's checkout was cut on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The launch runs in a worktree, named or fresh; a launch with none has no branch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub worktree: bool,
    /// The loop's needs-you reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_fix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking: Option<BlockingFacts>,
    /// Every unmet term's key, each once.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmet: Vec<String>,
    /// A trigger that is no condition, in the loop's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timing: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire: Option<FireFacts>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::sidebar) struct BlockingFacts {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reading: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    #[serde(default)]
    pub more: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::sidebar) struct FireFacts {
    /// The fire mark's time.
    pub at: Timestamp,
    /// The workflow run the fire created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<WorkflowRunId>,
    /// The latest end stamp the room's records hold for the launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Timestamp>,
    pub pr: BookingPr,
    /// A launched team's board stage is `Done`, read in the checkout the team launched in:
    /// the board's current reading, or, while the board cannot be read (its checkout or
    /// archive gone), the reading the last publication of the same fire held. `None` for a
    /// launch that is no team, or whose board no pass has read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_done: Option<bool>,
    /// The launch ids (the id a `booked_by` link carries) of the rows this fire launched:
    /// its leader and the leader's cohort, and every row its run pinned. Recorded each pass
    /// the rows are seen and kept for the same fire once they are gone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub launches: Vec<String>,
}

/// The lane's run-log fold and the consumed fires found past their lifetime, retained
/// across passes.
#[derive(Default)]
pub(super) struct BookingsState {
    stats: Option<HeldStats>,
    expired: BTreeSet<(String, WorkflowRunId)>,
}

impl std::fmt::Debug for BookingsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BookingsState")
            .field("expired", &self.expired)
            .finish_non_exhaustive()
    }
}

pub(in crate::sidebar) fn read_bookings_cache(path: &Path) -> BookingsCache {
    let cache: BookingsCache = crate::disk::atomic::read_json_cache(path);
    if cache.version == BOOKINGS_CACHE_VERSION {
        cache
    } else {
        BookingsCache::default()
    }
}

pub(super) fn refresh_bookings_for(
    snapshot: &SidebarSnapshot,
    state_paths: &StatePaths,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    loaded_teams: Option<&TeamsConfig>,
    state: &mut BookingsState,
) {
    let path = runtime.bookings_path();
    let prior = read_bookings_cache(&path);
    let bookings = match snapshot.project_root.as_deref() {
        Some(root) => {
            let lane = Lane {
                root,
                state: state_paths,
                runtime,
                prior: &prior,
                now: snapshot.now.to_zoned(config.time_zone()),
                window_secs: CONSUMED_TREELESS_SECS,
                rows: &snapshot.agents,
                worktrees: &config.agents.worktree,
            };
            lane.read(state, || match loaded_teams {
                Some(teams) => teams.clone(),
                None => SeatPolicy::load(config, Some(root)).teams,
            })
        }
        None => Vec::new(),
    };
    let refreshed = BookingsCache {
        version: BOOKINGS_CACHE_VERSION,
        bookings,
    };
    if refreshed == prior {
        return;
    }
    if let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(&path, &refreshed) {
        tracing::debug!(path = %path.display(), %error, "sidebar bookings cache write failed");
    }
}

/// How long a consumed entry whose launch runs in no worktree stays after its end: a
/// data-plane lifetime, not a display setting.
const CONSUMED_TREELESS_SECS: u64 = 15 * 60;

/// The room one pass reads.
struct Lane<'a> {
    root: &'a Path,
    state: &'a StatePaths,
    runtime: &'a RuntimePaths,
    prior: &'a BookingsCache,
    now: jiff::Zoned,
    window_secs: u64,
    /// The fold's rows.
    rows: &'a [AgentState],
    worktrees: &'a WorktreeConfig,
}

impl Lane<'_> {
    /// The room's bookings; `teams` loads the seat tables only when the room holds one. The
    /// gate is the catalog and the task record: with no booking row and no consumed task
    /// missing from the catalog outside `expired`, nothing more is read.
    fn read(
        self,
        lane: &mut BookingsState,
        teams: impl FnOnce() -> TeamsConfig,
    ) -> Vec<BookingFacts> {
        let Self {
            root,
            state,
            runtime,
            prior,
            ..
        } = self;
        let catalog = TaskCatalog::load_room_lenient(root, state);
        let task_records = task_record::load().unwrap_or_default();
        let ledger: BTreeMap<String, FireMark> = task_records
            .consumed(root)
            .into_iter()
            .filter(|(task, _)| !catalog.visible().contains_key(*task))
            .filter(|(task, mark)| {
                mark.run
                    .clone()
                    .is_some_and(|run| !lane.expired.contains(&((*task).to_owned(), run)))
            })
            .map(|(task, mark)| (task.to_owned(), mark.clone()))
            .collect();
        let candidates: Vec<(String, WorkflowRunId)> = ledger
            .iter()
            .filter_map(|(task, launch)| Some((task.clone(), launch.run.clone()?)))
            .collect();
        let booked = catalog.visible().values().any(is_booking);
        if !booked && candidates.is_empty() {
            return Vec::new();
        }
        let readings = if booked {
            let stats = lane
                .stats
                .get_or_insert_with(|| HeldStats::new(&crate::disk::paths::logs_dir()));
            attention::read_catalog(
                &catalog,
                root,
                state,
                runtime,
                ReadContext {
                    open: true,
                    stats: stats.stats(root, &self.now),
                    now: &self.now,
                },
            )
        } else {
            BTreeMap::new()
        };
        let fired = readings
            .values()
            .filter_map(|reading| reading.launch.as_ref()?.run.as_ref());
        let homes = run_homes(state, fired.chain(candidates.iter().map(|(_, run)| run)));
        let workflows = homes.values().map(|(record, _)| record.clone()).collect();
        let (commands, rejected) = booked_commands(&homes, &candidates);
        if !booked && commands.is_empty() {
            lane.expired.extend(rejected);
            return Vec::new();
        }
        let teams = teams();
        let records = RoomRecords {
            source: CiSource::read(runtime),
            workflows,
            runs: crate::store::run::list(&state.runs_dir).unwrap_or_default(),
            commands,
            rows: self.rows,
            worktrees: self.worktrees,
        };
        let mut facts = Vec::new();
        if booked {
            facts = collect(catalog.visible(), &readings, &teams, &records, root);
            carry(prior, &mut facts);
            reread_prs(&records.source, root, &mut facts);
        }
        let (kept, gone) = consumed(
            &ledger,
            &teams,
            &records,
            prior,
            root,
            self.now.timestamp(),
            self.window_secs,
        );
        lane.expired.extend(rejected);
        lane.expired.extend(gone);
        facts.extend(kept);
        facts
    }
}

/// The record and home of each named run this RimZ reads, found by id in the room's live
/// directory or its finished one: an ended run's home moves to the finished directory, and a
/// fired booking stays on its card past that end.
fn run_homes<'a>(
    state: &StatePaths,
    runs: impl Iterator<Item = &'a WorkflowRunId>,
) -> BTreeMap<WorkflowRunId, (crate::store::workflow::RunRecord, WorkflowPaths)> {
    let mut homes = BTreeMap::new();
    for run in runs {
        if homes.contains_key(run) {
            continue;
        }
        let Ok(Some(found)) = crate::store::workflow::find(state, run) else {
            continue;
        };
        if let crate::store::workflow::RunClass::Current(record) = found.class {
            homes.insert(run.clone(), (*record, found.paths));
        }
    }
    homes
}

/// The launch each candidate's command typed, read back from its generated definition, for the
/// candidates whose run is a scheduled fire an agent booked of a loop row's command; and the
/// candidates found to be no booking.
fn booked_commands(
    homes: &BTreeMap<WorkflowRunId, (crate::store::workflow::RunRecord, WorkflowPaths)>,
    candidates: &[(String, WorkflowRunId)],
) -> (
    BTreeMap<WorkflowRunId, FireLaunch>,
    Vec<(String, WorkflowRunId)>,
) {
    let mut booked = BTreeMap::new();
    let mut rejected = Vec::new();
    for (task, run) in candidates {
        let Some((record, paths)) = homes.get(run) else {
            continue;
        };
        let scheduled = matches!(
            &record.fire,
            Some(RunFire::Scheduled { booked_by, .. }) if !booked_by.is_empty()
        );
        if !scheduled || record.origin != RunOrigin::Inline {
            rejected.push((task.clone(), run.clone()));
            continue;
        }
        let Ok(spec) = crate::harness::workflow::load(&record.definition_dir(paths)) else {
            continue;
        };
        // The runner seals the command as `exec <argv as one shell line>`, whose words read
        // back unchanged (`schedule::command_line`).
        let Some(launch) = spec
            .check(crate::harness::schedule::runner::COMMAND_NODE)
            .and_then(|node| node.shell()?.strip_prefix("exec "))
            .and_then(shlex::split)
            .and_then(|argv| read_fire_launch(&argv))
        else {
            rejected.push((task.clone(), run.clone()));
            continue;
        };
        booked.insert(run.clone(), launch);
    }
    (booked, rejected)
}

/// The room's records a booking's facts read beyond its verdict.
pub(super) struct RoomRecords<'a> {
    pub source: CiSource,
    pub workflows: Vec<crate::store::workflow::RunRecord>,
    pub runs: Vec<crate::store::run::RunRecord>,
    /// The launch the command typed, of each run whose task row the loop consumed at its fire.
    pub commands: BTreeMap<WorkflowRunId, FireLaunch>,
    /// The fold's rows.
    pub rows: &'a [AgentState],
    /// Where a named worktree's checkout lies.
    pub worktrees: &'a WorktreeConfig,
}

/// A loop task of the machine tier or the room's state tier (the two that record
/// `booked_by`; a timed `--in` add lands in the state tier) that is a one-off whose command
/// is an agent or team launch (`read_fire_launch`), and was booked by an agent. Any trigger
/// qualifies; the room filter is `read_catalog`'s. Every wake-me or session-pinned state-tier
/// row carries `wait`, which no booking does.
fn is_booking(task: &LoadedTask) -> bool {
    let entry = task.entry();
    matches!(task.source(), TaskSource::Config | TaskSource::Instance)
        && task.kind() == TaskKind::OneOff
        && entry.wait.is_none()
        && !entry.booked_by.is_empty()
        && read_fire_launch(&entry.command).is_some()
}

pub(super) fn collect(
    tasks: &BTreeMap<String, LoadedTask>,
    readings: &BTreeMap<String, TaskReading>,
    teams: &TeamsConfig,
    records: &RoomRecords,
    root: &Path,
) -> Vec<BookingFacts> {
    tasks
        .iter()
        .filter(|(_, task)| is_booking(task))
        .filter_map(|(name, task)| {
            let reading = readings.get(name)?;
            let entry = task.entry();
            let launch = read_fire_launch(&entry.command)?;
            let team = teams.0.get(&launch.target);
            let record = reading
                .launch
                .as_ref()
                .and_then(|launch| launch.run.as_ref())
                .and_then(|run| records.workflows.iter().find(|record| &record.id == run));
            let named = named(&launch);
            let branch = destination_branch(named).or_else(|| record.and_then(fire_branch));
            let fire = reading.launch.as_ref().map(|fired| FireFacts {
                at: fired.at,
                run: fired.run.clone(),
                ended_at: fired
                    .run
                    .as_ref()
                    .and_then(|run| ended_at(run, name, fired.at, launch.detach, records)),
                pr: branch.as_deref().map_or(BookingPr::Unread, |branch| {
                    branch_pr(&records.source, branch, root)
                }),
                team_done: team.and(record).and_then(|record| {
                    team_done(launch_checkout(record, &launch, records, name, fired.at).as_deref())
                }),
                launches: launches(records.rows, name, fired.run.as_ref(), fired.at),
            });
            Some(BookingFacts {
                task: name.clone(),
                starts: starts(&launch, team.is_some()),
                leader: team.and_then(|team| team.leader.clone()),
                booked_by: entry.booked_by.clone(),
                branch,
                worktree: launch.worktree.is_some() || entry.fresh_worktree.is_some(),
                needs_fix: reading.attention.as_ref().map(|(_, reason)| reason.clone()),
                blocking: reading.blocking.as_ref().map(|term| BlockingFacts {
                    key: term.key.clone(),
                    reading: term.reading.clone(),
                    pr: term.pr,
                    more: term.more,
                }),
                unmet: reading.unmet.clone(),
                timing: task
                    .trigger()
                    .as_ref()
                    .ok()
                    .filter(|parsed| !matches!(parsed.trigger, Trigger::Condition { .. }))
                    .map(|parsed| parsed.describe()),
                fire,
            })
        })
        .collect()
}

/// What a booking starts: a team through `rimz teams` or where its spec names a configured
/// team, else the profile or spec as typed.
fn starts(launch: &FireLaunch, configured_team: bool) -> BookingStarts {
    if launch.team || configured_team {
        BookingStarts::Team(launch.target.clone())
    } else {
        BookingStarts::Profile(launch.target.clone())
    }
}

/// The worktree a launch names; a bare `-w` or `--from-pr` names none.
fn named(launch: &FireLaunch) -> Option<&str> {
    match &launch.worktree {
        Some(LaunchTree::Named(name)) => Some(name),
        _ => None,
    }
}

/// A named worktree's branch: its explicit branch, else its name where that is a legal
/// branch as written. A fresh tree per fire, or a generated name, has no name to join on.
fn destination_branch(named: Option<&str>) -> Option<String> {
    let requested = crate::worktree::parse_requested_name(named?).ok()?;
    Some(requested.branch.unwrap_or(requested.name))
}

pub(super) fn branch_pr(source: &CiSource, branch: &str, root: &Path) -> BookingPr {
    let (reading, number) = source.branch_pr(branch, root);
    match reading {
        Some("none") => BookingPr::None,
        Some("open") => BookingPr::Open { number },
        Some("merged") => BookingPr::Merged { number },
        Some("closed") => BookingPr::Closed { number },
        _ => BookingPr::Unread,
    }
}

/// A consumed one-shot's fired entry: each consumed task's newest fire mark (`ledger`, by name,
/// none with a catalog row) whose run is a scheduled fire an agent booked, rebuilt from that
/// run's record and the launch its generated definition's command types, while it is in its
/// lifetime (`in_lifetime`); and the fires found past it.
#[allow(clippy::too_many_arguments)]
pub(super) fn consumed(
    ledger: &BTreeMap<String, FireMark>,
    teams: &TeamsConfig,
    records: &RoomRecords,
    prior: &BookingsCache,
    root: &Path,
    now: Timestamp,
    window_secs: u64,
) -> (Vec<BookingFacts>, Vec<(String, WorkflowRunId)>) {
    let (mut facts, checkouts): (Vec<BookingFacts>, Vec<Option<PathBuf>>) = ledger
        .iter()
        .filter_map(|(task, launch)| {
            let run = launch.run.as_ref()?;
            let record = records.workflows.iter().find(|record| &record.id == run)?;
            let Some(RunFire::Scheduled { booked_by, .. }) = &record.fire else {
                return None;
            };
            if booked_by.is_empty() {
                return None;
            }
            let fired = records.commands.get(run)?;
            let named = named(fired);
            let team = teams.0.get(&fired.target);
            let branch = destination_branch(named).or_else(|| fire_branch(record));
            let checkout = launch_checkout(record, fired, records, task, launch.at);
            let facts = BookingFacts {
                task: task.to_owned(),
                starts: starts(fired, team.is_some()),
                leader: team.and_then(|team| team.leader.clone()),
                booked_by: booked_by.clone(),
                worktree: record.fire_tree || fired.worktree.is_some(),
                needs_fix: None,
                blocking: None,
                unmet: Vec::new(),
                timing: None,
                fire: Some(FireFacts {
                    at: launch.at,
                    run: Some(run.clone()),
                    ended_at: ended_at(run, task, launch.at, fired.detach, records),
                    pr: branch.as_deref().map_or(BookingPr::Unread, |branch| {
                        branch_pr(&records.source, branch, root)
                    }),
                    team_done: team.and_then(|_| team_done(checkout.as_deref())),
                    launches: launches(records.rows, task, Some(run), launch.at),
                }),
                branch,
            };
            Some((facts, checkout))
        })
        .unzip();
    carry(prior, &mut facts);
    reread_prs(&records.source, root, &mut facts);
    let (kept, gone): (Vec<_>, Vec<_>) =
        facts
            .into_iter()
            .zip(checkouts)
            .partition(|(facts, checkout)| {
                in_lifetime(facts, checkout.as_deref(), records, now, window_secs)
            });
    let kept = kept.into_iter().map(|(facts, _)| facts).collect();
    let gone = gone
        .into_iter()
        .filter_map(|(facts, _)| Some((facts.task, facts.fire?.run?)))
        .collect();
    (kept, gone)
}

/// A consumed entry's lifetime: while its run is not terminal or a row it launched lives;
/// after that, one with a known worktree until its pull request merges or `checkout` (the
/// launch's checkout) is gone, and one without, or whose tree is not known, for
/// `window_secs` after its end.
fn in_lifetime(
    facts: &BookingFacts,
    checkout: Option<&Path>,
    records: &RoomRecords,
    now: Timestamp,
    window_secs: u64,
) -> bool {
    let Some(fire) = &facts.fire else {
        return false;
    };
    let terminal = fire.run.as_ref().is_some_and(|run| {
        records
            .workflows
            .iter()
            .find(|record| &record.id == run)
            .is_none_or(|record| record.terminal_at.is_some())
    });
    let recent = fire.ended_at.is_some_and(|ended_at| {
        now.duration_since(ended_at).as_secs() <= i64::try_from(window_secs).unwrap_or(i64::MAX)
    });
    let living = records.rows.iter().any(|agent| {
        agent.ended_at.is_none() && agent.loop_task.as_deref() == Some(facts.task.as_str())
    });
    if !terminal || living {
        return true;
    }
    if facts.worktree
        && let Some(checkout) = checkout
    {
        return !matches!(fire.pr, BookingPr::Merged { .. }) && checkout.is_dir();
    }
    recent
}

/// The branch a fresh tree per fire was cut on, from its RimZ marker. `None` once the tree
/// is removed, and for any other fire, whose scope is the firing checkout.
fn fire_branch(record: &crate::store::workflow::RunRecord) -> Option<String> {
    if !record.fire_tree {
        return None;
    }
    crate::worktree::read_marker_from_checkout_metadata(&record.scope.path)
        .ok()
        .flatten()
        .map(|marker| marker.branch)
}

/// The checkout a fire's launch runs in: the worktree it names, resolved without creating
/// it; for a tree it leaves unnamed (a bare `-w`, `--from-pr`), the checkout of an agent it
/// launched, none while no agent is joined; else the checkout its command ran in (the run's
/// scope: the fire's fresh tree or the firing checkout).
fn launch_checkout(
    record: &crate::store::workflow::RunRecord,
    launch: &FireLaunch,
    records: &RoomRecords,
    task: &str,
    at: Timestamp,
) -> Option<PathBuf> {
    match &launch.worktree {
        Some(LaunchTree::Named(name)) => {
            crate::worktree::worktree_path(&record.project_root, records.worktrees, name).ok()
        }
        Some(LaunchTree::Unnamed) => records
            .rows
            .iter()
            .filter(|agent| launched_by(agent, task, Some(&record.id), at))
            .find_map(|agent| agent.worktree_path.as_deref())
            .map(PathBuf::from),
        None => Some(record.scope.path.clone()),
    }
}

/// Whether a launched team's board in its launch checkout reads `Done`; `None` while the
/// board cannot be read.
fn team_done(checkout: Option<&Path>) -> Option<bool> {
    crate::harness::scratch::board_stage(checkout?)
        .map(|stage| stage.name == crate::config::DONE_STAGE)
}

/// The launch ids of the rows the fire of `task` at `at` creating `run` launched: the rows
/// `launched_by` it, their team cohorts, and every row its run pinned; sorted, each once.
fn launches(
    rows: &[AgentState],
    task: &str,
    run: Option<&WorkflowRunId>,
    at: Timestamp,
) -> Vec<String> {
    let cohorts: BTreeSet<_> = rows
        .iter()
        .filter(|agent| launched_by(agent, task, run, at))
        .filter_map(AgentState::team_instance)
        .collect();
    let ids: BTreeSet<String> = rows
        .iter()
        .filter(|agent| {
            launched_by(agent, task, run, at)
                || agent
                    .team_instance()
                    .is_some_and(|cohort| cohorts.contains(&cohort))
                || agent
                    .workflow
                    .as_ref()
                    .zip(run)
                    .is_some_and(|(pin, run)| &pin.run == run)
        })
        .map(|agent| {
            agent
                .launch_id
                .as_ref()
                .unwrap_or(&agent.agent_id)
                .to_string()
        })
        .collect();
    ids.into_iter().collect()
}

/// Carries what this pass no longer reads from the previous publication of the same fire:
/// its destination branch once its tree is removed, a `merged` reading once its branch left
/// the pull request lane's demand, its team's board reading while the board is unreadable,
/// the launches its rows recorded, and its worktree flag.
pub(super) fn carry(prior: &BookingsCache, fresh: &mut [BookingFacts]) {
    for facts in fresh {
        let Some(fire) = facts.fire.as_mut() else {
            continue;
        };
        let Some((before, before_fire)) = prior.bookings.iter().find_map(|before| {
            let before_fire = before.fire.as_ref()?;
            (before.task == facts.task && before_fire.run == fire.run && before_fire.at == fire.at)
                .then_some((before, before_fire))
        }) else {
            continue;
        };
        if facts.branch.is_none() {
            facts.branch = before.branch.clone();
        }
        if fire.pr == BookingPr::Unread && matches!(before_fire.pr, BookingPr::Merged { .. }) {
            fire.pr = before_fire.pr;
        }
        if fire.team_done.is_none() {
            fire.team_done = before_fire.team_done;
        }
        let launches: BTreeSet<String> = fire
            .launches
            .drain(..)
            .chain(before_fire.launches.iter().cloned())
            .collect();
        fire.launches = launches.into_iter().collect();
        facts.worktree |= before.worktree;
    }
}

/// Reads the pull request of each fire whose branch only the carry supplied.
fn reread_prs(source: &CiSource, root: &Path, facts: &mut [BookingFacts]) {
    for facts in facts {
        if let (Some(branch), Some(fire)) = (&facts.branch, facts.fire.as_mut())
            && fire.pr == BookingPr::Unread
        {
            fire.pr = branch_pr(source, branch, root);
        }
    }
}

/// The pull request lane's demand: each fired booking's own branch until its pull request
/// merged or its team reached `Done`.
pub(in crate::sidebar) fn branch_demand(cache: &BookingsCache) -> BTreeSet<String> {
    cache
        .bookings
        .iter()
        .filter(|facts| {
            facts.fire.as_ref().is_some_and(|fire| {
                fire.team_done != Some(true) && !matches!(fire.pr, BookingPr::Merged { .. })
            })
        })
        .filter_map(|facts| facts.branch.clone())
        .collect()
}

/// The latest end stamp of the launch the fire of `task` at `at` made in `run`: the run's
/// terminal stamp, unless the launch detached and the run ended done, since a detached
/// launch's command returns once the launch is made; the completion of each supervised run the
/// task has owned since the fire, a blocking launch's own run; and, once every row the fire
/// launched has ended, the newest of their end stamps.
fn ended_at(
    run: &WorkflowRunId,
    task: &str,
    at: Timestamp,
    detached: bool,
    records: &RoomRecords,
) -> Option<Timestamp> {
    let terminal = records
        .workflows
        .iter()
        .find(|record| &record.id == run)
        .filter(|record| {
            !detached
                || record
                    .outcome
                    .as_ref()
                    .is_none_or(|outcome| outcome.ending != RunEnding::Done)
        })
        .and_then(|record| record.terminal_at);
    let owned = records
        .runs
        .iter()
        .filter(|supervised| {
            supervised.loop_task.as_deref() == Some(task) && supervised.started_at >= at
        })
        .filter_map(|supervised| supervised.completed_at);
    let rows: Vec<&AgentState> = records
        .rows
        .iter()
        .filter(|agent| launched_by(agent, task, Some(run), at))
        .collect();
    let rows_ended = rows
        .iter()
        .all(|agent| agent.ended_at.is_some())
        .then(|| rows.iter().filter_map(|agent| agent.ended_at).max())
        .flatten();
    terminal.into_iter().chain(owned).chain(rows_ended).max()
}

/// Whether `agent` is a row the fire of `task` at `at` creating `run` launched: its
/// `loop_task` is the task and it belongs to the ledger's launch. A step that detaches
/// clears the workflow pin, so a pinless row joins by registering at or after the fire.
pub(in crate::sidebar) fn launched_by(
    agent: &AgentState,
    task: &str,
    run: Option<&WorkflowRunId>,
    at: Timestamp,
) -> bool {
    agent.loop_task.as_deref() == Some(task)
        && match (&agent.workflow, run) {
            (Some(pin), Some(run)) => &pin.run == run,
            (Some(_), None) => false,
            (None, _) => agent
                .registered_at
                .is_some_and(|registered| registered >= at),
        }
}
