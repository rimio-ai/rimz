use serde_json::json;

use std::path::{Path, PathBuf};

use rimz::feed::{FeedItem, FeedKind, Surface};
use rimz::message::MessageStatus;

use crate::common::Env;

#[test]
fn queue_add_list_remove_and_clear_for_running_agent() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-queue", "feature-q", &[]);

    let first = queue_add(&env, "claude", "first task");
    let second = queue_add(&env, "claude", "second task");
    assert_ne!(first, second);

    let pending = env.ledger().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].text, "first task");
    assert_eq!(pending[1].text, "second task");
    assert!(pending.iter().all(|message| {
        message.status == MessageStatus::Pending
            && message.attempts == 0
            && message.last_attempt_at.is_none()
    }));

    let listed = env
        .rimz()
        .args(["queue", "list", "--json"])
        .output()
        .expect("queue list");
    assert!(
        listed.status.success(),
        "queue list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("json");
    assert_eq!(parsed.as_array().expect("messages").len(), 2);

    let removed = env
        .rimz()
        .args(["queue", "remove", &first])
        .output()
        .expect("queue remove");
    assert!(
        removed.status.success(),
        "queue remove failed: {}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let pending = env.ledger().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message_id.as_str(), second);

    let cleared = env
        .rimz()
        .args(["queue", "clear", "claude"])
        .output()
        .expect("queue clear");
    assert!(
        cleared.status.success(),
        "queue clear failed: {}",
        String::from_utf8_lossy(&cleared.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&cleared.stdout).trim(), "1");
    assert!(env.ledger().list_pending_messages().unwrap().is_empty());

    let methods: Vec<String> = env
        .read_events()
        .into_iter()
        .map(|event| event.method)
        .filter(|method| method.starts_with("message."))
        .collect();
    assert!(
        methods.iter().any(|method| method == "message.queued"),
        "queued audit event missing: {methods:?}"
    );
    assert!(
        methods.iter().any(|method| method == "message.removed"),
        "removed audit event missing: {methods:?}"
    );
}

/// Eligibility runs before the claim: an ineligible delivery pass (running
/// agent at add time, eligible-but-paneless agent at turn end) leaves the
/// message pending with no claim stamp, so the next real transition can
/// deliver it immediately.
#[test]
fn deliver_leaves_ineligible_message_unclaimed() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-deliver", "feature-d", &[]);

    let message_id = queue_add(&env, "claude", "next task");

    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-deliver",
            "worktree_branch": "feature-d",
        }),
        &[],
    );

    let out = env
        .rimz()
        .env("RIMZ_QUEUE_SETTLE_MS", "0")
        .args(["queue", "deliver", "--message-id", &message_id])
        .output()
        .expect("queue deliver");
    assert!(
        out.status.success(),
        "queue deliver failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let pending = env.ledger().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, MessageStatus::Pending);
    assert_eq!(pending[0].attempts, 0, "no-pane miss must not claim");
    assert!(pending[0].last_attempt_at.is_none());
}

#[test]
fn queue_refuses_without_installed_hooks() {
    let env = Env::new();
    register_running_agent(&env, "sess-no-hooks", "feature-q", &[]);

    let out = env
        .rimz()
        .args(["queue", "claude", "--", "next task"])
        .output()
        .expect("queue add");
    assert!(!out.status.success(), "queue should fail without hooks");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("requires claude hooks"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn steer_refuses_pending_ask_before_touching_pane() {
    let env = Env::new();
    register_running_agent(&env, "sess-steer", "feature-s", &[("TMUX_PANE", "%1")]);
    push_pending_agent_ask(&env, "sess-steer");

    let out = env
        .rimz()
        .args(["steer", "claude", "--", "continue"])
        .output()
        .expect("steer");
    assert!(!out.status.success(), "steer should fail on pending ask");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("has pending ask") && stderr.contains("--force"),
        "unexpected stderr: {stderr}"
    );
}

/// `steer` types the text and then presses Enter as a discrete key event —
/// never a carriage return folded into the typed text. Agent UIs submit on the
/// keystroke but take an embedded newline as a composer line break, so the
/// distinction is the whole feature. Drives a real `rimz steer` against the
/// zellij-trace shim and asserts the recorded action sequence: `write-chars`
/// of the text, then a discrete `write 13` (Enter), with no `\r` anywhere.
#[test]
fn steer_presses_enter_as_discrete_key() {
    let env = Env::new();
    register_running_agent(
        &env,
        "sess-steer-enter",
        "feature-se",
        &[("ZELLIJ_PANE_ID", "3")],
    );

    let trace_log = env.project_root.join("zellij-steer-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["steer", "claude", "--", "y"])
        .output()
        .expect("steer");
    assert!(
        out.status.success(),
        "steer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_text_then_enter(&trace_log, "y");
}

/// `--no-enter` types the text and stops — no Enter keystroke at all.
#[test]
fn steer_no_enter_suppresses_the_keystroke() {
    let env = Env::new();
    register_running_agent(
        &env,
        "sess-steer-quiet",
        "feature-sq",
        &[("ZELLIJ_PANE_ID", "3")],
    );

    let trace_log = env.project_root.join("zellij-steer-quiet-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["steer", "claude", "--no-enter", "--", "y"])
        .output()
        .expect("steer");
    assert!(
        out.status.success(),
        "steer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lines = trace_lines(&trace_log);
    assert!(
        lines.iter().any(|line| is_write_chars(line, "y")),
        "expected write-chars of `y`; trace: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| is_enter_key(line)),
        "--no-enter must not press Enter; trace: {lines:?}"
    );
}

/// Queue delivery routes through the same send path: a message delivered at an
/// open gate presses Enter as a discrete key, not a literal carriage return.
/// Bringing the agent to an idle turn boundary with a bound pane lets the queue
/// `add` deliver inline, so the assertion stays synchronous.
#[test]
fn queue_delivery_presses_enter_as_discrete_key() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-queue-enter", "feature-qe", pane_env);
    // A turn end opens the `done` gate; the agent keeps its bound pane.
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-queue-enter",
            "worktree_branch": "feature-qe",
        }),
        pane_env,
    );

    let trace_log = env.project_root.join("zellij-queue-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_QUEUE_SETTLE_MS", "0")
        .args(["queue", "claude", "--", "go"])
        .output()
        .expect("queue add");
    assert!(
        out.status.success(),
        "queue add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        env.ledger().list_pending_messages().unwrap().is_empty(),
        "an idle agent with a bound pane should deliver the queued message inline"
    );
    assert_text_then_enter(&trace_log, "go");
}

fn zellij_trace_shim() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_zellij-trace"))
}

fn trace_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|raw| raw.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The shim records each invocation as tab-separated argv (`argv0\t…`). Pin the
/// full action tail, including `--pane-id <pane>`, so a send to the wrong pane
/// fails rather than passing on action type alone.
const TRACE_PANE: &str = "terminal_3";

/// A `zellij action write-chars --pane-id <pane> <text>` line.
fn is_write_chars(line: &str, text: &str) -> bool {
    line.ends_with(&format!(
        "\taction\twrite-chars\t--pane-id\t{TRACE_PANE}\t{text}"
    ))
}

/// A discrete `zellij action write --pane-id <pane> 13` line — Enter sent as its
/// own key event (`NamedKey::Enter` writes byte `13`), distinct from the text.
fn is_enter_key(line: &str) -> bool {
    line.ends_with(&format!("\taction\twrite\t--pane-id\t{TRACE_PANE}\t13"))
}

/// The shim recorded `write-chars <text>` followed by a discrete `write 13` —
/// both to `TRACE_PANE` — with no carriage return folded into any sent payload.
fn assert_text_then_enter(trace_log: &Path, text: &str) {
    let raw = std::fs::read_to_string(trace_log).unwrap_or_default();
    assert!(
        !raw.contains('\r'),
        "no carriage return should be folded into the sent text; trace: {raw:?}"
    );
    let lines = trace_lines(trace_log);
    let text_at = lines.iter().position(|line| is_write_chars(line, text));
    let enter_at = lines.iter().position(|line| is_enter_key(line));
    assert!(
        text_at.is_some(),
        "expected write-chars of `{text}`; trace: {lines:?}"
    );
    assert!(
        enter_at.is_some(),
        "expected Enter as a discrete `write 13`; trace: {lines:?}"
    );
    assert!(
        text_at < enter_at,
        "text must be typed before Enter; trace: {lines:?}"
    );
}

fn register_running_agent(env: &Env, session_id: &str, branch: &str, pane_env: &[(&str, &str)]) {
    run_hook(
        env,
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
        }),
        pane_env,
    );
    run_hook(
        env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "work",
            "worktree_branch": branch,
        }),
        pane_env,
    );
}

fn run_hook(env: &Env, payload: serde_json::Value, pane_env: &[(&str, &str)]) {
    let payload = serde_json::to_string(&payload).expect("payload");
    let output = env.run_installed_hook_in_pane("claude", &payload, pane_env);
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn queue_add(env: &Env, target: &str, text: &str) -> String {
    let out = env
        .rimz()
        .args(["queue", target, "--", text])
        .output()
        .expect("queue add");
    assert!(
        out.status.success(),
        "queue add failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn push_pending_agent_ask(env: &Env, session_id: &str) {
    let mut item = FeedItem::new(
        env.workspace_id.clone(),
        Surface::NativeUi,
        FeedKind::Permission,
        "approve?",
        "claude",
        "agent-hook",
    );
    item.payload = json!({ "session_id": session_id });
    env.ledger()
        .push_feed_item(&item, "rimz-test")
        .expect("push pending ask");
}
