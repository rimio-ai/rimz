//! Build identity of the running executable.
//!
//! Durable sidebar artifacts are written by whichever process holds the role
//! at the time, and across an upgrade old and new builds overlap inside one
//! workspace. Stamping each published pane frame and diagnostic record with
//! the writer's build id turns that overlap into recorded evidence.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Hex digest prefix of the digest of the running executable's bytes.
const BUILD_ID_BYTES: usize = 6;
const BUILD_ID_CACHE_FILE: &str = "build-id.json";
static BUILD_ID: OnceLock<Option<String>> = OnceLock::new();

/// Build id of this process, computed once from the executable's bytes;
/// `None` when the binary cannot be read (for example replaced mid-upgrade
/// before the re-exec lands).
pub fn current() -> Option<&'static str> {
    BUILD_ID.get_or_init(compute).as_deref()
}

/// Return this process's build id only if a prior [`warm`] or [`current`] call
/// has already computed it.
pub fn current_if_ready() -> Option<&'static str> {
    BUILD_ID.get().and_then(Option::as_deref)
}

/// Start computing this process's build id on a background thread.
pub fn warm() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        std::thread::spawn(|| {
            let _ = current();
        });
    });
}

fn compute() -> Option<String> {
    of_file(&running_image_path()?).ok()
}

/// Digest the bytes at `path` into the short build id Rimz stamps into runtime
/// artifacts.
pub fn of_file(path: &Path) -> io::Result<String> {
    let path = resolve_on_disk_binary(path).unwrap_or_else(|| path.to_path_buf());
    of_file_with_cache_path(&path, &cache_path())
}

fn of_file_with_cache_path(path: &Path, cache_path: &Path) -> io::Result<String> {
    let key = cache_key(path)?;
    if let Some(id) = read_cached_id(cache_path, &key) {
        return Ok(id);
    }
    let id = hash_file(path)?;
    write_cached_id(cache_path, key, &id);
    Ok(id)
}

fn hash_file(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(hex::encode(&digest[..BUILD_ID_BYTES]))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuildIdCacheKey {
    path: PathBuf,
    mtime_ns: Option<u128>,
    len: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct BuildIdCacheRecord {
    path: PathBuf,
    mtime_ns: Option<u128>,
    len: u64,
    id: String,
}

impl BuildIdCacheRecord {
    fn key(&self) -> BuildIdCacheKey {
        BuildIdCacheKey {
            path: self.path.clone(),
            mtime_ns: self.mtime_ns,
            len: self.len,
        }
    }
}

fn cache_key(path: &Path) -> io::Result<BuildIdCacheKey> {
    let metadata = std::fs::metadata(path)?;
    Ok(BuildIdCacheKey {
        path: path.to_path_buf(),
        mtime_ns: metadata.modified().ok().and_then(system_time_ns),
        len: metadata.len(),
    })
}

fn system_time_ns(time: SystemTime) -> Option<u128> {
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    Some(duration.as_nanos())
}

fn read_cached_id(path: &Path, key: &BuildIdCacheKey) -> Option<String> {
    let record: BuildIdCacheRecord = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (record.key() == *key && valid_build_id(&record.id)).then_some(record.id)
}

fn write_cached_id(path: &Path, key: BuildIdCacheKey, id: &str) {
    let record = BuildIdCacheRecord {
        path: key.path,
        mtime_ns: key.mtime_ns,
        len: key.len,
        id: id.to_owned(),
    };
    let _ = crate::ledger::atomic::write_temp_then_rename_cache(path, &record);
}

fn valid_build_id(id: &str) -> bool {
    id.len() == BUILD_ID_BYTES * 2
        && id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

fn cache_path() -> PathBuf {
    crate::ledger::paths::cache_home()
        .join("rimz")
        .join(BUILD_ID_CACHE_FILE)
}

/// Resolve an executable path reported by the OS to the replacement binary on
/// disk. Linux annotates the running image path with " (deleted)" after an
/// atomic install unlinks that inode; the replacement lives at the stripped
/// path.
pub fn resolve_on_disk_binary(exe: &Path) -> Option<PathBuf> {
    if exe.is_file() {
        return Some(exe.to_path_buf());
    }
    strip_deleted_suffix(exe).filter(|path| path.is_file())
}

#[cfg(unix)]
fn strip_deleted_suffix(path: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    const DELETED_SUFFIX: &[u8] = b" (deleted)";
    let stripped = path.as_os_str().as_bytes().strip_suffix(DELETED_SUFFIX)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(stripped)))
}

#[cfg(not(unix))]
fn strip_deleted_suffix(_path: &Path) -> Option<PathBuf> {
    None
}

#[cfg(target_os = "linux")]
fn running_image_path() -> Option<PathBuf> {
    Some(PathBuf::from("/proc/self/exe"))
}

#[cfg(not(target_os = "linux"))]
fn running_image_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_id_is_stable_lowercase_hex() {
        let first = current().expect("the test binary is readable");
        let second = current().expect("the second call serves the cached id");

        assert_eq!(first, second);
        assert!(valid_build_id(first));
    }

    #[test]
    fn matching_file_cache_returns_cached_id_without_hashing() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("rimz");
        let cache = dir.path().join("cache/build-id.json");
        std::fs::write(&binary, b"not the cached digest").unwrap();
        let key = cache_key(&binary).unwrap();
        let cached = "abcdef123456";
        write_cached_id(&cache, key, cached);

        assert_eq!(
            of_file_with_cache_path(&binary, &cache).unwrap(),
            cached,
            "matching path/mtime/len serves the cached SHA prefix even when file bytes differ",
        );
    }

    #[test]
    fn changed_file_cache_key_recomputes_and_rewrites() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("rimz");
        let cache = dir.path().join("cache/build-id.json");
        std::fs::write(&binary, b"old").unwrap();
        let key = cache_key(&binary).unwrap();
        write_cached_id(&cache, key, "abcdef123456");

        std::fs::write(&binary, b"new bytes").unwrap();
        let expected = hash_file(&binary).unwrap();

        assert_eq!(of_file_with_cache_path(&binary, &cache).unwrap(), expected);
    }

    #[test]
    fn resolve_on_disk_binary_strips_deleted_suffix_when_replacement_exists() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("rimz");
        std::fs::write(&real, b"x").unwrap();
        let deleted = PathBuf::from(format!("{} (deleted)", real.display()));

        assert_eq!(resolve_on_disk_binary(&deleted), Some(real.clone()));
        assert_eq!(resolve_on_disk_binary(&real), Some(real));
    }

    #[test]
    fn resolve_on_disk_binary_returns_none_when_no_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("rimz");
        let deleted = PathBuf::from(format!("{} (deleted)", missing.display()));

        assert_eq!(resolve_on_disk_binary(&deleted), None);
        assert_eq!(resolve_on_disk_binary(&missing), None);
    }
}
