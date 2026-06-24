use serde_json::json;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use rimz::feed::{FeedItem, FeedKind, Surface};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use rimz::message::{MessageBody, MessageStatus};
use rimz::schema::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};

use crate::common::Env;

#[test]
fn queue_add_list_remove_and_clear_for_running_agent() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-queue", "feature-q", &[]);

    let first = queue_add(&env, "@claude", "first task");
    let second = queue_add(&env, "@claude", "second task");
    assert_ne!(first, second);

    let pending = env.ledger().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].text, "first task");
    assert_eq!(pending[1].text, "second task");
    assert!(pending.iter().all(|message| {
        message.status == MessageStatus::Queued
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
        .args(["queue", "clear", "@claude"])
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

#[test]
fn queue_add_for_bound_agent_does_not_enumerate_panes() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-rollup", "feature-rollup", &[]);

    let trace_log = env.project_root.join("zellij-queue-rollup-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["--mux", "zellij", "queue", "@claude", "--", "cached path"])
        .output()
        .expect("queue add");
    assert!(
        out.status.success(),
        "queue add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let trace = trace_lines(&trace_log);
    assert!(
        trace.is_empty(),
        "queue success path must not call zellij: {trace:?}"
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

    let message_id = queue_add(&env, "@claude", "next task");

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
    assert_eq!(pending[0].status, MessageStatus::Queued);
    assert_eq!(pending[0].attempts, 0, "no-pane miss must not claim");
    assert!(pending[0].last_attempt_at.is_none());
}

#[test]
fn queue_refuses_without_installed_hooks() {
    let env = Env::new();
    register_running_agent(&env, "sess-no-hooks", "feature-q", &[]);

    let out = env
        .rimz()
        .args(["queue", "@claude", "--", "next task"])
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
        .args(["steer", "@claude", "--", "continue"])
        .output()
        .expect("steer");
    assert!(!out.status.success(), "steer should fail on pending ask");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("has pending ask") && stderr.contains("--force"),
        "unexpected stderr: {stderr}"
    );
}

/// `steer` bracket-pastes the text and then presses Enter as a discrete key
/// event outside the paste — never a carriage return folded into the typed
/// text. Agent UIs submit on the keystroke but take an embedded newline as a
/// composer line break, so the distinction is the whole feature. Drives a real
/// `rimz steer` against the zellij-trace shim and asserts the recorded action
/// sequence: a bracketed paste of the text, then a discrete `write 13` (Enter),
/// with no `\r` anywhere.
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
        .args(["steer", "@claude", "--", "y"])
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
        .args(["steer", "@claude", "--no-enter", "--", "y"])
        .output()
        .expect("steer");
    assert!(
        out.status.success(),
        "steer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lines = trace_lines(&trace_log);
    assert!(
        lines.iter().any(|line| is_paste(line, "y")),
        "expected a bracketed paste of `y`; trace: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| is_enter_key(line)),
        "--no-enter must not press Enter; trace: {lines:?}"
    );
}

#[test]
fn steer_wait_conflicts_with_no_enter() {
    let env = Env::new();
    let out = env
        .rimz()
        .args(["steer", "@claude", "--wait", "--no-enter", "--", "y"])
        .output()
        .expect("steer --wait --no-enter");

    assert!(!out.status.success(), "--wait --no-enter should fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--wait requires submitting"),
        "error explains the conflict: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn steer_wait_times_out_without_turn_started_ack() {
    let env = Env::new();
    register_running_agent(
        &env,
        "sess-wait-timeout",
        "feature-wait-timeout",
        &[("ZELLIJ_PANE_ID", "3")],
    );

    let trace_log = env.project_root.join("zellij-wait-timeout-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["steer", "@claude", "--wait=0s", "--", "y"])
        .output()
        .expect("steer --wait");

    assert!(
        !out.status.success(),
        "--wait should exit nonzero on timeout"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("timed out"),
        "stdout reports timeout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let messages = env.ledger().list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].status, MessageStatus::TimedOut);
}

/// A `\n` in the steered text is a soft composer newline: it rides inside the
/// bracketed paste as a real newline byte, so the message lands multi-line and
/// the submit Enter is still the one discrete keystroke. The CLI interprets the
/// two-character `\n` escape so a multi-line prompt can be typed inline.
#[test]
fn steer_interprets_a_newline_escape_as_a_soft_break() {
    let env = Env::new();
    register_running_agent(&env, "sess-nl", "feature-nl", &[("ZELLIJ_PANE_ID", "3")]);

    let trace_log = env.project_root.join("zellij-nl-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["steer", "@claude", "--", "first\\nsecond"])
        .output()
        .expect("steer");
    assert!(
        out.status.success(),
        "steer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The paste carries a real newline (`first<LF>second`), then a discrete Enter.
    assert_text_then_enter(&trace_log, "first\nsecond");
}

/// `--file` sends a prompt read verbatim: a real newline rides as a soft break,
/// a literal `\n` stays two characters (no inline unescaping), and the trailing
/// newline is trimmed before the submit. queue shares the flag through the same
/// `SendFlags`.
#[test]
fn steer_sends_a_file_as_the_prompt() {
    let env = Env::new();
    register_running_agent(
        &env,
        "sess-file",
        "feature-file",
        &[("ZELLIJ_PANE_ID", "3")],
    );

    let prompt_file = env.project_root.join("prompt.txt");
    std::fs::write(&prompt_file, "keep \\n literal\nand a real break\n")
        .expect("write prompt file");

    let trace_log = env.project_root.join("zellij-file-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args([
            "steer",
            "@claude",
            "--file",
            prompt_file.to_str().expect("utf-8 path"),
        ])
        .output()
        .expect("steer --file");
    assert!(
        out.status.success(),
        "steer --file failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The literal `\n` survives as backslash-n, the real newline is the only soft
    // break, and the trailing newline is gone.
    assert_text_then_enter(&trace_log, "keep \\n literal\nand a real break");
}

#[test]
fn steer_agent_env_prefixes_sender_and_no_from_suppresses_it() {
    let env = Env::new();
    register_running_agent(
        &env,
        "sess-from-steer",
        "feature-from-steer",
        &[("ZELLIJ_PANE_ID", "3")],
    );

    let trace_log = env.project_root.join("zellij-from-steer-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_AGENT_KIND", "codex")
        .env("RIMZ_AGENT_NAME", "swift-otter")
        .args(["steer", "@claude", "--", "ping"])
        .output()
        .expect("steer from agent");
    assert!(
        out.status.success(),
        "steer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_text_then_enter(&trace_log, "from @swift-otter: ping");
    let sent = env
        .read_events()
        .into_iter()
        .find(|event| event.method == "message.sent")
        .expect("sent event");
    assert_eq!(sent.params["sender"]["origin"], "agent");
    assert_eq!(sent.params["sender"]["kind"], "codex");
    assert_eq!(sent.params["sender"]["name"], "swift-otter");
    assert_eq!(sent.params["text_len"], "ping".len());
    assert_eq!(sent.params["status"], "sent");

    let trace_log = env.project_root.join("zellij-from-steer-no-from-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_AGENT_KIND", "codex")
        .env("RIMZ_AGENT_NAME", "swift-otter")
        .args(["steer", "@claude", "--no-from", "--", "exact"])
        .output()
        .expect("steer --no-from from agent");
    assert!(
        out.status.success(),
        "steer --no-from failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_text_then_enter(&trace_log, "exact");
}

/// Send-now queue routes through the same live path as steer: an open-gate
/// target receives the text and Enter as a discrete key, not a literal carriage
/// return.
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
        .args(["queue", "@claude", "--", "go"])
        .output()
        .expect("queue add");
    assert!(
        out.status.success(),
        "queue add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        env.ledger().list_pending_messages().unwrap().is_empty(),
        "an idle agent with a bound pane should receive queue text immediately"
    );
    assert_text_then_enter(&trace_log, "go");
}

#[test]
fn queue_wait_conflicts_with_no_enter() {
    let env = Env::new();
    let out = env
        .rimz()
        .args(["queue", "@claude", "--wait", "--no-enter", "--", "go"])
        .output()
        .expect("queue --wait --no-enter");

    assert!(!out.status.success(), "--wait --no-enter should fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--wait requires submitting"),
        "error explains the conflict: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn queue_deliver_sends_deferred_message_and_marks_delivered() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-deferred-live", "feature-dl", pane_env);
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);

    let add = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["queue", "@claude", "--", "later"])
        .output()
        .expect("queue add");
    assert!(
        add.status.success(),
        "queue add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let message_id = queued_id_from_stdout(&add.stdout);
    let pending = env.ledger().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1, "running agent should park the message");
    assert_eq!(pending[0].status, MessageStatus::Queued);

    append_lifecycle(
        &env,
        "claude",
        "Stop",
        "sess-deferred-live",
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
        |observation| {
            observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
            observation.worktree_branch = Some("feature-dl".to_owned());
        },
    );

    let trace_log = env.project_root.join("zellij-deferred-deliver-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_QUEUE_SETTLE_MS", "0")
        .args(["queue", "deliver", "--message-id", &message_id])
        .output()
        .expect("queue deliver");
    assert!(
        out.status.success(),
        "queue deliver failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_text_then_enter(&trace_log, "later");
    let messages = env.ledger().list_messages().expect("messages");
    let message = messages
        .iter()
        .find(|message| message.message_id.as_str() == message_id)
        .expect("sent message");
    assert_eq!(message.status, MessageStatus::Sent);
    let methods: Vec<String> = env
        .read_events()
        .into_iter()
        .map(|event| event.method)
        .collect();
    assert!(
        methods.iter().any(|method| method == "message.sent"),
        "delivery records the shared send event: {methods:?}"
    );
    assert!(
        !methods.iter().any(|method| method == "message.delivered"),
        "delivery confirmation waits for the agent ack: {methods:?}"
    );

    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-deferred-live",
            "prompt": "later",
            "worktree_branch": "feature-dl",
        }),
        pane_env,
    );
    let messages = env.ledger().list_messages().expect("messages");
    let message = messages
        .iter()
        .find(|message| message.message_id.as_str() == message_id)
        .expect("delivered message");
    assert_eq!(message.status, MessageStatus::Delivered);
    assert!(
        env.read_events()
            .iter()
            .any(|event| event.method == "message.delivered"),
        "turn start confirms delivery"
    );
}

#[test]
fn queue_deliver_folds_provisional_message_to_registered_card_name() {
    let env = Env::new();
    env.install_agent_hooks("codex");
    trust_codex_hooks(&env);
    seed_running_provisional_codex_launch(
        &env,
        "launch_deferred_fold",
        "swift-otter",
        Some("coder"),
        "terminal_8",
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "codex")]);

    let add = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["queue", "@coder", "--", "read plan"])
        .output()
        .expect("queue add");
    assert!(
        add.status.success(),
        "queue add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let message_id = queued_id_from_stdout(&add.stdout);
    let pending = env.ledger().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1, "running launch card should park");
    assert_eq!(pending[0].agent_id.as_str(), "launch_deferred_fold");
    assert_eq!(pending[0].agent_name.as_deref(), Some("swift-otter"));

    append_lifecycle(
        &env,
        "codex",
        "SessionStart",
        "codex-real-session",
        LifecycleSignal::Registered,
        |observation| {
            observation.agent_name = Some("swift-otter".to_owned());
            observation.role = Some("coder".to_owned());
            observation.kind_ordinal = Some(1);
            observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
        },
    );

    let trace_log = env
        .project_root
        .join("zellij-provisional-deliver-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_QUEUE_SETTLE_MS", "0")
        .args(["queue", "deliver", "--message-id", &message_id])
        .output()
        .expect("queue deliver");
    assert!(
        out.status.success(),
        "queue deliver failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_text_then_enter(&trace_log, "read plan");
    let messages = env.ledger().list_messages().expect("messages");
    let message = messages
        .iter()
        .find(|message| message.message_id.as_str() == message_id)
        .expect("sent message");
    assert_eq!(message.status, MessageStatus::Sent);
    let agents = env.ledger().snapshot_cached().expect("snapshot").agents;
    assert!(
        agents.iter().any(|agent| {
            agent.agent_id.as_str() == "codex-real-session"
                && agent.name.as_deref() == Some("swift-otter")
        }),
        "registered card should consume the provisional name: {agents:?}"
    );
}

#[test]
fn queue_send_now_prefixes_sender_and_no_from_suppresses_it() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-from-queue", "feature-from-queue", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-from-queue",
            "worktree_branch": "feature-from-queue",
        }),
        pane_env,
    );

    let trace_log = env.project_root.join("zellij-from-queue-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_QUEUE_SETTLE_MS", "0")
        .env("RIMZ_AGENT_KIND", "codex")
        .env("RIMZ_AGENT_NAME", "swift-otter")
        .args(["queue", "@claude", "--", "later"])
        .output()
        .expect("queue add from agent");
    assert!(
        out.status.success(),
        "queue add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_text_then_enter(&trace_log, "from @swift-otter: later");
    let messages = env.ledger().list_messages().unwrap();
    assert_eq!(messages.len(), 1, "send-now queue writes a durable record");
    assert_eq!(messages[0].status, MessageStatus::Sent);
    let sent = env
        .read_events()
        .into_iter()
        .find(|event| event.method == "message.sent")
        .expect("sent event");
    assert_eq!(sent.params["sender"]["origin"], "agent");
    assert_eq!(sent.params["sender"]["kind"], "codex");
    assert_eq!(sent.params["sender"]["name"], "swift-otter");

    let trace_log = env.project_root.join("zellij-from-queue-no-from-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_QUEUE_SETTLE_MS", "0")
        .env("RIMZ_AGENT_KIND", "codex")
        .env("RIMZ_AGENT_NAME", "swift-otter")
        .args(["queue", "@claude", "--no-from", "--", "exact"])
        .output()
        .expect("queue add --no-from from agent");
    assert!(
        out.status.success(),
        "queue add --no-from failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_text_then_enter(&trace_log, "exact");
}

#[test]
fn queue_parked_message_lists_sender() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-from-queue-park", "feature-from-park", &[]);

    let out = env
        .rimz()
        .env("RIMZ_AGENT_KIND", "codex")
        .env("RIMZ_AGENT_NAME", "swift-otter")
        .args(["queue", "@claude", "--", "later"])
        .output()
        .expect("queue add from agent");
    assert!(
        out.status.success(),
        "queue add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

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
    let messages: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("json");
    assert_eq!(messages[0]["sender"]["origin"], "agent");
    assert_eq!(messages[0]["sender"]["kind"], "codex");
    assert_eq!(messages[0]["sender"]["name"], "swift-otter");
}

/// `steer --smart-compact 70%` against a window past the threshold sends a
/// tracked `/compact` command before the prompt. The command confirms on
/// `Compacting`; the prompt confirms independently on `TurnStarted`.
#[test]
fn steer_auto_compact_runs_compact_before_a_full_window() {
    let env = Env::new();
    register_running_agent(&env, "sess-ac", "feature-ac", &[("ZELLIJ_PANE_ID", "3")]);
    seed_context_fill(&env, "sess-ac", 80);

    let trace_log = env.project_root.join("zellij-ac-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["steer", "@claude", "--smart-compact", "70%", "--", "go"])
        .output()
        .expect("steer");
    assert!(
        out.status.success(),
        "steer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lines = trace_lines(&trace_log);
    let compact_at = lines.iter().position(|line| is_compact_command(line));
    let paste_at = lines.iter().position(|line| is_paste(line, "go"));
    assert!(
        compact_at.is_some(),
        "expected a `/compact` write-chars; trace: {lines:?}"
    );
    assert!(
        paste_at.is_some(),
        "expected a bracketed paste of `go`; trace: {lines:?}"
    );
    assert!(
        compact_at < paste_at,
        "compaction must precede the message; trace: {lines:?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("compacted"),
        "a single steer reports the compaction it ran: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("sent"),
        "a single steer still reports the prompt send: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let messages = env.ledger().list_messages().expect("messages");
    let command = messages
        .iter()
        .find(|message| message.body == MessageBody::Command)
        .expect("command message");
    let prompt = messages
        .iter()
        .find(|message| message.body == MessageBody::Prompt)
        .expect("prompt message");
    assert_eq!(command.text, "/compact");
    assert_eq!(command.status, MessageStatus::Sent);
    assert_eq!(prompt.text, "go");
    assert_eq!(prompt.status, MessageStatus::Sent);
    let command_id = command.message_id.clone();
    let prompt_id = prompt.message_id.clone();

    run_hook(
        &env,
        json!({
            "hook_event_name": "PreCompact",
            "session_id": "sess-ac",
            "worktree_branch": "feature-ac",
        }),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let messages = env.ledger().list_messages().expect("messages");
    assert_eq!(
        messages
            .iter()
            .find(|message| message.message_id == command_id)
            .expect("command after compacting")
            .status,
        MessageStatus::Delivered
    );
    assert_eq!(
        messages
            .iter()
            .find(|message| message.message_id == prompt_id)
            .expect("prompt after compacting")
            .status,
        MessageStatus::Sent,
        "Compacting confirms only the command"
    );
    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-ac",
            "prompt": "go",
            "worktree_branch": "feature-ac",
        }),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let messages = env.ledger().list_messages().expect("messages");
    assert_eq!(
        messages
            .iter()
            .find(|message| message.message_id == prompt_id)
            .expect("prompt after turn start")
            .status,
        MessageStatus::Delivered
    );
}

/// A stale carried-forward token gauge suppresses duplicate `/compact` for the
/// same full-window reading.
#[test]
fn steer_auto_compact_suppresses_a_second_compaction_on_an_unchanged_window() {
    let env = Env::new();
    register_running_agent(
        &env,
        "sess-ac-dupe",
        "feature-ac-dupe",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_context_tokens(&env, "sess-ac-dupe", 150_000, 200_000);

    let first_trace = env.project_root.join("zellij-ac-dupe-first-trace.log");
    let first = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &first_trace)
        .args(["steer", "@claude", "--smart-compact", "70%", "--", "go1"])
        .output()
        .expect("first steer");
    assert!(
        first.status.success(),
        "first steer failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let first_lines = trace_lines(&first_trace);
    let compact_at = first_lines.iter().position(|line| is_compact_command(line));
    let paste_at = first_lines.iter().position(|line| is_paste(line, "go1"));
    assert!(
        compact_at.is_some() && paste_at.is_some() && compact_at < paste_at,
        "first send should compact before prompt; trace: {first_lines:?}"
    );

    let second_trace = env.project_root.join("zellij-ac-dupe-second-trace.log");
    let second = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &second_trace)
        .args(["steer", "@claude", "--smart-compact", "70%", "--", "go2"])
        .output()
        .expect("second steer");
    assert!(
        second.status.success(),
        "second steer failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let second_lines = trace_lines(&second_trace);
    assert!(
        second_lines.iter().any(|line| is_paste(line, "go2")),
        "second send should still paste the prompt; trace: {second_lines:?}"
    );
    assert!(
        !second_lines.iter().any(|line| is_compact_command(line)),
        "unchanged token reading must not compact again; trace: {second_lines:?}"
    );

    let messages = env.ledger().list_messages().expect("messages");
    let commands: Vec<_> = messages
        .iter()
        .filter(|message| message.body == MessageBody::Command && message.text == "/compact")
        .collect();
    assert_eq!(commands.len(), 1, "command records: {commands:?}");
    assert_eq!(commands[0].compacted_context_tokens, Some(150_000));
}

/// A changed occupied-token reading means the agent filled the window again, so
/// the duplicate guard releases and smart-compact can run a new `/compact`.
#[test]
fn steer_auto_compact_recompacts_after_a_fresh_reading() {
    let env = Env::new();
    register_running_agent(
        &env,
        "sess-ac-refill",
        "feature-ac-refill",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_context_tokens(&env, "sess-ac-refill", 150_000, 200_000);

    let first_trace = env.project_root.join("zellij-ac-refill-first-trace.log");
    let first = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &first_trace)
        .args(["steer", "@claude", "--smart-compact", "70%", "--", "go1"])
        .output()
        .expect("first steer");
    assert!(
        first.status.success(),
        "first steer failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        trace_lines(&first_trace)
            .iter()
            .any(|line| is_compact_command(line)),
        "first send should compact"
    );

    seed_context_tokens(&env, "sess-ac-refill", 160_000, 200_000);

    let second_trace = env.project_root.join("zellij-ac-refill-second-trace.log");
    let second = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &second_trace)
        .args(["steer", "@claude", "--smart-compact", "70%", "--", "go2"])
        .output()
        .expect("second steer");
    assert!(
        second.status.success(),
        "second steer failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_lines = trace_lines(&second_trace);
    assert!(
        second_lines.iter().any(|line| is_compact_command(line))
            && second_lines.iter().any(|line| is_paste(line, "go2")),
        "fresh token reading should compact again before prompt; trace: {second_lines:?}"
    );

    let messages = env.ledger().list_messages().expect("messages");
    let commands: Vec<_> = messages
        .iter()
        .filter(|message| message.body == MessageBody::Command && message.text == "/compact")
        .collect();
    assert_eq!(commands.len(), 2, "command records: {commands:?}");
    let mut baselines: Vec<_> = commands
        .iter()
        .filter_map(|message| message.compacted_context_tokens)
        .collect();
    baselines.sort_unstable();
    assert_eq!(baselines, vec![150_000, 160_000]);
}

/// Auto-compacted sends are two messages. The pacer sleeps between the tracked
/// `/compact` command and the prompt.
#[test]
fn steer_auto_compact_paces_command_and_prompt() {
    let env = Env::new();
    register_running_agent(
        &env,
        "sess-ac-paced",
        "feature-ac-paced",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_context_fill(&env, "sess-ac-paced", 80);

    let trace_log = env.project_root.join("zellij-ac-paced-trace.log");
    let interval = Duration::from_millis(1500);
    let started = Instant::now();
    let out = env
        .rimz()
        .env("RIMZ_MESSAGE_INTERVAL_MS", interval.as_millis().to_string())
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["steer", "@claude", "--smart-compact", "70%", "--", "go"])
        .output()
        .expect("steer");
    let elapsed = started.elapsed();
    assert!(
        out.status.success(),
        "steer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        elapsed >= interval,
        "the prompt should wait for the message interval after `/compact`; elapsed {elapsed:?}"
    );

    let lines = trace_lines(&trace_log);
    let compact_at = lines.iter().position(|line| is_compact_command(line));
    let paste_at = lines.iter().position(|line| is_paste(line, "go"));
    assert!(
        compact_at.is_some() && paste_at.is_some() && compact_at < paste_at,
        "compaction must precede the message under pacing; trace: {lines:?}"
    );
}

/// `[harness] smart_compact` gives steer the same compact-first threshold
/// as the flag when the invocation omits it.
#[test]
fn steer_auto_compact_uses_config_default() {
    let env = Env::new();
    let config_dir = env.config_root().join("rimz");
    std::fs::create_dir_all(&config_dir).expect("mkdir config");
    std::fs::write(
        config_dir.join("config.toml"),
        "[harness]\nsmart_compact = \"70%\"\n",
    )
    .expect("write config");
    register_running_agent(
        &env,
        "sess-ac-config",
        "feature-ac-config",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_context_fill(&env, "sess-ac-config", 80);

    let trace_log = env.project_root.join("zellij-ac-config-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["steer", "@claude", "--", "go"])
        .output()
        .expect("steer");
    assert!(
        out.status.success(),
        "steer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lines = trace_lines(&trace_log);
    let compact_at = lines.iter().position(|line| is_compact_command(line));
    let paste_at = lines.iter().position(|line| is_paste(line, "go"));
    assert!(
        compact_at.is_some(),
        "expected a `/compact` write-chars; trace: {lines:?}"
    );
    assert!(
        paste_at.is_some(),
        "expected a bracketed paste of `go`; trace: {lines:?}"
    );
    assert!(
        compact_at < paste_at,
        "config default compaction must precede the message; trace: {lines:?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("compacted"),
        "a config-triggered single steer reports the compaction it ran: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// A window below the threshold delivers normally — no `/compact`, just the
/// message pasted and submitted.
#[test]
fn steer_auto_compact_leaves_a_window_below_threshold_alone() {
    let env = Env::new();
    register_running_agent(
        &env,
        "sess-ac-low",
        "feature-acl",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_context_fill(&env, "sess-ac-low", 50);

    let trace_log = env.project_root.join("zellij-ac-low-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["steer", "@claude", "--smart-compact", "70%", "--", "go"])
        .output()
        .expect("steer");
    assert!(
        out.status.success(),
        "steer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lines = trace_lines(&trace_log);
    assert!(
        !lines.iter().any(|line| is_compact_command(line)),
        "a window below the threshold must not compact; trace: {lines:?}"
    );
    assert_text_then_enter(&trace_log, "go");
}

/// Send-now queue honours `--smart-compact`: an idle agent past the threshold
/// gets `/compact` ahead of the text.
#[test]
fn queue_auto_compact_runs_compact_before_delivering() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-qac", "feature-qac", pane_env);
    seed_context_fill(&env, "sess-qac", 80);
    // A turn end opens the `done` gate; the agent keeps its bound pane.
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-qac",
            "worktree_branch": "feature-qac",
        }),
        pane_env,
    );

    let trace_log = env.project_root.join("zellij-qac-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_QUEUE_SETTLE_MS", "0")
        .args(["queue", "@claude", "--smart-compact", "70%", "--", "go"])
        .output()
        .expect("queue add");
    assert!(
        out.status.success(),
        "queue add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        env.ledger().list_pending_messages().unwrap().is_empty(),
        "the message should send immediately at the open gate"
    );
    let lines = trace_lines(&trace_log);
    let compact_at = lines.iter().position(|line| is_compact_command(line));
    let paste_at = lines.iter().position(|line| is_paste(line, "go"));
    assert!(
        compact_at.is_some() && paste_at.is_some() && compact_at < paste_at,
        "compaction must precede the queue text; trace: {lines:?}"
    );
    let messages = env.ledger().list_messages().expect("messages");
    assert!(
        messages.iter().any(|message| {
            message.body == MessageBody::Command
                && message.text == "/compact"
                && message.status == MessageStatus::Sent
        }),
        "command record missing: {messages:?}"
    );
    assert!(
        messages.iter().any(|message| {
            message.body == MessageBody::Prompt
                && message.text == "go"
                && message.status == MessageStatus::Sent
        }),
        "prompt record missing: {messages:?}"
    );
}

/// A pending ask reserves the agent's next input, so a queued message defers at
/// the open gate rather than landing on top of the ask — it stays pending for a
/// later boundary, and nothing is pasted.
#[test]
fn queue_defers_delivery_under_a_pending_ask() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-qd", "feature-qd", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-qd",
            "worktree_branch": "feature-qd",
        }),
        pane_env,
    );
    push_pending_agent_ask(&env, "sess-qd");

    let trace_log = env.project_root.join("zellij-qd-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_QUEUE_SETTLE_MS", "0")
        .args(["queue", "@claude", "--", "go"])
        .output()
        .expect("queue add");
    assert!(
        out.status.success(),
        "queue add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_eq!(
        env.ledger().list_pending_messages().unwrap().len(),
        1,
        "a pending ask defers delivery; the message stays queued"
    );
    assert!(
        trace_lines(&trace_log)
            .iter()
            .all(|line| !is_paste(line, "go")),
        "nothing is pasted while the ask reserves input"
    );
}

/// `--force` mirrors `steer --force`: queue sends past a pending ask at an open
/// gate instead of parking.
#[test]
fn queue_force_delivers_past_a_pending_ask() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-qf", "feature-qf", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-qf",
            "worktree_branch": "feature-qf",
        }),
        pane_env,
    );
    push_pending_agent_ask(&env, "sess-qf");

    let trace_log = env.project_root.join("zellij-qf-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_QUEUE_SETTLE_MS", "0")
        .args(["queue", "@claude", "--force", "--", "go"])
        .output()
        .expect("queue add");
    assert!(
        out.status.success(),
        "queue add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        env.ledger().list_pending_messages().unwrap().is_empty(),
        "--force delivers past the pending ask inline"
    );
    assert_text_then_enter(&trace_log, "go");
}

/// `queue @claude --all -y` fans out to every claude in the room: one queued
/// message per agent, all carrying the same text.
#[test]
fn queue_fanout_two_agents() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    // Distinct panes so the two cards stay distinct; both are `running`, so the
    // gate is closed and each message simply stays pending.
    register_running_agent(&env, "sess-fan-a", "feature-fa", &[("ZELLIJ_PANE_ID", "5")]);
    register_running_agent(&env, "sess-fan-b", "feature-fb", &[("ZELLIJ_PANE_ID", "6")]);

    let out = env
        .rimz()
        .args(["queue", "@claude", "--all", "--yes", "--", "shared task"])
        .output()
        .expect("queue fanout");
    assert!(
        out.status.success(),
        "queue fanout failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let pending = env.ledger().list_pending_messages().expect("pending queue");
    assert_eq!(
        pending.len(),
        2,
        "one queued message per agent: {pending:?}"
    );
    assert!(
        pending.iter().all(|message| message.text == "shared task"),
        "every fan-out message carries the same text: {pending:?}"
    );
}

/// Without `--all`, a selector that matches several agents is an ambiguity that
/// names the `--all` opt-in rather than a silent broadcast.
#[test]
fn steer_multi_match_without_all_is_ambiguous() {
    let env = Env::new();
    register_running_agent(
        &env,
        "sess-amb-a",
        "feature-aa",
        &[("ZELLIJ_PANE_ID", "11")],
    );
    register_running_agent(
        &env,
        "sess-amb-b",
        "feature-ab",
        &[("ZELLIJ_PANE_ID", "12")],
    );

    let out = env
        .rimz()
        .args(["steer", "@claude", "--", "hello"])
        .output()
        .expect("steer ambiguous");
    assert!(
        !out.status.success(),
        "a multi-match must not broadcast without --all"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--all"),
        "the ambiguity names the --all opt-in: {stderr}"
    );
}

/// `steer @claude --all -y` broadcasts to every claude with a bound pane and
/// prints a summary naming the count.
#[test]
fn steer_fanout_summary() {
    let env = Env::new();
    register_running_agent(&env, "sess-fsa", "feature-fsa", &[("ZELLIJ_PANE_ID", "3")]);
    register_running_agent(&env, "sess-fsb", "feature-fsb", &[("ZELLIJ_PANE_ID", "4")]);

    let trace_log = env.project_root.join("zellij-fanout-trace.log");
    let interval = Duration::from_millis(1000);
    let started = Instant::now();
    let out = env
        .rimz()
        .env("RIMZ_MESSAGE_INTERVAL_MS", interval.as_millis().to_string())
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["steer", "@claude", "--all", "--yes", "--", "hello"])
        .output()
        .expect("steer fanout");
    let elapsed = started.elapsed();
    assert!(
        out.status.success(),
        "steer fanout failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("sent 2 agent(s)"),
        "summary names the count: {stdout}"
    );
    assert!(
        elapsed >= interval,
        "two-message fan-out should wait between delivered messages; elapsed {elapsed:?}"
    );
    let pasted = trace_lines(&trace_log)
        .into_iter()
        .filter(|line| is_paste_to_any_pane(line, "hello"))
        .count();
    assert_eq!(pasted, 2, "fan-out should paste once per live pane");
}

/// A skipped agent never aborts a broadcast: both targeted agents have a pane,
/// but one holds a pending ask, so it is skipped while the other still receives
/// the steer, and the command summarizes and succeeds rather than failing on the
/// first skip.
#[test]
fn steer_fanout_skips_blocked_and_steers_the_rest() {
    let env = Env::new();
    register_running_agent(
        &env,
        "sess-skip-a",
        "feature-ska",
        &[("ZELLIJ_PANE_ID", "7")],
    );
    // A second pane-bound card, blocked by a pending ask — it can only be skipped.
    register_running_agent(
        &env,
        "sess-skip-b",
        "feature-skb",
        &[("ZELLIJ_PANE_ID", "9")],
    );
    push_pending_agent_ask(&env, "sess-skip-b");

    let trace_log = env.project_root.join("zellij-skip-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["steer", "@claude", "--all", "--yes", "--", "go"])
        .output()
        .expect("steer partial skip");
    assert!(
        out.status.success(),
        "a skipped agent must not abort the broadcast: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("sent 1 agent(s)") && stdout.contains("pending ask"),
        "summary names the sent and skipped agents: {stdout}"
    );
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

/// A bracketed-paste `zellij action write --pane-id <pane> 27 91 50 48 48 126
/// <text bytes…> 27 91 50 48 49 126` line — the text wrapped in `ESC[200~` …
/// `ESC[201~` decimal byte markers, so a following Enter reads as submit.
fn is_paste(line: &str, text: &str) -> bool {
    let payload = text
        .bytes()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join("\t");
    line.ends_with(&format!(
        "\taction\twrite\t--pane-id\t{TRACE_PANE}\t27\t91\t50\t48\t48\t126\t{payload}\t27\t91\t50\t48\t49\t126"
    ))
}

fn is_paste_to_any_pane(line: &str, text: &str) -> bool {
    let payload = text
        .bytes()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join("\t");
    line.contains("\taction\twrite\t--pane-id\t")
        && line.ends_with(&format!(
            "\t27\t91\t50\t48\t48\t126\t{payload}\t27\t91\t50\t48\t49\t126"
        ))
}

/// A discrete `zellij action write --pane-id <pane> 13` line — Enter sent as its
/// own key event (`NamedKey::Enter` writes byte `13`), distinct from the paste.
fn is_enter_key(line: &str) -> bool {
    line.ends_with(&format!("\taction\twrite\t--pane-id\t{TRACE_PANE}\t13"))
}

/// A `zellij action write-chars --pane-id <pane> /compact` line — the compaction
/// slash command typed as raw keystrokes ahead of an auto-compacted message,
/// distinct from the bracketed paste a message rides.
fn is_compact_command(line: &str) -> bool {
    line.ends_with(&format!(
        "\taction\twrite-chars\t--pane-id\t{TRACE_PANE}\t/compact"
    ))
}

/// The shim recorded a bracketed paste of `text` followed by a discrete
/// `write 13` — both to `TRACE_PANE` — with no carriage return folded in.
fn assert_text_then_enter(trace_log: &Path, text: &str) {
    let raw = std::fs::read_to_string(trace_log).unwrap_or_default();
    assert!(
        !raw.contains('\r'),
        "no carriage return should be folded into the sent text; trace: {raw:?}"
    );
    let lines = trace_lines(trace_log);
    let text_at = lines.iter().position(|line| is_paste(line, text));
    let enter_at = lines.iter().position(|line| is_enter_key(line));
    assert!(
        text_at.is_some(),
        "expected a bracketed paste of `{text}`; trace: {lines:?}"
    );
    assert!(
        enter_at.is_some(),
        "expected Enter as a discrete `write 13`; trace: {lines:?}"
    );
    assert!(
        text_at < enter_at,
        "text must be pasted before Enter; trace: {lines:?}"
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

/// Seed a context sidecar so `--smart-compact` reads `used_pct` as the agent's
/// window fill — the same record the producer would fold from a live statusline.
fn seed_context_fill(env: &Env, agent_id: &str, used_pct: u8) {
    let mut context = rimz::ledger::agent_context::empty_context("claude", jiff::Timestamp::now());
    context.tokens = Some(rimz::agents::AgentTokenUsage {
        used_percentage: Some(used_pct),
        ..Default::default()
    });
    let record = rimz::ledger::agent_context::new_record("claude", agent_id, context);
    rimz::ledger::agent_context::write_record(&env.runtime_paths(), &record)
        .expect("seed context sidecar");
}

/// Seed a context sidecar with token composition so `occupied_context_tokens`
/// has the deterministic baseline smart-compact uses to suppress duplicates.
fn seed_context_tokens(env: &Env, agent_id: &str, used: u64, window: u64) {
    let mut context = rimz::ledger::agent_context::empty_context("claude", jiff::Timestamp::now());
    context.tokens = Some(rimz::agents::AgentTokenUsage {
        context_window_size: Some(window),
        current_usage: Some(rimz::agents::AgentCurrentUsage {
            input_tokens: Some(used),
            ..Default::default()
        }),
        ..Default::default()
    });
    let record = rimz::ledger::agent_context::new_record("claude", agent_id, context);
    rimz::ledger::agent_context::write_record(&env.runtime_paths(), &record)
        .expect("seed context sidecar");
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

fn append_lifecycle(
    env: &Env,
    kind: &str,
    event_name: &str,
    agent_id: &str,
    signal: LifecycleSignal,
    configure: impl FnOnce(&mut AgentLifecycleObservation),
) {
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    let mut observation =
        AgentLifecycleObservation::new(Some(AgentSessionId::from(agent_id)), signal);
    observation.worktree_path = Some(env.project_root.display().to_string());
    configure(&mut observation);
    let event = EventEnvelope::agent_lifecycle(
        workspace.workspace_id,
        workspace.session_name,
        kind,
        event_name,
        &observation,
    );
    env.ledger().append_event(&event).expect("append lifecycle");
}

fn trust_codex_hooks(env: &Env) {
    let config = env.agent_config_path("codex");
    let mut text = std::fs::read_to_string(&config).expect("read codex config");
    for token in [
        "session_start",
        "user_prompt_submit",
        "subagent_start",
        "subagent_stop",
        "stop",
        "permission_request",
        "pre_tool_use",
        "post_tool_use",
        "pre_compact",
        "post_compact",
    ] {
        text.push_str(&format!(
            "\n[hooks.state.\"{}:{token}:0:0\"]\ntrusted_hash = \"sha256:deadbeef\"\n",
            config.display(),
        ));
    }
    std::fs::write(&config, text).expect("write trust state");
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
    queued_id_from_stdout(&out.stdout)
}

fn queued_id_from_stdout(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    trimmed
        .strip_prefix("queued ")
        .and_then(|rest| rest.rsplit_once('('))
        .and_then(|(_, id)| id.strip_suffix(')'))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("expected `queued @target (msg_...)`, got `{trimmed}`"))
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

/// A live `codex` pane at the workspace root with no bound session — the
/// producer synthesizes its idle `○ codex` row, but it never enters the rollup.
/// Its raw id is `TRACE_PANE` so the steer shim assertion matches.
fn unbound_codex_pane(env: &Env) -> rimz::pane::PaneRef {
    agent_pane(env, "codex")
}

fn agent_pane(env: &Env, command: &str) -> rimz::pane::PaneRef {
    rimz::pane::PaneRef {
        pane_id: rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, TRACE_PANE),
        session_name: "rimz-test".to_owned(),
        view_id: Some("tab_1".to_owned()),
        view_kind: Some(rimz::ids::ViewKind::Tab),
        view_name: Some("project".to_owned()),
        is_focused: false,
        is_floating: false,
        command: Some(command.to_owned()),
        spawn_command: None,
        cwd: Some(env.project_root.display().to_string()),
        pane_pid: None,
        pane_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

fn seed_provisional_codex_launch(
    env: &Env,
    launch_id: &str,
    agent_name: &str,
    role: Option<&str>,
    stale_pane: &str,
) {
    seed_provisional_codex_launch_with_prompt(env, launch_id, agent_name, role, stale_pane, None);
}

fn seed_running_provisional_codex_launch(
    env: &Env,
    launch_id: &str,
    agent_name: &str,
    role: Option<&str>,
    stale_pane: &str,
) {
    seed_provisional_codex_launch_with_prompt(
        env,
        launch_id,
        agent_name,
        role,
        stale_pane,
        Some("work"),
    );
}

fn seed_provisional_codex_launch_with_prompt(
    env: &Env,
    launch_id: &str,
    agent_name: &str,
    role: Option<&str>,
    stale_pane: &str,
    prompt: Option<&str>,
) {
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    let kind = AgentKind::new_unchecked("codex");
    let event = EventEnvelope::agent_launched(
        workspace.workspace_id,
        workspace.session_name,
        &kind,
        AgentLaunchPayload {
            agent_id: AgentSessionId::from(launch_id),
            agent_name: agent_name.to_owned(),
            profile: None,
            role: role.map(ToOwned::to_owned),
            team: None,
            kind_ordinal: Some(1),
            state: AgentLaunchState::Starting,
            run_id: None,
            pane_id: Some(PaneId::from_parts(MuxName::Zellij, stale_pane)),
            runtime_owner: None,
            worktree_path: Some(env.project_root.display().to_string()),
            worktree_branch: None,
            prompt: prompt.map(ToOwned::to_owned),
            description: None,
        },
    );
    env.ledger().append_event(&event).expect("append launch");
}

/// `steer @codex` reaches a bare codex started in a pane before its first turn:
/// the resolver folds the live pane frame, finds the synthesized idle row, and
/// pastes into its pane — reproducing and fixing the `no agent matches @codex`
/// failure. The pane fixture stands in for the mux, and codex must be wired
/// (hooks installed) for the idle row to synthesize.
#[test]
fn steer_reaches_unbound_codex_pane() {
    let env = Env::new();
    env.install_agent_hooks("codex");
    let pane_fixture = env.write_pane_fixture(&[unbound_codex_pane(&env)]);

    let trace_log = env.project_root.join("zellij-unbound-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["steer", "@codex", "--", "continue"])
        .output()
        .expect("steer");
    assert!(
        out.status.success(),
        "steer to an unbound codex pane failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_text_then_enter(&trace_log, "continue");
}

/// `queue @codex` for the same unbound pane sends immediately like `steer`
/// and records the send against a pane-derived placeholder session.
#[test]
fn queue_sends_now_to_unbound_codex_pane() {
    let env = Env::new();
    env.install_agent_hooks("codex");
    let pane_fixture = env.write_pane_fixture(&[unbound_codex_pane(&env)]);

    let trace_log = env.project_root.join("zellij-unbound-queue-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["queue", "@codex", "--", "later"])
        .output()
        .expect("queue add");
    assert!(
        out.status.success(),
        "queue to an unbound pane should send now: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_text_then_enter(&trace_log, "later");
    let messages = env.ledger().list_messages().unwrap();
    assert_eq!(messages.len(), 1, "send-now queue writes a durable record");
    assert_eq!(messages[0].status, MessageStatus::Sent);
    let methods: Vec<String> = env
        .read_events()
        .into_iter()
        .map(|event| event.method)
        .collect();
    assert!(
        methods.iter().any(|method| method == "message.sent"),
        "send-now queue records message.sent: {methods:?}"
    );
    assert!(
        methods.iter().all(|method| method != "message.queued"),
        "send-now queue is not parked: {methods:?}"
    );
}

#[test]
fn queue_to_provisional_codex_sends_to_live_pane_not_stale_rollup_pane() {
    let env = Env::new();
    env.install_agent_hooks("codex");
    seed_provisional_codex_launch(
        &env,
        "launch_queue_bug",
        "swift-otter",
        Some("coder"),
        "terminal_8",
    );
    let pane_fixture = env.write_pane_fixture(&[unbound_codex_pane(&env)]);

    let trace_log = env.project_root.join("zellij-provisional-queue-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["queue", "@coder", "--", "read plan"])
        .output()
        .expect("queue add");
    assert!(
        out.status.success(),
        "queue to a provisional codex should send now: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_text_then_enter(&trace_log, "read plan");
    let messages = env.ledger().list_messages().unwrap();
    assert_eq!(
        messages.len(),
        1,
        "send-now provisional queue writes a durable record"
    );
    assert_eq!(messages[0].status, MessageStatus::Sent);
    let methods: Vec<String> = env
        .read_events()
        .into_iter()
        .map(|event| event.method)
        .collect();
    assert!(
        methods.iter().any(|method| method == "message.sent"),
        "send-now queue records message.sent: {methods:?}"
    );
    assert!(
        methods.iter().all(|method| method != "message.queued"),
        "send-now queue is not parked: {methods:?}"
    );
}
