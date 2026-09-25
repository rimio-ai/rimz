//! Path-scoped workspace state/cache advisory lock.
//!
//! Resolutions and pushes take this lock briefly so that snapshot rebuilds
//! and per-file state or cache RMW sequences cannot interleave.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::MetadataExt;
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

    /// Attempt acquisition without waiting for another holder.
    pub fn try_acquire(path: &Path) -> Result<Option<Self>> {
        let mut file = open_lock_file(path)?;
        match try_lock_file(&mut file, path) {
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
        let mut file = open_lock_file(path)?;

        let started = Instant::now();
        let mut backoff = Duration::from_millis(1);
        loop {
            match try_lock_file(&mut file, path) {
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

/// Lock only the inode currently named by the path, reopening after GC unlinks it.
pub(crate) fn try_lock_file(
    file: &mut File,
    path: &Path,
) -> std::result::Result<(), std::fs::TryLockError> {
    lock_current(file, path, File::try_lock)
}

pub(crate) fn lock_file(file: &mut File, path: &Path) -> io::Result<()> {
    lock_current(file, path, |file| {
        file.lock().map_err(std::fs::TryLockError::Error)
    })
    .map_err(io::Error::from)
}

fn lock_current(
    file: &mut File,
    path: &Path,
    acquire: impl Fn(&File) -> std::result::Result<(), std::fs::TryLockError>,
) -> std::result::Result<(), std::fs::TryLockError> {
    loop {
        acquire(file)?;
        let current = (|| {
            let locked = file.metadata()?;
            match std::fs::metadata(path) {
                Ok(named) => Ok(locked.dev() == named.dev() && locked.ino() == named.ino()),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(err) => Err(err),
            }
        })();
        if matches!(current, Ok(true)) {
            return Ok(());
        }
        file.unlock().map_err(std::fs::TryLockError::Error)?;
        current.map_err(std::fs::TryLockError::Error)?;
        *file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(std::fs::TryLockError::Error)?;
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
    fn waiter_reopens_unlinked_inode_without_overlapping_new_holder() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.lock");
        let sweep_guard = WorkspaceLock::acquire(&path).unwrap();
        let mut waiting_file = open_lock_file(&path).unwrap();
        let old_inode = waiting_file.metadata().unwrap().ino();
        assert!(matches!(
            try_lock_file(&mut waiting_file, &path),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        let active = Arc::new(AtomicUsize::new(0));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (retry_tx, retry_rx) = mpsc::channel();
        let waiter_path = path.clone();
        let waiter_active = active.clone();
        let waiter = std::thread::spawn(move || {
            let attempts = std::cell::Cell::new(0);
            lock_current(&mut waiting_file, &waiter_path, |file| {
                if attempts.get() == 0 {
                    ready_tx.send(()).unwrap();
                } else {
                    retry_tx.send(()).unwrap();
                }
                attempts.set(attempts.get() + 1);
                file.lock().map_err(std::fs::TryLockError::Error)
            })
            .unwrap();
            assert_ne!(waiting_file.metadata().unwrap().ino(), old_inode);
            assert_eq!(waiter_active.fetch_add(1, Ordering::SeqCst), 0);
            assert_eq!(
                waiting_file.metadata().unwrap().ino(),
                std::fs::metadata(&waiter_path).unwrap().ino()
            );
            assert_eq!(waiter_active.fetch_sub(1, Ordering::SeqCst), 1);
        });
        ready_rx.recv().unwrap();
        // The collector unlinks only while holding the old inode's lock.
        std::fs::remove_file(&path).unwrap();
        let second_path = path.clone();
        let second_active = active.clone();
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let second = std::thread::spawn(move || {
            let _guard = WorkspaceLock::acquire(&second_path).unwrap();
            assert_eq!(second_active.fetch_add(1, Ordering::SeqCst), 0);
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            assert_eq!(second_active.fetch_sub(1, Ordering::SeqCst), 1);
        });
        held_rx.recv().unwrap();
        drop(sweep_guard);
        let retried = retry_rx.recv_timeout(Duration::from_secs(2));
        release_tx.send(()).unwrap();
        second.join().unwrap();
        waiter.join().unwrap();
        retried.expect("waiter must retry on the named inode while the second holder owns it");
    }

    #[test]
    fn immediate_acquire_reopens_missing_or_replaced_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspace.lock");
        let mut stale = open_lock_file(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        try_lock_file(&mut stale, &path).unwrap();
        assert_eq!(
            stale.metadata().unwrap().ino(),
            std::fs::metadata(&path).unwrap().ino()
        );
        std::fs::remove_file(&path).unwrap();
        let _held = WorkspaceLock::acquire(&path).unwrap();
        assert!(matches!(
            try_lock_file(&mut stale, &path),
            Err(std::fs::TryLockError::WouldBlock)
        ));
    }

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
