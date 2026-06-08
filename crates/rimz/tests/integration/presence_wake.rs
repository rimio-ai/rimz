//! Verifies the presence channel end to end at the CLI seam.
//!
//! The poke contract (`rimz sidebar wake`): every reason refreshes the presence
//! stamp that flips the producer's pane TTL to event mode. Every reason but
//! `alive` broadcasts a typed sidebar event to every fresh heartbeat — exact
//! command/focus/open/close events update renderer-owned overlays and never
//! patch `snapshot.json`; a sparse poke degrades to the identity-free
//! `PanesChanged` nudge. A producer publication broadcasts the same typed
//! event envelope so consumers refold from cache instead of producing.
//!
//! The producer contract (`rimz sidebar snapshot`): with a fresh stamp, a
//! pane cache far past the poll TTL is served with **zero** mux forks — the
//! layer's whole point — and without the stamp the same cache is stale and
//! the producer forks `list-panes` exactly as before the layer landed.
//!
//! No live Zellij needed; the `zellij-trace` shim stands in for every mux
//! fork and its log is the proof either way. `unsafe_code = "forbid"` is
//! workspace-wide including tests, so env is seeded onto the `rimz`
//! subprocess rather than mutated in-process (the `wakeup_pipe` discipline).

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use rimz::feed::PaneRef;
use rimz::ids::{MuxName, PaneId, SidebarInstanceId, WorkspaceId};
use rimz::ledger::RuntimePaths;
use rimz::schema::heartbeat::SidebarHeartbeat;
use rimz::schema::pane_topology::{PaneTopologyCache, PaneTopologyPane};
use rimz::sidebar::cache::read_pane_topology_cache;
use rimz::sidebar::snapshot::{
    PresenceStamp, assemble_frame, presence_stamp_path, read_snapshot_cache, unix_now_ms,
};
use tempfile::TempDir;

use crate::common::ScrubSessionEnvExt;

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
    /// Scoped `HOME`: the producer's enrichment lanes (agent transcripts,
    /// spending) discover under it, so an unscoped run would walk this
    /// machine's real agent history.
    home_root: PathBuf,
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
        let home_root = tempdir.path().join("home");
        let trace_log = tempdir.path().join("zellij-trace.log");
        std::fs::create_dir_all(&project_root).expect("mkdir project");
        std::fs::create_dir_all(&state_root).expect("mkdir state");
        std::fs::create_dir_all(&runtime_root).expect("mkdir runtime");
        std::fs::create_dir_all(&home_root).expect("mkdir home");

        let workspace_id = WorkspaceId::from_project_root(&project_root);
        let runtime =
            RuntimePaths::under(workspace_id.clone(), &runtime_root).expect("RuntimePaths::under");
        runtime.ensure_dirs().expect("ensure runtime dirs");

        Self {
            _tempdir: tempdir,
            project_root,
            state_root,
            runtime_root,
            home_root,
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
        self.wake_with(reason, explicit_workspace, &[])
    }

    fn wake_with(
        &self,
        reason: &str,
        explicit_workspace: bool,
        extra_args: &[&str],
    ) -> std::process::Output {
        let mut command = Command::new(rimz_cli_path());
        command.scrub_session_env();
        command.args(["sidebar", "wake", "--reason", reason]);
        if explicit_workspace {
            command.args(["--workspace-id", self.workspace_id.as_str()]);
        }
        command.args(extra_args);
        command
            .current_dir(&self.project_root)
            .env("HOME", &self.home_root)
            .env("XDG_STATE_HOME", &self.state_root)
            .env("XDG_RUNTIME_DIR", &self.runtime_root)
            .env("RIMZ_ZELLIJ_BIN", trace_shim_path())
            .env("RIMZ_TEST_ZELLIJ_LOG", &self.trace_log)
            .output()
            .expect("spawn rimz sidebar wake")
    }

    fn read_stamp(&self) -> Option<PresenceStamp> {
        let bytes = std::fs::read(presence_stamp_path(&self.runtime)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Seed the shared pane cache with an empty frame produced `age` ago —
    /// fresh under the event-mode TTL, stale under the poll TTL.
    fn seed_pane_cache(&self, age: Duration) {
        let cache = assemble_frame(
            Vec::new(),
            unix_now_ms().saturating_sub(age.as_millis() as u64),
            SESSION_NAME,
        );
        std::fs::write(
            self.runtime.root.join("snapshot.json"),
            serde_json::to_vec(&cache).expect("serialize pane cache"),
        )
        .expect("seed pane cache");
    }

    fn seed_pane_cache_with_shell(&self, pane_id: &str, produced_at_ms: u64) {
        let cache = assemble_frame(
            vec![PaneRef {
                pane_id: PaneId::from_parts(MuxName::Zellij, pane_id),
                session_name: SESSION_NAME.to_owned(),
                view_id: Some("tab_0".to_owned()),
                view_kind: Some(rimz::ids::ViewKind::Tab),
                view_name: None,
                is_focused: true,
                command: Some("zsh".to_owned()),
                spawn_command: None,
                cwd: Some(self.project_root.to_string_lossy().into_owned()),
                pane_pid: None,
                pane_process_start: None,
                resumed_session_id: None,
            }],
            produced_at_ms,
            SESSION_NAME,
        );
        std::fs::write(
            self.runtime.root.join("snapshot.json"),
            serde_json::to_vec(&cache).expect("serialize pane cache"),
        )
        .expect("seed pane cache");
    }

    fn seed_pane_cache_with_focus(&self, produced_at_ms: u64) {
        let mk_pane = |raw: &str, command: &str, is_focused: bool| PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, raw),
            session_name: SESSION_NAME.to_owned(),
            view_id: Some("tab_0".to_owned()),
            view_kind: Some(rimz::ids::ViewKind::Tab),
            view_name: None,
            is_focused,
            command: Some(command.to_owned()),
            spawn_command: None,
            cwd: Some(self.project_root.to_string_lossy().into_owned()),
            pane_pid: None,
            pane_process_start: None,
            resumed_session_id: None,
        };
        let cache = assemble_frame(
            vec![
                mk_pane("terminal_6", "rimz-sidebar", false),
                mk_pane("terminal_7", "zsh", true),
                mk_pane("terminal_8", "zsh", false),
            ],
            produced_at_ms,
            SESSION_NAME,
        );
        std::fs::write(
            self.runtime.root.join("snapshot.json"),
            serde_json::to_vec(&cache).expect("serialize pane cache"),
        )
        .expect("seed pane cache");
    }

    fn topology_cache(&self, produced_at_ms: u64) -> PaneTopologyCache {
        PaneTopologyCache {
            session_name: SESSION_NAME.to_owned(),
            produced_at_ms,
            panes: vec![
                PaneTopologyPane {
                    id: 6,
                    is_plugin: false,
                    is_held: false,
                    exited: false,
                    is_suppressed: false,
                    is_focused: false,
                    tab_position: 0,
                    tab_name: Some("main".to_owned()),
                    pane_columns: Some(20),
                    pane_x: Some(0),
                    title: Some("rimz-sidebar".to_owned()),
                    pane_command: Some("rimz-sidebar".to_owned()),
                    terminal_command: Some("rimz sidebar serve".to_owned()),
                },
                PaneTopologyPane {
                    id: 7,
                    is_plugin: false,
                    is_held: false,
                    exited: false,
                    is_suppressed: false,
                    is_focused: true,
                    tab_position: 0,
                    tab_name: Some("main".to_owned()),
                    pane_columns: Some(100),
                    pane_x: Some(20),
                    title: Some("zsh".to_owned()),
                    pane_command: Some("zsh".to_owned()),
                    terminal_command: Some("/bin/zsh".to_owned()),
                },
            ],
        }
    }

    /// Run the producer path (`rimz sidebar snapshot`) with this environment's
    /// scoped roots and the trace shim standing in for every mux fork.
    fn snapshot(&self) -> std::process::Output {
        let mut command = Command::new(rimz_cli_path());
        command
            .scrub_session_env()
            .args([
                "sidebar",
                "snapshot",
                "--mux",
                "zellij",
                "--session-name",
                SESSION_NAME,
                "--json",
            ])
            .current_dir(&self.project_root)
            .env("HOME", &self.home_root)
            .env("XDG_STATE_HOME", &self.state_root)
            .env("XDG_RUNTIME_DIR", &self.runtime_root)
            .env("RIMZ_ZELLIJ_BIN", trace_shim_path())
            .env("RIMZ_TEST_ZELLIJ_LOG", &self.trace_log)
            .output()
            .expect("spawn rimz sidebar snapshot")
    }

    fn trace_lines(&self) -> Vec<String> {
        std::fs::read(&self.trace_log)
            .map(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
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

fn recv_sidebar_event(recv: &std::os::unix::net::UnixDatagram, who: &str) -> serde_json::Value {
    let mut buf = [0u8; 4096];
    let len = recv
        .recv(&mut buf)
        .unwrap_or_else(|err| panic!("{who} receives sidebar event: {err}"));
    serde_json::from_slice(&buf[..len]).expect("sidebar event is JSON")
}

fn assert_sidebar_envelope(
    event: &serde_json::Value,
    workspace_id: &WorkspaceId,
    session: Option<&str>,
) {
    assert_eq!(
        event["v"],
        rimz::schema::sidebar_event::SIDEBAR_EVENT_VERSION
    );
    assert_eq!(
        event["workspace_id"],
        serde_json::to_value(workspace_id).expect("workspace id serializes"),
    );
    match session {
        Some(session) => assert_eq!(event["session_name"], session),
        None => assert!(
            event.get("session_name").is_none(),
            "a workspace-scoped envelope carries no session_name: {event}"
        ),
    }
    assert!(event["sent_at_ms"].as_u64().is_some());
}

#[test]
fn wake_panes_changed_broadcasts_topology_nudge_and_stamps() {
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

    // The plugin's generic topology poke carries no session or pane id — the
    // identity-free nudge must still reach every fresh sidebar so the producer
    // pulls fresh panes within one poke, never the event-mode pane TTL.
    let output = env.wake("panes-changed", true);
    assert!(
        output.status.success(),
        "wake failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    for (name, recv) in [("eldest", recv_eldest), ("younger", recv_younger)] {
        let event = recv_sidebar_event(&recv, name);
        assert_sidebar_envelope(&event, &env.workspace_id, None);
        assert_eq!(event["event"]["kind"], "panes_changed");
    }

    let stamp = env.read_stamp().expect("panes-changed writes the stamp");
    assert!(stamp.written_at_ms > 0);
    env.assert_no_mux_fork();
}

#[test]
fn wake_pane_opened_and_closed_broadcast_card_events() {
    let env = WakeEnv::new();
    if crate::common::af_unix_bind_sandboxed(&env.runtime.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let recv_eldest = env.bind_socket("sidebar.eldest.sock");
    env.plant_heartbeat("sidebar.eldest.json", ELDEST_ID, "sidebar.eldest.sock");

    let output = env.wake_with(
        "pane-opened",
        true,
        &[
            "--session-name",
            SESSION_NAME,
            "--pane-id",
            "terminal_9",
            "--command-arg",
            "zsh",
        ],
    );
    assert!(output.status.success());
    let event = recv_sidebar_event(&recv_eldest, "eldest");
    assert_sidebar_envelope(&event, &env.workspace_id, Some(SESSION_NAME));
    assert_eq!(event["event"]["kind"], "pane_opened");
    assert_eq!(event["event"]["pane_id"], "zellij:terminal_9");
    assert_eq!(event["event"]["command"], "zsh");

    let output = env.wake_with(
        "pane-closed",
        true,
        &["--session-name", SESSION_NAME, "--pane-id", "terminal_9"],
    );
    assert!(output.status.success());
    let event = recv_sidebar_event(&recv_eldest, "eldest");
    assert_sidebar_envelope(&event, &env.workspace_id, Some(SESSION_NAME));
    assert_eq!(event["event"]["kind"], "pane_closed");
    assert_eq!(event["event"]["pane_id"], "zellij:terminal_9");
    env.assert_no_mux_fork();
}

#[test]
fn wake_focus_stranded_broadcasts_renderer_action_event() {
    let env = WakeEnv::new();
    if crate::common::af_unix_bind_sandboxed(&env.runtime.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let recv_eldest = env.bind_socket("sidebar.eldest.sock");
    env.plant_heartbeat("sidebar.eldest.json", ELDEST_ID, "sidebar.eldest.sock");

    let output = env.wake_with(
        "focus-stranded",
        true,
        &["--session-name", SESSION_NAME, "--pane-id", "terminal_6"],
    );
    assert!(
        output.status.success(),
        "wake failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let event = recv_sidebar_event(&recv_eldest, "eldest");
    assert_sidebar_envelope(&event, &env.workspace_id, Some(SESSION_NAME));
    assert_eq!(event["event"]["kind"], "focus_stranded");
    assert_eq!(event["event"]["pane_id"], "zellij:terminal_6");
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
fn wake_with_topology_writes_runtime_cache_without_mux_fork() {
    let env = WakeEnv::new();
    let topology = env.topology_cache(unix_now_ms());
    let topology_json = serde_json::to_string(&topology).expect("serialize topology");

    let output = env.wake_with(
        "panes-changed",
        true,
        &[
            "--session-name",
            SESSION_NAME,
            "--topology",
            topology_json.as_str(),
        ],
    );
    assert!(
        output.status.success(),
        "wake failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let cached =
        read_pane_topology_cache(&env.runtime, SESSION_NAME).expect("topology cache written");
    assert_eq!(cached, topology);
    env.assert_no_mux_fork();
}

#[test]
fn snapshot_producer_uses_topology_cache_without_list_panes_fork() {
    let env = WakeEnv::new();
    let topology = env.topology_cache(unix_now_ms());
    let topology_json = serde_json::to_string(&topology).expect("serialize topology");
    let wake = env.wake_with(
        "alive",
        true,
        &[
            "--session-name",
            SESSION_NAME,
            "--topology",
            topology_json.as_str(),
        ],
    );
    assert!(wake.status.success(), "topology wake must succeed");

    let output = env.snapshot();
    assert!(
        output.status.success(),
        "snapshot failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        env.trace_lines(),
        Vec::<String>::new(),
        "fresh topology cache must avoid zellij list-panes",
    );
    let cached = read_snapshot_cache(&env.runtime.root.join("snapshot.json"), SESSION_NAME)
        .expect("snapshot cache published from topology");
    let panes = cached.to_pane_refs();
    assert!(
        panes.iter().any(|pane| pane.pane_id.raw() == "terminal_7"
            && pane.command.as_deref() == Some("zsh")
            && pane.view_name.as_deref() == Some("main")),
        "snapshot panes should include topology-derived working pane: {panes:?}",
    );
}

#[test]
fn pane_frame_publication_broadcasts_to_all_fresh_sidebars() {
    let env = WakeEnv::new();
    if crate::common::af_unix_bind_sandboxed(&env.runtime.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let recv_eldest = env.bind_socket("sidebar.eldest.sock");
    let recv_younger = env.bind_socket("sidebar.younger.sock");
    env.plant_heartbeat("sidebar.eldest.json", ELDEST_ID, "sidebar.eldest.sock");
    env.plant_heartbeat("sidebar.younger.json", YOUNGER_ID, "sidebar.younger.sock");

    let count = rimz::ledger::wakeup::wake_sidebars_pane_frame_published(&env.runtime)
        .expect("publication wakeup walks sidebars");
    assert_eq!(count, 2);

    for (name, recv) in [("eldest", recv_eldest), ("younger", recv_younger)] {
        let event = recv_sidebar_event(&recv, name);
        assert_sidebar_envelope(&event, &env.workspace_id, None);
        assert_eq!(event["event"]["kind"], "pane_frame_published");
    }
}

#[test]
fn wake_command_changed_broadcasts_event_without_patching_pane_frame() {
    let env = WakeEnv::new();
    if crate::common::af_unix_bind_sandboxed(&env.runtime.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let produced_at_ms = unix_now_ms().saturating_sub(5_000);
    env.seed_pane_cache_with_shell("terminal_7", produced_at_ms);
    let recv_eldest = env.bind_socket("sidebar.eldest.sock");
    let recv_younger = env.bind_socket("sidebar.younger.sock");
    env.plant_heartbeat("sidebar.eldest.json", ELDEST_ID, "sidebar.eldest.sock");
    env.plant_heartbeat("sidebar.younger.json", YOUNGER_ID, "sidebar.younger.sock");

    let output = env.wake_with(
        "command-changed",
        true,
        &[
            "--session-name",
            SESSION_NAME,
            "--pane-id",
            "terminal_7",
            "--command-arg",
            "codex",
        ],
    );
    assert!(
        output.status.success(),
        "wake failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    for (name, recv) in [("eldest", recv_eldest), ("younger", recv_younger)] {
        let event = recv_sidebar_event(&recv, name);
        assert_sidebar_envelope(&event, &env.workspace_id, Some(SESSION_NAME));
        assert_eq!(event["event"]["kind"], "command_changed");
        assert_eq!(event["event"]["pane_id"], "zellij:terminal_7");
        assert_eq!(event["event"]["command"], "codex");
    }
    let cached = read_snapshot_cache(&env.runtime.root.join("snapshot.json"), SESSION_NAME)
        .expect("pane cache remains readable");
    assert_eq!(
        cached.produced_at_ms, produced_at_ms,
        "typed overlay events must not masquerade as a fresh mux read",
    );
    let cached_panes = cached.to_pane_refs();
    assert_eq!(cached_panes[0].command.as_deref(), Some("zsh"));
    assert!(cached_panes[0].pane_process_start.is_none());
    env.assert_no_mux_fork();
}

#[test]
fn wake_focus_changed_broadcasts_event_without_patching_pane_frame() {
    let env = WakeEnv::new();
    if crate::common::af_unix_bind_sandboxed(&env.runtime.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let produced_at_ms = unix_now_ms().saturating_sub(5_000);
    env.seed_pane_cache_with_focus(produced_at_ms);
    let recv_eldest = env.bind_socket("sidebar.eldest.sock");
    let recv_younger = env.bind_socket("sidebar.younger.sock");
    env.plant_heartbeat("sidebar.eldest.json", ELDEST_ID, "sidebar.eldest.sock");
    env.plant_heartbeat("sidebar.younger.json", YOUNGER_ID, "sidebar.younger.sock");

    let output = env.wake_with(
        "focus-changed",
        true,
        &[
            "--session-name",
            SESSION_NAME,
            "--focused-pane-id",
            "terminal_8",
            "--unfocused-pane-id",
            "terminal_7",
        ],
    );
    assert!(
        output.status.success(),
        "wake failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    for (name, recv) in [("eldest", recv_eldest), ("younger", recv_younger)] {
        let event = recv_sidebar_event(&recv, name);
        assert_sidebar_envelope(&event, &env.workspace_id, Some(SESSION_NAME));
        assert_eq!(event["event"]["kind"], "focus_changed");
        assert_eq!(event["event"]["focused"][0], "zellij:terminal_8");
        assert_eq!(event["event"]["unfocused"][0], "zellij:terminal_7");
    }
    let cached = read_snapshot_cache(&env.runtime.root.join("snapshot.json"), SESSION_NAME)
        .expect("pane cache remains readable");
    assert_eq!(
        cached.produced_at_ms, produced_at_ms,
        "typed overlay events must not masquerade as a fresh mux read",
    );
    let cached_panes = cached.to_pane_refs();
    let terminal_7 = cached_panes
        .iter()
        .find(|pane| pane.pane_id.raw() == "terminal_7")
        .expect("terminal_7 remains present");
    let terminal_8 = cached_panes
        .iter()
        .find(|pane| pane.pane_id.raw() == "terminal_8")
        .expect("terminal_8 remains present");
    let sidebar = cached_panes
        .iter()
        .find(|pane| pane.pane_id.raw() == "terminal_6")
        .expect("sidebar remains present");
    assert!(!sidebar.is_focused);
    assert!(terminal_7.is_focused);
    assert!(!terminal_8.is_focused);
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

#[test]
fn event_mode_serves_a_stale_poll_cache_with_zero_mux_forks() {
    // The layer's acceptance criterion at the CLI seam: stamp fresh → a pane
    // cache 5s old (poll mode would have re-forked ~6 times over) is served
    // as-is, and the trace shim proves the producer forked nothing.
    let env = WakeEnv::new();
    let wake = env.wake("alive", false);
    assert!(wake.status.success(), "stamp write must succeed");
    env.seed_pane_cache(Duration::from_secs(5));

    let output = env.snapshot();
    assert!(
        output.status.success(),
        "snapshot failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        env.trace_lines(),
        Vec::<String>::new(),
        "event mode must serve the cache without a single mux fork",
    );
}

#[test]
fn without_a_stamp_the_same_cache_is_stale_and_the_producer_polls() {
    // The control: byte-identical inputs minus the stamp read as poll mode,
    // so the 5s-old cache is past SNAPSHOT_CACHE_TTL and the producer forks
    // `list-panes` exactly as it did before the layer landed.
    let env = WakeEnv::new();
    env.seed_pane_cache(Duration::from_secs(5));

    let _ = env.snapshot();
    let forked_list_panes = env
        .trace_lines()
        .iter()
        .any(|line| line.contains("list-panes"));
    assert!(
        forked_list_panes,
        "poll mode must re-fork list-panes for a 5s-old cache; trace: {:?}",
        env.trace_lines(),
    );
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
