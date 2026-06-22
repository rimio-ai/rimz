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
