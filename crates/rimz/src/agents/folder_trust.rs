//! Provider-owned folder-trust decisions and explicitly approved writes.

use std::path::{Path, PathBuf};

use super::login::RoomLoginSet;
use crate::ids::LoginName;

#[derive(Debug, PartialEq, Eq)]
pub enum FolderTrust {
    Decided,
    Undecided(FolderTrustGap),
}

#[derive(Debug, PartialEq, Eq)]
pub struct FolderTrustGap {
    pub path: PathBuf,
    pub key: PathBuf,
    pub grant: Result<FolderTrustPreview, String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FolderTrustPreview {
    pub original: Option<String>,
    pub candidate: String,
}

impl FolderTrustGap {
    pub fn fix(&self, kind: &str) -> String {
        match &self.grant {
            Ok(_) => format!(
                "run `rimz trust grant --agents {kind}` in `{}`, or answer {kind}'s folder-trust prompt once there",
                self.key.display()
            ),
            Err(reason) => format!(
                "{reason}; or answer {kind}'s folder-trust prompt once in `{}`",
                self.key.display()
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FolderTrustErr {
    #[error("{0}")]
    NotGrantable(String),
    #[error("{} changed since the preview; run the command again", path.display())]
    Changed { path: PathBuf },
    #[error(transparent)]
    Io(#[from] crate::disk::atomic::AtomicErr),
}

/// Publish an approved preview only if its original bytes are still current.
pub fn grant_folder_trust(gap: &FolderTrustGap) -> Result<(), FolderTrustErr> {
    use crate::disk::atomic::AtomicErr;
    let preview = gap
        .grant
        .as_ref()
        .map_err(|reason| FolderTrustErr::NotGrantable(reason.clone()))?;
    let current = match std::fs::read(&gap.path) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(AtomicErr::Io {
                path: gap.path.clone(),
                source,
            }
            .into());
        }
    };
    if current.as_deref() != preview.original.as_deref().map(str::as_bytes) {
        return Err(FolderTrustErr::Changed {
            path: gap.path.clone(),
        });
    }
    if let Some(parent) = gap
        .path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|source| AtomicErr::Io {
            path: parent.into(),
            source,
        })?;
    }
    super::provider_file::write_bytes(&gap.path, preview.candidate.as_bytes())?;
    Ok(())
}

#[derive(Debug)]
pub struct FolderTrustRow {
    pub kind: &'static str,
    pub login: LoginName,
    pub trust: FolderTrust,
}

pub fn folder_trust_rows(
    logins: &RoomLoginSet,
    cwd: &Path,
    repo_root: &Path,
) -> Vec<FolderTrustRow> {
    rows_with_locator(logins, cwd, repo_root, super::locate_binary)
}

fn rows_with_locator(
    logins: &RoomLoginSet,
    cwd: &Path,
    repo_root: &Path,
    locate: impl Fn(&super::AgentSpec) -> Option<PathBuf>,
) -> Vec<FolderTrustRow> {
    super::all_definitions()
        .filter_map(|adapter| {
            locate(adapter.spec())?;
            let kind = adapter.spec().kind;
            let login = logins.login(kind)?;
            let trust = adapter.folder_trust(cwd, Some(repo_root), &logins.env(&login))?;
            Some(FolderTrustRow {
                kind,
                login: login.name().clone(),
                trust,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
