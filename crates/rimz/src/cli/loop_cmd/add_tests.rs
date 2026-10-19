use super::*;
use clap::FromArgMatches;

#[test]
fn resident_named_worktree_accepts_scope_independent_triggers() {
    for trigger in [
        vec!["--when", "pr[topic]=merged"],
        vec!["--when", "window.5h.left<50"],
        vec!["--every", "1h"],
        vec!["--cron", "0 * * * *"],
    ] {
        let argv = ["--agent", "claude", "--stay", "--worktree", "lane"]
            .into_iter()
            .chain(trigger)
            .collect::<Vec<_>>();
        let result = validate_add_args(&args(&argv));
        assert!(result.is_ok(), "{argv:?}: {result:?}");
    }
}

#[test]
fn resident_named_worktree_refuses_fanout_and_scope_reading_conditions() {
    for (flags, key) in [
        (
            vec!["--each-worktree", "--when", "pr[topic]=merged"],
            "--each-worktree",
        ),
        (vec!["--when", "pr=open"], "pr"),
        (vec!["--when", "!ci=failed"], "ci"),
        (
            vec!["--when", "pr[topic]=merged && pr.queue=queued"],
            "pr.queue",
        ),
        (
            vec!["--when", "pr[topic]=merged || team.stage=Done"],
            "team.stage",
        ),
    ] {
        let argv = ["--agent", "claude", "--stay", "--worktree", "lane"]
            .into_iter()
            .chain(flags)
            .collect::<Vec<_>>();
        let error = validate_add_args(&args(&argv)).unwrap_err().to_string();
        assert!(
            error.contains("--worktree") && error.contains(key),
            "{error}"
        );
        if key != "--each-worktree" {
            assert!(
                error.contains("scope") && error.contains("pr[<branch>]"),
                "{error}"
            );
        }
    }
}

fn judge_args(extra: &[&str]) -> AddArgs {
    let matches = AddArgs::augment_args(clap::Command::new("add"))
        .try_get_matches_from(["add", "resident"].into_iter().chain(extra.iter().copied()));
    assert!(
        matches.is_ok(),
        "judge flags must be recognized: {matches:?}"
    );
    AddArgs::from_arg_matches(&matches.unwrap()).unwrap()
}

#[test]
fn shell_and_judge_flags_validate_caps_and_polarity() {
    for action in [vec!["--agent", "codex"], vec!["--wait"]] {
        let flags = action
            .into_iter()
            .chain([
                "--check",
                "cargo test",
                "--check-timeout",
                "10m",
                "--judge",
                "claude",
                "--judge-prompt",
                "q",
                "--judge-timeout",
                "2m",
                "--on",
                "fail",
                "--every",
                "15m",
            ])
            .collect::<Vec<_>>();
        assert!(validate_add_args(&judge_args(&flags)).is_ok(), "{flags:?}");
    }
    assert!(
        validate_add_args(&judge_args(&[
            "--agent",
            "codex",
            "--stay",
            "--check",
            "true",
            "--check-timeout",
            "10m"
        ]))
        .is_ok()
    );
}

#[test]
fn judge_modifiers_require_judge_action_and_explicit_polarity() {
    for (flags, expected) in [
        (
            vec![
                "--agent",
                "codex",
                "--judge",
                "claude",
                "--judge-prompt",
                "q",
            ],
            "--on",
        ),
        (
            vec!["--judge", "claude", "--judge-prompt", "q", "--on", "fail"],
            "--agent or --wait",
        ),
        (
            vec!["--agent", "codex", "--judge-prompt", "q"],
            "requires --judge",
        ),
        (
            vec!["--agent", "codex", "--judge-prompt-file", "q.md"],
            "requires --judge",
        ),
        (
            vec!["--agent", "codex", "--judge-recheck", "1h"],
            "requires --judge",
        ),
        (
            vec!["--agent", "codex", "--judge-timeout", "1m"],
            "--check-timeout",
        ),
        (
            vec!["--agent", "codex", "--check-timeout", "1m"],
            "requires --check",
        ),
        (
            vec![
                "--agent",
                "codex",
                "--judge",
                "claude",
                "--judge-prompt",
                "q",
                "--judge-recheck",
                "1h",
                "--on",
                "fail",
            ],
            "requires --stay",
        ),
    ] {
        let result = validate_add_args(&judge_args(&flags));
        assert!(result.is_err(), "{flags:?} must be refused");
        assert!(
            result.unwrap_err().to_string().contains(expected),
            "{flags:?}: expected {expected}"
        );
    }
}

#[test]
fn both_entry_builders_preserve_the_same_guard() {
    if std::env::var_os("RIMZ_TEST_GUARD_BUILDERS").is_none() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::loop_cmd::add::tests::both_entry_builders_preserve_the_same_guard",
                "--nocapture",
            ])
            .env("RIMZ_TEST_GUARD_BUILDERS", "1")
            .env("RIMZ_HOME", home.path())
            .output()
            .unwrap();
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let workspace = rimz::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let state = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&state).unwrap();
    rimz::Store::open(state, runtime)
        .unwrap()
        .record_workspace(&workspace)
        .unwrap();
    let flags = [
        "--check",
        "cargo test",
        "--check-timeout",
        "10m",
        "--judge",
        "claude",
        "--judge-prompt",
        "q",
        "--judge-timeout",
        "2m",
        "--on",
        "fail",
        "--every",
        "15m",
        "--prompt",
        "fix",
    ];
    let spawn_args = judge_args(&[vec!["--agent", "codex"], flags.to_vec()].concat());
    let action = resolve_add_action(&spawn_args, &workspace, TaskActionKind::Spawn).unwrap();
    let (spawn, _) = build_task_entry(
        &spawn_args,
        action,
        &workspace,
        resolve_add_check(&spawn_args, &workspace).unwrap(),
    )
    .unwrap();
    let wait_args = judge_args(&[vec!["--wait"], flags.to_vec()].concat());
    add_delivery(
        &wait_args,
        &workspace,
        TaskTarget {
            kind: rimz::ids::AgentKind::new_unchecked("codex"),
            session: "session".into(),
            handle: "@coder".into(),
        },
        None,
        BTreeMap::new(),
        resolve_add_check(&wait_args, &workspace).unwrap(),
    )
    .unwrap();
    let catalog = TaskCatalog::load(Some(root.path())).unwrap();
    let delivered = catalog.visible()["resident"].entry().clone();
    assert_eq!(spawn.guard().shell_timeout, Some("10m"));
    assert_eq!(spawn.guard().judges.len(), 1);
    assert_eq!(delivered.guard().shell, spawn.guard().shell);
    assert_eq!(delivered.guard().shell_timeout, spawn.guard().shell_timeout);
    assert_eq!(delivered.guard().judges, spawn.guard().judges);
    assert_eq!(delivered.on, spawn.on);
}

#[test]
fn approved_watchdog_receipt_names_the_whole_guard() {
    let entry: TaskEntry = toml::from_str("agent='codex'\nprompt='fix the failing test'\nevery='15m'\ncheck='cargo test'\ncheck-timeout='10m'\non='fail'\n[judge]\nagent='haiku'\nprompt='Are these failures flakes or already fixed on main?'\n").unwrap();
    let shape = schedule::TaskShape::compile("watchdog", &entry);
    let mut out = Vec::new();
    write_add_feedback(
        &mut out,
        &entry,
        shape.trigger().as_ref().unwrap(),
        shape.action().unwrap(),
        Some("codex"),
        None,
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.lines().any(|line| line == "guard: runs cargo test (cap 10m), then asks haiku (cap 5m); acts when both fail, a judge with no verdict counting as a fail"), "{text}");
}

fn args(extra: &[&str]) -> AddArgs {
    let matches = AddArgs::augment_args(clap::Command::new("add"))
        .try_get_matches_from(["add", "resident"].into_iter().chain(extra.iter().copied()))
        .unwrap();
    AddArgs::from_arg_matches(&matches).unwrap()
}

#[test]
fn resident_subscriptions_reject_one_shot_triggers() {
    for trigger in [["--in", "5m"], ["--at", "12:00"], ["--after-reset", "5h"]] {
        let argv = ["--agent", "claude", "--stay"]
            .into_iter()
            .chain(trigger)
            .collect::<Vec<_>>();
        assert!(validate_add_args(&args(&argv)).is_ok());
        let argv = argv
            .into_iter()
            .chain(["--subscribe", "ci.failed"])
            .collect::<Vec<_>>();
        let error =
            validate_add_args(&args(&argv)).expect_err("one-shot cannot retain subscriptions");
        assert!(error.to_string().contains("--subscribe"), "{error}");
    }
}

#[test]
fn resident_flags_require_their_launch_and_trigger() {
    for (argv, flag) in [
        (vec!["--stay", "--wait"], "--wait"),
        (vec!["--stay", "--check", "true"], "--agent"),
        (
            vec!["--agent", "claude", "--each-worktree", "--when", "pr=open"],
            "--stay",
        ),
        (
            vec![
                "--agent",
                "claude",
                "--stay",
                "--each-worktree",
                "--signal",
                "pr.merged",
            ],
            "--when",
        ),
        (
            vec!["--agent", "claude", "--subscribe", "ci.failed"],
            "--stay",
        ),
        (vec!["--agent", "claude", "--takeover"], "--stay"),
    ] {
        let error = validate_add_args(&args(&argv)).expect_err("invalid resident flags");
        assert!(error.to_string().contains(flag), "{error}");
    }
    let removed = AddArgs::augment_args(clap::Command::new("add"))
        .try_get_matches_from([
            "add",
            "resident",
            "--agent",
            "claude",
            "--stay",
            "--stop-team",
        ])
        .expect_err("--stop-team is gone");
    assert_eq!(removed.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn throttle_flag_takes_on_or_off_and_needs_an_agent() {
    let parse = |flags: &[&str]| {
        AddArgs::augment_args(clap::Command::new("add")).try_get_matches_from(
            ["add", "nightly", "--every", "1h"]
                .into_iter()
                .chain(flags.iter().copied()),
        )
    };
    assert_eq!(
        throttle_switch(&args(&["--agent", "claude", "--throttle", "off"])),
        Some(rimz::config::ThrottleSwitch::Off)
    );
    assert_eq!(
        throttle_switch(&args(&["--agent", "claude", "--throttle", "on"])),
        Some(rimz::config::ThrottleSwitch::On)
    );
    assert_eq!(throttle_switch(&args(&["--agent", "claude"])), None);
    parse(&["--agent", "claude", "--throttle", "maybe"]).expect_err("only on and off");
    parse(&["--check", "true", "--throttle", "off"]).expect_err("needs an agent task");
}

#[test]
fn resident_launch_rejects_supervised_and_one_shot_options() {
    for flags in [
        vec!["--verify", "true"],
        vec!["--max-attempts", "2", "--verify", "true"],
        vec!["--budget", "$2"],
        vec!["--budget-per-day", "$4", "--budget", "$2"],
        vec!["--surplus", "1.5x"],
        vec!["--surplus-after", "3d"],
        vec!["--timeout", "1h"],
        vec!["--system-prompt-file", "/prompt"],
        vec!["--worktree", "lane"],
        vec!["--once"],
        vec!["--project"],
    ] {
        let argv = ["--agent", "claude", "--stay", "--when", "pr=open"]
            .into_iter()
            .chain(flags.iter().copied())
            .collect::<Vec<_>>();
        let error = validate_add_args(&args(&argv)).expect_err("unsupported resident option");
        assert!(error.to_string().contains(flags[0]), "{error}");
        assert!(error.to_string().contains("--stay"), "{error}");
    }
    let error = validate_add_args(&args(&[
        "--agent", "claude", "--stay", "--every", "1m", "--until", "1h", "--check", "true",
    ]))
    .expect_err("resident poll-until must be refused");
    assert!(
        error.to_string().contains("--stay") && error.to_string().contains("--until"),
        "{error}"
    );
}

#[test]
fn agent_check_flags_require_a_profile_prompt_and_action() {
    for (flags, message) in [
        (vec!["--judge", "haiku"], "--judge-prompt"),
        (
            vec![
                "--judge",
                "haiku",
                "--judge-prompt",
                "q",
                "--judge-prompt-file",
                "q.md",
            ],
            "exactly one",
        ),
        (vec!["--judge-prompt", "q"], "--judge"),
        (vec!["--judge-prompt-file", "q.md"], "--judge"),
        (vec!["--judge-recheck", "6h"], "--judge"),
        (vec!["--judge-timeout", "5m"], "--judge"),
        (
            vec![
                "--judge",
                "haiku",
                "--judge-prompt",
                "q",
                "--judge-recheck",
                "6h",
                "--on",
                "success",
            ],
            "--stay",
        ),
    ] {
        let argv = ["--agent", "claude"]
            .into_iter()
            .chain(flags)
            .collect::<Vec<_>>();
        let result = validate_add_args(&args(&argv));
        assert!(
            result.is_err(),
            "invalid judge flags must be rejected: {argv:?}"
        );
        assert!(
            result.unwrap_err().to_string().contains(message),
            "{argv:?}"
        );
    }
    let result = validate_add_args(&args(&["--judge", "haiku", "--judge-prompt", "q"]));
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("--agent or --wait")
    );
}

#[test]
fn resident_checks_and_agent_check_polarity_are_accepted() {
    for flags in [
        vec![
            "--until",
            "1h",
            "--every",
            "1m",
            "--judge",
            "haiku",
            "--judge-prompt",
            "q",
            "--on",
            "fail",
        ],
        vec!["--stay", "--check", "true", "--on", "success"],
        vec![
            "--stay",
            "--judge",
            "haiku",
            "--judge-prompt",
            "q",
            "--judge-recheck",
            "6h",
            "--judge-timeout",
            "5m",
            "--on",
            "success",
        ],
    ] {
        let argv = ["--agent", "claude"]
            .into_iter()
            .chain(flags)
            .collect::<Vec<_>>();
        assert!(validate_add_args(&args(&argv)).is_ok(), "{argv:?}");
    }
    assert!(
        validate_add_args(&args(&[
            "--wait",
            "--judge",
            "haiku",
            "--judge-prompt-file",
            "q.md",
            "--on",
            "any"
        ]))
        .is_ok()
    );
}

#[test]
fn zero_recheck_reasks_without_accepting_a_zero_timeout() {
    let flags = [
        "--agent",
        "claude",
        "--stay",
        "--judge",
        "haiku",
        "--judge-prompt",
        "q",
        "--judge-recheck",
        "0",
        "--on",
        "success",
    ];
    assert!(
        validate_add_args(&args(&flags)).is_ok(),
        "zero recheck must be accepted"
    );
    let timeout = [
        "--agent",
        "claude",
        "--stay",
        "--judge",
        "haiku",
        "--judge-prompt",
        "q",
        "--judge-timeout",
        "0",
        "--on",
        "success",
    ];
    assert!(
        validate_add_args(&args(&timeout)).is_err(),
        "zero timeout must remain refused"
    );
}
