//! Integration coverage for `rimz worktree`.

#[cfg(unix)]
use std::io::Read;
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
    assert_eq!(parsed[0]["landed"], true);

    env.rimz()
        .args(["worktree", "remove", "demo"])
        .assert()
        .success()
        .stdout(contains("removed demo"));
    assert!(!path.exists(), "worktree removed");
}

#[test]
fn worktree_new_seeds_files_from_worktreeinclude() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);

    // Untracked files that `git worktree add` would not carry over.
    std::fs::write(env.project_root.join(".env"), "SECRET=1").expect("write .env");
    std::fs::create_dir_all(env.project_root.join("config")).expect("config dir");
    std::fs::write(env.project_root.join("config/local.toml"), "a = 1").expect("write local");
    std::fs::write(
        env.project_root.join(".worktreeinclude"),
        ".env\nconfig/*.toml\n",
    )
    .expect("write include");

    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success()
        .stdout(contains("seeded : 2 file(s) from .worktreeinclude"));

    let path = env.home_root.join("project-worktrees").join("demo");
    assert_eq!(
        std::fs::read_to_string(path.join(".env")).expect("seeded .env"),
        "SECRET=1"
    );
    assert!(
        path.join("config/local.toml").is_file(),
        "seeded glob match"
    );
}

#[test]
fn worktree_new_without_include_seeds_nothing() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);

    // A pattern that matches nothing still creates the worktree; no seed report.
    std::fs::write(env.project_root.join(".worktreeinclude"), "missing.txt\n")
        .expect("write include");

    let out = env
        .rimz()
        .args(["worktree", "new", "demo"])
        .output()
        .expect("spawn new");
    assert!(out.status.success(), "worktree still created");
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("seeded"),
        "no files seeded"
    );
    assert!(
        !env.home_root
            .join("project-worktrees")
            .join("demo")
            .join("missing.txt")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn worktree_new_symlinks_dirs_from_worktreelink_without_dirtying_checkout() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    std::fs::create_dir_all(env.project_root.join("node_modules/pkg")).expect("node_modules");
    std::fs::write(
        env.project_root.join("node_modules/pkg/index.js"),
        "module.exports = 1\n",
    )
    .expect("write module");
    std::fs::write(env.project_root.join(".worktreelink"), "node_modules\n")
        .expect("write worktreelink");

    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success()
        .stdout(contains("linked : 1 dir(s) from .worktreelink"));

    let path = env.home_root.join("project-worktrees").join("demo");
    let linked = path.join("node_modules");
    assert!(
        std::fs::symlink_metadata(&linked)
            .expect("link metadata")
            .is_symlink(),
        "linked dir is a symlink"
    );
    assert_eq!(
        linked.canonicalize().expect("linked canonical"),
        env.project_root
            .join("node_modules")
            .canonicalize()
            .expect("source canonical")
    );
    assert_eq!(
        git_stdout(&path, &["status", "--porcelain"]),
        "",
        ".worktreelink symlink is registered in git info/exclude"
    );
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");
    assert!(
        rimz::worktree::status(&path, &marker)
            .expect("status")
            .safe_to_remove(),
        "the linked dir does not block cleanup"
    );
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
fn worktree_new_with_at_base_keeps_pending_commits() {
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
            .landed,
        rimz::worktree::LandedVerdict::Pending,
        "the clean commit is still pending on main"
    );

    env.rimz()
        .args(["worktree", "remove", "demo"])
        .assert()
        .failure()
        .stderr(contains("--force"));

    assert!(path.exists(), "pending @-based worktree is kept");
    assert!(
        branch_exists(&env.project_root, "demo"),
        "pending @-based branch is kept"
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

    wait_for_ready(
        &mut child,
        &env.home_root.join("clean.ready"),
        &env.home_root.join("clean.pid"),
    );
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
fn agents_exec_sighup_keeps_dirty_and_pending_worktrees() {
    assert_sighup_keeps_worktree("dirty", |_, path| {
        std::fs::write(path.join("dirty.txt"), "dirty\n").expect("dirty file");
    });
    assert_sighup_keeps_worktree("ahead", |_, path| {
        let marker = rimz::worktree::read_marker_for_worktree(path)
            .expect("read marker")
            .expect("marker");
        commit_file(path, "feature.txt", "feature\n", "feature");
        assert_eq!(
            rimz::worktree::status(path, &marker)
                .expect("status")
                .landed,
            rimz::worktree::LandedVerdict::Pending,
            "clean local commit is pending until it lands on the base"
        );
    });
}

#[cfg(unix)]
fn assert_sighup_keeps_worktree(label: &str, setup: impl FnOnce(&Env, &Path)) {
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
    setup(&env, &path);
    let mut child = spawn_agent_exec(&env, &path, label);

    wait_for_ready(
        &mut child,
        &env.home_root.join(format!("{label}.ready")),
        &env.home_root.join(format!("{label}.pid")),
    );
    signal_child(&child, nix::sys::signal::Signal::SIGHUP);
    let _status = wait_for_exit(&mut child, &env.home_root.join(format!("{label}.pid")));

    assert!(path.exists(), "{label} worktree is kept after SIGHUP");
    assert!(
        branch_exists(&env.project_root, "demo"),
        "{label} worktree branch is kept"
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
            .landed,
        rimz::worktree::LandedVerdict::Landed,
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
fn gc_sweeps_merge_landed_worktree() {
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
    git(
        &env.project_root,
        &["merge", "--no-ff", "demo", "-m", "merge demo"],
    );
    commit_file(&env.project_root, "trunk.txt", "trunk\n", "trunk");
    git(&path, &["merge", "--no-ff", "main", "-m", "merge main"]);
    commit_file(&env.project_root, "after.txt", "after\n", "after merge");
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");
    assert_ne!(
        git_stdout(&env.project_root, &["rev-parse", "main^{tree}"]),
        git_stdout(&path, &["rev-parse", "HEAD^{tree}"]),
        "main advanced after the merge-back, so the fixture reaches the merge-tree scan"
    );
    assert_eq!(
        rimz::worktree::status(&path, &marker)
            .expect("status")
            .landed,
        rimz::worktree::LandedVerdict::Landed,
        "leftover merge commits are landed when their tree already exists on main"
    );
    assert_ne!(
        git_stdout(&path, &["rev-list", "--count", "main..HEAD"]),
        "0",
        "the fixture remains ahead by ancestry"
    );

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("worktrees swept: 1"));

    assert!(!path.exists(), "gc swept merge-landed worktree");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "gc force-deleted content-landed branch"
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
            .landed,
        rimz::worktree::LandedVerdict::Landed,
        "feature is landed on explicit base even though main lacks it"
    );

    let mut child = spawn_agent_exec(&env, &path, "explicit-base");
    wait_for_ready(
        &mut child,
        &env.home_root.join("explicit-base.ready"),
        &env.home_root.join("explicit-base.pid"),
    );
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
        .env("SHELL", "/definitely/not/a/shell")
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
         exec >/dev/null 2>/dev/null\n\
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
fn wait_for_ready(child: &mut Child, path: &Path, agent_pid_file: &Path) {
    let start = Instant::now();
    let timeout = ready_timeout();
    while start.elapsed() < timeout {
        if path.exists() {
            return;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                panic!(
                    "agents exec exited with {status} before writing {}\n{}",
                    path.display(),
                    child_output(child)
                );
            }
            Ok(None) => {}
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => panic!("wait failed before {} was ready: {err}", path.display()),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    signal_child(child, nix::sys::signal::Signal::SIGKILL);
    kill_agent_pid(agent_pid_file);
    let _ = child.wait();
    panic!(
        "timed out after {timeout:?} waiting for {}\n{}",
        path.display(),
        child_output(child)
    );
}

#[cfg(unix)]
fn ready_timeout() -> Duration {
    if std::env::var_os("LLVM_PROFILE_FILE").is_some()
        || std::env::var_os("CARGO_LLVM_COV").is_some()
    {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(5)
    }
}

#[cfg(unix)]
fn child_output(child: &mut Child) -> String {
    let mut stdout = String::new();
    if let Some(pipe) = child.stdout.as_mut() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    let mut stderr = String::new();
    if let Some(pipe) = child.stderr.as_mut() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    format!("stdout:\n{stdout}\nstderr:\n{stderr}")
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
    kill_agent_pid(agent_pid_file);
    let _ = child.wait();
    panic!("timed out waiting for agents exec to exit");
}

#[cfg(unix)]
fn kill_agent_pid(agent_pid_file: &Path) {
    if let Ok(raw) = std::fs::read_to_string(agent_pid_file)
        && let Ok(pid) = raw.trim().parse::<i32>()
    {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

#[cfg(unix)]
fn branch_exists(repo: &Path, branch: &str) -> bool {
    !git_stdout(repo, &["branch", "--list", branch])
        .trim()
        .is_empty()
}
