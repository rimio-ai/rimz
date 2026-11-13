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

/// Whether a wait pinned to `kind`/`session` is for `agent`. A target armed
/// since targets hold launch ids names the agent's launch, which every session
/// of the agent carries whatever its kind; an older target names one session.
pub fn wait_for(kind: &AgentKind, session: &AgentSessionId, agent: &AgentState) -> bool {
    agent.launch_id.as_ref() == Some(session) || (&agent.kind == kind && &agent.agent_id == session)
}

/// Whether two wait targets name the same agent, read over `rows` (pass Audit
/// rows) as [`holder_key`] keys them: one launch whichever kind armed each, an
/// older session target and the launch its session belongs to, and otherwise
/// one session of one kind.
pub fn same_target(
    rows: &[&AgentState],
    a: &crate::config::TaskTarget,
    b: &crate::config::TaskTarget,
) -> bool {
    holder_key(rows.iter().copied(), &a.kind, &a.session)
        == holder_key(rows.iter().copied(), &b.kind, &b.session)
}

/// The key a wait pinned to `kind`/`session` files under now: its holder's
/// kind and launch key, read over `agents` (pass Audit rows), so a launch
/// target and an older target holding a session of the same launch share one
/// key whichever kind armed each. A target no row answers to keeps its own.
pub fn holder_key<'a>(
    agents: impl IntoIterator<Item = &'a AgentState> + Clone,
    kind: &AgentKind,
    session: &AgentSessionId,
) -> (AgentKind, AgentSessionId) {
    session_holder(agents, kind, session).map_or_else(
        || (kind.clone(), session.clone()),
        |holder| (holder.kind.clone(), holder.launch_key().clone()),
    )
}

/// The current row of the agent `target` names, read over `agents` by launch
/// membership, so a session a swap replaced resolves to its holder. Pass Audit
/// rows. A target holding a session id follows only a swap within its kind.
pub fn target_holder<'a>(
    agents: impl IntoIterator<Item = &'a AgentState> + Clone,
    target: &crate::config::TaskTarget,
) -> Option<&'a AgentState> {
    session_holder(agents, &target.kind, &target.session)
}

fn session_holder<'a>(
    agents: impl IntoIterator<Item = &'a AgentState> + Clone,
    kind: &AgentKind,
    session: &AgentSessionId,
) -> Option<&'a AgentState> {
    crate::address::launch_row(agents, kind, session)
        .filter(|row| &row.kind == kind || row.launch_id.as_ref() == Some(session))
}

/// Whether `target` is a wait of `agent`, among `rows` (pass Audit rows): by
/// its launch, by its own session, or, for a target an older binary armed with
/// a session id, by that session's holder. Where `rows` name a holder, the
/// wait is the holder's alone.
pub fn wait_of(
    rows: &[&AgentState],
    target: &crate::config::TaskTarget,
    agent: &AgentState,
) -> bool {
    match target_holder(rows.iter().copied(), target) {
        Some(holder) => holder.kind == agent.kind && holder.agent_id == agent.agent_id,
        None => wait_for(&target.kind, &target.session, agent),
    }
}

/// The instance deliveries of `root`, or those for one agent: `agent` is a
/// row of it, matched through `rows` as [`wait_of`] reads them.
pub fn session_deliveries<'a>(
    catalog: &'a TaskCatalog,
    root: &Path,
    agent: Option<&AgentState>,
    rows: &[&AgentState],
) -> impl Iterator<Item = (&'a str, &'a LoadedTask)> {
    catalog.visible().iter().filter_map(move |(name, task)| {
        if task.source() != TaskSource::Instance || task.entry().resolved_root() != root {
            return None;
        }
        let target = task.entry().wait.as_ref()?;
        if agent.is_some_and(|agent| !wait_of(rows, target, agent)) {
            return None;
        }
        Some((name.as_str(), task))
    })
}

/// Take the waits `waits` files for `agent`, under its session or its launch.
pub fn take_waits(
    waits: &mut BTreeMap<(AgentKind, AgentSessionId), Vec<PendingWait>>,
    agent: &AgentState,
) -> Vec<PendingWait> {
    let keys = waits
        .keys()
        .filter(|(kind, session)| wait_for(kind, session, agent))
        .cloned()
        .collect::<Vec<_>>();
    let mut taken = keys
        .iter()
        .filter_map(|key| waits.remove(key))
        .flatten()
        .collect::<Vec<_>>();
    if keys.len() > 1 {
        sort_waits(&mut taken);
    }
    taken
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
    for (name, task) in session_deliveries(catalog, root, None, &[]) {
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
        sort_waits(waits);
    }
    waits
}

fn sort_waits(waits: &mut [PendingWait]) {
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
    // A current launch id matches across kinds, as `AgentState::launcher_is` does.
    let launches: std::collections::HashSet<_> = agents
        .iter()
        .filter_map(|agent| agent.launch_id.as_ref())
        .collect();
    agents.iter().any(|agent| {
        (!agent.is_team_seat()
            && agent
                .launcher()
                .is_some_and(|key| identities.contains(&key) || launches.contains(key.1)))
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
                        name: team.instance.to_string(),
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
    let status = agent.rowless_status(&Default::default(), now).0;
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

/// Armed one-shot deliveries keyed by the target they wake. Turn-completion
/// waits load these before the message queue and the rollup, since a wake
/// publishes its message record before its catalog row is consumed. Readers
/// match them only after [`Self::following`] files each under its holder.
pub(crate) struct SessionWaits {
    waits: BTreeMap<(AgentKind, AgentSessionId), Vec<PendingWait>>,
    /// The room's state, read for Audit rows when the reader's rows cannot
    /// place a wait.
    paths: Option<crate::StatePaths>,
}

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

    /// Whether a wait is armed for the agent whose session `kind`/`session`
    /// names among `agents`.
    pub(crate) fn contains_session<'a>(
        &self,
        agents: impl IntoIterator<Item = &'a AgentState>,
        kind: &AgentKind,
        session: &AgentSessionId,
    ) -> bool {
        self.waits.contains_key(&(kind.clone(), session.clone()))
            || agents
                .into_iter()
                .find(|agent| &agent.kind == kind && &agent.agent_id == session)
                .is_some_and(|agent| self.contains(agent))
    }

    pub(crate) fn contains(&self, agent: &AgentState) -> bool {
        self.waits
            .keys()
            .any(|(kind, session)| wait_for(kind, session, agent))
    }

    /// File each wait under its holder's launch, read over `rows`, so a wait an
    /// older binary armed for a session a swap within its kind replaced belongs
    /// to the holder. Where `rows` place no row under a wait that holds a
    /// session id (a Runtime projection drops the replaced row), the room's
    /// Audit rows place it instead.
    pub(crate) fn following(mut self, rows: &[&AgentState]) -> Self {
        self.follow_holders(rows.iter().copied());
        if self.has_stray_session(rows)
            && let Some(paths) = &self.paths
        {
            match crate::store::runtime::audit_rows(paths) {
                Ok(audit) => self.follow_holders(audit.iter()),
                Err(error) => tracing::debug!(%error, "failed to read rows for older waits"),
            }
        }
        self
    }

    fn follow_holders<'a>(&mut self, rows: impl IntoIterator<Item = &'a AgentState> + Clone) {
        for (kind, session) in self.waits.keys().cloned().collect::<Vec<_>>() {
            let key = holder_key(rows.clone(), &kind, &session);
            if key == (kind.clone(), session.clone()) {
                continue;
            }
            let Some(mut moved) = self.waits.remove(&(kind, session)) else {
                continue;
            };
            let waits = self.waits.entry(key).or_default();
            waits.append(&mut moved);
            sort_waits(waits);
        }
    }

    /// Whether a wait holds a session id (not a launch id) no row of `rows`
    /// answers to.
    fn has_stray_session(&self, rows: &[&AgentState]) -> bool {
        self.waits.keys().any(|(kind, session)| {
            !session.is_provisional() && !rows.iter().any(|row| wait_for(kind, session, row))
        })
    }

    fn load_at(project_root: Option<&Path>, now: impl FnOnce() -> jiff::Zoned) -> Self {
        let empty = Self {
            waits: BTreeMap::new(),
            paths: None,
        };
        let Some(root) = project_root else {
            return empty;
        };
        let Ok(paths) = crate::disk::paths::StatePaths::for_project_root(root) else {
            return empty;
        };
        if super::instances::load_from(&paths.root).0.is_empty() {
            return empty;
        }
        Self {
            waits: pending_waits_by_session(&TaskCatalog::load_lenient(Some(root)), root, &now()),
            paths: Some(paths),
        }
    }

    pub(crate) fn attach(self, snapshot: &mut SidebarSnapshot) {
        let mut waits = self
            .following(&snapshot.agents.iter().collect::<Vec<_>>())
            .waits;
        for agent in Arc::make_mut(&mut snapshot.agents) {
            agent.pending_waits = take_waits(&mut waits, agent);
        }
    }
}

#[cfg(test)]
mod tests;
