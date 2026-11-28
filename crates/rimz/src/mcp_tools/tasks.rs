//! `tasks`: what the caller started, from the list `rimz agents show` prints under `started`,
//! flattened to one row per thing a stop or join takes, each by its id.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use jiff::Timestamp;
use serde::Serialize;

use super::Tools;
use super::owned::Owners;
use super::room::Room;
use super::start::TaskKind;
use crate::agents::AgentState;
use crate::harness::idle_stop::started::{RowKey, RowKind, StartedRow};
use crate::harness::schedule::catalog::TaskCatalog;
use crate::launcher::context::resolve_me;
use crate::mcp::catalog::TasksArgs;
use crate::mcp::{Answer, Failure};

/// The longest `about` a row carries, in characters, before it is cut.
const ABOUT_MAX: usize = 80;

#[derive(Serialize)]
struct Tasks {
    tasks: Vec<Task>,
}

#[derive(Serialize)]
struct Task {
    id: String,
    kind: &'static str,
    status: &'static str,
    on_your_stop: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    age: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    about: Option<String>,
    detail: String,
}

/// What a row's lookups read.
struct Sources<'a> {
    owners: Owners<'a>,
    agents: Vec<&'a AgentState>,
    runs: crate::store::run::RunListing,
    state: &'a crate::StatePaths,
    catalog: TaskCatalog,
    now: Timestamp,
}

pub(super) fn tasks(tools: &Tools, args: &TasksArgs) -> Result<Answer, Failure> {
    let wanted = args.kind.map(TaskKind::from);
    let room = Room::open(&tools.start_dir)?;
    let snapshot = room
        .store
        .snapshot_cached()
        .context("reading agent snapshot")?;
    let agent = resolve_me(&room.store, &snapshot)?;
    let started = super::control::started(&room, agent)?;
    let audit = room.store.audit_rows().map_err(anyhow::Error::from)?;
    let runs = crate::harness::run::listing(room.store.paths()).map_err(anyhow::Error::from)?;
    let sources = Sources {
        owners: Owners::new(agent, &snapshot.agents, &audit, room.store.paths()),
        agents: snapshot.agents.iter().chain(audit.iter()).collect(),
        runs,
        state: room.store.paths(),
        catalog: TaskCatalog::load_room_lenient(&room.workspace.project_root, room.store.paths()),
        now: Timestamp::now(),
    };
    let groups = [
        ("holds", &started.holding),
        ("closes", &started.closing),
        ("outlives", &started.outliving),
    ];
    let mut seen = HashSet::new();
    let mut tasks = Vec::new();
    for (on_your_stop, rows) in groups {
        for row in rows {
            let Some((kind, task)) = sources.task(row, on_your_stop) else {
                continue;
            };
            if wanted.is_some_and(|wanted| wanted != kind) || !seen.insert(task.id.clone()) {
                continue;
            }
            tasks.push(task);
        }
    }
    Ok(Answer::data(&Tasks { tasks }))
}

impl Sources<'_> {
    /// The row a list row names by its key, with its kind; none for a row a stop or join does
    /// not take.
    fn task(&self, row: &StartedRow, on_your_stop: &'static str) -> Option<(TaskKind, Task)> {
        let (kind, name, status, about, started) = match row.key.as_ref()? {
            RowKey::Agent { kind, launch } => {
                let agent = crate::address::launch_row(self.agents.iter().copied(), kind, launch)?;
                let task_kind = if row.kind == RowKind::Subagent {
                    TaskKind::Subagent
                } else {
                    TaskKind::Agent
                };
                let terminal_child_run = task_kind == TaskKind::Subagent
                    && self
                        .runs
                        .iter()
                        .filter(|run| run.matches_agent(agent))
                        .max_by_key(|run| run.updated_at)
                        .is_some_and(|run| run.status.is_terminal());
                let status = if row.status == "ended" || terminal_child_run {
                    "done"
                } else {
                    "running"
                };
                let about = agent
                    .description
                    .as_deref()
                    .or(agent.first_prompt.as_deref());
                let name = super::owned::agent_name(self.agents.iter().copied(), agent);
                (
                    task_kind,
                    name,
                    status,
                    about.and_then(one_line),
                    agent.registered_at,
                )
            }
            RowKey::Team(instance) => {
                let goal = crate::address::team_cohorts(self.agents.iter().copied())
                    .into_iter()
                    .find(|cohort| &cohort.instance == instance)
                    .and_then(|cohort| {
                        cohort.members.iter().find_map(|member| {
                            let checkout = member.worktree_path.as_deref()?;
                            crate::harness::scratch::board_section(Path::new(checkout), "Goal")
                        })
                    });
                let about = goal.as_deref().and_then(one_line);
                (TaskKind::Team, instance.to_string(), "running", about, None)
            }
            RowKey::Workflow(id) => {
                let run = crate::ids::WorkflowRunId::parse(id).ok();
                let found = run
                    .as_ref()
                    .and_then(|run| crate::store::workflow::find(self.state, run).ok()?);
                let header = found.as_ref().and_then(|found| found.class.header());
                let ended = header.as_ref().is_some_and(|header| header.is_terminal());
                let prompt = match found.as_ref().map(|found| &found.class) {
                    Some(crate::store::workflow::RunClass::Current(record)) => {
                        record.prompt.as_deref().and_then(one_line)
                    }
                    _ => None,
                };
                let status = if ended { "done" } else { "running" };
                let created = header.and_then(|header| header.created_at);
                let name = run.map_or_else(
                    || id.clone(),
                    |run| super::owned::run_name(&self.owners, &run),
                );
                (TaskKind::Workflow, name, status, prompt, created)
            }
            RowKey::Task(name) => {
                let status = if row.status == "disabled" {
                    "disabled"
                } else {
                    "running"
                };
                (
                    TaskKind::Schedule,
                    name.clone(),
                    status,
                    self.label(name),
                    None,
                )
            }
            RowKey::Wait(name) => {
                // The started list holds every wait that wakes the caller; a stop or join takes
                // only one it armed, by the rule `owned::resolve` applies.
                let entry = self.catalog.visible().get(name).map(|task| task.entry());
                if let Some(entry) = entry
                    && let Some(target) = &entry.wait
                    && !self.owners.armed_wait(entry, target, &self.catalog)
                {
                    return None;
                }
                let kind = match entry.map(super::owned::wait_kind) {
                    Some(crate::harness::schedule::wait::WaitKind::Shell) => TaskKind::Shell,
                    _ => TaskKind::Watch,
                };
                (kind, name.clone(), "running", self.label(name), row.since)
            }
        };
        Some((
            kind,
            Task {
                id: kind.id(&name),
                kind: kind.as_str(),
                status,
                on_your_stop,
                age: started.map(|started| self.age(started)),
                about,
                detail: row.status.clone(),
            },
        ))
    }

    fn label(&self, name: &str) -> Option<String> {
        let task = self.catalog.visible().get(name)?;
        task.entry().label.as_deref().and_then(one_line)
    }

    fn age(&self, started: Timestamp) -> String {
        let millis = self.now.as_millisecond() - started.as_millisecond();
        crate::utils::time::format_duration_compact(Duration::from_millis(
            u64::try_from(millis).unwrap_or(0),
        ))
    }
}

/// The first line of `text` with words, cut at [`ABOUT_MAX`] characters.
fn one_line(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    if line.chars().count() <= ABOUT_MAX {
        return Some(line.to_owned());
    }
    let cut: String = line.chars().take(ABOUT_MAX - 1).collect();
    Some(format!("{}…", cut.trim_end()))
}
