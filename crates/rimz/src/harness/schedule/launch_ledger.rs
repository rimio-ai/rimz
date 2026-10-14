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
    #[serde(default)]
    pub(super) tier: usize,
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

pub(super) fn holding_decline<'a>(
    declines: &'a Declines,
    name: &str,
    entry: &crate::config::TaskEntry,
    checkout: &std::path::Path,
    since: Option<Timestamp>,
    now: Timestamp,
) -> Option<&'a DeclineRecord> {
    use crate::utils::time::{DurationUnit, parse_duration_units};
    let decline = declines.get(name)?.get(checkout)?;
    let guard = entry.guard();
    let judge = guard.judges.get(decline.tier)?;
    if !entry.stay
        || decline.since != since
        || decline.fingerprint.as_ref()? != check_fingerprint(entry).as_ref()?
    {
        return None;
    }
    if let Some(raw) = &judge.recheck {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CheckOn, Judge, TaskCheck, TaskEntry};

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
