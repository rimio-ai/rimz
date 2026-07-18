//! Shared filesystem stamps and cache policy for provider-local discovery.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

pub(super) const LOCAL_SESSION_DISCOVERY_BACKSTOP: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProviderPathState {
    Missing,
    File,
    Directory,
    SymlinkOrOther,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProviderPathStamp {
    pub(super) state: ProviderPathState,
    pub(super) len: u64,
    pub(super) modified: Option<SystemTime>,
}

impl ProviderPathStamp {
    pub(super) fn read(path: &Path) -> Self {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                let file_type = metadata.file_type();
                let state = if file_type.is_file() {
                    ProviderPathState::File
                } else if file_type.is_dir() {
                    ProviderPathState::Directory
                } else {
                    ProviderPathState::SymlinkOrOther
                };
                Self {
                    state,
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self {
                state: ProviderPathState::Missing,
                len: 0,
                modified: None,
            },
            Err(_) => Self {
                state: ProviderPathState::Unavailable,
                len: 0,
                modified: None,
            },
        }
    }

    pub(super) fn is_file(&self) -> bool {
        self.state == ProviderPathState::File
    }

    pub(super) fn is_dir(&self) -> bool {
        self.state == ProviderPathState::Directory
    }

    pub(super) fn is_stable(&self) -> bool {
        self.state != ProviderPathState::Unavailable
    }

    pub(super) fn kind_only(mut self) -> Self {
        self.len = 0;
        self.modified = None;
        self
    }
}

pub(super) fn stamp_paths(
    paths: impl IntoIterator<Item = PathBuf>,
) -> Vec<(PathBuf, ProviderPathStamp)> {
    paths
        .into_iter()
        .map(|path| {
            let stamp = ProviderPathStamp::read(&path);
            (path, stamp)
        })
        .collect()
}

pub(super) fn stamps_unchanged(stamps: &[(PathBuf, ProviderPathStamp)]) -> bool {
    stamps
        .iter()
        .all(|(path, prior)| prior.is_stable() && ProviderPathStamp::read(path) == *prior)
}

pub(super) fn normalized_workspace_inputs(workspaces: &[&Path]) -> Vec<PathBuf> {
    let mut workspaces = workspaces
        .iter()
        .filter(|workspace| workspace.is_absolute())
        .map(|workspace| crate::worktree::normalize_path_lexical(workspace))
        .collect::<Vec<_>>();
    workspaces.sort();
    workspaces.dedup();
    workspaces
}

pub(super) fn full_scan_due(last_scan: Option<Instant>, now: Instant) -> bool {
    last_scan.is_none_or(|last_scan| {
        now.checked_duration_since(last_scan)
            .is_none_or(|elapsed| elapsed >= LOCAL_SESSION_DISCOVERY_BACKSTOP)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_distinguish_absence_files_directories_and_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let missing = ProviderPathStamp::read(&temp.path().join("missing"));
        let file = temp.path().join("file");
        fs::write(&file, "body").unwrap();
        let dir = temp.path().join("dir");
        fs::create_dir(&dir).unwrap();

        assert_eq!(missing.state, ProviderPathState::Missing);
        assert!(ProviderPathStamp::read(&file).is_file());
        assert!(ProviderPathStamp::read(&dir).is_dir());

        #[cfg(unix)]
        {
            let link = temp.path().join("link");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert_eq!(
                ProviderPathStamp::read(&link).state,
                ProviderPathState::SymlinkOrOther
            );
        }
    }

    #[test]
    fn normalizes_exact_inputs_and_expires_monotonically() {
        let inputs = normalized_workspace_inputs(&[
            Path::new("/work/one/./src/.."),
            Path::new("relative"),
            Path::new("/work/one"),
        ]);
        assert_eq!(inputs, [PathBuf::from("/work/one")]);

        let start = Instant::now();
        assert!(!full_scan_due(Some(start), start + Duration::from_secs(29)));
        assert!(full_scan_due(Some(start), start + Duration::from_secs(30)));
    }
}
