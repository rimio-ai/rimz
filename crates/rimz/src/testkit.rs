//! Test and benchmark fixtures for external targets.
//!
//! This module is behind the `testkit` feature so synthetic fleet builders stay
//! out of the shipped binary while integration tests and benches can share the
//! same event and pane shapes.

pub use crate::ledger::atomic::testkit::fsync_count;
pub use crate::ledger::event_log::testkit::{bytes_read, bytes_written};
pub use crate::proc::testkit::spawn_count;

pub mod fleet {
    use crate::agents::AgentLifecycleObservation;
    use crate::agents::lifecycle::LifecycleSignal;
    use crate::ids::{AgentSessionId, MuxName, PaneId, ViewKind, WorkspaceId};
    use crate::ledger::{StatePaths, event_log};
    use crate::pane::PaneRef;
    use crate::schema::event::EventEnvelope;

    pub const SESSION_NAME: &str = "rimz-perf";

    /// One registered agent lifecycle event for a synthetic fleet slot.
    pub fn registered_lifecycle(workspace_id: &WorkspaceId, slot: usize) -> EventEnvelope {
        EventEnvelope::agent_lifecycle(
            workspace_id.clone(),
            format!("sess-{slot}"),
            "claude",
            "SessionStart",
            &registered_observation(slot),
        )
    }

    /// Live session panes shaped like `list-panes` output, with no cwd so the
    /// fleet stays off per-worktree git enrichment unless a test adds roots.
    pub fn synthetic_panes(n: usize) -> Vec<PaneRef> {
        (0..n).map(synthetic_pane).collect()
    }

    /// Append `history_events` lifecycle frames spread across `fleet` slots.
    pub fn seed_fleet_ledger(
        paths: &StatePaths,
        fleet: usize,
        history_events: usize,
    ) -> event_log::Result<()> {
        if fleet == 0 {
            return Ok(());
        }
        for i in 0..history_events {
            event_log::append(
                &paths.events_log,
                &registered_lifecycle(&paths.workspace_id, i % fleet),
            )?;
        }
        Ok(())
    }

    fn registered_observation(slot: usize) -> AgentLifecycleObservation {
        AgentLifecycleObservation {
            agent_id: Some(AgentSessionId::from(format!("agent-{slot}"))),
            agent_name: None,
            role: None,
            team: None,
            channel: None,
            profile: None,
            kind_ordinal: None,
            signal: LifecycleSignal::Registered,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            worktree_path: None,
            worktree_branch: Some(format!("wt-{slot}")),
            task: None,
            prompt: None,
            transcript_path: None,
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            turn_error: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            pane_id: None,
            parent_agent_id: None,
        }
    }

    fn synthetic_pane(i: usize) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, format!("terminal_{i}")),
            session_name: SESSION_NAME.to_owned(),
            view_id: Some(format!("tab_{}", i % 8)),
            view_kind: Some(ViewKind::Tab),
            view_name: None,
            is_focused: i == 0,
            is_floating: false,
            command: Some("zsh".to_owned()),
            spawn_command: None,
            cwd: None,
            pane_pid: None,
            pane_process_start: None,
            hosted_agent_kind: None,
            hosted_agent_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }
    }
}
