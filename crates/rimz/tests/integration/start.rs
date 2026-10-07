//! `rimz start` run from inside a session of the selected mux: a same-mux room
//! can't be nested, so the default launch reports the directory's room and
//! exits before any side effect instead of emitting a doomed nested
//! `attach --create`.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::common::room::{ShimRoom, seed_sidebar_heartbeat};
use crate::common::{COMMAND_TIMEOUT, CommandTimeoutExt, Env, path_with_front, zellij_trace_shim};

const MATERIALIZED_ROOM_PANES: &str = r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},{"id":2,"is_plugin":false,"tab_id":1,"title":"sh"}]"#;

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
        .env("RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS", "250")
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
fn start_refuses_sandbox_without_bwrap() {
    let env = Env::new();
    let config_dir = env.rimz_home();
    std::fs::create_dir_all(&config_dir).expect("mkdir config");
    let agents_path = config_dir.join("config.toml");
    let seed = "[agents]\nisolation = \"sandbox\"\n";
    std::fs::write(&agents_path, seed).expect("write sandbox config");
    let empty_bin = env.home_root.join("empty-bin");
    std::fs::create_dir(&empty_bin).expect("mkdir empty PATH");
    let mux_log = env.home_root.join("zellij.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "start", "--no-attach"])
        .env("PATH", &empty_bin)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &mux_log)
        .bounded_output()
        .expect("run sandbox start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("bubblewrap"), "{stderr}");
    assert!(stderr.contains("host"), "{stderr}");
    assert_eq!(std::fs::read_to_string(agents_path).unwrap(), seed);
    assert!(
        !config_dir.join("theme.toml").exists(),
        "no theme bootstrap"
    );
    assert!(!mux_log.exists(), "no multiplexer calls before refusal");
}

#[test]
fn start_refuses_a_missing_configured_agent_shell() {
    let env = Env::new();
    let config_dir = env.rimz_home();
    std::fs::create_dir_all(&config_dir).expect("mkdir config");
    let missing = env.home_root.join("missing").join("bash");
    std::fs::write(
        config_dir.join("config.toml"),
        format!("[agents]\nshell = \"{}\"\n", missing.display()),
    )
    .expect("write agent shell config");
    let mux_log = env.home_root.join("zellij.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "start", "--no-attach"])
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &mux_log)
        .bounded_output()
        .expect("run start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("[agents] shell") && stderr.contains("does not exist"),
        "{stderr}"
    );
    assert!(!mux_log.exists(), "no multiplexer calls before refusal");
}

#[test]
fn start_refuses_a_legacy_only_host_without_creating_the_home() {
    let env = Env::new();
    std::fs::remove_dir_all(env.rimz_home()).expect("remove fixture home");
    let legacy = env.state_root().join("rimz");
    std::fs::create_dir_all(&legacy).expect("seed legacy root");
    let mux_log = env.home_root.join("zellij.log");
    let output = env
        .rimz()
        .args(["--mux", "zellij", "start", "--no-attach"])
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &mux_log)
        .bounded_output()
        .expect("run start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains(&legacy.display().to_string()), "{stderr}");
    assert!(stderr.contains("moving-from-the-xdg-roots"), "{stderr}");
    assert!(
        !env.rimz_home().exists(),
        "no empty home beside legacy roots"
    );
    assert!(!mux_log.exists(), "no multiplexer calls before refusal");

    // A home that other commands created (stats, hook logs) holds no config
    // yet, so start and a cwd attach still refuse.
    std::fs::create_dir_all(env.rimz_home().join("cache/providers")).expect("seed home");
    for args in [&["start", "--no-attach"][..], &["attach"][..]] {
        let output = env
            .rimz()
            .args(["--mux", "zellij"])
            .args(args)
            .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", &mux_log)
            .bounded_output()
            .expect("run room entry");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{args:?}: {stderr}");
        assert!(stderr.contains("moving-from-the-xdg-roots"), "{stderr}");
    }
    assert!(!env.rimz_home().join("config.toml").exists());
    assert!(!mux_log.exists(), "no multiplexer calls before refusal");
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
        stderr.contains("rimz: 1 definition with another error: agents/broken"),
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
    let workspace = env.resolve_workspace(&env.project_root);
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
    assert!(
        !stdout.contains("attach --create"),
        "a nested run must not emit the doomed attach command, got stdout: {stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&workspace.session_name),
        "stderr should name the directory's room, got: {stderr}"
    );
    assert!(
        stderr.contains("nested"),
        "stderr should explain it can't nest a room, got: {stderr}"
    );
    // The guard returns before `ensure_detected_agent_hooks`, so the first-run
    // hook consent gate never prints — proving the bypass skips the ceremony.
    assert!(
        !stderr.contains("RimZ first run"),
        "the nested bypass must run before hook install, got: {stderr}"
    );
}

#[test]
fn start_rejects_unsupported_account_budget_before_room_state() {
    let env = Env::new();
    let config = env.rimz_home().join("config.toml");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("config dir");
    std::fs::write(config, "[accounts.budget]\nantigravity = \"50/day\"\n")
        .expect("machine config");
    let workspace_state = env.state_path_for(&env.project_root).root;

    let output = env
        .rimz()
        .arg("start")
        .bounded_output()
        .expect("run rimz start");

    assert!(!output.status.success(), "start accepted unsupported cap");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("accounts.budget.antigravity"), "{stderr}");
    assert!(
        stderr.contains("authoritative account-level dollars"),
        "{stderr}"
    );
    assert!(
        !workspace_state.exists(),
        "account-budget preflight must run before room state is created"
    );
}

#[test]
fn start_rejects_old_or_unrecognised_tmux_before_room_state() {
    let (maj, min, patch) = rimz::mux::tmux::MIN_TMUX_VERSION;
    for (version, message) in [
        (
            "tmux 3.4",
            format!(
                "tmux 3.4 is below RimZ's floor; upgrade tmux to >= {maj}.{min}.{patch}, or run this room with `--mux zellij`."
            ),
        ),
        (
            "tmux next-3.6",
            format!(
                "`tmux -V` output \"tmux next-3.6\" was not recognised; RimZ needs a release build >= {maj}.{min}.{patch}, or run this room with `--mux zellij`."
            ),
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
            !workspace_state.exists(),
            "tmux version preflight must run before room state is created"
        );
    }
}

#[test]
fn start_rejects_invalid_notifications_before_room_state() {
    let env = Env::new();
    let config = env.rimz_home().join("config.toml");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("config dir");
    std::fs::write(
        &config,
        "[[notifications.handler]]\nname = \"bad\"\ncommand = \"\"\n",
    )
    .expect("machine config");
    let workspace_state = env.state_path_for(&env.project_root).root;

    let output = env
        .rimz()
        .arg("start")
        .bounded_output()
        .expect("run rimz start");

    assert!(!output.status.success(), "start accepted invalid handler");
    let stderr = String::from_utf8_lossy(&output.stderr);
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
    assert!(
        !workspace_state.exists(),
        "notifications preflight must run before room state is created"
    );
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
fn start_checks_hooks_on_birth_but_not_live_reattach() {
    let birth = Env::new();
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
    let _birth_room = ShimRoom::watch(&birth, &birth_trace, MATERIALIZED_ROOM_PANES);
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
    let _live_room = ShimRoom::watch(&live, &live_trace, MATERIALIZED_ROOM_PANES);
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
    let mut room = ShimRoom::watch(&env, &trace, MATERIALIZED_ROOM_PANES);
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
    let _room = ShimRoom::watch(&env, &trace, MATERIALIZED_ROOM_PANES);
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
    let _room = ShimRoom::watch(&env, &zellij_log, MATERIALIZED_ROOM_PANES);

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
        command.env("RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS", "250");
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
    let zellij_log = env.project_root.join("zellij-reconnect.log");
    let _room = ShimRoom::watch(&env, &zellij_log, MATERIALIZED_ROOM_PANES);
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
    command.env("RIMZ_TEST_ZELLIJ_HEALTH_PROBE_MS", "250");
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
        output.contains("No terminal input — nothing installed or refreshed."),
        "the hooks gate must use its unattended fallback: {output}"
    );
    assert!(
        !output.contains("Install or refresh reporting hooks? [Y/n]"),
        "the reconnect must not prompt: {output}"
    );
    assert!(
        !output.contains("Trust this project's config on this machine?"),
        "the reconnect must not prompt for trust: {output}"
    );
}

fn room_logins(env: &Env) -> Option<serde_json::Value> {
    let record = env.state_path_for(&env.project_root).workspace_record;
    let text = std::fs::read_to_string(record).ok()?;
    let record: serde_json::Value = serde_json::from_str(&text).expect("workspace record json");
    record.get("logins").cloned()
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

fn start_with_accounts(
    env: &Env,
    sessions: &str,
    accounts: &[&str],
    no_trigger_reason: Option<&str>,
) -> std::process::Output {
    let bin_dir = seed_actionable_agent(env);
    let trace = env.project_root.join("zellij-accounts.log");
    let mut room = ShimRoom::watch(env, &trace, MATERIALIZED_ROOM_PANES);
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
fn start_freezes_room_accounts_until_reset() {
    let env = Env::new();
    write_machine_config(&env, "[accounts.claude.work]\n");
    let born = start_with_accounts(&env, "", &["claude=default"], None);
    assert!(
        born.status.success(),
        "birth failed: {}",
        String::from_utf8_lossy(&born.stderr)
    );
    let frozen = serde_json::json!({"claude": "default", "codex": "default"});
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
fn start_takes_project_accounts_only_under_trust() {
    let env = Env::new();
    let project_config = env.project_root.join(".rimz/config.toml");
    std::fs::create_dir_all(project_config.parent().expect("project config dir"))
        .expect("mkdir .rimz");
    std::fs::write(&project_config, "[accounts]\ncodex = \"default\"\n").expect("project config");

    let untrusted = start_with_accounts(
        &env,
        "",
        &[],
        Some("untrusted project accounts refuse before room birth"),
    );
    let stderr = String::from_utf8_lossy(&untrusted.stderr);
    assert!(!untrusted.status.success(), "{stderr}");
    assert!(
        stderr.contains("project account selections in .rimz/config.toml are untrusted"),
        "{stderr}"
    );
    assert!(stderr.contains("rimz trust grant"), "{stderr}");
    assert_eq!(room_logins(&env), None);

    // `attach` births a room that is not live under the same rule.
    let bin_dir = seed_actionable_agent(&env);
    let attach_log = env.project_root.join("zellij-attach.log");
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
    assert!(!attach.status.success(), "{stderr}");
    assert!(
        stderr.contains("project account selections in .rimz/config.toml are untrusted"),
        "{stderr}"
    );
    assert_eq!(room_logins(&env), None);

    env.rimz()
        .args(["trust", "grant"])
        .bounded_output()
        .expect("trust grant");
    let trusted = start_with_accounts(&env, "", &[], None);
    assert!(
        trusted.status.success(),
        "trusted start failed: {}",
        String::from_utf8_lossy(&trusted.stderr)
    );
    assert_eq!(
        room_logins(&env),
        Some(serde_json::json!({"claude": "default", "codex": "default"}))
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
                r#"*" new-window "*" -n ##"*) exit 1 ;;"#,
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
                r#"*" new-window "*" -n ##"*) exit 1 ;;"#,
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
        r#"*" new-window "*" -n ##"*) : > {}; until [ -e {} ]; do sleep 0.05; done ;;"#,
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
