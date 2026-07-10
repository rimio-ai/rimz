//! Durable per-run records for supervised `rimz agents -p` turns.
//!
//! Run records are cold-path durable state: a waiting CLI may exit, a user may
//! inspect the result later with `rimz agents show`, and the final assistant text
//! is the product output. Writes therefore use fsyncing temp-file-plus-rename,
//! unlike cache sidecars whose correctness rides the event log.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::harness::run::RunRecord;
use crate::ids::RunId;
use crate::store::atomic::{self, write_temp_then_rename};

#[derive(Debug, thiserror::Error)]
pub enum RunStoreErr {
    #[error("run {0} not found")]
    NotFound(RunId),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error(transparent)]
    Lock(#[from] crate::store::lock::LockErr),
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
    #[error("run {run_id} is {actual}; expected {expected}")]
    InvalidStatus {
        run_id: RunId,
        actual: &'static str,
        expected: &'static str,
    },
}

pub type Result<T> = std::result::Result<T, RunStoreErr>;

fn run_path(runs_dir: &Path, run_id: &RunId) -> PathBuf {
    runs_dir.join(format!("{run_id}.json"))
}

#[must_use = "durability barrier; check the result"]
pub fn write(runs_dir: &Path, record: &RunRecord) -> Result<()> {
    write_temp_then_rename(&run_path(runs_dir, &record.run_id), record)?;
    Ok(())
}

pub fn load(runs_dir: &Path, run_id: &RunId) -> Result<RunRecord> {
    let path = run_path(runs_dir, run_id);
    if !path.exists() {
        return Err(RunStoreErr::NotFound(run_id.clone()));
    }
    let bytes = fs::read(&path).map_err(|source| RunStoreErr::Io {
        path: path.clone(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| RunStoreErr::Json { path, source })
}

pub fn list(runs_dir: &Path) -> Result<Vec<RunRecord>> {
    if !runs_dir.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(runs_dir).map_err(|source| RunStoreErr::Io {
        path: runs_dir.to_path_buf(),
        source,
    })?;
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| RunStoreErr::Io {
            path: runs_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path).map_err(|source| RunStoreErr::Io {
            path: path.clone(),
            source,
        })?;
        records.push(
            serde_json::from_slice::<RunRecord>(&bytes).map_err(|source| RunStoreErr::Json {
                path: path.clone(),
                source,
            })?,
        );
    }
    records.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::run::{PermissionMode, RunStatus};
    use crate::ids::{AgentKind, WorkspaceId};
    use tempfile::tempdir;

    #[test]
    fn write_load_and_list_runs() {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let mut first = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "first".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        let mut second = RunRecord::new(
            workspace_id,
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "second".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        first.status = RunStatus::Completed;
        second.updated_at = first.updated_at + std::time::Duration::from_secs(1);

        write(dir.path(), &first).unwrap();
        write(dir.path(), &second).unwrap();

        let loaded = load(dir.path(), &first.run_id).unwrap();
        assert_eq!(loaded.prompt, "first");
        let listed = list(dir.path()).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].run_id, second.run_id);
    }
}
