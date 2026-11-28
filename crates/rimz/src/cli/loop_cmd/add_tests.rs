use super::*;
use clap::FromArgMatches;

fn parse(argv: &[&str]) -> Result<AddArgs, clap::Error> {
    let matches = AddArgs::augment_args(clap::Command::new("add"))
        .try_get_matches_from(["add"].into_iter().chain(argv.iter().copied()))?;
    AddArgs::from_arg_matches(&matches)
}

fn args(argv: &[&str]) -> AddArgs {
    parse(argv).unwrap()
}

#[test]
fn repeat_with_cron_refuses_and_repeat_needs_a_condition() {
    assert_eq!(
        validate_add_args(&args(&["--repeat", "--cron", "0 * * * *", "--", "true"]))
            .unwrap_err()
            .to_string(),
        "--repeat cannot be used with --cron; it repeats already"
    );
    assert_eq!(
        validate_add_args(&args(&["--repeat", "--at", "5m", "--", "true"]))
            .unwrap_err()
            .to_string(),
        "--repeat requires --when"
    );
    let argv = ["--repeat", "--when", "pr=open", "--", "true"];
    assert!(validate_add_args(&args(&argv)).is_ok(), "{argv:?}");
}

#[test]
fn at_and_cron_conflict_in_either_order() {
    for argv in [
        ["--at", "09:00", "--cron", "0 * * * *"],
        ["--cron", "0 * * * *", "--at", "09:00"],
    ] {
        let error = parse(&[&argv[..], &["--", "true"]].concat()).unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "{argv:?}: {error}"
        );
    }
}

#[test]
fn removed_flags_and_wait_are_unknown_to_loop_add() {
    for flag in [
        "--wait",
        "--prompt",
        "--stay",
        "--takeover",
        "--subscribe",
        "--check",
        "--judge",
        "--verify",
        "--on",
        "--throttle",
        "--capacity-wait",
        "--budget",
        "--once",
    ] {
        let error = parse(&[flag, "x", "--cron", "0 * * * *", "--", "true"]).unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::UnknownArgument,
            "{flag}: {error}"
        );
    }
}

#[test]
fn the_command_is_required_and_kept_word_for_word() {
    assert_eq!(
        validate_add_args(&args(&["--cron", "0 * * * *"]))
            .unwrap_err()
            .to_string(),
        "a shell task needs its command after `--`: `rimz loop add <name> ... -- <command>`"
    );
    let words = ["sh", "-c", "echo 'a b' \"$HOME\" *.rs", "--cron"];
    let argv = [&["nightly", "--cron", "0 * * * *", "--"][..], &words].concat();
    let parsed = args(&argv);
    assert_eq!(parsed.name.as_deref(), Some("nightly"));
    assert_eq!(parsed.args, words);
}

#[test]
fn an_added_row_stores_the_trigger_command_timeout_and_root_only() {
    let root = tempfile::tempdir().unwrap();
    let workspace = rimz::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let argv = [
        "--when",
        "pr[x]=merged",
        "--timeout",
        "45m",
        "--",
        "rimz",
        "teams",
        "launch",
        "forge",
        "-w",
        "y",
        "--detach",
        "p",
    ];
    let entry = build_task_entry(&args(&argv), "loop-test", &workspace).unwrap();
    assert_eq!(
        entry,
        TaskEntry {
            root: workspace.project_root.clone(),
            when: Some(vec!["pr[x]=merged".into()]),
            kind: Some(rimz::config::LaunchKind::Shell),
            args: argv[5..].iter().map(|word| (*word).to_owned()).collect(),
            timeout: Some("45m".into()),
            ..TaskEntry::default()
        }
    );
    assert_eq!(
        task_timeout(Some("0s")).unwrap_err().to_string(),
        "--timeout must be greater than zero"
    );
}

#[test]
fn once_per_checkout_is_stored_on_a_repeating_row_and_refused_on_a_one_shot() {
    let root = tempfile::tempdir().unwrap();
    let workspace = rimz::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let entry = |trigger: &[&str]| {
        let argv = [trigger, &["--once-per-checkout", "--", "true"]].concat();
        build_task_entry(&args(&argv), "task", &workspace)
    };
    for trigger in [
        &["--cron", "0 * * * *"][..],
        &["--when", "pr=open", "--repeat"],
        &["--project", "--cron", "0 * * * *"],
    ] {
        assert_eq!(
            entry(trigger).unwrap().once_per_checkout,
            Some(true),
            "{trigger:?}"
        );
    }
    for trigger in [
        &["--at", "5m"][..],
        &["--at", "2099-01-01 09:00"],
        &["--when", "pr=open"],
    ] {
        assert_eq!(
            entry(trigger).unwrap_err().to_string(),
            "--once-per-checkout cannot be used on a one-shot: its first fire removes the task, \
             so launch memory never acts; make it repeat with --cron, or --repeat beside --when",
            "{trigger:?}"
        );
    }
    let plain = build_task_entry(
        &args(&["--cron", "0 * * * *", "--", "true"]),
        "task",
        &workspace,
    );
    assert_eq!(plain.unwrap().once_per_checkout, None);
}

#[test]
fn project_rows_must_repeat() {
    assert_eq!(
        validate_add_args(&args(&["--project", "--at", "5m", "--", "true"]))
            .unwrap_err()
            .to_string(),
        "--project tasks must repeat; set --cron",
    );
    assert!(validate_add_args(&args(&["--project", "--cron", "0 * * * *", "--", "true"])).is_ok());
}

#[test]
fn receipts_name_where_each_fire_runs() {
    for (toml, expected) in [
        (
            "args=['cargo', 'test']\ntimeout='10m'\ncron='0 * * * *'\nroot='/repo'",
            "action: runs `cargo test` in /repo · timeout 10m",
        ),
        (
            "args=['true']\ntimeout='1h'\ncron='0 * * * *'\neach-worktree=true",
            "action: runs `true` in each RimZ-owned worktree where the trigger holds · timeout 1h",
        ),
    ] {
        let entry: TaskEntry = toml::from_str(toml).unwrap();
        let shape = schedule::TaskShape::compile("task", &entry);
        let mut out = Vec::new();
        write_add_feedback(
            &mut out,
            &entry,
            shape.action().unwrap(),
            &[],
            shape.trigger().as_ref().unwrap(),
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.lines().any(|line| line == expected), "{text}");
    }
}

#[test]
fn each_worktree_with_when_needs_repeat() {
    assert_eq!(
        validate_add_args(&args(&[
            "--each-worktree",
            "--when",
            "pr=merged",
            "--",
            "true"
        ]))
        .unwrap_err()
        .to_string(),
        "--each-worktree with --when needs --repeat; a one-shot would remove the task after its \
         first checkout"
    );
    for trigger in [
        &["--when", "pr=merged", "--repeat"][..],
        &["--cron", "0 * * * *"],
    ] {
        let argv = [&["--each-worktree"][..], trigger, &["--", "true"]].concat();
        assert!(validate_add_args(&args(&argv)).is_ok(), "{argv:?}");
    }
}

/// `-w` takes the name a launch's own `-w` takes, refuses `--each-worktree` for every kind,
/// and is stored as the row's `worktree`.
#[test]
fn the_loops_worktree_is_validated_like_a_launchs_and_stored_on_the_row() {
    assert_eq!(
        validate_add_args(&args(&["-w", "a b", "--cron", "0 * * * *", "--", "true"]))
            .unwrap_err()
            .to_string(),
        "invalid worktree name `a b`; use letters, numbers, `_`, `-`, with `/` separating \
         branch-style segments; omit the channel's leading `#` (use `feat-a` for `#feat-a`)"
    );
    for kind in [&[][..], &["--agent", "worker"][..]] {
        let argv = [
            &["-w", "deps", "--each-worktree", "--cron", "0 * * * *"][..],
            kind,
            &["--", "true"],
        ]
        .concat();
        assert_eq!(
            validate_add_args(&args(&argv)).unwrap_err().to_string(),
            "-w places every fire, and --each-worktree places each fire; remove one",
            "{argv:?}"
        );
    }
    let root = tempfile::tempdir().unwrap();
    let workspace = rimz::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let entry = build_task_entry(
        &args(&["--worktree", "feat/x", "--cron", "0 * * * *", "--", "true"]),
        "t",
        &workspace,
    )
    .unwrap();
    assert_eq!(entry.worktree.as_deref(), Some("feat/x"));
}

fn launch(worktree: Option<&str>, prompt: bool, print: Option<Option<u64>>) -> LoopLaunch {
    LoopLaunch {
        worktree: worktree.map(str::to_owned),
        prompt,
        print: print.map(|limit| limit.map(std::time::Duration::from_secs)),
        ..LoopLaunch::default()
    }
}

/// The receipt reads the launch back: the worktree its arguments name, whether a prompt was
/// found, and a `-p` run's time limit; a workflow names only its worktree, a shell nothing.
#[test]
fn the_launch_line_reads_the_parsed_launch_back() {
    use rimz::config::LaunchKind::{Agent, Shell, Team, Workflow};
    for (kind, parsed, expected) in [
        (
            Agent,
            launch(Some("one-word"), false, None),
            Some("launch: worktree `one-word` · no prompt"),
        ),
        (
            Agent,
            launch(None, true, Some(Some(1_800))),
            Some("launch: prompt found · time limit 30m"),
        ),
        (
            Agent,
            launch(Some(""), true, Some(None)),
            Some("launch: a new worktree each fire · prompt found · time limit none"),
        ),
        (
            Team,
            launch(Some("deps"), true, None),
            Some("launch: worktree `deps` · prompt found"),
        ),
        (
            Workflow,
            launch(Some("deps"), false, None),
            Some("launch: worktree `deps`"),
        ),
        (Workflow, launch(None, false, None), None),
        (Shell, launch(Some("deps"), false, None), None),
    ] {
        assert_eq!(
            launch_line(kind, &parsed).as_deref(),
            expected,
            "{parsed:?}"
        );
    }
}

/// The idle note is owed by a repeating agent or team task that is not a `-p` run and chose
/// no overlap policy; only an agent task's names `--overlap allow`, which a team task refuses.
#[test]
fn the_idle_note_is_owed_only_where_an_idle_agent_holds_later_fires() {
    for (argv, parsed, owed) in [
        (
            &["--agent", "w", "--cron", "0 * * * *"][..],
            launch(None, true, None),
            Some(AGENT_IDLE_NOTE),
        ),
        (
            &["--team", "t", "--cron", "0 * * * *"],
            launch(None, true, None),
            Some(TEAM_IDLE_NOTE),
        ),
        (
            &["--agent", "w", "--when", "pr=open", "--repeat"],
            launch(None, true, None),
            Some(AGENT_IDLE_NOTE),
        ),
        (
            &["--agent", "w", "--cron", "0 * * * *"],
            launch(None, true, Some(None)),
            None,
        ),
        (
            &["--agent", "w", "--cron", "0 * * * *", "--overlap", "skip"],
            launch(None, true, None),
            None,
        ),
        (
            &["--agent", "w", "--cron", "0 * * * *", "--overlap", "allow"],
            launch(None, true, None),
            None,
        ),
        (
            &["--agent", "w", "--cron", "0 * * * *", "--once-per-checkout"],
            launch(None, true, None),
            None,
        ),
        (
            &["--team", "t", "--cron", "0 * * * *", "--once-per-checkout"],
            launch(None, true, None),
            None,
        ),
        (
            &["--agent", "w", "--at", "5m"],
            launch(None, true, None),
            None,
        ),
        (
            &["--workflow", "w", "--cron", "0 * * * *"],
            launch(None, false, None),
            None,
        ),
        (
            &["--cron", "0 * * * *", "--", "true"],
            launch(None, false, None),
            None,
        ),
    ] {
        assert_eq!(idle_note(&args(argv), &parsed), owed, "{argv:?}");
    }
    assert_eq!(
        AGENT_IDLE_NOTE,
        "note: an agent left idle holds later fires; add `-p` for a run that ends itself, \
         `--overlap replace` to stop the old agent at each fire, or `--overlap allow` to launch \
         beside it"
    );
    assert_eq!(
        TEAM_IDLE_NOTE,
        "note: an agent left idle holds later fires; add `-p` for a run that ends itself, or \
         `--overlap replace` to stop the old agent at each fire"
    );
}

/// An agent task's window terms read the kind its launch runs on and the tail's `--account`
/// when the loop names neither; a loop flag always wins, several kinds refuse, and a task
/// whose provider stays unresolved refuses `--account` as before.
#[test]
fn window_terms_default_to_the_launchs_kind_and_account() {
    use rimz::config::LaunchKind::{Agent, Shell, Team};
    let parsed = |kinds: &[&str], account: Option<&str>| LoopLaunch {
        kinds: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
        account: account.map(|name| name.parse().unwrap()),
        ..LoopLaunch::default()
    };
    let name = |raw: &str| Some(raw.parse::<rimz::ids::LoginName>().unwrap());
    let window = ["--when", "window.5h.left>=40"];
    let source = |argv: &[&str], kind, launch: &LoopLaunch| {
        window_source(&args(&[argv, &["--", "go"]].concat()), kind, launch)
    };
    assert_eq!(
        source(&window, Agent, &parsed(&["claude"], Some("work"))).unwrap(),
        WindowSource {
            provider: Some("claude".into()),
            account: name("work"),
            provider_from_launch: true,
            account_from_launch: true,
        }
    );
    assert_eq!(
        source(
            &["--when", "window.7d.left>=80"],
            Agent,
            &parsed(&["claude"], None)
        )
        .unwrap(),
        WindowSource {
            provider: Some("claude".into()),
            provider_from_launch: true,
            ..WindowSource::default()
        }
    );
    assert_eq!(
        source(
            &[&window[..], &["--account", "other"]].concat(),
            Agent,
            &parsed(&["claude"], Some("work"))
        )
        .unwrap(),
        WindowSource {
            provider: Some("claude".into()),
            account: name("other"),
            provider_from_launch: true,
            account_from_launch: false,
        }
    );
    assert_eq!(
        source(
            &[&window[..], &["--provider", "codex"]].concat(),
            Agent,
            &parsed(&["claude"], Some("work"))
        )
        .unwrap(),
        WindowSource {
            provider: Some("codex".into()),
            ..WindowSource::default()
        }
    );
    assert_eq!(
        source(
            &[&window[..], &["--provider", "claude"]].concat(),
            Agent,
            &parsed(&["claude"], Some("work"))
        )
        .unwrap(),
        WindowSource {
            provider: Some("claude".into()),
            account: name("work"),
            provider_from_launch: false,
            account_from_launch: true,
        }
    );
    assert_eq!(
        source(
            &["--cron", "0 * * * *"],
            Agent,
            &parsed(&["claude"], Some("work"))
        )
        .unwrap(),
        WindowSource::default()
    );
    assert_eq!(
        source(&window, Team, &parsed(&[], None)).unwrap(),
        WindowSource::default()
    );
    assert_eq!(
        source(&window, Agent, &parsed(&["claude", "codex"], None))
            .unwrap_err()
            .to_string(),
        "the launch runs claude and codex; name the provider whose windows to read with \
         --provider <kind>"
    );
    for kind in [Shell, Agent] {
        assert_eq!(
            source(
                &["--cron", "0 * * * *", "--account", "work"],
                kind,
                &parsed(&["claude"], None)
            )
            .unwrap_err()
            .to_string(),
            "--account requires --provider; it picks the account whose windows a condition reads"
        );
    }
}

/// The receipt says where a defaulted window reading comes from, and nothing when the loop
/// named both.
#[test]
fn the_window_line_names_what_came_from_the_launch() {
    let line = |provider_from_launch, account: Option<&str>, account_from_launch| {
        window_source_line(&WindowSource {
            provider: Some("claude".into()),
            account: account.map(|name| name.parse().unwrap()),
            provider_from_launch,
            account_from_launch,
        })
    };
    assert_eq!(
        line(true, Some("work"), true).as_deref(),
        Some("windows: claude (from the launch), account `work` (from the launch)")
    );
    assert_eq!(
        line(true, None, false).as_deref(),
        Some("windows: claude (from the launch), the room's account")
    );
    assert_eq!(
        line(false, Some("work"), true).as_deref(),
        Some("windows: claude, account `work` (from the launch)")
    );
    assert_eq!(line(false, Some("work"), false), None);
}

/// A row placed in a worktree names it as the place each fire runs, and the linked-worktree
/// note, printed only for a task its own placement does not move, names `-w`.
#[test]
fn a_placed_row_names_its_worktree_and_the_linked_note_names_w() {
    let entry: TaskEntry = toml::from_str(
        "args=['true']\ntimeout='1h'\ncron='0 * * * *'\nroot='/repo'\nworktree='deps'",
    )
    .unwrap();
    assert_eq!(
        fire_place(&entry),
        "worktree `deps` (created at the first fire if missing)"
    );
    let workspace = rimz::ResolvedWorkspace {
        worktree_root: PathBuf::from("/repo-linked"),
        ..rimz::WorkspaceResolver::resolve(tempfile::tempdir().unwrap().path(), None).unwrap()
    };
    let note = |placed| {
        let mut out = Vec::new();
        write_linked_add_note(&mut out, &workspace, placed).unwrap();
        String::from_utf8(out).unwrap()
    };
    assert_eq!(
        note(false),
        format!(
            "note: runs at the project root {}, not /repo-linked; add -w <name> to run each \
             fire in a RimZ-owned worktree\n",
            workspace.project_root.display()
        )
    );
    assert_eq!(note(true), "");
}
