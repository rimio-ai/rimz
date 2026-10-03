//! Read-only resident-launch records shared with the sidebar planner.

use std::collections::BTreeMap;
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub at: Timestamp,
    pub leader: String,
}

pub type Ledger = BTreeMap<String, BTreeMap<PathBuf, LaunchRecord>>;

#[derive(Debug, thiserror::Error)]
pub enum LedgerErr {
    #[error(transparent)]
    Paths(#[from] crate::disk::paths::PathErr),
    #[error("reading resident launch ledger: {0}")]
    Read(#[from] std::io::Error),
    #[error("decoding resident launch ledger: {0}")]
    Decode(#[from] serde_json::Error),
}

pub(super) fn load_room(
    runtime: &crate::RuntimePaths,
    root: Option<&std::path::Path>,
) -> Result<Ledger, LedgerErr> {
    let paths = match root {
        Some(root) => crate::StatePaths::for_project_root(root)?,
        None => crate::StatePaths::for_workspace(runtime.workspace_id.clone())?,
    };
    load(&paths)
}

pub(super) fn path(paths: &crate::StatePaths) -> PathBuf {
    crate::StatePaths::class_path(
        &paths.root,
        crate::disk::paths::Class::Records,
        "loop-launches.json",
    )
}

pub fn load(paths: &crate::StatePaths) -> Result<Ledger, LedgerErr> {
    match std::fs::read(path(paths)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::new()),
        Err(error) => Err(error.into()),
    }
}
