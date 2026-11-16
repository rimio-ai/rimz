//! What a loop task's fires launched that is still alive, read from the fire stamps alone:
//! the workflow runs, supervised runs and agents whose `loop_fire` names the task.
//!
//! Alive is durable state: a workflow run whose state is not terminal, a supervised run whose
//! status is not terminal, an agent whose launch has not ended over the audit rows (an idle
//! agent is alive). A team is its stamped agents. The one process read is a fire's launch child
//! (`rimz` running the task's launch), alive while the pid and start token its mark holds run,
//! whether or not the fire's outcome is recorded. An agent a listed supervised run drives is
//! that run's, so it is not listed apart. A launch whose environment dropped `RIMZ_LOOP_FIRE`
//! carries no stamp and is never read here.

use crate::agents::AgentState;
use crate::agents::state::{AgentEnd, agent_end, launch_holder, launch_members};
use crate::disk::paths::StatePaths;
use crate::ids::{FireId, TaskId};

/// One alive launch and the fire that started it.
#[derive(Clone, Debug)]
pub struct AliveLaunch {
    pub fire: FireId,
    pub launch: Launch,
}

/// What an alive launch is.
#[derive(Clone, Debug)]
pub enum Launch {
    Workflow(Box<crate::store::workflow::RunRecord>),
    Supervised(Box<crate::store::run::RunRecord>),
    /// The launch's current holder row: one entry per launch id, every cell of a team apart.
    Agent(Box<AgentState>),
    /// A fire's launch child, from its fire mark.
    Child {
        child: super::task_record::ProcessMark,
        kind: Option<crate::config::LaunchKind>,
        checkout: std::path::PathBuf,
    },
}

impl Launch {
    /// What `rimz loop show` calls it.
    pub const fn what(&self) -> &'static str {
        match self {
            Self::Workflow(_) => "workflow run",
            Self::Supervised(_) => "supervised run",
            Self::Agent(_) => "agent",
            Self::Child { .. } => "launch process",
        }
    }

    /// Whether a fire of a `kind` task launched it itself, as against something that launch went
    /// on to start: a workflow fire's launch is its child and its run (`task` set), an agent
    /// fire's its child, its supervised runs and agents, a team fire's its child and agents.
    pub(super) fn launched_by(&self, kind: crate::config::LaunchKind) -> bool {
        use crate::config::LaunchKind;
        match (self, kind) {
            (Self::Child { .. }, _) | (_, LaunchKind::Shell) => true,
            (Self::Workflow(run), LaunchKind::Workflow) => run.task.is_some(),
            (Self::Supervised(_) | Self::Agent(_), LaunchKind::Agent)
            | (Self::Agent(_), LaunchKind::Team) => true,
            _ => false,
        }
    }

    /// How to address it: a run's id, a launch child's pid, or an agent's `@handle#lane` among
    /// the room's open rows in `agents`.
    pub fn address(&self, agents: &[AgentState]) -> String {
        match self {
            Self::Workflow(run) => run.id.to_string(),
            Self::Supervised(run) => run.run_id.to_string(),
            Self::Child { child, .. } => child.pid.to_string(),
            Self::Agent(agent) => {
                let peers = agents
                    .iter()
                    .filter(|row| !row.session_over())
                    .collect::<Vec<_>>();
                crate::address::agent_handle(agent, &peers, true)
            }
        }
    }
}

/// What holds a task in `rimz loop show`'s words: `agent @a#main` (a team's agents together),
/// `supervised run <id>`, `workflow run <id>`, or a launch child still starting by its pid.
pub(super) fn describe(launches: &[AliveLaunch], agents: &[AgentState]) -> String {
    let handles = launches
        .iter()
        .filter(|alive| matches!(alive.launch, Launch::Agent(_)))
        .map(|alive| alive.launch.address(agents))
        .collect::<Vec<_>>();
    let mut parts = launches
        .iter()
        .filter_map(|alive| match &alive.launch {
            Launch::Agent(_) => None,
            Launch::Child { child, .. } => {
                Some(format!("the launch still starting as pid {}", child.pid))
            }
            launch => Some(format!("{} {}", launch.what(), launch.address(agents))),
        })
        .collect::<Vec<_>>();
    match handles.len() {
        0 => {}
        1 => parts.push(format!("agent {}", handles[0])),
        _ => parts.push(format!("agents {}", handles.join(", "))),
    }
    parts.join(", ")
}

/// The addresses of what `fire` launched that is still alive, launch children aside, joined
/// with `, `: its agents or its supervised run. `None` when nothing is.
pub(super) fn launched(
    fire: &FireId,
    paths: &StatePaths,
    agents: &[AgentState],
) -> anyhow::Result<Option<String>> {
    let addresses = alive(&fire.task(), paths, agents)?
        .into_iter()
        .filter(|alive| &alive.fire == fire && !matches!(alive.launch, Launch::Child { .. }))
        .map(|alive| alive.launch.address(agents))
        .collect::<Vec<_>>();
    Ok((!addresses.is_empty()).then(|| addresses.join(", ")))
}

/// Every launch alive in the room at `paths` whose fire belongs to `task`: launch children,
/// then workflow runs, then supervised runs, then agents from `agents` (the room's audit rows).
pub fn alive<'a>(
    task: &TaskId,
    paths: &StatePaths,
    agents: impl IntoIterator<Item = &'a AgentState> + Clone,
) -> anyhow::Result<Vec<AliveLaunch>> {
    let ours = |fire: Option<&FireId>| fire.filter(|fire| &fire.task() == task).cloned();
    let mut alive = Vec::new();
    if let Some(record) = super::task_record::load()?.get(task) {
        for (checkout, mark) in &record.marks {
            if let Some(child) = mark.child.as_ref().filter(|child| child.is_live()) {
                alive.push(AliveLaunch {
                    fire: mark.fire.clone(),
                    launch: Launch::Child {
                        child: child.clone(),
                        kind: mark.kind,
                        checkout: checkout.clone(),
                    },
                });
            }
        }
    }
    if paths.root.exists() {
        for run in crate::store::workflow::live_runs(paths)? {
            if let Some(fire) = ours(run.loop_fire.as_ref()).filter(|_| !run.state.is_terminal()) {
                alive.push(AliveLaunch {
                    fire,
                    launch: Launch::Workflow(Box::new(run)),
                });
            }
        }
    }
    let runs = crate::harness::run::list(paths)?
        .into_iter()
        .filter(|run| !run.status.is_terminal())
        .filter_map(|run| ours(run.loop_fire.as_ref()).map(|fire| (fire, run)))
        .collect::<Vec<_>>();
    let mut seen = std::collections::BTreeSet::new();
    let mut agent_launches = Vec::new();
    for row in agents.clone() {
        let Some(fire) = ours(row.loop_fire.as_ref()) else {
            continue;
        };
        if row.is_provider_subagent()
            || runs.iter().any(|(_, run)| run.matches_agent(row))
            || !seen.insert(row.launch_key().clone())
            || agent_end(agents.clone(), &row.kind, &row.agent_id) != AgentEnd::Live
        {
            continue;
        }
        let members = launch_members(agents.clone(), &row.kind, &row.agent_id);
        let holder = launch_holder(&members)
            .into_iter()
            .find(|agent| !agent.session_over())
            .unwrap_or(row);
        agent_launches.push(AliveLaunch {
            fire,
            launch: Launch::Agent(Box::new(holder.clone())),
        });
    }
    alive.extend(runs.into_iter().map(|(fire, run)| AliveLaunch {
        fire,
        launch: Launch::Supervised(Box::new(run)),
    }));
    alive.extend(agent_launches);
    Ok(alive)
}

/// The room's audit rows, empty while the room has no store.
pub(super) fn room_agents(state: &StatePaths) -> anyhow::Result<Vec<AgentState>> {
    let runtime = crate::disk::paths::RuntimePaths::for_state(state)?;
    Ok(match crate::Store::open_existing(state.clone(), runtime) {
        Some(store) => store.audit_rows()?.iter().cloned().collect(),
        None => Vec::new(),
    })
}

#[cfg(test)]
#[path = "launches/tests.rs"]
mod tests;
