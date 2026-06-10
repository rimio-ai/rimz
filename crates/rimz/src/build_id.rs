//! Build identity of the running executable.
//!
//! Durable sidebar artifacts are written by whichever process holds the role
//! at the time, and across an upgrade old and new builds overlap inside one
//! workspace. Stamping each published pane frame and diagnostic record with
//! the writer's build id turns that overlap into recorded evidence.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

/// Hex digest prefix of the digest of the running executable's bytes.
const BUILD_ID_BYTES: usize = 6;
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
        assert_eq!(first.len(), BUILD_ID_BYTES * 2);
        assert!(
            first
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
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
