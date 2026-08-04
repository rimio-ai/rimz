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
use rimz::EventEnvelope;
use rimz::agents::{AgentLifecycleObservation, LaunchParams, LifecycleSignal};
#[cfg(unix)]
use rimz::harness::launch::{ExecAction, ExecIdentity, ExecRequest, ProviderAccountState};
#[cfg(unix)]
use rimz::ids::AgentKind;
use rimz::ids::AgentSessionId;
use rimz::message::{DeliveryGate, MessageRecord};
#[cfg(unix)]
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState};
use serde_json::Value;

use crate::common::Env;
#[cfg(unix)]
use crate::common::exec_args;

#[cfg(unix)]
fn worktree_exec_request(worktree: &Path) -> ExecRequest {
    ExecRequest {
        kind: AgentKind::new_unchecked("codex"),
        action: ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        provider_account: ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: Some(worktree.to_path_buf()),
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: false,
        identity: ExecIdentity::default(),
    }
}

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
    assert_eq!(
        parsed[0],
        serde_json::json!({
            "name": "demo",
            "path": path,
            "branch": "demo",
            "base_ref": marker.base_ref,
            "dirty": false,
            "landed": true,
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
fn worktree_sweep_previews_then_removes_only_safe_checkouts() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "landed"])
        .assert()
        .success();
    env.rimz()
        .args(["worktree", "new", "pending"])
        .assert()
        .success();
    let landed = env.home_root.join("project-worktrees/landed");
    let pending = env.home_root.join("project-worktrees/pending");
    commit_file(&pending, "feature.txt", "pending\n", "pending work");

    env.rimz()
        .args(["worktree", "sweep", "--dry-run"])
        .assert()
        .success()
        .stdout(contains("would remove 1"))
        .stdout(contains("kept: pending — not merged yet"));
    assert!(landed.exists(), "dry run keeps landed checkout");

    env.rimz()
        .args(["worktree", "sweep"])
        .assert()
        .success()
        .stdout(contains("removed 1"))
        .stdout(contains("kept: pending — not merged yet"));
    assert!(!landed.exists(), "safe checkout is swept");
    assert!(pending.exists(), "unmerged checkout is retained");
}

#[test]
fn worktree_sweep_dry_run_skips_an_absent_store_without_failing() {
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
        .success()
        .stdout(contains("sweep — nothing to remove · 0 kept"));
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

    env.rimz()
        .env("SHELL", &shell)
        .env("RIMZ_TEST_CD_PWD", &observed)
        .args(["worktree", "cd", "demo"])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(observed)
            .expect("observed cwd")
            .trim(),
        worktree.display().to_string()
    );
}

#[test]
fn worktree_merge_rebases_then_fast_forwards_main() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let worktree = env.home_root.join("project-worktrees/demo");
    commit_file(&worktree, "feature.txt", "feature\n", "feature");
    commit_file(&env.project_root, "main.txt", "main\n", "advance main");
    let main_before = git_stdout(&env.project_root, &["rev-parse", "HEAD"]);

    env.rimz()
        .args(["worktree", "merge", "demo"])
        .assert()
        .success()
        .stdout(contains("merged demo into main"));

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
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let worktree = env.home_root.join("project-worktrees/demo");
    std::fs::write(worktree.join("dirty.txt"), "dirty\n").expect("dirty worktree");

    env.rimz()
        .args(["worktree", "merge", "demo"])
        .assert()
        .failure()
        .stderr(contains("its checkout has local changes"));

    std::fs::remove_file(worktree.join("dirty.txt")).expect("clean worktree");
    std::fs::write(env.project_root.join("dirty.txt"), "dirty\n").expect("dirty main");
    env.rimz()
        .args(["worktree", "merge", "demo"])
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
        .args(["worktree", "merge", "demo"])
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
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    std::fs::write(env.project_root.join(".git/MERGE_HEAD"), "pending\n")
        .expect("mark merge in progress");

    env.rimz()
        .args(["worktree", "merge", "demo"])
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
        .args(["worktree", "merge", "demo"])
        .assert()
        .failure()
        .stderr(contains("rebase onto main failed"))
        .stderr(contains("no commits were merged into main"));

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
fn worktree_new_refuses_named_channel_conflict() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);

    env.rimz()
        .args(["channel", "new", "demo"])
        .assert()
        .success();

    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .failure()
        .stderr(contains("channel `demo` is a named channel"));
}

#[test]
fn worktree_new_checks_dashed_channel_for_branch_style_name() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);

    env.rimz()
        .args(["channel", "new", "feat-great"])
        .assert()
        .success();

    env.rimz()
        .args(["worktree", "new", "feat/great"])
        .assert()
        .failure()
        .stderr(contains("channel `feat-great` is a named channel"));
}

#[test]
fn worktree_new_archives_messages_for_recreated_channel() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
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

    env.rimz()
        .args(["worktree", "remove", "demo", "--force"])
        .assert()
        .success()
        .stderr(contains("warning: worktree `demo` is in use by"));
    assert!(!path.exists(), "--force removes it anyway");
}

#[test]
fn worktree_remove_reports_archive_failure_after_removal() {
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
    queue_channel_message(&env, "demo", "old work");
    block_message_archive(&env);

    env.rimz()
        .args(["worktree", "remove", "demo"])
        .assert()
        .failure()
        .stderr(contains("archiving messages for removed worktree channel"));

    assert!(!path.exists(), "removal completes before archive failure");
}

#[cfg(unix)]
#[test]
fn worktree_cleanup_retires_sessions_and_archives_messages_after_removal() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
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
fn worktree_cleanup_downgrades_archive_failure_after_removal() {
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
    queue_channel_message(&env, "demo", "old work");
    block_message_archive(&env);

    env.rimz()
        .args([
            "worktree",
            "cleanup",
            path.to_str().expect("utf-8 path"),
            "--non-interactive",
        ])
        .assert()
        .success();

    assert!(!path.exists(), "cleanup survives archive failure");
}

#[test]
fn worktree_gc_reports_archive_failure_after_removal() {
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
    queue_channel_message(&env, "demo", "old work");
    block_message_archive(&env);

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("removed: demo"))
        .stdout(contains("message archive failed"));

    assert!(
        !path.exists(),
        "gc removal completes before archive failure"
    );
}

#[cfg(unix)]
#[test]
fn worktree_new_from_pr_fetches_github_style_ref() {
    if git_missing() {
        return;
    }
    let env = Env::new();
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
fn worktree_new_from_pr_fast_forwards_ancestor_local_branch() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    let (pr_head, trunk) = publish_pr_ref(&env, "refs/pull/1/head");
    git(&env.project_root, &["branch", "feature", trunk.as_str()]);
    configure_github_origin_rewrite(&env);
    let shim_dir = write_gh_pr_head_shim(&env, gh_same_repo_head());

    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim_dir))
        .assert()
        .success();

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
    publish_pr_ref(&env, "refs/pull/1/head");
    git(&env.project_root, &["checkout", "-b", "feature"]);
    commit_file(&env.project_root, "local.txt", "local\n", "diverge locally");
    git(&env.project_root, &["checkout", "main"]);
    configure_github_origin_rewrite(&env);
    let shim_dir = write_gh_pr_head_shim(&env, gh_same_repo_head());

    env.rimz()
        .args(["worktree", "new", "--from-pr", "1"])
        .env("PATH", path_with_front(&shim_dir))
        .assert()
        .failure()
        .stderr(contains("local PR branch `feature` conflicts"))
        .stderr(contains("diverged"));
}

#[cfg(unix)]
#[test]
fn worktree_new_from_pr_refuses_branch_checked_out_elsewhere() {
    if git_missing() {
        return;
    }
    let env = Env::new();
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
        .stderr(contains("checked out at"));
}

#[test]
fn worktree_new_from_pr_without_forge_cli_is_review_only() {
    if git_missing() {
        return;
    }
    let env = Env::new();
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
fn worktree_new_from_fork_pr_prefixes_local_branch_collision() {
    if git_missing() {
        return;
    }
    let env = Env::new();
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
    assert_eq!(
        rimz::worktree::ProtectionSet::default().assess(
            &path,
            rimz::worktree::status(&path, &marker).expect("status"),
        ),
        rimz::worktree::RemovalAssessment::Removable,
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
fn agents_exec_clean_exit_leaves_clean_worktree_until_gc() {
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
    let missing = env.home_root.join("project-worktrees").join("missing");
    let store = env.store();
    let record = rimz::harness::run::RunRecord::new(
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
        .args(exec_args(&request))
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
    assert_eq!(run.status, rimz::harness::run::RunStatus::Failed);
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
fn worktree_status_rebase_landed_with_shifted_context_is_landed() {
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    if !git_succeeds(
        &env.project_root,
        &["merge-tree", "--write-tree", "HEAD", "HEAD"],
    ) {
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
        rimz::worktree::status(&path, &marker)
            .expect("status")
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
    if !git_succeeds(
        &env.project_root,
        &["merge-tree", "--write-tree", "HEAD", "HEAD"],
    ) {
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
        rimz::worktree::status(&path, &marker)
            .expect("status")
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
    if !git_succeeds(
        &env.project_root,
        &["merge-tree", "--write-tree", "HEAD", "HEAD"],
    ) {
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
        rimz::worktree::status(&path, &marker)
            .expect("status")
            .landed,
        rimz::worktree::LandedVerdict::Pending,
        "shared pre-divergence history cannot prove branch content landed"
    );

    env.rimz()
        .args(["gc", "--older-than", "1h"])
        .assert()
        .success()
        .stdout(contains("kept: demo — not merged yet"));

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
        rimz::worktree::status(&path, &marker)
            .expect("status")
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

fn git_missing() -> bool {
    Command::new("git").arg("--version").output().is_err()
}

fn init_repo(path: &Path) {
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.email", "rimz@example.com"]);
    git(path, &["config", "user.name", "RimZ Test"]);
    commit_file(path, "README.md", "fixture\n", "initial");
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
        true,
        DeliveryGate::Done,
    )
    .with_channel(Some(channel.to_owned()));
    let message_id = message.message_id.clone();
    env.store()
        .queue_message(&message, "rimz-test")
        .expect("queue message");
    message_id
}

fn block_message_archive(env: &Env) {
    let history = env
        .state_path_for(&env.project_root)
        .messages_dir
        .join("history.jsonl");
    std::fs::create_dir_all(history).expect("replace message history file with directory");
}

fn publish_pr_ref(env: &Env, remote_ref: &str) -> (String, String) {
    publish_pr_ref_inner(env, remote_ref, true)
}

fn publish_pr_ref_without_branch(env: &Env, remote_ref: &str) -> (String, String) {
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
fn configure_github_origin_rewrite(env: &Env) {
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

fn configure_origin_rewrite(env: &Env, origin_url: &str) {
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

#[cfg(unix)]
fn write_gh_pr_head_shim(env: &Env, payload: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let dir = env.home_root.join("forge-bin");
    std::fs::create_dir_all(&dir).expect("mkdir forge bin");
    let shim = dir.join("gh");
    std::fs::write(&shim, format!("#!/bin/sh\nprintf '%s\\n' '{payload}'\n"))
        .expect("write gh shim");
    let mut perms = std::fs::metadata(&shim)
        .expect("gh shim metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms).expect("chmod gh shim");
    dir
}

#[cfg(unix)]
fn gh_same_repo_head() -> &'static str {
    r#"{"headRefName":"feature","headRepository":{"name":"repo"},"headRepositoryOwner":{"login":"org"},"isCrossRepository":false}"#
}

#[cfg(unix)]
fn gh_fork_head() -> &'static str {
    r#"{"headRefName":"feature","headRepository":{"name":"fork"},"headRepositoryOwner":{"login":"alice"},"isCrossRepository":true}"#
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

fn git_succeeds(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git")
        .status
        .success()
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
fn spawn_agent_exec_once(env: &Env, worktree: &Path, label: &str) -> Child {
    let shim_dir = write_codex_spawn_marker_shim(env);
    let ready = env.home_root.join(format!("{label}.ready"));
    let mut cmd = env.rimz();
    cmd.args(exec_args(&worktree_exec_request(worktree)))
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
    cmd.args(exec_args(&worktree_exec_request(worktree_arg)))
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
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    let kind = AgentKind::new_unchecked("codex");
    let event = EventEnvelope::agent_launched(
        workspace.workspace_id,
        workspace.session_name,
        &kind,
        AgentLaunchPayload {
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
