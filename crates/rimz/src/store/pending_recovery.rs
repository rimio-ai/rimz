//! Lost agents awaiting recovery, replacement, or the user's decline.
//!
//! A rebirth boundary parks the dead incarnation's live roster here before it
//! deletes the roster. A settlement removes agents whose resumed or replacement
//! tab is confirmed open, or whose ended stamp is durable, including automatic
//! seat refills. A wrapper's durable pane attachment also settles its session.
//! Until then membership keeps an agent out of the dead and stale reap.
//!
//! Boundary and settlement are read-modify-write from separate CLI processes,
//! so both take the workspace lock and publish durably. Readers stay
//! lock-free; an absent or unreadable record reads as empty.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::disk::atomic;
use crate::disk::lock::{self, WorkspaceLock};
use crate::disk::paths::StatePaths;
use crate::ids::{AgentKind, AgentSessionId};

const PENDING_RECOVERY_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PendingRecoveryErr {
    #[error(transparent)]
    Lock(#[from] lock::LockErr),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
}

#[derive(Serialize, Deserialize)]
struct PendingRecovery {
    version: u32,
    agents: BTreeSet<(AgentKind, AgentSessionId)>,
}

pub(crate) fn read(path: &Path) -> BTreeSet<(AgentKind, AgentSessionId)> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PendingRecovery>(&bytes).ok())
        .filter(|record| record.version == PENDING_RECOVERY_VERSION)
        .map(|record| record.agents)
        .unwrap_or_default()
}

/// Add `agents` to the record.
pub(crate) fn park(
    paths: &StatePaths,
    agents: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Result<(), PendingRecoveryErr> {
    update(paths, |pending| pending.union(agents).cloned().collect())
}

/// Remove `agents` from the record once a settlement has resumed or ended them.
pub(crate) fn settle(
    paths: &StatePaths,
    agents: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Result<(), PendingRecoveryErr> {
    update(paths, |pending| {
        pending.difference(agents).cloned().collect()
    })
}

fn update(
    paths: &StatePaths,
    change: impl FnOnce(&BTreeSet<(AgentKind, AgentSessionId)>) -> BTreeSet<(AgentKind, AgentSessionId)>,
) -> Result<(), PendingRecoveryErr> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let pending = read(&paths.pending_recovery);
    let agents = change(&pending);
    if agents == pending {
        return Ok(());
    }
    atomic::write_temp_then_rename(
        &paths.pending_recovery,
        &PendingRecovery {
            version: PENDING_RECOVERY_VERSION,
            agents,
        },
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    fn key(id: &str) -> (AgentKind, AgentSessionId) {
        (AgentKind::new_unchecked("claude"), AgentSessionId::from(id))
    }

    #[test]
    fn park_accumulates_and_settle_removes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("paths");

        assert!(read(&paths.pending_recovery).is_empty());
        park(&paths, &BTreeSet::new()).expect("park nothing");
        assert!(!paths.pending_recovery.exists());

        park(&paths, &[key("a")].into()).expect("park a");
        park(&paths, &[key("b")].into()).expect("park b");
        assert_eq!(read(&paths.pending_recovery), [key("a"), key("b")].into());

        settle(&paths, &[key("a")].into()).expect("settle a");
        assert_eq!(read(&paths.pending_recovery), [key("b")].into());
    }

    #[test]
    fn bad_or_unknown_version_reads_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pending-recovery.json");

        std::fs::write(&path, b"not json").expect("write bad json");
        assert!(read(&path).is_empty());

        std::fs::write(&path, r#"{"version":999,"agents":[["claude","a"]]}"#)
            .expect("write unknown version");
        assert!(read(&path).is_empty());
    }
}
