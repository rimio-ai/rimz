//! Read-only launch memory and condition capacity waits.

use std::collections::BTreeMap;
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityRecord {
    pub since: Option<Timestamp>,
    pub first: Timestamp,
    pub last: Timestamp,
    pub count: u64,
    pub kinds: Vec<crate::ids::AgentKind>,
    pub reason: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::ids::HoldKind::deserialize_optional"
    )]
    pub hold: Option<crate::ids::HoldKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gives_up_at: Option<Timestamp>,
    pub gave_up: bool,
}

pub type CapacityWaits = BTreeMap<String, BTreeMap<PathBuf, CapacityRecord>>;

pub(super) fn capacity_path(paths: &crate::StatePaths) -> PathBuf {
    crate::StatePaths::class_path(
        &paths.root,
        crate::disk::paths::Class::Records,
        "loop-capacity-waits.json",
    )
}

pub fn load_capacity(paths: &crate::StatePaths) -> Result<CapacityWaits, LedgerErr> {
    match std::fs::read(capacity_path(paths)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(CapacityWaits::new()),
        Err(error) => Err(error.into()),
    }
}

/// One remembered launch: the run a deduping task created there, or a resident launch
/// remembered before every fire became a workflow run, which carries its leader instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowLaunchRecord {
    pub at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<crate::ids::WorkflowRunId>,
    pub scope: crate::store::workflow::ScopeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<PathBuf>,
}

/// Version 2 holds the resident launches version 1 left in their own file; reading a version 1
/// file, or none, folds that file in, so the next write keeps them and the older file is done.
const VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowLedger {
    version: u32,
    pub tasks: BTreeMap<String, BTreeMap<PathBuf, WorkflowLaunchRecord>>,
}

impl Default for WorkflowLedger {
    fn default() -> Self {
        Self {
            version: VERSION,
            tasks: BTreeMap::new(),
        }
    }
}

pub(super) fn workflow_path(paths: &crate::StatePaths) -> PathBuf {
    crate::StatePaths::class_path(
        &paths.root,
        crate::disk::paths::Class::Records,
        "loop-workflow-launches.json",
    )
}

pub fn load_workflows(paths: &crate::StatePaths) -> Result<WorkflowLedger, LedgerErr> {
    let mut ledger: WorkflowLedger = match std::fs::read(workflow_path(paths)) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => WorkflowLedger {
            version: 1,
            tasks: BTreeMap::new(),
        },
        Err(error) => return Err(error.into()),
    };
    match ledger.version {
        VERSION => return Ok(ledger),
        1 => {}
        version => return Err(LedgerErr::Version(version)),
    }
    for (task, launches) in load_residents(paths)? {
        let remembered = ledger.tasks.entry(task).or_default();
        for (checkout, resident) in launches {
            remembered
                .entry(checkout)
                .or_insert_with(|| WorkflowLaunchRecord {
                    at: resident.at,
                    run: None,
                    scope: crate::store::workflow::ScopeKind::Worktree,
                    leader: Some(resident.leader),
                    destination: resident.destination,
                });
        }
    }
    ledger.tasks.retain(|_, launches| !launches.is_empty());
    ledger.version = VERSION;
    Ok(ledger)
}

pub(super) fn load_workflows_room(
    runtime: &crate::RuntimePaths,
    root: Option<&std::path::Path>,
) -> Result<WorkflowLedger, LedgerErr> {
    let paths = match root {
        Some(root) => crate::StatePaths::for_project_root(root)?,
        None => crate::StatePaths::for_workspace(runtime.workspace_id.clone())?,
    };
    load_workflows(&paths)
}

pub fn launched<'a>(
    entry: &crate::config::TaskEntry,
    task: &str,
    checkout: &std::path::Path,
    workflows: &'a WorkflowLedger,
) -> Option<&'a WorkflowLaunchRecord> {
    if !entry.dedupes_checkout() {
        return None;
    }
    let launches = workflows.tasks.get(task)?;
    launches
        .values()
        .find(|record| record.scope == crate::store::workflow::ScopeKind::Project)
        .or_else(|| launches.get(checkout))
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerErr {
    #[error(transparent)]
    Paths(#[from] crate::disk::paths::PathErr),
    #[error("reading loop ledger: {0}")]
    Read(#[from] std::io::Error),
    #[error("decoding loop ledger: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("unsupported workflow loop ledger version {0}")]
    Version(u32),
}

/// A launch the resident path remembered before its fires became workflow runs; it is read
/// only to fold into a version 1 workflow ledger.
#[derive(Deserialize)]
struct ResidentRecord {
    at: Timestamp,
    leader: String,
    #[serde(default)]
    destination: Option<PathBuf>,
}

#[cfg(test)]
pub(super) fn resident_path(paths: &crate::StatePaths) -> PathBuf {
    residents_at(paths)
}

fn residents_at(paths: &crate::StatePaths) -> PathBuf {
    crate::StatePaths::class_path(
        &paths.root,
        crate::disk::paths::Class::Records,
        "loop-launches.json",
    )
}

fn load_residents(
    paths: &crate::StatePaths,
) -> Result<BTreeMap<String, BTreeMap<PathBuf, ResidentRecord>>, LedgerErr> {
    match std::fs::read(residents_at(paths)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_kind_round_trips_and_legacy_records_still_load() {
        let mut value = serde_json::json!({
            "since":null, "first":"2026-01-01T00:00:00Z", "last":"2026-01-01T00:00:00Z",
            "count":1, "kinds":["claude"], "reason":"check failed", "gave_up":false,
            "hold":"check_failed"
        });
        let record: super::CapacityRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(record).unwrap(), value);
        value.as_object_mut().unwrap().remove("hold");
        let record: super::CapacityRecord = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(record).unwrap(), value);
    }
    #[test]
    fn capacity_loader_is_strict_and_read_only() {
        let root = tempfile::tempdir().unwrap();
        let paths = crate::StatePaths::for_project_root(root.path()).unwrap();
        assert!(load_capacity(&paths).unwrap().is_empty());
        assert!(!paths.root.exists());
        let path = capacity_path(&paths);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"broken").unwrap();
        assert!(
            load_capacity(&paths).is_err(),
            "corruption is not an empty wait file"
        );
    }
}
