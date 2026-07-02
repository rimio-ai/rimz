//! Shared pending/terminal JSON store for cache-class item files.
//!
//! The split keeps decision scans O(pending): a terminal write first lands on
//! the pending side, then an atomic rename moves it into `terminal/`. A crash
//! between those steps leaves exactly one copy, and terminal-status stragglers
//! on the pending side stay inert for callers that filter pending status. These
//! files are rename-atomic, no-fsync cache state; correctness rides the CAS and
//! event log.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::ledger::atomic;

pub(crate) trait PendingTerminalRecord: Serialize + DeserializeOwned {
    fn file_stem(&self) -> String;
    fn is_terminal(&self) -> bool;
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreErr {
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
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

const TERMINAL_SUBDIR: &str = "terminal";

pub(crate) fn pending_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(format!("{stem}.json"))
}

pub(crate) fn terminal_path(dir: &Path, stem: &str) -> PathBuf {
    dir.join(TERMINAL_SUBDIR).join(format!("{stem}.json"))
}

#[must_use = "durability barrier; check the result"]
pub(crate) fn write<R: PendingTerminalRecord>(dir: &Path, record: &R) -> Result<(), StoreErr> {
    let stem = record.file_stem();
    let path = pending_path(dir, &stem);
    atomic::write_temp_then_rename_cache(&path, record)?;
    if record.is_terminal() {
        let dest = terminal_path(dir, &stem);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|source| StoreErr::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::rename(&path, &dest).map_err(|source| StoreErr::Io { path, source })?;
    } else {
        remove_terminal_copy(dir, &stem)?;
    }
    Ok(())
}

pub(crate) fn load<R: PendingTerminalRecord>(
    dir: &Path,
    stem: &str,
) -> Result<Option<R>, StoreErr> {
    let pending = pending_path(dir, stem);
    let path = if pending.exists() {
        pending
    } else {
        let terminal = terminal_path(dir, stem);
        if !terminal.exists() {
            return Ok(None);
        }
        terminal
    };
    read_item(&path).map(Some)
}

pub(crate) fn list_all<R: PendingTerminalRecord>(dir: &Path) -> Result<Vec<R>, StoreErr> {
    let mut by_stem = HashMap::new();
    for item in read_dir_items::<R>(&dir.join(TERMINAL_SUBDIR))?
        .into_iter()
        .chain(read_dir_items::<R>(dir)?)
    {
        by_stem.insert(item.file_stem(), item);
    }
    Ok(by_stem.into_values().collect())
}

pub(crate) fn list_pending_raw<R: DeserializeOwned>(dir: &Path) -> Result<Vec<R>, StoreErr> {
    read_dir_items(dir)
}

pub(crate) fn read_dir_items<R: DeserializeOwned>(dir: &Path) -> Result<Vec<R>, StoreErr> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    let entries = fs::read_dir(dir).map_err(|source| StoreErr::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| StoreErr::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        items.push(read_item(&path)?);
    }
    Ok(items)
}

pub(crate) fn remove_terminal_copy(dir: &Path, stem: &str) -> Result<(), StoreErr> {
    let path = terminal_path(dir, stem);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StoreErr::Io { path, source }),
    }
}

pub(crate) fn prune_terminal(
    dir: &Path,
    older_than: Duration,
) -> Result<atomic::PruneOutcome, StoreErr> {
    let terminal_dir = dir.join(TERMINAL_SUBDIR);
    Ok(atomic::prune_old_files(
        &terminal_dir,
        older_than,
        |path| path.extension().and_then(|s| s.to_str()) == Some("json"),
    )?)
}

fn read_item<R: DeserializeOwned>(path: &Path) -> Result<R, StoreErr> {
    let bytes = fs::read(path).map_err(|source| StoreErr::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| StoreErr::Json {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::time::SystemTime;
    use tempfile::tempdir;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct TestRecord {
        id: String,
        status: TestStatus,
        title: String,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
    enum TestStatus {
        Pending,
        Done,
    }

    impl PendingTerminalRecord for TestRecord {
        fn file_stem(&self) -> String {
            self.id.clone()
        }

        fn is_terminal(&self) -> bool {
            self.status == TestStatus::Done
        }
    }

    fn record(id: &str, status: TestStatus, title: &str) -> TestRecord {
        TestRecord {
            id: id.to_owned(),
            status,
            title: title.to_owned(),
        }
    }

    #[test]
    fn terminal_write_relocates_and_straggler_dedup_prefers_pending() {
        let dir = tempdir().unwrap();
        let mut item = record("one", TestStatus::Pending, "pending");

        write(dir.path(), &item).unwrap();
        assert!(pending_path(dir.path(), "one").exists());

        item.status = TestStatus::Done;
        item.title = "terminal".to_owned();
        write(dir.path(), &item).unwrap();

        assert!(!pending_path(dir.path(), "one").exists());
        assert!(terminal_path(dir.path(), "one").exists());
        assert!(
            list_pending_raw::<TestRecord>(dir.path())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            load::<TestRecord>(dir.path(), "one")
                .unwrap()
                .unwrap()
                .status,
            TestStatus::Done
        );

        let mut second = record("two", TestStatus::Pending, "pending");
        write(dir.path(), &second).unwrap();
        second.status = TestStatus::Done;
        second.title = "older terminal copy".to_owned();
        write(dir.path(), &second).unwrap();
        second.title = "newer pending-side copy".to_owned();
        std::fs::write(
            pending_path(dir.path(), "two"),
            serde_json::to_vec(&second).unwrap(),
        )
        .unwrap();

        let mut all = list_all::<TestRecord>(dir.path()).unwrap();
        all.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].title, "terminal");
        assert_eq!(all[1].title, "newer pending-side copy");
    }

    #[test]
    fn prune_terminal_removes_only_old_terminal_files() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            &record("pending", TestStatus::Pending, "pending"),
        )
        .unwrap();
        write(dir.path(), &record("old", TestStatus::Done, "old")).unwrap();
        write(dir.path(), &record("fresh", TestStatus::Done, "fresh")).unwrap();
        let old_path = terminal_path(dir.path(), "old");
        let old = SystemTime::now() - Duration::from_secs(3_600);
        std::fs::File::open(&old_path)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let report = prune_terminal(dir.path(), Duration::from_secs(60)).unwrap();

        assert_eq!(report.files_removed, 1);
        assert!(!old_path.exists());
        assert!(terminal_path(dir.path(), "fresh").exists());
        assert!(pending_path(dir.path(), "pending").exists());
    }
}
