//! Workspace metadata stored beside the store.
//!
//! `workspace.json` lets maintenance commands reason about known stores
//! after the project root has moved or disappeared. The store event log
//! remains the correctness source; this record is an index for
//! operator workflows such as `rimz gc` — and, in [`WorkspaceRecord::logins`],
//! the room's own launch-account truth, which the exec wrapper reads on every
//! launch without opening the store.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::disk::atomic::{self, write_temp_then_rename};
use crate::disk::paths::StatePaths;
use crate::ids::{RoomLogins, WorkspaceId};
use crate::workspace::{ResolvedWorkspace, RootClass};

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceRecordErr {
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("json parse error on {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, WorkspaceRecordErr>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceRecord {
    pub workspace_id: WorkspaceId,
    pub project_root: PathBuf,
    /// Active worktree cwd for room-local helper panes. Older records fall
    /// back to [`Self::project_root`] and self-heal on the next owner re-record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_root: Option<PathBuf>,
    pub session_name: String,
    /// Which ladder tier the root is. Records predating the field decode as
    /// [`RootClass::Repo`] — today's behavior — and self-heal on the next
    /// start/attach re-record.
    #[serde(default = "default_root_class")]
    pub root_class: RootClass,
    /// Room-owning RimZ binary used for session-local helpers such as the
    /// Zellij presence plugin. Generic re-records preserve it; owner flows
    /// (`start`, cwd-based `attach`, `reload`) set it explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rimz_bin: Option<PathBuf>,
    /// Digest of [`Self::rimz_bin`]. The pair is the verified executable target
    /// for long-lived room processes; legacy records omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rimz_build: Option<String>,
    /// The provider account each kind launches under, frozen at room birth.
    /// `None` is an unselected room — a record written before the field
    /// existed, or one cleared by `rimz reset`; `Some` is frozen until the
    /// next reset. Generic re-records preserve it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logins: Option<RoomLogins>,
    pub updated_at: Timestamp,
}

fn default_root_class() -> RootClass {
    RootClass::Repo
}

impl WorkspaceRecord {
    pub fn from_resolved(workspace: &ResolvedWorkspace) -> Self {
        Self {
            workspace_id: workspace.workspace_id.clone(),
            project_root: workspace.project_root.clone(),
            worktree_root: Some(workspace.worktree_root.clone()),
            session_name: workspace.session_name.clone(),
            root_class: workspace.root_class,
            rimz_bin: None,
            rimz_build: None,
            logins: None,
            updated_at: Timestamp::now(),
        }
    }
}

#[must_use = "durability barrier; check the result"]
pub fn write(paths: &StatePaths, record: &WorkspaceRecord) -> Result<()> {
    write_path(&paths.workspace_record, record)?;
    Ok(())
}

#[must_use = "durability barrier; check the result"]
pub fn write_path(path: &Path, record: &WorkspaceRecord) -> Result<()> {
    write_temp_then_rename(path, record)?;
    Ok(())
}

/// The record, or `None` where the room has never written one. A record that
/// exists but cannot be parsed is an error: the room's frozen account
/// selection lives here, and silently treating a corrupt file as absent would
/// reset it.
pub fn read_optional(path: &Path) -> Result<Option<WorkspaceRecord>> {
    match read(path) {
        Ok(record) => Ok(Some(record)),
        Err(WorkspaceRecordErr::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

pub fn read(path: &Path) -> Result<WorkspaceRecord> {
    let bytes = fs::read(path).map_err(|source| WorkspaceRecordErr::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| WorkspaceRecordErr::Json {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceResolver;
    use tempfile::tempdir;

    #[test]
    fn record_round_trips() {
        let dir = tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let workspace = WorkspaceResolver::resolve(&project, None).unwrap();
        let paths = StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
        let mut record = WorkspaceRecord::from_resolved(&workspace);
        record.rimz_bin = Some(dir.path().join("builds/build/rimz"));
        record.rimz_build = Some("build".to_owned());
        record.logins = Some(RoomLogins::from([(
            crate::ids::AgentKind::new_unchecked("claude"),
            "work".parse().unwrap(),
        )]));

        write(&paths, &record).unwrap();
        let loaded = read(&paths.workspace_record).unwrap();

        assert_eq!(loaded.workspace_id, workspace.workspace_id);
        assert_eq!(loaded.project_root, workspace.project_root);
        assert_eq!(
            loaded.worktree_root.as_ref(),
            Some(&workspace.worktree_root)
        );
        assert_eq!(loaded.session_name, workspace.session_name);
        assert_eq!(loaded.rimz_bin, record.rimz_bin);
        assert_eq!(loaded.rimz_build, record.rimz_build);
        assert_eq!(loaded.logins, record.logins);
    }

    #[test]
    fn legacy_record_without_rimz_bin_parses() {
        let record: WorkspaceRecord = serde_json::from_str(
            r#"{
                "workspace_id": "ws_0123456789abcdef01234567",
                "project_root": "/repo",
                "session_name": "rimz-repo",
                "root_class": "repo",
                "updated_at": "2024-01-01T00:00:00Z"
            }"#,
        )
        .expect("legacy record parses");

        assert_eq!(record.rimz_bin, None);
        assert_eq!(record.rimz_build, None);
        assert_eq!(record.worktree_root, None);
        assert_eq!(record.logins, None);
    }

    #[test]
    fn read_optional_separates_an_absent_record_from_a_corrupt_one() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("workspace.json");
        assert!(read_optional(&missing).unwrap().is_none());

        std::fs::write(&missing, b"{ not json").unwrap();
        assert!(matches!(
            read_optional(&missing),
            Err(WorkspaceRecordErr::Json { .. })
        ));
    }
}
