use std::path::{Path, PathBuf};
use std::process::Command;

use crate::RuntimePaths;
use crate::ids::WorkspaceId;
use crate::sidebar::refresh::git_stats::{
    DiffStatsCache, WorktreeRootsCache, read_diff_stats_cache,
};
use crate::store::atomic;
use crate::workspace::RootClass;

use super::roots::{list_group_roots, list_worktree_roots, project_group_roots};

fn git(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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
    let runtime_dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(WorkspaceId::from_project_root(&main), runtime_dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    let roots = project_group_roots(&main, RootClass::Repo, &runtime, None);
    let cache = read_diff_stats_cache(&runtime.diff_stats_path());
    let classifications = cache
        .worktrees
        .expect("roots cache")
        .marker_names
        .expect("current classifications");

    assert!(roots.contains(&main));
    assert!(roots.contains(&external));
    assert_eq!(
        classifications.get(&external).map(String::as_str),
        Some("feature")
    );
    assert!(!classifications.contains_key(&main));
}

#[test]
fn project_group_roots_refreshes_a_legacy_unclassified_cache() {
    let room = tempfile::tempdir().unwrap();
    let runtime_dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        WorkspaceId::from_project_root(room.path()),
        runtime_dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let old_root = room.path().join("old-root");
    atomic::write_temp_then_rename_cache(
        &runtime.diff_stats_path(),
        &DiffStatsCache {
            worktrees: Some(WorktreeRootsCache {
                refreshed_at_ms: crate::sidebar::timing::unix_now_ms(),
                roots: vec![old_root],
                marker_names: None,
            }),
            ..DiffStatsCache::default()
        },
    )
    .unwrap();

    assert!(project_group_roots(room.path(), RootClass::Directory, &runtime, None).is_empty());
    let refreshed = read_diff_stats_cache(&runtime.diff_stats_path())
        .worktrees
        .expect("refreshed roots");
    assert!(refreshed.roots.is_empty());
    assert_eq!(refreshed.marker_names, Some(Default::default()));
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
