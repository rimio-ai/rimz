//! Producer-published loop bookings: each one-off loop row an agent booked in this room whose
//! declared kind is agent or team, with the loop's own verdict on it, read once per pass through
//! `harness::schedule::attention::read_catalog`. The cache holds only what the room's
//! records say; which card owns a booking and what its launch has alive are joined in
//! the fold (`crate::sidebar::enrich::bookings`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::state::{AgentEnd, agent_end};
use crate::agents::{AgentState, LaunchedBy};
use crate::config::{LaunchKind, MachineConfig, Team, TeamsConfig, WorktreeConfig};
use crate::harness::cache_keepalive::SeatPolicy;
use crate::harness::schedule::attention::{self, ReadContext, TaskReading};
use crate::harness::schedule::catalog::{LoadedTask, TaskCatalog, TaskSource};
use crate::harness::schedule::run_log::{HeldStats, LoopRunResult};
use crate::harness::schedule::task_record::{self, FireMark};
use crate::harness::schedule::{TaskAction, TaskKind, Trigger};
use crate::harness::when::CiSource;
use crate::ids::FireId;
use crate::store::snapshot::{BookingPr, BookingStarts, SidebarSnapshot};
use crate::{RuntimePaths, RuntimeScope, StatePaths, Store};

#[cfg(test)]
mod tests;

const BOOKINGS_CACHE_VERSION: u32 = 5;

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
    /// The launch's destination branch: a team's `#lane` branch, else its name; once the
    /// fire's rows registered, the branch in the RimZ marker of the tree they run in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The launch runs in a worktree: a team's `#lane`, or a RimZ tree its rows run in; a
    /// launch with none has no branch.
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
    /// The lane the entry names: a team's typed `#lane` other than trunk's, else the named
    /// lane of the rows the fire launched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// The task's label, else its launch tail's `--description`; carried across its fire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
    /// The fire's id, which every row and run its launch made carries as `loop_fire`.
    pub fire: FireId,
    /// The latest end stamp the room's records hold for the launch; once they hold none and
    /// nothing the fire launched is alive (its rows pruned), the end the previous publication
    /// of the same fire held.
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
    /// every row carrying its id and their team cohorts. Recorded each pass the rows are seen
    /// and kept for the same fire once they are gone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub launches: Vec<String>,
}

/// The lane's run-log fold and the consumed fires found past their lifetime, retained
/// across passes.
#[derive(Default)]
pub(super) struct BookingsState {
    stats: Option<HeldStats>,
    expired: BTreeSet<FireId>,
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
    store: Option<&Store>,
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
                now: snapshot.now.to_zoned_datetime(config.time_zone()),
                window_secs: CONSUMED_TREELESS_SECS,
                store,
                worktrees: &config.agents.worktree,
            };
            let read = lane.read(state, || match loaded_teams {
                Some(teams) => teams.clone(),
                None => SeatPolicy::load(config, Some(root)).teams,
            });
            let Some(bookings) = read else {
                return;
            };
            bookings
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
/// data-plane lifetime, not a display setting. A launched peer's entry keeps the same one.
pub(super) const CONSUMED_TREELESS_SECS: u64 = 15 * 60;

/// The room one pass reads.
struct Lane<'a> {
    root: &'a Path,
    state: &'a StatePaths,
    runtime: &'a RuntimePaths,
    prior: &'a BookingsCache,
    now: jiff::ZonedDateTime,
    window_secs: u64,
    /// The room's store, whose Audit rows a fired booking reads; `None` when it cannot be
    /// opened.
    store: Option<&'a Store>,
    worktrees: &'a WorktreeConfig,
}

impl Lane<'_> {
    /// The room's bookings; `teams` loads the seat tables only when the room holds one. The
    /// gate is the catalog and the task record: with no booking row and no booked fire of a
    /// consumed task missing from the catalog outside `expired`, nothing more is read, and the
    /// room's Audit rows are read only for a fired booking. `None` when those rows cannot be
    /// read: the pass leaves the published cache as it was, since the fold's Runtime rows have
    /// dropped every ended row and would publish an ended launch as starting.
    fn read(
        self,
        lane: &mut BookingsState,
        teams: impl FnOnce() -> TeamsConfig,
    ) -> Option<Vec<BookingFacts>> {
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
            .filter(|(_, mark)| booked_fire(mark).is_some() && !lane.expired.contains(&mark.fire))
            .map(|(task, mark)| (task.to_owned(), mark.clone()))
            .collect();
        let booked = catalog.visible().values().any(is_booking);
        if !booked && ledger.is_empty() {
            return Some(Vec::new());
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
        let fired = !ledger.is_empty()
            || catalog.visible().iter().any(|(name, task)| {
                is_booking(task)
                    && readings
                        .get(name)
                        .is_some_and(|reading| reading.launch.is_some())
            });
        let rows = if fired {
            audit_rows(self.store)?
        } else {
            Vec::new()
        };
        let teams = teams();
        let records = RoomRecords {
            source: CiSource::read(runtime),
            runs: crate::store::run::list(&state.runs_dir).unwrap_or_default(),
            rows: &rows,
            worktrees: self.worktrees,
        };
        let mut facts = Vec::new();
        if booked {
            facts = collect(catalog.visible(), &readings, &teams, &records, prior, root);
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
        lane.expired.extend(gone);
        facts.extend(kept);
        Some(facts)
    }
}

/// The room's Audit rows, which keep an ended row with its `loop_fire` and end stamp; `None`,
/// logged, when the store cannot be opened or its rows cannot be read.
pub(super) fn audit_rows(store: Option<&Store>) -> Option<Vec<AgentState>> {
    let Some(store) = store else {
        tracing::debug!("sidebar bookings pass skipped: the room's store cannot be opened");
        return None;
    };
    match store.runtime_projection(RuntimeScope::Audit) {
        Ok(projection) => Some(projection.agents),
        Err(error) => {
            tracing::debug!(%error, "sidebar bookings pass skipped: the room's rows are unreadable");
            None
        }
    }
}

/// The room's records a booking's facts read beyond its verdict.
pub(super) struct RoomRecords<'a> {
    pub source: CiSource,
    pub runs: Vec<crate::store::run::RunRecord>,
    /// The room's Audit rows, ended and replaced ones included.
    pub rows: &'a [AgentState],
    /// Where a named worktree's checkout lies.
    pub worktrees: &'a WorktreeConfig,
}

/// What a booking launches, from its declared kind and target: an agent spec, or a team with
/// the `#lane` its target names.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Booked {
    team: bool,
    target: String,
    lane: Option<String>,
}

impl Booked {
    fn new(kind: LaunchKind, target: &str) -> Option<Self> {
        match kind {
            LaunchKind::Agent => Some(Self {
                team: false,
                target: target.to_owned(),
                lane: None,
            }),
            LaunchKind::Team => {
                let (team, lane) = match target.split_once('#') {
                    Some((team, lane)) => (team, Some(lane.to_owned()).filter(|l| !l.is_empty())),
                    None => (target, None),
                };
                Some(Self {
                    team: true,
                    target: team.to_owned(),
                    lane,
                })
            }
            LaunchKind::Workflow | LaunchKind::Shell => None,
        }
    }

    /// The lane the entry names: a team's typed `#lane` other than trunk's and `external`.
    fn named_lane(&self) -> Option<String> {
        let typed = crate::ids::TeamInstanceId::new(
            &self.target,
            self.lane.as_deref().map(crate::ids::LaneName::dashed),
        )
        .ok()?;
        typed
            .lane()
            .filter(|lane| **lane != crate::ids::LaneName::trunk())
            .and(self.lane.clone())
    }

    /// What it starts: a team when declared one or where its spec names a configured team,
    /// else the profile or spec as typed.
    fn starts(&self, configured_team: bool) -> BookingStarts {
        if self.team || configured_team {
            BookingStarts::Team(self.target.clone())
        } else {
            BookingStarts::Profile(self.target.clone())
        }
    }
}

/// A loop task of the machine tier or the room's state tier (the two that record
/// `booked_by`; a timed `--in` add lands in the state tier) that is a one-off whose declared
/// kind is agent or team, and was booked by an agent. Any trigger
/// qualifies; the room filter is `read_catalog`'s. Every wake-me or session-pinned state-tier
/// row carries `wait`, which no booking does.
fn is_booking(task: &LoadedTask) -> bool {
    let entry = task.entry();
    matches!(task.source(), TaskSource::Config | TaskSource::Instance)
        && task.kind() == TaskKind::OneOff
        && entry.wait.is_none()
        && !entry.booked_by.is_empty()
        && booked_launch(task).is_some()
}

/// The task's label, else the `--description` its launch tail passes before `--` (what follows
/// is the provider's), each single-lined; blank text reads as none.
fn booked_description(task: &LoadedTask) -> Option<String> {
    let single_line = crate::agents::single_line_description;
    if let Some(label) = task.entry().label.as_deref().and_then(single_line) {
        return Some(label);
    }
    let Ok(TaskAction::Launch(launch)) = task.action() else {
        return None;
    };
    let mut args = launch.args.iter().take_while(|arg| *arg != "--");
    while let Some(arg) = args.next() {
        if arg == "--description" {
            return args.next().and_then(|text| single_line(text));
        }
        if let Some(text) = arg.strip_prefix("--description=") {
            return single_line(text);
        }
    }
    None
}

/// What an agent or team task's fire launches, from its declared kind and target.
fn booked_launch(task: &LoadedTask) -> Option<Booked> {
    let Ok(TaskAction::Launch(launch)) = task.action() else {
        return None;
    };
    Booked::new(launch.kind, &launch.target)
}

/// What a consumed task's fire launched, from the kind and target its mark froze, when an
/// agent booked it.
fn booked_fire(mark: &FireMark) -> Option<Booked> {
    if mark.booked_by.is_empty() {
        return None;
    }
    Booked::new(mark.kind?, mark.target.as_deref()?)
}

pub(super) fn collect(
    tasks: &BTreeMap<String, LoadedTask>,
    readings: &BTreeMap<String, TaskReading>,
    teams: &TeamsConfig,
    records: &RoomRecords,
    prior: &BookingsCache,
    root: &Path,
) -> Vec<BookingFacts> {
    tasks
        .iter()
        .filter(|(_, task)| is_booking(task))
        .filter_map(|(name, task)| {
            let reading = readings.get(name)?;
            let entry = task.entry();
            let booked = booked_launch(task)?;
            let team = teams.0.get(&booked.target);
            let fired = reading
                .launch
                .as_ref()
                .map(|mark| Fired::read(mark, &booked, team, records, prior, root));
            Some(BookingFacts {
                task: name.clone(),
                starts: booked.starts(team.is_some()),
                leader: team.and_then(|team| team.leader.clone()),
                booked_by: entry.booked_by.clone(),
                branch: fired
                    .as_ref()
                    .map_or_else(|| destination_branch(&booked), |fired| fired.branch.clone()),
                worktree: booked.lane.is_some() || fired.as_ref().is_some_and(|f| f.worktree),
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
                lane: fired
                    .as_ref()
                    .map_or_else(|| booked.named_lane(), |fired| fired.lane.clone()),
                description: booked_description(task),
                fire: fired.map(|fired| fired.facts),
            })
        })
        .collect()
}

/// A team's `#lane` read as a worktree name: its explicit branch, else its name where that is
/// a legal branch as written. An agent booking names none before it fires.
fn destination_branch(booked: &Booked) -> Option<String> {
    let requested = crate::worktree::parse_requested_name(booked.lane.as_deref()?).ok()?;
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

/// One fire of a booking as this pass reads it from the rows and runs carrying its id.
struct Fired {
    facts: FireFacts,
    branch: Option<String>,
    worktree: bool,
    /// The checkout the launch runs in, where known.
    checkout: Option<PathBuf>,
    /// Something the fire launched is alive.
    alive: bool,
    /// A team's cohort lane from the rows carrying the fire's id (trunk's none), else its typed
    /// lane while no row is registered; an agent's first such row's named lane.
    lane: Option<String>,
}

impl Fired {
    fn read(
        mark: &FireMark,
        booked: &Booked,
        team: Option<&Team>,
        records: &RoomRecords,
        prior: &BookingsCache,
        root: &Path,
    ) -> Self {
        let rows: Vec<&AgentState> = records
            .rows
            .iter()
            .filter(|agent| launched_by(agent, &mark.fire))
            .collect();
        let row_tree = rows
            .iter()
            .find_map(|agent| agent.worktree_path.as_deref())
            .map(PathBuf::from);
        let marked = row_tree.as_deref().and_then(|tree| {
            crate::worktree::read_marker_from_checkout_metadata(tree)
                .ok()
                .flatten()
        });
        let checkout = match &booked.lane {
            Some(lane) => crate::worktree::worktree_path(root, records.worktrees, lane).ok(),
            None => row_tree,
        };
        let branch =
            destination_branch(booked).or_else(|| marked.as_ref().map(|m| m.branch.clone()));
        let alive = fire_alive(mark, records, &rows);
        let facts = FireFacts {
            at: mark.at,
            fire: mark.fire.clone(),
            ended_at: ended_at(mark, records, &rows)
                .or_else(|| (!alive).then(|| published_end(prior, &mark.fire)).flatten()),
            pr: branch.as_deref().map_or(BookingPr::Unread, |branch| {
                branch_pr(&records.source, branch, root)
            }),
            team_done: team.and_then(|_| team_done(checkout.as_deref())),
            launches: launches(records.rows, &mark.fire),
        };
        let lane = if booked.team {
            rows.iter()
                .find_map(|agent| agent.team_instance())
                .map_or_else(
                    || booked.named_lane(),
                    |instance| {
                        instance
                            .lane()
                            .filter(|lane| **lane != crate::ids::LaneName::trunk())
                            .map(ToString::to_string)
                    },
                )
        } else {
            rows.iter()
                .find_map(|agent| agent.lane.as_ref()?.name())
                .map(ToString::to_string)
        };
        Self {
            facts,
            branch,
            worktree: booked.lane.is_some() || marked.is_some(),
            checkout,
            alive,
            lane,
        }
    }
}

/// A consumed one-shot's fired entry: each consumed task's newest fire mark (`ledger`, by name,
/// none with a catalog row) of an agent or team launch an agent booked, rebuilt from that mark
/// and the rows and runs carrying its fire id, while it is in its lifetime (`in_lifetime`);
/// and the fires found past it.
#[allow(clippy::too_many_arguments)]
pub(super) fn consumed(
    ledger: &BTreeMap<String, FireMark>,
    teams: &TeamsConfig,
    records: &RoomRecords,
    prior: &BookingsCache,
    root: &Path,
    now: Timestamp,
    window_secs: u64,
) -> (Vec<BookingFacts>, Vec<FireId>) {
    let (mut facts, held): (Vec<BookingFacts>, Vec<(Option<PathBuf>, bool)>) = ledger
        .iter()
        .filter_map(|(task, mark)| {
            let booked = booked_fire(mark)?;
            let team = teams.0.get(&booked.target);
            let fired = Fired::read(mark, &booked, team, records, prior, root);
            let facts = BookingFacts {
                task: task.to_owned(),
                starts: booked.starts(team.is_some()),
                leader: team.and_then(|team| team.leader.clone()),
                booked_by: mark.booked_by.clone(),
                branch: fired.branch,
                worktree: fired.worktree,
                needs_fix: None,
                blocking: None,
                unmet: Vec::new(),
                timing: None,
                lane: fired.lane,
                description: None,
                fire: Some(fired.facts),
            };
            Some((facts, (fired.checkout, fired.alive)))
        })
        .unzip();
    carry(prior, &mut facts);
    reread_prs(&records.source, root, &mut facts);
    let (kept, gone): (Vec<_>, Vec<_>) =
        facts
            .into_iter()
            .zip(held)
            .partition(|(facts, (checkout, alive))| {
                in_lifetime(facts, checkout.as_deref(), *alive, now, window_secs)
            });
    let kept = kept.into_iter().map(|(facts, _)| facts).collect();
    let gone = gone
        .into_iter()
        .filter_map(|(facts, _)| Some(facts.fire?.fire))
        .collect();
    (kept, gone)
}

/// A consumed entry's lifetime: its fire's launch's [`Lifetime`], with `checkout` (the
/// launch's checkout) read only for an entry with a worktree.
fn in_lifetime(
    facts: &BookingFacts,
    checkout: Option<&Path>,
    alive: bool,
    now: Timestamp,
    window_secs: u64,
) -> bool {
    let Some(fire) = &facts.fire else {
        return false;
    };
    Lifetime {
        at: fire.at,
        ended_at: fire.ended_at,
        alive,
        checkout: checkout.filter(|_| facts.worktree),
        pr: fire.pr,
    }
    .holds(now, window_secs)
}

/// What a launch's lifetime reads: a booked fire's or a launched peer's.
pub(super) struct Lifetime<'a> {
    /// When the launch began: a fire's time, a peer's registration.
    pub at: Timestamp,
    /// The newest end its records hold.
    pub ended_at: Option<Timestamp>,
    /// Something it launched is alive.
    pub alive: bool,
    /// The checkout of a launch known to run in a worktree; `None` for one that runs in
    /// none or whose tree is not known.
    pub checkout: Option<&'a Path>,
    /// The pull request of its own branch.
    pub pr: BookingPr,
}

impl Lifetime<'_> {
    /// While something it launched is alive, or, with no end recorded, for `window_secs`
    /// after it began (it is starting); after that, one with a known checkout until its
    /// pull request merges or the checkout is gone, and one without for `window_secs`
    /// after its end.
    pub(super) fn holds(&self, now: Timestamp, window_secs: u64) -> bool {
        let within = |stamp: Timestamp| {
            now.duration_since(stamp).as_secs() <= i64::try_from(window_secs).unwrap_or(i64::MAX)
        };
        if self.alive || (self.ended_at.is_none() && within(self.at)) {
            return true;
        }
        if let Some(checkout) = self.checkout {
            return !matches!(self.pr, BookingPr::Merged { .. }) && checkout.is_dir();
        }
        self.ended_at.is_some_and(within)
    }
}

/// Whether anything the fire launched is alive, by `harness::schedule::launches::alive`'s rule
/// over what the lane already holds: the fire's launch child while its outcome is open, a
/// supervised run carrying its id that is not terminal, or a row carrying its id (`rows`) whose
/// agent is not over by `agents::state::agent_end`. The one process read is the open mark's
/// launch child.
fn fire_alive(mark: &FireMark, records: &RoomRecords, rows: &[&AgentState]) -> bool {
    rows.iter().any(|agent| !agent_over(records.rows, agent))
        || records
            .runs
            .iter()
            .any(|run| run.loop_fire.as_ref() == Some(&mark.fire) && !run.status.is_terminal())
        || mark.launching()
}

/// The end the previous publication of the fire `fire` held.
fn published_end(prior: &BookingsCache, fire: &FireId) -> Option<Timestamp> {
    prior
        .bookings
        .iter()
        .filter_map(|before| before.fire.as_ref())
        .find(|before| before.fire == *fire)?
        .ended_at
}

/// Whether a launched team's board in its launch checkout reads `Done`; `None` while the
/// board cannot be read.
fn team_done(checkout: Option<&Path>) -> Option<bool> {
    crate::harness::scratch::board_stage(checkout?)
        .map(|stage| stage.name == crate::config::DONE_STAGE)
}

/// The launch ids of the rows the fire `fire` launched: the rows carrying its id and their
/// team cohorts; sorted, each once.
fn launches(rows: &[AgentState], fire: &FireId) -> Vec<String> {
    let cohorts: BTreeSet<_> = rows
        .iter()
        .filter(|agent| launched_by(agent, fire))
        .filter_map(AgentState::team_instance)
        .collect();
    let ids: BTreeSet<String> = rows
        .iter()
        .filter(|agent| {
            launched_by(agent, fire)
                || agent
                    .team_instance()
                    .is_some_and(|cohort| cohorts.contains(&cohort))
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
/// the launches its rows recorded, its lane once its rows are gone, its worktree flag, and
/// its text: the same fire's, else the text of the task's unfired entry (a consumed one-shot's
/// mark keeps none).
pub(super) fn carry(prior: &BookingsCache, fresh: &mut [BookingFacts]) {
    for facts in fresh {
        let Some(fire) = facts.fire.as_mut() else {
            continue;
        };
        let same_fire = prior.bookings.iter().find_map(|before| {
            let before_fire = before.fire.as_ref()?;
            (before_fire.fire == fire.fire).then_some((before, before_fire))
        });
        if facts.description.is_none() {
            facts.description = same_fire
                .map(|(before, _)| before)
                .or_else(|| {
                    prior
                        .bookings
                        .iter()
                        .find(|before| before.fire.is_none() && before.task == facts.task)
                })
                .and_then(|before| before.description.clone());
        }
        let Some((before, before_fire)) = same_fire else {
            continue;
        };
        if facts.lane.is_none() {
            facts.lane = before.lane.clone();
        }
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

/// The latest end stamp of what the fire launched: the completion of each supervised run
/// carrying its id, a `-p` launch's own run; once every row carrying its id (`rows`) is over
/// by `agents::state::agent_end`, the newest of their end stamps (the fire's own time when
/// none stamped one); and, for a fire whose outcome is recorded as other
/// than `launched` with nothing carrying its id, the fire's own time (a launch that failed
/// made nothing to end later).
fn ended_at(mark: &FireMark, records: &RoomRecords, rows: &[&AgentState]) -> Option<Timestamp> {
    let stamped: Vec<&crate::store::run::RunRecord> = records
        .runs
        .iter()
        .filter(|run| run.loop_fire.as_ref() == Some(&mark.fire))
        .collect();
    let runs_ended = stamped.iter().filter_map(|run| run.completed_at);
    let rows_ended = (!rows.is_empty() && rows.iter().all(|agent| agent_over(records.rows, agent)))
        .then(|| {
            rows.iter()
                .filter_map(|agent| agent.ended_at)
                .max()
                .unwrap_or(mark.at)
        });
    let unlaunched = (stamped.is_empty()
        && rows.is_empty()
        && mark
            .result
            .is_some_and(|result| result != LoopRunResult::Launched))
    .then_some(mark.at);
    runs_ended.chain(rows_ended).chain(unlaunched).max()
}

/// Whether `agent`'s launch is over among the room's Audit `rows`: ended, or a provisional
/// launch that failed; a row no launch answers to is over once its session is.
pub(super) fn agent_over(rows: &[AgentState], agent: &AgentState) -> bool {
    match agent_end(rows, &agent.kind, &agent.agent_id) {
        AgentEnd::Ended => true,
        AgentEnd::Live => false,
        AgentEnd::Unknown => agent.session_over(),
    }
}

/// Whether `agent` is a row the fire `fire` launched: it carries the fire's id, on every cell
/// of the launch.
pub(in crate::sidebar) fn launched_by(agent: &AgentState, fire: &FireId) -> bool {
    agent.loop_fire.as_ref() == Some(fire)
}
