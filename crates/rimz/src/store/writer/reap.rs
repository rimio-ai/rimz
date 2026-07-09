use std::time::{Duration, SystemTime};

use tracing::warn;

use crate::agents::{AgentLifecycleObservation, LifecycleSignal};
use crate::store::event::EventEnvelope;
use crate::store::runtime::{self, AgentLiveness, RuntimeScope};

use super::super::{Result, StatePaths, Store, workspace_record};
use super::PublishPolicy;

const REAP_INTERVAL: Duration = Duration::from_secs(60);

fn dead_reap_stamp(paths: &StatePaths) -> std::path::PathBuf {
    paths.locks_dir.join("dead-reap.stamp")
}

fn write_reap_stamp(paths: &StatePaths) {
    let _ = std::fs::write(dead_reap_stamp(paths), b"");
}

pub(super) fn reap_due(paths: &StatePaths) -> bool {
    let Some(age) = std::fs::metadata(dead_reap_stamp(paths))
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
    else {
        return true;
    };
    age >= REAP_INTERVAL
}

fn reap_session_name(paths: &StatePaths) -> String {
    workspace_record::read(&paths.workspace_record)
        .map(|record| record.session_name)
        .unwrap_or_else(|_| "rimz-reap".to_owned())
}

impl Store {
    pub(crate) fn reap_dead_agents(&self) -> Result<usize> {
        // Lock-free by design: a same-id resume between this scan and the
        // tombstone append can be hidden briefly, and the session's next
        // lifecycle event re-inserts it because tombstones only suppress older
        // rollup state.
        let projection = self.runtime_projection(RuntimeScope::Audit)?;
        let dead = projection
            .agents
            .into_iter()
            .filter(|agent| agent.parent_agent_id.is_none())
            .filter(|agent| runtime::agent_liveness(agent) == AgentLiveness::Dead)
            .collect::<Vec<_>>();
        if dead.is_empty() {
            return Ok(0);
        }

        let session_name = reap_session_name(&self.inner.paths);
        self.commit(PublishPolicy::Debounced, |txn| {
            for agent in &dead {
                let observation = AgentLifecycleObservation::new(
                    Some(agent.agent_id.clone()),
                    LifecycleSignal::Ended,
                );
                txn.append(&EventEnvelope::agent_lifecycle(
                    txn.paths.workspace_id.clone(),
                    session_name.as_str(),
                    agent.kind.as_str(),
                    "ReapedDead",
                    &observation,
                ))?;
            }
            Ok(dead.len())
        })
    }

    pub(super) fn reap_dead_agents_if_due(&self) {
        if !reap_due(&self.inner.paths) {
            return;
        }
        write_reap_stamp(&self.inner.paths);
        if let Err(err) = self.reap_dead_agents() {
            warn!(error = %err, "dead agent reap failed after store commit");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::LaunchParams;
    use crate::ids::{AgentSessionId, WorkspaceId};
    use crate::store::event_log;
    use crate::store::paths::RuntimePaths;

    fn store() -> (tempfile::TempDir, Store, WorkspaceId) {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).expect("state paths");
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime paths");
        let store = Store::open(paths, runtime).expect("open store");
        (dir, store, workspace_id)
    }

    fn lifecycle(
        workspace_id: &WorkspaceId,
        agent_id: &str,
        pid: Option<u32>,
        parent: Option<&str>,
    ) -> EventEnvelope {
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(agent_id)),
            LifecycleSignal::Registered,
        );
        observation.launch = LaunchParams::default();
        observation.agent_pid = pid;
        observation.parent_agent_id = parent.map(AgentSessionId::from);
        EventEnvelope::agent_lifecycle(
            workspace_id.clone(),
            "rimz-test",
            "codex",
            if parent.is_some() {
                "SubagentStart"
            } else {
                "SessionStart"
            },
            &observation,
        )
    }

    #[cfg(unix)]
    #[test]
    fn reap_dead_agents_tombstones_only_dead_roots() {
        let (_dir, store, workspace_id) = store();
        for event in [
            lifecycle(&workspace_id, "dead-root", Some(u32::MAX), None),
            lifecycle(&workspace_id, "ownerless", None, None),
            lifecycle(&workspace_id, "live-root", Some(std::process::id()), None),
            lifecycle(
                &workspace_id,
                "dead-child",
                Some(u32::MAX),
                Some("dead-root"),
            ),
        ] {
            event_log::append(&store.paths().events_log, &event).expect("append event");
        }

        let reaped = store.reap_dead_agents().expect("reap dead agents");

        assert_eq!(reaped, 1);
        let projection = store
            .runtime_projection(RuntimeScope::Audit)
            .expect("audit projection");
        let ids = projection
            .agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>();
        assert!(
            !ids.contains(&"dead-root"),
            "dead root should be tombstoned: {ids:?}"
        );
        assert!(
            ids.contains(&"ownerless") && ids.contains(&"live-root"),
            "unknown and live owners should survive: {ids:?}"
        );
        assert_eq!(
            store.reap_dead_agents().expect("reap again"),
            0,
            "second pass is idempotent and subagents are not reaped independently"
        );
    }

    #[test]
    fn reap_due_tracks_missing_fresh_and_aged_stamp() {
        let (_dir, store, _workspace_id) = store();
        let paths = store.paths();

        assert!(reap_due(paths), "missing stamp is due");
        write_reap_stamp(paths);
        assert!(!reap_due(paths), "fresh stamp is not due");

        std::fs::File::options()
            .write(true)
            .open(dead_reap_stamp(paths))
            .expect("stamp exists")
            .set_modified(SystemTime::now() - REAP_INTERVAL - Duration::from_secs(1))
            .expect("age stamp");
        assert!(reap_due(paths), "aged stamp is due");
    }
}
