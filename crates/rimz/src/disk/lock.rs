//! Path-scoped workspace state/cache advisory lock.
//!
//! Resolutions and pushes take this lock briefly so that snapshot rebuilds
//! and per-file state or cache RMW sequences cannot interleave.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// Writers are short-lived CLI processes; matching the mux command timeout bounds
// a wedged holder without interrupting legitimate cold snapshot rebuilds.
pub(crate) const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_LOCK_BACKOFF: Duration = Duration::from_millis(50);

#[derive(Debug, thiserror::Error)]
pub enum LockErr {
    #[error("could not open workspace lock {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not acquire workspace lock {path}: {source}")]
    Acquire {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "timed out after {waited:?} acquiring workspace lock {path}; a stuck rimz process may hold it (run `fuser {path}` to find it)"
    )]
    Timeout { path: PathBuf, waited: Duration },
}

pub type Result<T> = std::result::Result<T, LockErr>;

/// Holds an exclusive advisory lock for the workspace. The lock is released
/// when the guard is dropped (or the process exits).
pub struct WorkspaceLock {
    file: File,
    path: PathBuf,
}

impl WorkspaceLock {
    pub fn acquire(path: &Path) -> Result<Self> {
        Self::acquire_with_timeout(path, LOCK_TIMEOUT)
    }

    /// Acquire the lock within a caller-selected bound.
    pub fn acquire_with_timeout(path: &Path, timeout: Duration) -> Result<Self> {
        Self::acquire_with_deadline(path, timeout)
    }

    /// Attempt one immediate acquisition without sleeping or retrying.
    pub fn try_acquire(path: &Path) -> Result<Option<Self>> {
        let file = open_lock_file(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self {
                file,
                path: path.to_path_buf(),
            })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(source)) => Err(LockErr::Acquire {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    fn acquire_with_deadline(path: &Path, timeout: Duration) -> Result<Self> {
        let file = open_lock_file(path)?;

        let started = Instant::now();
        let mut backoff = Duration::from_millis(1);
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    let elapsed = started.elapsed();
                    if elapsed >= timeout {
                        return Err(LockErr::Timeout {
                            path: path.to_path_buf(),
                            waited: elapsed,
                        });
                    }
                    std::thread::sleep(backoff.min(timeout - elapsed));
                    backoff = (backoff * 2).min(MAX_LOCK_BACKOFF);
                }
                Err(std::fs::TryLockError::Error(source)) => {
                    return Err(LockErr::Acquire {
                        path: path.to_path_buf(),
                        source,
                    });
                }
            }
        }
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }
}

fn open_lock_file(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| LockErr::Open {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|source| LockErr::Open {
            path: path.to_path_buf(),
            source,
        })
}

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        // Best-effort unlock; failure here is unrecoverable and would only
        // mean the lock is released on process exit instead.
        let _ = self.file.unlock();
    }
}

impl std::fmt::Debug for WorkspaceLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceLock")
            .field("path", &self.path)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_can_be_reacquired_after_guard_drops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.lock");

        drop(WorkspaceLock::acquire(&path).unwrap());
        WorkspaceLock::acquire(&path).unwrap();
    }

    #[test]
    fn try_acquire_reports_contention_and_reacquires_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.lock");
        let held = WorkspaceLock::acquire(&path).unwrap();

        assert!(WorkspaceLock::try_acquire(&path).unwrap().is_none());
        drop(held);
        assert!(WorkspaceLock::try_acquire(&path).unwrap().is_some());
    }

    #[test]
    fn contended_lock_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.lock");
        let _held = WorkspaceLock::acquire(&path).unwrap();

        let error = WorkspaceLock::acquire_with_timeout(&path, Duration::from_millis(50))
            .expect_err("held lock should time out");
        assert!(matches!(&error, LockErr::Timeout { .. }));
        let message = error.to_string();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("fuser"), "{message}");
    }

    #[test]
    fn contended_lock_retries_until_guard_drops() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.lock");
        let held = WorkspaceLock::acquire(&path).unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            drop(held);
        });

        WorkspaceLock::acquire_with_timeout(&path, Duration::from_secs(1)).unwrap();
        releaser.join().unwrap();
    }
}
