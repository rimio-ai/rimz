//! Shared room-runtime cockpit lens applied to sidebar body membership.

use std::fs;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::agents::AgentStatus;
use crate::store::snapshot::{SidebarRow, SidebarWorktreeGroup, WorktreePrState};
use crate::store::{RuntimePaths, atomic};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", content = "status", rename_all = "snake_case")]
pub(crate) enum BodyFilter {
    Status(AgentStatus),
    Unread,
    OpenPr,
}

impl BodyFilter {
    pub(crate) fn matches(self, row: &SidebarRow, pr_open: bool) -> bool {
        match self {
            Self::Status(status) => row.status() == Some(status),
            Self::Unread => row.unread,
            Self::OpenPr => pr_open,
        }
    }

    pub(crate) fn total(self, groups: &[SidebarWorktreeGroup]) -> usize {
        match self {
            Self::Status(status) => groups
                .iter()
                .flat_map(|group| &group.status_counts)
                .filter(|count| count.status == status)
                .map(|count| count.count)
                .sum(),
            Self::Unread => groups
                .iter()
                .flat_map(|group| &group.rows)
                .filter(|row| row.unread)
                .count(),
            Self::OpenPr => groups
                .iter()
                .filter(|group| group.pr_state == Some(WorktreePrState::Open))
                .map(|group| group.rows.len())
                .sum(),
        }
    }
}

pub(crate) fn load(runtime: &RuntimePaths) -> Option<BodyFilter> {
    let path = runtime.sidebar_filter_path();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar body filter unreadable");
            return None;
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(filter) => Some(filter),
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar body filter invalid");
            None
        }
    }
}

pub(crate) fn write(runtime: &RuntimePaths, filter: BodyFilter) -> atomic::Result<()> {
    atomic::write_temp_then_rename_cache(&runtime.sidebar_filter_path(), &filter)
}

/// Clear the shared show-all state. Idempotent: a missing file is success.
pub(crate) fn clear(runtime: &RuntimePaths) -> std::io::Result<()> {
    match fs::remove_file(runtime.sidebar_filter_path()) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    fn runtime(dir: &std::path::Path) -> RuntimePaths {
        RuntimePaths::under(
            WorkspaceId::parse("ws_0123456789abcdef01234567").expect("workspace id"),
            dir,
        )
        .expect("runtime paths")
    }

    #[test]
    fn round_trip_missing_and_invalid_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        assert_eq!(load(&runtime), None);

        let filter = BodyFilter::Status(AgentStatus::Waiting);
        write(&runtime, filter).expect("write filter");
        assert_eq!(load(&runtime), Some(filter));

        fs::write(runtime.sidebar_filter_path(), b"not json").expect("garbage file");
        assert_eq!(load(&runtime), None);
    }

    #[test]
    fn clear_removes_a_filter_and_accepts_a_missing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = runtime(dir.path());
        write(&runtime, BodyFilter::Unread).expect("write filter");

        clear(&runtime).expect("clear filter");
        assert_eq!(load(&runtime), None);
        clear(&runtime).expect("clear missing filter");
    }
}
