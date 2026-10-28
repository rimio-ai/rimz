//! Stat-gated parsed file caches. [`ParseCache`] retains a single successful
//! parse, while [`KeyedParseCache`] retains up to 1,024 paths, including missing
//! and malformed files. Both share parsed values through `Arc`.
//! [`FileStamp`] and [`StampedPath`] expose the same cheap identity outside a
//! parse cache, extended with the device/inode pair on Unix so atomic
//! replacements remain distinguishable at equal byte length and timestamp.
//!
//! The keyed cache uses the full [`FileStamp`] on every read, distinguishing
//! atomic replacements by inode and appended history by length. Other I/O errors
//! are never cached. The single-slot cache also supports `(mtime, len)` keys.
//! That identity is deliberately not airtight: two in-place rewrites inside one
//! mtime-granularity tick at equal byte length can serve the older parse.
//! Every caller re-validates the value against live truth — the
//! fold resumes from the cached extent, the freshness stamp is checked on
//! the live log — so a stale serve costs a larger fold or a re-read, never
//! a wrong result. A new caller must preserve that property: cache the
//! parse, never a verdict. Files only replaced by atomic rename use the full
//! stamp: the temp file has a distinct inode while the old one is still linked.
//! A locked read-modify-write never uses this cache: a later rename can reuse
//! a freed inode at equal length within one mtime tick, and publishing that
//! stale parse would lose intervening writes.
//!
//! Callers normally hold one per thread (`thread_local!`); session-effort also
//! keeps single-slot caches behind its process-wide mutex. These types provide
//! no synchronization of their own.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct FileStamp {
    pub(crate) len: u64,
    pub(crate) modified_secs: u64,
    pub(crate) modified_nanos: u32,
    device: u64,
    inode: u64,
}

impl FileStamp {
    fn of(path: &Path) -> Self {
        let Ok(meta) = std::fs::metadata(path) else {
            return Self {
                len: 0,
                modified_secs: 0,
                modified_nanos: 0,
                device: 0,
                inode: 0,
            };
        };
        Self::from_metadata(&meta)
    }

    pub(crate) fn from_metadata(meta: &std::fs::Metadata) -> Self {
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

#[derive(PartialEq, Eq)]
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
            (entry.path == path && entry.stamp == EntryStamp::Metadata { mtime, len })
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
            (entry.path == stamped.path && entry.stamp == EntryStamp::File(stamped.stamp))
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

impl<T: DeserializeOwned> ParseCache<T> {
    /// Read one JSON value under the file identity observed before the read,
    /// reusing this thread's last parse when that identity still matches.
    pub(crate) fn read_stamped_json(&self, path: &Path) -> Option<Arc<T>> {
        let stamped = StampedPath::of(path);
        if let Some(value) = self.get_stamped(&stamped) {
            return Some(value);
        }
        let value = Arc::new(serde_json::from_slice(&std::fs::read(path).ok()?).ok()?);
        self.store_stamped(&stamped, Arc::clone(&value));
        Some(value)
    }
}

const KEYED_CACHE_LIMIT: usize = 1_024;

pub(crate) struct KeyedParseCache<T> {
    entries: RefCell<HashMap<PathBuf, KeyedEntry<T>>>,
    order: RefCell<VecDeque<PathBuf>>,
}

struct KeyedEntry<T> {
    stamp: Option<FileStamp>,
    value: Option<Arc<T>>,
}

impl<T> Default for KeyedParseCache<T> {
    fn default() -> Self {
        Self {
            entries: RefCell::default(),
            order: RefCell::default(),
        }
    }
}

impl<T> KeyedParseCache<T> {
    pub(crate) fn read_with(
        &self,
        path: &Path,
        parse: impl FnOnce(&[u8]) -> Option<T>,
    ) -> io::Result<Option<Arc<T>>> {
        #[cfg(any(test, feature = "testkit"))]
        testkit::stat();
        let stamp = match std::fs::metadata(path) {
            Ok(meta) => Some(FileStamp::from_metadata(&meta)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        self.read_entry(path, stamp, || std::fs::read(path), parse)
    }

    fn read_entry(
        &self,
        path: &Path,
        stamp: Option<FileStamp>,
        read: impl FnOnce() -> io::Result<Vec<u8>>,
        parse: impl FnOnce(&[u8]) -> Option<T>,
    ) -> io::Result<Option<Arc<T>>> {
        if let Some(entry) = self.entries.borrow().get(path)
            && entry.stamp == stamp
        {
            return Ok(entry.value.clone());
        }
        let value = if stamp.is_some() {
            let bytes = match read() {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            #[cfg(any(test, feature = "testkit"))]
            testkit::parse();
            parse(&bytes).map(Arc::new)
        } else {
            None
        };
        let mut entries = self.entries.borrow_mut();
        if !entries.contains_key(path) {
            let mut order = self.order.borrow_mut();
            if entries.len() == KEYED_CACHE_LIMIT
                && let Some(oldest) = order.pop_front()
            {
                entries.remove(&oldest);
            }
            order.push_back(path.to_owned());
        }
        entries.insert(
            path.to_owned(),
            KeyedEntry {
                stamp,
                value: value.clone(),
            },
        );
        Ok(value)
    }
}

impl<T: DeserializeOwned> KeyedParseCache<T> {
    pub(crate) fn read_json(&self, path: &Path) -> io::Result<Option<Arc<T>>> {
        self.read_with(path, |bytes| serde_json::from_slice(bytes).ok())
    }
}

#[cfg(any(test, feature = "testkit"))]
pub(crate) mod testkit {
    use std::cell::Cell;

    thread_local! {
        static STATS: Cell<usize> = const { Cell::new(0) };
        static PARSES: Cell<usize> = const { Cell::new(0) };
    }

    pub(super) fn stat() {
        STATS.set(STATS.get() + 1);
    }

    pub(super) fn parse() {
        PARSES.set(PARSES.get() + 1);
    }

    /// This thread's cumulative (metadata attempts, parse attempts).
    pub fn read_counts() -> (usize, usize) {
        (STATS.get(), PARSES.get())
    }
}

#[cfg(test)]
mod tests;
