//! End-to-end integration tests for the reference Python hook-bridge
//! resolver under `examples/resolvers/`. The resolver speaks the public CLI
//! and the on-disk heartbeat protocol; these tests confirm the contract by
//! firing a real hook and asserting the agent-native decision JSON lands.
//!
//! Self-skips when `python3` is not on PATH or the sandbox forbids AF_UNIX.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as StdCommand, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use jiff::Timestamp;
use rimz::WorkspaceId;
use rimz::schema::heartbeat::ResolverHeartbeat;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Env {
    home: TempDir,
    workspace_id: WorkspaceId,
    runtime_root: PathBuf,
    rimz_path: PathBuf,
}

impl Env {
    fn new() -> Self {
        let home = TempDir::new().expect("tempdir");
        let project_root = canonical(home.path());
        let workspace_id = WorkspaceId::from_project_root(&project_root);
        let runtime_root = project_root.join("runtime");
        for d in ["state", "runtime", "config"] {
            std::fs::create_dir_all(project_root.join(d)).expect("mkdir env root");
        }
        let rimz_path = StdCommand::cargo_bin("rimz")
            .expect("cargo-bin")
            .get_program()
            .to_owned()
            .into();
        let env = Env {
            home,
            workspace_id,
            runtime_root,
            rimz_path,
        };
        std::fs::create_dir_all(env.heartbeat_dir()).expect("mkdir heartbeat");
        env
    }

    fn project_root(&self) -> PathBuf {
        canonical(self.home.path())
    }

    fn state_root(&self) -> PathBuf {
        self.project_root().join("state")
    }

    fn config_root(&self) -> PathBuf {
        self.project_root().join("config")
    }

    fn heartbeat_dir(&self) -> PathBuf {
        self.runtime_root
            .join("rimz")
            .join(self.workspace_id.as_str())
            .join("heartbeat")
    }

    fn sock_dir(&self) -> PathBuf {
        self.runtime_root
            .join("rimz")
            .join(self.workspace_id.as_str())
            .join("sock")
    }

    fn rimz(&self) -> StdCommand {
        let mut cmd = StdCommand::cargo_bin("rimz").expect("cargo-bin");
        cmd.env("XDG_STATE_HOME", self.state_root())
            .env("XDG_RUNTIME_DIR", &self.runtime_root)
            .env("XDG_CONFIG_HOME", self.config_root())
            .env("HOME", self.project_root())
            .env_remove("RUST_LOG")
            .current_dir(self.project_root())
            .args(["--root", &self.project_root().display().to_string()]);
        cmd
    }

    fn enrol(&self, id: &str, order: u32, budget: &str) {
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

    fn spawn_python_resolver(&self, resolver_id: &str, run_seconds: f32) -> Child {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .parent()
            .expect("workspace root")
            .join("examples/resolvers/hook_bridge_resolver.py");
        assert!(script.exists(), "resolver script missing: {script:?}");

        StdCommand::new("python3")
            .arg(&script)
            .args([
                "--workspace-id",
                self.workspace_id.as_str(),
                "--resolver-id",
                resolver_id,
                "--rimz-bin",
                &self.rimz_path.display().to_string(),
                "--tick-seconds",
                "0.1",
                "--run-seconds",
                &run_seconds.to_string(),
            ])
            .env("XDG_STATE_HOME", self.state_root())
            .env("XDG_RUNTIME_DIR", &self.runtime_root)
            .env("XDG_CONFIG_HOME", self.config_root())
            .env("HOME", self.project_root())
            .env_remove("RUST_LOG")
            .current_dir(self.project_root())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn python resolver")
    }
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

fn permission_payload(tool_name: &str) -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "tool_name": tool_name,
        "tool_input": { "command": "noop" }
    }))
    .expect("payload")
}

fn python3_present() -> bool {
    StdCommand::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn skip_preconditions(env: &Env) -> bool {
    if !python3_present() {
        tracing::warn!("skipping: python3 not on PATH");
        return true;
    }
    std::fs::create_dir_all(env.sock_dir()).unwrap();
    if common::af_unix_bind_sandboxed(&env.sock_dir()) {
        tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
        return true;
    }
    false
}

#[test]
fn python_resolver_allow_path_renders_claude_decision() {
    let env = Env::new();
    if skip_preconditions(&env) {
        return;
    }
    env.enrol("demo", 10, "30s");

    let mut resolver = env.spawn_python_resolver("demo", 8.0);

    // Give the resolver a beat to lay down its first heartbeat. The hook
    // bridge engages on the first fresh sample, so without this beat the
    // hook may take the no-resolver native_ui path.
    wait_for_heartbeat(&env, "demo", Instant::now() + Duration::from_secs(3));

    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(permission_payload("Read").as_bytes())
        .unwrap();

    let output = child.wait_with_output().expect("wait hook");
    let _ = resolver.kill();
    let _ = resolver.wait();
    assert!(
        output.status.success(),
        "hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf8");
    let decision: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|_| panic!("expected decision json, got: {stdout:?}"));
    assert_eq!(
        decision["hookSpecificOutput"]["decision"]["behavior"], "allow",
        "decision: {decision}"
    );
}

#[test]
fn python_resolver_abstain_path_exhausts_chain_to_neutral() {
    let env = Env::new();
    if skip_preconditions(&env) {
        return;
    }
    // Short budget so the chain-exhausted path fires before the test times
    // out (the resolver abstains on tool_name=Bash; chain has one link).
    env.enrol("demo", 10, "1s");

    let mut resolver = env.spawn_python_resolver("demo", 8.0);
    wait_for_heartbeat(&env, "demo", Instant::now() + Duration::from_secs(3));

    let mut child = env
        .rimz()
        .args(["hooks", "feed", "--source", "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hooks");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(permission_payload("Bash").as_bytes())
        .unwrap();

    let output = child.wait_with_output().expect("wait hook");
    let _ = resolver.kill();
    let _ = resolver.wait();
    assert!(
        output.status.success(),
        "hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "{}",
        "abstain on Bash should drain the chain and emit Claude's neutral payload"
    );
}

fn wait_for_heartbeat(env: &Env, resolver_id: &str, until: Instant) {
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
