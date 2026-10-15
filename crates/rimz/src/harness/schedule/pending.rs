//! Read-only pending wakes: catalog deliveries and unsettled launched runs.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::agents::{PendingWait, PendingWaitTrigger};
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
    Some(PendingWait {
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
    let projected: Vec<Vec<PendingWait>> = (0..snapshot.agents.len())
        .map(|index| {
            let mut waits: Vec<_> = fleets
                .of(index)
                .unsettled()
                .into_iter()
                .map(|(child, run)| PendingWait {
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
                })
                .collect();
            for run in fleets.owed_team_runs(index) {
                let Some(team) = &run.team else { continue };
                let stage = crate::harness::scratch::board_stage(&run.worktree_path)
                    .map(|stage| stage.name);
                if !fleet::team_stage_pending(stage.as_deref()) {
                    continue;
                }
                waits.push(PendingWait {
                    name: team.instance.clone(),
                    armed_at: Some(run.started_at),
                    trigger: PendingWaitTrigger::Team { stage },
                });
            }
            waits
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
mod tests {
    use super::*;
    use crate::config::TaskEntry;

    #[test]
    fn runs_are_read_only_when_an_agent_launched_children_or_a_team() {
        use crate::agents::{AgentState, AgentStatus, LaunchedBy};
        let parent = AgentState::stub("claude", "parent", AgentStatus::Idle);
        let mut peer = AgentState::stub("claude", "peer", AgentStatus::Idle);
        assert!(!has_launched_work(&[parent.clone(), peer.clone()]));

        let mut child = AgentState::stub("claude", "child", AgentStatus::Idle);
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.parent_agent_kind = Some(parent.kind.clone());
        child.launch_depth = Some(1);
        assert!(has_launched_work(&[parent.clone(), child]));

        peer.team = Some("forge".into());
        assert!(
            !has_launched_work(&[parent.clone(), peer.clone()]),
            "a team seat nobody launched is not launched work"
        );
        peer.launched_by = Some(LaunchedBy {
            kind: parent.kind.clone(),
            agent_id: parent.agent_id.clone(),
        });
        assert!(has_launched_work(&[parent, peer]));
    }

    #[test]
    fn run_waits_keep_launcher_member_and_turn_order() {
        use crate::agents::{AgentState, AgentStatus, LaunchedBy, PermissionMode};
        use crate::store::run::{self, PeerRun, ReportTo, RunRecord, RunStatus, TeamRun};
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::ids::WorkspaceId::from_project_root(dir.path()),
            dir.path(),
        )
        .unwrap();
        let now = jiff::Timestamp::from_second(4_000).unwrap();
        let mut first = AgentState::stub("claude", "first", AgentStatus::Idle);
        first.launch_id = Some("first-launch".into());
        let second = AgentState::stub("codex", "second", AgentStatus::Idle);
        let mut children: Vec<_> = ["plain", "peer", "other", "leader"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                let launcher = if index >= 2 { &second } else { &first };
                let mut child = AgentState::stub(launcher.kind.as_str(), name, AgentStatus::Idle);
                child.name = Some(name.into());
                child.registered_at = Some(jiff::Timestamp::from_second(index as i64).unwrap());
                child.launched_by = Some(LaunchedBy {
                    kind: launcher.kind.clone(),
                    agent_id: launcher
                        .launch_id
                        .as_ref()
                        .unwrap_or(&launcher.agent_id)
                        .clone(),
                });
                child
            })
            .collect();
        children[1].launch_id = Some("peer-launch".into());
        children[3].launch_id = Some("leader-launch".into());
        children[3].team = Some("forge".into());
        let make_run = |child: &AgentState, started| {
            let mut record = RunRecord::new(
                paths.workspace_id.clone(),
                child.kind.clone(),
                PermissionMode::Auto,
                "task".into(),
                dir.path().into(),
            );
            record.agent_id = Some(child.agent_id.clone());
            record.started_at = jiff::Timestamp::from_second(started).unwrap();
            record.updated_at = record.started_at;
            record
        };
        let mut runs = vec![
            make_run(&children[0], 1),
            make_run(&children[0], 2),
            make_run(&children[1], 3),
            make_run(&children[1], 4),
            make_run(&children[2], 5),
            make_run(&children[3], 6),
        ];
        runs[0].status = RunStatus::Completed;
        runs[1].deadline_at = Some(now);
        runs[2].peer = Some(PeerRun {
            launch_id: "peer-launch".into(),
            opened_by: Vec::new(),
        });
        runs[2].agent_id = None;
        runs[3].peer = runs[2].peer.clone();
        runs[3].status = RunStatus::Completed;
        runs[4].agent_id = None;
        runs[4].agent_name = children[2].name.clone();
        runs[5].team = Some(TeamRun {
            launch_id: "leader-launch".into(),
            instance: "forge#work".into(),
        });
        let mut detached = make_run(&children[1], 7);
        detached.peer = runs[2].peer.clone();
        detached.report_to = ReportTo::Nobody;
        runs.push(detached);
        for run in &runs {
            run::write(&paths.runs_dir, run).unwrap();
        }
        let mut agents = vec![first, second];
        agents.extend(children);
        let mut snapshot =
            SidebarSnapshot::build_with_agents(paths.workspace_id.clone(), agents, now);
        project_run_waits(&mut snapshot, &paths);
        let first = &snapshot.agents[0].pending_waits;
        assert_eq!(
            first
                .iter()
                .map(|wait| wait.name.as_str())
                .collect::<Vec<_>>(),
            ["@plain", "@peer", "@peer"]
        );
        assert_eq!(
            first
                .iter()
                .map(|wait| wait.armed_at.unwrap().as_second())
                .collect::<Vec<_>>(),
            [2, 4, 3]
        );
        let second = &snapshot.agents[1].pending_waits;
        assert_eq!(
            second
                .iter()
                .map(|wait| wait.name.as_str())
                .collect::<Vec<_>>(),
            ["@other", "forge#work"]
        );
        assert!(matches!(second[1].trigger, PendingWaitTrigger::Team { .. }));
        assert!(
            snapshot.agents[2..]
                .iter()
                .all(|agent| agent.pending_waits.is_empty())
        );
    }

    #[test]
    fn launched_run_waits_precede_catalog_and_disappear_after_settlement() {
        use crate::agents::{AgentState, AgentStatus, PermissionMode};
        use crate::store::run::{self, RunRecord, RunStatus, TeamRun};
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::under(
            crate::ids::WorkspaceId::from_project_root(dir.path()),
            dir.path(),
        )
        .unwrap();
        let now = jiff::Timestamp::from_second(4_000).unwrap();
        let parent = AgentState::stub("claude", "parent", AgentStatus::Idle);
        let children = ["calm-fox", "bright-owl"].map(|name| {
            let mut child = AgentState::stub("claude", name, AgentStatus::Idle);
            child.name = Some(name.into());
            child.parent_agent_id = Some(parent.agent_id.clone());
            child.parent_agent_kind = Some(parent.kind.clone());
            child.launch_depth = Some(1);
            child.last_activity = now;
            child
        });
        let mut leader = AgentState::stub("claude", "leader", AgentStatus::Idle);
        leader.team = Some("forge".into());
        leader.launch_id = Some("leader-launch".into());
        leader.launched_by = Some(crate::agents::LaunchedBy {
            kind: parent.kind.clone(),
            agent_id: parent.agent_id.clone(),
        });
        let mut runs: Vec<_> = children
            .iter()
            .chain([&leader])
            .map(|child| {
                let mut run = RunRecord::new(
                    paths.workspace_id.clone(),
                    child.kind.clone(),
                    PermissionMode::Auto,
                    "work".into(),
                    dir.path().into(),
                );
                run.agent_id = Some(child.agent_id.clone());
                run.started_at = jiff::Timestamp::UNIX_EPOCH;
                run.deadline_at = Some(now);
                run
            })
            .collect();
        runs[1].status = RunStatus::Completed;
        runs[2].team = Some(TeamRun {
            launch_id: "leader-launch".into(),
            instance: "forge#feat-x".into(),
        });
        for run in &runs {
            run::write(&paths.runs_dir, run).unwrap();
        }
        let agents = [vec![parent], children.to_vec(), vec![leader]].concat();
        let mut snapshot =
            SidebarSnapshot::build_with_agents(paths.workspace_id.clone(), agents, now);
        project_pending_waits(&mut snapshot, &paths, None, &MachineConfig::default());
        assert_eq!(
            snapshot.agents[0].pending_waits.len(),
            3,
            "two fleet rows and one team run"
        );
        assert_eq!(snapshot.agents[0].effective_status(), AgentStatus::Sleeping);
        for wait in &snapshot.agents[0].pending_waits[..2] {
            assert_eq!(wait.armed_at, Some(jiff::Timestamp::UNIX_EPOCH));
            let PendingWaitTrigger::Subagent {
                active_at,
                deadline_at,
                settled,
            } = &wait.trigger
            else {
                panic!("subagent comes before team")
            };
            assert_eq!(*active_at, now);
            assert_eq!(*deadline_at, Some(now));
            assert_eq!(
                settled.as_deref(),
                (wait.name == "@bright-owl").then_some("completed")
            );
            assert!(["@calm-fox", "@bright-owl"].contains(&wait.name.as_str()));
        }
        assert_eq!(
            snapshot.agents[0].pending_waits[2].trigger,
            PendingWaitTrigger::Team { stage: None }
        );
        std::fs::write(
            dir.path().join("blackboard.md"),
            "Stage: Review (@reviewer)\n",
        )
        .unwrap();
        let catalog_wait = PendingWait {
            name: "ci".into(),
            trigger: PendingWaitTrigger::Signal {
                selector: "pr.checks".into(),
            },
            armed_at: None,
        };
        Arc::make_mut(&mut snapshot.agents)[0].pending_waits = vec![catalog_wait.clone()];
        project_run_waits(&mut snapshot, &paths);
        assert_eq!(
            snapshot.agents[0].pending_waits[2].trigger,
            PendingWaitTrigger::Team {
                stage: Some("Review".into())
            }
        );
        assert_eq!(snapshot.agents[0].pending_waits[3], catalog_wait);
        std::fs::write(dir.path().join("blackboard.md"), "Stage: Done\n").unwrap();
        Arc::make_mut(&mut snapshot.agents)[0].pending_waits.clear();
        project_run_waits(&mut snapshot, &paths);
        assert!(
            snapshot.agents[0]
                .pending_waits
                .iter()
                .all(|wait| !matches!(wait.trigger, PendingWaitTrigger::Team { .. })),
            "a board at Done projects no team wait"
        );
        std::fs::remove_file(dir.path().join("blackboard.md")).unwrap();
        for run in &mut runs {
            run.report_to = run::ReportTo::Nobody;
            run::write(&paths.runs_dir, run).unwrap();
        }
        Arc::make_mut(&mut snapshot.agents)[0].pending_waits.clear();
        project_run_waits(&mut snapshot, &paths);
        assert_eq!(
            snapshot.agents[0].pending_waits,
            Vec::new(),
            "detached launches leave their launcher waiting on nothing"
        );
        for run in &mut runs {
            run.report_to = run::ReportTo::Launcher;
        }
        for (index, run) in runs.iter_mut().enumerate() {
            run.status = RunStatus::Completed;
            if index == 0 {
                run.joined_at = Some(now);
            } else {
                run.report_message_id = Some(crate::MessageId::new());
            }
            run::write(&paths.runs_dir, run).unwrap();
        }
        project_pending_waits(&mut snapshot, &paths, None, &MachineConfig::default());
        assert!(snapshot.agents[0].pending_waits.is_empty());
        assert_ne!(snapshot.agents[0].effective_status(), AgentStatus::Sleeping);
    }

    #[test]
    fn pending_wait_requires_an_ephemeral_valid_trigger_and_one_shot_clock() {
        let deadline = "2026-06-01T12:00:00Z".parse().unwrap();
        let now = "2026-06-01T10:00:00Z[UTC]".parse().unwrap();
        for entry in [
            TaskEntry {
                signal: Some("pr.merged".into()),
                ..TaskEntry::default()
            },
            TaskEntry {
                watch: Some(crate::config::WatchSpec::Command(String::new())),
                ..TaskEntry::default()
            },
            TaskEntry {
                every: Some("1h".into()),
                deadline: Some(deadline),
                ..TaskEntry::default()
            },
        ] {
            let task = LoadedTask::new("wait", entry, TaskSource::Instance);
            assert!(pending_wait("wait", &task, &now).is_none());
        }
        let task = LoadedTask::new(
            "signal",
            TaskEntry {
                signal: Some("pr.merged".into()),
                once: Some(true),
                ..TaskEntry::default()
            },
            TaskSource::Instance,
        );
        assert_eq!(
            pending_wait("signal", &task, &now).unwrap().trigger,
            PendingWaitTrigger::Signal {
                selector: "pr.merged".into(),
            }
        );
    }

    #[test]
    fn pending_waits_preserve_pid_and_delay() {
        let now = "2026-06-01T10:00:00Z[UTC]".parse().unwrap();
        let armed_at = "2026-06-01T09:42:00Z".parse().unwrap();
        let meta = crate::config::WaitMeta {
            armed_at,
            delay: Some("30m".into()),
            reader: None,
        };
        let timer = LoadedTask::new(
            "timer",
            TaskEntry {
                at: Some("10:12".into()),
                wait_meta: Some(meta.clone()),
                ..TaskEntry::default()
            },
            TaskSource::Instance,
        );
        let wait = pending_wait("timer", &timer, &now).unwrap();
        assert_eq!(wait.armed_at, Some(armed_at));
        assert_eq!(
            wait.trigger,
            PendingWaitTrigger::Timer {
                due: "2026-06-01T10:12:00Z".parse().unwrap(),
                delay: Some("30m".into()),
            }
        );
        for (spec, expected) in [
            (
                WatchSpec::Command("cargo test".into()),
                PendingWaitTrigger::Command {
                    command: "cargo test".into(),
                },
            ),
            (
                WatchSpec::Pid { pid: 16776 },
                PendingWaitTrigger::Pid { pid: 16776 },
            ),
            (
                WatchSpec::Check {
                    check: "nc -z localhost 3000".into(),
                    every: "1s".into(),
                    on: crate::config::CheckOn::Success,
                },
                PendingWaitTrigger::Check {
                    command: "nc -z localhost 3000".into(),
                },
            ),
        ] {
            let task = LoadedTask::new(
                "watch",
                TaskEntry {
                    watch: Some(spec),
                    wait_meta: Some(crate::config::WaitMeta {
                        delay: None,
                        ..meta.clone()
                    }),
                    ..TaskEntry::default()
                },
                TaskSource::Instance,
            );
            assert_eq!(
                pending_wait("watch", &task, &now).unwrap().trigger,
                expected
            );
        }
    }

    #[test]
    fn clock_due_uses_arm_time_or_the_snapshot_clock() {
        let now = "2026-06-02T10:00:00Z[UTC]".parse().unwrap();
        for (armed_at, expected) in [
            (Some("2026-06-01T10:00:00Z"), "2026-06-01T12:00:00Z"),
            (None, "2026-06-02T12:00:00Z"),
        ] {
            let task = LoadedTask::new(
                "timer",
                TaskEntry {
                    at: Some("12:00".into()),
                    wait_meta: armed_at.map(|at| crate::config::WaitMeta {
                        armed_at: at.parse().unwrap(),
                        delay: None,
                        reader: None,
                    }),
                    ..TaskEntry::default()
                },
                TaskSource::Instance,
            );
            assert_eq!(
                pending_wait("timer", &task, &now).unwrap().trigger,
                PendingWaitTrigger::Timer {
                    due: expected.parse().unwrap(),
                    delay: None,
                },
            );
        }
    }

    #[test]
    fn fire_at_due_is_its_instant_even_when_armed_at_it() {
        let now = "2026-06-02T10:00:00Z[UTC]".parse().unwrap();
        let at: jiff::Timestamp = "2026-06-02T10:00:00Z".parse().unwrap();
        let task = LoadedTask::new(
            "reset",
            TaskEntry {
                fire_at: Some(at),
                wait_meta: Some(crate::config::WaitMeta {
                    armed_at: at,
                    delay: None,
                    reader: None,
                }),
                ..TaskEntry::default()
            },
            TaskSource::Instance,
        );
        assert_eq!(
            pending_wait("reset", &task, &now).unwrap().trigger,
            PendingWaitTrigger::Timer {
                due: at,
                delay: None,
            },
        );
    }
}
