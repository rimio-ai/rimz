//! Integration coverage for `rimz worktree`.

use std::path::Path;
#[cfg(unix)]
use std::process::{Child, ExitStatus};
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use serde_json::Value;

use crate::common::Env;

#[test]
fn worktree_new_list_and_remove_round_trip() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);

    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success()
        .stdout(contains("created demo"));

    let path = env.home_root.join("project-worktrees").join("demo");
    assert!(path.is_dir(), "worktree path exists");
    assert_eq!(
        git_stdout(&path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "demo"
    );
    assert!(
        rimz::worktree::marker_path(&path)
            .expect("marker path")
            .is_file(),
        "marker lives in git admin dir"
    );
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");
    assert_eq!(
        marker.base_ref,
        git_stdout(&env.project_root, &["rev-parse", "HEAD"]),
        "marker stores the base commit snapshot"
    );
    assert_eq!(marker.base_branch.as_deref(), Some("main"));

    let out = env
        .rimz()
        .args(["worktree", "list", "--json"])
        .output()
        .expect("spawn list");
    assert!(out.status.success(), "list succeeds");
    let parsed: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(parsed.as_array().expect("array").len(), 1);
    assert_eq!(parsed[0]["name"], "demo");
    assert_eq!(parsed[0]["commits_unmerged"], 0);

    env.rimz()
        .args(["worktree", "remove", "demo"])
        .assert()
        .success()
        .stdout(contains("removed demo"));
    assert!(!path.exists(), "worktree removed");
}

#[test]
fn worktree_remove_refuses_dirty_without_force() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    std::fs::write(path.join("dirty.txt"), "dirty\n").expect("dirty file");

    env.rimz()
        .args(["worktree", "remove", "demo"])
        .assert()
        .failure()
        .stderr(contains("--force"));

    env.rimz()
        .args(["worktree", "remove", "demo", "--force"])
        .assert()
        .success();
    assert!(!path.exists(), "force removes dirty worktree");
}

#[test]
fn worktree_new_with_at_base_keeps_unmerged_commits() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo", "--base", "@"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");
    assert_eq!(
        marker.base_branch.as_deref(),
        Some("main"),
        "`@` is captured as the creation-time branch, not the linked worktree HEAD"
    );

    commit_file(&path, "feature.txt", "feature\n", "feature");
    assert_eq!(
        rimz::worktree::status(&path, &marker)
            .expect("status")
            .commits_unmerged,
        Some(1),
        "the clean commit is still unmerged into main"
    );

    env.rimz()
        .args(["worktree", "remove", "demo"])
        .assert()
        .failure()
        .stderr(contains("--force"));

    assert!(path.exists(), "unmerged @-based worktree is kept");
    assert!(
        branch_exists(&env.project_root, "demo"),
        "unmerged @-based branch is kept"
    );
}

#[cfg(unix)]
#[test]
fn agents_exec_sighup_removes_clean_worktree() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    let mut child = spawn_agent_exec(&env, &path, "clean");

    wait_for_file(&env.home_root.join("clean.ready"));
    signal_child(&child, nix::sys::signal::Signal::SIGHUP);
    let _status = wait_for_exit(&mut child, &env.home_root.join("clean.pid"));

    assert!(!path.exists(), "clean worktree removed after SIGHUP");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "worktree branch deleted"
    );
    assert!(
        !git_stdout(&env.project_root, &["worktree", "list", "--porcelain"])
            .contains(&path.display().to_string()),
        "git worktree list forgets the removed worktree"
    );
}

#[cfg(unix)]
#[test]
fn agents_exec_sighup_keeps_dirty_worktree() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    std::fs::write(path.join("dirty.txt"), "dirty\n").expect("dirty file");
    let mut child = spawn_agent_exec(&env, &path, "dirty");

    wait_for_file(&env.home_root.join("dirty.ready"));
    signal_child(&child, nix::sys::signal::Signal::SIGHUP);
    let _status = wait_for_exit(&mut child, &env.home_root.join("dirty.pid"));

    assert!(path.exists(), "dirty worktree is kept after SIGHUP");
    assert!(
        branch_exists(&env.project_root, "demo"),
        "dirty worktree branch is kept"
    );
}

#[cfg(unix)]
#[test]
fn agents_exec_sighup_keeps_unmerged_clean_worktree() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");
    commit_file(&path, "feature.txt", "feature\n", "feature");
    assert_eq!(
        rimz::worktree::status(&path, &marker)
            .expect("status")
            .commits_unmerged,
        Some(1),
        "clean local commit is unmerged until it lands on the base"
    );
    let mut child = spawn_agent_exec(&env, &path, "ahead");

    wait_for_file(&env.home_root.join("ahead.ready"));
    signal_child(&child, nix::sys::signal::Signal::SIGHUP);
    let _status = wait_for_exit(&mut child, &env.home_root.join("ahead.pid"));

    assert!(path.exists(), "unmerged worktree is kept after SIGHUP");
    assert!(
        branch_exists(&env.project_root, "demo"),
        "unmerged worktree branch is kept"
    );
}

#[test]
fn worktree_remove_split_landed_succeeds_without_force() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    commit_two_files(
        &path,
        "feature a",
        "feature-a.txt",
        "a\n",
        "feature-b.txt",
        "b\n",
    );
    commit_file(&env.project_root, "feature-a.txt", "a\n", "feature a");
    commit_file(&env.project_root, "feature-b.txt", "b\n", "feature b");
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");
    assert_eq!(
        rimz::worktree::status(&path, &marker)
            .expect("status")
            .commits_unmerged,
        Some(0),
        "identical trees count as landed even when one branch commit landed as multiple base commits"
    );

    env.rimz()
        .args(["worktree", "remove", "demo"])
        .assert()
        .success()
        .stdout(contains("removed demo"));

    assert!(!path.exists(), "split-landed worktree removed");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "split-landed branch deleted after proof"
    );
}

#[test]
fn worktree_cleanup_command_removes_clean_worktree() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");

    env.rimz()
        .args(["worktree", "cleanup"])
        .arg(&path)
        .arg("--non-interactive")
        .current_dir(&env.home_root)
        .assert()
        .success()
        .stderr(contains("removed clean worktree"));

    assert!(!path.exists(), "clean worktree removed by hidden cleanup");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "cleanup deleted clean branch"
    );
}

#[test]
fn gc_sweeps_merged_worktree() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    commit_file(&path, "feature.txt", "feature\n", "feature");
    git(&env.project_root, &["merge", "--ff-only", "demo"]);

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("worktrees swept: 1"));

    assert!(!path.exists(), "gc swept merged worktree");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "gc deleted merged branch"
    );
}

#[cfg(unix)]
#[test]
fn auto_remove_force_deletes_branch_merged_into_explicit_base() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    git(&env.project_root, &["branch", "develop"]);
    env.rimz()
        .args(["worktree", "new", "demo", "--base", "develop"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    commit_file(&path, "feature.txt", "feature\n", "feature");
    git(&env.project_root, &["fetch", ".", "demo:develop"]);
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");
    assert_eq!(marker.base_branch.as_deref(), Some("develop"));
    assert_eq!(
        rimz::worktree::status(&path, &marker)
            .expect("status")
            .commits_unmerged,
        Some(0),
        "feature is landed on explicit base even though main lacks it"
    );

    let mut child = spawn_agent_exec(&env, &path, "explicit-base");
    wait_for_file(&env.home_root.join("explicit-base.ready"));
    signal_child(&child, nix::sys::signal::Signal::SIGHUP);
    let _status = wait_for_exit(&mut child, &env.home_root.join("explicit-base.pid"));

    assert!(!path.exists(), "explicit-base worktree removed");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "branch deleted after proving it landed on develop"
    );
    assert!(
        branch_exists(&env.project_root, "develop"),
        "base branch remains"
    );
}

fn git_missing() -> bool {
    Command::new("git").arg("--version").output().is_err()
}

fn init_repo(path: &Path) {
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.email", "rimz@example.com"]);
    git(path, &["config", "user.name", "Rimz Test"]);
    commit_file(path, "README.md", "fixture\n", "initial");
}

fn commit_file(repo: &Path, name: &str, contents: &str, message: &str) {
    std::fs::write(repo.join(name), contents).expect("write committed file");
    git(repo, &["add", name]);
    git(repo, &["commit", "-m", message]);
}

fn commit_two_files(
    repo: &Path,
    message: &str,
    first_name: &str,
    first_contents: &str,
    second_name: &str,
    second_contents: &str,
) {
    std::fs::write(repo.join(first_name), first_contents).expect("write first committed file");
    std::fs::write(repo.join(second_name), second_contents).expect("write second committed file");
    git(repo, &["add", first_name, second_name]);
    git(repo, &["commit", "-m", message]);
}

fn git(cwd: &Path, args: &[&str]) {
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

fn git_stdout(cwd: &Path, args: &[&str]) -> String {
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

#[cfg(unix)]
fn spawn_agent_exec(env: &Env, worktree: &Path, label: &str) -> Child {
    spawn_agent_exec_command(env, env.rimz(), worktree, worktree, label)
}

#[cfg(unix)]
fn spawn_agent_exec_command(
    env: &Env,
    mut cmd: Command,
    worktree_arg: &Path,
    cwd: &Path,
    label: &str,
) -> Child {
    let shim_dir = write_codex_shim(env);
    let ready = env.home_root.join(format!("{label}.ready"));
    let pid_file = env.home_root.join(format!("{label}.pid"));
    cmd.args(["agents", "exec", "codex", "--worktree-path"])
        .arg(worktree_arg)
        .current_dir(cwd)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_READY", &ready)
        .env("RIMZ_TEST_AGENT_PID", &pid_file)
        .env("RIMZ_TEST_AGENT_TRAP_SIGNALS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn().expect("spawn agents exec")
}

#[cfg(unix)]
fn write_codex_shim(env: &Env) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let dir = env.home_root.join("agent-bin");
    std::fs::create_dir_all(&dir).expect("mkdir agent bin");
    let shim = dir.join("codex");
    std::fs::write(
        &shim,
        "#!/bin/sh\n\
         printf '%s\\n' \"$$\" > \"$RIMZ_TEST_AGENT_PID\"\n\
         : > \"$RIMZ_TEST_AGENT_READY\"\n\
         if [ \"$RIMZ_TEST_AGENT_TRAP_SIGNALS\" = 1 ]; then\n\
           trap ':' HUP TERM\n\
         fi\n\
         while :; do\n\
           sleep 1\n\
         done\n",
    )
    .expect("write codex shim");
    let mut perms = std::fs::metadata(&shim)
        .expect("shim metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms).expect("chmod codex shim");
    dir
}

#[cfg(unix)]
fn path_with_front(dir: &Path) -> std::ffi::OsString {
    let original = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&original));
    std::env::join_paths(paths).expect("join PATH")
}

#[cfg(unix)]
fn wait_for_file(path: &Path) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {}", path.display());
}

#[cfg(unix)]
fn signal_child(child: &Child, signal: nix::sys::signal::Signal) {
    signal_pid(child.id() as i32, signal);
}

#[cfg(unix)]
fn signal_pid(pid: i32, signal: nix::sys::signal::Signal) {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), signal).expect("signal pid");
}

#[cfg(unix)]
fn wait_for_exit(child: &mut Child, agent_pid_file: &Path) -> ExitStatus {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => panic!("wait failed: {err}"),
        }
    }
    signal_child(child, nix::sys::signal::Signal::SIGKILL);
    if let Ok(raw) = std::fs::read_to_string(agent_pid_file)
        && let Ok(pid) = raw.trim().parse::<i32>()
    {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let _ = child.wait();
    panic!("timed out waiting for agents exec to exit");
}

#[cfg(unix)]
fn branch_exists(repo: &Path, branch: &str) -> bool {
    !git_stdout(repo, &["branch", "--list", branch])
        .trim()
        .is_empty()
}
