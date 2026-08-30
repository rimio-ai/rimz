//! Integration coverage for `rimz trust {status,grant,revoke}` and the
//! auto-stale path.

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use rimz::harness::launch::{ExecAction, ExecIdentity, ExecRequest, ProviderAccountState};
use rimz::ids::AgentKind;

#[cfg(unix)]
use crate::common::{CommandTimeoutExt, path_with_front, write_env_dump_shim};
use crate::common::{Env, exec_args};

/// A minimal project config carrying one command-executing hook field — the
/// fixture the trust-surface tests grant against.
const CLAUDE_HOOK_CONFIG: &str =
    "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n";

fn exec_request(kind: &str, action: ExecAction) -> ExecRequest {
    ExecRequest {
        kind: AgentKind::new_unchecked(kind),
        action,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        provider_account: ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: false,
        identity: ExecIdentity::default(),
    }
}

fn fresh_exec(kind: &str) -> Vec<String> {
    exec_args(&exec_request(
        kind,
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    ))
}

fn resume_exec(kind: &str, session_id: &str) -> Vec<String> {
    exec_args(&exec_request(
        kind,
        ExecAction::Resume {
            session_id: session_id.to_owned(),
            extra_args: Vec::new(),
        },
    ))
}

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
        "display_name = \"Query Engine\"\n\n[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n",
    );

    env.rimz().args(["trust", "grant"]).assert().success();

    // Edit a non-command field — `display_name` and `sidebar` are not in the
    // executable surface.
    env.write_config(
        &env.project_root,
        "display_name = \"Query Engine dev\"\nsidebar = true\n\n[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks claude\"\n",
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
    assert!(parsed["surface_diff"].is_null());
}

#[test]
fn trust_status_shows_stale_field_diff() {
    let env = Env::new();
    env.write_config(&env.project_root, CLAUDE_HOOK_CONFIG);
    env.rimz().args(["trust", "grant"]).assert().success();
    env.write_config(
        &env.project_root,
        "[[hooks]]\nevent = \"PreToolUse\"\ncommand = \"rimz hooks codex\"\n",
    );

    env.rimz()
        .args(["trust", "status"])
        .assert()
        .success()
        .stdout(contains("trust: stale"))
        .stdout(contains("~ hooks[0].command"))
        .stdout(contains("rimz hooks claude"))
        .stdout(contains("rimz hooks codex"));
}

#[test]
fn trust_rejects_project_layout_table_with_per_machine_fix() {
    let env = Env::new();
    env.write_config(
        &env.project_root,
        "[[layout.initial_panes]]\ncommand = \"$SHELL\"\n",
    );

    env.rimz()
        .args(["trust", "status"])
        .assert()
        .failure()
        .stderr(contains("[layout]"))
        .stderr(contains("per-machine"));
}

#[test]
fn trust_rejects_singular_append_prompt_key_with_plural_fix() {
    let env = Env::new();
    env.write_config(
        &env.project_root,
        "[profiles.x]\nagent = \"claude\"\nappend-system-prompt-file = \"x.md\"\n",
    );
    env.rimz()
        .args(["trust", "status"])
        .assert()
        .failure()
        .stderr(contains("append-system-prompt-files"));
}

#[test]
fn config_without_retired_prompt_field_keeps_legacy_surface_hash() {
    let config: rimz::trust::ProjectConfig =
        toml::from_str("[profiles.x]\nagent = \"claude\"\n").expect("config");
    assert_eq!(
        rimz::trust::executable_surface_hash(&config),
        "sha256:74540a29de2af542ed3c03114b07a7e1f51189584dab6ed978f0fa2002ba407e"
    );
}

#[test]
fn trust_hash_covers_subagent_profile_namespace_and_launch_fields() {
    let cases = [
        "[subagents.profiles.x]\nagent = \"claude\"\n",
        "[subagents.profiles.x]\nagent = \"codex\"\n",
        "[subagents.profiles.x]\nagent = \"claude\"\nmode = \"ask\"\n",
        "[subagents.profiles.x]\nagent = \"claude\"\nmodel = \"opus\"\n",
        "[subagents.profiles.x]\nagent = \"claude\"\neffort = \"low\"\n",
        "[subagents.profiles.x]\nagent = \"claude\"\nsystem-prompt-file = \"prompts/x.md\"\n",
        "[subagents.profiles.x]\nagent = \"claude\"\nappend-system-prompt-files = [\"prompts/a.md\"]\n",
        "[subagents.profiles.x]\nagent = \"claude\"\nargs = \"--profile x\"\n",
        "[subagents.profiles.y]\nagent = \"claude\"\n",
    ];
    let hashes = cases
        .into_iter()
        .map(|text| {
            let config: rimz::trust::ProjectConfig = toml::from_str(text).expect("config");
            rimz::trust::executable_surface_hash(&config)
        })
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(hashes.len(), cases.len());
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
        .args(fresh_exec("codex"))
        .env("SHELL", "/definitely/not/a/shell")
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("trusted codex env launch");

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
        .args(fresh_exec("codex"))
        .assert()
        .failure()
        .stderr(contains("rimz trust grant"));
}

/// Agent-view policy belongs to Claude Code and remains a normal trusted
/// project launch setting.
#[cfg(unix)]
#[test]
fn trusted_claude_agent_view_env_reaches_the_process() {
    let env = Env::new();
    env.write_config(
        &env.project_root,
        "[[agents]]\nname = \"claude\"\nenv = { CLAUDE_CODE_DISABLE_AGENT_VIEW = \"0\" }\n",
    );
    env.rimz().args(["trust", "grant"]).assert().success();

    let shim_dir = write_env_dump_shim(&env, "claude");
    let dump = env.home_root.join("claude.env");
    env.rimz()
        .args(fresh_exec("claude"))
        .env("SHELL", "/definitely/not/a/shell")
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("trusted claude env launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped
            .lines()
            .any(|line| line == "CLAUDE_CODE_DISABLE_AGENT_VIEW=0"),
        "claude launch env misses the trusted project value:\n{dumped}"
    );
}

/// A resumed agent funnels through the same exec wrapper as a fresh launch:
/// the pane runs `rimz agents exec <kind> --resume <id>`, so trusted project
/// env reaches the resumed process, and the child receives the adapter's own
/// resume argv.
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
        .args(resume_exec("claude", "sess-1"))
        .env("SHELL", "/definitely/not/a/shell")
        .env("PATH", path_with_front(&shim_dir))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &dump)
        .assert_success_within_timeout("trusted claude resume launch");

    let dumped = std::fs::read_to_string(&dump).expect("read env dump");
    assert!(
        dumped.lines().any(|line| line == "ARGV_1=--resume")
            && dumped.lines().any(|line| line == "ARGV_2=sess-1")
            && dumped.contains("<system_reminder>"),
        "resumed agent misses the resume argv:\n{dumped}"
    );
    assert!(
        dumped.lines().any(|line| line == "RIMZ_TEST_INJECTED=yes"),
        "resumed agent env misses the trusted project var:\n{dumped}"
    );
}

#[test]
fn untrusted_agent_env_refuses_a_resume_launch() {
    let env = Env::new();
    env.write_config(&env.project_root, CODEX_ENV_CONFIG);

    env.rimz()
        .args(resume_exec("codex", "sess-1"))
        .assert()
        .failure()
        .stderr(contains("rimz trust grant"));
}

/// `rimz agents` gates agent-cell env at the command entry point: an untrusted
/// configured env refuses the whole command before the worktree is created or
/// the tab opens, instead of leaving a failed agent pane behind.
#[test]
fn untrusted_agent_env_refuses_an_agents_launch_before_side_effects() {
    let env = Env::new();
    env.write_config(&env.project_root, CODEX_ENV_CONFIG);

    env.rimz()
        .args(["agents", "codex", "--worktree=wt-a"])
        .assert()
        .failure()
        .stderr(contains("rimz trust grant"));

    assert!(
        !env.home_root.join("project-worktrees/wt-a").exists(),
        "final launch validation must precede worktree creation"
    );
    assert!(
        !env.state_path_for(&env.project_root).events_log.exists(),
        "final launch validation must precede identity allocation"
    );
}
