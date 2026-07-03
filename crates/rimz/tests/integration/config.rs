//! Integration coverage for `rimz config` and the conservative `rimz setup`.

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;

use crate::common::Env;

fn machine_config_path(env: &Env) -> std::path::PathBuf {
    env.config_root().join("rimz").join("config.toml")
}

fn theme_config_path(env: &Env) -> std::path::PathBuf {
    env.config_root().join("rimz").join("theme.toml")
}

fn agents_config_path(env: &Env) -> std::path::PathBuf {
    env.config_root().join("rimz").join("agents.toml")
}

fn loop_config_path(env: &Env) -> std::path::PathBuf {
    env.config_root().join("rimz").join("loop.toml")
}

fn write_machine_file(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("config file parent")).expect("mkdir config");
    std::fs::write(path, text).expect("write config seed");
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
        .stdout(contains("# === config.toml ==="))
        .stdout(contains("# === theme.toml ==="))
        .stdout(contains("# === agents.toml ==="))
        .stdout(contains("# === loop.toml ==="))
        .stdout(contains("[agents.worktree]"))
        .stdout(contains("# [tasks]"))
        .stdout(contains("[theme.display]"));

    env.rimz()
        .args(["config", "init"])
        .assert()
        .success()
        .stdout(contains("wrote"));

    let path = machine_config_path(&env);
    let text = std::fs::read_to_string(&path).expect("read generated config");
    assert!(text.contains("[notifications]"));
    assert!(text.contains("# enabled = true"));
    let theme_text = std::fs::read_to_string(theme_config_path(&env)).expect("read theme config");
    assert!(theme_text.contains("[theme]"));
    assert!(theme_text.contains("## [colors.primary]"));
    let agents_text =
        std::fs::read_to_string(agents_config_path(&env)).expect("read agents config");
    assert!(agents_text.contains("[agents.worktree]"));
    let loop_text = std::fs::read_to_string(loop_config_path(&env)).expect("read loop config");
    assert!(loop_text.contains("# [tasks]"));

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
        .args(["config", "set", "theme.display.max_cols", "80"])
        .assert()
        .success()
        .stdout(contains("set theme.display.max_cols"));

    env.rimz()
        .args(["config", "get", "theme.display.max_cols"])
        .assert()
        .success()
        .stdout("80\n");

    let text = std::fs::read_to_string(theme_config_path(&env)).expect("read theme config");
    assert!(
        text.contains("## max_cols = 72"),
        "set should preserve the commented default:\n{text}"
    );
    assert!(
        text.contains("max_cols = 80"),
        "set should write the override:\n{text}"
    );

    for (key, value, expected) in [
        ("theme.mode", "truecolor", "truecolor\n"),
        ("theme.mode", "256", "256\n"),
        ("theme.scheme", "TokyoNight Night", "TokyoNight Night\n"),
        ("theme.good", "'#a3be8c'", "#a3be8c\n"),
        ("theme.caution", "214", "214\n"),
        ("theme.providers.claude.color", "'#D97757'", "#d97757\n"),
        ("theme.colors.normal.green", "'#00ff00'", "#00ff00\n"),
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

    let theme_text = std::fs::read_to_string(theme_config_path(&env)).expect("read theme config");
    assert!(
        theme_text.contains("[colors.normal]") && theme_text.contains("green = '#00ff00'"),
        "theme.colors writes to root [colors] for Alacritty paste compatibility:\n{theme_text}"
    );

    env.rimz()
        .args(["config", "set", "theme", "Catppuccin Mocha"])
        .assert()
        .success()
        .stdout(contains("set theme"));
    env.rimz()
        .args(["config", "get", "theme.scheme"])
        .assert()
        .success()
        .stdout("Catppuccin Mocha\n");

    env.rimz()
        .args(["config", "set", "theme", "0x96f"])
        .assert()
        .success()
        .stdout(contains("set theme"));
    env.rimz()
        .args(["config", "get", "theme.scheme"])
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
        .args(["config", "set", "theme.display.max_cols", "0"])
        .assert()
        .failure()
        .stderr(contains("validating `theme.display.max_cols`"));

    env.rimz()
        .args(["config", "set", "theme.scheme", "does-not-exist"])
        .assert()
        .failure()
        .stderr(contains("unknown sidebar theme scheme `does-not-exist`"));

    env.rimz()
        .args(["config", "set", "theme", "auto"])
        .assert()
        .failure()
        .stderr(contains("unknown sidebar theme scheme `auto`"));

    env.rimz()
        .args(["config", "set", "harness.smart_compact", "abc"])
        .assert()
        .failure()
        .stderr(contains("invalid auto-compact threshold `abc`"));

    let bad_scheme = env.home_root.join("bad-theme.toml");
    std::fs::write(&bad_scheme, "[colors.primary]\nbackground = 'nothex'\n")
        .expect("write bad scheme");
    env.rimz()
        .args([
            "config",
            "set",
            "theme.scheme",
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
    assert!(theme_config_path(&env).exists());
    assert!(agents_config_path(&env).exists());
    assert!(loop_config_path(&env).exists());
}

#[test]
fn setup_yes_merges_overrides_and_skips_incompatible_keys() {
    let env = Env::new();
    write_machine_file(
        &machine_config_path(&env),
        r#"
[notifications]
enabled = false
bogus_key = 1

[zellij]
on_force_close = "explode"
"#,
    );

    env.rimz()
        .args(["setup", "--yes"])
        .assert()
        .success()
        .stdout(contains("Merged"))
        .stdout(contains("kept 1 setting(s)"))
        .stdout(contains(
            "skipped notifications.bogus_key (invalid: unknown config key `notifications.bogus_key`)",
        ))
        .stdout(contains("skipped zellij.on_force_close (invalid:"))
        .stdout(contains("Wrote"))
        .stdout(contains("No hooks or trust grants were changed"));

    let text = std::fs::read_to_string(machine_config_path(&env)).expect("read merged config");
    assert!(text.contains("enabled = false"), "override kept:\n{text}");
    assert!(
        text.contains("# on_rebirth = true"),
        "template comments kept:\n{text}"
    );
    assert!(
        !text.contains("bogus_key"),
        "unknown key should be dropped:\n{text}"
    );
    assert!(
        !text.contains("on_force_close = \"explode\""),
        "invalid key should be dropped:\n{text}"
    );
    assert!(theme_config_path(&env).exists());
    assert!(agents_config_path(&env).exists());
    assert!(loop_config_path(&env).exists());
}

#[test]
fn setup_yes_preserves_sentry_keys_during_merge() {
    let env = Env::new();
    write_machine_file(
        &machine_config_path(&env),
        r#"
[sentry]
dsn = "https://k@o0.ingest.sentry.io/0"
"#,
    );

    env.rimz().args(["setup", "--yes"]).assert().success();

    let text = std::fs::read_to_string(machine_config_path(&env)).expect("read merged config");
    assert!(
        text.contains("dsn = \"https://k@o0.ingest.sentry.io/0\""),
        "sentry dsn should survive:\n{text}"
    );
}

#[test]
fn setup_yes_merges_agents_team_layout_before_roles() {
    let env = Env::new();
    write_machine_file(
        &agents_config_path(&env),
        r#"
[agents.profiles.lead]
agent = "claude"

[agents.profiles.helper]
agent = "codex"

[agents.teams.duo]
layout = "lead+helper"
[[agents.teams.duo.roles]]
role = "lead"
profile = "lead"
[[agents.teams.duo.roles]]
role = "helper"
profile = "helper"
"#,
    );

    env.rimz()
        .args(["setup", "--yes"])
        .assert()
        .success()
        .stdout(contains("Merged"))
        .stdout(contains("No hooks or trust grants were changed"));

    let text = std::fs::read_to_string(agents_config_path(&env)).expect("read merged agents");
    assert!(
        text.contains("layout = \"lead+helper\""),
        "custom layout should survive:\n{text}"
    );
    assert!(
        text.contains("[[agents.teams.duo.roles]]"),
        "roles should render as array-of-tables:\n{text}"
    );
    assert!(
        !text.contains("roles = ["),
        "roles should not collapse to an inline array:\n{text}"
    );
    assert!(
        text.find("layout = \"lead+helper\"")
            .expect("layout survives")
            < text
                .find("[[agents.teams.duo.roles]]")
                .expect("roles block renders"),
        "layout should stay in the team table before roles:\n{text}"
    );
    assert!(
        text.contains("role = \"lead\"") && text.contains("profile = \"helper\""),
        "roles should survive:\n{text}"
    );
}

#[test]
fn setup_yes_merges_loop_tasks_as_table_blocks() {
    let env = Env::new();
    write_machine_file(
        &loop_config_path(&env),
        r#"
[tasks.self_wake]
bind = { kind = "claude", session = "s1", handle = "@planner" }
prompt = "resume"
root = "/r"

[tasks.pr_watch]
spec = "codex"
prompt = "check CI"
root = "/r"
every = "15m"
"#,
    );

    env.rimz()
        .args(["setup", "--yes"])
        .assert()
        .success()
        .stdout(contains("Merged"))
        .stdout(contains("No hooks or trust grants were changed"));

    let text = std::fs::read_to_string(loop_config_path(&env)).expect("read merged loop");
    assert!(
        text.contains("[tasks.self_wake]"),
        "task should render as a table block:\n{text}"
    );
    assert!(
        text.contains("[tasks.pr_watch]"),
        "bind-less task should render as a table block:\n{text}"
    );
    assert!(
        text.contains("[tasks.self_wake.bind]"),
        "bind should render as a nested table block:\n{text}"
    );
    assert!(
        !text.contains("tasks = {"),
        "tasks should not collapse to one inline table:\n{text}"
    );
    assert!(
        text.contains("spec = \"codex\"")
            && text.contains("every = \"15m\"")
            && text.contains("session = \"s1\""),
        "task fields should survive:\n{text}"
    );
}

#[test]
fn setup_yes_merges_agents_team_referencing_later_profile() {
    let env = Env::new();
    write_machine_file(
        &agents_config_path(&env),
        r#"
[agents.teams.duo]
[[agents.teams.duo.roles]]
role = "lead"
profile = "late"

[agents.profiles.late]
agent = "claude"
"#,
    );

    env.rimz().args(["setup", "--yes"]).assert().success();

    let text = std::fs::read_to_string(agents_config_path(&env)).expect("read merged agents");
    assert!(
        text.contains("role = \"lead\"") && text.contains("profile = \"late\""),
        "team role should survive after its profile is replayed:\n{text}"
    );
    assert!(
        text.contains("[agents.profiles.late]") && text.contains("agent = \"claude\""),
        "later profile should survive:\n{text}"
    );
}

#[test]
fn setup_yes_preserves_template_comments_for_untouched_config() {
    let env = Env::new();
    write_machine_file(
        &machine_config_path(&env),
        rimz::config::MachineConfig::template_core(),
    );

    env.rimz().args(["setup", "--yes"]).assert().success();

    let text = std::fs::read_to_string(machine_config_path(&env)).expect("read merged config");
    assert!(
        text.contains(
            "mouse_click_through = true            # single click on a card jumps to the agent"
        ),
        "zellij inline comment should stay attached:\n{text}"
    );
    assert!(
        text.contains("## pane_border_status = \"top\"          # \"off\", \"top\", or \"bottom\""),
        "tmux optional override comment should stay attached:\n{text}"
    );
}
