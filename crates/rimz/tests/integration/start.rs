//! `rimz start` run from inside a session of the selected mux: a same-mux room
//! can't be nested, so the default launch reports the directory's room and
//! exits before any side effect instead of emitting a doomed nested
//! `attach --create`.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::workspace::WorkspaceResolver;

use crate::common::{CommandTimeoutExt, Env, ROOM_WORKFLOW_TIMEOUT};

const MATERIALIZED_ROOM_PANES: &str = r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},{"id":2,"is_plugin":false,"tab_id":1,"title":"sh"}]"#;

fn zellij_trace_shim() -> PathBuf {
    crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace"))
}

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

fn seed_sidebar_heartbeat(env: &Env, session_name: &str, label: &str) -> PathBuf {
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    let heartbeat = rimz::sidebar::heartbeat::SidebarHeartbeat::new(
        env.workspace_id.clone(),
        rimz::ids::SidebarInstanceId::new(),
        rimz::MuxName::Zellij,
        session_name,
        runtime.sock_dir.join(format!("{label}.sock")),
        None,
    );
    let path = runtime.heartbeat_dir.join(format!("sidebar.{label}.json"));
    std::fs::write(
        &path,
        serde_json::to_vec(&heartbeat).expect("serialize heartbeat"),
    )
    .expect("write heartbeat");
    path
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
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("resolve");

    let output = env
        .rimz()
        .arg("start")
        // Pretend we're already inside a Zellij session: `auto_detect_backend`
        // selects Zellij from `ZELLIJ` alone, with no binary on PATH.
        .env("ZELLIJ", "1")
        .bounded_output()
        .expect("run rimz start");

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
    let config = env.config_root().join("rimz/config.toml");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("config dir");
    std::fs::write(config, "[accounts.budget]\nantigravity = \"50/day\"\n")
        .expect("machine config");
    let workspace_state = env
        .state_root()
        .join("rimz/workspaces")
        .join(env.workspace_id.as_str());

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
fn start_checks_hooks_on_birth_but_not_live_reattach() {
    let birth = Env::new();
    let birth_bin = seed_actionable_agent(&birth);
    let birth_workspace = WorkspaceResolver::resolve(&birth.project_root, None).expect("resolve");
    let birth_heartbeat = seed_sidebar_heartbeat(&birth, &birth_workspace.session_name, "birth");
    let birth_trace = birth.project_root.join("zellij-birth.log");
    let mut birth_command = birth.rimz();
    configure_actionable_hooks(&mut birth_command, &birth, &birth_bin, &birth_trace, "");
    let birth_output = birth_command
        .bounded_output_within(ROOM_WORKFLOW_TIMEOUT)
        .expect("run absent-room start");
    assert!(
        birth_output.status.success(),
        "birth failed: {}",
        String::from_utf8_lossy(&birth_output.stderr)
    );
    let birth_stderr = String::from_utf8_lossy(&birth_output.stderr);
    assert!(birth_stderr.contains("RimZ found 1 coding agent: antigravity."));
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
    let workspace = WorkspaceResolver::resolve(&live.project_root, None).expect("resolve live");
    let live_heartbeat = seed_sidebar_heartbeat(&live, &workspace.session_name, "live");
    let sessions = format!("{} [Created 1m ago]\n", workspace.session_name);
    let live_trace = live.project_root.join("zellij-live.log");
    let mut live_command = live.rimz();
    configure_actionable_hooks(&mut live_command, &live, &live_bin, &live_trace, &sessions);
    let live_output = live_command
        .bounded_output_within(ROOM_WORKFLOW_TIMEOUT)
        .expect("run live-room start");
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
fn reconnect_marker_keeps_pty_start_unattended() {
    let env = Env::new();
    let bin_dir = seed_actionable_agent(&env);
    let zellij_log = env.project_root.join("zellij-reconnect.log");
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
    let deadline = Instant::now() + ROOM_WORKFLOW_TIMEOUT;
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
        panic!("reconnect start did not finish within {ROOM_WORKFLOW_TIMEOUT:?}:\n{output}")
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
