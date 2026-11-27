//! What an id names, read from the named thing's own record, and whether the caller started
//! it: the record's starter chain is walked upward (an agent's launcher and the bookings that
//! launched it, a run's starter and the booking that fired it, the agent, team, or loop task
//! that armed a shell or a watch) until it meets the caller's launch.

use std::collections::{HashSet, VecDeque};
use std::str::FromStr;

use super::start::TaskKind;
use crate::StatePaths;
use crate::address::TeamCohort;
use crate::agents::{AgentState, LaunchedBy};
use crate::config::{TaskEntry, TaskTarget};
use crate::harness::schedule::arm::TaskName;
use crate::harness::schedule::catalog::TaskCatalog;
use crate::ids::{AgentKind, AgentSessionId, TeamInstanceId, WorkflowRunId};
use crate::mcp::Failure;
use crate::store::runtime::AuditRows;
use crate::store::workflow::{FoundRun, RunClass, RunStarter};

/// How many links a walk reads before it gives up: a chain is a few links long.
const MAX_WALK: usize = 64;

/// One link of a starter chain.
enum Link {
    Agent(AgentKind, AgentSessionId),
    Run(WorkflowRunId),
}

/// The caller and the records its starter walks read.
pub(super) struct Owners<'a> {
    pub(super) caller: &'a AgentState,
    live: &'a [AgentState],
    audit: &'a AuditRows,
    state: &'a StatePaths,
}

impl<'a> Owners<'a> {
    pub(super) fn new(
        caller: &'a AgentState,
        live: &'a [AgentState],
        audit: &'a AuditRows,
        state: &'a StatePaths,
    ) -> Self {
        Self {
            caller,
            live,
            audit,
            state,
        }
    }

    /// The row holding launch `id` now: the live snapshot's, else the log's, which keeps an
    /// ended agent.
    pub(super) fn row(&self, kind: &AgentKind, id: &AgentSessionId) -> Option<&'a AgentState> {
        crate::address::launch_row(self.live, kind, id).or_else(|| {
            crate::agents::state::launch_members(self.audit.iter(), kind, id)
                .into_iter()
                .next()
        })
    }

    /// The live row of `row`'s launch, while it has not ended.
    pub(super) fn live(&self, row: &AgentState) -> Option<&'a AgentState> {
        crate::address::launch_row(self.live, &row.kind, row.launch_key())
            .filter(|live| live.ended_at.is_none())
    }

    fn is_caller(&self, kind: &AgentKind, id: &AgentSessionId) -> bool {
        self.caller.launch_id.as_ref() == Some(id)
            || (kind == &self.caller.kind && self.caller.answers_to(id))
    }

    fn reaches(&self, start: Vec<Link>) -> bool {
        let mut queue = VecDeque::from(start);
        let mut seen = HashSet::new();
        while let Some(link) = queue.pop_front() {
            if seen.len() >= MAX_WALK {
                break;
            }
            match link {
                Link::Agent(kind, id) => {
                    if self.is_caller(&kind, &id) {
                        return true;
                    }
                    if seen.insert(format!("agent {kind} {id}"))
                        && let Some(row) = self.row(&kind, &id)
                    {
                        queue.extend(agent_links(row));
                    }
                }
                Link::Run(run) => {
                    if seen.insert(format!("run {run}"))
                        && let Ok(Some(found)) = crate::store::workflow::find(self.state, &run)
                    {
                        queue.extend(run_links(&found));
                    }
                }
            }
        }
        false
    }

    pub(super) fn started_agent(&self, row: &AgentState) -> bool {
        self.reaches(agent_links(row))
    }

    pub(super) fn started_run(&self, found: &FoundRun) -> bool {
        self.reaches(run_links(found))
    }

    /// The agent a wait pinned to `target` wakes, read through the wait rules of `pending`.
    fn wait_holder(&self, target: &TaskTarget) -> Option<&'a AgentState> {
        crate::harness::schedule::pending::target_holder(self.audit.iter(), target)
    }

    /// Whether the caller armed the wait `entry` books for `target`, or an agent it started
    /// did. A team's subscription was armed by the team, so it walks from the member's
    /// launcher, and a loop task's by that task's booking in `catalog`; any other wait was
    /// armed by the agent it wakes.
    pub(super) fn armed_wait(
        &self,
        entry: &TaskEntry,
        target: &TaskTarget,
        catalog: &TaskCatalog,
    ) -> bool {
        let holder = self.wait_holder(target);
        let armers = if entry.team.is_some() {
            holder.map(agent_links).unwrap_or_default()
        } else if let Some(task) = &entry.loop_task {
            catalog.visible().get(task).map_or_else(Vec::new, |task| {
                booking_links(&task.entry().booked_by).collect()
            })
        } else {
            vec![Self::woken(holder, target)]
        };
        self.reaches(armers)
    }

    /// Whether the caller armed a self wait (a shell) for `target`, or an agent it started did:
    /// such a wait is armed by the agent it wakes. Read once the wait's row is gone.
    pub(super) fn armed_self_wait(&self, target: &TaskTarget) -> bool {
        self.reaches(vec![Self::woken(self.wait_holder(target), target)])
    }

    /// Whether a wait for `target` wakes the caller itself, not an agent it started.
    pub(super) fn wakes_caller(&self, target: &TaskTarget) -> bool {
        match self.wait_holder(target) {
            Some(holder) => self.is_caller(&holder.kind, holder.launch_key()),
            None => self.is_caller(&target.kind, &target.session),
        }
    }

    /// The agent a wait for `target` wakes: its holder now, else the target as armed.
    fn woken(holder: Option<&AgentState>, target: &TaskTarget) -> Link {
        holder.map_or_else(
            || Link::Agent(target.kind.clone(), target.session.clone()),
            |holder| Link::Agent(holder.kind.clone(), holder.launch_key().clone()),
        )
    }

    fn started_booking(&self, booked_by: &[LaunchedBy]) -> bool {
        self.reaches(booking_links(booked_by).collect())
    }
}

fn agent_links(row: &AgentState) -> Vec<Link> {
    row.launcher()
        .map(|(kind, id)| Link::Agent(kind.clone(), id.clone()))
        .into_iter()
        .chain(booking_links(&row.loop_booked_by))
        .collect()
}

fn booking_links(booked_by: &[LaunchedBy]) -> impl Iterator<Item = Link> + '_ {
    booked_by
        .iter()
        .map(|link| Link::Agent(link.kind.clone(), link.agent_id.clone()))
}

fn run_links(found: &FoundRun) -> Vec<Link> {
    let mut links = Vec::new();
    match found
        .class
        .header()
        .and_then(|header| header.starter.clone())
    {
        Some(RunStarter::Agent { kind, launch }) => links.push(Link::Agent(kind, launch)),
        Some(RunStarter::Run { run, .. }) => links.push(Link::Run(run)),
        None => {}
    }
    if let RunClass::Current(record) = &found.class
        && let Some(fire) = &record.fire
    {
        links.extend(booking_links(fire.booked_by()));
    }
    links
}

/// What an id names, once the caller is known to have started it.
pub(super) enum Named<'a> {
    /// A subagent or agent, by its newest launch the caller started, with its live row while
    /// it runs.
    Agent {
        row: &'a AgentState,
        live: Option<&'a AgentState>,
    },
    Team(TeamCohort<'a>),
    Workflow {
        id: WorkflowRunId,
        found: Box<FoundRun>,
    },
    /// A schedule, with the booking the caller was found to have made, which its removal
    /// checks again.
    Schedule {
        booked_by: Vec<LaunchedBy>,
    },
    /// A wait, with the live row of the agent it wakes while that agent runs.
    Wait {
        target: Option<&'a AgentState>,
    },
    /// A wait, schedule, or team with no live record: one that ended leaves none, so it cannot
    /// be told from a name that never existed.
    Gone,
}

/// The newest run of team instance `name` led by a seat the caller started: what a `join` on
/// the team answers from once its cohort no longer runs. A launch on the same lane that another
/// agent started is never read.
pub(super) fn ended_team_run(
    owners: &Owners<'_>,
    name: &str,
) -> Option<crate::store::run::RunRecord> {
    let instance = TeamInstanceId::from_str(name).ok()?;
    let started: Vec<_> = owners
        .audit
        .iter()
        .filter(|row| row.team_instance().as_ref() == Some(&instance) && owners.started_agent(row))
        .map(|row| row.launch_key())
        .collect();
    crate::harness::run::list(owners.state)
        .ok()?
        .into_iter()
        .filter(|run| {
            run.team
                .as_ref()
                .is_some_and(|team| team.instance == instance && started.contains(&&team.launch_id))
        })
        .max_by_key(|run| run.started_at)
}

/// What the id `kind:name` names, refused when another agent started it or no record of it
/// exists. `catalog` is the room's loop catalog, read for a schedule or a wait.
pub(super) fn resolve<'a>(
    owners: &Owners<'a>,
    kind: TaskKind,
    name: &str,
    catalog: impl FnOnce() -> anyhow::Result<TaskCatalog>,
) -> Result<Named<'a>, Failure> {
    let id = kind.id(name);
    let refused = || refused(&id);
    let unknown = || {
        Failure::new(format!("no {} named `{name}`", kind.as_str()))
            .with_hint("`tasks` lists what you started, by id")
    };
    match kind {
        TaskKind::Subagent | TaskKind::Agent => {
            let subagent = kind == TaskKind::Subagent;
            let named: Vec<&'a AgentState> = owners
                .live
                .iter()
                .chain(owners.audit.iter())
                .filter(|row| {
                    row.is_launched_child() == subagent
                        && !row.is_provider_subagent()
                        && (row.name.as_deref() == Some(name) || row.launch_key().as_str() == name)
                })
                .collect();
            if named.is_empty() {
                return Err(unknown());
            }
            let row = named
                .into_iter()
                .filter(|row| owners.started_agent(row))
                .max_by_key(|row| row.registered_at.unwrap_or(row.last_activity))
                .ok_or_else(refused)?;
            Ok(Named::Agent {
                row,
                live: owners.live(row),
            })
        }
        TaskKind::Team => {
            let instance = TeamInstanceId::from_str(name).map_err(|err| {
                Failure::new(format!("invalid team `{name}`: {err}"))
                    .with_hint("a team id names its instance, as `team:forge#fix-login`")
            })?;
            let Some(cohort) = crate::address::team_cohorts(owners.live)
                .into_iter()
                .find(|cohort| cohort.instance == instance)
            else {
                return Ok(Named::Gone);
            };
            if !cohort
                .members
                .iter()
                .any(|member| owners.started_agent(member))
            {
                return Err(refused());
            }
            Ok(Named::Team(cohort))
        }
        TaskKind::Workflow => {
            let run = WorkflowRunId::parse(name).map_err(|_| unknown())?;
            let found = crate::store::workflow::find(owners.state, &run)
                .map_err(anyhow::Error::from)?
                .ok_or_else(unknown)?;
            if !owners.started_run(&found) {
                return Err(refused());
            }
            Ok(Named::Workflow {
                id: run,
                found: Box::new(found),
            })
        }
        TaskKind::Schedule | TaskKind::Shell | TaskKind::Watch => {
            TaskName::from_str(name).map_err(|err| {
                Failure::new(format!("invalid {} name `{name}`: {err}", kind.as_str()))
                    .with_hint("`tasks` lists what you started, by id")
            })?;
            let catalog = catalog()?;
            let Some(entry) = catalog.visible().get(name).map(|task| task.entry()) else {
                return Ok(Named::Gone);
            };
            match (&entry.wait, kind) {
                (None, TaskKind::Schedule) => {
                    if !owners.started_booking(&entry.booked_by) {
                        return Err(refused());
                    }
                    Ok(Named::Schedule {
                        booked_by: entry.booked_by.clone(),
                    })
                }
                (Some(target), TaskKind::Shell | TaskKind::Watch)
                    if wait_kind(entry).as_str() == kind.as_str() =>
                {
                    if !owners.armed_wait(entry, target, &catalog) {
                        return Err(refused());
                    }
                    Ok(Named::Wait {
                        target: owners.wait_holder(target).and_then(|row| owners.live(row)),
                    })
                }
                _ => Err(unknown()),
            }
        }
    }
}

/// Whether the wake row `entry` is a shell or a watch.
pub(super) fn wait_kind(entry: &TaskEntry) -> crate::harness::schedule::wait::WaitKind {
    crate::harness::schedule::wait::WaitKind::of(entry)
}

/// The refusal of an id another agent started.
pub(super) fn refused(id: &str) -> Failure {
    Failure::new(format!("{id} was started by another agent")).with_hint(
        "stop and join act on what you started, or what an agent you started started; \
         `tasks` lists it",
    )
}

/// The hint of a thing with no live record.
pub(super) const GONE: &str = "nothing of this name runs now: it ended, or never existed, and \
     RimZ keeps no record that tells which";
