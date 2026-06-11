//! Integration coverage for the supervised agent launch shell wrapper.

#[cfg(unix)]
use assert_cmd::assert::OutputAssertExt;

#[cfg(unix)]
use crate::common::{Env, path_with_front, write_env_dump_shim, write_fake_login_shell};

#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
#[test]
fn shell_rc_env_reaches_the_spawned_agent() {
    let env = Env::new();
    let shell = write_fake_login_shell(
        &env,
        "rimz-test-sh",
        &[("RIMZ_TEST_RC_MARKER", "from-shell")],
    );
    let shim_dir = write_env_dump_shim(&env, "codex");
    let dump = env.home_root.join("codex-shell.env");

    env.rimz()
        .args(["agents", "exec", "codex"])
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert()
        .success();

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "RIMZ_TEST_RC_MARKER=from-shell"),
        "agent process env misses the shell rc marker:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn bashrc_path_reaches_the_spawned_agent() {
    if !Path::new("/bin/bash").is_file() {
        return;
    }
    let env = Env::new();
    let shim_dir = write_env_dump_shim(&env, "codex");
    std::fs::write(
        env.home_root.join(".bashrc"),
        format!(
            "export PATH='{}':\"$PATH\"\nexport RIMZ_TEST_BASHRC_MARKER=from-bashrc\n",
            shim_dir.display()
        ),
    )
    .expect("write bashrc");
    let dump = env.home_root.join("codex-bashrc.env");

    env.rimz()
        .args(["agents", "exec", "codex"])
        .env("SHELL", "/bin/bash")
        .env("PATH", "/usr/bin:/bin")
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert()
        .success();

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "RIMZ_TEST_BASHRC_MARKER=from-bashrc"),
        "agent process env misses the bashrc marker:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn adapter_pin_overrides_shell_rc_env() {
    let env = Env::new();
    let shell = write_fake_login_shell(
        &env,
        "rimz-test-sh",
        &[("CLAUDE_CODE_DISABLE_AGENT_VIEW", "0")],
    );
    let shim_dir = write_env_dump_shim(&env, "claude");
    let dump = env.home_root.join("claude-shell.env");

    env.rimz()
        .args(["agents", "exec", "claude"])
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert()
        .success();

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "CLAUDE_CODE_DISABLE_AGENT_VIEW=1"),
        "claude launch env misses the built-in pin after shell rc:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn trusted_agent_env_overrides_shell_rc_env() {
    let env = Env::new();
    env.write_config(
        &env.project_root,
        "[[agents]]\nname = \"codex\"\nenv = { RIMZ_TEST_CONFIGURED = \"trusted\" }\n",
    );
    env.rimz().args(["trust", "grant"]).assert().success();
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[("RIMZ_TEST_CONFIGURED", "rc")]);
    let shim_dir = write_env_dump_shim(&env, "codex");
    let dump = env.home_root.join("codex-trusted-shell.env");

    env.rimz()
        .args(["agents", "exec", "codex"])
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert()
        .success();

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "RIMZ_TEST_CONFIGURED=trusted"),
        "trusted launch env did not override the shell rc value:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn missing_shell_path_falls_back_to_direct_exec() {
    let env = Env::new();
    let shim_dir = write_env_dump_shim(&env, "codex");
    let dump = env.home_root.join("codex-direct.env");

    env.rimz()
        .args(["agents", "exec", "codex"])
        .env("SHELL", "/definitely/not/a/shell")
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert()
        .success();

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped.lines().any(|line| line == "ARGV="),
        "direct fallback did not run the agent shim:\n{dumped}"
    );
}

#[cfg(unix)]
#[test]
fn prompt_with_shell_metacharacters_stays_one_argument() {
    let env = Env::new();
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let shim_dir = write_env_dump_shim(&env, "codex");
    let dump = env.home_root.join("codex-prompt.env");
    let prompt = r#"say "hello there" with spaces"#;

    env.rimz()
        .args(["agents", "exec", "codex", "--prompt", prompt])
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert()
        .success();

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped.lines().any(|line| line == "ARGC=1"),
        "prompt was split into multiple argv elements:\n{dumped}"
    );
    assert!(
        dumped
            .lines()
            .any(|line| line == format!("ARGV_1={prompt}")),
        "prompt argv element was changed by the shell wrapper:\n{dumped}"
    );
}
