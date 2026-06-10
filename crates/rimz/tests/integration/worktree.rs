//! Integration coverage for `rimz worktree`.

use std::path::Path;
use std::process::Command;
#[cfg(unix)]
use std::process::{Child, ExitStatus, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use serde_json::Value;
use serde_json::json;

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
fn worktree_new_errors_when_name_exists() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .failure()
        .stderr(contains("already exists"));
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
fn agents_exec_sighup_removes_clean_worktree_when_agent_exits_on_hup() {
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
    let mut child = spawn_agent_exec_with_signals(&env, &path, "clean-fast", AgentSignals::Default);
    let agent_pid_file = env.home_root.join("clean-fast.pid");

    wait_for_file(&env.home_root.join("clean-fast.ready"));
    signal_child(&child, nix::sys::signal::Signal::SIGHUP);
    signal_pid(read_pid(&agent_pid_file), nix::sys::signal::Signal::SIGHUP);
    let _status = wait_for_exit(&mut child, &agent_pid_file);

    assert!(
        !path.exists(),
        "clean worktree removed after prompt HUP exit"
    );
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "worktree branch deleted"
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

#[cfg(unix)]
#[test]
fn agents_exec_sighup_removes_fast_forward_merged_worktree() {
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

    let mut child = spawn_agent_exec(&env, &path, "ff-merged");
    wait_for_file(&env.home_root.join("ff-merged.ready"));
    signal_child(&child, nix::sys::signal::Signal::SIGHUP);
    let _status = wait_for_exit(&mut child, &env.home_root.join("ff-merged.pid"));

    assert!(!path.exists(), "merged worktree removed after SIGHUP");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "merged worktree branch deleted"
    );
}

#[cfg(unix)]
#[test]
fn agents_exec_sighup_removes_merge_committed_worktree() {
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
    commit_file(&env.project_root, "main.txt", "main\n", "main");
    git(
        &env.project_root,
        &["merge", "--no-ff", "-m", "merge demo", "demo"],
    );

    let mut child = spawn_agent_exec(&env, &path, "merge-committed");
    wait_for_file(&env.home_root.join("merge-committed.ready"));
    signal_child(&child, nix::sys::signal::Signal::SIGHUP);
    let _status = wait_for_exit(&mut child, &env.home_root.join("merge-committed.pid"));

    assert!(!path.exists(), "merge-committed worktree removed");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "merge-committed branch deleted"
    );
}

#[cfg(unix)]
#[test]
fn agents_exec_sighup_removes_squash_merged_worktree() {
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
    commit_file(&path, "feature-a.txt", "a\n", "feature a");
    commit_file(&path, "feature-b.txt", "b\n", "feature b");
    git(&env.project_root, &["merge", "--squash", "demo"]);
    git(&env.project_root, &["commit", "-m", "squash demo"]);

    let mut child = spawn_agent_exec(&env, &path, "squash-merged");
    wait_for_file(&env.home_root.join("squash-merged.ready"));
    signal_child(&child, nix::sys::signal::Signal::SIGHUP);
    let _status = wait_for_exit(&mut child, &env.home_root.join("squash-merged.pid"));

    assert!(!path.exists(), "squash-merged worktree removed");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "squash-merged branch deleted after proof"
    );
}

#[cfg(unix)]
#[test]
fn agents_exec_sighup_removes_cherry_picked_worktree() {
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
    commit_file(&path, "feature-a.txt", "a\n", "feature a");
    commit_file(&path, "feature-b.txt", "b\n", "feature b");
    let commits = git_stdout(&env.project_root, &["rev-list", "--reverse", "main..demo"]);
    for commit in commits.lines() {
        git(&env.project_root, &["cherry-pick", commit]);
    }
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");
    assert_eq!(
        rimz::worktree::status(&path, &marker)
            .expect("status")
            .commits_unmerged,
        Some(0),
        "patch-equivalent cherry-picked commits count as landed"
    );

    let mut child = spawn_agent_exec(&env, &path, "cherry-picked");
    wait_for_file(&env.home_root.join("cherry-picked.ready"));
    signal_child(&child, nix::sys::signal::Signal::SIGHUP);
    let _status = wait_for_exit(&mut child, &env.home_root.join("cherry-picked.pid"));

    assert!(!path.exists(), "cherry-picked worktree removed");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "cherry-picked branch deleted after proof"
    );
}

#[test]
fn worktree_remove_merged_succeeds_without_force() {
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
        .args(["worktree", "remove", "demo"])
        .assert()
        .success()
        .stdout(contains("removed demo"));

    assert!(!path.exists(), "merged worktree removed");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "merged branch deleted"
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

#[test]
fn legacy_marker_self_heals_via_gc_trunk_ladder() {
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
    rewrite_marker_as_v2(&path);
    commit_file(&path, "feature.txt", "feature\n", "feature");
    git(&env.project_root, &["merge", "--ff-only", "demo"]);

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("worktrees swept: 1"));

    assert!(
        !path.exists(),
        "legacy marker was swept through main ladder"
    );
}

#[test]
fn legacy_marker_snapshot_fallback_when_no_trunk() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo_on_branch(&env.project_root, "trunk");
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    rewrite_marker_as_v2(&path);
    commit_file(&path, "feature.txt", "feature\n", "feature");

    env.rimz()
        .args(["worktree", "remove", "demo"])
        .assert()
        .failure()
        .stderr(contains("--force"));

    assert!(path.exists(), "snapshot fallback keeps unmerged worktree");
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

#[cfg(unix)]
#[test]
fn agents_exec_sighup_shared_clean_worktree_removes_once() {
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
    let mut first = spawn_agent_exec(&env, &path, "shared-a");
    let mut second = spawn_agent_exec(&env, &path, "shared-b");

    wait_for_file(&env.home_root.join("shared-a.ready"));
    wait_for_file(&env.home_root.join("shared-b.ready"));
    signal_child(&first, nix::sys::signal::Signal::SIGHUP);
    signal_child(&second, nix::sys::signal::Signal::SIGHUP);
    let _first_status = wait_for_exit(&mut first, &env.home_root.join("shared-a.pid"));
    let _second_status = wait_for_exit(&mut second, &env.home_root.join("shared-b.pid"));

    assert!(!path.exists(), "shared clean worktree removed after SIGHUP");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "shared worktree branch deleted once"
    );
}

fn git_missing() -> bool {
    Command::new("git").arg("--version").output().is_err()
}

fn init_repo(path: &Path) {
    init_repo_on_branch(path, "main");
}

fn init_repo_on_branch(path: &Path, branch: &str) {
    git(path, &["init", "-b", branch]);
    git(path, &["config", "user.email", "rimz@example.com"]);
    git(path, &["config", "user.name", "Rimz Test"]);
    commit_file(path, "README.md", "fixture\n", "initial");
}

fn commit_file(repo: &Path, name: &str, contents: &str, message: &str) {
    std::fs::write(repo.join(name), contents).expect("write committed file");
    git(repo, &["add", name]);
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

fn rewrite_marker_as_v2(path: &Path) {
    let marker_path = rimz::worktree::marker_path(path).expect("marker path");
    let marker = rimz::worktree::read_marker_for_worktree(path)
        .expect("read marker")
        .expect("marker");
    let mut value = serde_json::to_value(marker).expect("marker json");
    let object = value.as_object_mut().expect("marker object");
    object.insert("version".to_owned(), json!(2));
    object.remove("base_branch");
    std::fs::write(
        &marker_path,
        serde_json::to_vec_pretty(&value).expect("serialize marker"),
    )
    .expect("rewrite marker");
}

#[cfg(unix)]
fn spawn_agent_exec(env: &Env, worktree: &Path, label: &str) -> Child {
    spawn_agent_exec_with_signals(env, worktree, label, AgentSignals::Trap)
}

#[cfg(unix)]
fn spawn_agent_exec_with_signals(
    env: &Env,
    worktree: &Path,
    label: &str,
    signals: AgentSignals,
) -> Child {
    let shim_dir = write_codex_shim(env);
    let ready = env.home_root.join(format!("{label}.ready"));
    let pid_file = env.home_root.join(format!("{label}.pid"));
    let mut cmd = env.rimz();
    cmd.args(["agents", "exec", "codex", "--worktree-path"])
        .arg(worktree)
        .current_dir(worktree)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_READY", &ready)
        .env("RIMZ_TEST_AGENT_PID", &pid_file)
        .env(
            "RIMZ_TEST_AGENT_TRAP_SIGNALS",
            match signals {
                AgentSignals::Trap => "1",
                AgentSignals::Default => "0",
            },
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn().expect("spawn agents exec")
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum AgentSignals {
    Trap,
    Default,
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
fn read_pid(path: &Path) -> i32 {
    std::fs::read_to_string(path)
        .expect("read pid")
        .trim()
        .parse()
        .expect("parse pid")
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
