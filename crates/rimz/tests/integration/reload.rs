//! Verifies reload delivery and the independent structural-repair CLI.
//!
//! No live mux needed — we plant heartbeats and bound sockets directly under a
//! `RuntimePaths::under` root and call the library function.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::os::unix::net::UnixDatagram;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use jiff::Timestamp;
use rimz::ids::{MuxName, SidebarInstanceId, WorkspaceId};
use rimz::sidebar::events::RELOAD_CONTROL_WORD;
use rimz::sidebar::heartbeat::SidebarHeartbeat;
use rimz::sidebar::wakeup::reload_all;
use rimz::store::RuntimePaths;

use crate::common::Env;

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
        !env.state_root().join("rimz/builds").exists(),
        "standalone structural repair must not publish an upgrade generation",
    );
}

#[cfg(unix)]
#[test]
fn reload_restarts_an_online_web_daemon_and_leaves_an_offline_one_offline() {
    let env = Env::new();
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("bind web port")
        .local_addr()
        .expect("web address")
        .port();
    let config = env.config_root().join("rimz/config.toml");
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
    let daemon_path = env.state_root().join("rimz/web-ttyd.json");
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
        String::from_utf8_lossy(&online.stdout).contains("Restarted the shared web daemon."),
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
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }

    let fresh_sock = runtime.sock_dir.join("sidebar.fresh.sock");
    let recv = UnixDatagram::bind(&fresh_sock).expect("bind fresh sock");
    write_heartbeat(
        &runtime,
        "sidebar.fresh.json",
        SidebarHeartbeat::new(
            workspace_id.clone(),
            SidebarInstanceId::new(),
            MuxName::Tmux,
            SESSION_NAME,
            fresh_sock,
            None,
        ),
    );

    // A heartbeat whose `last_seen` is well past the TTL must be skipped even
    // though its socket is bound and listening.
    let stale_sock = runtime.sock_dir.join("sidebar.stale.sock");
    let stale_recv = UnixDatagram::bind(&stale_sock).expect("bind stale sock");
    let mut stale = SidebarHeartbeat::new(
        workspace_id.clone(),
        SidebarInstanceId::new(),
        MuxName::Tmux,
        SESSION_NAME,
        stale_sock,
        None,
    );
    stale.last_seen = Timestamp::now() - Duration::from_secs(60);
    write_heartbeat(&runtime, "sidebar.stale.json", stale);

    let signaled = reload_all(&runtime).expect("reload_sidebars");
    assert_eq!(signaled, 1, "only the fresh sidebar should be signaled");

    recv.set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    let mut buf = [0_u8; 4096];
    let n = recv.recv(&mut buf).expect("fresh sidebar receives reload");
    assert_eq!(
        &buf[..n],
        RELOAD_CONTROL_WORD.as_bytes(),
        "reload must not ride the version-gated sidebar event envelope",
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
    std::fs::write(
        runtime.heartbeat_dir.join(filename),
        serde_json::to_vec(&hb).expect("serialize heartbeat"),
    )
    .expect("write heartbeat");
}
