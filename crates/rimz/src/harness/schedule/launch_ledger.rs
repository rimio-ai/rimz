//! Read-only resident launch and decline records shared with the sidebar planner.

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct DeclineRecord {
    pub(super) at: Timestamp,
    pub(super) since: Option<Timestamp>,
    pub(super) reason: String,
    pub(super) profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) fingerprint: Option<String>,
}

pub(super) type Declines = BTreeMap<String, BTreeMap<PathBuf, DeclineRecord>>;

pub(super) fn check_fingerprint(entry: &crate::config::TaskEntry) -> Option<String> {
    use sha2::{Digest as _, Sha256};

    let definition = serde_json::to_vec(&(entry.check.as_ref()?, entry.on)).ok()?;
    Some(hex::encode(Sha256::digest(definition)))
}

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

pub(super) fn load_declines_room(
    runtime: &crate::RuntimePaths,
    root: Option<&std::path::Path>,
) -> Result<Declines, LedgerErr> {
    let paths = match root {
        Some(root) => crate::StatePaths::for_project_root(root)?,
        None => crate::StatePaths::for_workspace(runtime.workspace_id.clone())?,
    };
    load_declines(&paths)
}

pub(super) fn holding_decline<'a>(
    declines: &'a Declines,
    name: &str,
    entry: &crate::config::TaskEntry,
    checkout: &std::path::Path,
    since: Option<Timestamp>,
    now: Timestamp,
) -> Option<&'a DeclineRecord> {
    use crate::utils::time::{DurationUnit, parse_duration_units};
    let crate::config::TaskCheck::Agent(check) = entry.check.as_ref()? else {
        return None;
    };
    let decline = declines.get(name)?.get(checkout)?;
    if !entry.stay
        || decline.since != since
        || decline.fingerprint.as_ref()? != check_fingerprint(entry).as_ref()?
    {
        return None;
    }
    if let Some(raw) = &check.recheck {
        if raw.trim() == "0" {
            return None;
        }
        let duration = parse_duration_units(
            raw,
            &[
                DurationUnit::Second,
                DurationUnit::Minute,
                DurationUnit::Hour,
                DurationUnit::Day,
            ],
        )
        .ok()?;
        if duration.is_zero()
            || u128::try_from(now.duration_since(decline.at).as_millis()).unwrap_or(0)
                >= duration.as_millis()
        {
            return None;
        }
    }
    Some(decline)
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerErr {
    #[error(transparent)]
    Paths(#[from] crate::disk::paths::PathErr),
    #[error("reading resident loop ledger: {0}")]
    Read(#[from] std::io::Error),
    #[error("decoding resident loop ledger: {0}")]
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
