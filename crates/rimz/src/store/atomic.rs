//! Disk-write primitives and disk hygiene sweeps.
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
//! *encoding* lives with its decoder in [`crate::store::event_log`]; this
//! module owns the syscall discipline alone.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[derive(Debug, thiserror::Error)]
pub enum AtomicErr {
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, AtomicErr>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PruneOutcome {
    pub files_removed: usize,
    pub bytes_removed: u64,
}

/// Write raw bytes to `path` via a same-directory temp file followed by an
/// atomic rename. fsync is applied to the temp file before the rename.
/// Used by writers (TOML, anything pre-serialised) that own their own
/// encoding; JSON callers prefer [`write_temp_then_rename`].
#[must_use = "durability barrier; check the result"]
pub fn write_bytes_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    replace_whole_file(path, Fsync::Durable, false, |writer, tmp| {
        writer.write_all(bytes).map_err(|source| AtomicErr::Io {
            path: tmp.to_path_buf(),
            source,
        })
    })
}

/// Whether a temp+rename write fsyncs before it becomes observable.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fsync {
    /// fsync the temp file and the parent dir — survives power loss. For
    /// cold-path durable state: trust grants, notification handlers, workspace
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
    write_temp_then_rename_with(path, value, Fsync::Durable, JsonStyle::Pretty, false)
}

/// Like [`write_temp_then_rename`], but the temp file is created and renamed
/// with mode 0600. Used for plaintext secret caches.
#[must_use = "durability barrier; check the result"]
pub fn write_private_temp_then_rename<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_temp_then_rename_with(path, value, Fsync::Durable, JsonStyle::Pretty, true)
}

/// Like [`write_temp_then_rename`] but skips the temp-file and parent-dir
/// fsyncs. For everything whose correctness rides rename atomicity rather
/// than crash durability: liveness files (sidebar heartbeats, agent activity),
/// and rebuilt-on-next-tick caches
/// (snapshots, diff stats, the agent-context sidecar). Two fsyncs per write
/// add disk latency to paths the UI (or a hook) waits on, and for these
/// files "survives a power cut" buys nothing — the rename is still atomic,
/// so a reader never sees a torn file.
pub fn write_temp_then_rename_cache<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_temp_then_rename_with(path, value, Fsync::Skip, JsonStyle::Pretty, false)
}

/// Like [`write_temp_then_rename_cache`] but emits compact JSON. Use for large
/// rebuilt caches where human-readable formatting materially affects size.
pub fn write_temp_then_rename_cache_compact<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_temp_then_rename_with(path, value, Fsync::Skip, JsonStyle::Compact, false)
}

#[derive(Clone, Copy)]
enum JsonStyle {
    Pretty,
    Compact,
}

fn write_temp_then_rename_with<T: Serialize>(
    path: &Path,
    value: &T,
    fsync: Fsync,
    style: JsonStyle,
    private: bool,
) -> Result<()> {
    replace_whole_file(path, fsync, private, |writer, tmp| {
        match style {
            JsonStyle::Pretty => serde_json::to_writer_pretty(&mut *writer, value)?,
            JsonStyle::Compact => serde_json::to_writer(&mut *writer, value)?,
        }
        writer.write_all(b"\n").map_err(|source| AtomicErr::Io {
            path: tmp.to_path_buf(),
            source,
        })
    })
}

fn replace_whole_file(
    path: &Path,
    fsync: Fsync,
    private: bool,
    encode: impl FnOnce(&mut BufWriter<File>, &Path) -> Result<()>,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AtomicErr::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let tmp = temp_sibling(path);
    let mut temp_guard = TempFileGuard::new(tmp.clone());
    {
        let file = create_temp_file(&tmp, private).map_err(|e| AtomicErr::Io {
            path: tmp.clone(),
            source: e,
        })?;
        let mut writer = BufWriter::new(file);
        encode(&mut writer, &tmp)?;
        let file = writer.into_inner().map_err(|e| AtomicErr::Io {
            path: tmp.clone(),
            source: e.into_error(),
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

fn create_temp_file(path: &Path, private: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if private {
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    if private {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// Append one pre-encoded record to `path` with a single `write()` call.
///
/// The frame encoding (and its decoder) live in
/// [`crate::store::event_log`]; this owns only the append discipline: one
/// `write()` call per record so a partial write doesn't fragment, and a
/// parent-dir sync when the append creates the file (file *existence* stays
/// durable). The record itself carries no fsync — appended bytes ride the
/// page cache until the write tail's debounced [`sync_file_data`] group
/// barrier, or the pre-rename sync in [`crate::store::event_log::rotate`].
/// Recovery in [`crate::store::event_log::read_all`] tolerates a torn
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

fn is_orphan_temp_name(name: &str) -> bool {
    let Some(idx) = name.rfind(".tmp.") else {
        return false;
    };
    if idx == 0 {
        return false;
    }
    let tail = &name[idx + ".tmp.".len()..];
    let Some((pid, nonce)) = tail.split_once('.') else {
        return false;
    };
    !pid.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit())
        && nonce.len() == 32
        && nonce.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Recursively remove orphaned whole-file-write temps under `root`.
///
/// These are same-directory siblings created by [`temp_sibling`] before a
/// rename. A hard kill can leave them behind. Only files older than `min_age`
/// are removed, so an in-flight write stays intact.
pub fn sweep_orphan_temps_under(root: &Path, min_age: Duration, dry_run: bool) -> (usize, u64) {
    let mut stack = vec![root.to_path_buf()];
    let now = SystemTime::now();
    let mut files_removed = 0usize;
    let mut bytes_removed = 0u64;

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(std::result::Result::ok) {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !is_orphan_temp_name(name) {
                continue;
            }
            let Some(metadata) = std::fs::symlink_metadata(&path).ok() else {
                continue;
            };
            let old_enough = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= min_age);
            if old_enough && (dry_run || std::fs::remove_file(&path).is_ok()) {
                files_removed += 1;
                bytes_removed = bytes_removed.saturating_add(metadata.len());
            }
        }
    }

    (files_removed, bytes_removed)
}

/// Remove old files under `dir` when `keep` selects them for this sweep.
#[must_use = "maintenance report; surface it to the caller"]
pub fn prune_old_files(
    dir: &Path,
    older_than: Duration,
    keep: impl Fn(&Path) -> bool,
) -> Result<PruneOutcome> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(PruneOutcome::default());
        }
        Err(source) => {
            return Err(AtomicErr::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };

    let now = SystemTime::now();
    let mut report = PruneOutcome::default();
    for entry in entries {
        let entry = entry.map_err(|source| AtomicErr::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !keep(&path) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(|source| AtomicErr::Io {
            path: path.clone(),
            source,
        })?;
        let modified = metadata.modified().map_err(|source| AtomicErr::Io {
            path: path.clone(),
            source,
        })?;
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age < older_than {
            continue;
        }
        let bytes = metadata.len();
        std::fs::remove_file(&path).map_err(|source| AtomicErr::Io {
            path: path.clone(),
            source,
        })?;
        report.files_removed += 1;
        report.bytes_removed = report.bytes_removed.saturating_add(bytes);
    }
    Ok(report)
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
    fn temp_rename_cache_compact_writes_single_line_json() {
        #[derive(Serialize)]
        struct Sample {
            a: u8,
            b: &'static str,
        }

        let dir = tempdir().unwrap();
        let path = dir.path().join("cache.json");
        write_temp_then_rename_cache_compact(&path, &Sample { a: 1, b: "two" }).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"a":1,"b":"two"}"#.to_owned() + "\n"
        );
    }

    #[test]
    fn raw_atomic_write_preserves_bytes_without_newline() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("raw.bin");

        write_bytes_atomically(&path, b"raw bytes").unwrap();

        assert_eq!(std::fs::read(path).unwrap(), b"raw bytes");
    }

    #[test]
    fn durable_and_cache_replacements_keep_fsync_classes() {
        let dir = tempdir().unwrap();
        let before = testkit::fsync_count();
        write_temp_then_rename(&dir.path().join("durable.json"), &json!({ "a": 1 })).unwrap();
        let durable = testkit::fsync_count();
        write_temp_then_rename_cache(&dir.path().join("cache.json"), &json!({ "a": 1 })).unwrap();

        assert_eq!(durable - before, 2, "temp file and parent dir sync");
        assert_eq!(
            testkit::fsync_count(),
            durable,
            "cache replacement skips sync"
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_replacement_enforces_owner_only_mode() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("private.json");

        write_private_temp_then_rename(&path, &json!({ "private": true })).unwrap();

        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
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
    fn sweep_orphan_temps_removes_matching_recursively() {
        let dir = tempdir().unwrap();
        let nonce = "00000000000000000000000000000000";
        let stale_root = dir.path().join(format!("spending.json.tmp.1.{nonce}"));
        let subdir = dir.path().join("nested");
        std::fs::create_dir_all(&subdir).unwrap();
        let stale_nested = subdir.join(format!("rollup.json.tmp.2.{nonce}"));
        let fresh = subdir.join(format!("workspace.json.tmp.3.{nonce}"));
        let keep = subdir.join("workspace.json");
        for path in [&stale_root, &stale_nested, &fresh, &keep] {
            std::fs::write(path, b"temp").unwrap();
        }
        let old = SystemTime::now() - Duration::from_secs(7200);
        for path in [&stale_root, &stale_nested] {
            std::fs::File::open(path)
                .unwrap()
                .set_modified(old)
                .unwrap();
        }

        let (files, bytes) = sweep_orphan_temps_under(dir.path(), Duration::from_secs(3600), false);

        assert_eq!(files, 2);
        assert_eq!(bytes, 8);
        assert!(!stale_root.exists());
        assert!(!stale_nested.exists());
        assert!(fresh.exists());
        assert!(keep.exists());
    }

    #[test]
    fn sweep_orphan_temps_dry_run_counts_without_removing() {
        let dir = tempdir().unwrap();
        let nonce = "00000000000000000000000000000000";
        let stale = dir.path().join(format!("spending.json.tmp.1.{nonce}"));
        std::fs::write(&stale, b"temp").unwrap();
        std::fs::File::open(&stale)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(7200))
            .unwrap();

        let (files, bytes) = sweep_orphan_temps_under(dir.path(), Duration::from_secs(3600), true);

        assert_eq!((files, bytes), (1, 4));
        assert!(stale.exists(), "dry-run leaves temp file in place");
    }

    #[test]
    fn sweep_orphan_temps_rejects_near_misses() {
        let dir = tempdir().unwrap();
        let hex_31 = "0000000000000000000000000000000";
        let hex_32 = "00000000000000000000000000000000";
        let no_nonce = dir.path().join("foo.json.tmp.12");
        let non_digit_pid = dir.path().join(format!("foo.json.tmp.ab.{hex_32}"));
        let short_nonce = dir.path().join(format!("foo.json.tmp.1.{hex_31}"));
        for path in [&no_nonce, &non_digit_pid, &short_nonce] {
            std::fs::write(path, b"keep").unwrap();
        }

        let (files, bytes) = sweep_orphan_temps_under(dir.path(), Duration::ZERO, false);

        assert_eq!((files, bytes), (0, 0));
        assert!(no_nonce.exists());
        assert!(non_digit_pid.exists());
        assert!(short_nonce.exists());
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
