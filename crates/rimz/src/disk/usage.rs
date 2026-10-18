//! Symlink-safe, hardlink-aware disk usage for `doctor`, `uninstall`, and `gc`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use crate::disk::paths;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskSpace {
    pub free: u64,
    pub total: u64,
}

/// Available (unprivileged) space and capacity on the filesystem holding `path`.
// The statvfs field widths differ by platform; conversion is a no-op on Linux.
#[allow(clippy::useless_conversion)]
pub fn disk_space(path: &Path) -> std::io::Result<DiskSpace> {
    #[cfg(any(test, feature = "testkit"))]
    if let Ok(raw) = std::env::var("RIMZ_TEST_DISK_SPACE") {
        let (free, total) = raw
            .split_once(',')
            .and_then(|(free, total)| Some((free.parse().ok()?, total.parse().ok()?)))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "RIMZ_TEST_DISK_SPACE must be <free>,<total>",
                )
            })?;
        return Ok(DiskSpace { free, total });
    }
    let stat = nix::sys::statvfs::statvfs(path)?;
    let fragment = u64::from(stat.fragment_size());
    Ok(DiskSpace {
        free: u64::from(stat.blocks_available()).saturating_mul(fragment),
        total: u64::from(stat.blocks()).saturating_mul(fragment),
    })
}

/// Best-effort recursive size for a path without following symlinks.
pub(crate) fn dir_size(path: &Path) -> u64 {
    dir_size_inner(path, &mut HashSet::new())
}

fn dir_size_inner(path: &Path, seen_files: &mut HashSet<FileIdentity>) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(_) => return 0,
    };
    if file_identity(&meta).is_some_and(|identity| !seen_files.insert(identity)) {
        return 0;
    }
    let mut bytes = meta.len();
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return bytes;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return bytes;
    };
    for entry in entries.flatten() {
        bytes = bytes.saturating_add(dir_size_inner(&entry.path(), seen_files));
    }
    bytes
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn file_identity(meta: &fs::Metadata) -> Option<FileIdentity> {
    meta.is_file().then(|| FileIdentity {
        device: meta.dev(),
        inode: meta.ino(),
    })
}

#[cfg(not(unix))]
fn file_identity(_meta: &fs::Metadata) -> Option<FileIdentity> {
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageKind {
    Home,
    Runtime,
}

impl StorageKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Runtime => "runtime",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageRoot {
    pub kind: StorageKind,
    pub path: PathBuf,
    pub bytes: u64,
    pub present: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeStorage {
    pub roots: Vec<StorageRoot>,
}

impl RuntimeStorage {
    pub fn total_bytes(&self) -> u64 {
        self.roots
            .iter()
            .fold(0_u64, |total, root| total.saturating_add(root.bytes))
    }
}

pub fn measure() -> RuntimeStorage {
    measure_under(&[
        (StorageKind::Home, paths::rimz_home()),
        (StorageKind::Runtime, paths::runtime_rimz_root()),
    ])
}

fn measure_under(roots: &[(StorageKind, PathBuf)]) -> RuntimeStorage {
    RuntimeStorage {
        roots: roots
            .iter()
            .map(|(kind, path)| StorageRoot {
                kind: *kind,
                path: path.clone(),
                present: path.exists(),
                bytes: dir_size(path),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn disk_space_reports_available_capacity() {
        let temp = tempdir().unwrap();
        let space = disk_space(temp.path()).unwrap();
        assert!(space.total > 0);
        assert!(space.free <= space.total);
        assert!(disk_space(&temp.path().join("missing")).is_err());
    }

    #[test]
    fn disk_space_override_is_process_local() {
        if let Ok(raw) = std::env::var("RIMZ_TEST_DISK_SPACE") {
            let result = disk_space(Path::new("/does-not-exist"));
            if raw == "1,100" {
                assert_eq!(
                    result.unwrap(),
                    DiskSpace {
                        free: 1,
                        total: 100
                    }
                );
            } else {
                assert!(result.is_err(), "malformed override must be refused");
            }
            return;
        }
        for raw in ["1,100", "invalid"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "disk::usage::tests::disk_space_override_is_process_local",
                ])
                .env("RIMZ_TEST_DISK_SPACE", raw)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }

    #[test]
    fn dir_size_sums_nested_tree() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("a.txt"), b"alpha").unwrap();
        fs::write(nested.join("b.txt"), b"bravo!").unwrap();

        assert!(
            dir_size(&root) >= 11,
            "file payload bytes are included with directory entries"
        );
        assert_eq!(dir_size(&temp.path().join("missing")), 0);
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("large.bin"), vec![0_u8; 16 * 1024]).unwrap();
        symlink(&outside, root.join("linked")).unwrap();

        assert!(
            dir_size(&root) < 16 * 1024,
            "symlink target contents are not charged to the root"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_counts_hardlinked_payload_once() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.bin");
        fs::write(&source, vec![0_u8; 16 * 1024]).unwrap();
        fs::hard_link(&source, root.join("alias.bin")).unwrap();

        let expected = fs::symlink_metadata(&root).unwrap().len()
            + fs::symlink_metadata(&source).unwrap().len();
        assert_eq!(dir_size(&root), expected);
    }

    #[test]
    fn measure_under_reports_roots_and_total() {
        let temp = tempdir().unwrap();
        let state = temp.path().join("state");
        let runtime = temp.path().join("runtime");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("store.json"), b"{}").unwrap();

        let disk_usage = measure_under(&[
            (StorageKind::Home, state.clone()),
            (StorageKind::Runtime, runtime.clone()),
        ]);

        assert_eq!(disk_usage.roots[0].kind, StorageKind::Home);
        assert_eq!(disk_usage.roots[0].path, state);
        assert!(disk_usage.roots[0].present);
        assert!(disk_usage.roots[0].bytes >= 2);
        assert_eq!(disk_usage.roots[1].kind, StorageKind::Runtime);
        assert_eq!(disk_usage.roots[1].path, runtime);
        assert!(!disk_usage.roots[1].present);
        assert_eq!(disk_usage.roots[1].bytes, 0);
        assert_eq!(
            disk_usage.total_bytes(),
            disk_usage
                .roots
                .iter()
                .fold(0_u64, |total, root| total.saturating_add(root.bytes))
        );
    }
}
