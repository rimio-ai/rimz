use std::path::{Path, PathBuf};
use std::process::Command;

use crate::RuntimePaths;
use crate::disk::atomic;
use crate::ids::WorkspaceId;
use crate::proc::testkit::spawn_count;
use crate::sidebar::refresh::git_stats::WorktreeRootsCache;
use crate::utils::time::unix_now_ms;
use crate::workspace::RootClass;

use super::{list_group_roots, list_worktree_roots, project_group_roots};

fn git(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn runtime_for(project: &Path) -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(project), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, runtime)
}

fn read_lane(runtime: &RuntimePaths) -> Option<WorktreeRootsCache> {
    atomic::read_json_cache(&runtime.group_roots_path())
}

#[test]
fn list_worktree_roots_includes_a_checkout_outside_the_project() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    if !git(&main, &["init", "-q"]) {
        // No git on PATH; the helper degrades to an empty list, which leaves
        // the reducer's project_root prefix test to stand alone.
        assert!(list_worktree_roots(&main).is_empty());
        return;
    }
    let _ = git(&main, &["config", "user.email", "t@example.com"]);
    let _ = git(&main, &["config", "user.name", "t"]);
    std::fs::write(main.join("f"), "x").unwrap();
    let _ = git(&main, &["add", "f"]);
    let _ = git(&main, &["commit", "-q", "-m", "init"]);

    // A worktree parked OUTSIDE the project root (a sibling of `main`).
    let external = tmp.path().join("external-wt");
    let _ = git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            external.to_str().unwrap(),
            "-b",
            "feature",
        ],
    );

    let roots = list_worktree_roots(&main);
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let roots: Vec<PathBuf> = roots.iter().map(|r| canon(r)).collect();
    assert!(
        roots.contains(&canon(&main)),
        "the main checkout is one of the worktree roots"
    );
    assert!(
        roots.contains(&canon(&external)),
        "a worktree outside the project root is enumerated, so it groups as project-related"
    );

    // A non-repository path has no worktrees to list.
    let plain = tempfile::tempdir().unwrap();
    assert!(list_worktree_roots(plain.path()).is_empty());
}

#[test]
fn project_group_roots_publishes_exact_marker_names() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    if !git(&main, &["init", "-q"]) {
        return;
    }
    let _ = git(&main, &["config", "user.email", "t@example.com"]);
    let _ = git(&main, &["config", "user.name", "t"]);
    std::fs::write(main.join("f"), "x").unwrap();
    let _ = git(&main, &["add", "f"]);
    let _ = git(&main, &["commit", "-q", "-m", "init"]);
    let external = tmp.path().join("external-wt");
    assert!(git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            external.to_str().unwrap(),
            "-b",
            "feature",
        ],
    ));
    let marker = crate::worktree::WorktreeMarker {
        version: 1,
        name: "feature".to_owned(),
        branch: "feature".to_owned(),
        base_branch: Some("main".to_owned()),
        from_pr: None,
        base_ref: "HEAD".to_owned(),
        repo_root: main.clone(),
        worktree_path: external.clone(),
        created_at: jiff::Timestamp::now(),
    };
    let output = Command::new("git")
        .arg("-C")
        .arg(&external)
        .args(["rev-parse", "--git-dir"])
        .output()
        .expect("git dir");
    assert!(output.status.success(), "resolve git dir");
    let git_dir = PathBuf::from(
        String::from_utf8(output.stdout)
            .expect("utf8 git dir")
            .trim(),
    );
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        external.join(git_dir)
    };
    atomic::write_temp_then_rename(&git_dir.join("rimz-worktree.json"), &marker).unwrap();
    let (_runtime_dir, runtime) = runtime_for(&main);

    let floor = unix_now_ms();
    let legacy = serde_json::json!({
        "entries": { main.display().to_string(): { "refreshed_at_ms": floor, "added": 7 } },
        "worktrees": {
            "refreshed_at_ms": floor - 1,
            "roots": [main.join("old-root")],
            "marker_names": {}
        }
    });
    atomic::write_temp_then_rename_cache(&runtime.diff_stats_path(), &legacy).unwrap();
    let before = std::fs::read(runtime.diff_stats_path()).unwrap();

    let roots = project_group_roots(&main, RootClass::Repo, &runtime, Some(floor));
    assert_eq!(
        std::fs::read(runtime.diff_stats_path()).unwrap(),
        before,
        "enumerating group roots must leave diff-stats.json byte-identical"
    );
    let cache = read_lane(&runtime).expect("the roots lane must hold the enumeration");

    assert!(roots.contains(&main));
    assert!(roots.contains(&external));
    assert_eq!(cache.roots, roots);
    assert_eq!(
        cache.marker_names.get(&external).map(String::as_str),
        Some("feature")
    );
    assert!(!cache.marker_names.contains_key(&main));
}

#[test]
fn project_group_roots_serves_a_fresh_lane_without_forks_or_writes() {
    let room = tempfile::tempdir().unwrap();
    let (_dir, runtime) = runtime_for(room.path());
    let roots = vec![room.path().join("cached")];
    let stamp = unix_now_ms();
    atomic::write_temp_then_rename_cache(
        &runtime.group_roots_path(),
        &serde_json::json!({ "refreshed_at_ms": stamp, "roots": roots, "marker_names": {} }),
    )
    .unwrap();
    let before = std::fs::read(runtime.group_roots_path()).unwrap();
    let forks = spawn_count();

    assert_eq!(
        project_group_roots(room.path(), RootClass::Repo, &runtime, Some(stamp)),
        roots
    );
    assert_eq!(spawn_count() - forks, 0);
    assert_eq!(std::fs::read(runtime.group_roots_path()).unwrap(), before);
    assert!(!runtime.diff_stats_path().exists());
}

#[test]
fn project_group_roots_floor_refuses_a_fresh_lane() {
    let room = tempfile::tempdir().unwrap();
    assert!(git(room.path(), &["init", "-q"]));
    let (_dir, runtime) = runtime_for(room.path());
    let floor = unix_now_ms();
    atomic::write_temp_then_rename_cache(
        &runtime.group_roots_path(),
        &serde_json::json!({
            "refreshed_at_ms": floor - 1,
            "roots": [room.path().join("old-root")],
            "marker_names": {}
        }),
    )
    .unwrap();
    let forks = spawn_count();
    let roots = project_group_roots(room.path(), RootClass::Repo, &runtime, Some(floor));
    assert_eq!(spawn_count() - forks, 1);
    assert_eq!(roots, vec![room.path().to_path_buf()]);
    let cache = read_lane(&runtime).expect("the roots lane must hold the re-enumeration");
    assert!(
        cache.refreshed_at_ms >= floor,
        "the floor forces a new publication"
    );
    assert_eq!(cache.roots, roots);
}

#[test]
fn project_group_roots_backs_off_after_a_failed_probe() {
    let room = tempfile::tempdir().unwrap();
    let (_dir, runtime) = runtime_for(room.path());
    let started_ms = unix_now_ms();
    let forks = spawn_count();
    assert!(project_group_roots(room.path(), RootClass::Repo, &runtime, None).is_empty());
    assert_eq!(spawn_count() - forks, 1);
    let cache = read_lane(&runtime).expect("a failed probe publishes its empty result for backoff");
    assert!(cache.roots.is_empty());
    assert!(cache.refreshed_at_ms >= started_ms);
    let before = std::fs::read(runtime.group_roots_path()).unwrap();
    let forks = spawn_count();

    assert!(project_group_roots(room.path(), RootClass::Repo, &runtime, None).is_empty());
    assert_eq!(spawn_count() - forks, 0);
    assert_eq!(std::fs::read(runtime.group_roots_path()).unwrap(), before);
}

#[test]
fn project_group_roots_replaces_an_unreadable_lane() {
    let room = tempfile::tempdir().unwrap();
    assert!(git(room.path(), &["init", "-q"]));
    let (_dir, runtime) = runtime_for(room.path());
    std::fs::write(runtime.group_roots_path(), b"garbage").unwrap();
    let forks = spawn_count();

    let roots = project_group_roots(room.path(), RootClass::Repo, &runtime, None);
    assert_eq!(spawn_count() - forks, 1);
    assert_eq!(roots, vec![room.path().to_path_buf()]);
    let cache =
        read_lane(&runtime).expect("an unreadable lane must be replaced with a valid record");
    assert_eq!(cache.roots, roots);
}

#[test]
fn project_group_roots_returns_the_enumeration_when_publication_fails() {
    let room = tempfile::tempdir().unwrap();
    assert!(git(room.path(), &["init", "-q"]));
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(room.path()), dir.path()).unwrap();
    std::fs::create_dir_all(&runtime.root).unwrap();
    std::fs::write(&runtime.lanes_dir, b"blocker").unwrap();

    assert_eq!(
        project_group_roots(room.path(), RootClass::Repo, &runtime, None),
        vec![room.path().to_path_buf()]
    );
    assert!(!runtime.group_roots_path().exists());
    assert_eq!(std::fs::read(&runtime.lanes_dir).unwrap(), b"blocker");
}

#[test]
fn group_roots_dispatch_follows_the_root_class() {
    // Directory and non-git marker rooms do not scan children. A marker room
    // with `.git` at the root keeps repo semantics.
    let room = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(room.path().join("app/.git")).unwrap();
    std::fs::write(room.path().join("Cargo.toml"), "[workspace]").unwrap();

    assert!(list_group_roots(room.path(), RootClass::Directory).is_empty());
    assert!(list_group_roots(room.path(), RootClass::Marker).is_empty());
}
