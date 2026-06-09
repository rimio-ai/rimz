//! Integration coverage for `rimz config` and the conservative `rimz setup`.

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;

use crate::common::Env;

fn machine_config_path(env: &Env) -> std::path::PathBuf {
    env.config_root().join("rimz").join("config.toml")
}

#[test]
fn config_init_prints_and_writes_the_template() {
    let env = Env::new();

    let expected_path = format!("{}\n", machine_config_path(&env).display());
    env.rimz()
        .args(["config", "path"])
        .assert()
        .success()
        .stdout(expected_path);

    env.rimz()
        .args(["config", "init", "--print"])
        .assert()
        .success()
        .stdout(contains("[worktree]"))
        .stdout(contains("# max_cols = 72"));

    env.rimz()
        .args(["config", "init"])
        .assert()
        .success()
        .stdout(contains("wrote"));

    let path = machine_config_path(&env);
    let text = std::fs::read_to_string(&path).expect("read generated config");
    assert!(text.contains("[notifications]"));
    assert!(text.contains("# enabled = true"));

    env.rimz()
        .args(["config", "init"])
        .assert()
        .failure()
        .stderr(contains("already exists"));

    env.rimz()
        .args(["config", "init", "--force"])
        .assert()
        .success();
}

#[test]
fn config_get_set_round_trip_preserves_template_comments() {
    let env = Env::new();
    env.rimz().args(["config", "init"]).assert().success();

    env.rimz()
        .args(["config", "get", "notifications.triggers"])
        .assert()
        .success()
        .stdout("[\"waiting\", \"failed\", \"paused\", \"success\"]\n");

    env.rimz()
        .args(["config", "set", "sidebar.max_cols", "80"])
        .assert()
        .success()
        .stdout(contains("set sidebar.max_cols"));

    env.rimz()
        .args(["config", "get", "sidebar.max_cols"])
        .assert()
        .success()
        .stdout("80\n");

    let text = std::fs::read_to_string(machine_config_path(&env)).expect("read config");
    assert!(
        text.contains("# max_cols = 72"),
        "set should preserve the commented default:\n{text}"
    );
    assert!(
        text.contains("max_cols = 80"),
        "set should write the override:\n{text}"
    );
}

#[test]
fn config_set_round_trips_arrays_and_context_bands() {
    let env = Env::new();
    env.rimz().args(["config", "init"]).assert().success();

    env.rimz()
        .args([
            "config",
            "set",
            "notifications.triggers",
            "[\"waiting\", \"failed\"]",
        ])
        .assert()
        .success();

    env.rimz()
        .args(["config", "get", "notifications.triggers"])
        .assert()
        .success()
        .stdout("[\"waiting\", \"failed\"]\n");

    env.rimz()
        .args([
            "config",
            "set",
            "sidebar.context.red",
            "{ percent = 90, tokens = 400000 }",
        ])
        .assert()
        .success();

    env.rimz()
        .args(["config", "get", "sidebar.context.red.percent"])
        .assert()
        .success()
        .stdout("90\n");

    env.rimz()
        .args(["config", "get", "sidebar.card_density"])
        .assert()
        .success()
        .stdout("auto\n");

    env.rimz()
        .args(["config", "set", "sidebar.card_density", "compact"])
        .assert()
        .success();

    env.rimz()
        .args(["config", "get", "sidebar.card_density"])
        .assert()
        .success()
        .stdout("compact\n");

    env.rimz()
        .args(["config", "set", "sidebar.card_density", "tiny"])
        .assert()
        .failure()
        .stderr(contains("validating `sidebar.card_density`"));
}

#[test]
fn config_set_rejects_unknown_keys_and_bad_values() {
    let env = Env::new();

    env.rimz()
        .args(["config", "set", "sidebar.nope", "80"])
        .assert()
        .failure()
        .stderr(contains("unknown config key `sidebar.nope`"));

    env.rimz()
        .args(["config", "set", "sidebar.max_cols", "0"])
        .assert()
        .failure()
        .stderr(contains("validating `sidebar.max_cols`"));
}

#[test]
fn config_get_distinguishes_unset_optional_from_unknown_key() {
    let env = Env::new();

    env.rimz()
        .args(["config", "get", "notifications.command"])
        .assert()
        .failure()
        .stderr(contains("config key `notifications.command` is unset"));

    env.rimz()
        .args(["config", "get", "notifications.nope"])
        .assert()
        .failure()
        .stderr(contains("unknown config key `notifications.nope`"));
}

#[test]
fn setup_without_tty_reports_and_writes_nothing() {
    let env = Env::new();

    env.rimz()
        .arg("setup")
        .assert()
        .success()
        .stdout(contains("Rimz setup"))
        .stdout(contains("changed nothing"));

    assert!(!machine_config_path(&env).exists());
}

#[test]
fn setup_yes_writes_default_config_without_hook_or_trust_side_effects() {
    let env = Env::new();

    env.rimz()
        .args(["setup", "--yes"])
        .assert()
        .success()
        .stdout(contains("Wrote"))
        .stdout(contains("No hooks or trust grants were changed"));

    let text = std::fs::read_to_string(machine_config_path(&env)).expect("read setup config");
    assert!(text.contains("[resume]"));
    assert!(text.contains("# on_rebirth = true"));
}
