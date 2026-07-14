//! Out-of-process CLI harness: drives the `rimz` binary with XDG roots scoped
//! to a tempdir, the workspace resolved from the project root, and the hook
//! helpers every CLI test repeats.

use std::io::{self, Write};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use super::command::ScrubSessionEnvExt;
use rimz::pane::PaneRef;
use rimz::{EventEnvelope, RuntimePaths, StatePaths, Store, WorkspaceId, WorkspaceResolver};
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
        title: None,
        is_focused: false,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(cwd.display().to_string()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
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
    _runtime: TempDir,
    /// The harness `$HOME`: per-user agent config (`.claude/`, `.codex/`) and
    /// the XDG roots live here.
    pub home_root: PathBuf,
    /// The workspace root — a bare `project/` directory under `home_root`, so
    /// the harness exercises the directory-workspace class by default.
    pub project_root: PathBuf,
    pub workspace_id: WorkspaceId,
    pub runtime_root: PathBuf,
    /// A session-scrubbed process whose `/proc` environment the journey hook
    /// fallback reads as launch identity (see [`Env::agent_owner_pid`]). Spawned
    /// on first use, shared across every harness of one test, and kept alive for
    /// the test's whole span.
    agent_owner: std::sync::OnceLock<Child>,
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(owner) = self.agent_owner.get_mut() {
            let _ = owner.kill();
            let _ = owner.wait();
        }
    }
}

impl Env {
    pub fn new() -> Self {
        let home = TempDir::new().expect("tempdir");
        let runtime = tempfile::Builder::new()
            .prefix("rr")
            .tempdir_in("/tmp")
            .expect("short runtime tempdir");
        let home_root = canonical(home.path());
        let project_root = home_root.join("project");
        std::fs::create_dir_all(&project_root).expect("mkdir project root");
        let workspace_id = WorkspaceId::from_project_root(&project_root);
        let runtime_root = runtime.path().to_path_buf();
        for dir in ["state", "config"] {
            std::fs::create_dir_all(home_root.join(dir)).expect("mkdir env root");
        }
        let env = Env {
            _home: home,
            _runtime: runtime,
            home_root,
            project_root,
            workspace_id,
            runtime_root,
            agent_owner: std::sync::OnceLock::new(),
        };
        // Pre-create the heartbeat dir so sidebar writes never race the store creating it.
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

    /// Absolute path to the built `rimz` binary, for helper scripts that shell out.
    pub fn rimz_bin(&self) -> PathBuf {
        super::shim::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"))
    }

    /// Base command: XDG roots scoped to the tempdir, HOME pinned, `RUST_LOG`
    /// cleared, cwd at the project root. The workspace resolves from cwd — no
    /// `--root`; tests targeting another project override `current_dir`.
    pub fn rimz(&self) -> Command {
        self.rimz_at(&self.rimz_bin())
    }

    /// Base command using a caller-supplied rimz executable path.
    pub fn rimz_at(&self, rimz_bin: &Path) -> Command {
        let mut cmd = Command::new(rimz_bin);
        cmd.scrub_session_env()
            .env("XDG_STATE_HOME", self.state_root())
            .env("XDG_RUNTIME_DIR", &self.runtime_root)
            .env("XDG_CONFIG_HOME", self.config_root())
            .env("HOME", &self.home_root)
            .env("SHELL", "/bin/sh")
            .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
            .env_remove("ENV")
            .env_remove("BASH_ENV")
            .env_remove("ZDOTDIR")
            .env_remove("RUST_LOG")
            .env_remove("COPILOT_HOME")
            .env_remove("COPILOT_OTEL_FILE_EXPORTER_PATH")
            .current_dir(&self.project_root);
        cmd
    }

    // --- hooks ---

    /// The pid of this test's long-lived, session-scrubbed owner process. A
    /// journey hook stamps it as `RIMZ_AGENT_PID`, so the lifecycle fallback
    /// reads a clean `/proc` environment — a developer's ambient room channel
    /// never leaks into a fixture. The process is spawned once and outlives every
    /// [`RoomHarness`], so a detach/reattach never kills the simulated agent it
    /// owns and the reattach reads a still-live owner.
    pub fn agent_owner_pid(&self) -> u32 {
        self.agent_owner
            .get_or_init(|| {
                Command::new("sleep")
                    .arg("600")
                    .scrub_session_env()
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("spawn journey agent owner")
            })
            .id()
    }

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

    /// Copilot's installed hook commands carry the native event name as an
    /// explicit flag because its payload has no event-name field.
    pub fn copilot_hook_command(&self, event: &str) -> Command {
        let mut cmd = self.hook_command("copilot");
        cmd.args(["--event", event]);
        cmd
    }

    pub fn run_copilot_hook(&self, event: &str, payload: &str) -> Output {
        self.spawn_payload(self.copilot_hook_command(event), payload)
            .wait_with_output()
            .expect("wait Copilot hook")
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

    /// Spawn the hook and feed it `payload`, returning the live child.
    pub fn spawn_hook(&self, source: &str, payload: &str) -> Child {
        self.spawn_payload(self.hook_command(source), payload)
    }

    /// Run the hook to completion — the one-shot waiting / neutral path.
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
            "cursor" => self.home_root.join(".cursor").join("hooks.json"),
            "copilot" => self.home_root.join(".copilot/hooks/rimz.json"),
            "pi" => self
                .home_root
                .join(".pi")
                .join("agent")
                .join("extensions")
                .join("rimz.ts"),
            "qwen" => self.home_root.join(".qwen").join("settings.json"),
            other => panic!("unknown agent `{other}`"),
        }
    }

    pub fn cursor_cli_config_path(&self) -> PathBuf {
        self.home_root.join(".cursor").join("cli-config.json")
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
            "claude" | "copilot" | "pi" | "qwen" => text.contains("_rimz_managed"),
            "cursor" => {
                text.contains("rimz hooks feed --source cursor")
                    && std::fs::read_to_string(self.cursor_cli_config_path())
                        .is_ok_and(|config| config.contains("rimz statusline feed --source cursor"))
            }
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
    pub fn agent_contexts(&self) -> Vec<rimz::store::agent_context::AgentContextRecord> {
        rimz::store::agent_context::read_all(&self.runtime_paths())
    }

    /// Read every persisted subagent-context sidecar for the harness workspace.
    pub fn subagent_contexts(&self) -> Vec<rimz::store::subagent_context::SubagentContextRecord> {
        rimz::store::subagent_context::read_all(&self.runtime_paths())
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

    // --- store access (per project root) ---

    pub fn state_path_for(&self, project_root: &Path) -> StatePaths {
        let workspace_id = WorkspaceId::from_project_root(&canonical(project_root));
        StatePaths::under(workspace_id, &self.state_root()).expect("state paths")
    }

    pub fn store_for(&self, project_root: &Path) -> Store {
        let workspace_id = WorkspaceId::from_project_root(&canonical(project_root));
        let state =
            StatePaths::under(workspace_id.clone(), &self.state_root()).expect("state paths");
        let runtime = self.runtime_paths_for(workspace_id);
        Store::open(state, runtime).expect("open store")
    }

    /// Open the store for the harness's own project root.
    pub fn store(&self) -> Store {
        self.store_for(&self.project_root)
    }

    /// Runtime paths (heartbeat/sock dirs) for the harness workspace.
    pub fn runtime_paths(&self) -> RuntimePaths {
        self.runtime_paths_for(self.workspace_id.clone())
    }

    pub fn publish_provider_spending(&self, spending: &rimz::agents::spending::Spending) {
        rimz::agents::spending::write_provider_spending_cache(
            &self.runtime_paths().shared_provider_spending_path(),
            rimz::sidebar::timing::unix_now_ms(),
            spending,
        );
    }

    pub fn publish_accounts(&self, accounts: &rimz::sidebar::refresh::AccountsCache) {
        rimz::store::atomic::write_temp_then_rename_cache(
            &self.runtime_paths().shared_accounts_path(),
            accounts,
        )
        .expect("publish accounts cache");
    }

    pub fn publish_rate_limits(&self, cache: &rimz::agents::RateLimitsCache) {
        rimz::store::atomic::write_temp_then_rename_cache(
            &self.runtime_paths().shared_rate_limits_path(),
            cache,
        )
        .expect("publish rate-limit cache");
    }

    fn runtime_paths_for(&self, workspace_id: WorkspaceId) -> RuntimePaths {
        let mut paths =
            RuntimePaths::under(workspace_id, &self.runtime_root).expect("runtime paths");
        paths.persistent_shared_root = self.state_root().join("rimz").join("shared");
        paths
    }

    /// Resolve and record a workspace so `rimz list`/`workspace` see it.
    pub fn record(&self, project_root: &Path) {
        std::fs::create_dir_all(project_root).expect("mkdir project");
        let workspace = WorkspaceResolver::resolve(project_root, None).expect("resolve");
        self.store_for(project_root)
            .record_workspace(&workspace)
            .expect("record workspace");
    }

    /// Write `<project_root>/.rimz/config.toml`.
    pub fn write_config(&self, project_root: &Path, body: &str) {
        let dir = project_root.join(".rimz");
        std::fs::create_dir_all(&dir).expect("mkdir .rimz");
        std::fs::write(dir.join("config.toml"), body).expect("write config");
    }

    /// Read the harness project's event log through the public `Store` API,
    /// so test code never hand-rolls the length framing.
    pub fn read_events(&self) -> Vec<EventEnvelope> {
        self.store().read_events().expect("read events")
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
