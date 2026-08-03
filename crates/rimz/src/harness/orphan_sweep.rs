//! Producer-side backstop for subagents whose parent watchdog failed.
//!
//! The producer only reads durable records and starts a short-lived hidden
//! helper. That helper re-verifies the orphan and owns the diagnostic write
//! plus pane reclamation.

use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::agents::AgentState;
use crate::harness::run::RunRecord;
use crate::ids::{AgentKind, AgentSessionId, WorkspaceId};
use crate::{RuntimePaths, StatePaths};

const ORPHAN_GRACE: Duration = Duration::from_secs(10 * 60);
#[cfg(any(test, feature = "testkit"))]
const TEST_GRACE_MS_ENV: &str = "RIMZ_TEST_SUBAGENT_ORPHAN_GRACE_MS";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanSubagentRequest {
    pub workspace_id: WorkspaceId,
    pub child_kind: AgentKind,
    pub child_agent_id: AgentSessionId,
    pub parent_agent_id: AgentSessionId,
}

#[derive(Clone, Debug)]
pub struct OrphanedSubagent {
    pub child: AgentState,
    pub run: Option<RunRecord>,
    pub orphaned_at: Timestamp,
}

#[derive(Debug, thiserror::Error)]
pub enum OrphanSweepErr {
    #[error(transparent)]
    Snapshot(#[from] crate::store::snapshot::SnapshotErr),
    #[error(transparent)]
    RunStore(#[from] crate::harness::run::RunStoreErr),
}

/// Detect durable parent orphans and delegate each repair to a hidden helper.
pub fn enforce(paths: &StatePaths, runtime: &RuntimePaths, runs: &[RunRecord], now: Timestamp) {
    let orphans = match find_with_runs(paths, runs, now) {
        Ok(orphans) => orphans,
        Err(err) => {
            tracing::debug!(
                workspace = %runtime.workspace_id,
                error = &err as &dyn std::error::Error,
                "sidebar: failed to scan for orphaned subagents",
            );
            return;
        }
    };
    for orphan in orphans {
        spawn_helper(runtime, &orphan);
    }
}

/// Re-read one producer request immediately before the helper repairs it.
pub fn resolve(
    paths: &StatePaths,
    request: &OrphanSubagentRequest,
    now: Timestamp,
) -> Result<Option<OrphanedSubagent>, OrphanSweepErr> {
    let runs = crate::harness::run::list(paths)?;
    Ok(find_with_runs(paths, &runs, now)?
        .into_iter()
        .find(|orphan| {
            orphan.child.kind == request.child_kind
                && orphan.child.agent_id == request.child_agent_id
                && orphan.child.parent_agent_id.as_ref() == Some(&request.parent_agent_id)
        }))
}

fn find_with_runs(
    paths: &StatePaths,
    runs: &[RunRecord],
    now: Timestamp,
) -> Result<Vec<OrphanedSubagent>, OrphanSweepErr> {
    let agents = crate::store::runtime::audit_projection(paths)?.agents;
    Ok(agents
        .iter()
        .filter_map(|child| orphaned_child(child, &agents, runs, now))
        .collect())
}

fn orphaned_child(
    child: &AgentState,
    agents: &[AgentState],
    runs: &[RunRecord],
    now: Timestamp,
) -> Option<OrphanedSubagent> {
    if child.ended_at.is_some() || !child.is_launched_child() {
        return None;
    }
    let parent_id = child.parent_agent_id.as_ref()?;
    let run = newest_run(child, runs);
    if run.is_some_and(|run| run.keep) {
        return None;
    }
    let parent = agents.iter().find(|agent| {
        (agent.agent_id == *parent_id || agent.launch_id.as_ref() == Some(parent_id))
            && child
                .parent_agent_kind
                .as_ref()
                .is_none_or(|kind| agent.kind == *kind)
    });
    let orphaned_at = match parent {
        Some(parent) => parent.ended_at?,
        None => child
            .registered_at
            .or_else(|| run.map(|run| run.started_at))?,
    };
    if orphaned_at + orphan_grace() > now {
        return None;
    }
    Some(OrphanedSubagent {
        child: child.clone(),
        run: run.cloned(),
        orphaned_at,
    })
}

fn newest_run<'a>(child: &AgentState, runs: &'a [RunRecord]) -> Option<&'a RunRecord> {
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

fn spawn_helper(runtime: &RuntimePaths, orphan: &OrphanedSubagent) {
    let request = OrphanSubagentRequest {
        workspace_id: runtime.workspace_id.clone(),
        child_kind: orphan.child.kind.clone(),
        child_agent_id: orphan.child.agent_id.clone(),
        parent_agent_id: orphan
            .child
            .parent_agent_id
            .clone()
            .expect("launched child has a parent id"),
    };
    let args = crate::child_process::agent_helper_argv("orphan-subagent", &request);
    if let Err(err) =
        crate::child_process::spawn_detached_rimz(runtime, args, "orphan-subagent-repair")
    {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            child = %orphan.child.agent_id,
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn orphaned subagent repair helper",
        );
    }
}

fn orphan_grace() -> Duration {
    #[cfg(any(test, feature = "testkit"))]
    if let Some(ms) = std::env::var(TEST_GRACE_MS_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
    {
        return Duration::from_millis(ms);
    }
    ORPHAN_GRACE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentStatus;
    use crate::harness::run::PermissionMode;
    use std::path::{Path, PathBuf};

    fn run(name: &str, at: Timestamp) -> RunRecord {
        let mut run = RunRecord::new(
            WorkspaceId::from_project_root(Path::new("/tmp/orphan-sweep")),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "work".to_owned(),
            PathBuf::from("/tmp/orphan-sweep"),
        );
        run.agent_name = Some(name.to_owned());
        run.started_at = at;
        run
    }

    #[test]
    fn only_old_unkept_children_of_ended_or_missing_parents_are_orphans() {
        let now = Timestamp::from_second(1_000).unwrap();
        let old = now - Duration::from_secs(601);
        let mut parent = crate::testkit::agent_state("codex", "parent", old);
        parent.ended_at = Some(old);
        let mut child = crate::testkit::agent_state("codex", "child", old);
        child.name = Some("child".to_owned());
        child.status = AgentStatus::Idle;
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.parent_agent_kind = Some(parent.kind.clone());
        child.launch_depth = Some(1);
        child.registered_at = Some(old);
        let mut child_run = run("child", old);

        assert!(
            orphaned_child(
                &child,
                &[parent.clone(), child.clone()],
                &[child_run.clone()],
                now
            )
            .is_some()
        );

        child_run.keep = true;
        assert!(
            orphaned_child(
                &child,
                &[parent.clone(), child.clone()],
                &[child_run.clone()],
                now
            )
            .is_none()
        );

        child_run.keep = false;
        parent.ended_at = None;
        assert!(
            orphaned_child(&child, &[parent, child.clone()], &[child_run.clone()], now).is_none()
        );
        assert!(orphaned_child(&child, &[child.clone()], &[child_run], now).is_some());

        child.registered_at = Some(now);
        assert!(orphaned_child(&child, &[child.clone()], &[], now).is_none());
    }

    #[test]
    fn adopted_live_parent_still_matches_the_childs_launch_id() {
        let now = Timestamp::from_second(1_000).unwrap();
        let old = now - Duration::from_secs(601);
        let mut parent = crate::testkit::agent_state("codex", "parent-session", old);
        parent.launch_id = Some(AgentSessionId::from("launch-parent"));
        let mut child = crate::testkit::agent_state("codex", "child-session", old);
        child.parent_agent_id = Some(AgentSessionId::from("launch-parent"));
        child.parent_agent_kind = Some(parent.kind.clone());
        child.launch_depth = Some(1);
        child.registered_at = Some(old);

        assert!(orphaned_child(&child, &[parent, child.clone()], &[], now).is_none());
    }
}
