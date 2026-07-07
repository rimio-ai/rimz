//! Workspace-scoped advisory lock.
//!
//! Resolutions and pushes take this lock briefly so that snapshot rebuilds
//! and per-file CAS sequences can't interleave. The lock guards the *store*
//! directory, not the runtime directory.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| LockErr::Open {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(|e| LockErr::Open {
                path: path.to_path_buf(),
                source: e,
            })?;
        file.lock().map_err(|e| LockErr::Acquire {
            path: path.to_path_buf(),
            source: e,
        })?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }
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
