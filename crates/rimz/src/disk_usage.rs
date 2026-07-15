//! RimZ-owned disk usage measurement: symlink-safe byte walks plus the account
//! roots `doctor` and `gc` surface.

use std::fs;
use std::path::{Path, PathBuf};

use crate::store::paths;

const RIMZ_SUBDIR: &str = "rimz";

/// Best-effort recursive size for a path without following symlinks.
pub fn dir_size(path: &Path) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(_) => return 0,
    };
    let mut bytes = meta.len();
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return bytes;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return bytes;
    };
    for entry in entries.flatten() {
        bytes = bytes.saturating_add(dir_size(&entry.path()));
    }
    bytes
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageKind {
    State,
    Runtime,
    Data,
    Cache,
    Config,
}

impl StorageKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Runtime => "runtime",
            Self::Data => "data",
            Self::Cache => "cache",
            Self::Config => "config",
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
        (StorageKind::State, paths::state_home().join(RIMZ_SUBDIR)),
        (
            StorageKind::Runtime,
            paths::runtime_home().join(RIMZ_SUBDIR),
        ),
        (StorageKind::Data, paths::data_home().join(RIMZ_SUBDIR)),
        (StorageKind::Cache, paths::cache_home().join(RIMZ_SUBDIR)),
        (StorageKind::Config, paths::config_home().join(RIMZ_SUBDIR)),
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

    #[test]
    fn measure_under_reports_roots_and_total() {
        let temp = tempdir().unwrap();
        let state = temp.path().join("state");
        let runtime = temp.path().join("runtime");
        fs::create_dir_all(&state).unwrap();
        fs::write(state.join("store.json"), b"{}").unwrap();

        let disk_usage = measure_under(&[
            (StorageKind::State, state.clone()),
            (StorageKind::Runtime, runtime.clone()),
        ]);

        assert_eq!(disk_usage.roots[0].kind, StorageKind::State);
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
