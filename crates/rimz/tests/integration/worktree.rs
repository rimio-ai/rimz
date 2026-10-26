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
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use rimz::EventEnvelope;
use rimz::agents::{AgentLifecycleObservation, LaunchParams, LifecycleSignal};
#[cfg(unix)]
use rimz::harness::launch::{ExecAction, ExecIdentity, ExecRequest, ProviderAccountState};
#[cfg(unix)]
use rimz::ids::AgentKind;
use rimz::ids::AgentSessionId;
#[cfg(unix)]
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState};
use rimz::store::message::{DeliveryGate, MessageRecord};
use serde_json::Value;

use crate::common::Env;
#[cfg(unix)]
use crate::common::exec_args;
use crate::common::git::{
    commit_file, configure_origin_rewrite, git, git_missing, git_stdout, git_succeeds, init_repo,
    publish_pr_ref, publish_pr_ref_without_branch,
};
#[cfg(unix)]
use crate::common::git::{configure_gitea_origin_rewrite, configure_github_origin_rewrite};
#[cfg(unix)]
use crate::common::{
    gh_fork_head, gh_same_repo_head, path_with_front, tea_fork_head, tea_same_repo_head,
    write_gh_pr_head_shim, write_tea_pr_head_shim,
};

#[cfg(unix)]
fn worktree_exec_request(worktree: &Path) -> ExecRequest {
    ExecRequest {
        isolation_default: None,
        kind: AgentKind::new_unchecked("codex"),
        action: ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: None,
        no_skill_tool: false,
        allowed_tools: None,
        provider_account: ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: Some(worktree.to_path_buf()),
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: false,
        follow_up_message: None,
        loop_reminder: None,
        headless: None,
        identity: ExecIdentity::default(),
    }
}

#[test]
fn stale_base_warning_is_stderr_only_and_uses_last_fetched_upstream() {
    let env = Env::new();
    let repo = &env.project_root;
    init_repo(repo);
    env.record(&env.project_root);
    let origin = env.home_root.join("origin.git");
    git(repo, &["clone", "--bare", ".", origin.to_str().unwrap()]);
    git(repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    for _ in 0..2 {
        git(repo, &["commit", "--allow-empty", "-m", "upstream"]);
    }
    git(repo, &["push", "-u", "origin", "main"]);
    env.rimz()
        .args(["worktree", "new", "current"])
        .assert()
        .success()
        .stderr(contains("behind").not());
    git(repo, &["reset", "--hard", "HEAD~2"]);
    env.rimz().args(["worktree", "new", "stale"]).assert().success()
        .stderr(contains("warning: base branch main is 2 commits behind origin/main as last fetched; fast-forward main first, or pass --base origin/main"))
        .stdout(contains("behind").not());
    git(repo, &["reset", "--hard", "origin/main~1"]);
    env.rimz()
        .args(["worktree", "new", "one-behind"])
        .assert()
        .success()
        .stderr(contains("1 commit behind origin/main"));
}

#[test]
fn unreadable_machine_config_refuses_worktree_creation() {
    let env = Env::new();
    init_repo(&env.project_root);
    std::fs::create_dir_all(env.rimz_home()).unwrap();
    let config = env.rimz_home().join("config.toml");
    std::fs::write(
        &config,
        "[agents.worktree.hooks]\ncreated = touch HOOKRAN\n",
    )
    .unwrap();
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .failure()
        .stderr(contains("cannot create a worktree"))
        .stderr(contains("TOML error"))
        .stderr(contains(config.display().to_string()))
        .stderr(contains("rimz: warning:").not());
    assert!(!env.home_root.join("project-worktrees/demo").exists());
    assert!(!git_stdout(&env.project_root, &["branch", "--list", "demo"]).contains("demo"));
    env.rimz()
        .args(["worktree", "list"])
        .assert()
        .success()
        .stderr(contains("warning:"))
        .stderr(contains("TOML error"))
        .stderr(contains(config.display().to_string()));
}

#[cfg(unix)]
#[test]
fn launch_creation_signals_once_and_reuse_is_silent() {
    use crate::common::{CommandTimeoutExt, write_failing_agent_shim, write_fake_login_shell};

    let env = Env::new();
    init_repo(&env.project_root);
    env.install_agent_hooks("claude");
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let shim = write_failing_agent_shim(&env, "claude", 7);
    std::fs::create_dir_all(env.rimz_home()).unwrap();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = 'host'\n",
    )
    .unwrap();
    for attempt in 0..2 {
        // Launch transport may be unavailable; creation precedes it.
        let mut command = env.rimz();
        command.args(["agents", "claude", "-p", "test signal", "-w", "demo"]);
        if attempt == 1 {
            command.arg("--fresh");
        }
        let output = command
            .env("SHELL", &shell)
            .env("PATH", crate::common::path_with_front(&shim))
            .bounded_output()
            .unwrap();
        assert!(
            env.home_root.join("project-worktrees/demo").exists(),
            "{output:?}"
        );
        let signals: Vec<Value> = env
            .read_events()
            .into_iter()
            .map(|event| serde_json::from_str::<Value>(event.params.get()).unwrap())
            .filter(|event| event["name"] == "worktree.created")
            .collect();
        assert_eq!(signals.len(), 1, "{output:?}");
        assert_eq!(signals[0]["payload"]["name"], "demo");
    }
}

#[test]
fn worktree_hooks_run_and_report_without_streaming_output() {
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    std::fs::create_dir_all(env.rimz_home()).unwrap();
    std::fs::write(env.rimz_home().join("config.toml"), "[agents.worktree.hooks]\ncreated = 'echo captured; pwd > hook-cwd; echo \"$RIMZ_WORKTREE_PATH\" >> hook-cwd'\nremoved = 'echo removed > hook-removed'\n").unwrap();
    env.write_config(
        &env.project_root,
        "[agents.worktree.hooks]\ncreated = 'exit 9'\nremoved = 'exit 9'\n",
    );
    env.rimz()
        .args(["worktree", "new", "demo"])
        .env("RIMZ_WORKTREE_PATH", "/inherited/wrong")
        .assert()
        .success()
        .stdout(contains("  hook   : worktree.created ran"))
        .stdout(contains("captured").not());
    let path = env.home_root.join("project-worktrees/demo");
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(path.join("hook-cwd")).unwrap(),
        format!("{}\n{}\n", path.display(), marker.worktree_path.display())
    );
    env.rimz()
        .args(["worktree", "remove", "demo", "--force"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(env.project_root.join("hook-removed")).unwrap(),
        "removed\n"
    );
    let signals: Vec<Value> = env
        .read_events()
        .into_iter()
        .filter(|event| event.method == "signal.emit")
        .map(|event| serde_json::from_str(event.params.get()).unwrap())
        .collect();
    assert_eq!(signals.len(), 2);
    for (event, name) in signals.iter().zip(["worktree.created", "worktree.removed"]) {
        assert_eq!(event["name"], name);
        assert_eq!(event["source"], "worktree");
        assert_eq!(event["payload"]["name"], "demo");
        assert_eq!(event["payload"]["branch"], "demo");
        assert_eq!(
            event["payload"]["path"],
            marker.worktree_path.to_str().unwrap()
        );
        assert_eq!(event["payload"]["repo"], marker.repo_root.to_str().unwrap());
    }
    assert!(signals[0]["payload"]["base"].is_string());
    assert_eq!(signals[1]["payload"]["branch_deleted"], true);
}

#[test]
fn failed_worktree_hook_reports_output_and_removes_tree_and_branch() {
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    std::fs::create_dir_all(env.rimz_home()).unwrap();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents.worktree.hooks]\ncreated = 'echo hook-out; echo hook-err >&2; exit 3'\n",
    )
    .unwrap();
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .failure()
        .stderr(contains("worktree.created"))
        .stderr(contains("hook-out"))
        .stderr(contains("hook-err"))
        .stderr(contains("tree removed"))
        .stderr(contains("agents.worktree.hooks.created"));
    assert!(!env.home_root.join("project-worktrees/demo").exists());
    assert!(!git_stdout(&env.project_root, &["branch", "--list", "demo"]).contains("demo"));
    assert!(
        env.read_events()
            .iter()
            .all(|event| event.method != "signal.emit")
    );
}

#[test]
fn worktree_reports_unknown_hook_keys() {
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    std::fs::create_dir_all(env.rimz_home()).unwrap();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents.worktree.hooks]\ncretaed = 'exit 3'\n",
    )
    .unwrap();
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success()
        .stderr(contains("agents.worktree.hooks.cretaed"));
}

#[test]
fn blank_worktree_hook_refuses_creation_before_side_effects() {
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    std::fs::create_dir_all(env.rimz_home()).unwrap();
    for event in ["created", "removed"] {
        std::fs::write(
            env.rimz_home().join("config.toml"),
            format!("[agents.worktree.hooks]\n{event} = '  '\n"),
        )
        .unwrap();
        env.rimz()
            .args(["worktree", "new", "demo"])
            .assert()
            .failure()
            .stderr(contains(format!("agents.worktree.hooks.{event}")));
        assert!(!env.home_root.join("project-worktrees/demo").exists());
        assert!(!git_stdout(&env.project_root, &["branch", "--list", "demo"]).contains("demo"));
    }
}

#[test]
fn worktree_new_list_and_remove_round_trip() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);

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
    assert_eq!(
        parsed[0],
        serde_json::json!({
            "name": "demo",
            "path": path,
            "branch": "demo",
            "base_ref": marker.base_ref,
            "base_branch": "main",
            "dirty": false,
            "landed": true,
            "reasons": [],
            "holders": {"agents": [], "panes": 0, "loop_tasks": [], "teams": []},
        })
    );

    commit_file(&path, "feature.txt", "feature\n", "feature");
    let out = env
        .rimz()
        .args(["worktree", "list", "--json"])
        .output()
        .expect("spawn pending list");
    assert!(out.status.success(), "pending list succeeds");
    let pending: Value = serde_json::from_slice(&out.stdout).expect("pending json");
    assert_eq!(pending[0]["landed"], false);
    git(&env.project_root, &["merge", "--ff-only", "demo"]);

    env.rimz()
        .args(["worktree", "remove", "demo"])
        .assert()
        .success()
        .stdout(contains("removed demo"));
    assert!(!path.exists(), "worktree removed");
}

#[test]
fn unmanaged_launch_checkout_preserves_ownership_and_local_work() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    let path = env.home_root.join("project-worktrees/feat-review");
    git_stdout(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-b",
            "feat/review",
            path.to_str().expect("path"),
        ],
    );
    std::fs::write(path.join("local.txt"), "keep me\n").expect("local work");
    let workspace = env.resolve_workspace(&env.project_root);
    let config = rimz::config::WorktreeConfig::default();
    assert!(matches!(
        rimz::worktree::resolve_launch_checkout(
            &workspace,
            &config,
            Some("feat/review"),
            None,
            None,
            None,
            rimz::worktree::LaunchBase::Configured,
        ),
        Err(rimz::worktree::WorktreeErr::Unmarked { .. })
    ));
    let checkout =
        rimz::worktree::resolve_unmanaged_launch_checkout(&workspace, &config, "feat/review")
            .expect("resolve user-owned checkout");
    assert_eq!(
        checkout.cwd.canonicalize().expect("launch cwd"),
        path.canonicalize().expect("canonical checkout")
    );
    assert_eq!(checkout.worktree_name.as_deref(), Some("feat-review"));
    assert_eq!(checkout.generated_name(), None);
    assert!(!checkout.owns_checkout_lifecycle());
    #[cfg(unix)]
    {
        let alias = env.home_root.join("linked-worktrees");
        std::os::unix::fs::symlink(env.home_root.join("project-worktrees"), &alias)
            .expect("symlink worktree directory");
        let config = rimz::config::WorktreeConfig {
            dir: alias.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let checkout =
            rimz::worktree::resolve_unmanaged_launch_checkout(&workspace, &config, "feat-review")
                .expect("resolve through configured symlink");
        assert_eq!(checkout.cwd, alias.join("feat-review"));
    }
    assert_eq!(
        std::fs::read_to_string(path.join("local.txt")).expect("local work"),
        "keep me\n"
    );
    assert!(
        rimz::worktree::read_marker_for_worktree(&path)
            .expect("marker")
            .is_none()
    );
    assert!(
        rimz::worktree::discover_owned(&env.project_root)
            .expect("managed trees")
            .is_empty()
    );
    env.rimz()
        .args(["worktree", "remove", "feat-review", "--force"])
        .assert()
        .failure()
        .stderr(contains("not a RimZ-managed worktree"));
    assert!(path.exists());
}

#[test]
fn unmanaged_launch_checkout_rejects_non_worktrees_and_other_repositories() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    let outside = env.home_root.join("project-worktrees/plain");
    std::fs::create_dir_all(&outside).expect("plain sibling directory");
    assert!(matches!(
        rimz::worktree::resolve_unmanaged_launch_checkout(
            &workspace,
            &rimz::config::WorktreeConfig::default(),
            "plain",
        ),
        Err(rimz::worktree::WorktreeErr::NotLinkedWorktree { .. })
    ));
    let config = rimz::config::WorktreeConfig {
        dir: "checkouts".to_owned(),
        ..Default::default()
    };
    let plain = env.project_root.join("checkouts/plain");
    std::fs::create_dir_all(&plain).expect("plain directory");
    let other = env.project_root.join("checkouts/other");
    std::fs::create_dir_all(&other).expect("other repository directory");
    init_repo(&other);
    let linked = env.project_root.join("checkouts/linked-other");
    git_stdout(
        &other,
        &[
            "worktree",
            "add",
            "-b",
            "linked-other",
            linked.to_str().expect("path"),
        ],
    );
    for name in ["plain", "other", "linked-other"] {
        assert!(
            matches!(
                rimz::worktree::resolve_unmanaged_launch_checkout(&workspace, &config, name),
                Err(rimz::worktree::WorktreeErr::NotLinkedWorktree { .. })
            ),
            "{name} must not be accepted as a worktree of this repository"
        );
    }
}

#[test]
fn worktree_remove_distinguishes_missing_from_unmarked() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);

    env.rimz()
        .args(["worktree", "remove", "ghost"])
        .assert()
        .failure()
        .stderr(contains("no worktree `ghost` in"))
        .stderr(contains("none exist"));

    let plain = env.home_root.join("project-worktrees/plain");
    std::fs::create_dir_all(&plain).expect("create unmarked directory");
    env.rimz()
        .args(["worktree", "remove", "plain"])
        .assert()
        .failure()
        .stderr(contains(
            "worktree `plain` is not a RimZ-managed worktree at",
        ));
}

#[test]
fn worktree_sweep_previews_then_removes_only_safe_checkouts() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "landed"])
        .assert()
        .success();
    env.rimz()
        .args(["worktree", "new", "unlanded"])
        .assert()
        .success();
    let landed = env.home_root.join("project-worktrees/landed");
    let pending = env.home_root.join("project-worktrees/unlanded");
    commit_file(&pending, "feature.txt", "unlanded\n", "unlanded work");

    env.rimz()
        .args(["worktree", "sweep", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("would remove 1"))
        .stdout(contains("kept, unlanded work (1)"))
        .stdout(contains(
            "nothing was removed; run `rimz worktree sweep` to remove the 1",
        ));
    assert!(landed.exists(), "dry run keeps landed checkout");

    env.rimz()
        .args(["worktree", "sweep"])
        .assert()
        .success()
        .stdout(contains("removed 1"))
        .stdout(contains("kept, unlanded work (1)"));
    assert!(!landed.exists(), "safe checkout is swept");
    assert!(pending.exists(), "unmerged checkout is retained");
    let removed: Vec<Value> = env
        .read_events()
        .into_iter()
        .map(|event| serde_json::from_str::<Value>(event.params.get()).unwrap())
        .filter(|event| event["name"] == "worktree.removed")
        .collect();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0]["payload"]["name"], "landed");
}

#[test]
fn loop_tasks_hold_sweep_listing_and_cleanup_but_not_explicit_removal() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "followup"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees/followup");
    std::fs::write(
        env.rimz_home().join("loop.toml"),
        format!(
            "[tasks.nightly]\nagent = 'claude'\nroot = '{}'\ndir = '{}'\nevery = '1h'\n",
            env.project_root.display(),
            path.display()
        ),
    )
    .unwrap();
    for args in [
        vec!["worktree", "sweep", "--json"],
        vec!["worktree", "sweep", "--dry-run", "--json"],
    ] {
        let output = env.rimz().args(args).assert().success();
        let report: Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
        assert_eq!(
            report["kept"][0]["holders"]["loop_tasks"],
            serde_json::json!(["nightly"])
        );
        assert_eq!(report["kept"][0]["reasons"], serde_json::json!(["in_use"]));
    }
    let output = env
        .rimz()
        .args(["worktree", "list", "--json"])
        .assert()
        .success();
    let rows: Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(
        rows[0]["holders"]["loop_tasks"],
        serde_json::json!(["nightly"])
    );
    env.rimz()
        .args(["worktree", "list"])
        .assert()
        .success()
        .stdout(contains("loop task nightly"));
    env.rimz()
        .args(["worktree", "sweep"])
        .assert()
        .success()
        .stdout(contains("loop task nightly"));
    env.rimz()
        .args(["worktree", "cleanup"])
        .arg(&path)
        .arg("--non-interactive")
        .assert()
        .success();
    assert!(path.exists());
    env.rimz()
        .args(["worktree", "merge", "followup", "--yes"])
        .assert()
        .success();
    env.rimz()
        .args(["worktree", "remove", "followup"])
        .assert()
        .success();
    assert!(!path.exists());
}

#[test]
fn team_boards_hold_sweep_listing_and_cleanup_but_not_explicit_removal() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    let path = team_worktree(&env, "followup", "Stage: Build (@coder)\n");
    for args in [
        vec!["worktree", "sweep", "--dry-run", "--json"],
        vec!["worktree", "sweep", "--json"],
    ] {
        let output = env.rimz().args(args).assert().success();
        let report: Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
        assert_eq!(report["removed"], serde_json::json!([]));
        assert_eq!(report["kept"][0]["reasons"], serde_json::json!(["in_use"]));
        assert_eq!(
            report["kept"][0]["holders"]["teams"],
            serde_json::json!(["forge"])
        );
    }
    let output = env
        .rimz()
        .args(["worktree", "list", "--json"])
        .assert()
        .success();
    let rows: Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(rows[0]["holders"]["teams"], serde_json::json!(["forge"]));
    assert_eq!(rows[0]["reasons"], serde_json::json!(["in_use"]));
    env.rimz()
        .args(["worktree", "list"])
        .assert()
        .success()
        .stdout(contains("team forge").and(contains("1 in use")));
    env.rimz()
        .args(["worktree", "sweep"])
        .assert()
        .success()
        .stdout(contains("kept, in use").and(contains("team forge")));
    let output = env
        .rimz()
        .args(["gc", "--dry-run", "--json"])
        .assert()
        .success();
    let report: Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert_eq!(
        report["worktrees"]["kept"][0]["holders"]["teams"],
        serde_json::json!(["forge"])
    );
    assert_eq!(
        report["worktrees"]["kept"][0]["reasons"],
        serde_json::json!(["in_use"])
    );
    env.rimz()
        .args(["worktree", "cleanup"])
        .arg(&path)
        .arg("--non-interactive")
        .assert()
        .success();
    assert!(path.exists());
    env.rimz()
        .args(["worktree", "merge", "followup", "--yes"])
        .assert()
        .success();
    env.rimz()
        .args(["worktree", "remove", "followup"])
        .assert()
        .success();
    assert!(!path.exists());
}

#[test]
fn team_boards_hold_cleanup_until_done() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    let path = team_worktree(&env, "followup", "Stage: Build (@coder)\n");
    env.rimz()
        .args(["worktree", "cleanup"])
        .arg(&path)
        .arg("--non-interactive")
        .assert()
        .success();
    assert!(path.exists(), "unfinished team keeps its checkout");
    assert!(git_succeeds(
        &env.project_root,
        &["show-ref", "--verify", "refs/heads/followup"]
    ));
    std::fs::write(path.join(".rimz/team/blackboard.md"), "Stage: Done\n").unwrap();
    env.rimz()
        .args(["worktree", "cleanup"])
        .arg(&path)
        .arg("--non-interactive")
        .assert()
        .success();
    assert!(!path.exists(), "Done releases the checkout");
    assert!(!git_succeeds(
        &env.project_root,
        &["show-ref", "--verify", "refs/heads/followup"]
    ));
}

#[test]
fn dangling_team_link_does_not_hold_cleanup() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    let path = team_worktree(&env, "followup", "Stage: Build\n");
    std::fs::remove_dir_all(env.home_root.join("team-run-followup")).unwrap();
    env.rimz()
        .args(["worktree", "cleanup"])
        .arg(&path)
        .arg("--non-interactive")
        .assert()
        .success();
    assert!(!path.exists());
}

fn team_worktree(env: &Env, name: &str, board: &str) -> std::path::PathBuf {
    env.rimz()
        .args(["worktree", "new", name])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join(name);
    let run = env.home_root.join(format!("team-run-{name}"));
    std::fs::create_dir(&run).unwrap();
    std::fs::write(run.join("blackboard.md"), board).unwrap();
    std::fs::write(run.join("run.json"), r#"{"team":"forge"}"#).unwrap();
    std::fs::create_dir_all(path.join(".rimz")).unwrap();
    std::os::unix::fs::symlink(&run, path.join(".rimz/team")).unwrap();
    rimz::worktree::exclude_team_scratch(&path, &["/.rimz/team".to_owned()]);
    assert!(git_stdout(&path, &["status", "--porcelain"]).is_empty());
    path
}

#[test]
fn team_board_listing_failure_refuses_automatic_sweep() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    let path = team_worktree(&env, "followup", "Stage: Build\n");
    let real_git = which::which("git").unwrap();
    let bin = env.home_root.join("failing-git");
    let log = env.home_root.join("list-calls");
    crate::common::write_path_shim(
        &bin,
        "git",
        r#"
case " $* " in
    *' worktree list '*)
        printf 'list\n' >> "$RIMZ_TEST_GIT_LOG"
        printf 'worktree listing unavailable\n' >&2
        exit 1
        ;;
esac
exec "$RIMZ_TEST_REAL_GIT" "$@"
"#,
    );
    for args in [
        vec!["worktree", "sweep"],
        vec!["worktree", "sweep", "--dry-run"],
        vec!["gc", "--json"],
        vec!["gc", "--dry-run", "--json"],
    ] {
        std::fs::write(&log, "").unwrap();
        let mut command = env.rimz();
        command
            .args(&args)
            .env("PATH", path_with_front(&bin))
            .env("RIMZ_TEST_REAL_GIT", &real_git)
            .env("RIMZ_TEST_GIT_LOG", &log);
        if args[0] == "worktree" {
            command.assert().code(1).stderr(contains(
                "reading protection facts before sweeping worktrees",
            ));
        } else {
            let output = command.assert().success();
            let report: Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
            assert_eq!(report["worktrees"]["skipped"], "list_failed");
        }
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "list\n",
            "{args:?}: one enumeration attempt"
        );
        assert!(path.exists());
    }
}

#[test]
fn loop_source_failure_refuses_sweep_and_keeps_wrapper_checkout() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "followup"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees/followup");
    let source = env.rimz_home().join("loop.toml");
    std::fs::write(&source, "broken toml").unwrap();
    for args in [
        vec!["worktree", "sweep"],
        vec!["worktree", "sweep", "--dry-run"],
    ] {
        env.rimz()
            .args(args)
            .assert()
            .code(1)
            .stderr(contains(source.display().to_string()));
        assert!(path.exists());
    }
    env.rimz()
        .args(["worktree", "cleanup"])
        .arg(&path)
        .arg("--non-interactive")
        .assert()
        .success();
    assert!(path.exists());
    env.rimz().args(["worktree", "list"]).assert().success();
}

#[test]
fn worktree_list_state_summary_and_bare_default() {
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    env.rimz()
        .args(["worktree", "list"])
        .assert()
        .success()
        .stdout(contains(
            "1 worktrees in ~/project-worktrees · 1 removable (rimz worktree sweep)",
        ))
        .stdout(contains("STATE"))
        .stdout(contains("HOLDERS"))
        .stdout(contains("landed"))
        .stdout(contains("MERGED").not());
    let path = env.home_root.join("project-worktrees/demo");
    commit_file(&path, "feature.txt", "feature\n", "feature");
    std::fs::write(path.join("dirty.txt"), "dirty\n").unwrap();
    env.rimz()
        .args(["worktree"])
        .assert()
        .success()
        .stdout(contains("1 with unlanded work"))
        .stdout(contains("unlanded · dirty"));
}

#[test]
fn worktree_sweep_json_reports_all_reasons_and_no_store() {
    let env = Env::new();
    init_repo(&env.project_root);
    let skipped = env
        .rimz()
        .args(["worktree", "sweep", "--dry-run", "--json"])
        .output()
        .unwrap();
    assert!(skipped.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&skipped.stdout).unwrap(),
        serde_json::json!({
            "dry_run": true, "skipped": "no_store", "removed": [], "kept": [], "failed": []
        })
    );
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees/demo");
    commit_file(&path, "feature.txt", "feature\n", "feature");
    let preview = env
        .rimz()
        .args(["worktree", "sweep", "--dry-run", "--json"])
        .output()
        .unwrap();
    assert!(preview.status.success());
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(preview["dry_run"], true);
    assert_eq!(
        preview["kept"][0]["reasons"],
        serde_json::json!(["unlanded"])
    );
    std::fs::write(path.join("dirty.txt"), "dirty\n").unwrap();
    let preview = env
        .rimz()
        .args(["worktree", "sweep", "--dry-run", "--json"])
        .output()
        .unwrap();
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(
        preview["kept"][0]["reasons"],
        serde_json::json!(["uncommitted_changes", "unlanded"])
    );
}

#[test]
fn worktree_remove_unlanded_names_count_target_and_restore_tip() {
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees/demo");
    commit_file(&path, "feature.txt", "feature\n", "feature");
    let tip = git_stdout(&path, &["rev-parse", "HEAD"]);
    env.rimz()
        .args(["worktree", "remove", "demo"])
        .assert()
        .failure()
        .stderr(contains(
            "has 1 commits not landed on main; run `rimz worktree merge demo`",
        ));
    env.rimz()
        .args(["worktree", "remove", "demo", "--force"])
        .assert()
        .success()
        .stdout(contains(format!(
            "branch deleted: demo was at {tip}; restore with `git branch demo {tip}`"
        )));
}

#[test]
fn worktree_remove_unknown_requires_landing_proof() {
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees/demo");
    let mut marker = rimz::worktree::read_marker_for_worktree(&path)
        .unwrap()
        .unwrap();
    marker.base_branch = None;
    marker.base_ref = "0000000000000000000000000000000000000000".to_owned();
    let marker_path = path.join(git_stdout(
        &path,
        &["rev-parse", "--git-path", "rimz-worktree.json"],
    ));
    std::fs::write(marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
    git(&env.project_root, &["checkout", "--detach"]);
    git(&env.project_root, &["branch", "-D", "main"]);
    env.rimz().args(["worktree", "remove", "demo"]).assert().failure()
        .stderr(contains("has work RimZ cannot prove landed; land it yourself, or pass --force to delete its branch"));
    let listed = env
        .rimz()
        .args(["worktree", "list", "--json"])
        .output()
        .unwrap();
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert!(listed[0]["landed"].is_null());
    assert_eq!(listed[0]["reasons"], serde_json::json!(["unknown"]));
}

#[test]
fn worktree_sweep_previews_an_absent_room_but_refuses_mutation() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);

    env.rimz()
        .args(["worktree", "sweep", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("sweep — skipped · no RimZ store here"));
    env.rimz()
        .args(["worktree", "sweep"])
        .assert()
        .failure()
        .stderr(contains("no room at").and(contains("rimz start")));
    assert!(!env.state_path_for(&env.project_root).root.exists());
}

#[cfg(unix)]
#[test]
fn worktree_cd_execs_the_user_shell_inside_the_named_checkout() {
    use std::os::unix::fs::PermissionsExt;

    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let worktree = env.home_root.join("project-worktrees/demo");
    let observed = env.home_root.join("cd-pwd");
    let shell = env.home_root.join("pwd-shell");
    std::fs::write(&shell, "#!/bin/sh\npwd > \"$RIMZ_TEST_CD_PWD\"\n").expect("write shell");
    let mut permissions = std::fs::metadata(&shell)
        .expect("shell metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&shell, permissions).expect("chmod shell");

    let (success, output) = worktree_pty(&env, &["cd", "demo"], "", Some(&shell));
    assert!(success, "{output}");
    assert!(
        output
            .contains("entered ~/project-worktrees/demo in a new shell; exit returns to ~/project"),
        "{output}"
    );

    assert_eq!(
        std::fs::read_to_string(observed)
            .expect("observed cwd")
            .trim(),
        worktree.display().to_string()
    );
}

#[test]
fn worktree_merge_lands_on_master_and_a_stacked_base_checkout() {
    for target in ["master", "stack/base"] {
        let env = Env::new();
        init_repo(&env.project_root);
        env.record(&env.project_root);
        let target_checkout = if target == "master" {
            git(&env.project_root, &["branch", "-m", "master"]);
            env.project_root.clone()
        } else {
            env.rimz()
                .args(["worktree", "new", "stack/base"])
                .assert()
                .success();
            let path = env.home_root.join("project-worktrees/stack-base");
            commit_file(&path, "base.txt", "base\n", "base");
            path
        };
        env.rimz()
            .args(["worktree", "new", "demo", "--base", target])
            .assert()
            .success();
        let path = env.home_root.join("project-worktrees/demo");
        commit_file(&path, "feature.txt", "feature\n", "feature");
        commit_file(&target_checkout, "advance.txt", "advance\n", "advance base");
        let before = git_stdout(&target_checkout, &["rev-parse", "HEAD"]);
        env.rimz()
            .args(["worktree", "merge", "demo", "--yes"])
            .assert()
            .success()
            .stdout(contains(format!("landed demo on {target}")));
        let head = git_stdout(&path, &["rev-parse", "HEAD"]);
        assert_eq!(git_stdout(&target_checkout, &["rev-parse", "HEAD"]), head);
        assert!(git_succeeds(
            &path,
            &["merge-base", "--is-ancestor", &before, &head]
        ));
        assert!(path.exists());
    }
}

#[test]
fn worktree_merge_fresh_maps_to_a_local_branch_but_list_waits_for_push() {
    for (target, tracking) in [("main", true), ("master", false), ("release", true)] {
        let env = Env::new();
        let repo = &env.project_root;
        init_repo(repo);
        env.record(repo);
        let origin = env.home_root.join("origin.git");
        git(repo, &["clone", "--bare", ".", origin.to_str().unwrap()]);
        git(repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
        git(repo, &["fetch", "origin"]);
        git(repo, &["remote", "set-head", "origin", "--auto"]);
        match target {
            "master" => git(repo, &["branch", "-m", "master"]),
            "release" => git(repo, &["checkout", "-b", "release"]),
            _ => {}
        }
        if tracking {
            git(repo, &["branch", "--set-upstream-to=origin/main", target]);
        }
        env.rimz()
            .args(["worktree", "new", "demo", "--base", "fresh"])
            .assert()
            .success();
        let path = env.home_root.join("project-worktrees/demo");
        commit_file(&path, "feature.txt", "feature\n", "feature");
        commit_file(repo, "advance.txt", "advance\n", "advance target");
        env.rimz()
            .args(["worktree", "merge", "demo", "--yes"])
            .assert()
            .success()
            .stdout(contains(format!("landed demo on {target}")));
        assert_eq!(
            git_stdout(repo, &["rev-parse", "HEAD"]),
            git_stdout(&path, &["rev-parse", "HEAD"])
        );
        for pushed in [false, true] {
            if pushed {
                git(repo, &["push", "origin", &format!("{target}:main")]);
            }
            let output = env
                .rimz()
                .args(["worktree", "list", "--json"])
                .output()
                .unwrap();
            assert!(output.status.success());
            let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(rows[0]["base_branch"], "origin/main");
            assert_eq!(rows[0]["landed"], pushed);
        }
    }
}

#[test]
fn worktree_merge_fresh_requires_a_local_checked_out_target() {
    let env = Env::new();
    let repo = &env.project_root;
    init_repo(repo);
    env.record(repo);
    let origin = env.home_root.join("origin.git");
    git(repo, &["clone", "--bare", ".", origin.to_str().unwrap()]);
    git(repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(repo, &["fetch", "origin"]);
    git(repo, &["remote", "set-head", "origin", "--auto"]);
    git(repo, &["branch", "--set-upstream-to=origin/main", "main"]);
    env.rimz()
        .args(["worktree", "new", "demo", "--base", "fresh"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees/demo");
    commit_file(&path, "feature.txt", "feature\n", "feature");
    let before = git_stdout(&path, &["rev-parse", "HEAD"]);
    git(repo, &["checkout", "--detach"]);
    env.rimz()
        .args(["worktree", "merge", "demo", "--yes"])
        .assert()
        .failure()
        .stderr(contains("main must be checked out"))
        .stderr(contains("origin/main must be checked out").not());
    git(repo, &["branch", "-D", "main"]);
    env.rimz()
        .args(["worktree", "merge", "demo", "--yes"])
        .assert()
        .failure()
        .stderr(contains("no branch landing target"));
    assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"]), before);
}

#[test]
fn worktree_merge_uses_the_superseded_bases_comparison_target() {
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "base"])
        .assert()
        .success();
    let base = env.home_root.join("project-worktrees/base");
    commit_file(&base, "base.txt", "base\n", "base");
    env.rimz()
        .args(["worktree", "new", "demo", "--base", "base"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees/demo");
    commit_file(&path, "feature.txt", "feature\n", "feature");
    commit_file(&env.project_root, "trunk.txt", "trunk\n", "trunk");
    git(&env.project_root, &["merge", "--squash", "base"]);
    git(&env.project_root, &["commit", "-m", "land base"]);
    let base_before = git_stdout(&base, &["rev-parse", "HEAD"]);
    env.rimz()
        .args(["worktree", "merge", "demo", "--yes"])
        .assert()
        .success()
        .stdout(contains("landed demo on main"));
    assert_eq!(
        git_stdout(&env.project_root, &["rev-parse", "HEAD"]),
        git_stdout(&path, &["rev-parse", "HEAD"])
    );
    assert_eq!(git_stdout(&base, &["rev-parse", "HEAD"]), base_before);
}

#[test]
fn worktree_merge_requires_a_checked_out_branch_target() {
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees/demo");
    commit_file(&path, "feature.txt", "feature\n", "feature");
    let before = git_stdout(&path, &["rev-parse", "HEAD"]);
    git(&env.project_root, &["checkout", "--detach"]);
    env.rimz()
        .args(["worktree", "merge", "demo", "--yes"])
        .assert()
        .failure()
        .stderr(contains("main must be checked out"));
    git(&env.project_root, &["branch", "-D", "main"]);
    env.rimz()
        .args(["worktree", "merge", "demo", "--yes"])
        .assert()
        .failure()
        .stderr(contains("no branch landing target"));
    assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"]), before);
}

#[test]
fn worktree_merge_without_a_terminal_requires_yes_and_changes_nothing() {
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees/demo");
    commit_file(&path, "feature.txt", "feature\n", "feature");
    let before = git_stdout(&env.project_root, &["rev-parse", "HEAD"]);
    env.rimz().args(["worktree", "merge", "demo"]).assert().failure()
        .stderr(contains("rimz worktree merge needs a terminal to confirm; pass --yes to land `demo` on `main` without asking"));
    assert_eq!(
        git_stdout(&env.project_root, &["rev-parse", "HEAD"]),
        before
    );
}

#[cfg(unix)]
fn worktree_pty(env: &Env, args: &[&str], answer: &str, shell: Option<&Path>) -> (bool, String) {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::Write;

    let pair = native_pty_system()
        .openpty(PtySize::default())
        .expect("open pty");
    let mut command = CommandBuilder::new(env.rimz().get_program());
    env.pin_pty_command(&mut command);
    command.cwd(&env.project_root);
    command.arg("worktree");
    command.args(args);
    if let Some(shell) = shell {
        command.env("SHELL", shell);
        command.env("RIMZ_TEST_CD_PWD", env.home_root.join("cd-pwd"));
    }
    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let mut writer = pair.master.take_writer().expect("pty writer");
    let mut child = pair.slave.spawn_command(command).expect("spawn worktree");
    drop(pair.slave);
    writer.write_all(answer.as_bytes()).expect("answer prompt");
    let status = child.wait().expect("wait worktree");
    let mut output = String::new();
    reader.read_to_string(&mut output).expect("read pty output");
    (status.success(), output)
}

#[cfg(unix)]
#[test]
fn worktree_merge_prompts_and_declining_changes_nothing() {
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees/demo");
    commit_file(&path, "feature.txt", "feature\n", "feature");
    let before = git_stdout(&env.project_root, &["rev-parse", "HEAD"]);
    let (success, output) = worktree_pty(&env, &["merge", "demo"], "n\n", None);
    assert!(success, "{output}");
    assert!(output.contains("land demo on main? [y/N]"), "{output}");
    assert!(output.contains("nothing changed."), "{output}");
    assert_eq!(
        git_stdout(&env.project_root, &["rev-parse", "HEAD"]),
        before
    );
    let (success, output) = worktree_pty(&env, &["merge", "demo"], "y\n", None);
    assert!(success, "{output}");
    assert!(output.contains("landed demo on main"), "{output}");
    assert_eq!(
        git_stdout(&env.project_root, &["rev-parse", "HEAD"]),
        git_stdout(&path, &["rev-parse", "HEAD"])
    );
}

#[test]
fn worktree_path_cd_aliases_and_missing_names() {
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "feat/demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees/feat-demo");
    env.rimz()
        .args(["worktree", "path", "feat/demo"])
        .assert()
        .success()
        .stdout(format!("{}\n", path.display()))
        .stderr("");
    env.rimz().args(["worktree", "cd", "feat/demo"]).assert().failure()
        .stderr(contains("rimz worktree cd needs a terminal; the tree is at ~/project-worktrees/feat-demo (`rimz worktree path feat/demo` prints it)"));
    env.rimz()
        .args(["worktree", "ls", "--json"])
        .assert()
        .success()
        .stdout(contains("feat-demo"));
    for verb in ["cd", "path", "merge", "remove"] {
        env.rimz().args(["worktree", verb, "feat-dmo"]).assert().failure()
            .stderr(contains("no worktree `feat-dmo` in ~/project-worktrees; did you mean `feat-demo`? `rimz worktree list` shows the 1 that exist"));
        env.rimz().args(["worktree", verb, "missing"]).assert().failure()
            .stderr(contains("no worktree `missing` in ~/project-worktrees; `rimz worktree list` shows the 1 that exist"));
    }
    env.rimz()
        .args(["worktree", "rm", "feat/demo"])
        .assert()
        .success()
        .stdout(contains("removed feat/demo"));
    assert!(!path.exists());
}

#[test]
fn worktree_new_reports_short_provenance_next_step_and_curated_json() {
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    let base = git_stdout(&env.project_root, &["rev-parse", "HEAD"]);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success()
        .stdout(contains(format!(
            "branch : demo, from main at {}",
            &base[..7]
        )))
        .stdout(contains(
            "next   : rimz agents <spec> -w demo    or    rimz worktree cd demo",
        ))
        .stdout(contains("base branch:").not())
        .stdout(contains("base   :").not());
    let output = env
        .rimz()
        .args(["worktree", "new", "feat/json", "--json"])
        .output()
        .expect("new json");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let row: Value = serde_json::from_slice(&output.stdout).expect("json receipt");
    assert_eq!(
        row,
        serde_json::json!({
            "name": "feat-json", "path": env.home_root.join("project-worktrees/feat-json"),
            "branch": "feat/json", "base_branch": "main", "base_ref": base, "reused": false,
        })
    );
    git(&env.project_root, &["checkout", "--detach"]);
    env.rimz()
        .args(["worktree", "new", "detached"])
        .assert()
        .success()
        .stdout(contains(format!("branch : detached, from {}", &base[..7])));
}

#[test]
fn worktree_merge_rebases_then_fast_forwards_main() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let worktree = env.home_root.join("project-worktrees/demo");
    commit_file(&worktree, "feature.txt", "feature\n", "feature");
    commit_file(&env.project_root, "main.txt", "main\n", "advance main");
    let main_before = git_stdout(&env.project_root, &["rev-parse", "HEAD"]);

    env.rimz()
        .args(["worktree", "merge", "demo", "--yes"])
        .assert()
        .success()
        .stdout(contains("landed demo on main"))
        .stdout(contains("the worktree stays at ~/project-worktrees/demo; `rimz worktree remove demo` reclaims it"));

    let main_after = git_stdout(&env.project_root, &["rev-parse", "HEAD"]);
    assert_eq!(main_after, git_stdout(&worktree, &["rev-parse", "HEAD"]));
    assert_ne!(main_before, main_after);
    assert!(
        Command::new("git")
            .current_dir(&env.project_root)
            .args(["merge-base", "--is-ancestor", &main_before, &main_after])
            .status()
            .expect("git merge-base")
            .success(),
        "rebased feature remains a fast-forward descendant of main"
    );
}

#[test]
fn worktree_merge_refuses_dirty_checkouts() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let worktree = env.home_root.join("project-worktrees/demo");
    std::fs::write(worktree.join("dirty.txt"), "dirty\n").expect("dirty worktree");

    env.rimz()
        .args(["worktree", "merge", "demo", "--yes"])
        .assert()
        .failure()
        .stderr(contains("its checkout has local changes"));

    std::fs::remove_file(worktree.join("dirty.txt")).expect("clean worktree");
    std::fs::write(env.project_root.join("dirty.txt"), "dirty\n").expect("dirty main");
    env.rimz()
        .args(["worktree", "merge", "demo", "--yes"])
        .assert()
        .failure()
        .stderr(contains("the main checkout has local changes"));
}

#[test]
fn worktree_merge_refuses_a_checkout_with_a_live_agent() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let worktree = env.home_root.join("project-worktrees/demo");
    let live_id = AgentSessionId::from("sess-live-merge-worker");
    let mut live =
        AgentLifecycleObservation::new(Some(live_id.clone()), LifecycleSignal::Registered);
    live.worktree_path = Some(worktree.display().to_string());
    live.worktree_branch = Some("demo".to_owned());
    live.runtime_owner = Some(rimz::pane::RuntimeOwner::new(
        rimz::pane::RuntimeOwnerKind::Agent,
        live_id.as_str(),
        std::process::id(),
        None,
    ));
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "rimz-test",
            "claude",
            "SessionStart",
            &live,
        ))
        .expect("append live worktree session");

    env.rimz()
        .args(["worktree", "merge", "demo", "--yes"])
        .assert()
        .failure()
        .stderr(contains(
            "cannot merge worktree `demo`: it is in use by @claude",
        ));
}

#[test]
fn worktree_merge_refuses_an_in_progress_git_operation() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    std::fs::write(env.project_root.join(".git/MERGE_HEAD"), "pending\n")
        .expect("mark merge in progress");

    env.rimz()
        .args(["worktree", "merge", "demo", "--yes"])
        .assert()
        .failure()
        .stderr(contains(
            "the main checkout has a Git operation in progress",
        ));
}

#[test]
fn worktree_merge_aborts_a_conflicting_rebase_without_advancing_main() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let worktree = env.home_root.join("project-worktrees/demo");
    std::fs::write(worktree.join("README.md"), "feature\n").expect("feature content");
    git(&worktree, &["add", "README.md"]);
    git(&worktree, &["commit", "-m", "feature conflict"]);
    let feature_before = git_stdout(&worktree, &["rev-parse", "HEAD"]);
    std::fs::write(env.project_root.join("README.md"), "main\n").expect("main content");
    git(&env.project_root, &["add", "README.md"]);
    git(&env.project_root, &["commit", "-m", "main conflict"]);
    let main_before = git_stdout(&env.project_root, &["rev-parse", "HEAD"]);

    env.rimz()
        .args(["worktree", "merge", "demo", "--yes"])
        .assert()
        .failure()
        .stderr(contains(format!(
            "rebase onto {} failed",
            git_stdout(&env.project_root, &["symbolic-ref", "--short", "HEAD"])
        )))
        .stderr(contains("no commits were landed on main"));

    assert_eq!(
        git_stdout(&env.project_root, &["rev-parse", "HEAD"]),
        main_before
    );
    assert_eq!(
        git_stdout(&worktree, &["rev-parse", "HEAD"]),
        feature_before
    );
    assert!(
        git_stdout(&worktree, &["status", "--porcelain"]).is_empty(),
        "failed rebase is aborted cleanly"
    );
}

#[test]
fn worktree_remove_leaves_candidate_cwd_before_git_removal() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");

    env.rimz()
        .current_dir(&path)
        .args(["worktree", "remove", "demo"])
        .assert()
        .success()
        .stdout(contains("removed demo"));

    assert!(!path.exists(), "worktree removed from inside its checkout");
}

#[test]
fn worktree_new_accepts_branch_style_name_and_removes_by_raw_spelling() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);

    env.rimz()
        .args(["worktree", "new", "feat/great"])
        .assert()
        .success()
        .stdout(contains("created feat-great"))
        .stdout(contains("branch : feat/great"));

    let path = env.home_root.join("project-worktrees").join("feat-great");
    assert!(path.is_dir(), "dashed worktree path exists");
    assert_eq!(
        git_stdout(&path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feat/great"
    );

    env.rimz()
        .args(["worktree", "new", "feat-great"])
        .assert()
        .failure()
        .stderr(contains("worktree `feat-great` already exists"));

    env.rimz()
        .args(["worktree", "remove", "feat/great"])
        .assert()
        .success()
        .stdout(contains("removed feat/great"));
    assert!(!path.exists(), "slash spelling removes dashed worktree");
    assert!(
        !branch_exists(&env.project_root, "feat/great"),
        "slash branch deleted"
    );
}

#[test]
fn worktree_new_explicit_branch_overrides_branch_style_name() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);

    env.rimz()
        .args(["worktree", "new", "feat/great", "--branch", "other"])
        .assert()
        .success()
        .stdout(contains("created feat-great"))
        .stdout(contains("branch : other"));

    let path = env.home_root.join("project-worktrees").join("feat-great");
    assert_eq!(
        git_stdout(&path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "other"
    );
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");
    assert_eq!(marker.name, "feat-great");
    assert_eq!(marker.branch, "other");
}

#[test]
fn worktree_new_rejects_empty_base() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);

    env.rimz()
        .args(["worktree", "new", "demo", "--base", ""])
        .assert()
        .failure()
        .stderr(contains("base ref cannot be empty"));
}

#[test]
fn worktree_new_refuses_a_name_live_agents_hold_as_a_lane() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);

    env.install_agent_hooks("claude");
    crate::channel::register_idle_lane_agent(&env, "sess-lane", "demo", None);

    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .failure()
        .stderr(contains("channel `demo` is held by live agents"));
}

#[test]
fn worktree_new_checks_dashed_lane_for_branch_style_name() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);

    env.install_agent_hooks("claude");
    crate::channel::register_idle_lane_agent(&env, "sess-lane", "feat-great", None);

    env.rimz()
        .args(["worktree", "new", "feat/great"])
        .assert()
        .failure()
        .stderr(contains("channel `feat-great` is held by live agents"));
}

#[test]
fn worktree_new_archives_messages_for_recreated_channel() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    let message_id = queue_channel_message(&env, "demo", "old work");

    env.rimz()
        .args(["worktree", "new", "demo", "--branch", "scratch"])
        .assert()
        .success();

    assert!(env.store().list_messages().expect("messages").is_empty());
    let archived = env
        .read_events()
        .into_iter()
        .find(|event| {
            event.method == "message.archived"
                && event.params_value()["message_id"] == message_id.as_str()
        })
        .expect("message");
    assert_eq!(archived.params_value()["reason"], "channel recreated");
}

#[test]
fn worktree_remove_archives_messages_for_removed_channel() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo", "--branch", "scratch"])
        .assert()
        .success();
    let worktree = env.home_root.join("project-worktrees").join("demo");
    let ghost_id = AgentSessionId::from("sess-removed-ghost");
    let mut ghost =
        AgentLifecycleObservation::new(Some(ghost_id.clone()), LifecycleSignal::Registered);
    ghost.worktree_path = Some(worktree.display().to_string());
    ghost.worktree_branch = Some("scratch".to_owned());
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "rimz-test",
            "claude",
            "SessionStart",
            &ghost,
        ))
        .expect("append stale worktree session");
    let message_id = queue_channel_message(&env, "demo", "old work");

    env.rimz()
        .args(["worktree", "remove", "demo"])
        .assert()
        .success();

    assert!(env.store().list_messages().expect("messages").is_empty());
    let archived = env
        .read_events()
        .into_iter()
        .find(|event| {
            event.method == "message.archived"
                && event.params_value()["message_id"] == message_id.as_str()
        })
        .expect("message");
    assert_eq!(archived.params_value()["reason"], "worktree removed");
    let store = env.store();
    let audit = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("audit projection");
    assert!(
        audit
            .agents
            .iter()
            .any(|agent| agent.agent_id == ghost_id && agent.ended_at.is_some())
    );
    let runtime = store
        .runtime_projection(rimz::RuntimeScope::Runtime)
        .expect("runtime projection");
    assert!(
        runtime
            .agents
            .iter()
            .all(|agent| agent.agent_id != ghost_id)
    );
}

#[test]
fn worktree_remove_refuses_while_a_live_agent_works_there() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo", "--branch", "scratch"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    let live_id = AgentSessionId::from("sess-live-worker");
    let mut live =
        AgentLifecycleObservation::new(Some(live_id.clone()), LifecycleSignal::Registered);
    live.worktree_path = Some(path.display().to_string());
    live.worktree_branch = Some("scratch".to_owned());
    live.runtime_owner = Some(rimz::pane::RuntimeOwner::new(
        rimz::pane::RuntimeOwnerKind::Agent,
        live_id.as_str(),
        std::process::id(),
        None,
    ));
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "rimz-test",
            "claude",
            "SessionStart",
            &live,
        ))
        .expect("append live worktree session");

    env.rimz()
        .args(["worktree", "remove", "demo"])
        .assert()
        .failure()
        .stderr(contains("worktree `demo` is in use by @claude"))
        .stderr(contains("use --force to remove it"));
    assert!(path.exists(), "refusal keeps the checkout");

    let listed = env
        .rimz()
        .args(["worktree", "list", "--json"])
        .output()
        .unwrap();
    let listed: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed[0]["reasons"], serde_json::json!(["in_use"]));
    assert!(
        listed[0]["holders"]["agents"][0]
            .as_str()
            .unwrap()
            .starts_with("@claude")
    );
    let preview = env
        .rimz()
        .args(["worktree", "sweep", "--dry-run", "--json"])
        .output()
        .unwrap();
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(preview["kept"][0]["reasons"], listed[0]["reasons"]);
    assert_eq!(preview["kept"][0]["holders"], listed[0]["holders"]);

    env.rimz()
        .args(["worktree", "remove", "demo", "--force"])
        .assert()
        .success()
        .stderr(contains("warning: worktree `demo` is in use by"));
    assert!(!path.exists(), "--force removes it anyway");
}

#[test]
fn worktree_remove_survives_history_append_failure() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    queue_channel_message(&env, "demo", "old work");
    block_message_history(&env);

    env.rimz()
        .args(["worktree", "remove", "demo"])
        .assert()
        .success()
        .stderr(contains("cannot append terminal message history"));

    assert!(!path.exists(), "removal survives history failure");
    assert_messages_archived_without_history(&env);
}

#[cfg(unix)]
#[test]
fn worktree_remove_reports_live_queue_write_failure_after_removal() {
    use std::os::unix::fs::PermissionsExt;

    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    commit_file(&path, "feature.txt", "feature\n", "feature");
    let tip = git_stdout(&path, &["rev-parse", "HEAD"]);
    let message_id = queue_channel_message(&env, "demo", "old work");
    let messages_dir = env.store().paths().messages_dir.clone();
    let permissions = std::fs::metadata(&messages_dir)
        .expect("messages directory")
        .permissions();
    std::fs::set_permissions(&messages_dir, std::fs::Permissions::from_mode(0o555))
        .expect("block atomic queue replacement, not queue reads");
    let probe = messages_dir.join("write-probe");
    if std::fs::write(&probe, b"").is_ok() {
        std::fs::set_permissions(&messages_dir, permissions).expect("restore permissions");
        crate::common::skip("this user bypasses directory write permissions");
        return;
    }
    let output = env
        .rimz()
        .args(["worktree", "remove", "demo", "--force"])
        .output();
    std::fs::set_permissions(&messages_dir, permissions).expect("restore permissions");
    output
        .expect("remove command")
        .assert()
        .code(1)
        .stdout(contains("removed demo\n"))
        .stdout(contains(format!(
            "branch deleted: demo was at {tip}; restore with `git branch demo {tip}`"
        )))
        .stderr(contains("archiving messages for removed worktree channel"))
        .stderr(contains("messages.jsonl.tmp."));

    assert!(
        !path.exists(),
        "removal completes before the queue write fails"
    );
    assert!(!branch_exists(&env.project_root, "demo"));
    assert!(
        env.store()
            .list_messages()
            .expect("queue is still readable")
            .iter()
            .any(|message| message.message_id == message_id
                && message.status == rimz::store::message::MessageStatus::Queued)
    );
    assert!(
        !env.read_events()
            .iter()
            .any(|event| event.method == "message.archived")
    );
}

#[cfg(unix)]
#[test]
fn worktree_cleanup_retires_sessions_and_archives_messages_after_removal() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo", "--branch", "scratch"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    let ghost_id = AgentSessionId::from("sess-cleanup-ghost");
    let mut ghost =
        AgentLifecycleObservation::new(Some(ghost_id.clone()), LifecycleSignal::Registered);
    ghost.worktree_path = Some(path.display().to_string());
    ghost.worktree_branch = Some("scratch".to_owned());
    ghost.runtime_owner = Some(rimz::pane::RuntimeOwner::new(
        rimz::pane::RuntimeOwnerKind::Agent,
        ghost_id.as_str(),
        u32::MAX,
        None,
    ));
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "rimz-test",
            "claude",
            "SessionStart",
            &ghost,
        ))
        .expect("append stale worktree session");
    let message_id = queue_channel_message(&env, "demo", "old work");

    env.rimz()
        .args([
            "worktree",
            "cleanup",
            path.to_str().expect("utf-8 path"),
            "--non-interactive",
        ])
        .assert()
        .success();

    assert!(!path.exists(), "cleanup removed worktree");
    assert!(env.store().list_messages().expect("messages").is_empty());
    let archived = env
        .read_events()
        .into_iter()
        .find(|event| {
            event.method == "message.archived"
                && event.params_value()["message_id"] == message_id.as_str()
        })
        .expect("archived message");
    assert_eq!(archived.params_value()["reason"], "worktree removed");
    let store = env.store();
    let audit = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("audit projection");
    assert!(
        audit
            .agents
            .iter()
            .any(|agent| agent.agent_id == ghost_id && agent.ended_at.is_some())
    );
    let runtime = store
        .runtime_projection(rimz::RuntimeScope::Runtime)
        .expect("runtime projection");
    assert!(
        runtime
            .agents
            .iter()
            .all(|agent| agent.agent_id != ghost_id)
    );
}

#[test]
fn worktree_cleanup_survives_history_append_failure() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    queue_channel_message(&env, "demo", "old work");
    block_message_history(&env);

    env.rimz()
        .args([
            "worktree",
            "cleanup",
            path.to_str().expect("utf-8 path"),
            "--non-interactive",
        ])
        .assert()
        .success();

    assert!(!path.exists(), "cleanup survives history failure");
    assert_messages_archived_without_history(&env);
}

#[test]
fn worktree_gc_survives_history_append_failure() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    queue_channel_message(&env, "demo", "old work");
    block_message_history(&env);

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("removed: demo"))
        .stderr(contains("cannot append terminal message history"));

    assert!(!path.exists(), "gc removal survives history failure");
    assert_messages_archived_without_history(&env);
}

#[cfg(unix)]
#[test]
fn worktree_new_from_pr_fetches_github_style_ref() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let (pr_head, trunk) = publish_pr_ref(&env, "refs/pull/1/head");
    configure_github_origin_rewrite(&env);
    let shim_dir = write_gh_pr_head_shim(&env, gh_same_repo_head());

    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim_dir))
        .assert()
        .success()
        .stdout(contains("created pr-1"))
        .stdout(contains("branch : feature"));

    let path = env.home_root.join("project-worktrees").join("pr-1");
    assert_eq!(
        git_stdout(&path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feature"
    );
    assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"]), pr_head);
    assert_eq!(
        git_stdout(&path, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
        "origin/feature"
    );
    commit_file(&path, "pushed.txt", "pushed\n", "push from PR worktree");
    git(&path, &["push"]);
    assert_eq!(
        git_stdout(
            &env.home_root.join("origin.git"),
            &["rev-parse", "refs/heads/feature"]
        ),
        git_stdout(&path, &["rev-parse", "HEAD"]),
        "plain push advances the PR head branch"
    );
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");
    assert_eq!(marker.base_branch.as_deref(), Some("main"));
    assert_eq!(marker.base_ref, trunk);
}

#[test]
fn worktree_new_from_pr_url_fetches_gitlab_ref() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let (pr_head, _trunk) = publish_pr_ref(&env, "refs/merge-requests/1/head");
    configure_origin_rewrite(&env, "https://gitlab.com/org/repo.git");

    env.rimz()
        .args([
            "worktree",
            "new",
            "--from-pr",
            "https://gitlab.com/org/repo/-/merge_requests/1",
        ])
        .assert()
        .success()
        .stdout(contains("created pr-1"))
        .stdout(contains("branch : pr-1"))
        .stdout(contains(
            "review-only checkout (origin has no supported forge CLI)",
        ));

    let path = env.home_root.join("project-worktrees").join("pr-1");
    assert_eq!(
        git_stdout(&path, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "pr-1"
    );
    assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"]), pr_head);
    assert!(
        !git_succeeds(&path, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
        "review-only checkout has no upstream"
    );
    assert_eq!(
        git_stdout(&env.project_root, &["for-each-ref", "refs/rimz/"]),
        "",
        "temporary PR ref is cleaned up"
    );
}

#[test]
fn worktree_new_rejects_pr_url_for_another_repository() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    publish_pr_ref(&env, "refs/pull/1/head");
    configure_origin_rewrite(&env, "https://github.com/org/repo.git");

    env.rimz()
        .args([
            "worktree",
            "new",
            "--from-pr",
            "https://github.com/other/repo/pull/1",
        ])
        .assert()
        .failure()
        .stderr(contains(
            "PR URL targets `other/repo` but origin is `org/repo`",
        ));

    assert!(!env.home_root.join("project-worktrees/pr-1").exists());
    assert_eq!(
        git_stdout(&env.project_root, &["for-each-ref", "refs/rimz/"]),
        ""
    );
}

#[test]
fn from_pr_reuse_requires_matching_pr_provenance() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    publish_pr_ref_without_branch(&env, "refs/pull/1/head");
    env.rimz()
        .args(["worktree", "new", "review", "--from-pr", "1"])
        .assert()
        .success();

    let target = rimz::forge::parse("2").expect("PR target");
    let err = rimz::worktree::create_from_pr(
        &env.project_root,
        &rimz::config::WorktreeConfig::default(),
        &target,
        Some("review"),
        None,
        true,
        None,
    )
    .expect_err("different PR must not reuse worktree");

    assert!(err.to_string().contains("not requested PR 2"), "{err}");

    configure_origin_rewrite(&env, "https://github.com/org/repo.git");
    let other_repo = rimz::forge::parse("https://github.com/other/repo/pull/1").unwrap();
    let err = rimz::worktree::create_from_pr(
        &env.project_root,
        &rimz::config::WorktreeConfig::default(),
        &other_repo,
        Some("review"),
        None,
        true,
        None,
    )
    .expect_err("reused worktree must still validate URL identity");
    assert!(err.to_string().contains("other/repo"), "{err}");
}

#[cfg(unix)]
#[test]
fn worktree_new_from_pr_adopts_matching_local_branch() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let (pr_head, _) = publish_pr_ref(&env, "refs/pull/1/head");
    git(&env.project_root, &["branch", "feature", pr_head.as_str()]);
    configure_github_origin_rewrite(&env);
    let shim_dir = write_gh_pr_head_shim(&env, gh_same_repo_head());

    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim_dir))
        .assert()
        .success();

    let path = env.home_root.join("project-worktrees/pr-1");
    assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"]), pr_head);
    assert_eq!(
        git_stdout(&path, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
        "origin/feature"
    );
}

#[cfg(unix)]
#[test]
fn worktree_new_from_pr_adopts_matching_local_branch_from_tea() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let (pr_head, _) = publish_pr_ref(&env, "refs/pull/1/head");
    git(&env.project_root, &["branch", "feature", pr_head.as_str()]);
    configure_gitea_origin_rewrite(&env);
    let shim_dir = write_tea_pr_head_shim(&env, tea_same_repo_head());

    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim_dir))
        .assert()
        .success()
        .stdout(contains("branch : feature"));

    let path = env.home_root.join("project-worktrees/pr-1");
    assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"]), pr_head);
    assert_eq!(
        git_stdout(&path, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
        "origin/feature"
    );
}

#[cfg(unix)]
#[test]
fn worktree_new_from_pr_fast_forwards_ancestor_local_branch() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let (pr_head, trunk) = publish_pr_ref(&env, "refs/pull/1/head");
    git(&env.project_root, &["branch", "feature", trunk.as_str()]);
    configure_github_origin_rewrite(&env);
    let shim_dir = write_gh_pr_head_shim(&env, gh_same_repo_head());

    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim_dir))
        .assert()
        .success()
        .stderr(contains("fast-forwarding"));

    let path = env.home_root.join("project-worktrees/pr-1");
    assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"]), pr_head);
}

#[cfg(unix)]
#[test]
fn worktree_new_from_pr_refuses_diverged_local_branch() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    publish_pr_ref(&env, "refs/pull/1/head");
    git(&env.project_root, &["checkout", "-b", "feature"]);
    commit_file(&env.project_root, "local.txt", "local\n", "diverge locally");
    let local = git_stdout(&env.project_root, &["rev-parse", "HEAD"]);
    git(&env.project_root, &["checkout", "main"]);
    configure_github_origin_rewrite(&env);
    let shim_dir = write_gh_pr_head_shim(&env, gh_same_repo_head());

    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim_dir))
        .assert()
        .failure()
        .stderr(contains("local commits are not in the PR head"))
        .stderr(contains("merge cleanly"));
    assert_eq!(
        git_stdout(&env.project_root, &["rev-parse", "feature"]),
        local
    );
}

#[cfg(unix)]
#[test]
fn worktree_new_from_pr_refuses_branch_checked_out_elsewhere() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let (pr_head, _) = publish_pr_ref(&env, "refs/pull/1/head");
    git(&env.project_root, &["branch", "feature", pr_head.as_str()]);
    let other = env.home_root.join("other-feature");
    git(
        &env.project_root,
        &[
            "worktree",
            "add",
            other.to_str().expect("utf8 worktree"),
            "feature",
        ],
    );
    configure_github_origin_rewrite(&env);
    let shim_dir = write_gh_pr_head_shim(&env, gh_same_repo_head());

    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim_dir))
        .assert()
        .failure()
        .stderr(contains("local PR branch `feature` conflicts"))
        .stderr(contains("does not manage"));
}

#[cfg(unix)]
#[test]
fn worktree_new_from_pr_reuses_marked_head_and_preserves_channel() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let (head, _) = publish_pr_ref(&env, "refs/pull/1/head");
    env.rimz()
        .args(["worktree", "new", "feature", "--base", &head])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees/feature");
    let mut marker = rimz::worktree::read_marker_for_worktree(&path)
        .unwrap()
        .unwrap();
    queue_channel_message(&env, "feature", "keep this");
    configure_github_origin_rewrite(&env);
    let shim = write_gh_pr_head_shim(&env, gh_same_repo_head());
    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim))
        .assert()
        .success()
        .stdout(contains("reused feature"));
    marker.from_pr = Some(1);
    assert_eq!(
        rimz::worktree::read_marker_for_worktree(&path).unwrap(),
        Some(marker)
    );
    assert!(!env.home_root.join("project-worktrees/pr-1").exists());
    assert_eq!(env.store().list_messages().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn worktree_new_from_pr_named_holder_backfills_and_wrong_name_refuses() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let (head, _) = publish_pr_ref(&env, "refs/pull/1/head");
    env.rimz()
        .args(["worktree", "new", "feature", "--base", &head])
        .assert()
        .success();
    configure_github_origin_rewrite(&env);
    let shim = write_gh_pr_head_shim(&env, gh_same_repo_head());
    env.rimz()
        .args(["worktree", "new", "other", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim))
        .assert()
        .failure()
        .stderr(contains("omit -w, or pass -w feature"));
    env.rimz()
        .args(["worktree", "new", "feature", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim))
        .assert()
        .success()
        .stdout(contains("reused feature"));
    let path = env.home_root.join("project-worktrees/feature");
    assert_eq!(
        rimz::worktree::read_marker_for_worktree(&path)
            .unwrap()
            .unwrap()
            .from_pr,
        Some(1)
    );
}

#[cfg(unix)]
#[test]
fn worktree_new_from_pr_holder_fast_forward_protects_dirty_files() {
    if git_missing() {
        return;
    }
    for dirty in [false, true] {
        let env = Env::new();
        env.record(&env.project_root);
        let (head, trunk) = publish_pr_ref(&env, "refs/pull/1/head");
        env.rimz()
            .args(["worktree", "new", "feature", "--base", &trunk])
            .assert()
            .success();
        let path = env.home_root.join("project-worktrees/feature");
        if dirty {
            std::fs::write(path.join("feature.txt"), "uncommitted").unwrap();
        }
        configure_github_origin_rewrite(&env);
        let shim = write_gh_pr_head_shim(&env, gh_same_repo_head());
        let output = env
            .rimz()
            .args(["worktree", "new", "--from-pr", "1"])
            .env("PATH", path_with_front(&shim))
            .output()
            .unwrap();
        if dirty {
            output
                .assert()
                .failure()
                .stderr(contains("could not move local PR branch `feature`"))
                .stderr(contains("commit or stash"))
                .stderr(contains(path.to_str().unwrap()))
                .stderr(contains("--branch").not());
            assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"]), trunk);
            assert_eq!(
                std::fs::read_to_string(path.join("feature.txt")).unwrap(),
                "uncommitted"
            );
        } else {
            output
                .assert()
                .success()
                .stderr(contains("fast-forwarding"))
                .stdout(contains("reused feature"));
            assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"]), head);
        }
    }
}

#[cfg(unix)]
#[test]
fn worktree_new_from_pr_refuses_main_and_other_pr_holders() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let (head, _) = publish_pr_ref(&env, "refs/pull/1/head");
    git(&env.project_root, &["checkout", "-b", "feature", &head]);
    configure_github_origin_rewrite(&env);
    let shim = write_gh_pr_head_shim(&env, gh_same_repo_head());
    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim))
        .assert()
        .failure()
        .stderr(contains("main checkout"));
    git(&env.project_root, &["checkout", "main"]);
    env.rimz()
        .args(["worktree", "new", "--from-pr", "2"])
        .env("PATH", path_with_front(&shim))
        .assert()
        .success();
    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim))
        .assert()
        .failure()
        .stderr(contains("not requested PR 1"));
}

#[cfg(unix)]
#[test]
fn worktree_new_from_pr_rebased_and_conflicting_tips_do_not_move() {
    if git_missing() {
        return;
    }
    for holder in [false, true] {
        for rebased in [false, true] {
            let env = Env::new();
            env.record(&env.project_root);
            let (head, trunk) = publish_pr_ref(&env, "refs/pull/1/head");
            git(&env.project_root, &["checkout", "-b", "feature", &trunk]);
            if rebased {
                commit_file(&env.project_root, "base.txt", "base\n", "new base");
                git(&env.project_root, &["cherry-pick", &head]);
            } else {
                commit_file(
                    &env.project_root,
                    "feature.txt",
                    "conflicting\n",
                    "local patch",
                );
            }
            let local = git_stdout(&env.project_root, &["rev-parse", "HEAD"]);
            // For patch equivalence, keep only the common feature patch on the local side.
            if rebased {
                git(&env.project_root, &["push", "--force", "origin", "feature"]);
                git(&env.project_root, &["reset", "--hard", &head]);
            }
            let expected = if rebased { &head } else { &local };
            git(&env.project_root, &["checkout", "main"]);
            if holder {
                git(&env.project_root, &["branch", "-D", "feature"]);
                env.rimz()
                    .args(["worktree", "new", "feature", "--base", expected])
                    .assert()
                    .success();
            }
            configure_github_origin_rewrite(&env);
            let shim = write_gh_pr_head_shim(&env, gh_same_repo_head());
            env.rimz()
                .args(["worktree", "new", "--from-pr", "1"])
                .env("PATH", path_with_front(&shim))
                .assert()
                .failure()
                .stderr(contains(if rebased {
                    "are all contained in it"
                } else {
                    "they conflict"
                }))
                .stderr(contains(if holder { "reset --keep" } else { "branch -f" }));
            assert_eq!(
                git_stdout(&env.project_root, &["rev-parse", "feature"]),
                *expected
            );
        }
    }
}

#[test]
fn worktree_new_from_pr_without_forge_cli_is_review_only() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let (pr_head, _) = publish_pr_ref_without_branch(&env, "refs/pull/1/head");

    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .assert()
        .success()
        .stdout(contains("branch : pr-1"))
        .stdout(contains("review-only checkout"));

    let path = env.home_root.join("project-worktrees/pr-1");
    assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"]), pr_head);
    assert!(
        !git_succeeds(&path, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
        "automatic review branch has no upstream"
    );

    env.rimz()
        .args([
            "worktree",
            "new",
            "explicit-review",
            "--from-pr",
            "1",
            "--branch",
            "review-1",
        ])
        .assert()
        .success()
        .stdout(contains("branch : review-1"));
    let path = env.home_root.join("project-worktrees/explicit-review");
    assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"]), pr_head);
    assert!(
        !git_succeeds(&path, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
        "explicit review branch has no upstream"
    );
}

#[cfg(unix)]
#[test]
fn worktree_new_from_fork_pr_tracks_fork_from_gh() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let (_pr_head, _) = publish_pr_ref(&env, "refs/pull/1/head");
    configure_github_origin_rewrite(&env);
    let shim_dir = write_gh_pr_head_shim(&env, gh_fork_head());

    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim_dir))
        .assert()
        .success()
        .stdout(contains("branch : feature"))
        .stdout(contains(
            "pushes : https://github.com/alice/fork.git refs/heads/feature",
        ));

    let path = env.home_root.join("project-worktrees/pr-1");
    assert_eq!(
        git_stdout(&path, &["config", "--get", "branch.feature.remote"]),
        "https://github.com/alice/fork.git"
    );
    assert_eq!(
        git_stdout(&path, &["config", "--get", "branch.feature.merge"]),
        "refs/heads/feature"
    );
    assert_eq!(
        git_stdout(&env.project_root, &["for-each-ref", "refs/rimz/"]),
        "",
        "temporary PR ref is cleaned up"
    );
    commit_file(&path, "fork-push.txt", "pushed\n", "push fork PR worktree");
    git(&path, &["push"]);
    assert_eq!(
        git_stdout(
            &env.home_root.join("origin.git"),
            &["rev-parse", "refs/heads/feature"]
        ),
        git_stdout(&path, &["rev-parse", "HEAD"])
    );
}

#[cfg(unix)]
#[test]
fn worktree_new_from_fork_pr_tracks_fork_from_tea() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let (pr_head, _) = publish_pr_ref(&env, "refs/pull/1/head");
    configure_gitea_origin_rewrite(&env);
    let shim_dir = write_tea_pr_head_shim(&env, tea_fork_head());

    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim_dir))
        .assert()
        .success()
        .stdout(contains("branch : feature"))
        .stdout(contains(
            "pushes : https://gitea.example.test/alice/fork.git refs/heads/feature",
        ));

    let path = env.home_root.join("project-worktrees/pr-1");
    assert_eq!(git_stdout(&path, &["rev-parse", "HEAD"]), pr_head);
    assert_eq!(
        git_stdout(&path, &["config", "--get", "branch.feature.remote"]),
        "https://gitea.example.test/alice/fork.git"
    );
    assert_eq!(
        git_stdout(&path, &["config", "--get", "branch.feature.merge"]),
        "refs/heads/feature"
    );
    assert_eq!(
        git_stdout(&env.project_root, &["for-each-ref", "refs/rimz/"]),
        "",
        "temporary PR ref is cleaned up"
    );
    commit_file(&path, "fork-push.txt", "pushed\n", "push fork PR worktree");
    git(&path, &["push"]);
    assert_eq!(
        git_stdout(
            &env.home_root.join("origin.git"),
            &["rev-parse", "refs/heads/feature"]
        ),
        git_stdout(&path, &["rev-parse", "HEAD"])
    );
}

#[cfg(unix)]
#[test]
fn worktree_new_from_fork_pr_prefixes_local_branch_collision() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    publish_pr_ref_without_branch(&env, "refs/pull/1/head");
    configure_github_origin_rewrite(&env);
    git(&env.project_root, &["branch", "feature", "main"]);
    let shim_dir = write_gh_pr_head_shim(&env, gh_fork_head());

    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim_dir))
        .assert()
        .success()
        .stdout(contains("branch : alice/feature"))
        .stdout(contains(
            "push   : git push https://github.com/alice/fork.git HEAD:feature",
        ));

    let path = env.home_root.join("project-worktrees/pr-1");
    assert_eq!(
        git_stdout(&path, &["config", "--get", "branch.alice/feature.merge"]),
        "refs/heads/feature"
    );
}

#[test]
fn worktree_new_seeds_files_from_worktreeinclude() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);

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
    env.record(&env.project_root);

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
    env.record(&env.project_root);
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
        rimz::worktree::inspect(
            &path,
            &marker,
            &rimz::worktree::ProtectionSet::default(),
            &rimz::worktree::LandedMemo::load(&path),
        )
        .expect("inspect")
        .removable(),
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
    env.record(&env.project_root);
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
        .stderr(contains(
            "has uncommitted changes (1 files); commit or stash them",
        ));

    env.rimz()
        .args(["worktree", "remove", "demo", "--force"])
        .assert()
        .success()
        .stdout(contains(format!(
            "restore with `git branch demo {}`",
            git_stdout(&env.project_root, &["rev-parse", "HEAD"])
        )));
    assert!(!path.exists(), "force removes dirty worktree");
}

#[test]
fn worktree_new_with_at_base_keeps_pending_commits() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
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
        rimz::worktree::inspect(
            &path,
            &marker,
            &rimz::worktree::ProtectionSet::default(),
            &rimz::worktree::LandedMemo::load(&path)
        )
        .expect("status")
        .status
        .landed,
        rimz::worktree::LandedVerdict::Unlanded,
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
fn agents_exec_clean_exit_leaves_clean_worktree_until_gc() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    let mut child = spawn_agent_exec_once(&env, &path, "clean");

    wait_for_ready(
        &mut child,
        &env.home_root.join("clean.ready"),
        &env.home_root.join("clean.pid"),
    );
    let _status = wait_for_exit(&mut child, &env.home_root.join("clean.pid"));

    assert!(
        path.exists(),
        "clean quit leaves worktree for shell inspection"
    );
    assert!(
        branch_exists(&env.project_root, "demo"),
        "clean quit keeps worktree branch"
    );

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("worktrees"))
        .stdout(contains("1 removed"))
        .stdout(contains("removed: demo"));

    wait_for_path_absent(&path, "clean worktree removed by gc");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "worktree branch deleted by gc"
    );
    assert!(
        !git_stdout(&env.project_root, &["worktree", "list", "--porcelain"])
            .contains(&path.display().to_string()),
        "git worktree list forgets the gc-removed worktree"
    );
}

#[cfg(unix)]
#[test]
fn agents_exec_sighup_keeps_worktree_with_inflight_relaunch() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    seed_agent_launch(
        &env,
        &path,
        "launch_inflight",
        "inflight",
        AgentLaunchState::Starting,
    );
    let mut child = spawn_agent_exec(&env, &path, "inflight");

    wait_for_ready(
        &mut child,
        &env.home_root.join("inflight.ready"),
        &env.home_root.join("inflight.pid"),
    );
    signal_child(&child, nix::sys::signal::Signal::SIGHUP);
    let _status = wait_for_exit(&mut child, &env.home_root.join("inflight.pid"));

    assert!(path.exists(), "in-flight relaunch pins worktree cleanup");
    assert!(
        branch_exists(&env.project_root, "demo"),
        "in-flight relaunch pins worktree branch"
    );
}

#[cfg(unix)]
#[test]
fn agents_exec_missing_worktree_path_fails_launch_without_spawning() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    let missing = env.home_root.join("project-worktrees").join("missing");
    let store = env.store();
    let record = rimz::store::run::RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        rimz::agents::PermissionMode::Auto,
        "missing".to_owned(),
        missing.clone(),
    );
    let run_id = record.run_id.clone();
    rimz::harness::run::create(store.paths(), &record).expect("create run");
    seed_agent_launch(
        &env,
        &missing,
        "launch_missing",
        "missing-agent",
        AgentLaunchState::Starting,
    );
    let shim_dir = write_codex_spawn_marker_shim(&env);
    let ready = env.home_root.join("missing.ready");

    let mut request = worktree_exec_request(&missing);
    request.run_id = Some(run_id.clone());
    request.identity.name = Some("missing-agent".to_owned());
    request.identity.launch_id = Some("launch_missing".to_owned());
    env.rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_READY", &ready)
        .assert()
        .failure()
        .stderr(contains("refusing to launch the agent in the project root"));

    assert!(!ready.exists(), "agent shim was not spawned");
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "launch_missing")
        .expect("failed launch remains in roster");
    assert_eq!(agent.status, rimz::agents::AgentStatus::Failed);
    let run = rimz::harness::run::load(store.paths(), &run_id).expect("load run");
    assert_eq!(run.status, rimz::store::run::RunStatus::Failed);
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
            rimz::worktree::inspect(
                path,
                &marker,
                &rimz::worktree::ProtectionSet::default(),
                &rimz::worktree::LandedMemo::load(path)
            )
            .expect("status")
            .status
            .landed,
            rimz::worktree::LandedVerdict::Unlanded,
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
    env.record(&env.project_root);
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
    env.record(&env.project_root);
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
        rimz::worktree::inspect(
            &path,
            &marker,
            &rimz::worktree::ProtectionSet::default(),
            &rimz::worktree::LandedMemo::load(&path)
        )
        .expect("status")
        .status
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
fn worktree_status_rebase_landed_with_shifted_context_is_landed() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    if !git_succeeds(
        &env.project_root,
        &["merge-tree", "--write-tree", "HEAD", "HEAD"],
    ) {
        crate::common::skip("git lacks merge-tree --write-tree");
        return;
    }
    commit_file(
        &env.project_root,
        "f.txt",
        "l1\nl2\nl3\nl4\nl5\nl6\nl7\n",
        "seed context",
    );
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");

    commit_file(
        &path,
        "f.txt",
        "l1\nl2\nl3\nl4-feature\nl5\nl6\nl7\n",
        "feature",
    );
    commit_file(
        &env.project_root,
        "f.txt",
        "l1\nl2\nl3\nl4\nl5\nl6-trunk\nl7\n",
        "trunk context",
    );
    commit_file(
        &env.project_root,
        "f.txt",
        "l1\nl2\nl3\nl4-feature\nl5\nl6-trunk\nl7\n",
        "rebased feature",
    );
    assert_ne!(
        git_stdout(&env.project_root, &["rev-parse", "main^{tree}"]),
        git_stdout(&path, &["rev-parse", "HEAD^{tree}"]),
        "trunk has an extra context-line edit, so the fixture reaches merge absorption"
    );
    assert_ne!(
        git_stdout(&path, &["rev-list", "--count", "main..HEAD"]),
        "0",
        "rebased landing has different commit IDs and remains ahead by ancestry"
    );
    assert!(
        !git_stdout(
            &path,
            &[
                "log",
                "--right-only",
                "--cherry-pick",
                "--no-merges",
                "--format=%H",
                "main...HEAD",
            ],
        )
        .is_empty(),
        "patch-id sees residue after context drift, so merge absorption proves landing"
    );
    assert_eq!(
        rimz::worktree::inspect(
            &path,
            &marker,
            &rimz::worktree::ProtectionSet::default(),
            &rimz::worktree::LandedMemo::load(&path)
        )
        .expect("status")
        .status
        .landed,
        rimz::worktree::LandedVerdict::Landed,
        "rebase-landed content with shifted patch context is landed"
    );
}

#[test]
fn gc_sweeps_merged_worktree() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
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
        .stdout(contains("worktrees"))
        .stdout(contains("1 removed"))
        .stdout(contains("removed: demo"));

    assert!(!path.exists(), "gc swept merged worktree");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "gc deleted merged branch"
    );
}

#[test]
fn gc_sweeps_rewritten_worktree_whose_tip_tree_landed() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    if !git_succeeds(
        &env.project_root,
        &["merge-tree", "--write-tree", "HEAD", "HEAD"],
    ) {
        crate::common::skip("git lacks merge-tree --write-tree");
        return;
    }
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");

    commit_file(
        &path,
        "builder.txt",
        "builder.private.example\n",
        "record private builder",
    );
    git(&path, &["rm", "builder.txt"]);
    git(&path, &["commit", "-m", "remove private builder"]);
    commit_file(&path, "feature.txt", "feature\n", "finish feature");
    let branch_tree = git_stdout(&path, &["rev-parse", "HEAD^{tree}"]);

    commit_file(
        &env.project_root,
        "builder.txt",
        "builder.redacted.example\n",
        "record redacted builder",
    );
    git(&env.project_root, &["rm", "builder.txt"]);
    git(
        &env.project_root,
        &["commit", "-m", "remove redacted builder"],
    );
    commit_file(
        &env.project_root,
        "feature.txt",
        "feature\n",
        "land rewritten feature",
    );
    let landing_commit = git_stdout(&env.project_root, &["rev-parse", "HEAD"]);
    assert_eq!(
        git_stdout(&env.project_root, &["rev-parse", "HEAD^{tree}"]),
        branch_tree,
        "rewritten history reaches the complete worktree tip tree"
    );

    commit_file(
        &env.project_root,
        "feature.txt",
        "feature advanced\n",
        "advance feature",
    );
    for index in 0..501 {
        let message = format!("advance main {index}");
        git(
            &env.project_root,
            &["commit", "--allow-empty", "-m", message.as_str()],
        );
    }

    assert_ne!(
        git_stdout(&env.project_root, &["rev-parse", "main^{tree}"]),
        branch_tree,
        "main advances beyond the previously landed worktree tree"
    );
    assert!(
        !git_succeeds(&path, &["merge-tree", "--write-tree", "main", "HEAD"]),
        "the later feature edit prevents merge absorption from proving landing"
    );
    let residue = git_stdout(
        &path,
        &[
            "log",
            "--right-only",
            "--cherry-pick",
            "--no-merges",
            "--format=%H",
            "main...HEAD",
        ],
    );
    assert_eq!(
        residue.lines().count(),
        2,
        "the rewritten transient commits remain as branch-side patch residue"
    );
    assert!(
        git_stdout(&path, &["log", "--format=%T", "HEAD..main"])
            .lines()
            .any(|tree| tree == branch_tree),
        "the exact worktree tip tree occurs in main's exclusive history"
    );
    assert!(
        git_stdout(
            &env.project_root,
            &["rev-list", "--count", &format!("{landing_commit}..main")],
        )
        .parse::<u32>()
        .expect("commit count")
            > 500,
        "the matching snapshot sits beyond the capped merge-tree scan"
    );
    assert_eq!(
        rimz::worktree::inspect(
            &path,
            &marker,
            &rimz::worktree::ProtectionSet::default(),
            &rimz::worktree::LandedMemo::load(&path)
        )
        .expect("status")
        .status
        .landed,
        rimz::worktree::LandedVerdict::Landed,
        "the complete branch-tip snapshot proves rewritten work landed"
    );

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("worktrees"))
        .stdout(contains("1 removed"))
        .stdout(contains("removed: demo"));

    assert!(!path.exists(), "gc swept rewritten worktree");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "gc force-deleted rewritten content-landed branch"
    );
}

#[test]
fn gc_keeps_unlanded_worktree_whose_tip_tree_is_only_shared_history() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    if !git_succeeds(
        &env.project_root,
        &["merge-tree", "--write-tree", "HEAD", "HEAD"],
    ) {
        crate::common::skip("git lacks merge-tree --write-tree");
        return;
    }
    let shared_history_tree = git_stdout(&env.project_root, &["rev-parse", "HEAD^{tree}"]);
    commit_file(
        &env.project_root,
        "shared.txt",
        "shared\n",
        "establish branch point",
    );
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");

    commit_file(
        &path,
        "shared.txt",
        "branch scratch\n",
        "change shared file on branch",
    );
    git(&path, &["rm", "shared.txt"]);
    git(&path, &["commit", "-m", "remove shared file on branch"]);
    let branch_tree = git_stdout(&path, &["rev-parse", "HEAD^{tree}"]);
    assert_eq!(
        branch_tree, shared_history_tree,
        "the branch tip repeats a tree from shared pre-divergence history"
    );

    commit_file(
        &env.project_root,
        "shared.txt",
        "main content\n",
        "change shared file on main",
    );
    let comparison_tree = git_stdout(&env.project_root, &["rev-parse", "main^{tree}"]);
    assert_ne!(
        branch_tree, comparison_tree,
        "the current main tree must not match the branch tip"
    );
    assert!(
        !git_succeeds(&path, &["merge-tree", "--write-tree", "main", "HEAD"]),
        "the modify-delete conflict prevents merge absorption"
    );
    assert_eq!(
        git_stdout(
            &path,
            &[
                "log",
                "--right-only",
                "--cherry-pick",
                "--no-merges",
                "--format=%H",
                "main...HEAD",
            ],
        )
        .lines()
        .count(),
        2,
        "the branch retains unlanded patch residue"
    );
    assert!(
        git_stdout(&path, &["log", "--format=%T", "main"])
            .lines()
            .any(|tree| tree == branch_tree),
        "an unrestricted main scan would find the shared historical tree"
    );
    let exclusive_trees = git_stdout(&path, &["log", "--format=%T", "HEAD..main"]);
    assert!(
        exclusive_trees.lines().any(|tree| tree == comparison_tree),
        "main's tip tree is present but cannot stand in for the branch tip"
    );
    assert!(
        exclusive_trees.lines().all(|tree| tree != branch_tree),
        "the shared historical tree is absent from main's exclusive history"
    );
    assert_eq!(
        rimz::worktree::inspect(
            &path,
            &marker,
            &rimz::worktree::ProtectionSet::default(),
            &rimz::worktree::LandedMemo::load(&path)
        )
        .expect("status")
        .status
        .landed,
        rimz::worktree::LandedVerdict::Unlanded,
        "shared pre-divergence history cannot prove branch content landed"
    );

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("kept: demo — unlanded"));

    assert!(path.exists(), "gc keeps the unlanded worktree");
    assert!(
        branch_exists(&env.project_root, "demo"),
        "gc keeps the unlanded branch"
    );
}

#[test]
fn gc_sweeps_merge_landed_worktree() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
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
        rimz::worktree::inspect(
            &path,
            &marker,
            &rimz::worktree::ProtectionSet::default(),
            &rimz::worktree::LandedMemo::load(&path)
        )
        .expect("status")
        .status
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
        .stdout(contains("worktrees"))
        .stdout(contains("1 removed"))
        .stdout(contains("removed: demo"));

    assert!(!path.exists(), "gc swept merge-landed worktree");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "gc force-deleted content-landed branch"
    );
}

#[test]
fn gc_sweeps_worktree_whose_base_branch_landed_on_trunk() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    git(&env.project_root, &["branch", "feature"]);
    git(&env.project_root, &["checkout", "feature"]);
    commit_file(&env.project_root, "base.txt", "base\n", "base");
    let feature_tip = git_stdout(&env.project_root, &["rev-parse", "HEAD"]);
    git(&env.project_root, &["checkout", "main"]);
    commit_file(&env.project_root, "trunk.txt", "trunk\n", "trunk");

    env.rimz()
        .args(["worktree", "new", "demo", "--base", "feature"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees").join("demo");
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .expect("read marker")
        .expect("marker");
    assert_eq!(marker.base_branch.as_deref(), Some("feature"));
    assert_eq!(marker.base_ref, feature_tip);
    commit_file(&path, "work.txt", "work\n", "work");
    let demo_tip = git_stdout(&path, &["rev-parse", "HEAD"]);

    git(&env.project_root, &["cherry-pick", feature_tip.as_str()]);
    git(&env.project_root, &["cherry-pick", demo_tip.as_str()]);
    commit_file(&env.project_root, "extra.txt", "extra\n", "extra");
    assert!(
        !git_succeeds(
            &env.project_root,
            &["merge-base", "--is-ancestor", "feature", "main"]
        ),
        "feature landed by patch, not by ancestry"
    );
    assert_ne!(
        git_stdout(&path, &["rev-list", "--count", "main..HEAD"]),
        "0",
        "the fixture remains ahead by ancestry"
    );
    assert_eq!(
        rimz::worktree::inspect(
            &path,
            &marker,
            &rimz::worktree::ProtectionSet::default(),
            &rimz::worktree::LandedMemo::load(&path)
        )
        .expect("status")
        .status
        .landed,
        rimz::worktree::LandedVerdict::Landed,
        "worktree content is landed on trunk once its stale base branch is superseded"
    );

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("worktrees"))
        .stdout(contains("1 removed"))
        .stdout(contains("removed: demo"));

    assert!(!path.exists(), "gc swept stale-base worktree");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "gc force-deleted content-landed branch"
    );
    assert!(
        branch_exists(&env.project_root, "feature"),
        "superseded base branch remains"
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
    env.record(&env.project_root);
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
        rimz::worktree::inspect(
            &path,
            &marker,
            &rimz::worktree::ProtectionSet::default(),
            &rimz::worktree::LandedMemo::load(&path)
        )
        .expect("status")
        .status
        .landed,
        rimz::worktree::LandedVerdict::Landed,
        "feature is landed on explicit base even though main lacks it"
    );

    let mut child = spawn_agent_exec_once(&env, &path, "explicit-base");
    wait_for_ready(
        &mut child,
        &env.home_root.join("explicit-base.ready"),
        &env.home_root.join("explicit-base.pid"),
    );
    let _status = wait_for_exit(&mut child, &env.home_root.join("explicit-base.pid"));

    assert!(
        path.exists(),
        "explicit-base clean quit leaves worktree for shell inspection"
    );
    assert!(
        branch_exists(&env.project_root, "demo"),
        "explicit-base clean quit keeps worktree branch"
    );

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("worktrees"))
        .stdout(contains("1 removed"))
        .stdout(contains("removed: demo"));

    wait_for_path_absent(&path, "explicit-base worktree removed by gc");
    assert!(
        !branch_exists(&env.project_root, "demo"),
        "gc deletes branch after proving it landed on develop"
    );
    assert!(
        branch_exists(&env.project_root, "develop"),
        "base branch remains"
    );
}

fn queue_channel_message(env: &Env, channel: &str, text: &str) -> rimz::MessageId {
    let session_id = AgentSessionId::from(format!("sess-{channel}"));
    let mut observation =
        AgentLifecycleObservation::new(Some(session_id.clone()), LifecycleSignal::Registered);
    observation.worktree_branch = Some(channel.to_owned());
    let event = EventEnvelope::agent_lifecycle(
        env.workspace_id.clone(),
        "rimz-test",
        "claude",
        "SessionStart",
        &observation,
    );
    env.store().append_event(&event).expect("append agent");
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id == session_id)
        .expect("agent");
    let message = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        text.to_owned(),
        DeliveryGate::Done,
    )
    .with_channel(Some(channel.to_owned()));
    let message_id = message.message_id.clone();
    env.store()
        .queue_message(&message, "rimz-test")
        .expect("queue message");
    message_id
}

fn assert_messages_archived_without_history(env: &Env) {
    assert!(
        env.store()
            .list_messages()
            .expect("live messages")
            .is_empty()
    );
    assert!(
        env.read_events()
            .iter()
            .any(|event| event.method == "message.archived")
    );
}

fn block_message_history(env: &Env) {
    let history = env.state_path_for(&env.project_root).message_history_dir;
    std::fs::create_dir_all(history.parent().unwrap()).unwrap();
    std::fs::write(history, b"blocked").expect("replace message history directory with file");
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

#[cfg(unix)]
fn spawn_agent_exec(env: &Env, worktree: &Path, label: &str) -> Child {
    spawn_agent_exec_command(env, env.rimz(), worktree, worktree, label)
}

#[cfg(unix)]
fn spawn_agent_exec_once(env: &Env, worktree: &Path, label: &str) -> Child {
    let shim_dir = write_codex_spawn_marker_shim(env);
    let ready = env.home_root.join(format!("{label}.ready"));
    let mut cmd = env.rimz();
    cmd.args(exec_args(env, &worktree_exec_request(worktree)))
        .current_dir(worktree)
        .env("SHELL", "/definitely/not/a/shell")
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_READY", &ready)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn().expect("spawn one-shot agents exec")
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
    cmd.args(exec_args(env, &worktree_exec_request(worktree_arg)))
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
fn seed_agent_launch(
    env: &Env,
    worktree: &Path,
    launch_id: &str,
    agent_name: &str,
    state: AgentLaunchState,
) {
    let workspace = env.resolve_workspace(&env.project_root);
    let kind = AgentKind::new_unchecked("codex");
    let event = EventEnvelope::agent_launched(
        workspace.workspace_id,
        workspace.session_name,
        &kind,
        AgentLaunchPayload {
            system_prompt_digest: None,
            agent_id: AgentSessionId::from(launch_id),
            launch_id: None,
            agent_name: agent_name.to_owned(),
            agent_name_explicit: false,
            launch: LaunchParams {
                profile: None,
                mode: None,

                role: None,

                model: None,

                effort: None,

                budget: None,

                team: None,

                launch_group: None,

                launch_ordinal: None,

                channel: None,

                kind_ordinal: None,
                ..LaunchParams::default()
            },
            state,
            run_id: None,
            pane_id: None,
            runtime_owner: None,
            worktree_path: Some(worktree.display().to_string()),
            worktree_branch: worktree
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned),
            prompt: None,
            description: None,
        },
    );
    env.store().append_event(&event).expect("append launch");
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
fn write_codex_spawn_marker_shim(env: &Env) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let dir = env.home_root.join("agent-bin-marker");
    std::fs::create_dir_all(&dir).expect("mkdir agent marker bin");
    let shim = dir.join("codex");
    std::fs::write(
        &shim,
        "#!/bin/sh\n\
         : > \"$RIMZ_TEST_AGENT_READY\"\n",
    )
    .expect("write codex marker shim");
    let mut perms = std::fs::metadata(&shim)
        .expect("shim metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms).expect("chmod codex marker shim");
    dir
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
    // Signal-driven exits run the worktree cleanup inspection (git status,
    // marker reads) first; a generous ceiling only bites on a real hang.
    while start.elapsed() < Duration::from_secs(30) {
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
fn wait_for_path_absent(path: &Path, message: &str) {
    let start = Instant::now();
    let timeout = ready_timeout();
    while start.elapsed() < timeout {
        if !path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("{message}: {}", path.display());
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

#[test]
fn entry_read_combines_git_history_activity_and_takeover_occupancy() {
    use jiff::Timestamp;
    use rimz::harness::worktree_entry;
    use rimz::ids::{MuxName, PaneId};
    use rimz::pane::{PaneRef, RuntimeOwner, RuntimeOwnerKind};

    let env = Env::new();
    init_repo(&env.project_root);
    env.record(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let path = env.home_root.join("project-worktrees/demo");
    let marker = rimz::worktree::read_marker_for_worktree(&path)
        .unwrap()
        .unwrap();
    let mut row = rimz::testkit::agent_state(
        "claude",
        "entry-agent",
        Timestamp::from_second(100).unwrap(),
    );
    row.worktree_path = Some(path.join("src").display().to_string());
    row.pane = Some(PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, "%3")));
    let mut ended = row.clone();
    ended.ended_at = Some(Timestamp::from_second(200).unwrap());
    ended.last_activity = Timestamp::from_second(200).unwrap();
    let mut outside = row.clone();
    outside.worktree_path = Some(env.project_root.display().to_string());
    outside.last_activity = Timestamp::from_second(300).unwrap();
    let mut dead = row.clone();
    dead.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "dead",
        u32::MAX,
        None,
    ));
    let cases = [
        vec![],
        vec![ended.clone()],
        vec![outside.clone()],
        vec![dead],
        vec![row.clone()],
    ];
    let occupied =
        cases.map(|alive| worktree_entry::occupied(&path, &env.project_root, &alive).unwrap());
    let facts = [vec![ended.clone(), outside.clone()], vec![]].map(|audit| {
        let entry = worktree_entry::read(&path, &marker, &audit).unwrap();
        (
            entry.last_activity,
            entry.created_at,
            entry.status.changed_files,
            entry.removable,
        )
    });
    let expected_facts =
        [Some(ended.last_activity), None].map(|activity| (activity, marker.created_at, 0, true));
    let nested = rimz::worktree::create(
        &env.project_root,
        &rimz::config::WorktreeConfig {
            dir: path.join(".nested").display().to_string(),
            ..Default::default()
        },
        Some("nested"),
        None,
        None,
        false,
        rimz::worktree::LaunchBase::Configured,
    )
    .unwrap();
    row.worktree_path = Some(nested.marker.worktree_path.display().to_string());
    let nested_occupied = worktree_entry::occupied(&path, &env.project_root, &[row]).unwrap();
    assert_eq!(
        (occupied, facts, nested_occupied),
        ([false, false, false, false, true], expected_facts, false)
    );
}
