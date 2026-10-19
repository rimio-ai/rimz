//! Shared settlement rules for a launcher's fleet runs.

use std::collections::{HashMap, HashSet};

use crate::agents::AgentState;
use crate::ids::{AgentKind, AgentSessionId};
use crate::store::run::{ReportTo, RunRecord};

/// Newest run of one launched child.
pub(super) fn newest_run<'a>(child: &AgentState, runs: &'a [RunRecord]) -> Option<&'a RunRecord> {
    runs.iter()
        .filter(|run| run.matches_agent(child))
        .max_by_key(|run| run.started_at)
}

/// Whether `launcher` launched anyone at all, answered from the agent rows
/// alone so a caller can skip reading the runs directory.
pub(super) fn has_members(agents: &[AgentState], launcher: &AgentState) -> bool {
    !crate::address::launched_fleet(agents, launcher).is_empty()
}

/// Newest non-peer run and all open or unclaimed peer turns per member, deduplicated by run id and ordered as `address::launched_fleet` orders its members. A member whose selected run reports to nobody has no row.
pub struct FleetRuns<'a>(Vec<(&'a AgentState, &'a RunRecord)>);

type AgentKey = (AgentKind, AgentSessionId);

#[derive(Hash, PartialEq, Eq)]
enum RunMatchKey {
    Session(AgentKind, AgentSessionId),
    Launch(AgentKind, AgentSessionId),
    Name(AgentKind, String),
}

#[derive(Default)]
struct SelectedRuns<'a> {
    newest: Option<(usize, &'a RunRecord)>,
    peers: Vec<(usize, &'a RunRecord)>,
}

#[derive(Default)]
pub(super) struct LaunchRows {
    answers: HashMap<AgentKey, Vec<usize>>,
    launches: HashMap<AgentKey, Vec<usize>>,
    aliases: HashMap<AgentKey, Option<AgentSessionId>>,
}

impl LaunchRows {
    pub(super) fn new<'a>(agents: impl IntoIterator<Item = &'a AgentState>) -> Self {
        let mut rows = Self::default();
        for (index, agent) in agents
            .into_iter()
            .enumerate()
            .filter(|(_, agent)| !agent.is_provider_subagent())
        {
            rows.aliases
                .entry((agent.kind.clone(), agent.agent_id.clone()))
                .or_insert_with(|| agent.launch_id.clone());
            let ids: HashSet<_> = agent.identity_ids().collect();
            for id in ids {
                rows.answers
                    .entry((agent.kind.clone(), id.clone()))
                    .or_default()
                    .push(index);
            }
            if let Some(id) = &agent.launch_id {
                rows.launches
                    .entry((agent.kind.clone(), id.clone()))
                    .or_default()
                    .push(index);
            }
        }
        rows
    }

    pub(super) fn members(&self, kind: &AgentKind, id: &AgentSessionId) -> Vec<usize> {
        let key = (kind.clone(), id.clone());
        let mut members = self.answers.get(&key).cloned().unwrap_or_default();
        if let Some(Some(alias)) = self.aliases.get(&key)
            && let Some(siblings) = self.launches.get(&(kind.clone(), alias.clone()))
        {
            members.extend(siblings);
            members.sort_unstable();
            members.dedup();
        }
        members
    }
}

/// One projection's keyed fleet joins; member and run order match [`FleetRuns::of`].
pub(super) struct FleetRunIndex<'a> {
    agents: &'a [AgentState],
    members: Vec<Vec<usize>>,
    selected: HashMap<RunMatchKey, SelectedRuns<'a>>,
    teams: Vec<Vec<&'a RunRecord>>,
}

impl<'a> FleetRunIndex<'a> {
    pub(super) fn new(agents: &'a [AgentState], runs: &'a [RunRecord]) -> Self {
        let launch_rows = LaunchRows::new(agents);
        let mut children: HashMap<AgentKey, Vec<usize>> = HashMap::new();
        let mut launchers: HashMap<AgentKey, Vec<usize>> = HashMap::new();
        for (index, agent) in agents.iter().enumerate() {
            if !agent.is_team_seat()
                && let Some((kind, id)) = agent.launcher()
            {
                children
                    .entry((kind.clone(), id.clone()))
                    .or_default()
                    .push(index);
            }
            let ids: HashSet<_> = agent.identity_ids().collect();
            for id in ids {
                launchers
                    .entry((agent.kind.clone(), id.clone()))
                    .or_default()
                    .push(index);
            }
        }
        let mut fleets: HashMap<AgentKey, Vec<usize>> = HashMap::new();
        let members = agents
            .iter()
            .map(|launcher| {
                let key = (
                    launcher.kind.clone(),
                    launcher
                        .launch_id
                        .as_ref()
                        .unwrap_or(&launcher.agent_id)
                        .clone(),
                );
                let group = fleets.entry(key.clone()).or_insert_with(|| {
                    let group = launch_rows.members(&key.0, &key.1);
                    let mut keys = HashSet::new();
                    for member in group.iter().map(|index| &agents[*index]) {
                        for id in member.identity_ids() {
                            keys.insert((member.kind.clone(), id.clone()));
                        }
                    }
                    let mut members: Vec<_> = keys
                        .iter()
                        .filter_map(|key| children.get(key))
                        .flatten()
                        .copied()
                        .collect();
                    members.sort_unstable();
                    members.dedup();
                    members.sort_by_key(|index| {
                        (
                            agents[*index].registered_at,
                            agents[*index].agent_id.clone(),
                        )
                    });
                    members
                });
                let mut members = group.clone();
                if launcher.is_provider_subagent() {
                    for id in launcher.identity_ids() {
                        if let Some(direct) = children.get(&(launcher.kind.clone(), id.clone())) {
                            members.extend(direct);
                        }
                    }
                    members.sort_unstable();
                    members.dedup();
                    members.sort_by_key(|index| {
                        (
                            agents[*index].registered_at,
                            agents[*index].agent_id.clone(),
                        )
                    });
                }
                members
            })
            .collect();
        let mut selected: HashMap<RunMatchKey, SelectedRuns<'a>> = HashMap::new();
        let mut teams = vec![Vec::new(); agents.len()];
        let mut team_launchers: HashMap<AgentKey, Vec<usize>> = HashMap::new();
        for (run_index, run) in runs.iter().enumerate() {
            let mut keys = Vec::new();
            if let Some(peer) = &run.peer {
                keys.push(RunMatchKey::Launch(
                    run.kind.clone(),
                    peer.launch_id.clone(),
                ));
                if let Some(id) = &run.agent_id {
                    keys.push(RunMatchKey::Session(run.kind.clone(), id.clone()));
                }
            } else if let Some(team) = &run.team {
                keys.push(RunMatchKey::Launch(
                    run.kind.clone(),
                    team.launch_id.clone(),
                ));
            } else {
                if let Some(id) = &run.agent_id {
                    keys.push(RunMatchKey::Session(run.kind.clone(), id.clone()));
                }
                if let Some(name) = &run.agent_name {
                    keys.push(RunMatchKey::Name(run.kind.clone(), name.clone()));
                }
            }
            for key in keys {
                let selected = selected.entry(key).or_default();
                if run.peer.is_some() {
                    if !run.status.is_terminal() || run.owes_report() {
                        selected.peers.push((run_index, run));
                    }
                } else if selected
                    .newest
                    .is_none_or(|(_, previous)| previous.started_at <= run.started_at)
                {
                    selected.newest = Some((run_index, run));
                }
            }
            if !run.status.is_terminal()
                && run.report_to == ReportTo::Launcher
                && let Some(team) = &run.team
            {
                let owners = team_launchers
                    .entry((run.kind.clone(), team.launch_id.clone()))
                    .or_insert_with(|| {
                        let group = launch_rows.members(&run.kind, &team.launch_id);
                        crate::address::launch_row(
                            group.iter().map(|index| &agents[*index]),
                            &run.kind,
                            &team.launch_id,
                        )
                        .and_then(|leader| leader.launcher())
                        .and_then(|(kind, id)| launchers.get(&(kind.clone(), id.clone())))
                        .cloned()
                        .unwrap_or_default()
                    });
                for index in owners {
                    teams[*index].push(run);
                }
            }
        }
        Self {
            agents,
            members,
            selected,
            teams,
        }
    }

    pub(super) fn of(&self, launcher: usize) -> FleetRuns<'a> {
        let mut seen = HashSet::new();
        let mut seen_groups = HashSet::new();
        let mut fleet = Vec::new();
        for index in &self.members[launcher] {
            let child = &self.agents[*index];
            let mut keys = vec![
                RunMatchKey::Session(child.kind.clone(), child.agent_id.clone()),
                RunMatchKey::Launch(child.kind.clone(), child.agent_id.clone()),
            ];
            if let Some(id) = &child.launch_id
                && id != &child.agent_id
            {
                keys.push(RunMatchKey::Launch(child.kind.clone(), id.clone()));
            }
            if let Some(name) = &child.name {
                keys.push(RunMatchKey::Name(child.kind.clone(), name.clone()));
            }
            let groups: Vec<_> = keys
                .iter()
                .filter_map(|key| self.selected.get_key_value(key))
                .collect();
            let newest = groups
                .iter()
                .filter_map(|(_, selected)| selected.newest)
                .max_by_key(|(index, run)| (run.started_at, *index));
            if let Some((_, run)) = newest
                && run.report_to == ReportTo::Launcher
                && seen.insert(&run.run_id)
            {
                fleet.push((child, run));
            }
            let mut peers: Vec<_> = groups
                .into_iter()
                .filter(|(key, _)| seen_groups.insert(*key))
                .map(|(_, selected)| selected.peers.iter().peekable())
                .collect();
            while let Some(source) = peers
                .iter_mut()
                .enumerate()
                .filter_map(|(source, peers)| peers.peek().map(|(index, _)| (source, *index)))
                .min_by_key(|(_, index)| *index)
                .map(|(source, _)| source)
            {
                // The selected source was just peeked and cannot be empty.
                let (_, run) = peers[source].next().expect("peeked peer run");
                if run.report_to == ReportTo::Launcher && seen.insert(&run.run_id) {
                    fleet.push((child, *run));
                }
            }
        }
        FleetRuns(fleet)
    }

    pub(super) fn owed_team_runs(&self, launcher: usize) -> &[&'a RunRecord] {
        &self.teams[launcher]
    }
}

/// Open team runs report once at Done, separately from the per-member fleet digest.
fn open_team_runs<'a, 'agents>(
    agents: impl IntoIterator<Item = &'agents AgentState> + Clone,
    runs: &'a [RunRecord],
    launcher: &AgentState,
) -> Vec<&'a RunRecord> {
    runs.iter()
        .filter(|run| {
            !run.status.is_terminal()
                && run.team.as_ref().is_some_and(|team| {
                    crate::address::launch_row(agents.clone(), &run.kind, &team.launch_id)
                        .is_some_and(|leader| leader.launcher_is(launcher))
                })
        })
        .collect()
}

/// Open team runs whose Done the launcher is still owed; a detached team owes none.
pub(super) fn owed_team_runs<'a>(
    agents: &[AgentState],
    runs: &'a [RunRecord],
    launcher: &AgentState,
) -> Vec<&'a RunRecord> {
    let mut owed = open_team_runs(agents, runs, launcher);
    owed.retain(|run| run.report_to == ReportTo::Launcher);
    owed
}

fn team_cohort_gone<'a>(
    agents: impl IntoIterator<Item = &'a AgentState> + Clone,
    instance: &str,
) -> bool {
    crate::address::team_cohorts(agents)
        .into_iter()
        .find(|cohort| format!("{}#{}", cohort.team, cohort.channel) == instance)
        .is_none_or(|cohort| {
            cohort.members.iter().all(|member| {
                crate::store::runtime::agent_liveness(member)
                    == crate::store::runtime::AgentLiveness::Dead
            })
        })
}

/// A hand-edited Done board leaves its run open with no report coming.
pub(super) fn team_stage_pending(stage: Option<&str>) -> bool {
    stage != Some(crate::config::DONE_STAGE)
}

/// Open runs belonging to this launcher whose whole cohort has ended.
pub fn ended_team_runs<'a, 'agents>(
    agents: impl IntoIterator<Item = &'agents AgentState> + Clone,
    runs: &'a [RunRecord],
    launcher: &AgentState,
) -> Vec<&'a RunRecord> {
    open_team_runs(agents.clone(), runs, launcher)
        .into_iter()
        .filter(|run| {
            run.team
                .as_ref()
                .is_some_and(|team| team_cohort_gone(agents.clone(), &team.instance))
        })
        .collect()
}

impl<'a> FleetRuns<'a> {
    pub fn of(
        agents: impl IntoIterator<Item = &'a AgentState> + Clone,
        runs: &'a [RunRecord],
        launcher: &AgentState,
    ) -> Self {
        let mut seen = HashSet::new();
        Self(
            crate::address::launched_fleet(agents, launcher)
                .into_iter()
                .flat_map(|child| {
                    let newest = runs
                        .iter()
                        .filter(|run| run.peer.is_none() && run.matches_agent(child))
                        .max_by_key(|run| run.started_at);
                    newest
                        .into_iter()
                        .chain(runs.iter().filter(move |run| {
                            run.peer.is_some()
                                && run.matches_agent(child)
                                && (!run.status.is_terminal() || run.owes_report())
                        }))
                        .map(move |run| (child, run))
                })
                .filter(|(_, run)| run.report_to == ReportTo::Launcher && seen.insert(&run.run_id))
                .collect(),
        )
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn any_running(&self) -> bool {
        self.0.iter().any(|(_, run)| !run.status.is_terminal())
    }

    pub(super) fn unsettled(&self) -> Vec<(&'a AgentState, &'a RunRecord)> {
        self.0
            .iter()
            .copied()
            .filter(|(_, run)| !run.status.is_terminal() || run.owes_report())
            .collect()
    }

    /// Terminal rows in member order; claimed peer turns have left the fleet.
    pub fn settled(&self) -> Vec<(&'a AgentState, &'a RunRecord)> {
        self.0
            .iter()
            .copied()
            .filter(|(_, run)| run.status.is_terminal())
            .collect()
    }

    /// Settled rows no digest carried and no caller joined, in member order.
    pub fn unreported(&self) -> Vec<(&'a AgentState, &'a RunRecord)> {
        self.0
            .iter()
            .copied()
            .filter(|(_, run)| run.owes_report())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentStatus, PermissionMode};
    use crate::ids::{AgentKind, WorkspaceId};
    use jiff::Timestamp;

    fn fleet_runs<'a>(
        agents: &'a [AgentState],
        runs: &'a [RunRecord],
        launcher: &AgentState,
    ) -> FleetRuns<'a> {
        let fleet = FleetRuns::of(agents, runs, launcher);
        let Some(launcher) = agents.iter().position(|agent| agent == launcher) else {
            return fleet;
        };
        let indexed = FleetRunIndex::new(agents, runs);
        assert_eq!(
            indexed
                .of(launcher)
                .0
                .iter()
                .map(|(agent, run)| (&agent.agent_id, &run.run_id))
                .collect::<Vec<_>>(),
            fleet
                .0
                .iter()
                .map(|(agent, run)| (&agent.agent_id, &run.run_id))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            indexed
                .owed_team_runs(launcher)
                .iter()
                .map(|run| &run.run_id)
                .collect::<Vec<_>>(),
            owed_team_runs(agents, runs, &agents[launcher])
                .iter()
                .map(|run| &run.run_id)
                .collect::<Vec<_>>()
        );
        fleet
    }

    fn owed_runs<'a>(
        agents: &'a [AgentState],
        runs: &'a [RunRecord],
        launcher: &AgentState,
    ) -> Vec<&'a RunRecord> {
        fleet_runs(agents, runs, launcher);
        owed_team_runs(agents, runs, launcher)
    }

    #[test]
    fn peer_fleet_keeps_each_unclaimed_turn_and_rejects_reused_names() {
        let parent = AgentState::stub("codex", "parent", AgentStatus::Idle);
        let mut child = AgentState::stub("codex", "session", AgentStatus::Idle);
        child.name = Some("peer".into());
        child.launch_id = Some("launch".into());
        child.launched_by = Some(crate::agents::LaunchedBy {
            kind: parent.kind.clone(),
            agent_id: parent.agent_id.clone(),
        });
        let mut record = RunRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/repo")),
            child.kind.clone(),
            PermissionMode::Auto,
            "task".into(),
            "/repo".into(),
        );
        record.agent_name = child.name.clone();
        record.status = crate::store::run::RunStatus::Completed;
        let mut value = serde_json::to_value(record).unwrap();
        value["peer"] = serde_json::json!({"launch_id": "launch"});
        let first: RunRecord = serde_json::from_value(value.clone()).unwrap();
        let mut second = first.clone();
        second.run_id = crate::ids::RunId::new();
        let mut runs = vec![first, second];
        let agents = [parent.clone(), child.clone()];
        assert_eq!(fleet_runs(&agents, &runs, &parent).unreported().len(), 2);
        assert_eq!(fleet_runs(&agents, &runs, &parent).unsettled().len(), 2);
        runs[0].joined_at = Some(Timestamp::UNIX_EPOCH);
        runs[1].report_message_id = Some(crate::MessageId::new());
        assert!(fleet_runs(&agents, &runs, &parent).settled().is_empty());
        runs[0].status = crate::store::run::RunStatus::Running;
        assert!(fleet_runs(&agents, &runs, &parent).any_running());
        value["peer"]["launch_id"] = serde_json::json!("old-launch");
        let old: RunRecord = serde_json::from_value(value).unwrap();
        assert!(newest_run(&child, std::slice::from_ref(&old)).is_none());
        assert!(fleet_runs(&agents, &[old], &parent).is_empty());
        child.launch_id = None;
        child.agent_id = "launch".into();
        assert!(newest_run(&child, &runs).is_some());
        child.kind = AgentKind::new_unchecked("claude");
        assert!(newest_run(&child, &runs).is_none());
    }

    #[test]
    fn open_team_runs_belong_only_to_the_agent_launcher_until_settled() {
        let parent = AgentState::stub("codex", "parent", AgentStatus::Idle);
        let mut leader = AgentState::stub("codex", "leader", AgentStatus::Idle);
        leader.team = Some("forge".into());
        leader.channel = Some("feat-x".into());
        leader.launch_id = Some("leader-launch".into());
        leader.launched_by = Some(crate::agents::LaunchedBy {
            kind: parent.kind.clone(),
            agent_id: parent.agent_id.clone(),
        });
        let mut run = RunRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/repo")),
            leader.kind.clone(),
            PermissionMode::Auto,
            "work".into(),
            "/repo".into(),
        );
        run.team = Some(crate::store::run::TeamRun {
            launch_id: "leader-launch".into(),
            instance: "forge#feat-x".into(),
        });
        let mut runs = vec![run];
        let mut agents = vec![parent.clone(), leader];
        let open = open_team_runs(&agents, &runs, &parent);
        assert_eq!(open.len(), 1, "an open team run belongs to its launcher");
        assert_eq!(open[0].team.as_ref().unwrap().instance, "forge#feat-x");
        assert!(!team_cohort_gone(&agents, "forge#feat-x"));
        assert!(ended_team_runs(&agents, &runs, &parent).is_empty());
        agents[1].ended_at = Some(Timestamp::now());
        assert!(team_cohort_gone(&agents, "forge#feat-x"));
        assert_eq!(ended_team_runs(&agents, &runs, &parent).len(), 1);
        agents[1].ended_at = None;
        agents[1].runtime_owner = Some(crate::pane::RuntimeOwner::new(
            crate::pane::RuntimeOwnerKind::Agent,
            "dead",
            u32::MAX,
            None,
        ));
        assert!(team_cohort_gone(&agents, "forge#feat-x"));
        let mut member = agents[1].clone();
        member.agent_id = "member".into();
        member.launch_id = Some("member-launch".into());
        member.runtime_owner = None;
        agents.push(member);
        assert!(!team_cohort_gone(&agents, "forge#feat-x"));
        agents.pop();
        assert!(team_cohort_gone(&[], "forge#feat-x"));
        runs[0].report_to = crate::store::run::ReportTo::Nobody;
        assert!(
            owed_runs(&agents, &runs, &parent).is_empty(),
            "a detached team owes its launcher no Done"
        );
        assert_eq!(
            ended_team_runs(&agents, &runs, &parent).len(),
            1,
            "a dead detached cohort still settles its run"
        );
        runs[0].report_to = crate::store::run::ReportTo::Launcher;
        assert_eq!(owed_runs(&agents, &runs, &parent).len(), 1);
        runs[0].status = crate::store::run::RunStatus::Completed;
        assert!(open_team_runs(&agents, &runs, &parent).is_empty());
        assert!(ended_team_runs(&agents, &runs, &parent).is_empty());
        runs[0].status = crate::store::run::RunStatus::Running;
        agents[1].launched_by.as_mut().unwrap().agent_id = "someone-else".into();
        assert!(open_team_runs(&agents, &runs, &parent).is_empty());
        assert!(ended_team_runs(&agents, &runs, &parent).is_empty());
        agents[1].launched_by = None;
        assert!(open_team_runs(&agents, &runs, &parent).is_empty());
    }

    #[test]
    fn newest_run_requires_kind_and_positive_identity_and_uses_latest() {
        let mut child = AgentState::stub("codex", "child", AgentStatus::Idle);
        let mut run = RunRecord::new(
            WorkspaceId::from_project_root(std::path::Path::new("/repo")),
            child.kind.clone(),
            PermissionMode::Auto,
            "work".to_owned(),
            "/repo".into(),
        );
        assert!(
            newest_run(&child, &[run.clone()]).is_none(),
            "absent names cannot match"
        );
        run.agent_id = Some(child.agent_id.clone());
        run.kind = AgentKind::new_unchecked("claude");
        assert!(
            newest_run(&child, &[run.clone()]).is_none(),
            "session ids are kind-scoped"
        );
        run.kind = child.kind.clone();
        run.started_at = Timestamp::UNIX_EPOCH;
        let mut latest = run.clone();
        latest.run_id = crate::ids::RunId::new();
        latest.started_at = Timestamp::from_second(1).unwrap();
        latest.agent_id = None;
        child.name = Some("named".to_owned());
        latest.agent_name = child.name.clone();
        let runs = [latest, run];
        assert_eq!(newest_run(&child, &runs).unwrap().run_id, runs[0].run_id);
    }

    #[test]
    fn fleet_deduplicates_runs_in_member_order_and_reports_only_settled_rows() {
        let parent = AgentState::stub("codex", "parent", AgentStatus::Idle);
        let children = ["first", "second", "alias"].map(|name| {
            let mut child = AgentState::stub("codex", name, AgentStatus::Idle);
            child.parent_agent_id = Some(parent.agent_id.clone());
            child.parent_agent_kind = Some(parent.kind.clone());
            child.launch_depth = Some(1);
            child.name = Some(if name == "alias" { "first" } else { name }.to_owned());
            child
        });
        let mut runs = children[..2]
            .iter()
            .map(|child| {
                let mut run = RunRecord::new(
                    WorkspaceId::from_project_root(std::path::Path::new("/repo")),
                    child.kind.clone(),
                    PermissionMode::Auto,
                    "work".to_owned(),
                    "/repo".into(),
                );
                run.agent_name = child.name.clone();
                run
            })
            .collect::<Vec<_>>();
        let agents = [vec![parent.clone()], children.to_vec()].concat();
        let fleet = fleet_runs(&agents, &runs, &parent);
        assert!(fleet.any_running());
        assert!(fleet.unreported().is_empty());
        runs[1].status = crate::store::run::RunStatus::Completed;
        let fleet = fleet_runs(&agents, &runs, &parent);
        let unsettled = fleet.unsettled();
        assert_eq!(unsettled.len(), 2);
        let ordered = crate::address::launched_fleet(&agents, &parent);
        assert_eq!(unsettled[0].0.agent_id, ordered[0].agent_id);
        assert_eq!(unsettled[1].0.agent_id, ordered[1].agent_id);
        for run in &mut runs {
            run.status = crate::store::run::RunStatus::Completed;
        }
        let fleet = fleet_runs(&agents, &runs, &parent);
        assert!(!fleet.any_running());
        let settled = fleet.unreported();
        assert_eq!(settled.len(), 2, "the alias shares its run with `first`");
        let ordered = crate::address::launched_fleet(&agents, &parent);
        assert_eq!(settled[0].0.agent_id, ordered[0].agent_id);
        runs[0].joined_at = Some(Timestamp::UNIX_EPOCH);
        runs[1].report_message_id = Some(crate::MessageId::new());
        assert!(fleet_runs(&agents, &runs, &parent).unsettled().is_empty());
        assert!(fleet_runs(&agents, &runs, &parent).unreported().is_empty());
        assert!(fleet_runs(&agents, &[], &parent).is_empty());
        runs[0].joined_at = None;
        runs[0].report_to = crate::store::run::ReportTo::Nobody;
        runs[0].status = crate::store::run::RunStatus::Running;
        runs[1].report_message_id = None;
        let mut attached_before = runs[0].clone();
        attached_before.run_id = crate::ids::RunId::new();
        attached_before.report_to = crate::store::run::ReportTo::Launcher;
        attached_before.status = crate::store::run::RunStatus::Completed;
        attached_before.started_at = runs[0].started_at - std::time::Duration::from_secs(60);
        runs.push(attached_before);
        let fleet = fleet_runs(&agents, &runs, &parent);
        assert!(
            !fleet.any_running(),
            "a running detached member holds nobody"
        );
        let unreported = fleet.unreported();
        assert_eq!(unreported.len(), 1, "only the attached member reports");
        assert_eq!(unreported[0].1.run_id, runs[1].run_id);
        assert_eq!(fleet.unsettled().len(), 1);
        runs[0].status = crate::store::run::RunStatus::Completed;
        let fleet = fleet_runs(&agents, &runs, &parent);
        assert_eq!(
            fleet.unreported().len(),
            1,
            "settled, it still owes nothing"
        );
        assert_eq!(fleet.settled().len(), 1);
        assert!(has_members(&agents, &parent), "runs are not membership");
        assert!(!has_members(&agents, &children[0]), "a child launched none");
    }
}
