//! Verifies `rimz reload` delivery: `reload_sidebars` posts the `reload`
//! control word to every fresh sidebar's wakeup socket and skips stale ones,
//! returning the count it signaled. The renderer decodes `reload` into a
//! re-exec (covered by `rimz-sidebar`'s `input` unit tests).
//!
//! No live mux needed — we plant heartbeats and bound sockets directly under a
//! `RuntimePaths::under` root and call the library function.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::os::unix::net::UnixDatagram;
use std::time::Duration;

use jiff::Timestamp;
use rimz::ids::{MuxName, SidebarInstanceId, WorkspaceId};
use rimz::ledger::RuntimePaths;
use rimz::ledger::wakeup::{RELOAD_WAKEUP, reload_sidebars};
use rimz::schema::heartbeat::SidebarHeartbeat;
use tempfile::TempDir;

const SESSION_NAME: &str = "rimz-reload-test";

#[test]
fn reload_signals_fresh_sidebars_and_skips_stale() {
    let tempdir = TempDir::new().expect("tempdir");
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

    let signaled = reload_sidebars(&runtime).expect("reload_sidebars");
    assert_eq!(signaled, 1, "only the fresh sidebar should be signaled");

    recv.set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    let mut buf = [0_u8; 64];
    let n = recv.recv(&mut buf).expect("fresh sidebar receives reload");
    assert_eq!(&buf[..n], RELOAD_WAKEUP);

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
