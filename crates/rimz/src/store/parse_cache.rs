//! One thread's last parse of a JSON cache file, keyed by path plus byte
//! identity `(mtime, len)` — the shared core behind every single-slot
//! stat-gated parse cache (`rollup.json`, `latest.json`, the published
//! `snapshot.json`).
//! [`FileStamp`] and [`StampedPath`] expose the same cheap identity outside a
//! parse cache, extended with the device/inode pair on Unix so atomic
//! replacements remain distinguishable at equal byte length and timestamp.
//!
//! Every file it fronts is republished by atomic rename of a fresh temp
//! file, so a changed payload almost surely changes the identity; a hit
//! returns a shared handle to the in-memory value instead of re-deserializing
//! or deep-cloning 100–500 KB of JSON, and the read itself stays
//! page-cache-hot. The
//! identity is deliberately not airtight: two republishes inside one
//! mtime-granularity tick at equal byte length can serve the older parse.
//! Every caller therefore re-validates the value against live truth — the
//! fold resumes from the cached extent, the freshness stamp is checked on
//! the live log — so a stale serve costs a larger fold or a re-read, never
//! a wrong result. A new caller must preserve that property: cache the
//! parse, never a verdict.
//!
//! Callers hold one per thread (`thread_local!`), so the slot needs no lock
//! and is never shared across threads.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) struct FileStamp {
    pub(crate) len: u64,
    pub(crate) modified_secs: u64,
    pub(crate) modified_nanos: u32,
    device: u64,
    inode: u64,
}

impl FileStamp {
    pub(crate) fn of(path: &Path) -> Self {
        let Ok(meta) = std::fs::metadata(path) else {
            return Self {
                len: 0,
                modified_secs: 0,
                modified_nanos: 0,
                device: 0,
                inode: 0,
            };
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok());
        Self {
            len: meta.len(),
            modified_secs: modified.as_ref().map_or(0, |duration| duration.as_secs()),
            modified_nanos: modified.map_or(0, |duration| duration.subsec_nanos()),
            #[cfg(unix)]
            device: meta.dev(),
            #[cfg(not(unix))]
            device: 0,
            #[cfg(unix)]
            inode: meta.ino(),
            #[cfg(not(unix))]
            inode: 0,
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub(crate) struct StampedPath {
    pub(crate) path: PathBuf,
    pub(crate) stamp: FileStamp,
}

impl StampedPath {
    pub(crate) fn of(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            stamp: FileStamp::of(path),
        }
    }
}

pub(crate) struct ParseCache<T> {
    slot: RefCell<Option<Entry<T>>>,
}

struct Entry<T> {
    path: PathBuf,
    stamp: EntryStamp,
    value: Arc<T>,
}

enum EntryStamp {
    Metadata { mtime: SystemTime, len: u64 },
    File(FileStamp),
}

impl<T> ParseCache<T> {
    pub(crate) const fn new() -> Self {
        Self {
            slot: RefCell::new(None),
        }
    }

    /// The cached parse when `(path, mtime, len)` matches this thread's
    /// last [`Self::store`].
    pub(crate) fn get(&self, path: &Path, mtime: SystemTime, len: u64) -> Option<Arc<T>> {
        self.slot.borrow().as_ref().and_then(|entry| {
            (entry.path == path
                && matches!(
                    entry.stamp,
                    EntryStamp::Metadata {
                        mtime: cached_mtime,
                        len: cached_len,
                    } if cached_mtime == mtime && cached_len == len
                ))
            .then(|| Arc::clone(&entry.value))
        })
    }

    /// Remember `value` as the parse of `(path, mtime, len)`, displacing
    /// whatever the slot held.
    pub(crate) fn store(&self, path: &Path, mtime: SystemTime, len: u64, value: Arc<T>) {
        *self.slot.borrow_mut() = Some(Entry {
            path: path.to_path_buf(),
            stamp: EntryStamp::Metadata { mtime, len },
            value,
        });
    }

    /// The cached parse when the full atomic-file identity matches. Unlike the
    /// ordinary `(mtime, len)` key, the Unix device/inode pair distinguishes
    /// equal-length replacements inside one timestamp tick.
    pub(crate) fn get_stamped(&self, stamped: &StampedPath) -> Option<Arc<T>> {
        self.slot.borrow().as_ref().and_then(|entry| {
            (entry.path == stamped.path
                && matches!(entry.stamp, EntryStamp::File(stamp) if stamp == stamped.stamp))
            .then(|| Arc::clone(&entry.value))
        })
    }

    /// Remember `value` under the full atomic-file identity.
    pub(crate) fn store_stamped(&self, stamped: &StampedPath, value: Arc<T>) {
        *self.slot.borrow_mut() = Some(Entry {
            path: stamped.path.clone(),
            stamp: EntryStamp::File(stamped.stamp),
            value,
        });
    }
}
