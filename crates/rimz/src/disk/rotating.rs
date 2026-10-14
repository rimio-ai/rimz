//! Best-effort rotating JSONL mechanics; the caller owns the record schema,
//! path, and cap. Incremental folds retain only their state and generation
//! cursors, reading complete appended lines and rebuilding after rotation.

use std::fs::OpenOptions;
use std::io::{BufRead, Seek, SeekFrom};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Serialize, de::DeserializeOwned};

use crate::disk::atomic;

const ROTATE_LOCK_STALE: Duration = Duration::from_secs(60);

pub(crate) struct IncrementalFold<S> {
    paths: [PathBuf; 2],
    generations: Option<[Option<Generation>; 2]>,
    offsets: [u64; 2],
    state: S,
    #[cfg(test)]
    opens: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Generation {
    device: u64,
    inode: u64,
    len: u64,
}

impl Generation {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(not(unix))]
            device: 0,
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(not(unix))]
            inode: 0,
            len: metadata.len(),
        }
    }

    fn continues(self, previous: Self, offset: u64) -> bool {
        self.device == previous.device && self.inode == previous.inode && self.len >= offset
    }
}

impl<S: Default> IncrementalFold<S> {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            paths: [rotated_path(&path), path],
            generations: None,
            offsets: [0; 2],
            state: S::default(),
            #[cfg(test)]
            opens: 0,
        }
    }

    pub(crate) fn refresh<T: DeserializeOwned>(&mut self, mut fold: impl FnMut(&mut S, T)) -> &S {
        let observed = self
            .paths
            .each_ref()
            .map(|path| path.metadata().ok().map(|meta| Generation::of(&meta)));
        let reset = self.generations.is_none_or(|previous| {
            // A late append to the rotated handle must precede current records.
            observed[0] != previous[0]
                || (0..2).any(|i| match (observed[i], previous[i]) {
                    (Some(next), Some(old)) => !next.continues(old, self.offsets[i]),
                    (None, None) => false,
                    _ => true,
                })
        });
        if reset {
            self.state = S::default();
            self.offsets = [0; 2];
        }
        let mut generations = self.generations.unwrap_or([None; 2]);
        for i in 0..2 {
            if !reset && observed[i] == generations[i] {
                continue;
            }
            generations[i] = None;
            let Ok(mut file) = std::fs::File::open(&self.paths[i]) else {
                continue;
            };
            #[cfg(test)]
            {
                self.opens += 1;
            }
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            if file.seek(SeekFrom::Start(self.offsets[i])).is_err() {
                continue;
            }
            generations[i] = Some(Generation::of(&metadata));
            let mut reader = std::io::BufReader::new(file);
            let mut line = Vec::new();
            loop {
                line.clear();
                let Ok(bytes) = reader.read_until(b'\n', &mut line) else {
                    break;
                };
                if bytes == 0 || line.last() != Some(&b'\n') {
                    break;
                }
                self.offsets[i] += bytes as u64;
                if let Ok(record) = serde_json::from_slice(&line) {
                    fold(&mut self.state, record);
                }
            }
        }
        self.generations = Some(generations);
        &self.state
    }
}

/// Appends best-effort; failures log at debug with the path.
pub(crate) fn append<T: Serialize>(path: &Path, max_bytes: u64, record: &T) {
    if let Err(err) = append_rotating_jsonl(path, max_bytes, record) {
        tracing::debug!(path = %path.display(), error = %err, "rotating log append failed");
    }
}

/// Visit decodable records oldest generation first, best-effort per file and line.
pub(crate) fn visit_records<T: DeserializeOwned>(path: &Path, mut visit: impl FnMut(T)) {
    let rotated = rotated_path(path);
    for path in [rotated.as_path(), path] {
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

pub(crate) fn append_rotating_jsonl<T: Serialize>(
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

pub(crate) fn rotated_path(path: &Path) -> PathBuf {
    path.with_file_name(format!("{}.1.jsonl", log_stem(path)))
}

fn lock_path(path: &Path) -> PathBuf {
    path.with_file_name(format!("{}.rotate.lock", log_stem(path)))
}

fn log_stem(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".jsonl"))
        .unwrap_or("log")
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
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)?;
        Ok(Self {
            path: lock_path.to_owned(),
        })
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

    fn refresh(reader: &mut IncrementalFold<Vec<u64>>) -> Vec<u64> {
        reader.refresh(|seen, n: u64| seen.push(n)).clone()
    }

    #[test]
    fn incremental_unchanged_and_append_open_only_needed_generations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::write(rotated_path(&path), b"1\n").unwrap();
        std::fs::write(&path, b"2\n").unwrap();
        let mut reader = IncrementalFold::new(path.clone());
        assert_eq!(refresh(&mut reader), [1, 2]);
        assert_eq!(reader.opens, 2);
        for _ in 0..3 {
            assert_eq!(refresh(&mut reader), [1, 2]);
        }
        assert_eq!(reader.opens, 2);
        append(&path, 1024, &3);
        assert_eq!(refresh(&mut reader), [1, 2, 3]);
        assert_eq!(reader.opens, 3);
        assert_eq!(refresh(&mut reader), [1, 2, 3]);
        assert_eq!(reader.opens, 3);
    }

    #[test]
    fn incremental_waits_for_newline_and_skips_bad_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        std::fs::write(&path, b"1\ninvalid\n2").unwrap();
        let mut reader = IncrementalFold::new(path.clone());
        assert_eq!(refresh(&mut reader), [1]);
        assert_eq!(refresh(&mut reader), [1]);
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
        assert_eq!(refresh(&mut reader), [1, 2]);
        assert_eq!(refresh(&mut reader), [1, 2]);
    }

    #[test]
    fn incremental_resets_on_rotation_replacement_truncation_and_removal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        append(&path, 1, &1);
        append(&path, 1, &222);
        let mut reader = IncrementalFold::new(path.clone());
        assert_eq!(refresh(&mut reader), [1, 222]);
        append(&path, 1, &333);
        let mut full = Vec::new();
        visit_records::<u64>(&path, |n| full.push(n));
        assert_eq!(refresh(&mut reader), full);
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"4\n").unwrap();
        std::fs::rename(replacement, rotated_path(&path)).unwrap();
        assert_eq!(refresh(&mut reader), [4, 333]);
        std::fs::write(&path, b"5\n").unwrap();
        assert_eq!(refresh(&mut reader), [4, 5]);
        std::fs::remove_file(rotated_path(&path)).unwrap();
        assert_eq!(refresh(&mut reader), [5]);
        std::fs::remove_file(&path).unwrap();
        assert!(refresh(&mut reader).is_empty());
        append(&path, 1024, &6);
        assert_eq!(refresh(&mut reader), [6]);
    }

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
        append(&path, 16, &serde_json::json!({ "n": 1 }));
        append(&path, 16, &serde_json::json!({ "n": 2 }));
        append(&path, 16, &serde_json::json!({ "n": 3 }));
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"not-json\n{\"n\":4}\n")
            .unwrap();

        let mut seen = Vec::new();
        visit_records::<serde_json::Value>(&path, |record| {
            seen.push(record["n"].as_u64().unwrap());
        });

        assert_eq!(seen, vec![1, 2, 3, 4]);
    }

    #[test]
    fn visitor_skips_missing_rotated_generation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("binding.log.jsonl");
        append(&path, 1024, &serde_json::json!({ "n": 1 }));

        let mut seen = Vec::new();
        visit_records::<serde_json::Value>(&path, |record| {
            seen.push(record["n"].as_u64().unwrap());
        });

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
