//! Verifies the presence poke contract (`rimz sidebar wake`): both reasons
//! refresh the presence stamp that flips the producer's pane TTL to event
//! mode, and only `panes-changed` datagrams a sidebar — the **eldest** fresh
//! heartbeat alone, because the wire word maps to a force-produce fetch and a
//! broadcast would fork an N-way produce storm per topology change.
//!
//! No live Zellij needed; the wake path never touches the mux at all, and the
//! `zellij-trace` shim proves it stays that way. `unsafe_code = "forbid"` is
//! workspace-wide including tests, so env is seeded onto the `rimz`
//! subprocess rather than mutated in-process (the `wakeup_pipe` discipline).

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use rimz::ids::{MuxName, SidebarInstanceId, WorkspaceId};
use rimz::ledger::RuntimePaths;
use rimz::schema::heartbeat::SidebarHeartbeat;
use rimz::sidebar::snapshot::{PresenceStamp, presence_stamp_path};
use tempfile::TempDir;

const SESSION_NAME: &str = "rimz-presence-wake-test";

/// The eldest of the two planted instance ids — UUIDv7 order, the same the
/// producer election uses. Fixed ids make the eldest pick deterministic.
const ELDEST_ID: &str = "sb_019e8c565bbd708097fce9514f79da04";
const YOUNGER_ID: &str = "sb_019e8c565bbd7b22854f93a905e1034c";

struct WakeEnv {
    _tempdir: TempDir,
    project_root: PathBuf,
    state_root: PathBuf,
    runtime_root: PathBuf,
    trace_log: PathBuf,
    workspace_id: WorkspaceId,
    runtime: RuntimePaths,
}

impl WakeEnv {
    fn new() -> Self {
        let tempdir = TempDir::new().expect("tempdir");
        let project_root = tempdir.path().join("project");
        let state_root = tempdir.path().join("state");
        let runtime_root = tempdir.path().join("runtime");
        let trace_log = tempdir.path().join("zellij-trace.log");
        std::fs::create_dir_all(&project_root).expect("mkdir project");
        std::fs::create_dir_all(&state_root).expect("mkdir state");
        std::fs::create_dir_all(&runtime_root).expect("mkdir runtime");

        let workspace_id = WorkspaceId::from_project_root(&project_root);
        let runtime =
            RuntimePaths::under(workspace_id.clone(), &runtime_root).expect("RuntimePaths::under");
        runtime.ensure_dirs().expect("ensure runtime dirs");

        Self {
            _tempdir: tempdir,
            project_root,
            state_root,
            runtime_root,
            trace_log,
            workspace_id,
            runtime,
        }
    }

    fn bind_socket(&self, name: &str) -> std::os::unix::net::UnixDatagram {
        let path = self.runtime.sock_dir.join(name);
        let socket = std::os::unix::net::UnixDatagram::bind(&path).expect("bind wakeup socket");
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        socket
    }

    fn plant_heartbeat(&self, filename: &str, instance_id: &str, socket_name: &str) {
        let hb = SidebarHeartbeat::new(
            self.workspace_id.clone(),
            SidebarInstanceId::parse(instance_id).expect("fixed instance id parses"),
            MuxName::Zellij,
            SESSION_NAME,
            self.runtime.sock_dir.join(socket_name),
            None,
        );
        std::fs::write(
            self.runtime.heartbeat_dir.join(filename),
            serde_json::to_vec(&hb).expect("serialize heartbeat"),
        )
        .expect("write heartbeat");
    }

    /// Run `rimz sidebar wake` with this environment's scoped XDG roots and
    /// the zellij-trace shim standing in for any (erroneous) mux fork.
    fn wake(&self, reason: &str, explicit_workspace: bool) -> std::process::Output {
        let mut command = Command::new(rimz_cli_path());
        command.args(["sidebar", "wake", "--reason", reason]);
        if explicit_workspace {
            command.args(["--workspace-id", self.workspace_id.as_str()]);
        }
        command
            .current_dir(&self.project_root)
            .env("XDG_STATE_HOME", &self.state_root)
            .env("XDG_RUNTIME_DIR", &self.runtime_root)
            .env("RIMZ_ZELLIJ_BIN", trace_shim_path())
            .env("RIMZ_TEST_ZELLIJ_LOG", &self.trace_log)
            .env_remove("ZELLIJ")
            .env_remove("ZELLIJ_PANE_ID")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .expect("spawn rimz sidebar wake")
    }

    fn read_stamp(&self) -> Option<PresenceStamp> {
        let bytes = std::fs::read(presence_stamp_path(&self.runtime)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn assert_no_mux_fork(&self) {
        let lines = std::fs::read(&self.trace_log)
            .map(|bytes| String::from_utf8_lossy(&bytes).lines().count())
            .unwrap_or(0);
        assert_eq!(lines, 0, "the wake path must never fork the mux client");
    }
}

/// A datagram never arrives on `recv`: the wake subprocess has already exited,
/// so anything it sent is queued — a short timeout reads as proof of absence.
fn assert_no_datagram(recv: &std::os::unix::net::UnixDatagram, who: &str) {
    recv.set_read_timeout(Some(Duration::from_millis(100)))
        .expect("tighten read timeout");
    let mut buf = [0u8; 256];
    match recv.recv(&mut buf) {
        Ok(len) => panic!(
            "{who} must receive no datagram, got {:?}",
            String::from_utf8_lossy(&buf[..len])
        ),
        Err(err) => assert!(
            matches!(
                err.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "{who}: expected a recv timeout, got {err}"
        ),
    }
}

#[test]
fn wake_panes_changed_datagrams_the_eldest_only_and_stamps() {
    let env = WakeEnv::new();
    if crate::common::af_unix_bind_sandboxed(&env.runtime.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let recv_eldest = env.bind_socket("sidebar.eldest.sock");
    let recv_younger = env.bind_socket("sidebar.younger.sock");
    // Plant younger first so the eldest pick is the id order, never file order.
    env.plant_heartbeat("sidebar.younger.json", YOUNGER_ID, "sidebar.younger.sock");
    env.plant_heartbeat("sidebar.eldest.json", ELDEST_ID, "sidebar.eldest.sock");

    let output = env.wake("panes-changed", true);
    assert!(
        output.status.success(),
        "wake failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let mut buf = [0u8; 256];
    let len = recv_eldest
        .recv(&mut buf)
        .expect("the eldest sidebar receives the panes_changed poke");
    assert_eq!(&buf[..len], b"panes_changed");
    assert_no_datagram(&recv_younger, "the younger sidebar");

    let stamp = env.read_stamp().expect("panes-changed writes the stamp");
    assert!(stamp.written_at_ms > 0);
    env.assert_no_mux_fork();
}

#[test]
fn wake_alive_stamps_without_a_datagram() {
    let env = WakeEnv::new();
    if crate::common::af_unix_bind_sandboxed(&env.runtime.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let recv_eldest = env.bind_socket("sidebar.eldest.sock");
    env.plant_heartbeat("sidebar.eldest.json", ELDEST_ID, "sidebar.eldest.sock");

    let output = env.wake("alive", true);
    assert!(
        output.status.success(),
        "wake failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    assert_no_datagram(&recv_eldest, "a keepalive poke");
    let stamp = env.read_stamp().expect("alive writes the stamp");
    assert!(stamp.written_at_ms > 0);
    env.assert_no_mux_fork();
}

#[test]
fn wake_without_live_sidebars_still_stamps_via_cwd_resolution() {
    // No heartbeats at all (a headless room) and no --workspace-id: the wake
    // resolves the workspace from cwd like every participant command, writes
    // the stamp, and exits 0 — the channel stays alive for the producer even
    // when no renderer is up to poke.
    let env = WakeEnv::new();
    let output = env.wake("panes-changed", false);
    assert!(
        output.status.success(),
        "wake failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stamp = env.read_stamp().expect("stamp written with no sidebars");
    assert!(stamp.written_at_ms > 0);
    env.assert_no_mux_fork();
}

fn trace_shim_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| PathBuf::from(env!("CARGO_BIN_EXE_zellij-trace")))
        .clone()
}

fn rimz_cli_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| PathBuf::from(env!("CARGO_BIN_EXE_rimz")))
        .clone()
}
