//! Shared settlement rules for a launcher's fleet runs.

use std::collections::HashSet;

use crate::agents::AgentState;
use crate::store::run::RunRecord;

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

/// Newest non-peer run and all open or unclaimed peer turns per member, deduplicated by run id and ordered as `address::launched_fleet` orders its members.
pub struct FleetRuns<'a>(Vec<(&'a AgentState, &'a RunRecord)>);

impl<'a> FleetRuns<'a> {
    pub fn of(agents: &'a [AgentState], runs: &'a [RunRecord], launcher: &AgentState) -> Self {
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
                .filter(|(_, run)| seen.insert(&run.run_id))
                .collect(),
        )
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn any_running(&self) -> bool {
        self.0.iter().any(|(_, run)| !run.status.is_terminal())
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
                .unreported()
                .is_empty()
        );
        assert!(FleetRuns::of(&agents, &[], &parent).is_empty());
        assert!(has_members(&agents, &parent), "runs are not membership");
        assert!(!has_members(&agents, &children[0]), "a child launched none");
    }
}
