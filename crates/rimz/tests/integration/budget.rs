//! CLI coverage for room-fleet and provider-account daily dollar caps.

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;

use crate::common::Env;
use rimz::ids::{AgentKind, LoginKey};

#[test]
fn budget_set_raise_clear_and_config_routes() {
    let env = Env::new();
    env.rimz().args(["config", "init"]).assert().success();
    env.rimz()
        .args(["config", "set", "harness.budget", "50/day"])
        .assert()
        .success();
    env.rimz()
        .args(["config", "set", "accounts.budget.claude", "100/day"])
        .assert()
        .success();
    env.rimz()
        .args([
            "config",
            "set",
            "accounts.claude.work.home",
            "/srv/budget-test-work",
        ])
        .assert()
        .success();
    let inspected = env.rimz().arg("budget").assert().success();
    let output = String::from_utf8_lossy(&inspected.get_output().stdout);
    let table_rows = output
        .lines()
        .filter(|line| line.starts_with("claude@"))
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(
        table_rows,
        vec![
            vec!["claude@default", "$100.00/day", "config", "$0.00", "no"],
            vec!["claude@work", "$100.00/day", "config", "$0.00", "no"],
        ]
    );

    env.rimz()
        .args(["budget", "20/day", "--no-continue"])
        .assert()
        .success()
        .stdout(contains("cap:    $20.00/day"))
        .stdout(contains("source: override"));
    env.rimz()
        .args(["budget", "+10", "--no-continue"])
        .assert()
        .success()
        .stdout(contains("cap:    $30.00/day"))
        .stdout(contains("source: raised"));
    env.rimz()
        .args(["budget", "--account", "claude", "80/day", "--no-continue"])
        .assert()
        .success()
        .stdout(contains("scope:  claude@default account"))
        .stdout(contains("cap:    $80.00/day"));
    env.rimz()
        .args(["budget", "--account", "claude", "clear", "--no-continue"])
        .assert()
        .success()
        .stdout(contains("source: cleared"));

    env.rimz()
        .args(["budget", "5", "--no-continue"])
        .assert()
        .failure()
        .stderr(contains("must end in `/day`"))
        .stderr(contains("`off` to disable"));
    env.rimz()
        .args(["budget", "off", "--no-continue"])
        .assert()
        .success()
        .stdout(contains("source: cleared"));
}

#[test]
fn budget_ignores_ineligible_siblings_and_warns_only_for_fleet_reports() {
    let env = Env::new();
    std::fs::create_dir_all(env.rimz_home()).expect("config dir");
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[harness]\nbudget = '50/day'\n[accounts.budget]\nclaude = '100/day'\nantigravity = '50/day'\nunknown = '10/day'\n",
    )
    .expect("config");

    for args in [
        vec!["budget", "--account", "claude"],
        vec!["budget", "--account", "claude", "80/day", "--no-continue"],
    ] {
        let inspected = env.rimz().args(args).assert().success().stderr("");
        let output = String::from_utf8_lossy(&inspected.get_output().stdout);
        assert!(output.contains("scope:  claude@default account"));
        assert!(!output.contains("antigravity"));
        assert!(!output.contains("unknown"));
    }

    for args in [vec!["budget"], vec!["budget", "20/day", "--no-continue"]] {
        let inspected = env.rimz().args(&args).assert().success();
        let output = String::from_utf8_lossy(&inspected.get_output().stdout);
        let rows = output
            .lines()
            .skip_while(|line| !line.starts_with("ACCOUNT"))
            .skip(1)
            .map(|line| line.split_whitespace().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            vec![vec![
                "claude@default",
                "$80.00/day",
                "raised",
                "$0.00",
                "no"
            ]]
        );
        if args.len() > 1 {
            assert!(output.contains("cap:    $20.00/day"));
            assert!(output.contains("source: override"));
        }
        assert_eq!(
            String::from_utf8_lossy(&inspected.get_output().stderr),
            "rimz: warning: unsupported `accounts.budget.antigravity`; remove it because antigravity has no durable account-spend source with authoritative account-level dollars\nrimz: warning: unknown agent kind in `accounts.budget.unknown`; remove it because no adapter can publish authoritative account-level dollars\n"
        );
    }
}

#[test]
fn budget_refuses_to_arm_unconfigured_daily_caps() {
    let env = Env::new();

    env.rimz()
        .args(["budget", "20/day", "--no-continue"])
        .assert()
        .failure()
        .stderr(contains(
            "turn it on with `rimz config set harness.budget 50/day`",
        ));
    env.rimz()
        .args(["budget", "--account", "claude", "100/day", "--no-continue"])
        .assert()
        .failure()
        .stderr(contains(
            "turn it on with `rimz config set accounts.budget.claude 100/day`",
        ));
}

#[test]
fn unsupported_account_caps_leave_config_and_ledger_untouched() {
    let env = Env::new();
    env.rimz().args(["config", "init"]).assert().success();
    let config_path = env.rimz_home().join("config.toml");
    let before = std::fs::read(&config_path).expect("read generated config");

    env.rimz()
        .args(["config", "set", "accounts.budget.cursor", "100/day"])
        .assert()
        .failure()
        .stderr(contains("no durable account-spend source"));
    assert_eq!(
        std::fs::read(&config_path).expect("read rejected config"),
        before
    );

    let cursor = AgentKind::new_unchecked("cursor");
    let ledger = rimz::harness::budget::DailyBudgetScope::Account(LoginKey::default_for(cursor))
        .ledger_path(&env.runtime_paths(), env.store().paths());
    env.rimz()
        .args(["budget", "--account", "cursor", "100/day", "--no-continue"])
        .assert()
        .failure()
        .stderr(contains("no durable account-spend source"));
    assert!(
        !ledger.exists(),
        "unsupported account must not create a ledger"
    );
}

#[test]
fn config_set_rejects_unsupported_account_budget_without_writing() {
    let env = Env::new();
    env.rimz().args(["config", "init"]).assert().success();
    let path = env.rimz_home().join("config.toml");
    let before = std::fs::read_to_string(&path).expect("config");

    env.rimz()
        .args(["config", "set", "accounts.budget.antigravity", "50/day"])
        .assert()
        .failure()
        .stderr(contains("accounts.budget.antigravity"))
        .stderr(contains("authoritative account-level dollars"));

    assert_eq!(std::fs::read_to_string(path).expect("config"), before);
}
