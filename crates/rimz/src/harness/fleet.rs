//! Shared settlement rules for a launcher's newest fleet runs.

use std::collections::HashSet;

use crate::agents::AgentState;
use crate::store::run::RunRecord;

/// Newest run of one launched child.
pub(super) fn newest_run<'a>(child: &AgentState, runs: &'a [RunRecord]) -> Option<&'a RunRecord> {
    runs.iter()
        .filter(|run| {
            run.kind == child.kind
                && (run.agent_id.as_ref() == Some(&child.agent_id)
                    || child
                        .name
                        .as_ref()
                        .is_some_and(|name| run.agent_name.as_ref() == Some(name)))
        })
        .max_by_key(|run| run.started_at)
}

/// Newest run per member of `launcher`'s fleet, deduplicated by run id and
/// ordered as `address::launched_fleet` orders its members.
pub struct FleetRuns<'a>(Vec<(&'a AgentState, &'a RunRecord)>);

impl<'a> FleetRuns<'a> {
    pub fn of(agents: &'a [AgentState], runs: &'a [RunRecord], launcher: &AgentState) -> Self {
        let mut seen = HashSet::new();
        Self(
            crate::address::launched_fleet(agents, launcher)
                .into_iter()
                .filter_map(|child| {
                    let run = newest_run(child, runs)?;
                    seen.insert(&run.run_id).then_some((child, run))
                })
                .collect(),
        )
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn any_running(&self) -> bool {
        self.0.iter().any(|(_, run)| !run.status.is_terminal())
    }

    /// Settled rows no digest carried and no caller joined, in member order.
    pub fn unreported(&self) -> Vec<(&'a AgentState, &'a RunRecord)> {
        self.0
            .iter()
            .copied()
            .filter(|(_, run)| {
                run.status.is_terminal()
                    && run.report_message_id.is_none()
                    && run.joined_at.is_none()
            })
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
    }
}
