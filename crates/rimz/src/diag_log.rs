//! Shared size-capped JSONL diagnostic log appends.
//!
//! Diagnostic logs are cache-class runtime evidence: each append is one JSON
//! line, rotation keeps one previous generation, and correctness code never
//! reads them.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::ledger::atomic;

const ROTATE_LOCK_STALE: Duration = Duration::from_secs(60);

pub fn append<T: Serialize>(path: &Path, max_bytes: u64, record: &T) {
    if let Err(err) = rotate_if_needed(path, max_bytes) {
        tracing::debug!(path = %path.display(), error = %err, "diagnostic log rotation skipped");
    }
    let mut line = match serde_json::to_vec(record) {
        Ok(line) => line,
        Err(err) => {
            tracing::debug!(error = %err, "diagnostic log record serialization failed");
            return;
        }
    };
    line.push(b'\n');
    if let Err(err) = atomic::append_record_bytes(path, &line) {
        tracing::debug!(path = %path.display(), error = %err, "diagnostic log append failed");
    }
}

fn rotate_if_needed(path: &Path, max_bytes: u64) -> std::io::Result<()> {
    if !path
        .metadata()
        .is_ok_and(|metadata| metadata.len() >= max_bytes)
    {
        return Ok(());
    }
    let Some(_lock) = RotationLock::try_acquire(path)? else {
        return Ok(());
    };
    if !path
        .metadata()
        .is_ok_and(|metadata| metadata.len() >= max_bytes)
    {
        return Ok(());
    }
    let rotated = rotated_path(path);
    match std::fs::remove_file(&rotated) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    std::fs::rename(path, rotated)
}

fn rotated_path(path: &Path) -> PathBuf {
    path.with_file_name(format!("{}.1.jsonl", log_stem(path)))
}

fn lock_path(path: &Path) -> PathBuf {
    path.with_file_name(format!("{}.rotate.lock", log_stem(path)))
}

fn log_stem(path: &Path) -> String {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return "diagnostic.log".to_owned();
    };
    name.strip_suffix(".jsonl").unwrap_or(name).to_owned()
}

struct RotationLock {
    path: PathBuf,
}

impl RotationLock {
    fn try_acquire(path: &Path) -> std::io::Result<Option<Self>> {
        let lock_path = lock_path(path);
        match Self::create(&lock_path) {
            Ok(lock) => return Ok(Some(lock)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
        remove_stale_lock(&lock_path)?;
        match Self::create(&lock_path) {
            Ok(lock) => Ok(Some(lock)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn create(lock_path: &Path) -> std::io::Result<Self> {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(_) => Ok(Self {
                path: lock_path.to_owned(),
            }),
            Err(err) => Err(err),
        }
    }
}

fn remove_stale_lock(lock_path: &Path) -> std::io::Result<()> {
    let metadata = match lock_path.metadata() {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    let Ok(modified) = metadata.modified() else {
        return Ok(());
    };
    if !lock_is_stale(SystemTime::now(), modified) {
        return Ok(());
    }
    match std::fs::remove_file(lock_path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn lock_is_stale(now: SystemTime, modified: SystemTime) -> bool {
    now.duration_since(modified)
        .is_ok_and(|age| age >= ROTATE_LOCK_STALE)
}

impl Drop for RotationLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_writes_jsonl_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("observe.log.jsonl");

        append(
            &path,
            1_048_576,
            &serde_json::json!({ "event": "selected" }),
        );

        let bytes = std::fs::read_to_string(path).unwrap();
        assert_eq!(bytes, "{\"event\":\"selected\"}\n");
    }

    #[test]
    fn rotated_and_lock_paths_follow_log_name() {
        let path = Path::new("/tmp/rimz/observe.log.jsonl");

        assert_eq!(
            rotated_path(path),
            PathBuf::from("/tmp/rimz/observe.log.1.jsonl")
        );
        assert_eq!(
            lock_path(path),
            PathBuf::from("/tmp/rimz/observe.log.rotate.lock")
        );
    }

    #[test]
    fn rotation_lock_stales_by_age() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(120);

        assert!(lock_is_stale(
            now,
            now - ROTATE_LOCK_STALE - Duration::from_secs(1)
        ));
        assert!(!lock_is_stale(
            now,
            now - ROTATE_LOCK_STALE + Duration::from_secs(1)
        ));
    }
}
