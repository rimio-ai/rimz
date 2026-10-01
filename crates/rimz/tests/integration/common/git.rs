//! Shared Git repositories and published PR refs for CLI tests.

use super::Env;
use std::path::Path;
use std::process::Command;

/// `true`, with the self-skip recorded, when no `git` runs; every caller
/// returns early on it.
pub(crate) fn git_missing() -> bool {
    let missing = Command::new("git").arg("--version").output().is_err();
    if missing {
        super::skip("git not on PATH");
    }
    missing
}

pub(crate) fn init_repo(path: &Path) {
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.email", "rimz@example.com"]);
    git(path, &["config", "user.name", "RimZ Test"]);
    commit_file(path, "README.md", "fixture\n", "initial");
}

pub(crate) fn publish_pr_ref(env: &Env, remote_ref: &str) -> (String, String) {
    publish_pr_ref_inner(env, remote_ref, true)
}

pub(crate) fn publish_pr_ref_without_branch(env: &Env, remote_ref: &str) -> (String, String) {
    publish_pr_ref_inner(env, remote_ref, false)
}

fn publish_pr_ref_inner(env: &Env, remote_ref: &str, publish_branch: bool) -> (String, String) {
    init_repo(&env.project_root);
    let remote = env.home_root.join("origin.git");
    let remote_arg = remote.to_str().expect("utf8 remote path");
    git(&env.project_root, &["init", "--bare", remote_arg]);
    git(&env.project_root, &["remote", "add", "origin", remote_arg]);
    git(&env.project_root, &["push", "-u", "origin", "main"]);
    let trunk = git_stdout(&env.project_root, &["rev-parse", "main"]);

    git(&env.project_root, &["checkout", "-b", "feature"]);
    commit_file(&env.project_root, "feature.txt", "feature\n", "feature");
    let pr_head = git_stdout(&env.project_root, &["rev-parse", "HEAD"]);
    let refspec = format!("{pr_head}:{remote_ref}");
    git(&env.project_root, &["push", "origin", refspec.as_str()]);
    if publish_branch {
        git(&env.project_root, &["push", "origin", "feature"]);
    }
    git(&env.project_root, &["checkout", "main"]);
    git(&env.project_root, &["branch", "-D", "feature"]);

    (pr_head, trunk)
}

#[cfg(unix)]
pub(crate) fn configure_github_origin_rewrite(env: &Env) {
    configure_origin_rewrite(env, "https://github.com/org/repo.git");
    let remote = env.home_root.join("origin.git");
    let remote = remote.to_str().expect("utf8 remote path");
    let key = format!("url.{remote}.insteadOf");
    git(
        &env.project_root,
        &[
            "config",
            "--add",
            key.as_str(),
            "https://github.com/alice/fork.git",
        ],
    );
}

#[cfg(unix)]
pub(crate) fn configure_gitea_origin_rewrite(env: &Env) {
    configure_origin_rewrite(env, "https://gitea.example.test/org/repo.git");
    let remote = env.home_root.join("origin.git");
    let remote = remote.to_str().expect("utf8 remote path");
    let key = format!("url.{remote}.insteadOf");
    git(
        &env.project_root,
        &[
            "config",
            "--add",
            key.as_str(),
            "https://gitea.example.test/alice/fork.git",
        ],
    );
}

pub(crate) fn configure_origin_rewrite(env: &Env, origin_url: &str) {
    let remote = env.home_root.join("origin.git");
    let remote = remote.to_str().expect("utf8 remote path");
    git(
        &env.project_root,
        &["remote", "set-url", "origin", origin_url],
    );
    let key = format!("url.{remote}.insteadOf");
    git(
        &env.project_root,
        &["config", "--add", key.as_str(), origin_url],
    );
}

pub(crate) fn commit_file(repo: &Path, name: &str, contents: &str, message: &str) {
    std::fs::write(repo.join(name), contents).expect("write committed file");
    git(repo, &["add", name]);
    git(repo, &["commit", "-m", message]);
}

pub(crate) fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

pub(crate) fn git_succeeds(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git")
        .status
        .success()
}

pub(crate) fn git_stdout(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
