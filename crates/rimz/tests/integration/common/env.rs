//! Out-of-process CLI harness: drives the `rimz` binary with XDG roots scoped
//! to a tempdir, the workspace resolved from the project root, and the
//! hook/feed/resolver round-trip helpers every CLI test repeats.

use std::io::{self, Write};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use jiff::Timestamp;

use super::command::ScrubSessionEnvExt;
use rimz::feed::PaneRef;
use rimz::schema::heartbeat::ResolverHeartbeat;
use rimz::{EventEnvelope, Ledger, RuntimePaths, StatePaths, WorkspaceId, WorkspaceResolver};
use serde_json::Value;
use tempfile::TempDir;

/// Canonicalize, falling back to the original path when it does not yet exist
/// (a project root the test is about to create). Workspace IDs hash the
/// canonical root, so harness and binary must agree on the same form.
pub fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// A live tmux pane fixture for `Env::snapshot_json_with_panes`: the given raw
/// id (`%5`), foreground command, and cwd, in the harness session's one window.
pub fn tmux_pane(raw: &str, command: &str, cwd: &Path) -> PaneRef {
    PaneRef {
        pane_id: rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Tmux, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some("@0".to_owned()),
        view_kind: Some(rimz::ids::ViewKind::Window),
        view_name: None,
        is_focused: false,
        command: Some(command.to_owned()),
        spawn_command: None,
        cwd: Some(cwd.display().to_string()),
        pane_pid: None,
        pane_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

/// Out-of-process CLI harness: a tempdir `HOME` holding `state/runtime/config`
/// plus a `project/` workspace root under it — the shape a real machine has,
/// so per-user agent config and the workspace never share a directory — and a
/// configured `rimz` command builder.
pub struct Env {
    _home: TempDir,
    /// The harness `$HOME`: per-user agent config (`.claude/`, `.codex/`) and
    /// the XDG roots live here.
    pub home_root: PathBuf,
    /// The workspace root — a bare `project/` directory under `home_root`, so
    /// the harness exercises the directory-workspace class by default.
    pub project_root: PathBuf,
    pub workspace_id: WorkspaceId,
    pub runtime_root: PathBuf,
}

impl Env {
    pub fn new() -> Self {
        let home = TempDir::new().expect("tempdir");
        let home_root = canonical(home.path());
        let project_root = home_root.join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project root");
        let workspace_id = WorkspaceId::from_project_root(&project_root);
        let runtime_root = home_root.join("runtime");
        for dir in ["state", "runtime", "config"] {
            std::fs::create_dir_all(home_root.join(dir)).expect("mkdir env root");
        }
        let env = Env {
            _home: home,
            home_root,
            project_root,
            workspace_id,
            runtime_root,
        };
        // Pre-create the heartbeat dir so resolver-heartbeat writes never race
        // the ledger creating it.
        std::fs::create_dir_all(env.heartbeat_dir()).expect("mkdir heartbeat");
        env
    }

    // --- paths ---

    pub fn state_root(&self) -> PathBuf {
        self.home_root.join("state")
    }

    pub fn config_root(&self) -> PathBuf {
        self.home_root.join("config")
    }

    pub fn heartbeat_dir(&self) -> PathBuf {
        self.workspace_runtime().join("heartbeat")
    }

    pub fn sock_dir(&self) -> PathBuf {
        self.workspace_runtime().join("sock")
    }

    fn workspace_runtime(&self) -> PathBuf {
        self.runtime_root
            .join("rimz")
            .join(self.workspace_id.as_str())
    }

    /// Absolute path to the built `rimz` binary, for resolvers that shell out.
    pub fn rimz_bin(&self) -> PathBuf {
        Command::cargo_bin("rimz")
            .expect("cargo-bin")
            .get_program()
            .to_owned()
            .into()
    }

    /// Base command: XDG roots scoped to the tempdir, HOME pinned, `RUST_LOG`
    /// cleared, cwd at the project root. The workspace resolves from cwd — no
    /// `--root`; tests targeting another project override `current_dir`.
    pub fn rimz(&self) -> Command {
        let mut cmd = Command::cargo_bin("rimz").expect("cargo-bin");
        cmd.scrub_session_env()
            .env("XDG_STATE_HOME", self.state_root())
            .env("XDG_RUNTIME_DIR", &self.runtime_root)
            .env("XDG_CONFIG_HOME", self.config_root())
            .env("HOME", &self.home_root)
            .env_remove("RUST_LOG")
            .current_dir(&self.project_root);
        cmd
    }

    // --- resolver enrolment & heartbeats ---

    pub fn enrol(&self, id: &str, order: u32, budget: &str) {
        let status = self
            .rimz()
            .args([
                "resolver",
                "add",
                id,
                "--order",
                &order.to_string(),
                "--budget",
                budget,
            ])
            .status()
            .expect("spawn resolver add");
        assert!(status.success(), "resolver add `{id}` failed");
    }

    pub fn write_heartbeat(&self, id: &str, last_seen: Timestamp) {
        let resolver_id = id.parse().expect("resolver id parse");
        let mut hb = ResolverHeartbeat::new(self.workspace_id.clone(), resolver_id);
        hb.last_seen = last_seen;
        let path = self.heartbeat_dir().join(format!("resolver.{id}.json"));
        std::fs::write(&path, serde_json::to_vec(&hb).expect("serialize hb"))
            .expect("write heartbeat");
    }

    // --- hooks ---

    /// `rimz hooks feed --source <source>` with all three stdio piped, ready
    /// for extra `.env(...)` before spawning.
    pub fn hook_command(&self, source: &str) -> Command {
        let mut cmd = self.rimz();
        cmd.args(["hooks", "feed", "--source", source])
            .env("RIMZ_AGENT_PID", std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Spawn a prepared command and write `payload` to its stdin.
    pub fn spawn_payload(&self, mut cmd: Command, payload: &str) -> Child {
        let mut child = cmd.spawn().expect("spawn rimz");
        child
            .stdin
            .take()
            .expect("child stdin")
            .write_all(payload.as_bytes())
            .expect("write stdin");
        child
    }

    /// Spawn the hook and feed it `payload`, returning the live child so the
    /// test can drive the ledger while the hook blocks on the bridge.
    pub fn spawn_hook(&self, source: &str, payload: &str) -> Child {
        self.spawn_payload(self.hook_command(source), payload)
    }

    /// Run the hook to completion — the one-shot native_ui / neutral path.
    pub fn run_hook(&self, source: &str, payload: &str) -> Output {
        self.spawn_hook(source, payload)
            .wait_with_output()
            .expect("wait hook")
    }

    // --- agent onboarding (the user's `rimz hooks install` setup step) ---
    //
    // A real agent only ever fires `rimz hooks feed` when its per-user config
    // carries a Rimz-managed hook block — i.e. the user ran `rimz hooks
    // install`. Until then the agent runs but never calls Rimz, so the sidebar
    // stays empty. These helpers let the journey reproduce that wiring instead
    // of hand-firing the hook unconditionally.

    /// Per-user agent config path under the harness `HOME` (`project_root`).
    /// The installer writes here because [`Env::rimz`] pins `HOME` to the
    /// project root, and the sidebar's snapshot subprocess reads the same path.
    pub fn agent_config_path(&self, source: &str) -> PathBuf {
        match source {
            "codex" => self.home_root.join(".codex").join("config.toml"),
            "claude" => self.home_root.join(".claude").join("settings.json"),
            "pi" => self
                .home_root
                .join(".pi")
                .join("agent")
                .join("extensions")
                .join("rimz.ts"),
            other => panic!("unknown agent `{other}`"),
        }
    }

    /// Wire an agent the way the user does: `rimz hooks install <source>`. The
    /// install wires the full lifecycle (prompt + tool events) so the journey
    /// can exercise every phase.
    pub fn install_agent_hooks(&self, source: &str) {
        let out = self
            .rimz()
            .args(["hooks", "install", source])
            .output()
            .expect("spawn hooks install");
        assert!(
            out.status.success(),
            "hooks install `{source}` failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    /// Whether `source`'s per-user config carries Rimz-managed hooks. A real
    /// agent fires `rimz hooks feed` only when this holds; otherwise it never
    /// calls Rimz at all.
    pub fn agent_hooks_installed(&self, source: &str) -> bool {
        let Ok(text) = std::fs::read_to_string(self.agent_config_path(source)) else {
            return false;
        };
        match source {
            "codex" => text.contains("rimz hooks feed --source codex"),
            "claude" | "pi" => text.contains("_rimz_managed"),
            _ => false,
        }
    }

    /// Fire the exact command an installed hook runs: `rimz hooks feed
    /// --source <source>` with `payload` on stdin. The event is read from the
    /// payload's `hook_event_name` — no `--event` flag, mirroring the installed
    /// command. This is what a supported agent invokes when the event fires.
    pub fn run_installed_hook(&self, source: &str, payload: &str) -> Output {
        self.run_installed_hook_in_pane(source, payload, &[])
    }

    /// Fire an installed hook with the per-pane env the mux exports
    /// (`TMUX_PANE` / `ZELLIJ_PANE_ID`), so the hook stamps the pane it ran
    /// inside exactly as it does under a real multiplexer. Any mux pane var
    /// leaking from the test runner is cleared first, so the stamp is
    /// deterministic.
    pub fn run_installed_hook_in_pane(
        &self,
        source: &str,
        payload: &str,
        pane_env: &[(&str, &str)],
    ) -> Output {
        self.spawn_installed_hook_in_pane(source, payload, pane_env)
            .wait_with_output()
            .expect("wait installed hook")
    }

    pub fn spawn_installed_hook_in_pane(
        &self,
        source: &str,
        payload: &str,
        pane_env: &[(&str, &str)],
    ) -> Child {
        let mut cmd = self.rimz();
        cmd.args(["hooks", "feed", "--source", source])
            .env("RIMZ_AGENT_PID", std::process::id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in pane_env {
            cmd.env(key, value);
        }
        self.spawn_payload(cmd, payload)
    }

    // --- statusline datasource ---

    /// `rimz statusline feed --source <source>` with all three stdio piped,
    /// mirroring how Claude's installed `statusLine` command invokes it.
    pub fn statusline_feed_command(&self, source: &str) -> Command {
        let mut cmd = self.rimz();
        cmd.args(["statusline", "feed", "--source", source])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Run the statusline feed to completion, writing `payload` to its stdin.
    pub fn run_statusline_feed(&self, source: &str, payload: &str) -> Output {
        self.spawn_payload(self.statusline_feed_command(source), payload)
            .wait_with_output()
            .expect("wait statusline feed")
    }

    /// `rimz statusline feed --source <source> --subagent`, mirroring how
    /// Claude's installed `subagentStatusLine` command invokes it.
    pub fn run_subagent_statusline_feed(&self, source: &str, payload: &str) -> Output {
        let mut cmd = self.statusline_feed_command(source);
        cmd.arg("--subagent");
        self.spawn_payload(cmd, payload)
            .wait_with_output()
            .expect("wait subagent statusline feed")
    }

    /// Read every persisted agent-context sidecar for the harness workspace.
    pub fn agent_contexts(&self) -> Vec<rimz::ledger::agent_context::AgentContextRecord> {
        rimz::ledger::agent_context::read_all(&self.runtime_paths())
    }

    /// Read every persisted subagent-context sidecar for the harness workspace.
    pub fn subagent_contexts(&self) -> Vec<rimz::ledger::subagent_context::SubagentContextRecord> {
        rimz::ledger::subagent_context::read_all(&self.runtime_paths())
    }

    // --- feed commands ---

    pub fn feed_ask_no_block(&self, title: &str, options: &[&str]) -> String {
        self.feed_ask_no_block_in(&self.project_root, title, options)
    }

    pub fn feed_ask_no_block_in(&self, cwd: &Path, title: &str, options: &[&str]) -> String {
        let mut cmd = self.rimz();
        cmd.current_dir(cwd)
            .args(["feed", "ask", "--title", title, "--no-block"]);
        if !options.is_empty() {
            cmd.arg("--options").arg(options.join(","));
        }
        let out = cmd.output().expect("spawn feed ask --no-block");
        assert!(
            out.status.success(),
            "feed ask --no-block failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        let request_id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        assert!(
            !request_id.is_empty(),
            "feed ask --no-block printed no request id"
        );
        request_id
    }

    pub fn resolve(
        &self,
        request_id: &str,
        decision: &str,
        resolver_id: &str,
        method: &str,
    ) -> Output {
        self.rimz()
            .args([
                "feed",
                "resolve",
                request_id,
                "--decision",
                decision,
                "--resolver-id",
                resolver_id,
                "--method",
                method,
            ])
            .output()
            .expect("spawn feed resolve")
    }

    pub fn abstain(&self, request_id: &str, resolver_id: &str, reason: &str) -> Output {
        self.rimz()
            .args([
                "feed",
                "abstain",
                request_id,
                "--resolver-id",
                resolver_id,
                "--reason",
                reason,
            ])
            .output()
            .expect("spawn feed abstain")
    }

    pub fn feed_list_json(&self) -> Value {
        let out = self
            .rimz()
            .args(["feed", "list", "--json"])
            .output()
            .expect("spawn feed list");
        assert!(
            out.status.success(),
            "feed list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("feed list json")
    }

    pub fn feed_list_audit_json(&self) -> Value {
        let out = self
            .rimz()
            .args(["feed", "list", "--json", "--audit"])
            .output()
            .expect("spawn feed list --audit");
        assert!(
            out.status.success(),
            "feed list --audit failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("feed list audit json")
    }

    pub fn feed_show_json(&self, request_id: &str) -> Value {
        let out = self
            .rimz()
            .args(["feed", "show", request_id, "--json"])
            .output()
            .expect("spawn feed show");
        assert!(
            out.status.success(),
            "feed show failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("feed show json")
    }

    pub fn snapshot_json(&self) -> Value {
        let out = self
            .rimz()
            .args([
                "sidebar",
                "snapshot",
                "--workspace-id",
                self.workspace_id.as_str(),
                "--json",
            ])
            .output()
            .expect("spawn sidebar snapshot");
        assert!(
            out.status.success(),
            "sidebar snapshot failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("snapshot json")
    }

    pub fn snapshot_json_with_panes(&self, panes: &[PaneRef]) -> Value {
        let path = self.write_pane_fixture(panes);
        let out = self
            .rimz()
            .args([
                "sidebar",
                "snapshot",
                "--workspace-id",
                self.workspace_id.as_str(),
                "--mux",
                "tmux",
                "--session-name",
                "rimz-test",
                "--json",
            ])
            .env("RIMZ_TEST_PANE_LIST", &path)
            .output()
            .expect("spawn sidebar snapshot");
        assert!(
            out.status.success(),
            "sidebar snapshot failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("snapshot json")
    }

    pub fn write_pane_fixture(&self, panes: &[PaneRef]) -> PathBuf {
        std::fs::create_dir_all(&self.runtime_root).expect("mkdir runtime root");
        let path = self.runtime_root.join("snapshot-panes.json");
        let tmp = self.runtime_root.join("snapshot-panes.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(panes).expect("pane json"))
            .expect("write pane fixture temp");
        std::fs::rename(&tmp, &path).expect("publish pane fixture");
        path
    }

    // --- polling ---

    pub fn poll_pending_request_id(&self, until: Instant) -> Option<String> {
        while Instant::now() < until {
            let out = self
                .rimz()
                .args(["feed", "list", "--json"])
                .output()
                .expect("feed list");
            if out.status.success() {
                let parsed: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
                if let Some(arr) = parsed.as_array() {
                    for item in arr {
                        if item["status"] == "pending" {
                            return item["request_id"].as_str().map(ToOwned::to_owned);
                        }
                    }
                }
            }
            // Each iteration spawns a fresh `rimz`, so back off well above the
            // spawn cost rather than hammering process creation every 20 ms.
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    pub fn poll_active_resolver(&self, request_id: &str, expected: &str, until: Instant) -> bool {
        while Instant::now() < until {
            let out = self
                .rimz()
                .args(["feed", "show", request_id, "--json"])
                .output()
                .expect("feed show");
            if out.status.success() {
                let parsed: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
                if parsed["chain_active_resolver"] == expected {
                    return true;
                }
            }
            // Spawns `rimz feed show` per iteration; the chain only re-evaluates
            // on the producer's ~1 s tick, so 100 ms loses no real responsiveness.
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    // --- ledger access (per project root) ---

    pub fn state_path_for(&self, project_root: &Path) -> StatePaths {
        let workspace_id = WorkspaceId::from_project_root(&canonical(project_root));
        StatePaths::under(workspace_id, &self.state_root()).expect("state paths")
    }

    pub fn ledger_for(&self, project_root: &Path) -> Ledger {
        let workspace_id = WorkspaceId::from_project_root(&canonical(project_root));
        let state =
            StatePaths::under(workspace_id.clone(), &self.state_root()).expect("state paths");
        let runtime = RuntimePaths::under(workspace_id, &self.runtime_root).expect("runtime paths");
        Ledger::open(state, runtime).expect("open ledger")
    }

    /// Open the ledger for the harness's own project root.
    pub fn ledger(&self) -> Ledger {
        self.ledger_for(&self.project_root)
    }

    /// Runtime paths (heartbeat/sock dirs) for the harness workspace.
    pub fn runtime_paths(&self) -> RuntimePaths {
        RuntimePaths::under(self.workspace_id.clone(), &self.runtime_root).expect("runtime paths")
    }

    /// Resolve and record a workspace so `rimz list`/`workspace` see it.
    pub fn record(&self, project_root: &Path) {
        std::fs::create_dir_all(project_root).expect("mkdir project");
        let workspace = WorkspaceResolver::resolve(project_root, None).expect("resolve");
        self.ledger_for(project_root)
            .record_workspace(&workspace)
            .expect("record workspace");
    }

    /// Write `<project_root>/.rimz/config.toml`.
    pub fn write_config(&self, project_root: &Path, body: &str) {
        let dir = project_root.join(".rimz");
        std::fs::create_dir_all(&dir).expect("mkdir .rimz");
        std::fs::write(dir.join("config.toml"), body).expect("write config");
    }

    /// Read the harness project's event log through the public `Ledger` API,
    /// so test code never hand-rolls the length framing.
    pub fn read_events(&self) -> Vec<EventEnvelope> {
        self.ledger().read_events().expect("read events")
    }

    /// `true` when the sandbox forbids binding AF_UNIX datagram sockets; tests
    /// emit a warning and return early. Ensures `sock_dir` exists first.
    pub fn skip_if_sandboxed(&self) -> bool {
        std::fs::create_dir_all(self.sock_dir()).expect("mkdir sock");
        if af_unix_bind_sandboxed(&self.sock_dir()) {
            tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
            return true;
        }
        false
    }
}

/// Probe whether the current sandbox forbids binding AF_UNIX datagram
/// sockets. Returns `true` when a bind under `dir` fails with `EPERM` /
/// `EACCES` (`io::ErrorKind::PermissionDenied`) — the shape we see in
/// hermetic CI sandboxes that block `bind(2)` on Unix sockets. Tests that
/// would otherwise hard-fail should call this at the top, emit a
/// `tracing::warn!`, and return early — mirroring the "skip if mux binary
/// missing" idiom used by the zellij/tmux backend tests.
pub fn af_unix_bind_sandboxed(dir: &std::path::Path) -> bool {
    let probe = dir.join("rimz-af-unix-probe.sock");
    let _ = std::fs::remove_file(&probe);
    match UnixDatagram::bind(&probe) {
        Ok(sock) => {
            drop(sock);
            let _ = std::fs::remove_file(&probe);
            false
        }
        Err(e) if matches!(e.kind(), io::ErrorKind::PermissionDenied) => true,
        Err(_) => false,
    }
}
