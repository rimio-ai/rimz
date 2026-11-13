//! Verifies reload delivery and the independent structural-repair CLI.
//!
//! No live mux needed — we plant heartbeats and bound sockets directly under a
//! `RuntimePaths::under` root and call the library function.

#![allow(clippy::print_stdout)]

use std::os::unix::net::UnixDatagram;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use rimz::disk::paths::RuntimePaths;
use rimz::ids::{MuxName, SidebarInstanceId, WorkspaceId};
use rimz::wakeup::events::RELOAD_CONTROL_WORD;
use rimz::wakeup::heartbeat::SidebarHeartbeat;
use rimz::wakeup::reload_all;

use crate::common::{Env, available_loopback_port};

const SESSION_NAME: &str = "rimz-reload-test";

#[test]
fn standalone_sidebar_repair_does_not_stage_a_build() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["sidebar", "repair"])
        .output()
        .expect("run sidebar repair");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "No running sidebars to repair.\n"
    );
    assert!(
        !env.rimz_home().join("builds").exists(),
        "standalone structural repair must not publish an upgrade generation",
    );
}

#[test]
fn missing_presence_artifact_is_a_reload_problem_with_or_without_repair() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().unwrap();
    let mut heartbeat = SidebarHeartbeat::new(
        workspace.workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Zellij,
        &workspace.session_name,
        runtime.sock_dir.join("reload.sock"),
        Some("zellij:terminal_1".parse().unwrap()),
    );
    heartbeat.build = Some(
        rimz::build_id::of_file(&crate::common::cargo_bin(
            "rimz",
            env!("CARGO_BIN_EXE_rimz"),
        ))
        .unwrap(),
    );
    write_heartbeat(&runtime, "sidebar.live.json", heartbeat);
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[zellij]\npane_frames = false\n",
    )
    .unwrap();
    for repair in [false, true] {
        let mut command = env.rimz();
        command
            .arg("reload")
            .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join("zellij.log"))
            .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", &workspace.session_name)
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
            .env(
                "RIMZ_PRESENCE_PLUGIN",
                env.project_root.join("missing.wasm"),
            );
        if repair {
            command.arg("--repair");
        }
        let output = command.output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert_eq!(output.status.code(), Some(1), "{text}");
        assert!(
            text.contains("presence plugin not upgraded: presence plugin artifact unavailable"),
            "{text}"
        );
        assert!(text.contains("rimz doctor --root"), "{text}");
    }
}

/// A room whose sidebar pane exists but has no fresh heartbeat, as while a
/// restarted host re-attaches, settles: exit 0, and the mount pass, which lists
/// the room, adds, closes and resizes nothing.
#[test]
fn reload_reports_a_silent_room_as_settling_and_mounts_nothing() {
    let env = Env::new();
    env.record(&env.project_root);
    let workspace = env.resolve_workspace(&env.project_root);
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().unwrap();
    rimz::mux::zellij::pane_topology::write_pane_topology_cache(
        &runtime,
        &rimz::mux::zellij::pane_topology::PaneTopologyCache {
            session_name: workspace.session_name.clone(),
            produced_at_ms: rimz::utils::time::unix_now_ms(),
            writer: None,
            focused_pane: None,
            clients_withheld: false,
            clients: None,
            panes: serde_json::from_str(crate::common::room::MATERIALIZED_ROOM_PANES).unwrap(),
        },
    )
    .unwrap();
    let trace = env.project_root.join("zellij.log");
    let output = env
        .rimz()
        .arg("reload")
        .env("RIMZ_ZELLIJ_BIN", crate::common::zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace)
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", &workspace.session_name)
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            crate::common::room::MATERIALIZED_ROOM_PANES,
        )
        .env("RIMZ_TEST_SKIP_STATS_RELOAD", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{text}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("0 sidebars · settling, retries on its own"),
        "{text}"
    );
    assert!(!text.contains("fix:"), "{text}");
    let log = std::fs::read_to_string(&trace).unwrap();
    // The trace joins argv words with tabs; the plugin configuration carries
    // `opt_stacked_resize`, so match whole words.
    assert!(
        log.contains("\tlist-panes\t"),
        "the pass lists the room: {log}"
    );
    for mutation in [
        "\tnew-pane",
        "\tclose-pane",
        "\tresize\t",
        "\tdelete-session",
        "\tattach\t",
    ] {
        assert!(
            !log.contains(mutation),
            "reload issued `{mutation}`:\n{log}"
        );
    }
}

#[cfg(unix)]
#[test]
fn reload_restarts_an_online_web_daemon_and_leaves_an_offline_one_offline() {
    let env = Env::new();
    let port = available_loopback_port();
    let config = env.rimz_home().join("config.toml");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("mkdir config");
    std::fs::write(
        &config,
        format!("[web]\nport = {port}\nstyle_client = false\n"),
    )
    .expect("write config");
    let bin_dir = env.home_root.join("reload-web-bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir web bin");
    let ttyd = bin_dir.join("ttyd");
    std::os::unix::fs::symlink(
        crate::common::cargo_bin("ttyd-trace", env!("CARGO_BIN_EXE_ttyd-trace")),
        &ttyd,
    )
    .expect("link ttyd shim");
    let log = env.project_root.join("reload-web.log");
    let daemon_path = env.rimz_home().join("web/web-ttyd.json");
    let command = || web_command(&env, &bin_dir, &ttyd, &log);

    let offline = command().arg("reload").output().expect("reload offline");
    assert!(offline.status.success(), "{:?}", offline.stderr);
    assert!(!daemon_path.exists(), "reload started an offline daemon");

    let start = command()
        .args(["web", "start"])
        .output()
        .expect("start web daemon");
    assert!(start.status.success(), "{:?}", start.stderr);
    let before: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&daemon_path).expect("daemon record before reload"))
            .expect("daemon JSON before reload");

    let online = command().arg("reload").output().expect("reload online");
    assert!(online.status.success(), "{:?}", online.stderr);
    assert!(
        String::from_utf8_lossy(&online.stdout).contains("web daemon restarted"),
        "{}",
        String::from_utf8_lossy(&online.stdout)
    );
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&daemon_path).expect("daemon record after reload"))
            .expect("daemon JSON after reload");
    assert_ne!(before["pid"], after["pid"], "reload reused the web daemon");

    let stop = command()
        .args(["web", "stop"])
        .output()
        .expect("stop web daemon");
    assert!(stop.status.success(), "{:?}", stop.stderr);
}

#[cfg(unix)]
fn web_command(env: &Env, bin_dir: &Path, ttyd: &Path, log: &Path) -> Command {
    let mut command = env.rimz();
    command
        .env("PATH", bin_dir)
        .env("RIMZ_TTYD_BIN", ttyd)
        .env("RIMZ_TEST_TTYD_LOG", log)
        .env("RIMZ_WEB_FONTS_OFFLINE", "1");
    command
}

#[test]
fn reload_signals_fresh_sidebars_and_skips_stale() {
    let tempdir = tempfile::Builder::new()
        .prefix("rl")
        .tempdir_in("/tmp")
        .expect("short tempdir");
    let project_root = tempdir.path().join("project");
    let runtime_root = tempdir.path().join("runtime");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    std::fs::create_dir_all(&runtime_root).expect("mkdir runtime");

    let workspace_id = WorkspaceId::from_project_root(&project_root);
    let runtime =
        RuntimePaths::under(workspace_id.clone(), &runtime_root).expect("RuntimePaths::under");
    runtime.ensure_dirs().expect("ensure runtime dirs");

    if crate::common::af_unix_bind_sandboxed(&runtime.sock_dir) {
        crate::common::skip(crate::common::AF_UNIX_SANDBOXED);
        return;
    }

    let fresh_sock = runtime.sock_dir.join("event.fresh.sock");
    let recv = UnixDatagram::bind(&fresh_sock).expect("bind fresh sock");
    write_heartbeat(
        &runtime,
        "sidebar.fresh.json",
        SidebarHeartbeat::new(
            workspace_id.clone(),
            SidebarInstanceId::new(),
            MuxName::Tmux,
            SESSION_NAME,
            fresh_sock.clone(),
            None,
        ),
    );

    write_heartbeat(
        &runtime,
        "sidebar.sibling.json",
        SidebarHeartbeat::new(
            workspace_id.clone(),
            SidebarInstanceId::new(),
            MuxName::Tmux,
            SESSION_NAME,
            fresh_sock,
            None,
        ),
    );

    // A heartbeat whose mtime is well past the TTL must be skipped even
    // though its socket is bound and listening.
    let stale_sock = runtime.sock_dir.join("event.stale.sock");
    let stale_recv = UnixDatagram::bind(&stale_sock).expect("bind stale sock");
    let stale = SidebarHeartbeat::new(
        workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Tmux,
        SESSION_NAME,
        stale_sock,
        None,
    );
    write_heartbeat(&runtime, "sidebar.stale.json", stale);
    std::fs::File::open(runtime.heartbeat_dir.join("sidebar.stale.json"))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
        .unwrap();

    let signaled = reload_all(&runtime).expect("reload_sidebars");
    assert_eq!(signaled, 2, "count fresh panes even when they share a host");

    recv.set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    let mut buf = [0_u8; 4096];
    let n = recv.recv(&mut buf).expect("fresh sidebar receives reload");
    assert_eq!(
        &buf[..n],
        RELOAD_CONTROL_WORD.as_bytes(),
        "reload must not ride the version-gated sidebar event envelope",
    );
    recv.set_nonblocking(true).expect("nonblocking receive");
    assert_eq!(
        recv.recv(&mut buf).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "one host receives only one reload for its two panes",
    );

    // The stale sidebar must not have received anything.
    stale_recv
        .set_read_timeout(Some(Duration::from_millis(100)))
        .expect("set read timeout");
    let mut stale_buf = [0_u8; 64];
    assert!(
        stale_recv.recv(&mut stale_buf).is_err(),
        "stale sidebar must be skipped",
    );
}

fn write_heartbeat(runtime: &RuntimePaths, filename: &str, hb: SidebarHeartbeat) {
    let path = runtime.heartbeat_dir.join(filename);
    std::fs::write(&path, serde_json::to_vec(&hb).expect("serialize heartbeat"))
        .expect("write heartbeat");
    crate::common::room::date_heartbeat_as_live(&path);
}
