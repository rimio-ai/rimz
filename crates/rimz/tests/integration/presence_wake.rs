//! Verifies the presence channel end to end at the CLI seam.
//!
//! The poke contract (`rimz sidebar wake`): every reason refreshes the presence
//! stamp that flips the producer's pane TTL to event mode. Every reason but
//! `alive` broadcasts a typed sidebar event to every fresh heartbeat — exact
//! command/open/close events update renderer-owned overlays and never patch
//! `snapshot.json`; a sparse poke degrades to the identity-free
//! `PanesChanged` nudge.
//!
//! The producer contract (`rimz sidebar snapshot`): with a fresh stamp, a pane
//! cache far past the default TTL is served without a mux roster read — the
//! layer's whole point. Zellij pane discovery comes from plugin topology, not a
//! CLI fallback.
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

use rimz::ids::{MuxName, PaneId, SidebarInstanceId, WorkspaceId};
use rimz::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane, TopologyWriter};
use rimz::pane::PaneRef;
use rimz::sidebar::cache::{
    PresenceStamp, presence_stamp_path, read_pane_topology_cache, read_snapshot_cache,
};
use rimz::sidebar::frame::assemble_frame;
use rimz::sidebar::heartbeat::SidebarHeartbeat;
use rimz::sidebar::presence::read_topology_writer_conflict;
use rimz::sidebar::timing::unix_now_ms;
use rimz::store::RuntimePaths;
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
        let tempdir = tempfile::Builder::new()
            .prefix("rw")
            .tempdir_in("/tmp")
            .expect("short tempdir");
        let project_root = tempdir.path().join("project");
        let state_root = tempdir.path().join("state");
        let runtime_root = tempdir.path().join("runtime");
        let home_root = tempdir.path().join("home");
        let trace_log = tempdir.path().join("zellij-trace.log");
        std::fs::create_dir_all(&project_root).expect("mkdir project");
        std::fs::create_dir_all(&state_root).expect("mkdir state");
        std::fs::create_dir_all(&runtime_root).expect("mkdir runtime");
        std::fs::create_dir_all(&home_root).expect("mkdir home");
        let project_root = project_root.canonicalize().expect("canonical project");

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

    fn plugin_presence_log(&self) -> PathBuf {
        let state = rimz::StatePaths::under(self.workspace_id.clone(), &self.state_root)
            .expect("state paths");
        rimz::diag::plugin_presence::log(&state.root)
            .path()
            .to_owned()
    }

    fn topology_diag_log(&self) -> PathBuf {
        let state = rimz::StatePaths::under(self.workspace_id.clone(), &self.state_root)
            .expect("state paths");
        rimz::diag::DiagSink::under(
            state.root.clone(),
            self.workspace_id.clone(),
            SESSION_NAME,
            None,
        )
        .log_path()
        .expect("diagnostic log path")
    }

    fn topology_diag_kinds(&self) -> Vec<String> {
        std::fs::read_to_string(self.topology_diag_log())
            .map(|contents| {
                contents
                    .lines()
                    .map(|line| {
                        serde_json::from_str::<serde_json::Value>(line)
                            .expect("diagnostic line is JSON")["event"]["kind"]
                            .as_str()
                            .expect("diagnostic event kind")
                            .to_owned()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Seed the shared pane cache with a publishable shell frame produced
    /// `age` ago — fresh under the event-mode TTL, stale under the default TTL.
    fn seed_pane_cache(&self, age: Duration) {
        self.seed_pane_cache_with_shell(
            "terminal_7",
            unix_now_ms().saturating_sub(age.as_millis() as u64),
        );
    }

    fn seed_pane_cache_with_shell(&self, pane_id: &str, produced_at_ms: u64) {
        let pane = PaneId::from_parts(MuxName::Zellij, pane_id);
        let mut cache = assemble_frame(
            vec![PaneRef {
                pane_id: pane.clone(),
                session_name: SESSION_NAME.to_owned(),
                view_id: Some("tab_0".to_owned()),
                view_kind: Some(rimz::ids::ViewKind::Tab),
                view_name: None,
                title: None,
                is_floating: false,
                command: Some("zsh".to_owned()),
                foreground_cmdline: None,
                spawn_command: None,
                cwd: Some(self.project_root.to_string_lossy().into_owned()),
                pane_pid: None,
                pane_process_start: None,
                hosted_agent_kind: None,
                hosted_agent_process_start: None,
                resumed_session_id: None,
                elevated_agent: None,
                first_seen_at_ms: None,
            }],
            produced_at_ms,
            SESSION_NAME,
        );
        cache.viewed_panes = vec![pane];
        std::fs::write(
            self.runtime.pane_frame_path(),
            serde_json::to_vec(&cache).expect("serialize pane cache"),
        )
        .expect("seed pane cache");
    }

    fn topology_cache(&self, produced_at_ms: u64) -> PaneTopologyCache {
        PaneTopologyCache {
            session_name: SESSION_NAME.to_owned(),
            produced_at_ms,
            writer: None,
            focused_pane: Some(7),
            clients: None,
            panes: vec![
                PaneTopologyPane {
                    id: 6,
                    is_plugin: false,
                    is_held: false,
                    exited: false,
                    is_suppressed: false,
                    is_floating: false,
                    tab_position: 0,
                    tab_name: Some("main".to_owned()),
                    pane_columns: Some(20),
                    pane_x: Some(0),
                    title: Some("rimz-sidebar".to_owned()),
                    pane_command: Some("rimz-sidebar".to_owned()),
                    pane_cwd: None,
                    pane_pid: None,
                    terminal_command: Some("rimz sidebar serve".to_owned()),
                },
                PaneTopologyPane {
                    id: 7,
                    is_plugin: false,
                    is_held: false,
                    exited: false,
                    is_suppressed: false,
                    is_floating: false,
                    tab_position: 0,
                    tab_name: Some("main".to_owned()),
                    pane_columns: Some(100),
                    pane_x: Some(20),
                    title: Some("zsh".to_owned()),
                    pane_command: Some("zsh".to_owned()),
                    pane_cwd: None,
                    pane_pid: None,
                    terminal_command: Some("/bin/zsh".to_owned()),
                },
            ],
        }
    }

    fn topology_cache_with_writer(
        &self,
        produced_at_ms: u64,
        plugin_id: u32,
        loaded_at_ms: u64,
    ) -> PaneTopologyCache {
        let mut cache = self.topology_cache(produced_at_ms);
        cache.writer = Some(TopologyWriter {
            plugin_id,
            loaded_at_ms,
            build: None,
            config: None,
        });
        cache
    }

    fn topology_cache_for_tabs(
        &self,
        produced_at_ms: u64,
        panes: &[(u64, u64, &str)],
    ) -> PaneTopologyCache {
        PaneTopologyCache {
            session_name: SESSION_NAME.to_owned(),
            produced_at_ms,
            writer: None,
            focused_pane: panes.first().map(|(id, _, _)| *id),
            clients: None,
            panes: panes
                .iter()
                .map(|(id, tab_position, tab_name)| PaneTopologyPane {
                    id: *id,
                    is_plugin: false,
                    is_held: false,
                    exited: false,
                    is_suppressed: false,
                    is_floating: false,
                    tab_position: *tab_position,
                    tab_name: Some((*tab_name).to_owned()),
                    pane_columns: Some(100),
                    pane_x: Some(0),
                    title: Some("zsh".to_owned()),
                    pane_command: Some("zsh".to_owned()),
                    pane_cwd: None,
                    pane_pid: None,
                    terminal_command: Some("/bin/zsh".to_owned()),
                })
                .collect(),
        }
    }

    /// Run the producer path (`rimz sidebar snapshot`) with this environment's
    /// scoped roots and the trace shim standing in for every mux fork.
    fn snapshot(&self) -> std::process::Output {
        self.snapshot_with_list_clients(None)
    }

    fn snapshot_with_list_clients(&self, list_clients: Option<&str>) -> std::process::Output {
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
            .env("RIMZ_TEST_ZELLIJ_LOG", &self.trace_log);
        if let Some(list_clients) = list_clients {
            command.env("RIMZ_TEST_ZELLIJ_LIST_CLIENTS", list_clients);
        }
        command.output().expect("spawn rimz sidebar snapshot")
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

    fn assert_no_list_panes_fork(&self, context: &str) {
        let trace = self.trace_lines();
        let forked = trace.iter().any(|line| line.contains("list-panes"));
        assert!(
            !forked,
            "{context} must avoid zellij list-panes fallback: {trace:?}"
        );
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
    assert_eq!(event["v"], rimz::sidebar::events::SIDEBAR_EVENT_VERSION);
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
fn wake_alive_with_plugin_telemetry_records_a_sample() {
    let env = WakeEnv::new();
    let output = env.wake_with(
        "alive",
        true,
        &[
            "--session-name",
            SESSION_NAME,
            "--plugin-mem-pages",
            "42",
            "--plugin-uptime-ms",
            "1000",
            "--plugin-commands",
            "5",
            "--plugin-commands-failed",
            "2",
            "--plugin-zellij-version",
            "0.44.3",
        ],
    );
    assert!(
        output.status.success(),
        "wake failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let bytes = std::fs::read_to_string(env.plugin_presence_log()).expect("presence log exists");
    let lines = bytes.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 1);
    let sample: serde_json::Value = serde_json::from_str(lines[0]).expect("sample is JSON");
    assert_eq!(sample["session_name"], SESSION_NAME);
    assert!(sample["at_ms"].as_u64().is_some());
    assert_eq!(sample["pages"], 42);
    assert_eq!(sample["bytes"], 42 * 65_536);
    assert_eq!(sample["uptime_ms"], 1_000);
    assert_eq!(sample["commands"], 5);
    assert_eq!(sample["commands_failed"], 2);
    assert_eq!(sample["zellij_version"], "0.44.3");
    env.assert_no_mux_fork();
}

#[test]
fn wake_alive_without_telemetry_writes_no_sample() {
    let env = WakeEnv::new();
    let output = env.wake("alive", true);
    assert!(
        output.status.success(),
        "wake failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    assert!(
        !env.plugin_presence_log().exists(),
        "older plugins that omit telemetry args must not create a presence log"
    );
    env.assert_no_mux_fork();
}

#[test]
fn stale_topology_writer_rejects_the_whole_poke_and_throttles_diagnostics() {
    let env = WakeEnv::new();
    if crate::common::af_unix_bind_sandboxed(&env.runtime.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let recv = env.bind_socket("sidebar.eldest.sock");
    env.plant_heartbeat("sidebar.eldest.json", ELDEST_ID, "sidebar.eldest.sock");

    let accepted = env.topology_cache_with_writer(unix_now_ms(), 2, 200);
    let accepted_json = serde_json::to_string(&accepted).expect("serialize accepted topology");
    let output = env.wake_with(
        "alive",
        true,
        &[
            "--session-name",
            SESSION_NAME,
            "--topology",
            accepted_json.as_str(),
        ],
    );
    assert!(output.status.success(), "accepted writer wake must succeed");
    std::fs::remove_file(presence_stamp_path(&env.runtime)).expect("remove accepted wake stamp");

    let stale = env.topology_cache_with_writer(unix_now_ms(), 1, 100);
    let stale_json = serde_json::to_string(&stale).expect("serialize stale topology");
    let stale_args = [
        "--session-name",
        SESSION_NAME,
        "--topology",
        stale_json.as_str(),
        "--plugin-mem-pages",
        "42",
    ];
    let output = env.wake_with("panes-changed", true, &stale_args);
    assert_eq!(
        output.status.code(),
        Some(rimz::sidebar::presence::STALE_WRITER_EXIT_CODE),
        "stale writer wake reports the private retirement status",
    );

    assert_eq!(
        read_pane_topology_cache(&env.runtime, SESSION_NAME),
        Some(accepted.clone()),
        "stale writer must not replace accepted topology",
    );
    assert!(env.read_stamp().is_none(), "stale writer must not stamp");
    assert!(
        !env.plugin_presence_log().exists(),
        "stale writer must not append plugin telemetry",
    );
    assert_no_datagram(&recv, "a stale writer poke");
    let conflict = read_topology_writer_conflict(&env.runtime).expect("writer conflict sidecar");
    assert_eq!(conflict.stale_writer, stale.writer);
    assert_eq!(conflict.accepted_writer, accepted.writer);
    assert_eq!(conflict.rejected_count, 1);

    let output = env.wake_with("panes-changed", true, &stale_args);
    assert_eq!(
        output.status.code(),
        Some(rimz::sidebar::presence::STALE_WRITER_EXIT_CODE),
        "repeated stale writer wake keeps reporting retirement status",
    );
    assert_no_datagram(&recv, "a repeated stale writer poke");
    let conflict = read_topology_writer_conflict(&env.runtime).expect("updated conflict sidecar");
    assert_eq!(conflict.rejected_count, 2);
    assert_eq!(
        env.topology_diag_kinds()
            .iter()
            .filter(|kind| kind.as_str() == "topology_write_rejected")
            .count(),
        1,
        "repeated conflict inside the throttle window emits one diagnostic",
    );
    env.assert_no_mux_fork();
}

#[test]
fn stale_topology_cache_accepts_an_older_writer_takeover() {
    let env = WakeEnv::new();
    let stale_at = unix_now_ms()
        .saturating_sub(rimz::sidebar::timing::PRESENCE_STAMP_FRESH.as_millis() as u64 + 1);
    let prior = env.topology_cache_with_writer(stale_at, 2, 200);
    let prior_json = serde_json::to_string(&prior).expect("serialize prior topology");
    let output = env.wake_with(
        "alive",
        true,
        &[
            "--session-name",
            SESSION_NAME,
            "--topology",
            prior_json.as_str(),
        ],
    );
    assert!(output.status.success(), "prior writer wake must succeed");
    std::fs::remove_file(presence_stamp_path(&env.runtime)).expect("remove prior wake stamp");

    let takeover = env.topology_cache_with_writer(unix_now_ms(), 1, 100);
    let takeover_json = serde_json::to_string(&takeover).expect("serialize takeover topology");
    let output = env.wake_with(
        "alive",
        true,
        &[
            "--session-name",
            SESSION_NAME,
            "--topology",
            takeover_json.as_str(),
        ],
    );
    assert!(output.status.success(), "stale-cache takeover must succeed");

    assert_eq!(
        read_pane_topology_cache(&env.runtime, SESSION_NAME),
        Some(takeover),
        "older writer becomes accepted cache writer after staleness",
    );
    assert!(
        env.read_stamp().is_some(),
        "accepted takeover refreshes presence"
    );
    assert!(
        env.topology_diag_kinds()
            .iter()
            .any(|kind| kind == "topology_writer_changed"),
        "stale-cache takeover emits a writer-change diagnostic",
    );
    env.assert_no_mux_fork();
}

#[test]
fn malformed_topology_is_best_effort_while_wake_stamps_and_broadcasts() {
    let env = WakeEnv::new();
    if crate::common::af_unix_bind_sandboxed(&env.runtime.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let recv = env.bind_socket("sidebar.eldest.sock");
    env.plant_heartbeat("sidebar.eldest.json", ELDEST_ID, "sidebar.eldest.sock");

    let output = env.wake_with(
        "panes-changed",
        true,
        &["--session-name", SESSION_NAME, "--topology", "{not-json"],
    );
    assert!(
        output.status.success(),
        "malformed topology wake failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let event = recv_sidebar_event(&recv, "eldest");
    assert_sidebar_envelope(&event, &env.workspace_id, Some(SESSION_NAME));
    assert_eq!(event["event"]["kind"], "panes_changed");
    assert!(
        env.read_stamp().is_some(),
        "accepted wake refreshes presence"
    );
    assert!(
        read_pane_topology_cache(&env.runtime, SESSION_NAME).is_none(),
        "malformed topology publishes no cache",
    );
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

    let output = env.snapshot_with_list_clients(Some(
        "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n1 terminal_6 rimz-sidebar\n",
    ));
    assert!(
        output.status.success(),
        "snapshot failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    env.assert_no_list_panes_fork("fresh topology cache");
    assert!(
        env.trace_lines()
            .iter()
            .any(|line| line.contains("list-clients")),
        "topology-served producer must still refresh client focus",
    );
    let cached = read_snapshot_cache(&env.runtime.pane_frame_path(), SESSION_NAME)
        .expect("snapshot cache published from topology");
    assert_eq!(
        cached.viewed_panes,
        vec![PaneId::from_parts(MuxName::Zellij, "terminal_6")],
        "topology cache avoids zellij list-panes but refreshes viewed panes even when the sample lags",
    );
    assert_eq!(
        cached.presence.map(|presence| presence.human_clients),
        Some(1),
        "client focus sample carries attached-client presence",
    );
    assert_eq!(
        cached.observed_at_ms, topology.produced_at_ms,
        "topology cache production time is the frame observation time"
    );
    assert_eq!(
        cached.focused_pane.as_ref().map(PaneId::raw),
        Some("terminal_7"),
        "topology authoritative focus should win over a stale client-view sample"
    );
    let panes = cached.to_pane_refs();
    assert!(
        panes.iter().any(|pane| pane.pane_id.raw() == "terminal_7"
            && pane.command.as_deref() == Some("zsh")
            && pane.view_name.as_deref() == Some("main")),
        "snapshot panes should include topology-derived working pane: {panes:?}",
    );
}

#[test]
fn focus_changed_wake_publishes_authoritative_topology_focus_without_list_panes_fork() {
    let env = WakeEnv::new();
    let mut topology =
        env.topology_cache_for_tabs(unix_now_ms(), &[(7, 0, "main"), (8, 1, "agent")]);
    topology.focused_pane = Some(8);
    let topology_json = serde_json::to_string(&topology).expect("serialize topology");
    let wake = env.wake_with(
        "focus-changed",
        true,
        &[
            "--session-name",
            SESSION_NAME,
            "--unfocused-pane-id",
            "terminal_7",
            "--focused-pane-id",
            "terminal_8",
            "--topology",
            topology_json.as_str(),
        ],
    );
    assert!(
        wake.status.success(),
        "focus-changed topology wake failed: stderr={}",
        String::from_utf8_lossy(&wake.stderr),
    );

    let output = env.snapshot_with_list_clients(Some(""));
    assert!(
        output.status.success(),
        "snapshot failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    env.assert_no_list_panes_fork("fresh focus-changed topology");
    let cached = read_snapshot_cache(&env.runtime.pane_frame_path(), SESSION_NAME)
        .expect("snapshot cache published from focus-changed topology");
    assert_eq!(
        cached.focused_pane.as_ref().map(PaneId::raw),
        Some("terminal_8"),
        "focus-changed topology should update focus without waiting for list-clients",
    );
}

#[test]
fn pane_closed_pruned_topology_publishes_without_a_list_panes_fork() {
    let env = WakeEnv::new();
    let full = env.topology_cache_for_tabs(unix_now_ms(), &[(7, 0, "main"), (8, 1, "agent")]);
    let full_json = serde_json::to_string(&full).expect("serialize full topology");
    let wake = env.wake_with(
        "alive",
        true,
        &[
            "--session-name",
            SESSION_NAME,
            "--topology",
            full_json.as_str(),
        ],
    );
    assert!(wake.status.success(), "full topology wake must succeed");
    let output = env.snapshot();
    assert!(
        output.status.success(),
        "snapshot failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    let cached = read_snapshot_cache(&env.runtime.pane_frame_path(), SESSION_NAME)
        .expect("snapshot cache published from full topology");
    let panes = cached.to_pane_refs();
    assert!(panes.iter().any(|pane| pane.pane_id.raw() == "terminal_7"));
    assert!(panes.iter().any(|pane| pane.pane_id.raw() == "terminal_8"));

    let pruned = env.topology_cache_for_tabs(unix_now_ms(), &[(7, 0, "main")]);
    let pruned_json = serde_json::to_string(&pruned).expect("serialize pruned topology");
    let wake = env.wake_with(
        "alive",
        true,
        &[
            "--session-name",
            SESSION_NAME,
            "--topology",
            pruned_json.as_str(),
        ],
    );
    assert!(
        wake.status.success(),
        "PaneClosed-pruned topology wake must succeed"
    );
    std::fs::remove_file(env.runtime.pane_frame_path()).expect("drop old pane frame");

    let output = env.snapshot();
    assert!(
        output.status.success(),
        "snapshot failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    env.assert_no_list_panes_fork("fresh PaneClosed-pruned topology");
    let cached = read_snapshot_cache(&env.runtime.pane_frame_path(), SESSION_NAME)
        .expect("snapshot cache published from pruned topology");
    let panes = cached.to_pane_refs();
    assert!(panes.iter().any(|pane| pane.pane_id.raw() == "terminal_7"));
    assert!(
        !panes.iter().any(|pane| pane.pane_id.raw() == "terminal_8"),
        "PaneClosed-pruned topology drops the closed pane: {panes:?}",
    );
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
    let cached = read_snapshot_cache(&env.runtime.pane_frame_path(), SESSION_NAME)
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
fn wake_command_changed_treats_agents_launch_as_nudge_and_strips_topology_command() {
    let env = WakeEnv::new();
    if crate::common::af_unix_bind_sandboxed(&env.runtime.sock_dir) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return;
    }
    let launch = "rimz agents claude,codex --worktree=quality-pass";
    let mut topology = env.topology_cache(unix_now_ms());
    topology
        .panes
        .iter_mut()
        .find(|pane| pane.id == 7)
        .expect("fixture working pane exists")
        .pane_command = Some(launch.to_owned());
    let topology_json = serde_json::to_string(&topology).expect("serialize topology");
    let recv = env.bind_socket("sidebar.eldest.sock");
    env.plant_heartbeat("sidebar.eldest.json", ELDEST_ID, "sidebar.eldest.sock");

    let output = env.wake_with(
        "command-changed",
        true,
        &[
            "--session-name",
            SESSION_NAME,
            "--pane-id",
            "terminal_7",
            "--command-arg",
            launch,
            "--topology",
            topology_json.as_str(),
        ],
    );
    assert!(
        output.status.success(),
        "wake failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let event = recv_sidebar_event(&recv, "eldest");
    assert_sidebar_envelope(&event, &env.workspace_id, Some(SESSION_NAME));
    assert_eq!(event["event"]["kind"], "panes_changed");
    let cached =
        read_pane_topology_cache(&env.runtime, SESSION_NAME).expect("topology cache written");
    let working = cached
        .panes
        .iter()
        .find(|pane| pane.id == 7)
        .expect("working pane remains cached");
    assert_eq!(
        working.pane_command, None,
        "rimz agents launch chrome must not republish as foreground process truth"
    );
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
fn presence_stamp_extends_the_pane_cache_ttl() {
    let event_env = WakeEnv::new();
    let wake = event_env.wake("alive", false);
    assert!(
        wake.status.success(),
        "stamp write must succeed: status={:?}, stderr={}",
        wake.status,
        String::from_utf8_lossy(&wake.stderr),
    );
    event_env.seed_pane_cache(Duration::from_secs(5));
    let output = event_env.snapshot();
    assert!(
        output.status.success(),
        "snapshot failed: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        event_env.trace_lines(),
        Vec::<String>::new(),
        "event mode must serve the default-stale cache without a mux fork",
    );
}

fn trace_shim_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace"))
    })
    .clone()
}

fn rimz_cli_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz")))
        .clone()
}
