use super::*;

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn no_args_default_to_ci() {
    assert_eq!(
        parse_args(&args(&[])).unwrap(),
        Action::Run {
            task: "ci",
            args: &[],
        },
    );
}

#[test]
fn root_help_is_first_class() {
    assert_eq!(parse_args(&args(&["--help"])).unwrap(), Action::Help(None));
    assert_eq!(parse_args(&args(&["-h"])).unwrap(), Action::Help(None));
    assert_eq!(parse_args(&args(&["help"])).unwrap(), Action::Help(None));
}

#[test]
fn task_help_does_not_run_the_task() {
    assert_eq!(
        parse_args(&args(&["test", "--help"])).unwrap(),
        Action::Help(Some("test")),
    );
    assert_eq!(
        parse_args(&args(&["help", "test"])).unwrap(),
        Action::Help(Some("test")),
    );
}

#[test]
fn unexpected_task_args_fail_instead_of_being_ignored() {
    let err = parse_args(&args(&["lint", "--package", "rimz"]))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("xtask `lint` takes no arguments"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_forwards_filter_args() {
    let argv = args(&["test", "auth"]);

    assert_eq!(
        parse_args(&argv).unwrap(),
        Action::Run {
            task: "test",
            args: &argv[1..],
        },
    );
}

#[test]
fn test_forwards_exact_names_and_list_requests() {
    for argv in [
        args(&["test", "--name", "leaf", "--name=tests::full"]),
        args(&["test", "--list", "auth"]),
    ] {
        assert_eq!(
            parse_args(&argv).unwrap(),
            Action::Run {
                task: "test",
                args: &argv[1..],
            },
        );
    }
}

#[test]
fn test_archive_forwards_archive_args() {
    let argv = args(&[
        "test-archive",
        "--archive-file",
        "target/ci/archive.tar.zst",
    ]);

    assert_eq!(
        parse_args(&argv).unwrap(),
        Action::Run {
            task: "test-archive",
            args: &argv[1..],
        },
    );
}

#[test]
fn sandbox_forwards_the_command() {
    let argv = args(&["sandbox", "--", "target/debug/rimz", "--zellij", "doctor"]);

    assert_eq!(
        parse_args(&argv).unwrap(),
        Action::Run {
            task: "sandbox",
            args: &argv[1..],
        },
    );
}

#[test]
fn screenshot_accepts_subcommands() {
    let argv = args(&["screenshot", "state", "fleet"]);

    assert_eq!(
        parse_args(&argv).unwrap(),
        Action::Run {
            task: "screenshot",
            args: &argv[1..],
        },
    );
}

#[test]
fn screenshot_subcommand_help_reaches_the_task_parser() {
    let argv = args(&["screenshot", "state", "--help"]);

    assert_eq!(
        parse_args(&argv).unwrap(),
        Action::Run {
            task: "screenshot",
            args: &argv[1..],
        },
    );
}

#[test]
fn atlas_accepts_verbs_and_verb_help() {
    for argv in [
        args(&["atlas", "rank"]),
        args(&["atlas", "conform", "--ratchet"]),
        args(&["atlas", "shapes", "--help"]),
        args(&["atlas", "survey", "--help"]),
        args(&["atlas", "brief", "--help"]),
    ] {
        assert_eq!(
            parse_args(&argv).unwrap(),
            Action::Run {
                task: "atlas",
                args: &argv[1..],
            },
        );
    }
}

#[test]
fn gate_accepts_the_read_only_check_flag() {
    let argv = args(&["gate", "--check"]);

    assert_eq!(
        parse_args(&argv).unwrap(),
        Action::Run {
            task: "gate",
            args: &argv[1..],
        },
    );
    assert_eq!(
        parse_args(&args(&["gate"])).unwrap(),
        Action::Run {
            task: "gate",
            args: &[],
        },
    );
}

#[test]
fn every_quiet_pass_task_is_a_real_task() {
    for task in QUIET_PASS_TASKS {
        assert!(task_info(task).is_some(), "unknown quiet-pass task: {task}");
    }
}

#[test]
fn profile_build_is_a_first_class_no_arg_task() {
    assert!(task_info("profile-build").is_some());
    assert_eq!(
        parse_args(&args(&["profile-build"])).unwrap(),
        Action::Run {
            task: "profile-build",
            args: &[],
        },
    );
}

#[test]
fn check_is_a_first_class_no_arg_task() {
    assert!(task_info("check").is_some());
    assert_eq!(
        parse_args(&args(&["check"])).unwrap(),
        Action::Run {
            task: "check",
            args: &[],
        },
    );
    assert!(
        parse_args(&args(&["check", "--package", "rimz"]))
            .unwrap_err()
            .to_string()
            .contains("takes no arguments")
    );
}
