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
fn repeat_with_every_refuses_and_repeat_needs_a_signal_or_condition() {
    for clock in [["--every", "1h"], ["--cron", "0 * * * *"]] {
        let argv = [&["--repeat"][..], &clock, &["--", "true"]].concat();
        assert_eq!(
            validate_add_args(&args(&argv)).unwrap_err().to_string(),
            "--repeat cannot be used with --every or --cron; they repeat already"
        );
    }
    assert_eq!(
        validate_add_args(&args(&["--repeat", "--in", "5m", "--", "true"]))
            .unwrap_err()
            .to_string(),
        "--repeat requires --signal or --when"
    );
    for trigger in [["--signal", "pr.merged"], ["--when", "pr=open"]] {
        let argv = [&["--repeat"][..], &trigger, &["--", "true"]].concat();
        assert!(validate_add_args(&args(&argv)).is_ok(), "{argv:?}");
    }
}

#[test]
fn removed_flags_and_wait_are_unknown_to_loop_add() {
    for flag in [
        "--wait",
        "--agent",
        "--prompt",
        "--stay",
        "--takeover",
        "--subscribe",
        "--worktree",
        "--check",
        "--judge",
        "--verify",
        "--on",
        "--throttle",
        "--capacity-wait",
        "--budget",
        "--once",
    ] {
        let error = parse(&[flag, "x", "--every", "1h", "--", "true"]).unwrap_err();
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::UnknownArgument,
            "{flag}: {error}"
        );
    }
}

#[test]
fn the_command_is_required_and_kept_word_for_word() {
    let error = parse(&["--every", "1h"]).unwrap_err();
    assert_eq!(
        error.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
    let words = ["sh", "-c", "echo 'a b' \"$HOME\" *.rs", "--every"];
    let argv = [&["nightly", "--every", "1h", "--"][..], &words].concat();
    let parsed = args(&argv);
    assert_eq!(parsed.name.as_deref(), Some("nightly"));
    assert_eq!(parsed.command, words);
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
            command: argv[5..].iter().map(|word| (*word).to_owned()).collect(),
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
fn project_rows_must_repeat_and_account_needs_a_provider() {
    for argv in [
        vec!["--project", "--in", "5m", "--", "true"],
        vec!["--project", "--signal", "pr.merged", "--", "true"],
    ] {
        assert_eq!(
            validate_add_args(&args(&argv)).unwrap_err().to_string(),
            "--project tasks must repeat; set --every, --cron, or --signal with --repeat",
            "{argv:?}"
        );
    }
    assert!(
        validate_add_args(&args(&[
            "--project",
            "--signal",
            "pr.merged",
            "--repeat",
            "--",
            "true"
        ]))
        .is_ok()
    );
    assert_eq!(
        validate_add_args(&args(&["--every", "1h", "--account", "work", "--", "true"]))
            .unwrap_err()
            .to_string(),
        "--account requires --provider; it picks the account whose windows a condition reads"
    );
}

#[test]
fn fresh_worktree_takes_a_name_prefix_and_receipts_name_the_fire_tree() {
    let root = tempfile::tempdir().unwrap();
    let workspace = rimz::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let argv = ["--every", "1h", "--fresh-worktree", "nightly", "--", "true"];
    let entry = build_task_entry(&args(&argv), "task", &workspace).unwrap();
    assert_eq!(entry.fresh_worktree.as_deref(), Some("nightly"));
    assert_eq!(entry.dir, None);
    for prefix in ["", "v1.2"] {
        let bad = args(&["--every", "1h", "--fresh-worktree", prefix, "--", "true"]);
        assert_eq!(
            validate_add_args(&bad).unwrap_err().to_string(),
            format!(
                "--fresh-worktree `{prefix}` is not a worktree name; use letters, digits, `-` and `_`, with `/` between segments"
            )
        );
    }

    for (toml, expected) in [
        (
            "command=['true']\ntimeout='2h'\nevery='1h'\nfresh-worktree='nightly'",
            "action: runs `true` in a fresh worktree `nightly-<time>` per fire, branched from trunk · timeout 2h",
        ),
        (
            "command=['cargo', 'test']\ntimeout='10m'\nevery='1h'\nroot='/repo'",
            "action: runs `cargo test` in /repo · timeout 10m",
        ),
        (
            "command=['true']\ntimeout='1h'\nevery='1h'\neach-worktree=true",
            "action: runs `true` in each RimZ-owned worktree where the trigger holds · timeout 1h",
        ),
        (
            "command=['true']\ntimeout='1h'\nsignal='deploy.done'\nrepeat=true",
            "trigger: fires on deploy.done; repeats after each fire",
        ),
    ] {
        let entry: TaskEntry = toml::from_str(toml).unwrap();
        let shape = schedule::TaskShape::compile("task", &entry);
        let mut out = Vec::new();
        write_add_feedback(&mut out, &entry, shape.trigger().as_ref().unwrap(), None).unwrap();
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
        &["--every", "1h"],
        &["--cron", "0 * * * *"],
    ] {
        let argv = [&["--each-worktree"][..], trigger, &["--", "true"]].concat();
        assert!(validate_add_args(&args(&argv)).is_ok(), "{argv:?}");
    }
}
