//! Durable named-channel registry.
//!
//! Worktree, team, and directory channels are derived from their backing state.
//! This file stores only bare named lanes so an empty cooperation tab survives
//! room rebirth.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ledger::atomic::{self, write_temp_then_rename};
use crate::ledger::lock::{self, WorkspaceLock};
use crate::ledger::paths::StatePaths;

#[derive(Debug, thiserror::Error)]
pub enum ChannelErr {
    #[error("invalid channel name `{name}`; use ASCII letters, numbers, `_`, or `-`")]
    InvalidName { name: String },
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error(transparent)]
    Lock(#[from] lock::LockErr),
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("json parse error on {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, ChannelErr>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelRecord {
    pub name: String,
    pub created_at: Timestamp,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Channels(pub BTreeMap<String, ChannelRecord>);

impl Channels {
    pub fn into_records(self) -> Vec<ChannelRecord> {
        self.0.into_values().collect()
    }
}

pub fn validate_name(name: &str) -> Result<()> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(ChannelErr::InvalidName {
            name: name.to_owned(),
        })
    }
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn read(path: &Path) -> Result<Channels> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| ChannelErr::Json {
            path: path.to_path_buf(),
            source,
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(Channels::default()),
        Err(source) => Err(ChannelErr::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn list(path: &Path) -> Result<Vec<ChannelRecord>> {
    Ok(read(path)?.into_records())
}

#[must_use = "durability barrier; check the result"]
pub fn register(paths: &StatePaths, name: &str) -> Result<ChannelRecord> {
    validate_name(name)?;
    let _lock = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut channels = read(&paths.channels_record)?;
    let record = channels
        .0
        .entry(name.to_owned())
        .or_insert_with(|| ChannelRecord {
            name: name.to_owned(),
            created_at: Timestamp::now(),
        })
        .clone();
    write_temp_then_rename(&paths.channels_record, &channels)?;
    Ok(record)
}

#[must_use = "durability barrier; check the result"]
pub fn remove(paths: &StatePaths, name: &str) -> Result<Option<ChannelRecord>> {
    validate_name(name)?;
    let _lock = WorkspaceLock::acquire(&paths.workspace_lock)?;
    let mut channels = read(&paths.channels_record)?;
    let removed = channels.0.remove(name);
    write_temp_then_rename(&paths.channels_record, &channels)?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;
    use tempfile::tempdir;

    fn paths() -> StatePaths {
        let dir = tempdir().expect("tempdir");
        let root = dir.keep();
        let id = WorkspaceId::from_project_root(&root);
        StatePaths::under(id, &root).expect("state paths")
    }

    #[test]
    fn registry_round_trips_and_register_is_idempotent() {
        let paths = paths();

        let first = register(&paths, "design").expect("register");
        let second = register(&paths, "design").expect("register again");
        let records = list(&paths.channels_record).expect("list");

        assert_eq!(first, second);
        assert_eq!(records, vec![first]);
    }

    #[test]
    fn remove_deletes_named_record() {
        let paths = paths();
        register(&paths, "ops").expect("register");

        let removed = remove(&paths, "ops").expect("remove");
        let records = list(&paths.channels_record).expect("list");

        assert_eq!(removed.map(|record| record.name), Some("ops".to_owned()));
        assert!(records.is_empty());
    }

    #[test]
    fn validates_bare_channel_names() {
        for name in ["design", "ops_2", "run-42"] {
            assert!(valid_name(name), "{name}");
        }
        for name in ["", "a.b", "a/b", "#ops", "two words"] {
            assert!(!valid_name(name), "{name}");
        }
    }
}
