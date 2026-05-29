//! Runtime projection for durable ledger records.
//!
//! Expel is read-time filtering: default live views keep only records whose
//! recorded owner process is still the same live process. Audit views bypass
//! this filter and read durable history as written.

use std::fs;

use crate::feed::{AgentState, FeedItem, RuntimeOwner, RuntimeOwnerKind, Surface};
use crate::schema::event::EventEnvelope;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeScope {
    Runtime,
    Audit,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuntimeProjection {
    pub items: Vec<FeedItem>,
    pub events: Vec<EventEnvelope>,
    pub agents: Vec<AgentState>,
}

impl RuntimeProjection {
    pub fn from_parts(
        items: Vec<FeedItem>,
        events: Vec<EventEnvelope>,
        agents: Vec<AgentState>,
        scope: RuntimeScope,
    ) -> Self {
        match scope {
            RuntimeScope::Audit => Self {
                items,
                events,
                agents,
            },
            RuntimeScope::Runtime => Self {
                items: items.into_iter().filter(item_is_runtime_visible).collect(),
                events,
                agents: agents
                    .into_iter()
                    .filter(agent_is_runtime_visible)
                    .collect(),
            },
        }
    }
}

/// Runtime visibility for a feed item. The owner-required liveness gate is a
/// script concern: a script that exits must not strand its prompt as attention.
/// Agent and bridge asks are governed by the agent rollup join in the snapshot
/// reducer (`agent_hook_session_stale`), so a missing owner there is not by
/// itself a reason to hide — only a *known-dead* owner suppresses them.
fn item_is_runtime_visible(item: &FeedItem) -> bool {
    if item.surface == Surface::Script {
        return item.runtime_owner.as_ref().is_some_and(owner_is_live);
    }
    item.runtime_owner.as_ref().is_none_or(owner_is_live)
}

/// Runtime visibility for an agent. Liveness suppresses; it never gates an
/// agent in. An unknown pid abstains (foreground/pane corroboration carries
/// liveness — see `docs/internals/agent.md`); a known owner that is known-dead
/// suppresses the stale overlay.
fn agent_is_runtime_visible(agent: &AgentState) -> bool {
    agent.runtime_owner.as_ref().is_none_or(owner_is_live)
}

pub fn current_process_owner(
    kind: RuntimeOwnerKind,
    subject_id: impl Into<String>,
) -> RuntimeOwner {
    let pid = std::process::id();
    RuntimeOwner::new(kind, subject_id, pid, process_start_token(pid))
}

pub fn process_owner(
    kind: RuntimeOwnerKind,
    subject_id: impl Into<String>,
    pid: u32,
) -> RuntimeOwner {
    RuntimeOwner::new(kind, subject_id, pid, process_start_token(pid))
}

pub fn owner_is_live(owner: &RuntimeOwner) -> bool {
    process_is_live(owner.pid, owner.process_start.as_deref())
}

#[cfg(target_os = "linux")]
pub fn process_start_token(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    linux_process_start_from_stat(&stat).map(ToOwned::to_owned)
}

#[cfg(not(target_os = "linux"))]
pub fn process_start_token(_pid: u32) -> Option<String> {
    None
}

#[cfg(target_os = "linux")]
fn process_is_live(pid: u32, expected_start: Option<&str>) -> bool {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(_) => return false,
    };
    match expected_start {
        Some(expected) => linux_process_start_from_stat(&stat) == Some(expected),
        None => true,
    }
}

#[cfg(not(target_os = "linux"))]
fn process_is_live(_pid: u32, _expected_start: Option<&str>) -> bool {
    true
}

#[cfg(target_os = "linux")]
fn linux_process_start_from_stat(stat: &str) -> Option<&str> {
    let after_comm = stat.rsplit_once(") ")?.1;
    after_comm.split_whitespace().nth(19)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::{AgentStatus, FeedKind, PermissionPosture, Surface};
    use crate::ids::WorkspaceId;
    use jiff::Timestamp;
    use std::path::Path;

    fn agent(owner: Option<RuntimeOwner>) -> AgentState {
        AgentState {
            agent_id: "sess-1".to_owned(),
            kind: "claude".to_owned(),
            status: AgentStatus::Idle,
            permission_posture: PermissionPosture::Default,
            pane: None,
            agent_pid: owner.as_ref().map(|owner| owner.pid),
            agent_process_start: owner.as_ref().and_then(|owner| owner.process_start.clone()),
            runtime_owner: owner,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            model: None,
            effort: None,
            context_pct: None,
            total_tokens: None,
            todo_done: None,
            todo_total: None,
            last_seen: Timestamp::UNIX_EPOCH,
            last_activity: Timestamp::UNIX_EPOCH,
        }
    }

    #[test]
    fn runtime_projection_includes_live_owner() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut item = FeedItem::new(
            workspace,
            Surface::Script,
            FeedKind::Question,
            "deploy?",
            "rimz",
            "cli",
        );
        item.runtime_owner = Some(current_process_owner(RuntimeOwnerKind::Script, "ask"));

        let projection = RuntimeProjection::from_parts(
            vec![item],
            Vec::new(),
            Vec::new(),
            RuntimeScope::Runtime,
        );

        assert_eq!(projection.items.len(), 1);
    }

    #[test]
    fn runtime_projection_excludes_ownerless_legacy_item() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let item = FeedItem::new(
            workspace,
            Surface::Script,
            FeedKind::Question,
            "deploy?",
            "rimz",
            "cli",
        );

        let runtime = RuntimeProjection::from_parts(
            vec![item.clone()],
            Vec::new(),
            Vec::new(),
            RuntimeScope::Runtime,
        );
        let audit =
            RuntimeProjection::from_parts(vec![item], Vec::new(), Vec::new(), RuntimeScope::Audit);

        assert!(runtime.items.is_empty());
        assert_eq!(audit.items.len(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_projection_excludes_start_token_mismatch() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut item = FeedItem::new(
            workspace,
            Surface::Script,
            FeedKind::Question,
            "deploy?",
            "rimz",
            "cli",
        );
        item.runtime_owner = Some(RuntimeOwner::new(
            RuntimeOwnerKind::Script,
            "ask",
            std::process::id(),
            Some("definitely-not-this-process".to_owned()),
        ));

        let projection = RuntimeProjection::from_parts(
            vec![item],
            Vec::new(),
            Vec::new(),
            RuntimeScope::Runtime,
        );

        assert!(projection.items.is_empty());
    }

    #[test]
    fn runtime_projection_keeps_ownerless_agent() {
        // An agent with no captured pid abstains — foreground/pane corroboration
        // carries its liveness, so the owner-required gate must not hide it.
        let projection = RuntimeProjection::from_parts(
            Vec::new(),
            Vec::new(),
            vec![agent(None)],
            RuntimeScope::Runtime,
        );
        assert_eq!(projection.agents.len(), 1);
    }

    #[test]
    fn runtime_projection_keeps_ownerless_non_script_item() {
        // A bridge ask whose owner pid was never captured stays visible; its
        // staleness is the agent rollup join's job, not the owner gate.
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let item = FeedItem::new(
            workspace,
            Surface::Bridge,
            FeedKind::Permission,
            "allow?",
            "claude",
            "agent-hook",
        );
        let projection = RuntimeProjection::from_parts(
            vec![item],
            Vec::new(),
            Vec::new(),
            RuntimeScope::Runtime,
        );
        assert_eq!(projection.items.len(), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_projection_excludes_known_dead_agent() {
        let dead = RuntimeOwner::new(RuntimeOwnerKind::Agent, "sess-1", u32::MAX, None);
        let projection = RuntimeProjection::from_parts(
            Vec::new(),
            Vec::new(),
            vec![agent(Some(dead))],
            RuntimeScope::Runtime,
        );
        assert!(projection.agents.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn runtime_projection_excludes_dead_pid() {
        let workspace = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let mut item = FeedItem::new(
            workspace,
            Surface::Script,
            FeedKind::Question,
            "deploy?",
            "rimz",
            "cli",
        );
        item.runtime_owner = Some(RuntimeOwner::new(
            RuntimeOwnerKind::Script,
            "ask",
            u32::MAX,
            None,
        ));

        let projection = RuntimeProjection::from_parts(
            vec![item],
            Vec::new(),
            Vec::new(),
            RuntimeScope::Runtime,
        );

        assert!(projection.items.is_empty());
    }
}
