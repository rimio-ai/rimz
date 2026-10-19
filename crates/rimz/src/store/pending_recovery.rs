//! Lost agents awaiting recovery, replacement, or the user's decline.
//!
//! A rebirth boundary parks the dead incarnation's live roster here before it
//! deletes the roster. A settlement removes agents whose resumed or replacement
//! tab is confirmed open, or whose ended stamp is durable, including automatic
//! seat refills. A wrapper's durable pane attachment settles a parked
//! session, but a seeded session stays pending until its tab is confirmed;
//! an unconfirmed one returns to parked, so an explicit resume releases it
//! on attach. Until then membership keeps an agent out of the dead and stale
//! reap.
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PendingRecovery {
    version: u32,
    agents: BTreeSet<(AgentKind, AgentSessionId)>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    seeded: BTreeSet<(AgentKind, AgentSessionId)>,
}

pub(crate) fn read(path: &Path) -> BTreeSet<(AgentKind, AgentSessionId)> {
    read_record(path).agents
}

fn read_record(path: &Path) -> PendingRecovery {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<PendingRecovery>(&bytes).ok())
        .filter(|record| record.version == PENDING_RECOVERY_VERSION)
        .unwrap_or_else(|| PendingRecovery {
            version: PENDING_RECOVERY_VERSION,
            agents: BTreeSet::new(),
            seeded: BTreeSet::new(),
        })
}

/// Add `agents` to the record.
pub(crate) fn park(
    paths: &StatePaths,
    agents: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Result<(), PendingRecoveryErr> {
    update(paths, |pending| {
        pending.agents.extend(agents.iter().cloned())
    })
}

/// Remove `agents` from the record once a settlement has confirmed or ended them.
pub(crate) fn settle(
    paths: &StatePaths,
    agents: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Result<(), PendingRecoveryErr> {
    update(paths, |pending| {
        pending.agents.retain(|key| !agents.contains(key));
    })
}

/// Settle ended keys and replace stale seeds with this settlement's resumed keys.
pub(crate) fn seed(
    paths: &StatePaths,
    settled: &BTreeSet<(AgentKind, AgentSessionId)>,
    seeded: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Result<(), PendingRecoveryErr> {
    update(paths, |pending| {
        pending.agents.retain(|key| !settled.contains(key));
        pending.seeded = seeded.clone();
    })
}

/// Release confirmed agents and return unconfirmed agents to parked.
pub(crate) fn confirm(
    paths: &StatePaths,
    confirmed: &BTreeSet<(AgentKind, AgentSessionId)>,
    unconfirmed: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Result<(), PendingRecoveryErr> {
    update(paths, |pending| {
        pending.agents.retain(|key| !confirmed.contains(key));
        pending.seeded.retain(|key| !unconfirmed.contains(key));
    })
}

/// Release a pane-bound agent unless its seeded tab still awaits confirmation.
pub(super) fn attach(
    paths: &StatePaths,
    key: &(AgentKind, AgentSessionId),
) -> Result<(), PendingRecoveryErr> {
    update(paths, |pending| {
        if !pending.seeded.contains(key) {
            pending.agents.remove(key);
        }
    })
}

fn update(
    paths: &StatePaths,
    change: impl FnOnce(&mut PendingRecovery),
) -> Result<(), PendingRecoveryErr> {
    let _guard = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut pending = read_record(&paths.pending_recovery);
    let before = pending.clone();
    change(&mut pending);
    pending.seeded.retain(|key| pending.agents.contains(key));
    if pending == before {
        return Ok(());
    }
    atomic::write_temp_then_rename(&paths.pending_recovery, &pending)?;
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

    fn record(paths: &StatePaths) -> serde_json::Value {
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&paths.pending_recovery).expect("record"))
                .expect("decode record");
        let agents: BTreeSet<(AgentKind, AgentSessionId)> =
            serde_json::from_value(record["agents"].clone()).expect("agents");
        let seeded: BTreeSet<(AgentKind, AgentSessionId)> = record
            .get("seeded")
            .map(|value| serde_json::from_value(value.clone()).expect("seeded"))
            .unwrap_or_default();
        assert!(seeded.is_subset(&agents), "seeded must stay pending");
        record
    }

    fn write_record(paths: &StatePaths, contents: &str) {
        std::fs::create_dir_all(paths.pending_recovery.parent().expect("record parent"))
            .expect("record directory");
        std::fs::write(&paths.pending_recovery, contents).expect("record");
    }

    #[test]
    fn version_one_without_seeded_keeps_its_agents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("paths");
        write_record(&paths, r#"{"version":1,"agents":[["claude","a"]]}"#);

        assert_eq!(read(&paths.pending_recovery), [key("a")].into());
        seed(&paths, &BTreeSet::new(), &BTreeSet::new()).expect("no tabs");
        assert_eq!(record(&paths)["version"], 1);
        assert!(record(&paths).get("seeded").is_none());
        assert_eq!(read(&paths.pending_recovery), [key("a")].into());
    }

    #[test]
    fn settle_removes_keys_from_agents_and_seeded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("paths");
        write_record(
            &paths,
            r#"{"version":1,"agents":[["claude","a"],["claude","b"]],"seeded":[["claude","a"],["claude","b"]]}"#,
        );

        settle(&paths, &[key("a")].into()).expect("settle a");
        assert_eq!(
            record(&paths)["agents"],
            serde_json::json!([["claude", "b"]])
        );
        assert_eq!(
            record(&paths)["seeded"],
            serde_json::json!([["claude", "b"]])
        );
    }

    #[test]
    fn settlement_replaces_stale_seeded_and_intersects_remaining_agents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("paths");
        write_record(
            &paths,
            r#"{"version":1,"agents":[["claude","a"],["claude","b"]],"seeded":[["claude","a"]]}"#,
        );

        seed(
            &paths,
            &[key("a")].into(),
            &[key("a"), key("b"), key("absent")].into(),
        )
        .expect("new settlement");
        assert_eq!(
            record(&paths)["agents"],
            serde_json::json!([["claude", "b"]])
        );
        assert_eq!(
            record(&paths)["seeded"],
            serde_json::json!([["claude", "b"]])
        );

        seed(&paths, &BTreeSet::new(), &BTreeSet::new()).expect("settlement without tabs");
        assert_eq!(
            record(&paths)["agents"],
            serde_json::json!([["claude", "b"]])
        );
        assert!(record(&paths).get("seeded").is_none());
    }

    #[test]
    fn parking_preserves_seeds_and_repairs_the_subset() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("paths");
        write_record(
            &paths,
            r#"{"version":1,"agents":[["claude","a"]],"seeded":[["claude","a"],["claude","absent"]]}"#,
        );

        park(&paths, &[key("b")].into()).expect("park b");
        assert_eq!(
            record(&paths)["agents"],
            serde_json::json!([["claude", "a"], ["claude", "b"]])
        );
        assert_eq!(
            record(&paths)["seeded"],
            serde_json::json!([["claude", "a"]])
        );
    }

    #[test]
    fn confirmation_settles_open_tabs_and_unseeds_failed_tabs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("paths");
        write_record(
            &paths,
            r#"{"version":1,"agents":[["claude","a"],["claude","b"],["claude","c"]],"seeded":[["claude","a"],["claude","b"],["claude","c"]]}"#,
        );

        confirm(&paths, &[key("a")].into(), &[key("b")].into()).expect("confirm tabs");
        assert_eq!(
            record(&paths)["agents"],
            serde_json::json!([["claude", "b"], ["claude", "c"]])
        );
        assert_eq!(
            record(&paths)["seeded"],
            serde_json::json!([["claude", "c"]])
        );
    }
}
