//! Integration coverage for the reference Python pane-send resolver under
//! `examples/resolvers/`. The full happy path requires a live multiplexer
//! pane to capture/send against; that is gated behind `tmux` availability
//! (see `tmux_backend.rs` for the self-skip idiom). The path that does not
//! require a multiplexer — abstain when the feed item carries no pane — is
//! exercised here as a fast end-to-end signal that the resolver loop, the
//! heartbeat protocol, and the `feed abstain` CLI integrate.

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
            .join("examples/resolvers/pane_send_resolver.py");
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

fn permission_payload() -> String {
    serde_json::to_string(&json!({
        "hook_event_name": "PermissionRequest",
        "tool_name": "Bash",
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

#[test]
fn pane_send_resolver_abstains_when_item_has_no_pane() {
    let env = Env::new();
    if skip_preconditions(&env) {
        return;
    }
    // Single-link chain with a short budget so the test ends quickly when
    // the resolver abstains: chain exhausts and the hook emits neutral.
    env.enrol("pane-demo", 10, "1s");

    let mut resolver = env.spawn_python_resolver("pane-demo", 8.0);
    wait_for_heartbeat(&env, "pane-demo", Instant::now() + Duration::from_secs(3));

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
        .write_all(permission_payload().as_bytes())
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
        "pane-send resolver should abstain on no_pane; chain exhaustion emits Claude neutral"
    );

    // The abstain reason should appear in the audit log so unattended runs
    // can tell pane misses apart from policy misses.
    let state =
        rimz::StatePaths::under(env.workspace_id.clone(), &env.state_root()).expect("state paths");
    let runtime =
        rimz::RuntimePaths::under(env.workspace_id.clone(), &env.runtime_root).expect("runtime");
    let ledger = rimz::Ledger::open(state, runtime).expect("ledger");
    let events = ledger.read_events().expect("events");
    let abstain_reasons: Vec<String> = events
        .into_iter()
        .filter(|e| e.method == "feed.abstain")
        .filter_map(|e| {
            e.params
                .get("reason")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
        })
        .collect();
    assert!(
        abstain_reasons.iter().any(|r| r == "no_pane"),
        "expected feed.abstain with reason=no_pane, got {abstain_reasons:?}"
    );
}

#[test]
fn pane_send_resolver_match_prompt_recognises_bounded_patterns() {
    if !python3_present() {
        tracing::warn!("skipping: python3 not on PATH");
        return;
    }
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root")
        .join("examples/resolvers/pane_send_resolver.py");
    let import_dir = script
        .parent()
        .expect("script parent")
        .display()
        .to_string();

    let probe = r#"
import sys
sys.path.insert(0, sys.argv[1])
import pane_send_resolver as r

ok_cases = [
    ["Are you sure? [y/N]"],
    ["junk above", "Do you want to continue? [y/N]"],
    ["Proceed? [Y/n]   "],
]
bad_cases = [
    ["unrelated prompt $"],
    ["Please type the secret"],
    [],
]

for lines in ok_cases:
    assert r.match_prompt(lines) == "y\n", lines
for lines in bad_cases:
    assert r.match_prompt(lines) is None, lines
print("ok")
"#;

    let out = StdCommand::new("python3")
        .args(["-c", probe, &import_dir])
        .output()
        .expect("spawn python probe");
    assert!(
        out.status.success(),
        "pattern probe failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "ok");
}

#[test]
fn pane_send_resolver_help_is_well_formed() {
    if !python3_present() {
        tracing::warn!("skipping: python3 not on PATH");
        return;
    }
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("workspace root")
        .join("examples/resolvers/pane_send_resolver.py");
    let out = StdCommand::new("python3")
        .arg(&script)
        .arg("--help")
        .output()
        .expect("spawn --help");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(
        stdout.contains("--workspace-id") && stdout.contains("--resolver-id"),
        "help missing required arg flags:\n{stdout}"
    );
    // The body is in a single docstring; the first line should mention the
    // resolver's role so the help text doubles as documentation.
    assert!(
        stdout.contains("pane-send resolver"),
        "help missing role line:\n{stdout}"
    );
    let _: Value = json!({}); // keep the serde_json import in use
}
