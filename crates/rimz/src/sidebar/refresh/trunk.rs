//! Repository-wide trunk transitions, independent of per-worktree diff probes.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::RuntimePaths;
use crate::disk::single_flight::Coalesced;
use crate::sidebar::timing::TRUNK_STATE_TTL;
use crate::store::event::GitSignal;
use crate::store::snapshot::SidebarSnapshot;
use crate::utils::time::unix_now_ms;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct TrunkState {
    refreshed_at_ms: u64,
    trunk: Option<(String, String)>,
}

pub(super) fn produce_trunk_state(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    configured: Option<&str>,
) {
    let Some(repo) = snapshot.project_root.as_deref() else {
        return;
    };
    if crate::worktree::git_admin_dir_from_checkout_metadata(repo)
        .ok()
        .flatten()
        .is_none()
    {
        return;
    }
    let path = runtime.trunk_state_path();
    let fresh = || {
        let state: TrunkState = crate::disk::atomic::read_json_cache(&path);
        (unix_now_ms().saturating_sub(state.refreshed_at_ms) < TRUNK_STATE_TTL.as_millis() as u64)
            .then_some(state)
    };
    if fresh().is_some() {
        return;
    }
    let lock = runtime.lock_path("trunk-state.lock");
    let _guard =
        match crate::disk::single_flight::coalesce(&lock, Duration::from_millis(20), 15, fresh) {
            Coalesced::Produce(guard) => guard,
            Coalesced::Shared(_) | Coalesced::ProduceLocal => return,
        };
    if fresh().is_some() {
        return;
    }
    let prior: TrunkState = crate::disk::atomic::read_json_cache(&path);
    // A pass that cannot resolve the refs (a lock held mid `pack-refs`, an
    // unborn trunk) carries the prior reading forward, so the next resolvable
    // pass still compares against the last known trunk instead of first sight.
    let next = TrunkState {
        refreshed_at_ms: unix_now_ms(),
        trunk: super::git_refs::resolve(repo, configured)
            .map(|refs| (refs.trunk_name, refs.trunk_sha))
            .or_else(|| prior.trunk.clone()),
    };
    if let Err(error) = crate::disk::atomic::write_temp_then_rename_cache(&path, &next) {
        tracing::debug!(%error, "sidebar trunk-state cache write failed");
        return;
    }
    if let Some(payload) = transition(&prior, &next, repo)
        && let Err(error) = crate::child_process::spawn_detached_rimz(
            runtime,
            transition_argv(repo, &payload),
            "trunk-signal-emit",
        )
    {
        tracing::debug!(%error, "sidebar: failed to spawn trunk signal emitter");
    }
}

fn transition(prior: &TrunkState, next: &TrunkState, repo: &Path) -> Option<Map<String, Value>> {
    let (prior_name, from) = prior.trunk.as_ref()?;
    let (trunk, to) = next.trunk.as_ref()?;
    if prior_name != trunk || from == to {
        return None;
    }
    Some(Map::from_iter([
        ("trunk".into(), Value::String(trunk.clone())),
        ("from".into(), Value::String(from.clone())),
        ("to".into(), Value::String(to.clone())),
        (
            "repo".into(),
            Value::String(repo.to_string_lossy().into_owned()),
        ),
    ]))
}

fn transition_argv(repo: &Path, payload: &Map<String, Value>) -> Vec<OsString> {
    let payload = serde_json::to_string(payload)
        .expect("trunk payload contains only serializable JSON values");
    vec![
        "--root".into(),
        repo.as_os_str().to_owned(),
        "events".into(),
        "emit".into(),
        GitSignal::TrunkMoved.as_str().into(),
        "--source".into(),
        "git".into(),
        "--json".into(),
        payload.into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state(name: &str, sha: &str) -> TrunkState {
        TrunkState {
            refreshed_at_ms: 0,
            trunk: Some((name.into(), sha.into())),
        }
    }

    #[test]
    fn only_same_trunk_sha_changes_emit() {
        let prior = state("main", "old");
        let next = state("main", "new");
        let repo = Path::new("/repo");
        assert_eq!(
            transition(&prior, &next, repo),
            json!({"trunk": "main", "from": "old", "to": "new", "repo": "/repo"})
                .as_object()
                .cloned()
        );
        for (prior, next) in [
            (TrunkState::default(), next.clone()),
            (prior.clone(), prior.clone()),
            (prior.clone(), state("master", "new")),
            (prior, TrunkState::default()),
        ] {
            assert!(transition(&prior, &next, repo).is_none());
        }
    }

    #[test]
    fn trunk_signal_argv_keeps_root_name_and_payload_as_distinct_values() {
        let payload = json!({"trunk": "main", "from": "a", "to": "b", "repo": "/repo with spaces"});
        let args = transition_argv(Path::new("/repo with spaces"), payload.as_object().unwrap());
        assert_eq!(
            args,
            vec![
                OsString::from("--root"),
                "/repo with spaces".into(),
                "events".into(),
                "emit".into(),
                "trunk.moved".into(),
                "--source".into(),
                "git".into(),
                "--json".into(),
                payload.to_string().into()
            ]
        );
    }
}
