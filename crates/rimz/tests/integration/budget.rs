//! CLI coverage for room-fleet and provider-account daily dollar caps.

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;

use crate::common::Env;
use rimz::ids::AgentKind;

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
        .stdout(contains("scope:  claude account"))
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
    let config_path = env.config_root().join("rimz/config.toml");
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
    let ledger = rimz::harness::budget::account_ledger_path(&env.runtime_paths(), &cursor);
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
