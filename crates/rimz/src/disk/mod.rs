//! Durable file mechanics shared across RimZ modules.
//!
//! This module imports only `ids`, `sock` and `utils`; every fsync lives in
//! `atomic.rs`. Rotating JSONL appends are best-effort, skip the per-record
//! fsync, and sync the parent directory only when they create the file.

pub mod atomic;
pub(crate) mod buckets;
pub mod lock;
pub(crate) mod parse_cache;
pub mod paths;
pub mod retention;
pub(crate) mod rotating;
pub(crate) mod single_flight;
pub mod summary;
pub mod usage;

/// Whether a directory entry sits at `path`, without following a final symlink:
/// a dangling link is present. Only `NotFound` reads as absent.
pub fn entry_exists(path: &std::path::Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
