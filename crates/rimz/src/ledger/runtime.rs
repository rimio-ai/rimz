//! Runtime projection for durable ledger records.
//!
//! Expel is read-time filtering: default live views keep only records whose
//! recorded owner process is still the same live process. Audit views bypass
//! this filter and read durable history as written.

use std::fs;

use crate::feed::{AgentState, FeedItem, RuntimeOwner, RuntimeOwnerKind};
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
                items: items
                    .into_iter()
                    .filter(|item| item.runtime_owner.as_ref().is_some_and(owner_is_live))
                    .collect(),
                events,
                agents: agents
                    .into_iter()
                    .filter(|agent| agent.runtime_owner.as_ref().is_some_and(owner_is_live))
                    .collect(),
            },
        }
    }
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
    use crate::feed::{FeedKind, Surface};
    use crate::ids::WorkspaceId;
    use std::path::Path;

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
