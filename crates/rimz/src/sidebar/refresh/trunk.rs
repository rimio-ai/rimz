//! Repository-wide trunk transitions, independent of per-worktree diff probes.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::git_refs::TrunkRefs;
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
    /// The reftable stack's `tables.list` the trunk was last resolved
    /// against; `None` in the files backend or before the first resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reftable_stack: Option<String>,
    /// The `[sidebar] trunk` setting the trunk was resolved under, so a changed
    /// setting re-reads an unchanged reftable stack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    configured: Option<String>,
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
    let (trunk, reftable_stack) = resolve_trunk(repo, configured, &prior);
    // A pass that cannot resolve the trunk ref (a lock held mid `pack-refs`, an
    // unborn trunk, a failed reftable read) carries the prior reading forward,
    // so the next resolvable pass still compares against the last known trunk
    // instead of first sight. HEAD is never read: its state says nothing
    // about the trunk.
    let next = TrunkState {
        refreshed_at_ms: unix_now_ms(),
        trunk: trunk.or_else(|| prior.trunk.clone()),
        reftable_stack,
        configured: configured.map(str::to_owned),
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

/// The trunk this pass reads, and the reftable stack to record beside it. The
/// files backend reads ref files in process. A reftable stack and trunk setting
/// equal to the ones the prior trunk was resolved under reuse that trunk without
/// a fork; otherwise one bounded `git for-each-ref` runs, and a failed one
/// records no stack so the next pass retries.
fn resolve_trunk(
    repo: &Path,
    configured: Option<&str>,
    prior: &TrunkState,
) -> (Option<(String, String)>, Option<String>) {
    let stack = match super::git_refs::trunk(repo, configured) {
        None => return (None, None),
        Some(TrunkRefs::Files(trunk)) => return (trunk, None),
        Some(TrunkRefs::Reftable { stack }) => stack,
    };
    if prior.reftable_stack.as_ref() == Some(&stack) && prior.configured.as_deref() == configured {
        return (prior.trunk.clone(), Some(stack));
    }
    let mut args = vec![
        "for-each-ref",
        "--format=%(refname)%00%(objectname)%00%(symref)",
    ];
    let patterns = super::git_refs::trunk_ref_patterns(configured);
    args.extend(patterns.iter().map(String::as_str));
    match super::git_output(repo, &args).filter(|output| output.status.success()) {
        Some(output) => (
            super::git_refs::trunk_from_for_each_ref(configured, &output.stdout),
            Some(stack),
        ),
        None => (None, None),
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
    use crate::ids::WorkspaceId;
    use crate::proc::testkit::spawn_count;
    use serde_json::json;
    use std::process::Command;

    const MAIN: &str = "2222222222222222222222222222222222222222";
    const MOVED: &str = "5555555555555555555555555555555555555555";

    fn runtime_for(project: &Path) -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempfile::tempdir().unwrap();
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(project), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        (dir, runtime)
    }

    fn pass(repo: &Path, runtime: &RuntimePaths) -> TrunkState {
        pass_under(repo, runtime, None)
    }

    /// Run one lane pass under the `configured` trunk setting after aging the
    /// record past the TTL, and return it.
    fn pass_under(repo: &Path, runtime: &RuntimePaths, configured: Option<&str>) -> TrunkState {
        let path = runtime.trunk_state_path();
        let mut state: TrunkState = crate::disk::atomic::read_json_cache(&path);
        state.refreshed_at_ms = 0;
        crate::disk::atomic::write_temp_then_rename_cache(&path, &state).unwrap();
        let mut snapshot = SidebarSnapshot::build(
            WorkspaceId::from_project_root(repo),
            vec![],
            jiff::Timestamp::now(),
        );
        snapshot.project_root = Some(repo.to_path_buf());
        produce_trunk_state(&snapshot, runtime, configured);
        crate::disk::atomic::read_json_cache(&path)
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn an_unborn_head_still_tracks_the_files_trunk() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join(".git/HEAD"), "ref: refs/heads/feature\n");
        write(&dir.path().join(".git/refs/heads/main"), MAIN);
        let (_runtime_dir, runtime) = runtime_for(dir.path());

        assert_eq!(
            pass(dir.path(), &runtime).trunk,
            Some(("main".into(), MAIN.into()))
        );
        write(&dir.path().join(".git/refs/heads/main"), MOVED);
        assert_eq!(
            pass(dir.path(), &runtime).trunk,
            Some(("main".into(), MOVED.into()))
        );
    }

    /// Each HEAD state against both ref backends, plus a linked worktree as
    /// the repo input: the trunk resolves from the trunk ref alone.
    #[test]
    fn trunk_resolves_in_every_head_state_and_ref_backend() {
        for format in ["files", "reftable"] {
            for head in ["loose", "packed", "detached", "unborn", "linked"] {
                let dir = tempfile::tempdir().unwrap();
                let repo = dir.path().join("repo");
                let format_arg = format!("--ref-format={format}");
                git(
                    dir.path(),
                    &["init", "-q", &format_arg, "-b", "main", "repo"],
                );
                git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
                let sha = git(&repo, &["rev-parse", "refs/heads/main"]);
                let mut root = repo.clone();
                match head {
                    "packed" => drop(git(&repo, &["pack-refs", "--all"])),
                    "detached" => drop(git(&repo, &["checkout", "-q", "--detach"])),
                    "unborn" => drop(git(&repo, &["symbolic-ref", "HEAD", "refs/heads/none"])),
                    "linked" => {
                        root = dir.path().join("wt");
                        let wt = root.to_str().unwrap();
                        git(&repo, &["worktree", "add", "-q", "-b", "feature", wt]);
                    }
                    _ => {}
                }
                let (_runtime_dir, runtime) = runtime_for(&root);
                assert_eq!(
                    pass(&root, &runtime).trunk,
                    Some(("main".into(), sha)),
                    "{format} backend, {head} HEAD"
                );
            }
        }
    }

    #[test]
    fn a_reftable_trunk_forks_only_when_the_stack_changes() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        git(
            dir.path(),
            &["init", "-q", "--ref-format=reftable", "-b", "main", "repo"],
        );
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git(&repo, &["branch", "develop"]);
        let first = git(&repo, &["rev-parse", "main"]);
        let (_runtime_dir, runtime) = runtime_for(&repo);
        assert_eq!(
            pass(&repo, &runtime).trunk,
            Some(("main".into(), first.clone()))
        );

        let before = spawn_count();
        assert_eq!(
            pass(&repo, &runtime).trunk,
            Some(("main".into(), first.clone()))
        );
        assert_eq!(
            spawn_count() - before,
            0,
            "an unchanged stack forks nothing"
        );

        let before = spawn_count();
        assert_eq!(
            pass_under(&repo, &runtime, Some("develop")).trunk,
            Some(("develop".into(), first))
        );
        assert_eq!(spawn_count() - before, 1, "a changed setting re-reads");
        pass(&repo, &runtime);

        git(&repo, &["commit", "-q", "--allow-empty", "-m", "two"]);
        let second = git(&repo, &["rev-parse", "main"]);
        let before = spawn_count();
        assert_eq!(pass(&repo, &runtime).trunk, Some(("main".into(), second)));
        assert_eq!(spawn_count() - before, 1, "a moved stack forks once");
    }

    fn state(name: &str, sha: &str) -> TrunkState {
        TrunkState {
            refreshed_at_ms: 0,
            trunk: Some((name.into(), sha.into())),
            reftable_stack: None,
            configured: None,
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
