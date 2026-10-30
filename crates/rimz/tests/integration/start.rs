//! `rimz start` run from inside a session of the selected mux: a same-mux room
//! can't be nested, so the default launch reports the directory's room and
//! exits before any side effect instead of emitting a doomed nested
//! `attach`.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::common::room::{MATERIALIZED_ROOM_PANES, ShimRoom, seed_sidebar_heartbeat};
use crate::common::{COMMAND_TIMEOUT, CommandTimeoutExt, Env, path_with_front, zellij_trace_shim};

fn seed_actionable_agent(env: &Env) -> PathBuf {
    let bin_dir = env.home_root.join("agent-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir agent bin");
    let agy = bin_dir.join("agy");
    std::fs::write(&agy, "#!/bin/sh\nexit 0\n").expect("write agent shim");
    std::fs::set_permissions(&agy, std::fs::Permissions::from_mode(0o755))
        .expect("chmod agent shim");
    bin_dir
}

fn configure_actionable_hooks(
    command: &mut std::process::Command,
    env: &Env,
    bin_dir: &Path,
    zellij_log: &Path,
    sessions: &str,
) {
    let presence = env.project_root.join("presence.wasm");
    std::fs::write(&presence, b"test-presence").expect("write presence fixture");
    command
        .args(["--mux", "zellij", "start", "--no-attach"])
        .env("PATH", bin_dir)
        .env("TERM", "dumb")
        .env("RIMZ_PETS_OFFLINE", "1")
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", zellij_log)
        .env("RIMZ_PRESENCE_PLUGIN", presence)
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", sessions)
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", MATERIALIZED_ROOM_PANES)
        .env(
            "RIMZ_ANTIGRAVITY_HOOKS",
            env.home_root.join("agent-config/hooks.json"),
        )
        .env(
            "RIMZ_ANTIGRAVITY_SETTINGS",
            env.home_root.join("agent-config/settings.json"),
        );
}

fn assert_health_before_presence(trace: &str) {
    let lines = trace.lines().collect::<Vec<_>>();
    let presence = lines
        .iter()
        .position(|line| line.contains("\tpipe\t--plugin\t"))
        .expect("presence pipe in trace");
    let health = lines[..presence]
        .iter()
        .rposition(|line| line.contains("\tlist-sessions"))
        .expect("health list before presence");
    assert!(
        health < presence,
        "health gate must precede presence:\n{trace}"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn start_warns_and_records_an_unavailable_sandbox_without_overwriting_config() {
    let env = Env::new();
    let config_dir = env.rimz_home();
    std::fs::create_dir_all(&config_dir).expect("mkdir config");
    let agents_path = config_dir.join("config.toml");
    let seed = "[agents]\nisolation = \"scoped\"\n";
    std::fs::write(&agents_path, seed).expect("write sandbox config");
    let output = start_with_accounts(
        &env,
        "",
        &[],
        Some("configuration warnings must permit birth"),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("bubblewrap"), "{stderr}");
    assert!(
        stderr.contains("rimz config set agents.isolation host"),
        "{stderr}"
    );
    assert!(
        stderr.contains("[security; RimZ won't change this]"),
        "{stderr}"
    );
    assert_eq!(std::fs::read_to_string(&agents_path).unwrap(), seed);
    assert_skipped(&env, &stderr, "agents.isolation", "bubblewrap");

    let broken = "[agents\nisolation = \"scoped\"\n";
    std::fs::write(&agents_path, broken).unwrap();
    let output = start_with_accounts(
        &env,
        "",
        &[],
        Some("configuration warnings must permit birth"),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(std::fs::read_to_string(&agents_path).unwrap(), broken);
    assert_skipped(
        &env,
        &stderr,
        &agents_path.display().to_string(),
        "unparseable",
    );
}

#[test]
fn start_warns_and_records_an_uninstalled_mux_default() {
    let env = Env::new();
    write_machine_config(&env, "[mux]\ndefault = 'tmux'\n");
    let output = start_with_accounts(
        &env,
        "",
        &[],
        Some("an unavailable default must permit birth"),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_skipped(&env, &stderr, "mux.default", "zellij");
}

#[test]
fn start_records_an_unsupported_pack_version_and_keeps_its_profiles() {
    let env = Env::new();
    let source = env.home_root.join("version-pack");
    std::fs::create_dir_all(source.join("agents")).unwrap();
    std::fs::write(
        source.join("rimz-packs.toml"),
        "[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = '.'\nrimz = '<0.0.1'\n",
    )
    .unwrap();
    std::fs::write(
        source.join("agents/claude.md"),
        "---\ndescription: Base\n---\nBase.",
    )
    .unwrap();
    std::fs::write(
        source.join("agents/coder.md"),
        "---\ndescription: Pack version coder\nagent: claude\ntools: [Read]\n---\nCode.",
    )
    .unwrap();
    rimz::packs::install(&env.agents_home(), &source).unwrap();
    let output = start_with_accounts(
        &env,
        "",
        &[],
        Some("a pack version warning must permit birth"),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{output:?}");
    let packs = rimz::packs::resolve(&env.agents_home());
    let manifest = env
        .agents_home()
        .join("packs")
        .join(&packs.packs[0].entry)
        .join("rimz-packs.toml");
    assert_skipped(&env, &stderr, &manifest.display().to_string(), "<0.0.1");
    assert!(
        stderr.contains(&format!(
            "update the linked checkout at {}",
            source.display()
        )),
        "{stderr}"
    );
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(env.state_path_for(&env.project_root).workspace_record).unwrap(),
    )
    .unwrap();
    assert_eq!(
        record["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["setting"] == manifest.display().to_string())
            .count(),
        1,
        "{record:?}"
    );
    let profiles = env
        .rimz()
        .args(["agents", "profiles"])
        .bounded_output()
        .unwrap();
    assert!(profiles.status.success(), "{profiles:?}");
    assert!(
        String::from_utf8_lossy(&profiles.stdout).contains("Pack version coder"),
        "{profiles:?}"
    );
    let validate = env
        .rimz()
        .args(["agents", "validate"])
        .bounded_output()
        .unwrap();
    assert!(validate.status.success(), "{validate:?}");
    let listed = env
        .rimz()
        .args(["packs", "list", "--json"])
        .bounded_output()
        .unwrap();
    assert!(listed.status.success(), "{listed:?}");
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert!(
        listed["warnings"].to_string().contains("<0.0.1"),
        "{listed}"
    );
}

#[test]
fn start_and_attach_record_missing_expected_packs_before_project_trust() {
    let env = Env::new();
    let source = env.home_root.join("expected-pack");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("rimz-packs.toml"),
        "[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = '.'\n",
    )
    .unwrap();
    rimz::packs::install(&env.agents_home(), &source).unwrap();
    env.write_config(
        &env.project_root,
        "[packs]\nexpect = ['crew/crew', 'skills/expected-tools', 'github:acme/tools/expected-tools', 'github:acme/tools/expected-lint']\n",
    );
    for attach in [false, true] {
        let output = if attach {
            let bin = seed_actionable_agent(&env);
            let trace = env.project_root.join("zellij-expect-attach.log");
            let _room = ShimRoom::watch(&env, &trace);
            env.rimz()
                .args(["--mux", "zellij", "attach", "--print"])
                .env("PATH", &bin)
                .env("TERM", "dumb")
                .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
                .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
                .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", "")
                .bounded_output()
                .unwrap()
        } else {
            start_with_accounts(&env, "", &[], None)
        };
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        let record: serde_json::Value = serde_json::from_slice(
            &std::fs::read(env.state_path_for(&env.project_root).workspace_record).unwrap(),
        )
        .unwrap();
        let warnings: Vec<_> = record["skipped"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|entry| entry["setting"] == "packs.expect")
            .collect();
        assert_eq!(warnings.len(), 2, "{record}");
        for (entry, spelling) in warnings
            .iter()
            .zip(["skills/expected-tools", "github:acme/tools/expected-lint"])
        {
            let message = entry["message"].as_str().unwrap();
            assert!(message.contains("project expects"), "{message}");
            assert!(
                message.contains(&format!("rimz packs install {spelling}")),
                "{message}"
            );
            assert_eq!(
                stderr.matches(&format!("rimz: warning: {message}")).count(),
                1,
                "{stderr}"
            );
        }
    }
    let trust = env
        .rimz()
        .args(["trust", "status", "--json"])
        .bounded_output()
        .unwrap();
    let trust: serde_json::Value = serde_json::from_slice(&trust.stdout).unwrap();
    assert_eq!(trust["state"], "untrusted", "{trust}");
}

#[test]
fn start_records_malformed_pack_expectations_without_revoking_trust() {
    let env = Env::new();
    env.write_config(&env.project_root, "[packs]\nexpect = 5\n");
    let grant = env
        .rimz()
        .args(["trust", "grant"])
        .bounded_output()
        .unwrap();
    assert!(grant.status.success(), "{grant:?}");
    let started = start_with_accounts(&env, "", &[], None);
    let stderr = String::from_utf8_lossy(&started.stderr);
    assert!(started.status.success(), "{stderr}");
    assert!(stderr.contains("packs.expect"), "{stderr}");
    assert_skipped(&env, &stderr, "packs.expect", "5");
    for spelling in [
        "crew/<pack>",
        "skills/<pack>",
        "github:<owner>/<repo>/<pack>",
    ] {
        assert!(stderr.contains(spelling), "{stderr}");
    }
    let trust = env
        .rimz()
        .args(["trust", "status", "--json"])
        .bounded_output()
        .unwrap();
    let trust: serde_json::Value = serde_json::from_slice(&trust.stdout).unwrap();
    assert_eq!(trust["state"], "trusted", "{trust}");
}

#[test]
fn birth_saves_the_shell_before_panes_and_live_reentry_keeps_it() {
    let env = Env::new();
    write_machine_config(&env, "[agents]\nshell = 'bash'\n");
    let bin = seed_actionable_agent(&env);
    let shell = bin.join("bash");
    std::fs::write(&shell, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    let wrapper = env.home_root.join("zellij-shell-probe");
    std::fs::write(
        &wrapper,
        "#!/bin/sh\n\
         case \" $* \" in\n\
           *' --create-background '*)\n\
             /bin/dd if=\"$RIMZ_TEST_BIRTH_RECORD\" of=\"$RIMZ_TEST_BIRTH_OBSERVED\" 2>/dev/null\n\
             ;;\n\
         esac\n\
         exec \"$RIMZ_TEST_ZELLIJ_REAL\" \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let record_path = env.state_path_for(&env.project_root).workspace_record;
    let observed = env.home_root.join("record-before-panes.json");
    let trace = env.home_root.join("shell-birth.log");
    let _room = ShimRoom::watch(&env, &trace);
    let mut command = env.rimz();
    configure_actionable_hooks(&mut command, &env, &bin, &trace, "");
    let output = command
        .env("RIMZ_ZELLIJ_BIN", &wrapper)
        .env("RIMZ_TEST_ZELLIJ_REAL", zellij_trace_shim())
        .env("RIMZ_TEST_BIRTH_RECORD", &record_path)
        .env("RIMZ_TEST_BIRTH_OBSERVED", &observed)
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let before: serde_json::Value =
        serde_json::from_slice(&std::fs::read(observed).unwrap()).unwrap();
    let expected = serde_json::json!({"name": "bash", "path": shell});
    assert_eq!(
        before["agent_shell"], expected,
        "the session creator must already see the lookup"
    );
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    let session = record["session_name"].as_str().unwrap();
    let other = env.home_root.join("other-bin");
    std::fs::create_dir(&other).unwrap();
    std::os::unix::fs::symlink(&shell, other.join("bash")).unwrap();
    let mut command = env.rimz();
    configure_actionable_hooks(&mut command, &env, &bin, &trace, session);
    let output = command
        .env("PATH", std::env::join_paths([&other, &bin]).unwrap())
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(record_path).unwrap()).unwrap();
    assert_eq!(
        after["agent_shell"], expected,
        "live entry must not refresh the lookup"
    );
}

#[test]
fn new_birth_clears_a_previous_shell_for_unset_absolute_or_missing_values() {
    use std::os::unix::ffi::OsStrExt;

    let mut actual = Vec::new();
    for value in [
        None,
        Some("/bin/sh"),
        Some("rimz-test-shell-not-installed"),
        Some("bash"),
    ] {
        let env = Env::new();
        env.record(&env.project_root);
        let record_path = env.state_path_for(&env.project_root).workspace_record;
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
        record["agent_shell"] = serde_json::json!({"name": "bash", "path": "/old/bash"});
        std::fs::write(&record_path, record.to_string()).unwrap();
        write_machine_config(
            &env,
            &value
                .map(|shell| format!("[agents]\nshell = '{shell}'\n"))
                .unwrap_or_default(),
        );
        let bin = seed_actionable_agent(&env);
        let trace = env.project_root.join("zellij-accounts.log");
        let mut room = ShimRoom::watch(&env, &trace);
        room.allow_no_trigger("shell configuration must permit birth");
        let mut command = env.rimz();
        configure_actionable_hooks(&mut command, &env, &bin, &trace, "");
        if value == Some("bash") {
            let non_utf8 = env.home_root.join(std::ffi::OsStr::from_bytes(b"bin-\xff"));
            crate::common::write_path_shim(&non_utf8, "bash", "exit 0");
            command.env("PATH", std::env::join_paths([&non_utf8, &bin]).unwrap());
        }
        let output = command.bounded_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(record_path).unwrap()).unwrap();
        actual.push(record.get("agent_shell").cloned());
        if value == Some("rimz-test-shell-not-installed") {
            assert_skipped(
                &env,
                &String::from_utf8_lossy(&output.stderr),
                "agents.shell",
                "not on PATH",
            );
        }
    }
    assert_eq!(actual, [None, None, None, None]);
}

#[test]
fn start_warns_and_records_a_missing_configured_agent_shell() {
    let env = Env::new();
    let config_dir = env.rimz_home();
    std::fs::create_dir_all(&config_dir).expect("mkdir config");
    let missing = env.home_root.join("missing").join("bash");
    std::fs::write(
        config_dir.join("config.toml"),
        format!("[agents]\nshell = \"{}\"\n", missing.display()),
    )
    .expect("write agent shell config");
    let output = start_with_accounts(
        &env,
        "",
        &[],
        Some("configuration warnings must permit birth"),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("[agents] shell") && stderr.contains("does not exist"),
        "{stderr}"
    );
    assert_skipped(&env, &stderr, "agents.shell", "does not exist");
}

#[test]
fn start_warns_about_a_legacy_only_host_without_writing_config() {
    let env = Env::new();
    std::fs::remove_dir_all(env.rimz_home()).expect("remove fixture home");
    let legacy = env.state_root().join("rimz");
    std::fs::create_dir_all(&legacy).expect("seed legacy root");
    let output = start_with_accounts(
        &env,
        "",
        &[],
        Some("configuration warnings must permit birth"),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains(&legacy.display().to_string()), "{stderr}");
    assert!(stderr.contains("moving-from-the-xdg-roots"), "{stderr}");
    assert!(!env.rimz_home().join("config.toml").exists());
    assert_skipped(
        &env,
        &stderr,
        &env.rimz_home().join("config.toml").display().to_string(),
        "legacy",
    );
}

#[test]
fn start_auto_fallback_reports_the_open_room() {
    let env = Env::new();
    let bin_dir = seed_actionable_agent(&env);
    let trace = env.project_root.join("zellij-fallback.log");
    let _room = ShimRoom::watch(&env, &trace);
    let mut command = env.rimz();
    configure_actionable_hooks(&mut command, &env, &bin_dir, &trace, "");
    // Remove the fixture's explicit print flag without changing its environment.
    let args: Vec<_> = command
        .get_args()
        .filter(|arg| *arg != "--no-attach")
        .map(|arg| arg.to_owned())
        .collect();
    let mut auto = std::process::Command::new(command.get_program());
    auto.args(args).current_dir(&env.project_root);
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            auto.env(key, value);
        } else {
            auto.env_remove(key);
        }
    }
    let output = auto.bounded_output().expect("auto start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
    let notice = "rimz: no terminal here: the room is open but not entered; printed the attach command instead (same as --print). Pass --attach to enter it anyway.";
    assert_eq!(
        stderr.lines().filter(|line| *line == notice).count(),
        1,
        "{stderr}"
    );
}

#[test]
fn start_names_a_broken_definition_and_still_opens_the_room() {
    let env = Env::new();
    let broken = crate::common::write_definition(
        &env,
        "agents",
        "broken",
        "description: Broken\nagent: missing-parent",
        "",
    );
    let output = start_with_accounts(&env, "", &[], None);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("rimz: warning: 1 definition with another error: agents/broken"),
        "{stderr}"
    );
    assert!(
        stderr.contains("launches that select them are refused"),
        "{stderr}"
    );
    assert!(
        stderr.contains("`rimz agents validate` lists every error"),
        "{stderr}"
    );
    assert!(!stderr.contains(&broken.display().to_string()), "{stderr}");
}

#[test]
fn start_names_a_broken_theme_and_still_opens_the_room() {
    let env = Env::new();
    let path = env.rimz_home().join("theme.toml");
    std::fs::write(&path, "[theme]\nscheme = 'missing scheme'\n").unwrap();
    let output = start_with_accounts(&env, "", &[], None);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("theme.toml: unknown sidebar theme scheme `missing scheme`"),
        "{stderr}"
    );
    assert!(
        stderr
            .contains("the sidebar keeps the default scheme `TokyoNight Night` until it is fixed"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("every setting in it is ignored"),
        "{stderr}"
    );
}

#[test]
fn singular_agent_is_unknown_subcommand_with_agents_suggestion() {
    let env = Env::new();

    let output = env
        .rimz()
        .arg("agent")
        .bounded_output()
        .expect("run rimz agent");

    assert!(
        !output.status.success(),
        "`rimz agent` should fail, got: {:?}",
        output.status,
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("agents"),
        "stderr should suggest the plural subcommand, got: {stderr}"
    );
    assert!(
        !stderr.contains("nested room"),
        "stderr should come from clap, not the nested-room guard, got: {stderr}"
    );
}

#[test]
fn start_inside_selected_mux_reports_and_skips_launch() {
    let env = Env::new();
    let bin = env.home_root.join("sandbox-bin");
    std::fs::create_dir(&bin).expect("mkdir bwrap PATH");
    let bwrap = bin.join("bwrap");
    let marker = env.home_root.join("bwrap-probed");
    std::fs::write(
        &bwrap,
        "#!/bin/sh\n: > \"$RIMZ_TEST_BWRAP_PROBE\"\nexit 1\n",
    )
    .expect("write bwrap shim");
    std::fs::set_permissions(&bwrap, std::fs::Permissions::from_mode(0o755))
        .expect("chmod bwrap shim");

    let output = env
        .rimz()
        .arg("start")
        // Pretend we're already inside a Zellij session: `auto_detect_backend`
        // selects Zellij from `ZELLIJ` alone, with no binary on PATH.
        .env("ZELLIJ", "1")
        .env("PATH", &bin)
        .env("RIMZ_TEST_BWRAP_PROBE", &marker)
        .bounded_output()
        .expect("run rimz start");

    assert!(
        !marker.exists(),
        "default host isolation must not execute bwrap"
    );

    assert!(
        output.status.success(),
        "a nested run is a no-op success, got: {:?}",
        output.status,
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.is_empty(), "{stdout}");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr,
        "This directory's room isn't running, and zellij can't start one inside this session.\nDetach and run `rimz` here, or run it from another terminal.\n"
    );
    // The guard returns before `ensure_detected_agent_hooks`, so the first-run
    // hook consent gate never prints — proving the bypass skips the ceremony.
    assert!(
        !stderr.contains("RimZ first run"),
        "the nested bypass must run before hook install, got: {stderr}"
    );
    assert!(!env.rimz_home().join("config.toml").exists());
    assert!(
        !env.state_path_for(&env.project_root)
            .workspace_record
            .exists()
    );
}

#[test]
fn start_inside_selected_mux_reports_a_stopped_recorded_room_without_writing() {
    let env = Env::new();
    env.record(&env.project_root);
    let paths = env.state_path_for(&env.project_root);
    let before = std::fs::read(&paths.workspace_record).expect("record bytes");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "start"])
        .env("ZELLIJ", "1")
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("zellij.log"))
        .bounded_output()
        .expect("run nested start for a stopped room");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "This directory's room isn't running, and zellij can't start one inside this session.\nDetach and run `rimz` here, or run it from another terminal.\n"
    );
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read(&paths.workspace_record).unwrap(), before);
    assert!(!env.rimz_home().join("config.toml").exists());
}

#[test]
fn nested_start_account_refusal_matches_the_room_situation() {
    for (pinned, live) in [(false, false), (false, true), (true, false)] {
        let env = Env::new();
        let workspace = env.resolve_workspace(&env.project_root);
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij", "start", "--account", "claude=work"])
            .env("ZELLIJ", "1")
            .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("zellij.log"));
        if live {
            command.env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", &workspace.session_name);
        }
        if pinned {
            command
                .env("RIMZ_WORKSPACE_ID", workspace.workspace_id.as_str())
                .env("RIMZ_PROJECT_ROOT", &workspace.project_root);
        }
        let output = command.bounded_output().expect("nested start with account");
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        if pinned || live {
            assert_eq!(
                stderr,
                "error: --account applies when a room is born, and this room is already running; run `rimz accounts use <KIND> <NAME>` to change its default for future launches\n"
            );
        } else {
            assert_eq!(
                stderr,
                "error: --account applies when a room is born, and zellij can't start one inside this session; detach and rerun the same command\n"
            );
        }
        assert!(output.stdout.is_empty());
        assert!(!env.rimz_home().join("config.toml").exists());
        assert!(
            !env.state_path_for(&env.project_root)
                .workspace_record
                .exists()
        );
    }
}

#[test]
fn start_inside_its_pinned_room_reports_the_current_room_without_writing() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let output = env
        .rimz()
        .env("ZELLIJ", "1")
        .env("RIMZ_WORKSPACE_ID", workspace.workspace_id.as_str())
        .env("RIMZ_PROJECT_ROOT", &workspace.project_root)
        .bounded_output()
        .expect("run bare rimz");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        stderr,
        format!(
            "You're already in this directory's room (`{}`, zellij).\n",
            workspace.session_name
        )
    );
    assert!(output.stdout.is_empty());
    assert!(!env.rimz_home().join("config.toml").exists());
    assert!(
        !env.state_path_for(&env.project_root)
            .workspace_record
            .exists()
    );
}

#[test]
fn start_inside_another_session_reports_the_live_room_without_writing() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let log = env.home_root.join("zellij.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "start"])
        .env("ZELLIJ", "1")
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", &workspace.session_name)
        .bounded_output()
        .expect("run nested rimz start");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        stderr,
        format!(
            "This directory's room `{}` is running in another zellij session.\nDetach from this session, then run `rimz` here to enter it.\n",
            workspace.session_name
        )
    );
    assert!(output.stdout.is_empty());
    assert!(!env.rimz_home().join("config.toml").exists());
    assert!(
        !env.state_path_for(&env.project_root)
            .workspace_record
            .exists()
    );
    let trace = std::fs::read_to_string(log).expect("mux trace");
    assert!(
        trace
            .lines()
            .all(|line| line.split('\t').nth(1) == Some("list-sessions")),
        "{trace}"
    );
}

#[test]
fn session_lookup_skips_unrecorded_siblings_and_obeys_log_color() {
    let env = Env::new();
    let siblings = env.rimz_home().join("ws");
    let unrecorded = siblings.join("unrecorded");
    let corrupt = siblings.join("corrupt").join("workspace.json");
    std::fs::create_dir_all(&unrecorded).unwrap();
    std::fs::create_dir_all(corrupt.parent().unwrap()).unwrap();
    std::fs::write(&corrupt, "not JSON").unwrap();
    for (color, ansi) in [("auto", false), ("always", true), ("never", false)] {
        let mut command = env.rimz();
        command
            .args([
                "--color", color, "--mux", "zellij", "attach", "nosuch", "--print",
            ])
            .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("zellij.log"))
            .env_remove("CLICOLOR_FORCE")
            .env_remove("NO_COLOR");
        if color == "always" {
            command.env("NO_COLOR", "1");
        }
        let output = command
            .bounded_output()
            .expect("unknown session with sibling records");
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.contains('\x1b'), ansi, "{color}: {stderr}");
        assert_eq!(
            stderr
                .matches("skipping unreadable workspace record")
                .count(),
            1,
            "{stderr}"
        );
        assert!(stderr.contains(&corrupt.display().to_string()), "{stderr}");
        assert!(
            !stderr.contains(&unrecorded.display().to_string()),
            "{stderr}"
        );
    }
}

#[test]
fn attach_unknown_name_refuses_without_emitting_an_attach_command() {
    let env = Env::new();
    let log = env.home_root.join("zellij.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "attach", "nosuch", "--print"])
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &log)
        .bounded_output()
        .expect("run unknown attach");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: no room or zellij session named `nosuch`\n  all rooms: rimz list --all\n"
    );
    assert!(output.stdout.is_empty());
    let trace = std::fs::read_to_string(log).expect("mux trace");
    assert!(
        !trace
            .lines()
            .any(|line| line.split('\t').nth(1) == Some("attach")),
        "{trace}"
    );
    assert!(
        !env.state_path_for(&env.project_root)
            .workspace_record
            .exists()
    );
}

#[test]
fn attach_unknown_name_suggests_one_nearby_recorded_room() {
    let env = Env::new();
    env.record(&env.project_root);
    let paths = env.state_path_for(&env.project_root);
    let mut record = rimz::workspace::record::read(&paths.workspace_record).expect("read record");
    record.session_name = "agents-72ff".into();
    rimz::workspace::record::write(&paths, &record).expect("write named record");
    let before = std::fs::read(&paths.workspace_record).expect("record bytes");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "attach", "agnts-72ff", "--print"])
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("zellij.log"))
        .bounded_output()
        .expect("run misspelled attach");

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: no room or zellij session named `agnts-72ff`\n  did you mean `agents-72ff`?   all rooms: rimz list --all\n"
    );
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read(&paths.workspace_record).unwrap(), before);
}

#[test]
fn attach_unknown_name_preserves_a_record_lookup_error_as_its_cause() {
    let env = Env::new();
    let records = env.rimz_home().join("ws");
    std::fs::create_dir_all(&records).expect("records root");
    std::fs::remove_dir_all(&records).expect("remove records directory");
    std::fs::write(&records, b"not a directory").expect("block record lookup");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "attach", "nosuch", "--print"])
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("zellij.log"))
        .bounded_output()
        .expect("run attach with unreadable records");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no room or zellij session named `nosuch`"),
        "{stderr}"
    );
    assert!(stderr.contains("all rooms: rimz list --all"), "{stderr}");
    assert!(
        stderr.contains(&format!("reading {}", records.display())),
        "{stderr}"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn start_warns_and_records_an_unsupported_account_budget() {
    let env = Env::new();
    let config = env.rimz_home().join("config.toml");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("config dir");
    std::fs::write(config, "[accounts.budget]\nantigravity = \"50/day\"\n")
        .expect("machine config");
    let output = start_with_accounts(&env, "", &[], Some("budget warnings must permit birth"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("accounts.budget.antigravity"), "{stderr}");
    assert!(
        stderr.contains("authoritative account-level dollars"),
        "{stderr}"
    );
    assert_skipped(
        &env,
        &stderr,
        "accounts.budget.antigravity",
        "authoritative account-level dollars",
    );
}

#[test]
fn start_rejects_old_or_unrecognised_tmux_before_room_state() {
    let (maj, min, patch) = rimz::mux::tmux::MIN_TMUX_VERSION;
    for (version, message) in [
        ("tmux 3.4", "tmux 3.4 is below RimZ's floor".to_owned()),
        (
            "tmux next-3.6",
            "`tmux -V` output \"tmux next-3.6\" was not recognised".to_owned(),
        ),
    ] {
        let env = Env::new();
        let bin_dir = env.home_root.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("bin dir");
        let tmux = bin_dir.join("tmux");
        std::fs::write(
            &tmux,
            format!("#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = '-V' ]; then\n    printf '%s\\n' '{version}'\n  fi\ndone\n"),
        )
        .expect("tmux shim");
        std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755))
            .expect("chmod tmux shim");
        let workspace_state = env.state_path_for(&env.project_root).root;

        let output = env
            .rimz()
            .args(["start", "--mux", "tmux"])
            .env("PATH", &bin_dir)
            .bounded_output()
            .expect("run rimz start");

        assert!(!output.status.success(), "start accepted {version}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(&message), "{stderr}");
        assert!(
            stderr.contains(&format!(">= {maj}.{min}.{patch}")),
            "{stderr}"
        );
        assert!(stderr.contains("rimz start --mux zellij"), "{stderr}");
        assert!(
            !workspace_state.exists(),
            "tmux version preflight must run before room state is created"
        );
    }
}

#[test]
fn start_rejects_old_zellij_with_copyable_backend_alternative() {
    let env = Env::new();
    // Started by path from another directory, the copied fix must still name
    // the same room and account.
    let output = env
        .rimz()
        .current_dir(&env.home_root)
        .arg("start")
        .arg(&env.project_root)
        .args(["--mux", "zellij", "--account", "claude=work"])
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_VERSION", "0.43.0")
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("zellij.log"))
        .bounded_output()
        .expect("run start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("below RimZ's floor"), "{stderr}");
    let rerun = format!(
        "rimz start --mux tmux {} --account 'claude=work'",
        env.project_root.display()
    );
    assert!(stderr.contains(&rerun), "{stderr}");
    assert!(!env.state_path_for(&env.project_root).root.exists());
}

/// A detached birth judges every host again without requiring an attended choice.
#[test]
fn detached_birth_warns_and_waives_a_blocked_host_again() {
    let env = Env::new();
    env.record(&env.project_root);
    let record_path = env.state_path_for(&env.project_root).workspace_record;
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record_path).expect("read record"))
            .expect("record json");
    record["remote_control_off"] = serde_json::json!(["claude"]);
    std::fs::write(&record_path, record.to_string()).expect("write waiver");
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nshell = 'bash'\n[remote_control]\nclaude = true\n",
    )
    .expect("machine config");
    let settings = env.home_root.join("claude-settings.json");
    std::fs::write(&settings, r#"{ "disableRemoteControl": true }"#).expect("claude settings");
    let bin_dir = env.home_root.join("claude-bin");
    std::fs::create_dir_all(&bin_dir).expect("claude bin");
    let claude = bin_dir.join("claude");
    std::fs::write(&claude, "#!/bin/sh\nexit 1\n").expect("claude shim");
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755))
        .expect("chmod claude shim");
    let shell = bin_dir.join("bash");
    std::os::unix::fs::symlink("/bin/sh", &shell).unwrap();
    let added = env
        .rimz()
        .args(["loop", "add", "probe", "--check", "true", "--every", "1h"])
        .output()
        .expect("add loop");
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );

    let trace = env.home_root.join("zellij.log");
    let mut room = ShimRoom::watch(&env, &trace);
    room.allow_no_trigger("blocked remote control must permit birth");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "loop", "run", "probe"])
        .env("PATH", path_with_front(&bin_dir))
        .env("RIMZ_CLAUDE_SETTINGS", &settings)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_VERSION", "0.45.0")
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("zellij.log"))
        .bounded_output()
        .expect("run loop");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("disableRemoteControl"), "{stderr}");
    assert!(!stderr.contains("Choose [t/p/q]"), "{stderr}");
    assert_skipped(
        &env,
        &stderr,
        "remote_control.claude",
        "disableRemoteControl",
    );
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(record_path).unwrap()).unwrap();
    assert_eq!(record["remote_control_off"], serde_json::json!(["claude"]));
    assert_eq!(record.get("agent_shell"), None);
}

#[test]
fn start_warns_and_records_invalid_notifications() {
    let env = Env::new();
    let config = env.rimz_home().join("config.toml");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("config dir");
    std::fs::write(
        &config,
        "[[notifications.handler]]\nname = \"bad\"\ncommand = \"\"\n",
    )
    .expect("machine config");
    let output = start_with_accounts(
        &env,
        "",
        &[],
        Some("notification warnings must permit birth"),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains(&config.display().to_string()), "{stderr}");
    assert!(
        stderr.contains(
            "notification command in [notifications.handler #1 `bad`].command must not be empty"
        ),
        "{stderr}"
    );
    assert!(
        !stderr.contains("every setting in it is ignored and built-in defaults apply"),
        "{stderr}"
    );
    assert_skipped(&env, &stderr, "notifications", "command must not be empty");
}

#[test]
fn start_opens_the_room_when_the_invalid_notifications_table_is_switched_off() {
    let env = Env::new();
    write_machine_config(
        &env,
        "[notifications]\nenabled = false\n\n[[notifications.handler]]\nname = \"bad\"\ncommand = \"\"\n",
    );

    let output = start_with_accounts(&env, "", &[], None);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    // The lenient loader still warns and falls back, as it does at runtime; what
    // a switched-off table must not do is refuse the room.
    assert!(
        !stderr.contains("error: invalid per-machine notifications config"),
        "a switched-off table is a precondition of nothing: {stderr}"
    );
}

#[test]
fn start_warns_about_saved_untrusted_on_birth_and_reattach() {
    for live in [false, true] {
        let env = Env::new();
        let bin = seed_actionable_agent(&env);
        std::fs::copy(bin.join("agy"), bin.join("codex")).unwrap();
        let config = env.home_root.join("codex.toml");
        let original = format!(
            "[projects.{}]\ntrust_level = \"untrusted\"\n",
            toml::Value::String(env.project_root.display().to_string())
        );
        std::fs::write(&config, &original).unwrap();
        let workspace = env.resolve_workspace(&env.project_root);
        let sessions = if live {
            seed_sidebar_heartbeat(
                &env.runtime_paths(),
                rimz::MuxName::Zellij,
                &workspace.session_name,
                "live",
            );
            format!("{} [Created 1m ago]\n", workspace.session_name)
        } else {
            String::new()
        };
        let trace = env.project_root.join("restricted-start.log");
        let _room = (!live).then(|| ShimRoom::watch(&env, &trace));
        let mut command = env.rimz();
        configure_actionable_hooks(&mut command, &env, &bin, &trace, &sessions);
        let output = command
            .env("RIMZ_CODEX_CONFIG", &config)
            .bounded_output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert!(stderr.contains("saved untrusted"), "{stderr}");
        assert!(stderr.contains("Open restricted"), "{stderr}");
        assert!(!stderr.contains("rimz trust grant --agents"), "{stderr}");
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
    }
}

#[test]
fn start_checks_hooks_on_birth_but_not_live_reattach() {
    let birth = Env::new();
    std::fs::create_dir(birth.project_root.join(".git")).unwrap();
    std::fs::write(
        birth.project_root.join(".git/HEAD"),
        "ref: refs/heads/main\n",
    )
    .unwrap();
    let birth_bin = seed_actionable_agent(&birth);
    let codex = birth_bin.join("codex");
    std::fs::copy(birth_bin.join("agy"), &codex).unwrap();
    let config = birth.home_root.join("codex.toml");
    std::fs::write(&config, "").unwrap();
    let birth_workspace = birth.resolve_workspace(&birth.project_root);
    let birth_heartbeat = seed_sidebar_heartbeat(
        &birth.runtime_paths(),
        rimz::MuxName::Zellij,
        &birth_workspace.session_name,
        "birth",
    );
    let birth_trace = birth.project_root.join("zellij-birth.log");
    let _birth_room = ShimRoom::watch(&birth, &birth_trace);
    let mut birth_command = birth.rimz();
    configure_actionable_hooks(&mut birth_command, &birth, &birth_bin, &birth_trace, "");
    birth_command.env("RIMZ_CODEX_CONFIG", &config);
    let birth_output = birth_command
        .bounded_output()
        .expect("run absent-room start");
    assert!(
        birth_output.status.success(),
        "birth failed: {}",
        String::from_utf8_lossy(&birth_output.stderr)
    );
    let birth_stderr = String::from_utf8_lossy(&birth_output.stderr);
    assert!(
        birth
            .runtime_paths()
            .root
            .join("live/hibernate/admission.json")
            .exists()
    );
    assert!(birth_stderr.contains("RimZ found 2 coding agents: codex, antigravity."));
    assert!(
        birth_stderr.contains("codex has no trust decision"),
        "{birth_stderr}"
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), "");
    assert!(birth_stderr.contains("No terminal input — nothing installed or refreshed."));
    assert!(
        !birth_heartbeat.exists(),
        "fresh birth must purge prior heartbeat"
    );
    let birth_events = birth.read_events();
    assert_eq!(
        birth_events
            .iter()
            .filter(|event| matches!(event.kind(), rimz::store::event::EventKind::SessionRebirth))
            .count(),
        1,
        "fresh birth records one rebirth boundary"
    );
    let birth_trace = std::fs::read_to_string(&birth_trace).expect("read birth trace");
    let create = birth_trace
        .lines()
        .position(|line| line.contains("\tattach\t--create-background\t"))
        .expect("fresh session/sidebar create");
    let presence = birth_trace
        .lines()
        .position(|line| line.contains("\tpipe\t--plugin\t"))
        .expect("fresh presence pipe");
    assert!(
        create < presence,
        "sidebar create must precede presence:\n{birth_trace}"
    );
    assert_health_before_presence(&birth_trace);

    let live = Env::new();
    let live_bin = seed_actionable_agent(&live);
    let workspace = live.resolve_workspace(&live.project_root);
    let live_heartbeat = seed_sidebar_heartbeat(
        &live.runtime_paths(),
        rimz::MuxName::Zellij,
        &workspace.session_name,
        "live",
    );
    let sessions = format!("{} [Created 1m ago]\n", workspace.session_name);
    let live_trace = live.project_root.join("zellij-live.log");
    let mut live_command = live.rimz();
    configure_actionable_hooks(&mut live_command, &live, &live_bin, &live_trace, &sessions);
    let live_output = live_command.bounded_output().expect("run live-room start");
    assert!(
        live_output.status.success(),
        "reattach failed: {}",
        String::from_utf8_lossy(&live_output.stderr)
    );
    let live_stderr = String::from_utf8_lossy(&live_output.stderr);
    assert!(
        !live_stderr.contains("RimZ found"),
        "live reattach must skip hook detection: {live_stderr}"
    );
    assert!(
        !live_stderr.contains("No terminal input"),
        "live reattach must skip the hook fallback notice: {live_stderr}"
    );
    assert!(live_heartbeat.exists(), "live reattach preserves heartbeat");
    assert_eq!(
        live.read_events()
            .iter()
            .filter(|event| matches!(event.kind(), rimz::store::event::EventKind::SessionRebirth))
            .count(),
        0,
        "live reattach records no rebirth boundary"
    );
    let live_trace = std::fs::read_to_string(&live_trace).expect("read live trace");
    assert!(
        !live_trace.contains("\tattach\t--create-background\t"),
        "live reattach must not recreate session/sidebar:\n{live_trace}"
    );
    assert!(
        !live_trace.contains("\tdelete-all-sessions") && !live_trace.contains("\tkill-session"),
        "live reattach must not delete session:\n{live_trace}"
    );
    assert_health_before_presence(&live_trace);
}

#[test]
fn failed_recovery_park_refuses_birth_before_creating_a_session() {
    let env = Env::new();
    let bin = seed_actionable_agent(&env);
    let store = env.store();
    rimz::store::live_roster::publish(
        &store.paths().live_roster,
        [(rimz::ids::AgentKind::new_unchecked("claude"), "lost".into())].into(),
    )
    .expect("publish lost roster");
    let roster = std::fs::read(&store.paths().live_roster).unwrap();
    std::fs::create_dir_all(&store.paths().pending_recovery)
        .expect("block pending record publication");
    let trace = env.project_root.join("zellij.log");
    let mut room = ShimRoom::watch(&env, &trace);
    room.allow_no_trigger("a failed park refuses before creating the session");
    let mut command = env.rimz();
    configure_actionable_hooks(&mut command, &env, &bin, &trace, "");
    let output = command.arg("--no-resume").bounded_output().expect("start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "birth must fail when parking fails: {stderr}"
    );
    assert!(
        stderr.contains("park") && stderr.contains("retry"),
        "{stderr}"
    );
    assert_eq!(std::fs::read(&store.paths().live_roster).unwrap(), roster);
    let trace = std::fs::read_to_string(trace).unwrap();
    assert!(
        !trace.contains("\tattach\t--create-background\t"),
        "no session creation: {trace}"
    );
}

#[test]
fn unattended_start_without_resume_parks_lost_agents() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    use rimz::store::event::EventKind;

    let env = Env::new();
    let bin = seed_actionable_agent(&env);
    let store = env.store();
    let lost = (
        rimz::ids::AgentKind::new_unchecked("claude"),
        rimz::ids::AgentSessionId::from("lost"),
    );
    store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            store.paths().workspace_id.clone(),
            "rimz-test",
            lost.0.as_str(),
            "SessionStart",
            &AgentLifecycleObservation::new(Some(lost.1.clone()), LifecycleSignal::Registered),
        ))
        .expect("register lost agent");
    rimz::store::live_roster::publish(&store.paths().live_roster, [lost.clone()].into())
        .expect("publish the dead session's roster");
    let trace = env.project_root.join("zellij.log");
    let _room = ShimRoom::watch(&env, &trace);
    let mut command = env.rimz();
    configure_actionable_hooks(&mut command, &env, &bin, &trace, "");

    let output = command
        .arg("--no-resume")
        .bounded_output()
        .expect("run unattended start");

    assert!(
        output.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = env.read_events();
    let count = |wanted: fn(&EventKind) -> bool| {
        events.iter().filter(|event| wanted(&event.kind())).count()
    };
    assert_eq!(
        count(|kind| matches!(
            kind,
            EventKind::AgentLifecycle(payload)
                if matches!(payload.observation.signal, LifecycleSignal::Ended)
        )),
        0,
        "nobody was asked, so nobody is ended"
    );
    assert_eq!(count(|kind| matches!(kind, EventKind::SessionDeath(_))), 1);
    assert_eq!(count(|kind| matches!(kind, EventKind::SessionRebirth)), 1);
    assert!(!store.paths().live_roster.exists());
    let pending: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&store.paths().pending_recovery).expect("pending-recovery record"),
    )
    .expect("pending-recovery JSON");
    assert_eq!(pending["agents"], serde_json::json!([lost]));
}

#[test]
fn live_room_start_keeps_parked_agents_without_prompting() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    use rimz::store::event::EventKind;

    let env = Env::new();
    let bin_dir = seed_actionable_agent(&env);
    let workspace = env.resolve_workspace(&env.project_root);
    let store = env.store();
    let mut observation =
        AgentLifecycleObservation::new(Some("lost".into()), LifecycleSignal::Registered);
    observation.worktree_path = Some(
        env.project_root
            .join("missing-worktree")
            .display()
            .to_string(),
    );
    store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "SessionStart",
            &observation,
        ))
        .expect("register lost agent");
    let parked = serde_json::json!({"version": 1, "agents": [["claude", "lost"]]});
    std::fs::create_dir_all(
        store
            .paths()
            .pending_recovery
            .parent()
            .expect("records dir"),
    )
    .expect("records dir");
    std::fs::write(&store.paths().pending_recovery, parked.to_string()).expect("park lost agent");
    let pending = || -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(&store.paths().pending_recovery).expect("record"))
            .expect("pending-recovery JSON")
    };
    let ended = || {
        env.read_events()
            .iter()
            .filter_map(|event| match event.kind() {
                EventKind::AgentLifecycle(payload)
                    if matches!(payload.observation.signal, LifecycleSignal::Ended) =>
                {
                    payload.event_name.clone()
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let sessions = format!("{} [Created 1m ago]\n", workspace.session_name);
    let zellij_log = env.project_root.join("zellij-live.log");
    let _room = ShimRoom::watch(&env, &zellij_log);

    let mut unattended = env.rimz();
    configure_actionable_hooks(&mut unattended, &env, &bin_dir, &zellij_log, &sessions);
    let output = unattended.bounded_output().expect("run unattended start");
    assert!(
        output.status.success(),
        "unattended start failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(pending(), parked, "nobody was asked");
    assert_eq!(ended(), Vec::<String>::new());

    for no_resume in [false, true] {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open pty");
        let mut command = CommandBuilder::new(env.rimz_bin());
        env.pin_pty_command(&mut command);
        command.cwd(&env.project_root);
        command.args(["--mux", "zellij", "start", "--no-attach"]);
        if no_resume {
            command.arg("--no-resume");
        }
        command.env("PATH", &bin_dir);
        command.env("TERM", "dumb");
        command.env("RIMZ_PETS_OFFLINE", "1");
        command.env("RIMZ_ZELLIJ_BIN", zellij_trace_shim());
        command.env("RIMZ_TEST_ZELLIJ_LOG", &zellij_log);
        command.env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", &sessions);
        command.env("RIMZ_TEST_ZELLIJ_LIST_PANES", MATERIALIZED_ROOM_PANES);
        let mut child = pair
            .slave
            .spawn_command(command)
            .expect("spawn attended start");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
        let reader_thread = std::thread::spawn(move || {
            let mut output = Vec::new();
            let _ = reader.read_to_end(&mut output);
            output
        });
        let mut writer = pair.master.take_writer().expect("pty writer");
        std::io::Write::write_all(&mut writer, b"n\n").expect("finish any unexpected prompt");
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll attended start") {
                break Some(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        drop(writer);
        drop(pair.master);
        let output =
            String::from_utf8_lossy(&reader_thread.join().expect("join pty reader")).into_owned();
        let status = status.unwrap_or_else(|| {
            panic!("attended start did not finish within {COMMAND_TIMEOUT:?}:\n{output}")
        });
        assert!(status.success(), "attended start failed: {output}");
        assert!(!output.contains("Recover "), "{output}");
        assert!(!output.contains("Drop "), "{output}");
        assert!(!output.contains("cannot be resumed"), "{output}");
        if no_resume {
            assert_eq!(
                output
                    .matches("rimz: ended 1 parked agent (--no-resume)")
                    .count(),
                1,
                "{output}"
            );
            assert_eq!(ended(), ["rimz.recovery-declined"]);
            assert_eq!(pending()["agents"], serde_json::json!([]));
        } else {
            assert_eq!(
                output
                    .matches("rimz: 1 agent from an earlier session stays parked")
                    .count(),
                1,
                "{output}"
            );
            assert!(
                output.contains(
                    "rimz agents resume <lane> brings a lane back, rimz start --no-resume ends them"
                ),
                "{output}"
            );
            assert_eq!(pending(), parked);
            assert!(ended().is_empty());
        }
    }
    let events = env.read_events();
    assert!(
        !events.iter().any(|event| matches!(
            event.kind(),
            EventKind::SessionDeath(_) | EventKind::SessionRebirth
        )),
        "a live settlement writes no session event"
    );
}

#[test]
fn reconnect_marker_keeps_pty_start_unattended() {
    let env = Env::new();
    let bin_dir = seed_actionable_agent(&env);
    std::fs::copy(bin_dir.join("agy"), bin_dir.join("codex")).unwrap();
    env.install_agent_hooks("codex");
    let config = env.agent_config_path("codex");
    let original = format!(
        "{}\n[projects.{}]\ntrust_level = \"untrusted\"\n",
        std::fs::read_to_string(&config).unwrap(),
        toml::Value::String(env.project_root.display().to_string()),
    );
    std::fs::write(&config, &original).unwrap();
    let zellij_log = env.project_root.join("zellij-reconnect.log");
    let _room = ShimRoom::watch(&env, &zellij_log);
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");
    let mut command = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut command);
    command.cwd(&env.project_root);
    command.args(["--mux", "zellij", "start", "--no-attach"]);
    command.env("PATH", &bin_dir);
    command.env("TERM", "dumb");
    command.env("RIMZ_PETS_OFFLINE", "1");
    command.env("RIMZ_REMOTE_RECONNECT", "1");
    command.env("RIMZ_ZELLIJ_BIN", zellij_trace_shim());
    command.env("RIMZ_TEST_ZELLIJ_LOG", &zellij_log);
    command.env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", "");
    command.env("RIMZ_TEST_ZELLIJ_LIST_PANES", MATERIALIZED_ROOM_PANES);
    command.env(
        "RIMZ_ANTIGRAVITY_HOOKS",
        env.home_root.join("agent-config/hooks.json"),
    );
    command.env(
        "RIMZ_ANTIGRAVITY_SETTINGS",
        env.home_root.join("agent-config/settings.json"),
    );

    let mut child = pair
        .slave
        .spawn_command(command)
        .expect("spawn reconnect start");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
        output
    });
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll reconnect start") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    drop(pair.master);
    let output =
        String::from_utf8_lossy(&reader_thread.join().expect("join pty reader")).into_owned();
    let status = status.unwrap_or_else(|| {
        panic!("reconnect start did not finish within {COMMAND_TIMEOUT:?}:\n{output}")
    });
    assert!(status.success(), "reconnect start failed: {output}");
    assert!(
        output.contains("Not asked on a reconnect — nothing installed or refreshed."),
        "the hooks gate must explain the reconnect posture: {output}"
    );
    for lasting_notice in [
        "No terminal input",
        "hooks are installed but untrusted",
        "saved untrusted",
        "has no trust decision",
        "directory workspace rooted at",
        "marker-rooted workspace at",
    ] {
        assert!(
            !output.contains(lasting_notice),
            "{lasting_notice}: {output}"
        );
    }
    assert_eq!(std::fs::read_to_string(config).unwrap(), original);
    assert!(
        !output.contains("Install or refresh reporting hooks? [Y/n]"),
        "the reconnect must not prompt: {output}"
    );
    assert!(
        !output.contains("Trust this project's config on this machine?"),
        "the reconnect must not prompt for trust: {output}"
    );
}

#[test]
fn reconnect_marker_silences_undecided_folder_trust_without_a_terminal() {
    let env = Env::new();
    std::fs::create_dir(env.project_root.join(".git")).unwrap();
    std::fs::write(env.project_root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let bin = seed_actionable_agent(&env);
    std::fs::copy(bin.join("agy"), bin.join("codex")).unwrap();
    let config = env.home_root.join("codex.toml");
    std::fs::write(&config, "").unwrap();
    let trace = env.project_root.join("zellij-reconnect-no-tty.log");
    let _room = ShimRoom::watch(&env, &trace);
    let mut command = env.rimz();
    configure_actionable_hooks(&mut command, &env, &bin, &trace, "");
    let output = command
        .env("RIMZ_CODEX_CONFIG", &config)
        .env("RIMZ_REMOTE_RECONNECT", "1")
        .bounded_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("codex has no trust decision"), "{stderr}");
    assert!(stderr.contains("Not asked on a reconnect"), "{stderr}");
    assert!(!stderr.contains("No terminal input"), "{stderr}");
    assert_eq!(std::fs::read_to_string(config).unwrap(), "");
}

fn room_logins(env: &Env) -> Option<serde_json::Value> {
    let record = env.state_path_for(&env.project_root).workspace_record;
    let text = std::fs::read_to_string(record).ok()?;
    let record: serde_json::Value = serde_json::from_str(&text).expect("workspace record json");
    record.get("pins").cloned()
}

#[test]
fn start_refuses_a_held_old_layout_room_before_replacement() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let paths = env.state_path_for(&env.project_root);
    let runtime = env.runtime_paths();
    std::fs::create_dir_all(&runtime.root).unwrap();
    let old_runtime = runtime.root.join("old-runtime");
    std::fs::write(&old_runtime, b"old").unwrap();
    std::fs::create_dir_all(paths.root.join("messages")).unwrap();
    let record = serde_json::to_vec(&serde_json::json!({
        "workspace_id": workspace.workspace_id,
        "project_root": workspace.project_root,
        "session_name": "rimz-old-name",
        "updated_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    std::fs::write(&paths.workspace_record, &record).unwrap();
    let history = paths.root.join("events.log.jsonl");
    let messages = paths.root.join("messages/messages.jsonl");
    std::fs::write(&history, b"old history").unwrap();
    std::fs::write(&messages, b"old messages").unwrap();
    let _held = rimz::disk::lock::RoomLock::hold(&runtime.room_lock()).unwrap();

    let output = start_with_accounts(&env, "", &[], Some("held room must refuse before birth"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "held room must refuse: {stderr}");
    for text in [
        workspace.session_name.as_str(),
        "already held by another running room (another multiplexer, or a renamed session)",
        "RimZ keeps one room per project",
        "rimz attach",
        "rimz reset --no-start",
        "close it from that multiplexer",
    ] {
        assert!(stderr.contains(text), "missing {text}: {stderr}");
    }
    assert!(!stderr.contains("older RimZ"), "{stderr}");
    let trace = std::fs::read_to_string(env.project_root.join("zellij-accounts.log")).unwrap();
    for action in [
        "delete-session",
        "kill-session",
        "new-session",
        "--create-background",
    ] {
        assert!(!trace.contains(action), "replacement ran: {trace}");
    }
    assert!(paths.root.exists());
    assert!(runtime.root.exists());
    assert_eq!(std::fs::read(&paths.workspace_record).unwrap(), record);
    assert_eq!(std::fs::read(&history).unwrap(), b"old history");
    assert_eq!(std::fs::read(&messages).unwrap(), b"old messages");
    assert_eq!(std::fs::read(&old_runtime).unwrap(), b"old");
}

#[test]
fn start_replaces_an_old_layout_room_before_birth() {
    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let paths = env.state_path_for(&env.project_root);
    let runtime = env.runtime_paths();
    std::fs::create_dir_all(&runtime.root).unwrap();
    let old_runtime = runtime.root.join("old-runtime");
    std::fs::write(&old_runtime, b"old").unwrap();
    std::fs::create_dir_all(paths.root.join("messages")).unwrap();
    std::fs::write(
        &paths.workspace_record,
        serde_json::to_vec(&serde_json::json!({
            "workspace_id": workspace.workspace_id,
            "project_root": workspace.project_root,
            "session_name": "rimz-old-name",
            "updated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(paths.root.join("events.log.jsonl"), b"old history").unwrap();
    std::fs::write(paths.root.join("messages/messages.jsonl"), b"old messages").unwrap();

    let output = start_with_accounts(&env, "", &[], None);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let notice = format!(
        "rimz: room {} was written by an older RimZ (layout 1); it was torn down and this project starts with a fresh room. Its history was not carried over.",
        paths.dir_name
    );
    assert_eq!(stderr.matches(&notice).count(), 1, "{stderr}");
    let paths = env.state_path_for(&env.project_root);
    assert_eq!(
        rimz::workspace::record::read(&paths.workspace_record)
            .unwrap()
            .layout,
        2
    );
    assert!(!paths.root.join("events.log.jsonl").exists());
    assert!(!paths.root.join("messages/messages.jsonl").exists());
    assert!(!old_runtime.exists());
    let trace = std::fs::read_to_string(env.project_root.join("zellij-accounts.log")).unwrap();
    for session in [&workspace.session_name, &"rimz-old-name".to_owned()] {
        let killed = trace
            .lines()
            .position(|line| line.contains("delete-session") && line.contains(session))
            .expect("old session deleted");
        let born = trace
            .lines()
            .position(|line| line.contains("--create-background"))
            .expect("fresh room born");
        assert!(killed < born, "{trace}");
    }
}

pub(super) fn start_with_accounts(
    env: &Env,
    sessions: &str,
    accounts: &[&str],
    no_trigger_reason: Option<&str>,
) -> std::process::Output {
    let bin_dir = seed_actionable_agent(env);
    let trace = env.project_root.join("zellij-accounts.log");
    let mut room = ShimRoom::watch(env, &trace);
    if let Some(reason) = no_trigger_reason {
        room.allow_no_trigger(reason);
    }
    let mut command = env.rimz();
    configure_actionable_hooks(&mut command, env, &bin_dir, &trace, sessions);
    for account in accounts {
        command.args(["--account", account]);
    }
    command.bounded_output().expect("run rimz start")
}

fn write_machine_config(env: &Env, text: &str) {
    let config = env.rimz_home().join("config.toml");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("config dir");
    std::fs::write(config, text).expect("machine config");
}

pub(super) fn assert_skipped(env: &Env, stderr: &str, setting: &str, detail: &str) {
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(env.state_path_for(&env.project_root).workspace_record).unwrap(),
    )
    .unwrap();
    assert!(
        record["skipped"].is_array(),
        "missing recorded warnings: {record}"
    );
    let entry = record["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["setting"] == setting)
        .expect("recorded setting");
    let message = entry["message"].as_str().unwrap();
    assert!(message.contains(detail), "{entry}");
    assert_eq!(
        stderr.matches(&format!("rimz: warning: {message}")).count(),
        1,
        "{stderr}"
    );
}

#[test]
fn start_refuses_named_accounts_it_cannot_launch_into() {
    let env = Env::new();
    let home = env.home_root.join("accounts/work");
    write_machine_config(
        &env,
        &format!("[accounts.claude.work]\nhome = \"{}\"\n", home.display()),
    );

    let unknown = start_with_accounts(
        &env,
        "",
        &["claude=travel"],
        Some("unknown account refuses before room birth"),
    );
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(!unknown.status.success(), "{stderr}");
    assert!(
        stderr.contains("unknown claude account `travel`; configured: default, work; run `rimz accounts add claude travel`"),
        "{stderr}"
    );

    let missing = start_with_accounts(
        &env,
        "",
        &["claude=work"],
        Some("missing account home refuses before room birth"),
    );
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(!missing.status.success(), "{stderr}");
    assert!(
        stderr.contains("is not a directory; run `rimz accounts add claude work`"),
        "{stderr}"
    );

    std::fs::create_dir_all(&home).expect("account home");
    let unhooked = start_with_accounts(
        &env,
        "",
        &["claude=work"],
        Some("missing account hooks refuse before room birth"),
    );
    let stderr = String::from_utf8_lossy(&unhooked.stderr);
    assert!(!unhooked.status.success(), "{stderr}");
    assert!(
        stderr.contains("RimZ hooks are missing for claude account `work`"),
        "{stderr}"
    );
    assert_eq!(room_logins(&env), None, "a refused start freezes nothing");
}

#[test]
fn start_pins_only_requested_accounts_until_reset() {
    let env = Env::new();
    write_machine_config(&env, "[accounts.claude.work]\n");
    let born = start_with_accounts(&env, "", &["claude=default"], None);
    assert!(
        born.status.success(),
        "birth failed: {}",
        String::from_utf8_lossy(&born.stderr)
    );
    let frozen = serde_json::json!({"claude": "default"});
    assert_eq!(room_logins(&env), Some(frozen.clone()));

    let workspace = env.resolve_workspace(&env.project_root);
    let live = format!("{} [Created 1m ago]\n", workspace.session_name);
    for sessions in ["", live.as_str()] {
        let refused = start_with_accounts(
            &env,
            sessions,
            &["claude=work"],
            Some("frozen room accounts refuse before room birth"),
        );
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(!refused.status.success(), "{stderr}");
        assert!(
            stderr.contains("this room uses claude account `default`, not `work`; switch it with `rimz accounts use claude work`"),
            "{stderr}"
        );
    }

    let reborn = start_with_accounts(
        &env,
        "",
        &[],
        Some("cached live topology needs neither room birth nor a topology dump"),
    );
    assert!(
        reborn.status.success(),
        "rebirth failed: {}",
        String::from_utf8_lossy(&reborn.stderr)
    );
    assert_eq!(room_logins(&env), Some(frozen));
}

#[test]
fn start_warns_and_records_untrusted_project_accounts_without_pinning_them() {
    let env = Env::new();
    let project_config = env.project_root.join(".rimz/config.toml");
    std::fs::create_dir_all(project_config.parent().expect("project config dir"))
        .expect("mkdir .rimz");
    std::fs::write(&project_config, "[accounts]\ncodex = \"default\"\n").expect("project config");

    let untrusted =
        start_with_accounts(&env, "", &[], Some("untrusted accounts must permit birth"));
    let stderr = String::from_utf8_lossy(&untrusted.stderr);
    assert!(untrusted.status.success(), "{stderr}");
    assert!(
        stderr.contains("project account selections in .rimz/config.toml are untrusted"),
        "{stderr}"
    );
    assert!(stderr.contains("rimz trust grant"), "{stderr}");
    assert_skipped(&env, &stderr, "accounts.codex", "untrusted");
    assert_eq!(room_logins(&env), None);

    // `attach` births a room that is not live under the same rule.
    let bin_dir = seed_actionable_agent(&env);
    let attach_log = env.project_root.join("zellij-attach.log");
    let _room = ShimRoom::watch(&env, &attach_log);
    let attach = env
        .rimz()
        .args(["--mux", "zellij", "attach", "--print"])
        .env("PATH", &bin_dir)
        .env("TERM", "dumb")
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &attach_log)
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", "")
        .bounded_output()
        .expect("run rimz attach");
    let stderr = String::from_utf8_lossy(&attach.stderr);
    assert!(attach.status.success(), "{stderr}");
    assert!(
        stderr.contains("project account selections in .rimz/config.toml are untrusted"),
        "{stderr}"
    );
    assert_eq!(room_logins(&env), None);
    assert_skipped(&env, &stderr, "accounts.codex", "untrusted");

    env.rimz()
        .args(["trust", "grant"])
        .bounded_output()
        .expect("trust grant");
    let trusted = start_with_accounts(
        &env,
        "",
        &[],
        Some("cached live topology needs neither room birth nor a topology dump"),
    );
    assert!(
        trusted.status.success(),
        "trusted start failed: {}",
        String::from_utf8_lossy(&trusted.stderr)
    );
    assert_eq!(room_logins(&env), None);
    let record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(env.state_path_for(&env.project_root).workspace_record).unwrap(),
    )
    .unwrap();
    assert!(
        record.get("skipped").is_none(),
        "a new judgment replaces the warnings: {record}"
    );
}

#[test]
fn start_warns_about_a_configured_missing_account_and_launch_refuses_it() {
    let env = Env::new();
    let home = env.home_root.join("accounts/work");
    write_machine_config(
        &env,
        &format!(
            "[accounts.claude.work]\nhome = \"{}\"\n[accounts.use]\nclaude = \"work\"\n",
            home.display(),
        ),
    );
    let output = start_with_accounts(
        &env,
        "",
        &[],
        Some("unavailable accounts must permit birth"),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_skipped(
        &env,
        &stderr,
        "accounts.claude.work",
        "rimz accounts add claude work",
    );
    let bin = seed_actionable_agent(&env);
    crate::common::write_path_shim(&bin, "claude", "exit 0");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "agents", "claude"])
        .env("PATH", &bin)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!(
                "{} [Created 1s ago]\n",
                env.resolve_workspace(&env.project_root).session_name
            ),
        )
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", MATERIALIZED_ROOM_PANES)
        .bounded_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("rimz accounts add claude work"), "{stderr}");
    assert!(!home.exists(), "no empty account home is created");
}

#[test]
fn start_states_a_missing_account_once_when_its_remote_control_is_waived() {
    let env = Env::new();
    let home = env.home_root.join("accounts/work");
    write_machine_config(
        &env,
        &format!(
            "[accounts.claude.work]\nhome = \"{}\"\n[accounts.use]\nclaude = \"work\"\n[remote_control]\nclaude = true\n",
            home.display(),
        ),
    );
    let output = start_with_accounts(
        &env,
        "",
        &[],
        Some("unavailable accounts must permit birth"),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_skipped(
        &env,
        &stderr,
        "accounts.claude.work",
        "rimz accounts add claude work",
    );
    assert_skipped(
        &env,
        &stderr,
        "remote_control.claude",
        "see accounts.claude.work",
    );
    assert_eq!(
        stderr.matches("rimz accounts add claude work").count(),
        1,
        "{stderr}"
    );
}

type LostAgent = (rimz::ids::AgentKind, rimz::ids::AgentSessionId);

/// A dead tmux room's one rostered `claude` agent, and the PATH resolving its
/// shim. Its runtime owner is dead, so only the pending record stands between
/// the agent and the reap.
fn seed_lost_tmux_agent(env: &Env) -> (LostAgent, std::ffi::OsString) {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};

    let agent_path = host_claude_path(env);
    let workspace = env.resolve_workspace(&env.project_root);
    let store = env.store();
    let worktree = env.project_root.join("alpha");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let lost = (
        rimz::ids::AgentKind::new_unchecked("claude"),
        rimz::ids::AgentSessionId::from("alpha"),
    );
    crate::common::write_claude_conversation(env, &worktree, lost.1.as_str());
    // Published first: the registering commit below already runs the reap.
    rimz::store::live_roster::publish(&store.paths().live_roster, [lost.clone()].into())
        .expect("publish the dead session's roster");
    let mut observation =
        AgentLifecycleObservation::new(Some("alpha".into()), LifecycleSignal::Registered);
    observation.agent_name = Some("alpha".into());
    observation.worktree_path = Some(worktree.display().to_string());
    observation.worktree_branch = Some("alpha".into());
    observation.runtime_owner = Some(rimz::pane::RuntimeOwner::new(
        rimz::pane::RuntimeOwnerKind::Agent,
        "alpha",
        u32::MAX,
        None,
    ));
    store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "SessionStart",
            &observation,
        ))
        .expect("register lost agent");
    (lost, agent_path)
}

/// Host-isolation machine config and a PATH whose `claude` sleeps until killed.
fn host_claude_path(env: &Env) -> std::ffi::OsString {
    env.install_agent_hooks("claude");
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = 'host'\n",
    )
    .expect("machine config");
    let bin = env.home_root.join("recovery-bin");
    std::fs::create_dir_all(&bin).expect("agent bin");
    let claude = bin.join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\nif [ \"$1\" = --version ]; then echo '2.1.0'; exit 0; fi\nexec sleep 600\n",
    )
    .expect("claude shim");
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755))
        .expect("chmod claude shim");
    path_with_front(&bin)
}

/// `agent_path` behind a `tmux` that runs `arms` (shell `case` arms over its
/// space-padded argv) before handing the command to the real one.
fn path_with_tmux_wrapper(
    env: &Env,
    tmux: &Path,
    agent_path: &std::ffi::OsStr,
    arms: &str,
) -> std::ffi::OsString {
    let dir = env.home_root.join("wrapped-tmux");
    std::fs::create_dir_all(&dir).expect("tmux wrapper dir");
    let wrapper = dir.join("tmux");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \" $* \" in {arms} esac\nexec {} \"$@\"\n",
            shlex::try_quote(tmux.to_str().expect("tmux path is UTF-8"))
                .expect("quote tmux executable")
        ),
    )
    .expect("tmux wrapper");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
        .expect("chmod tmux wrapper");
    std::env::join_paths(std::iter::once(dir).chain(std::env::split_paths(agent_path)))
        .expect("join PATH")
}

/// Run an attended tmux `start --no-attach` that answers yes to its prompt.
fn attended_tmux_start_accepting(env: &Env, path: &std::ffi::OsStr) -> String {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");
    let mut command = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut command);
    command.cwd(&env.project_root);
    command.args(["--mux", "tmux", "start", "--no-attach"]);
    command.env("PATH", path);
    command.env("TERM", "dumb");
    let mut child = pair
        .slave
        .spawn_command(command)
        .expect("spawn attended start");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
        output
    });
    let mut writer = pair.master.take_writer().expect("pty writer");
    std::io::Write::write_all(&mut writer, b"y\n").expect("accept the recovery");
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll attended start") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    drop(writer);
    drop(pair.master);
    let output =
        String::from_utf8_lossy(&reader_thread.join().expect("join pty reader")).into_owned();
    let status = status.unwrap_or_else(|| {
        panic!("attended start did not finish within {COMMAND_TIMEOUT:?}:\n{output}")
    });
    assert!(status.success(), "attended start failed: {output}");
    output
}

/// The consumer proof for a birth whose resume window never opened: the agent
/// survives the reap and stays parked when the next attended start attaches.
#[test]
fn birth_keeps_agents_pending_when_their_resume_window_does_not_open() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    use rimz::store::event::EventKind;

    let Ok(tmux) = which::which("tmux") else {
        crate::common::skip("tmux not on PATH");
        return;
    };
    let env = Env::new();
    let (lost, agent_path) = seed_lost_tmux_agent(&env);
    let workspace = env.resolve_workspace(&env.project_root);
    let store = env.store();
    let pending = || -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(&store.paths().pending_recovery).expect("record"))
            .expect("pending-recovery JSON")
    };
    let ended = || {
        env.read_events()
            .iter()
            .filter(|event| {
                matches!(
                    event.kind(),
                    EventKind::AgentLifecycle(payload)
                        if matches!(payload.observation.signal, LifecycleSignal::Ended)
                )
            })
            .count()
    };
    let seeded = env.read_events().len();
    // Every event from the first birth on, so a failing `ended()` names its
    // writer and places it against the resume.
    let since_birth = || {
        env.read_events()
            .iter()
            .skip(seeded)
            .map(|event| {
                let (name, signal, agent, owner_pid) = match event.kind() {
                    EventKind::AgentLifecycle(payload) => (
                        payload.event_name,
                        Some(payload.observation.signal),
                        payload.observation.agent_id,
                        payload.observation.runtime_owner.map(|owner| owner.pid),
                    ),
                    EventKind::AgentAttach(payload) => (
                        None,
                        None,
                        Some(payload.agent_id),
                        Some(payload.runtime_owner.pid),
                    ),
                    _ => (None, None, None, None),
                };
                format!(
                    "{} {} event={name:?} signal={signal:?} agent={agent:?} owner_pid={owner_pid:?}",
                    event.timestamp, event.method
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let birth = env
        .rimz()
        .env(
            "PATH",
            // Fails the one command that opens a `#channel` resume window.
            path_with_tmux_wrapper(
                &env,
                &tmux,
                &agent_path,
                r#"*" new-window "*" rename-window "*" ##"*) exit 1 ;;"#,
            ),
        )
        .args(["--mux", "tmux", "start", "--no-attach"])
        .bounded_output()
        .expect("run the birth");
    let stderr = String::from_utf8_lossy(&birth.stderr);
    assert!(
        birth.status.success(),
        "an unopened tab is a warning: {stderr}"
    );
    assert!(
        stderr.contains("could not open resumed tab #alpha")
            && stderr.contains("stay pending for a later rebirth or explicit resume"),
        "{stderr}"
    );
    assert!(!stderr.contains("rimz: resumed"), "{stderr}");
    assert_eq!(pending()["agents"], serde_json::json!([lost]));

    // The next commit past the debounce stamp runs the reap.
    let _ = std::fs::remove_file(store.paths().cache_dir.join("dead-reap.stamp"));
    store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "SessionStart",
            &AgentLifecycleObservation::new(Some("bystander".into()), LifecycleSignal::Registered),
        ))
        .expect("a commit that drives the reap");
    assert_eq!(pending()["agents"], serde_json::json!([lost]));
    assert_eq!(
        ended(),
        0,
        "the reap leaves a pending agent alone; events since the first birth:\n{}",
        since_birth()
    );

    let output = attended_tmux_start_accepting(&env, &agent_path);
    assert!(!output.contains("Recover "), "{output}");
    assert!(!output.contains("rimz: resumed"), "{output}");
    assert!(
        output.contains("1 agent from an earlier session stays parked (#alpha)"),
        "{output}"
    );
    assert_eq!(pending()["agents"], serde_json::json!([lost]));
    let ended_after_start = ended();
    if ended_after_start != 0 {
        // Name what the pane printed and the room recorded, so a CI failure
        // explains an unexpected end despite pending membership.
        let rimz_out = |args: &[&str]| {
            env.rimz()
                .env("PATH", &agent_path)
                .args(["--mux", "tmux"])
                .args(args)
                .bounded_output()
                .expect("run a forensic rimz command")
        };
        let pane_list = rimz_out(&["pane", "list", "--json"]);
        let panes = format!(
            "{}{}",
            String::from_utf8_lossy(&pane_list.stdout),
            String::from_utf8_lossy(&pane_list.stderr)
        );
        let captures = match serde_json::from_slice::<serde_json::Value>(&pane_list.stdout) {
            Ok(list) => list["tabs"]
                .as_array()
                .expect("pane list tabs")
                .iter()
                .flat_map(|tab| tab["panes"].as_array().expect("tab panes"))
                .map(|pane| {
                    let id = pane["pane_id"].as_str().expect("pane id");
                    let captured = rimz_out(&["pane", "capture", id]);
                    format!(
                        "{id}:\n{}{}",
                        String::from_utf8_lossy(&captured.stdout),
                        String::from_utf8_lossy(&captured.stderr)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Err(error) => format!("could not decode pane list: {error}"),
        };
        panic!(
            "the live start ends nobody; events since the first birth:\n{}\n\
             attended start output:\n{output}\npanes:\n{panes}\npane captures:\n{captures}\n\
             diagnostics:\n{}",
            since_birth(),
            env.diag_tail(&workspace.session_name, 40),
        );
    }
}

/// An agent left pending in a live room and brought back by hand is a normal
/// agent again once its wrapper takes the pane: closing that pane ends it.
#[test]
fn closing_an_agent_resumed_while_pending_ends_it() {
    use rimz::agents::LifecycleSignal;
    use rimz::store::event::EventKind;

    let Ok(tmux) = which::which("tmux") else {
        crate::common::skip("tmux not on PATH");
        return;
    };
    let env = Env::new();
    // The hand resume runs the launch account check, which needs the hooks.
    env.install_agent_hooks("claude");
    let (lost, agent_path) = seed_lost_tmux_agent(&env);
    let store = env.store();
    let pending = || -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(&store.paths().pending_recovery).expect("record"))
            .expect("pending-recovery JSON")
    };
    let birth = env
        .rimz()
        .env(
            "PATH",
            path_with_tmux_wrapper(
                &env,
                &tmux,
                &agent_path,
                r#"*" new-window "*" rename-window "*" ##"*) exit 1 ;;"#,
            ),
        )
        .args(["--mux", "tmux", "start", "--no-attach"])
        .bounded_output()
        .expect("run the birth");
    assert!(
        birth.status.success(),
        "{}",
        String::from_utf8_lossy(&birth.stderr)
    );
    assert_eq!(pending()["agents"], serde_json::json!([lost]));
    let seeded = env.read_events().len();

    let resumed = env
        .rimz()
        .env("PATH", &agent_path)
        .args(["--mux", "tmux", "agents", "resume", "#alpha"])
        .bounded_output()
        .expect("resume the pending agent");
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    // Each of alpha's attaches since the birth: the pane, the wrapper's pid,
    // and the runtime owner's pid. The wrapper binds its own pid first, then
    // the provider's pid after the spawn.
    let attaches = || {
        env.read_events()
            .into_iter()
            .skip(seeded)
            .filter_map(|event| match event.kind() {
                EventKind::AgentAttach(payload) if payload.agent_id.as_str() == "alpha" => {
                    Some((payload.pane_id, payload.pane_pid, payload.runtime_owner.pid))
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    // The wrapper's first attach takes alpha out of pending recovery only after
    // its commit; the provider's attach follows that, so the close counts as
    // deliberate.
    let supervising_pane = || {
        attaches()
            .into_iter()
            .find(|(_, wrapper, owner)| *wrapper != Some(*owner))
            .map(|(pane, _, _)| pane)
    };
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let pane = loop {
        if let Some(pane) = supervising_pane()
            && pending()["agents"] == serde_json::json!([])
        {
            break pane;
        }
        assert!(
            Instant::now() < deadline,
            "the resumed wrapper never attached its provider; attaches (pane, wrapper pid, owner pid): {:?}; pending: {}",
            attaches(),
            pending()
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(pending()["agents"], serde_json::json!([]));

    let socket = rimz::mux::tmux::managed_server_socket_path_under(&env.runtime_root);
    let killed = std::process::Command::new(&tmux)
        .arg("-S")
        .arg(&socket)
        .args(["kill-pane", "-t", pane.raw()])
        .output()
        .expect("run tmux kill-pane");
    assert!(
        killed.status.success(),
        "{}",
        String::from_utf8_lossy(&killed.stderr)
    );
    let ended = || {
        env.read_events().into_iter().skip(seeded).any(|event| {
            matches!(
                event.kind(),
                EventKind::AgentLifecycle(payload)
                    if matches!(payload.observation.signal, LifecycleSignal::Ended)
                        && payload.event_name.as_deref() == Some("rimz.agent-ended")
                        && payload.observation.agent_id.as_ref().map(|id| id.as_str())
                            == Some("alpha")
            )
        })
    };
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    while !ended() {
        assert!(
            Instant::now() < deadline,
            "closing the resumed agent's pane never ended it; attaches (pane, wrapper pid, owner pid): {:?}; pending: {}",
            attaches(),
            pending()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A supervising wrapper owes the store an end from its first pane binding:
/// a pane closed right after the wrapper's own attach still settles through
/// `rimz.agent-ended`, never left to the reaper's inference.
#[test]
fn closing_a_pane_at_its_wrappers_first_attach_ends_the_agent() {
    use rimz::agents::LifecycleSignal;
    use rimz::store::event::EventKind;

    let Ok(tmux) = which::which("tmux") else {
        crate::common::skip("tmux not on PATH");
        return;
    };
    let env = Env::new();
    // The launch runs the account check, which needs the hooks.
    env.install_agent_hooks("claude");
    let agent_path = host_claude_path(&env);
    let birth = env
        .rimz()
        .env("PATH", &agent_path)
        .args(["--mux", "tmux", "start", "--no-attach"])
        .bounded_output()
        .expect("run the birth");
    assert!(
        birth.status.success(),
        "{}",
        String::from_utf8_lossy(&birth.stderr)
    );
    let seeded = env.read_events().len();
    let events = || {
        env.read_events()
            .iter()
            .skip(seeded)
            .map(|event| {
                let (name, signal, agent, owner_pid) = match event.kind() {
                    EventKind::AgentLifecycle(payload) => (
                        payload.event_name,
                        Some(payload.observation.signal),
                        payload.observation.agent_id,
                        payload.observation.runtime_owner.map(|owner| owner.pid),
                    ),
                    EventKind::AgentAttach(payload) => (
                        None,
                        None,
                        Some(payload.agent_id),
                        Some(payload.runtime_owner.pid),
                    ),
                    _ => (None, None, None, None),
                };
                format!(
                    "{} {} event={name:?} signal={signal:?} agent={agent:?} owner_pid={owner_pid:?}",
                    event.timestamp, event.method
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let launched = env
        .rimz()
        .env("PATH", &agent_path)
        .args(["--mux", "tmux", "agents", "claude"])
        .bounded_output()
        .expect("launch the agent");
    assert!(
        launched.status.success(),
        "{}",
        String::from_utf8_lossy(&launched.stderr)
    );
    // The wrapper's own attach: it names the wrapper as the pane's owner.
    let wrapper_attach = || {
        env.read_events()
            .into_iter()
            .skip(seeded)
            .find_map(|event| match event.kind() {
                EventKind::AgentAttach(payload)
                    if payload.pane_pid == Some(payload.runtime_owner.pid) =>
                {
                    Some((payload.agent_id, payload.pane_id))
                }
                _ => None,
            })
    };
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let (agent, pane) = loop {
        if let Some(attach) = wrapper_attach() {
            break attach;
        }
        assert!(
            Instant::now() < deadline,
            "the wrapper never attached its pane; events since the birth:\n{}",
            events()
        );
        std::thread::sleep(Duration::from_millis(1));
    };

    let socket = rimz::mux::tmux::managed_server_socket_path_under(&env.runtime_root);
    let killed = std::process::Command::new(&tmux)
        .arg("-S")
        .arg(&socket)
        .args(["kill-pane", "-t", pane.raw()])
        .output()
        .expect("run tmux kill-pane");
    assert!(
        killed.status.success(),
        "{}",
        String::from_utf8_lossy(&killed.stderr)
    );
    let end = || {
        env.read_events()
            .into_iter()
            .skip(seeded)
            .find_map(|event| match event.kind() {
                EventKind::AgentLifecycle(payload)
                    if matches!(payload.observation.signal, LifecycleSignal::Ended)
                        && payload.observation.agent_id.as_ref() == Some(&agent) =>
                {
                    Some(payload.event_name)
                }
                _ => None,
            })
    };
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let ended_by = loop {
        if let Some(name) = end() {
            break name;
        }
        assert!(
            Instant::now() < deadline,
            "closing the pane at the wrapper's first attach never ended {agent}; events since the birth:\n{}",
            events()
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(
        ended_by.as_deref(),
        Some("rimz.agent-ended"),
        "the wrapper must settle its own end; events since the birth:\n{}",
        events()
    );
}

/// A birth holds the recovery lock until it has confirmed its resume windows,
/// so an attended start on the room it just made live cannot settle the same
/// still-pending agents into a second window.
#[test]
fn attended_start_waits_for_a_birth_to_confirm_its_resume_window() {
    let Ok(tmux) = which::which("tmux") else {
        crate::common::skip("tmux not on PATH");
        return;
    };
    let env = Env::new();
    let (_lost, agent_path) = seed_lost_tmux_agent(&env);
    let reached = env.home_root.join("resume-window-reached");
    let release = env.home_root.join("resume-window-release");
    // Parks the birth where it opens the resume window: settled, the session
    // live, and the agent not yet running in a pane.
    let arms = format!(
        r#"*" new-window "*" rename-window "*" ##"*) : > {}; until [ -e {} ]; do sleep 0.05; done ;;"#,
        shlex::try_quote(reached.to_str().expect("UTF-8 path")).expect("quote"),
        shlex::try_quote(release.to_str().expect("UTF-8 path")).expect("quote"),
    );
    let mut birth = env.rimz();
    birth
        .env(
            "PATH",
            path_with_tmux_wrapper(&env, &tmux, &agent_path, &arms),
        )
        .args(["--mux", "tmux", "start", "--no-attach"]);
    let birth = std::thread::spawn(move || birth.bounded_output().expect("run the birth"));
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    while !reached.exists() {
        if Instant::now() >= deadline || birth.is_finished() {
            let birth = birth.join().expect("join the birth");
            panic!(
                "birth never reached its resume window: {}",
                String::from_utf8_lossy(&birth.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let output = std::thread::scope(|scope| {
        let attended = scope.spawn(|| attended_tmux_start_accepting(&env, &agent_path));
        // Long enough for an unserialized start to settle on its own.
        let unserialized = Instant::now() + Duration::from_secs(5);
        while !attended.is_finished() && Instant::now() < unserialized {
            std::thread::sleep(Duration::from_millis(25));
        }
        std::fs::write(&release, "").expect("release the birth");
        attended.join().expect("attended start")
    });
    let birth = birth.join().expect("join the birth");
    let stderr = String::from_utf8_lossy(&birth.stderr);
    assert!(birth.status.success(), "{stderr}");
    assert!(stderr.contains("rimz: resumed 1 agent: #alpha"), "{stderr}");
    assert!(
        !output.contains("rimz: resumed"),
        "the birth already resumed the agent: {output}"
    );
    let pending: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&env.store().paths().pending_recovery).expect("record"),
    )
    .expect("pending-recovery JSON");
    assert_eq!(pending["agents"], serde_json::json!([]));
}
