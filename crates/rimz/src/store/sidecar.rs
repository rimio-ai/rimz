//! Shared latest-wins JSON sidecar store for display enrichment.
//!
//! Sidecars are cache-class files: writers publish by temp-file plus rename,
//! with no fsync, because the payload is rebuilt from provider/session state
//! and durable truth stays in the store. File names digest
//! `(kind, agent_id)` so a path-hostile session id maps to a safe fixed-width
//! name; readers still confirm the on-disk key on direct lookups. Long-lived
//! consumers keep a per-thread `(mtime, len)` parse cache, capping steady-state
//! scans at one stat per file per tick.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use crate::store::atomic;

pub(crate) trait SidecarRecord: Serialize + DeserializeOwned + Clone {
    const FILE_PREFIX: &'static str;

    fn kind(&self) -> &str;
    fn agent_id(&self) -> &str;
}

pub(crate) struct ParsedSidecar<R> {
    pub mtime: SystemTime,
    pub len: u64,
    pub record: Option<R>,
}

pub(crate) fn path(dir: &Path, prefix: &str, kind: &str, agent_id: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(agent_id.as_bytes());
    let digest = hex::encode(hasher.finalize());
    dir.join(format!("{prefix}.{}.json", &digest[..32]))
}

pub(crate) fn lock_path(dir: &Path, prefix: &str, kind: &str, agent_id: &str) -> PathBuf {
    path(dir, prefix, kind, agent_id).with_extension("lock")
}

/// Per-record advisory lock shared by sidecar writers that need an atomic
/// read-modify-write across independent CLI processes.
pub(crate) struct RecordLock {
    file: File,
}

impl RecordLock {
    pub(crate) fn acquire(
        dir: &Path,
        prefix: &str,
        kind: &str,
        agent_id: &str,
    ) -> Result<Self, atomic::AtomicErr> {
        std::fs::create_dir_all(dir).map_err(|source| atomic::AtomicErr::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = lock_path(dir, prefix, kind, agent_id);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| atomic::AtomicErr::Io {
                path: path.clone(),
                source,
            })?;
        file.lock()
            .map_err(|source| atomic::AtomicErr::Io { path, source })?;
        Ok(Self { file })
    }
}

impl Drop for RecordLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(crate) fn write_record<R: SidecarRecord>(
    dir: &Path,
    record: &R,
) -> Result<(), atomic::AtomicErr> {
    atomic::write_temp_then_rename_cache(
        &path(dir, R::FILE_PREFIX, record.kind(), record.agent_id()),
        record,
    )
}

pub(crate) fn read_one<R: SidecarRecord>(dir: &Path, kind: &str, agent_id: &str) -> Option<R> {
    let path = path(dir, R::FILE_PREFIX, kind, agent_id);
    let record: R = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())?;
    (record.kind() == kind && record.agent_id() == agent_id).then_some(record)
}

pub(crate) fn remove<R: SidecarRecord>(
    dir: &Path,
    kind: &str,
    agent_id: &str,
) -> std::io::Result<()> {
    match fs::remove_file(path(dir, R::FILE_PREFIX, kind, agent_id)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

pub(crate) fn read_all<R: SidecarRecord>(
    dir: &Path,
    cache: &RefCell<HashMap<PathBuf, ParsedSidecar<R>>>,
) -> Vec<R> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cache = cache.borrow_mut();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        let len = meta.len();
        seen.insert(path.clone());
        let record = match cache.get(&path) {
            Some(parsed) if parsed.mtime == mtime && parsed.len == len => parsed.record.clone(),
            _ => {
                let record = fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                cache.insert(
                    path,
                    ParsedSidecar {
                        mtime,
                        len,
                        record: record.clone(),
                    },
                );
                record
            }
        };
        let Some(record) = record else { continue };
        out.push(record);
    }
    cache.retain(|path, _| seen.contains(path));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tempfile::tempdir;

    thread_local! {
        static TEST_PARSE_CACHE: RefCell<HashMap<PathBuf, ParsedSidecar<TestRecord>>> =
            RefCell::new(HashMap::new());
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct TestRecord {
        kind: String,
        agent_id: String,
        observed_at: i64,
        note: String,
    }

    impl SidecarRecord for TestRecord {
        const FILE_PREFIX: &'static str = "test";

        fn kind(&self) -> &str {
            &self.kind
        }

        fn agent_id(&self) -> &str {
            &self.agent_id
        }
    }

    fn record(agent_id: &str, observed_at: i64) -> TestRecord {
        TestRecord {
            kind: "codex".to_owned(),
            agent_id: agent_id.to_owned(),
            observed_at,
            note: "cached".to_owned(),
        }
    }

    fn read_all_test(dir: &Path) -> Vec<TestRecord> {
        TEST_PARSE_CACHE.with(|cache| read_all(dir, cache))
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempdir().unwrap();
        let record = record("sess-1", 1_700_000_000);

        write_record(dir.path(), &record).unwrap();

        assert_eq!(
            read_one::<TestRecord>(dir.path(), "codex", "sess-1"),
            Some(record)
        );
    }

    #[test]
    fn corrupt_file_is_skipped() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("test.bogus.json"), b"not json").unwrap();

        assert!(read_all_test(dir.path()).is_empty());
    }

    #[test]
    fn old_record_is_served_liveness_gating_is_the_rollups_job() {
        let dir = tempdir().unwrap();
        write_record(dir.path(), &record("sess-old", 0)).unwrap();

        assert_eq!(read_all_test(dir.path())[0].agent_id, "sess-old");
    }

    #[test]
    fn unchanged_stat_skips_the_reparse() {
        let dir = tempdir().unwrap();
        write_record(dir.path(), &record("sess-1", 1_700_000_000)).unwrap();
        assert_eq!(read_all_test(dir.path())[0].agent_id, "sess-1");

        let path = path(dir.path(), TestRecord::FILE_PREFIX, "codex", "sess-1");
        let original = std::fs::read(&path).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let swapped = String::from_utf8(original)
            .unwrap()
            .replace("sess-1", "sess-9");
        std::fs::write(&path, swapped).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(mtime).unwrap();
        drop(file);

        assert_eq!(
            read_all_test(dir.path())[0].agent_id,
            "sess-1",
            "same (mtime, len) serves the cached parse"
        );

        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(mtime + std::time::Duration::from_secs(3))
            .unwrap();
        drop(file);
        assert_eq!(read_all_test(dir.path())[0].agent_id, "sess-9");
    }

    #[test]
    fn remove_targets_one_key() {
        let dir = tempdir().unwrap();
        write_record(dir.path(), &record("sess-1", 1_700_000_000)).unwrap();
        write_record(dir.path(), &record("sess-2", 1_700_000_000)).unwrap();

        remove::<TestRecord>(dir.path(), "codex", "sess-1").unwrap();

        let ids: Vec<_> = read_all_test(dir.path())
            .into_iter()
            .map(|record| record.agent_id)
            .collect();
        assert_eq!(ids, vec!["sess-2".to_owned()]);
        remove::<TestRecord>(dir.path(), "codex", "sess-1").unwrap();
    }

    #[test]
    fn read_one_reads_directly_without_the_parse_cache() {
        let dir = tempdir().unwrap();
        write_record(dir.path(), &record("sess-1", 1_700_000_000)).unwrap();
        assert_eq!(read_all_test(dir.path())[0].note, "cached");

        let path = path(dir.path(), TestRecord::FILE_PREFIX, "codex", "sess-1");
        let original = std::fs::read(&path).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let swapped = String::from_utf8(original)
            .unwrap()
            .replace("cached", "direct");
        std::fs::write(&path, swapped).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(mtime).unwrap();
        drop(file);

        assert_eq!(
            read_one::<TestRecord>(dir.path(), "codex", "sess-1")
                .unwrap()
                .note,
            "direct",
            "direct reads bypass the stat-keyed parse cache"
        );
        assert_eq!(
            read_all_test(dir.path())[0].note,
            "cached",
            "same (mtime, len) still serves the cached parse"
        );
    }
}
