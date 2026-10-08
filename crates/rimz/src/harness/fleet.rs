//! Shared settlement rules for a launcher's fleet runs.

use std::collections::HashSet;

use crate::agents::AgentState;
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
        assert_eq!(FleetRuns::of(&agents, &runs, &parent).unreported().len(), 2);
        assert_eq!(FleetRuns::of(&agents, &runs, &parent).unsettled().len(), 2);
        runs[0].joined_at = Some(Timestamp::UNIX_EPOCH);
        runs[1].report_message_id = Some(crate::MessageId::new());
        assert!(FleetRuns::of(&agents, &runs, &parent).settled().is_empty());
        runs[0].status = crate::store::run::RunStatus::Running;
        assert!(FleetRuns::of(&agents, &runs, &parent).any_running());
        value["peer"]["launch_id"] = serde_json::json!("old-launch");
        let old: RunRecord = serde_json::from_value(value).unwrap();
        assert!(newest_run(&child, std::slice::from_ref(&old)).is_none());
        assert!(FleetRuns::of(&agents, &[old], &parent).is_empty());
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
            owed_team_runs(&agents, &runs, &parent).is_empty(),
            "a detached team owes its launcher no Done"
        );
        assert_eq!(
            ended_team_runs(&agents, &runs, &parent).len(),
            1,
            "a dead detached cohort still settles its run"
        );
        runs[0].report_to = crate::store::run::ReportTo::Launcher;
        assert_eq!(owed_team_runs(&agents, &runs, &parent).len(), 1);
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
        let fleet = FleetRuns::of(&agents, &runs, &parent);
        assert!(fleet.any_running());
        assert!(fleet.unreported().is_empty());
        runs[1].status = crate::store::run::RunStatus::Completed;
        let fleet = FleetRuns::of(&agents, &runs, &parent);
        let unsettled = fleet.unsettled();
        assert_eq!(unsettled.len(), 2);
        let ordered = crate::address::launched_fleet(&agents, &parent);
        assert_eq!(unsettled[0].0.agent_id, ordered[0].agent_id);
        assert_eq!(unsettled[1].0.agent_id, ordered[1].agent_id);
        for run in &mut runs {
            run.status = crate::store::run::RunStatus::Completed;
        }
        let fleet = FleetRuns::of(&agents, &runs, &parent);
        assert!(!fleet.any_running());
        let settled = fleet.unreported();
        assert_eq!(settled.len(), 2, "the alias shares its run with `first`");
        let ordered = crate::address::launched_fleet(&agents, &parent);
        assert_eq!(settled[0].0.agent_id, ordered[0].agent_id);
        runs[0].joined_at = Some(Timestamp::UNIX_EPOCH);
        runs[1].report_message_id = Some(crate::MessageId::new());
        assert!(
            FleetRuns::of(&agents, &runs, &parent)
                .unsettled()
                .is_empty()
        );
        assert!(
            FleetRuns::of(&agents, &runs, &parent)
                .unreported()
                .is_empty()
        );
        assert!(FleetRuns::of(&agents, &[], &parent).is_empty());
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
        let fleet = FleetRuns::of(&agents, &runs, &parent);
        assert!(
            !fleet.any_running(),
            "a running detached member holds nobody"
        );
        let unreported = fleet.unreported();
        assert_eq!(unreported.len(), 1, "only the attached member reports");
        assert_eq!(unreported[0].1.run_id, runs[1].run_id);
        assert_eq!(fleet.unsettled().len(), 1);
        runs[0].status = crate::store::run::RunStatus::Completed;
        let fleet = FleetRuns::of(&agents, &runs, &parent);
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
