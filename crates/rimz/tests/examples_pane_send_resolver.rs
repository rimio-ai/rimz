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
        for d in ["state", "runtime", "config", "tmux"] {
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

    /// Isolated `TMUX_TMPDIR` so the live-pane happy path never collides with
    /// the user's tmux server. The production `rimz` binary the resolver
    /// shells out to uses the default tmux socket, so isolation has to flow
    /// through the environment rather than a `-S` flag.
    fn tmux_tmpdir(&self) -> PathBuf {
        self.project_root().join("tmux")
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

    /// Spawn the reference resolver. When `tmux_pane` is `Some`, point the
    /// resolver's `rimz` invocations at the isolated tmux server: `TMUX_PANE`
    /// nudges backend auto-detection to tmux (zellij is tried first by
    /// `auto_detect_backend`), `TMUX_TMPDIR` pins the socket, and any ambient
    /// `TMUX` is dropped so it cannot hijack the target server.
    fn spawn_python_resolver(
        &self,
        resolver_id: &str,
        run_seconds: f32,
        tmux_pane: Option<&str>,
    ) -> Child {
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .parent()
            .expect("workspace root")
            .join("examples/resolvers/pane_send_resolver.py");
        assert!(script.exists(), "resolver script missing: {script:?}");

        let mut cmd = StdCommand::new("python3");
        cmd.arg(&script)
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
            .current_dir(self.project_root());
        if let Some(pane) = tmux_pane {
            cmd.env("TMUX_TMPDIR", self.tmux_tmpdir())
                .env("TMUX_PANE", pane)
                .env_remove("TMUX");
        }
        cmd.stdout(Stdio::piped())
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

    let mut resolver = env.spawn_python_resolver("pane-demo", 8.0, None);
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

fn tmux_present() -> bool {
    StdCommand::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run a `tmux` command against the isolated server. `TMUX` is removed so an
/// ambient session (a developer running the suite inside tmux) can't redirect
/// us onto their server; `TMUX_TMPDIR` selects our private socket.
fn run_tmux(tmpdir: &Path, args: &[&str]) -> std::process::Output {
    StdCommand::new("tmux")
        .args(args)
        .env("TMUX_TMPDIR", tmpdir)
        .env_remove("TMUX")
        .output()
        .expect("spawn tmux")
}

fn wait_for_prompt(tmpdir: &Path, pane_raw: &str, until: Instant) {
    while Instant::now() < until {
        let out = run_tmux(tmpdir, &["capture-pane", "-p", "-t", pane_raw]);
        if out.status.success()
            && String::from_utf8_lossy(&out.stdout).contains("Are you sure? [y/N]")
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("pane never displayed the bounded prompt");
}

fn poll_until_resolved(env: &Env, request_id: &str, until: Instant) -> Option<Value> {
    while Instant::now() < until {
        let out = env
            .rimz()
            .args(["feed", "show", request_id, "--json"])
            .output()
            .expect("feed show");
        if out.status.success() {
            let parsed: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
            if parsed["status"] == "resolved" {
                return Some(parsed);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// Stage a pending `bridge` feed item whose active chain link is `resolver_id`
/// and whose pane points at the live tmux pane. The product never attaches a
/// pane to a hook-created bridge item, so the happy path is built through the
/// library rather than driven through `rimz hooks feed`.
fn stage_bridge_item_with_pane(
    env: &Env,
    resolver_id: &str,
    pane_raw: &str,
    session: &str,
) -> String {
    let state =
        rimz::StatePaths::under(env.workspace_id.clone(), &env.state_root()).expect("state paths");
    let runtime =
        rimz::RuntimePaths::under(env.workspace_id.clone(), &env.runtime_root).expect("runtime");
    let ledger = rimz::Ledger::open(state, runtime).expect("open ledger");

    let mut item = rimz::FeedItem::new(
        env.workspace_id.clone(),
        rimz::Surface::Bridge,
        rimz::FeedKind::Permission,
        "Are you sure?",
        "claude",
        "agent-hook",
    );
    item.pane = Some(rimz::feed::PaneRef {
        pane_id: rimz::PaneId::from_parts(rimz::MuxName::Tmux, pane_raw),
        session_name: session.to_owned(),
        view_id: None,
        view_kind: None,
        pane_process_start: None,
    });
    item.activate_resolver_chain(vec![rimz::ResolverStep {
        resolver_id: resolver_id.parse().expect("resolver id"),
        display_name: None,
        order: 10,
        budget_ms: 60_000,
        state: rimz::ResolverStepState::Queued,
        reason: None,
    }]);
    let request_id = item.request_id.to_string();
    ledger
        .push_feed_item(&item, session)
        .expect("push bridge item");
    request_id
}

/// Full happy path against a live tmux pane: the resolver captures the pane,
/// matches the bounded prompt, types `y` + Enter through `rimz pane send`,
/// re-captures, and resolves the item with `--method pane-send`. Asserts both
/// halves of the round trip — the pane received the keystrokes (sentinel file)
/// and the ledger records a `pane_send` resolution from this resolver.
#[test]
fn pane_send_resolver_completes_full_round_trip() {
    let env = Env::new();
    if skip_preconditions(&env) {
        return;
    }
    if !tmux_present() {
        tracing::warn!("skipping: tmux not on PATH");
        return;
    }

    let tmpdir = env.tmux_tmpdir();
    let session = "rimz-pane-send";
    let sentinel = env.project_root().join("answered.txt");
    let _ = std::fs::remove_file(&sentinel);

    // The pane prints exactly one bounded prompt, blocks on a line of input,
    // then records the answer so the test can prove the keystrokes landed.
    let script = format!(
        "printf 'Are you sure? [y/N] '; read ans; printf 'ANSWERED:%s' \"$ans\" > '{}'; sleep 30",
        sentinel.display()
    );
    let started = run_tmux(
        &tmpdir,
        &["new-session", "-d", "-s", session, "sh", "-c", &script],
    );
    assert!(
        started.status.success(),
        "tmux new-session failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );

    let listed = run_tmux(&tmpdir, &["list-panes", "-t", session, "-F", "#{pane_id}"]);
    assert!(listed.status.success(), "tmux list-panes failed");
    let pane_raw = String::from_utf8_lossy(&listed.stdout).trim().to_owned();
    assert!(
        pane_raw.starts_with('%'),
        "unexpected tmux pane id {pane_raw:?}"
    );

    wait_for_prompt(&tmpdir, &pane_raw, Instant::now() + Duration::from_secs(5));

    let resolver_id = "pane-happy";
    let request_id = stage_bridge_item_with_pane(&env, resolver_id, &pane_raw, session);

    let mut resolver = env.spawn_python_resolver(resolver_id, 10.0, Some(&pane_raw));
    wait_for_heartbeat(&env, resolver_id, Instant::now() + Duration::from_secs(3));

    let resolved = poll_until_resolved(&env, &request_id, Instant::now() + Duration::from_secs(10));

    let _ = resolver.kill();
    let _ = resolver.wait();
    let _ = run_tmux(&tmpdir, &["kill-server"]);

    let item = resolved.expect("resolver never resolved the bridge item");
    assert_eq!(item["status"], "resolved", "feed item: {item}");
    assert_eq!(
        item["resolution"]["method"], "pane_send",
        "resolution should be attributed to pane-send: {item}"
    );
    assert_eq!(
        item["resolution"]["resolver_id"], resolver_id,
        "resolution should name the answering resolver: {item}"
    );

    // The pane actually received `y` + Enter — the send half of the loop.
    let answer = std::fs::read_to_string(&sentinel).unwrap_or_default();
    assert_eq!(
        answer.trim(),
        "ANSWERED:y",
        "pane did not receive the keystrokes; sentinel was {answer:?}"
    );
}
