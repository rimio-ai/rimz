//! A daemon's stderr log, bounded in place by the processes that write it.
//!
//! Several processes hold one of these files open for append at once: the
//! daemon and every spawn site that hands it a descriptor. At or past the cap a
//! trim publishes the newest part of the live content (at most the cap, ending
//! at the size read under the lock, starting at a line where it can) whole as
//! the one previous file, then empties the live file in place, so every append
//! descriptor keeps writing at the new end. Trimmers serialize on a room lock
//! and read the size again under it. Lines appended after that read are lost.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::disk::atomic;
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::{StatePaths, previous_log};
use crate::disk::retention::DAEMON_LOG_MAX_BYTES;

/// How often a daemon checks the size of its own stderr file. Its writers can
/// carry the file past the cap by what they append within one interval.
const CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// Open `live` for append, trimming it first when it is at the cap. A failed
/// trim warns and still opens.
pub fn open(paths: &StatePaths, live: &Path) -> io::Result<File> {
    CappedLog::new(paths, live).open()
}

/// Check `live` on an interval from a background thread, when this process's
/// stderr is that file. A command run by hand in a terminal starts none.
pub fn watch_own_stderr(paths: &StatePaths, live: &Path) {
    CappedLog::new(paths, live).watch(io::stderr().as_fd(), CHECK_INTERVAL);
}

struct CappedLog {
    live: PathBuf,
    previous: PathBuf,
    lock: PathBuf,
    cap: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileId {
    device: u64,
    inode: u64,
}

impl FileId {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

impl CappedLog {
    fn new(paths: &StatePaths, live: &Path) -> Self {
        Self {
            live: live.to_path_buf(),
            previous: previous_log(live),
            lock: paths.log_trim_lock(live),
            cap: DAEMON_LOG_MAX_BYTES,
        }
    }

    fn open(&self) -> io::Result<File> {
        if let Some(parent) = self.live.parent() {
            std::fs::create_dir_all(parent)?;
        }
        self.trim_or_warn();
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.live)
    }

    fn at_cap(&self) -> bool {
        self.live
            .metadata()
            .is_ok_and(|metadata| metadata.len() >= self.cap)
    }

    /// Whether this call emptied the live file. A trimmer that finds the lock
    /// held leaves the trim to its holder.
    fn trim(&self) -> io::Result<bool> {
        if !self.at_cap() {
            return Ok(false);
        }
        let Some(_lock) = WorkspaceLock::try_acquire(&self.lock).map_err(io::Error::other)? else {
            return Ok(false);
        };
        if !self.at_cap() {
            return Ok(false);
        }
        let mut live = OpenOptions::new().read(true).write(true).open(&self.live)?;
        let size = live.metadata()?.len();
        let start = newest_line_start(&mut live, size.saturating_sub(self.cap), size)?;
        live.seek(SeekFrom::Start(start))?;
        atomic::copy_cache_atomically(&self.previous, &mut (&live).take(size - start))
            .map_err(io::Error::other)?;
        live.set_len(0)?;
        Ok(true)
    }

    fn trim_or_warn(&self) {
        if let Err(error) = self.trim() {
            tracing::warn!(path = %self.live.display(), %error, "capping daemon log failed");
        }
    }

    /// The file `fd` writes to, when `live` names it.
    fn held_by(&self, fd: BorrowedFd<'_>) -> Option<FileId> {
        let held = File::from(fd.try_clone_to_owned().ok()?).metadata().ok()?;
        let id = FileId::of(&held);
        self.names(id).then_some(id)
    }

    fn names(&self, id: FileId) -> bool {
        self.live
            .metadata()
            .is_ok_and(|metadata| FileId::of(&metadata) == id)
    }

    /// Start the interval check when `fd` writes to `live`. The thread ends
    /// once `live` no longer names that file.
    fn watch(self, fd: BorrowedFd<'_>, interval: Duration) -> bool {
        let Some(id) = self.held_by(fd) else {
            return false;
        };
        let live = self.live.clone();
        std::thread::Builder::new()
            .name("daemon-log-cap".to_owned())
            .spawn(move || {
                loop {
                    std::thread::sleep(interval);
                    if !self.names(id) {
                        return;
                    }
                    self.trim_or_warn();
                }
            })
            .inspect_err(|error| {
                tracing::warn!(path = %live.display(), %error, "starting daemon log check failed");
            })
            .is_ok()
    }
}

/// Where the kept part of a file of `size` bytes starts: `from`, moved
/// forward to the first line that starts at or after it, unless no line does.
fn newest_line_start(file: &mut File, from: u64, size: u64) -> io::Result<u64> {
    let Some(before) = from.checked_sub(1) else {
        return Ok(0);
    };
    file.seek(SeekFrom::Start(before))?;
    let skipped = BufReader::new(file.take(size - before)).skip_until(b'\n')? as u64;
    let start = before + skipped;
    Ok(if start < size { start } else { from })
}

#[cfg(test)]
mod tests;
