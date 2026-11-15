//! What a loop task's fires launched that is still alive, read from the fire stamps alone:
//! the workflow runs, supervised runs and agents whose `loop_fire` names the task.
//!
//! Alive is durable state, never process liveness: a workflow run whose state is not
//! terminal, a supervised run whose status is not terminal, an agent whose launch has not
//! ended over the audit rows (an idle agent is alive). A team is its stamped agents. An agent
//! a listed supervised run drives is that run's, so it is not listed apart. A launch whose
//! environment dropped `RIMZ_LOOP_FIRE` carries no stamp and is never read here.

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
}

/// Every launch alive in the room at `paths` whose fire belongs to `task`: workflow runs,
/// then supervised runs, then agents from `agents` (the room's audit rows).
pub fn alive<'a>(
    task: &TaskId,
    paths: &StatePaths,
    agents: impl IntoIterator<Item = &'a AgentState> + Clone,
) -> anyhow::Result<Vec<AliveLaunch>> {
    let ours = |fire: Option<&FireId>| fire.filter(|fire| &fire.task() == task).cloned();
    let mut alive = Vec::new();
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

#[cfg(test)]
#[path = "launches/tests.rs"]
mod tests;
