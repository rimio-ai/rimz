//! Integration coverage for `rimz config` and the conservative `rimz setup`.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use assert_cmd::assert::OutputAssertExt;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

use crate::common::Env;

const PI_EXTENSION_SOURCE: &str = include_str!("../../src/agents/adapters/pi/extension.ts");
const OPENCODE_PLUGIN_SOURCE: &str = include_str!("../../src/agents/adapters/opencode/plugin.ts");
const STALE_MANAGED_SOURCE: &str = "// old _rimz_managed source\n";

fn machine_config_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("config.toml")
}

fn theme_config_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("theme.toml")
}

fn legacy_agents_config_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("agents.toml")
}

fn loop_config_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("loop.toml")
}

fn write_machine_file(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("config file parent")).expect("mkdir config");
    std::fs::write(path, text).expect("write config seed");
}

#[test]
fn config_worktree_hooks_round_trip() {
    let env = Env::new();
    for event in ["created", "removed"] {
        let key = format!("agents.worktree.hooks.{event}");
        env.rimz()
            .args(["config", "set", &key, "echo hook"])
            .assert()
            .success();
        env.rimz()
            .args(["config", "get", &key])
            .assert()
            .success()
            .stdout(contains("echo hook"));
    }
}

#[test]
#[cfg(target_os = "linux")]
fn config_set_sandbox_probes_bwrap_before_writing() {
    let env = Env::new();
    let path = machine_config_path(&env);
    let seed = "[agents]\nisolation = \"host\"\n";
    write_machine_file(&path, seed);
    let empty_bin = env.home_root.join("empty-bin");
    std::fs::create_dir(&empty_bin).expect("mkdir empty PATH");
    for value in ["sandbox", "\"sandbox\"", "'sandbox'", "\"sand\\u0062ox\""] {
        env.rimz()
            .args(["config", "set", "agents.isolation", value])
            .env("PATH", &empty_bin)
            .assert()
            .failure()
            .stderr(contains("bubblewrap"))
            .stderr(contains("host"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), seed);
    }
    let bwrap = empty_bin.join("bwrap");
    let marker = env.home_root.join("bwrap-probed");
    std::fs::write(
        &bwrap,
        "#!/bin/sh\n: > \"$RIMZ_TEST_BWRAP_PROBE\"\nexit 0\n",
    )
    .expect("write working bwrap shim");
    std::fs::set_permissions(&bwrap, std::fs::Permissions::from_mode(0o755))
        .expect("chmod bwrap shim");
    env.rimz()
        .args(["config", "set", "agents.isolation", "sandbox"])
        .env("PATH", &empty_bin)
        .env("RIMZ_TEST_BWRAP_PROBE", &marker)
        .assert()
        .success();
    assert!(marker.exists(), "sandbox must execute the preflight probe");
    let config: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(config["agents"]["isolation"].as_str(), Some("sandbox"));
    assert!(
        !legacy_agents_config_path(&env).exists(),
        "isolation belongs in config.toml"
    );
}

#[test]
fn config_set_host_never_probes_bwrap() {
    let env = Env::new();
    let path = machine_config_path(&env);
    write_machine_file(&path, "[agents]\nisolation = \"sandbox\"\n");
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
    for value in ["host", "\"host\"", "'host'"] {
        env.rimz()
            .args(["config", "set", "agents.isolation", value])
            .env("PATH", &bin)
            .env("RIMZ_TEST_BWRAP_PROBE", &marker)
            .assert()
            .success();
        let config: toml::Value = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config["agents"]["isolation"].as_str(), Some("host"));
        assert!(!marker.exists(), "host must not execute bwrap");
    }
    for value in [
        "\" sandbox\"",
        "\"sandbox \"",
        "\"sandbox\" trailing",
        "[\"sandbox\"]",
    ] {
        let before = std::fs::read(&path).unwrap();
        env.rimz()
            .args(["config", "set", "agents.isolation", value])
            .env("PATH", &bin)
            .env("RIMZ_TEST_BWRAP_PROBE", &marker)
            .assert()
            .failure();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!marker.exists(), "invalid isolation must not execute bwrap");
    }
}

#[test]
fn agents_validate_refuses_what_launch_refuses() {
    let env = Env::new();
    write_machine_file(
        &env.rimz_home().join("config.toml"),
        "[agents.commands]\nprobe = \"echo\"\n",
    );
    crate::common::write_definition(
        &env,
        "agents",
        "worker",
        "description: Worker\nmodel: opus\ntools: [Bash]",
        "",
    );
    crate::common::write_definition(
        &env,
        "teams",
        "probe",
        "leader: lead\nstages: [Plan]\nroles:\n  - agent: worker\n    role: lead\n    owns: [Plan]",
        "Pipeline.",
    );
    env.rimz()
        .args(["agents", "explain", "worker"])
        .assert()
        .failure()
        .stderr(contains("probe"));
    env.rimz()
        .args(["agents", "validate"])
        .assert()
        .failure()
        .stdout(contains("probe"));
}

#[test]
fn agents_home_definitions_feed_both_profile_catalogues_without_kind_rows() {
    let env = Env::new();
    crate::common::write_kind_base(&env, "claude");
    crate::common::write_kind_base(&env, "codex");
    write_machine_file(
        &env.agents_home().join("agents/explorer.md"),
        "---\nagent: claude\ntools: []\ndescription: Maps the main workspace\n---\n",
    );
    write_machine_file(
        &env.agents_home().join("subagents/explorer-child.md"),
        "---\nagent: codex\ntools: []\ndescription: Maps a delegated workspace\n---\n",
    );

    for (doorway, expected_name, expected_agent, expected_description) in [
        ("agents", "explorer", "claude", "Maps the main workspace"),
        (
            "subagents",
            "explorer-child",
            "codex",
            "Maps a delegated workspace",
        ),
    ] {
        let output = env
            .rimz()
            .args([doorway, "profiles", "--json"])
            .output()
            .expect("run profile catalogue");
        assert!(
            output.status.success(),
            "{doorway} profiles failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let entries: Vec<serde_json::Value> =
            serde_json::from_slice(&output.stdout).expect("profile catalogue json");
        assert!(
            entries.iter().all(|entry| entry["source"] != "kind"),
            "{doorway} profiles must omit built-in kind rows: {entries:?}"
        );
        assert!(
            entries.iter().any(|entry| {
                entry["name"] == expected_name
                    && entry["source"] == "profile"
                    && entry["agent"] == expected_agent
                    && entry["description"] == expected_description
            }),
            "{doorway} profiles must include its Markdown definition: {entries:?}"
        );
        assert!(
            entries.iter().all(|entry| entry.get("path").is_none()),
            "{doorway} profiles must omit paths unless requested: {entries:?}"
        );
    }
}

fn run_setup_pty(
    env: &Env,
    input: &str,
    path: Option<&std::path::Path>,
    adapter_paths: &[(&str, &std::path::Path)],
) -> String {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut cmd);
    cmd.arg("setup");
    cmd.cwd(env.project_root.as_os_str());
    cmd.env("RIMZ_PETS_OFFLINE", "1");
    cmd.env("TERM", "dumb");
    cmd.env_remove("COLORTERM");
    let empty_path = env.home_root.join("empty-bin");
    std::fs::create_dir_all(&empty_path).expect("mkdir empty PATH");
    cmd.env("PATH", path.unwrap_or(&empty_path));
    for (name, value) in adapter_paths {
        cmd.env(name, value);
    }

    let mut child = pair.slave.spawn_command(cmd).expect("spawn rimz setup");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
        output
    });
    let mut writer = pair.master.take_writer().expect("pty writer");
    writer
        .write_all(input.as_bytes())
        .expect("write setup input");
    writer.flush().expect("flush setup input");
    drop(writer);

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut status = None;
    while Instant::now() < deadline {
        if let Some(done) = child.try_wait().expect("poll rimz setup") {
            status = Some(done);
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(pair.master);
    let output =
        String::from_utf8_lossy(&reader_thread.join().expect("join pty reader")).into_owned();
    let status = status.unwrap_or_else(|| panic!("rimz setup did not exit; output:\n{output}"));
    assert!(
        status.success(),
        "rimz setup failed with {status:?}; output:\n{output}"
    );
    output
}

struct SetupAgentFiles {
    bin_dir: std::path::PathBuf,
    antigravity_hooks: std::path::PathBuf,
    antigravity_settings: std::path::PathBuf,
    pi_extension: std::path::PathBuf,
    opencode_plugin: std::path::PathBuf,
}

fn seed_setup_agents(env: &Env) -> SetupAgentFiles {
    let bin_dir = env.home_root.join("agent-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir agent PATH");
    for name in ["agy", "pi", "opencode"] {
        let path = bin_dir.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write agent stub");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod agent stub");
    }

    let files = SetupAgentFiles {
        bin_dir,
        antigravity_hooks: env.home_root.join("agent-config/antigravity/hooks.json"),
        antigravity_settings: env.home_root.join("agent-config/antigravity/settings.json"),
        pi_extension: env.home_root.join("agent-config/pi/rimz.ts"),
        opencode_plugin: env.home_root.join("agent-config/opencode/rimz.ts"),
    };
    for path in [&files.pi_extension, &files.opencode_plugin] {
        write_machine_file(path, STALE_MANAGED_SOURCE);
    }
    files
}

fn run_setup_agents_pty(env: &Env, input: &str, files: &SetupAgentFiles) -> String {
    let adapter_paths = [
        ("RIMZ_ANTIGRAVITY_HOOKS", files.antigravity_hooks.as_path()),
        (
            "RIMZ_ANTIGRAVITY_SETTINGS",
            files.antigravity_settings.as_path(),
        ),
        ("RIMZ_PI_EXTENSION", files.pi_extension.as_path()),
        ("RIMZ_OPENCODE_PLUGIN", files.opencode_plugin.as_path()),
    ];
    run_setup_pty(env, input, Some(&files.bin_dir), &adapter_paths)
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
        .stdout(contains("# === agents.toml ===").not())
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
    assert!(!legacy_agents_config_path(&env).exists());
    assert!(text.contains("[agents.worktree]"));
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
fn config_init_refuses_a_lone_existing_loop_file() {
    let env = Env::new();
    let path = loop_config_path(&env);
    let original = b"# keep this loop file\n[tasks]\n";
    write_machine_file(&path, std::str::from_utf8(original).expect("utf-8 fixture"));

    env.rimz()
        .args(["config", "init"])
        .assert()
        .failure()
        .stderr(contains(format!("{} already exists", path.display())));

    assert_eq!(std::fs::read(path).expect("read loop file"), original);
}

#[test]
fn config_get_json_distinguishes_unset_and_unknown_keys() {
    let env = Env::new();

    env.rimz()
        .args(["config", "get", "--json"])
        .assert()
        .success()
        .stdout(contains("\"notifications\""));
    env.rimz()
        .args(["config", "get", "notifications", "--json"])
        .assert()
        .success()
        .stdout(contains("\"enabled\""));
    env.rimz()
        .args(["config", "get", "timezone"])
        .assert()
        .failure()
        .stderr(contains("config key `timezone` is unset"));
    env.rimz()
        .args(["config", "get", "not_a_config_key"])
        .assert()
        .failure()
        .stderr(contains("unknown config key `not_a_config_key`"));
}

#[test]
fn invalid_config_edit_preserves_existing_file_bytes() {
    let env = Env::new();
    let path = theme_config_path(&env);
    let original = b"# keep this comment\n[theme.display]\nmax_cols = 72\n";
    write_machine_file(&path, std::str::from_utf8(original).expect("utf-8 fixture"));

    env.rimz()
        .args(["config", "set", "theme.display.max_cols", "0"])
        .assert()
        .failure()
        .stderr(contains("invalid value 0 for `theme.display.max_cols`"));

    assert_eq!(std::fs::read(path).expect("read theme file"), original);
}

#[test]
fn config_get_set_round_trip_preserves_template_comments() {
    let env = Env::new();
    env.rimz().args(["config", "init"]).assert().success();

    env.rimz()
        .args(["config", "get", "notifications.triggers"])
        .assert()
        .success()
        .stdout("[\"waiting\", \"failed\"]\n");

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

    env.rimz()
        .args(["config", "set", "loop.default-timeout", "3h"])
        .assert()
        .success()
        .stdout(contains("set loop.default-timeout"));
    env.rimz()
        .args(["config", "get", "loop.default-timeout"])
        .assert()
        .success()
        .stdout("3h\n");
    let loop_text =
        std::fs::read_to_string(loop_config_path(&env)).expect("read updated loop config");
    assert!(
        loop_text.contains("default-timeout = \"3h\""),
        "{loop_text}"
    );

    env.rimz()
        .args(["config", "set", "theme.display.width_percent", "25"])
        .assert()
        .success()
        .stdout(contains("set theme.display.width_percent"));

    env.rimz()
        .args(["config", "get", "theme.display.width_percent"])
        .assert()
        .success()
        .stdout("25\n");

    let text = std::fs::read_to_string(theme_config_path(&env)).expect("read theme config");
    assert!(
        !text.contains("## width_percent = 30"),
        "set should replace the commented default:\n{text}"
    );
    assert!(
        text.contains(
            "width_percent = 25                 # fixed share; unset uses 30% above 240 cols, 25% at/below"
        ),
        "set should write the override with its template note:\n{text}"
    );
    assert!(
        !text.contains("## max_cols = 72"),
        "set should replace the commented default:\n{text}"
    );
    assert!(
        text.contains(
            "max_cols = 80                      # live column cap for sidebar pane width"
        ),
        "set should write the override with its template note:\n{text}"
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
fn remote_control_codex_config_set_applies_start_and_stop_immediately() {
    let env = Env::new();
    let codex = env
        .home_root
        .join(".codex/packages/standalone/current/codex");
    let log = env.home_root.join("codex-remote-control.log");
    std::fs::create_dir_all(codex.parent().expect("standalone parent"))
        .expect("mkdir standalone install");
    std::fs::write(
        &codex,
        format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
    )
    .expect("write Codex standalone stub");
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755))
        .expect("chmod Codex standalone stub");

    env.rimz()
        .args(["config", "set", "remote_control.codex", "true"])
        .assert()
        .success()
        .stdout(contains("set remote_control.codex"));
    env.rimz()
        .args(["config", "set", "remote_control.codex", "false"])
        .assert()
        .success()
        .stdout(contains("set remote_control.codex"));

    assert_eq!(
        std::fs::read_to_string(log).expect("read Codex control log"),
        "remote-control start\nremote-control stop\n"
    );
}

/// A `claude` on PATH that answers the version probe and nothing else. Remote
/// control never runs here; readiness only needs the executable and its version.
fn write_claude_version_stub(env: &Env) -> std::path::PathBuf {
    let dir = env.home_root.join("claude-bin");
    std::fs::create_dir_all(&dir).expect("mkdir claude bin");
    let stub = dir.join("claude");
    std::fs::write(
        &stub,
        "#!/bin/sh\ncase \"$1\" in --version) echo '2.1.215 (Claude Code)';; *) exit 0;; esac\n",
    )
    .expect("write claude stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
        .expect("chmod claude stub");
    dir
}

fn claude_global_config(env: &Env) -> std::path::PathBuf {
    env.home_root.join(".claude.json")
}

#[test]
fn remote_control_claude_config_set_records_the_first_run_dialog_answer() {
    let env = Env::new();
    let bin = write_claude_version_stub(&env);
    let global = claude_global_config(&env);
    // A machine that has run Claude but never its remote-control host: the
    // config exists and the dialog flag does not.
    std::fs::write(&global, "{\n  \"numStartups\": 7\n}\n").expect("write global config");

    env.rimz()
        .args(["config", "set", "remote_control.claude", "true"])
        .env("PATH", crate::common::path_with_front(&bin))
        .assert()
        .success()
        .stdout(contains("set remote_control.claude"));

    let recorded = std::fs::read_to_string(&global).expect("read global config");
    assert!(
        recorded.contains("\"remoteDialogSeen\": true"),
        "enabling the toggle records the dialog answer so the host serves unattended: {recorded}"
    );
    assert!(
        recorded.contains("\"numStartups\": 7"),
        "every key Claude owns survives the edit: {recorded}"
    );
}

#[test]
fn remote_control_claude_config_set_keeps_an_explicit_refusal() {
    let env = Env::new();
    let bin = write_claude_version_stub(&env);
    let global = claude_global_config(&env);
    let original = "{\n  \"remoteDialogSeen\": false\n}\n";
    std::fs::write(&global, original).expect("write global config");

    env.rimz()
        .args(["config", "set", "remote_control.claude", "true"])
        .env("PATH", crate::common::path_with_front(&bin))
        .assert()
        .failure()
        .stderr(contains("remoteDialogSeen"));

    assert_eq!(
        std::fs::read_to_string(&global).expect("read global config"),
        original,
        "an operator's explicit refusal is reported, never overwritten",
    );
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
        .stderr(contains("invalid value 0 for `theme.display.max_cols`"));

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

    env.rimz()
        .args(["config", "set", "loop.default-timeout", "forever"])
        .assert()
        .failure()
        .stderr(contains(
            "invalid value \"forever\" for `loop.default-timeout`",
        ));

    env.rimz()
        .args(["config", "set", "loop.default-timeout", "0s"])
        .assert()
        .failure()
        .stderr(contains(
            "invalid value \"0s\" for `loop.default-timeout`: must be greater than zero",
        ));

    let config_path = machine_config_path(&env).display().to_string();
    env.rimz()
        .args(["config", "set", "remote_control.claude", "flase"])
        .assert()
        .failure()
        .stderr(contains(
            "invalid value \"flase\" for `remote_control.claude`",
        ))
        .stderr(contains("must be a boolean (true or false)"))
        .stderr(contains("TOML parse error").not())
        .stderr(contains(config_path).not());

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

    let config_path = machine_config_path(&env);
    write_machine_file(&config_path, "[remote_control]\ncodex = \"nope\"\n");
    env.rimz()
        .args(["config", "set", "remote_control.claude", "true"])
        .assert()
        .failure()
        .stderr(contains(
            "cannot set `remote_control.claude`: the existing config is invalid",
        ))
        .stderr(contains(config_path.display().to_string()))
        .stderr(contains("invalid value true").not());
}

#[test]
fn config_set_reports_a_duplicate_key_with_its_fix() {
    let env = Env::new();
    let path = machine_config_path(&env);
    write_machine_file(
        &path,
        "[resume]\nauto_continue = false\nauto_continue = true\n",
    );

    let output = env
        .rimz()
        .args(["config", "set", "remote_control.claude", "true"])
        .output()
        .expect("run config set");

    assert!(!output.status.success(), "duplicate key blocks editing");
    assert_eq!(
        String::from_utf8(output.stderr).expect("utf8 stderr"),
        format!(
            "error: cannot edit {} — the file has a TOML error\n  3 | auto_continue = true\n    | `auto_continue` is defined more than once in the same table\n  fix: remove the extra `auto_continue` at {}:3, then re-run\n",
            path.display(),
            path.display(),
        )
    );
}

#[test]
fn setup_without_tty_reports_and_writes_nothing() {
    let env = Env::new();

    let output = env.rimz().arg("setup").output().expect("run setup");
    assert!(output.status.success(), "setup exits zero");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stdout.contains("RimZ setup"));
    assert!(stdout.contains("changed nothing"));
    assert!(!stdout.contains("Use truecolor?"));
    assert!(!stderr.contains("Use truecolor?"));
    assert!(!stdout.contains("Use Nerd Font icons?"));
    assert!(!stderr.contains("Use Nerd Font icons?"));
    assert!(!stdout.contains("Want a pet?"));
    assert!(!stderr.contains("Want a pet?"));
    assert!(!stdout.contains("Enable hands-off automation?"));
    assert!(!stderr.contains("Enable hands-off automation?"));

    assert!(!machine_config_path(&env).exists());
    assert!(!env.rimz_home().join("teams/consensus.md").exists());
}

#[test]
fn setup_yes_writes_default_config_without_hook_or_trust_side_effects() {
    let env = Env::new();
    let pi_extension = env.home_root.join("setup-yes/pi/rimz.ts");
    let opencode_plugin = env.home_root.join("setup-yes/opencode/rimz.ts");
    write_machine_file(&pi_extension, STALE_MANAGED_SOURCE);
    write_machine_file(&opencode_plugin, STALE_MANAGED_SOURCE);

    let output = env
        .rimz()
        .args(["setup", "--yes"])
        .env("RIMZ_PI_EXTENSION", &pi_extension)
        .env("RIMZ_OPENCODE_PLUGIN", &opencode_plugin)
        .output()
        .expect("run setup --yes");
    assert!(output.status.success(), "setup --yes exits zero");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("Wrote"));
    assert!(stdout.contains("No hooks or trust grants were changed"));
    assert!(!stdout.contains("Use truecolor?"));
    assert!(!stderr.contains("Use truecolor?"));
    assert!(!stdout.contains("Use Nerd Font icons?"));
    assert!(!stderr.contains("Use Nerd Font icons?"));
    assert!(!stdout.contains("Want a pet?"));
    assert!(!stderr.contains("Want a pet?"));
    assert!(!stdout.contains("Enable hands-off automation?"));
    assert!(!stderr.contains("Enable hands-off automation?"));

    let text = std::fs::read_to_string(machine_config_path(&env)).expect("read setup config");
    assert!(text.contains("[resume]"));
    assert!(text.contains("# on_rebirth = true"));
    assert!(
        !text
            .lines()
            .any(|line| line.trim() == "auto_continue = true"),
        "--yes should not opt into auto-continue:\n{text}"
    );
    assert!(
        !text
            .lines()
            .any(|line| line.trim().starts_with("idle_compact =")),
        "--yes should not opt into idle compaction:\n{text}"
    );
    assert!(theme_config_path(&env).exists());
    assert!(!legacy_agents_config_path(&env).exists());
    assert!(loop_config_path(&env).exists());
    for path in [&pi_extension, &opencode_plugin] {
        assert_eq!(
            std::fs::read(path).expect("read stale managed source"),
            STALE_MANAGED_SOURCE.as_bytes(),
            "setup --yes preserves managed sources byte-for-byte",
        );
    }
    let theme_text = std::fs::read_to_string(theme_config_path(&env)).expect("read theme config");
    assert!(
        !theme_text
            .lines()
            .any(|line| line.trim() == "enabled = true"),
        "--yes should not opt into pets:\n{theme_text}"
    );

    let consensus = env.rimz_home().join("teams/consensus.md");
    let copy = std::fs::read_to_string(&consensus).expect("setup publishes the consensus copy");
    assert!(copy.starts_with("<!-- Generated by rimz "), "{copy}");
    assert!(copy.contains("# Team consensus"), "{copy}");
    assert!(stdout.contains(&format!("Wrote {}", consensus.display())));
    let rerun = env
        .rimz()
        .args(["setup", "--yes"])
        .output()
        .expect("rerun setup --yes");
    assert!(rerun.status.success(), "setup --yes reruns");
    assert!(
        !String::from_utf8_lossy(&rerun.stdout).contains(&consensus.display().to_string()),
        "an unchanged consensus copy is not rewritten"
    );
    let validate = env
        .rimz()
        .args(["agents", "validate"])
        .output()
        .expect("run agents validate");
    assert!(
        !String::from_utf8_lossy(&validate.stdout).contains("consensus.md")
            && !String::from_utf8_lossy(&validate.stderr).contains("consensus.md"),
        "the consensus copy is not a team definition"
    );
}

#[test]
fn setup_pty_writes_and_reruns_first_run_answers() {
    let env = Env::new();

    let output = run_setup_pty(&env, "y\ny\ny\ny\n", None, &[]);

    assert!(output.contains(&format!("Wrote {}", loop_config_path(&env).display())));
    assert!(output.contains("Use truecolor?"));
    assert!(output.contains("Use Nerd Font icons?"));
    assert!(output.contains("Want a pet?"));
    assert!(output.contains("Enable hands-off automation?"));
    assert!(output.contains("✓ truecolor"));
    assert!(output.contains("✓ Nerd Font icons"));
    assert!(output.contains("rocky joins the room"));
    assert!(
        output.contains("✓ auto-continue + idle compaction on"),
        "{output}"
    );
    assert!(
        !output.contains("auto-redeem"),
        "no Codex on PATH:\n{output}"
    );
    let text = std::fs::read_to_string(theme_config_path(&env)).expect("read theme config");
    assert!(
        text.contains("mode = \"truecolor\""),
        "truecolor set:\n{text}"
    );
    assert!(
        text.contains("set = \"nerd_font\""),
        "Nerd Font glyphs set:\n{text}"
    );
    assert!(
        !text.lines().any(|line| line.trim().starts_with("style =")),
        "style stays unset:\n{text}"
    );
    assert!(
        text.contains("[theme.pets]") && text.contains("enabled = true"),
        "pet enabled:\n{text}"
    );
    let text = std::fs::read_to_string(machine_config_path(&env)).expect("read machine config");
    assert!(
        text.contains("auto_continue = true") && !text.contains("auto_redeem = true"),
        "automation enabled without Codex's row:\n{text}"
    );
    let idle_compact = env
        .rimz()
        .args(["config", "get", "harness.idle_compact"])
        .output()
        .expect("config get idle_compact");
    assert_eq!(String::from_utf8_lossy(&idle_compact.stdout).trim(), "auto");

    let output = run_setup_pty(&env, "\nn\nn\nn\nn\n", None, &[]);

    assert!(output.contains("Keep your current config? [Y/n]"));
    assert!(output.contains("Use truecolor?"));
    assert!(output.contains("Use Nerd Font icons?"));
    assert!(output.contains("Want a pet? It lives in the sidebar and reacts to your fleet."));
    assert!(output.contains("Enable hands-off automation?"));
    assert!(output.matches("[Y/n]").count() >= 4);
    assert!(output.contains("256-color palette"));
    assert!(output.contains("Unicode glyphs"));
    assert!(output.contains("pet disabled"), "setup output:\n{output}");
    assert!(
        output.contains("✓ auto-continue + idle compaction off"),
        "setup output:\n{output}"
    );
    let text = std::fs::read_to_string(theme_config_path(&env)).expect("read theme config");
    assert!(
        text.contains("mode = 256") || text.contains("mode = \"256\""),
        "indexed color set:\n{text}"
    );
    assert!(
        text.contains("set = \"unicode\""),
        "Unicode glyphs set:\n{text}"
    );
    assert!(
        !text.lines().any(|line| line.trim().starts_with("style =")),
        "style stays unset:\n{text}"
    );
    assert!(
        text.contains("[theme.pets]") && text.contains("enabled = false"),
        "pet disabled:\n{text}"
    );
    let text = std::fs::read_to_string(machine_config_path(&env)).expect("read machine config");
    assert!(
        text.contains("auto_continue = false") && text.contains("idle_compact = \"off\""),
        "automation disabled:\n{text}"
    );

    let codex_bin = env.home_root.join("codex-bin");
    std::fs::create_dir_all(&codex_bin).expect("mkdir codex PATH");
    let codex = codex_bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 0\n").expect("write codex stub");
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755))
        .expect("chmod codex stub");

    let output = run_setup_pty(&env, "\nn\n\n\n\ny\n", Some(&codex_bin), &[]);

    assert!(output.contains("auto-redeem"), "Codex on PATH:\n{output}");
    assert!(
        output.contains("✓ auto-continue + auto-redeem + idle compaction on"),
        "setup output:\n{output}"
    );
    let text = std::fs::read_to_string(machine_config_path(&env)).expect("read machine config");
    assert!(
        text.contains("auto_continue = true")
            && text.contains("auto_redeem = true")
            && text.contains("idle_compact = \"auto\""),
        "every offered row enabled:\n{text}"
    );
}

#[test]
fn setup_pty_preserves_a_lone_loop_file_when_config_is_kept() {
    let env = Env::new();
    let path = loop_config_path(&env);
    write_machine_file(
        &path,
        "[tasks.keep]\nagent = \"codex\"\nprompt = \"keep this task\"\nroot = \"/r\"\nevery = \"15m\"\n",
    );

    let output = run_setup_pty(&env, "\nn\nn\nn\nn\n", None, &[]);

    assert!(output.contains("Keep your current config? [Y/n]"));
    assert!(
        std::fs::read_to_string(path)
            .expect("read loop config")
            .contains("prompt = \"keep this task\"")
    );
}

#[test]
fn setup_pty_installs_and_refreshes_detected_agent_hooks_together() {
    let env = Env::new();
    let files = seed_setup_agents(&env);
    let output = run_setup_agents_pty(&env, "y\nn\nn\nn\nn\n", &files);

    assert!(
        output
            .lines()
            .any(|line| line.contains("agent antigravity:")
                && line.contains("on PATH; hooks not installed")),
        "{output}"
    );
    for name in ["pi", "opencode"] {
        assert!(
            output
                .lines()
                .any(|line| line.contains(&format!("agent {name}:"))
                    && line.contains("on PATH; hooks installed; upgrade available")),
            "{output}"
        );
    }
    assert_eq!(
        output
            .matches("Install or refresh reporting hooks?")
            .count(),
        1,
        "{output}"
    );
    for name in ["antigravity", "pi", "opencode"] {
        assert!(output.contains(name), "missing {name}:\n{output}");
    }
    assert!(output.contains("hooks.json"), "{output}");
    assert!(output.contains("settings.json"), "{output}");
    assert!(output.contains("updates existing config"), "{output}");
    assert!(output.contains("new file"), "{output}");

    let hooks = std::fs::read_to_string(&files.antigravity_hooks).expect("read hooks config");
    let settings =
        std::fs::read_to_string(&files.antigravity_settings).expect("read settings config");
    assert!(
        hooks.contains("rimz hooks feed --source antigravity"),
        "{hooks}"
    );
    assert!(settings.contains("_rimz_managed"), "{settings}");
    assert_eq!(
        std::fs::read_to_string(&files.pi_extension).expect("read Pi extension"),
        PI_EXTENSION_SOURCE,
    );
    assert_eq!(
        std::fs::read_to_string(&files.opencode_plugin).expect("read OpenCode plugin"),
        OPENCODE_PLUGIN_SOURCE,
    );
}

#[test]
fn setup_pty_decline_preserves_every_hook_candidate() {
    let env = Env::new();
    let files = seed_setup_agents(&env);
    let output = run_setup_agents_pty(&env, "n\nn\nn\nn\nn\n", &files);

    assert!(
        output.contains(
            "Nothing changed - install or refresh agents any time with `rimz hooks install`."
        ),
        "{output}"
    );
    assert!(!files.antigravity_hooks.exists());
    assert!(!files.antigravity_settings.exists());
    assert_eq!(
        std::fs::read(&files.pi_extension).unwrap(),
        STALE_MANAGED_SOURCE.as_bytes()
    );
    assert_eq!(
        std::fs::read(&files.opencode_plugin).unwrap(),
        STALE_MANAGED_SOURCE.as_bytes()
    );
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
    assert!(!legacy_agents_config_path(&env).exists());
    assert!(loop_config_path(&env).exists());
}

#[test]
fn setup_yes_leaves_unparseable_config_untouched() {
    let env = Env::new();
    let path = theme_config_path(&env);
    let broken = b"[theme.display]\nmax_cols = 64\nmax_cols = 72\n";
    write_machine_file(&path, std::str::from_utf8(broken).expect("utf8 fixture"));

    env.rimz()
        .args(["setup", "--yes"])
        .assert()
        .success()
        .stdout(contains(format!(
            "Left {} untouched - unparseable:",
            path.display()
        )))
        .stdout(contains(
            "line 3: `max_cols` is defined more than once in the same table",
        ))
        .stdout(contains("fix the file and rerun rimz setup"));

    assert_eq!(
        std::fs::read(&path).expect("read preserved config"),
        broken,
        "setup preserves the broken file byte-for-byte",
    );
}

#[test]
fn interactive_setup_stops_cleanly_before_partial_setup_for_unparseable_config() {
    let env = Env::new();
    let path = theme_config_path(&env);
    let broken = "[theme.display]\nmax_cols = 64\nmax_cols = 72\n";
    write_machine_file(&path, broken);

    let output = run_setup_pty(&env, "\n", None, &[]);

    assert!(
        output.contains("Left "),
        "merge outcome is visible:\n{output}"
    );
    assert!(output.contains("theme.toml untouched"), "{output}");
    assert!(
        output.contains("Fix the unparseable file(s), then rerun `rimz setup`."),
        "clean early-exit guidance:\n{output}",
    );
    assert!(!output.contains("Error:"), "no raw error:\n{output}");
    assert!(
        !output.contains("Want a pet?"),
        "first-run prompts do not start:\n{output}",
    );
    assert!(
        !env.rimz_home().join("remote.toml").exists(),
        "remote setup does not partially run",
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read preserved theme"),
        broken,
    );
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
fn setup_yes_keeps_markdown_team_and_launch_preferences() {
    let env = Env::new();
    let team_path = env.agents_home().join("teams/duo.md");
    let source = "---\nleader: lead\nstages: [Plan]\nroles:\n  - agent: worker\n    role: lead\n    owns: [Plan]\n---\nPipeline.";
    write_machine_file(
        &env.agents_home().join("agents/codex.md"),
        "---\ndescription: Base\n---\nBase.",
    );
    write_machine_file(
        &env.agents_home().join("agents/worker.md"),
        "---\ndescription: Worker\nagent: codex\ntools: []\n---\n",
    );
    write_machine_file(&team_path, source);
    write_machine_file(&machine_config_path(&env), "[agents]\nplacement = 'tab'\n");
    env.rimz()
        .args(["setup", "--yes"])
        .assert()
        .success()
        .stdout(contains("Merged"));
    assert_eq!(std::fs::read_to_string(team_path).unwrap(), source);
    env.rimz()
        .args(["config", "get", "agents.teams.duo.roles", "--json"])
        .assert()
        .success()
        .stdout(contains("duo.lead"));
    env.rimz()
        .args(["config", "get", "agents.placement", "--json"])
        .assert()
        .success()
        .stdout(contains("tab"));
}

#[test]
fn setup_yes_merges_loop_tasks_as_table_blocks() {
    let env = Env::new();
    write_machine_file(
        &loop_config_path(&env),
        r#"
[tasks.self_wait]
wait = { kind = "claude", session = "s1", handle = "@planner" }
prompt = "resume"
root = "/r"

[tasks.pr_watch]
agent = "codex"
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
        text.contains("[tasks.self_wait]"),
        "task should render as a table block:\n{text}"
    );
    assert!(
        text.contains("[tasks.pr_watch]"),
        "wait-less task should render as a table block:\n{text}"
    );
    assert!(
        text.contains("[tasks.self_wait.wait]"),
        "wait should render as a nested table block:\n{text}"
    );
    assert!(
        !text.contains("tasks = {"),
        "tasks should not collapse to one inline table:\n{text}"
    );
    assert!(
        text.contains("agent = \"codex\"")
            && text.contains("every = \"15m\"")
            && text.contains("session = \"s1\""),
        "task fields should survive:\n{text}"
    );
}

#[test]
fn setup_yes_keeps_legacy_agents_file_byte_for_byte() {
    let env = Env::new();
    let source = "# legacy, ignored\n[agents.teams.duo]\nlayout = 'missing'\n";
    write_machine_file(&legacy_agents_config_path(&env), source);
    env.rimz().args(["setup", "--yes"]).assert().success();
    assert_eq!(
        std::fs::read_to_string(legacy_agents_config_path(&env)).unwrap(),
        source
    );
    env.rimz()
        .args(["config", "get", "agents.teams", "--json"])
        .assert()
        .success()
        .stdout(contains("duo").not());
}

#[test]
fn setup_yes_preserves_kind_base_definitions() {
    let env = Env::new();
    let path = env.agents_home().join("agents/codex.md");
    let source = "---\ndescription: Base\n---\nBase instructions.";
    write_machine_file(&path, source);
    env.rimz().args(["setup", "--yes"]).assert().success();
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    env.rimz()
        .args(["config", "get", "agents.profiles.codex", "--json"])
        .assert()
        .success()
        .stdout(contains("Base instructions."));
}

#[test]
fn setup_yes_ignores_broken_legacy_fragments_and_preserves_broken_markdown() {
    let env = Env::new();
    let legacy = env.agents_home().join("teams/broken/team.toml");
    let definition = env.agents_home().join("teams/broken.md");
    write_machine_file(&legacy, "not = = toml");
    write_machine_file(&definition, "not frontmatter");
    env.rimz().args(["setup", "--yes"]).assert().success();
    assert_eq!(std::fs::read_to_string(legacy).unwrap(), "not = = toml");
    assert_eq!(
        std::fs::read_to_string(&definition).unwrap(),
        "not frontmatter"
    );
    env.rimz()
        .args(["config", "get", "agents", "--json"])
        .assert()
        .success();
}

#[test]
fn setup_yes_preserves_template_comments_for_untouched_config() {
    let env = Env::new();
    write_machine_file(
        &machine_config_path(&env),
        rimz::config::ConfigEditor::machine().files().ordered()[0].template(),
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
