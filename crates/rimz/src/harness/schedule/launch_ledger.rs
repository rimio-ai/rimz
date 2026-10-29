//! Read-only launch memory, judge declines, and condition capacity waits.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

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

/// Unfinished capacity waits still matching the planner's current condition clocks.
pub fn load_live_capacity(
    paths: &crate::StatePaths,
    runtime: &crate::RuntimePaths,
) -> Result<CapacityWaits, LedgerErr> {
    let clocks = super::fire::last_when_states(runtime);
    let mut waits = load_capacity(paths)?;
    waits.retain(|name, checkouts| {
        checkouts.retain(|checkout, wait| {
            !wait.gave_up
                && clocks
                    .get(&super::fire::scope_key(name, checkout))
                    .or_else(|| clocks.get(name))
                    .is_some_and(|clock| wait.since == Some(clock.since))
        });
        !checkouts.is_empty()
    });
    Ok(waits)
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
    if entry.stay {
        return residents.get(task)?.get(checkout).map(Launch::Resident);
    }
    if !entry.is_condition_workflow() {
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

pub(super) fn check_fingerprint(entry: &crate::config::TaskEntry) -> Option<String> {
    use sha2::{Digest as _, Sha256};

    let guard = entry.guard();
    if guard.shell.is_none() && guard.judges.is_empty() {
        return None;
    }
    let definition = serde_json::to_vec(&(guard.shell, guard.judges, entry.on)).ok()?;
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

pub(super) fn judge_recheck(judge: &crate::config::Judge) -> Option<Duration> {
    use crate::utils::time::{DurationUnit, parse_duration_units};
    let raw = judge.recheck.as_deref()?;
    if raw.trim() == "0" {
        return None;
    }
    parse_duration_units(
        raw,
        &[
            DurationUnit::Second,
            DurationUnit::Minute,
            DurationUnit::Hour,
            DurationUnit::Day,
        ],
    )
    .ok()
    .filter(|duration| !duration.is_zero())
}

pub(super) fn holding_decline<'a>(
    declines: &'a Declines,
    name: &str,
    entry: &crate::config::TaskEntry,
    checkout: &std::path::Path,
    since: Option<Timestamp>,
    now: Timestamp,
) -> Option<&'a DeclineRecord> {
    let decline = declines.get(name)?.get(checkout)?;
    let guard = entry.guard();
    let judge = guard.judges.get(decline.tier)?;
    if !entry.stay
        || decline.since != since
        || decline.fingerprint.as_ref()? != check_fingerprint(entry).as_ref()?
    {
        return None;
    }
    if judge.recheck.is_some() {
        let duration = judge_recheck(judge)?;
        if u128::try_from(now.duration_since(decline.at).as_millis()).unwrap_or(0)
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
    use crate::config::{CheckOn, Judge, TaskCheck, TaskEntry};

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
    fn live_capacity_requires_the_current_scoped_or_task_condition_clock() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = crate::ids::WorkspaceId::from_project_root(dir.path());
        let paths = crate::StatePaths::under(workspace.clone(), dir.path()).unwrap();
        let runtime = crate::RuntimePaths::under(workspace, dir.path()).unwrap();
        let at = Timestamp::from_second(100).unwrap();
        let wait = CapacityRecord {
            since: Some(at),
            first: at,
            last: at,
            count: 1,
            kinds: vec![crate::ids::AgentKind::new_unchecked("claude")],
            reason: "no capacity".into(),
            gives_up_at: None,
            gave_up: false,
        };
        let mut waits = CapacityWaits::new();
        for name in ["live", "stale", "missing", "no-since", "gave-up", "fanout"] {
            waits.insert(
                name.into(),
                BTreeMap::from([(PathBuf::from("/repo/a"), wait.clone())]),
            );
        }
        waits
            .get_mut("no-since")
            .unwrap()
            .get_mut(std::path::Path::new("/repo/a"))
            .unwrap()
            .since = None;
        waits
            .get_mut("gave-up")
            .unwrap()
            .get_mut(std::path::Path::new("/repo/a"))
            .unwrap()
            .gave_up = true;
        waits
            .get_mut("fanout")
            .unwrap()
            .insert("/repo/b".into(), wait);
        let path = capacity_path(&paths);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&waits).unwrap()).unwrap();
        let clock = |since| super::super::when::WhenState {
            fingerprint: None,
            since,
            fired: true,
        };
        let clocks = BTreeMap::from([
            ("live".into(), clock(at)),
            ("stale".into(), clock(Timestamp::from_second(101).unwrap())),
            ("no-since".into(), clock(at)),
            ("gave-up".into(), clock(at)),
            (
                super::super::fire::scope_key("fanout", std::path::Path::new("/repo/a")),
                clock(at),
            ),
        ]);
        let when = runtime.lane_path("loop-when.json");
        std::fs::create_dir_all(when.parent().unwrap()).unwrap();
        std::fs::write(when, serde_json::to_vec(&clocks).unwrap()).unwrap();
        let live = load_live_capacity(&paths, &runtime).unwrap();
        let scopes = live
            .iter()
            .flat_map(|(name, checkouts)| {
                checkouts
                    .keys()
                    .map(move |path| (name.as_str(), path.as_path()))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            scopes,
            [
                ("fanout", std::path::Path::new("/repo/a")),
                ("live", std::path::Path::new("/repo/a"))
            ]
        );
    }

    fn judge(prompt: &str, recheck: Option<&str>) -> Judge {
        Judge {
            agent: "codex".into(),
            prompt: Some(prompt.into()),
            prompt_file: None,
            timeout: None,
            recheck: recheck.map(str::to_owned),
        }
    }

    #[test]
    fn guard_fingerprint_covers_shell_judges_and_polarity() {
        let mut entry = TaskEntry {
            check: Some(TaskCheck::Shell("cargo test".into())),
            judge: vec![
                judge("first question", None),
                judge("second question", None),
            ],
            on: Some(CheckOn::Fail),
            ..Default::default()
        };
        let original = check_fingerprint(&entry).unwrap();
        for tier in 0..2 {
            let prompt = entry.judge[tier].prompt.replace("edited".into());
            assert_ne!(
                check_fingerprint(&entry).as_deref(),
                Some(original.as_str())
            );
            entry.judge[tier].prompt = prompt;
        }
        entry.check = Some(TaskCheck::Shell("cargo check".into()));
        assert_ne!(
            check_fingerprint(&entry).as_deref(),
            Some(original.as_str())
        );
        entry.check = Some(TaskCheck::Shell("cargo test".into()));
        entry.on = Some(CheckOn::Success);
        assert_ne!(
            check_fingerprint(&entry).as_deref(),
            Some(original.as_str())
        );
        entry.on = Some(CheckOn::Fail);
        entry.check = None;
        assert!(
            check_fingerprint(&entry).is_some(),
            "judge-only chains have a fingerprint"
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

    #[test]
    fn holding_decline_reads_the_recorded_judge_tier() {
        let entry = TaskEntry {
            check: Some(TaskCheck::Agent(judge("first question", None))),
            judge: vec![judge("second question", Some("1m"))],
            stay: true,
            on: Some(CheckOn::Success),
            ..Default::default()
        };
        let checkout = PathBuf::from("/checkout");
        let mut declines = Declines::from([(
            "guard".into(),
            BTreeMap::from([(
                checkout.clone(),
                DeclineRecord {
                    at: Timestamp::from_second(1000).unwrap(),
                    since: None,
                    reason: "nothing to do".into(),
                    profile: "codex".into(),
                    tier: 1,
                    kind: None,
                    model: None,
                    fingerprint: check_fingerprint(&entry),
                },
            )]),
        )]);
        for (second, holds) in [(1059, true), (1060, false)] {
            assert_eq!(
                holding_decline(
                    &declines,
                    "guard",
                    &entry,
                    &checkout,
                    None,
                    Timestamp::from_second(second).unwrap()
                )
                .is_some(),
                holds
            );
        }
        declines
            .get_mut("guard")
            .unwrap()
            .get_mut(&checkout)
            .unwrap()
            .tier = 2;
        assert!(
            holding_decline(
                &declines,
                "guard",
                &entry,
                &checkout,
                None,
                Timestamp::from_second(1000).unwrap()
            )
            .is_none(),
            "removed tiers do not hold"
        );
    }
}
