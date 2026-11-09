//! Read-only launch memory, judge declines, and condition capacity waits.

use std::collections::BTreeMap;
use std::path::PathBuf;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchRecord {
    pub at: Timestamp,
    pub leader: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<PathBuf>,
}

pub type Ledger = BTreeMap<String, BTreeMap<PathBuf, LaunchRecord>>;

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowLaunchRecord {
    pub at: Timestamp,
    pub run: crate::ids::WorkflowRunId,
    pub scope: crate::store::workflow::ScopeKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowLedger {
    version: u32,
    pub tasks: BTreeMap<String, BTreeMap<PathBuf, WorkflowLaunchRecord>>,
}

impl Default for WorkflowLedger {
    fn default() -> Self {
        Self {
            version: 1,
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
    let ledger: WorkflowLedger = match std::fs::read(workflow_path(paths)) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(WorkflowLedger::default());
        }
        Err(error) => return Err(error.into()),
    };
    if ledger.version != 1 {
        return Err(LedgerErr::Version(ledger.version));
    }
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

#[derive(Clone, Copy, Debug)]
pub enum Launch<'a> {
    Resident(&'a LaunchRecord),
    Workflow(&'a WorkflowLaunchRecord),
}

pub fn launched<'a>(
    entry: &crate::config::TaskEntry,
    task: &str,
    checkout: &std::path::Path,
    residents: &'a Ledger,
    workflows: &'a WorkflowLedger,
) -> Option<Launch<'a>> {
    // A resident remembered before the cutover still counts; its runs since are workflows.
    if entry.stay
        && let Some(launch) = residents
            .get(task)
            .and_then(|launches| launches.get(checkout))
    {
        return Some(Launch::Resident(launch));
    }
    if !entry.dedupes_checkout() {
        return None;
    }
    let launches = workflows.tasks.get(task)?;
    launches
        .values()
        .find(|record| record.scope == crate::store::workflow::ScopeKind::Project)
        .or_else(|| launches.get(checkout))
        .map(Launch::Workflow)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct DeclineRecord {
    pub(super) at: Timestamp,
    pub(super) since: Option<Timestamp>,
    pub(super) reason: String,
    pub(super) profile: String,
    #[serde(default)]
    pub(super) tier: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) kind: Option<crate::ids::AgentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) fingerprint: Option<String>,
}

pub(super) type Declines = BTreeMap<String, BTreeMap<PathBuf, DeclineRecord>>;

pub(super) fn decline_path(paths: &crate::StatePaths) -> PathBuf {
    crate::StatePaths::class_path(
        &paths.root,
        crate::disk::paths::Class::Records,
        "loop-declines.json",
    )
}

pub(super) fn load_declines(paths: &crate::StatePaths) -> Result<Declines, LedgerErr> {
    match std::fs::read(decline_path(paths)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Declines::new()),
        Err(error) => Err(error.into()),
    }
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

    #[test]
    fn legacy_decline_defaults_to_the_first_judge() {
        let decoded = serde_json::from_value::<DeclineRecord>(serde_json::json!({
            "at": "2026-06-01T00:00:00Z", "since": null,
            "profile": "codex", "reason": "nothing to do"
        }));
        assert!(
            decoded.is_ok(),
            "declines stored without a tier remain readable: {decoded:?}"
        );
        assert_eq!(decoded.unwrap().tier, 0);
    }
}
