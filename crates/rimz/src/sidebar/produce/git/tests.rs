use std::path::{Path, PathBuf};
use std::process::Command;

use crate::workspace::RootClass;

use super::roots::{list_group_roots, list_worktree_roots};

#[test]
fn list_worktree_roots_includes_a_checkout_outside_the_project() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    let git = |cwd: &Path, args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    };
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
fn group_roots_dispatch_follows_the_root_class() {
    // Directory and non-git marker rooms do not scan children. A marker room
    // with `.git` at the root keeps repo semantics.
    let room = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(room.path().join("app/.git")).unwrap();
    std::fs::write(room.path().join("Cargo.toml"), "[workspace]").unwrap();

    assert!(list_group_roots(room.path(), RootClass::Directory).is_empty());
    assert!(list_group_roots(room.path(), RootClass::Marker).is_empty());
}
