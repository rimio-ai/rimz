//! Machine-wide uninstall helpers.
//!
//! The CLI owns prompting and presentation; this module owns the filesystem
//! mechanics so tests can drive them against tempdirs.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::ledger::paths;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removed {
    Removed,
    AlreadyAbsent,
}

#[derive(Debug)]
pub struct RemovalOutcome {
    pub path: PathBuf,
    pub result: io::Result<Removed>,
}

impl RemovalOutcome {
    pub fn removed(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            result: Ok(Removed::Removed),
        }
    }

    pub fn already_absent(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            result: Ok(Removed::AlreadyAbsent),
        }
    }

    pub fn failed(path: impl Into<PathBuf>, err: io::Error) -> Self {
        Self {
            path: path.into(),
            result: Err(err),
        }
    }
}

pub fn remove_root(path: &Path) -> RemovalOutcome {
    match fs::remove_dir_all(path) {
        Ok(()) => RemovalOutcome::removed(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => RemovalOutcome::already_absent(path),
        Err(err) => RemovalOutcome::failed(path, err),
    }
}

pub fn remove_runtime_root() -> Vec<RemovalOutcome> {
    remove_runtime_root_at(
        &paths::runtime_home(),
        paths::env_path("XDG_RUNTIME_DIR").is_none(),
    )
}

pub fn remove_runtime_root_at(
    runtime_home: &Path,
    cleanup_fallback_parent: bool,
) -> Vec<RemovalOutcome> {
    let mut outcomes = vec![remove_root(&runtime_home.join("rimz"))];
    if cleanup_fallback_parent {
        match dir_is_empty(runtime_home) {
            Ok(true) => outcomes.push(remove_empty_dir(runtime_home)),
            Ok(false) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => outcomes.push(RemovalOutcome::failed(runtime_home, err)),
        }
    }
    outcomes
}

fn remove_empty_dir(path: &Path) -> RemovalOutcome {
    match fs::remove_dir(path) {
        Ok(()) => RemovalOutcome::removed(path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => RemovalOutcome::already_absent(path),
        Err(err) => RemovalOutcome::failed(path, err),
    }
}

fn dir_is_empty(path: &Path) -> io::Result<bool> {
    Ok(fs::read_dir(path)?.next().is_none())
}

pub fn binary_candidates(
    current_exe: Option<PathBuf>,
    cargo_bin: Option<PathBuf>,
    system_bin: &Path,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = current_exe {
        candidates.push(path);
    }
    if let Some(dir) = cargo_bin {
        candidates.push(dir.join("rimz"));
    }
    candidates.push(system_bin.join("rimz"));

    let mut seen = HashSet::new();
    let mut existing = Vec::new();
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if seen.insert(key) {
            existing.push(path);
        }
    }
    existing
}

pub fn remove_binaries(candidates: &[PathBuf]) -> Vec<RemovalOutcome> {
    candidates
        .iter()
        .map(|path| match fs::remove_file(path) {
            Ok(()) => RemovalOutcome::removed(path),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                RemovalOutcome::already_absent(path)
            }
            Err(err) => RemovalOutcome::failed(path, err),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn remove_root_sweeps_tree() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("rimz");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/file"), b"data").unwrap();

        let outcome = remove_root(&root);

        assert!(matches!(outcome.result, Ok(Removed::Removed)));
        assert!(!root.exists());
    }

    #[test]
    fn remove_root_tolerates_missing_path() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("missing");

        let outcome = remove_root(&root);

        assert!(matches!(outcome.result, Ok(Removed::AlreadyAbsent)));
    }

    #[test]
    fn binary_candidates_keep_existing_files_and_dedupe_canonical_paths() {
        let temp = tempdir().unwrap();
        let bin = temp.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let rimz = bin.join("rimz");
        fs::write(&rimz, b"binary").unwrap();

        let candidates = binary_candidates(Some(rimz.clone()), Some(bin.clone()), &bin);

        assert_eq!(candidates, vec![rimz]);
    }

    #[test]
    fn remove_runtime_root_removes_empty_fallback_parent() {
        let runtime = tempdir().unwrap();
        fs::create_dir_all(runtime.path().join("rimz/ws")).unwrap();

        let outcomes = remove_runtime_root_at(runtime.path(), true);

        assert!(
            outcomes
                .iter()
                .any(|outcome| outcome.path == runtime.path() && outcome.result.is_ok())
        );
        assert!(!runtime.path().exists());
    }
}
