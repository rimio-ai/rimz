//! Integration coverage for `rimz trust {status,grant,revoke}` and the
//! auto-stale path.

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;

use crate::common::Env;
#[cfg(unix)]
use crate::common::{path_with_front, write_env_dump_shim};

/// A minimal project config carrying one command-executing hook field — the
/// fixture the trust-surface tests grant against.
const CLAUDE_HOOK_CONFIG: &str =
    "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n";

#[test]
fn trust_status_grant_revoke_lifecycle() {
    let env = Env::new();
    env.write_config(&env.project_root, CLAUDE_HOOK_CONFIG);

    env.rimz()
        .args(["trust", "status"])
        .assert()
        .success()
        .stdout(contains("trust: untrusted"));

    env.rimz()
        .args(["trust", "grant"])
        .assert()
        .success()
        .stdout(contains("trust: trusted"));

    env.rimz()
        .args(["trust", "status"])
        .assert()
        .success()
        .stdout(contains("trust: trusted"));

    env.rimz()
        .args(["trust", "revoke"])
        .assert()
        .success()
        .stdout(contains("trust: untrusted"));
}

#[test]
fn trust_auto_revokes_when_executable_surface_drifts() {
    let env = Env::new();
    env.write_config(&env.project_root, CLAUDE_HOOK_CONFIG);

    env.rimz().args(["trust", "grant"]).assert().success();

    // Mutate the hook command — a command-running field.
    env.write_config(
        &env.project_root,
        "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks codex\"\n",
    );

    env.rimz()
        .args(["trust", "status"])
        .assert()
        .success()
        .stdout(contains("trust: stale"));
}

#[test]
fn trust_ignores_non_command_field_edits() {
    let env = Env::new();
    env.write_config(
        &env.project_root,
        "display_name = \"Query Engine\"\n\n[[layout.initial_panes]]\ncommand = \"$SHELL\"\n",
    );

    env.rimz().args(["trust", "grant"]).assert().success();

    // Edit a non-command field — `display_name` and `sidebar` are not in the
    // executable surface.
    env.write_config(
        &env.project_root,
        "display_name = \"Query Engine dev\"\nsidebar = true\n\n[[layout.initial_panes]]\ncommand = \"$SHELL\"\n",
    );

    env.rimz()
        .args(["trust", "status"])
        .assert()
        .success()
        .stdout(contains("trust: trusted"));
}

#[test]
fn trust_status_json_emits_canonical_fields() {
    let env = Env::new();
    env.write_config(
        &env.project_root,
        "[notifications]\ncommand = \"notify-send rimz\"\n",
    );

    let output = env
        .rimz()
        .args(["trust", "status", "--json"])
        .output()
        .expect("run rimz");
    assert!(
        output.status.success(),
        "rimz trust status --json failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parse json");
    assert_eq!(parsed["state"], "untrusted");
    assert!(
        parsed["current_hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(parsed["granted_hash"].is_null());
    assert!(parsed["granted_at"].is_null());
}

#[test]
fn trust_no_config_reports_no_config() {
    let env = Env::new();
    env.rimz()
        .args(["trust", "status"])
        .assert()
        .success()
        .stdout(contains("trust: no project config"));
}

/// One agent env var on the codex kind — the fixture for the launch-time
/// env-injection gate.
const CODEX_ENV_CONFIG: &str =
    "[[agents]]\nname = \"codex\"\nenv = { RIMZ_TEST_INJECTED = \"yes\" }\n";

#[cfg(unix)]
#[test]
fn trusted_agent_env_reaches_the_spawned_agent() {
    let env = Env::new();
    env.write_config(&env.project_root, CODEX_ENV_CONFIG);
    env.rimz().args(["trust", "grant"]).assert().success();

    let shim_dir = write_env_dump_shim(&env, "codex");
    let dump = env.home_root.join("codex.env");
    env.rimz()
        .args(["agents", "exec", "codex"])
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert()
        .success();

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped.lines().any(|line| line == "RIMZ_TEST_INJECTED=yes"),
        "agent process env misses the injected var:\n{dumped}"
    );
}

#[test]
fn untrusted_agent_env_refuses_the_launch() {
    let env = Env::new();
    env.write_config(&env.project_root, CODEX_ENV_CONFIG);

    env.rimz()
        .args(["agents", "exec", "codex"])
        .assert()
        .failure()
        .stderr(contains("rimz trust grant"));
}

/// The Claude launch pin is an adapter built-in, applied over project config:
/// a trusted workspace declaring the var cannot switch the pane back into the
/// agents dashboard the integration cannot drive.
#[cfg(unix)]
#[test]
fn builtin_claude_launch_env_overrides_project_config() {
    let env = Env::new();
    env.write_config(
        &env.project_root,
        "[[agents]]\nname = \"claude\"\nenv = { CLAUDE_CODE_DISABLE_AGENT_VIEW = \"0\" }\n",
    );
    env.rimz().args(["trust", "grant"]).assert().success();

    let shim_dir = write_env_dump_shim(&env, "claude");
    let dump = env.home_root.join("claude.env");
    env.rimz()
        .args(["agents", "exec", "claude"])
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert()
        .success();

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "CLAUDE_CODE_DISABLE_AGENT_VIEW=1"),
        "claude launch env misses the built-in pin:\n{dumped}"
    );
}

/// A resumed agent funnels through the same exec wrapper as a fresh launch:
/// the pane runs `rimz agents exec <kind> --resume <id>`, so trusted project
/// env and the adapter's built-in pins reach the resumed process, and the
/// child receives the adapter's own resume argv.
#[cfg(unix)]
#[test]
fn resumed_agent_env_funnels_through_the_exec_wrapper() {
    let env = Env::new();
    env.write_config(
        &env.project_root,
        "[[agents]]\nname = \"claude\"\nenv = { RIMZ_TEST_INJECTED = \"yes\" }\n",
    );
    env.rimz().args(["trust", "grant"]).assert().success();

    let shim_dir = write_env_dump_shim(&env, "claude");
    let dump = env.home_root.join("claude-resume.env");
    env.rimz()
        .args(["agents", "exec", "claude", "--resume", "sess-1"])
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert()
        .success();

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped.lines().any(|line| line == "ARGV=--resume sess-1"),
        "resumed agent misses the resume argv:\n{dumped}"
    );
    assert!(
        dumped.lines().any(|line| line == "RIMZ_TEST_INJECTED=yes"),
        "resumed agent env misses the trusted project var:\n{dumped}"
    );
    assert!(
        dumped
            .lines()
            .any(|line| line == "CLAUDE_CODE_DISABLE_AGENT_VIEW=1"),
        "resumed agent env misses the built-in pin:\n{dumped}"
    );
}

#[test]
fn untrusted_agent_env_refuses_a_resume_launch() {
    let env = Env::new();
    env.write_config(&env.project_root, CODEX_ENV_CONFIG);

    env.rimz()
        .args(["agents", "exec", "codex", "--resume", "sess-1"])
        .assert()
        .failure()
        .stderr(contains("rimz trust grant"));
}

/// `rimz tab` gates agent-cell env at the command entry point: an untrusted
/// configured env refuses the whole command before the worktree is created or
/// the tab opens, instead of leaving a failed agent pane behind.
#[test]
fn untrusted_agent_env_refuses_a_tab_launch_before_side_effects() {
    let env = Env::new();
    env.write_config(&env.project_root, CODEX_ENV_CONFIG);

    env.rimz()
        .args(["tab", "--layout", "codex", "--worktree", "wt-a"])
        .assert()
        .failure()
        .stderr(contains("rimz trust grant"));
}
