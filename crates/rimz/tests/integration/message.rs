use serde_json::json;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use rimz::agents::{
    AgentLifecycleObservation, AgentRateLimits, AgentTurnError, AskKind, LaunchParams,
    LifecycleSignal, RateLimitWindow, TurnErrorClass,
};
use rimz::ids::{AgentKind, AgentSessionId, MessageId, MuxName, PaneId};
use rimz::message::{
    DeliveryGate, MessageBody, MessageRecord, MessageSender, MessageStatus, WhenCondition,
};
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope, MessageEventMethod};

use crate::common::Env;

#[test]
fn queue_remove_and_clear_workflow() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-queue", "feature-q", &[]);

    let first = queue_add(&env, "@claude", "first task");
    let second = queue_add(&env, "@claude", "second task");
    let third = queue_add(&env, "@claude", "third task");
    let missing = "msg_0000000000009999";
    let removed = env
        .rimz()
        .args(["message", "remove", &first, missing, &second])
        .output()
        .expect("mixed remove");
    assert!(!removed.status.success(), "missing ID reports failure");
    let stdout = String::from_utf8_lossy(&removed.stdout);
    assert!(stdout.contains(&format!("removed {first}")));
    assert!(stdout.contains(&format!("{missing} is not queued or claimed")));
    assert!(stdout.contains(&format!("removed {second}")));

    let cleared = env
        .rimz()
        .args(["message", "clear", "@claude"])
        .output()
        .expect("queue clear");
    assert!(
        cleared.status.success(),
        "queue clear failed: {}",
        String::from_utf8_lossy(&cleared.stderr)
    );
    let cleared_stdout = String::from_utf8_lossy(&cleared.stdout);
    assert!(cleared_stdout.contains("removed 1 message(s) for @claude"));
    assert!(cleared_stdout.contains(&third));
    assert!(env.store().list_pending_messages().unwrap().is_empty());

    let methods: Vec<String> = env
        .read_events()
        .into_iter()
        .map(|event| event.method)
        .filter(|method| method.starts_with("message."))
        .collect();
    assert!(methods.iter().any(|method| method == "message.queued"));
    assert!(methods.iter().any(|method| method == "message.removed"));
}

#[test]
fn message_list_scopes_orders_and_limits_records() {
    let env = Env::new();
    register_running_agent(&env, "sess-docs", "docs", &[]);
    let main = seed_channel_message(&env, 1, 100, None, "main task");
    let archived_docs = seed_channel_message(&env, 2, 200, Some("docs"), "old docs task");
    env.store()
        .archive_channel_messages("docs", "test archive", "rimz-test")
        .expect("archive docs channel");
    let docs = seed_channel_message(&env, 3, 300, Some("docs"), "docs task");
    let docs_forge = seed_channel_message(&env, 4, 400, Some("docs/forge"), "forge task");
    let ops = seed_channel_message(&env, 5, 500, Some("ops"), "ops task");

    assert_eq!(
        list_message_ids(&env, &["message", "list", "--json"], None),
        vec![main.clone()]
    );
    assert_eq!(
        list_message_ids(&env, &["message", "list", "--json"], Some("docs")),
        vec![docs.clone()]
    );
    assert_eq!(
        list_message_ids(
            &env,
            &["message", "list", "--status", "archived", "--json"],
            Some("docs")
        ),
        vec![archived_docs.clone()]
    );
    assert_eq!(
        list_message_ids(
            &env,
            &["message", "list", "--channel", "docs/forge", "--json"],
            None
        ),
        vec![docs_forge.clone()]
    );
    assert_eq!(
        list_message_ids(
            &env,
            &["message", "list", "--all", "--limit", "2", "--json"],
            None
        ),
        vec![ops.clone(), docs_forge.clone()]
    );
    assert_eq!(
        list_message_ids(
            &env,
            &["message", "list", "--all", "--limit", "0", "--json"],
            None
        ),
        vec![ops, docs_forge, docs, archived_docs, main]
    );

    let digest = run_success(env.rimz().args(["message"]), "bare message list");
    assert!(String::from_utf8_lossy(&digest.stdout).contains("→"));
}

#[test]
fn terminal_message_history_keeps_text_for_list_and_show() {
    let env = Env::new();
    register_running_agent(&env, "sess-history", "history", &[]);
    let message_id = queue_direct_channel_message(&env, "history", "kept body");
    env.store()
        .settle_message(
            &MessageId::parse(&message_id).expect("message id"),
            MessageStatus::Delivered,
            "rimz-test",
            None,
        )
        .expect("settle delivered");

    let listed = env
        .rimz()
        .args(["message", "list", "--all", "--json"])
        .output()
        .expect("message list");
    assert!(
        listed.status.success(),
        "message list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("json");
    let row = parsed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["message_id"] == message_id)
        .expect("history row");
    assert_eq!(row["status"], "delivered");
    assert_eq!(row["text"], "kept body");

    let shown = env
        .rimz()
        .args(["message", "show", &message_id])
        .output()
        .expect("message show");
    assert!(
        shown.status.success(),
        "message show failed: {}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(shown.contains("kept body"));
    assert!(shown.contains("TIMELINE"));
    assert!(shown.contains("\n  delivered  "));
    assert!(!shown.contains("message.delivered"));
    assert!(!shown.contains("attempts:"));
    assert!(!shown.contains("unconfirmed_sends:"));
    assert!(!shown.contains("last_error:"));
    assert_second_precision_created(&shown);
}

#[test]
fn message_show_keeps_channel_in_textless_transcript_hint() {
    let env = Env::new();
    register_running_agent(&env, "sess-textless", "docs", &[]);
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.parent_agent_id.is_none())
        .expect("agent");
    let mut message = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "pre-history body".to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_channel(Some("docs".to_owned()));
    message.status = MessageStatus::Delivered;
    message.delivered_at = Some(message.enqueued_at);
    let message_id = message.message_id.to_string();
    let event =
        EventEnvelope::message_event(&message, "rimz-test", MessageEventMethod::Delivered, None);
    env.store().append_event(&event).expect("append event");

    let shown = env
        .rimz()
        .args(["message", "show", &message_id])
        .output()
        .expect("message show");
    assert!(
        shown.status.success(),
        "message show failed: {}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.contains("(content in `rimz transcript @claude#docs`)"),
        "{shown}"
    );
}

#[test]
fn message_edit_updates_queued_record_and_rejects_empty_edit() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-edit", "edit-message", &[]);
    let queued = env
        .rimz()
        .args(["message", "--schedule", "60m", "@claude", "--", "old text"])
        .output()
        .expect("scheduled message");
    assert!(
        queued.status.success(),
        "queue failed: {}",
        String::from_utf8_lossy(&queued.stderr)
    );
    let message_id = queued_id_from_stdout(&queued.stdout);

    let edited = env
        .rimz()
        .args([
            "message",
            "edit",
            &message_id,
            "--text",
            "new text",
            "--no-schedule",
            "--on",
            "any",
        ])
        .output()
        .expect("message edit");
    assert!(
        edited.status.success(),
        "edit failed: {}",
        String::from_utf8_lossy(&edited.stderr)
    );
    let stdout = String::from_utf8_lossy(&edited.stdout);
    assert!(stdout.contains(&format!("edited {message_id}")));
    assert!(stdout.contains("text, gate, schedule"));

    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].text, "new text");
    assert_eq!(pending[0].gate, DeliveryGate::Any);
    assert_eq!(pending[0].not_before, None);

    let shown = env
        .rimz()
        .args(["message", "show", &message_id])
        .output()
        .expect("message show");
    assert!(
        shown.status.success(),
        "show failed: {}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(shown.contains("new text"));
    assert!(shown.contains("\n  edited  "));
    assert!(shown.contains("text, gate, schedule"));

    let edited = env
        .rimz()
        .args(["message", "edit", &message_id])
        .output()
        .expect("message edit");

    assert!(!edited.status.success());
    assert!(String::from_utf8_lossy(&edited.stderr).contains("nothing to edit"));
}

#[test]
fn clear_without_target_removes_scoped_channel_lane() {
    let env = Env::new();
    register_running_agent(&env, "sess-clear-lane", "docs", &[]);
    let docs = queue_direct_channel_message(&env, "docs", "docs");
    let docs_team = queue_direct_channel_message(&env, "docs/forge", "forge");
    let ops = queue_direct_channel_message(&env, "ops", "ops");

    let cleared = env
        .rimz()
        .env(rimz::harness::run::ENV_CHANNEL, "docs")
        .args(["message", "clear"])
        .output()
        .expect("clear lane");
    assert!(
        cleared.status.success(),
        "clear lane failed: {}",
        String::from_utf8_lossy(&cleared.stderr)
    );
    let stdout = String::from_utf8_lossy(&cleared.stdout);
    assert!(stdout.contains("removed 1 message(s) in #docs"));
    assert!(stdout.contains(&docs));
    // Lane membership is exact: the `docs/forge` team lane is its own scope.
    assert!(!stdout.contains(&docs_team));
    let pending = env.store().list_pending_messages().unwrap();
    let pending_ids: Vec<&str> = pending
        .iter()
        .map(|message| message.message_id.as_str())
        .collect();
    assert_eq!(pending_ids, vec![docs_team.as_str(), ops.as_str()]);
}

#[test]
fn receiver_end_archives_open_messages() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-ended", "docs", &[]);
    let message_id = queue_add_in_channel(&env, "docs", "@claude", "stale task");

    run_hook(
        &env,
        json!({
            "hook_event_name": "SessionEnd",
            "session_id": "sess-ended",
            "worktree_branch": "docs",
        }),
        &[],
    );

    assert!(env.store().list_messages().expect("messages").is_empty());
    let archived = env
        .read_events()
        .into_iter()
        .find(|event| event.method == "message.archived")
        .expect("archived event");
    let params = archived.params_value();
    assert_eq!(params["message_id"], message_id);
    assert_eq!(params["reason"], "receiver ended");
}

#[test]
fn watched_agent_end_archives_unmet_when_message() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_role_agent(&env, "claude", "sess-coder", "coder", false, None);
    register_role_agent(&env, "claude", "sess-planner", "planner", true, None);

    let queued = env
        .rimz()
        .args([
            "message",
            "@coder",
            "--when",
            "@planner running 2h",
            "check planner",
        ])
        .output()
        .expect("queue when message");
    assert!(
        queued.status.success(),
        "queue failed: {}",
        String::from_utf8_lossy(&queued.stderr)
    );
    let message_id = queued_id_from_stdout(&queued.stdout);

    run_hook(
        &env,
        json!({
            "hook_event_name": "SessionEnd",
            "session_id": "sess-planner",
            "worktree_branch": "feature-planner",
        }),
        &[],
    );

    assert!(env.store().list_messages().expect("messages").is_empty());
    let archived = env
        .store()
        .list_message_history()
        .expect("history")
        .into_iter()
        .find(|message| message.message_id.as_str() == message_id)
        .expect("archived when message");
    assert_eq!(archived.status, MessageStatus::Archived);
    let reason = archived.last_error.as_deref().expect("expiry reason");
    assert!(reason.starts_with("watched agent @planner"), "{reason}");
    assert!(
        reason.ends_with("ended before 'running 2h' was met"),
        "{reason}"
    );
}

#[test]
fn message_when_accepts_self_reference_and_stamps_met_dwell_at_enqueue() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_old_idle_role_agent(&env, "sess-coder", "coder", 120);

    let queued = env
        .rimz()
        .args(["message", "@coder", "--when", "@coder idle 1m", "ping"])
        .output()
        .expect("queue self when");
    assert!(
        queued.status.success(),
        "queue failed: {}",
        String::from_utf8_lossy(&queued.stderr)
    );
    let mut messages = env.store().list_messages().expect("messages");
    messages.extend(env.store().list_message_history().expect("history"));
    assert_eq!(
        messages.len(),
        1,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&queued.stdout),
        String::from_utf8_lossy(&queued.stderr)
    );
    assert_eq!(messages[0].when.len(), 1);
    assert!(messages[0].when[0].met_at.is_some());
}

#[test]
fn message_when_sweep_stamps_due_dwell_and_defers_to_future_trip() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let base = register_old_idle_role_agent(&env, "sess-coder", "coder", 120);
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-coder")
        .expect("coder");
    let condition = |dwell_secs| WhenCondition {
        kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
        agent_name: agent.name.clone(),
        address: "@coder".to_owned(),
        status: rimz::agents::AgentStatus::Idle,
        dwell_secs,
        met_at: None,
    };
    let due = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "due".to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_when(vec![condition(60)]);
    let future = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "future".to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_when(vec![condition(3_600)]);
    env.store().queue_message(&due, "rimz-test").unwrap();
    env.store().queue_message(&future, "rimz-test").unwrap();
    let shown = env
        .rimz()
        .args(["message", "show", future.message_id.as_str()])
        .output()
        .expect("show when blocker");
    assert!(shown.status.success());
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.contains("waiting for @coder idle ≥ 1h — idle"),
        "{shown}"
    );
    assert!(shown.contains("so far"), "{shown}");
    assert!(shown.contains("trips"), "{shown}");
    let listed = env
        .rimz()
        .args(["message", "list", "--all"])
        .output()
        .expect("list when blocker");
    assert!(listed.status.success());
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("when @coder idle 1h"),
        "{}",
        String::from_utf8_lossy(&listed.stdout)
    );
    let panes = env.write_pane_fixture(&[]);

    let sweep = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", panes)
        .args(["message", "sweep"])
        .output()
        .expect("when sweep");
    assert!(
        sweep.status.success(),
        "sweep failed: {}",
        String::from_utf8_lossy(&sweep.stderr)
    );

    let messages = env.store().list_messages().expect("messages");
    let due = messages
        .iter()
        .find(|message| message.message_id == due.message_id)
        .expect("due message");
    assert!(due.when[0].met_at.is_some());
    let future = messages
        .iter()
        .find(|message| message.message_id == future.message_id)
        .expect("future message");
    assert_eq!(future.when[0].met_at, None);
    assert_eq!(future.retry_after, Some(base + Duration::from_secs(3_600)));
    assert!(
        env.read_events()
            .iter()
            .any(|event| event.method == "message.when_met")
    );
}

#[test]
fn message_when_rejects_invalid_grammar_status_duration_and_broadcast() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_role_agent(&env, "claude", "sess-coder", "coder", false, None);
    register_role_agent(&env, "claude", "sess-reviewer", "reviewer", false, None);
    for (condition, expected) in [
        ("@coder idle", "use `@handle <status> <duration>`"),
        ("@coder paused 1m", "supported statuses"),
        ("@coder idle soon", "invalid --when duration"),
        ("@all idle 1m", "broadcasts are not supported"),
        ("@ghost idle 1m", "no agent matches"),
        ("@claude idle 1m", "exactly one agent"),
    ] {
        let out = env
            .rimz()
            .args(["message", "@coder", "--when", condition, "ping"])
            .output()
            .expect("invalid when");
        assert!(
            !out.status.success(),
            "condition unexpectedly passed: {condition}"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(expected),
            "condition={condition}\nstderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn message_when_rejects_wait_and_create() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_role_agent(&env, "claude", "sess-coder", "coder", false, None);

    let wait = env
        .rimz()
        .args([
            "message",
            "@coder",
            "--when",
            "@coder idle 1m",
            "--wait",
            "ping",
        ])
        .output()
        .expect("when wait conflict");
    assert!(!wait.status.success());
    assert!(
        String::from_utf8_lossy(&wait.stderr).contains("cannot be used with '--wait"),
        "{}",
        String::from_utf8_lossy(&wait.stderr)
    );

    let create = env
        .rimz()
        .args([
            "message",
            "@coder",
            "--when",
            "@coder idle 1m",
            "--create",
            "ping",
        ])
        .output()
        .expect("when create conflict");
    assert!(!create.status.success());
    assert!(
        String::from_utf8_lossy(&create.stderr).contains("--when needs an existing recipient"),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );
}

#[test]
fn scheduled_message_parks_with_not_before_and_wake_stamp() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-scheduled", "feature-scheduled", &[]);

    let before = jiff::Timestamp::now();
    let out = env
        .rimz()
        .args(["message", "--schedule", "60m", "@claude", "later"])
        .output()
        .expect("scheduled message");
    assert!(
        out.status.success(),
        "scheduled message failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    let not_before = pending[0].not_before.expect("scheduled timestamp");
    assert!(not_before > before);
    assert!(not_before <= before + jiff::SignedDuration::from_secs(61 * 60));

    let listed = env
        .rimz()
        .args(["message", "list", "--all", "--json"])
        .output()
        .expect("message list");
    assert!(
        listed.status.success(),
        "message list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("json");
    assert!(parsed[0]["not_before"].is_string());

    let wake: Option<jiff::Timestamp> =
        serde_json::from_slice(&std::fs::read(wake_stamp_path(&env)).expect("wake stamp"))
            .expect("wake stamp json");
    assert_eq!(wake, Some(not_before));
}

#[test]
fn message_after_rejects_self_reference_and_fanout() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_role_agent(
        &env,
        "claude",
        "sess-coder",
        "coder",
        false,
        Some("terminal_3"),
    );
    register_role_agent(&env, "claude", "sess-planner", "planner", true, None);

    let self_reference = env
        .rimz()
        .args(["message", "@coder", "--after", "@coder", "x"])
        .output()
        .expect("self reference");
    assert!(!self_reference.status.success());
    assert!(
        String::from_utf8_lossy(&self_reference.stderr).contains("use --on"),
        "stderr: {}",
        String::from_utf8_lossy(&self_reference.stderr)
    );

    let fanout = env
        .rimz()
        .args(["message", "@coder", "--after", "@all", "x"])
        .output()
        .expect("after fanout");
    assert!(!fanout.status.success());
    assert!(
        String::from_utf8_lossy(&fanout.stderr).contains("broadcasts are not supported"),
        "stderr: {}",
        String::from_utf8_lossy(&fanout.stderr)
    );
}

#[test]
fn message_after_sweep_completes_cross_agent_relay() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_role_agent(
        &env,
        "claude",
        "sess-coder",
        "coder",
        false,
        Some(TRACE_PANE),
    );
    register_role_agent(&env, "claude", "sess-planner", "planner", true, None);

    let add = env
        .rimz()
        .args(["message", "@coder", "--after", "@planner", "read plan.md"])
        .output()
        .expect("queue relay");
    assert!(
        add.status.success(),
        "queue relay failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let message_id = queued_id_from_stdout(&add.stdout);
    let pending = env.store().list_pending_messages().expect("pending relay");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].after.len(), 1);
    assert!(pending[0].after[0].address.starts_with("@planner"));
    assert_eq!(pending[0].after[0].met_at, None);

    append_lifecycle(
        &env,
        "claude",
        "Stop",
        "sess-planner",
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
        |_| {},
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let trace_log = env.project_root.join("zellij-after-sweep-trace.log");
    let sweep = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "sweep"])
        .output()
        .expect("sweep relay");
    assert!(
        sweep.status.success(),
        "sweep relay failed: {}",
        String::from_utf8_lossy(&sweep.stderr)
    );

    assert_text_then_enter(&trace_log, "read plan.md");
    let sent = env
        .store()
        .list_messages()
        .expect("messages")
        .into_iter()
        .find(|message| message.message_id.as_str() == message_id)
        .expect("sent relay");
    assert_eq!(sent.status, MessageStatus::Sent);
    assert!(sent.after[0].met_at.is_some());
    assert!(
        env.read_events()
            .iter()
            .any(|event| event.method == "message.after_met")
    );
}

#[test]
fn message_sweep_delivers_due_message_and_registers_reconcile_wake() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-sweep", "feature-sweep", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-sweep",
            "worktree_branch": "feature-sweep",
        }),
        pane_env,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-sweep")
        .expect("agent");
    let due = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1);
    let message = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "due now".to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_not_before(Some(due));
    let message_id = message.message_id.clone();
    env.store()
        .queue_message(&message, "rimz-test")
        .expect("queue due message");
    rimz::store::atomic::write_temp_then_rename_cache(&wake_stamp_path(&env), &Some(due))
        .expect("write wake stamp");

    let trace_log = env.project_root.join("zellij-sweep-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "sweep"])
        .output()
        .expect("message sweep");
    assert!(
        out.status.success(),
        "message sweep failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_text_then_enter(&trace_log, "due now");
    let sent = env
        .store()
        .list_messages()
        .expect("messages")
        .into_iter()
        .find(|message| message.message_id == message_id)
        .expect("swept message");
    assert_eq!(sent.status, MessageStatus::Sent);
    let wake: Option<jiff::Timestamp> =
        serde_json::from_slice(&std::fs::read(wake_stamp_path(&env)).expect("wake stamp"))
            .expect("wake stamp json");
    assert_eq!(
        wake,
        sent.sent_reconcile_deadline(Duration::from_secs(30)),
        "wake stamp tracks the sent reconcile deadline"
    );
}

#[test]
fn resume_gate_waits_for_recovery_then_delivers() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-resume-ready", "feature-resume", pane_env);
    seed_turn_error(&env, "sess-resume-ready", TurnErrorClass::PausedRateLimit);
    seed_rate_limit_budget(&env, 100);
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-resume-ready")
        .expect("agent");
    let message = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "continue".to_owned(),
        true,
        DeliveryGate::Resume,
    )
    .with_pane_id(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
    let message_id = message.message_id.clone();
    env.store()
        .queue_message(&message, "rimz-test")
        .expect("queue resume message");

    let trace_log = env.project_root.join("zellij-resume-ready-trace.log");
    run_success(
        env.rimz()
            .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "deliver", "--message-id", message_id.as_str()]),
        "deferred resume delivery",
    );
    assert!(trace_lines(&trace_log).is_empty());
    let queued = message_by_id(&env, &message_id);
    assert_eq!(queued.status, MessageStatus::Queued);
    assert_eq!(queued.attempts, 0);

    seed_rate_limit_budget(&env, 20);
    run_success(
        env.rimz()
            .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "deliver", "--message-id", message_id.as_str()]),
        "recovered resume delivery",
    );

    assert_text_then_enter(&trace_log, "continue");
    assert_eq!(message_by_id(&env, &message_id).status, MessageStatus::Sent);
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
        .args(["--mux", "zellij", "message", "@claude", "--", "cached path"])
        .output()
        .expect("message");
    assert!(
        out.status.success(),
        "message failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let trace = trace_lines(&trace_log);
    assert!(
        trace.is_empty(),
        "queue success path must not call zellij: {trace:?}"
    );
}

#[test]
fn message_add_does_not_resolve_reaped_dead_owner_agent() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "sess-audit-reviewer",
        LifecycleSignal::Registered,
        |observation| {
            observation.agent_name = Some("quiet-reviewer".to_owned());
            observation.launch.role = Some("reviewer".to_owned());
            observation.worktree_branch = Some("audit-work".to_owned());
            observation.runtime_owner = Some(rimz::pane::RuntimeOwner::new(
                rimz::pane::RuntimeOwnerKind::Agent,
                "sess-audit-reviewer",
                u32::MAX,
                Some("dead-process".to_owned()),
            ));
        },
    );
    assert!(
        env.store()
            .snapshot_cached()
            .expect("runtime snapshot")
            .agents
            .is_empty(),
        "runtime projection should expel the dead-owner agent"
    );
    assert!(
        env.store()
            .runtime_projection(rimz::RuntimeScope::Audit)
            .expect("audit projection")
            .agents
            .iter()
            .all(|agent| agent.agent_id != "sess-audit-reviewer"),
        "write-path reap should tombstone the dead-owner agent before audit fallback"
    );

    let out = env
        .rimz()
        .args(["message", "@reviewer", "--", "handoff"])
        .output()
        .expect("message");
    assert!(
        !out.status.success(),
        "dead-owner ghost should not queue through audit fallback\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let pending = env.store().list_pending_messages().expect("pending queue");
    assert!(pending.is_empty());
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
        .env("RIMZ_MESSAGE_SETTLE_MS", "0")
        .args(["message", "deliver", "--message-id", &message_id])
        .output()
        .expect("message deliver");
    assert!(
        out.status.success(),
        "message deliver failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let pending = env.store().list_pending_messages().expect("pending queue");
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
        .args(["message", "@claude", "--", "next task"])
        .output()
        .expect("message");
    assert!(!out.status.success(), "queue should fail without hooks");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("requires claude hooks"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn message_steer_queued_record_respects_waiting_force_and_overrides_gate() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-steer-queued", "steer-queued", pane_env);
    let message_id = queue_add(&env, "@claude", "push now");
    push_pending_agent_ask(&env, "sess-steer-queued");
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let trace_log = env.project_root.join("zellij-steer-queued-trace.log");

    let blocked = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["message", "steer", &message_id])
        .output()
        .expect("message steer");
    assert!(!blocked.status.success());
    let stderr = String::from_utf8_lossy(&blocked.stderr);
    assert!(stderr.contains("is waiting on your input") && stderr.contains("--force"));
    let queued = message_by_id(&env, &MessageId::parse(&message_id).expect("message id"));
    assert_eq!(queued.status, MessageStatus::Queued);
    assert_eq!(queued.attempts, 0);
    assert!(trace_lines(&trace_log).is_empty());

    let steered = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["message", "steer", &message_id, "--force"])
        .output()
        .expect("forced message steer");
    assert!(
        steered.status.success(),
        "steer failed: {}",
        String::from_utf8_lossy(&steered.stderr)
    );
    let stdout = String::from_utf8_lossy(&steered.stdout);
    assert!(stdout.contains(&format!("sent to @claude ({message_id})")));
    assert_text_then_enter(&trace_log, "push now");
    assert_eq!(
        message_by_id(&env, &MessageId::parse(&message_id).expect("message id")).status,
        MessageStatus::Sent
    );
}

#[test]
fn message_requeue_copies_terminal_record_and_refuses_open_record() {
    let env = Env::new();
    register_running_agent(&env, "sess-requeue", "requeue", &[]);
    let open_id = queue_direct_channel_message(&env, "requeue", "still open");

    let refused = env
        .rimz()
        .args(["message", "requeue", &open_id])
        .output()
        .expect("message requeue");
    assert!(!refused.status.success(), "open requeue should fail");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("still queued"),
        "unexpected stderr: {stderr}"
    );

    let message = env
        .store()
        .list_messages()
        .expect("messages")
        .into_iter()
        .find(|message| message.message_id.as_str() == open_id)
        .expect("open message");
    env.store()
        .record_send_error(&message, "test error", "rimz-test")
        .expect("record error");

    let requeued = env
        .rimz()
        .args([
            "message",
            "requeue",
            &open_id,
            "--text",
            "try again",
            "--on",
            "any",
        ])
        .output()
        .expect("message requeue");
    assert!(
        requeued.status.success(),
        "requeue failed: {}",
        String::from_utf8_lossy(&requeued.stderr)
    );
    let stdout = String::from_utf8_lossy(&requeued.stdout);
    assert!(stdout.contains(&format!("(from {open_id})")));
    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_ne!(pending[0].message_id.as_str(), open_id);
    assert_eq!(pending[0].text, "try again");
    assert_eq!(pending[0].gate, DeliveryGate::Any);
}

#[test]
fn steer_queues_when_durable_agent_has_no_live_pane() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "sess-steer-audit",
        LifecycleSignal::Registered,
        |observation| {
            observation.agent_name = Some("steady-reviewer".to_owned());
            observation.launch.role = Some("reviewer".to_owned());
            observation.worktree_branch = Some("audit-steer".to_owned());
        },
    );

    let out = env
        .rimz()
        .args(["message", "--steer", "@reviewer", "--", "please review"])
        .output()
        .expect("steer");
    assert!(
        out.status.success(),
        "steer should queue through durable fallback: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let message_id = queued_id_from_stdout(&out.stdout);
    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message_id.as_str(), message_id);
    assert_eq!(pending[0].agent_id.as_str(), "sess-steer-audit");
}

/// `steer` bracket-pastes the text and then presses Enter as a discrete key
/// event outside the paste — never a carriage return folded into the typed
/// text. Agent UIs submit on the keystroke but take an embedded newline as a
/// composer line break, so the distinction is the whole feature. Drives a real
/// `rimz message --steer` against the zellij-trace shim and asserts the recorded action
/// sequence: a bracketed paste of the text, then a discrete `write 13` (Enter),
/// with no `\r` anywhere.
#[test]
fn steer_enter_modes_respect_discrete_submit_key() {
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
        .args(["message", "--steer", "@claude", "--", "y"])
        .output()
        .expect("steer");
    assert!(
        out.status.success(),
        "steer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_text_then_enter(&trace_log, "y");

    // `--no-enter` types the text and stops — no Enter keystroke at all.
    let trace_log = env.project_root.join("zellij-steer-quiet-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "--steer", "@claude", "--no-enter", "--", "y"])
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
fn agent_wait_refuses_existing_reply_wait_cycle_before_enqueue() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_role_agent(&env, "claude", "sess-coder", "coder", true, None);
    register_role_agent(&env, "claude", "sess-reviewer", "reviewer", true, None);
    let snapshot = env.store().snapshot().expect("snapshot");
    let coder = snapshot
        .agents
        .iter()
        .find(|agent| agent.role.as_deref() == Some("coder"))
        .expect("coder card");
    let existing = MessageRecord::new(
        snapshot.workspace_id.clone(),
        coder,
        "answer the review".to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_address(Some("@coder".to_owned()))
    .with_sender(MessageSender::Agent {
        kind: AgentKind::new_unchecked("claude"),
        name: Some("reviewer-agent".to_owned()),
        profile: None,
        role: Some("reviewer".to_owned()),
        channel: None,
    })
    .with_reply_wait(true);
    let existing_id = existing.message_id.clone();
    env.store()
        .queue_message(&existing, "rimz-test")
        .expect("seed reviewer wait");

    let out = env
        .rimz()
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_NAME", "coder-agent")
        .args([
            "message",
            "@reviewer",
            "--no-from",
            "--wait=1s",
            "counter-question",
        ])
        .output()
        .expect("cycle guard");

    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--wait would deadlock")
            && stderr.contains("@reviewer")
            && stderr.contains(existing_id.as_str()),
        "stderr names the blocking wait: {stderr}"
    );
    let messages = env.store().list_messages().expect("messages");
    assert_eq!(messages.len(), 1, "the counter-wait was not enqueued");
    assert_eq!(messages[0].message_id, existing_id);
}

#[test]
fn steer_wait_times_out_without_turn_started_ack() {
    let env = Env::new();
    env.install_agent_hooks("claude");
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
        .args(["message", "--steer", "@claude", "--wait=0s", "--", "y"])
        .output()
        .expect("steer --wait");

    assert!(
        !out.status.success(),
        "--wait should exit nonzero on timeout"
    );
    assert_eq!(out.status.code(), Some(124));
    assert!(
        out.stdout.is_empty(),
        "timeout keeps stdout reserved for a final reply: {}",
        String::from_utf8_lossy(&out.stdout),
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("wait timed out"),
        "stderr reports timeout: {}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(env.store().list_messages().unwrap().is_empty());
    assert!(
        env.read_events()
            .iter()
            .any(|event| event.method == "message.timed_out"),
        "wait timeout records a terminal event"
    );
}

#[test]
fn message_wait_prints_the_reply_after_the_turn_ends() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let transcript = env.runtime_root.join("message-wait-reply.jsonl");
    std::fs::write(
        &transcript,
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"old answer\"}]}}\n",
    )
    .expect("seed transcript");
    register_idle_agent_with_transcript(
        &env,
        "sess-wait-reply",
        "feature-wait-reply",
        &transcript,
        &[("ZELLIJ_PANE_ID", "3")],
    );

    let trace_log = env.project_root.join("zellij-wait-reply-trace.log");
    let child = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "@claude", "--wait", "did it land?"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn message --wait");

    wait_for_message_event(&env, "message.sent", Duration::from_secs(2));
    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-wait-reply",
            "prompt": "did it land?",
            "worktree_branch": "feature-wait-reply",
            "transcript_path": transcript.to_string_lossy(),
        }),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    append_claude_assistant(&transcript, "migration landed");
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-wait-reply",
            "last_assistant_message": "migration landed",
            "worktree_branch": "feature-wait-reply",
            "transcript_path": transcript.to_string_lossy(),
        }),
        &[("ZELLIJ_PANE_ID", "3")],
    );

    let out = child.wait_with_output().expect("wait message --wait");
    assert!(
        out.status.success(),
        "--wait should succeed after the reply turn: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "migration landed\n");
    assert!(
        out.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        env.store().list_messages().unwrap().is_empty(),
        "delivered record self-cleans while wait still succeeds"
    );
}

#[test]
fn message_wait_gathers_fanout_replies_in_completion_order() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let first = env.runtime_root.join("message-wait-gather-first.jsonl");
    let second = env.runtime_root.join("message-wait-gather-second.jsonl");
    std::fs::write(&first, "").expect("seed first transcript");
    std::fs::write(&second, "").expect("seed second transcript");
    register_idle_agent_with_transcript(
        &env,
        "sess-wait-gather-first",
        "feature-gather-first",
        &first,
        &[("ZELLIJ_PANE_ID", "3")],
    );
    register_idle_agent_with_transcript(
        &env,
        "sess-wait-gather-second",
        "feature-gather-second",
        &second,
        &[("ZELLIJ_PANE_ID", "4")],
    );

    let trace_log = env.project_root.join("zellij-wait-gather-trace.log");
    let child = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "@all", "--wait=5s", "status?"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fanout wait");
    wait_for_message_event_count(&env, "message.sent", 2, Duration::from_secs(2));
    begin_wait_reply(
        &env,
        "sess-wait-gather-first",
        "feature-gather-first",
        &first,
        "3",
    );
    begin_wait_reply(
        &env,
        "sess-wait-gather-second",
        "feature-gather-second",
        &second,
        "4",
    );
    finish_wait_reply(
        &env,
        "sess-wait-gather-second",
        "feature-gather-second",
        &second,
        "4",
        "second finished",
        false,
    );
    std::thread::sleep(Duration::from_millis(600));
    finish_wait_reply(
        &env,
        "sess-wait-gather-first",
        "feature-gather-first",
        &first,
        "3",
        "first finished",
        false,
    );

    let out = child.wait_with_output().expect("wait fanout gather");
    assert!(
        out.status.success(),
        "fanout wait failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "@claude#feature-gather-second:\nsecond finished\n\n@claude#feature-gather-first:\nfirst finished\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn message_wait_json_emits_one_fanout_map() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let first = env.runtime_root.join("message-wait-json-first.jsonl");
    let second = env.runtime_root.join("message-wait-json-second.jsonl");
    std::fs::write(&first, "").expect("seed first transcript");
    std::fs::write(&second, "").expect("seed second transcript");
    register_idle_agent_with_transcript(
        &env,
        "sess-wait-json-first",
        "feature-json-first",
        &first,
        &[("ZELLIJ_PANE_ID", "3")],
    );
    register_idle_agent_with_transcript(
        &env,
        "sess-wait-json-second",
        "feature-json-second",
        &second,
        &[("ZELLIJ_PANE_ID", "4")],
    );

    let trace_log = env.project_root.join("zellij-wait-json-trace.log");
    let child = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "@all", "--wait=5s", "--json", "status?"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn JSON fanout wait");
    wait_for_message_event_count(&env, "message.sent", 2, Duration::from_secs(2));
    begin_wait_reply(
        &env,
        "sess-wait-json-first",
        "feature-json-first",
        &first,
        "3",
    );
    begin_wait_reply(
        &env,
        "sess-wait-json-second",
        "feature-json-second",
        &second,
        "4",
    );
    finish_wait_reply(
        &env,
        "sess-wait-json-first",
        "feature-json-first",
        &first,
        "3",
        "first JSON reply",
        false,
    );
    finish_wait_reply(
        &env,
        "sess-wait-json-second",
        "feature-json-second",
        &second,
        "4",
        "second JSON reply",
        false,
    );

    let out = child.wait_with_output().expect("wait JSON fanout gather");
    assert!(
        out.status.success(),
        "JSON fanout wait failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let replies: serde_json::Value = serde_json::from_slice(&out.stdout).expect("reply JSON");
    assert_eq!(replies.as_object().unwrap().len(), 2);
    for (label, reply) in [
        ("@claude#feature-json-first", "first JSON reply"),
        ("@claude#feature-json-second", "second JSON reply"),
    ] {
        assert_eq!(replies[label]["status"], "completed");
        assert_eq!(replies[label]["reply"], reply);
        assert!(
            replies[label]["message_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("msg_"))
        );
        assert!(replies[label].get("error").is_none());
    }
    assert!(out.stderr.is_empty());
}

#[test]
fn message_wait_gathers_other_replies_after_one_leg_fails() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let failed = env.runtime_root.join("message-wait-partial-failed.jsonl");
    let completed = env
        .runtime_root
        .join("message-wait-partial-completed.jsonl");
    std::fs::write(&failed, "").expect("seed failed transcript");
    std::fs::write(&completed, "").expect("seed completed transcript");
    register_idle_agent_with_transcript(
        &env,
        "sess-wait-partial-failed",
        "feature-partial-failed",
        &failed,
        &[("ZELLIJ_PANE_ID", "3")],
    );
    register_idle_agent_with_transcript(
        &env,
        "sess-wait-partial-completed",
        "feature-partial-completed",
        &completed,
        &[("ZELLIJ_PANE_ID", "4")],
    );

    let trace_log = env.project_root.join("zellij-wait-partial-trace.log");
    let child = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "@all", "--wait=5s", "try it"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn partial fanout wait");
    wait_for_message_event_count(&env, "message.sent", 2, Duration::from_secs(2));
    begin_wait_reply(
        &env,
        "sess-wait-partial-failed",
        "feature-partial-failed",
        &failed,
        "3",
    );
    begin_wait_reply(
        &env,
        "sess-wait-partial-completed",
        "feature-partial-completed",
        &completed,
        "4",
    );
    finish_wait_reply(
        &env,
        "sess-wait-partial-failed",
        "feature-partial-failed",
        &failed,
        "3",
        "partial answer",
        true,
    );
    std::thread::sleep(Duration::from_millis(600));
    finish_wait_reply(
        &env,
        "sess-wait-partial-completed",
        "feature-partial-completed",
        &completed,
        "4",
        "surviving reply",
        false,
    );

    let out = child
        .wait_with_output()
        .expect("wait partial fanout gather");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "@claude#feature-partial-completed:\nsurviving reply\n"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("rimz: @claude#feature-partial-failed turn failed (exit 1)"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn message_wait_any_returns_only_the_first_terminal_leg() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let first = env.runtime_root.join("message-wait-any-first.jsonl");
    let second = env.runtime_root.join("message-wait-any-second.jsonl");
    std::fs::write(&first, "").expect("seed first transcript");
    std::fs::write(&second, "").expect("seed second transcript");
    register_idle_agent_with_transcript(
        &env,
        "sess-wait-any-first",
        "feature-any-first",
        &first,
        &[("ZELLIJ_PANE_ID", "3")],
    );
    register_idle_agent_with_transcript(
        &env,
        "sess-wait-any-second",
        "feature-any-second",
        &second,
        &[("ZELLIJ_PANE_ID", "4")],
    );

    let trace_log = env.project_root.join("zellij-wait-any-trace.log");
    let child = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "@all", "--wait=5s", "--any", "first?"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn any fanout wait");
    wait_for_message_event_count(&env, "message.sent", 2, Duration::from_secs(2));
    begin_wait_reply(
        &env,
        "sess-wait-any-first",
        "feature-any-first",
        &first,
        "3",
    );
    begin_wait_reply(
        &env,
        "sess-wait-any-second",
        "feature-any-second",
        &second,
        "4",
    );
    finish_wait_reply(
        &env,
        "sess-wait-any-second",
        "feature-any-second",
        &second,
        "4",
        "winner",
        false,
    );

    let out = child.wait_with_output().expect("wait any fanout");
    assert!(
        out.status.success(),
        "any wait failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "@claude#feature-any-second:\nwinner\n"
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn message_wait_json_classifies_every_unfinished_fanout_leg_on_deadline() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let first = env.runtime_root.join("message-wait-timeout-first.jsonl");
    let second = env.runtime_root.join("message-wait-timeout-second.jsonl");
    std::fs::write(&first, "").expect("seed first transcript");
    std::fs::write(&second, "").expect("seed second transcript");
    register_idle_agent_with_transcript(
        &env,
        "sess-wait-timeout-first",
        "feature-timeout-first",
        &first,
        &[("ZELLIJ_PANE_ID", "3")],
    );
    register_idle_agent_with_transcript(
        &env,
        "sess-wait-timeout-second",
        "feature-timeout-second",
        &second,
        &[("ZELLIJ_PANE_ID", "4")],
    );

    let trace_log = env.project_root.join("zellij-wait-timeout-fanout.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "@all", "--wait=0s", "--json", "status?"])
        .output()
        .expect("fanout wait deadline");

    assert_eq!(out.status.code(), Some(124));
    let replies: serde_json::Value = serde_json::from_slice(&out.stdout).expect("reply JSON");
    assert_eq!(replies.as_object().unwrap().len(), 2);
    for label in [
        "@claude#feature-timeout-first",
        "@claude#feature-timeout-second",
    ] {
        assert_eq!(replies[label]["status"], "timed_out");
        assert!(replies[label]["reply"].is_null());
    }
    assert!(out.stderr.is_empty());
    assert_eq!(
        env.read_events()
            .iter()
            .filter(|event| event.method == "message.timed_out")
            .count(),
        2
    );
}

#[test]
fn message_wait_timeout_marks_only_sent_leg_and_keeps_queued_leg() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let sent_transcript = env.runtime_root.join("message-wait-mixed-sent.jsonl");
    let queued_transcript = env.runtime_root.join("message-wait-mixed-queued.jsonl");
    std::fs::write(&sent_transcript, "").expect("seed sent transcript");
    std::fs::write(&queued_transcript, "").expect("seed queued transcript");
    register_idle_agent_with_transcript(
        &env,
        "sess-wait-mixed-sent",
        "feature-mixed-sent",
        &sent_transcript,
        &[("ZELLIJ_PANE_ID", "3")],
    );
    register_idle_agent_with_transcript(
        &env,
        "sess-wait-mixed-queued",
        "feature-mixed-queued",
        &queued_transcript,
        &[("ZELLIJ_PANE_ID", "4")],
    );
    push_pending_agent_ask(&env, "sess-wait-mixed-queued");

    let trace_log = env.project_root.join("zellij-wait-mixed-timeout.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "@all", "--wait=0s", "--json", "status?"])
        .output()
        .expect("mixed-state fanout wait deadline");

    assert_eq!(out.status.code(), Some(124));
    let replies: serde_json::Value = serde_json::from_slice(&out.stdout).expect("reply JSON");
    let sent_label = "@claude#feature-mixed-sent";
    let queued_label = "@claude#feature-mixed-queued";
    assert_eq!(replies[sent_label]["status"], "timed_out");
    assert_eq!(replies[queued_label]["status"], "timed_out");

    let timed_out = env
        .read_events()
        .into_iter()
        .filter(|event| event.method == "message.timed_out")
        .collect::<Vec<_>>();
    assert_eq!(timed_out.len(), 1, "only sent durable record times out");
    assert_eq!(
        timed_out[0].params_value()["message_id"],
        replies[sent_label]["message_id"]
    );
    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1, "queued reply leg remains deliverable");
    assert_eq!(pending[0].status, MessageStatus::Queued);
    assert_eq!(
        pending[0].message_id.as_str(),
        replies[queued_label]["message_id"]
    );
}

#[test]
fn message_wait_rejects_invalid_modes_pane_targets_and_missing_hooks() {
    let env = Env::new();
    for args in [
        vec!["message", "@claude", "--create", "--wait=1s", "x"],
        vec!["message", "@claude", "--schedule", "1m", "--wait=1s", "x"],
        vec!["message", "@claude", "--steer", "--wait", "--no-enter", "x"],
        vec!["message", "@claude", "--json", "x"],
        vec!["message", "@claude", "--any", "x"],
    ] {
        let out = env.rimz().args(args).output().expect("wait conflict");
        assert!(!out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--wait"),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "bash")]);
    let pane = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["message", "zellij:terminal_3", "--wait=1s", "x"])
        .output()
        .expect("pane wait");
    assert!(!pane.status.success());
    assert!(
        String::from_utf8_lossy(&pane.stderr).contains("not bound to a known agent"),
        "stderr: {}",
        String::from_utf8_lossy(&pane.stderr)
    );

    register_running_agent(
        &env,
        "sess-wait-no-hooks",
        "feature-wait-no-hooks",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let missing = env
        .rimz()
        .args(["message", "@claude", "--wait=1s", "x"])
        .output()
        .expect("hooks missing wait");
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("rimz hooks install claude"),
        "stderr: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
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
        .args(["message", "--steer", "@claude", "--", "first\\nsecond"])
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
            "message",
            "--steer",
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

/// Piped stdin follows an inline instruction inside explicit tags, preserving
/// real newlines without applying inline escape interpretation.
#[test]
fn steer_combines_inline_text_with_piped_stdin() {
    let env = Env::new();
    register_running_agent(
        &env,
        "sess-stdin",
        "feature-stdin",
        &[("ZELLIJ_PANE_ID", "3")],
    );

    let trace_log = env.project_root.join("zellij-stdin-trace.log");
    let mut cmd = env.rimz();
    cmd.env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "--steer", "@claude", "--stdin", "review this"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = env
        .spawn_payload(cmd, "diff --git a/file b/file\n-old\n+new\n")
        .wait_with_output()
        .expect("wait for piped message");
    assert!(
        out.status.success(),
        "piped steer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_text_then_enter(
        &trace_log,
        "review this\n\n<stdin>\ndiff --git a/file b/file\n-old\n+new\n</stdin>",
    );
}

#[test]
fn message_ignores_an_open_empty_stdin_without_the_flag() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-open-stdin", "feature-open-stdin", &[]);

    let mut cmd = env.rimz();
    cmd.args(["message", "@claude", "hi"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn message with open stdin");
    let open_stdin = child.stdin.take().expect("child stdin");
    let out = wait_with_output_bounded(child, Duration::from_secs(2));
    drop(open_stdin);

    assert!(
        out.status.success(),
        "message failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("waiting on piped stdin"),
        "old wait notice survived: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].text, "hi");
}

#[cfg(unix)]
#[test]
fn message_warns_when_ignoring_buffered_stdin() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-buffered-stdin", "feature-buffered-stdin", &[]);
    let (read_end, write_end) = nix::unistd::pipe().expect("pipe");
    nix::unistd::write(&write_end, b"ignored context\n").expect("prefill pipe");

    let mut cmd = env.rimz();
    cmd.args(["message", "@claude", "hi"])
        .stdin(Stdio::from(read_end))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn message with buffered stdin");
    let out = wait_with_output_bounded(child, Duration::from_secs(2));
    drop(write_end);

    assert!(
        out.status.success(),
        "message failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("stdin has data but --stdin was not passed; ignoring it"),
        "missing ignored-stdin hint: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].text, "hi");
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
        .args(["message", "--steer", "@claude", "--", "ping"])
        .output()
        .expect("steer from agent");
    assert!(
        out.status.success(),
        "steer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_single_sigil_sent(&out.stdout);
    assert_text_then_enter(&trace_log, "from @codex: ping");
    let sent = env
        .read_events()
        .into_iter()
        .find(|event| event.method == "message.sent")
        .expect("sent event");
    let params = sent.params_value();
    assert_eq!(params["sender"]["origin"], "agent");
    assert_eq!(params["sender"]["kind"], "codex");
    assert_eq!(params["sender"]["name"], "swift-otter");
    assert_eq!(params["text_len"], "ping".len());
    assert_eq!(params["status"], "sent");

    let trace_log = env.project_root.join("zellij-from-steer-no-from-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_AGENT_KIND", "codex")
        .env("RIMZ_AGENT_NAME", "swift-otter")
        .args(["message", "--steer", "@claude", "--no-from", "--", "exact"])
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
fn queue_send_now_persists_and_sends() {
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
        .env("RIMZ_MESSAGE_SETTLE_MS", "0")
        .args(["message", "@claude", "--", "go"])
        .output()
        .expect("message");
    assert!(
        out.status.success(),
        "message failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_single_sigil_sent(&out.stdout);

    assert!(
        env.store().list_pending_messages().unwrap().is_empty(),
        "an idle agent with a bound pane should receive queue text immediately"
    );
    assert_text_then_enter(&trace_log, "go");
    let messages = env.store().list_messages().unwrap();
    assert_eq!(messages.len(), 1, "send-now queue writes a durable record");
    assert_eq!(messages[0].status, MessageStatus::Sent);
}

#[test]
fn sweep_requeues_unconfirmed_send_now_message_and_redelivers() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-reconcile", "feature-reconcile", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-reconcile",
            "worktree_branch": "feature-reconcile",
        }),
        pane_env,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);

    let first_trace = env.project_root.join("zellij-reconcile-first-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &first_trace)
        .args(["message", "@claude", "--", "recover me"])
        .output()
        .expect("send-now message");
    assert!(
        out.status.success(),
        "send-now failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let message_id = sent_id_from_stdout(&out.stdout);
    assert_text_then_enter(&first_trace, "recover me");

    let second_trace = env.project_root.join("zellij-reconcile-second-trace.log");
    let sweep = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &second_trace)
        .env("RIMZ_MESSAGE_DELIVERY_WINDOW_MS", "0")
        .args(["message", "sweep"])
        .output()
        .expect("message sweep");
    assert!(
        sweep.status.success(),
        "message sweep failed: {}",
        String::from_utf8_lossy(&sweep.stderr)
    );
    assert_text_then_enter(&second_trace, "recover me");

    let sent = env
        .store()
        .list_messages()
        .expect("messages")
        .into_iter()
        .find(|message| message.message_id.as_str() == message_id)
        .expect("redelivered message");
    assert_eq!(sent.status, MessageStatus::Sent);
    assert_eq!(sent.attempts, 1, "redelivery claim counted attempts");
    assert_eq!(sent.unconfirmed_sends, 1);

    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-reconcile",
            "prompt": "recover me",
            "worktree_branch": "feature-reconcile",
        }),
        pane_env,
    );
    assert!(
        env.store()
            .list_messages()
            .expect("messages")
            .into_iter()
            .all(|message| message.message_id.as_str() != message_id),
        "delivered message self-cleans from the live queue"
    );
    assert!(
        env.read_events()
            .iter()
            .any(|event| event.method == "message.delivered"),
        "delivery confirmation records a terminal event"
    );
}

#[test]
fn send_now_write_failure_leaves_queued_record_for_sweep_retry() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-send-fail", "feature-send-fail", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-send-fail",
            "worktree_branch": "feature-send-fail",
        }),
        pane_env,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);

    let failing_trace = env.project_root.join("zellij-send-fail-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &failing_trace)
        .env("RIMZ_TEST_ZELLIJ_MODE", "fail-write")
        .args(["message", "@claude", "--", "retry me"])
        .output()
        .expect("send-now message");
    assert!(
        out.status.success(),
        "send failure should queue, not fail\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let message_id = queued_id_from_stdout(&out.stdout);
    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message_id.as_str(), message_id);
    assert_eq!(pending[0].status, MessageStatus::Queued);
    assert!(pending[0].last_error.is_some(), "send error is recorded");
    assert_eq!(pending[0].pane_id, None, "retry re-resolves a fresh pane");

    let retry_trace = env.project_root.join("zellij-send-retry-trace.log");
    let sweep = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &retry_trace)
        .env("RIMZ_MESSAGE_DELIVERY_WINDOW_MS", "0")
        .args(["message", "sweep"])
        .output()
        .expect("message sweep");
    assert!(
        sweep.status.success(),
        "message sweep failed: {}",
        String::from_utf8_lossy(&sweep.stderr)
    );
    assert_text_then_enter(&retry_trace, "retry me");
    let sent = env
        .store()
        .list_messages()
        .expect("messages")
        .into_iter()
        .find(|message| message.message_id.as_str() == message_id)
        .expect("retried message");
    assert_eq!(sent.status, MessageStatus::Sent);
    assert_eq!(sent.attempts, 1);
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
        .args(["message", "@claude", "--", "later"])
        .output()
        .expect("message");
    assert!(
        add.status.success(),
        "message failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let message_id = queued_id_from_stdout(&add.stdout);
    let pending = env.store().list_pending_messages().expect("pending queue");
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
        .env("RIMZ_MESSAGE_SETTLE_MS", "0")
        .args(["message", "deliver", "--message-id", &message_id])
        .output()
        .expect("message deliver");
    assert!(
        out.status.success(),
        "message deliver failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_text_then_enter(&trace_log, "later");
    let messages = env.store().list_messages().expect("messages");
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
    assert!(
        env.store()
            .list_messages()
            .expect("messages")
            .into_iter()
            .all(|message| message.message_id.as_str() != message_id),
        "delivered message self-cleans from the live queue"
    );
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
    seed_provisional_codex_launch(
        &env,
        "launch_deferred_fold",
        "swift-otter",
        Some("coder"),
        "terminal_8",
        Some("work"),
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "codex")]);

    let add = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["message", "@coder", "--", "read plan"])
        .output()
        .expect("message");
    assert!(
        add.status.success(),
        "message failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let message_id = queued_id_from_stdout(&add.stdout);
    let pending = env.store().list_pending_messages().expect("pending queue");
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
            observation.launch.role = Some("coder".to_owned());
            observation.launch.kind_ordinal = Some(1);
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
        .env("RIMZ_MESSAGE_SETTLE_MS", "0")
        .args(["message", "deliver", "--message-id", &message_id])
        .output()
        .expect("message deliver");
    assert!(
        out.status.success(),
        "message deliver failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert_text_then_enter(&trace_log, "read plan");
    let messages = env.store().list_messages().expect("messages");
    let message = messages
        .iter()
        .find(|message| message.message_id.as_str() == message_id)
        .expect("sent message");
    assert_eq!(message.status, MessageStatus::Sent);
    let agents = env.store().snapshot_cached().expect("snapshot").agents;
    assert!(
        agents.iter().any(|agent| {
            agent.agent_id.as_str() == "codex-real-session"
                && agent.name.as_deref() == Some("swift-otter")
        }),
        "registered card should consume the provisional name: {agents:?}"
    );
}

#[test]
fn queued_delivery_batches_compatible_prompts() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-batch", "feature-batch", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-batch",
            "worktree_branch": "feature-batch",
        }),
        pane_env,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-batch")
        .expect("agent");
    let message = |id: u64, text: &str, role: &str, sender_channel: &str| {
        let mut record = MessageRecord::new(
            env.workspace_id.clone(),
            agent,
            text.to_owned(),
            true,
            DeliveryGate::Done,
        )
        .with_channel(Some("feature-batch".to_owned()))
        .with_sender(MessageSender::Agent {
            kind: AgentKind::new_unchecked("codex"),
            name: None,
            profile: None,
            role: Some(role.to_owned()),
            channel: Some(sender_channel.to_owned()),
        });
        record.message_id = fixed_message_id(id);
        record
    };
    let first = message(1, "first", "planner", "feature-batch");
    let second = message(2, "second", "coder", "feature-batch");
    let third = message(3, "third", "reviewer", "docs");
    let first_id = first.message_id.clone();
    let second_id = second.message_id.clone();
    let third_id = third.message_id.clone();
    queue_messages(&env, &[&first, &second, &third]);

    let trace_log = env.project_root.join("zellij-batch-trace.log");
    run_success(
        env.rimz()
            .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "deliver", "--message-id", first_id.as_str()]),
        "batch delivery",
    );

    let payload = "from @planner: first\n\nfrom @coder: second";
    assert_text_then_enter(&trace_log, payload);
    let lines = trace_lines(&trace_log);
    assert_eq!(
        lines.iter().filter(|line| is_paste(line, payload)).count(),
        1
    );
    assert_eq!(lines.iter().filter(|line| is_enter_key(line)).count(), 1);

    let sent_first = message_by_id(&env, &first_id);
    let sent_second = message_by_id(&env, &second_id);
    let queued_third = message_by_id(&env, &third_id);
    assert_eq!(sent_first.status, MessageStatus::Sent);
    assert_eq!(sent_second.status, MessageStatus::Sent);
    assert_eq!(sent_first.batch_id, Some(first_id.clone()));
    assert_eq!(sent_second.batch_id, Some(first_id.clone()));
    assert_eq!(queued_third.status, MessageStatus::Queued);
    assert_eq!(queued_third.batch_id, None);

    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-batch",
            "prompt": payload,
            "worktree_branch": "feature-batch",
        }),
        pane_env,
    );
    assert_eq!(message_by_id(&env, &third_id).status, MessageStatus::Queued);
    let delivered = delivered_message_ids(&env);
    assert_eq!(delivered, vec![first_id.to_string(), second_id.to_string()]);
}

#[test]
fn steer_auto_compact_runs_before_a_full_window() {
    let env = Env::new();
    register_running_agent(&env, "sess-ac", "feature-ac", &[("ZELLIJ_PANE_ID", "3")]);
    seed_context_fill(&env, "sess-ac", 80);

    let trace_log = env.project_root.join("zellij-ac-trace.log");
    let out = run_traced_smart_compact(&env, &trace_log, "go");

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

    let messages = env.store().list_messages().expect("messages");
    let command = messages
        .iter()
        .find(|message| message.body == MessageBody::Command)
        .expect("command message");
    let prompt = messages
        .iter()
        .find(|message| message.body == MessageBody::Prompt)
        .expect("prompt message");
    assert_eq!(command.text, "/compact");
    assert_eq!(command.sender, MessageSender::System);
    assert!(command.automated);
    assert_eq!(command.status, MessageStatus::Sent);
    assert_eq!(prompt.text, "go");
    assert_eq!(prompt.status, MessageStatus::Sent);
}

#[test]
fn boundary_auto_compact_defers_prompt_until_compaction_ends() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    run_hook(
        &env,
        json!({
            "hook_event_name": "SessionStart",
            "session_id": "sess-ac-boundary",
            "worktree_branch": "feature-ac-boundary",
        }),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_context_fill(&env, "sess-ac-boundary", 80);

    let trace_log = env.project_root.join("zellij-ac-boundary-trace.log");
    let out = run_success(
        env.rimz()
            .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
            .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
            .args([
                "message",
                "@claude",
                "--smart-compact",
                "70%",
                "--",
                "go after compact",
            ]),
        "boundary smart compact",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("delivers when compaction completes"),
        "{stdout}"
    );
    let lines = trace_lines(&trace_log);
    assert_eq!(
        lines.iter().filter(|line| is_compact_command(line)).count(),
        1
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| is_paste(line, "go after compact"))
            .count(),
        0
    );

    let messages = env.store().list_messages().expect("messages");
    let command = messages
        .iter()
        .find(|message| message.body == MessageBody::Command)
        .expect("compact command");
    let prompt = messages
        .iter()
        .find(|message| message.body == MessageBody::Prompt)
        .expect("deferred prompt");
    assert_eq!(command.status, MessageStatus::Sent);
    assert_eq!(command.sender, MessageSender::System);
    assert_eq!(prompt.status, MessageStatus::Queued);
    assert_eq!(prompt.attempts, 0);
    let prompt_id = prompt.message_id.clone();

    run_hook(
        &env,
        json!({
            "hook_event_name": "PreCompact",
            "session_id": "sess-ac-boundary",
            "worktree_branch": "feature-ac-boundary",
        }),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let shim = zellij_trace_shim();
    let shim = shim.to_str().expect("utf-8 shim path");
    let trace = trace_log.to_str().expect("utf-8 trace path");
    run_hook(
        &env,
        json!({
            "hook_event_name": "PostCompact",
            "session_id": "sess-ac-boundary",
            "trigger": "manual",
            "worktree_branch": "feature-ac-boundary",
        }),
        &[
            ("ZELLIJ_PANE_ID", "3"),
            ("RIMZ_ZELLIJ_BIN", shim),
            ("RIMZ_TEST_ZELLIJ_LOG", trace),
            ("RIMZ_MESSAGE_SETTLE_MS", "0"),
            ("RIMZ_MESSAGE_INTERVAL_MS", "0"),
        ],
    );
    wait_for_message_event_count(&env, "message.sent", 2, Duration::from_secs(5));

    let lines = trace_lines(&trace_log);
    assert_eq!(
        lines.iter().filter(|line| is_compact_command(line)).count(),
        1
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| is_paste(line, "go after compact"))
            .count(),
        1
    );
    let sent = message_by_id(&env, &prompt_id);
    assert_eq!(sent.status, MessageStatus::Sent);
    assert_eq!(sent.attempts, 1);
    assert_eq!(sent.unconfirmed_sends, 0);

    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-ac-boundary",
            "prompt": "go after compact",
            "worktree_branch": "feature-ac-boundary",
        }),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let delivered = env
        .store()
        .list_message_history()
        .expect("history")
        .into_iter()
        .find(|message| message.message_id == prompt_id)
        .expect("delivered prompt");
    assert_eq!(delivered.status, MessageStatus::Delivered);
    assert_eq!(delivered.attempts, 1);
    assert_eq!(delivered.unconfirmed_sends, 0);
}

#[test]
fn steer_auto_compact_write_failure_keeps_only_prompt_queued() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(
        &env,
        "sess-ac-fail",
        "feature-ac-fail",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_context_fill(&env, "sess-ac-fail", 80);

    let trace_log = env.project_root.join("zellij-ac-fail-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_TEST_ZELLIJ_MODE", "fail-write")
        .args([
            "message",
            "--steer",
            "@claude",
            "--smart-compact",
            "70%",
            "--",
            "go",
        ])
        .output()
        .expect("steer");
    assert!(
        out.status.success(),
        "steer should queue the prompt on mux failure\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].text, "go");
    assert_eq!(pending[0].body, MessageBody::Prompt);
    assert_eq!(pending[0].status, MessageStatus::Queued);
    assert!(pending[0].last_error.is_some(), "send error is recorded");
}

#[test]
fn steer_auto_compact_suppresses_only_an_unchanged_baseline() {
    let env = Env::new();
    register_running_agent(
        &env,
        "sess-ac-dupe",
        "feature-ac-dupe",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_context_tokens(&env, "sess-ac-dupe", 150_000, 200_000);

    let first_trace = env.project_root.join("zellij-ac-dupe-first-trace.log");
    run_traced_smart_compact(&env, &first_trace, "go1");
    let first_lines = trace_lines(&first_trace);
    let compact_at = first_lines.iter().position(|line| is_compact_command(line));
    let paste_at = first_lines.iter().position(|line| is_paste(line, "go1"));
    assert!(compact_at.is_some() && compact_at < paste_at);

    let second_trace = env.project_root.join("zellij-ac-dupe-second-trace.log");
    run_traced_smart_compact(&env, &second_trace, "go2");
    let second_lines = trace_lines(&second_trace);
    assert!(!second_lines.iter().any(|line| is_compact_command(line)));

    seed_context_tokens(&env, "sess-ac-dupe", 160_000, 200_000);
    let third_trace = env.project_root.join("zellij-ac-dupe-third-trace.log");
    run_traced_smart_compact(&env, &third_trace, "go3");
    let third_lines = trace_lines(&third_trace);
    assert!(third_lines.iter().any(|line| is_compact_command(line)));

    let messages = env.store().list_messages().expect("messages");
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
        .args(["message", "--steer", "@claude", "--", "go"])
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
}

/// A waiting agent reserves the next input, so a queued message defers at
/// the open gate rather than landing on top of the ask — it stays pending for a
/// later boundary, and nothing is pasted.
#[test]
fn queue_waiting_agent_defers_unforced_and_force_delivers() {
    {
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
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "@claude", "--", "go"])
            .output()
            .expect("message");
        assert!(
            out.status.success(),
            "message failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        assert_eq!(
            env.store().list_pending_messages().unwrap().len(),
            1,
            "a waiting agent defers delivery; the message stays queued"
        );
        assert!(
            trace_lines(&trace_log)
                .iter()
                .all(|line| !is_paste(line, "go")),
            "nothing is pasted while the ask reserves input"
        );
    }

    {
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
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "@claude", "--force", "--", "go"])
            .output()
            .expect("message");
        assert!(
            out.status.success(),
            "message failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        assert!(
            env.store().list_pending_messages().unwrap().is_empty(),
            "--force delivers past the waiting agent inline"
        );
        assert_text_then_enter(&trace_log, "go");
    }
}

#[test]
fn broadcast_at_all_sends_without_yes() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(
        &env,
        "sess-at-all",
        "feature-at-all",
        &[("ZELLIJ_PANE_ID", "5")],
    );

    let out = env
        .rimz()
        .args(["message", "@all", "--", "heads up"])
        .output()
        .expect("queue broadcast");
    assert!(
        out.status.success(),
        "broadcast failed without a confirmation flag: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].text, "@all, heads up");
}

#[test]
fn message_miss_lists_available_agents() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-miss-list", "feature-miss-list", &[]);
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "sess-miss-list",
        LifecycleSignal::Registered,
        |observation| {
            observation.agent_name = Some("swift-otter".to_owned());
            observation.launch.role = Some("helper".to_owned());
            observation.worktree_branch = Some("feature-miss-list".to_owned());
        },
    );

    let out = env
        .rimz()
        .args(["message", "@ghost", "--", "hi"])
        .output()
        .expect("message miss");
    assert!(!out.status.success(), "miss should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no agent matches target `@ghost`"),
        "miss header missing: {stderr}"
    );
    assert!(
        stderr.contains("AGENT") && stderr.contains("STATUS"),
        "agent table header missing: {stderr}"
    );
    assert!(
        stderr.contains("@helper"),
        "running agent handle missing from miss table: {stderr}"
    );
    let bounce = env
        .read_events()
        .into_iter()
        .find(|event| event.method == "message.errored")
        .expect("miss records a bounce event");
    let params = bounce.params_value();
    assert_eq!(params["address"], "@ghost");
    assert_eq!(params["status"], "errored");
    assert_eq!(params["reason"], "receiver not found");

    let listed = env
        .rimz()
        .args(["message", "list", "--all", "--json"])
        .output()
        .expect("message list");
    assert!(
        listed.status.success(),
        "message list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("json");
    assert!(
        parsed
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["address"] == "@ghost" && row["status"] == "errored"),
        "bounce row missing from list: {parsed}"
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
        .args(["message", "--steer", "@claude", "--", "hello"])
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

/// `steer @claude --all` broadcasts to every claude with a bound pane and
/// prints a summary naming the count.
#[test]
fn steer_fanout_summary() {
    let env = Env::new();
    register_running_agent(&env, "sess-fsa", "feature-fsa", &[("ZELLIJ_PANE_ID", "3")]);
    register_running_agent(&env, "sess-fsb", "feature-fsb", &[("ZELLIJ_PANE_ID", "4")]);

    let trace_log = env.project_root.join("zellij-fanout-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "--steer", "@claude", "--all", "--", "hello"])
        .output()
        .expect("steer fanout");
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
    let pasted = trace_lines(&trace_log)
        .into_iter()
        .filter(|line| is_paste_to_any_pane(line, "@claude, hello"))
        .count();
    assert_eq!(pasted, 2, "fan-out should paste once per live pane");
}

/// A skipped agent never aborts a broadcast: both targeted agents have a pane,
/// but one waits on input, so it is skipped while the other still receives
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
    // A second pane-bound card, blocked by waiting input — it can only be skipped.
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
        .args(["message", "--steer", "@claude", "--all", "--", "go"])
        .output()
        .expect("steer partial skip");
    assert!(
        out.status.success(),
        "a skipped agent must not abort the broadcast: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("sent 1 agent(s)") && stdout.contains("waiting in pane"),
        "summary names the sent and skipped agents: {stdout}"
    );
}

fn zellij_trace_shim() -> PathBuf {
    crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace"))
}

fn run_success(cmd: &mut std::process::Command, label: &str) -> std::process::Output {
    let output = cmd
        .output()
        .unwrap_or_else(|error| panic!("{label}: {error}"));
    assert!(
        output.status.success(),
        "{label} failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn run_traced_smart_compact(env: &Env, trace_log: &Path, text: &str) -> std::process::Output {
    let output = run_success(
        env.rimz()
            .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
            .env("RIMZ_TEST_ZELLIJ_LOG", trace_log)
            .args([
                "message",
                "--steer",
                "@claude",
                "--smart-compact",
                "70%",
                "--",
                text,
            ]),
        "smart compact steer",
    );
    assert!(
        trace_lines(trace_log)
            .iter()
            .any(|line| is_paste(line, text))
    );
    output
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
    let worktree_path = env.home_root.join(branch).display().to_string();
    run_hook(
        env,
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
            "worktree_path": worktree_path.as_str(),
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
            "worktree_path": worktree_path.as_str(),
        }),
        pane_env,
    );
}

fn register_role_agent(
    env: &Env,
    kind: &str,
    session_id: &str,
    role: &str,
    running: bool,
    pane_id: Option<&str>,
) {
    append_lifecycle(
        env,
        kind,
        "SessionStart",
        session_id,
        LifecycleSignal::Registered,
        |observation| {
            observation.agent_name = Some(format!("{role}-agent"));
            observation.launch.role = Some(role.to_owned());
            observation.worktree_branch = Some(format!("feature-{role}"));
            observation.pane_id =
                pane_id.map(|pane_id| PaneId::from_parts(MuxName::Zellij, pane_id));
        },
    );
    if running {
        append_lifecycle(
            env,
            kind,
            "UserPromptSubmit",
            session_id,
            LifecycleSignal::TurnStarted,
            |observation| {
                observation.agent_name = Some(format!("{role}-agent"));
                observation.launch.role = Some(role.to_owned());
                observation.worktree_branch = Some(format!("feature-{role}"));
                observation.pane_id =
                    pane_id.map(|pane_id| PaneId::from_parts(MuxName::Zellij, pane_id));
            },
        );
    }
}

fn register_old_idle_role_agent(
    env: &Env,
    session_id: &str,
    role: &str,
    age_secs: i64,
) -> jiff::Timestamp {
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(session_id)),
        LifecycleSignal::Registered,
    );
    observation.agent_name = Some(format!("{role}-agent"));
    observation.launch.role = Some(role.to_owned());
    observation.worktree_branch = Some(format!("feature-{role}"));
    observation.worktree_path = Some(env.project_root.display().to_string());
    let mut event = EventEnvelope::agent_lifecycle(
        workspace.workspace_id,
        workspace.session_name,
        "claude",
        "SessionStart",
        &observation,
    );
    event.timestamp = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(age_secs);
    let at = event.timestamp;
    env.store()
        .append_event(&event)
        .expect("append old idle state");
    at
}

fn register_idle_agent_with_transcript(
    env: &Env,
    session_id: &str,
    branch: &str,
    transcript: &Path,
    pane_env: &[(&str, &str)],
) {
    let worktree_path = env.home_root.join(branch).display().to_string();
    run_hook(
        env,
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
            "worktree_path": worktree_path,
            "transcript_path": transcript.to_string_lossy(),
        }),
        pane_env,
    );
}

fn append_claude_assistant(transcript: &Path, text: &str) {
    let line = json!({
        "type": "assistant",
        "message": {
            "content": [{ "type": "text", "text": text }]
        }
    });
    let mut transcript = std::fs::OpenOptions::new()
        .append(true)
        .open(transcript)
        .expect("open transcript");
    writeln!(transcript, "{line}").expect("append assistant message");
}

fn begin_wait_reply(env: &Env, session_id: &str, branch: &str, transcript: &Path, pane: &str) {
    run_hook(
        env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "fanout question",
            "worktree_branch": branch,
            "transcript_path": transcript.to_string_lossy(),
        }),
        &[("ZELLIJ_PANE_ID", pane)],
    );
}

#[allow(clippy::too_many_arguments)]
fn finish_wait_reply(
    env: &Env,
    session_id: &str,
    branch: &str,
    transcript: &Path,
    pane: &str,
    reply: &str,
    failed: bool,
) {
    append_claude_assistant(transcript, reply);
    let mut payload = json!({
        "hook_event_name": "Stop",
        "session_id": session_id,
        "last_assistant_message": reply,
        "worktree_branch": branch,
        "transcript_path": transcript.to_string_lossy(),
    });
    if failed {
        payload["is_error"] = json!(true);
    }
    run_hook(env, payload, &[("ZELLIJ_PANE_ID", pane)]);
}

/// Seed a context sidecar so `--smart-compact` reads `used_pct` as the agent's
/// window fill — the same record the producer would fold from a live statusline.
fn seed_context_fill(env: &Env, agent_id: &str, used_pct: u8) {
    let mut context = rimz::store::agent_context::empty_context("claude", jiff::Timestamp::now());
    context.tokens = Some(rimz::agents::AgentTokenUsage {
        used_percentage: Some(used_pct),
        ..Default::default()
    });
    let record = rimz::store::agent_context::new_record("claude", agent_id, context);
    rimz::store::agent_context::write_record(&env.runtime_paths(), &record)
        .expect("seed context sidecar");
}

/// Seed a context sidecar with token composition so `occupied_context_tokens`
/// has the deterministic baseline smart-compact uses to suppress duplicates.
fn seed_context_tokens(env: &Env, agent_id: &str, used: u64, window: u64) {
    let mut context = rimz::store::agent_context::empty_context("claude", jiff::Timestamp::now());
    context.tokens = Some(rimz::agents::AgentTokenUsage {
        context_window_size: Some(window),
        current_usage: Some(rimz::agents::AgentCurrentUsage {
            input_tokens: Some(used),
            ..Default::default()
        }),
        ..Default::default()
    });
    let record = rimz::store::agent_context::new_record("claude", agent_id, context);
    rimz::store::agent_context::write_record(&env.runtime_paths(), &record)
        .expect("seed context sidecar");
}

fn seed_turn_error(env: &Env, agent_id: &str, class: TurnErrorClass) {
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == agent_id)
        .expect("agent");
    let at = agent.last_activity + jiff::SignedDuration::from_secs(1);
    let mut context = rimz::store::agent_context::empty_context("claude", at);
    context.turn_error = Some(AgentTurnError {
        class,
        at,
        label: Some("provider parked".to_owned()),
    });
    let record = rimz::store::agent_context::new_record("claude", agent_id, context);
    rimz::store::agent_context::write_record(&env.runtime_paths(), &record)
        .expect("seed turn error");
}

fn seed_rate_limit_budget(env: &Env, used_percentage: u8) {
    let window = RateLimitWindow {
        used_percentage: Some(used_percentage),
        resets_at: Some(jiff::Timestamp::now() + jiff::SignedDuration::from_secs(300)),
        duration_mins: Some(300),
        ..Default::default()
    };
    let cache = rimz::agents::account::RateLimitsCache {
        refreshed_at_ms: 0,
        entries: [(
            "claude".to_owned(),
            rimz::agents::account::RateLimitCacheEntry {
                limits: AgentRateLimits {
                    windows: vec![window],
                },
                ..Default::default()
            },
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    rimz::store::atomic::write_temp_then_rename_cache(
        &env.runtime_paths().shared_rate_limits_path(),
        &cache,
    )
    .expect("seed rate-limit cache");
}

fn run_hook(env: &Env, payload: serde_json::Value, pane_env: &[(&str, &str)]) {
    let mut payload = payload;
    stamp_worktree_path(env, &mut payload);
    let payload = serde_json::to_string(&payload).expect("payload");
    let mut cmd = env.hook_command("claude");
    scrub_launch_identity(&mut cmd);
    let owner = dummy_agent_process();
    let owner_pid = owner.id();
    reap_later(owner);
    cmd.env("RIMZ_AGENT_PID", owner_pid.to_string());
    for (key, value) in pane_env {
        cmd.env(key, value);
    }
    let output = env
        .spawn_payload(cmd, &payload)
        .wait_with_output()
        .expect("wait hook");
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn dummy_agent_process() -> std::process::Child {
    let mut cmd = std::process::Command::new("sleep");
    scrub_launch_identity(&mut cmd);
    // ponytail: bounded sleeper keeps hook-owned agents live for test snapshots;
    // add a per-test owner guard if tests start lasting longer than this window.
    cmd.arg("30").spawn().expect("spawn dummy agent process")
}

fn reap_later(mut child: std::process::Child) {
    let _ = std::thread::spawn(move || {
        let _ = child.wait();
    });
}

fn scrub_launch_identity(cmd: &mut std::process::Command) {
    for key in [
        rimz::harness::run::ENV_AGENT_NAME,
        rimz::harness::run::ENV_AGENT_PROFILE,
        rimz::harness::run::ENV_AGENT_ROLE,
        rimz::harness::run::ENV_TEAM,
        rimz::harness::run::ENV_LAUNCH_GROUP,
        rimz::harness::run::ENV_LAUNCH_ORDINAL,
        rimz::harness::run::ENV_CHANNEL,
        rimz::harness::run::ENV_AGENT_MODEL,
        rimz::harness::run::ENV_AGENT_EFFORT,
    ] {
        cmd.env(key, "");
    }
}

fn stamp_worktree_path(env: &Env, payload: &mut serde_json::Value) {
    if payload.get("worktree_path").is_some() {
        return;
    }
    let Some(branch) = payload
        .get("worktree_branch")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
    else {
        return;
    };
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    object.insert(
        "worktree_path".to_owned(),
        json!(env.home_root.join(branch).display().to_string()),
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
    env.store().append_event(&event).expect("append lifecycle");
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
        .args(["message", target, "--", text])
        .output()
        .expect("message");
    assert!(
        out.status.success(),
        "message failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    queued_id_from_stdout(&out.stdout)
}

fn queue_add_in_channel(env: &Env, channel: &str, target: &str, text: &str) -> String {
    let out = env
        .rimz()
        .args(["message", "--channel", channel, target, "--", text])
        .output()
        .expect("message");
    assert!(
        out.status.success(),
        "message failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    queued_id_from_stdout(&out.stdout)
}

fn message_by_id(env: &Env, message_id: &MessageId) -> MessageRecord {
    env.store()
        .list_messages()
        .expect("messages")
        .into_iter()
        .find(|message| &message.message_id == message_id)
        .unwrap_or_else(|| panic!("missing message {message_id}"))
}

fn queue_messages(env: &Env, messages: &[&MessageRecord]) {
    for message in messages {
        env.store()
            .queue_message(message, "rimz-test")
            .expect("queue message");
    }
}

fn delivered_message_ids(env: &Env) -> Vec<String> {
    env.read_events()
        .into_iter()
        .filter(|event| event.method == "message.delivered")
        .map(|event| {
            event.params_value()["message_id"]
                .as_str()
                .expect("delivered message ID")
                .to_owned()
        })
        .collect()
}

fn seed_channel_message(
    env: &Env,
    id: u64,
    epoch_seconds: i64,
    channel: Option<&str>,
    text: &str,
) -> String {
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.parent_agent_id.is_none())
        .expect("agent");
    let mut message = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        text.to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_channel(channel.map(str::to_owned));
    message.message_id = fixed_message_id(id);
    message.enqueued_at = jiff::Timestamp::from_second(epoch_seconds).expect("fixed timestamp");
    let message_id = message.message_id.to_string();
    env.store()
        .queue_message(&message, "rimz-test")
        .expect("seed message");
    message_id
}

fn list_message_ids(env: &Env, args: &[&str], channel: Option<&str>) -> Vec<String> {
    let mut cmd = env.rimz();
    cmd.args(args);
    if let Some(channel) = channel {
        cmd.env(rimz::harness::run::ENV_CHANNEL, channel);
    }
    let output = run_success(&mut cmd, "message list");
    serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .expect("message list JSON")
        .as_array()
        .expect("message rows")
        .iter()
        .map(|row| row["message_id"].as_str().expect("message ID").to_owned())
        .collect()
}

fn queue_direct_channel_message(env: &Env, channel: &str, text: &str) -> String {
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.parent_agent_id.is_none())
        .expect("agent");
    let message = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        text.to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_channel(Some(channel.to_owned()));
    let message_id = message.message_id.to_string();
    env.store()
        .queue_message(&message, "rimz-test")
        .expect("queue message");
    message_id
}

fn wake_stamp_path(env: &Env) -> PathBuf {
    env.runtime_paths()
        .root
        .join(rimz::message::MESSAGE_WAKE_FILE)
}

fn wait_for_message_event(env: &Env, method: &str, timeout: Duration) {
    wait_for_message_event_count(env, method, 1, timeout);
}

fn wait_for_message_event_count(env: &Env, method: &str, count: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if env
            .read_events()
            .iter()
            .filter(|event| event.method == method)
            .count()
            >= count
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {count} {method} events"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_with_output_bounded(
    mut child: std::process::Child,
    timeout: Duration,
) -> std::process::Output {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().expect("poll child").is_some() {
            return child.wait_with_output().expect("collect child output");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().expect("collect timed-out output");
            panic!(
                "child did not finish within {timeout:?}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn queued_id_from_stdout(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    trimmed
        .strip_prefix("queued for ")
        .and_then(|rest| rest.rsplit_once('('))
        .and_then(|(_, id)| id.strip_suffix(')'))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("expected `queued for @target (msg_...)`, got `{trimmed}`"))
}

fn assert_second_precision_created(shown: &str) {
    let line = shown
        .lines()
        .find(|line| line.trim_start().starts_with("created:"))
        .expect("created row");
    let absolute = line
        .rsplit_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .unwrap_or_else(|| panic!("created row has absolute timestamp: {line}"));
    assert_eq!(absolute.len(), "2026-07-06T12:47:26Z".len(), "{line}");
    assert!(absolute.contains('T') && absolute.ends_with('Z'), "{line}");
    assert!(!absolute.contains('.'), "{line}");
}

fn sent_id_from_stdout(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    trimmed
        .strip_prefix("sent to ")
        .and_then(|rest| rest.rsplit_once('('))
        .and_then(|(_, id)| id.strip_suffix(')'))
        .map(str::to_owned)
        .unwrap_or_else(|| panic!("expected `sent to @target (msg_...)`, got `{trimmed}`"))
}

fn fixed_message_id(value: u64) -> MessageId {
    MessageId::parse(&format!("msg_{value:016}")).unwrap()
}

fn assert_single_sigil_sent(stdout: &[u8]) {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    assert!(
        trimmed.starts_with("sent to @") && !trimmed.starts_with("sent to @@"),
        "send confirmation should carry one sigil: {trimmed}"
    );
}

fn push_pending_agent_ask(env: &Env, session_id: &str) {
    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(session_id)),
        LifecycleSignal::AwaitingInput {
            kind: AskKind::Permission,
            ask_id: None,
            detail: None,
        },
    );
    env.store()
        .append_event(&EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "rimz-test",
            "claude",
            "PermissionRequest",
            &observation,
        ))
        .expect("append waiting signal");
}

fn agent_pane(env: &Env, command: &str) -> rimz::pane::PaneRef {
    rimz::pane::PaneRef {
        pane_id: rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, TRACE_PANE),
        session_name: "rimz-test".to_owned(),
        view_id: Some("tab_1".to_owned()),
        view_kind: Some(rimz::ids::ViewKind::Tab),
        view_name: Some("project".to_owned()),
        title: None,
        is_focused: false,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(env.project_root.display().to_string()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
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
            agent_name_explicit: false,
            launch: LaunchParams {
                profile: None,
                mode: None,

                role: role.map(ToOwned::to_owned),

                model: None,

                effort: None,

                budget: None,

                team: None,

                launch_group: None,

                launch_ordinal: None,

                channel: None,

                kind_ordinal: Some(1),
            },
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
    env.store().append_event(&event).expect("append launch");
}

/// `steer @codex` reaches a bare codex started in a pane before its first turn:
/// the selector folds the live pane frame, finds the synthesized idle row, and
/// pastes into its pane — reproducing and fixing the `no agent matches @codex`
/// failure. The pane fixture stands in for the mux, and codex must be wired
/// (hooks installed) for the idle row to synthesize.
#[test]
fn steer_reaches_unbound_codex_pane() {
    let env = Env::new();
    env.install_agent_hooks("codex");
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "codex")]);

    let trace_log = env.project_root.join("zellij-unbound-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["message", "--steer", "@codex", "--", "continue"])
        .output()
        .expect("steer");
    assert!(
        out.status.success(),
        "steer to an unbound codex pane failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_text_then_enter(&trace_log, "continue");
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
        None,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "codex")]);

    let trace_log = env.project_root.join("zellij-provisional-queue-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["message", "@coder", "--", "read plan"])
        .output()
        .expect("message");
    assert!(
        out.status.success(),
        "queue to a provisional codex should send now: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_text_then_enter(&trace_log, "read plan");
    let messages = env.store().list_messages().unwrap();
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
        methods.iter().any(|method| method == "message.queued"),
        "send-now queue records the durable record before sending: {methods:?}"
    );
}

#[test]
fn provisional_without_live_frame_parks_queue_and_steer() {
    let env = Env::new();
    env.install_agent_hooks("codex");
    trust_codex_hooks(&env);
    seed_provisional_codex_launch(
        &env,
        "launch_no_frame",
        "swift-otter",
        Some("coder"),
        "terminal_8",
        None,
    );

    let trace_log = env
        .project_root
        .join("zellij-provisional-no-frame-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "@coder", "--", "read plan"])
        .output()
        .expect("message");
    assert!(
        out.status.success(),
        "queue to a provisional codex should park without a live pane: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let messages = env.store().list_messages().unwrap();
    assert_eq!(
        messages.len(),
        1,
        "parked provisional queue writes a record"
    );
    assert_eq!(messages[0].status, MessageStatus::Queued);
    assert_eq!(messages[0].agent_id.as_str(), "launch_no_frame");
    let methods: Vec<String> = env
        .read_events()
        .into_iter()
        .map(|event| event.method)
        .collect();
    assert!(
        methods.iter().any(|method| method == "message.queued"),
        "no-live-frame queue records message.queued: {methods:?}"
    );
    assert!(
        methods.iter().all(|method| method != "message.sent"),
        "no-live-frame queue is not sent: {methods:?}"
    );
    let lines = trace_lines(&trace_log);
    assert!(
        lines
            .iter()
            .all(|line| !is_paste_to_any_pane(line, "read plan")),
        "no-live-frame queue must not paste into the stale launch pane: {lines:?}"
    );

    let trace_log = env
        .project_root
        .join("zellij-provisional-no-frame-steer-trace.log");
    let out = env
        .rimz()
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_log)
        .args(["message", "--steer", "@coder", "--", "read plan"])
        .output()
        .expect("message --steer");
    assert!(
        out.status.success(),
        "steer to a provisional codex without a live pane should park: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let messages = env.store().list_messages().unwrap();
    assert_eq!(messages.len(), 2, "steer parks a second record");
    assert!(
        messages
            .iter()
            .any(|message| message.text == "read plan" && message.status == MessageStatus::Queued)
    );
    let lines = trace_lines(&trace_log);
    assert!(
        lines
            .iter()
            .all(|line| !is_paste_to_any_pane(line, "read plan")),
        "no-live-frame steer must not paste into the stale launch pane: {lines:?}"
    );
}
