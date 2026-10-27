//! Durable hibernation entries, at most one per agent session.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use crate::StatePaths;
use crate::disk::parse_cache::ParseCache;
use crate::disk::{atomic, lock};
use crate::ids::{AgentKind, AgentSessionId, PaneId};

#[derive(Debug, thiserror::Error)]
pub enum HibernateRecordErr {
    #[error(transparent)]
    Lock(#[from] lock::LockErr),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub pane_id: PaneId,
    pub scope: PathBuf,
    pub provider_pid: u32,
    pub frozen_at: Timestamp,
    pub reclaimed_bytes: u64,
    #[serde(default)]
    pub anon_at_freeze: u64,
    #[serde(default)]
    pub step: u32,
    #[serde(default)]
    pub wake_error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    entries: Arc<Vec<Entry>>,
}

thread_local! {
    static HIBERNATION_CACHE: ParseCache<Record> = const { ParseCache::new() };
}

pub fn read(paths: &StatePaths) -> Vec<Entry> {
    std::fs::read(&paths.hibernation)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
        .filter(|record| record.version == 1)
        .map(|record| Arc::unwrap_or_clone(record.entries))
        .unwrap_or_default()
}

pub(crate) fn read_cached(paths: &StatePaths) -> Arc<Vec<Entry>> {
    HIBERNATION_CACHE
        .with(|cache| cache.read_stamped_json(&paths.hibernation))
        .filter(|record| record.version == 1)
        .map(|record| Arc::clone(&record.entries))
        .unwrap_or_default()
}

pub fn record(paths: &StatePaths, entry: Entry) -> Result<(), HibernateRecordErr> {
    update(
        paths,
        &entry.kind.clone(),
        &entry.agent_id.clone(),
        Some(entry),
    )?;
    Ok(())
}

pub fn withdraw(
    paths: &StatePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> Result<bool, HibernateRecordErr> {
    if !read(paths)
        .iter()
        .any(|entry| entry.kind == *kind && entry.agent_id == *agent_id)
    {
        return Ok(false);
    }
    update(paths, kind, agent_id, None)
}

fn update(
    paths: &StatePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    replacement: Option<Entry>,
) -> Result<bool, HibernateRecordErr> {
    let _guard = lock::WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut entries = read(paths);
    let before = entries.len();
    entries.retain(|entry| entry.kind != *kind || entry.agent_id != *agent_id);
    let dropped = entries.len() != before;
    if !dropped && replacement.is_none() {
        return Ok(false);
    }
    entries.extend(replacement);
    atomic::write_temp_then_rename(
        &paths.hibernation,
        &Record {
            version: 1,
            entries: Arc::new(entries),
        },
    )?;
    Ok(dropped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;

    fn entry(id: &str) -> Entry {
        Entry {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: id.into(),
            pane_id: "tmux:%3".parse().unwrap(),
            scope: "/sys/fs/cgroup/rimz-agent-test.scope".into(),
            provider_pid: 1234,
            frozen_at: Timestamp::UNIX_EPOCH,
            anon_at_freeze: 0,
            step: 0,
            wake_error: None,
            reclaimed_bytes: 200,
        }
    }

    #[test]
    fn records_replace_only_the_same_session_and_withdraw() {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        record(&paths, entry("a")).unwrap();
        record(&paths, entry("b")).unwrap();
        assert_eq!(read(&paths), [entry("a"), entry("b")]);
        let mut replacement = entry("a");
        replacement.reclaimed_bytes = 400;
        record(&paths, replacement.clone()).unwrap();
        assert_eq!(read(&paths), [entry("b"), replacement]);
        assert!(!withdraw(&paths, &AgentKind::new_unchecked("codex"), &"a".into()).unwrap());
        assert!(withdraw(&paths, &entry("a").kind, &"a".into()).unwrap());
        assert_eq!(read(&paths), [entry("b")]);
        let wire: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&paths.hibernation).unwrap()).unwrap();
        assert_eq!(wire["version"], 1);
        assert_eq!(wire["entries"][0]["pane_id"], "tmux:%3");
    }

    #[test]
    fn cached_reads_share_the_parse_until_replacement_and_follow_withdrawal() {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        record(&paths, entry("a")).unwrap();
        let first = read_cached(&paths);
        assert!(std::sync::Arc::ptr_eq(&first, &read_cached(&paths)));
        record(&paths, entry("b")).unwrap();
        assert_eq!(*read_cached(&paths), [entry("a"), entry("b")]);
        withdraw(&paths, &entry("a").kind, &"a".into()).unwrap();
        assert_eq!(*read_cached(&paths), [entry("b")]);
        assert_eq!(*first, [entry("a")]);
    }

    #[test]
    fn unknown_version_and_corruption_read_empty() {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        record(&paths, entry("a")).unwrap();
        assert_eq!(read(&paths), [entry("a")]);
        let bytes = serde_json::to_vec(&serde_json::json!({"version": 2, "entries": [entry("a")]}))
            .unwrap();
        std::fs::write(&paths.hibernation, bytes).unwrap();
        assert!(read(&paths).is_empty());
        std::fs::write(&paths.hibernation, "broken").unwrap();
        assert!(read(&paths).is_empty());
    }

    #[test]
    fn version_one_entries_without_steps_keep_their_wake_identity() {
        let dir = tempfile::tempdir().unwrap();
        let paths =
            StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
        record(&paths, entry("old")).unwrap();
        let mut old = serde_json::to_value(entry("old")).unwrap();
        for key in ["anon_at_freeze", "step", "wake_error"] {
            old.as_object_mut().unwrap().remove(key);
        }
        old["last_reclaimed_at"] = serde_json::json!("1970-01-01T00:00:00Z");
        std::fs::write(
            &paths.hibernation,
            serde_json::to_vec(&serde_json::json!({"version": 1, "entries": [old]})).unwrap(),
        )
        .unwrap();
        assert_eq!(read(&paths), [entry("old")]);
    }
}
