//! Workspace metadata stored beside the ledger.
//!
//! `workspace.json` lets maintenance commands reason about known ledgers
//! after the project root has moved or disappeared. The ledger and feed files
//! remain the correctness source for requests; this record is an index for
//! operator workflows such as `workspace prune`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::ids::WorkspaceId;
use crate::ledger::atomic::{self, write_temp_then_rename};
use crate::ledger::paths::StatePaths;
use crate::workspace::ResolvedWorkspace;

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceRecordErr {
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error("io error on {path}: {source}")]
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
    pub session_name: String,
    pub updated_at: Timestamp,
}

impl WorkspaceRecord {
    pub fn from_resolved(workspace: &ResolvedWorkspace) -> Self {
        Self {
            workspace_id: workspace.workspace_id.clone(),
            project_root: workspace.project_root.clone(),
            session_name: workspace.session_name.clone(),
            updated_at: Timestamp::now(),
        }
    }
}

#[must_use = "durability barrier; check the result"]
pub fn write(paths: &StatePaths, record: &WorkspaceRecord) -> Result<()> {
    write_temp_then_rename(&paths.workspace_record, record)?;
    Ok(())
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
        let record = WorkspaceRecord::from_resolved(&workspace);

        write(&paths, &record).unwrap();
        let loaded = read(&paths.workspace_record).unwrap();

        assert_eq!(loaded.workspace_id, workspace.workspace_id);
        assert_eq!(loaded.project_root, workspace.project_root);
        assert_eq!(loaded.session_name, workspace.session_name);
    }
}
