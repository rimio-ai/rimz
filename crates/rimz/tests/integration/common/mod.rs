//! Shared harness for integration tests. Real tempdir, real ledger files —
//! no in-memory stubs per `docs/contributing/testing.md`.
//!
//! Two entry points:
//! - [`Env`] drives the `rimz` binary out of process (the CLI tier): XDG roots
//!   scoped to a tempdir, the workspace resolved from the project root, and
//!   helpers for the hook/feed/resolver round trips every CLI test repeats.
//! - [`Harness`] opens a real [`Ledger`] in process (the library tier) for
//!   tests that drive ledger APIs directly without spawning a subprocess.

#![allow(dead_code)]

pub mod redact;

use std::io::{self, Write};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use jiff::Timestamp;
use rimz::schema::heartbeat::ResolverHeartbeat;
use rimz::{EventEnvelope, Ledger, RuntimePaths, StatePaths, WorkspaceId, WorkspaceResolver};
use serde_json::{Value, json};
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

    // --- feed commands ---

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

// --- agent payload fixtures ---

/// Claude-shaped `PermissionRequest` hook payload for `tool_name`.
pub fn permission_payload(tool_name: &str) -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "tool_name": tool_name,
        "tool_input": { "command": "echo hi" },
    }))
    .expect("payload")
}

/// Codex-shaped `PermissionRequest` payload (shell command vector, no
/// Claude-only fields).
pub fn codex_permission_payload() -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "tool_name": "shell",
        "command": ["echo", "hi"],
    }))
    .expect("payload")
}

/// Claude `PreToolUse` blocking-hook payload (`ExitPlanMode`,
/// `AskUserQuestion`).
pub fn claude_pre_tool_use_payload(tool_name: &str) -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PreToolUse",
        "tool_name": tool_name,
        "tool_input": { "plan": "ship it" },
        "session_id": "sess-claude-pretool",
    }))
    .expect("payload")
}

/// Whether `python3` is on PATH — example-resolver tests self-skip without it.
pub fn python3_present() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Block until `resolver_id` has written a fresh heartbeat, or panic at
/// `until`. Used by the example-resolver tests that wait for a spawned Python
/// resolver to come alive before firing a hook.
pub fn wait_for_heartbeat(env: &Env, resolver_id: &str, until: Instant) {
    let path = env
        .heartbeat_dir()
        .join(format!("resolver.{resolver_id}.json"));
    let ttl = Duration::from_secs(3);
    while Instant::now() < until {
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(parsed) = serde_json::from_slice::<ResolverHeartbeat>(&bytes)
        {
            let age = Timestamp::now().duration_since(parsed.last_seen);
            if !age.is_negative() && (age.as_secs() as u64) < ttl.as_secs() {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("python resolver never wrote a fresh heartbeat at {path:?}");
}

/// In-process ledger fixture for tests that drive `Ledger` APIs directly.
pub struct Harness {
    pub state_root: PathBuf,
    pub runtime_root: PathBuf,
    pub workspace_id: WorkspaceId,
    pub runtime_paths: RuntimePaths,
    pub ledger: Ledger,
    _tempdir: TempDir,
}

impl Harness {
    pub fn new() -> Self {
        let tempdir = TempDir::new().expect("tempdir");
        let state_root = tempdir.path().join("state");
        let runtime_root = tempdir.path().join("runtime");
        let workspace_id = WorkspaceId::from_project_root(tempdir.path());
        let paths = StatePaths::under(workspace_id.clone(), &state_root).expect("state paths");
        let runtime_paths =
            RuntimePaths::under(workspace_id.clone(), &runtime_root).expect("runtime paths");
        let ledger = Ledger::open(paths, runtime_paths.clone()).expect("open ledger");

        Self {
            state_root,
            runtime_root,
            workspace_id,
            runtime_paths,
            ledger,
            _tempdir: tempdir,
        }
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
