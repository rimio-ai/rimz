//! One thread's last parse of a JSON cache file, keyed by path plus byte
//! identity `(mtime, len)` — the shared core behind every single-slot
//! stat-gated parse cache (`rollup.json`, `latest.json`, the published
//! `snapshot.json`).
//!
//! Every file it fronts is republished by atomic rename of a fresh temp
//! file, so a changed payload almost surely changes the identity; a hit
//! returns a clone of the in-memory value instead of re-deserializing
//! 100–500 KB of JSON, and the read itself stays page-cache-hot. The
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
use std::time::SystemTime;

pub(crate) struct ParseCache<T> {
    slot: RefCell<Option<Entry<T>>>,
}

struct Entry<T> {
    path: PathBuf,
    mtime: SystemTime,
    len: u64,
    value: T,
}

impl<T: Clone> ParseCache<T> {
    pub(crate) const fn new() -> Self {
        Self {
            slot: RefCell::new(None),
        }
    }

    /// The cached parse when `(path, mtime, len)` matches this thread's
    /// last [`Self::store`].
    pub(crate) fn get(&self, path: &Path, mtime: SystemTime, len: u64) -> Option<T> {
        self.slot.borrow().as_ref().and_then(|entry| {
            (entry.path == path && entry.mtime == mtime && entry.len == len)
                .then(|| entry.value.clone())
        })
    }

    /// Remember `value` as the parse of `(path, mtime, len)`, displacing
    /// whatever the slot held.
    pub(crate) fn store(&self, path: &Path, mtime: SystemTime, len: u64, value: T) {
        *self.slot.borrow_mut() = Some(Entry {
            path: path.to_path_buf(),
            mtime,
            len,
            value,
        });
    }
}
