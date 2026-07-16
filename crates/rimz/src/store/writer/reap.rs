use std::time::Duration;

use jiff::Timestamp;
use tracing::warn;

use crate::agents::{AgentLifecycleObservation, LifecycleSignal};
use crate::store::event::EventEnvelope;
use crate::store::runtime::{self, AgentLiveness, RuntimeScope};
use crate::store::{live_roster, session_death};

use super::super::{Result, StatePaths, Store, workspace_record};
use super::debounce;

const REAP_INTERVAL: Duration = Duration::from_secs(60);

fn dead_reap_stamp(paths: &StatePaths) -> std::path::PathBuf {
    paths.locks_dir.join("dead-reap.stamp")
}

pub(super) fn reap_due(paths: &StatePaths) -> bool {
    debounce::stamp_due(&dead_reap_stamp(paths), REAP_INTERVAL)
}

fn reap_session_name(paths: &StatePaths) -> String {
    workspace_record::read(&paths.workspace_record)
        .map(|record| record.session_name)
        .unwrap_or_else(|_| "rimz-reap".to_owned())
}

impl Store {
    pub(crate) fn reap_dead_sessions(&self) -> Result<usize> {
        // The persisted roster protects crash-recovery candidates until room
        // rebirth consumes it. The remaining scan stays lock-free: a live
        // same-id session that races the append clears its end stamp on its
        // next lifecycle event.
        let projection = self.runtime_projection(RuntimeScope::Audit)?;
        let protected = live_roster::read(&self.inner.paths.live_roster)
            .map(|roster| roster.agents)
            .unwrap_or_default();
        let now = Timestamp::now();
        let victims = projection
            .agents
            .iter()
            .filter(|agent| agent.parent_agent_id.is_none())
            .filter(|agent| agent.ended_at.is_none())
            .filter(|agent| !protected.contains(&(agent.kind.clone(), agent.agent_id.clone())))
            .filter_map(|agent| {
                let event_name = if runtime::agent_liveness(agent) == AgentLiveness::Dead {
                    "ReapedDead"
                } else if session_death::agent_is_pidless(agent)
                    && session_death::session_age_secs(now, agent)
                        > session_death::GHOST_SESSION_TTL_SECS
                {
                    "ReapedStale"
                } else if projection.agents.iter().any(|newer| {
                    newer.parent_agent_id.is_none()
                        && newer.ended_at.is_none()
                        && session_death::supersedes(agent, newer)
                }) {
                    "ReapedSuperseded"
                } else {
                    return None;
                };
                Some((agent.kind.clone(), agent.agent_id.clone(), event_name))
            })
            .collect::<Vec<_>>();
        if victims.is_empty() {
            return Ok(0);
        }

        let session_name = reap_session_name(&self.inner.paths);
        self.commit(|txn| {
            for (kind, agent_id, event_name) in &victims {
                let observation =
                    AgentLifecycleObservation::new(Some(agent_id.clone()), LifecycleSignal::Ended);
                txn.append(&EventEnvelope::agent_lifecycle(
                    txn.paths.workspace_id.clone(),
                    session_name.as_str(),
                    kind.as_str(),
                    *event_name,
                    &observation,
                ))?;
            }
            Ok(victims.len())
        })
    }

    pub(super) fn reap_dead_sessions_if_due(&self) {
        if !reap_due(&self.inner.paths) {
            return;
        }
        debounce::touch_stamp(&dead_reap_stamp(&self.inner.paths));
        if let Err(err) = self.reap_dead_sessions() {
            warn!(error = %err, "dead session reap failed after store commit");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::agents::{AgentAdapter, AmpAdapter, LaunchParams, SessionOrigin};
    use crate::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
    use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
    use crate::store::event::EventKind;
    use crate::store::event_log;
    use crate::store::paths::RuntimePaths;
    use serde_json::json;

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
        observation.worktree_path = Some(format!("/repo/{agent_id}"));
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

    fn fresh_pane_lifecycle(
        workspace_id: &WorkspaceId,
        agent_id: &str,
        pane_id: &str,
    ) -> EventEnvelope {
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(agent_id)),
            LifecycleSignal::Registered,
        );
        observation.agent_pid = Some(std::process::id());
        observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, pane_id));
        observation.origin = Some(SessionOrigin::Fresh);
        EventEnvelope::agent_lifecycle(
            workspace_id.clone(),
            "rimz-test",
            "codex",
            "SessionStart",
            &observation,
        )
    }

    fn amp_focus_lifecycle(
        workspace_id: &WorkspaceId,
        agent_id: &str,
        pane_id: &str,
    ) -> EventEnvelope {
        let mut observation = AmpAdapter
            .observe_lifecycle(
                "session_start",
                &json!({ "session_id": agent_id, "cwd": "/repo" }),
            )
            .expect("Amp session start observation");
        observation.agent_pid = Some(std::process::id());
        observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, pane_id));
        EventEnvelope::agent_lifecycle(
            workspace_id.clone(),
            "rimz-test",
            "amp",
            "session_start",
            &observation,
        )
    }

    fn daemon_lifecycle(workspace_id: &WorkspaceId, agent_id: &str) -> EventEnvelope {
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(agent_id)),
            LifecycleSignal::Registered,
        );
        observation.runtime_owner = Some(RuntimeOwner::new(
            RuntimeOwnerKind::Daemon,
            agent_id,
            std::process::id(),
            None,
        ));
        EventEnvelope::agent_lifecycle(
            workspace_id.clone(),
            "rimz-test",
            "codex",
            "SessionStart",
            &observation,
        )
    }

    #[cfg(unix)]
    #[test]
    fn reap_dead_sessions_stamps_store_provable_roots_once() {
        let (_dir, store, workspace_id) = store();
        let now = Timestamp::now();
        let mut stale_ownerless = lifecycle(&workspace_id, "stale-ownerless", None, None);
        stale_ownerless.timestamp =
            now - Duration::from_secs((session_death::GHOST_SESSION_TTL_SECS + 60) as u64);
        let mut stale_daemon = daemon_lifecycle(&workspace_id, "stale-daemon");
        stale_daemon.timestamp =
            now - Duration::from_secs((session_death::GHOST_SESSION_TTL_SECS + 60) as u64);
        let mut cleared = fresh_pane_lifecycle(&workspace_id, "cleared", "%1");
        cleared.timestamp = now - Duration::from_secs(2);
        let mut replacement = fresh_pane_lifecycle(&workspace_id, "replacement", "%1");
        replacement.timestamp = now - Duration::from_secs(1);
        for event in [
            lifecycle(&workspace_id, "dead-root", Some(u32::MAX), None),
            stale_ownerless,
            stale_daemon,
            lifecycle(&workspace_id, "fresh-ownerless", None, None),
            lifecycle(&workspace_id, "live-root", Some(std::process::id()), None),
            cleared,
            replacement,
            lifecycle(
                &workspace_id,
                "dead-child",
                Some(u32::MAX),
                Some("dead-root"),
            ),
        ] {
            event_log::append(&store.paths().events_log, &event).expect("append event");
        }

        let reaped = store.reap_dead_sessions().expect("reap dead sessions");

        assert_eq!(reaped, 4);
        let projection = store
            .runtime_projection(RuntimeScope::Audit)
            .expect("audit projection");
        let ids = projection
            .agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>();
        assert!(
            ids.contains(&"fresh-ownerless")
                && ids.contains(&"live-root")
                && ids.contains(&"replacement")
                && ids.contains(&"dead-root")
                && ids.contains(&"stale-ownerless")
                && ids.contains(&"stale-daemon")
                && ids.contains(&"cleared"),
            "audit retains active and ended roots: {ids:?}"
        );
        for agent_id in ["dead-root", "stale-ownerless", "stale-daemon", "cleared"] {
            assert!(
                projection
                    .agents
                    .iter()
                    .any(|agent| agent.agent_id == agent_id && agent.ended_at.is_some()),
                "missing end stamp for {agent_id}"
            );
        }
        let events = store.read_events().expect("read reap events");
        for (agent_id, event_name) in [
            ("dead-root", "ReapedDead"),
            ("stale-ownerless", "ReapedStale"),
            ("stale-daemon", "ReapedStale"),
            ("cleared", "ReapedSuperseded"),
        ] {
            assert!(
                events.iter().any(|event| {
                    matches!(
                        event.kind(),
                        EventKind::AgentLifecycle(payload)
                            if payload.event_name.as_deref() == Some(event_name)
                                && payload.observation.agent_id.as_deref() == Some(agent_id)
                    )
                }),
                "missing {event_name} end stamp event for {agent_id}"
            );
        }
        assert_eq!(
            store.reap_dead_sessions().expect("reap again"),
            0,
            "second pass is idempotent and subagents are not reaped independently"
        );
    }

    #[cfg(unix)]
    #[test]
    fn amp_focus_switch_retires_then_revives_threads_in_one_pane() {
        let (_dir, store, workspace_id) = store();
        let now = Timestamp::now();
        let mut thread_a = amp_focus_lifecycle(&workspace_id, "T-a", "%1");
        thread_a.timestamp = now - Duration::from_secs(2);
        let mut thread_b = amp_focus_lifecycle(&workspace_id, "T-b", "%1");
        thread_b.timestamp = now - Duration::from_secs(1);
        for event in [thread_a, thread_b] {
            event_log::append(&store.paths().events_log, &event).expect("append focus event");
        }

        assert_eq!(store.reap_dead_sessions().expect("reap thread A"), 1);
        let after_a_to_b = store
            .runtime_projection(RuntimeScope::Audit)
            .expect("projection after A to B");
        assert!(
            after_a_to_b
                .agents
                .iter()
                .any(|agent| { agent.agent_id == "T-a" && agent.ended_at.is_some() }),
            "expected T-a ended in {:#?}",
            after_a_to_b.agents
        );
        assert!(
            after_a_to_b
                .agents
                .iter()
                .any(|agent| { agent.agent_id == "T-b" && agent.ended_at.is_none() }),
            "expected T-b active in {:#?}",
            after_a_to_b.agents
        );

        let mut thread_a = amp_focus_lifecycle(&workspace_id, "T-a", "%1");
        thread_a.timestamp = Timestamp::now() + Duration::from_secs(1);
        event_log::append(&store.paths().events_log, &thread_a).expect("append switch-back event");

        assert_eq!(store.reap_dead_sessions().expect("reap thread B"), 1);
        let after_b_to_a = store
            .runtime_projection(RuntimeScope::Audit)
            .expect("projection after B to A");
        assert!(
            after_b_to_a
                .agents
                .iter()
                .any(|agent| { agent.agent_id == "T-a" && agent.ended_at.is_none() })
        );
        assert!(
            after_b_to_a
                .agents
                .iter()
                .any(|agent| { agent.agent_id == "T-b" && agent.ended_at.is_some() })
        );
    }

    #[cfg(unix)]
    #[test]
    fn live_roster_protects_crash_recovery_candidate_until_removed() {
        let (_dir, store, workspace_id) = store();
        event_log::append(
            &store.paths().events_log,
            &lifecycle(&workspace_id, "recoverable", Some(u32::MAX), None),
        )
        .expect("append dead-owner event");
        let key = (
            AgentKind::new_unchecked("codex"),
            AgentSessionId::from("recoverable"),
        );
        live_roster::publish(
            &store.paths().live_roster,
            [key.clone()].into_iter().collect(),
        )
        .expect("publish roster");

        assert_eq!(store.reap_dead_sessions().expect("guarded reap"), 0);
        let guarded = store
            .runtime_projection(RuntimeScope::Audit)
            .expect("guarded projection");
        assert!(guarded.agents.iter().any(|agent| agent.agent_id == key.1));
        assert!(!guarded.ended.contains(&key));

        std::fs::remove_file(&store.paths().live_roster).expect("remove roster");
        assert_eq!(store.reap_dead_sessions().expect("unguarded reap"), 1);
        let reaped = store
            .runtime_projection(RuntimeScope::Audit)
            .expect("reaped projection");
        assert!(
            reaped
                .agents
                .iter()
                .any(|agent| agent.agent_id == key.1 && agent.ended_at.is_some())
        );
        assert!(reaped.ended.contains(&key));
    }

    #[test]
    fn reap_due_tracks_missing_fresh_and_aged_stamp() {
        let (_dir, store, _workspace_id) = store();
        let paths = store.paths();

        assert!(reap_due(paths), "missing stamp is due");
        debounce::touch_stamp(&dead_reap_stamp(paths));
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
