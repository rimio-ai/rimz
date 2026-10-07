//! Integration coverage for `rimz loop` instance-bound delivery.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use jiff::{SignedDuration, Timestamp};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::json;

use rimz::agents::PermissionMode;
use rimz::agents::{AgentRateLimits, RateLimitCacheEntry, RateLimitWindow, RateLimitsCache};
use rimz::config::{CheckOn, LoopConfig, TaskEntry, TaskTarget, Tasks};
use rimz::harness::budget::{BudgetLedger, DayBaseline, write_ledger};
use rimz::harness::schedule::arming::Arming;
use rimz::harness::schedule::run_log::{LoopRunMode, LoopRunRecord, LoopRunResult};
use rimz::harness::schedule::runner::RunLockInfo;
use rimz::ids::{AgentKind, AgentSessionId, MuxName, SidebarInstanceId, WorkspaceId};
use rimz::store::message::{AutoCompact, MessageStatus};
use rimz::store::run::{RunRecord, RunStatus};
use rimz::wakeup::heartbeat::SidebarHeartbeat;

use crate::common::{Env, ScrubSessionEnvExt, canonical};
#[path = "loop_list.rs"]
mod list_tests;
#[cfg(unix)]
use crate::common::{
    path_with_front, trust_codex_preflight_hooks, trust_codex_project, write_fake_login_shell,
    write_path_shim,
};

#[cfg(unix)]
#[test]
fn scheduled_agent_opens_named_tab_despite_ambient_pane() {
    loop_channel_case(LoopLaunch::Agent, LoopFirer::None);
}

#[cfg(unix)]
#[test]
fn check_launch_uses_its_worktree_channel_not_the_firers_team() {
    loop_channel_case(LoopLaunch::Check, LoopFirer::None);
}

#[cfg(unix)]
#[test]
fn resident_launch_does_not_inherit_the_firers_channel() {
    loop_channel_case(LoopLaunch::Resident, LoopFirer::None);
}

#[cfg(unix)]
#[test]
fn scheduled_launch_ignores_a_subagent_firer() {
    loop_channel_case(LoopLaunch::Agent, LoopFirer::Subagent);
}

#[cfg(unix)]
#[test]
fn resident_launch_ignores_a_firer_at_the_chain_limit() {
    loop_channel_case(LoopLaunch::Resident, LoopFirer::ChainLimit);
}

#[cfg(unix)]
#[test]
fn check_launch_ignores_a_firer_found_by_ancestry() {
    loop_channel_case(LoopLaunch::Check, LoopFirer::Ancestry);
}

#[cfg(unix)]
#[test]
fn scheduled_launch_ignores_a_firers_room_pin() {
    loop_channel_case(LoopLaunch::Agent, LoopFirer::RoomPin);
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq)]
enum LoopLaunch {
    Agent,
    Check,
    Resident,
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq)]
enum LoopFirer {
    None,
    Subagent,
    ChainLimit,
    Ancestry,
    RoomPin,
}

#[cfg(unix)]
fn loop_channel_case(action: LoopLaunch, firer: LoopFirer) {
    use crate::common::CommandTimeoutExt;

    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let workspace = env.resolve_workspace(&env.project_root);
    let cwd = if action == LoopLaunch::Check {
        let linked = env.home_root.join("linked");
        assert!(git_ok(
            &env.project_root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked",
                linked.to_str().unwrap()
            ]
        ));
        env.write_config(
            &env.project_root,
            r#"
            [profiles.coder]
            agent = "codex"
            [[agents.teams.forge.roles]]
            role = "coder"
            profile = "coder"
            "#,
        );
        loop_ok(&env, &["trust", "grant"]);
        seed_agent_launch(
            &env,
            &env.project_root,
            "probe-member",
            rimz::agents::LaunchParams {
                team: Some("forge".to_owned()),
                role: Some("coder".to_owned()),
                channel: Some("probe".to_owned()),
                ..Default::default()
            },
            None,
        );
        trust_codex_project(&env, &linked);
        canonical(&linked)
    } else {
        env.project_root.clone()
    };
    let trace = env.home_root.join("scheduled-tab.log");
    let panes = r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},{"id":2,"is_plugin":false,"tab_id":1,"title":"sh"},{"id":3,"is_plugin":false,"tab_id":2,"tab_name":"rimzd","title":"loop","pane_command":"rimz loop watch --hold"}]"#;
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        serde_json::from_str(panes).unwrap(),
    );
    let effect = match action {
        LoopLaunch::Check => format!(
            "dir = {:?}\ncheck = {:?}\n",
            cwd.to_str().unwrap(),
            shlex::try_join([
                env.rimz_bin().to_str().unwrap(),
                "agents",
                "coder",
                "repair"
            ])
            .unwrap(),
        ),
        LoopLaunch::Resident => "agent = \"codex\"\nprompt = \"repair\"\nstay = true\n".to_owned(),
        LoopLaunch::Agent => "agent = \"codex\"\nprompt = \"repair\"\n".to_owned(),
    };
    let timeout = if action == LoopLaunch::Check {
        "30s"
    } else {
        "2s"
    };
    write_loop_config(
        &env,
        &format!(
            "default-timeout = \"1s\"\n[tasks.rimzd]\nevery = \"1h\"\n{effect}root = {:?}\ntimeout = \"{timeout}\"\nthrottle = \"off\"\n",
            env.project_root.to_str().unwrap(),
        ),
    );
    if firer != LoopFirer::None {
        seed_agent_launch(
            &env,
            &env.project_root,
            "firer",
            rimz::agents::LaunchParams {
                parent_agent_id: (firer == LoopFirer::Subagent)
                    .then(|| AgentSessionId::from("launch_parent")),
                parent_agent_kind: (firer == LoopFirer::Subagent)
                    .then(|| AgentKind::new_unchecked("claude")),
                launch_depth: Some(match firer {
                    LoopFirer::Subagent => 1,
                    LoopFirer::RoomPin => 0,
                    _ => 3,
                }),
                ..Default::default()
            },
            (firer == LoopFirer::Ancestry).then(|| {
                rimz::pane::RuntimeOwner::new(
                    rimz::pane::RuntimeOwnerKind::Agent,
                    "firer",
                    std::process::id(),
                    None,
                )
            }),
        );
    }
    let mut command = env.rimz();
    if matches!(
        firer,
        LoopFirer::Subagent | LoopFirer::ChainLimit | LoopFirer::RoomPin
    ) {
        command
            .env("RIMZ_AGENT_KIND", "claude")
            .env("RIMZ_AGENT_ID", "launch_firer");
    }
    if firer == LoopFirer::RoomPin {
        let firer_root = env.home_root.join("firer-project");
        std::fs::create_dir(&firer_root).unwrap();
        assert!(init_git_repo(&firer_root));
        let firer_workspace = env.resolve_workspace(&firer_root);
        assert_ne!(workspace.workspace_id, firer_workspace.workspace_id);
        command.envs(rimz::workspace::pin_env(
            &firer_workspace.workspace_id,
            &firer_workspace.project_root,
        ));
    }
    let output = command
        .args(["--mux", "zellij", "loop", "run", "rimzd"])
        .env("SHELL", shell)
        .env("PATH", path_with_front(&agent_bin))
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
        .env("RIMZ_TEST_ZELLIJ_VERSION", "0.45.0")
        .env("RIMZ_TEST_ZELLIJ_LOG_LAYOUTS", "1")
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", panes)
        .env("ZELLIJ_PANE_ID", "2")
        .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
        .env("RIMZ_CHANNEL", "probe")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        )
        .bounded_output()
        .unwrap();
    let trace = std::fs::read_to_string(trace).unwrap_or_default();
    let tabs = trace
        .lines()
        .filter(|line| line.contains("action\tnew-tab"))
        .collect::<Vec<_>>();
    assert_eq!(
        tabs.len(),
        1,
        "{trace}\n{}\n{:?}",
        String::from_utf8_lossy(&output.stderr),
        read_loop_run_records(&env),
    );
    if action != LoopLaunch::Resident {
        assert!(tabs[0].contains("\t--name\tloop rimzd"), "{trace}");
    }
    assert!(tabs[0].contains("\t--no-focus"), "{trace}");
    assert!(
        trace
            .lines()
            .any(|line| line.starts_with("layout\t") && line.contains("rimz-sidebar")),
        "run tab must dock a sidebar: {trace}",
    );
    assert!(!trace.contains("\tnew-pane\t"), "{trace}");

    let layout: String = serde_json::from_str(
        trace
            .lines()
            .find_map(|line| line.strip_prefix("layout-json\t"))
            .unwrap(),
    )
    .unwrap();
    let layout: kdl::KdlDocument = layout.parse().unwrap();
    let argv = loop_agent_pane_args(&layout).expect("agent pane args");
    let request = argv.windows(2).find(|pair| pair[0] == "--request").unwrap();
    let request = rimz::harness::launch::decode_exec_request("codex", None, &request[1]).unwrap();
    let mut pane_env: BTreeMap<String, String> = argv
        .iter()
        .take_while(|arg| arg.contains('='))
        .map(|arg| {
            let (key, value) = arg.split_once('=').unwrap();
            (key.to_owned(), value.to_owned())
        })
        .collect();
    let pane_channel = pane_env.get("RIMZ_CHANNEL").cloned();
    pane_env.insert(
        "RIMZ_AGENT_ID".to_owned(),
        request.identity.launch_id.clone().unwrap(),
    );
    pane_env.insert(
        "RIMZ_AGENT_NAME".to_owned(),
        request.identity.name.clone().unwrap(),
    );
    if let Some(run_id) = request.run_id {
        pane_env.insert("RIMZ_RUN_ID".to_owned(), run_id.to_string());
    }
    let pane_env = pane_env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let hook = env.run_installed_hook_in_pane(
        "codex",
        &json!({
            "hook_event_name": "SessionStart", "session_id": "loop-channel-session", "cwd": cwd,
        })
        .to_string(),
        &pane_env,
    );
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );
    let agents = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    let agent = agents
        .iter()
        .find(|agent| agent.name.as_deref() == request.identity.name.as_deref())
        .unwrap();
    assert!(agent.launched_by.is_none(), "{agent:?}");
    assert!(agent.parent_agent_id.is_none(), "{agent:?}");
    assert_eq!(agent.launch_depth, None, "{agent:?}");
    let expected = (action == LoopLaunch::Check).then_some("linked");
    assert_eq!(
        agent.channel.as_deref(),
        expected,
        "recorded channel after the pane's registration hook"
    );
    assert_eq!(
        pane_channel.as_deref(),
        expected,
        "channel exported to the agent pane"
    );
    assert_eq!(
        agent.team, None,
        "the firer's team must not qualify a loop's bare profile"
    );
}

#[cfg(unix)]
fn loop_agent_pane_args(document: &kdl::KdlDocument) -> Option<Vec<String>> {
    for node in document.nodes() {
        if node.name().value() == "args" {
            let args = node
                .entries()
                .iter()
                .map(|entry| entry.value().as_string().unwrap().to_owned())
                .collect::<Vec<_>>();
            if args.iter().any(|arg| arg == "--request") {
                return Some(args);
            }
        }
        if let Some(args) = node.children().and_then(loop_agent_pane_args) {
            return Some(args);
        }
    }
    None
}

#[test]
fn resident_layout_refuses_account_pin() {
    let env = Env::new();
    let output = env
        .rimz()
        .args([
            "loop",
            "add",
            "resident",
            "--every",
            "1h",
            "--agent",
            "codex",
            "--prompt",
            "watch",
            "--stay",
            "--account",
            "work",
        ])
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{error}");
    assert!(error.contains("--stay cannot use --account"), "{error}");
    assert!(error.contains("omit --account or --stay"), "{error}");
}

#[test]
fn condition_tick_observes_board_and_records_evidence() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    let scope = env.home_root.join("condition-scope");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "condition-scope",
            scope.to_str().unwrap()
        ]
    ));
    let root_board = env.project_root.join("blackboard.md");
    let board = scope.join("blackboard.md");
    std::fs::write(&root_board, "Stage: Done\n").unwrap();
    std::fs::write(&board, "Stage: Review\n").unwrap();
    let receipt = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ready",
            "--when",
            "team.stage=Done",
            "--check",
            "true",
            "--root",
            scope.to_str().unwrap(),
            "--once",
        ],
    );
    assert!(
        receipt.contains(&format!("scope: {}", canonical(&scope).display())),
        "{receipt}"
    );
    assert!(
        receipt.contains("waiting · team.stage: Review"),
        "{receipt}"
    );
    let instances = read_loop_instances(&env);
    assert_eq!(instances.0["ready"].run_dir(), canonical(&scope));
    assert_eq!(instances.0["ready"].once, Some(true));
    loop_ok(&env, &["loop", "tick"]);
    let armed = rimz::harness::schedule::last_stamps(&env.runtime_paths());
    assert!(armed.contains_key("ready"));
    loop_ok(&env, &["loop", "tick"]);
    assert_eq!(
        rimz::harness::schedule::last_stamps(&env.runtime_paths()),
        armed
    );
    assert!(read_loop_run_records(&env).is_empty());
    let listing = loop_ok(&env, &["loop", "list"]);
    assert!(listing.contains("when team.stage=Done"), "{listing}");
    let shown = loop_ok(&env, &["loop", "show", "ready"]);
    assert!(shown.contains("waiting · team.stage: Review"), "{shown}");
    std::fs::write(&root_board, "Stage: Review\n").unwrap();
    std::fs::write(&board, "Stage: Done\n").unwrap();
    loop_ok(&env, &["loop", "tick"]);
    wait_for_loop_run_records(&env, "ready record and once-instance removal", |records| {
        !records.is_empty() && !read_loop_instances(&env).0.contains_key("ready")
    });
    let record = last_loop_record(&env);
    assert_eq!(record.result, LoopRunResult::Completed);
    let condition = record.condition.unwrap();
    assert_eq!(condition.when, "team.stage=Done");
    assert_eq!(condition.readings["team.stage"].as_deref(), Some("Done"));
    assert!(!read_loop_instances(&env).0.contains_key("ready"));
}

#[test]
fn queue_condition_selects_a_dequeued_open_pr_from_the_room_cache() {
    let env = Env::new();
    let runtime = env.runtime_paths();
    crate::common::room::seed_sidebar_heartbeat(
        &runtime,
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "queue",
    );
    let write_queue = |queue: serde_json::Value| {
        std::fs::create_dir_all(runtime.lane_path("pr-state.json").parent().unwrap()).unwrap();
        std::fs::write(
            runtime.lane_path("pr-state.json"),
            json!({"states": {env.project_root.to_string_lossy().to_string(): {
                "state": "open", "number": 91,
                "open": {"head": "head-a", "base": "main", "queue": queue},
            }}})
            .to_string(),
        )
        .unwrap();
    };
    write_queue(json!({
        "state": "dequeued", "at": "2026-10-03T12:46:03Z",
        "reason": "failed_checks", "commit": "queue-a",
    }));
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "requeue",
            "--when",
            "pr=open && pr.queue=dequeued",
            "--check",
            "true",
        ],
    );
    let show = loop_ok(&env, &["loop", "show", "requeue"]);
    for term in ["pr=open", "pr.queue=dequeued"] {
        let line = show
            .lines()
            .find(|line| line.trim_start().starts_with(term))
            .unwrap_or_else(|| panic!("{show}"));
        assert!(line.contains('✓'), "{show}");
    }
    assert!(show.contains("dequeued"), "{show}");

    write_queue(json!({"state": "queued", "at": "2026-10-03T13:00:00Z"}));
    let show = loop_ok(&env, &["loop", "show", "requeue"]);
    let line = show
        .lines()
        .find(|line| line.trim_start().starts_with("pr.queue=dequeued"))
        .unwrap_or_else(|| panic!("{show}"));
    assert!(line.ends_with("queued") && !line.contains('✓'), "{show}");
}

#[test]
fn condition_add_refuses_invalid_predicates_and_options() {
    let env = Env::new();
    for (flags, expected) in [
        (
            vec!["--when", "wat=yes"],
            "team.stage, ci, pr, pr.queue, window.5h.left, window.7d.left",
        ),
        (
            vec!["--when", "window.5h.left>=40"],
            "a --check-only task has none; add --agent or --wait",
        ),
        (
            vec!["--when", "team.stage=Missing"],
            "no team defines stage Missing; stages: Done",
        ),
        (vec!["--for", "30m"], "--when"),
        (
            vec!["--project", "--when", "ci=passed"],
            "--project tasks cannot use --when yet",
        ),
        (vec!["--when", "ci!=failed"], "!ci=failed"),
    ] {
        let output = env
            .rimz()
            .args(["loop", "add", "bad", "--check", "true"])
            .args(flags)
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(error.contains(expected), "expected {expected}: {error}");
    }
}

#[test]
fn paused_fanout_keeps_hold_and_fired_state() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    loop_ok(&env, &["worktree", "new", "lane"]);
    let owned = rimz::worktree::discover_owned(&env.project_root).unwrap();
    assert_eq!(owned.len(), 1);
    std::fs::write(owned[0].path.join("blackboard.md"), "Stage: Done\n").unwrap();
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nroot = {:?}\nagent = \"codex\"\nprompt = \"repair\"\nstay = true\neach-worktree = true\nwhen = [\"team.stage=Done\"]\nfor = \"30m\"\n",
            env.project_root.to_str().unwrap()
        ),
    );
    loop_ok(&env, &["loop", "tick"]);
    loop_ok(&env, &["loop", "tick"]);
    let runtime = env.runtime_paths();
    let fire_path = runtime.lane_path("loop-fire.json");
    let when_path = runtime.lane_path("loop-when.json");
    let stamps = std::fs::read(&fire_path).unwrap();
    let mut states: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&when_path).unwrap()).unwrap();
    assert_eq!(states.as_object().unwrap().len(), 1);
    for fired in [false, true] {
        for state in states.as_object_mut().unwrap().values_mut() {
            state["fired"] = json!(fired);
        }
        std::fs::write(&when_path, serde_json::to_vec(&states).unwrap()).unwrap();
        loop_ok(&env, &["loop", "pause", "resident", "--for", "2h"]);
        loop_ok(&env, &["loop", "tick"]);
        assert_eq!(std::fs::read(&fire_path).unwrap(), stamps);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&when_path).unwrap())
                .unwrap(),
            states
        );
        loop_ok(&env, &["loop", "enable", "resident"]);
        loop_ok(&env, &["loop", "tick"]);
        assert_eq!(std::fs::read(&fire_path).unwrap(), stamps);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&when_path).unwrap())
                .unwrap(),
            states
        );
    }
}

#[test]
fn resident_add_accepts_layout_and_persists_launch_options() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let output = env
        .rimz()
        .args([
            "loop",
            "add",
            "resident",
            "--agent",
            "claude,codex",
            "--stay",
            "--each-worktree",
            "--when",
            "pr=open",
            "--takeover",
            "--subscribe",
            "ci.failed",
            "--subscribe",
            "pr.conflicted",
            "--mode",
            "ask",
            "--effort",
            "high",
            "--prompt",
            "repair",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    let entry = serde_json::to_value(&config.tasks.0["resident"]).unwrap();
    assert_eq!(entry["stay"], true);
    assert_eq!(entry["each-worktree"], true);
    assert_eq!(entry["takeover"], true);
    assert_eq!(entry["subscribe"][0]["signal"], "ci.failed");
    assert_eq!(entry["subscribe"][1]["signal"], "pr.conflicted");
    assert_eq!(entry["mode"], "ask");
    assert_eq!(entry["effort"], "high");
}

#[test]
fn resident_subscriptions_require_a_usable_scope_at_add() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    for (signal, fix) in [
        ("ci.failed", "--each-worktree"),
        ("pr.conflicted", "--each-worktree"),
        ("team.stage", "team layout"),
    ] {
        let (_, error) = loop_fail(
            &env,
            &[
                "loop",
                "add",
                "resident",
                "--agent",
                "claude",
                "--stay",
                "--every",
                "1h",
                "--subscribe",
                signal,
                "--prompt",
                "watch",
            ],
        );
        assert!(
            error.contains("--subscribe") && error.contains(fix),
            "{error}"
        );
    }
}

#[test]
fn resident_subscribe_requires_prompt_leader_and_installed_hooks() {
    let env = Env::new();
    for (spec, expected) in [("term", "prompt leader"), ("claude", "--subscribe")] {
        let (_, error) = loop_fail(
            &env,
            &[
                "loop",
                "add",
                "resident",
                "--agent",
                spec,
                "--stay",
                "--each-worktree",
                "--when",
                "pr=open",
                "--subscribe",
                "ci.failed",
                "--prompt",
                "repair",
            ],
        );
        assert!(error.contains(expected), "{error}");
    }
    assert!(read_loop_instances(&env).0.is_empty());
}

#[cfg(unix)]
#[test]
fn resident_launch_deduplicates_and_manual_fire_relaunches() {
    resident_launch_case(false, false);
}

#[cfg(unix)]
#[test]
fn resident_show_summarizes_worktree_conditions() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nroot = {:?}\nagent = \"codex\"\nprompt = \"repair\"\nstay = true\neach-worktree = true\nwhen = [\"pr=open\"]\n",
            env.project_root
        ),
    );
    let summary = loop_ok(&env, &["loop", "show", "resident"]);
    assert!(
        summary.contains("condition: evaluated per owned worktree · 0 launched"),
        "{summary}"
    );
    assert!(!summary.contains("SUBSCRIPTIONS"), "{summary}");
}

#[test]
fn loop_show_lists_each_derived_subscription() {
    loop_show_subscriptions_case(false);
}

#[test]
fn loop_show_loads_subscriptions_and_results_from_the_tasks_room() {
    loop_show_subscriptions_case(true);
}

fn loop_show_subscriptions_case(other_room: bool) {
    use rimz::harness::schedule::run_log::SignalRecord;

    let env = Env::new();
    let root = if other_room {
        let root = env.home_root.join("other-room");
        std::fs::create_dir(&root).unwrap();
        root
    } else {
        env.project_root.clone()
    };
    write_loop_config(
        &env,
        &format!(
            "[tasks.sweep]\nroot = {root:?}\nagent = \"codex\"\nprompt = \"repair\"\nstay = true\nwhen = [\"pr=open\"]\n"
        ),
    );
    let subscription = |checkout: &str, handle: &str, signal: &str| TaskEntry {
        root: root.clone(),
        dir: Some(env.home_root.join(checkout)),
        loop_task: Some("sweep".into()),
        wait: Some(TaskTarget {
            kind: AgentKind::new_unchecked("codex"),
            session: "session".into(),
            handle: handle.into(),
        }),
        signal: Some(signal.into()),
        ..TaskEntry::default()
    };
    let mut unrelated = subscription("feature-a", "@fixer#feature-a", "ci.failed");
    unrelated.loop_task = Some("another-loop".into());
    let mut team_only = unrelated.clone();
    team_only.loop_task = None;
    team_only.team = Some("forge#feature-a".parse().unwrap());
    let tasks = Tasks(BTreeMap::from([
        (
            "derived-a".into(),
            subscription("feature-a", "@fixer#feature-a", "ci.failed"),
        ),
        (
            "derived-b".into(),
            subscription("feature-a", "@fixer#feature-a", "pr.merged"),
        ),
        (
            "derived-c".into(),
            subscription("feature-b", "@reviewer#feature-b", "ci.failed"),
        ),
        (
            "derived-d".into(),
            subscription("feature-b", "@reviewer#feature-b", "ci.passed"),
        ),
        ("unrelated".into(), unrelated),
        ("team-only".into(), team_only),
    ]));
    let path = env
        .state_path_for(&root)
        .root
        .join("records/loop-instances.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec(&tasks).unwrap()).unwrap();
    let record = |name: &str, result: LoopRunResult, minutes: i64| {
        let mut record = LoopRunRecord::new(name, result, LoopRunMode::Scheduled, 0);
        record.root = Some(root.clone());
        record.at = Timestamp::now() - SignedDuration::from_mins(minutes);
        if result == LoopRunResult::SignalSkipped {
            record.signal = Some(SignalRecord {
                name: tasks.0[name].signal.as_deref().unwrap().parse().unwrap(),
                payload: serde_json::Map::new(),
            });
        }
        record
    };
    let mut foreign = record("derived-a", LoopRunResult::Failed, 1);
    foreign.root = Some(env.home_root.join("foreign-room"));
    write_loop_run_records(
        &env,
        &[
            record("derived-a", LoopRunResult::Delivered, 5),
            record("derived-b", LoopRunResult::Completed, 4),
            record("derived-b", LoopRunResult::SignalSkipped, 3),
            record("derived-b", LoopRunResult::SignalSkipped, 2),
            record("derived-b", LoopRunResult::SignalSkipped, 1),
            record("derived-d", LoopRunResult::SignalSkipped, 1),
            foreign,
        ],
    );
    let shown = loop_ok(&env, &["loop", "show", "sweep"]);
    let subscriptions = shown
        .split_once("SUBSCRIPTIONS\n")
        .unwrap_or_else(|| panic!("missing SUBSCRIPTIONS block: {shown}"))
        .1
        .split("\n\n")
        .next()
        .unwrap();
    for (name, checkout, target, signal, last) in [
        (
            "derived-a",
            "feature-a",
            "@fixer#feature-a",
            "ci.failed",
            "✓ 5m ago",
        ),
        (
            "derived-b",
            "feature-a",
            "@fixer#feature-a",
            "pr.merged",
            "✓ 4m ago",
        ),
        (
            "derived-c",
            "feature-b",
            "@reviewer#feature-b",
            "ci.failed",
            "never fired",
        ),
        (
            "derived-d",
            "feature-b",
            "@reviewer#feature-b",
            "ci.passed",
            "heard ci.passed 1m ago",
        ),
    ] {
        let row = subscriptions
            .lines()
            .find(|line| line.contains(name))
            .unwrap_or_else(|| panic!("missing {name}: {shown}"));
        for cell in [checkout, target, signal, last] {
            assert!(row.contains(cell), "missing {cell}: {row}");
        }
        assert_eq!(subscriptions.matches(name).count(), 1, "{shown}");
    }
    assert!(!subscriptions.contains("unrelated"), "{shown}");
    assert!(!subscriptions.contains("team-only"), "{shown}");
    assert!(!subscriptions.contains("✗ failed"), "{shown}");
    assert!(!subscriptions.contains("skipped"), "{shown}");
}

#[cfg(unix)]
#[test]
fn resident_worktree_launch_preserves_marker_path_without_a_room() {
    resident_launch_case(true, false);
}

#[cfg(unix)]
#[test]
fn resident_team_layout_uses_the_helpers_resolved_checkout() {
    resident_launch_case(true, true);
}

#[test]
fn agents_team_cwd_subdirectory_refuses_root_checkout_bindings() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.install_agent_hooks("codex");
    env.write_config(
        &env.project_root,
        r#"
        [profiles.codex]
        agent = "codex"
        [[agents.teams.forge.roles]]
        role = "coder"
        profile = "codex"
        signals = [{ signal = "ci.failed" }]
        "#,
    );
    loop_ok(&env, &["trust", "grant"]);
    let cwd = env.project_root.join("docs");
    std::fs::create_dir(&cwd).unwrap();
    let output = env
        .rimz()
        .args(["agents", "forge", "--cwd"])
        .arg(&cwd)
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{error}");
    assert!(
        error.contains("root checkout needs an explicit scope"),
        "{error}"
    );
}

#[cfg(unix)]
fn resident_launch_case(each_worktree: bool, team: bool) {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    let actual = env.home_root.join("actual-worktrees");
    let alias = env.home_root.join("linked-worktrees");
    std::fs::create_dir(&actual).unwrap();
    std::os::unix::fs::symlink(&actual, &alias).unwrap();
    std::fs::create_dir(alias.join("spare")).unwrap();
    let worktree_dir = if each_worktree {
        alias.join("spare/..")
    } else {
        alias.clone()
    };
    std::fs::write(
        env.rimz_home().join("config.toml"),
        format!(
            "[agents.worktree]\ndir = {:?}\n",
            worktree_dir.to_str().unwrap()
        ),
    )
    .unwrap();
    loop_ok(&env, &["worktree", "new", "lane"]);
    if team {
        env.write_config(
            &env.project_root,
            r#"
            [profiles.codex]
            agent = "codex"
            [[agents.teams.forge.roles]]
            role = "coder"
            profile = "codex"
            signals = [{ signal = "ci.failed" }]
        "#,
        );
        loop_ok(&env, &["trust", "grant"]);
    }
    let checkout = alias.join("lane");
    let marker = rimz::worktree::read_marker_for_worktree(&checkout)
        .unwrap()
        .unwrap();
    assert_eq!(marker.worktree_path, worktree_dir.join("lane"));
    assert_ne!(checkout, canonical(&checkout));
    trust_codex_project(&env, &checkout);
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let workspace = env.resolve_workspace(&env.project_root);
    let trace = env.home_root.join("resident-mux.log");
    let _room = if each_worktree {
        Some(crate::common::room::ShimRoom::watch(&env, &trace, "[]"))
    } else {
        crate::common::room::seed_live_zellij_room(
            &env.runtime_paths(),
            &workspace.session_name,
            Vec::new(),
        );
        None
    };
    let command = || {
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij"])
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&agent_bin))
            .env(
                "RIMZ_ZELLIJ_BIN",
                crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
            )
            .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                if each_worktree {
                    String::new()
                } else {
                    format!("{} [Created 1s ago]\n", workspace.session_name)
                },
            );
        command
    };
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nagent = {:?}\nprompt = \"repair\"\nroot = {:?}\ndir = {:?}\n{}\nstay = true\nsubscribe = [{{ signal = \"ci.failed\" }}]\n",
            if team { "forge" } else { "codex" },
            env.project_root.to_str().unwrap(),
            checkout.to_str().unwrap(),
            if each_worktree {
                "each-worktree = true\nwhen = [\"pr=open\"]"
            } else {
                "every = \"1h\""
            },
        ),
    );
    if each_worktree {
        let runtime = env.runtime_paths();
        std::fs::create_dir_all(runtime.lane_path("pr-state.json").parent().unwrap()).unwrap();
        std::fs::write(
            runtime.lane_path("pr-state.json"),
            json!({"states": {checkout.to_string_lossy().to_string(): {"state": "open"}}})
                .to_string(),
        )
        .unwrap();
        let source = rimz::harness::schedule::when::CiSource::read(&runtime);
        let expr = rimz::harness::schedule::when::WhenExpr::parse(&["pr=open".to_owned()]).unwrap();
        let windows =
            rimz::harness::schedule::when::WindowReadings::new(Some(&runtime), Timestamp::now());
        assert!(
            rimz::harness::schedule::when::evaluate(
                &expr,
                &checkout,
                Some(&source),
                None,
                None,
                &windows
            )
            .ok
        );
    }
    for _ in 0..2 {
        let mut run = command();
        run.args(["loop", "run", "resident"]);
        if each_worktree {
            run.arg("--cwd").arg(&checkout).arg("--condition-json").arg(
                json!({
                    "when": "pr=open", "hold": null, "held_ms": 0,
                    "readings": {"pr": "open"}
                })
                .to_string(),
            );
        }
        let output = run.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let store = env.store();
    let agents = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert_eq!(agents.len(), 1);
    let expected_checkout = if each_worktree {
        checkout.clone()
    } else {
        canonical(&checkout)
    };
    assert_eq!(
        agents[0].worktree_path.as_deref(),
        expected_checkout.to_str()
    );
    assert_eq!(agents[0].channel.as_deref(), Some("lane"));
    assert!(rimz::harness::run::list(store.paths()).unwrap().is_empty());
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1);
    assert_eq!(serde_json::to_value(records[0].result).unwrap(), "launched");
    let ledger = rimz::harness::schedule::launch_ledger::load(store.paths()).unwrap();
    assert!(ledger["resident"].contains_key(&expected_checkout));
    if team {
        assert_eq!(agents[0].team.as_deref(), Some("forge"));
        return;
    }
    let (_, error) = loop_fail(&env, &["loop", "rename", "resident", "renamed"]);
    assert!(error.contains("resident leaders"), "{error}");
    assert_eq!(
        rimz::harness::schedule::launch_ledger::load(store.paths()).unwrap(),
        ledger
    );
    let assists = rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None);
    assert_eq!(assists.len(), 1);
    let assist = serde_json::to_value(&assists[0]).unwrap();
    assert_eq!(assist["assist"], "resident_launch");
    assert_eq!(assist["task"], "resident");
    assert!(loop_ok(&env, &["stats", "--assists"]).contains("loop resident opened"));
    if each_worktree {
        assert_eq!(assist["condition"]["readings"]["pr"], "open");
    }
    assert_eq!(
        assist["checkout"],
        expected_checkout.to_string_lossy().as_ref()
    );
    assert_eq!(
        assist["handles"],
        json!([format!("@{}", agents[0].name.as_deref().unwrap())])
    );
    assert_eq!(
        serde_json::to_value(&agents[0]).unwrap()["loop_task"],
        "resident"
    );
    let owner = dummy_agent_process();
    let owner_pid = owner.id();
    reap_later(owner);
    let transcript = env.home_root.join("resident-transcript.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    let hook = |event: &str| {
        let mut command = env.hook_command("codex");
        command
            .env("RIMZ_AGENT_PID", owner_pid.to_string())
            .env("ZELLIJ", "1")
            .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
            .env("ZELLIJ_PANE_ID", "51");
        command.current_dir(&checkout).env(
            rimz::harness::launch::ENV_AGENT_NAME,
            agents[0].name.as_deref().unwrap(),
        );
        let output = env
            .spawn_payload(
                command,
                &json!({
                    "hook_event_name": event, "session_id": "resident-session",
                    "cwd": expected_checkout, "prompt": "work",
                    "transcript_path": transcript
                })
                .to_string(),
            )
            .wait_with_output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    hook("SessionStart");
    let armed = read_loop_instances(&env);
    assert_eq!(armed.0.len(), 1);
    let (subscription, binding) = armed.0.iter().next().unwrap();
    assert!(
        subscription.contains("resident")
            && subscription.contains(agents[0].name.as_deref().unwrap())
    );
    assert_eq!(
        binding.matches.as_ref().unwrap()["path"],
        expected_checkout.to_string_lossy()
    );
    assert_eq!(
        binding.wait.as_ref().unwrap().session.as_str(),
        "resident-session"
    );
    assert!(binding.once.is_none() && binding.team.is_none());
    hook("SessionStart");
    assert_eq!(read_loop_instances(&env), armed);
    let restarted = command()
        .args([
            "agents",
            "restart",
            &format!("@{}", agents[0].name.as_deref().unwrap()),
        ])
        // The split names the room and resolves the replaced pane's tab there.
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            r#"[{"id":51,"is_plugin":false,"tab_id":1,"title":"codex"}]"#,
        )
        .output()
        .unwrap();
    assert!(
        restarted.status.success(),
        "{}",
        String::from_utf8_lossy(&restarted.stderr)
    );
    assert!(
        String::from_utf8_lossy(&restarted.stdout).contains("resumed session resident-session")
    );
    hook("UserPromptSubmit");
    for (path, matching) in [(&env.project_root, false), (&expected_checkout, true)] {
        loop_ok(
            &env,
            &[
                "events",
                "emit",
                "ci.failed",
                "--source",
                "forge",
                "--json",
                &json!({"path":path}).to_string(),
            ],
        );
        if !matching {
            assert!(store.list_pending_messages().unwrap().is_empty());
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !read_loop_run_records(&env)
        .iter()
        .any(|row| row.task == *subscription)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(25));
    }
    let messages = store.list_pending_messages().unwrap();
    assert_eq!(messages.len(), 1, "{:?}", read_loop_run_records(&env));
    assert_eq!(messages[0].kind.as_str(), "codex");
    assert_eq!(messages[0].agent_id.as_str(), "resident-session");
    assert_eq!(messages[0].status, MessageStatus::Queued);
    assert!(messages[0].text.contains("ci.failed"));
    assert_eq!(read_loop_instances(&env), armed);
    store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "codex",
            "rimz.agent-ended",
            &rimz::agents::AgentLifecycleObservation::new(
                Some(AgentSessionId::from("resident-session")),
                rimz::agents::LifecycleSignal::Ended,
            ),
        ))
        .unwrap();
    loop_ok(&env, &["events", "emit", "deploy.done"]);
    assert!(read_loop_instances(&env).0.is_empty());
    if each_worktree {
        let refused = command()
            .args(["loop", "fire", "resident"])
            .output()
            .unwrap();
        assert!(!refused.status.success());
        assert!(String::from_utf8_lossy(&refused.stderr).contains("owned worktree"));
    }
    let output = command()
        .args(["loop", "fire", "resident"])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read_loop_run_records(&env)
            .iter()
            .filter(|row| row.task == "resident")
            .count(),
        2
    );
    assert_eq!(
        rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None).len(),
        2
    );
    assert_eq!(
        store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents
            .len(),
        2
    );
    let mux_log = std::fs::read_to_string(&trace).unwrap();
    assert!(!mux_log.contains("close-tab"), "{mux_log}");
    assert_eq!(mux_log.matches("close-pane").count(), 1, "{mux_log}");
    if each_worktree {
        assert!(mux_log.contains("--create-background"), "{mux_log}");
    }
    let shown = command()
        .args(["loop", "show", "resident", "--json"])
        .output()
        .unwrap();
    assert!(shown.status.success());
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["launches"].as_object().unwrap().len(), 1);
    assert_eq!(
        shown["runs"][0]["checkout"],
        expected_checkout.to_string_lossy().as_ref()
    );
    if each_worktree {
        let summary = loop_ok(&env, &["loop", "show", "resident"]);
        assert!(
            summary.contains("condition: evaluated per owned worktree · 1 launched"),
            "{summary}"
        );
        assert!(!summary.contains("condition: unarmed"), "{summary}");
    }
    let assist_log = env.rimz_home().join("logs/assists.log.jsonl");
    std::fs::remove_file(&assist_log).unwrap();
    std::fs::create_dir(&assist_log).unwrap();
    let launched = rimz::harness::schedule::launch_ledger::load(store.paths()).unwrap();
    let unrecorded = command()
        .args(["loop", "fire", "resident"])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(!unrecorded.status.success());
    assert_eq!(
        serde_json::to_value(read_loop_run_records(&env).last().unwrap().result).unwrap(),
        "errored"
    );
    let kept = rimz::harness::schedule::launch_ledger::load(store.paths()).unwrap();
    assert_ne!(
        kept["resident"][&expected_checkout].leader,
        launched["resident"][&expected_checkout].leader
    );
    loop_ok(&env, &["loop", "remove", "resident"]);
    hook("SessionStart");
    assert!(read_loop_instances(&env).0.is_empty());
    assert!(
        rimz::harness::schedule::launch_ledger::load(store.paths())
            .unwrap()
            .is_empty()
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Takeover {
    IdleOccupants,
    WorkingOccupant,
    SymlinkedCheckout,
    FailedStop,
    OpenRunPane,
    EmptyCheckout,
    TeamLayout,
}

#[test]
fn resident_takeover_stops_idle_occupants_of_every_origin_and_no_other_checkout() {
    resident_takeover_case(Takeover::IdleOccupants);
}

#[test]
fn resident_takeover_waits_for_a_working_occupant_then_stops_it_and_launches() {
    resident_takeover_case(Takeover::WorkingOccupant);
}

#[cfg(unix)]
#[test]
fn resident_takeover_of_a_symlinked_checkout_finds_the_occupant_at_its_physical_path() {
    resident_takeover_case(Takeover::SymlinkedCheckout);
}

#[test]
fn resident_takeover_failed_stop_commits_no_launch_or_ledger() {
    resident_takeover_case(Takeover::FailedStop);
}

/// A kept run's pane that is still listed after a failed close is an occupant
/// still in the checkout.
#[test]
fn resident_takeover_of_a_run_whose_pane_stays_open_commits_no_launch_or_ledger() {
    resident_takeover_case(Takeover::OpenRunPane);
}

#[test]
fn resident_takeover_of_an_empty_checkout_launches_and_stops_nothing() {
    resident_takeover_case(Takeover::EmptyCheckout);
}

#[test]
fn resident_takeover_into_a_team_layout_relaunches_the_team_holding_the_checkout() {
    resident_takeover_case(Takeover::TeamLayout);
}

fn resident_takeover_case(case: Takeover) {
    let env = Env::new();
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    // The launch checkout as the task names it, and as a provider hook reports
    // the occupant's cwd: the same path unless a symlink leads to the checkout.
    let symlinked = case == Takeover::SymlinkedCheckout;
    let (launch_cwd, cwd) = if symlinked {
        let launch_cwd = symlinked_owned_worktree(&env);
        let physical = canonical(&launch_cwd);
        assert_ne!(launch_cwd, physical);
        (launch_cwd, physical)
    } else {
        (cwd.clone(), cwd)
    };
    let working = symlinked || case == Takeover::WorkingOccupant;
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &launch_cwd);
    let sibling = env.home_root.join("sibling");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "sibling",
            sibling.to_str().unwrap()
        ]
    ));
    let docs = cwd.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let workspace = env.resolve_workspace(&env.project_root);
    let store = env.store();
    let team = |channel: &str| rimz::agents::LaunchParams {
        team: Some("forge".to_owned()),
        role: Some("coder".to_owned()),
        channel: Some(channel.to_owned()),
        ..Default::default()
    };
    // The fire runs under the test process, so a launched child owned by it
    // would make the fire itself a subagent.
    let child_owner = dummy_agent_process();
    let child_pid = child_owner.id();
    reap_later(child_owner);
    let owner_pid = |session: &str| {
        if session.ends_with("-child") {
            child_pid
        } else {
            std::process::id()
        }
    };
    let hook = |session: &str, checkout: &Path, pane: Option<&str>, event: &str| {
        let mut hook = env.hook_command("claude");
        hook.current_dir(checkout)
            .env(rimz::harness::launch::ENV_AGENT_NAME, session)
            .env("RIMZ_AGENT_PID", owner_pid(session).to_string());
        if let Some(pane) = pane {
            hook.env("ZELLIJ", "1")
                .env("ZELLIJ_SESSION_NAME", &workspace.session_name)
                .env("ZELLIJ_PANE_ID", pane);
        }
        let output = env
            .spawn_payload(
                hook,
                &json!({"hook_event_name":event, "session_id":session, "cwd":checkout, "prompt":"work"})
                    .to_string(),
            )
            .wait_with_output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let child_of_old_coder = || rimz::agents::LaunchParams {
        parent_agent_id: Some(AgentSessionId::from("launch_old-coder")),
        parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
        launch_depth: Some(1),
        ..Default::default()
    };
    let mut occupants = vec![("sibling-coder", &sibling, Some("52"), team("sibling-coder"))];
    if case != Takeover::EmptyCheckout {
        occupants.push(("old-coder", &cwd, Some("51"), team("old-coder")));
    }
    if case == Takeover::IdleOccupants {
        occupants.push((
            "sweeper",
            &cwd,
            Some("53"),
            rimz::agents::LaunchParams {
                loop_task: Some("sweep".to_owned()),
                ..Default::default()
            },
        ));
        occupants.push(("solo", &docs, Some("54"), Default::default()));
        occupants.push(("old-child", &cwd, Some("55"), child_of_old_coder()));
    }
    if case == Takeover::FailedStop {
        occupants.push(("paneless-child", &cwd, None, child_of_old_coder()));
    }
    let seeded = occupants.len();
    for (session, checkout, pane, launch) in occupants {
        seed_agent_launch(&env, checkout, session, launch, None);
        hook(session, checkout, pane, "SessionStart");
    }
    if working {
        hook("old-coder", &cwd, Some("51"), "UserPromptSubmit");
    }
    let team_members = if case == Takeover::EmptyCheckout {
        1
    } else {
        2
    };
    assert_eq!(read_loop_instances(&env).0.len(), team_members);
    let open_run_pane = case == Takeover::OpenRunPane;
    if open_run_pane {
        let mut run = rimz::store::run::RunRecord::new(
            env.workspace_id.clone(),
            AgentKind::new_unchecked("claude"),
            rimz::agents::PermissionMode::Auto,
            "task".to_owned(),
            cwd.clone(),
        );
        run.agent_name = Some("old-coder".to_owned());
        run.pane_id = Some(rimz::ids::PaneId::from_parts(
            rimz::ids::MuxName::Zellij,
            "terminal_51",
        ));
        run.status = rimz::store::run::RunStatus::Completed;
        rimz::harness::run::create(store.paths(), &run).unwrap();
    }
    let runtime = env.runtime_paths();
    crate::common::room::seed_live_zellij_room(&runtime, &workspace.session_name, Vec::new());
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let trace = env.home_root.join("takeover.log");
    let command = || {
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij"])
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&agent_bin))
            .env(
                "RIMZ_ZELLIJ_BIN",
                crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
            )
            .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1s ago]\n", workspace.session_name),
            );
        if open_run_pane {
            command.env("RIMZ_TEST_ZELLIJ_FAIL_CLOSE_PANE", "1").env(
                "RIMZ_TEST_ZELLIJ_LIST_PANES",
                r#"[{"id":51,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
            );
        }
        command
    };
    write_loop_config(
        &env,
        &format!(
            "[tasks.resident]\nagent = {:?}\nprompt = \"repair\"\nroot = {:?}\ndir = {:?}\n{}\nstay = true\ntakeover = true\n",
            if case == Takeover::TeamLayout {
                "forge"
            } else {
                "codex"
            },
            env.project_root,
            launch_cwd,
            if symlinked {
                "each-worktree = true\nwhen = [\"pr=open\"]"
            } else {
                "every = \"1h\""
            },
        ),
    );
    let strikes = BTreeMap::from([(machine_task_key("resident"), 2_u32)]);
    std::fs::create_dir_all(loop_strikes_path(&env).parent().unwrap()).unwrap();
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&strikes).unwrap(),
    )
    .unwrap();
    let fire = || {
        let mut fire = command();
        fire.args(["loop", "run", "resident"]);
        if symlinked {
            fire.arg("--cwd")
                .arg(&launch_cwd)
                .arg("--condition-json")
                .arg(
                    json!({
                        "when": "pr=open", "hold": null, "held_ms": 0,
                        "readings": {"pr": "open"}
                    })
                    .to_string(),
                );
        }
        fire.output().unwrap()
    };
    let trace_text = || std::fs::read_to_string(&trace).unwrap_or_default();
    let ledger = || rimz::harness::schedule::launch_ledger::load(store.paths()).unwrap();
    let assists = || rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None);
    let checkout = rimz::utils::path::normalize_path_lexical(&launch_cwd);

    let output = fire();
    if case == Takeover::FailedStop || open_run_pane {
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = if open_run_pane {
            "terminal_51 is still open"
        } else {
            "no bound pane"
        };
        assert!(stderr.contains(reason), "{stderr}");
        assert!(!trace_text().contains("new-tab"), "{}", trace_text());
        assert!(ledger().is_empty());
        assert_eq!(last_loop_record(&env).result, LoopRunResult::Errored);
        assert!(assists().is_empty());
        return;
    }
    if working {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let trace_text = trace_text();
        assert!(!trace_text.contains("close-pane"), "{trace_text}");
        assert!(!trace_text.contains("new-tab"), "{trace_text}");
        assert!(ledger().is_empty());
        assert!(assists().is_empty());
        let record = last_loop_record(&env);
        assert_eq!(record.result, LoopRunResult::TakeoverBlocked);
        assert_eq!(record.checkout.as_deref(), Some(checkout.as_path()));
        let reason = record.error.as_deref().unwrap();
        assert!(reason.contains("old-coder"), "{reason}");
        assert!(reason.ends_with("is working"), "{reason}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!("{reason}; skipping")),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(
            serde_json::from_slice::<BTreeMap<String, u32>>(
                &std::fs::read(loop_strikes_path(&env)).unwrap()
            )
            .unwrap(),
            strikes
        );
        assert_eq!(read_loop_instances(&env).0.len(), 2);
        assert_eq!(
            store
                .runtime_projection(rimz::RuntimeScope::Audit)
                .unwrap()
                .agents
                .len(),
            seeded
        );

        hook("old-coder", &cwd, Some("51"), "Stop");
    }
    let output = if working { fire() } else { output };
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(last_loop_record(&env).result, LoopRunResult::Launched);
    assert!(ledger()["resident"].contains_key(&checkout));
    let assists = assists();
    assert_eq!(assists.len(), 1);
    let assist = serde_json::to_value(&assists[0]).unwrap();
    let trace_text = trace_text();
    let stops = trace_text.matches("close-pane").count();
    if case == Takeover::EmptyCheckout {
        assert_eq!(stops, 0, "{trace_text}");
        assert!(assist.get("stopped").is_none(), "{assist}");
        assert_eq!(read_loop_instances(&env).0.len(), 1);
        return;
    }
    let stopped: Vec<&str> = assist["stopped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|handle| handle.as_str().unwrap())
        .collect();
    let expected: &[&str] = if case == Takeover::IdleOccupants {
        &["old-coder", "sweeper", "solo", "old-child"]
    } else {
        &["old-coder"]
    };
    assert_eq!(stopped.len(), expected.len(), "{stopped:?}");
    for name in expected {
        assert!(
            stopped.iter().any(|handle| handle.contains(name)),
            "{name} missing from {stopped:?}"
        );
    }
    assert_eq!(stops, expected.len(), "{trace_text}");
    assert!(
        trace_text.rfind("close-pane").unwrap() < trace_text.find("new-tab").unwrap(),
        "{trace_text}"
    );
    if case == Takeover::TeamLayout {
        // The hold admits a relaunch of the team that holds the checkout.
        let agents = store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents;
        let mut launched: Vec<(Option<&str>, Option<&str>)> = agents
            .iter()
            .filter(|agent| {
                !agent
                    .name
                    .as_deref()
                    .is_some_and(|name| name.ends_with("-coder"))
            })
            .map(|agent| {
                assert_eq!(agent.team.as_deref(), Some("forge"));
                assert_eq!(agent.worktree_path.as_deref(), checkout.to_str());
                (agent.role.as_deref(), agent.loop_task.as_deref())
            })
            .collect();
        launched.sort();
        assert_eq!(
            launched,
            [(Some("coder"), Some("resident")), (Some("reviewer"), None)]
        );
        return;
    }
    let subscriptions = read_loop_instances(&env);
    assert_eq!(subscriptions.0.len(), 1);
    assert_eq!(
        subscriptions
            .0
            .values()
            .next()
            .unwrap()
            .wait
            .as_ref()
            .unwrap()
            .session
            .as_str(),
        "sibling-coder"
    );
    assert_eq!(
        store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents
            .iter()
            .filter(|agent| agent.loop_task.as_deref() == Some("resident"))
            .count(),
        1
    );
    stamp_session_ended(&env, "old-coder");
    let output = command().args(["teams", "stop", "forge"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(read_loop_instances(&env).0.is_empty());
}

/// An owned worktree whose marker path runs through a symlinked worktree
/// directory, so its lexical and physical forms differ.
#[cfg(unix)]
fn symlinked_owned_worktree(env: &Env) -> std::path::PathBuf {
    let actual = env.home_root.join("actual-worktrees");
    let alias = env.home_root.join("linked-worktrees");
    std::fs::create_dir(&actual).unwrap();
    std::os::unix::fs::symlink(&actual, &alias).unwrap();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        format!("[agents.worktree]\ndir = {:?}\n", alias.to_str().unwrap()),
    )
    .unwrap();
    loop_ok(env, &["worktree", "new", "lane"]);
    alias.join("lane")
}

fn publish_claude_5h(env: &Env, used: u8) {
    publish_claude_5h_resetting(env, used, SignedDuration::from_hours(2));
}

fn publish_claude_5h_resetting(env: &Env, used: u8, resets_in: SignedDuration) {
    env.publish_rate_limits(&RateLimitsCache {
        entries: BTreeMap::from([(
            "claude@default".parse().unwrap(),
            RateLimitCacheEntry {
                limits: AgentRateLimits {
                    windows: vec![RateLimitWindow {
                        used_percentage: Some(used),
                        resets_at: Some(Timestamp::now() + resets_in),
                        duration_mins: Some(rimz::agents::WindowSpan::FiveHour.minutes()),
                        ..RateLimitWindow::default()
                    }],
                },
                ..Default::default()
            },
        )]),
        ..Default::default()
    });
}

#[test]
fn after_reset_fires_once_at_the_window_reset_and_removes_its_row() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-reset", "feature-loop");
    let add = |name: &str, extra: &[&str]| {
        calling_loop(&env, "sess-reset")
            .args(["loop", "add", name, "--after-reset", "5h"])
            .args(extra)
            .output()
            .unwrap()
    };
    for (extra, expected) in [
        (
            &["--check", "true"][..],
            "--after-reset requires --agent or --wait",
        ),
        (
            &["--project", "--agent", "claude", "--prompt", "go"][..],
            "--project tasks cannot use --after-reset",
        ),
        (&["--wait", "--in", "5m"][..], "cannot be used with"),
    ] {
        let refused = add("refused", extra);
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            !refused.status.success() && stderr.contains(expected),
            "{extra:?}: {stderr}"
        );
    }
    let refused = add("refused", &["--wait"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("no current claude 5h window reading; open the room's sidebar"),
        "no cache: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    publish_claude_5h_resetting(&env, 30, SignedDuration::from_mins(-1));
    let refused = add("refused", &["--wait"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("no current claude 5h window reading"),
        "expired cache: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        read_loop_instances(&env).0.is_empty(),
        "a refusal writes nothing"
    );

    publish_claude_5h(&env, 30);
    let started = add("later", &["--wait"]);
    let receipt = String::from_utf8_lossy(&started.stdout);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    for line in [
        "trigger: fires once after the next claude 5h reset\n",
        "window: claude 5h · 70% left · resets ",
        "next fire: ",
    ] {
        assert!(receipt.contains(line), "{line}: {receipt}");
    }
    assert!(read_loop_instances(&env).0["later"].fire_at.is_some());
    loop_ok(&env, &["loop", "tick"]);
    loop_ok(&env, &["loop", "tick"]);
    assert!(read_loop_run_records(&env).is_empty());
    assert!(read_loop_instances(&env).0.contains_key("later"));

    publish_claude_5h_resetting(&env, 0, SignedDuration::from_hours(5));
    let fresh = add("now", &["--wait"]);
    let receipt = String::from_utf8_lossy(&fresh.stdout);
    for line in [
        "trigger: fires once after the next claude 5h reset\n",
        "window: claude 5h · not started · 100% left\n",
        "next fire: the next scheduler tick, since the window has not started\n",
    ] {
        assert!(receipt.contains(line), "{line}: {receipt}");
    }
    loop_ok(&env, &["loop", "tick"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while (read_loop_run_records(&env).is_empty()
        || read_loop_instances(&env).0.contains_key("now"))
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(last_loop_record(&env).task, "now");
    let rows = read_loop_instances(&env).0;
    assert!(!rows.contains_key("now"));
    assert!(rows.contains_key("later"));
}

#[test]
fn window_condition_waits_for_room_in_the_provider_window() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-window", "feature-loop");
    publish_claude_5h(&env, 92);
    let output = calling_loop(&env, "sess-window")
        .args([
            "loop",
            "add",
            "roomy",
            "--wait",
            "--when",
            "window.5h.left >= 40",
            "--once",
        ])
        .output()
        .unwrap();
    let receipt = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        receipt.contains("now: waiting · window.5h.left: 8"),
        "{receipt}"
    );
    let row = &read_loop_instances(&env).0["roomy"];
    assert_eq!(
        row.when.as_deref(),
        Some(&["window.5h.left>=40".to_owned()][..])
    );
    assert_eq!(row.provider, Some(AgentKind::new_unchecked("claude")));
    loop_ok(&env, &["loop", "tick"]);
    loop_ok(&env, &["loop", "tick"]);
    assert!(read_loop_run_records(&env).is_empty());
    publish_claude_5h(&env, 10);
    loop_ok(&env, &["loop", "tick"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while (read_loop_run_records(&env).is_empty()
        || read_loop_instances(&env).0.contains_key("roomy"))
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(25));
    }
    let condition = last_loop_record(&env).condition.unwrap();
    assert_eq!(condition.when, "window.5h.left>=40");
    assert_eq!(condition.readings["window.5h.left"].as_deref(), Some("90"));
    assert!(!read_loop_instances(&env).0.contains_key("roomy"));
}

#[test]
fn condition_wait_pins_projects_and_deduplicates() {
    let env = Env::new();
    assert!(init_git_repo(&env.project_root));
    let first = env.home_root.join("condition-first");
    let second = env.home_root.join("condition-second");
    for (branch, path) in [("first", &first), ("second", &second)] {
        assert!(git_ok(
            &env.project_root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                path.to_str().unwrap()
            ]
        ));
    }
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-condition", "feature-loop");
    for (name, scope, expected) in [
        ("ready", &first, "added loop task `ready`"),
        ("duplicate", &first, "already subscribed as ready"),
        ("second", &second, "added loop task `second`"),
        ("second-duplicate", &second, "already subscribed as second"),
    ] {
        let output = calling_loop(&env, "sess-condition")
            .args([
                "loop",
                "add",
                name,
                "--wait",
                "--when",
                "team.stage=Done",
                "--for",
                "30m",
                "--once",
            ])
            .arg("--root")
            .arg(scope)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    let instances = read_loop_instances(&env);
    assert_eq!(instances.0.len(), 2);
    assert_eq!(instances.0["ready"].run_dir(), canonical(&first));
    assert_eq!(instances.0["second"].run_dir(), canonical(&second));
    assert_eq!(
        instances.0["ready"].when.as_ref().unwrap(),
        &["team.stage=Done"]
    );
    assert_eq!(instances.0["ready"].hold.as_deref(), Some("30m"));
    let output = env
        .rimz()
        .args(["sidebar", "snapshot", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        snapshot
            .to_string()
            .contains("\"when\":\"team.stage=Done\""),
        "{snapshot}"
    );
    loop_ok(&env, &["loop", "run", "ready"]);
    assert_eq!(
        env.store().list_pending_messages().unwrap()[0].sender,
        rimz::store::message::MessageSender::Harness {
            notice: rimz::store::message::HarnessNotice::Wait
        }
    );
}
#[test]
fn trunk_signal_fires_only_through_git_source() {
    let env = Env::new();
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "trunk",
            "--signal",
            "trunk.moved",
            "--check",
            "true",
        ],
    );
    let refused = env
        .rimz()
        .args(["events", "emit", "trunk.moved"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("reserved"));
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "trunk",
    );
    loop_ok(&env, &["events", "emit", "trunk.moved", "--source", "git"]);
    wait_for_loop_run_records(&env, "trunk Completed", |records| {
        records
            .iter()
            .any(|r| r.task == "trunk" && r.result == LoopRunResult::Completed)
    });
    assert!(
        read_loop_run_records(&env)
            .iter()
            .any(|r| r.task == "trunk" && r.result == LoopRunResult::Completed)
    );
}

#[test]
fn worktree_created_fires_a_loop_subscriber() {
    let env = Env::new();
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&env.project_root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "created",
            "--signal",
            "worktree.created",
            "--check",
            "true",
        ],
    );
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "created",
    );
    loop_ok(&env, &["worktree", "new", "signal-test"]);
    wait_for_loop_run_records(&env, "created Completed", |records| {
        records
            .iter()
            .any(|r| r.task == "created" && r.result == LoopRunResult::Completed)
    });
    assert!(
        read_loop_run_records(&env)
            .iter()
            .any(|r| r.task == "created" && r.result == LoopRunResult::Completed)
    );
}

#[test]
fn forge_behind_signal_fires_matching_task_and_skips_merged_sibling() {
    let env = Env::new();
    for (name, signal) in [("behind", "pr.behind"), ("merged", "pr.merged")] {
        loop_ok(
            &env,
            &[
                "loop",
                "add",
                name,
                "--signal",
                signal,
                "--match",
                "branch=feature",
                "--check",
                "true",
            ],
        );
    }
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "behind",
    );
    for branch in ["feature", "other"] {
        loop_ok(
            &env,
            &[
                "events",
                "emit",
                "pr.behind",
                "--source",
                "forge",
                "--json",
                &json!({"branch": branch}).to_string(),
            ],
        );
    }
    wait_for_loop_run_records(
        &env,
        "behind Completed and merged SignalSkipped",
        |records| {
            records.len() == 2
                && records.iter().any(|record| {
                    record.task == "behind" && record.result == LoopRunResult::Completed
                })
                && records.iter().any(|record| {
                    record.task == "merged" && record.result == LoopRunResult::SignalSkipped
                })
        },
    );
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 2, "{records:?}");
    assert!(
        records
            .iter()
            .any(|record| record.task == "behind" && record.result == LoopRunResult::Completed),
        "{records:?}"
    );
    assert!(
        records
            .iter()
            .any(|record| record.task == "merged" && record.result == LoopRunResult::SignalSkipped),
        "{records:?}"
    );
}

#[test]
fn wildcard_team_binding_arms_at_root_and_delivers_across_worktrees() {
    let env = Env::new();
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return;
    }
    let cwd = env.home_root.join("feature-team");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature-team",
            cwd.to_str().unwrap()
        ]
    ));
    env.install_agent_hooks("claude");
    env.write_config(&env.project_root, r#"
        [profiles.claude]
        agent = "claude"
        [[agents.teams.forge.roles]]
        role = "coder"
        profile = "claude"
        signals = [{ signal = "pr.conflicted", match = { branch = "*" } }, { signal = "team.stage", match = { team = "*", to = "Done" } }]
    "#);
    loop_ok(&env, &["trust", "grant"]);
    let launch = env.rimz().args(["teams", "forge"]).output().unwrap();
    let error = String::from_utf8_lossy(&launch.stderr);
    assert!(!error.contains("root checkout"), "{error}");
    assert!(!error.contains("unknown team"), "{error}");
    seed_team_signal_member(&env, &env.project_root, "sess-any", None);
    team_signal_hook(&env, &env.project_root, "sess-any", "SessionStart");
    let armed = read_loop_instances(&env);
    assert_eq!(armed.0.len(), 2, "launch stderr: {error}");
    for (name, matches) in [
        (
            "team-forge-feature-team-coder-pr-conflicted",
            BTreeMap::from([("branch".to_owned(), "*".to_owned())]),
        ),
        (
            "team-forge-feature-team-coder-team-stage",
            BTreeMap::from([
                ("team".to_owned(), "*".to_owned()),
                ("to".to_owned(), "Done".to_owned()),
            ]),
        ),
    ] {
        let entry = &armed.0[name];
        assert_eq!(
            entry.team.as_ref().unwrap().to_string(),
            "forge#feature-team"
        );
        assert_eq!(entry.matches.as_ref(), Some(&matches));
    }
    let listing = loop_ok(&env, &["loop", "list"]);
    assert_eq!(listing.matches("↳ team forge").count(), 2, "{listing}");
    team_signal_hook(&env, &env.project_root, "sess-any", "UserPromptSubmit");
    for payload in [
        json!({"path":cwd,"repo":"o/r","number":7}),
        json!({"branch":"feature-team","path":cwd,"repo":"o/r","number":7}),
    ] {
        let output = env
            .rimz()
            .current_dir(&cwd)
            .args([
                "events",
                "emit",
                "pr.conflicted",
                "--source",
                "forge",
                "--json",
                &payload.to_string(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if payload.get("branch").is_none() {
            assert!(read_loop_run_records(&env).is_empty());
            assert!(env.store().list_pending_messages().unwrap().is_empty());
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(read_loop_run_records(&env).iter().any(|record| record.task
        == "team-forge-feature-team-coder-pr-conflicted"
        && record.result == LoopRunResult::Delivered));
    assert_pending_message(&env, "sess-any", "feature-team");
}

#[test]
fn team_signal_binding_registers_delivers_and_retires() {
    let env = Env::new();
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    seed_team_signal_member(&env, &cwd, "sess-team-coder", None);
    team_signal_hook(&env, &cwd, "sess-team-coder", "SessionStart");
    let audit = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap();
    let member = audit
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-team-coder")
        .unwrap();
    assert_eq!(
        member.launch_id.as_ref().unwrap().as_str(),
        "launch_sess-team-coder"
    );
    let armed = read_loop_instances(&env);
    assert_eq!(armed.0.len(), 1);
    let entry = &armed.0["team-forge-feature-team-coder-ci-failed"];
    assert_eq!(
        entry.team.as_ref().unwrap().to_string(),
        "forge#feature-team"
    );
    assert_eq!(entry.signal.as_deref(), Some("ci.failed"));
    assert!(entry.once.is_none() && entry.deadline.is_none());
    assert_eq!(
        entry.matches.as_ref().unwrap()["path"],
        cwd.display().to_string()
    );
    assert_eq!(
        entry.wait.as_ref().unwrap().session.as_str(),
        "sess-team-coder"
    );
    team_signal_hook(&env, &cwd, "sess-team-coder", "SessionStart");
    assert_eq!(read_loop_instances(&env), armed);
    team_signal_hook(&env, &cwd, "sess-team-coder", "UserPromptSubmit");
    loop_ok(&env, &["events", "emit", "deploy.done"]);
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    assert!(read_loop_run_records(&env).is_empty());
    for (path, matching) in [(&env.project_root, false), (&cwd, true)] {
        loop_ok(
            &env,
            &[
                "events",
                "emit",
                "ci.failed",
                "--source",
                "forge",
                "--json",
                &json!({"path": path}).to_string(),
            ],
        );
        if !matching {
            assert!(env.store().list_pending_messages().unwrap().is_empty());
            assert!(read_loop_run_records(&env).is_empty());
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_pending_message(&env, "sess-team-coder", "ci.failed");
    assert_eq!(
        env.store().list_pending_messages().unwrap()[0].sender,
        rimz::store::message::MessageSender::Harness {
            notice: rimz::store::message::HarnessNotice::Signal,
        }
    );
    assert_eq!(last_loop_record(&env).result, LoopRunResult::Delivered);
    assert_eq!(read_loop_instances(&env), armed);
    team_signal_hook(&env, &cwd, "sess-team-coder", "SessionEnd");
    assert!(read_loop_instances(&env).0.is_empty());
}

#[test]
fn team_signal_registration_preserves_same_named_machine_configuration() {
    let env = Env::new();
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    let config = format!(
        "[tasks.team-forge-feature-team-coder-ci-failed]\ncheck = \"true\"\nevery = \"15m\"\nroot = {:?}\n",
        env.project_root.display().to_string()
    );
    write_loop_config(&env, &config);
    seed_team_signal_member(&env, &cwd, "configured-session", None);
    let mut command = env.hook_command("claude");
    command
        .current_dir(&cwd)
        .env(rimz::harness::launch::ENV_AGENT_NAME, "configured-session")
        .env(rimz::workspace::ENV_CHANNEL, "feature-team");
    let output = env.spawn_payload(command, &json!({ "hook_event_name": "SessionStart", "session_id": "configured-session", "cwd": cwd }).to_string()).wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(loop_config_path(&env)).unwrap(),
        config
    );
    assert!(read_loop_instances(&env).0.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("configuration-owned"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn team_signal_slug_collisions_preserve_distinct_subscriptions() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    write_team_signal_config(&env);
    crate::common::write_definition(
        &env,
        "teams",
        "forge",
        r#"leader: coder
stages: [Build]
roles:
  - role: coder
    agent: worker
    owns: [Build]
    signals:
      - deploy.foo-bar
      - deploy.foo_bar
      - {signal: deploy.done, match: {target: first}}
      - {signal: deploy.done, match: {target: second}}
      - deploy.done-2
      - deploy.done-2-2"#,
        "Complete the work.",
    );
    seed_team_signal_member(&env, &env.project_root, "slug-session", None);
    team_signal_hook(&env, &env.project_root, "slug-session", "SessionStart");
    let tasks = read_loop_instances(&env);
    assert_eq!(tasks.0.len(), 6);
    assert_eq!(
        tasks
            .0
            .values()
            .map(|entry| (
                entry.signal.as_deref().unwrap(),
                entry
                    .matches
                    .as_ref()
                    .and_then(|matches| matches.get("target"))
                    .map(String::as_str),
            ))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            ("deploy.foo-bar", None),
            ("deploy.foo_bar", None),
            ("deploy.done", Some("first")),
            ("deploy.done", Some("second")),
            ("deploy.done-2", None),
            ("deploy.done-2-2", None),
        ])
    );
    team_signal_hook(&env, &env.project_root, "slug-session", "SessionStart");
    assert_eq!(read_loop_instances(&env), tasks);
}

#[test]
fn team_signal_bindings_ignore_child_registration() {
    let env = Env::new();
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    seed_team_signal_member(&env, &cwd, "sess-team-child", Some("sess-parent"));
    team_signal_hook(&env, &cwd, "sess-team-child", "SessionStart");
    let audit = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap();
    let child = audit
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-team-child")
        .unwrap();
    assert!(child.parent_agent_id.is_some());
    assert!(read_loop_instances(&env).0.is_empty());
}

#[test]
fn team_signal_binding_resume_keeps_session_rows_separate() {
    let env = Env::new();
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    seed_team_signal_member(&env, &cwd, "sess-team-old", None);
    team_signal_hook(&env, &cwd, "sess-team-old", "SessionStart");
    seed_team_signal_member(&env, &cwd, "sess-team-new", None);
    team_signal_hook(&env, &cwd, "sess-team-new", "SessionStart");
    team_signal_hook(&env, &cwd, "sess-team-old", "SessionEnd");
    let armed = read_loop_instances(&env);
    assert_eq!(armed.0.len(), 1);
    assert_eq!(
        armed
            .0
            .values()
            .next()
            .unwrap()
            .wait
            .as_ref()
            .unwrap()
            .session
            .as_str(),
        "sess-team-new"
    );
    team_signal_hook(&env, &cwd, "sess-team-new", "UserPromptSubmit");
    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "ci.failed",
            "--source",
            "forge",
            "--json",
            &json!({"path": cwd}).to_string(),
        ],
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_pending_message(&env, "sess-team-new", "ci.failed");
}

#[test]
fn team_idle_and_root_end_hooks_deliver_only_to_the_matching_instance() {
    for (signal, hook) in [("team.idle", "Stop"), ("team.ended", "SessionEnd")] {
        let env = Env::new();
        let Some(cwd) = team_signal_fixture(&env) else {
            return;
        };
        register_running_agent(&env, "sess-team-observer", "main");
        seed_team_signal_member(&env, &cwd, "sess-team-member", None);
        team_signal_hook(&env, &cwd, "sess-team-member", "SessionStart");
        team_signal_hook(&env, &cwd, "sess-team-member", "UserPromptSubmit");
        for (name, instance) in [
            ("matching", "forge#feature-team"),
            ("sibling", "forge#sibling"),
        ] {
            loop_ok(
                &env,
                &[
                    "loop",
                    "add",
                    name,
                    "--wait",
                    "@claude#project",
                    "--signal",
                    signal,
                    "--match",
                    &format!("instance={instance}"),
                    "--once",
                ],
            );
        }
        team_signal_hook(&env, &cwd, "sess-team-member", hook);
        let deadline = Instant::now() + Duration::from_secs(5);
        while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        let records = read_loop_run_records(&env);
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].result, LoopRunResult::Delivered);
        let evidence = records[0].signal.as_ref().unwrap();
        assert_eq!(evidence.name.as_str(), signal);
        assert_eq!(evidence.payload["instance"], "forge#feature-team");
        assert_pending_message(&env, "sess-team-observer", signal);
        let instances = read_loop_instances(&env);
        assert!(!instances.0.contains_key("matching"));
        assert!(instances.0.contains_key("sibling"));
    }
}

#[test]
fn team_signal_launch_refuses_root_before_side_effects() {
    let env = Env::new();
    write_team_signal_config(&env);
    let before = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap();
    let (_, error) = loop_fail(&env, &["teams", "forge"]);
    assert!(
        error.contains("team `forge` role `coder` signal binding 1"),
        "{error}"
    );
    assert!(
        error.contains("root checkout needs an explicit scope"),
        "{error}"
    );
    assert!(error.contains("launch with -w"), "{error}");
    assert_eq!(
        env.store()
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents,
        before.agents
    );
    assert!(read_loop_instances(&env).0.is_empty());
    assert!(!env.project_root.join(".worktrees").exists());
}

fn write_team_signal_config(env: &Env) {
    crate::common::write_definition(
        env,
        "agents",
        "claude",
        "description: Claude base",
        "Follow instructions.",
    );
    crate::common::write_definition(
        env,
        "agents",
        "worker",
        "description: Worker\nagent: claude\ntools: []",
        "",
    );
    crate::common::write_definition(
        env,
        "teams",
        "forge",
        "leader: coder\nstages: [Build]\nroles:\n  - {role: coder, agent: worker, owns: [Build], signals: [ci.failed]}\n  - {role: reviewer, agent: worker, signals: [deploy.done]}",
        "Complete the work.",
    );
}

fn team_signal_fixture(env: &Env) -> Option<std::path::PathBuf> {
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return None;
    }
    let cwd = env.home_root.join("feature-team");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature-team",
            cwd.to_str().unwrap()
        ]
    ));
    env.install_agent_hooks("claude");
    write_team_signal_config(env);
    Some(cwd)
}

fn seed_team_signal_member(env: &Env, cwd: &Path, session: &str, parent: Option<&str>) {
    seed_agent_launch(
        env,
        cwd,
        session,
        rimz::agents::LaunchParams {
            team: Some("forge".to_owned()),
            role: Some("coder".to_owned()),
            channel: Some("feature-team".to_owned()),
            parent_agent_id: parent.map(AgentSessionId::from),
            parent_agent_kind: parent.map(|_| AgentKind::new_unchecked("claude")),
            launch_depth: parent.map(|_| 1),
            ..Default::default()
        },
        None,
    );
}

fn seed_agent_launch(
    env: &Env,
    cwd: &Path,
    session: &str,
    launch: rimz::agents::LaunchParams,
    runtime_owner: Option<rimz::pane::RuntimeOwner>,
) {
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_event(&rimz::store::event::EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            rimz::store::event::AgentLaunchPayload {
                agent_id: AgentSessionId::from(format!("launch_{session}")),
                launch_id: Some(AgentSessionId::from(format!("launch_{session}"))),
                agent_name: session.to_owned(),
                agent_name_explicit: true,
                launch,
                state: rimz::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner,
                worktree_path: Some(cwd.display().to_string()),
                worktree_branch: Some("feature-team".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
}

fn team_signal_hook(env: &Env, cwd: &Path, session: &str, event: &str) {
    let mut command = env.hook_command("claude");
    command
        .current_dir(cwd)
        .env(rimz::harness::launch::ENV_AGENT_NAME, session)
        .env(rimz::workspace::ENV_CHANNEL, "feature-team");
    let payload =
        json!({"hook_event_name": event, "session_id": session, "cwd": cwd, "prompt": "work"})
            .to_string();
    let output = env
        .spawn_payload(command, &payload)
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("failed to arm team"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn loop_deliveries_always_persist_as_instances() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-instance-storage", "feature-loop");
    for (name, trigger) in [
        ("recurring", ["--every", "15m"]),
        ("standing", ["--signal", "deploy.done"]),
        ("one-shot", ["--in", "1h"]),
    ] {
        loop_ok(
            &env,
            &[
                "loop", "add", name, "--wait", "@claude", trigger[0], trigger[1],
            ],
        );
    }
    let instances = read_loop_instances(&env);
    for name in ["recurring", "standing", "one-shot"] {
        assert!(instances.0[name].wait.is_some());
    }
    let config = std::fs::read_to_string(loop_config_path(&env)).unwrap_or_default();
    assert!(!config.contains("[tasks."), "{config}");
    let (_, error) = loop_fail(
        &env,
        &[
            "loop",
            "add",
            "project-wait",
            "--project",
            "--wait",
            "@claude",
            "--every",
            "15m",
        ],
    );
    assert!(error.contains("--project"), "{error}");
}

#[test]
fn loop_wait_me_and_bare_wait_pin_the_calling_session() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-caller", "feature-loop");
    for (name, wait) in [
        ("explicit", vec!["--wait", "@me"]),
        ("bare", vec!["--wait"]),
    ] {
        let output = calling_loop(&env, "sess-loop-caller")
            .args(["loop", "add", name, "--every", "15m"])
            .args(wait)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let instances = read_loop_instances(&env);
        let target = instances.0[name].wait.as_ref().unwrap();
        assert_eq!(target.kind, AgentKind::new_unchecked("claude"));
        assert_eq!(target.session, AgentSessionId::from("sess-loop-caller"));
        assert!(instances.0[name].wait_meta.is_none());
    }
}

#[test]
fn loop_signal_dedupe_preserves_the_existing_definition_and_overlays() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-dedupe", "feature-loop");
    let output = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "original",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
            "--match",
            "zone=west",
            "--match",
            "status=green",
            "--prompt",
            "keep this prompt",
        ],
    );
    assert!(output.contains("added loop task `original`"), "{output}");
    let mut original = read_loop_instances(&env);
    original.0.get_mut("original").unwrap().team = Some("forge#feature-loop".parse().unwrap());
    write_loop_instances(&env, original.clone());
    let key = project_task_key(&env.project_root, "original");
    let arming = BTreeMap::from([(
        key.clone(),
        Arming {
            enabled: true,
            at: Some(Timestamp::from_second(1).unwrap()),
            pause_until: Some(Timestamp::from_second(2).unwrap()),
            strikes: None,
        },
    )]);
    write_loop_arming(&env, &arming);
    let strikes = BTreeMap::from([(key, 2_u32)]);
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&strikes).unwrap(),
    )
    .unwrap();
    let output = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "replacement",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
            "--match",
            "status=green",
            "--match",
            "zone=west",
            "--once",
            "--prompt",
            "discard this prompt",
        ],
    );
    assert!(
        output.contains("already subscribed as original"),
        "{output}"
    );
    assert_eq!(read_loop_instances(&env), original);
    assert_eq!(read_loop_arming(&env), arming);
    assert_eq!(read_loop_strikes(&env), strikes);
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "different-match",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
            "--match",
            "zone=east",
            "--match",
            "status=green",
        ],
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "different-selector",
            "--wait",
            "@claude",
            "--signal",
            "deploy.failed",
            "--match",
            "zone=west",
            "--match",
            "status=green",
        ],
    );
    assert_eq!(read_loop_instances(&env).0.len(), 3);
    register_running_agent(&env, "sess-loop-other-target", "feature-other");
    let output = calling_loop(&env, "sess-loop-other-target")
        .args([
            "loop",
            "add",
            "different-target",
            "--wait",
            "@me",
            "--signal",
            "deploy.done",
            "--match",
            "status=green",
            "--match",
            "zone=west",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let instances = read_loop_instances(&env);
    assert_eq!(instances.0.len(), 4);
    assert_eq!(
        instances.0["different-target"]
            .wait
            .as_ref()
            .unwrap()
            .session,
        AgentSessionId::from("sess-loop-other-target")
    );
}

#[test]
fn concurrent_signal_adds_return_one_existing_name() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-concurrent", "feature-loop");
    let children = ["first", "second"].map(|name| {
        env.rimz()
            .args([
                "loop",
                "add",
                name,
                "--wait",
                "@claude",
                "--signal",
                "deploy.done",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    });
    let outputs = children.map(|child| {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    });
    let instances = read_loop_instances(&env);
    assert_eq!(instances.0.len(), 1);
    let name = instances.0.keys().next().unwrap();
    for output in outputs {
        assert!(output.contains(name), "{output}");
    }
}

#[test]
fn loop_signal_defaults_follow_the_caller_worktree_and_team() {
    let env = Env::new();
    let Some(cwd) = team_signal_fixture(&env) else {
        return;
    };
    register_running_agent(&env, "sess-scope-target", "main");
    seed_team_signal_member(&env, &cwd, "sess-scope-caller", None);
    team_signal_hook(&env, &cwd, "sess-scope-caller", "SessionStart");
    for (name, signal, key, expected) in [
        ("caller-ci", "ci.passed", "path", cwd.display().to_string()),
        (
            "caller-team",
            "team.idle",
            "instance",
            "forge#feature-team".to_owned(),
        ),
    ] {
        let output = calling_loop(&env, "launch_sess-scope-caller")
            .env("RIMZ_AGENT_NAME", "sess-scope-caller")
            .args([
                "loop",
                "add",
                name,
                "--wait",
                "@claude#project",
                "--signal",
                signal,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let instances = read_loop_instances(&env);
        assert_eq!(instances.0[name].matches.as_ref().unwrap()[key], expected);
        assert_eq!(
            instances.0[name].wait.as_ref().unwrap().session,
            AgentSessionId::from("sess-scope-target")
        );
    }
    let output = calling_loop(&env, "sess-scope-target")
        .args(["loop", "add", "root-ci", "--wait", "--signal", "ci.failed"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("root checkout needs an explicit scope"),
        "{error}"
    );
    assert!(!read_loop_instances(&env).0.contains_key("root-ci"));
    let output = calling_loop(&env, "sess-scope-target")
        .args([
            "loop",
            "add",
            "root-any",
            "--wait",
            "--signal",
            "ci.failed",
            "--match",
            "branch=*",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read_loop_instances(&env).0["root-any"].matches,
        Some(BTreeMap::from([("branch".to_owned(), "*".to_owned())]))
    );
}

#[test]
fn signal_siblings_keep_subscriptions_and_matches_consume_only_once() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-signal-lifetimes", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "standing",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
            "--match",
            "branch=feature-loop",
        ],
    );
    let mut instances = read_loop_instances(&env);
    let mut once = instances.0["standing"].clone();
    once.once = Some(true);
    instances.0.insert("once".to_owned(), once);
    write_loop_instances(&env, instances.clone());
    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.done",
            "--json",
            r#"{"branch":"sibling"}"#,
        ],
    );
    assert_eq!(read_loop_instances(&env), instances);
    assert!(read_loop_run_records(&env).is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.done",
            "--json",
            r#"{"branch":"feature-loop"}"#,
        ],
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 2, "{records:?}");
    assert!(
        records
            .iter()
            .all(|record| record.result == LoopRunResult::Delivered)
    );
    instances.0.remove("once");
    assert_eq!(read_loop_instances(&env), instances);
    let messages = env.store().list_pending_messages().unwrap();
    assert_eq!(messages.len(), 2);
    assert!(messages.iter().all(|message| message.sender
        == rimz::store::message::MessageSender::Harness {
            notice: rimz::store::message::HarnessNotice::Signal,
        }));
}

#[test]
fn session_end_hook_retires_all_own_deliveries_and_their_overlays() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-retire", "feature-loop");
    for (name, trigger) in [
        ("clock", ["--every", "15m"]),
        ("signal", ["--signal", "deploy.done"]),
    ] {
        loop_ok(
            &env,
            &[
                "loop", "add", name, "--wait", "@claude", trigger[0], trigger[1],
            ],
        );
    }
    loop_ok(&env, &["loop", "disable", "clock"]);
    loop_ok(&env, &["loop", "pause", "signal", "--for", "2h"]);
    let mut instances = read_loop_instances(&env);
    let mut sibling = instances.0["clock"].clone();
    sibling.wait.as_mut().unwrap().session = AgentSessionId::from("sess-retire-sibling");
    instances.0.insert("sibling".to_owned(), sibling.clone());
    write_loop_instances(&env, instances);
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&BTreeMap::from([
            (project_task_key(&env.project_root, "clock"), 2_u32),
            (project_task_key(&env.project_root, "signal"), 1),
        ]))
        .unwrap(),
    )
    .unwrap();
    run_hook(
        &env,
        json!({"hook_event_name": "SessionEnd", "session_id": "sess-retire"}),
        &env.project_root,
    );
    assert_eq!(
        read_loop_instances(&env).0,
        BTreeMap::from([("sibling".to_owned(), sibling)])
    );
    for name in ["clock", "signal"] {
        let key = project_task_key(&env.project_root, name);
        assert!(!read_loop_arming(&env).contains_key(&key));
        assert!(!read_loop_strikes(&env).contains_key(&key));
    }
}

#[test]
fn non_hook_end_retires_matching_subscription_before_delivery() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-ended", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ended",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
            "--match",
            "branch=feature-loop",
        ],
    );
    register_running_agent(&env, "sess-live", "feature-loop");
    let mut instances = read_loop_instances(&env);
    let mut live = instances.0["ended"].clone();
    live.wait.as_mut().unwrap().session = AgentSessionId::from("sess-live");
    instances.0.insert("live".to_owned(), live.clone());
    write_loop_instances(&env, instances);
    loop_ok(&env, &["loop", "enable", "ended"]);
    let key = project_task_key(&env.project_root, "ended");
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&BTreeMap::from([(key.clone(), 1_u32)])).unwrap(),
    )
    .unwrap();
    stamp_session_ended(&env, "sess-ended");
    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.done",
            "--json",
            r#"{"branch":"feature-loop"}"#,
        ],
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].task, "live");
    assert_eq!(records[0].result, LoopRunResult::Delivered);
    assert_pending_message(&env, "sess-live", "deploy.done");
    assert_eq!(
        read_loop_instances(&env).0,
        BTreeMap::from([("live".to_owned(), live)])
    );
    assert!(!read_loop_arming(&env).contains_key(&key));
    assert!(!read_loop_strikes(&env).contains_key(&key));
}

/// The reported symptom: a listener whose family mostly fires siblings never
/// reaches the delivering path's liveness gate, so the sibling fire itself has
/// to refuse a durably ended target. A live sibling session and a target with no
/// agent row at all still record their skip — the latter stays gc's business.
#[test]
fn sibling_fire_retires_only_the_durably_ended_subscription() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-ended", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ended",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
        ],
    );
    register_running_agent(&env, "sess-live", "feature-loop");
    let mut instances = read_loop_instances(&env);
    let template = instances.0["ended"].clone();
    for (name, session) in [("live", "sess-live"), ("absent", "sess-never-registered")] {
        let mut row = template.clone();
        row.wait.as_mut().unwrap().session = AgentSessionId::from(session);
        instances.0.insert(name.to_owned(), row);
    }
    write_loop_instances(&env, instances.clone());
    loop_ok(&env, &["loop", "enable", "ended"]);
    let key = project_task_key(&env.project_root, "ended");
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&BTreeMap::from([(key.clone(), 2_u32)])).unwrap(),
    )
    .unwrap();
    stamp_session_ended(&env, "sess-ended");

    // Same family, different exact name: the skip branch, which returns before
    // the fire path's only liveness gate.
    loop_ok(&env, &["events", "emit", "deploy.started"]);

    let records = read_loop_run_records(&env);
    assert_eq!(
        records
            .iter()
            .map(|record| (record.task.as_str(), record.result))
            .collect::<BTreeMap<_, _>>(),
        BTreeMap::from([
            ("live", LoopRunResult::SignalSkipped),
            ("absent", LoopRunResult::SignalSkipped),
        ]),
        "{records:?}"
    );
    instances.0.remove("ended");
    assert_eq!(read_loop_instances(&env), instances);
    assert!(!read_loop_arming(&env).contains_key(&key));
    assert!(!read_loop_strikes(&env).contains_key(&key));
}

#[test]
fn other_session_hook_retires_non_hook_ended_subscription() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-ended", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ended",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
        ],
    );
    register_running_agent(&env, "sess-live", "feature-loop");
    let mut instances = read_loop_instances(&env);
    let mut live = instances.0["ended"].clone();
    live.wait.as_mut().unwrap().session = AgentSessionId::from("sess-live");
    instances.0.insert("live".to_owned(), live.clone());
    write_loop_instances(&env, instances);
    stamp_session_ended(&env, "sess-ended");
    run_hook(
        &env,
        json!({"hook_event_name": "SessionStart", "session_id": "sess-live"}),
        &env.project_root,
    );
    assert_eq!(
        read_loop_instances(&env).0,
        BTreeMap::from([("live".to_owned(), live)])
    );
}

#[test]
fn side_conversation_registration_hook_retires_ended_subscription() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-ended", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ended",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
        ],
    );
    assert!(read_loop_instances(&env).0.contains_key("ended"));
    stamp_session_ended(&env, "sess-ended");
    run_agent_hook(
        &env,
        "codex",
        json!({
            "hook_event_name": "SessionStart",
            "session_id": "codex-side",
            "source": "fork",
            "transcript_path": null,
        }),
        &env.project_root,
    );
    assert!(read_loop_instances(&env).0.is_empty());
}

/// The hook reconcile is a price every observation pays, so one whose commit
/// appends nothing must not pay it: nothing was published, the session reaper
/// never ran, and no end appeared that was not already there. A side
/// conversation's repeat hook is that case — the store returns before it stages
/// anything — and the next observation that does append still takes the row.
#[test]
fn repeat_side_conversation_hook_reconciles_nothing() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-ended", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ended",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
        ],
    );
    let armed = read_loop_instances(&env);
    // A side conversation's first hook registers it, so that one does append.
    run_agent_hook(
        &env,
        "codex",
        json!({
            "hook_event_name": "SessionStart",
            "session_id": "codex-side",
            "source": "fork",
            "transcript_path": null,
        }),
        &env.project_root,
    );
    stamp_session_ended(&env, "sess-ended");
    run_agent_hook(
        &env,
        "codex",
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "codex-side",
            "transcript_path": null,
            "prompt": "a side question",
        }),
        &env.project_root,
    );
    assert_eq!(read_loop_instances(&env), armed);
    register_running_agent(&env, "sess-live", "feature-loop");
    assert!(read_loop_instances(&env).0.is_empty());
}

#[test]
fn revived_session_subscription_survives_matching_signal() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-revived", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "revived",
            "--wait",
            "@claude",
            "--signal",
            "deploy.done",
        ],
    );
    let instances = read_loop_instances(&env);
    stamp_session_ended(&env, "sess-revived");
    run_hook(
        &env,
        json!({"hook_event_name": "SessionStart", "session_id": "sess-revived"}),
        &env.project_root,
    );
    loop_ok(&env, &["events", "emit", "deploy.done"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].task, "revived");
    assert_eq!(records[0].result, LoopRunResult::Delivered);
    assert_pending_message(&env, "sess-revived", "deploy.done");
    assert_eq!(read_loop_instances(&env), instances);
}

#[test]
fn retired_delivery_runner_preserves_replacement_session_subscription() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "retired-session", "feature-cas");
    let ready = env.project_root.join("check-ready");
    let release = env.project_root.join("check-release");
    let check = format!(
        "touch {}; while ! test -e {}; do sleep 0.01; done",
        shlex::try_quote(ready.to_str().unwrap()).unwrap(),
        shlex::try_quote(release.to_str().unwrap()).unwrap(),
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "reused",
            "--wait",
            "@retired-session",
            "--every",
            "15m",
            "--check",
            &check,
            "--on",
            "any",
            "--prompt",
            "old",
        ],
    );
    let runner = env
        .rimz()
        .args(["loop", "run", "reused"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_path(&ready);
    run_hook(
        &env,
        json!({"hook_event_name": "SessionEnd", "session_id": "retired-session"}),
        &env.project_root,
    );
    register_running_agent(&env, "replacement-session", "feature-cas");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "reused",
            "--wait",
            "@replacement-session",
            "--every",
            "15m",
            "--prompt",
            "replacement",
        ],
    );
    std::fs::write(release, "").unwrap();
    let output = runner.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read_loop_instances(&env).0["reused"]
            .wait
            .as_ref()
            .unwrap()
            .session
            .as_str(),
        "replacement-session"
    );
    loop_ok(&env, &["loop", "run", "reused"]);
    assert_pending_message(&env, "replacement-session", "replacement");
}

#[test]
fn same_task_name_in_two_rooms_does_not_collide() {
    let env = Env::new();
    let other = env.home_root.join("other-project");
    std::fs::create_dir(&other).expect("other project");
    for root in [&env.project_root, &other] {
        let output = env
            .rimz()
            .current_dir(root)
            .args(["loop", "add", "same", "--check", "true", "--at", "07:00"])
            .output()
            .expect("add instance");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let other_path = env
        .state_path_for(&other)
        .root
        .join("records/loop-instances.json");
    let other_bytes = std::fs::read(&other_path).expect("other instances");
    let other_tasks: Tasks = serde_json::from_slice(&other_bytes).expect("tasks");
    assert_eq!(
        read_loop_instances(&env).0["same"].resolved_root(),
        env.project_root
    );
    assert_eq!(other_tasks.0["same"].resolved_root(), other);
    loop_ok(&env, &["loop", "disable", "same"]);
    let output = env
        .rimz()
        .current_dir(&other)
        .args(["loop", "enable", "same"])
        .output()
        .expect("enable other");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let arming = read_loop_arming(&env);
    assert!(!arming[&project_task_key(&env.project_root, "same")].enabled);
    assert!(
        arming
            .get(&project_task_key(&other, "same"))
            .is_none_or(|state| state.enabled)
    );
    loop_ok(&env, &["loop", "remove", "same"]);
    assert!(!read_loop_instances(&env).0.contains_key("same"));
    assert_eq!(
        std::fs::read(&other_path).expect("other unchanged"),
        other_bytes
    );
}

#[test]
fn loop_launched_logs_show_checkout_and_leader() {
    let env = Env::new();
    let mut record =
        LoopRunRecord::new("resident", LoopRunResult::Launched, LoopRunMode::Manual, 0);
    record.checkout = Some(env.project_root.join("lane"));
    record.target = Some("@spry-cargo".into());
    write_loop_run_records(&env, &[record]);
    let output = loop_ok(&env, &["loop", "logs", "resident"]);
    assert!(
        output.contains("checkout:") && output.contains("/lane"),
        "{output}"
    );
    assert!(output.contains("leader: @spry-cargo"), "{output}");
}

#[test]
fn loop_history_filters_workspace_and_keeps_legacy_records() {
    let env = Env::new();
    let records = [
        (None, "legacy-room"),
        (Some(env.project_root.clone()), "this-room"),
        (Some(env.home_root.join("other")), "foreign-room"),
    ]
    .map(|(root, message)| {
        let mut record =
            LoopRunRecord::new("history", LoopRunResult::Errored, LoopRunMode::Manual, 0);
        record.root = root;
        record.error = Some(message.to_owned());
        record
    });
    write_loop_run_records(&env, &records);
    for command in ["logs", "show"] {
        let output = loop_ok(&env, &["loop", command, "history"]);
        assert!(
            output.contains("legacy-room") && output.contains("this-room"),
            "{output}"
        );
        assert!(!output.contains("foreign-room"), "{output}");
    }
    let output = env
        .rimz()
        .args(["loop", "show", "history", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "missing task must fail JSON show");
    assert!(
        output.stdout.is_empty(),
        "JSON show must not print human history"
    );
}

#[test]
fn external_tick_fires_a_machine_task_without_a_workspace_record() {
    if which::which("tmux").is_err() && which::which("zellij").is_err() {
        crate::common::skip("neither tmux nor zellij on PATH");
        return;
    }
    let env = Env::new();
    let marker = env.project_root.join("machine-tick-ran");
    write_loop_config(
        &env,
        &format!(
            "[tasks.machine-tick]\ncheck = \"touch {}\"\nroot = \"{}\"\nevery = \"1m\"\n",
            marker.display(),
            env.project_root.display(),
        ),
    );
    let prior = Timestamp::now() - SignedDuration::from_mins(2);
    write_loop_fire_state(&env, BTreeMap::from([("machine-tick".to_owned(), prior)]));
    assert!(
        !env.state_path_for(&env.project_root)
            .workspace_record
            .exists()
    );
    assert!(!env.runtime_paths().shared_root.exists());

    loop_ok(&env, &["loop", "tick"]);

    wait_for_path(&marker);
    assert!(env.runtime_paths().shared_root.is_dir());
    assert!(
        env.state_path_for(&env.project_root)
            .workspace_record
            .exists()
    );
}

#[test]
fn external_tick_discovers_a_trusted_project_without_a_workspace_record() {
    if which::which("tmux").is_err() && which::which("zellij").is_err() {
        crate::common::skip("neither tmux nor zellij on PATH");
        return;
    }
    let env = Env::new();
    let project = env.home_root.join("never-roomed-project");
    std::fs::create_dir(&project).expect("project root");
    let marker = project.join("project-tick-ran");
    let output = env
        .rimz()
        .current_dir(&project)
        .args([
            "loop",
            "add",
            "project-tick",
            "--project",
            "--check",
            &format!("touch {}", marker.display()),
            "--every",
            "1m",
        ])
        .output()
        .expect("add project loop");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        rimz::trust::status_with_roots(&project, &env.rimz_home())
            .expect("project trust")
            .state,
        rimz::trust::TrustState::Trusted,
    );
    assert!(!env.state_path_for(&project).workspace_record.exists());
    let prior = Timestamp::now() - SignedDuration::from_mins(2);
    let mut arming = read_loop_arming(&env);
    arming
        .get_mut(&project_task_key(&project, "project-tick"))
        .expect("project arming")
        .at = Some(prior);
    write_loop_arming(&env, &arming);
    write_loop_fire_state_for_root(
        &env,
        &project,
        BTreeMap::from([("project-tick".to_owned(), prior)]),
    );

    loop_ok(&env, &["loop", "tick"]);

    wait_for_path(&marker);
    assert!(env.state_path_for(&project).workspace_record.exists());
}

#[test]
fn malformed_instances_fail_reads_and_survive_adds() {
    let env = Env::new();
    let path = loop_instances_path(&env);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("workspace dir");
    std::fs::write(&path, b"not json").expect("instances");
    let (_, error) = loop_fail(&env, &["loop", "list"]);
    assert!(error.contains("loop-instances.json"), "{error}");
    loop_fail(
        &env,
        &["loop", "add", "new", "--check", "true", "--at", "07:00"],
    );
    assert_eq!(std::fs::read(&path).expect("unchanged"), b"not json");
}

#[test]
fn instance_task_in_a_project_without_a_room_reaches_machine_wide_readers() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "nightly", "--at", "07:00", "--check", "true"],
    );
    assert!(
        !env.state_path_for(&env.project_root)
            .workspace_record
            .exists()
    );

    // Doctor reads instance tasks through the same enumeration the external
    // loop timer fires from.
    let output = env
        .rimz()
        .current_dir("/")
        .args(["doctor", "--json"])
        .output()
        .expect("rimz doctor");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"nightly\""), "{stdout}");
}

#[test]
fn external_tick_yields_a_root_with_a_fresh_sidebar() {
    let env = Env::new();
    let marker = env.project_root.join("open-root-tick-ran");
    write_loop_config(
        &env,
        &format!(
            "[tasks.open-root]\ncheck = \"touch {}\"\nroot = \"{}\"\nevery = \"1m\"\n",
            marker.display(),
            env.project_root.display(),
        ),
    );
    let prior = Timestamp::now() - SignedDuration::from_mins(2);
    write_loop_fire_state(&env, BTreeMap::from([("open-root".to_owned(), prior)]));
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    let instance_id = SidebarInstanceId::new();
    let heartbeat = SidebarHeartbeat::new(
        env.workspace_id.clone(),
        instance_id.clone(),
        MuxName::Tmux,
        "rimz-test",
        runtime.sock_dir.join("sidebar.sock"),
        None,
    );
    std::fs::write(
        runtime.sidebar_heartbeat_path(&instance_id),
        serde_json::to_vec(&heartbeat).expect("heartbeat json"),
    )
    .expect("heartbeat");

    loop_ok(&env, &["loop", "tick"]);

    let stamps: BTreeMap<String, Timestamp> = serde_json::from_slice(
        &std::fs::read(runtime.lane_path("loop-fire.json")).expect("fire state"),
    )
    .expect("fire state json");
    assert_eq!(stamps.get("open-root"), Some(&prior));
    assert!(!marker.exists());

    loop_ok(&env, &["loop", "run", "open-root"]);
    assert!(marker.exists());
    assert!(
        !env.state_path_for(&env.project_root)
            .workspace_record
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn external_tick_refuses_when_the_systemd_user_manager_is_unreachable() {
    let env = Env::new();
    let shims = env.home_root.join("tick-shims");
    write_path_shim(&shims, "systemd-run", "exit 0");
    write_path_shim(
        &shims,
        "systemctl",
        "echo 'Failed to connect to bus: No medium found' >&2\nexit 1",
    );
    let marker = env.project_root.join("unreachable-bus-tick-ran");
    write_loop_config(
        &env,
        &format!(
            "[tasks.unreachable-bus]\ncheck = \"touch {}\"\nroot = \"{}\"\nevery = \"1m\"\n",
            marker.display(),
            env.project_root.display(),
        ),
    );
    let prior = Timestamp::now() - SignedDuration::from_mins(2);
    write_loop_fire_state(
        &env,
        BTreeMap::from([("unreachable-bus".to_owned(), prior)]),
    );

    let output = env
        .rimz()
        .args(["loop", "tick"])
        .env("INVOCATION_ID", "fixture-timer-unit")
        .env("PATH", path_with_front(&shims))
        .output()
        .expect("rimz loop tick");

    assert!(
        !output.status.success(),
        "an unreachable user manager must refuse the tick"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Failed to connect to bus"), "{stderr}");
    assert!(stderr.contains("rimz loop timer install"), "{stderr}");
    assert!(!marker.exists());
    let stamps: BTreeMap<String, Timestamp> = serde_json::from_slice(
        &std::fs::read(env.runtime_paths().lane_path("loop-fire.json")).expect("fire state"),
    )
    .expect("fire state json");
    assert_eq!(stamps.get("unreachable-bus"), Some(&prior));
    assert!(read_loop_run_records(&env).is_empty());
}

#[test]
fn external_tick_records_a_spawn_failure_without_retrying_or_striking() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.missing-runner]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"1h\"\n",
            env.project_root.display(),
        ),
    );
    let prior = Timestamp::now() - SignedDuration::from_hours(2);
    write_loop_fire_state(&env, BTreeMap::from([("missing-runner".to_owned(), prior)]));
    let mut tick = env.rimz();
    tick.args(["loop", "tick"])
        .env("RIMZ_BIN", env.home_root.join("missing-rimz"));
    let output = tick.output().expect("rimz loop tick");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record.task, "missing-runner");
    let root = canonical(&env.project_root);
    assert_eq!(record.root.as_deref(), Some(root.as_path()));
    assert_eq!(record.result, LoopRunResult::StartFailed);
    assert_eq!(record.mode, Some(LoopRunMode::Scheduled));
    assert_eq!(record.duration_ms, None);
    assert!(record.error.as_ref().is_some_and(|error| !error.is_empty()));
    let logs = loop_ok(&env, &["loop", "logs", "missing-runner"]);
    assert!(logs.contains("start failed"), "{logs}");
    let show = loop_ok(&env, &["loop", "show", "missing-runner"]);
    assert!(show.contains("start failed"), "{show}");
    let fire_path = env.runtime_paths().lane_path("loop-fire.json");
    let stamps: BTreeMap<String, Timestamp> =
        serde_json::from_slice(&std::fs::read(&fire_path).expect("fire state"))
            .expect("fire state json");
    assert!(stamps["missing-runner"] > prior);
    assert!(!loop_strikes_path(&env).exists());
    assert!(!loop_arming_path(&env).exists());

    let output = tick.output().expect("second rimz loop tick");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(read_loop_run_records(&env), records);
    let after: BTreeMap<String, Timestamp> =
        serde_json::from_slice(&std::fs::read(&fire_path).expect("fire state"))
            .expect("fire state json");
    assert_eq!(after, stamps);
    assert!(!loop_strikes_path(&env).exists());
    assert!(!loop_arming_path(&env).exists());
}

#[test]
fn signal_emit_records_a_spawn_failure_without_reporting_the_task_as_fired() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.missing-runner]\ncheck = \"true\"\nroot = \"{}\"\nsignal = \"deploy.finished\"\n",
            env.project_root.display(),
        ),
    );
    let payload = json!({"outcome": "failure", "attempt": 2});
    let output = env
        .rimz()
        .args([
            "events",
            "emit",
            "deploy.finished",
            "--json",
            &payload.to_string(),
        ])
        .env("RIMZ_BIN", env.home_root.join("missing-rimz"))
        .output()
        .expect("rimz events emit");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record.task, "missing-runner");
    assert_eq!(record.result, LoopRunResult::StartFailed);
    assert_eq!(record.duration_ms, None);
    let signal = record.signal.as_ref().expect("signal forensics");
    assert_eq!(signal.name.as_str(), "deploy.finished");
    assert_eq!(&signal.payload, payload.as_object().unwrap());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("fired 0 tasks"), "{stdout}");
    assert!(!stdout.contains("missing-runner"), "{stdout}");
}

#[cfg(unix)]
#[test]
fn external_tick_records_a_failed_scope_handoff() {
    let env = Env::new();
    let shims = env.home_root.join("tick-shims");
    write_path_shim(&shims, "systemctl", "exit 0");
    write_path_shim(&shims, "systemd-run", "exit 1");
    write_loop_config(
        &env,
        &format!(
            "[tasks.failed-handoff]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"1h\"\n",
            env.project_root.display(),
        ),
    );
    let prior = Timestamp::now() - SignedDuration::from_hours(2);
    write_loop_fire_state(&env, BTreeMap::from([("failed-handoff".to_owned(), prior)]));

    let output = env
        .rimz()
        .args(["loop", "tick"])
        .env("INVOCATION_ID", "fixture-timer-unit")
        .env("PATH", path_with_front(&shims))
        .output()
        .expect("rimz loop tick");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert_eq!(record.task, "failed-handoff");
    assert_eq!(record.result, LoopRunResult::StartFailed);
    assert_eq!(record.mode, Some(LoopRunMode::Scheduled));
    let error = record.error.as_deref().expect("handoff failure reason");
    assert!(error.contains("scope hand-off"), "{error}");
    assert!(error.contains("no history row"), "{error}");
    let logs = loop_ok(&env, &["loop", "logs", "failed-handoff"]);
    assert!(logs.contains("start failed"), "{logs}");
    let stamps: BTreeMap<String, Timestamp> = serde_json::from_slice(
        &std::fs::read(env.runtime_paths().lane_path("loop-fire.json")).expect("fire state"),
    )
    .expect("fire state json");
    assert!(stamps["failed-handoff"] > prior);
}

#[cfg(unix)]
#[test]
fn external_tick_does_not_record_a_fast_run_as_a_failed_start() {
    let env = Env::new();
    let shims = env.home_root.join("tick-shims");
    write_path_shim(&shims, "systemctl", "exit 0");
    write_path_shim(
        &shims,
        "systemd-run",
        "while [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = -- ]; then\n    shift\n    exec \"$@\"\n  fi\n  shift\ndone\nexit 1",
    );
    write_loop_config(
        &env,
        &format!(
            "[tasks.fast-run]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"1h\"\ndeadline = \"1970-01-01T00:00:01Z\"\n",
            env.project_root.display(),
        ),
    );
    let prior = Timestamp::now() - SignedDuration::from_hours(2);
    write_loop_fire_state(&env, BTreeMap::from([("fast-run".to_owned(), prior)]));

    let output = env
        .rimz()
        .args(["loop", "tick"])
        .env("INVOCATION_ID", "fixture-timer-unit")
        .env("PATH", path_with_front(&shims))
        .output()
        .expect("rimz loop tick");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    wait_for_path(&loop_runs_path(&env));
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].task, "fast-run");
    let root = canonical(&env.project_root);
    assert_eq!(records[0].root.as_deref(), Some(root.as_path()));
    assert_eq!(records[0].result, LoopRunResult::Expired);
    assert_eq!(records[0].mode, Some(LoopRunMode::Scheduled));
    let logs = loop_ok(&env, &["loop", "logs", "fast-run"]);
    assert!(logs.contains("expired"), "{logs}");
    assert!(!logs.contains("start failed"), "{logs}");
}

#[test]
fn loop_watch_reloads_tasks_without_reprobing_workspace() {
    let env = Env::new();
    let Some(real_git) = find_real_git() else {
        crate::common::skip("git not on PATH");
        return;
    };
    if !Command::new(&real_git)
        .args(["-C", env.project_root.to_str().expect("utf-8 project root")])
        .args(["init", "-q"])
        .status()
        .is_ok_and(|status| status.success())
    {
        crate::common::skip("git unavailable");
        return;
    }

    let bin_dir = env.home_root.join("git-trace-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir git trace bin");
    std::os::unix::fs::symlink(
        crate::common::cargo_bin("git-trace", env!("CARGO_BIN_EXE_git-trace")),
        bin_dir.join("git"),
    )
    .expect("symlink git trace shim");
    let git_log = env.home_root.join("loop-watch-git.log");

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open loop watch pty");
    let mut cmd = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut cmd);
    cmd.args(["loop", "watch", "--hold"]);
    cmd.cwd(env.project_root.as_os_str());
    cmd.env("TERM", "xterm-256color");
    cmd.env("RIMZ_TEST_GIT_LOG", &git_log);
    cmd.env("RIMZ_TEST_REAL_GIT", &real_git);
    cmd.env("PATH", crate::common::path_with_front(&bin_dir));

    let mut child = pair.slave.spawn_command(cmd).expect("spawn loop watch");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
        output
    });

    std::thread::sleep(Duration::from_millis(1_200));
    let exited_after_startup = child.try_wait().expect("poll loop watch");
    write_loop_config(
        &env,
        &format!(
            "[tasks.watch-reloaded]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );
    std::thread::sleep(Duration::from_millis(1_300));
    let exited_after_reload = child.try_wait().expect("poll loop watch");
    let exited_early = exited_after_startup.or(exited_after_reload);
    if exited_early.is_none() {
        child.kill().expect("terminate loop watch");
        let _ = child.wait().expect("reap loop watch");
    }
    drop(pair.master);
    let output =
        String::from_utf8_lossy(&reader_thread.join().expect("join pty reader")).into_owned();
    assert!(
        exited_early.is_none() && output.contains("watch-reloaded"),
        "loop watch exited or missed config reload: {exited_early:?}\n{output}"
    );

    let git_trace = std::fs::read_to_string(&git_log).expect("read loop watch git trace");
    for probe in [
        "git\trev-parse\t--show-toplevel",
        "git\trev-parse\t--git-common-dir",
        "git\trev-parse\t--abbrev-ref\tHEAD",
    ] {
        assert_eq!(
            git_trace.lines().filter(|line| *line == probe).count(),
            1,
            "workspace probe should run once: {probe}\n{git_trace}"
        );
    }
}

#[test]
fn loop_wait_workflow_pins_and_delivers_to_live_session() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-live", "feature-loop");
    loop_ok(&env, &["config", "set", "harness.smart_compact", "70%"]);

    let added = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "wait",
            "--wait",
            "@claude",
            "--every",
            "15m",
            "--prompt",
            "next step",
        ],
    );
    assert!(added.contains("pinned to claude session `sess-loop-live`"));
    let instances = read_loop_instances(&env);
    assert_eq!(
        instances.0["wait"]
            .wait
            .as_ref()
            .map(|wait| wait.session.as_str()),
        Some("sess-loop-live")
    );

    loop_ok(&env, &["loop", "run", "wait"]);
    assert_pending_message(&env, "sess-loop-live", "next step");
    assert_eq!(
        env.store().list_pending_messages().unwrap()[0].sender,
        rimz::store::message::MessageSender::Harness {
            notice: rimz::store::message::HarnessNotice::Wait,
        }
    );
    assert_eq!(
        env.store().list_pending_messages().unwrap()[0].auto_compact,
        Some(AutoCompact::Percent(70))
    );
    assert_eq!(last_loop_record(&env).result, LoopRunResult::Delivered);
    assert_eq!(
        last_loop_record(&env).root.as_deref(),
        Some(env.project_root.as_path())
    );
    let list = loop_ok(&env, &["loop", "list"]);
    let show = loop_ok(&env, &["loop", "show", "wait"]);
    assert!(
        list.lines()
            .any(|line| line.contains("wait") && line.contains('✓'))
            && show.contains("source:")
            && show.contains("state"),
        "list/show smoke failed:\n{list}\n{show}"
    );
}

#[test]
fn emitted_signal_reaches_the_matching_wait_consumer() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_event(&rimz::store::event::EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            rimz::store::event::AgentLaunchPayload {
                agent_id: AgentSessionId::from("sess-signal-live"),
                launch_id: None,
                agent_name: "claude".to_owned(),
                agent_name_explicit: true,
                launch: rimz::agents::LaunchParams::default(),
                state: rimz::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: Some("feature-signal".to_owned()),
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    for signal in [
        rimz::agents::LifecycleSignal::Registered,
        rimz::agents::LifecycleSignal::TurnStarted { turn_id: None },
    ] {
        let mut observation = rimz::agents::AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-signal-live")),
            signal,
        );
        observation.agent_name = Some("claude".to_owned());
        env.store()
            .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                session_name: &workspace.session_name,
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    }
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "ci-wait",
            "--wait",
            "@claude",
            "--signal",
            "deploy.finished",
            "--match",
            "outcome=failure",
            "--prompt",
            "Inspect deployment",
        ],
    );

    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.finished",
            "--json",
            r#"{"outcome":"success"}"#,
        ],
    );
    assert!(read_loop_run_records(&env).is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());

    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.finished",
            "--json",
            r#"{"outcome":"failure"}"#,
        ],
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while read_loop_run_records(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].result, LoopRunResult::Delivered);
    let signal = records[0].signal.as_ref().expect("signal forensics");
    assert_eq!(signal.name.as_str(), "deploy.finished");
    assert_eq!(signal.payload["outcome"], "failure");
    let message = &env.store().list_pending_messages().unwrap()[0];
    assert_eq!(records[0].message_id.as_ref(), Some(&message.message_id));
    assert_eq!(message.agent_id.as_str(), "sess-signal-live");
    assert_eq!(
        message.sender,
        rimz::store::message::MessageSender::Harness {
            notice: rimz::store::message::HarnessNotice::Signal,
        }
    );
    assert!(
        message.text.contains("Inspect deployment"),
        "{}",
        message.text
    );
    assert!(
        message
            .text
            .starts_with("waited on deploy.finished\nfired [ci-wait]\n"),
        "{}",
        message.text
    );
    assert_eq!(message.text.lines().nth(2), Some("outcome: failure"));
    assert!(message.text.ends_with("\n\nInspect deployment"));

    let show = loop_ok(&env, &["loop", "show", "ci-wait"]);
    assert!(show.contains(" · signal deploy.finished\n"), "{show}");
    assert!(
        show.contains(&format!("message: {}", message.message_id)),
        "{show}"
    );

    loop_ok(&env, &["loop", "disable", "ci-wait"]);
    loop_ok(
        &env,
        &[
            "events",
            "emit",
            "deploy.finished",
            "--json",
            r#"{"outcome":"failure"}"#,
        ],
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(read_loop_run_records(&env).len(), 1);
    assert_eq!(env.store().list_pending_messages().unwrap().len(), 1);

    // A merge-queue rejection reaches its waiter with the reason and the
    // queue's own checks link; the sibling `pr.queued` is recorded skipped.
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "queue-wait",
            "--wait",
            "@claude",
            "--signal",
            "pr.dequeued",
            "--match",
            "branch=feature-signal",
            "--prompt",
            "Fix the queue failure",
        ],
    );
    let queue_checks_url = "https://github.com/org/repo/commit/queue-a/checks";
    for (name, payload) in [
        (
            "pr.queued",
            json!({"branch": "feature-signal", "queued_at": "2026-10-03T12:28:46Z"}),
        ),
        (
            "pr.dequeued",
            json!({
                "branch": "feature-signal", "dequeued_at": "2026-10-03T12:46:03Z",
                "reason": "failed_checks", "queue_checks_url": queue_checks_url,
            }),
        ),
    ] {
        loop_ok(
            &env,
            &[
                "events",
                "emit",
                name,
                "--source",
                "forge",
                "--json",
                &payload.to_string(),
            ],
        );
    }
    wait_for_loop_run_records(&env, "queue-wait SignalSkipped then Delivered", |records| {
        records.len() == 3
    });
    let records = read_loop_run_records(&env);
    let queue_results: Vec<_> = records
        .iter()
        .filter(|record| record.task == "queue-wait")
        .map(|record| {
            let signal = record.signal.as_ref().expect("signal forensics");
            (signal.name.as_str(), record.result)
        })
        .collect();
    assert_eq!(
        queue_results,
        vec![
            ("pr.queued", LoopRunResult::SignalSkipped),
            ("pr.dequeued", LoopRunResult::Delivered),
        ],
        "{records:?}"
    );
    let messages = env.store().list_pending_messages().unwrap();
    assert_eq!(messages.len(), 2, "{messages:?}");
    // Substrings only: the body's layout around these two values is not this test's contract.
    let queue_message = messages
        .iter()
        .find(|message| message.text.contains("Fix the queue failure"))
        .unwrap_or_else(|| panic!("{messages:?}"));
    assert_eq!(queue_message.agent_id.as_str(), "sess-signal-live");
    for value in ["failed_checks", queue_checks_url] {
        assert!(queue_message.text.contains(value), "{}", queue_message.text);
    }
}

#[test]
fn lifecycle_signal_wakes_only_for_the_matching_agent_session() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "planner-session", "feature-planner");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "review-finished",
            "--wait",
            "@claude",
            "--signal",
            "agent.idle",
            "--match",
            "session=reviewer-session",
            "--prompt",
            "review finished",
        ],
    );

    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "planner-session",
            "last_assistant_message": "planning done",
            "worktree_branch": "feature-planner",
        }),
        &env.project_root,
    );
    assert!(read_loop_run_records(&env).is_empty());
    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "planner-session",
            "prompt": "wait for review",
            "worktree_branch": "feature-planner",
        }),
        &env.project_root,
    );

    register_running_agent(&env, "reviewer-session", "feature-reviewer");
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "reviewer-session",
            "last_assistant_message": "review done",
            "worktree_branch": "feature-reviewer",
        }),
        &env.project_root,
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while !read_loop_run_records(&env)
        .iter()
        .any(|record| record.result == LoopRunResult::Delivered)
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(25));
    }
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 2, "{records:?}");
    assert_eq!(records[0].result, LoopRunResult::SignalSkipped);
    assert_eq!(
        records[1]
            .signal
            .as_ref()
            .map(|signal| signal.name.as_str()),
        Some("agent.idle")
    );
    assert_pending_message(&env, "planner-session", "review finished");
}

#[test]
fn agent_budget_edits_and_views_use_local_day() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-budget", "feature-budget");
    let agent_id = AgentSessionId::from("sess-budget");
    let kind = AgentKind::new_unchecked("claude");

    for (value, expected_cap, disabled) in [
        ("10", Some(10.0), false),
        ("+5", Some(15.0), false),
        ("clear", None, true),
    ] {
        loop_ok(
            &env,
            &["agents", "budget", "@claude", value, "--no-continue"],
        );
        let ledger = rimz::harness::budget::read_ledger(&env.runtime_paths(), &kind, &agent_id)
            .expect("budget ledger");
        assert_eq!(
            (ledger.effective_cap_usd(), ledger.disabled),
            (expected_cap, disabled)
        );
    }

    let mut context = rimz::agents::AgentContext::new("claude", Timestamp::now());
    context.cost = Some(rimz::agents::AgentCost {
        total_cost_usd: Some(50.0),
        ..rimz::agents::AgentCost::default()
    });
    let record = rimz::agents::context::record::AgentContextRecord::new(
        "claude",
        agent_id.as_str(),
        context,
    );
    rimz::store::agent_context::write_record(&env.runtime_paths(), &record)
        .expect("write cost sidecar");
    let mut ledger = BudgetLedger::new("20/day".parse().expect("budget"));
    ledger.day_baseline = Some(DayBaseline {
        date: jiff::civil::date(2026, 6, 1),
        cost_usd: 40.0,
    });
    write_ledger(&env.runtime_paths(), &kind, &agent_id, &ledger).expect("write budget ledger");

    for args in [
        &["agents", "budget", "@claude"][..],
        &["agents", "show", "@claude"][..],
    ] {
        let output = loop_ok(&env, args);
        assert!(output.contains("$10.00"), "rimz {args:?}: {output}");
    }

    // The budget view names its agent the way every other surface does: by the
    // handle the reader can type back, never by the session id behind it.
    let view = loop_ok(&env, &["agents", "budget", "@claude"]);
    assert!(
        view.contains("agent:  @claude#main"),
        "budget view must print the handle: {view}"
    );
    assert!(
        !view.contains("sess-budget"),
        "budget view must not print the session id: {view}"
    );

    let show: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["agents", "show", "@claude", "--json"]))
            .expect("show JSON");
    let budget = &show["agent"]["budget"];
    assert_eq!(budget["cap"], "$20.00/day");
    assert_eq!(budget["spent_usd"], 10.0);
    assert_eq!(budget["parked"], false);
    assert_eq!(budget["park"], serde_json::Value::Null);
}

#[test]
fn loop_spawn_controls_persist_render_and_gate_daily_budget() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    loop_ok(&env, &["config", "init"]);
    loop_ok(&env, &["config", "set", "harness.budget", "0/day"]);
    write_loop_config(&env, "default-timeout = \"3h\"\n");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "bounded",
            "--agent",
            "claude",
            "--prompt",
            "bounded work",
            "--every",
            "15m",
            "--budget",
            "$5",
            "--budget-per-day",
            "20",
            "--verify",
            "cargo xtask gate",
            "--max-attempts",
            "4",
            "--max-strikes",
            "5",
        ],
    );

    let text = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(text.contains("budget = \"$5.00\"") && text.contains("budget-per-day = \"$20.00\""));
    let config: LoopConfig = toml::from_str(&text).expect("parse loop config");
    let task = &config.tasks.0["bounded"];
    assert_eq!(task.budget.as_deref(), Some("$5.00"));
    assert_eq!(task.budget_per_day.as_deref(), Some("$20.00"));
    assert_eq!(task.verify.as_deref(), Some("cargo xtask gate"));
    assert_eq!((task.max_attempts, task.max_strikes), (Some(4), Some(5)));
    let show = loop_ok(&env, &["loop", "show", "bounded"]);
    assert!(
        show.contains("\n  verify:  cargo xtask gate (up to 4 attempts)\n")
            && show.contains("\n  timeout: 3h (default)\n"),
        "{show}"
    );

    let spent = (0..4)
        .map(|_| {
            let mut record =
                LoopRunRecord::new("bounded", LoopRunResult::Completed, LoopRunMode::Manual, 1);
            record.cost_usd = Some(5.0);
            record
        })
        .collect::<Vec<_>>();
    write_loop_run_records(&env, &spent);
    loop_ok(&env, &["loop", "run", "bounded"]);
    let skipped = last_loop_record(&env);
    assert_eq!(skipped.result, LoopRunResult::BudgetSkipped);
    assert!(
        skipped
            .error
            .as_deref()
            .is_some_and(|error| error.contains("daily budget"))
    );
    assert!(
        !skipped
            .error
            .as_deref()
            .is_some_and(|error| error.contains("fleet budget")),
        "task daily budget must gate before the simultaneously closed fleet scope"
    );
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .expect("read loop config")
            .contains("[tasks.bounded]")
    );

    let marker = env.project_root.join("scope-check-ran");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "scope-bounded",
            "--agent",
            "claude",
            "--prompt",
            "bounded work",
            "--check",
            &format!("touch {}", marker.display()),
            "--on",
            "success",
            "--every",
            "15m",
        ],
    );
    loop_ok(&env, &["loop", "run", "scope-bounded"]);
    let skipped = last_loop_record(&env);
    assert_eq!(skipped.result, LoopRunResult::BudgetSkipped);
    assert!(
        skipped
            .error
            .as_deref()
            .is_some_and(|error| error.contains("fleet budget exhausted"))
    );
    assert!(!marker.exists(), "scope caps must gate before the check");
}

#[test]
fn loop_project_trust_controls_visibility_execution_and_precedence() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.shared]\ncheck = \"printf machine\"\nroot = \"{}\"\nevery = \"15m\"\n\
             [tasks.profile-ref]\nagent = \"repo-agent\"\nprompt = \"work\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display(),
            env.project_root.display()
        ),
    );
    write_project_config(
        &env,
        "[profiles.repo-agent]\nagent = \"claude\"\n\
         [tasks.repo-check]\ncheck = \"true\"\nevery = \"15m\"\n\
         [tasks.shared]\ncheck = \"printf project\"\nevery = \"15m\"\n",
    );

    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.contains("repo-check")
            && list.contains("blocked · project untrusted")
            && list.contains("NEEDS YOU")
            && list.contains("rimz trust grant"),
        "{list}"
    );
    let (_stdout, error) = loop_fail(&env, &["loop", "run", "repo-check"]);
    assert!(
        error.contains("loop task `repo-check` is blocked — project trust is untrusted")
            && error.contains("rimz trust grant"),
        "{error}"
    );
    let (_stdout, error) = loop_fail(&env, &["loop", "run", "profile-ref"]);
    assert!(
        error.contains("profiles are configured")
            && error.contains("untrusted")
            && error.contains("rimz trust grant"),
        "the loop launch resolver must refuse an untrusted project profile: {error}"
    );

    loop_ok(&env, &["loop", "run", "shared"]);
    assert!(
        last_loop_record(&env)
            .check
            .is_some_and(|check| check.output.contains("machine"))
    );
    grant_project_trust(&env);
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.contains("repo-check") && list.contains("off · repo task, enable here to run"),
        "{list}"
    );
    let runs_before = read_loop_run_records(&env).len();
    loop_ok(&env, &["loop", "run", "shared"]);
    assert_eq!(read_loop_run_records(&env).len(), runs_before);
    loop_ok(&env, &["loop", "enable", "shared"]);
    loop_ok(&env, &["loop", "run", "shared"]);
    assert!(
        last_loop_record(&env)
            .check
            .is_some_and(|check| check.output.contains("project"))
    );
}

#[test]
fn project_task_enablement_is_scoped_by_project_root() {
    let env = Env::new();
    let other = env.home_root.join("other-project");
    std::fs::create_dir_all(&other).expect("other project");
    write_project_config_at(
        &env.project_root,
        "[tasks.nightly]\ncheck = \"true\"\nevery = \"15m\"\n",
    );
    write_project_config_at(
        &other,
        "[tasks.nightly]\ncheck = \"true\"\nevery = \"15m\"\n",
    );
    loop_ok_root(&env, &env.project_root, &["trust", "grant"]);
    loop_ok_root(&env, &other, &["trust", "grant"]);

    assert!(
        loop_ok_root(&env, &env.project_root, &["loop", "list"])
            .contains("off · repo task, enable here to run")
    );
    assert!(
        loop_ok_root(&env, &other, &["loop", "list"])
            .contains("off · repo task, enable here to run")
    );

    loop_ok_root(&env, &env.project_root, &["loop", "enable", "nightly"]);
    assert!(
        !loop_ok_root(&env, &env.project_root, &["loop", "list"])
            .contains("off · repo task, enable here to run")
    );
    assert!(
        loop_ok_root(&env, &other, &["loop", "list"])
            .contains("off · repo task, enable here to run")
    );

    let arming = read_loop_arming(&env);
    assert!(arming[&project_task_key(&env.project_root, "nightly")].enabled);
    assert!(!arming.contains_key(&project_task_key(&other, "nightly")));
}

#[test]
fn trusted_project_task_edits_repin_trust() {
    let env = Env::new();
    write_project_config(&env, "[tasks.first]\ncheck = \"true\"\nevery = \"15m\"\n");
    grant_project_trust(&env);
    let dismissal = env
        .rimz_home()
        .join("trust")
        .join(rimz::WorkspaceId::from_project_root(&env.project_root).as_str())
        .join("folder-trust-prompt.toml");
    let declined = "dismissed_kinds = ['codex']\ndismissed_at = '2026-01-01T00:00:00Z'\n";
    std::fs::write(&dismissal, declined).unwrap();

    let added = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "second",
            "--project",
            "--check",
            "true",
            "--every",
            "15m",
        ],
    );
    assert!(
        added.contains("trust: granted — task enabled and ready to fire")
            && !added.contains("stay inert")
            && !added.contains("grant trust now"),
        "{added}"
    );
    assert!(loop_ok(&env, &["trust"]).contains("trust: trusted"));
    assert!(read_loop_arming(&env)[&project_task_key(&env.project_root, "second")].enabled);
    loop_ok(&env, &["loop", "run", "second"]);

    let renamed = loop_ok(&env, &["loop", "rename", "second", "renamed"]);
    assert!(renamed.contains("trust: kept"), "{renamed}");
    assert!(loop_ok(&env, &["trust"]).contains("trust: trusted"));
    assert!(read_loop_arming(&env)[&project_task_key(&env.project_root, "renamed")].enabled);
    loop_ok(&env, &["loop", "run", "renamed"]);

    let removed = loop_ok(&env, &["loop", "remove", "renamed"]);
    assert!(removed.contains("trust: kept"), "{removed}");
    assert!(loop_ok(&env, &["trust"]).contains("trust: trusted"));
    loop_ok(&env, &["loop", "run", "first"]);
    assert_eq!(
        std::fs::read_to_string(dismissal).ok().as_deref(),
        Some(declined)
    );
}

#[test]
fn first_project_task_grants_fresh_config() {
    let env = Env::new();
    assert!(!env.project_root.join(".rimz/config.toml").exists());

    let added = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "fresh",
            "--project",
            "--check",
            "true",
            "--every",
            "15m",
        ],
    );
    assert!(
        added.contains("trust: granted — task enabled and ready to fire"),
        "{added}"
    );
    assert!(loop_ok(&env, &["trust"]).contains("trust: trusted"));
    let arming = read_loop_arming(&env);
    assert!(
        arming
            .get(&project_task_key(&env.project_root, "fresh"))
            .is_some_and(|entry| entry.enabled),
        "{arming:?}"
    );

    // A root whose `.rimz` is the RimZ home holds the machine config, never
    // a project layer, so a project task edit there refuses untouched.
    let config = env.project_root.join(".rimz/config.toml");
    let before = std::fs::read_to_string(&config).unwrap();
    let output = env
        .rimz()
        .env("RIMZ_HOME", env.project_root.join(".rimz"))
        .args([
            "loop",
            "add",
            "home",
            "--project",
            "--check",
            "true",
            "--every",
            "1h",
        ])
        .output()
        .expect("rimz loop add");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("holds the RimZ home"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
}

#[test]
fn project_task_edit_does_not_grant_foreign_change() {
    let env = Env::new();
    write_project_config(
        &env,
        "[tasks.remove-me]\ncheck = \"true\"\nevery = \"15m\"\n",
    );
    grant_project_trust(&env);
    write_project_config(
        &env,
        "[tasks.remove-me]\ncheck = \"true\"\nevery = \"15m\"\n\n\
         [[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks codex\"\n",
    );

    let removed = loop_ok(&env, &["loop", "remove", "remove-me"]);
    assert!(
        removed
            .contains("trust: stale — project tasks stay inert until you run `rimz trust grant`")
            && !removed.contains("trust: kept"),
        "{removed}"
    );
    assert!(loop_ok(&env, &["trust"]).contains("trust: stale"));
}

#[test]
fn loop_task_storage_policy_and_manual_fire_preserve_one_shots() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "# keep unrelated task comment\n[tasks.keep]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"1h\"\n",
            env.project_root.display()
        ),
    );

    loop_ok(
        &env,
        &["loop", "add", "probe", "--check", "printf ok", "--in", "5m"],
    );
    assert!(read_loop_instances(&env).0.contains_key("probe"));
    assert!(
        !std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("[tasks.probe]")
    );
    loop_ok(&env, &["loop", "fire", "probe"]);
    assert!(read_loop_instances(&env).0.contains_key("probe"));
    loop_ok(&env, &["loop", "run", "probe"]);
    assert!(!read_loop_instances(&env).0.contains_key("probe"));

    loop_ok(
        &env,
        &[
            "loop", "add", "morning", "--check", "true", "--every", "weekday", "--at", "07:00",
        ],
    );
    assert!(!read_loop_instances(&env).0.contains_key("morning"));
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("[tasks.morning]")
    );

    loop_ok(
        &env,
        &["loop", "add", "swap", "--check", "true", "--every", "15m"],
    );
    loop_ok(
        &env,
        &["loop", "add", "swap", "--check", "true", "--in", "5m"],
    );
    assert!(read_loop_instances(&env).0.contains_key("swap"));
    assert!(
        !std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("[tasks.swap]")
    );
    loop_ok(
        &env,
        &[
            "loop", "add", "swap", "--check", "true", "--every", "weekday", "--at", "07:00",
        ],
    );
    let text = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        !read_loop_instances(&env).0.contains_key("swap")
            && text.contains("[tasks.swap]")
            && text.contains("# keep unrelated task comment"),
        "{text}"
    );

    let (_stdout, error) = loop_fail(
        &env,
        &[
            "loop",
            "add",
            "project-once",
            "--project",
            "--check",
            "true",
            "--in",
            "5m",
        ],
    );
    assert!(
        error.contains("need a trigger") && error.contains("--every, --cron, or --signal"),
        "{error}"
    );
}

#[test]
fn loop_multi_name_enable_disable_resolves_before_writing() {
    let env = Env::new();
    for name in ["first", "second"] {
        loop_ok(
            &env,
            &["loop", "add", name, "--check", "true", "--every", "15m"],
        );
    }
    for verb in ["disable", "enable"] {
        let before = read_loop_arming(&env);
        let (out, error) = loop_fail(&env, &["loop", verb, "first", "missing"]);
        assert!(
            error.contains("no loop task named `missing`; see `rimz loop list`"),
            "{error}"
        );
        assert!(out.is_empty(), "{out}");
        assert_eq!(read_loop_arming(&env), before);
        let output = loop_ok(&env, &["loop", verb, "first", "second"]);
        for name in ["first", "second"] {
            assert!(
                output.contains(&format!("loop `{name}`: {verb}d")),
                "{output}"
            );
            assert_eq!(
                read_loop_arming(&env)[&machine_task_key(name)].enabled,
                verb == "enable"
            );
        }
        loop_fail(&env, &["loop", verb, "first", "--all"]);
        loop_fail(&env, &["loop", verb]);
    }
}

#[test]
fn loop_multi_name_remove_reports_each_outcome() {
    let env = Env::new();
    for name in ["first", "second"] {
        loop_ok(
            &env,
            &["loop", "add", name, "--check", "true", "--every", "15m"],
        );
    }
    let output = env
        .rimz()
        .args(["loop", "remove", "first", "missing", "second", "first"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        text,
        "removed loop task `first`\nno loop task named `missing`\nremoved loop task `second`\nno loop task named `first`\n"
    );
    assert!(output.status.success());
    assert!(loop_ok(&env, &["loop", "list"]).contains("no loop tasks"));
    loop_fail(&env, &["loop", "remove"]);
}

#[test]
fn loop_enable_disable_pause_workflow() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "probe", "--check", "true", "--every", "15m"],
    );
    let (_stdout, error) = loop_fail(&env, &["loop", "pause", "probe"]);
    assert!(
        error.contains("--for") && error.contains("required"),
        "{error}"
    );
    assert!(loop_ok(&env, &["loop", "disable", "probe"]).contains("disabled"));
    assert!(loop_ok(&env, &["loop", "list"]).contains("off"));
    let fired = loop_ok(&env, &["loop", "fire", "probe"]);
    assert!(fired.contains("task is disabled; firing anyway") && fired.contains("check passed"));
    assert!(loop_ok(&env, &["loop", "enable", "probe"]).contains("enabled"));

    let timed = loop_ok(&env, &["loop", "pause", "probe", "--for", "2h"]);
    assert!(
        timed.contains("resumes in 2h")
            && read_loop_arming(&env)
                .get(&machine_task_key("probe"))
                .is_some_and(|entry| entry.pause_until.is_some()),
        "{timed}"
    );
    assert!(loop_ok(&env, &["loop", "show", "probe"]).contains("· paused"));
    let fired = loop_ok(&env, &["loop", "fire", "probe"]);
    assert!(fired.contains("task is paused; firing anyway") && fired.contains("check passed"));
    assert!(loop_ok(&env, &["loop", "enable", "probe"]).contains("enabled"));
    assert!(!loop_ok(&env, &["loop", "list"]).contains("paused, resumes"));
    assert!(loop_ok(&env, &["loop", "enable", "probe"]).contains("already enabled"));

    let key = machine_task_key("probe");
    let prior_enable = Timestamp::now() - SignedDuration::from_hours(3);
    let expired_pause_end = Timestamp::now() - SignedDuration::from_mins(1);
    let expired = BTreeMap::from([(
        key.clone(),
        Arming {
            enabled: true,
            at: Some(prior_enable),
            pause_until: Some(expired_pause_end),
            strikes: None,
        },
    )]);
    std::fs::write(
        loop_arming_path(&env),
        serde_json::to_vec(&expired).expect("serialize expired pause"),
    )
    .expect("write expired pause");
    std::fs::write(
        loop_strikes_path(&env),
        serde_json::to_vec(&BTreeMap::from([(key.clone(), 2_u32)]))
            .expect("serialize loop strikes"),
    )
    .expect("write loop strikes");
    assert!(loop_ok(&env, &["loop", "enable", "probe"]).contains("already enabled"));
    let enabled = read_loop_arming(&env)[&key];
    assert_eq!(enabled.at, Some(prior_enable));
    assert_eq!(enabled.pause_until, Some(expired_pause_end));
    assert!(!read_loop_strikes(&env).contains_key(&key));

    loop_ok(
        &env,
        &["loop", "add", "second", "--check", "true", "--every", "15m"],
    );
    let disabled = loop_ok(&env, &["loop", "disable", "--all"]);
    assert!(
        disabled.contains("loop `probe`: disabled") && disabled.contains("loop `second`: disabled"),
        "{disabled}"
    );
    let enabled = loop_ok(&env, &["loop", "enable", "--all"]);
    assert!(
        enabled.contains("loop `probe`: enabled") && enabled.contains("loop `second`: enabled"),
        "{enabled}"
    );
}

#[test]
fn loop_repeated_failures_auto_disable_notify_once_and_enable() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-strikes", "feature-loop");
    let notify_log = env.project_root.join("loop-disabled-notify.log");
    let config_path = env.rimz_home().join("config.toml");
    std::fs::create_dir_all(config_path.parent().expect("config parent")).expect("mkdir config");
    std::fs::write(
        config_path,
        format!(
            "[notifications]\ncommand = '''printf '%s|%s\\n' \"$RIMZ_NOTIFY_KIND\" \"$RIMZ_NOTIFY_TITLE\" >> '{}' '''\n",
            notify_log.display()
        ),
    )
    .expect("write notification config");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "watchdog",
            "--wait",
            "@claude",
            "--every",
            "15m",
            "--check",
            "printf broken; exit 1",
            "--prompt",
            "fix it",
        ],
    );

    for _ in 0..2 {
        loop_ok(&env, &["loop", "run", "watchdog"]);
    }
    let third = loop_ok(&env, &["loop", "run", "watchdog"]);
    assert!(
        third.contains("disabled after 3 consecutive failed fires"),
        "{third}"
    );
    assert_eq!(
        read_loop_arming(&env)
            .get(&project_task_key(&env.project_root, "watchdog"))
            .and_then(|arming| arming.strikes),
        Some(3)
    );

    let fire = loop_ok(&env, &["loop", "fire", "watchdog"]);
    assert!(fire.contains("task is disabled; firing anyway") && fire.contains("delivered"));
    assert_eq!(
        read_loop_strikes(&env).get(&project_task_key(&env.project_root, "watchdog")),
        Some(&4)
    );
    assert_eq!(
        read_loop_arming(&env)
            .get(&project_task_key(&env.project_root, "watchdog"))
            .and_then(|arming| arming.strikes),
        Some(3),
        "manual fire must not replace an existing disable"
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    let notification = loop {
        let text = std::fs::read_to_string(&notify_log).unwrap_or_default();
        if text.contains("loop_disabled|RimZ: loop watchdog disabled") {
            break text;
        }
        assert!(Instant::now() < deadline, "notification missing: {text}");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(notification.lines().count(), 1, "{notification}");

    loop_ok(&env, &["loop", "enable", "watchdog"]);
    assert!(
        !read_loop_strikes(&env).contains_key(&project_task_key(&env.project_root, "watchdog"))
    );
    assert!(
        read_loop_arming(&env)
            .get(&project_task_key(&env.project_root, "watchdog"))
            .is_some_and(|arming| arming.enabled
                && arming.pause_until.is_none()
                && arming.strikes.is_none())
    );
}

#[test]
fn loop_task_mutations_move_and_clear_overlays() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "# keep unrelated task comment\n[tasks.keep]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"1h\"\n\
             [tasks.old]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display(),
            env.project_root.display()
        ),
    );
    loop_ok(&env, &["loop", "pause", "old", "--for", "2h"]);
    std::fs::write(loop_strikes_path(&env), r#"{"machine::old":2}"#).expect("write loop strikes");
    loop_ok(&env, &["loop", "rename", "old", "new"]);
    let text = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(
        text.contains("# keep unrelated task comment")
            && text.contains("[tasks.new]")
            && !text.contains("[tasks.old]")
    );
    assert!(read_loop_arming(&env).contains_key(&machine_task_key("new")));
    assert_eq!(
        read_loop_strikes(&env).get(&machine_task_key("new")),
        Some(&2)
    );

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "old-state",
            "--check",
            "true",
            "--at",
            "07:00",
        ],
    );
    loop_ok(&env, &["loop", "pause", "old-state", "--for", "2h"]);
    loop_ok(&env, &["loop", "rename", "old-state", "new-state"]);
    let instances = read_loop_instances(&env);
    assert!(
        !instances.0.contains_key("old-state")
            && instances.0.contains_key("new-state")
            && read_loop_arming(&env)
                .contains_key(&project_task_key(&env.project_root, "new-state"))
    );

    loop_ok(&env, &["loop", "remove", "new"]);
    assert!(!read_loop_arming(&env).contains_key(&machine_task_key("new")));
    assert!(!read_loop_strikes(&env).contains_key(&machine_task_key("new")));
    loop_ok(
        &env,
        &["loop", "add", "swap", "--check", "true", "--every", "15m"],
    );
    loop_ok(&env, &["loop", "pause", "swap", "--for", "2h"]);
    let replaced = loop_ok(
        &env,
        &["loop", "add", "swap", "--check", "true", "--every", "30m"],
    );
    assert!(
        replaced.contains("arming: reset")
            && !read_loop_arming(&env).contains_key(&machine_task_key("swap"))
    );
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("# keep unrelated task comment")
    );
}

#[test]
fn loop_rename_rejects_collisions_and_reports_missing() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.old]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n\
             [tasks.existing]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display(),
            env.project_root.display()
        ),
    );
    loop_ok(
        &env,
        &["loop", "add", "state", "--check", "true", "--at", "07:00"],
    );

    for (old, new, expected) in [
        ("old", "old", "must differ"),
        ("old", "existing", "already exists"),
        ("old", "state", "already exists"),
        ("state", "existing", "already exists"),
    ] {
        let (_stdout, error) = loop_fail(&env, &["loop", "rename", old, new]);
        assert!(error.contains(expected), "rename {old} -> {new}: {error}");
    }
    let output = loop_ok(&env, &["loop", "rename", "missing", "free"]);
    assert!(output.contains("no loop task named `missing`"), "{output}");
}

#[test]
fn loop_qwen_exact_quota_skip_precedes_check_command() {
    let env = Env::new();
    loop_ok(&env, &["config", "init"]);
    loop_ok(&env, &["config", "set", "harness.budget", "0/day"]);
    let settings = env.agent_config_path("qwen");
    std::fs::create_dir_all(settings.parent().expect("Qwen config parent"))
        .expect("create Qwen config");
    std::fs::write(
        &settings,
        r#"{
            "security":{"auth":{"selectedType":"openai"}},
            "model":{"name":"qwen3-coder-plus"},
            "modelProviders":{"openai":[{
                "id":"qwen3-coder-plus",
                "baseUrl":"https://coding-intl.dashscope.aliyuncs.com/v1",
                "envKey":"BAILIAN_CODING_PLAN_API_KEY"
            }]},
            "env":{"BAILIAN_CODING_PLAN_API_KEY":"sentinel-loop-secret"}
        }"#,
    )
    .expect("write Qwen settings");
    env.install_agent_hooks("qwen");

    let launch_env = BTreeMap::from([(
        "RIMZ_QWEN_SETTINGS".to_owned(),
        settings.display().to_string(),
    )]);
    let adapter = rimz::agents::find_definition("qwen").expect("Qwen definition");
    let argv = adapter
        .launch_command(&[], Some("work"))
        .expect("Qwen launch argv");
    let binding = adapter
        .resolve_managed_launch(&env.project_root, &launch_env, None, &argv)
        .binding()
        .cloned()
        .expect("exact Qwen binding");
    let encoded = serde_json::to_value(&binding).expect("serialize binding");
    let account_key = encoded["account_key"]
        .as_str()
        .expect("binding account key")
        .to_owned();
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    let rate_cache = |used_7d, used_30d| RateLimitsCache {
        entries: [(
            rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("qwen")),
            RateLimitCacheEntry {
                scope: binding.scope().clone(),
                account_key: Some(account_key.clone()),
                limits: AgentRateLimits {
                    windows: vec![
                        RateLimitWindow {
                            used_percentage: Some(used_7d),
                            resets_at: Some(
                                Timestamp::now() + jiff::SignedDuration::from_hours(84),
                            ),
                            duration_mins: Some(7 * 24 * 60),
                            ..RateLimitWindow::default()
                        },
                        RateLimitWindow {
                            used_percentage: Some(used_30d),
                            resets_at: Some(
                                Timestamp::now() + jiff::SignedDuration::from_hours(360),
                            ),
                            duration_mins: Some(30 * 24 * 60),
                            ..RateLimitWindow::default()
                        },
                    ],
                },
                bound_limits: None,
                pending: Vec::new(),
                unknown_since_ms: None,
            },
        )]
        .into_iter()
        .collect(),
        ..RateLimitsCache::default()
    };
    rimz::disk::atomic::write_temp_then_rename_cache(
        &runtime.shared_rate_limits_path(),
        &rate_cache(20, 100),
    )
    .expect("write exact quota cache");
    assert!(
        rimz::agents::provider_budget_gate(
            &runtime,
            &rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("qwen")),
            &binding,
            Timestamp::now()
        )
        .is_some(),
        "the exact cache must close the provider gate before the loop runner starts"
    );

    let marker = env.project_root.join("check-ran");
    qwen_loop_ok(
        &env,
        &settings,
        &[
            "loop",
            "add",
            "qwen-bounded",
            "--check",
            &format!("touch {}", marker.display()),
            "--on",
            "success",
            "--agent",
            "qwen",
            "--prompt",
            "work",
            "--surplus",
            "2x",
            "--every",
            "15m",
        ],
    );
    let mut config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).expect("read loop config"))
            .expect("parse loop config");
    config
        .tasks
        .0
        .get_mut("qwen-bounded")
        .expect("Qwen task")
        .deadline = Some(Timestamp::from_second(1).expect("deadline"));
    std::fs::write(
        loop_config_path(&env),
        toml::to_string_pretty(&config).expect("serialize loop config"),
    )
    .expect("write loop config");
    let lock_path = loop_run_lock_path(&env, "qwen-bounded");
    std::fs::create_dir_all(lock_path.parent().expect("lock parent")).expect("mkdir runtime");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open run lock");
    lock_file.try_lock().expect("hold run lock");

    qwen_loop_ok(&env, &settings, &["loop", "run", "qwen-bounded"]);
    let scope_skip = last_loop_record(&env);
    assert_eq!(scope_skip.result, LoopRunResult::BudgetSkipped);
    assert!(
        scope_skip
            .error
            .as_deref()
            .is_some_and(|reason| reason.contains("fleet budget exhausted")),
        "scope cap must gate before provider quota"
    );

    loop_ok(&env, &["budget", "off", "--no-continue"]);
    qwen_loop_ok(&env, &settings, &["loop", "run", "qwen-bounded"]);

    assert!(
        !marker.exists(),
        "provider quota must gate before the check"
    );
    let records = read_loop_run_records(&env);
    let skip = records.last().expect("Qwen budget skip record");
    assert_eq!(skip.result, LoopRunResult::BudgetSkipped);
    let reason = skip.error.as_deref().expect("skip reason");
    assert!(
        reason.contains("Qwen Alibaba International 30d window exhausted"),
        "{reason}"
    );
    assert!(!reason.contains("sentinel-loop-secret"), "{reason}");
    assert!(!reason.contains(&account_key), "{reason}");

    rimz::disk::atomic::write_temp_then_rename_cache(
        &runtime.shared_rate_limits_path(),
        &rate_cache(80, 80),
    )
    .expect("write surplus cache");
    qwen_loop_ok(&env, &settings, &["loop", "run", "qwen-bounded"]);
    let surplus = last_loop_record(&env);
    assert_eq!(surplus.result, LoopRunResult::SurplusSkipped);
    assert!(
        surplus
            .error
            .as_deref()
            .is_some_and(|reason| reason.contains("surplus")),
        "surplus must gate before run lock, deadline, and check"
    );

    config
        .tasks
        .0
        .get_mut("qwen-bounded")
        .expect("Qwen task")
        .surplus = None;
    std::fs::write(
        loop_config_path(&env),
        toml::to_string_pretty(&config).expect("serialize loop config"),
    )
    .expect("write loop config");
    qwen_loop_ok(&env, &settings, &["loop", "run", "qwen-bounded"]);
    assert_eq!(last_loop_record(&env).result, LoopRunResult::Overlapped);

    lock_file.unlock().expect("unlock run lock");
    qwen_loop_ok(&env, &settings, &["loop", "run", "qwen-bounded"]);
    assert_eq!(last_loop_record(&env).result, LoopRunResult::Expired);
    assert!(
        !marker.exists(),
        "ordered gates must stop before the check command"
    );
}

#[test]
fn loop_check_runs_in_the_arming_worktree() {
    let env = Env::new();
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return;
    }
    let linked = env.home_root.join("linked");
    let add = Command::new("git")
        .args(["worktree", "add", "-q", "-b", "linked"])
        .arg(&linked)
        .current_dir(&env.project_root)
        .status()
        .expect("run git worktree add");
    assert!(add.success(), "git worktree add failed");
    let linked = canonical(&linked);
    let output = env
        .rimz()
        .current_dir(&linked)
        .args([
            "loop",
            "add",
            "where",
            "--check",
            "pwd -P; printf '%s\\n' \"$RIMZ_WORKTREE_PATH\" \"$RIMZ_PROJECT_ROOT\" \"$RIMZ_WORKSPACE_ID\"",
            "--every",
            "15m",
        ])
        .output()
        .expect("add check from linked worktree");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(&format!("action: runs check in {}", linked.display()))
    );
    let stored: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    let entry = &stored.tasks.0["where"];
    let root = canonical(&env.project_root);
    assert_eq!(entry.root, root);
    assert_eq!(entry.dir.as_deref(), Some(linked.as_path()));

    loop_ok(&env, &["loop", "fire", "where"]);
    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].result, LoopRunResult::Completed);
    assert_eq!(records[0].root.as_deref(), Some(root.as_path()));
    let check = records[0].check.as_ref().unwrap();
    assert_eq!(check.code, Some(0));
    assert_eq!(
        check.output,
        format!(
            "{0}\n{0}\n{1}\n{2}\n",
            linked.display(),
            root.display(),
            WorkspaceId::from_project_root(&root)
        )
    );
    let shown = loop_ok(&env, &["loop", "show", "where"]);
    assert!(
        shown
            .lines()
            .any(|line| line.contains("dir") && line.contains("~/linked")),
        "{shown}"
    );

    let output = env
        .rimz()
        .current_dir(&linked)
        .args([
            "loop",
            "add",
            "project-where",
            "--project",
            "--check",
            "pwd -P; printf '%s\\n' \"$RIMZ_WORKTREE_PATH\"",
            "--every",
            "15m",
        ])
        .output()
        .expect("add project check from linked worktree");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains(&format!("action: runs check in {}", root.display()))
    );
    let project: toml::Value = toml::from_str(
        &std::fs::read_to_string(env.project_root.join(".rimz/config.toml")).unwrap(),
    )
    .unwrap();
    let entry = project["tasks"]["project-where"].as_table().unwrap();
    assert!(!entry.contains_key("root"));
    assert!(!entry.contains_key("dir"));
    let output = env
        .rimz()
        .current_dir(&linked)
        .env(rimz::workspace::ENV_WORKTREE_PATH, &linked)
        .args(["loop", "fire", "project-where"])
        .output()
        .expect("fire project check with inherited worktree context");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = read_loop_run_records(&env);
    let record = records.last().unwrap();
    assert_eq!(record.task, "project-where");
    assert_eq!(record.root.as_deref(), Some(root.as_path()));
    assert_eq!(
        record.check.as_ref().unwrap().output,
        format!("{0}\n{0}\n", root.display())
    );
}

#[test]
fn loop_check_failure_records_and_renders_history() {
    let env = Env::new();
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "history",
            "--check",
            "printf healthy",
            "--every",
            "15m",
        ],
    );
    let output = loop_ok(&env, &["loop", "fire", "history"]);
    assert!(
        output.find("  check: printf healthy").unwrap() < output.find("  │ healthy").unwrap(),
        "{output}"
    );
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "history",
            "--check",
            "printf broken; definitely-missing-rimz-loop-command",
            "--every",
            "15m",
        ],
    );
    loop_ok(&env, &["loop", "fire", "history"]);

    let records = read_loop_run_records(&env);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].result, LoopRunResult::Completed);
    assert_eq!(
        records[0].check.as_ref().and_then(|check| check.code),
        Some(0)
    );
    assert!(
        records[0]
            .check
            .as_ref()
            .unwrap()
            .output
            .contains("healthy")
    );
    assert_eq!(records[1].result, LoopRunResult::Failed);
    assert_eq!(
        records[1].check.as_ref().and_then(|check| check.code),
        Some(127)
    );
    assert!(records[1].check.as_ref().unwrap().output.contains("broken"));

    let show = loop_ok(&env, &["loop", "show", "history"]);
    assert!(
        show.contains("LAST RUN — ✗ failed (exit 127)") && show.contains("broken"),
        "{show}"
    );
    let logs = loop_ok(&env, &["loop", "logs", "history"]);
    assert!(
        logs.find("healthy").unwrap() < logs.find("broken").unwrap(),
        "{logs}"
    );
    let failed = loop_ok(&env, &["loop", "logs", "history", "--failed"]);
    assert!(
        failed.contains("broken") && !failed.contains("healthy"),
        "{failed}"
    );
}

#[test]
fn loop_guard_skips_or_delivers_with_evidence() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-check", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "healthy",
            "--wait",
            "@claude",
            "--every",
            "15m",
            "--check",
            "printf probe-line",
            "--on",
            "fail",
            "--prompt",
            "fix it",
        ],
    );
    loop_ok(&env, &["loop", "run", "healthy"]);
    assert!(
        env.store()
            .list_pending_messages()
            .expect("messages")
            .is_empty()
    );
    let skipped = last_loop_record(&env);
    assert_eq!(skipped.result, LoopRunResult::CheckSkipped);
    assert_eq!(skipped.check.and_then(|check| check.code), Some(0));

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "broken",
            "--wait",
            "@claude",
            "--every",
            "15m",
            "--check",
            "printf boom; exit 1",
            "--on",
            "fail",
            "--prompt",
            "fix it",
        ],
    );
    loop_ok(&env, &["loop", "run", "broken"]);
    assert_pending_message(
        &env,
        "sess-loop-check",
        "check `printf boom; exit 1` exited 1",
    );
    let delivered = last_loop_record(&env);
    assert_eq!(delivered.result, LoopRunResult::Delivered);
    let check = delivered.check.expect("guard check detail");
    assert_eq!(check.code, Some(1));
    assert!(check.output.contains("boom"));
}

#[test]
fn loop_trip_then_preparation_error_records_and_renders() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-trip-error", "feature-loop");
    write_loop_config(
        &env,
        &format!(
            "[tasks.trip_error]\n\
             wait = {{ kind = \"claude\", session = \"sess-loop-trip-error\", handle = \"@claude\" }}\n\
             prompt-file = \"missing-prompt.txt\"\ncheck = \"false\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );

    let (stdout, error) = loop_fail(&env, &["loop", "fire", "trip_error"]);
    assert!(stdout.contains("✗ check failed (exit 1)") && stdout.contains("→ waking @claude"));
    assert!(
        error.contains("reading prompt-file") && error.contains("missing-prompt.txt"),
        "{error}"
    );
    let records = read_loop_run_records(&env);
    assert_eq!(
        records.len(),
        1,
        "a preparation error must append exactly one terminal row"
    );
    let record = &records[0];
    assert_eq!(record.result, LoopRunResult::Errored);
    assert!(
        record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("reading prompt-file"))
    );
    let show = loop_ok(&env, &["loop", "show", "trip_error"]);
    assert!(
        show.contains("error") && show.contains("reading prompt-file"),
        "{show}"
    );
    assert!(
        std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("[tasks.trip_error]")
    );
}

#[test]
fn loop_missing_spawn_prompt_names_task() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    write_loop_config(
        &env,
        &format!(
            "[tasks.named_spawn]\nagent = \"claude\"\nroot = \"{}\"\nat = \"07:00\"\n",
            env.project_root.display()
        ),
    );
    let (_stdout, error) = loop_fail(&env, &["loop", "run", "named_spawn"]);
    assert!(
        error.contains("loop task `named_spawn` has no prompt")
            && !error.contains("loop task `claude` has no prompt"),
        "{error}"
    );
}

#[cfg(unix)]
#[test]
fn loop_scheduled_one_shot_consumption_follows_preflight_boundary() {
    let dispatch = Env::new();
    dispatch.install_agent_hooks("claude");
    loop_ok(
        &dispatch,
        &[
            "loop",
            "add",
            "dispatch-fails",
            "--agent",
            "claude",
            "--prompt",
            "ship it",
            "--at",
            "07:00",
        ],
    );
    let empty_path = dispatch.home_root.join("empty-path");
    std::fs::create_dir_all(&empty_path).expect("empty PATH");
    let shell = write_fake_login_shell(&dispatch, "loop-preflight-test-sh", &[]);
    let output = dispatch
        .rimz()
        .env("SHELL", shell)
        .env("PATH", &empty_path)
        .args(["loop", "run", "dispatch-fails"])
        .output()
        .expect("run scheduled spawn");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && error.contains("finding `claude` on PATH"),
        "{error}"
    );
    assert!(
        !read_loop_instances(&dispatch)
            .0
            .contains_key("dispatch-fails")
    );
    assert_eq!(last_loop_record(&dispatch).task, "dispatch-fails");

    let preflight = Env::new();
    write_loop_instances(
        &preflight,
        Tasks(BTreeMap::from([(
            "preflight-fails".to_owned(),
            TaskEntry {
                agent: Some("claude".to_owned()),
                prompt: Some("ship it".to_owned()),
                root: preflight.project_root.clone(),
                at: Some("07:00".to_owned()),
                ..TaskEntry::default()
            },
        )])),
    );
    let (_stdout, error) = loop_fail(&preflight, &["loop", "run", "preflight-fails"]);
    assert!(error.contains("hooks are not installed"), "{error}");
    assert!(
        read_loop_instances(&preflight)
            .0
            .contains_key("preflight-fails")
    );
    assert_eq!(last_loop_record(&preflight).task, "preflight-fails");
}

#[test]
fn loop_show_surfaces_spawn_failure_tail_and_prior_error() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.forensics]\nagent = \"codex\"\nprompt = \"go\"\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );
    let paths = env.state_path_for(&env.project_root);
    paths.ensure_dirs().unwrap();
    let mut run = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        env.project_root.clone(),
    );
    run.status = RunStatus::Failed;
    run.failure_tail = Some("agent startup failed\nmissing binary".to_owned());
    run.transcript_path = Some("/tmp/rimz-transcript.jsonl".to_owned());
    rimz::harness::run::create(&paths, &run).unwrap();

    let mut prior =
        LoopRunRecord::new("forensics", LoopRunResult::Errored, LoopRunMode::Manual, 42);
    prior.at = Timestamp::from_second(10).unwrap();
    prior.error = Some("reading system-prompt-file `/missing.md`\ncaused by: not found".to_owned());
    let mut failed =
        LoopRunRecord::new("forensics", LoopRunResult::Failed, LoopRunMode::Manual, 50);
    failed.at = Timestamp::from_second(20).unwrap();
    failed.run_id = Some(run.run_id.to_string());
    failed.transcript_path = Some("/tmp/rimz-transcript.jsonl".to_owned());
    write_loop_run_records(&env, &[prior, failed]);

    let show = loop_ok(&env, &["loop", "show", "forensics"]);
    assert!(
        show.contains("LAST RUN — ✗ failed (exit 1)")
            && show.contains("agent startup failed\n  │ missing binary")
            && show.contains("transcript: /tmp/rimz-transcript.jsonl")
            && !show.contains("last failure"),
        "{show}"
    );
    assert!(!show.contains("caused by: not found"), "{show}");
}

#[test]
fn loop_poll_until_delivers_once_or_expires() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-until", "feature-loop");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "green",
            "--wait",
            "@claude",
            "--every",
            "2m",
            "--check",
            "true",
            "--on",
            "success",
            "--until",
            "30m",
            "--prompt",
            "merge now",
        ],
    );
    assert!(read_loop_instances(&env).0.contains_key("green"));
    loop_ok(&env, &["loop", "run", "green"]);
    assert_pending_message(&env, "sess-loop-until", "merge now");
    assert!(!read_loop_instances(&env).0.contains_key("green"));

    let expired = Env::new();
    write_loop_instances(
        &expired,
        Tasks(BTreeMap::from([(
            "expired".to_owned(),
            TaskEntry {
                wait: Some(TaskTarget {
                    kind: AgentKind::new_unchecked("claude"),
                    session: AgentSessionId::from("sess-expired"),
                    handle: "@claude".to_owned(),
                }),
                prompt: Some("too late".to_owned()),
                check: Some("true".to_owned()),
                on: Some(CheckOn::Success),
                root: expired.project_root.clone(),
                every: Some("2m".to_owned()),
                deadline: Some(Timestamp::from_second(1).unwrap()),
                ..TaskEntry::default()
            },
        )])),
    );
    loop_ok(&expired, &["loop", "run", "expired"]);
    assert!(read_loop_instances(&expired).0.is_empty());
    let records = read_loop_run_records(&expired);
    assert_eq!(
        records.len(),
        1,
        "a prepare-time Done result must append exactly one terminal row"
    );
    assert_eq!(records[0].result, LoopRunResult::Expired);
    assert!(
        expired
            .store()
            .list_pending_messages()
            .expect("messages")
            .is_empty()
    );
}

#[test]
fn loop_worktree_target_delivery_preserves_session() {
    let env = Env::new();
    if !init_git_repo(&env.project_root) {
        crate::common::skip("git unavailable");
        return;
    }
    let worktree = env.home_root.join("project-worktrees/feature-loop");
    std::fs::create_dir_all(worktree.parent().expect("worktree parent")).expect("mkdir worktrees");
    assert!(git_ok(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature-loop",
            worktree.to_str().expect("utf8 worktree"),
        ]
    ));
    env.install_agent_hooks("claude");
    register_running_agent_at(&env, "sess-loop-worktree", "feature-loop", &worktree);
    let added = env
        .rimz()
        .current_dir(&worktree)
        .args([
            "loop",
            "add",
            "wait-worktree",
            "--wait",
            "@claude",
            "--every",
            "15m",
            "--prompt",
            "worktree next step",
        ])
        .output()
        .expect("loop add");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    loop_ok(&env, &["loop", "run", "wait-worktree"]);
    assert_pending_message(&env, "sess-loop-worktree", "worktree next step");
}

#[test]
fn loop_dead_target_run_removes_but_fire_keeps_task() {
    let env = Env::new();
    let config = format!(
        "[tasks.dead]\nwait = {{ kind = \"claude\", session = \"sess-dead\", handle = \"@claude\" }}\n\
         prompt = \"wake up\"\ncheck = \"false\"\nroot = \"{}\"\nat = \"07:00\"\n",
        env.project_root.display()
    );
    write_loop_config(&env, &config);
    let run = loop_ok(&env, &["loop", "run", "dead"]);
    assert!(run.contains("not alive; removing schedule"), "{run}");
    assert!(
        !std::fs::read_to_string(loop_config_path(&env))
            .unwrap()
            .contains("[tasks.dead]")
    );

    write_loop_config(&env, &config);
    let fire = loop_ok(&env, &["loop", "fire", "dead"]);
    assert!(
        fire.contains("@claude not alive — schedule left in place")
            && std::fs::read_to_string(loop_config_path(&env))
                .unwrap()
                .contains("[tasks.dead]"),
        "{fire}"
    );
    assert_eq!(last_loop_record(&env).result, LoopRunResult::TargetGone);
}

#[test]
fn loop_list_uses_room_arm_stamp_for_next_fire() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.next]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.lines()
            .any(|line| { line.trim_start().starts_with("next") && line.contains("never fired") })
    );
    write_loop_fire_state(
        &env,
        BTreeMap::from([(
            "next".to_owned(),
            Timestamp::now() - SignedDuration::from_secs(16 * 60),
        )]),
    );
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().unwrap();
    let instance_id = SidebarInstanceId::new();
    let heartbeat = SidebarHeartbeat::new(
        env.workspace_id.clone(),
        instance_id.clone(),
        MuxName::Tmux,
        "rimz-test",
        runtime.sock_dir.join("sidebar.sock"),
        None,
    );
    std::fs::write(
        runtime.sidebar_heartbeat_path(&instance_id),
        serde_json::to_vec(&heartbeat).unwrap(),
    )
    .unwrap();
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.lines()
            .any(|line| line.trim_start().starts_with("next") && line.contains("due"))
    );
}

#[test]
fn malformed_schedule_stays_visible_and_manual_action_remains_runnable() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.invalid]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\nat = \"07:00\"\n",
            env.project_root.display()
        ),
    );

    let list = loop_ok(&env, &["loop", "list"]);
    let show = loop_ok(&env, &["loop", "show", "invalid"]);
    let fire = loop_ok(&env, &["loop", "fire", "invalid"]);

    assert!(
        list.lines()
            .any(|line| { line.trim_start().starts_with("invalid") && line.contains("invalid:") })
            && show.contains("invalid — invalid:")
            && fire.contains("check passed"),
        "{list}\n{show}\n{fire}"
    );

    env.install_agent_hooks("claude");
    register_running_agent(&env, "manual-invalid-session", "feature-manual");
    let mut config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).unwrap()).unwrap();
    let entry = config.tasks.0.get_mut("invalid").unwrap();
    entry.check = None;
    entry.wait = Some(TaskTarget {
        kind: AgentKind::new_unchecked("claude"),
        session: AgentSessionId::from("manual-invalid-session"),
        handle: "@claude#feature-manual".to_owned(),
    });
    write_loop_config(&env, &toml::to_string(&config).unwrap());
    let fire = loop_ok(&env, &["loop", "fire", "invalid"]);
    assert!(fire.contains("delivered"), "{fire}");
}

#[test]
fn loop_legacy_run_record_renders_through_list_and_show() {
    let env = Env::new();
    write_loop_config(
        &env,
        &format!(
            "[tasks.legacy]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            env.project_root.display()
        ),
    );
    append_legacy_loop_record(&env, "legacy", LoopRunResult::Completed);
    let list = loop_ok(&env, &["loop", "list"]);
    let show = loop_ok(&env, &["loop", "show", "legacy"]);
    assert!(
        list.lines()
            .any(|line| { line.trim_start().starts_with("legacy") && line.contains('✓') })
            && show.contains("✓ completed")
            && show.contains("RECENT RUNS (newest first · 1 of 1)")
            && !show.contains("MODE")
            && !show.contains("last failure"),
        "{list}\n{show}"
    );
    assert_eq!(
        show.lines().rev().take(3).collect::<Vec<_>>()[2],
        "  task:   check · true",
        "{show}"
    );
    assert!(
        show.lines().last().unwrap().starts_with("  source: "),
        "{show}"
    );
}

#[test]
fn loop_overlap_records_holder_and_preserves_one_shot() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "busy", "--check", "true", "--at", "07:00"],
    );
    let holder = RunLockInfo {
        pid: 42_424,
        started_at: Timestamp::now() - SignedDuration::from_secs(25 * 60),
    };
    let lock_file = hold_loop_run_lock(&loop_run_lock_path(&env, "busy"), &holder);

    let run = loop_ok(&env, &["loop", "run", "busy"]);
    assert!(
        run.contains("previous run still active (pid 42424, started 25m ago) — skipped"),
        "{run}"
    );
    assert!(read_loop_instances(&env).0.contains_key("busy"));
    let record = last_loop_record(&env);
    assert_eq!(record.result, LoopRunResult::Overlapped);
    assert!(
        record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("pid 42424"))
    );
    let show = loop_ok(&env, &["loop", "show", "busy"]);
    assert!(
        show.starts_with("busy — once at 07:00 · ▸ running 25m · pid 42424\n")
            && show.contains("  1 fire skipped while the active run holds the lock\n")
            && show.contains("  stop with `rimz loop stop busy`\n")
            && !show
                .lines()
                .any(|line| line.trim_start().starts_with("active:"))
            && show.contains("○ overlapped")
            && !show.contains("LAST RUN"),
        "{show}"
    );
    let logs = loop_ok(&env, &["loop", "logs", "busy"]);
    assert!(
        logs.contains("○ overlapped")
            && logs.contains("previous run still active (pid 42424, started 25m ago) — skipped")
            && logs.ends_with("\n\n▸ running 25m · pid 42424\n"),
        "{logs}"
    );
    let failed = loop_ok(&env, &["loop", "logs", "busy", "--failed"]);
    assert!(!failed.contains("▸ running"), "{failed}");
    let json = loop_ok(&env, &["loop", "show", "busy", "--json"]);
    assert!(json.contains("\"result\": \"overlapped\""), "{json}");
    let json: serde_json::Value = serde_json::from_str(&json).expect("show json");
    assert_eq!(
        json["running"],
        json!({ "pid": 42_424, "started_at": holder.started_at, "run_id": null })
    );
    lock_file.unlock().expect("unlock loop run lock");
    let json = loop_ok(&env, &["loop", "show", "busy", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&json).expect("show json");
    assert!(json["running"].is_null(), "{json}");
    assert!(
        json.as_object()
            .is_some_and(|keys| keys.contains_key("running")),
        "{json}"
    );
}

#[test]
fn loop_list_shows_a_running_task_beside_its_last_result() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "busy", "--check", "true", "--every", "1h"],
    );
    loop_ok(&env, &["loop", "fire", "busy"]);
    let row = |list: &str, head: &str| -> Vec<String> {
        list.lines()
            .find(|line| line.trim_start().starts_with(head))
            .unwrap_or_else(|| panic!("no `{head}` line: {list}"))
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    };

    let list = loop_ok(&env, &["loop", "list"]);
    assert_eq!(row(&list, "NAME"), ["NAME", "TRIGGER", "ACTION", "LAST"]);
    let idle = row(&list, "busy");
    assert!(idle.contains(&"✓".to_owned()), "{list}");
    assert!(!list.contains("▸ running"), "{list}");

    let holder = RunLockInfo {
        pid: 42_424,
        started_at: Timestamp::now() - SignedDuration::from_secs(3 * 60),
    };
    let lock_file = hold_loop_run_lock(&loop_run_lock_path(&env, "busy"), &holder);
    let list = loop_ok(&env, &["loop", "list"]);
    let running = row(&list, "busy");
    assert!(
        running.ends_with(&["▸".into(), "running".into(), "3m".into()]),
        "{list}"
    );
    let value: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "list", "--json"])).unwrap();
    assert_eq!(value["rooms"][0]["tasks"][0]["last"]["result"], "completed");
    assert_eq!(
        value["rooms"][0]["tasks"][0]["running_since"],
        json!(holder.started_at)
    );
    lock_file.unlock().expect("unlock loop run lock");
}

#[cfg(unix)]
#[test]
fn loop_list_shows_a_consumed_one_shot_until_its_run_ends() {
    let env = Env::new();
    let (_fixture, mut runner, _run) = start_consumed_spawn(&env, "later");

    let list = loop_ok(&env, &["loop", "list"]);
    let lines = list.lines().collect::<Vec<_>>();
    assert!(
        lines.len() == 5
            && lines[0].contains("room")
            && lines[2] == "ROOM"
            && lines[3].trim_start().starts_with("NAME")
            && lines[4].trim_start().starts_with("later")
            && lines[4].contains("▸ running")
            && lines[4].contains("one-shot fired"),
        "{list}"
    );

    runner.kill().unwrap();
    runner.wait().unwrap();
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.contains("no loop tasks") && !list.contains("later"),
        "{list}"
    );
}

#[test]
fn loop_list_adds_one_row_per_held_lock_no_task_claims() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "fan", "--check", "true", "--every", "1h"],
    );
    let holder = RunLockInfo {
        pid: 42_424,
        started_at: Timestamp::now() - SignedDuration::from_secs(3 * 60),
    };
    let _locks = ["fan", "fan-ws_0123456789abcdef01234567", "later"]
        .map(|name| hold_loop_run_lock(&loop_run_lock_path(&env, name), &holder));

    let list = loop_ok(&env, &["loop", "list"]);
    let lines = list.lines().collect::<Vec<_>>();
    assert!(
        lines.len() == 6
            && lines[0].contains("room")
            && lines[4].trim_start().starts_with("fan ")
            && lines[4].contains("▸ running 3m")
            && lines[4].contains("every 1h")
            && lines[5].trim_start().starts_with("later ")
            && lines[5].contains("▸ running 3m")
            && lines[5].contains("one-shot fired"),
        "{list}"
    );
}

#[test]
fn loop_list_warns_once_when_the_run_locks_cannot_be_listed() {
    let env = Env::new();
    for name in ["first", "second"] {
        loop_ok(
            &env,
            &["loop", "add", name, "--check", "true", "--every", "1h"],
        );
    }
    // A file where the locks directory belongs makes listing it fail.
    let locks = loop_run_lock_path(&env, "first");
    let locks = locks.parent().expect("locks directory");
    let _ = std::fs::remove_dir_all(locks);
    std::fs::create_dir_all(locks.parent().expect("runtime root")).unwrap();
    std::fs::write(locks, "").unwrap();

    let list = env.rimz().args(["loop", "list"]).output().unwrap();
    let stderr = String::from_utf8(list.stderr).unwrap();
    let stdout = String::from_utf8(list.stdout).unwrap();
    assert!(list.status.success(), "loop list: {stderr}");
    assert!(
        stderr.matches("warning:").count() == 1
            && stderr.contains(locks.to_str().expect("utf-8 locks path"))
            && stderr.contains("no active run is shown"),
        "loop list: {stderr}"
    );
    assert!(
        stdout.contains("first") && stdout.contains("second") && !stdout.contains("▸ running"),
        "{stdout}"
    );
}

#[test]
fn loop_watch_keeps_acting_results_after_skipped_signals() {
    use rimz::harness::schedule::run_log::SignalRecord;

    let env = Env::new();
    write_loop_config(
        &env,
        &toml::to_string(&LoopConfig {
            tasks: Tasks(
                ["good", "bad", "heard"]
                    .map(|name| {
                        (
                            name.into(),
                            TaskEntry {
                                root: env.project_root.clone(),
                                check: Some("true".into()),
                                signal: Some("ci.failed".into()),
                                ..TaskEntry::default()
                            },
                        )
                    })
                    .into(),
            ),
            ..LoopConfig::default()
        })
        .unwrap(),
    );
    let mut records = Vec::new();
    for (name, result) in [
        ("good", LoopRunResult::Completed),
        ("bad", LoopRunResult::Failed),
    ] {
        let mut record = LoopRunRecord::new(name, result, LoopRunMode::Manual, 0);
        record.root = Some(env.project_root.clone());
        record.at = Timestamp::now() - SignedDuration::from_mins(10);
        records.push(record);
    }
    for minutes in [7, 6, 5] {
        for name in ["good", "bad", "heard"] {
            let mut record =
                LoopRunRecord::new(name, LoopRunResult::SignalSkipped, LoopRunMode::Manual, 0);
            record.root = Some(env.project_root.clone());
            record.at = Timestamp::now() - SignedDuration::from_mins(minutes);
            record.signal = Some(SignalRecord {
                name: "ci.passed".parse().unwrap(),
                payload: serde_json::Map::new(),
            });
            records.push(record);
        }
    }
    write_loop_run_records(&env, &records);

    let output = loop_watch_frames(&env);
    let good = output.lines().find(|line| line.contains("good")).unwrap();
    assert!(
        good.contains("10m ago") && good.contains("completed"),
        "{output}"
    );
    let bad = output.lines().find(|line| line.contains("bad")).unwrap();
    assert!(
        bad.contains("10m ago") && bad.contains("failed"),
        "{output}"
    );
    assert!(output.contains("✗ 1 failed"), "{output}");
    assert!(output.contains("heard ci.passed"), "{output}");
    assert!(!output.contains("skipped"), "{output}");
}

#[test]
fn loop_watch_scopes_to_the_whole_callers_room() {
    let env = Env::new();
    let checkout = env.home_root.join("checkout");
    let elsewhere = env.home_root.join("elsewhere");
    std::fs::create_dir(&checkout).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    // Opening another room's instances would block the first repaint.
    let remote_instances = env
        .state_path_for(&elsewhere)
        .root
        .join("records/loop-instances.json");
    std::fs::create_dir_all(remote_instances.parent().unwrap()).unwrap();
    nix::unistd::mkfifo(&remote_instances, nix::sys::stat::Mode::S_IRWXU).unwrap();
    write_loop_config(
        &env,
        &toml::to_string(&LoopConfig {
            tasks: Tasks(BTreeMap::from([
                (
                    "local".into(),
                    TaskEntry {
                        root: env.project_root.clone(),
                        check: Some("true".into()),
                        every: Some("1h".into()),
                        ..TaskEntry::default()
                    },
                ),
                (
                    "bound".into(),
                    TaskEntry {
                        root: env.project_root.clone(),
                        dir: Some(checkout),
                        check: Some("true".into()),
                        every: Some("1h".into()),
                        ..TaskEntry::default()
                    },
                ),
                (
                    "remote-task".into(),
                    TaskEntry {
                        root: elsewhere,
                        check: Some("true".into()),
                        every: Some("1h".into()),
                        ..TaskEntry::default()
                    },
                ),
            ])),
            ..LoopConfig::default()
        })
        .unwrap(),
    );

    let output = loop_watch_frames(&env);
    assert!(output.contains("loop · 2 tasks"), "{output}");
    assert!(
        output.contains("local") && output.contains("bound"),
        "{output}"
    );
    assert!(
        !output.contains("remote-task") && !output.contains("~/elsewhere"),
        "{output}"
    );
    assert!(
        output.contains("waits for room") && !output.contains("next:"),
        "{output}"
    );
}

#[test]
fn loop_watch_shows_a_run_whose_task_row_is_gone() {
    let env = Env::new();
    let holder = RunLockInfo {
        pid: 42_424,
        started_at: Timestamp::now() - SignedDuration::from_secs(3 * 60),
    };
    let _lock = hold_loop_run_lock(&loop_run_lock_path(&env, "later"), &holder);

    let output = loop_watch_frames(&env);

    assert!(
        output.contains("loop · 1 tasks")
            && output.contains("▸ 1 running")
            && output.contains("later")
            && output.contains("▸ running 3m")
            && !output.contains("next:")
            && !output.contains("no loop tasks"),
        "{output}"
    );
}

#[test]
fn loop_watch_stays_silent_when_a_task_run_lock_cannot_be_read() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "probe", "--check", "true", "--every", "1h"],
    );
    // A directory where the run lock file belongs makes opening the lock fail.
    std::fs::create_dir_all(loop_run_lock_path(&env, "probe")).unwrap();

    let output = loop_watch_frames(&env);

    assert!(
        output.contains("loop · 1 tasks")
            && output.contains("probe")
            && !output.contains("▸ running")
            && !output.contains("warning")
            && !output.contains("cannot read"),
        "{output}"
    );
}

#[test]
fn loop_watch_stays_silent_and_drops_a_group_holding_only_a_listing_error() {
    let env = Env::new();
    let elsewhere = env.home_root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    write_loop_config(
        &env,
        &format!(
            "[tasks.remote-task]\ncheck = \"true\"\nroot = \"{}\"\nevery = \"15m\"\n",
            elsewhere.display()
        ),
    );
    // A file where the project's locks directory belongs makes listing it fail.
    let locks = loop_run_lock_path(&env, "any");
    let locks = locks.parent().expect("locks directory");
    let _ = std::fs::remove_dir_all(locks);
    std::fs::create_dir_all(locks.parent().expect("runtime root")).unwrap();
    std::fs::write(locks, "").unwrap();

    let output = loop_watch_frames(&env);

    assert!(
        output.contains("loop · 0 tasks")
            && output.contains("no loop tasks")
            && !output.contains("~/elsewhere")
            && !output.contains("remote-task")
            && !output.contains("~/project")
            && !output.contains("warning")
            && !output.contains("listing loop run locks"),
        "{output}"
    );
}

#[cfg(unix)]
#[test]
fn loop_stop_reaches_a_run_holding_only_a_per_checkout_lock() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "fan", "--check", "true", "--every", "1h"],
    );
    let mut runner = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn holder");
    let holder = RunLockInfo {
        pid: runner.id(),
        started_at: Timestamp::now(),
    };
    let lock_file = hold_loop_run_lock(
        &loop_run_lock_path(&env, "fan-ws_0123456789abcdef01234567"),
        &holder,
    );

    let stop = env
        .rimz()
        .args(["loop", "stop", "fan"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn loop stop");
    // `loop stop` signals the holder only for a lock it found held.
    runner.wait().expect("holder stopped by loop stop");
    lock_file.unlock().expect("unlock loop run lock");
    let stop = stop.wait_with_output().expect("loop stop");
    let stdout = String::from_utf8_lossy(&stop.stdout);
    assert!(
        stop.status.success() && stdout.contains("loop `fan`: stopped · SIGTERM"),
        "{stdout}{}",
        String::from_utf8_lossy(&stop.stderr)
    );
}

#[test]
fn loop_stop_without_active_run_reports_no_active_run() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "idle", "--check", "true", "--every", "15m"],
    );
    let stopped = loop_ok(&env, &["loop", "stop", "idle"]);
    assert!(stopped.contains("loop `idle`: no active run"), "{stopped}");
}

#[cfg(unix)]
#[test]
fn terminal_check_runs_interactive_shell_to_completion() {
    if which::which("bash").is_err() {
        crate::common::skip("bash not on PATH");
        return;
    }
    let env = Env::new();
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "shell-probe",
            "--every",
            "15m",
            // A bound on the failure only: the check exits on its own, and a
            // shell stopped by terminal job control is what runs into it.
            "--timeout",
            "60s",
            "--check",
            "bash -i -c 'printf shell-ready'",
        ],
    );
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut command);
    command.args(["loop", "fire", "shell-probe"]);
    command.cwd(&env.project_root);
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        bytes
    });
    child.wait().unwrap();
    drop(pair.master);
    output.join().unwrap();
    let check = last_loop_record(&env).check.unwrap();
    assert!(!check.timed_out, "{}", check.output);
    assert!(check.output.contains("shell-ready"), "{}", check.output);
}

#[cfg(unix)]
#[test]
fn manual_check_clears_the_agent_identity_overlay() {
    let env = Env::new();
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "identity",
            "--every",
            "15m",
            "--check",
            "test -z \"${RIMZ_AGENT_ID+x}${RIMZ_AGENT_KIND+x}${RIMZ_AGENT_NAME+x}${RIMZ_AGENT_PROFILE+x}${RIMZ_AGENT_ROLE+x}${RIMZ_AGENT_MODEL+x}${RIMZ_AGENT_EFFORT+x}${RIMZ_AGENT_BUDGET+x}${RIMZ_AGENT_PID+x}\" && printf 'clean %s' \"${RIMZ_CHANNEL-unset}\"",
        ],
    );
    let output = env
        .rimz()
        .args(["loop", "fire", "identity"])
        .envs([
            ("RIMZ_CHANNEL", "probe"),
            ("RIMZ_AGENT_ID", "stale-launch"),
            ("RIMZ_AGENT_KIND", "claude"),
            ("RIMZ_AGENT_NAME", "old-name"),
            ("RIMZ_AGENT_PROFILE", "old-profile"),
            ("RIMZ_AGENT_ROLE", "old-role"),
            ("RIMZ_AGENT_MODEL", "old-model"),
            ("RIMZ_AGENT_EFFORT", "old-effort"),
            ("RIMZ_AGENT_BUDGET", "99"),
            ("RIMZ_AGENT_PID", "1"),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record = last_loop_record(&env);
    assert_eq!(record.result, LoopRunResult::Completed);
    assert_eq!(record.check.unwrap().output, "clean unset");
}

#[cfg(unix)]
#[test]
fn manual_fire_forwards_interrupt_to_the_check_group() {
    for (on, ignores_interrupt) in [
        (None, false),
        (Some("fail"), false),
        (Some("success"), false),
        (Some("any"), false),
        (Some("fail"), true),
    ] {
        let env = Env::new();
        let check = if ignores_interrupt {
            "trap '' INT; printf ready > check-ready; sleep 30"
        } else {
            "trap 'exit 130' INT; sh -c 'trap \"printf stopped > interrupted; kill \\$!; exit 130\" INT; sleep 30 & printf ready > check-ready; wait'"
        };
        let mut args = vec![
            "loop",
            "add",
            "interruptible",
            "--every",
            "15m",
            "--check",
            check,
        ];
        if let Some(on) = on {
            env.install_agent_hooks("claude");
            args.extend([
                "--agent",
                "claude",
                "--prompt",
                "must not launch",
                "--on",
                on,
            ]);
        }
        loop_ok(&env, &args);
        let mut runner = env
            .rimz()
            .args(["loop", "fire", "interruptible"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_for_path(&env.project_root.join("check-ready"));
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(runner.id() as i32),
            nix::sys::signal::Signal::SIGINT,
        )
        .unwrap();
        if !ignores_interrupt {
            wait_for_path(&env.project_root.join("interrupted"));
        }
        assert_eq!(runner.wait().unwrap().code(), Some(130));
        let record = last_loop_record(&env);
        assert_eq!(record.result, LoopRunResult::Canceled);
        assert!(record.run_id.is_none());
        let check = record.check.unwrap();
        assert_eq!(check.code, (!ignores_interrupt).then_some(130));
        assert!(!check.timed_out);
    }
}

#[cfg(unix)]
#[test]
fn loop_stop_cancels_a_spawn_run_in_an_unroomed_project() {
    loop_stop_of_a_spawn_run(false);
}

/// The run stops and its lock is released, then the command fails naming
/// the pane it could not close and the stop that does close it.
#[cfg(unix)]
#[test]
fn loop_stop_fails_after_stopping_a_run_whose_pane_stays_open() {
    loop_stop_of_a_spawn_run(true);
}

#[cfg(unix)]
fn loop_stop_of_a_spawn_run(pane_open: bool) {
    let env = Env::new();
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    trust_codex_project(&env, &env.project_root);
    let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let workspace = env.resolve_workspace(&env.project_root);
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        Vec::new(),
    );
    let pane_fixture = env.project_root.join("panes.json");
    std::fs::write(&pane_fixture, "[]").unwrap();
    let command = || {
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij"])
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&agent_bin))
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env(
                "RIMZ_ZELLIJ_BIN",
                crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
            )
            .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join("spawn.log"))
            .env("RIMZ_TEST_ZELLIJ_LOG_LAYOUTS", "1")
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
            .env("ZELLIJ_PANE_ID", "1")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1s ago]\n", workspace.session_name),
            );
        command
    };
    loop_ok(
        &env,
        &[
            "loop", "add", "spawn", "--agent", "codex", "--prompt", "fix it", "--every", "15m",
        ],
    );
    let paths = env.state_path_for(&env.project_root);
    assert!(!paths.workspace_record.exists());
    let mut runner = command()
        .args(["loop", "run", "spawn"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn loop runner");
    let lock_path = loop_run_lock_path(&env, "spawn");
    let info = wait_for_held_loop_lock(&mut runner, &lock_path);
    assert_eq!(info.pid, runner.id());
    let deadline = Instant::now() + Duration::from_secs(10);
    let run = loop {
        let records = rimz::harness::run::list(&paths).expect("list runs");
        if let Some(record) = records.first() {
            if record.status == RunStatus::Running {
                break record.clone();
            }
            // The trace mux opens no provider process, so supply its startup hook.
            if record.status == RunStatus::Pending {
                let mut hook = env.hook_command("codex");
                hook.env(rimz::harness::launch::ENV_RUN_ID, record.run_id.as_str())
                    .env(
                        rimz::harness::launch::ENV_AGENT_NAME,
                        record.agent_name.as_ref().unwrap().as_str(),
                    );
                let output = env
                    .spawn_payload(
                        hook,
                        &json!({
                            "hook_event_name": "SessionStart",
                            "session_id": "loop-spawn-session",
                            "cwd": env.project_root,
                        })
                        .to_string(),
                    )
                    .wait_with_output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        if let Some(status) = runner.try_wait().expect("poll loop runner") {
            let output = runner.wait_with_output().unwrap();
            panic!(
                "loop runner exited {status}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for Running run"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(run.loop_task.as_deref(), Some("spawn"));
    let trace = std::fs::read_to_string(env.project_root.join("spawn.log")).unwrap();
    // The run's pane opens in a `new-tab --layout`, which spells its argv as JSON strings.
    let payload = trace
        .split("\"--request\" ")
        .nth(1)
        .and_then(|rest| {
            serde_json::Deserializer::from_str(rest)
                .into_iter::<String>()
                .next()?
                .ok()
        })
        .unwrap_or_else(|| panic!("pane request in {trace}"));
    let request = rimz::harness::launch::decode_exec_request("codex", None, &payload).unwrap();
    assert_eq!(
        request.loop_reminder.as_deref(),
        Some(
            "RimZ started you from the rule `spawn`, which launches an agent every 15m. The prompt is the rule's fixed text, not a message someone just typed.\n\nEach run is a fresh agent with no memory of earlier runs. This is one turn, and the pane closes when it ends. Nobody is watching. Your final message is the result. Ask only when you cannot go on: a question waits until the user notices or the run is stopped. The run is stopped after 2h."
        )
    );
    assert!(paths.workspace_record.is_file());
    assert!(
        rimz::StatePaths::history_paths(&paths.root)
            .events_log
            .is_file()
    );

    let mut stop = command();
    if pane_open {
        rimz::harness::run::record_pane(
            &paths,
            &run.run_id,
            rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, "terminal_51"),
        )
        .unwrap();
        stop.env("RIMZ_TEST_ZELLIJ_FAIL_CLOSE_PANE", "1").env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            r#"[{"id":51,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
        );
    }
    let stopped = stop.args(["loop", "stop", "spawn"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&stopped.stderr);
    assert_eq!(stopped.status.success(), !pane_open, "{stderr}");
    if pane_open {
        assert!(
            String::from_utf8_lossy(&stopped.stdout).contains("loop `spawn`: stopped"),
            "{stopped:?}"
        );
        let retry = format!("run `rimz agents stop {}` to close it", run.run_id);
        assert!(
            stderr.contains("is still open")
                && stderr.contains(&retry)
                && !stderr.contains("rerun the stop"),
            "{stderr}"
        );
    }
    assert_eq!(
        rimz::harness::run::load(&paths, &run.run_id)
            .unwrap()
            .status,
        RunStatus::Canceled
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while runner.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "stopped runner did not exit");
        std::thread::sleep(Duration::from_millis(25));
    }
    let released = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    released.try_lock().expect("loop lock released");
    drop(released);
    if pane_open {
        let closes = || {
            std::fs::read_to_string(env.project_root.join("spawn.log"))
                .unwrap()
                .matches("close-pane")
                .count()
        };
        assert_eq!(closes(), 1);
        let again = command().args(["loop", "stop", "spawn"]).output().unwrap();
        assert!(again.status.success(), "{again:?}");
        assert_eq!(closes(), 1, "a second loop stop never reaches the pane");
        let retried = command()
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_PANES",
                r#"[{"id":51,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
            )
            .args(["agents", "stop", run.run_id.as_str()])
            .output()
            .unwrap();
        assert!(retried.status.success(), "{retried:?}");
        assert_eq!(closes(), 2, "the named stop closes the run's pane");
    }
    for entry in std::fs::read_dir(env.rimz_home().join("ws")).unwrap() {
        let name = entry.unwrap().file_name();
        let name = name.to_string_lossy();
        assert!(
            !name
                .strip_prefix("ws-")
                .is_some_and(|suffix| suffix.len() == 24
                    && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())),
            "fallback workspace directory: {name}"
        );
    }

    loop_ok(&env, &["gc", "--all"]);
    assert!(paths.root.is_dir());
    assert!(
        paths
            .runs_dir
            .join(format!("{}.json", run.run_id))
            .is_file()
    );
    assert_eq!(
        rimz::harness::run::load(&paths, &run.run_id)
            .unwrap()
            .status,
        RunStatus::Canceled
    );
}

#[cfg(unix)]
#[test]
fn loop_stop_terminates_holder_and_records_cancellation() {
    let env = Env::new();
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "stuck",
            "--check",
            "touch check-ready; parent=$PPID; while kill -0 \"$parent\" 2>/dev/null; do sleep 1; done",
            "--every",
            "15m",
        ],
    );
    let mut runner = env
        .rimz()
        .args(["loop", "run", "stuck"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stuck loop runner");
    let info = wait_for_held_loop_lock(&mut runner, &loop_run_lock_path(&env, "stuck"));
    assert_eq!(info.pid, runner.id());
    wait_for_path(&env.project_root.join("check-ready"));
    let stopped = loop_ok(&env, &["loop", "stop", "stuck"]);
    assert!(
        stopped.contains("stopped") && stopped.contains("SIGTERM"),
        "{stopped}"
    );
    let released = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(loop_run_lock_path(&env, "stuck"))
        .expect("open released loop lock");
    released.try_lock().expect("loop lock released");
    assert!(!runner.wait().expect("wait for stopped runner").success());
    let record = last_loop_record(&env);
    assert_eq!(record.result, LoopRunResult::Canceled);
    assert_eq!(record.error.as_deref(), Some("stopped by rimz loop stop"));
}

/// A fire that would start an agent waits behind another start's turn: it
/// is skipped at `max-wait`, shows as held while it waits, and a stop leaves
/// one `canceled` row and nothing of its own in the queue.
#[cfg(unix)]
#[test]
fn loop_run_held_by_the_start_throttle_is_shown_skipped_and_stopped() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    loop_ok(
        &env,
        &[
            "loop", "add", "queued", "--agent", "claude", "--prompt", "work", "--stay", "--every",
            "1h",
        ],
    );
    // Another start holds the turn for as long as this test process lives.
    let queue = env.rimz_home().join("loops/throttle");
    std::fs::create_dir_all(&queue).expect("mkdir throttle queue");
    std::fs::write(
        queue.join(format!("{:020}-01890000-0000-7000-8000-000000000000", 1)),
        json!({
            "owner": { "pid": std::process::id(), "start": null },
            "task": "other", "root": "/elsewhere", "checkout": "/elsewhere",
            "enqueued_ms": 1, "state": "admitted",
        })
        .to_string(),
    )
    .expect("write the held turn");
    let queued = || std::fs::read_dir(&queue).expect("read queue").count();

    loop_ok(&env, &["config", "set", "loop.throttle.max-wait", "2s"]);
    let output = env
        .rimz()
        .args(["loop", "run", "queued"])
        .output()
        .expect("run held loop");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record = last_loop_record(&env);
    assert_eq!(record.result, LoopRunResult::ThrottleSkipped);
    assert_eq!(record.error.as_deref(), Some("1 start ahead; held 2s"));
    assert_eq!(queued(), 1, "a skipped run leaves the queue");
    let show = loop_ok(&env, &["loop", "show", "queued"]);
    assert!(
        show.contains("throttle skipped") && show.contains("1 start ahead; held 2s"),
        "{show}"
    );
    assert!(show.contains("THROTTLE"), "{show}");

    loop_ok(&env, &["config", "set", "loop.throttle.max-wait", "30m"]);
    let mut runner = env
        .rimz()
        .args(["loop", "run", "queued"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn held loop runner");
    wait_for_held_loop_lock(&mut runner, &loop_run_lock_path(&env, "queued"));
    let deadline = Instant::now() + Duration::from_secs(15);
    let show = loop {
        let show = loop_ok(&env, &["loop", "show", "queued"]);
        if show.contains("held: 1 start ahead") {
            break show;
        }
        assert!(Instant::now() < deadline, "never shown as held: {show}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        show.contains(&format!("pid {}", runner.id())) && !show.contains("▸ running"),
        "{show}"
    );
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(
        list.contains("held: 1 start ahead") && !list.contains("▸ running"),
        "{list}"
    );
    let document: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "queued", "--json"])).unwrap();
    assert_eq!(document["held"]["reason"], "1 start ahead");
    assert_eq!(document["held"]["position"], 2);
    assert_eq!(document["held"]["pid"], runner.id());
    assert!(document["held"]["since"].is_string(), "{document}");
    assert_eq!(document["running"]["pid"], runner.id());
    assert_eq!(document["waiting"], json!([]));
    assert_eq!(queued(), 2);

    // Another checkout of the task waits in another process: the runner
    // keeps its own reason, and the other shows beside it, never paired
    // with the runner's pid.
    let own_ticket = std::fs::read_dir(&queue)
        .expect("read queue")
        .map(|entry| entry.expect("queue entry").path())
        .find(|path| {
            std::fs::read_to_string(path).is_ok_and(|ticket| ticket.contains("\"queued\""))
        })
        .expect("the runner's ticket");
    let mut other: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&own_ticket).unwrap()).unwrap();
    other["owner"] = json!({ "pid": std::process::id(), "start": null });
    other["checkout"] = json!("/elsewhere/checkout-b");
    other["reason"] = serde_json::Value::Null;
    let other_ticket = queue.join(format!(
        "{:020}-01890000-0000-7000-8000-000000000001",
        u64::MAX / 2
    ));
    std::fs::write(&other_ticket, other.to_string()).expect("write the other checkout's ticket");
    let show = loop_ok(&env, &["loop", "show", "queued"]);
    assert!(
        show.contains(&format!(
            "pid {} · 1 held: waiting for its turn",
            runner.id()
        )) && show.contains("held: 1 start ahead"),
        "{show}"
    );
    let list = loop_ok(&env, &["loop", "list"]);
    assert!(list.contains("· 1 held: waiting for its turn"), "{list}");
    let document: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "queued", "--json"])).unwrap();
    assert_eq!(document["held"]["pid"], runner.id());
    assert_eq!(document["waiting"][0]["pid"], std::process::id());
    assert_eq!(document["waiting"][0]["checkout"], "/elsewhere/checkout-b");
    // Another checkout's run of the task is active: it is not the held
    // runner's, which has no run yet.
    let paths = env.state_path_for(&env.project_root);
    paths.ensure_dirs().unwrap();
    let mut active = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "work".to_owned(),
        std::path::PathBuf::from("/elsewhere/checkout-c"),
    );
    active.status = RunStatus::Running;
    active.loop_task = Some("queued".to_owned());
    rimz::harness::run::create(&paths, &active).unwrap();
    let show = loop_ok(&env, &["loop", "show", "queued"]);
    assert!(
        show.contains("held: 1 start ahead") && !show.contains(&active.run_id.to_string()),
        "{show}"
    );
    let document: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "queued", "--json"])).unwrap();
    assert_eq!(document["running"]["pid"], runner.id());
    assert!(document["running"]["run_id"].is_null(), "{document}");
    rimz::harness::run::cancel(&paths, &active.run_id).unwrap();
    std::fs::remove_file(&other_ticket).expect("remove the other checkout's ticket");

    let stopped = loop_ok(&env, &["loop", "stop", "queued"]);
    assert!(stopped.contains("stopped"), "{stopped}");
    assert!(!runner.wait().expect("wait for stopped runner").success());
    let rows: Vec<_> = read_loop_run_records(&env)
        .into_iter()
        .map(|row| row.result)
        .collect();
    assert_eq!(
        rows,
        [LoopRunResult::ThrottleSkipped, LoopRunResult::Canceled],
        "a stop before any run record leaves exactly one row"
    );
    assert_eq!(queued(), 1, "the stopped run's ticket is reaped");
    let document: serde_json::Value =
        serde_json::from_str(&loop_ok(&env, &["loop", "show", "queued", "--json"])).unwrap();
    assert!(document["held"].is_null() && document["running"].is_null());
}

/// Ctrl-C ends a manual fire held by the start throttle after its check
/// passed: the check's own interrupt handling is gone by then, so the hold
/// listens for itself. The fire records `canceled` with the check it ran,
/// leaves no ticket, and launches nothing.
#[cfg(unix)]
#[test]
fn ctrl_c_ends_a_manual_fire_held_after_its_check_passed() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "checked",
            "--every",
            "15m",
            "--check",
            "printf ok > check-done",
            "--timeout",
            "5s",
            "--on",
            "success",
            "--agent",
            "claude",
            "--prompt",
            "must not launch",
        ],
    );
    let queue = env.rimz_home().join("loops/throttle");
    std::fs::create_dir_all(&queue).expect("mkdir throttle queue");
    std::fs::write(
        queue.join(format!("{:020}-01890000-0000-7000-8000-000000000000", 1)),
        json!({
            "owner": { "pid": std::process::id(), "start": null },
            "task": "other", "root": "/elsewhere", "checkout": "/elsewhere",
            "enqueued_ms": 1, "state": "admitted",
        })
        .to_string(),
    )
    .expect("write the held turn");
    let held_tickets = || {
        std::fs::read_dir(&queue)
            .expect("read queue")
            .filter_map(|entry| std::fs::read_to_string(entry.ok()?.path()).ok())
            .filter(|ticket| ticket.contains("1 start ahead"))
            .count()
    };

    let mut runner = env
        .rimz()
        .args(["loop", "fire", "checked"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn held fire");
    let deadline = Instant::now() + Duration::from_secs(30);
    while held_tickets() == 0 {
        if Instant::now() >= deadline || runner.try_wait().unwrap().is_some() {
            let _ = runner.kill();
            let output = runner.wait_with_output().unwrap();
            panic!(
                "the fire was never held: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(env.project_root.join("check-done").exists());
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(runner.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = runner.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = runner.kill();
            let _ = runner.wait();
            panic!("Ctrl-C did not end the held fire");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(130));
    let record = last_loop_record(&env);
    assert_eq!(record.result, LoopRunResult::Canceled);
    assert_eq!(record.error.as_deref(), Some("interrupted while held"));
    assert!(record.run_id.is_none(), "nothing launched");
    assert_eq!(record.check.expect("the check that ran").code, Some(0));
    assert_eq!(
        std::fs::read_dir(&queue).expect("read queue").count(),
        1,
        "only the other start's turn is left"
    );

    // Fired again and released mid-wait, the fire records how long it held.
    let mut runner = env
        .rimz()
        .args(["loop", "fire", "checked"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn held fire");
    let deadline = Instant::now() + Duration::from_secs(30);
    while held_tickets() == 0 {
        assert!(Instant::now() < deadline, "the second fire was never held");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(1_500));
    std::fs::remove_file(queue.join(format!("{:020}-01890000-0000-7000-8000-000000000000", 1)))
        .expect("release the turn");
    let deadline = Instant::now() + Duration::from_secs(60);
    while runner.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = runner.kill();
            panic!("the released fire never finished");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let record = last_loop_record(&env);
    assert_ne!(record.result, LoopRunResult::ThrottleSkipped);
    assert_ne!(record.result, LoopRunResult::Canceled);
    assert!(
        record
            .throttle_wait_ms
            .is_some_and(|waited| waited >= 1_000),
        "{record:?}"
    );
}

/// A scheduled `--in` spawn one-shot whose runner holds its lock with a Running
/// run record, after the fire consumed its instance row.
#[cfg(unix)]
struct ConsumedSpawn {
    shell: std::path::PathBuf,
    agent_bin: std::path::PathBuf,
    pane_fixture: std::path::PathBuf,
    session_name: String,
}

#[cfg(unix)]
impl ConsumedSpawn {
    fn command(&self, env: &Env) -> Command {
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij"])
            .env("SHELL", &self.shell)
            .env("PATH", path_with_front(&self.agent_bin))
            .env("RIMZ_TEST_PANE_LIST", &self.pane_fixture)
            .env(
                "RIMZ_ZELLIJ_BIN",
                crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
            )
            .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join("spawn.log"))
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
            .env("ZELLIJ_PANE_ID", "1")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1s ago]\n", self.session_name),
            );
        command
    }
}

#[cfg(unix)]
fn start_consumed_spawn(env: &Env, name: &str) -> (ConsumedSpawn, std::process::Child, RunRecord) {
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(env);
    trust_codex_project(env, &env.project_root);
    let pane_fixture = env.project_root.join("panes.json");
    std::fs::write(&pane_fixture, "[]").unwrap();
    let workspace = env.resolve_workspace(&env.project_root);
    crate::common::room::seed_live_zellij_room(
        &env.runtime_paths(),
        &workspace.session_name,
        Vec::new(),
    );
    let fixture = ConsumedSpawn {
        shell: write_fake_login_shell(env, "rimz-test-sh", &[]),
        agent_bin: crate::common::write_failing_agent_shim(env, "codex", 1),
        pane_fixture,
        session_name: workspace.session_name.to_string(),
    };
    loop_ok(
        env,
        &[
            "loop", "add", name, "--agent", "codex", "--prompt", "fix it", "--in", "30m",
        ],
    );
    assert!(read_loop_instances(env).0.contains_key(name));
    let paths = env.state_path_for(&env.project_root);
    let mut runner = fixture
        .command(env)
        .args(["loop", "run", name])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn loop runner");
    wait_for_held_loop_lock(&mut runner, &loop_run_lock_path(env, name));
    let deadline = Instant::now() + Duration::from_secs(10);
    let run = loop {
        let records = rimz::harness::run::list(&paths).expect("list runs");
        if let Some(record) = records.first() {
            if record.status == RunStatus::Running {
                break record.clone();
            }
            // The trace mux opens no provider process, so supply its startup hook.
            if record.status == RunStatus::Pending {
                let mut hook = env.hook_command("codex");
                hook.env(rimz::harness::launch::ENV_RUN_ID, record.run_id.as_str())
                    .env(
                        rimz::harness::launch::ENV_AGENT_NAME,
                        record.agent_name.as_ref().unwrap().as_str(),
                    );
                let output = env
                    .spawn_payload(
                        hook,
                        &json!({
                            "hook_event_name": "SessionStart",
                            "session_id": "loop-spawn-session",
                            "cwd": env.project_root,
                        })
                        .to_string(),
                    )
                    .wait_with_output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        if let Some(status) = runner.try_wait().expect("poll loop runner") {
            let output = runner.wait_with_output().unwrap();
            panic!(
                "loop runner exited {status}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for Running run"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(run.loop_task.as_deref(), Some(name));
    assert!(!read_loop_instances(env).0.contains_key(name));
    (fixture, runner, run)
}

#[cfg(unix)]
#[test]
fn loop_show_and_logs_find_a_consumed_one_shot_while_its_run_is_in_flight() {
    let env = Env::new();
    let (_fixture, mut runner, run) = start_consumed_spawn(&env, "later");
    let run_id = run.run_id.to_string();

    let show = loop_ok(&env, &["loop", "show", "later"]);
    assert!(
        show.contains("▸ running")
            && show.contains(&run_id)
            && show.contains("stop with `rimz loop stop later`")
            && !show.contains("no loop task")
            && !show.contains("rimz loop fire"),
        "{show}"
    );
    let logs = loop_ok(&env, &["loop", "logs", "later"]);
    assert!(
        logs.contains("▸ running") && logs.contains(&run_id) && !logs.contains("no loop task"),
        "{logs}"
    );
    let failed = loop_ok(&env, &["loop", "logs", "later", "--failed"]);
    assert!(
        failed.contains("no failed runs recorded") && !failed.contains("▸ running"),
        "{failed}"
    );
    runner.kill().unwrap();
    runner.wait().unwrap();
}

#[cfg(unix)]
#[test]
fn loop_stop_reaches_a_consumed_one_shot_run() {
    let env = Env::new();
    let (fixture, mut runner, run) = start_consumed_spawn(&env, "later");
    let paths = env.state_path_for(&env.project_root);

    let stopped = fixture
        .command(&env)
        .args(["loop", "stop", "later"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&stopped.stdout);
    assert!(
        stopped.status.success(),
        "{}",
        String::from_utf8_lossy(&stopped.stderr)
    );
    assert!(
        stdout.contains("loop `later`: stopped") && stdout.contains(run.run_id.as_str()),
        "{stdout}"
    );
    assert_eq!(
        rimz::harness::run::load(&paths, &run.run_id)
            .unwrap()
            .status,
        RunStatus::Canceled
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while runner.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "stopped runner did not exit");
        std::thread::sleep(Duration::from_millis(25));
    }
    let released = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(loop_run_lock_path(&env, "later"))
        .unwrap();
    released.try_lock().expect("loop lock released");
    drop(released);
    let record = last_loop_record(&env);
    assert_eq!(record.task, "later");
    assert_eq!(record.result, LoopRunResult::Canceled);
    assert!(!read_loop_instances(&env).0.contains_key("later"));
    let show = loop_ok(&env, &["loop", "show", "later"]);
    assert!(
        show.contains("canceled") && !show.contains("▸ running"),
        "{show}"
    );
}

#[cfg(unix)]
#[test]
fn loop_commands_follow_the_lock_before_a_consumed_one_shot_has_a_run_record() {
    let env = Env::new();
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "stuck",
            "--check",
            "touch check-ready; parent=$PPID; while kill -0 \"$parent\" 2>/dev/null; do sleep 1; done",
            "--in",
            "30m",
        ],
    );
    let mut runner = env
        .rimz()
        .args(["loop", "run", "stuck"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stuck loop runner");
    wait_for_held_loop_lock(&mut runner, &loop_run_lock_path(&env, "stuck"));
    wait_for_path(&env.project_root.join("check-ready"));
    let mut instances = read_loop_instances(&env);
    assert!(instances.0.remove("stuck").is_some());
    write_loop_instances(&env, instances);

    let show = loop_ok(&env, &["loop", "show", "stuck"]);
    assert!(
        show.contains("▸ running")
            && !show.contains("run run_")
            && show.contains("stop with `rimz loop stop stuck`"),
        "{show}"
    );
    let logs = loop_ok(&env, &["loop", "logs", "stuck"]);
    assert!(
        logs.contains("▸ running") && !logs.contains("run run_"),
        "{logs}"
    );
    let stopped = loop_ok(&env, &["loop", "stop", "stuck"]);
    assert!(
        stopped.contains("stopped") && stopped.contains("SIGTERM"),
        "{stopped}"
    );
    assert!(!runner.wait().expect("wait for stopped runner").success());
    let record = last_loop_record(&env);
    assert_eq!(record.task, "stuck");
    assert_eq!(record.result, LoopRunResult::Canceled);
    assert_eq!(record.error.as_deref(), Some("stopped by rimz loop stop"));
    let show = loop_ok(&env, &["loop", "show", "stuck"]);
    assert!(
        show.contains("stopped by rimz loop stop") && !show.contains("▸ running"),
        "{show}"
    );
}

#[test]
fn loop_commands_reject_an_unknown_name_with_a_free_lock() {
    let env = Env::new();
    for command in ["show", "logs", "stop"] {
        let (_stdout, error) = loop_fail(&env, &["loop", command, "ghost"]);
        assert!(error.contains("no loop task named `ghost`"), "{error}");
    }
    let paths = env.state_path_for(&env.project_root);
    paths.ensure_dirs().unwrap();
    let mut run = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        env.project_root.clone(),
    );
    run.status = RunStatus::Running;
    run.loop_task = Some("ghost".to_owned());
    rimz::harness::run::create(&paths, &run).unwrap();
    for command in ["show", "logs", "stop"] {
        let (_stdout, error) = loop_fail(&env, &["loop", command, "ghost"]);
        assert!(error.contains("no loop task named `ghost`"), "{error}");
    }
}

#[test]
fn loop_show_and_logs_drop_only_the_active_run_when_the_lock_lookup_fails() {
    let env = Env::new();
    loop_ok(
        &env,
        &["loop", "add", "probe", "--check", "true", "--every", "1h"],
    );
    loop_ok(&env, &["loop", "fire", "probe"]);
    // A directory where the run lock file belongs makes opening the lock fail.
    for name in ["probe", "ghost"] {
        let lock = loop_run_lock_path(&env, name);
        let _ = std::fs::remove_file(&lock);
        std::fs::create_dir_all(&lock).unwrap();
    }
    let display = |command: &str| {
        let output = env
            .rimz()
            .args(["loop", command, "probe"])
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "loop {command}: {stderr}");
        assert_eq!(
            stderr.matches("cannot read the loop run lock").count(),
            1,
            "loop {command}: {stderr}"
        );
        assert!(!stdout.contains("▸ running"), "{stdout}");
        stdout
    };
    let list = env.rimz().args(["loop", "list"]).output().unwrap();
    let stderr = String::from_utf8(list.stderr).unwrap();
    let stdout = String::from_utf8(list.stdout).unwrap();
    assert!(list.status.success(), "loop list: {stderr}");
    assert_eq!(
        stderr
            .matches("cannot read the loop run lock of `probe`")
            .count(),
        1,
        "loop list: {stderr}"
    );
    assert!(
        stdout.contains("probe") && !stdout.contains("▸ running"),
        "{stdout}"
    );

    let show = display("show");
    assert!(
        show.contains("every 1h") && show.contains("manual"),
        "{show}"
    );
    loop_ok(&env, &["loop", "remove", "probe"]);
    for command in ["show", "logs"] {
        let history = display(command);
        assert!(history.contains("manual"), "{history}");
    }
    let (_stdout, error) = loop_fail(&env, &["loop", "logs", "ghost"]);
    assert!(
        error.contains("cannot read the loop run lock")
            && error.contains("no loop task named `ghost`"),
        "{error}"
    );
}

#[test]
fn loop_add_persists_machine_and_project_signal_triggers() {
    let env = Env::new();
    let added = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "machine-signal",
            "--check",
            "true",
            "--on",
            "any",
            "--signal",
            "ci.failed",
            "--match",
            "branch=feature",
        ],
    );
    assert!(
        added.contains("trigger: fires on ci.failed [branch=feature]"),
        "{added}"
    );
    let machine: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).expect("read loop config"))
            .expect("parse loop config");
    let machine_task = &machine.tasks.0["machine-signal"];
    assert_eq!(machine_task.signal.as_deref(), Some("ci.failed"));
    assert_eq!(machine_task.on, Some(CheckOn::Any));
    assert_eq!(
        machine_task
            .matches
            .as_ref()
            .and_then(|matches| matches.get("branch"))
            .map(String::as_str),
        Some("feature")
    );

    let project_added = loop_ok(
        &env,
        &[
            "loop",
            "add",
            "project-signal",
            "--project",
            "--check",
            "true",
            "--signal",
            "ci.failed",
            "--match",
            "branch=feature",
        ],
    );
    assert!(
        project_added.contains("fires on ci.failed"),
        "{project_added}"
    );
    let config_path = env.project_root.join(".rimz/config.toml");
    let project_text = std::fs::read_to_string(&config_path).expect("project config");
    assert!(
        project_text.contains("[tasks.project-signal]"),
        "{project_text}"
    );
    assert!(
        project_text.contains("signal = \"ci.failed\"")
            && project_text.contains("[tasks.project-signal.match]")
            && project_text.contains("branch = \"feature\""),
        "{project_text}"
    );
    let trusted = rimz::trust::status_with_roots(&env.project_root, &env.rimz_home())
        .expect("project trust after add");
    assert_eq!(trusted.state, rimz::trust::TrustState::Trusted);
    let trusted_hash = trusted.current_hash.expect("trusted surface hash");

    std::fs::write(
        &config_path,
        project_text.replace("branch = \"feature\"", "branch = \"success\""),
    )
    .expect("change project signal match");
    let stale = rimz::trust::status_with_roots(&env.project_root, &env.rimz_home())
        .expect("project trust after signal change");
    assert_eq!(stale.state, rimz::trust::TrustState::Stale);
    assert_ne!(stale.current_hash.as_deref(), Some(trusted_hash.as_str()));
}

#[test]
fn loop_add_rejects_agent_signal_self_waits() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-self-wait", "feature-loop");

    for extra in [Vec::new(), vec!["--match", "handle=@claude"]] {
        let mut args = vec![
            "loop",
            "add",
            "self-wait",
            "--wait",
            "@claude",
            "--signal",
            "agent.idle",
            "--prompt",
            "continue",
        ];
        args.extend(extra);
        let (_stdout, error) = loop_fail(&env, &args);
        assert!(
            error.contains("requires --match handle=<other> or --match session=<other> to avoid waking the target from its own lifecycle signal"),
            "{error}"
        );
        assert!(read_loop_instances(&env).0.is_empty());
    }

    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "peer-wait",
            "--wait",
            "@claude",
            "--signal",
            "agent.idle",
            "--match",
            "handle=@reviewer",
            "--prompt",
            "continue",
        ],
    );
    let instances = read_loop_instances(&env);
    assert_eq!(
        instances.0["peer-wait"]
            .matches
            .as_ref()
            .and_then(|matches| matches.get("handle"))
            .map(String::as_str),
        Some("@reviewer")
    );
}

fn add_claude_account(env: &Env, name: &str) -> std::path::PathBuf {
    let home = env.home_root.join(format!("claude-{name}"));
    let output = env
        .rimz()
        .args(["accounts", "add", "claude", name, "--home"])
        .arg(&home)
        .output()
        .expect("rimz accounts add");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    home
}

#[test]
fn loop_add_pins_a_declared_account_and_refuses_the_rest() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    add_claude_account(&env, "work");
    let pinned = |name: &'static str, agent: &'static str, pin: &'static str| {
        vec![
            "loop",
            "add",
            name,
            "--agent",
            agent,
            "--account",
            pin,
            "--prompt",
            "x",
            "--every",
            "15m",
        ]
    };

    let (_, undeclared) = loop_fail(&env, &pinned("ghost", "claude", "ghost"));
    assert!(
        undeclared.contains(
            "unknown claude account `ghost`; configured: default, work; run `rimz accounts add claude ghost`"
        ),
        "{undeclared}"
    );
    let (_, unsupported) = loop_fail(&env, &pinned("unsupported", "grok", "work"));
    assert!(
        unsupported.contains("grok has no named accounts; only `default` is available"),
        "{unsupported}"
    );
    assert!(
        !loop_config_path(&env).exists(),
        "a refused add writes no row"
    );

    let added = loop_ok(&env, &pinned("held", "claude", "work"));
    assert!(
        added.contains("action: launches a fresh claude pane on account `work` in "),
        "{added}"
    );
    let config: LoopConfig =
        toml::from_str(&std::fs::read_to_string(loop_config_path(&env)).expect("loop config"))
            .expect("parse loop config");
    assert_eq!(
        config.tasks.0["held"].account,
        Some("work".parse().expect("login name"))
    );
    for view in [&["loop", "show", "held"][..], &["loop", "list"]] {
        let shown = loop_ok(&env, view);
        assert!(shown.contains("claude · account work"), "{shown}");
    }
}

#[test]
fn loop_fire_skips_when_its_pinned_account_cannot_run() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let home = add_claude_account(&env, "work");
    loop_ok(
        &env,
        &[
            "loop",
            "add",
            "pinned",
            "--agent",
            "claude",
            "--account",
            "work",
            "--prompt",
            "work",
            "--every",
            "15m",
        ],
    );
    let skipped = || {
        loop_ok(&env, &["loop", "run", "pinned"]);
        let record = last_loop_record(&env);
        assert_eq!(record.result, LoopRunResult::AccountSkipped);
        record.error.expect("skip reason")
    };

    env.publish_accounts(&rimz::agents::account::AccountsCache {
        logins: BTreeMap::from([(
            "claude@work".parse().unwrap(),
            rimz::agents::account::ProviderRecord {
                login: None,
                probed_at_ms: 1,
                ok: true,
                account: None,
            },
        )]),
    });
    assert_eq!(
        skipped(),
        format!(
            "claude account `work` is logged out; log in with `CLAUDE_CONFIG_DIR={} claude`",
            home.display()
        )
    );

    std::fs::remove_file(env.runtime_paths().shared_accounts_path()).unwrap();
    std::fs::remove_dir_all(&home).unwrap();
    let reason = skipped();
    assert!(reason.contains("is not a directory; run `"), "{reason}");

    let config = std::fs::read_to_string(loop_config_path(&env)).unwrap();
    std::fs::write(
        loop_config_path(&env),
        config.replace("account = \"work\"", "account = \"ghost\""),
    )
    .unwrap();
    assert_eq!(
        skipped(),
        "unknown claude account `ghost`; configured: default, work; run `rimz accounts add claude ghost`"
    );

    assert_eq!(read_loop_run_records(&env).len(), 3);
    assert!(read_loop_strikes(&env).is_empty(), "a skip adds no strike");
    let paths = env.state_path_for(&env.project_root);
    assert!(
        rimz::harness::run::list(&paths)
            .unwrap_or_default()
            .is_empty(),
        "a skipped fire launches nothing"
    );
}

/// A codex task pinned to `work` or `default` fires in a room whose codex
/// account is `spare`: the launch is stamped with the pin, never the room's.
/// A `default` pin stores no stamp, which every launch reads as the provider's
/// own home.
#[cfg(unix)]
#[test]
fn loop_fire_launches_on_the_pinned_account_not_the_rooms() {
    for (pin, stamp) in [("work", Some("work")), ("default", None)] {
        let env = Env::new();
        env.install_agent_hooks("codex");
        trust_codex_preflight_hooks(&env);
        trust_codex_project(&env, &env.project_root);
        for name in ["work", "spare"] {
            let home = env.home_root.join(name);
            let output = env
                .rimz()
                .args(["accounts", "add", "codex", name, "--home"])
                .arg(&home)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            // Trust the named home's hooks the way the default home's are.
            let config = env.agent_config_path("codex");
            let mut table: toml::Table = std::fs::read_to_string(&config).unwrap().parse().unwrap();
            let state = table["hooks"]["state"].as_table_mut().unwrap();
            let prefix = format!("{}:", config.display());
            let named: Vec<_> = state
                .iter()
                .filter_map(|(key, value)| {
                    key.strip_prefix(&prefix).map(|event| {
                        (
                            format!("{}:{event}", home.join("config.toml").display()),
                            value.clone(),
                        )
                    })
                })
                .collect();
            state.extend(named);
            std::fs::write(config, toml::to_string(&table).unwrap()).unwrap();
        }
        let agent_bin = crate::common::write_failing_agent_shim(&env, "codex", 1);
        let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
        let workspace = env.resolve_workspace(&env.project_root);
        let store = env.store();
        store.record_workspace(&workspace).expect("record room");
        store
            .switch_room_login(
                &workspace,
                &AgentKind::new_unchecked("codex"),
                &"spare".parse().unwrap(),
            )
            .unwrap();
        crate::common::room::seed_live_zellij_room(
            &env.runtime_paths(),
            &workspace.session_name,
            Vec::new(),
        );
        let pane_fixture = env.project_root.join("panes.json");
        std::fs::write(&pane_fixture, "[]").unwrap();
        let command = || {
            let mut command = env.rimz();
            command
                .args(["--mux", "zellij"])
                .env("SHELL", &shell)
                .env("PATH", path_with_front(&agent_bin))
                .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
                .env(
                    "RIMZ_ZELLIJ_BIN",
                    crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
                )
                .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join("spawn.log"))
                .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
                .env("ZELLIJ_PANE_ID", "1")
                .env(
                    "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                    format!("{} [Created 1s ago]\n", workspace.session_name),
                );
            command
        };
        loop_ok(
            &env,
            &[
                "loop",
                "add",
                "spawn",
                "--agent",
                "codex",
                "--account",
                pin,
                "--prompt",
                "fix it",
                "--every",
                "15m",
            ],
        );
        let mut runner = command()
            .args(["loop", "run", "spawn"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn loop runner");
        let deadline = Instant::now() + Duration::from_secs(10);
        let launched = loop {
            let agents = store
                .runtime_projection(rimz::RuntimeScope::Audit)
                .expect("launch history")
                .agents;
            if let Some(agent) = agents.into_iter().next() {
                break agent;
            }
            if let Some(status) = runner.try_wait().expect("poll loop runner") {
                let output = runner.wait_with_output().unwrap();
                panic!(
                    "loop runner exited {status}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the launch"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        assert_eq!(
            launched.login.as_ref().map(|name| name.as_str()),
            stamp,
            "task pinned to `{pin}`"
        );
        let stopped = command().args(["loop", "stop", "spawn"]).output().unwrap();
        assert!(
            stopped.status.success(),
            "{}",
            String::from_utf8_lossy(&stopped.stderr)
        );
        runner.wait().expect("stopped runner exits");
    }
}

#[test]
fn loop_add_rejects_invalid_action_shapes() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-loop-validate", "feature-loop");
    let cases = [
        (
            vec!["loop", "add", "missing", "--every", "15m", "--prompt", "x"],
            "needs --agent, --wait, or --check",
        ),
        (
            vec![
                "loop", "add", "conflict", "--agent", "claude", "--wait", "@claude", "--every",
                "15m", "--prompt", "x",
            ],
            "cannot be used with",
        ),
        (
            vec![
                "loop",
                "add",
                "wait-mode",
                "--wait",
                "@claude",
                "--mode",
                "auto",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "`wait-mode` uses --wait, so --mode only apply to --agent tasks",
        ),
        (
            vec![
                "loop",
                "add",
                "wait-flags",
                "--wait",
                "@claude",
                "--mode",
                "auto",
                "--effort",
                "high",
                "--budget",
                "5",
                "--budget-per-day",
                "20",
                "--system-prompt-file",
                "missing-system-prompt",
                "--timeout",
                "1m",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "`wait-flags` uses --wait, so --mode, --effort, --budget, --budget-per-day, --system-prompt-file, --timeout only apply to --agent tasks",
        ),
        (
            vec![
                "loop",
                "add",
                "check-flags",
                "--check",
                "true",
                "--worktree",
                "lane",
                "--mode",
                "auto",
                "--effort",
                "high",
                "--budget",
                "5",
                "--budget-per-day",
                "20",
                "--system-prompt-file",
                "missing-system-prompt",
                "--timeout",
                "1m",
                "--every",
                "15m",
            ],
            "`check-flags` uses --check without an agent action, so --worktree, --mode, --effort, --budget, --budget-per-day, --system-prompt-file only apply to --agent tasks",
        ),
        (
            vec![
                "loop",
                "add",
                "wait-account",
                "--wait",
                "@claude",
                "--account",
                "work",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "`wait-account` uses --wait, so --account only apply to --agent tasks",
        ),
        (
            vec![
                "loop",
                "add",
                "check-account",
                "--check",
                "true",
                "--account",
                "work",
                "--every",
                "15m",
            ],
            "`check-account` uses --check without an agent action, so --account only apply to --agent tasks",
        ),
        (
            vec![
                "loop", "add", "on", "--agent", "claude", "--on", "fail", "--every", "15m",
                "--prompt", "x",
            ],
            "--on requires --check",
        ),
        (
            vec![
                "loop",
                "add",
                "match",
                "--check",
                "true",
                "--match",
                "status=failed",
                "--every",
                "15m",
            ],
            "--match requires --signal",
        ),
        (
            vec![
                "loop",
                "add",
                "bad-match",
                "--check",
                "true",
                "--signal",
                "ci.failed",
                "--match",
                "status",
            ],
            "invalid --match `status`; expected KEY=VALUE",
        ),
        (
            vec![
                "loop",
                "add",
                "signal-schedule",
                "--check",
                "true",
                "--signal",
                "ci.failed",
                "--every",
                "15m",
            ],
            "cannot be used with",
        ),
        (
            vec![
                "loop",
                "add",
                "surplus",
                "--check",
                "true",
                "--surplus",
                "1.5x",
                "--every",
                "15m",
            ],
            "--surplus and --surplus-after require --agent or --wait",
        ),
        (
            vec![
                "loop",
                "add",
                "attempts",
                "--agent",
                "claude",
                "--verify",
                "true",
                "--max-attempts",
                "0",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "--max-attempts must be at least 1",
        ),
        (
            vec![
                "loop",
                "add",
                "until-check",
                "--agent",
                "claude",
                "--until",
                "1h",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "--until requires --check",
        ),
        (
            vec![
                "loop",
                "add",
                "until-every",
                "--agent",
                "claude",
                "--until",
                "1h",
                "--check",
                "true",
                "--prompt",
                "x",
            ],
            "--until requires --every",
        ),
        (
            vec![
                "loop", "add", "until-in", "--agent", "claude", "--until", "1h", "--check", "true",
                "--in", "15m", "--prompt", "x",
            ],
            "--until requires --every",
        ),
        (
            vec![
                "loop",
                "add",
                "until-action",
                "--until",
                "1h",
                "--check",
                "true",
                "--every",
                "15m",
            ],
            "--until requires --agent or --wait",
        ),
        (
            vec![
                "loop",
                "add",
                "project-wait",
                "--project",
                "--wait",
                "@claude",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "--project tasks cannot use --wait; project config cannot pin a machine-local session",
        ),
        (
            vec![
                "loop",
                "add",
                "project-deadline",
                "--project",
                "--agent",
                "claude",
                "--check",
                "true",
                "--until",
                "1h",
                "--every",
                "15m",
                "--prompt",
                "x",
            ],
            "--project tasks cannot use --until; poll-until deadlines are machine state",
        ),
        (
            vec![
                "loop",
                "add",
                "project-once",
                "--project",
                "--check",
                "true",
                "--at",
                "12:00",
            ],
            "--project tasks need a trigger; set --every, --cron, or --signal",
        ),
        (
            vec![
                "loop",
                "add",
                "project-signal-once",
                "--project",
                "--check",
                "true",
                "--signal",
                "ci.failed",
                "--once",
            ],
            "--project tasks cannot use --once; one-shot subscriptions are machine state",
        ),
    ];
    for (args, expected) in cases {
        let (_stdout, error) = loop_fail(&env, &args);
        assert!(error.contains(expected), "rimz {args:?}: {error}");
    }
}

fn loop_ok(env: &Env, args: &[&str]) -> String {
    let output = env.rimz().args(args).output().expect("rimz loop");
    assert!(
        output.status.success(),
        "rimz {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout")
}

fn calling_loop(env: &Env, session: &str) -> Command {
    let mut command = env.rimz();
    command
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", session);
    command
}

fn loop_ok_root(env: &Env, root: &Path, args: &[&str]) -> String {
    let output = env
        .rimz()
        .arg("--root")
        .arg(root)
        .args(args)
        .output()
        .expect("rimz loop at root");
    assert!(
        output.status.success(),
        "rimz --root {} {args:?} failed: {}",
        root.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout")
}

fn qwen_loop_ok(env: &Env, settings: &Path, args: &[&str]) -> String {
    let output = env
        .rimz()
        .env("RIMZ_QWEN_SETTINGS", settings)
        .args(args)
        .output()
        .expect("rimz Qwen loop");
    assert!(
        output.status.success(),
        "rimz {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout")
}

fn loop_fail(env: &Env, args: &[&str]) -> (String, String) {
    let output = env.rimz().args(args).output().expect("rimz loop");
    assert!(
        !output.status.success(),
        "rimz {args:?} should fail: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    (
        String::from_utf8(output.stdout).expect("stdout"),
        String::from_utf8(output.stderr).expect("stderr"),
    )
}

fn last_loop_record(env: &Env) -> LoopRunRecord {
    read_loop_run_records(env)
        .pop()
        .expect("last loop run record")
}

fn grant_project_trust(env: &Env) {
    loop_ok(env, &["trust", "grant"]);
}

fn assert_pending_message(env: &Env, session: &str, text_fragment: &str) {
    let messages = env
        .store()
        .list_pending_messages()
        .expect("pending messages");
    assert_eq!(messages.len(), 1, "{messages:?}");
    let message = &messages[0];
    assert_eq!(message.kind.as_str(), "claude");
    assert_eq!(message.agent_id.as_str(), session);
    assert_eq!(message.status, MessageStatus::Queued);
    assert!(message.text.contains(text_fragment), "{}", message.text);
}

fn stamp_session_ended(env: &Env, session_id: &str) {
    let workspace = env.resolve_workspace(&env.project_root);
    let observation = rimz::agents::AgentLifecycleObservation::new(
        Some(AgentSessionId::from(session_id)),
        rimz::agents::LifecycleSignal::Ended,
    );
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "rimz.agent-ended",
            &observation,
        ))
        .expect("stamp session ended without a hook");
}

fn register_running_agent(env: &Env, session_id: &str, branch: &str) {
    register_running_agent_at(env, session_id, branch, &env.project_root);
}

fn register_running_agent_at(env: &Env, session_id: &str, branch: &str, cwd: &Path) {
    run_hook(
        env,
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
        }),
        cwd,
    );
    run_hook(
        env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "work",
            "worktree_branch": branch,
        }),
        cwd,
    );
}

fn run_hook(env: &Env, payload: serde_json::Value, cwd: &Path) {
    run_agent_hook(env, "claude", payload, cwd);
}

fn run_agent_hook(env: &Env, source: &str, payload: serde_json::Value, cwd: &Path) {
    let payload = serde_json::to_string(&payload).expect("payload");
    let owner = dummy_agent_process();
    let owner_pid = owner.id();
    reap_later(owner);
    let mut cmd = env.hook_command(source);
    cmd.current_dir(cwd)
        .env("RIMZ_AGENT_PID", owner_pid.to_string());
    if let Some(channel) =
        rimz::harness::spec::resolve_room_channel(&env.project_root, cwd, None, None)
    {
        cmd.env(rimz::workspace::ENV_CHANNEL, channel);
    }
    let output = env
        .spawn_payload(cmd, &payload)
        .wait_with_output()
        .expect("wait hook");
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn dummy_agent_process() -> std::process::Child {
    let mut cmd = Command::new("sleep");
    cmd.scrub_session_env();
    // ponytail: bounded sleeper keeps hook-owned agents live for test snapshots;
    // add a per-test owner guard if tests start lasting longer than this window.
    cmd.arg("30").spawn().expect("spawn dummy agent process")
}

fn reap_later(mut child: std::process::Child) {
    let _ = std::thread::spawn(move || {
        let _ = child.wait();
    });
}

fn write_loop_config(env: &Env, text: &str) {
    let path = loop_config_path(env);
    std::fs::create_dir_all(path.parent().expect("config dir")).expect("mkdir config");
    std::fs::write(path, text).expect("write loop config");
}

fn write_project_config(env: &Env, text: &str) {
    write_project_config_at(&env.project_root, text);
}

fn write_project_config_at(project_root: &Path, text: &str) {
    let path = project_root.join(".rimz/config.toml");
    std::fs::create_dir_all(path.parent().expect("project config dir"))
        .expect("mkdir project config");
    std::fs::write(path, text).expect("write project config");
}

fn loop_arming_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("loops/loop-arming.json")
}

fn loop_strikes_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("loops/loop-strikes.json")
}

fn loop_instances_path(env: &Env) -> std::path::PathBuf {
    env.state_path_for(&env.project_root)
        .root
        .join("records/loop-instances.json")
}

fn loop_runs_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("logs/loop-runs.log.jsonl")
}

fn read_loop_run_records(env: &Env) -> Vec<LoopRunRecord> {
    let path = loop_runs_path(env);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(|line| serde_json::from_str(line).expect("loop run record"))
        .collect()
}

fn wait_for_loop_run_records(
    env: &Env,
    description: &str,
    ready: impl Fn(&[LoopRunRecord]) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let records = read_loop_run_records(env);
        if ready(&records) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}: {records:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn write_loop_run_records(env: &Env, records: &[LoopRunRecord]) {
    let path = loop_runs_path(env);
    std::fs::create_dir_all(path.parent().expect("log parent")).expect("mkdir log parent");
    let mut text = String::new();
    for record in records {
        text.push_str(&serde_json::to_string(record).expect("loop record json"));
        text.push('\n');
    }
    std::fs::write(path, text).expect("write loop run records");
}

fn read_loop_instances(env: &Env) -> Tasks {
    let path = loop_instances_path(env);
    let Ok(text) = std::fs::read_to_string(path) else {
        return Tasks::default();
    };
    serde_json::from_str(&text).expect("loop instances")
}

fn read_loop_arming(env: &Env) -> BTreeMap<String, Arming> {
    let path = loop_arming_path(env);
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).expect("loop arming")
}

fn write_loop_arming(env: &Env, entries: &BTreeMap<String, Arming>) {
    let path = loop_arming_path(env);
    std::fs::create_dir_all(path.parent().expect("arming parent")).expect("mkdir arming parent");
    std::fs::write(
        path,
        serde_json::to_vec_pretty(entries).expect("arming json"),
    )
    .expect("write loop arming");
}

fn machine_task_key(name: &str) -> String {
    format!("machine::{name}")
}

fn project_task_key(project_root: &Path, name: &str) -> String {
    format!(
        "{}::{name}",
        rimz::ids::WorkspaceId::from_project_root(project_root)
    )
}

fn read_loop_strikes(env: &Env) -> BTreeMap<String, u32> {
    let path = loop_strikes_path(env);
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).expect("loop strikes")
}

fn write_loop_instances(env: &Env, tasks: Tasks) {
    let path = loop_instances_path(env);
    std::fs::create_dir_all(path.parent().expect("instances parent")).expect("mkdir state");
    std::fs::write(path, serde_json::to_vec_pretty(&tasks).expect("json"))
        .expect("write loop instances");
}

fn write_loop_fire_state(env: &Env, stamps: BTreeMap<String, Timestamp>) {
    write_loop_fire_state_for_root(env, &env.project_root, stamps);
}

fn write_loop_fire_state_for_root(env: &Env, root: &Path, stamps: BTreeMap<String, Timestamp>) {
    let state = env.state_path_for(root);
    let path =
        rimz::RuntimePaths::for_state_under(&state, &env.runtime_root).lane_path("loop-fire.json");
    std::fs::create_dir_all(path.parent().expect("loop fire parent")).expect("mkdir runtime");
    std::fs::write(path, serde_json::to_vec_pretty(&stamps).expect("json"))
        .expect("write loop fire state");
}

fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(path.exists(), "timed out waiting for {}", path.display());
}

/// Run `loop watch --hold` in a pty through its first complete repaint, and return everything it wrote, stderr included.
fn loop_watch_frames(env: &Env) -> String {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open loop watch pty");
    let mut cmd = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut cmd);
    cmd.args(["loop", "watch", "--hold"]);
    cmd.cwd(env.project_root.as_os_str());
    cmd.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn loop watch");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0; 4096];
        while let Ok(count) = reader.read(&mut buffer) {
            if count == 0 {
                break;
            }
            output.extend_from_slice(&buffer[..count]);
            // replace_frame ends each complete repaint with Clear(FromCursorDown).
            if output.windows(3).any(|bytes| bytes == b"\x1b[J") {
                let _ = ready_tx.send(());
            }
        }
        output
    });
    let ready = ready_rx.recv_timeout(Duration::from_secs(15));
    child.kill().expect("terminate loop watch");
    let _ = child.wait().expect("reap loop watch");
    drop(pair.master);
    let output = reader_thread.join().expect("join pty reader");
    let output = String::from_utf8_lossy(&output).into_owned();
    assert!(
        ready.is_ok(),
        "loop watch did not repaint: {ready:?}\n{output}"
    );
    output
}

fn loop_run_lock_path(env: &Env, name: &str) -> std::path::PathBuf {
    env.runtime_paths()
        .lock_path(format!("loop-run-{name}.lock"))
}

/// Hold a run lock as a runner would, with `holder` as its payload.
fn hold_loop_run_lock(path: &Path, holder: &RunLockInfo) -> std::fs::File {
    std::fs::create_dir_all(path.parent().expect("lock parent")).expect("mkdir runtime");
    std::fs::write(path, serde_json::to_vec(holder).unwrap()).expect("write lock holder");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open lock");
    lock_file.try_lock().expect("hold loop run lock");
    lock_file
}

#[cfg(unix)]
fn wait_for_held_loop_lock(child: &mut std::process::Child, path: &Path) -> RunLockInfo {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            && matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
            && let Ok(bytes) = std::fs::read(path)
            && let Ok(info) = serde_json::from_slice(&bytes)
        {
            return info;
        }
        assert!(
            child.try_wait().expect("poll loop runner").is_none(),
            "loop runner exited before holding its lock"
        );
        assert!(
            Instant::now() < deadline,
            "timed out waiting for loop run lock {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn append_legacy_loop_record(env: &Env, task: &str, result: LoopRunResult) {
    let path = loop_runs_path(env);
    std::fs::create_dir_all(path.parent().expect("log parent")).expect("mkdir log parent");
    let result = serde_json::to_string(&result).expect("result json");
    let line =
        format!("{{\"task\":\"{task}\",\"at\":\"1970-01-01T00:00:10Z\",\"result\":{result}}}\n");
    std::fs::write(path, line).expect("write legacy loop run record");
}

fn loop_config_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("loop.toml")
}

fn init_git_repo(root: &Path) -> bool {
    if !git_ok(root, &["init", "-q", "-b", "main"]) {
        return false;
    }
    let _ = git_ok(root, &["config", "user.email", "test@example.com"]);
    let _ = git_ok(root, &["config", "user.name", "Test User"]);
    std::fs::write(root.join("README.md"), "base\n").expect("write README");
    git_ok(root, &["add", "README.md"]) && git_ok(root, &["commit", "-q", "-m", "base"])
}

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .status()
        .is_ok_and(|status| status.success())
}

fn find_real_git() -> Option<std::path::PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
}

#[test]
fn loop_add_spawn_still_requires_prompt() {
    let env = Env::new();
    let output = env
        .rimz()
        .args([
            "loop", "add", "no-note", "--agent", "claude", "--every", "15m",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("needs a prompt; pass --prompt or --prompt-file"),
        "{error}"
    );
}
