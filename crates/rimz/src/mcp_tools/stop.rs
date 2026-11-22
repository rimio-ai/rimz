//! `stop`: end one thing the caller started, by its id, through the library function the
//! command of the same job calls: the tree stop of `rimz agents stop`, `rimz subagents stop`, and
//! `rimz teams stop`, the cancel of `rimz wait cancel` and `rimz workflow cancel`, and a
//! schedule's removal that lets each run it started finish.

use std::str::FromStr;

use serde::Serialize;

use super::Tools;
use super::owned::{self, GONE, Named, Owners};
use super::room::Room;
use super::start::TaskKind;
use crate::harness::schedule::arm::TaskName;
use crate::harness::schedule::catalog::TaskCatalog;
use crate::harness::schedule::launches::Launch;
use crate::harness::schedule::wait::{self, CancelTarget};
use crate::launcher::context::resolve_me;
use crate::launcher::stop::{TreeStop, stop_resolved, stop_subagents};
use crate::mcp::catalog::StopArgs;
use crate::mcp::{Answer, Failure};
use crate::store::workflow::RunClass;

/// What `stop` answers: `stopped`, or `ended` for a thing that had ended before the call, with
/// what a stopped schedule had started that runs on.
#[derive(Serialize)]
struct Stopped {
    id: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    still_running: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'static str>,
}

impl Stopped {
    fn new(id: String, status: &'static str) -> Self {
        Self {
            id,
            status,
            still_running: None,
            hint: None,
        }
    }
}

pub(super) fn stop(tools: &Tools, args: &StopArgs) -> Result<Answer, Failure> {
    let (kind, name) = TaskKind::parse(&args.id)?;
    let id = args.id.clone();
    let room = Room::open(&tools.start_dir)?;
    let snapshot = room.alive_snapshot()?;
    let caller = resolve_me(&room.store, &snapshot)?;
    let audit = room.store.audit_rows().map_err(anyhow::Error::from)?;
    let owners = Owners::new(caller, &snapshot.agents, &audit, room.store.paths());
    let root = room.workspace.project_root.clone();
    let named = owned::resolve(&owners, kind, name, || TaskCatalog::load(Some(&root)))?;
    let mut notices = Vec::new();
    let stopped = match named {
        Named::Gone => Stopped {
            hint: Some(GONE),
            ..Stopped::new(id, "ended")
        },
        Named::Agent { live: None, .. } => Stopped::new(id, "ended"),
        Named::Agent {
            live: Some(agent), ..
        } => {
            let stop = if kind == TaskKind::Subagent {
                let stops = stop_subagents(room.stop_room(), &snapshot, vec![agent], &mut notices)?;
                if let Some((_, err)) = stops.unsettled.into_iter().next() {
                    return Err(Failure::new(format!(
                        "{id} was not stopped: its report could not be settled: {err:#}"
                    )));
                }
                stops.stops.into_iter().map(|(_, stop)| stop).next()
            } else {
                stop_resolved(room.stop_room(), &snapshot, &[agent], &mut notices)?
                    .into_iter()
                    .next()
            };
            failed(&id, stop.iter(), &notices)?;
            Stopped::new(id, "stopped")
        }
        Named::Team(cohort) => {
            let stops = stop_resolved(room.stop_room(), &snapshot, &cohort.members, &mut notices)?;
            failed(&id, stops.iter(), &notices)?;
            Stopped::new(id, "stopped")
        }
        Named::Workflow { id: run, found } => {
            let terminal = found
                .class
                .header()
                .is_some_and(|header| header.is_terminal());
            if found.finished || terminal {
                Stopped::new(id, "ended")
            } else {
                let state = room.store.paths();
                let host = crate::launcher::workflow_host::Host::new(
                    state,
                    None,
                    None,
                    None,
                    Box::new(crate::launcher::Unattended),
                );
                let changed = crate::harness::workflow::cancel(state, &run, &host, Some("stop"));
                crate::harness::schedule::run_log::collect_run(state, &run);
                if changed.map_err(anyhow::Error::from)? {
                    // A cancel stops only the step's waiting, so the runs it started go on.
                    let still_running = match &found.class {
                        RunClass::Current(record) => {
                            crate::harness::workflow::started_runs_running(state, record)
                                .iter()
                                .map(|run| TaskKind::Workflow.id(run.as_str()))
                                .collect()
                        }
                        _ => Vec::new(),
                    };
                    Stopped {
                        still_running: Some(still_running),
                        ..Stopped::new(id, "stopped")
                    }
                } else {
                    Stopped::new(id, "ended")
                }
            }
        }
        Named::Schedule { booked_by } => {
            let catalog = TaskCatalog::load(Some(&root))?;
            let (removed, alive) = catalog.remove_keeping_runs(name, &booked_by, audit.iter())?;
            if removed.changed() {
                Stopped {
                    still_running: Some(
                        alive
                            .iter()
                            .filter_map(|alive| launch_id(&alive.launch))
                            .collect(),
                    ),
                    ..Stopped::new(id, "stopped")
                }
            } else {
                Stopped::new(id, "ended")
            }
        }
        Named::Wait { target: None } => Stopped::new(id, "ended"),
        Named::Wait {
            target: Some(target),
        } => {
            let task = TaskName::from_str(name)
                .map_err(|err| Failure::new(format!("invalid wait name `{name}`: {err}")))?;
            let canceled = wait::cancel(
                &super::wait::wait_room(&room, &snapshot),
                target,
                CancelTarget::Name(task),
            );
            match canceled {
                Ok(_) => Stopped::new(id, "stopped"),
                // A wait that fired since the call read it has ended.
                Err(_) if !TaskCatalog::load(Some(&root))?.visible().contains_key(name) => {
                    Stopped::new(id, "ended")
                }
                Err(err) => return Err(err.into()),
            }
        }
    };
    for notice in notices {
        tracing::info!(%notice, "stop");
    }
    Ok(Answer::data(&stopped))
}

/// A failure naming each member a tree stop could not stop, with the backend's notices.
fn failed<'a>(
    id: &str,
    stops: impl Iterator<Item = &'a TreeStop>,
    notices: &[String],
) -> Result<(), Failure> {
    let errors: Vec<String> = stops.filter_map(TreeStop::error).collect();
    if errors.is_empty() {
        return Ok(());
    }
    let failure = Failure::new(format!(
        "{id} was not wholly stopped: {}",
        errors.join("; ")
    ));
    Err(match notices {
        [] => failure.with_hint("`tasks` shows what still runs; stop it again"),
        notices => failure.with_hint(notices.join("; ")),
    })
}

/// The id a schedule's still-running launch answers by, when it has one.
fn launch_id(launch: &Launch) -> Option<String> {
    match launch {
        Launch::Workflow(record) => Some(TaskKind::Workflow.id(record.id.as_str())),
        Launch::Supervised(run) => run.agent_name.as_deref().map(|name| {
            let kind = if run.subagent {
                TaskKind::Subagent
            } else {
                TaskKind::Agent
            };
            kind.id(name)
        }),
        Launch::Agent(row) => {
            let kind = if row.is_launched_child() {
                TaskKind::Subagent
            } else {
                TaskKind::Agent
            };
            Some(kind.id(row.name.as_deref().unwrap_or(row.launch_key().as_str())))
        }
        Launch::Child { .. } => None,
    }
}
