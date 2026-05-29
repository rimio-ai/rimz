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

/// Out-of-process CLI harness: a tempdir holding `state/runtime/config`, the
/// workspace rooted at the tempdir, and a configured `rimz` command builder.
pub struct Env {
    _home: TempDir,
    pub project_root: PathBuf,
    pub workspace_id: WorkspaceId,
    pub runtime_root: PathBuf,
}

impl Env {
    pub fn new() -> Self {
        let home = TempDir::new().expect("tempdir");
        let project_root = canonical(home.path());
        let workspace_id = WorkspaceId::from_project_root(&project_root);
        let runtime_root = project_root.join("runtime");
        for dir in ["state", "runtime", "config"] {
            std::fs::create_dir_all(project_root.join(dir)).expect("mkdir env root");
        }
        let env = Env {
            _home: home,
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
        self.project_root.join("state")
    }

    pub fn config_root(&self) -> PathBuf {
        self.project_root.join("config")
    }

    pub fn heartbeat_dir(&self) -> PathBuf {
        self.workspace_runtime().join("heartbeat")
    }

    pub fn sock_dir(&self) -> PathBuf {
        self.workspace_runtime().join("sock")
    }

    pub fn events_log_path(&self) -> PathBuf {
        self.state_root()
            .join("rimz")
            .join(self.workspace_id.as_str())
            .join("events.log.jsonl")
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
        cmd.env("XDG_STATE_HOME", self.state_root())
            .env("XDG_RUNTIME_DIR", &self.runtime_root)
            .env("XDG_CONFIG_HOME", self.config_root())
            .env("HOME", &self.project_root)
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
            "codex" => self.project_root.join(".codex").join("config.toml"),
            "claude" => self.project_root.join(".claude").join("settings.json"),
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
            "claude" => text.contains("_rimz_managed"),
            _ => false,
        }
    }

    /// Fire the exact command an installed hook runs: `rimz hooks feed
    /// --source <source> --event <event>` with `payload` on stdin. This is
    /// what a supported agent invokes when the event fires.
    pub fn run_installed_hook(&self, source: &str, event: &str, payload: &str) -> Output {
        self.run_installed_hook_in_pane(source, event, payload, &[])
    }

    /// Spawn the installed-hook command, returning the live child so a test
    /// can drive the ledger while a blocking hook holds open on the bridge.
    pub fn spawn_installed_hook(&self, source: &str, event: &str, payload: &str) -> Child {
        self.spawn_installed_hook_in_pane(source, event, payload, &[])
    }

    /// Fire an installed hook with the per-pane env the mux exports
    /// (`TMUX_PANE` / `ZELLIJ_PANE_ID`), so the hook stamps the pane it ran
    /// inside exactly as it does under a real multiplexer. Any mux pane var
    /// leaking from the test runner is cleared first, so the stamp is
    /// deterministic.
    pub fn run_installed_hook_in_pane(
        &self,
        source: &str,
        event: &str,
        payload: &str,
        pane_env: &[(&str, &str)],
    ) -> Output {
        self.spawn_installed_hook_in_pane(source, event, payload, pane_env)
            .wait_with_output()
            .expect("wait installed hook")
    }

    pub fn spawn_installed_hook_in_pane(
        &self,
        source: &str,
        event: &str,
        payload: &str,
        pane_env: &[(&str, &str)],
    ) -> Child {
        let mut cmd = self.rimz();
        cmd.args(["hooks", "feed", "--source", source, "--event", event])
            .env("RIMZ_AGENT_PID", std::process::id().to_string())
            .env_remove("TMUX_PANE")
            .env_remove("ZELLIJ_PANE_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in pane_env {
            cmd.env(key, value);
        }
        self.spawn_payload(cmd, payload)
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

    pub fn resolve_from_sidebar(&self, request_id: &str, decision: &str) -> Output {
        self.rimz()
            .args([
                "feed",
                "resolve",
                request_id,
                "--decision",
                decision,
                "--method",
                "sidebar",
            ])
            .output()
            .expect("spawn feed resolve --method sidebar")
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
            std::thread::sleep(Duration::from_millis(20));
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
            std::thread::sleep(Duration::from_millis(50));
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
