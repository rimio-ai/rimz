use serde_json::json;

use rimz::feed::{FeedItem, FeedKind, Surface};
use rimz::ids::{MuxName, PaneId};
use rimz::message::MessageStatus;

use crate::common::Env;

#[test]
fn queue_add_list_remove_and_clear_for_running_agent() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-queue", "feature-q", None);

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
    register_running_agent(&env, "sess-deliver", "feature-d", None);

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
    register_running_agent(&env, "sess-no-hooks", "feature-q", None);

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
    register_running_agent(
        &env,
        "sess-steer",
        "feature-s",
        Some(PaneId::from_parts(MuxName::Tmux, "%1")),
    );
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

fn register_running_agent(env: &Env, session_id: &str, branch: &str, pane: Option<PaneId>) {
    let pane_env = pane
        .as_ref()
        .and_then(|pane| (pane.mux() == MuxName::Tmux).then_some(("TMUX_PANE", pane.raw())));
    let pane_env = pane_env.into_iter().collect::<Vec<_>>();
    run_hook(
        env,
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
        }),
        &pane_env,
    );
    run_hook(
        env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "work",
            "worktree_branch": branch,
        }),
        &pane_env,
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
