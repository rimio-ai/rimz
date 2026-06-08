//! Durable diagnostics for pane-binding decisions.
//!
//! Hook stderr is not a reliable operator surface for daemon-routed agents, so
//! binding decisions append compact JSONL records under the workspace runtime
//! directory. The log is diagnostic state: append-only within a size cap, rebuilt
//! from fresh attempts, and never read by correctness code.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::ledger::atomic;
use crate::ledger::paths::RuntimePaths;

const BINDING_LOG_NAME: &str = "binding.log.jsonl";
const BINDING_LOG_MAX_BYTES: u64 = 1_048_576;
const BINDING_LOG_ROTATE_LOCK_STALE: Duration = Duration::from_secs(60);

pub fn path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join(BINDING_LOG_NAME)
}

pub fn append<T: Serialize>(runtime: &RuntimePaths, record: &T) {
    let path = path(runtime);
    if let Err(err) = rotate_if_needed(&path) {
        tracing::debug!(path = %path.display(), error = %err, "binding log rotation skipped");
    }
    let mut line = match serde_json::to_vec(record) {
        Ok(line) => line,
        Err(err) => {
            tracing::debug!(error = %err, "binding log record serialization failed");
            return;
        }
    };
    line.push(b'\n');
    if let Err(err) = atomic::append_record_bytes(&path, &line) {
        tracing::debug!(path = %path.display(), error = %err, "binding log append failed");
    }
}

fn rotate_if_needed(path: &Path) -> std::io::Result<()> {
    if !path
        .metadata()
        .is_ok_and(|metadata| metadata.len() >= BINDING_LOG_MAX_BYTES)
    {
        return Ok(());
    }
    let Some(_lock) = RotationLock::try_acquire(path)? else {
        return Ok(());
    };
    if !path
        .metadata()
        .is_ok_and(|metadata| metadata.len() >= BINDING_LOG_MAX_BYTES)
    {
        return Ok(());
    }
    let rotated = path.with_file_name("binding.log.1.jsonl");
    match std::fs::remove_file(&rotated) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    std::fs::rename(path, rotated)
}

struct RotationLock {
    path: PathBuf,
}

impl RotationLock {
    fn try_acquire(path: &Path) -> std::io::Result<Option<Self>> {
        let lock_path = path.with_file_name("binding.log.rotate.lock");
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
        .is_ok_and(|age| age >= BINDING_LOG_ROTATE_LOCK_STALE)
}

impl Drop for RotationLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ids::WorkspaceId;

    #[test]
    fn append_writes_jsonl_record() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime");

        append(&runtime, &serde_json::json!({ "event": "selected" }));

        let bytes = std::fs::read_to_string(path(&runtime)).unwrap();
        assert_eq!(bytes, "{\"event\":\"selected\"}\n");
    }

    #[test]
    fn rotation_lock_stales_by_age() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(120);

        assert!(lock_is_stale(
            now,
            now - BINDING_LOG_ROTATE_LOCK_STALE - Duration::from_secs(1)
        ));
        assert!(!lock_is_stale(
            now,
            now - BINDING_LOG_ROTATE_LOCK_STALE + Duration::from_secs(1)
        ));
    }
}
