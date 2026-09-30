//! Integration coverage for interactive peer launches and the supervised agent launch shell wrapper.

#[cfg(unix)]
use assert_cmd::assert::OutputAssertExt;
#[cfg(unix)]
use predicates::str::contains;
#[cfg(unix)]
use rimz::agents::{AgentLifecycleObservation, LaunchParams, LifecycleSignal};
#[cfg(unix)]
use rimz::harness::launch::{ExecAction, ExecIdentity, ExecRequest, ProviderAccountState};
#[cfg(unix)]
use rimz::ids::{AgentKind, AgentSessionId};
#[cfg(unix)]
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope, EventKind};

#[cfg(unix)]
use crate::common::{
    CommandTimeoutExt, Env, canonical, exec_args, path_with_front, write_env_dump_shim,
    write_failing_agent_shim, write_fake_bash_shell, write_fake_login_shell,
};

#[cfg(unix)]
fn init_launch_repo(path: &std::path::Path) -> bool {
    std::fs::create_dir_all(path).expect("mkdir repo");
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
    };
    match run(&["init", "-q", "-b", "main"]) {
        Ok(status) if status.success() => {}
        _ => return false,
    }
    assert!(
        run(&["config", "user.email", "rimz@example.test"])
            .expect("git config email")
            .success()
    );
    assert!(
        run(&["config", "user.name", "RimZ Test"])
            .expect("git config name")
            .success()
    );
    std::fs::write(path.join("README.md"), "base\n").expect("base file");
    assert!(run(&["add", "README.md"]).expect("git add").success());
    assert!(
        run(&["commit", "-q", "-m", "base"])
            .expect("git commit")
            .success()
    );
    true
}

#[cfg(unix)]
fn fresh_exec(kind: &str, prompt: Option<&str>) -> ExecRequest {
    ExecRequest {
        isolation_default: None,
        kind: AgentKind::new_unchecked(kind),
        action: ExecAction::Launch {
            prompt: prompt.map(ToOwned::to_owned),
            extra_args: Vec::new(),
        },
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: None,
        allowed_tools: None,
        provider_account: ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: false,
        identity: ExecIdentity::default(),
    }
}

#[test]
fn agent_launch_tab_keeps_focus() {
    assert_launch_focus(&["agents", "claude", "--new-tab"], true, "new-tab");
}

#[test]
fn agent_launch_pane_keeps_focus() {
    assert_launch_focus(&["agents", "claude", "--new-pane"], true, "new-pane");
}

#[test]
fn agent_launch_pane_restores_client_on_zellij_044() {
    assert_launch_focus_version(
        &["agents", "claude", "--new-pane"],
        true,
        "new-pane",
        "0.44.3",
    );
}

#[test]
fn agent_launch_auto_splits_without_focus() {
    assert_launch_focus(&["agents", "claude"], true, "new-pane");
}

#[test]
fn agent_team_launch_keeps_focus() {
    assert_launch_focus(&["teams", "duo"], true, "new-tab");
}

#[test]
fn user_launch_tab_takes_focus() {
    assert_launch_focus(&["agents", "claude", "--new-tab"], false, "new-tab");
}

#[test]
fn agent_restart_keeps_focus_on_another_pane() {
    assert_launch_focus(&["agents", "restart", "@planner"], true, "new-pane");
}

#[test]
fn agent_resume_live_lane_keeps_focus() {
    assert_launch_focus(&["agents", "resume", "#review"], true, "live");
}

#[test]
fn agent_reconcile_live_cohort_keeps_focus() {
    assert_launch_focus(&["teams", "duo", "-w", "review"], true, "cohort");
}

#[test]
fn different_team_live_hold_refuses_launch() {
    assert_launch_focus(&["teams", "solo", "-w", "review"], true, "hold-live");
}

#[test]
fn different_team_live_hold_refuses_in_place_launch() {
    assert_launch_focus(&["teams", "solo"], true, "hold-root");
}

#[test]
fn different_team_board_hold_refuses_launch() {
    assert_launch_focus(&["teams", "solo", "-w", "review"], true, "hold-board");
}

#[test]
fn different_team_done_board_allows_launch() {
    assert_launch_focus(&["teams", "solo", "-w", "review"], true, "hold-done");
}

#[test]
fn held_team_can_resume_before_done() {
    assert_launch_focus(
        &["teams", "resume", "duo", "-w", "review"],
        true,
        "hold-resume",
    );
}

#[test]
fn channel_hold_refuses_live_launch() {
    assert_launch_focus(
        &["teams", "solo", "--channel", "review"],
        true,
        "hold-channel-live",
    );
}

#[test]
fn channel_hold_refuses_pending_launch() {
    assert_launch_focus(
        &["teams", "solo", "--channel", "review"],
        true,
        "hold-channel-board",
    );
}

#[test]
fn channel_hold_refuses_same_team_elsewhere() {
    assert_launch_focus(
        &["teams", "duo", "--channel", "review"],
        true,
        "hold-channel-live",
    );
}

#[test]
fn channel_hold_refuses_derived_channel() {
    assert_launch_focus(
        &["teams", "solo", "-w", "review"],
        true,
        "hold-channel-derived",
    );
}

#[test]
fn channel_hold_allows_done_and_same_checkout() {
    assert_launch_focus(
        &["teams", "solo", "--channel", "review"],
        true,
        "hold-channel-done",
    );
    assert_launch_focus(
        &["teams", "duo", "--channel", "review"],
        true,
        "hold-channel-same",
    );
}

#[test]
fn channel_hold_refuses_inferred_role_channel() {
    assert_launch_focus(
        &["agents", "lead", "--new-tab"],
        true,
        "hold-channel-inferred",
    );
}

#[cfg(unix)]
#[test]
fn channel_hold_allows_symlinked_checkout() {
    assert_launch_focus(
        &["agents", "duo", "--channel", "review"],
        true,
        "hold-channel-symlink",
    );
}

#[test]
fn from_pr_live_cohort_preserves_behind_and_equal_tips() {
    for behind in [true, false] {
        assert_from_pr_launch(Some(LifecycleSignal::Registered), behind, false);
    }
}

#[test]
fn from_pr_live_cohort_matches_holder_through_symlinked_worktree_dir() {
    assert_from_pr_launch(Some(LifecycleSignal::Registered), true, true);
}

#[test]
fn from_pr_launch_fast_forwards_and_reports_reuse() {
    assert_from_pr_launch(None, true, false);
}

#[test]
fn from_pr_closed_cohort_prints_commands_without_moving_tip() {
    assert_from_pr_launch(Some(LifecycleSignal::Ended), true, false);
}

/// `symlinked_dir` routes `agents.worktree.dir` through a symlink, so the
/// marker's lexical holder path and Git's realpath'd listing differ.
fn assert_from_pr_launch(cohort: Option<LifecycleSignal>, behind: bool, symlinked_dir: bool) {
    use crate::common::git::{configure_github_origin_rewrite, git_stdout, publish_pr_ref};

    let env = Env::new();
    let (pr_head, trunk) = publish_pr_ref(&env, "refs/pull/1/head");
    configure_github_origin_rewrite(&env);
    let mut config = "[agents]\nisolation = 'host'\n".to_owned();
    let mut holder = env.home_root.join("project-worktrees/feature");
    if symlinked_dir {
        let real = env.home_root.join("real-worktrees");
        let link = env.home_root.join("linked-worktrees");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        config.push_str(&format!("[agents.worktree]\ndir = '{}'\n", link.display()));
        holder = link.join("feature");
    }
    std::fs::write(env.rimz_home().join("config.toml"), &config).unwrap();
    let tip = if behind { &trunk } else { &pr_head };
    env.rimz()
        .args(["worktree", "new", "feature", "--base", tip])
        .assert()
        .success();
    crate::common::wait::register_calling_agent(&env);
    crate::common::write_definition(
        &env,
        "agents",
        "worker",
        "description: Worker\nagent: claude\ntools: []",
        "",
    );
    crate::common::write_definition(
        &env,
        "teams",
        "duo",
        "layout: lead,worker\nleader: lead\nstages: [Build]\nroles:\n  - {role: lead, agent: worker, owns: [Build]}\n  - {role: worker, agent: worker}",
        "Complete the work.",
    );
    let shim = write_env_dump_shim(&env, "claude");
    crate::common::write_path_shim(
        &shim,
        "gh",
        &format!("printf '%s\\n' '{}'\n", crate::common::gh_same_repo_head()),
    );
    let workspace = env.resolve_workspace(&env.project_root);
    if let Some(signal) = cohort.as_ref() {
        let observation = AgentLifecycleObservation {
            pane_id: Some(rimz::ids::PaneId::from_parts(
                rimz::ids::MuxName::Zellij,
                "terminal_3",
            )),
            runtime_owner: Some(rimz::pane::RuntimeOwner::new(
                rimz::pane::RuntimeOwnerKind::Agent,
                "provider-session",
                std::process::id(),
                None,
            )),
            worktree_path: Some(holder.display().to_string()),
            launch: LaunchParams {
                team: Some("duo".to_owned()),
                ..Default::default()
            },
            ..AgentLifecycleObservation::new(
                Some("provider-session".into()),
                LifecycleSignal::Registered,
            )
        };
        env.store()
            .append_event(&EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                "claude",
                "test",
                &observation,
            ))
            .unwrap();
        if matches!(signal, LifecycleSignal::Ended) {
            env.store()
                .append_event(&EventEnvelope::agent_lifecycle(
                    workspace.workspace_id.clone(),
                    &workspace.session_name,
                    "claude",
                    "rimz.agent-ended",
                    &AgentLifecycleObservation::new(
                        Some("provider-session".into()),
                        LifecycleSignal::Ended,
                    ),
                ))
                .unwrap();
        }
    }
    let before = serde_json::to_value(env.store().snapshot_cached().unwrap().agents).unwrap();
    let log = env.home_root.join("mux.log");
    let args = if cohort.is_some() {
        ["teams", "duo", "--from-pr", "1"]
    } else {
        ["agents", "worker", "--from-pr", "1"]
    };
    let output = env.rimz().args(["--mux", "zellij"]).args(args)
        .env("ZELLIJ_PANE_ID", "1")
        .env("PATH", path_with_front(&shim))
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "launch-session")
        .env("RIMZ_TEST_AGENT_ENV_DUMP", env.home_root.join("agent-env"))
        .env("RIMZ_TEST_ZELLIJ_VERSION", "0.45.1")
        .env("RIMZ_ZELLIJ_BIN", crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")))
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env("RIMZ_TEST_ZELLIJ_LIST_CLIENTS", "1 terminal_3 claude\n")
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"sh"},{"id":3,"is_plugin":false,"tab_id":2,"title":"claude"}]"#)
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", format!("{} [Created 1s ago]\n", workspace.session_name))
        .bounded_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let trace = std::fs::read_to_string(log).unwrap();
    if let Some(signal) = cohort {
        if matches!(signal, LifecycleSignal::Ended) {
            assert!(stderr.contains("rimz worktree remove feature"), "{stderr}");
            assert!(
                stderr.contains("rimz teams duo -w feature --fresh"),
                "{stderr}"
            );
        } else {
            assert!(
                stderr.contains("team `duo` is already running in worktree `feature`"),
                "{stderr}"
            );
        }
        assert!(!stderr.contains("fast-forwarding"), "{stderr}");
        assert!(!stderr.contains("reusing worktree"), "{stderr}");
        assert_eq!(git_stdout(&holder, &["rev-parse", "HEAD"]), *tip);
        assert!(!trace.contains("new-tab"), "{trace}");
        assert_eq!(
            serde_json::to_value(env.store().snapshot_cached().unwrap().agents).unwrap(),
            before
        );
    } else {
        assert!(
            stderr.contains("fast-forwarding `feature` to the PR head"),
            "{stderr}"
        );
        let marker = rimz::worktree::read_marker_for_worktree(&holder)
            .unwrap()
            .unwrap();
        assert!(
            stderr.contains(&format!(
                "reusing worktree `feature` at {} (PR #1 head branch `feature`)",
                marker.worktree_path.display()
            )),
            "{stderr}"
        );
        assert_eq!(git_stdout(&holder, &["rev-parse", "HEAD"]), pr_head);
        assert_eq!(marker.from_pr, Some(1));
        assert!(
            trace
                .lines()
                .any(|line| line.split('\t').any(|arg| arg == "new-tab")
                    && line.split('\t').any(|arg| arg == "--no-focus")),
            "{trace}"
        );
    }
}

fn assert_launch_focus(args: &[&str], agent: bool, action: &str) {
    assert_launch_focus_version(args, agent, action, "0.45.1");
}

fn assert_launch_focus_version(args: &[&str], agent: bool, action: &str, version: &str) {
    let env = Env::new();
    crate::common::wait::register_calling_agent(&env);
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = 'host'\n",
    )
    .unwrap();
    crate::common::write_definition(
        &env,
        "agents",
        "worker",
        "description: Worker\nagent: claude\ntools: []",
        "",
    );
    crate::common::write_definition(
        &env,
        "teams",
        "duo",
        "layout: lead,worker\nleader: lead\nstages: [Build]\nroles:\n  - {role: lead, agent: worker, owns: [Build]}\n  - {role: worker, agent: worker}",
        "Complete the work.",
    );
    let hold = action.starts_with("hold-");
    let channel_hold = action.starts_with("hold-channel-");
    let root_hold = matches!(
        action,
        "hold-root" | "hold-channel-derived" | "hold-channel-same" | "hold-channel-symlink"
    );
    let live_hold = matches!(
        action,
        "hold-live"
            | "hold-root"
            | "hold-channel-live"
            | "hold-channel-inferred"
            | "hold-channel-derived"
            | "hold-channel-same"
            | "hold-channel-symlink"
    );
    if hold {
        crate::common::write_definition(
            &env,
            "teams",
            "solo",
            "layout: lead\nleader: lead\nstages: [Build]\nroles:\n  - {role: lead, agent: worker, owns: [Build]}",
            "Complete the work.",
        );
    }
    let shim = write_env_dump_shim(&env, "claude");
    let workspace = env.resolve_workspace(&env.project_root);
    if matches!(args[1], "restart" | "resume") || action == "cohort" || hold {
        let worktree = if root_hold {
            if channel_hold {
                assert!(init_launch_repo(&env.project_root));
            }
            if action == "hold-channel-symlink" {
                let alias = env.home_root.join("checkout-alias");
                #[cfg(unix)]
                std::os::unix::fs::symlink(&env.project_root, &alias).unwrap();
                alias
            } else {
                env.project_root.clone()
            }
        } else if action == "cohort" || hold {
            assert!(init_launch_repo(&env.project_root));
            let path =
                rimz::worktree::worktree_path(&env.project_root, &Default::default(), "review")
                    .unwrap();
            if matches!(
                action,
                "hold-done"
                    | "hold-resume"
                    | "hold-channel-live"
                    | "hold-channel-board"
                    | "hold-channel-inferred"
            ) {
                env.rimz()
                    .args(["worktree", "new", "review"])
                    .assert()
                    .success();
            } else if !hold {
                std::fs::create_dir_all(&path).unwrap();
            }
            if matches!(
                action,
                "hold-board"
                    | "hold-done"
                    | "hold-resume"
                    | "hold-channel-board"
                    | "hold-channel-done"
            ) {
                std::fs::create_dir_all(&path).unwrap();
                std::fs::write(
                    path.join("blackboard.md"),
                    if matches!(action, "hold-done" | "hold-channel-done") {
                        "Stage: Done\n"
                    } else {
                        "Stage: Build (@lead)\n"
                    },
                )
                .unwrap();
            }
            path
        } else {
            env.project_root.clone()
        };
        let mut observation = AgentLifecycleObservation {
            agent_id: Some("provider-session".into()),
            pane_id: Some(rimz::ids::PaneId::from_parts(
                rimz::ids::MuxName::Zellij,
                "terminal_3",
            )),
            runtime_owner: Some(rimz::pane::RuntimeOwner::new(
                rimz::pane::RuntimeOwnerKind::Agent,
                "provider-session",
                std::process::id(),
                None,
            )),
            worktree_path: Some(worktree.display().to_string()),
            launch: LaunchParams {
                channel: Some("review".to_owned()),
                team: (action == "cohort" || hold).then(|| "duo".to_owned()),
                role: hold.then(|| "lead".to_owned()),
                ..Default::default()
            },
            ..AgentLifecycleObservation::new(
                Some("provider-session".into()),
                LifecycleSignal::Registered,
            )
        };
        if hold && !live_hold {
            observation.runtime_owner = None;
        }
        env.store()
            .append_event(&EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                "claude",
                "test",
                &observation,
            ))
            .unwrap();
        if hold && !live_hold {
            env.store()
                .append_event(&EventEnvelope::agent_lifecycle(
                    workspace.workspace_id.clone(),
                    &workspace.session_name,
                    "claude",
                    "test",
                    &AgentLifecycleObservation::new(
                        Some("provider-session".into()),
                        LifecycleSignal::Ended,
                    ),
                ))
                .unwrap();
        }
    }
    let before = serde_json::to_value(env.store().snapshot_cached().unwrap().agents).unwrap();
    let events_before = std::fs::read(env.state_path_for(&env.project_root).events_log).unwrap();
    let channels_before = std::fs::read(env.state_path_for(&env.project_root).channels_record).ok();
    let log = env.home_root.join("mux.log");
    let mut command = env.rimz();
    command.args(["--mux", "zellij"]).args(args)
        .env("ZELLIJ_PANE_ID", "1")
        .env("PATH", path_with_front(&shim))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", env.home_root.join("agent-env"))
        .env("RIMZ_TEST_ZELLIJ_TRACE_CONTEXT", "1")
        .env("RIMZ_ZELLIJ_BIN", crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")))
        .env("RIMZ_TEST_ZELLIJ_VERSION", version)
        .env("RIMZ_TEST_ZELLIJ_LIST_CLIENTS", "1 terminal_3 claude\n")
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"sh"},{"id":3,"is_plugin":false,"tab_id":2,"title":"claude"}]"#)
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", format!("{} [Created 1s ago]\n", workspace.session_name));
    if action == "hold-channel-symlink" {
        command
            .arg("--cwd")
            .arg(env.home_root.join("checkout-alias"));
    }
    if agent {
        command
            .env("RIMZ_AGENT_KIND", "claude")
            .env("RIMZ_AGENT_ID", "launch-session");
    }
    if action == "hold-channel-inferred" {
        command.env(rimz::workspace::ENV_CHANNEL, "review");
    }
    let output = command.bounded_output().unwrap();
    if matches!(
        action,
        "hold-live"
            | "hold-board"
            | "hold-root"
            | "hold-channel-live"
            | "hold-channel-board"
            | "hold-channel-derived"
            | "hold-channel-inferred"
    ) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{stderr}");
        assert!(
            stderr.contains(if channel_hold {
                "already carries team `duo`"
            } else {
                "already holds team `duo`"
            }),
            "{stderr}"
        );
        if channel_hold {
            assert!(stderr.contains("channel `#review`"), "{stderr}");
            assert!(stderr.contains("one team per channel"), "{stderr}");
            let holder = if root_hold {
                env.project_root.clone()
            } else {
                rimz::worktree::worktree_path(&env.project_root, &Default::default(), "review")
                    .unwrap()
            };
            assert!(
                stderr.contains(&format!("at checkout `{}`", holder.display())),
                "{stderr}"
            );
            if action == "hold-channel-board" {
                assert!(
                    stderr.contains(&holder.join("blackboard.md").display().to_string()),
                    "{stderr}"
                );
            }
        }
        if root_hold {
            assert!(stderr.contains("checkout `"), "{stderr}");
            assert!(stderr.contains("rimz teams resume duo` from"), "{stderr}");
        } else {
            assert!(
                stderr.contains("rimz teams resume duo -w review"),
                "{stderr}"
            );
        }
        if matches!(action, "hold-board" | "hold-channel-board") {
            assert!(stderr.contains("`Stage: Done` to release it"), "{stderr}");
        }
        assert!(
            stderr.contains(if live_hold {
                "a member is live"
            } else {
                "its board is at `Build`"
            }),
            "{stderr}"
        );
        assert_eq!(
            serde_json::to_value(env.store().snapshot_cached().unwrap().agents).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read(env.state_path_for(&env.project_root).events_log).unwrap(),
            events_before
        );
        assert_eq!(
            std::fs::read(env.state_path_for(&env.project_root).channels_record).ok(),
            channels_before
        );
        assert!(!std::fs::read_to_string(&log).unwrap().contains("new-tab"));
        if matches!(action, "hold-live" | "hold-channel-derived") {
            assert!(
                !rimz::worktree::worktree_path(&env.project_root, &Default::default(), "review")
                    .unwrap()
                    .exists()
            );
        }
        return;
    }
    let action = if hold { "new-tab" } else { action };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let trace = std::fs::read_to_string(log).unwrap();
    if matches!(args[1], "claude" | "duo") && action != "cohort" {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            stderr.matches("claude has no trust decision").count(),
            1,
            "{stderr}"
        );
    }
    if matches!(action, "live" | "cohort") {
        let receipt = if action == "live" {
            String::from_utf8_lossy(&output.stdout)
        } else {
            String::from_utf8_lossy(&output.stderr)
        };
        assert!(
            receipt.contains(if action == "live" {
                "is already live"
            } else {
                "is already running"
            }),
            "{receipt}"
        );
        assert!(!receipt.contains("focused"), "{receipt}");
        assert!(
            !trace
                .split_whitespace()
                .any(|arg| matches!(arg, "go-to-tab" | "focus-pane-id")),
            "{trace}"
        );
        return;
    }
    let spawn = trace
        .lines()
        .find(|line| line.split('\t').any(|arg| arg == action));
    assert!(spawn.is_some(), "missing {action}: {trace}");
    if version == "0.44.3" {
        assert!(trace.contains("focus-pane-id\tterminal_3"), "{trace}");
        assert!(!trace.contains("focus-pane-id\tterminal_1"), "{trace}");
        assert!(!trace.contains("--session\t\t"), "{trace}");
        return;
    }
    assert_eq!(
        spawn.unwrap().split('\t').any(|arg| arg == "--no-focus"),
        agent,
        "{trace}"
    );
    if args[1] == "restart" {
        assert!(spawn.unwrap().starts_with("pane=3\t"), "{trace}");
    }
    if agent {
        assert!(
            !trace
                .split_whitespace()
                .any(|arg| matches!(arg, "go-to-tab" | "focus-pane-id" | "move-focus")),
            "{trace}"
        );
    }
}

#[test]
fn unsupported_plugin_peer_launch_explains_that_no_report_will_come() {
    let env = Env::new();
    crate::common::wait::register_calling_agent(&env);
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = 'host'\n",
    )
    .unwrap();
    env.rimz()
        .args(["agents", "register", "testbot"])
        .assert()
        .success();
    let shim = write_env_dump_shim(&env, "testbot");
    let session_name = env.resolve_workspace(&env.project_root).session_name;
    let out = env
        .rimz()
        .args([
            "--mux",
            "zellij",
            "agents",
            "testbot",
            "--bg",
            "--new-pane",
            "task",
        ])
        .env("ZELLIJ_PANE_ID", "1")
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "launch-session")
        .env("PATH", path_with_front(&shim))
        .env(
            "RIMZ_ZELLIJ_BIN",
            crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
        )
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("mux.log"))
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{session_name} [Created 1s ago]\n"),
        )
        .bounded_output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let receipt = String::from_utf8_lossy(&out.stdout);
    assert!(receipt.contains("will not report back"), "{receipt}");
    assert!(
        rimz::harness::run::list(env.store().paths())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn peer_launch_reports_only_launcher_opened_turns() {
    use rimz::harness::run;
    use rimz::store::run::RunStatus;
    use serde_json::json;
    use std::time::{Duration, Instant};

    let env = Env::new();
    crate::common::wait::register_calling_agent(&env);
    let store = env.store();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = 'host'\n",
    )
    .unwrap();
    let shim = write_env_dump_shim(&env, "claude");
    let session_name = env.resolve_workspace(&env.project_root).session_name;
    let out = env
        .rimz()
        .args([
            "--mux",
            "zellij",
            "agents",
            "claude",
            "--bg",
            "--new-pane",
            "--name",
            "peer",
            "first task",
        ])
        .env("ZELLIJ_PANE_ID", "1")
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "launch-session")
        .env("PATH", path_with_front(&shim))
        .env(
            "RIMZ_ZELLIJ_BIN",
            crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
        )
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("mux.log"))
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{session_name} [Created 1s ago]\n"),
        )
        .bounded_output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let runs = run::list(store.paths()).unwrap();
    assert_eq!(
        runs.len(),
        1,
        "prompted peer launch must create its run before a provider hook"
    );
    let first = &runs[0];
    assert_eq!(first.status, RunStatus::Pending);
    let pending_wait = env
        .rimz()
        .args(["agents", "wait", "@peer", "--timeout", "1s"])
        .bounded_output()
        .unwrap();
    assert_eq!(
        pending_wait.status.code(),
        Some(RunStatus::TimedOut.exit_code()),
        "wait must find the pending run before registration: {}",
        String::from_utf8_lossy(&pending_wait.stderr)
    );
    let receipt = String::from_utf8_lossy(&out.stdout);
    assert!(receipt.contains("AGENT_REPORT"), "{receipt}");
    assert!(
        receipt.contains(&format!("rimz agents wait {}", first.run_id)),
        "{receipt}"
    );
    let launch_id = &first.peer.as_ref().unwrap().launch_id;
    // The mux shim does not execute its pane command. Bind the pane as the exec wrapper does.
    store
        .bind_agent_launch(
            &rimz::store::writer::AgentLaunchIdentity {
                kind: first.kind.clone(),
                agent_id: launch_id.clone(),
                name: "peer".into(),
                name_explicit: true,
                launch: LaunchParams {
                    launched_by: Some(Box::new(rimz::agents::LaunchedBy {
                        kind: AgentKind::new_unchecked("claude"),
                        agent_id: "launch-session".into(),
                    })),
                    ..Default::default()
                },
                run_id: None,
                prompt: Some("first task".into()),
            },
            &session_name,
            &env.project_root,
            &rimz::PaneId::from_parts(rimz::MuxName::Zellij, "terminal_2"),
        )
        .unwrap();
    let hook =
        |event: &str, prompt: &str, answer: &str| {
            let mut command = env.hook_command("claude");
            command
                .env("RIMZ_BIN", env.rimz_bin())
                .env("RIMZ_AGENT_KIND", "claude")
                .env("ZELLIJ_PANE_ID", "2")
                .env("RIMZ_AGENT_ID", launch_id.as_str());
            let out = env.spawn_payload(command, &json!({
            "hook_event_name": event, "session_id": "peer-session", "cwd": env.project_root,
            "prompt": prompt, "last_assistant_message": answer,
        }).to_string()).wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(out.stdout.is_empty(), "hook stdout stays neutral");
        };
    let mut waiter = env
        .rimz()
        .args(["agents", "wait", first.run_id.as_str(), "--timeout", "10s"])
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "launch-session")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    hook("SessionStart", "", "");
    hook("UserPromptSubmit", "first task", "");
    let running = run::load(store.paths(), &first.run_id).unwrap();
    assert_eq!(running.status, RunStatus::Running);
    assert_eq!(
        running.agent_id.as_ref().map(AgentSessionId::as_str),
        Some("peer-session")
    );
    assert!(waiter.try_wait().unwrap().is_none());
    hook("Stop", "", "first answer");
    let wait_output = waiter.wait_with_output().unwrap();
    assert!(
        wait_output.status.success(),
        "{}",
        String::from_utf8_lossy(&wait_output.stderr)
    );
    assert!(String::from_utf8_lossy(&wait_output.stdout).contains("first answer"));
    assert!(
        run::load(store.paths(), &first.run_id)
            .unwrap()
            .joined_at
            .is_none(),
        "a wait outside the launcher's turn leaves its report owed"
    );
    let wait_reports = |count| {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let reports = store
                .list_messages()
                .unwrap()
                .into_iter()
                .filter(|message| {
                    matches!(
                        message.sender,
                        rimz::store::message::MessageSender::Harness {
                            notice: rimz::store::message::HarnessNotice::AgentReport
                        }
                    )
                })
                .collect::<Vec<_>>();
            if reports.len() == count {
                return reports;
            }
            assert!(
                Instant::now() < until,
                "expected {count} reports, got {}",
                reports.len()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let reports = wait_reports(1);
    let first_path = run::peer_response_path(store.paths(), "peer", &first.run_id);
    let first_bytes = std::fs::read(&first_path).unwrap();
    assert!(reports[0].text.contains("first task"));
    assert!(reports[0].text.contains(&first_path.display().to_string()));
    assert!(reports[0].text.contains(" in "));
    assert!(reports[0].text.contains("tokens"), "{}", reports[0].text);
    hook("UserPromptSubmit", "human task", "");
    hook("Stop", "", "human answer");
    assert_eq!(run::list(store.paths()).unwrap().len(), 1);
    assert_eq!(std::fs::read(&first_path).unwrap(), first_bytes);
    assert_eq!(wait_reports(1).len(), 1);

    let peer = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| agent.agent_id.as_str() == "peer-session")
        .unwrap();
    let mut message = rimz::store::message::MessageRecord::new(
        env.workspace_id.clone(),
        &peer,
        "second task".into(),
        rimz::store::message::DeliveryGate::Any,
    );
    message.sender = rimz::store::message::MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: Some("launch-session".into()),
        name: Some("planner".into()),
        profile: None,
        role: None,
        channel: None,
    };
    store
        .record_sent_batch(&[message], "test fake provider delivery")
        .unwrap();
    hook(
        "UserPromptSubmit",
        "Type: AGENT_MESSAGE\nFrom: @planner\nContent:\nsecond task",
        "",
    );
    hook("Stop", "", "second answer");
    let reports = wait_reports(2);
    assert_eq!(
        reports
            .iter()
            .filter(|report| report.text.contains("second task"))
            .count(),
        1
    );
    let second = run::list(store.paths())
        .unwrap()
        .into_iter()
        .find(|run| run.run_id != first.run_id)
        .unwrap();
    let second_path = run::peer_response_path(store.paths(), "peer", &second.run_id);
    assert_ne!(first_path, second_path);
    assert_eq!(std::fs::read(&first_path).unwrap(), first_bytes);
    assert_eq!(
        std::fs::read_to_string(second_path).unwrap().trim(),
        "second answer"
    );
    assert!(
        store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents
            .iter()
            .any(|agent| agent.agent_id.as_str() == "peer-session" && agent.ended_at.is_none()),
        "peer remains open after reporting"
    );
}

#[cfg(unix)]
#[test]
fn over_limit_agent_launch_refuses_before_creating_runtime_state() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let launch_id = AgentSessionId::from("launch_caller");
    env.store()
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("codex"),
            AgentLaunchPayload {
                agent_id: AgentSessionId::from("provider-caller"),
                launch_id: Some(launch_id.clone()),
                agent_name: "caller".to_owned(),
                agent_name_explicit: true,
                launch: LaunchParams {
                    launch_depth: Some(3),
                    ..Default::default()
                },
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: Some("main".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .expect("seed launched caller");

    let output = env
        .rimz()
        .args(["agents", "claude", "--worktree=depth-refused"])
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(rimz::harness::launch::ENV_AGENT_ID, launch_id.as_str())
        .output()
        .expect("run nested launch");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "nested launch unexpectedly succeeded"
    );
    assert!(stderr.contains("launch refused"), "{stderr}");
    assert!(stderr.contains("maximum chain length of 3"), "{stderr}");
    assert!(stderr.contains("do not retry"), "{stderr}");
    assert!(!stderr.contains("--top-level"), "{stderr}");
    assert_eq!(
        env.store().read_events().expect("read events").len(),
        1,
        "refusal must not append a provisional launch"
    );
    assert!(
        !env.home_root
            .join("project-worktrees")
            .join("depth-refused")
            .exists(),
        "refusal must precede worktree creation"
    );
}

/// Seeds a bound `rimz subagents` child and returns the launch id its
/// process environment carries.
fn seed_subagent_caller(env: &Env) -> AgentSessionId {
    let workspace = env.resolve_workspace(&env.project_root);
    let launch_id = AgentSessionId::from("launch_subagent");
    env.store()
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("codex"),
            AgentLaunchPayload {
                agent_id: AgentSessionId::from("provider-subagent"),
                launch_id: Some(launch_id.clone()),
                agent_name: "subagent".to_owned(),
                agent_name_explicit: true,
                launch: LaunchParams {
                    parent_agent_id: Some(AgentSessionId::from("root-session")),
                    parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
                    launch_depth: Some(1),
                    ..Default::default()
                },
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: Some("main".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .expect("seed subagent caller");
    launch_id
}

#[cfg(unix)]
#[test]
fn subagent_caller_refuses_subagent_launch_before_creating_runtime_state() {
    let env = Env::new();
    let launch_id = seed_subagent_caller(&env);

    let output = env
        .rimz()
        .args(["subagents", "claude", "try to delegate again"])
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(rimz::harness::launch::ENV_AGENT_ID, launch_id.as_str())
        .output()
        .expect("run launch from subagent");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "subagent launch unexpectedly succeeded"
    );
    assert!(stderr.contains("subagents cannot launch"), "{stderr}");
    assert!(stderr.contains("do not retry"), "{stderr}");
    assert_eq!(
        env.store().read_events().expect("read events").len(),
        1,
        "refusal must not append a provisional launch"
    );
}

#[cfg(unix)]
#[test]
fn user_shell_subagents_list_inspects_the_channel() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let rows = [
        ("claude", "planner", "planner", None, None, "feat-x"),
        (
            "codex",
            "swift-child",
            "swift-otter",
            Some("planner"),
            Some("claude"),
            "feat-x",
        ),
        ("claude", "other", "other", None, None, "other"),
        (
            "codex",
            "calm-child",
            "calm-fox",
            Some("other"),
            Some("claude"),
            "other",
        ),
    ];
    for (kind, id, name, parent_id, parent_kind, channel) in rows {
        env.store()
            .append_event(&EventEnvelope::agent_launched(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                &AgentKind::new_unchecked(kind),
                AgentLaunchPayload {
                    agent_id: AgentSessionId::from(id),
                    launch_id: None,
                    agent_name: name.to_owned(),
                    agent_name_explicit: true,
                    launch: LaunchParams {
                        parent_agent_id: parent_id.map(AgentSessionId::from),
                        parent_agent_kind: parent_kind.map(AgentKind::new_unchecked),
                        launch_depth: parent_id.map(|_| 1),
                        channel: Some(channel.to_owned()),
                        ..Default::default()
                    },
                    state: AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: None,
                    runtime_owner: None,
                    worktree_path: Some(env.project_root.display().to_string()),
                    worktree_branch: Some(channel.to_owned()),
                    prompt: None,
                    description: None,
                },
            ))
            .expect("seed agent row");
    }

    let instances_path = env.store().paths().root.join("records/loop-instances.json");
    std::fs::create_dir_all(instances_path.parent().unwrap()).unwrap();
    let tasks = std::collections::BTreeMap::from([(
        "wait-child",
        rimz::config::TaskEntry {
            root: env.project_root.clone(),
            signal: Some("deploy.done".to_owned()),
            once: Some(true),
            wait: Some(rimz::config::TaskTarget {
                kind: AgentKind::new_unchecked("codex"),
                session: AgentSessionId::from("swift-child"),
                handle: "@swift-otter".to_owned(),
            }),
            ..Default::default()
        },
    )]);
    std::fs::write(&instances_path, serde_json::to_vec(&tasks).unwrap()).unwrap();

    let output = env
        .rimz()
        .args(["subagents", "list", "--json"])
        .env(rimz::workspace::ENV_CHANNEL, "feat-x")
        .output()
        .expect("list current channel subagents");
    assert!(
        output.status.success(),
        "user-shell list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("channel list json");
    let rows = rows.as_array().expect("channel list array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "swift-otter");
    assert_eq!(rows[0]["status"], "sleeping");
    assert_eq!(rows[0]["parent"], "@planner");
    assert_eq!(rows[0]["channel"], "feat-x");

    std::fs::write(&instances_path, b"{}").unwrap();
    let output = env
        .rimz()
        .args(["subagents", "list", "--json"])
        .output()
        .expect("list all channel subagents");
    assert!(
        output.status.success(),
        "project-root list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("all-channel list json");
    let rows = rows.as_array().expect("all-channel list array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["channel"], "feat-x");
    assert_ne!(rows[0]["status"], "sleeping");
    assert_eq!(rows[1]["channel"], "other");

    for args in [
        &["subagents", "wait"][..],
        &["subagents", "stop", "--all"],
        &["subagents", "codex", "hello"],
    ] {
        let output = env
            .rimz()
            .args(args)
            .output()
            .expect("run agent-only subagents command");
        assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("only available to an agent RimZ can identify"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(unix)]
#[test]
fn user_shell_subagent_entrypoints_do_not_create_room_state() {
    let env = Env::new();
    let state = env.state_path_for(&env.project_root);
    let long_runtime = env.home_root.join("r".repeat(180));
    std::fs::create_dir_all(&long_runtime).expect("long runtime root");

    let profiles = env
        .rimz()
        .env("XDG_RUNTIME_DIR", &long_runtime)
        .args(["subagents", "profiles"])
        .output()
        .expect("list profiles");
    assert!(
        profiles.status.success(),
        "profiles failed: {}",
        String::from_utf8_lossy(&profiles.stderr)
    );
    assert!(!state.root.exists(), "profiles created room state");

    let launch = env
        .rimz()
        .env("XDG_RUNTIME_DIR", &long_runtime)
        .args(["subagents", "codex", "hello"])
        .output()
        .expect("reject user-shell launch");
    assert!(!launch.status.success(), "user-shell launch succeeded");
    assert!(
        String::from_utf8_lossy(&launch.stderr)
            .contains("only available to an agent RimZ can identify"),
        "{}",
        String::from_utf8_lossy(&launch.stderr)
    );
    assert!(!state.root.exists(), "launch refusal created room state");
}

#[cfg(unix)]
#[test]
fn explain_prints_the_plan_without_side_effects() {
    let env = Env::new();
    assert!(init_launch_repo(&env.project_root));
    let state = env.state_path_for(&env.project_root);
    let runtime = env.runtime_paths();
    let config_dir = env.rimz_home();
    std::fs::create_dir_all(&config_dir).expect("mkdir config");
    std::fs::write(
        config_dir.join("config.toml"),
        "[agents]\nisolation = \"host\"\n",
    )
    .expect("write explain profiles");
    crate::common::write_definition(
        &env,
        "agents",
        "claude",
        "description: Claude base",
        "Base instructions.",
    );
    crate::common::write_definition(
        &env,
        "agents",
        "codex",
        "description: Codex base",
        "Base instructions.",
    );
    crate::common::write_definition(
        &env,
        "agents",
        "writer",
        "description: Writer\nagent: claude\nmodel: fable\neffort: high\nmode: ask\ntools: [Skill]",
        "More instructions.",
    );
    crate::common::write_definition(
        &env,
        "agents",
        "worker",
        "description: Worker\nagent: writer\nskills: []",
        "",
    );
    crate::common::write_definition(
        &env,
        "agents",
        "fast",
        "description: Fast worker\nagent: codex\ntools: []",
        "",
    );
    crate::common::write_definition(
        &env,
        "agents",
        "leader",
        "description: Team leader\nagent: claude\ntools: []",
        "",
    );
    crate::common::write_definition(
        &env,
        "teams",
        "duo",
        "layout: lead\nleader: lead\nstages: [Build]\nroles:\n  - {role: lead, agent: leader, owns: [Build]}",
        "Complete the work.",
    );
    crate::common::write_definition(
        &env,
        "subagents",
        "scout",
        "description: Inspect the code\nagent: codex\ntools: []",
        "",
    );
    assert!(!state.root.exists());
    assert!(!runtime.prompt_dir().exists());

    let output = env
        .rimz()
        .args(["agents", "explain", "worker", "--json"])
        .assert()
        .success()
        .get_output()
        .clone();
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("explain JSON");
    assert_eq!(report["tier"], "principal");
    assert!(report.get("reentry").is_none());
    let artifact = std::path::Path::new(report["prompt"]["artifact"].as_str().unwrap());
    assert_eq!(artifact.parent(), Some(runtime.prompt_dir().as_path()));
    let filename = artifact.file_name().unwrap().to_str().unwrap();
    let digest = filename
        .strip_prefix("sys.")
        .unwrap()
        .strip_suffix(".md")
        .unwrap();
    assert_eq!(digest.len(), 32);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(
        !artifact.exists(),
        "explain materialized its planned prompt"
    );
    let provider_argv = report["provider_argv"].as_array().unwrap();
    assert!(provider_argv.windows(2).any(|pair| {
        pair[0] == "--system-prompt-file" && pair[1] == report["prompt"]["artifact"]
    }));
    assert!(
        provider_argv
            .windows(2)
            .any(|pair| pair[0] == "--model" && pair[1] == "fable")
    );
    let reminder = report["prompt"]["reminder"].as_str().unwrap();
    assert!(reminder.contains("- shell: sh"));
    assert!(!reminder.contains("git status"));
    assert!(reminder.starts_with("<system_reminder>\nYou are @worker, running on"));
    assert!(!reminder.contains("effort"));
    assert!(reminder.contains("- `scout`: Inspect the code"));
    assert!(reminder.ends_with("\n</system_reminder>"));
    assert!(
        provider_argv
            .windows(2)
            .any(|pair| { pair[0] == "--append-system-prompt" && pair[1] == reminder })
    );
    assert_eq!(report["env"]["RIMZ_AGENT_PROFILE"], "worker");
    assert_eq!(report["env"]["RIMZ_AGENT_ID"], "");
    assert!(report["env"].get("RIMZ_AGENT_NAME").is_none());

    let mut protocol = serde_json::json!({
        "target": report["target"], "kind": report["kind"], "action": report["action"],
        "name": report["name"], "launch_id": report["launch_id"], "cwd": report["cwd"],
        "profile": report["profile"], "mode": report["mode"], "model": report["model"],
        "effort": report["effort"], "skills": report["skills"], "program": report["program"],
        "shell": report["argv"][0], "prompt": report["prompt"], "sandbox": report["sandbox"]
    });
    assert_eq!(protocol["shell"], "/bin/sh");
    protocol["shell"] = "<shell>".into();
    protocol["prompt"]["artifact"] = "<runtime>/prompt/sys.<digest>.md".into();
    protocol["prompt"]["reminder"] =
        "<system_reminder>\n<model and subagent catalog>\n</system_reminder>".into();
    let protocol = serde_json::to_string(&protocol)
        .unwrap()
        .replace(env.home_root.to_str().unwrap(), "<home>");
    insta::assert_json_snapshot!(
        serde_json::from_str::<serde_json::Value>(&protocol).unwrap(),
        @r#"
    {
      "action": "launch",
      "cwd": "<home>/project",
      "effort": "high",
      "kind": "claude",
      "launch_id": null,
      "mode": "ask",
      "model": "fable",
      "name": null,
      "profile": {
        "chain": [
          "worker",
          "claude"
        ],
        "name": "worker",
        "role": null,
        "team": null
      },
      "program": "claude",
      "prompt": {
        "artifact": "<runtime>/prompt/sys.<digest>.md",
        "channel": "--system-prompt-file",
        "composed": "Base instructions.\n\nMore instructions.\n",
        "reminder": "<system_reminder>\n<model and subagent catalog>\n</system_reminder>",
        "reminder_channel": "--append-system-prompt",
        "reminder_delivered": true,
        "sources": [
          {
            "bytes": 18,
            "path": "<home>/rimz-home/agents/claude.md"
          },
          {
            "bytes": 18,
            "path": "<home>/rimz-home/agents/writer.md"
          }
        ]
      },
      "sandbox": null,
      "shell": "<shell>",
      "skills": {
        "applied": false,
        "callable": [],
        "host": {
          "Applied": {
            "effect": "user-only",
            "flag": "--settings skillOverrides",
            "listed": [],
            "unlisted": []
          }
        }
      },
      "target": "worker"
    }
    "#
    );

    let human = env
        .rimz()
        .args(["agents", "explain", "worker", "--tier", "senior"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let human = String::from_utf8(human).unwrap();
    assert!(
        human
            .lines()
            .any(|line| line.trim_start().starts_with("tier:")
                && line.contains("senior")
                && line.contains("opus"))
    );

    let prompt = env
        .rimz()
        .args(["agents", "explain", "worker", "--prompt"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert_eq!(
        prompt,
        format!("Base instructions.\n\nMore instructions.\n\n\n{reminder}").as_bytes()
    );

    let overridden = env
        .rimz()
        .args([
            "agents", "explain", "worker", "--json", "--model", "opus", "--yolo", "--", "--foo",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let overridden: serde_json::Value = serde_json::from_slice(&overridden).unwrap();
    assert_eq!(
        overridden["overrides"],
        serde_json::json!(["--model opus", "--yolo", "-- --foo"])
    );
    assert_eq!(overridden["model"], "opus");
    assert_eq!(overridden["tier"], "senior");
    assert_eq!(overridden["mode"], "yolo");
    let argv = overridden["provider_argv"].as_array().unwrap();
    assert!(
        argv.windows(2)
            .any(|pair| pair[0] == "--model" && pair[1] == "opus")
    );
    assert_eq!(
        argv.iter()
            .filter(|arg| *arg == "--dangerously-skip-permissions")
            .count(),
        1
    );
    assert!(argv.iter().any(|arg| arg == "--foo"));
    let team = env
        .rimz()
        .args(["agents", "explain", "duo.lead"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let team = String::from_utf8(team).unwrap();
    assert!(team.contains("duo.lead ← claude"), "{team}");
    assert!(!team.contains("duo.lead ← duo.lead"), "{team}");
    for (target, agent_override, kind, chain) in [
        (
            "worker",
            " codex ",
            "claude",
            serde_json::json!(["worker", "claude", "codex"]),
        ),
        (
            "worker",
            "fast",
            "codex",
            serde_json::json!(["worker", "claude", "fast", "codex"]),
        ),
        (
            "codex",
            "claude",
            "claude",
            serde_json::json!(["codex", "claude"]),
        ),
        (
            "writer",
            "writer",
            "claude",
            serde_json::json!(["writer", "claude"]),
        ),
    ] {
        let output = env
            .rimz()
            .args([
                "agents",
                "explain",
                target,
                "--agent",
                agent_override,
                "--json",
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(report["kind"], kind);
        assert_eq!(report["profile"]["chain"], chain);
        if agent_override == "fast" {
            assert!(
                report.get("tier").is_none(),
                "a profile rebase does not route"
            );
        }
    }
    let settings_body = r#"{"env":{"ANTHROPIC_API_KEY":"sk-secret-123"}}"#;
    let settings = env.home_root.join("settings.json");
    std::fs::write(&settings, settings_body).unwrap();
    for value in [settings.to_str().unwrap(), settings_body] {
        for json in [false, true] {
            let mut command = env.rimz();
            command.args(["agents", "explain", "worker"]);
            if json {
                command.arg("--json");
            }
            let output = command
                .args(["--", "--settings", value])
                .assert()
                .success()
                .get_output()
                .stdout
                .clone();
            assert!(!String::from_utf8_lossy(&output).contains("sk-secret-123"));
            if json {
                let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
                let argv = report["provider_argv"].as_array().unwrap();
                let index = argv.iter().position(|arg| arg == "--settings").unwrap();
                let path = std::path::Path::new(argv[index + 1].as_str().unwrap());
                assert_eq!(path.parent(), Some(runtime.prompt_dir().as_path()));
                assert!(!path.exists());
            }
        }
    }
    assert!(
        !state.root.exists(),
        "explain created room state or launch events"
    );
    assert!(!state.tmp_dir.exists(), "explain created room tmp");
    assert!(
        !runtime.prompt_dir().exists(),
        "explain created prompt artifacts"
    );
}

#[cfg(unix)]
#[test]
fn explain_redacts_trusted_project_env_in_json_and_human_output() {
    let env = Env::new();
    let secret = "explain-credential-not-for-output";
    env.write_config(
        &env.project_root,
        &format!("[[agents]]\nname = \"claude\"\nenv = {{ RIMZ_TEST_CREDENTIAL = {secret:?} }}\n"),
    );
    env.rimz().args(["trust", "grant"]).assert().success();
    for json in [true, false] {
        let mut command = env.rimz();
        command.args(["agents", "explain", "claude"]);
        if json {
            command.arg("--json");
        }
        let output = command.assert().success().get_output().clone();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains(secret), "credential leaked on stdout");
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains(secret),
            "credential leaked on stderr"
        );
        assert!(stdout.contains("RIMZ_TEST_CREDENTIAL"));
        assert!(stdout.contains("<redacted>"));
        if json {
            let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
            assert_eq!(report["env"]["RIMZ_TEST_CREDENTIAL"], "<redacted>");
            assert_eq!(
                report["redacted_keys"],
                serde_json::json!(["RIMZ_TEST_CREDENTIAL"])
            );
            assert!(
                report["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|arg| arg == "RIMZ_TEST_CREDENTIAL=<redacted>")
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn explain_seat_replays_current_profile_without_writes_and_refuses_overrides() {
    for stamped in [false, true] {
        let env = Env::new();
        let provider_home = env.home_root.join(".claude");
        std::fs::create_dir_all(provider_home.join("projects"))
            .expect("empty conversation catalog");
        let config_dir = env.rimz_home();
        std::fs::create_dir_all(&config_dir).expect("mkdir config");
        std::fs::write(
            config_dir.join("config.toml"),
            "[agents]\nisolation = \"host\"\n",
        )
        .expect("write current profile");
        crate::common::write_definition(
            &env,
            "agents",
            "worker",
            "description: Worker\nagent: claude\nmodel: opus\ntools: []",
            "",
        );
        let workspace = env.resolve_workspace(&env.project_root);
        let store = env.store();
        store
            .append_event(&EventEnvelope::agent_launched(
                workspace.workspace_id,
                &workspace.session_name,
                &AgentKind::new_unchecked("claude"),
                AgentLaunchPayload {
                    agent_id: AgentSessionId::from("missing-conversation"),
                    launch_id: Some(AgentSessionId::from("launch_explain_worker")),
                    agent_name: "worker".to_owned(),
                    agent_name_explicit: true,
                    launch: LaunchParams {
                        profile: Some("worker".to_owned()),
                        model: Some("fable".to_owned()),
                        tier: stamped.then(|| {
                            Box::new(rimz::agents::TierStamp {
                                tier: rimz::config::tiers::ModelTier::Senior,
                                model: "claude-fable-5-1".to_owned(),
                                used_tier: Some(rimz::config::tiers::ModelTier::Principal),
                                skipped: vec![rimz::agents::TierSkip {
                                    model: "astra".to_owned(),
                                    reason: rimz::agents::TierSkipReason::LoggedOut,
                                }],
                            })
                        }),
                        isolation: Some(rimz::config::Isolation::Host),
                        ..Default::default()
                    },
                    state: AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: None,
                    runtime_owner: None,
                    worktree_path: Some(env.project_root.display().to_string()),
                    worktree_branch: None,
                    prompt: None,
                    description: None,
                },
            ))
            .expect("seed durable seat");
        let before = serde_json::to_value(store.read_events().unwrap()).unwrap();
        let output = env
            .rimz()
            .env("CLAUDE_CONFIG_DIR", &provider_home)
            .args(["agents", "explain", "@worker", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(report["action"], "launch");
        assert_eq!(report["action_note"], "no recorded conversation");
        assert_eq!(report["name"], "worker");
        assert_eq!(report["launch_id"], "launch_explain_worker");
        let expected_model = if stamped { "claude-fable-5-1" } else { "opus" };
        assert_eq!(report["model"], expected_model);
        if stamped {
            assert_eq!(report["tier"], "senior");
            assert_eq!(report["tier_skipped"][0]["reason"], "logged_out");
        }
        assert_eq!(report["isolation_source"], "recorded --isolation");
        assert_eq!(report["env"]["RIMZ_AGENT_NAME"], "worker");
        assert!(
            report["provider_argv"]
                .as_array()
                .unwrap()
                .windows(2)
                .any(|pair| pair[0] == "--model" && pair[1] == expected_model)
        );
        for overrides in [
            &["--model", "fable"][..],
            &["--yolo"],
            &["--budget", "1"],
            &["--", "--foo"],
        ] {
            env.rimz()
                .args(["agents", "explain", "@worker"])
                .args(overrides)
                .assert()
                .failure()
                .stderr(contains("overrides apply to profile plans"));
        }
        assert_eq!(
            serde_json::to_value(store.read_events().unwrap()).unwrap(),
            before
        );
        assert!(!env.runtime_paths().prompt_dir().exists());
    }
}

#[cfg(unix)]
#[test]
fn explain_from_a_subagent_reports_the_refusal_and_prints_the_plan() {
    let env = Env::new();
    let config_dir = env.rimz_home();
    std::fs::create_dir_all(&config_dir).expect("mkdir config");
    std::fs::write(
        config_dir.join("config.toml"),
        "[agents]\nisolation = \"host\"\n",
    )
    .expect("write host isolation");
    let launch_id = seed_subagent_caller(&env);

    let output = env
        .rimz()
        .args(["agents", "explain", "claude", "--prompt"])
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(rimz::harness::launch::ENV_AGENT_ID, launch_id.as_str())
        .output()
        .expect("run explain from subagent");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "explain failed: {stderr}");
    assert!(
        stderr.contains("a real launch from here would be refused")
            && stderr.contains("subagents cannot launch"),
        "{stderr}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("<system_reminder>"),
        "explain printed no reminder"
    );
    assert_eq!(
        env.store().read_events().expect("read events").len(),
        1,
        "explain must not append events"
    );
}

#[cfg(unix)]
#[test]
fn explain_refuses_missing_seats_and_multi_agent_layouts_without_state() {
    let env = Env::new();
    let state = env.state_path_for(&env.project_root);
    env.rimz()
        .args(["agents", "explain", "@missing"])
        .assert()
        .failure()
        .stderr(contains("@handle needs a room that has run"));
    env.rimz()
        .args(["agents", "explain", "claude,codex"])
        .assert()
        .failure()
        .stderr(contains("explain describes one agent"));
    assert!(!state.root.exists(), "explain refusal created room state");
}

#[cfg(unix)]
#[test]
fn unresolved_subagent_list_caller_falls_back_to_channel_scope() {
    let env = Env::new();
    let output = env
        .rimz()
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .args(["subagents", "list", "--json"])
        .output()
        .expect("list with stale caller identity");

    assert!(
        output.status.success(),
        "list failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"[]\n");
}

#[cfg(unix)]
#[test]
fn launch_identity_and_parentage_survive_event_log_rotation() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let kind = AgentKind::new_unchecked("codex");
    let agent_id = AgentSessionId::from("provider-rotated-child");
    let launch_id = AgentSessionId::from("launch-rotated-child");
    let parent_id = AgentSessionId::from("provider-parent");
    env.store()
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            &kind,
            AgentLaunchPayload {
                agent_id: agent_id.clone(),
                launch_id: Some(launch_id.clone()),
                agent_name: "rotated-child".to_owned(),
                agent_name_explicit: true,
                launch: LaunchParams {
                    parent_agent_id: Some(parent_id.clone()),
                    parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
                    launch_depth: Some(1),
                    ..Default::default()
                },
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: Some("main".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .expect("seed launched child");

    let outcome = env
        .store()
        .rotate_event_log(1, None)
        .expect("rotate event log");
    assert!(outcome.rotation.is_rotated());
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            workspace.workspace_id,
            &workspace.session_name,
            kind.as_str(),
            "UserPromptSubmit",
            &AgentLifecycleObservation::new(
                Some(agent_id.clone()),
                LifecycleSignal::TurnStarted { turn_id: None },
            ),
        ))
        .expect("append first post-rotation lifecycle event");

    let output = env
        .rimz()
        .args(["subagents", "list", "--json"])
        .env(rimz::harness::launch::ENV_AGENT_KIND, kind.as_str())
        .env(rimz::harness::launch::ENV_AGENT_ID, launch_id.as_str())
        .output()
        .expect("resolve rotated launch identity through subagents list");
    assert!(
        output.status.success(),
        "rotated launch identity did not reach the caller resolver: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let projection = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("read audit projection");
    let child = projection
        .agents
        .iter()
        .find(|agent| agent.agent_id == agent_id)
        .expect("rotated child remains in the audit projection");
    assert_eq!(child.launch_id.as_ref(), Some(&launch_id));
    assert_eq!(child.parent_agent_id.as_ref(), Some(&parent_id));
    assert_eq!(
        child.parent_agent_kind.as_ref(),
        Some(&AgentKind::new_unchecked("claude"))
    );
    assert_eq!(child.launch_depth, Some(1));
}

#[cfg(unix)]
#[test]
fn profile_default_exec_stamps_effective_isolation_not_an_override() {
    use rimz::config::Isolation;
    let env = Env::new();
    std::fs::create_dir_all(env.rimz_home()).unwrap();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = \"sandbox\"\n",
    )
    .unwrap();
    let shim_dir = write_env_dump_shim(&env, "codex");
    let launch_id = "launch_profile_default";
    seed_provisional_agent_launch(&env, launch_id, "pruner");
    let mut request = fresh_exec("codex", None);
    request.isolation_default = Some(Isolation::Host);
    request.identity.name = Some("pruner".to_owned());
    request.identity.launch_id = Some(launch_id.to_owned());
    let dump = env.home_root.join("profile.env");
    env.rimz()
        .args(exec_args(&env, &request))
        .arg("--root")
        .arg(&env.project_root)
        .env("SHELL", "/definitely/not/a/shell")
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .env("TMUX_PANE", "%4")
        .assert_success_within_timeout("profile-default host exec");
    assert!(
        std::fs::read_to_string(dump)
            .unwrap()
            .contains("RIMZ_ISOLATION=host")
    );
    let events = env.store().read_events().unwrap();
    let mut attached = false;
    for event in events {
        match event.kind() {
            EventKind::AgentLaunch(payload) => assert_eq!(payload.launch.isolation, None),
            EventKind::AgentAttach(payload) => {
                attached = true;
                assert_eq!(payload.isolation, None);
                assert_eq!(payload.effective_isolation, Some(Isolation::Host));
            }
            _ => {}
        }
    }
    assert!(attached);
}

#[cfg(unix)]
#[test]
fn resume_exec_attaches_only_the_resumed_session_to_its_pane() {
    let kind = AgentKind::new_unchecked("codex");
    // (wrapped, subagent): a direct-exec resume, a wrapped root resume, a wrapped subagent resume.
    for (wrapped, subagent) in [(false, false), (true, false), (true, true)] {
        let env = Env::new();
        let shim_dir = write_env_dump_shim(&env, "codex");
        let session_id = AgentSessionId::from("sess-resumed");
        let workspace = env.resolve_workspace(&env.project_root);
        env.store()
            .append_event(&EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                kind.as_str(),
                "SessionStart",
                &AgentLifecycleObservation::new(
                    Some(session_id.clone()),
                    LifecycleSignal::Registered,
                ),
            ))
            .expect("seed resumed session");
        env.store()
            .append_event(&EventEnvelope::agent_lifecycle(
                workspace.workspace_id,
                &workspace.session_name,
                kind.as_str(),
                "SessionEnd",
                &AgentLifecycleObservation::new(Some(session_id.clone()), LifecycleSignal::Ended),
            ))
            .unwrap();

        let dump = env.home_root.join("codex-resume.env");
        let mut resume = ExecRequest {
            isolation_default: None,
            kind: kind.clone(),
            action: ExecAction::Resume {
                session_id: session_id.to_string(),
                extra_args: Vec::new(),
            },
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            team_prompt: None,
            skills: None,
            allowed_tools: None,
            provider_account: ProviderAccountState::Unbound,
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: false,
            exit_on_run_completion: false,
            subagent: false,
            identity: ExecIdentity::default(),
        };
        resume.identity.params.isolation = Some(rimz::config::Isolation::Host);
        resume.close_pane_on_exit = wrapped && !subagent;
        if subagent {
            let mut run = rimz::store::run::RunRecord::new(
                env.workspace_id.clone(),
                kind.clone(),
                rimz::agents::PermissionMode::Auto,
                "finished task".into(),
                env.project_root.clone(),
            );
            run.agent_id = Some(session_id.clone());
            run.subagent = true;
            run.status = rimz::store::run::RunStatus::Completed;
            run.joined_at = Some(jiff::Timestamp::now());
            rimz::harness::run::create(env.store().paths(), &run).unwrap();
            resume.run_id = Some(run.run_id);
            resume.subagent = true;
        }
        env.rimz()
            .args(exec_args(&env, &resume))
            .arg("--root")
            .arg(&env.project_root)
            .env("SHELL", "/definitely/not/a/shell")
            .env("PATH", path_with_front(&shim_dir))
            .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
            .env("TMUX_PANE", "%4")
            .assert_success_within_timeout("codex resume attach");

        let store = env.store();
        let events = store.read_events().unwrap();
        let lifecycle = events
            .iter()
            .skip(2)
            .filter_map(|event| match event.kind() {
                EventKind::AgentAttach(_) => Some("attach".to_owned()),
                EventKind::AgentLifecycle(payload) => {
                    let observation = payload.observation;
                    assert_eq!(observation.agent_id.as_ref(), Some(&session_id));
                    match payload.event_name.as_deref() {
                        Some("rimz.agent-resumed") => {
                            assert_eq!(observation.signal, LifecycleSignal::Registered)
                        }
                        Some("rimz.agent-ended") => {
                            assert_eq!(observation.signal, LifecycleSignal::Ended)
                        }
                        _ => panic!("unexpected lifecycle event"),
                    }
                    payload.event_name
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let expected = match (wrapped, subagent) {
            (true, true) => vec!["attach", "attach", "rimz.agent-resumed", "rimz.agent-ended"],
            (true, false) => vec!["attach", "attach", "rimz.agent-ended"],
            (false, _) => vec!["attach"],
        };
        assert_eq!(lifecycle, expected);
        let attaches = store
            .read_events()
            .expect("read events")
            .into_iter()
            .filter_map(|event| match event.kind() {
                EventKind::AgentAttach(payload) => Some(payload),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(attaches.len(), if wrapped { 2 } else { 1 });
        let attach = &attaches[0];
        assert_eq!(attach.agent_id, session_id);
        assert_eq!(attach.isolation, Some(rimz::config::Isolation::Host));
        assert_eq!(
            attach.effective_isolation,
            Some(rimz::config::Isolation::Host)
        );
        assert_eq!(attach.pane_id.as_str(), "tmux:%4");
        assert_eq!(attach.pane_pid, Some(attach.runtime_owner.pid));
        assert_ne!(attach.runtime_owner.pid, 0);
        assert_eq!(
            attach.runtime_owner.kind,
            rimz::pane::RuntimeOwnerKind::Agent
        );
        assert_eq!(attach.runtime_owner.subject_id, "sess-resumed");
    }

    for action in [
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
        ExecAction::Fork {
            session_id: "sess-source".to_owned(),
            extra_args: Vec::new(),
        },
    ] {
        let env = Env::new();
        let shim_dir = write_env_dump_shim(&env, "codex");
        let dump = env.home_root.join("codex-no-attach.env");
        let request = ExecRequest {
            isolation_default: None,
            kind: kind.clone(),
            action,
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            team_prompt: None,
            skills: None,
            allowed_tools: None,
            provider_account: ProviderAccountState::Unbound,
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: false,
            exit_on_run_completion: false,
            subagent: false,
            identity: ExecIdentity::default(),
        };
        env.rimz()
            .args(exec_args(&env, &request))
            .arg("--root")
            .arg(&env.project_root)
            .env("SHELL", "/definitely/not/a/shell")
            .env("PATH", path_with_front(&shim_dir))
            .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
            .env("TMUX_PANE", "%4")
            .assert_success_within_timeout("codex non-resume exec");
        assert!(
            env.store()
                .read_events()
                .expect("read non-resume events")
                .iter()
                .all(|event| !matches!(event.kind(), EventKind::AgentAttach(_)))
        );
        let is_resume_stamp = |event: &EventEnvelope| {
            matches!(event.kind(), EventKind::AgentLifecycle(payload)
                if payload.event_name.as_deref() == Some("rimz.agent-resumed"))
        };
        assert!(
            !env.store()
                .read_events()
                .unwrap()
                .iter()
                .any(is_resume_stamp)
        );
    }
}

#[cfg(unix)]
#[test]
fn resume_exec_waits_for_turn_started_before_self_cleanup() {
    resume_exec_cleanup_case(false);
}

#[cfg(unix)]
#[test]
fn resume_exec_archives_queued_messages_after_provider_exit() {
    resume_exec_cleanup_case(true);
}

#[cfg(unix)]
fn resume_exec_cleanup_case(queue_before_exit: bool) {
    use notify::Watcher as _;
    use std::time::{Duration, Instant};

    // Like run::WaitResolutionWatch, this needs open notifications, which kqueue lacks.
    if !cfg!(target_os = "linux") {
        tracing::warn!("skipping: observing the wrapper's run reads requires Linux inotify");
        return;
    }
    let env = Env::new();
    let store = env.store();
    let workspace = env.resolve_workspace(&env.project_root);
    let kind = AgentKind::new_unchecked("codex");
    let session_id = AgentSessionId::from("sess-resumed");
    store
        .append_event(&EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "codex",
            "SessionStart",
            &AgentLifecycleObservation::new(Some("parent".into()), LifecycleSignal::Registered),
        ))
        .unwrap();
    let observe = |name, signal| {
        let mut observation = AgentLifecycleObservation::new(Some(session_id.clone()), signal);
        observation.agent_name = Some("otter".into());
        observation.launch.parent_agent_id = Some("parent".into());
        observation.launch.parent_agent_kind = Some(kind.clone());
        observation.launch.launch_depth = Some(1);
        store
            .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                session_name: &workspace.session_name,
                agent_kind: kind.clone(),
                event_name: name,
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
        observation
    };
    observe("SessionStart", LifecycleSignal::Registered);
    observe("SessionEnd", LifecycleSignal::Ended);
    let mut run = rimz::store::run::RunRecord::new(
        env.workspace_id.clone(),
        kind.clone(),
        rimz::agents::PermissionMode::Auto,
        "finished task".into(),
        env.project_root.clone(),
    );
    run.agent_id = Some(session_id.clone());
    run.subagent = true;
    run.status = rimz::store::run::RunStatus::Completed;
    run.joined_at = Some(jiff::Timestamp::now());
    rimz::harness::run::create(store.paths(), &run).unwrap();

    let shim_dir = write_env_dump_shim(&env, "codex");
    // No natural provider exit may masquerade as the wrapper's self-cleanup.
    std::fs::write(shim_dir.join("codex"), "#!/bin/sh\nexec sleep 300\n").unwrap();
    std::os::unix::fs::symlink(crate::common::zellij_trace_shim(), shim_dir.join("zellij"))
        .unwrap();
    let mut resume = ExecRequest {
        isolation_default: None,
        kind: kind.clone(),
        action: ExecAction::Resume {
            session_id: session_id.to_string(),
            extra_args: Vec::new(),
        },
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: None,
        allowed_tools: None,
        provider_account: ProviderAccountState::Unbound,
        run_id: Some(run.run_id.clone()),
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: true,
        subagent: true,
        identity: ExecIdentity::default(),
    };
    resume.identity.params.isolation = Some(rimz::config::Isolation::Host);
    let mut wrapper = env
        .rimz()
        .args(exec_args(&env, &resume))
        .args([
            "--root",
            env.project_root.to_str().unwrap(),
            "--mux",
            "zellij",
        ])
        .env("SHELL", "/definitely/not/a/shell")
        .env("PATH", path_with_front(&shim_dir))
        .env("ZELLIJ_PANE_ID", "4")
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("zellij.log"))
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", &workspace.session_name)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while rimz::harness::run::load(store.paths(), &run.run_id)
        .unwrap()
        .provider_pid
        .is_none()
    {
        assert!(Instant::now() < deadline, "provider was not recorded");
        std::thread::sleep(Duration::from_millis(10));
    }

    let (tx, reads) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx).unwrap();
    watcher
        .watch(&store.paths().runs_dir, notify::RecursiveMode::NonRecursive)
        .unwrap();
    let run_file = store.paths().runs_dir.join(format!("{}.json", run.run_id));
    // After provider registration there is at most one setup read. Without the hold, the first poll and exit settlement add only four reads (no parent/report fleet).
    // Six opens therefore prove repeated live monitor polls, not just spawn or teardown.
    let mut opens = 0;
    while opens < 6 && wrapper.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "wrapper did not poll the terminal run"
        );
        match reads.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => {
                let event = event.unwrap();
                if matches!(
                    event.kind,
                    notify::EventKind::Access(notify::event::AccessKind::Open(_))
                ) && event.paths.contains(&run_file)
                {
                    opens += 1;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("run watcher disconnected: {error}"),
        }
    }
    drop(watcher);

    let started = observe(
        "TurnStarted",
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    rimz::harness::run::record_lifecycle(
        store.paths(),
        &run.run_id,
        kind.as_str(),
        &started,
        None,
        || None,
    )
    .unwrap();
    let reopened = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(reopened.status, rimz::store::run::RunStatus::Running);
    assert_eq!(reopened.follow_ups, run.follow_ups + 1);
    let child = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| agent.agent_id == session_id)
        .unwrap();
    let queued = rimz::store::message::MessageRecord::new(
        env.workspace_id.clone(),
        &child,
        "follow up after this turn".into(),
        rimz::store::message::DeliveryGate::Done,
    );
    let later = rimz::store::message::MessageRecord::new(
        env.workspace_id.clone(),
        &child,
        "later follow up".into(),
        rimz::store::message::DeliveryGate::Done,
    )
    .with_not_before(Some(jiff::Timestamp::now() + Duration::from_secs(3600)));
    if queue_before_exit {
        for record in [&queued, &later] {
            store
                .queue_message(record, &workspace.session_name)
                .unwrap();
        }
    }
    let ended = observe(
        "TurnEnded",
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    rimz::harness::run::record_lifecycle(
        store.paths(),
        &run.run_id,
        kind.as_str(),
        &ended,
        None,
        || None,
    )
    .unwrap();
    rimz::harness::run::report::join_and_settle_digest(
        &store,
        &workspace.session_name,
        &run.run_id,
        None,
        "test parent received answer",
    )
    .unwrap();
    if queue_before_exit {
        let provider_pid = rimz::harness::run::load(store.paths(), &run.run_id)
            .unwrap()
            .provider_pid
            .unwrap();
        assert!(rimz::child_process::signal_process_term(provider_pid, None));
    }
    while wrapper.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "wrapper did not self-clean up after completion"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let lifecycle = store
        .read_events()
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.kind() {
            EventKind::AgentLifecycle(payload)
                if payload.observation.agent_id.as_ref() == Some(&session_id) =>
            {
                payload.event_name
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let resumed = lifecycle
        .iter()
        .position(|name| name == "rimz.agent-resumed")
        .unwrap();
    assert_eq!(
        &lifecycle[resumed..],
        [
            "rimz.agent-resumed",
            "TurnStarted",
            "TurnEnded",
            "rimz.agent-ended"
        ],
        "self-cleanup must wait for the resumed child's TurnStarted"
    );
    if !queue_before_exit {
        return;
    }
    let panes = env.write_pane_fixture(&[]);
    env.rimz()
        .args(["message", "sweep"])
        .env("RIMZ_TEST_PANE_LIST", &panes)
        .assert()
        .success();
    let history = store.list_message_history().unwrap();
    for record in [&queued, &later] {
        let archived = history
            .iter()
            .find(|row| row.message_id == record.message_id)
            .expect("ended receiver message archived");
        assert_eq!(
            archived.status,
            rimz::store::message::MessageStatus::Archived
        );
        assert_eq!(
            archived.last_error.as_deref(),
            Some("receiver ended; rimz message @otter resumes it")
        );
    }
    env.rimz()
        .args(["message", "show", queued.message_id.as_str()])
        .assert()
        .success()
        .stdout(contains("receiver ended; rimz message @otter resumes it"));
}

#[cfg(unix)]
#[test]
fn shell_rc_env_reaches_the_spawned_agent() {
    let env = Env::new();
    let shell = write_fake_login_shell(
        &env,
        "rimz-test-sh",
        &[("RIMZ_TEST_RC_MARKER", "from-shell")],
    );
    let shim_dir = write_env_dump_shim(&env, "codex");
    let dump = env.home_root.join("codex-shell.env");

    env.rimz()
        .args(exec_args(&env, &fresh_exec("codex", None)))
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("codex shell rc launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "RIMZ_TEST_RC_MARKER=from-shell"),
        "agent process env misses the shell rc marker:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn bashrc_path_reaches_the_spawned_agent() {
    let env = Env::new();
    let shell = write_fake_bash_shell(&env);
    let shim_dir = write_env_dump_shim(&env, "codex");
    std::fs::write(
        env.home_root.join(".bashrc"),
        format!(
            "export PATH='{}':\"$PATH\"\nexport RIMZ_TEST_BASHRC_MARKER=from-bashrc\n",
            shim_dir.display()
        ),
    )
    .expect("write bashrc");
    let dump = env.home_root.join("codex-bashrc.env");

    env.rimz()
        .args(exec_args(&env, &fresh_exec("codex", None)))
        .env("SHELL", &shell)
        .env("PATH", "/usr/bin:/bin")
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("codex bashrc launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "RIMZ_TEST_BASHRC_MARKER=from-bashrc"),
        "agent process env misses the bashrc marker:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn adapter_preserves_agent_view_shell_env() {
    let env = Env::new();
    let shell = write_fake_login_shell(
        &env,
        "rimz-test-sh",
        &[("CLAUDE_CODE_DISABLE_AGENT_VIEW", "0")],
    );
    let shim_dir = write_env_dump_shim(&env, "claude");
    let dump = env.home_root.join("claude-shell.env");

    env.rimz()
        .args(exec_args(&env, &fresh_exec("claude", None)))
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("claude agent-view env launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "CLAUDE_CODE_DISABLE_AGENT_VIEW=0"),
        "claude launch env did not preserve the shell value:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn trusted_agent_env_overrides_shell_rc_env() {
    let env = Env::new();
    env.write_config(
        &env.project_root,
        "[[agents]]\nname = \"codex\"\nenv = { RIMZ_TEST_CONFIGURED = \"trusted\" }\n",
    );
    env.rimz().args(["trust", "grant"]).assert().success();
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[("RIMZ_TEST_CONFIGURED", "rc")]);
    let shim_dir = write_env_dump_shim(&env, "codex");
    let dump = env.home_root.join("codex-trusted-shell.env");

    env.rimz()
        .args(exec_args(&env, &fresh_exec("codex", None)))
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("codex trusted env launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "RIMZ_TEST_CONFIGURED=trusted"),
        "trusted launch env did not override the shell rc value:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn missing_shell_path_falls_back_to_direct_exec() {
    let env = Env::new();
    let shim_dir = write_env_dump_shim(&env, "codex");
    let dump = env.home_root.join("codex-direct.env");

    env.rimz()
        .args(exec_args(&env, &fresh_exec("codex", None)))
        .env("SHELL", "/definitely/not/a/shell")
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("codex direct launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped.lines().any(|line| line == "ARGC=3") && dumped.contains("<system_reminder>"),
        "direct fallback did not run the agent shim:\n{dumped}"
    );
}

/// An invalid explicit `--new-pane` (here a multi-cell layout) refuses the
/// whole launch before any side effect, so it leaves no provisional store rows
/// and never creates the requested worktree. Resolution runs ahead of the
/// live-session probe, so the rejection needs neither a running room nor a mux.
#[cfg(unix)]
#[test]
fn invalid_new_pane_refuses_an_agents_launch_before_side_effects() {
    let env = Env::new();

    env.rimz()
        .args(["agents", "claude,codex", "--worktree=wt-a", "--new-pane"])
        .assert()
        .failure()
        .stderr(contains("single agent cell"));

    assert!(
        !env.home_root
            .join("project-worktrees")
            .join("wt-a")
            .exists(),
        "a rejected --new-pane must not create the worktree",
    );
    assert!(
        !env.state_path_for(&env.project_root).events_log.exists(),
        "a rejected --new-pane must not append launch events",
    );
}

#[cfg(unix)]
#[test]
fn unreadable_machine_config_blocks_worktree_launch_before_store_events() {
    let env = Env::new();
    if !init_launch_repo(&env.project_root) {
        return;
    }
    env.install_agent_hooks("claude");
    std::fs::create_dir_all(env.rimz_home()).unwrap();
    let config = env.rimz_home().join("config.toml");
    std::fs::write(
        &config,
        "[agents.worktree.hooks]\ncreated = touch HOOKRAN\n",
    )
    .unwrap();
    env.rimz()
        .args(["agents", "claude,codex", "--new-pane"])
        .assert()
        .failure()
        .stderr(contains("single agent cell"));
    for args in [
        vec!["agents", "claude", "-p", "test hook", "-w", "demo"],
        vec!["agents", "claude", "-w", "demo", "--fresh"],
        vec!["agents", "claude", "-p", "test hook", "--from-pr", "1"],
        vec!["agents", "claude", "--from-pr", "1"],
    ] {
        env.rimz()
            .args(args)
            .assert()
            .failure()
            .stderr(contains("cannot create a worktree"))
            .stderr(contains("TOML error"))
            .stderr(contains(config.display().to_string()));
        assert!(!env.state_path_for(&env.project_root).events_log.exists());
        assert!(!env.home_root.join("project-worktrees").exists());
    }
}

#[cfg(unix)]
#[test]
fn failed_created_hook_blocks_launch_before_store_events() {
    let env = Env::new();
    if !init_launch_repo(&env.project_root) {
        return;
    }
    env.install_agent_hooks("claude");
    std::fs::create_dir_all(env.rimz_home()).unwrap();
    std::fs::write(env.rimz_home().join("config.toml"), "[agents]\nisolation = 'host'\n[agents.worktree.hooks]\ncreated = 'echo launch-hook-failed >&2; exit 3'\n").unwrap();
    env.rimz()
        .args(["agents", "claude", "-p", "test hook", "-w", "demo"])
        .assert()
        .failure()
        .stderr(contains("launch-hook-failed"));
    assert!(!env.state_path_for(&env.project_root).events_log.exists());
    assert!(!env.home_root.join("project-worktrees/demo").exists());
}

#[cfg(unix)]
#[test]
fn fresh_launch_requires_a_named_worktree_before_side_effects() {
    let env = Env::new();
    for args in [
        vec!["agents", "claude,codex", "--fresh"],
        vec!["agents", "claude,codex", "--fresh", "-w"],
    ] {
        env.rimz()
            .args(args)
            .assert()
            .failure()
            .stderr(contains("--fresh needs a named worktree (-w NAME)"));
    }
    assert!(!env.state_path_for(&env.project_root).events_log.exists());
    assert!(!env.home_root.join("project-worktrees").exists());
}

#[cfg(unix)]
#[test]
fn supervised_unmanaged_worktree_requires_terminal_confirmation() {
    let env = Env::new();
    if !init_launch_repo(&env.project_root) {
        return;
    }
    let path = env.home_root.join("project-worktrees/review");
    assert!(
        std::process::Command::new("git")
            .current_dir(&env.project_root)
            .args(["worktree", "add", "-b", "review"])
            .arg(&path)
            .status()
            .expect("add worktree")
            .success()
    );
    env.rimz()
        .args(["agents", "codex", "-p", "review this", "-w", "review"])
        .stdin(std::process::Stdio::null())
        .assert()
        .failure()
        .stderr(contains("rerun in a terminal to confirm entering it"));
    assert!(
        rimz::worktree::read_marker_for_worktree(&path)
            .expect("marker")
            .is_none()
    );
    assert!(env.store().snapshot().expect("snapshot").agents.is_empty());
}

#[cfg(unix)]
#[test]
fn supervised_cross_repo_worktree_refuses_non_terminal_input() {
    let env = Env::new();
    let current_root = env.home_root.join("current");
    if !init_launch_repo(&env.project_root) || !init_launch_repo(&current_root) {
        tracing::warn!("skipping: git unavailable");
        return;
    }
    let room_root = canonical(&env.project_root);
    let current_root = canonical(&current_root);
    let workspace_id = rimz::WorkspaceId::from_project_root(&room_root);

    env.rimz()
        .current_dir(&current_root)
        .env(rimz::workspace::ENV_WORKSPACE_ID, workspace_id.as_str())
        .env(rimz::workspace::ENV_PROJECT_ROOT, &room_root)
        .stdin(std::process::Stdio::null())
        .args([
            "agents",
            "codex",
            "-p",
            "fix the parser",
            "--worktree=supervised-cross-root",
        ])
        .assert()
        .failure()
        .stderr(contains(format!("room root: {}", room_root.display())))
        .stderr(contains(format!(
            "current git root: {}",
            current_root.display()
        )))
        .stderr(contains(format!("--root {}", current_root.display())));

    assert!(
        !env.home_root
            .join("current-worktrees")
            .join("supervised-cross-root")
            .exists(),
        "a non-terminal mismatch must refuse before creating the worktree",
    );
}

#[cfg(unix)]
#[test]
fn ambiguous_prompt_leader_refuses_before_side_effects() {
    let env = Env::new();

    env.rimz()
        .args(["agents", "claude,claude", "do the thing"])
        .assert()
        .failure()
        .stderr(contains("this layout has several `claude` cells"))
        .stderr(contains(
            "give the first cell an inline role (`claude:lead,claude`)",
        ));

    assert!(
        !env.state_path_for(&env.project_root).events_log.exists(),
        "an ambiguous leader must not append launch events",
    );
}

#[cfg(unix)]
#[test]
fn prompt_without_an_agent_cell_refuses_before_side_effects() {
    let env = Env::new();

    env.rimz()
        .args(["agents", "term", "do the thing"])
        .assert()
        .failure()
        .stderr(contains(
            "this layout has no agent cell to receive a prompt",
        ));

    assert!(
        !env.state_path_for(&env.project_root).events_log.exists(),
        "a missing prompt target must not append launch events",
    );
}

#[cfg(unix)]
#[test]
fn resume_with_empty_store_refuses_before_mux_probe() {
    let env = Env::new();

    env.rimz()
        .args(["agents", "claude", "--resume"])
        .assert()
        .failure()
        .stderr(contains(
            "nothing to resume for `claude`; launch without `--resume`",
        ));
}

#[cfg(unix)]
#[test]
fn prompt_with_shell_metacharacters_stays_one_argument_after_terminator() {
    let env = Env::new();
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let shim_dir = write_env_dump_shim(&env, "codex");
    let dump = env.home_root.join("codex-prompt.env");
    let mut prompt = r#"say "hello there"; $HOME `whoami` \\ with spaces "#.repeat(1024);
    prompt.truncate(40 * 1024);
    assert_eq!(prompt.len(), 40 * 1024);
    let argv = rimz::harness::launch::exec_argv(
        &env.rimz_bin(),
        &env.runtime_paths(),
        &fresh_exec("codex", Some(&prompt)),
    )
    .expect("encode large prompt");

    env.rimz()
        .args(argv.into_iter().skip(1))
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("codex prompt launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped.lines().any(|line| line == "ARGC=5"),
        "launch argv did not contain the reminder, terminator, and prompt:\n{dumped}"
    );
    assert!(
        dumped.lines().any(|line| line == "ARGV_4=--"),
        "launch argv did not protect the prompt with --:\n{dumped}"
    );
    assert!(
        dumped
            .lines()
            .any(|line| line == format!("ARGV_5={prompt}")),
        "prompt argv element was changed by the shell wrapper:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn launch_prompt_artifact_round_trips_and_missing_file_fails() {
    use rimz::harness::launch::{ExecWireErr, decode_exec_request, exec_argv};

    let env = Env::new();
    let runtime = env.runtime_paths();
    for prompt in [String::new(), "line with \"quotes\"\n雪\r\n".repeat(2048)] {
        let request = fresh_exec("codex", Some(&prompt));
        let argv = exec_argv(&env.rimz_bin(), &runtime, &request).expect("exec argv");
        let payload = argv.last().expect("payload");
        assert!(
            payload.len() < 4096,
            "prompt must not travel through mux argv"
        );
        let wire: serde_json::Value = serde_json::from_str(payload).expect("wire JSON");
        assert!(wire["action"]["prompt"].is_null());
        let path = std::path::Path::new(wire["prompt_file"].as_str().expect("prompt path"));
        assert_eq!(path.parent(), Some(runtime.prompt_dir().as_path()));
        assert!(
            path.file_name()
                .expect("name")
                .to_string_lossy()
                .starts_with("task.")
        );
        assert_eq!(
            std::fs::read_to_string(path).expect("prompt contents"),
            prompt
        );
        assert_eq!(
            decode_exec_request("codex", None, payload).expect("decode"),
            request
        );

        std::fs::remove_file(path).expect("remove artifact");
        assert!(matches!(
            decode_exec_request("codex", None, payload),
            Err(ExecWireErr::PromptRead { .. })
        ));
        assert!(matches!(
            decode_exec_request("claude", None, payload),
            Err(ExecWireErr::KindMismatch { .. })
        ));
        assert!(matches!(
            decode_exec_request("codex", Some(&env.project_root), payload),
            Err(ExecWireErr::WorktreeMismatch)
        ));

        let mut invalid = wire;
        invalid["action"] =
            serde_json::json!({"action": "resume", "session_id": "session", "extra_args": []});
        assert!(matches!(
            decode_exec_request("codex", None, &invalid.to_string()),
            Err(ExecWireErr::Parse(_))
        ));
    }
    std::fs::remove_dir(runtime.prompt_dir()).expect("empty artifact directory");
    std::fs::write(runtime.prompt_dir(), "not a directory").expect("block artifact writes");
    assert!(matches!(
        exec_argv(
            &env.rimz_bin(),
            &runtime,
            &fresh_exec("codex", Some("prompt"))
        ),
        Err(ExecWireErr::PromptWrite(_))
    ));
}

#[cfg(unix)]
#[test]
fn unsupported_profile_skills_refuse_before_launch_and_run_records() {
    for supervised in [false, true] {
        let env = Env::new();
        let config_dir = env.rimz_home();
        std::fs::create_dir_all(&config_dir).expect("mkdir config");
        std::fs::write(
            config_dir.join("config.toml"),
            "[agents]\nisolation = \"sandbox\"\n",
        )
        .expect("write unsupported skills profile");
        crate::common::write_definition(
            &env,
            "agents",
            "worker",
            "description: Worker\nagent: amp\nskills: []",
            "",
        );
        let mut command = env.rimz();
        command.args(["agents", "worker", "hello"]);
        if supervised {
            command.args(["-p", "--bg"]);
        }
        command.assert().failure().stderr(contains(
            "provider amp cannot mark skills user-only; remove the profile skills list",
        ));

        let paths = env.state_path_for(&env.project_root);
        assert!(
            !paths.runs_dir.exists()
                || std::fs::read_dir(&paths.runs_dir)
                    .expect("read runs")
                    .next()
                    .is_none(),
            "unsupported skills must not create a run record (supervised={supervised})",
        );
        assert!(
            env.store().read_events().expect("read events").is_empty(),
            "unsupported skills must not append launch events (supervised={supervised})",
        );
    }
}

#[cfg(unix)]
#[test]
fn oversized_prompt_refuses_before_launch_and_run_records() {
    for supervised in [false, true] {
        let env = Env::new();
        let prompt = "x".repeat(120 * 1024 + 1);
        let mut command = env.rimz();
        command.args(["agents", "codex", &prompt]);
        if supervised {
            command.args(["-p", "--bg"]);
        }
        command
            .assert()
            .failure()
            .stderr(contains("122881 bytes"))
            .stderr(contains("122880-byte argv safety limit"));

        let paths = env.state_path_for(&env.project_root);
        assert!(
            !paths.runs_dir.exists()
                || std::fs::read_dir(&paths.runs_dir)
                    .expect("read runs")
                    .next()
                    .is_none(),
            "oversized prompt must not create a run record (supervised={supervised})",
        );
        assert!(
            env.store().read_events().expect("read events").is_empty(),
            "oversized prompt must not append launch events (supervised={supervised})",
        );
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn exec_prompt_failures_fail_provisional_launch_and_release_run_waiter() {
    use rimz::harness::run::RunCancellation;
    use rimz::harness::run_wake::{ExpectedRunFrame, RunWaiter};
    use rimz::store::run::{RunRecord, RunStatus};

    for missing_artifact in [true, false] {
        let env = Env::new();
        let store = env.store();
        let launch_id = "launch_prompt_failure";
        seed_provisional_agent_launch(&env, launch_id, "pruner");
        let prompt = if missing_artifact {
            "prompt".to_owned()
        } else {
            "x".repeat(120 * 1024 + 1)
        };
        let record = RunRecord::new(
            env.workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            rimz::agents::PermissionMode::Auto,
            prompt.clone(),
            env.project_root.clone(),
        );
        rimz::harness::run::create(store.paths(), &record).expect("pending run");
        let waiter = RunWaiter::bind(
            store.runtime_paths(),
            ExpectedRunFrame {
                workspace_id: env.workspace_id.clone(),
                run_id: record.run_id.clone(),
            },
            RunCancellation::new(),
        )
        .expect("run waiter");
        let mut request = fresh_exec("codex", Some(&prompt));
        request.run_id = Some(record.run_id.clone());
        request.identity.name = Some("pruner".to_owned());
        request.identity.launch_id = Some(launch_id.to_owned());
        let argv = exec_args(&env, &request);
        if missing_artifact {
            let wire: serde_json::Value =
                serde_json::from_str(argv.last().expect("payload")).expect("wire");
            std::fs::remove_file(wire["prompt_file"].as_str().expect("artifact path"))
                .expect("remove prompt artifact");
        }
        let output = env
            .rimz()
            .args(argv)
            .bounded_output()
            .expect("wrapper exits");
        assert!(!output.status.success());
        let expected = if missing_artifact {
            "reading launch prompt"
        } else {
            "122880-byte argv safety limit"
        };
        assert!(String::from_utf8_lossy(&output.stderr).contains(expected));
        let terminal = waiter
            .wait_terminal(&store, Some(std::time::Duration::from_secs(1)), None)
            .await
            .expect("parent unblocks");
        assert_eq!(
            terminal.status,
            RunStatus::Failed,
            "failure must not leave the parent waiting until timeout"
        );
        assert!(store.read_events().expect("launch events").iter().any(|event| matches!(
            event.kind(), EventKind::AgentLaunch(ref payload) if payload.state == AgentLaunchState::Failed && payload.agent_id == launch_id
        )));
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn exec_failed_definition_persists_detail_before_releasing_run_waiter() {
    use rimz::harness::run::RunCancellation;
    use rimz::harness::run_wake::{ExpectedRunFrame, RunWaiter};
    use rimz::store::run::{RunRecord, RunStatus};

    let env = Env::new();
    std::fs::create_dir_all(env.rimz_home()).expect("config directory");
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = \"sandbox\"\n",
    )
    .expect("sandbox config");
    crate::common::write_definition(
        &env,
        "agents",
        "worker",
        "description: Worker\nagent: codex\ntools: [Skill]\nskills: [missing]",
        "Worker prompt.",
    );
    let store = env.store();
    let launch_id = "launch_definition_failure";
    seed_provisional_agent_launch(&env, launch_id, "pruner");
    let record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        rimz::agents::PermissionMode::Auto,
        "prompt".to_owned(),
        env.project_root.clone(),
    );
    rimz::harness::run::create(store.paths(), &record).expect("pending run");
    let waiter = RunWaiter::bind(
        store.runtime_paths(),
        ExpectedRunFrame {
            workspace_id: env.workspace_id.clone(),
            run_id: record.run_id.clone(),
        },
        RunCancellation::new(),
    )
    .expect("run waiter");
    let mut request = fresh_exec("codex", Some("prompt"));
    request.run_id = Some(record.run_id.clone());
    request.identity.name = Some("pruner".to_owned());
    request.identity.launch_id = Some(launch_id.to_owned());
    request.identity.params.profile = Some("worker".to_owned());
    let output = env
        .rimz()
        .args(exec_args(&env, &request))
        .env("PATH", "/nonexistent")
        .bounded_output()
        .expect("wrapper exits before bubblewrap preflight");
    assert!(!output.status.success());
    let terminal = waiter
        .wait_terminal(&store, Some(std::time::Duration::from_secs(1)), None)
        .await
        .expect("parent unblocks");
    assert_eq!(terminal.status, RunStatus::Failed);
    let detail = terminal.failure_tail.expect("definition failure detail");
    assert!(detail.contains("agents/worker.md:"), "{detail}");
    assert!(detail.contains("missing"), "{detail}");
    assert!(String::from_utf8_lossy(&output.stderr).contains(&detail));
    assert!(store.read_events().expect("launch events").iter().any(|event| matches!(
        event.kind(), EventKind::AgentLaunch(ref payload) if payload.state == AgentLaunchState::Failed && payload.agent_id == launch_id
    )));
}

#[cfg(unix)]
#[test]
fn close_pane_exec_reports_startup_failure_before_dropping_to_shell() {
    let env = Env::new();
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let shim_dir = write_failing_agent_shim(&env, "codex", 7);
    let idle_shell_marker = env.home_root.join("idle-shell.marker");
    let launch_id = "launch_startup_failure";
    seed_provisional_agent_launch(&env, launch_id, "pruner");

    let mut request = fresh_exec("codex", None);
    request.close_pane_on_exit = true;
    request.identity = ExecIdentity {
        name: Some("pruner".to_owned()),
        launch_id: Some(launch_id.to_owned()),
        params: LaunchParams {
            team: Some("trim".to_owned()),
            role: Some("pruner".to_owned()),
            ..LaunchParams::default()
        },
        ..ExecIdentity::default()
    };
    let output = env
        .rimz()
        .args(exec_args(&env, &request))
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_IDLE_SHELL_MARKER", &idle_shell_marker)
        .bounded_output()
        .expect("agents exec returns without waiting on non-tty stdin");

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&idle_shell_marker).expect("idle shell marker"),
        "idle shell\n"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("failed to start"), "{stderr}");
    assert!(stderr.contains("exit status: 7"), "{stderr}");
    assert!(stderr.contains("rimz agents trim.pruner"), "{stderr}");
}

#[cfg(unix)]
fn seed_provisional_agent_launch(env: &Env, launch_id: &str, agent_name: &str) {
    let workspace = env.resolve_workspace(&env.project_root);
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
                role: Some("pruner".to_owned()),
                model: None,
                effort: None,
                budget: None,
                team: Some("trim".to_owned()),
                launch_group: None,
                launch_ordinal: None,
                channel: None,
                kind_ordinal: Some(1),
                ..LaunchParams::default()
            },
            state: AgentLaunchState::Starting,
            run_id: None,
            pane_id: None,
            runtime_owner: None,
            worktree_path: Some(env.project_root.display().to_string()),
            worktree_branch: None,
            prompt: None,
            description: None,
        },
    );
    env.store().append_event(&event).expect("append launch");
}

#[cfg(unix)]
#[test]
fn host_skill_errors_refuse_before_launch_without_writes() {
    let env = Env::new();
    let config = env.project_root.join(".rimz");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[profiles.worker]\nagent = \"claude\"\nskills = [\"not-installed\"]\n",
    )
    .unwrap();
    env.rimz().args(["trust", "grant"]).assert().success();
    let before = env.store().read_events().unwrap();
    let output = env
        .rimz()
        .current_dir(&env.project_root)
        .args(["agents", "worker", "--isolation", "host"])
        .bounded_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("unknown skill 'not-installed'"), "{stderr}");
    assert_eq!(env.store().read_events().unwrap().len(), before.len());
    assert!(!env.runtime_paths().prompt_dir().exists());
}

#[cfg(unix)]
#[test]
fn profile_isolation_is_preflighted_before_launch() {
    let env = Env::new();
    std::fs::create_dir_all(env.rimz_home().join("agents")).unwrap();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = \"host\"\n",
    )
    .unwrap();
    std::fs::write(
        env.rimz_home().join("agents/codex.md"),
        "---\ndescription: Base.\n---\nBase.",
    )
    .unwrap();
    std::fs::write(
        env.rimz_home().join("agents/boxed.md"),
        "---\ndescription: Boxed.\nagent: codex\nisolation: sandbox\ntools: [Bash]\n---\n",
    )
    .unwrap();
    let shim_dir = write_env_dump_shim(&env, "codex");
    let bwrap = shim_dir.join("bwrap");
    std::fs::write(
        &bwrap,
        "#!/bin/sh\necho profile-bwrap-refuses >&2\nexit 1\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bwrap, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = env
        .rimz()
        .current_dir(&env.project_root)
        .env("PATH", path_with_front(&shim_dir))
        .args(["agents", "boxed"])
        .bounded_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("profile-bwrap-refuses"), "{stderr}");
    assert!(env.store().read_events().unwrap().is_empty());
    std::fs::write(
        env.rimz_home().join("agents/boxed.md"),
        "---\ndescription: Boxed.\nagent: codex\nisolation: host\ntools: [Bash]\n---\n",
    )
    .unwrap();
    let output = env
        .rimz()
        .current_dir(&env.project_root)
        .env("PATH", path_with_front(&shim_dir))
        .args(["agents", "explain", "boxed", "--json"])
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["isolation"], "host");
    assert_eq!(report["isolation_source"], "profile default boxed");
}

/// `--resume` preflights a matched session on the isolation it will run
/// under: the `--isolation` override, else the stored value, never the
/// machine default. A failing `bwrap` on PATH makes any sandbox preflight
/// refuse, so only the stored-sandbox case without an override reaches it.
#[cfg(unix)]
#[test]
fn cohort_resume_preflights_a_matched_session_on_its_effective_isolation() {
    use rimz::config::Isolation;

    for (stored, flag, needs_bwrap) in [
        (Some(Isolation::Sandbox), None, true),
        (Some(Isolation::Host), None, false),
        (Some(Isolation::Sandbox), Some("host"), false),
    ] {
        let env = Env::new();
        std::fs::create_dir_all(env.rimz_home()).expect("config directory");
        std::fs::write(
            env.rimz_home().join("config.toml"),
            "[agents]\nisolation = \"sandbox\"\n",
        )
        .expect("sandbox machine config");
        let shim_dir = write_env_dump_shim(&env, "codex");
        let bwrap = shim_dir.join("bwrap");
        std::fs::write(&bwrap, "#!/bin/sh\necho fake-bwrap-refuses >&2\nexit 1\n")
            .expect("write failing bwrap");
        let mut permissions = std::fs::metadata(&bwrap)
            .expect("bwrap metadata")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&bwrap, permissions).expect("chmod bwrap");

        let workspace = env.resolve_workspace(&env.project_root);
        let kind = AgentKind::new_unchecked("codex");
        let session_id = "closed-codex-session";
        let store = env.store();
        store
            .append_event(&EventEnvelope::agent_launched(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                &kind,
                AgentLaunchPayload {
                    agent_id: session_id.into(),
                    launch_id: Some(session_id.into()),
                    agent_name: "quiet-otter".to_owned(),
                    agent_name_explicit: false,
                    launch: LaunchParams {
                        isolation: stored,
                        ..LaunchParams::default()
                    },
                    state: AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: None,
                    runtime_owner: Some(rimz::pane::RuntimeOwner::new(
                        rimz::pane::RuntimeOwnerKind::Agent,
                        session_id,
                        u32::MAX,
                        None,
                    )),
                    worktree_path: Some(workspace.worktree_root.display().to_string()),
                    worktree_branch: None,
                    prompt: None,
                    description: None,
                },
            ))
            .expect("seed closed session");
        let transcript = env.home_root.join("closed.jsonl");
        std::fs::write(&transcript, "{}\n").expect("write conversation");
        let mut observation =
            AgentLifecycleObservation::new(Some(session_id.into()), LifecycleSignal::Ended);
        observation.transcript_path = Some(transcript.display().to_string());
        store
            .append_event(&EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                kind.as_str(),
                "SessionEnd",
                &observation,
            ))
            .expect("close session");

        let mut command = env.rimz();
        command
            .current_dir(&env.project_root)
            .env("PATH", path_with_front(&shim_dir))
            .args(["agents", "codex", "--resume"]);
        if let Some(flag) = flag {
            command.args(["--isolation", flag]);
        }
        let output = command.bounded_output().expect("resume exits");
        let stderr = String::from_utf8_lossy(&output.stderr);
        // No room runs here, so a case that clears preflight stops at the
        // live-room check; the sandbox preflight refuses before it.
        assert!(!output.status.success(), "{stderr}");
        assert!(
            !stderr.contains("nothing to resume"),
            "the seeded session must match (stored={stored:?}, flag={flag:?}): {stderr}"
        );
        let expected = if needs_bwrap {
            "fake-bwrap-refuses"
        } else {
            "no live RimZ room"
        };
        assert!(
            stderr.contains(expected),
            "stored={stored:?}, flag={flag:?}: {stderr}"
        );
    }
}
