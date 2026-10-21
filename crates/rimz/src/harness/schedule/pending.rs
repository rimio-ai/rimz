//! Read-only pending wakes: catalog deliveries and unsettled launched runs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use crate::agents::{AgentState, AgentStatus, PendingWait, PendingWaitClass, PendingWaitTrigger};
use crate::config::{MachineConfig, WatchSpec};
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::snapshot::SidebarSnapshot;

use super::Trigger;
use super::catalog::{LoadedTask, TaskCatalog, TaskSource};

pub fn session_deliveries<'a>(
    catalog: &'a TaskCatalog,
    root: &Path,
    session: Option<&(AgentKind, AgentSessionId)>,
) -> impl Iterator<Item = (&'a str, &'a LoadedTask)> {
    catalog.visible().iter().filter_map(move |(name, task)| {
        if task.source() != TaskSource::Instance || task.entry().resolved_root() != root {
            return None;
        }
        let target = task.entry().wait.as_ref()?;
        if session.is_some_and(|(kind, session)| target.kind != *kind || target.session != *session)
        {
            return None;
        }
        Some((name.as_str(), task))
    })
}

fn pending_wait(name: &str, task: &LoadedTask, now: &jiff::Zoned) -> Option<PendingWait> {
    if !task.is_ephemeral() {
        return None;
    }
    let parsed = task.trigger().as_ref().ok()?;
    let meta = task.entry().wait_meta.as_ref();
    let armed_at = meta.map(|meta| meta.armed_at);
    let trigger = match &parsed.trigger {
        Trigger::Condition { expr, hold } => PendingWaitTrigger::Condition {
            when: expr.to_string(),
            hold: hold.map(super::arm::duration_label),
        },
        Trigger::Schedule(schedule) => {
            // A deadline shortens row lifetime, but does not make a recurring
            // clock's next occurrence derivable from its original arm time.
            if !schedule.once {
                return None;
            }
            let anchor = armed_at.map(|at| at.to_zoned(now.time_zone().clone()));
            let due = match schedule.schedule.instant() {
                Some(at) => at,
                None => parsed.next_after(anchor.as_ref().unwrap_or(now))?,
            };
            PendingWaitTrigger::Timer {
                due,
                delay: meta.and_then(|meta| meta.delay.clone()),
            }
        }
        Trigger::Watch(spec) => match spec {
            WatchSpec::Command(command) => PendingWaitTrigger::Command {
                command: command.clone(),
            },
            WatchSpec::Pid { pid } => PendingWaitTrigger::Pid { pid: *pid },
            WatchSpec::Check { check, .. } => PendingWaitTrigger::Check {
                command: check.clone(),
            },
            WatchSpec::File { file, grep, .. } => PendingWaitTrigger::File {
                path: file.clone(),
                grep: grep.clone(),
            },
        },
        Trigger::Signal { selector, .. } => PendingWaitTrigger::Signal {
            selector: selector.to_string(),
        },
    };
    let class = match trigger {
        PendingWaitTrigger::Pid { .. }
        | PendingWaitTrigger::Command { .. }
        | PendingWaitTrigger::Check { .. } => PendingWaitClass::Working,
        _ => PendingWaitClass::Sleeping,
    };
    Some(PendingWait {
        class,
        name: name.to_owned(),
        trigger,
        armed_at,
    })
}

pub fn pending_waits_by_session(
    catalog: &TaskCatalog,
    root: &Path,
    now: &jiff::Zoned,
) -> BTreeMap<(AgentKind, AgentSessionId), Vec<PendingWait>> {
    let mut waits = BTreeMap::<_, Vec<_>>::new();
    for (name, task) in session_deliveries(catalog, root, None) {
        let Some(wait) = pending_wait(name, task, now) else {
            continue;
        };
        let Some(target) = task.entry().wait.as_ref() else {
            continue;
        };
        waits
            .entry((target.kind.clone(), target.session.clone()))
            .or_default()
            .push(wait);
    }
    for waits in waits.values_mut() {
        waits.sort_by_key(|wait| {
            let (kind, due) = match wait.trigger {
                PendingWaitTrigger::Timer { due, .. } => (0, Some(due)),
                PendingWaitTrigger::Pid { .. }
                | PendingWaitTrigger::Command { .. }
                | PendingWaitTrigger::Check { .. }
                | PendingWaitTrigger::File { .. } => (1, None),
                PendingWaitTrigger::Signal { .. }
                | PendingWaitTrigger::Condition { .. }
                | PendingWaitTrigger::Subagent { .. }
                | PendingWaitTrigger::Team { .. } => (2, None),
            };
            (kind, due, wait.name.clone())
        });
    }
    waits
}

pub(crate) fn project_pending_waits(
    snapshot: &mut SidebarSnapshot,
    paths: &crate::StatePaths,
    project_root: Option<&Path>,
    config: &MachineConfig,
) {
    let now = snapshot.now.to_zoned(config.time_zone());
    SessionWaits::load_at(project_root, || now).attach(snapshot);
    project_run_waits(snapshot, paths);
}

/// Whether any agent has launched children or a team, the only case in which
/// the projection lists `runs/`; a room without launched work never reads it.
fn has_launched_work(agents: &[crate::agents::AgentState]) -> bool {
    let identities: std::collections::HashSet<_> = agents
        .iter()
        .flat_map(|agent| agent.identity_ids().map(move |id| (&agent.kind, id)))
        .collect();
    agents.iter().any(|agent| {
        (!agent.is_team_seat()
            && agent
                .launcher()
                .is_some_and(|key| identities.contains(&key)))
            || (agent.is_team_seat() && agent.launched_by.is_some())
    })
}

fn project_run_waits(snapshot: &mut SidebarSnapshot, paths: &crate::StatePaths) {
    use crate::harness::fleet::{self, FleetRunIndex};
    use crate::store::run;

    if !has_launched_work(&snapshot.agents) {
        return;
    }
    let runs = match run::listing(&paths.runs_dir) {
        Ok(runs) => runs,
        Err(error) => {
            tracing::debug!(%error, "failed to read pending launched runs");
            return;
        }
    };
    let peers = crate::address::addressable_agents(snapshot);
    let fleets = FleetRunIndex::new(&snapshot.agents, &runs);
    let agent_indices: BTreeMap<_, _> = snapshot
        .agents
        .iter()
        .enumerate()
        .map(|(index, agent)| ((&agent.kind, &agent.agent_id), index))
        .collect();
    let projected: Vec<Vec<(PendingWait, Option<usize>)>> = (0..snapshot.agents.len())
        .map(|index| {
            let mut waits: Vec<_> = fleets
                .of(index)
                .unsettled()
                .into_iter()
                .map(|(child, run)| {
                    (
                        PendingWait {
                            class: PendingWaitClass::Inert,
                            name: crate::address::agent_handle(child, &peers, false),
                            armed_at: Some(run.started_at),
                            trigger: PendingWaitTrigger::Subagent {
                                active_at: child.last_activity,
                                deadline_at: run.deadline_at,
                                settled: run
                                    .status
                                    .is_terminal()
                                    .then(|| run.status.label().to_owned()),
                            },
                        },
                        (!run.status.is_terminal())
                            .then(|| agent_indices.get(&(&child.kind, &child.agent_id)).copied())
                            .flatten(),
                    )
                })
                .collect();
            for run in fleets.owed_team_runs(index) {
                let Some(team) = &run.team else { continue };
                let stage = crate::harness::scratch::board_stage(&run.worktree_path)
                    .map(|stage| stage.name);
                if !fleet::team_stage_pending(stage.as_deref()) {
                    continue;
                }
                waits.push((
                    PendingWait {
                        class: PendingWaitClass::Working,
                        name: team.instance.clone(),
                        armed_at: Some(run.started_at),
                        trigger: PendingWaitTrigger::Team { stage },
                    },
                    None,
                ));
            }
            waits
        })
        .collect();
    let projected: Vec<Vec<PendingWait>> = projected
        .iter()
        .enumerate()
        .map(|(index, waits)| {
            waits
                .iter()
                .map(|(wait, target)| {
                    let mut wait = wait.clone();
                    if let Some(target) = target {
                        wait.class = target_work_class(
                            *target,
                            &snapshot.agents,
                            &projected,
                            &mut BTreeSet::from([index]),
                            snapshot.now,
                            snapshot.attention.stalled_after_secs.get(),
                        );
                    }
                    wait
                })
                .collect()
        })
        .collect();
    for (agent, mut waits) in Arc::make_mut(&mut snapshot.agents)
        .iter_mut()
        .zip(projected)
    {
        waits.append(&mut agent.pending_waits);
        agent.pending_waits = waits;
    }
}

fn target_work_class(
    index: usize,
    agents: &[AgentState],
    projected: &[Vec<(PendingWait, Option<usize>)>],
    visiting: &mut BTreeSet<usize>,
    now: jiff::Timestamp,
    stalled_after_secs: u32,
) -> PendingWaitClass {
    if !visiting.insert(index) {
        return PendingWaitClass::Inert;
    }
    let agent = &agents[index];
    let status = agent.rowless_status(&Default::default()).0;
    let class = if agent.silent_child_for(now, stalled_after_secs).is_some() {
        PendingWaitClass::Inert
    } else if status == AgentStatus::Running {
        PendingWaitClass::Working
    } else if matches!(
        status,
        AgentStatus::Idle | AgentStatus::Success | AgentStatus::Sleeping
    ) {
        let mut class = if status == AgentStatus::Sleeping {
            PendingWaitClass::Sleeping
        } else {
            PendingWaitClass::Inert
        };
        for (wait, target) in &projected[index] {
            let next = target.map_or(wait.class, |target| {
                target_work_class(target, agents, projected, visiting, now, stalled_after_secs)
            });
            if next == PendingWaitClass::Working {
                class = next;
                break;
            }
            if next == PendingWaitClass::Sleeping {
                class = next;
            }
        }
        class
    } else {
        PendingWaitClass::Inert
    };
    visiting.remove(&index);
    class
}

/// Armed one-shot deliveries keyed by the session they wake. Turn-completion
/// waits load these before the message queue and the rollup, since a wake
/// publishes its message record before its catalog row is consumed.
pub(crate) struct SessionWaits(BTreeMap<(AgentKind, AgentSessionId), Vec<PendingWait>>);

impl SessionWaits {
    /// Read the catalog now. The machine config is loaded only for a
    /// workspace that holds instance rows.
    pub(crate) fn load(paths: &crate::StatePaths) -> Self {
        // An unreadable record leaves no root, as snapshot assembly does.
        let project_root = crate::workspace::record::read_optional(&paths.workspace_record)
            .ok()
            .flatten()
            .map(|record| record.project_root);
        Self::load_at(project_root.as_deref(), || {
            jiff::Zoned::now().with_time_zone(MachineConfig::load_lenient().time_zone())
        })
    }

    pub(crate) fn contains(&self, kind: &AgentKind, session: &AgentSessionId) -> bool {
        self.0.contains_key(&(kind.clone(), session.clone()))
    }

    fn load_at(project_root: Option<&Path>, now: impl FnOnce() -> jiff::Zoned) -> Self {
        let Some(root) = project_root else {
            return Self(BTreeMap::new());
        };
        let Ok(paths) = crate::disk::paths::StatePaths::for_project_root(root) else {
            return Self(BTreeMap::new());
        };
        if super::instances::load_from(&paths.root).0.is_empty() {
            return Self(BTreeMap::new());
        }
        Self(pending_waits_by_session(
            &TaskCatalog::load_lenient(Some(root)),
            root,
            &now(),
        ))
    }

    pub(crate) fn attach(mut self, snapshot: &mut SidebarSnapshot) {
        for agent in Arc::make_mut(&mut snapshot.agents) {
            agent.pending_waits = self
                .0
                .remove(&(agent.kind.clone(), agent.agent_id.clone()))
                .unwrap_or_default();
        }
    }
}

#[cfg(test)]
mod tests;
