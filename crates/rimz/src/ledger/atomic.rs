//! Disk-write primitives.
//!
//! Two write shapes cover every disk write in the project:
//!
//! - [`write_temp_then_rename`] (cold-path durable) and
//!   [`write_temp_then_rename_cache`] (rename-atomic, no fsync) for whole
//!   files.
//! - [`append_record_bytes`] for the event log — one `write()` per record,
//!   no fsync; durability rides the write tail's debounced [`sync_file_data`]
//!   group barrier and the pre-rotation sync.
//!
//! No module hand-rolls its own atomic dance, and every fsync syscall in the
//! project lives in this file (CI grep), counted through [`testkit`]. Frame
//! *encoding* lives with its decoder in [`crate::ledger::event_log`]; this
//! module owns the syscall discipline alone.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AtomicErr {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, AtomicErr>;

/// Write raw bytes to `path` via a same-directory temp file followed by an
/// atomic rename. fsync is applied to the temp file before the rename.
/// Used by writers (TOML, anything pre-serialised) that own their own
/// encoding; JSON callers prefer [`write_temp_then_rename`].
#[must_use = "durability barrier; check the result"]
pub fn write_bytes_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AtomicErr::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let tmp = temp_sibling(path);
    let mut temp_guard = TempFileGuard::new(tmp.clone());
    {
        let mut file = File::create(&tmp).map_err(|e| AtomicErr::Io {
            path: tmp.clone(),
            source: e,
        })?;
        file.write_all(bytes).map_err(|e| AtomicErr::Io {
            path: tmp.clone(),
            source: e,
        })?;
        testkit::count_fsync();
        file.sync_all().map_err(|e| AtomicErr::Io {
            path: tmp.clone(),
            source: e,
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|e| AtomicErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    temp_guard.disarm();
    sync_parent_dir(path)?;
    Ok(())
}

/// Whether a temp+rename write fsyncs before it becomes observable.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fsync {
    /// fsync the temp file and the parent dir — survives power loss. For
    /// cold-path durable state: trust grants, resolver allowlists, workspace
    /// records, hook installs, the rotation carryover. Cold paths keep the
    /// fsync even where the same-host argument would allow relaxing it,
    /// because removing it there buys nothing.
    Durable,
    /// Skip both fsyncs. The rename stays atomic (a reader never sees a torn
    /// file), but the write is not crash-durable.
    Skip,
}

struct TempFileGuard {
    path: PathBuf,
    active: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        TempFileGuard { path, active: true }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Write `value` as pretty JSON to `path` via a same-directory temp file
/// followed by an atomic rename. fsync is applied to the temp file before
/// the rename. Caller has already created `path.parent()`.
#[must_use = "durability barrier; check the result"]
pub fn write_temp_then_rename<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_temp_then_rename_with(path, value, Fsync::Durable)
}

/// Like [`write_temp_then_rename`] but skips the temp-file and parent-dir
/// fsyncs. For everything whose correctness rides rename atomicity rather
/// than crash durability: feed files (CAS state re-checked under the
/// workspace lock, with the event log as the audit trail), liveness files
/// (sidebar heartbeats, agent activity), and rebuilt-on-next-tick caches
/// (snapshots, diff stats, the agent-context sidecar). Two fsyncs per write
/// add disk latency to paths the UI (or a hook) waits on, and for these
/// files "survives a power cut" buys nothing — the rename is still atomic,
/// so a reader never sees a torn file.
pub fn write_temp_then_rename_cache<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_temp_then_rename_with(path, value, Fsync::Skip)
}

fn write_temp_then_rename_with<T: Serialize>(path: &Path, value: &T, fsync: Fsync) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AtomicErr::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let tmp = temp_sibling(path);
    let mut temp_guard = TempFileGuard::new(tmp.clone());
    {
        let mut file = File::create(&tmp).map_err(|e| AtomicErr::Io {
            path: tmp.clone(),
            source: e,
        })?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n").map_err(|e| AtomicErr::Io {
            path: tmp.clone(),
            source: e,
        })?;
        if fsync == Fsync::Durable {
            testkit::count_fsync();
            file.sync_all().map_err(|e| AtomicErr::Io {
                path: tmp.clone(),
                source: e,
            })?;
        }
    }
    std::fs::rename(&tmp, path).map_err(|e| AtomicErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    temp_guard.disarm();
    if fsync == Fsync::Durable {
        sync_parent_dir(path)?;
    }
    Ok(())
}

/// Append one pre-encoded record to `path` with a single `write()` call.
///
/// The frame encoding (and its decoder) live in
/// [`crate::ledger::event_log`]; this owns only the append discipline: one
/// `write()` call per record so a partial write doesn't fragment, and a
/// parent-dir sync when the append creates the file (file *existence* stays
/// durable). The record itself carries no fsync — appended bytes ride the
/// page cache until the write tail's debounced [`sync_file_data`] group
/// barrier, or the pre-rename sync in [`crate::ledger::event_log::rotate`].
/// Recovery in [`crate::ledger::event_log::read_all`] tolerates a torn
/// trailing record, and the frame CRC makes a power-cut's lost writeback
/// read as deterministic corruption for `repair` to truncate.
#[must_use = "durability barrier; check the result"]
pub fn append_record_bytes(path: &Path, line: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AtomicErr::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let first_create = !path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| AtomicErr::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    file.write_all(line).map_err(|e| AtomicErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    if first_create {
        sync_parent_dir(path)?;
    }
    Ok(())
}

/// fdatasync `path` — the group-commit barrier for relaxed appends. One call
/// flushes every dirty page on the inode regardless of which process wrote
/// them, so a single caller per interval makes the whole fleet's appends
/// durable.
#[must_use = "durability barrier; check the result"]
pub fn sync_file_data(path: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| AtomicErr::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    testkit::count_fsync();
    file.sync_data().map_err(|e| AtomicErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

/// Truncate `path` to `len` bytes and fsync — the event-log repair
/// primitive, cutting a corrupt suffix at a frame boundary. Caller owns the
/// write serialization point (the workspace lock).
#[must_use = "durability barrier; check the result"]
pub fn truncate_file(path: &Path, len: u64) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| AtomicErr::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    file.set_len(len).map_err(|e| AtomicErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    testkit::count_fsync();
    file.sync_data().map_err(|e| AtomicErr::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

fn sync_parent_dir(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    sync_dir(parent)
}

/// Remove temp siblings old enough that they cannot be an active write by this
/// process family. Callers use this on large rebuilt caches, where process death
/// can otherwise leave expensive orphan temp files behind.
pub fn sweep_stale_temp_siblings(path: &Path, min_age: Duration) -> usize {
    let Some(parent) = path.parent() else {
        return 0;
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return 0;
    };
    let prefix = format!("{file_name}.tmp.");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return 0;
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with(&prefix) {
            continue;
        }
        let old_enough = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= min_age);
        if old_enough && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// fsync a directory so its rename/unlink operations are durable.
pub fn sync_dir(dir: &Path) -> Result<()> {
    let handle = File::open(dir).map_err(|e| AtomicErr::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;
    testkit::count_fsync();
    handle.sync_all().map_err(|e| AtomicErr::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

/// Test-only observability seam. Every fsync syscall in the project funnels
/// through this module (CI grep), so one relaxed counter beside each sync
/// site lets the performance tier prove "zero fsyncs on the hot path" from
/// the integration binary, where `cfg(test)` statics in this crate are
/// invisible. An uncontended relaxed `fetch_add` beside a syscall costs
/// nothing; the counter is per-process, and nextest's process-per-test model
/// keeps readings isolated.
#[doc(hidden)]
pub mod testkit {
    use std::sync::atomic::{AtomicU64, Ordering};

    static FSYNCS: AtomicU64 = AtomicU64::new(0);

    /// File and directory fsync syscalls issued since process start.
    pub fn fsync_count() -> u64 {
        FSYNCS.load(Ordering::Relaxed)
    }

    pub(super) fn count_fsync() {
        FSYNCS.fetch_add(1, Ordering::Relaxed);
    }
}

fn temp_sibling(path: &Path) -> PathBuf {
    let pid = std::process::id();
    let nonce = uuid::Uuid::now_v7().simple();
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(format!(".tmp.{pid}.{nonce}"));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn temp_rename_writes_pretty_json_with_trailing_newline() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested/file.json");
        write_temp_then_rename(&path, &json!({ "a": 1, "b": "two" })).unwrap();
        let read = std::fs::read_to_string(&path).unwrap();
        assert!(read.ends_with('\n'));
        let parsed: serde_json::Value = serde_json::from_str(&read).unwrap();
        assert_eq!(parsed["a"], 1);
    }

    #[test]
    fn failed_temp_rename_cleans_its_temp_file() {
        struct Broken;

        impl serde::Serialize for Broken {
            fn serialize<S>(&self, _serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                Err(serde::ser::Error::custom("boom"))
            }
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("file.json");

        let _ = write_temp_then_rename_cache(&path, &Broken).unwrap_err();

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("file.json.tmp."))
            })
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn stale_temp_sibling_sweep_keeps_other_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("file.json");
        let stale = dir.path().join("file.json.tmp.1.dead");
        let other = dir.path().join("other.json.tmp.1.dead");
        std::fs::write(&stale, b"stale").unwrap();
        std::fs::write(&other, b"other").unwrap();

        let removed = sweep_stale_temp_siblings(&path, Duration::ZERO);

        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(other.exists());
    }

    #[test]
    fn append_record_bytes_appends_verbatim() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        append_record_bytes(&path, b"first\n").unwrap();
        append_record_bytes(&path, b"second\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first\nsecond\n");
    }

    #[test]
    fn truncate_file_cuts_to_the_requested_length() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        append_record_bytes(&path, b"keep\ncut\n").unwrap();
        truncate_file(&path, 5).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"keep\n");
    }
}
