//! Integration coverage for the reference Python pane-send resolver handler
//! under `examples/resolvers/`. The full happy path requires a live tmux pane
//! to capture/send against and self-skips without `python3` or `tmux`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use rimz::ledger::runtime::current_process_owner;
use rimz::pane::RuntimeOwnerKind;

use crate::common::{Env, ScrubSessionEnvExt, example_resolver_script, skip_preconditions};

fn tmux_tmpdir(env: &Env) -> PathBuf {
    let dir = env.project_root.join("tmux");
    std::fs::create_dir_all(&dir).expect("mkdir tmux");
    dir
}

fn tmux_present() -> bool {
    Command::new("tmux")
        .scrub_session_env()
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_tmux(tmpdir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .scrub_session_env()
        .args(args)
        .env("TMUX_TMPDIR", tmpdir)
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

fn stage_native_item_with_pane(env: &Env, pane_raw: &str, session: &str) -> (String, String) {
    let ledger = env.ledger();
    let pane_id = rimz::PaneId::from_parts(rimz::MuxName::Tmux, pane_raw);

    let mut item = rimz::FeedItem::new(
        env.workspace_id.clone(),
        rimz::Surface::NativeUi,
        rimz::FeedKind::Permission,
        "Are you sure?",
        "claude",
        "agent-hook",
    );
    item.pane = Some(rimz::pane::PaneRef {
        pane_id: pane_id.clone(),
        session_name: session.to_owned(),
        view_id: None,
        view_kind: None,
        view_name: None,
        is_focused: false,
        is_floating: false,
        command: Some("sh".to_owned()),
        spawn_command: None,
        cwd: None,
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    });
    item.runtime_owner = Some(current_process_owner(
        RuntimeOwnerKind::Agent,
        "pane-send-agent",
    ));
    let request_id = item.request_id.to_string();
    ledger.push_feed_item(&item, session).expect("push item");
    (request_id, pane_id.as_str().to_owned())
}

fn run_handler(
    env: &Env,
    tmpdir: &Path,
    request_id: &str,
    pane_raw: &str,
    pane_id: &str,
) -> std::process::Output {
    let script = example_resolver_script("pane_send_resolver.py");
    assert!(script.exists(), "resolver script missing: {script:?}");
    Command::new("python3")
        .scrub_session_env()
        .arg(script)
        .arg("--rimz-bin")
        .arg(env.rimz_bin())
        .arg("--by")
        .arg("pane-happy")
        .env("XDG_STATE_HOME", env.state_root())
        .env("XDG_RUNTIME_DIR", &env.runtime_root)
        .env("XDG_CONFIG_HOME", env.config_root())
        .env("HOME", &env.home_root)
        .env("TMUX_TMPDIR", tmpdir)
        .env("TMUX_PANE", pane_raw)
        .env("RIMZ_NOTIFY_REQUEST_ID", request_id)
        .env("RIMZ_NOTIFY_PANE", pane_id)
        .env("RIMZ_NOTIFY_ROOT", &env.project_root)
        .env_remove("RUST_LOG")
        .current_dir(&env.project_root)
        .output()
        .expect("spawn resolver handler")
}

/// Full happy path against a live tmux pane: the handler captures the pane,
/// matches the bounded prompt, types `y` + Enter through `rimz pane send`,
/// re-captures, and resolves the item with `--method pane-send`.
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

    let tmpdir = tmux_tmpdir(&env);
    let session = "rimz-pane-send";
    let sentinel = env.project_root.join("answered.txt");
    let _ = std::fs::remove_file(&sentinel);

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

    let (request_id, pane_id) = stage_native_item_with_pane(&env, &pane_raw, session);
    let handler = run_handler(&env, &tmpdir, &request_id, &pane_raw, &pane_id);
    let _ = run_tmux(&tmpdir, &["kill-server"]);
    assert!(
        handler.status.success(),
        "handler stderr: {}",
        String::from_utf8_lossy(&handler.stderr)
    );

    let item = poll_until_resolved(&env, &request_id, Instant::now() + Duration::from_secs(5))
        .expect("handler never resolved the native_ui item");
    assert_eq!(item["status"], "resolved", "feed item: {item}");
    assert_eq!(
        item["resolution"]["method"], "pane_send",
        "resolution should be attributed to pane-send: {item}"
    );
    assert_eq!(
        item["resolution"]["by"], "pane-happy",
        "resolution should name the answering handler: {item}"
    );

    let answer = std::fs::read_to_string(&sentinel).unwrap_or_default();
    assert_eq!(
        answer.trim(),
        "ANSWERED:y",
        "pane did not receive the keystrokes; sentinel was {answer:?}"
    );
}
