//! Small rotating JSONL streaming read/write helper for diagnostic logs.
//!
//! The caller owns the record schema and path. This module owns the shared
//! rotation lock and append discipline so diagnostic files do not grow separate
//! implementations.

use std::fs::OpenOptions;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Serialize, de::DeserializeOwned};

use crate::store::atomic;

const ROTATE_LOCK_STALE: Duration = Duration::from_secs(60);

pub struct JsonlLog {
    path: PathBuf,
    max_bytes: u64,
}

impl JsonlLog {
    pub fn new(path: PathBuf, max_bytes: u64) -> Self {
        Self { path, max_bytes }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends best-effort; failures log at debug with the path.
    pub fn append<T: Serialize>(&self, record: &T) {
        if let Err(err) = append_rotating_jsonl(&self.path, self.max_bytes, record) {
            tracing::debug!(path = %self.path.display(), error = %err, "diagnostic log append failed");
        }
    }

    /// Visit decodable records oldest generation first, best-effort per file and line.
    pub fn visit_records<T: DeserializeOwned>(&self, mut visit: impl FnMut(T)) {
        let rotated = rotated_path(&self.path);
        for path in [rotated.as_path(), self.path.as_path()] {
            let Ok(file) = std::fs::File::open(path) else {
                continue;
            };
            for line in std::io::BufReader::new(file).lines() {
                let Ok(line) = line else {
                    break;
                };
                if let Ok(record) = serde_json::from_str(&line) {
                    visit(record);
                }
            }
        }
    }
}

fn append_rotating_jsonl<T: Serialize>(
    path: &Path,
    max_bytes: u64,
    record: &T,
) -> std::io::Result<()> {
    rotate_if_needed(path, max_bytes)?;
    let mut line = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    line.push(b'\n');
    atomic::append_record_bytes(path, &line).map_err(std::io::Error::other)
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
    path.with_file_name(rotated_file_name(path))
}

fn lock_path(path: &Path) -> PathBuf {
    path.with_file_name(format!("{}.rotate.lock", log_stem(path)))
}

fn rotated_file_name(path: &Path) -> String {
    format!("{}.1.jsonl", log_stem(path))
}

fn log_stem(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".jsonl"))
        .unwrap_or("log")
        .to_owned()
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
    use std::io::Write as _;

    #[test]
    fn appends_jsonl_and_rotates_one_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binding.log.jsonl");
        let cap = b"{\"n\":1}\n".len() as u64;

        append_rotating_jsonl(&path, cap, &serde_json::json!({ "n": 1 })).unwrap();
        append_rotating_jsonl(&path, cap, &serde_json::json!({ "n": 2 })).unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.path().join("binding.log.1.jsonl")).unwrap(),
            "{\"n\":1}\n"
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{\"n\":2}\n");
    }

    #[test]
    fn visitor_streams_rotated_then_active_and_skips_bad_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binding.log.jsonl");
        let log = JsonlLog::new(path.clone(), 16);
        log.append(&serde_json::json!({ "n": 1 }));
        log.append(&serde_json::json!({ "n": 2 }));
        log.append(&serde_json::json!({ "n": 3 }));
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"not-json\n{\"n\":4}\n")
            .unwrap();

        let mut seen = Vec::new();
        log.visit_records::<serde_json::Value>(|record| seen.push(record["n"].as_u64().unwrap()));

        assert_eq!(seen, vec![1, 2, 3, 4]);
    }

    #[test]
    fn visitor_skips_missing_rotated_generation() {
        let dir = tempfile::tempdir().unwrap();
        let log = JsonlLog::new(dir.path().join("binding.log.jsonl"), 1024);
        log.append(&serde_json::json!({ "n": 1 }));

        let mut seen = Vec::new();
        log.visit_records::<serde_json::Value>(|record| seen.push(record["n"].as_u64().unwrap()));

        assert_eq!(seen, vec![1]);
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
