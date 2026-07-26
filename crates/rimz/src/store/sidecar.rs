//! Shared latest-wins JSON sidecar store for display enrichment.
//!
//! Sidecars are cache-class files: writers publish by temp-file plus rename,
//! with no fsync, because the payload is rebuilt from provider/session state
//! and durable truth stays in the store. File names digest
//! `(kind, agent_id)` so a path-hostile session id maps to a safe fixed-width
//! name; readers still confirm the on-disk key on direct lookups. Long-lived
//! consumers keep a per-thread `(mtime, len)` parse cache, capping steady-state
//! scans at one stat per file per tick.

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

struct ParsedSidecar<R> {
    mtime: SystemTime,
    len: u64,
    record: Option<R>,
}

pub(crate) struct ParseCache<R>(std::cell::RefCell<HashMap<PathBuf, ParsedSidecar<R>>>);

impl<R> Default for ParseCache<R> {
    fn default() -> Self {
        Self(std::cell::RefCell::new(HashMap::new()))
    }
}

pub(crate) fn digest(kind: &str, agent_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(agent_id.as_bytes());
    let digest = hex::encode(hasher.finalize());
    digest[..32].to_owned()
}

pub(crate) fn path(dir: &Path, prefix: &str, kind: &str, agent_id: &str) -> PathBuf {
    dir.join(format!("{prefix}.{}.json", digest(kind, agent_id)))
}

fn lock_path(dir: &Path, prefix: &str, kind: &str, agent_id: &str) -> PathBuf {
    path(dir, prefix, kind, agent_id).with_extension("lock")
}

/// Per-record advisory lock shared by sidecar writers that need an atomic
/// read-modify-write across independent CLI processes.
struct RecordLock {
    file: File,
}

impl RecordLock {
    fn acquire(
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

#[cfg(test)]
fn write_record<R: SidecarRecord>(dir: &Path, record: &R) -> Result<(), atomic::AtomicErr> {
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

/// Mutate one record against its latest published bytes under the canonical
/// per-record lock. Missing, malformed, or key-mismatched bytes use `default`;
/// returning `false` leaves the file untouched.
pub(crate) fn update<R: SidecarRecord>(
    dir: &Path,
    kind: &str,
    agent_id: &str,
    default: impl FnOnce() -> R,
    apply: impl FnOnce(&mut R, bool) -> bool,
) -> Result<bool, atomic::AtomicErr> {
    let _lock = RecordLock::acquire(dir, R::FILE_PREFIX, kind, agent_id)?;
    let prior = read_one(dir, kind, agent_id);
    let existed = prior.is_some();
    let mut record = prior.unwrap_or_else(default);
    if !apply(&mut record, existed) {
        return Ok(false);
    }
    atomic::write_temp_then_rename_cache(&path(dir, R::FILE_PREFIX, kind, agent_id), &record)?;
    Ok(true)
}

pub(crate) fn remove_locked<R: SidecarRecord>(
    dir: &Path,
    kind: &str,
    agent_id: &str,
) -> Result<(), atomic::AtomicErr> {
    let _lock = RecordLock::acquire(dir, R::FILE_PREFIX, kind, agent_id)?;
    let record_path = path(dir, R::FILE_PREFIX, kind, agent_id);
    match fs::remove_file(&record_path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(atomic::AtomicErr::Io {
            path: record_path,
            source,
        }),
    }
}

pub(crate) fn read_all<R: SidecarRecord>(dir: &Path, cache: &ParseCache<R>) -> Vec<R> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cache = cache.0.borrow_mut();
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

/// Read only requested live identities, deduplicating their canonical paths.
/// Each unchanged `(mtime, len)` serves the per-thread cached parse, including
/// cached failures; dropped keys and vanished files leave no cache entry.
pub(crate) fn read_for_keys<'a, R: SidecarRecord>(
    dir: &Path,
    keys: impl IntoIterator<Item = (&'a str, &'a str)>,
    cache: &ParseCache<R>,
) -> Vec<R> {
    let mut seen = BTreeSet::new();
    let mut cache = cache.0.borrow_mut();
    let records = keys
        .into_iter()
        .filter_map(|(kind, agent_id)| {
            let record_path = path(dir, R::FILE_PREFIX, kind, agent_id);
            if !seen.insert(record_path.clone()) {
                return None;
            }
            let Ok(meta) = fs::metadata(&record_path) else {
                cache.remove(&record_path);
                return None;
            };
            let Ok(mtime) = meta.modified() else {
                cache.remove(&record_path);
                return None;
            };
            let len = meta.len();
            let record = match cache.get(&record_path) {
                Some(parsed) if parsed.mtime == mtime && parsed.len == len => parsed.record.clone(),
                _ => {
                    let record = fs::read(&record_path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                    cache.insert(
                        record_path,
                        ParsedSidecar {
                            mtime,
                            len,
                            record: record.clone(),
                        },
                    );
                    record
                }
            }?;
            (record.kind() == kind && record.agent_id() == agent_id).then_some(record)
        })
        .collect();
    cache.retain(|path, _| seen.contains(path));
    records
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tempfile::tempdir;

    thread_local! {
        static TEST_PARSE_CACHE: ParseCache<TestRecord> = ParseCache::default();
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

    fn read_keys_test<'a>(
        dir: &Path,
        keys: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<TestRecord> {
        TEST_PARSE_CACHE.with(|cache| read_for_keys(dir, keys, cache))
    }

    #[test]
    fn digest_and_path_preserve_sidecar_names() {
        assert_eq!(
            digest("codex", "sess-1"),
            "8b13d7a2e1e761f29386b7f853b83f17"
        );
        assert_eq!(
            path(Path::new("/tmp/cache"), "test", "codex", "sess-1"),
            Path::new("/tmp/cache/test.8b13d7a2e1e761f29386b7f853b83f17.json")
        );
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

        remove_locked::<TestRecord>(dir.path(), "codex", "sess-1").unwrap();

        let ids: Vec<_> = read_all_test(dir.path())
            .into_iter()
            .map(|record| record.agent_id)
            .collect();
        assert_eq!(ids, vec!["sess-2".to_owned()]);
        remove_locked::<TestRecord>(dir.path(), "codex", "sess-1").unwrap();
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

    #[test]
    fn locked_update_creates_defaults_skips_no_ops_and_reads_latest_bytes() {
        let dir = tempdir().unwrap();
        assert!(
            !update(
                dir.path(),
                "codex",
                "sess-1",
                || record("sess-1", 1),
                |_, existed| {
                    assert!(!existed);
                    false
                },
            )
            .unwrap()
        );
        assert!(!path(dir.path(), "test", "codex", "sess-1").exists());

        assert!(
            update(
                dir.path(),
                "codex",
                "sess-1",
                || record("sess-1", 1),
                |record, existed| {
                    assert!(!existed);
                    record.note = "created".to_owned();
                    true
                },
            )
            .unwrap()
        );
        assert_eq!(
            read_one::<TestRecord>(dir.path(), "codex", "sess-1")
                .unwrap()
                .note,
            "created"
        );

        assert_eq!(read_all_test(dir.path())[0].note, "created");
        let record_path = path(dir.path(), "test", "codex", "sess-1");
        let mut latest = read_one::<TestRecord>(dir.path(), "codex", "sess-1").unwrap();
        latest.note = "direct!".to_owned();
        std::fs::write(&record_path, serde_json::to_vec(&latest).unwrap()).unwrap();
        assert!(
            update(
                dir.path(),
                "codex",
                "sess-1",
                || record("sess-1", 2),
                |record, existed| {
                    assert!(existed);
                    record.note.push_str(" updated");
                    true
                },
            )
            .unwrap()
        );
        assert_eq!(
            read_one::<TestRecord>(dir.path(), "codex", "sess-1")
                .unwrap()
                .note,
            "direct! updated"
        );
    }

    #[test]
    fn keyed_reads_dedupe_validate_and_evict_dead_keys() {
        let dir = tempdir().unwrap();
        write_record(dir.path(), &record("sess-1", 1)).unwrap();
        write_record(dir.path(), &record("sess-2", 2)).unwrap();
        assert_eq!(
            read_keys_test(
                dir.path(),
                [
                    ("codex", "sess-1"),
                    ("codex", "sess-1"),
                    ("codex", "sess-2"),
                ],
            )
            .len(),
            2
        );

        let wrong_path = path(dir.path(), "test", "codex", "wrong");
        std::fs::write(
            &wrong_path,
            serde_json::to_vec(&record("different", 3)).unwrap(),
        )
        .unwrap();
        assert!(read_keys_test(dir.path(), [("codex", "wrong")]).is_empty());

        read_keys_test(dir.path(), [("codex", "sess-1"), ("codex", "sess-2")]);
        read_keys_test(dir.path(), [("codex", "sess-1")]);
        let sess_2_path = path(dir.path(), "test", "codex", "sess-2");
        std::fs::write(&sess_2_path, b"malformed").unwrap();
        assert!(
            read_keys_test(dir.path(), [("codex", "sess-2")]).is_empty(),
            "a dropped key must not retain its prior cached parse"
        );
    }

    #[test]
    fn keyed_reads_cache_success_and_failure_at_the_same_stamp() {
        let dir = tempdir().unwrap();
        write_record(dir.path(), &record("sess-1", 1)).unwrap();
        let record_path = path(dir.path(), "test", "codex", "sess-1");
        assert_eq!(read_keys_test(dir.path(), [("codex", "sess-1")]).len(), 1);
        let original = std::fs::read(&record_path).unwrap();
        let mtime = std::fs::metadata(&record_path).unwrap().modified().unwrap();
        let swapped = String::from_utf8(original)
            .unwrap()
            .replace("cached", "direct");
        std::fs::write(&record_path, swapped).unwrap();
        File::options()
            .write(true)
            .open(&record_path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        assert_eq!(
            read_keys_test(dir.path(), [("codex", "sess-1")])[0].note,
            "cached",
            "same-stamp success serves the cached parse"
        );

        let malformed_path = path(dir.path(), "test", "codex", "sess-bad");
        let valid = serde_json::to_vec(&record("sess-bad", 2)).unwrap();
        let mut malformed = valid.clone();
        malformed[0] = b'!';
        std::fs::write(&malformed_path, &malformed).unwrap();
        let mtime = std::fs::metadata(&malformed_path)
            .unwrap()
            .modified()
            .unwrap();
        assert!(read_keys_test(dir.path(), [("codex", "sess-bad")]).is_empty());
        std::fs::write(&malformed_path, valid).unwrap();
        File::options()
            .write(true)
            .open(&malformed_path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        assert!(
            read_keys_test(dir.path(), [("codex", "sess-bad")]).is_empty(),
            "same-stamp malformed bytes stay negative-cached"
        );
    }
}
