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

    for (key, value, expected) in [
        ("sidebar.theme.mode", "truecolor", "truecolor\n"),
        ("sidebar.theme.mode", "256", "256\n"),
        ("sidebar.theme.scheme", "slate", "slate\n"),
        ("sidebar.theme.good", "'#a3be8c'", "#a3be8c\n"),
        ("sidebar.theme.caution", "214", "214\n"),
        ("sidebar.providers.claude.color", "'#D97757'", "#d97757\n"),
    ] {
        env.rimz()
            .args(["config", "set", key, value])
            .assert()
            .success()
            .stdout(contains(format!("set {key}")));
        env.rimz()
            .args(["config", "get", key])
            .assert()
            .success()
            .stdout(expected);
    }

    env.rimz()
        .args(["config", "set", "sidebar.theme", "Catppuccin Mocha"])
        .assert()
        .success()
        .stdout(contains("set sidebar.theme"));
    env.rimz()
        .args(["config", "get", "sidebar.theme.scheme"])
        .assert()
        .success()
        .stdout("Catppuccin Mocha\n");

    env.rimz()
        .args(["config", "set", "sidebar.theme", "0x96f"])
        .assert()
        .success()
        .stdout(contains("set sidebar.theme"));
    env.rimz()
        .args(["config", "get", "sidebar.theme.scheme"])
        .assert()
        .success()
        .stdout("0x96f\n");
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

    env.rimz()
        .args(["config", "set", "sidebar.theme.scheme", "does-not-exist"])
        .assert()
        .failure()
        .stderr(contains("unknown sidebar theme scheme `does-not-exist`"));

    env.rimz()
        .args(["config", "set", "sidebar.theme", "auto"])
        .assert()
        .failure()
        .stderr(contains("unknown sidebar theme scheme `auto`"));

    let bad_scheme = env.home_root.join("bad-theme.toml");
    std::fs::write(&bad_scheme, "[colors.primary]\nbackground = 'nothex'\n")
        .expect("write bad scheme");
    env.rimz()
        .args([
            "config",
            "set",
            "sidebar.theme.scheme",
            bad_scheme.to_str().expect("utf-8 path"),
        ])
        .assert()
        .failure()
        .stderr(contains("colors.primary.background"));
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
