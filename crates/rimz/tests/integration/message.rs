use serde_json::json;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use rimz::agents::{
    AgentLifecycleObservation, AgentRateLimits, AgentTurnError, AskKind, COMPACTING_WINDOW_SECS,
    LaunchParams, LifecycleSignal, RateLimitWindow, TurnErrorClass,
};
use rimz::ids::{AgentKind, AgentSessionId, MessageId, MuxName, PaneId};
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope, MessageEventMethod};
use rimz::store::message::{
    AutoCompact, DeliveryGate, HarnessNotice, MessageBody, MessageRecord, MessageSender,
    MessageStatus, WhenCondition,
};

use crate::common::{Env, trust_codex_preflight_hooks, zellij_trace_shim};

#[test]
fn targetless_send_flags_refuse_with_usage_error() {
    let env = Env::new();
    let mut failures = Vec::new();
    for flags in [
        vec!["--on", "done"],
        vec!["--steer"],
        vec!["--interrupt"],
        vec!["--schedule", "1h"],
        vec!["--after", "@planner"],
        vec!["--when", "@coder idle 58m"],
        vec!["--worktree", "auth"],
        vec!["--channel", "auth"],
        vec!["--no-enter"],
        vec!["--force"],
        vec!["--all"],
        vec!["--create"],
        vec!["--smart-compact", "70%"],
        vec!["--file", "missing.txt"],
        vec!["--stdin"],
        vec!["--no-from"],
        vec!["--wait"],
        vec!["--json"],
        vec!["--any"],
        vec!["--json", "list"],
        vec!["--steer", "list"],
    ] {
        let output = env.rimz().arg("message").args(&flags).output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.code() != Some(2)
            || !output.stdout.is_empty()
            || !stderr.contains(flags[0])
            || !stderr.contains("rimz message list")
        {
            failures.push(format!(
                "{flags:?}: exit {:?}, stdout {:?}, stderr {stderr}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn malformed_message_commands_are_usage_errors() {
    let env = Env::new();
    let mut failures = Vec::new();
    for (args, hint) in [
        (vec!["cancel"], "<MESSAGE_ID>..."),
        (vec!["codex"], "unknown subcommand `codex`"),
        (
            vec!["msg_0000000000000001"],
            "did you mean `rimz message show",
        ),
        (vec!["codex", "hello"], "@codex"),
    ] {
        let output = env.rimz().arg("message").args(&args).output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.code() != Some(2) || !output.stdout.is_empty() || !stderr.contains(hint) {
            failures.push(format!(
                "{args:?}: exit {:?}, stderr {stderr}",
                output.status.code()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn interrupt_fixture(kind: &str) -> (Env, PathBuf) {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks(kind);
    if kind == "codex" {
        trust_codex_preflight_hooks(&env);
    }
    for (event, signal) in [
        ("SessionStart", LifecycleSignal::Registered),
        (
            "UserPromptSubmit",
            LifecycleSignal::TurnStarted { turn_id: None },
        ),
    ] {
        append_lifecycle(&env, kind, event, "sess-interrupt", signal, |observation| {
            observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
        });
    }
    let panes = env.write_pane_fixture(&[agent_pane(&env, kind)]);
    (env, panes)
}

fn is_escape_key(line: &str) -> bool {
    line.ends_with(&format!("\taction\twrite\t--pane-id\t{TRACE_PANE}\t27"))
}

fn interrupt_order_case(kind: &str, queued: bool, force: bool) {
    let (env, panes) = interrupt_fixture(kind);
    let message_id = queued.then(|| queue_add(&env, &format!("@{kind}"), "new direction"));
    if force {
        push_pending_agent_ask(&env, "sess-interrupt");
    }
    let trace = env.project_root.join("interrupt-trace.log");
    let mut command = traced_rimz(&env, &trace);
    command
        .env("RIMZ_TEST_PANE_LIST", &panes)
        .env("RIMZ_MESSAGE_INTERRUPT_DELAY_MS", "50");
    if let Some(id) = &message_id {
        command.args(["message", "interrupt", id]);
    } else {
        command.args([
            "message",
            "--interrupt",
            &format!("@{kind}"),
            "new direction",
        ]);
    }
    if force {
        command.arg("--force");
    }
    run_success(&mut command, "interrupt");
    let lines = trace_lines(&trace);
    assert_eq!(lines.iter().filter(|line| is_escape_key(line)).count(), 1);
    let paste = lines
        .iter()
        .position(|line| is_paste(line, &user_message("new direction")))
        .expect("prompt pasted");
    let enter = lines.iter().position(|line| is_enter_key(line)).unwrap();
    let escape = lines.iter().position(|line| is_escape_key(line)).unwrap();
    assert!(paste < enter && enter < escape, "{lines:?}");
    let records = env.store().list_messages().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, MessageStatus::Sent);
}

#[test]
fn interrupt_codex_pastes_then_presses_escape() {
    interrupt_order_case("codex", false, false);
}

#[test]
fn interrupt_claude_pastes_then_presses_escape() {
    interrupt_order_case("claude", false, false);
}

#[test]
fn interrupt_queued_record_pastes_then_presses_escape() {
    interrupt_order_case("codex", true, false);
}

#[test]
fn interrupt_force_pastes_past_native_ask() {
    interrupt_order_case("claude", false, true);
}

#[test]
fn interrupt_waiting_refuses_before_record() {
    let (env, panes) = interrupt_fixture("claude");
    push_pending_agent_ask(&env, "sess-interrupt");
    let trace = env.project_root.join("interrupt-refused.log");
    let output = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_PANE_LIST", panes)
        .args(["message", "--interrupt", "@claude", "new direction"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--force"));
    assert!(
        env.store().list_messages().unwrap().is_empty(),
        "refuse before persisting"
    );
    assert!(
        env.store().list_message_history().unwrap().is_empty(),
        "refusal must not terminalize a newly written record"
    );
    assert!(trace_lines(&trace).is_empty());
}

#[test]
fn interrupt_idle_skips_escape() {
    let (env, panes) = interrupt_fixture("claude");
    append_lifecycle(
        &env,
        "claude",
        "Stop",
        "sess-interrupt",
        LifecycleSignal::TurnInterrupted { turn_id: None },
        |_| {},
    );
    let trace = env.project_root.join("interrupt-idle.log");
    let output = run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", panes)
            .args(["message", "--interrupt", "@claude", "new direction"]),
        "interrupt idle",
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("sent to"));
    assert!(!trace_lines(&trace).iter().any(|line| is_escape_key(line)));
    assert_text_then_enter(&trace, &user_message("new direction"));
    assert_eq!(
        env.store().list_messages().unwrap()[0].status,
        MessageStatus::Sent
    );
}

#[test]
fn interrupt_keyless_refuses_before_record() {
    let (env, panes) = interrupt_fixture("pi");
    let output = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", panes)
        .args(["message", "--interrupt", "@pi", "new direction"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "keyless kind must refuse");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("has no interrupt key") && stderr.contains("--steer"),
        "{stderr}"
    );
    assert!(env.store().list_messages().unwrap().is_empty());
}

#[test]
fn interrupt_fanout_keyless_refuses_entire_send() {
    let (env, _) = interrupt_fixture("claude");
    append_lifecycle(
        &env,
        "pi",
        "SessionStart",
        "sess-keyless",
        LifecycleSignal::Registered,
        |_| {},
    );
    let output = env
        .rimz()
        .args(["message", "--interrupt", "@all", "new direction"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("has no interrupt key"));
    assert!(
        env.store().list_messages().unwrap().is_empty(),
        "whole fanout preflight"
    );
}

#[test]
fn interrupt_unbound_live_pane_refuses_before_record() {
    let env = Env::new();
    env.record(&env.project_root);
    let panes = env.write_pane_fixture(&[agent_pane(&env, "codex")]);
    let output = traced_rimz(&env, "interrupt-unbound.log")
        .env("RIMZ_TEST_PANE_LIST", panes)
        .args(["message", "--interrupt", "@codex", "new direction"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no durable session"));
    assert!(env.store().list_messages().unwrap().is_empty());
}

#[test]
fn interrupt_without_live_pane_parks_with_interrupt_hint() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "sess-interrupt",
        LifecycleSignal::Registered,
        |_| {},
    );
    let panes = env.write_pane_fixture(&[]);
    let output = run_success(
        env.rimz().env("RIMZ_TEST_PANE_LIST", panes).args([
            "message",
            "--interrupt",
            "@claude",
            "new direction",
        ]),
        "interrupt absent pane",
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("rimz message interrupt"));
    assert_eq!(
        env.store().list_messages().unwrap()[0].status,
        MessageStatus::Queued
    );
}

#[test]
fn interrupt_queued_record_refuses_claimed_and_terminal() {
    let (env, _) = interrupt_fixture("claude");
    let id = MessageId::parse(&queue_add(&env, "@claude", "new direction")).unwrap();
    env.store()
        .claim_message_for_steer(&id, jiff::Timestamp::now())
        .unwrap()
        .unwrap();
    let output = env
        .rimz()
        .args(["message", "interrupt", id.as_str()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("delivery in progress"));
    env.store()
        .cancel_message(&id, "rimz-test", "test cancellation")
        .unwrap();
    let output = env
        .rimz()
        .args(["message", "interrupt", id.as_str()])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requeue"));
}

#[test]
fn interrupt_conflicts_with_other_timing_modes() {
    let env = Env::new();
    for flags in [
        vec!["--steer"],
        vec!["--on", "any"],
        vec!["--schedule", "1m"],
        vec!["--after", "@claude"],
        vec!["--when", "@claude idle 1m"],
    ] {
        let output = env
            .rimz()
            .args(["message", "--interrupt", "@claude", "text"])
            .args(flags)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
    }
}

#[test]
fn interrupt_refuses_create_policy_before_delivery() {
    let (env, panes) = interrupt_fixture("claude");
    let output = traced_rimz(&env, "interrupt-create.log")
        .env("RIMZ_TEST_PANE_LIST", panes)
        .args([
            "message",
            "--interrupt",
            "--create",
            "@claude",
            "new direction",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "interrupt requires an existing durable recipient, not create-on-miss"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("remove --create"));
    assert!(env.store().list_messages().unwrap().is_empty());
}

#[test]
fn message_cancel_and_clear_respect_ids_targets_and_channel_lanes() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-queue", "feature-q", &[]);

    let first = queue_add(&env, "@claude", "first task");
    let second = queue_add(&env, "@claude", "second task");
    let third = queue_add(&env, "@claude", "third task");
    let missing = "msg_0000000000009999";
    let canceled = env
        .rimz()
        .args(["message", "cancel", &first, missing, &second])
        .output()
        .expect("mixed cancel");
    assert!(!canceled.status.success(), "missing ID reports failure");
    let stdout = String::from_utf8_lossy(&canceled.stdout);
    assert!(stdout.contains(&format!("canceled {first}")));
    assert!(stdout.contains(&format!("{missing} cannot be canceled")));
    assert!(stdout.contains(&format!("canceled {second}")));

    let alias = queue_add(&env, "@claude", "alias task");
    let alias_output = run_success(
        env.rimz().args(["message", "remove", &alias]),
        "remove alias",
    );
    assert!(String::from_utf8_lossy(&alias_output.stdout).contains(&format!("canceled {alias}")));

    let cleared = run_success(
        env.rimz().args(["message", "clear", "@claude"]),
        "queue clear",
    );
    let cleared_stdout = String::from_utf8_lossy(&cleared.stdout);
    assert!(cleared_stdout.contains("canceled 1 message(s) for @claude"));
    assert!(cleared_stdout.contains(&third));
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    let canceled_ids = list_message_ids(
        &env,
        &["message", "list", "--all", "--status", "canceled", "--json"],
        None,
    );
    assert!(canceled_ids.contains(&first));

    let methods: Vec<String> = env
        .read_events()
        .into_iter()
        .map(|event| event.method)
        .filter(|method| method.starts_with("message."))
        .collect();
    assert!(methods.iter().any(|method| method == "message.queued"));
    assert!(methods.iter().any(|method| method == "message.canceled"));

    let docs = queue_direct_channel_message(&env, "docs", "docs");
    let docs_team = queue_direct_channel_message(&env, "docs/forge", "forge");
    let ops = queue_direct_channel_message(&env, "ops", "ops");
    let cleared = run_success(
        env.rimz()
            .env(rimz::workspace::ENV_CHANNEL, "docs")
            .args(["message", "clear"]),
        "clear lane",
    );
    let stdout = String::from_utf8_lossy(&cleared.stdout);
    assert!(stdout.contains("canceled 1 message(s) in #docs"));
    assert!(stdout.contains(&docs));
    assert!(!stdout.contains(&docs_team));
    let pending = env.store().list_pending_messages().unwrap();
    let pending_ids: Vec<&str> = pending
        .iter()
        .map(|message| message.message_id.as_str())
        .collect();
    assert_eq!(pending_ids, vec![docs_team.as_str(), ops.as_str()]);
}

#[test]
fn message_list_empty_lane_counts_other_visible_rows() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(&env, "sess-docs", "docs", &[]);
    seed_channel_message(&env, 1, 100, Some("old"), "archived text");
    env.store()
        .archive_channel_messages("old", "archive", "rimz-test")
        .unwrap();
    seed_channel_message(&env, 2, 200, Some("ops"), "private ops text");
    seed_channel_message(&env, 3, 300, Some("review"), "private review text");
    let snapshot = env.store().snapshot_cached().unwrap();
    let system = MessageRecord::new(
        env.workspace_id.clone(),
        &snapshot.agents[0],
        "system text".to_owned(),
        DeliveryGate::Done,
    )
    .with_channel(Some("ops".to_owned()))
    .with_sender(MessageSender::System);
    env.store().queue_message(&system, "rimz-test").unwrap();
    let output = run_success(env.rimz().args(["message", "list"]), "empty lane");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "no messages in #main. 2 in 2 other lanes, 2 not yet delivered — rimz message list --all --status queued"
    );
    let output = run_success(
        env.rimz().args(["message", "list", "--system"]),
        "all traffic",
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "no messages in #main. 3 in 2 other lanes, 3 not yet delivered — rimz message list --all --status queued"
    );
    let output = run_success(
        env.rimz().args(["message", "list", "--status", "queued"]),
        "filtered lane",
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "no queued messages in #main — rimz message list --all shows every channel"
    );
    let output = run_success(
        env.rimz()
            .args(["message", "list", "--channel", "docs", "@claude"]),
        "filtered target",
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "no messages in #docs — rimz message list --all shows every channel"
    );
    assert!(list_message_ids(&env, &["message", "list", "--json"], None).is_empty());
}

#[test]
fn message_list_empty_lane_counts_delivered_rows_without_queue_hint() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(&env, "sess-docs", "docs", &[]);
    deliver_direct_channel_message(&env, "docs", "delivered text");
    let output = run_success(env.rimz().args(["message", "list"]), "empty lane");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "no messages in #main. 1 in 1 other lane — rimz message list --all"
    );
}

#[test]
fn message_list_validates_explicit_channels_but_not_ambient() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(&env, "sess-docs", "docs", &[]);
    seed_channel_message(&env, 1, 100, Some("old"), "old text");
    env.store()
        .archive_channel_messages("old", "archive", "rimz-test")
        .unwrap();
    for args in [
        vec!["message", "list", "--channel", "nope-zz"],
        vec!["message", "list", "@claude#nope-zz"],
    ] {
        let output = env.rimz().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            "error: no channel #nope-zz\n  known channels: docs, old"
        );
    }
    assert!(
        list_message_ids(
            &env,
            &["message", "list", "--channel", "old", "--json"],
            None
        )
        .is_empty()
    );
    assert!(list_message_ids(&env, &["message", "list", "--json"], Some("never-seen")).is_empty());
}

#[test]
fn message_list_inline_channel_matches_flag_and_all_keeps_other_rows() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(&env, "sess-docs", "docs", &[]);
    let docs = seed_channel_message(&env, 1, 100, Some("docs"), "docs text");
    let main = seed_channel_message(&env, 2, 200, None, "main text");
    let inline = list_message_ids(&env, &["message", "list", "@claude#docs", "--json"], None);
    assert_eq!(inline, vec![docs.clone()]);
    assert_eq!(
        inline,
        list_message_ids(
            &env,
            &["message", "list", "--channel", "docs", "@claude", "--json"],
            None
        )
    );
    assert_eq!(
        list_message_ids(
            &env,
            &["message", "list", "--all", "@claude#docs", "--json"],
            None
        ),
        vec![main, docs]
    );
    let mismatch = env
        .rimz()
        .args(["message", "list", "@claude#docs", "--channel", "ops"])
        .output()
        .unwrap();
    assert_eq!(mismatch.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("but channel flag names"));
}

#[test]
fn message_list_scopes_orders_and_limits_records() {
    let env = Env::new();
    env.record(&env.project_root);
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
fn message_list_root_address_correction_lists_the_agents_rows() {
    message_list_root_correction_case(false);
}

#[test]
fn message_list_main_from_root_shell_includes_root_routing_keys() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "sess-root",
        LifecycleSignal::Registered,
        |observation| {
            observation.launch.role = Some("reader".into());
            observation.agent_pid = Some(env.agent_owner_pid());
        },
    );
    let current = queue_add(&env, "@reader#main", "root message");
    let named = seed_channel_message(&env, 1, 100, Some("main"), "named main message");
    assert_eq!(
        list_message_ids(&env, &["message", "list", "--json"], Some("main")).as_slice(),
        std::slice::from_ref(&named)
    );
    for channel in ["main", "project", env.project_root.to_str().unwrap()] {
        let ids = list_message_ids(
            &env,
            &["message", "list", "--channel", channel, "--json"],
            None,
        );
        assert_eq!(ids, [current.clone(), named.clone()], "{channel}");
    }
    let rows = env.store().list_messages().unwrap();
    let root = rows
        .iter()
        .find(|row| row.message_id.as_str() == current)
        .unwrap();
    assert_eq!(root.channel.as_deref(), Some("project"));
    let miss = env
        .rimz()
        .args(["message", "list", "--channel", "missing"])
        .output()
        .unwrap();
    assert_eq!(miss.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&miss.stderr).trim(),
        "error: no channel #missing\n  known channels: main"
    );
}

#[test]
fn message_clear_root_aliases_cancel_the_same_records_as_list() {
    for scope in ["main", "project", "path", "caller"] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        append_lifecycle(
            &env,
            "claude",
            "SessionStart",
            "sess-root",
            LifecycleSignal::Registered,
            |observation| {
                observation.launch.role = Some("reader".into());
                observation.agent_pid = Some(env.agent_owner_pid());
            },
        );
        queue_add(&env, "@reader#main", "root message");
        seed_channel_message(&env, 1, 100, None, "legacy root message");
        seed_channel_message(&env, 2, 200, Some("main"), "named main message");
        let other = seed_channel_message(&env, 3, 300, Some("feature"), "other message");
        let mut command = env.rimz();
        command.args(["message", "clear"]);
        match scope {
            "caller" => {
                command
                    .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
                    .env(rimz::harness::launch::ENV_AGENT_ID, "sess-root");
            }
            "path" => {
                command.arg("--channel").arg(&env.project_root);
            }
            channel => {
                command.args(["--channel", channel]);
            }
        }
        let output = run_success(&mut command, "clear root lane");
        let rows = env.store().list_messages().unwrap();
        assert_eq!(rows.len(), 1, "{scope}: {rows:?}");
        assert_eq!(rows[0].message_id.as_str(), other);
        assert!(String::from_utf8_lossy(&output.stdout).contains("in #main"));
        let history = env.store().list_message_history().unwrap();
        assert_eq!(history.len(), 3);
        assert!(
            history
                .iter()
                .all(|row| row.status == MessageStatus::Canceled)
        );
    }
}

#[test]
fn message_list_root_flag_correction_lists_the_agents_rows() {
    message_list_root_correction_case(true);
}

fn message_list_root_correction_case(flag: bool) {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "sess-root",
        LifecycleSignal::Registered,
        |observation| {
            observation.launch.role = Some("reader".to_owned());
            observation.agent_pid = Some(env.agent_owner_pid());
        },
    );
    let legacy = seed_channel_message(&env, 1, 100, None, "legacy root message");
    let current = queue_add(&env, "@reader#main", "current root message");
    let mut command = env.rimz();
    command
        .env(rimz::workspace::ENV_CHANNEL, "scratch")
        .args(["message", "list", "@reader", "--json"]);
    if flag {
        seed_channel_message(&env, 2, 200, Some("scratch"), "another lane message");
        command.args(["--channel", "scratch"]);
    }
    let miss = command.output().unwrap();
    assert!(!miss.status.success());
    let stderr = String::from_utf8_lossy(&miss.stderr);
    let correction = stderr
        .lines()
        .find_map(|line| line.trim().strip_prefix("try: "))
        .unwrap_or_else(|| panic!("missing correction: {stderr}"));
    let argv = shlex::split(correction).unwrap();
    let output = run_success(
        env.rimz()
            .env(rimz::workspace::ENV_CHANNEL, "scratch")
            .args(&argv[1..]),
        "corrected root inbox",
    );
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let ids = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["message_id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids, [current.as_str(), legacy.as_str()], "{correction}");
    let digest = run_success(
        env.rimz()
            .env(rimz::workspace::ENV_CHANNEL, "scratch")
            .args(["message", "list", "@reader#main", "--status", "archived"]),
        "empty root inbox",
    );
    let text = String::from_utf8_lossy(&digest.stdout);
    assert!(text.contains("no archived messages in #main"), "{text}");
}

#[test]
fn message_list_target_follows_stamped_and_worktree_record_channels() {
    for stamped in [true, false] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        append_lifecycle(
            &env,
            "claude",
            "SessionStart",
            "sess-reader",
            LifecycleSignal::Registered,
            |observation| {
                observation.launch.role = Some("reader".to_owned());
                observation.agent_pid = Some(env.agent_owner_pid());
                if stamped {
                    observation.launch.channel = Some("docs".to_owned());
                } else {
                    observation.worktree_path =
                        Some(env.home_root.join("docs").display().to_string());
                }
            },
        );
        let id = queue_add(&env, "@reader#docs", "docs message");
        assert_eq!(
            list_message_ids(
                &env,
                &["message", "list", "@reader#docs", "--json"],
                Some("scratch"),
            ),
            [id],
            "stamped={stamped}"
        );
        let digest = run_success(
            env.rimz()
                .env(rimz::workspace::ENV_CHANNEL, "scratch")
                .args(["message", "list", "@reader#docs", "--status", "archived"]),
            "empty docs inbox",
        );
        let text = String::from_utf8_lossy(&digest.stdout);
        assert!(text.contains("no archived messages in #docs"), "{text}");
    }
}

#[test]
fn message_list_hides_system_traffic_unless_asked() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(&env, "sess-conversation", "docs", &[]);
    let human = queue_direct_channel_message(&env, "docs", "human conversation");
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = &snapshot.agents[0];
    let mut system_ids = Vec::new();
    for sender in [
        MessageSender::Harness {
            notice: HarnessNotice::Wait,
        },
        MessageSender::System,
    ] {
        let message = MessageRecord::new(
            env.workspace_id.clone(),
            agent,
            "system traffic".to_owned(),
            DeliveryGate::Done,
        )
        .with_channel(Some("docs".to_owned()))
        .with_sender(sender);
        env.store().queue_message(&message, "rimz-test").unwrap();
        system_ids.push(message.message_id.to_string());
    }
    queue_direct_channel_message(&env, "ops", "other lane");
    assert_eq!(
        list_message_ids(
            &env,
            &["message", "list", "--json", "--limit", "1"],
            Some("docs")
        ),
        vec![human.clone()]
    );
    let all = list_message_ids(
        &env,
        &["message", "list", "--system", "--json"],
        Some("docs"),
    );
    assert_eq!(all.len(), 3);
    assert!(all.contains(&human));
    assert!(system_ids.iter().all(|id| all.contains(id)));
    let conversation = list_message_ids(&env, &["message", "list", "--all", "--json"], None);
    assert_eq!(conversation.len(), 2);
    assert!(system_ids.iter().all(|id| !conversation.contains(id)));

    let digest = run_success(
        env.rimz()
            .env(rimz::workspace::ENV_CHANNEL, "docs")
            .args(["message", "list"]),
        "conversation digest",
    );
    let digest = String::from_utf8_lossy(&digest.stdout);
    assert!(digest.contains("human conversation"));
    assert!(!digest.contains("system traffic"));
    assert!(digest.contains("2 system messages hidden (--system shows them)"));
    let shown = run_success(
        env.rimz().args(["message", "show", &system_ids[0]]),
        "system audit",
    );
    assert!(String::from_utf8_lossy(&shown.stdout).contains("system traffic"));
}

#[test]
fn terminal_history_list_and_show_preserve_content_and_channel_fallback() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(&env, "sess-history", "docs", &[]);
    let message_id = deliver_direct_channel_message(&env, "docs", "kept body");

    let listed = run_success(
        env.rimz().args(["message", "list", "--all", "--json"]),
        "message list",
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

    let shown = run_success(
        env.rimz().args(["message", "show", &message_id]),
        "message show",
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
        DeliveryGate::Done,
    )
    .with_channel(Some("docs".to_owned()));
    message.status = MessageStatus::Delivered;
    message.delivered_at = Some(message.enqueued_at);
    let message_id = message.message_id.to_string();
    let event =
        EventEnvelope::message_event(&message, "rimz-test", MessageEventMethod::Delivered, None);
    env.store().append_event(&event).expect("append event");

    let shown = run_success(
        env.rimz().args(["message", "show", &message_id]),
        "textless message show",
    );
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.contains("(content in `rimz transcript @claude#docs`)"),
        "{shown}"
    );
}

#[test]
fn message_edit_and_requeue_enforce_record_lifecycle() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-edit", "edit-message", &[]);
    let queued = run_success(
        env.rimz()
            .args(["message", "--schedule", "60m", "@claude", "--", "old text"]),
        "scheduled message",
    );
    let message_id = queued_id_from_stdout(&queued.stdout);

    let edited = run_success(
        env.rimz().args([
            "message",
            "edit",
            &message_id,
            "--text",
            "new text",
            "--no-schedule",
            "--on",
            "any",
        ]),
        "message edit",
    );
    let stdout = String::from_utf8_lossy(&edited.stdout);
    assert!(stdout.contains(&format!("edited {message_id}")));
    assert!(stdout.contains("text, gate, schedule"));

    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].text, "new text");
    assert_eq!(pending[0].gate, DeliveryGate::Any);
    assert_eq!(pending[0].not_before, None);

    let shown = run_success(
        env.rimz().args(["message", "show", &message_id]),
        "message show",
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

    let open = env
        .rimz()
        .args(["message", "requeue", &message_id, "--text", "blocked"])
        .output()
        .expect("requeue open record");
    assert!(!open.status.success());
    assert!(String::from_utf8_lossy(&open.stderr).contains("still queued"));

    let message = message_by_id(&env, &MessageId::parse(&message_id).expect("message id"));
    let message = env
        .store()
        .claim_delivery_batch(
            &message.message_id,
            rimz::agents::AgentStatus::Idle,
            jiff::Timestamp::now(),
        )
        .expect("claim message")
        .expect("claimed")
        .remove(0);
    env.store()
        .record_send_error(&message, "terminal failure", "rimz-test")
        .expect("terminalize message");
    // A wake stamp that cannot be written must not fail a requeue whose copy is already durable,
    // or the user's retry queues a second copy.
    let wake_stamp = env.runtime_paths().lane_path("message-wake.json");
    let _ = std::fs::remove_file(&wake_stamp);
    std::fs::create_dir(&wake_stamp).expect("block the wake stamp");
    let requeued = run_success(
        env.rimz()
            .args(["message", "requeue", &message_id, "--text", "try again"]),
        "requeue terminal record",
    );
    assert!(String::from_utf8_lossy(&requeued.stdout).contains(&format!("(from {message_id})")));
    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_ne!(pending[0].message_id.as_str(), message_id);
    assert_eq!(pending[0].text, "try again");
    assert_eq!(pending[0].gate, DeliveryGate::Any);
    assert_eq!(pending[0].status, MessageStatus::Queued);
}

#[test]
fn receiver_end_archives_open_messages() {
    let env = Env::new();
    env.record(&env.project_root);
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

    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "child",
        LifecycleSignal::Registered,
        |observation| {
            observation.agent_name = Some("otter".to_owned());
            observation.launch.parent_agent_id = Some("sess-ended".into());
            observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
            observation.launch.launch_depth = Some(1);
        },
    );
    let child_message_id = queue_add(&env, "@otter", "child task");
    run_hook(
        &env,
        json!({ "hook_event_name": "SessionEnd", "session_id": "child" }),
        &[],
    );

    let archived = env
        .read_events()
        .into_iter()
        .find(|event| {
            event.method == "message.archived"
                && event.params_value()["message_id"] == child_message_id
        })
        .expect("child archived event");
    assert_eq!(
        archived.params_value()["reason"],
        "receiver ended; rimz message @otter#main resumes it"
    );
}

#[test]
fn watched_agent_end_archives_unmet_when_message() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_role_agent(&env, "claude", "sess-coder", "coder", false, None);
    register_role_agent(&env, "claude", "sess-planner", "planner", true, None);

    let queued = run_success(
        env.rimz().args([
            "message",
            "@coder",
            "--when",
            "@planner running 2h",
            "check planner",
        ]),
        "queue when message",
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
fn message_when_latches_met_dwell_and_schedules_future_trip() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let base = register_old_idle_role_agent(&env, "sess-coder", "coder", 120);

    let queued = run_success(
        env.rimz()
            .args(["message", "@coder", "--when", "@coder idle 1m", "ping"]),
        "queue self when",
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
        DeliveryGate::Done,
    )
    .with_when(vec![condition(60)]);
    let future = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "future".to_owned(),
        DeliveryGate::Done,
    )
    .with_when(vec![condition(3_600)]);
    env.store().queue_message(&due, "rimz-test").unwrap();
    env.store().queue_message(&future, "rimz-test").unwrap();
    let shown = run_success(
        env.rimz()
            .args(["message", "show", future.message_id.as_str()]),
        "show when blocker",
    );
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.contains("waiting for @coder idle ≥ 1h — idle"),
        "{shown}"
    );
    assert!(shown.contains("so far"), "{shown}");
    assert!(shown.contains("trips"), "{shown}");
    let listed = run_success(
        env.rimz().args(["message", "list", "--all"]),
        "list when blocker",
    );
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("when @coder idle 1h"),
        "{}",
        String::from_utf8_lossy(&listed.stdout)
    );
    let panes = env.write_pane_fixture(&[]);

    run_success(
        env.rimz()
            .env("RIMZ_TEST_PANE_LIST", panes)
            .args(["message", "sweep"]),
        "when sweep",
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
fn sweep_delivers_harness_wake_to_stamped_daemon_view_agent() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(
        &env,
        "sess-daemon-view",
        "feature-daemon-view",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let mut pane = agent_pane(&env, "claude");
    pane.view_name = Some("rimzd".to_owned());
    let pane_fixture = env.write_pane_fixture(&[pane]);
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let wake = MessageRecord::new(
        env.workspace_id.clone(),
        &snapshot.agents[0],
        "wait finished".to_owned(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Harness {
        notice: HarnessNotice::Wait,
    });
    env.store().queue_message(&wake, "rimz-test").unwrap();
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-daemon-view",
            "worktree_branch": "feature-daemon-view",
        }),
        &[("ZELLIJ_PANE_ID", "3")],
    );

    let trace_log = env.project_root.join("zellij-daemon-view-wake-trace.log");
    run_success(
        traced_rimz(&env, &trace_log)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "sweep"]),
        "sweep daemon-view wake",
    );

    assert_eq!(
        message_by_id(&env, &wake.message_id).status,
        MessageStatus::Sent
    );
    assert_text_then_enter(
        &trace_log,
        "Type: WAIT\nFrom: @rimz\nContent:\nwait finished",
    );
}

#[test]
fn sweep_records_missing_pane_blocker_without_losing_harness_wake_pin() {
    for pinned in [false, true] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        register_running_agent(
            &env,
            "sess-missing-pane",
            "feature-missing-pane",
            &[("ZELLIJ_PANE_ID", "3")],
        );
        let pane_fixture = env.write_pane_fixture(&[]);
        let snapshot = env.store().snapshot_cached().expect("snapshot");
        let mut wake = MessageRecord::new(
            env.workspace_id.clone(),
            &snapshot.agents[0],
            "wait finished".to_owned(),
            DeliveryGate::Done,
        )
        .with_sender(MessageSender::Harness {
            notice: HarnessNotice::Wait,
        });
        let blocker = if pinned {
            wake = wake.with_pane_id(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
            "stuck: pinned pane zellij:terminal_3 is not live"
        } else {
            "stuck: no live pane"
        };
        env.store().queue_message(&wake, "rimz-test").unwrap();
        run_hook(
            &env,
            json!({
                "hook_event_name": "Stop",
                "session_id": "sess-missing-pane",
                "worktree_branch": "feature-missing-pane",
            }),
            &[("ZELLIJ_PANE_ID", "3")],
        );

        let trace_log = env.project_root.join("zellij-missing-pane-wake-trace.log");
        run_success(
            traced_rimz(&env, &trace_log)
                .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
                .args(["message", "sweep"]),
            "sweep unbindable wake",
        );

        let queued = message_by_id(&env, &wake.message_id);
        assert_eq!(queued.status, MessageStatus::Queued);
        assert_eq!(queued.attempts, 0);
        assert_eq!(queued.pane_id, wake.pane_id);
        assert_eq!(queued.last_error.as_deref(), Some(blocker));
        assert!(trace_lines(&trace_log).is_empty());
        let shown = run_success(
            env.rimz().env("RIMZ_TEST_PANE_LIST", &pane_fixture).args([
                "message",
                "show",
                wake.message_id.as_str(),
            ]),
            "show unbindable wake",
        );
        let shown = String::from_utf8_lossy(&shown.stdout);
        assert!(shown.contains(blocker), "{shown}");
    }
}

#[test]
fn sweep_archives_an_ended_receiver_hidden_from_the_runtime_snapshot() {
    let env = Env::new();
    env.record(&env.project_root);
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "root",
        LifecycleSignal::Registered,
        |_| {},
    );
    let store = env.store();
    let receiver = store.snapshot_cached().unwrap().agents.remove(0);
    let message = MessageRecord::new(
        env.workspace_id.clone(),
        &receiver,
        "follow up".into(),
        DeliveryGate::Done,
    );
    store.queue_message(&message, "rimz-test").unwrap();
    append_lifecycle(
        &env,
        "claude",
        "rimz.agent-ended",
        "root",
        LifecycleSignal::Ended,
        |_| {},
    );
    assert!(store.snapshot_cached().unwrap().agents.is_empty());
    let panes = env.write_pane_fixture(&[]);
    let shown = run_success(
        env.rimz().env("RIMZ_TEST_PANE_LIST", &panes).args([
            "message",
            "show",
            message.message_id.as_str(),
        ]),
        "show ended receiver",
    );
    assert!(
        String::from_utf8_lossy(&shown.stdout).contains("has ended"),
        "{}",
        String::from_utf8_lossy(&shown.stdout)
    );
    run_success(
        env.rimz()
            .env("RIMZ_TEST_PANE_LIST", &panes)
            .args(["message", "sweep"]),
        "archive ended receiver",
    );
    let archived = store
        .list_message_history()
        .unwrap()
        .into_iter()
        .find(|row| row.message_id == message.message_id)
        .expect("ended root archived");
    assert_eq!(archived.status, MessageStatus::Archived);
    assert_eq!(archived.last_error.as_deref(), Some("receiver ended"));
}

#[test]
fn sweep_clears_a_recorded_no_pane_blocker_once_the_pane_returns() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    // Every rimz process here, hooks included, reaches the trace shim rather
    // than a live multiplexer; the empty trace at the end proves none tried.
    let trace_name = "zellij-pane-returns-trace.log";
    let trace = env.project_root.join(trace_name);
    let shim = zellij_trace_shim();
    let pane_env: &[(&str, &str)] = &[
        ("ZELLIJ_PANE_ID", "3"),
        ("RIMZ_ZELLIJ_BIN", shim.to_str().expect("shim path")),
        ("RIMZ_TEST_ZELLIJ_LOG", trace.to_str().expect("trace path")),
    ];
    register_running_agent(&env, "sess-pane-returns", "feature-pane-returns", pane_env);
    // The Stop hook opens the Done gate, so the missing pane is the first blocker the check reaches.
    // The wake is queued after it: a Stop with a queued head spawns a detached
    // `message deliver` the test cannot await, which would race the sweeps below.
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-pane-returns",
            "worktree_branch": "feature-pane-returns",
        }),
        pane_env,
    );
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let wake = MessageRecord::new(
        env.workspace_id.clone(),
        &snapshot.agents[0],
        "wait finished".to_owned(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Harness {
        notice: HarnessNotice::Wait,
    });
    env.store().queue_message(&wake, "rimz-test").unwrap();
    let no_panes = env.write_pane_fixture(&[]);
    run_success(
        traced_rimz(&env, trace_name)
            .env("RIMZ_TEST_PANE_LIST", &no_panes)
            .args(["message", "sweep"]),
        "sweep an unbindable wake",
    );
    assert_eq!(
        message_by_id(&env, &wake.message_id).last_error.as_deref(),
        Some("stuck: no live pane")
    );

    // The pane comes back while the agent is mid-turn: the blocker is now the closed gate.
    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-pane-returns",
            "prompt": "working again",
            "worktree_branch": "feature-pane-returns",
        }),
        pane_env,
    );
    let panes = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    run_success(
        traced_rimz(&env, trace_name)
            .env("RIMZ_TEST_PANE_LIST", &panes)
            .args(["message", "sweep"]),
        "sweep a gated wake",
    );

    let queued = message_by_id(&env, &wake.message_id);
    assert_eq!(queued.status, MessageStatus::Queued);
    assert_eq!(queued.last_error, None);
    let shown = run_success(
        traced_rimz(&env, trace_name)
            .env("RIMZ_TEST_PANE_LIST", &panes)
            .args(["message", "show", wake.message_id.as_str()]),
        "show a gated wake",
    );
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(!shown.contains("stuck:"), "{shown}");
    assert!(shown.contains("waiting:"), "{shown}");
    let calls = trace_lines(&trace);
    assert!(calls.is_empty(), "no path may reach zellij: {calls:?}");
}

#[test]
fn resume_prompt_wakes_and_sends_after_registration_without_a_stop() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    crate::common::wait::register_calling_agent(&env);
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[agents]\nisolation = 'host'\n",
    )
    .unwrap();
    for signal in [LifecycleSignal::Registered, LifecycleSignal::Ended] {
        append_lifecycle(
            &env,
            "claude",
            "test",
            "resume-session",
            signal,
            |observation| {
                observation.agent_name = Some("resume-leader".into());
            },
        );
    }
    let workspace = env.resolve_workspace(&env.project_root);
    let shim = crate::common::write_env_dump_shim(&env, "claude");
    run_success(
        traced_rimz(&env, "resume-launch.log")
            .current_dir(&env.project_root)
            .env("PATH", crate::common::path_with_front(&shim))
            .env("ZELLIJ_PANE_ID", "1")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{} [Created 1s ago]\n", workspace.session_name),
            )
            .args([
                "--mux",
                "zellij",
                "agents",
                "claude",
                "--resume",
                "--new-tab",
                "say hi",
            ]),
        "resume with prompt",
    );
    let wake: Option<jiff::Timestamp> = serde_json::from_slice(
        &std::fs::read(wake_stamp_path(&env)).expect("resume must arm the message sweep"),
    )
    .unwrap();
    assert!(wake.is_some_and(|time| time <= jiff::Timestamp::now()));
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let trace = env.project_root.join("resume-send.log");
    run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "sweep"]),
        "sweep before registration",
    );
    assert!(
        trace_lines(&trace).is_empty(),
        "must not send before registration"
    );
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "resume-session",
        LifecycleSignal::Registered,
        |observation| {
            observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
            observation.runtime_owner = Some(rimz::pane::RuntimeOwner::new(
                rimz::pane::RuntimeOwnerKind::Agent,
                "resume-session",
                std::process::id(),
                None,
            ));
        },
    );
    run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "sweep"]),
        "sweep after registration, without Stop",
    );
    assert_text_then_enter(&trace, &user_message("say hi"));
    let messages = env.store().list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].status, MessageStatus::Sent);
    assert_eq!(messages[0].sender, MessageSender::Human);
}

#[test]
fn host_sweep_delivers_from_the_room_store_without_the_pane_env() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-host-sweep", "feature-host-sweep", pane_env);
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-host-sweep",
            "worktree_branch": "feature-host-sweep",
        }),
        pane_env,
    );
    let store = env.store();
    let snapshot = store.snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-host-sweep")
        .expect("agent");
    let due_at = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1);
    let due = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "host sweep delivery".to_owned(),
        DeliveryGate::Done,
    )
    .with_not_before(Some(due_at));
    store
        .queue_message(&due, "rimz-test")
        .expect("queue due message");
    rimz::disk::atomic::write_temp_then_rename_cache(&wake_stamp_path(&env), &Some(due_at))
        .expect("write wake stamp");
    let shared_root = env.runtime_paths().shared_root;
    std::fs::create_dir_all(&shared_root).expect("mkdir shared runtime root");
    let trace_log = env.project_root.join("host-sweep-trace.log");
    run_success(
        traced_rimz(&env, &trace_log)
            .current_dir(&shared_root)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args([
                "--mux",
                "zellij",
                "message",
                "sweep",
                "--workspace-id",
                env.workspace_id.as_str(),
            ]),
        "host message sweep",
    );
    assert_text_then_enter(&trace_log, &user_message("host sweep delivery"));
    let messages = store.list_messages().expect("messages");
    let sent = messages
        .iter()
        .find(|message| message.message_id == due.message_id)
        .expect("swept message");
    assert_eq!(sent.status, MessageStatus::Sent);
    let wait: Option<jiff::Timestamp> =
        serde_json::from_slice(&std::fs::read(wake_stamp_path(&env)).expect("wake stamp"))
            .expect("wake stamp json");
    assert_eq!(wait, sent.sent_reconcile_deadline());
    let workspaces: Vec<_> = std::fs::read_dir(env.rimz_home().join("ws"))
        .expect("workspace directories")
        .map(|entry| entry.expect("workspace entry").path())
        .collect();
    assert_eq!(workspaces, vec![store.paths().root.clone()]);
}

#[test]
fn sweep_without_a_room_refuses_and_creates_nothing() {
    let env = Env::new();
    let shared_root = env.runtime_paths().shared_root;
    std::fs::create_dir_all(&shared_root).expect("mkdir shared runtime root");
    let output = env
        .rimz()
        .current_dir(&shared_root)
        .args(["message", "sweep"])
        .output()
        .expect("message sweep");
    assert!(!output.status.success(), "a sweep must not create a room");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no room at"), "{stderr}");
    assert!(stderr.contains("nothing to sweep"), "{stderr}");
    let workspaces = env.rimz_home().join("ws");
    assert!(
        !workspaces.exists()
            || std::fs::read_dir(&workspaces)
                .expect("workspace entries")
                .next()
                .is_none(),
        "a sweep must not create workspace state"
    );
}

#[test]
fn scheduled_message_parks_and_sweep_delivers_due_work() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-scheduled", "feature-scheduled", pane_env);
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);

    let before = jiff::Timestamp::now();
    let scheduled = run_success(
        env.rimz().env("RIMZ_TEST_PANE_LIST", &pane_fixture).args([
            "message",
            "--schedule",
            "60m",
            "@claude",
            "later",
        ]),
        "scheduled message",
    );
    let scheduled_id = queued_id_from_stdout(&scheduled.stdout);
    let receipt = String::from_utf8_lossy(&scheduled.stdout);
    assert!(receipt.starts_with(&format!(
        "queued for @claude#feature-scheduled ({scheduled_id}) — opens in "
    )));

    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].pane_id, None, "delivery re-resolves the pane");
    let future_id = pending[0].message_id.clone();
    assert_eq!(future_id.as_str(), scheduled_id);
    let not_before = pending[0].not_before.expect("scheduled timestamp");
    assert!(receipt.contains(&not_before.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()));
    assert!(not_before > before);
    assert!(not_before <= before + jiff::SignedDuration::from_secs(61 * 60));

    let listed = run_success(
        env.rimz().args(["message", "list", "--all", "--json"]),
        "message list",
    );
    let parsed: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("json");
    assert!(parsed[0]["not_before"].is_string());

    let wait: Option<jiff::Timestamp> =
        serde_json::from_slice(&std::fs::read(wake_stamp_path(&env)).expect("wake stamp"))
            .expect("wake stamp json");
    assert_eq!(wait, Some(not_before));

    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-scheduled",
            "worktree_branch": "feature-scheduled",
        }),
        pane_env,
    );
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-scheduled")
        .expect("agent");
    let due_at = jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1);
    let due = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "due now".to_owned(),
        DeliveryGate::Done,
    )
    .with_not_before(Some(due_at));
    let due_id = due.message_id.clone();
    env.store()
        .queue_message(&due, "rimz-test")
        .expect("queue due message");
    rimz::disk::atomic::write_temp_then_rename_cache(&wake_stamp_path(&env), &Some(due_at))
        .expect("write wake stamp");

    let trace_log = env.project_root.join("zellij-sweep-trace.log");
    run_success(
        traced_rimz(&env, "zellij-sweep-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "sweep"]),
        "message sweep",
    );
    assert_text_then_enter(&trace_log, &user_message("due now"));
    let messages = env.store().list_messages().expect("messages");
    let future = messages
        .iter()
        .find(|message| message.message_id == future_id)
        .expect("future message");
    assert_eq!(future.status, MessageStatus::Queued);
    assert_eq!(future.pane_id, None);
    let sent = messages
        .iter()
        .find(|message| message.message_id == due_id)
        .expect("swept message");
    assert_eq!(sent.status, MessageStatus::Sent);
    let wait: Option<jiff::Timestamp> =
        serde_json::from_slice(&std::fs::read(wake_stamp_path(&env)).expect("wake stamp"))
            .expect("wake stamp json");
    assert_eq!(wait, sent.sent_reconcile_deadline());
}

#[test]
fn a_bare_sweep_from_shared_root_uses_the_host_pin_without_a_phantom_workspace() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-host-pin", "feature-host-pin", pane_env);
    run_hook(
        &env,
        json!({"hook_event_name": "Stop", "session_id": "sess-host-pin", "worktree_branch": "feature-host-pin"}),
        pane_env,
    );
    let store = env.store();
    let snapshot = store.snapshot_cached().unwrap();
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-host-pin")
        .unwrap();
    let due = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "due from host".to_owned(),
        DeliveryGate::Done,
    )
    .with_not_before(Some(
        jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1),
    ));
    store.queue_message(&due, "rimz-test").unwrap();
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().unwrap();
    let trace = env.project_root.join("host-pin-sweep.log");
    let mut command = traced_rimz(&env, &trace);
    command
        .current_dir(&runtime.shared_root)
        .envs(rimz::workspace::pin_env(
            &env.workspace_id,
            &env.project_root,
        ))
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["message", "sweep"]);
    run_success(&mut command, "bare host sweep");
    let roots: Vec<_> = std::fs::read_dir(env.rimz_home().join("ws"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(
        roots,
        vec![store.paths().root.clone()],
        "the helper must not create a phantom workspace from shared_root"
    );
    assert_eq!(
        message_by_id(&env, &due.message_id).status,
        MessageStatus::Sent
    );
    assert_text_then_enter(&trace, &user_message("due from host"));
}

#[test]
fn message_after_rejects_cycles_then_delivers_cross_agent_relay() {
    let env = Env::new();
    env.record(&env.project_root);
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

    let self_reference = env
        .rimz()
        .args(["message", "@coder", "--after", "@coder", "x"])
        .output()
        .expect("self reference");
    assert!(!self_reference.status.success());
    assert!(String::from_utf8_lossy(&self_reference.stderr).contains("use --on"));

    let fanout = env
        .rimz()
        .args(["message", "@coder", "--after", "@all", "x"])
        .output()
        .expect("after fanout");
    assert!(!fanout.status.success());
    assert!(String::from_utf8_lossy(&fanout.stderr).contains("broadcasts are not supported"));

    let add = run_success(
        env.rimz()
            .args(["message", "@coder", "--after", "@planner", "read plan.md"]),
        "queue relay",
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
            turn_id: None,
        },
        |_| {},
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let trace_log = env.project_root.join("zellij-after-sweep-trace.log");
    run_success(
        traced_rimz(&env, "zellij-after-sweep-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "sweep"]),
        "sweep relay",
    );

    assert_text_then_enter(&trace_log, &user_message("read plan.md"));
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
fn resume_gate_waits_for_recovery_then_delivers() {
    let env = Env::new();
    env.record(&env.project_root);
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
        DeliveryGate::Resume,
    )
    .with_pane_id(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
    let message_id = message.message_id.clone();
    env.store()
        .queue_message(&message, "rimz-test")
        .expect("queue resume message");

    let trace_log = env.project_root.join("zellij-resume-ready-trace.log");
    run_success(
        traced_rimz(&env, "zellij-resume-ready-trace.log")
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
        traced_rimz(&env, "zellij-resume-ready-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "deliver", "--message-id", message_id.as_str()]),
        "recovered resume delivery",
    );

    assert_text_then_enter(&trace_log, &user_message("continue"));
    assert_eq!(message_by_id(&env, &message_id).status, MessageStatus::Sent);
}

#[test]
fn resume_gate_failed_park_waits_for_recovery_then_delivers() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-failed-resume", "feature-resume", pane_env);
    append_lifecycle(
        &env,
        "claude",
        "Stop",
        "sess-failed-resume",
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
            turn_id: None,
        },
        |_| {},
    );
    seed_turn_error(&env, "sess-failed-resume", TurnErrorClass::PausedRateLimit);
    seed_rate_limit_budget(&env, 100);
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-failed-resume")
        .expect("agent");
    assert_eq!(agent.status, rimz::agents::AgentStatus::Failed);
    let message = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "continue".to_owned(),
        DeliveryGate::Resume,
    )
    .with_pane_id(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
    let message_id = message.message_id.clone();
    env.store()
        .queue_message(&message, "rimz-test")
        .expect("queue resume message");

    let trace_log = env.project_root.join("zellij-failed-resume-trace.log");
    run_success(
        traced_rimz(&env, "zellij-failed-resume-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "deliver", "--message-id", message_id.as_str()]),
        "deferred failed-park resume delivery",
    );
    assert!(trace_lines(&trace_log).is_empty());
    let queued = message_by_id(&env, &message_id);
    assert_eq!(queued.status, MessageStatus::Queued);
    assert_eq!(queued.attempts, 0);

    seed_rate_limit_budget(&env, 20);
    run_success(
        traced_rimz(&env, "zellij-failed-resume-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "deliver", "--message-id", message_id.as_str()]),
        "recovered failed-park resume delivery",
    );

    assert_eq!(message_by_id(&env, &message_id).status, MessageStatus::Sent);
    assert_text_then_enter(&trace_log, &user_message("continue"));
}

#[test]
fn auto_continue_queues_a_pinned_system_resume_then_defers_on_a_closed_gate() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(
        &env,
        "sess-resume-ready",
        "feature-resume",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_turn_error(&env, "sess-resume-ready", TurnErrorClass::PausedRateLimit);
    seed_rate_limit_budget(&env, 100);
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let request = rimz::harness::AutoContinueRequest {
        workspace_id: env.workspace_id.clone(),
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from("sess-resume-ready"),
        pane_id: PaneId::from_parts(MuxName::Zellij, TRACE_PANE),
        message_id: None,
        parked_since: jiff::Timestamp::now(),
        text: "continue".to_owned(),
        reason: "overloaded_backoff_retry".to_owned(),
        label: None,
    };
    run_success(
        env.rimz().env("RIMZ_TEST_PANE_LIST", &pane_fixture).args(
            rimz::child_process::agent_helper_argv("auto-continue", &request),
        ),
        "deferred auto-continue",
    );
    let pending = env.store().list_pending_messages().expect("pending resume");
    assert_eq!(pending.len(), 1);
    let message = &pending[0];
    assert_eq!(message.agent_id, request.agent_id);
    assert_eq!(message.sender, MessageSender::System);
    assert_eq!(message.gate, DeliveryGate::Resume);
    assert_eq!(message.pane_id, None);
    let queued = env
        .read_events()
        .into_iter()
        .filter(|event| event.method == "message.queued")
        .map(|event| {
            serde_json::from_str::<rimz::store::event::MessageEventPayload>(event.params.get())
                .expect("queued message payload")
        })
        .find(|event| event.message_id == message.message_id)
        .expect("queued resume event");
    assert_eq!(queued.pane_id.as_ref(), Some(&request.pane_id));
    assert_eq!(message.text, "continue");
    assert!(message.enter);
    assert_eq!(message.status, MessageStatus::Queued);
    assert_eq!(
        message.last_error.as_deref(),
        Some("resume delivery gate closed (overloaded_backoff_retry)"),
    );

    let request = rimz::harness::AutoContinueRequest {
        reason: "budget_day_reset".to_owned(),
        text: "day reset".to_owned(),
        ..request
    };
    run_success(
        env.rimz().env("RIMZ_TEST_PANE_LIST", &pane_fixture).args(
            rimz::child_process::agent_helper_argv("auto-continue", &request),
        ),
        "deferred day-reset auto-continue",
    );
    let pending = env
        .store()
        .list_pending_messages()
        .expect("pending messages");
    assert_eq!(pending.len(), 2);
    let message = pending
        .iter()
        .find(|message| message.text == "day reset")
        .expect("day-reset message");
    assert_eq!(message.gate, DeliveryGate::Done);
}

#[test]
fn idle_stop_holds_while_a_message_is_owed_then_stops_without_canceling_the_run() {
    use rimz::harness::idle_stop::IdleStopHelperRequest;
    use rimz::store::run::{RunRecord, RunStatus};
    for owed in [Some("message"), Some("wait"), Some("collision"), None] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        let pane_id = PaneId::from_parts(MuxName::Zellij, TRACE_PANE);
        for (event, signal) in [
            ("SessionStart", LifecycleSignal::Registered),
            (
                "UserPromptSubmit",
                LifecycleSignal::TurnStarted { turn_id: None },
            ),
            (
                "Stop",
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: false,
                    turn_id: None,
                },
            ),
        ] {
            append_lifecycle(
                &env,
                "claude",
                event,
                "provider-session",
                signal,
                |observation| {
                    observation.pane_id = Some(pane_id.clone());
                },
            );
        }
        let store = env.store();
        let kind = AgentKind::new_unchecked("claude");
        let mut run = RunRecord::new(
            env.workspace_id.clone(),
            kind.clone(),
            rimz::agents::PermissionMode::Auto,
            "task".to_owned(),
            env.project_root.clone(),
        );
        run.agent_id = Some("provider-session".into());
        run.status = RunStatus::Completed;
        rimz::harness::run::create(store.paths(), &run).expect("create run");
        let session = env.resolve_workspace(&env.project_root).session_name;
        let mut pane = agent_pane(&env, "claude");
        pane.session_name = session.clone();
        let fixture = env.write_pane_fixture(std::slice::from_ref(&pane));
        let frame = rimz::sidebar::frame::assemble_frame(
            vec![pane],
            rimz::utils::time::unix_now_ms(),
            session.clone(),
        );
        rimz::disk::atomic::write_temp_then_rename_cache(
            &store.runtime_paths().pane_frame_path(),
            &frame,
        )
        .unwrap();

        let stop = |args: &[&str]| {
            let output = traced_rimz(&env, "idle-stop-trace.log")
                .env("RIMZ_TEST_PANE_LIST", &fixture)
                .args(["agents", "stop"])
                .args(args)
                .output()
                .expect("agents stop --when-idle");
            (
                output.status.success(),
                String::from_utf8_lossy(&output.stdout).into_owned()
                    + &String::from_utf8_lossy(&output.stderr),
            )
        };
        assert_eq!(
            stop(&["@claude", "--when-idle", "off"]),
            (true, "@claude#main has no pending idle stop\n".to_owned())
        );
        assert_eq!(
            stop(&["@claude", "--when-idle"]),
            (
                true,
                "@claude#main stops once idle for 3m with nothing owed\n".to_owned()
            )
        );
        let shown = run_success(
            env.rimz()
                .env("RIMZ_TEST_PANE_LIST", &fixture)
                .args(["agents", "show", "@claude", "--json"]),
            "agents show",
        );
        let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(shown["agent"]["idle_stop"]["after_secs"], 180, "{shown}");
        assert!(
            shown["agent"]["idle_stop"]["due_at"].is_string(),
            "a resting agent's clock runs: {shown}"
        );
        assert_eq!(
            stop(&["@claude", "--when-idle", "0s"]),
            (
                true,
                "@claude#main stops once idle for 0s with nothing owed (replaces the pending 3m request)\n"
                    .to_owned()
            )
        );
        let (ok, refusal) = stop(&[run.run_id.as_str(), "--when-idle"]);
        assert!(
            !ok && refusal.contains("names a run") && refusal.contains("rimz agents stop"),
            "{refusal}"
        );
        let pending = rimz::store::idle_stop::read(store.paths());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].stop.after_secs, 0);

        let mut collision = None;
        let as_agent = |args: &[&str]| {
            run_success(
                env.rimz()
                    .env("RIMZ_TEST_PANE_LIST", &fixture)
                    .env("RIMZ_AGENT_KIND", "claude")
                    .env("RIMZ_AGENT_ID", "provider-session")
                    .args(args),
                "wait",
            );
        };
        match owed {
            Some("message") => {
                let agent = store.snapshot_cached().unwrap().agents.remove(0);
                let message = MessageRecord::new(
                    env.workspace_id.clone(),
                    &agent,
                    "rebase first".to_owned(),
                    DeliveryGate::Done,
                );
                store.queue_message(&message, "idle-stop-test").unwrap();
            }
            Some("wait") => as_agent(&["wait", "--in", "30m"]),
            // Another kind's newer run under this agent's name is the run a
            // stop of this agent would select and cancel.
            Some(_) => {
                let agent = store.snapshot_cached().unwrap().agents.remove(0);
                let mut other = RunRecord::new(
                    env.workspace_id.clone(),
                    AgentKind::new_unchecked("codex"),
                    rimz::agents::PermissionMode::Auto,
                    "task".to_owned(),
                    env.project_root.clone(),
                );
                other.agent_name = agent.name;
                other.started_at = run.started_at + std::time::Duration::from_secs(60);
                other.status = RunStatus::Running;
                rimz::harness::run::create(store.paths(), &other).expect("create run");
                collision = Some(other);
            }
            None => {}
        }
        let helper = || {
            let mut helper = traced_rimz(&env, "idle-stop-trace.log");
            // The stop picks its backend by the room's live session.
            helper
                .env(
                    "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                    format!("{session} [Created 1s ago]\n"),
                )
                .env("RIMZ_TEST_PANE_LIST", &fixture)
                .args(rimz::child_process::agent_helper_argv(
                    "idle-stop",
                    &IdleStopHelperRequest {
                        workspace_id: env.workspace_id.clone(),
                        kind: kind.clone(),
                        agent_id: "provider-session".into(),
                        pane_id: pane_id.clone(),
                        label: "@claude".into(),
                    },
                ));
            helper
        };
        let idle_stop_assists = || {
            let output = run_success(env.rimz().args(["stats", "--json"]), "assist stats");
            let stats: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let assists = stats["assists"]["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|event| event["assist"] == "idle_stop")
                .cloned()
                .collect::<Vec<_>>();
            (stats, assists)
        };
        if owed.is_none() {
            // The pane is still listed and its close fails: nothing stopped,
            // so the request stays armed for the producer's next ask.
            let failed = helper()
                .env("RIMZ_TEST_ZELLIJ_FAIL_CLOSE_PANE", "1")
                .env(
                    "RIMZ_TEST_ZELLIJ_LIST_PANES",
                    r#"[{"id":3,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
                )
                .output()
                .expect("idle stop with a failing close");
            assert!(!failed.status.success());
            assert_eq!(rimz::store::idle_stop::read(store.paths()), pending);
            let (_, assists) = idle_stop_assists();
            assert_eq!(assists.len(), 1, "{assists:?}");
            assert_eq!(assists[0]["stopped"], false, "{assists:?}");
            let error = assists[0]["error"].as_str().unwrap();
            assert!(
                error.contains(TRACE_PANE) && error.contains("rerun the stop"),
                "{error}"
            );
        }
        run_success(&mut helper(), "idle stop");
        assert_eq!(
            rimz::harness::run::load(store.paths(), &run.run_id).unwrap(),
            run,
            "a soft stop never rewrites a settled run"
        );
        let (stats, assists) = idle_stop_assists();
        if let Some(owed) = owed {
            assert_eq!(
                rimz::store::idle_stop::read(store.paths()),
                pending,
                "{owed}"
            );
            assert!(
                assists.is_empty(),
                "a declined stop is no assist ({owed}): {assists:?}"
            );
            if owed == "wait" {
                as_agent(&["wait", "cancel", "--all"]);
            }
            if let Some(other) = &collision {
                assert_eq!(
                    &rimz::harness::run::load(store.paths(), &other.run_id).unwrap(),
                    other,
                    "the conflicting run is not canceled"
                );
            }
            assert_eq!(
                stop(&["@claude", "--when-idle", "off"]),
                (
                    true,
                    "withdrew the pending idle stop for @claude#main\n".to_owned()
                )
            );
            assert!(rimz::store::idle_stop::read(store.paths()).is_empty());
            continue;
        }
        assert!(
            rimz::store::idle_stop::read(store.paths()).is_empty(),
            "the stop retires the session's request"
        );
        assert_eq!(assists.len(), 2, "{assists:?}");
        let stopped = assists
            .iter()
            .find(|assist| assist["stopped"] == true)
            .unwrap_or_else(|| panic!("no stopped assist: {assists:?}"));
        assert!(stopped.get("error").is_none(), "{stopped}");
        assert_eq!(stopped["label"], "@claude");
        assert_eq!(stopped["idle_after_secs"], 0);
        assert_eq!(stats["assists"]["rollup"]["idle_stops"], 1);
        let trace = std::fs::read_to_string(env.project_root.join("idle-stop-trace.log")).unwrap();
        assert_eq!(trace.matches("close-pane").count(), 2, "{trace}");
    }
}

/// A stop says which pane closed: a root whose close fails was not stopped and
/// keeps its idle-stop request; a root that closed while its child failed was
/// stopped and retired, and the command still fails naming the child.
#[test]
fn stop_reports_a_pane_left_open_and_a_child_that_failed() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let register = |id: &'static str, parent: Option<&'static str>| {
        append_lifecycle(
            &env,
            "claude",
            "SessionStart",
            id,
            LifecycleSignal::Registered,
            |observation| {
                observation.agent_name = Some(id.to_owned());
                let Some(parent) = parent else {
                    observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
                    return;
                };
                observation.launch.parent_agent_id = Some(parent.into());
                observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
                observation.launch.launch_depth = Some(1);
            },
        );
    };
    register("parent", None);
    register("child", Some("parent"));
    let stop = |args: &[&str], fail_close: bool| {
        let mut command = traced_rimz(&env, "stop-trace.log");
        if fail_close {
            command.env("RIMZ_TEST_ZELLIJ_FAIL_CLOSE_PANE", "1").env(
                "RIMZ_TEST_ZELLIJ_LIST_PANES",
                r#"[{"id":3,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
            );
        }
        let output = command
            .args(["--mux", "zellij", "agents", "stop"])
            .args(args)
            .output()
            .expect("agents stop");
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned()
                + &String::from_utf8_lossy(&output.stderr),
        )
    };
    let armed = || rimz::store::idle_stop::read(env.store().paths()).len();
    assert!(stop(&["@parent", "--when-idle"], false).0);

    let (ok, text) = stop(&["@parent"], true);
    assert!(!ok, "{text}");
    assert!(text.contains("@parent#main was not stopped: "), "{text}");
    assert!(text.contains("close-pane"), "{text}");
    assert_eq!(armed(), 1, "a root left open keeps its request");

    let (ok, text) = stop(&["@parent", "--all"], true);
    assert!(!ok, "{text}");
    assert!(text.contains("error @parent#main: "), "{text}");
    assert!(!text.contains("stopped @parent"), "{text}");
    assert_eq!(armed(), 1);

    let (ok, text) = stop(&["@parent"], false);
    assert!(!ok, "{text}");
    assert!(text.contains("@parent#main stopped, but: "), "{text}");
    assert!(
        text.contains("@child") && text.contains("has no bound pane"),
        "{text}"
    );
    assert_eq!(armed(), 0, "a root that closed is retired");
}

/// An orphan whose run pane stays open is not stamped ended: the repair
/// fails and the next sweep finds it again.
#[test]
fn orphan_repair_records_no_end_while_the_run_pane_stays_open() {
    use rimz::store::run::{RunRecord, RunStatus};
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let kind = AgentKind::new_unchecked("claude");
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "child",
        LifecycleSignal::Registered,
        |observation| {
            observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
            observation.launch.parent_agent_id = Some("gone".into());
            observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
            observation.launch.launch_depth = Some(1);
        },
    );
    let store = env.store();
    let mut run = RunRecord::new(
        env.workspace_id.clone(),
        kind.clone(),
        rimz::agents::PermissionMode::Auto,
        "task".to_owned(),
        env.project_root.clone(),
    );
    run.agent_id = Some("child".into());
    run.status = RunStatus::Completed;
    run.subagent = true;
    rimz::harness::run::create(store.paths(), &run).expect("create run");
    let session = env.resolve_workspace(&env.project_root).session_name;

    // The helper finds the store by workspace id, which needs the committed
    // workspace record a store-writing command leaves.
    run_success(
        env.rimz().args(["events", "emit", "orphan.test"]),
        "events emit",
    );
    let request = rimz::harness::orphan_sweep::OrphanSubagentRequest {
        workspace_id: env.workspace_id.clone(),
        child_kind: kind,
        child_agent_id: "child".into(),
        parent_agent_id: "gone".into(),
    };
    let output = traced_rimz(&env, "orphan-trace.log")
        .env("RIMZ_TEST_SUBAGENT_ORPHAN_GRACE_MS", "0")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{session} [Created 1s ago]\n"),
        )
        .env("RIMZ_TEST_ZELLIJ_FAIL_CLOSE_PANE", "1")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            r#"[{"id":3,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
        )
        .args(rimz::child_process::agent_helper_argv(
            "orphan-subagent",
            &request,
        ))
        .output()
        .expect("orphan repair");

    assert!(
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let agents = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert_eq!(agents.len(), 1);
    assert!(agents[0].ended_at.is_none(), "{:?}", agents[0].ended_at);
    let failed = env
        .diag_records(&session)
        .into_iter()
        .find_map(|record| match record.event {
            rimz::diag::record::DiagEvent::SubagentOrphanRepairFailed { error, .. } => Some(error),
            _ => None,
        })
        .expect("a repair-failed diagnostic");
    assert!(failed.contains("is still open"), "{failed}");
}

#[test]
fn orphan_repair_records_one_end_with_and_without_a_run() {
    use rimz::store::run::{RunRecord, RunStatus};

    for with_run in [false, true] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        append_lifecycle(
            &env,
            "claude",
            "SessionStart",
            "child",
            LifecycleSignal::Registered,
            |observation| {
                observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
                observation.launch.parent_agent_id = Some("gone".into());
                observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
                observation.launch.launch_depth = Some(1);
            },
        );
        let store = env.store();
        let kind = AgentKind::new_unchecked("claude");
        if with_run {
            let mut run = RunRecord::new(
                env.workspace_id.clone(),
                kind.clone(),
                rimz::agents::PermissionMode::Auto,
                "task".to_owned(),
                env.project_root.clone(),
            );
            run.agent_id = Some("child".into());
            run.status = RunStatus::Completed;
            run.subagent = true;
            rimz::harness::run::create(store.paths(), &run).unwrap();
        }
        run_success(
            env.rimz().args(["events", "emit", "orphan.test"]),
            "events emit",
        );
        let session = env.resolve_workspace(&env.project_root).session_name;
        let request = rimz::harness::orphan_sweep::OrphanSubagentRequest {
            workspace_id: env.workspace_id.clone(),
            child_kind: kind,
            child_agent_id: "child".into(),
            parent_agent_id: "gone".into(),
        };
        let output = traced_rimz(&env, "orphan-trace.log")
            .env("RIMZ_TEST_SUBAGENT_ORPHAN_GRACE_MS", "0")
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{session} [Created 1s ago]\n"),
            )
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
            .args(rimz::child_process::agent_helper_argv(
                "orphan-subagent",
                &request,
            ))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let agents = store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents;
        assert!(agents[0].ended_at.is_some(), "{agents:?}");
        let events = env.read_events();
        let ends = events
            .iter()
            .filter_map(|event| match event.kind() {
                rimz::store::event::EventKind::AgentLifecycle(payload)
                    if payload.observation.signal == LifecycleSignal::Ended =>
                {
                    Some(payload)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(ends.len(), 1, "{events:?}");
        assert_eq!(
            ends[0].event_name.as_deref(),
            Some(if with_run {
                "rimz.subagent-stopped"
            } else {
                "rimz.subagent-orphan-reaped"
            })
        );
        assert!(env.diag_records(&session).iter().any(|record| matches!(
            record.event,
            rimz::diag::record::DiagEvent::SubagentOrphanReaped { .. }
        )));
    }
}

#[test]
fn cache_keepalive_rechecks_and_terminalizes_a_miss_with_an_assist() {
    use crate::common::wait::{register_calling_agent, wait_ok};
    use rimz::harness::cache_keepalive::CacheKeepaliveRequest;
    // The capped case's real request sits 220s before a keepalive turn that
    // anchors the ping: under a 4m maximum a ping 1s after that turn would
    // start at 221s and is allowed, and a helper 25s past the anchor (246s)
    // delivers it as the last one.
    for (blocked, capped) in [(false, false), (true, false), (false, true)] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        register_calling_agent(&env);
        wait_ok(
            &env,
            &["config", "set", "harness.prompt_cache_ttl.claude", "61s"],
        );
        if capped {
            wait_ok(
                &env,
                &["config", "set", "harness.cache_keepalive_max", "4m"],
            );
        }
        wait_ok(&env, &["wait", "--in", "5m"]);
        let store = env.store();
        let anchor = jiff::Timestamp::now() - Duration::from_secs(if capped { 25 } else { 0 });
        let turn = |offset: i64, prompt: Option<&str>| {
            (
                offset,
                prompt.map(str::to_owned),
                LifecycleSignal::TurnStarted { turn_id: None },
            )
        };
        let ended = |offset: i64| {
            (
                offset,
                None,
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: false,
                    turn_id: None,
                },
            )
        };
        let turns = if capped {
            vec![
                turn(-220, None),
                ended(-220),
                turn(
                    0,
                    Some(
                        "Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nCache keepalive, no action needed.",
                    ),
                ),
                ended(0),
            ]
        } else {
            vec![turn(0, None), ended(0)]
        };
        for (offset, prompt, signal) in turns {
            let mut observation =
                AgentLifecycleObservation::new(Some("provider-session".into()), signal);
            observation.prompt = rimz::agents::SanitizedPrompt::new(prompt.as_deref());
            observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
            let mut event = EventEnvelope::agent_lifecycle(
                env.workspace_id.clone(),
                "rimz-test",
                "claude",
                "test",
                &observation,
            );
            event.timestamp = anchor + jiff::SignedDuration::from_secs(offset);
            store.append_event(&event).unwrap();
        }
        let session = rimz::workspace::record::read(&store.paths().workspace_record)
            .unwrap()
            .session_name;
        let mut pane = agent_pane(&env, "claude");
        pane.session_name = session.clone();
        let fixture = env.write_pane_fixture(if blocked {
            &[]
        } else {
            std::slice::from_ref(&pane)
        });
        let frame = rimz::sidebar::frame::assemble_frame(
            vec![pane],
            rimz::utils::time::unix_now_ms(),
            session.clone(),
        );
        rimz::disk::atomic::write_temp_then_rename_cache(
            &store.runtime_paths().pane_frame_path(),
            &frame,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        let mut request = CacheKeepaliveRequest {
            workspace_id: env.workspace_id.clone(),
            kind: AgentKind::new_unchecked("claude"),
            agent_id: "provider-session".into(),
            pane_id: PaneId::from_parts(MuxName::Zellij, TRACE_PANE),
            anchor: anchor - Duration::from_secs(1),
            label: "@planner".into(),
        };
        let send = |request: &CacheKeepaliveRequest| {
            run_success(
                traced_rimz(&env, "keepalive-trace.log")
                    .env("RIMZ_TEST_PANE_LIST", &fixture)
                    .args(rimz::child_process::agent_helper_argv(
                        "cache-keepalive",
                        request,
                    )),
                "cache keepalive",
            )
        };
        send(&request);
        assert!(
            store.list_messages().unwrap().is_empty(),
            "moved anchor must not send"
        );
        request.anchor = anchor;
        let current = run_success(
            env.rimz()
                .args(["sidebar", "snapshot", "--no-produce", "--json"]),
            "published snapshot",
        );
        let current: rimz::store::snapshot::SidebarSnapshot =
            serde_json::from_slice(&current.stdout).unwrap();
        assert!(
            request
                .target(
                    &current,
                    &toml::from_str("[prompt_cache_ttl]\nclaude = \"61s\"").unwrap(),
                    &Default::default(),
                    jiff::Timestamp::now()
                )
                .is_some(),
            "fixture must be eligible: agents {:?}, panes {:?}",
            current
                .agents
                .iter()
                .map(|agent| (
                    &agent.agent_id,
                    agent.effective_status(),
                    agent.last_request_at(),
                    &agent.pending_waits
                ))
                .collect::<Vec<_>>(),
            current.agent_panes
        );
        send(&request);
        let mut messages = store.list_messages().unwrap();
        messages.extend(store.list_message_history().unwrap());
        let ping = messages
            .iter()
            .find(|message| {
                matches!(
                    message.sender,
                    MessageSender::Harness {
                        notice: HarnessNotice::CacheKeepalive
                    }
                )
            })
            .expect("keepalive record");
        assert_eq!(
            ping.status,
            if blocked {
                MessageStatus::Errored
            } else {
                MessageStatus::Sent
            }
        );
        assert!(ping.text.contains("timer"));
        assert_eq!(
            ping.text.ends_with(
                "\nKeepalive limit 4m reached: this is the last ping until your next turn."
            ),
            capped,
            "{}",
            ping.text
        );
        assert_eq!(ping.gate, DeliveryGate::Done);
        let output = run_success(env.rimz().args(["stats", "--json"]), "assist stats");
        let stats: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let assist = stats["assists"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|event| event["assist"] == "cache_keepalive")
            .expect("keepalive assist");
        assert_eq!(assist["delivered"], !blocked);
        assert_eq!(assist["waits"], 1);
        assert_eq!(assist["capped"], capped);
        if blocked {
            assert!(
                !store
                    .list_pending_messages()
                    .unwrap()
                    .iter()
                    .any(|message| message.message_id == ping.message_id),
                "miss cannot deliver at a later boundary"
            );
        } else {
            assert!(
                trace_lines(&env.project_root.join("keepalive-trace.log"))
                    .iter()
                    .any(|line| is_paste(
                        line,
                        &format!(
                            "Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\n{}",
                            ping.text
                        )
                    )),
                "typed header reached the pane"
            );
        }
    }
}

#[test]
fn deliver_helper_settles_before_reading_state() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(
        &env,
        "sess-settle",
        "feature-settle",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let message_id = queue_add(&env, "@claude", "continue");
    let started = Instant::now();
    run_success(
        env.rimz()
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "600")
            .args(["message", "deliver", "--message-id", &message_id]),
        "settled delivery helper",
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(600),
        "elapsed: {elapsed:?}"
    );
}

#[test]
fn queue_add_for_bound_agent_does_not_enumerate_panes() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-rollup", "feature-rollup", &[]);

    let trace_log = env.project_root.join("zellij-queue-rollup-trace.log");
    run_success(
        traced_rimz(&env, "zellij-queue-rollup-trace.log").args([
            "--mux",
            "zellij",
            "message",
            "@claude",
            "--",
            "cached path",
        ]),
        "message",
    );
    let trace = trace_lines(&trace_log);
    assert!(
        trace.is_empty(),
        "queue success path must not call zellij: {trace:?}"
    );
}

#[test]
fn parent_message_to_ended_child_reports_missing_conversation() {
    assert_ended_child_resume_refusal(false);
}

#[test]
fn parent_message_to_ended_child_refuses_a_login_without_hooks() {
    assert_ended_child_resume_refusal(true);
}

#[test]
fn parent_message_to_stopped_pane_gone_child_reaches_resume_refusal() {
    use rimz::store::run::{RunRecord, RunStatus};

    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "parent",
        LifecycleSignal::Registered,
        |observation| {
            observation.agent_name = Some("parent".to_owned());
        },
    );
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "child",
        LifecycleSignal::Registered,
        |observation| {
            observation.agent_name = Some("otter".to_owned());
            observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
            observation.launch.parent_agent_id = Some("parent".into());
            observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
            observation.launch.launch_depth = Some(1);
            observation.launch.isolation = Some(rimz::config::Isolation::Host);
            observation.transcript_path =
                Some(env.project_root.join("missing.jsonl").display().to_string());
        },
    );
    let store = env.store();
    let mut run = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        rimz::agents::PermissionMode::Auto,
        "task".to_owned(),
        env.project_root.clone(),
    );
    run.agent_id = Some("child".into());
    run.agent_name = Some("otter".to_owned());
    run.pane_id = Some(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
    run.status = RunStatus::Running;
    run.subagent = true;
    rimz::harness::run::create(store.paths(), &run).unwrap();
    let session = env.resolve_workspace(&env.project_root).session_name;
    let panes = env.write_pane_fixture(&[]);
    let as_parent = || {
        let mut command = traced_rimz(&env, "stopped-child-trace.log");
        command
            .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
            .env(rimz::harness::launch::ENV_AGENT_ID, "parent")
            .env("RIMZ_TEST_PANE_LIST", &panes)
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{session} [Created 1s ago]\n"),
            )
            .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]");
        command
    };
    run_success(
        as_parent().args(["subagents", "stop", "otter"]),
        "subagents stop",
    );
    assert_eq!(
        rimz::harness::run::load(store.paths(), &run.run_id)
            .unwrap()
            .status,
        RunStatus::Canceled
    );

    // Check the consumer first: without the end stamp this succeeds and queues forever.
    let output = as_parent()
        .args(["message", "@otter", "follow up"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "stdout={}; stderr={}; pending={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        store.list_pending_messages().unwrap()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot resume @otter") && stderr.contains("no recorded conversation"),
        "{stderr}"
    );
    assert!(store.list_pending_messages().unwrap().is_empty());
    let agents = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert!(
        agents
            .iter()
            .find(|agent| agent.agent_id == "child")
            .unwrap()
            .ended_at
            .is_some(),
        "{agents:?}"
    );
    let events = env.read_events();
    let ends = events
        .iter()
        .filter_map(|event| match event.kind() {
            rimz::store::event::EventKind::AgentLifecycle(payload)
                if payload
                    .observation
                    .agent_id
                    .as_ref()
                    .is_some_and(|id| id == "child")
                    && payload.observation.signal == LifecycleSignal::Ended =>
            {
                Some(payload)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ends.len(), 1, "{events:?}");
    assert_eq!(ends[0].event_name.as_deref(), Some("rimz.subagent-stopped"));
}

fn assert_ended_child_resume_refusal(unhooked: bool) {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    for id in ["parent", "peer"] {
        append_lifecycle(
            &env,
            "claude",
            "SessionStart",
            id,
            LifecycleSignal::Registered,
            |observation| {
                observation.agent_name = Some(id.to_owned());
            },
        );
    }
    append_lifecycle(
        &env,
        "claude",
        "SessionEnd",
        "child",
        LifecycleSignal::Ended,
        |observation| {
            observation.agent_name = Some("otter".to_owned());
            observation.launch.parent_agent_id = Some("parent".into());
            observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
            observation.launch.launch_depth = Some(1);
            observation.launch.isolation = Some(rimz::config::Isolation::Host);
            observation.transcript_path =
                Some(env.project_root.join("missing.jsonl").display().to_string());
        },
    );
    let audit = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap();
    let child = audit
        .agents
        .iter()
        .find(|agent| agent.agent_id == "child")
        .unwrap();
    assert!(child.ended_at.is_some(), "{child:?}");
    if unhooked {
        std::fs::write(env.project_root.join("missing.jsonl"), "{}\n").unwrap();
        std::fs::remove_file(env.agent_config_path("claude")).unwrap();
    }
    for caller in [Some("parent"), Some("peer"), None] {
        let mut command = env.rimz();
        if let Some(caller) = caller {
            command
                .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
                .env(rimz::harness::launch::ENV_AGENT_ID, caller);
        }
        let output = command
            .args(["message", "@otter", "follow up"])
            .output()
            .expect("message");
        assert!(
            !output.status.success(),
            "caller={caller:?}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        if caller == Some("parent") {
            assert!(
                stderr.contains(if unhooked {
                    "cannot resume @otter: RimZ hooks are missing for claude account `default`"
                } else {
                    "no recorded conversation"
                }),
                "{stderr}"
            );
            assert!(
                !unhooked || stderr.contains("; run `rimz hooks install claude`"),
                "{stderr}"
            );
        } else {
            assert!(!stderr.contains("cannot resume"), "{stderr}");
        }
        assert!(env.store().list_pending_messages().unwrap().is_empty());
    }
}

#[cfg(unix)]
#[test]
fn parent_message_to_resumed_child_waits_for_installed_registration() {
    use crate::common::{exec_args, path_with_front, write_env_dump_shim};
    use rimz::harness::launch::{ExecAction, ExecIdentity, ExecRequest, ProviderAccountState};

    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let store = env.store();
    let workspace = env.resolve_workspace(&env.project_root);
    let transcript = env.project_root.join("child.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "parent",
        LifecycleSignal::Registered,
        |o| {
            o.agent_name = Some("parent".into());
            o.pane_id = Some(PaneId::from_parts(MuxName::Zellij, "terminal_1"));
        },
    );
    store
        .append_event(&EventEnvelope::agent_launched(
            env.workspace_id.clone(),
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            AgentLaunchPayload {
                agent_id: "child".into(),
                launch_id: Some("launch_child".into()),
                agent_name: "otter".into(),
                agent_name_explicit: true,
                launch: LaunchParams {
                    parent_agent_id: Some("parent".into()),
                    parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
                    launch_depth: Some(1),
                    isolation: Some(rimz::config::Isolation::Host),
                    ..LaunchParams::default()
                },
                state: AgentLaunchState::Starting,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    append_lifecycle(
        &env,
        "claude",
        "SessionEnd",
        "child",
        LifecycleSignal::Ended,
        |o| {
            o.agent_name = Some("otter".into());
            o.transcript_path = Some(transcript.display().to_string());
        },
    );
    let mut run = rimz::store::run::RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        rimz::agents::PermissionMode::Auto,
        "finished task".into(),
        env.project_root.clone(),
    );
    run.agent_id = Some("child".into());
    run.agent_name = Some("otter".into());
    run.subagent = true;
    run.status = rimz::store::run::RunStatus::Completed;
    run.joined_at = Some(jiff::Timestamp::now());
    rimz::harness::run::create(store.paths(), &run).unwrap();
    let shim = write_env_dump_shim(&env, "claude");
    // Keep the fixture provider alive without producing its registration hook.
    std::fs::write(shim.join("claude"), "#!/bin/sh\nexec sleep 300\n").unwrap();
    std::os::unix::fs::symlink(zellij_trace_shim(), shim.join("zellij")).unwrap();
    let mut parent_pane = agent_pane(&env, "claude");
    parent_pane.pane_id = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let panes = env.write_pane_fixture(&[parent_pane.clone()]);
    let trace = env.project_root.join("resume-trace.log");
    let mut sender = traced_rimz(&env, &trace)
        .args(["--mux", "zellij", "message", "@otter", "follow up"])
        .env("PATH", path_with_front(&shim))
        .env("RIMZ_TEST_PANE_LIST", &panes)
        .env("RIMZ_AGENT_KIND", "claude")
        .env("RIMZ_AGENT_ID", "parent")
        .env("ZELLIJ_PANE_ID", "1")
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", &workspace.session_name)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let log = std::fs::read_to_string(&trace).unwrap_or_default();
        if log.contains("\tnew-tab\t") || log.contains("\tnew-pane\t") {
            break;
        }
        assert!(
            sender.try_wait().unwrap().is_none(),
            "sender exited before opening: {:?}",
            sender.wait_with_output().unwrap()
        );
        assert!(
            Instant::now() < deadline,
            "resume did not open a pane: {log}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // The trace mux records opens rather than executing them; start the same exec wrapper in the requested child pane.
    env.write_pane_fixture(&[parent_pane, agent_pane(&env, "claude")]);
    let mut request = ExecRequest {
        isolation_default: None,
        kind: AgentKind::new_unchecked("claude"),
        action: ExecAction::Resume {
            session_id: "child".into(),
            extra_args: Vec::new(),
        },
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: None,
        allowed_tools: None,
        provider_account: ProviderAccountState::Unbound,
        run_id: Some(run.run_id.clone()),
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: true,
        subagent: true,
        loop_reminder: None,
        headless: None,
        identity: ExecIdentity::default(),
    };
    request.identity.name = Some("otter".into());
    request.identity.name_explicit = true;
    request.identity.launch_id = Some("launch_child".into());
    request.identity.params = LaunchParams {
        parent_agent_id: Some("parent".into()),
        parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
        launch_depth: Some(1),
        isolation: Some(rimz::config::Isolation::Host),
        ..LaunchParams::default()
    };
    let mut wrapper = traced_rimz(&env, &trace)
        .args(exec_args(&env, &request))
        .args(["--mux", "zellij"])
        .env("SHELL", "/definitely/not/a/shell")
        .env("PATH", path_with_front(&shim))
        .env("ZELLIJ_PANE_ID", "3")
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", &workspace.session_name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let output = sender.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = store.list_messages().unwrap();
    let message = records
        .iter()
        .find(|record| record.text == "follow up")
        .unwrap();
    assert_eq!(message.status, MessageStatus::Queued);
    assert!(String::from_utf8_lossy(&output.stdout).contains("is resuming"));
    let shown = run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &panes)
            .args(["message", "show", message.message_id.as_str()]),
        "show resumed queue",
    );
    assert!(
        String::from_utf8_lossy(&shown.stdout)
            .contains("is resuming; delivers when its provider registers")
    );
    let zellij = zellij_trace_shim();
    let provider_pid = rimz::harness::run::load(store.paths(), &run.run_id)
        .unwrap()
        .provider_pid
        .unwrap()
        .to_string();
    let hook_env = [
        ("ZELLIJ_PANE_ID", "3"),
        ("RIMZ_AGENT_PID", provider_pid.as_str()),
        ("RIMZ_MESSAGE_SETTLE_MS", "0"),
        ("RIMZ_TEST_PANE_LIST", panes.to_str().unwrap()),
        ("RIMZ_ZELLIJ_BIN", zellij.to_str().unwrap()),
        ("RIMZ_TEST_ZELLIJ_LOG", trace.to_str().unwrap()),
    ];
    let registered = env.run_installed_hook_in_pane("claude", &json!({"hook_event_name":"SessionStart", "session_id":"child", "source":"resume", "cwd":env.project_root}).to_string(), &hook_env);
    assert!(registered.status.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let record = store
            .list_messages()
            .unwrap()
            .into_iter()
            .find(|record| record.message_id == message.message_id)
            .unwrap();
        if record.status == MessageStatus::Sent
            && trace_lines(&trace).iter().any(|line| is_enter_key(line))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "detached registration delivery did not send: {record:?}; show={:?}; hook={registered:?}; agents={:?}",
            traced_rimz(&env, &trace)
                .env("RIMZ_TEST_PANE_LIST", &panes)
                .args(["message", "show", message.message_id.as_str()])
                .output()
                .unwrap(),
            store
                .runtime_projection(rimz::RuntimeScope::Audit)
                .unwrap()
                .agents,
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let submitted = "Type: AGENT_MESSAGE\nFrom: @claude\nContent:\nfollow up";
    assert_text_then_enter(&trace, submitted);
    let started = env.run_installed_hook_in_pane(
        "claude",
        &json!({"hook_event_name":"UserPromptSubmit", "session_id":"child", "prompt":submitted})
            .to_string(),
        &hook_env,
    );
    assert!(started.status.success());
    assert!(
        store
            .list_message_history()
            .unwrap()
            .iter()
            .any(|record| record.message_id == message.message_id
                && record.status == MessageStatus::Delivered)
    );
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
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
            .any(|agent| { agent.agent_id == "sess-audit-reviewer" && agent.ended_at.is_some() }),
        "write-path reap should retain an ended audit row before address fallback"
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

#[test]
fn deliver_leaves_ineligible_message_unclaimed() {
    let env = Env::new();
    env.record(&env.project_root);
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

    run_success(
        env.rimz().env("RIMZ_MESSAGE_SETTLE_MS", "0").args([
            "message",
            "deliver",
            "--message-id",
            &message_id,
        ]),
        "message deliver",
    );

    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, MessageStatus::Queued);
    assert_eq!(pending[0].attempts, 0, "no-pane miss must not claim");
    assert!(pending[0].last_attempt_at.is_none());
}

#[test]
fn message_steer_queued_record_respects_waiting_force_and_overrides_gate() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-steer-queued", "steer-queued", pane_env);
    let message_id = queue_add(&env, "@claude", "push now");
    push_pending_agent_ask(&env, "sess-steer-queued");
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let trace_log = env.project_root.join("zellij-steer-queued-trace.log");

    let blocked = traced_rimz(&env, "zellij-steer-queued-trace.log")
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

    let steered = run_success(
        traced_rimz(&env, "zellij-steer-queued-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "steer", &message_id, "--force"]),
        "forced message steer",
    );
    let stdout = String::from_utf8_lossy(&steered.stdout);
    assert!(stdout.contains(&format!("sent to @claude ({message_id})")));
    assert_text_then_enter(&trace_log, &user_message("push now"));
    assert_eq!(
        message_by_id(&env, &MessageId::parse(&message_id).expect("message id")).status,
        MessageStatus::Sent
    );
}

#[test]
fn steer_queues_when_durable_agent_has_no_live_pane() {
    let env = Env::new();
    env.record(&env.project_root);
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

    let out = run_success(
        env.rimz()
            .args(["message", "--steer", "@reviewer", "--", "please review"]),
        "steer durable fallback",
    );
    let message_id = queued_id_from_stdout(&out.stdout);
    let pending = env.store().list_pending_messages().expect("pending queue");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message_id.as_str(), message_id);
    assert_eq!(pending[0].agent_id.as_str(), "sess-steer-audit");
}

/// Enter stays a discrete key event after bracketed paste.
#[test]
fn steer_enter_modes_respect_discrete_submit_key() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(
        &env,
        "sess-steer-enter",
        "feature-se",
        &[("ZELLIJ_PANE_ID", "3")],
    );

    let trace_log = env.project_root.join("zellij-steer-trace.log");
    run_success(
        traced_rimz(&env, "zellij-steer-trace.log")
            .args(["message", "--steer", "@claude", "--", "y"]),
        "steer",
    );

    assert_text_then_enter(&trace_log, &user_message("y"));
    let session = env.resolve_workspace(&env.project_root).session_name;
    let lines = trace_lines(&trace_log);
    assert!(
        lines
            .iter()
            .filter(|line| line.contains("\taction\twrite\t"))
            .all(|line| line.contains(&format!("\t--session\t{session}\taction\twrite\t"))),
        "the paste and the submit key name the room's session {session}; trace: {lines:?}"
    );

    let trace_log = env.project_root.join("zellij-steer-quiet-trace.log");
    run_success(
        traced_rimz(&env, "zellij-steer-quiet-trace.log").args([
            "message",
            "--steer",
            "@claude",
            "--no-enter",
            "--",
            "y",
        ]),
        "no-enter steer",
    );

    let lines = trace_lines(&trace_log);
    assert!(
        lines.iter().any(|line| is_paste(line, &user_message("y"))),
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
    env.record(&env.project_root);
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
        DeliveryGate::Done,
    )
    .with_address(Some("@coder".to_owned()))
    .with_sender(MessageSender::Agent {
        agent_id: None,
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
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(
        &env,
        "sess-wait-timeout",
        "feature-wait-timeout",
        &[("ZELLIJ_PANE_ID", "3")],
    );

    let out = traced_rimz(&env, "zellij-wait-timeout-trace.log")
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

fn assert_sent_receipts(stderr: &[u8], expected: usize) {
    let text = String::from_utf8_lossy(stderr);
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("sent to ") && line.contains(" (msg_"))
            .count(),
        expected,
        "{text}"
    );
}

#[test]
fn message_wait_prints_the_reply_after_the_turn_ends() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let agent = ReplyAgentFixture::single(&env, "reply");
    std::fs::write(
        &agent.transcript_path,
        "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"old answer\"}]}}\n",
    )
    .expect("seed transcript");

    let mut child = traced_rimz(&env, "zellij-wait-reply-trace.log")
        .args(["message", "@claude", "--wait", "did it land?"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn message --wait");

    let stderr = child.stderr.take().unwrap();
    let (receipt_tx, receipt_rx) = std::sync::mpsc::channel();
    let stderr_reader = std::thread::spawn(move || {
        use std::io::{BufRead, Read};
        let mut reader = std::io::BufReader::new(stderr);
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        let _ = receipt_tx.send(first);
        let mut rest = String::new();
        reader.read_to_string(&mut rest).unwrap();
        rest
    });

    wait_for_message_event(&env, "message.sent", Duration::from_secs(2));
    let receipt = receipt_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("receipt arrives before the receiver starts its reply turn");
    assert!(receipt.starts_with("sent to @claude"), "{receipt}");
    agent.start(&env, "did it land?");
    let store = env.store();
    let message = store
        .list_message_history()
        .unwrap()
        .into_iter()
        .find(|message| message.text == "did it land?")
        .unwrap();
    let mut run = rimz::store::run::RunRecord::new(
        message.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        rimz::agents::PermissionMode::Auto,
        "did it land?".into(),
        env.project_root.clone(),
    );
    run.subagent = true;
    run.agent_id = Some(agent.session_id.as_str().into());
    run.opened_by.push(message.message_id);
    run.status = rimz::store::run::RunStatus::Completed;
    rimz::harness::run::create(store.paths(), &run).unwrap();
    agent.finish(&env, "migration landed", false);

    let out = child.wait_with_output().expect("wait message --wait");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "migration landed\n");
    assert!(receipt.contains(&run.opened_by[0].to_string()));
    assert!(stderr_reader.join().unwrap().is_empty());
    assert!(env.store().list_messages().unwrap().is_empty());
    let joined = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert!(joined.joined_at.is_some());
    assert!(!joined.owes_report());
}

#[test]
fn message_wait_prints_the_wake_turns_reply() {
    message_wait_wake_reply_case(false);
}

#[test]
fn message_wait_prints_the_fast_wake_turns_reply() {
    message_wait_wake_reply_case(true);
}

fn message_wait_wake_reply_case(fast_wake: bool) {
    const POLL_WINDOW: Duration = Duration::from_millis(1500);

    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let agent = ReplyAgentFixture::single(&env, "wake-reply");
    let mut child = traced_rimz(&env, "zellij-wait-wake-reply-trace.log")
        .args(["message", "@claude", "--wait", "did it land?"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn message --wait");

    let stderr = child.stderr.take().unwrap();
    let (receipt_tx, receipt_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let stderr_reader = std::thread::spawn(move || {
        use std::io::{BufRead, Read};
        let mut reader = std::io::BufReader::new(stderr);
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        let _ = receipt_tx.send(first);
        let mut rest = String::new();
        reader.read_to_string(&mut rest).unwrap();
        let _ = finished_tx.send(());
        rest
    });

    wait_for_message_event(&env, "message.sent", Duration::from_secs(2));
    let receipt = receipt_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("receipt arrives before the receiver starts its reply turn");
    assert!(receipt.starts_with("sent to @claude"), "{receipt}");
    agent.start(&env, "did it land?");
    let wake = agent.send_wake(&env);
    if fast_wake {
        // Anchor the first turn before skipping the sleeping poll.
        let _ = finished_rx.recv_timeout(POLL_WINDOW);
    }
    agent.finish(&env, "pausing until the wake", false);

    if !fast_wake {
        std::thread::sleep(POLL_WINDOW);
        assert!(
            child.try_wait().expect("poll message --wait").is_none(),
            "reply wait ended at the sleeping turn boundary"
        );
    }

    agent.start_reported(&env, "Type: WAIT\nFrom: @rimz\nContent:\nwake now");
    assert!(
        !env.store()
            .list_messages()
            .unwrap()
            .iter()
            .any(|pending| pending.message_id == wake.message_id),
        "wake turn did not acknowledge its wake"
    );
    // Keep the wake turn open across polls; an early exit is checked below.
    let _ = finished_rx.recv_timeout(POLL_WINDOW);
    agent.finish(&env, "migration landed", false);

    let out = wait_with_output_bounded(child, Duration::from_secs(10));
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "migration landed\n");
    assert!(stderr_reader.join().unwrap().is_empty());
    assert!(env.store().list_messages().unwrap().is_empty());
}

#[test]
fn message_wait_gathers_fanout_replies_in_completion_order() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let [first, second] = ReplyAgentFixture::pair(&env, "gather");

    let child = traced_rimz(&env, "zellij-wait-gather-trace.log")
        .args(["message", "@all", "--wait=60s", "status?"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fanout wait");
    wait_for_message_event_count(&env, "message.sent", 2, Duration::from_secs(60));
    first.start(&env, "@all, status?");
    second.start(&env, "@all, status?");
    second.finish(&env, "second finished", false);
    std::thread::sleep(Duration::from_millis(600));
    first.finish(&env, "first finished", false);

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
    assert_sent_receipts(&out.stderr, 2);
}

#[test]
fn agent_broadcast_waits_for_peers_without_waiting_on_itself() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let [caller, peer] = ReplyAgentFixture::pair_in_channel(&env, "agent-gather");
    caller.stamp_launch_identity(&env, "launch-agent-gather", "planner");

    let child = traced_rimz(&env, "zellij-agent-wait-gather-trace.log")
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .env(rimz::harness::launch::ENV_AGENT_ID, "launch-agent-gather")
        .env(rimz::harness::launch::ENV_AGENT_NAME, "planner")
        .args(["message", "@all", "--wait=60s", "status?"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agent fanout wait");
    wait_for_message_event_count(&env, "message.sent", 1, Duration::from_secs(60));
    peer.start_reported(
        &env,
        "Type: AGENT_MESSAGE\nFrom: @planner\nContent:\n@all, status?",
    );
    peer.finish(&env, "peer finished", false);

    let out = child.wait_with_output().expect("wait for peer reply");
    assert!(
        out.status.success(),
        "agent fanout wait failed: {}\nstdout={}\nstderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "peer finished\n");
    assert_sent_receipts(&out.stderr, 1);
    assert!(
        env.read_events()
            .iter()
            .filter(|event| event.method == "message.sent")
            .all(|event| event.params_value()["agent_id"] == peer.session_id),
        "the caller never receives a message leg"
    );
}

#[test]
fn message_wait_json_emits_one_fanout_map() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let [first, second] = ReplyAgentFixture::pair(&env, "json");

    let child = traced_rimz(&env, "zellij-wait-json-trace.log")
        .args(["message", "@all", "--wait=60s", "--json", "status?"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn JSON fanout wait");
    wait_for_message_event_count(&env, "message.sent", 2, Duration::from_secs(60));
    first.start(&env, "@all, status?");
    second.start(&env, "@all, status?");
    first.finish(&env, "first JSON reply", false);
    second.finish(&env, "second JSON reply", false);

    let out = child.wait_with_output().expect("wait JSON fanout gather");
    assert!(
        out.status.success(),
        "JSON fanout wait failed: {}\nstdout={}\nstderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
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
    assert_sent_receipts(&out.stderr, 2);
}

#[test]
fn message_wait_gathers_other_replies_after_one_leg_fails() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let [failed, completed] =
        ReplyAgentFixture::pair_named(&env, "partial", ["failed", "completed"]);

    let child = traced_rimz(&env, "zellij-wait-partial-trace.log")
        .args(["message", "@all", "--wait=60s", "try it"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn partial fanout wait");
    wait_for_message_event_count(&env, "message.sent", 2, Duration::from_secs(60));
    failed.start(&env, "@all, try it");
    completed.start(&env, "@all, try it");
    failed.finish(&env, "partial answer", true);
    std::thread::sleep(Duration::from_millis(600));
    completed.finish(&env, "surviving reply", false);

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
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let [first, second] = ReplyAgentFixture::pair(&env, "any");

    let child = traced_rimz(&env, "zellij-wait-any-trace.log")
        .args(["message", "@all", "--wait=60s", "--any", "first?"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn any fanout wait");
    wait_for_message_event_count(&env, "message.sent", 2, Duration::from_secs(60));
    first.start(&env, "@all, first?");
    second.start(&env, "@all, first?");
    second.finish(&env, "winner", false);

    let out = child.wait_with_output().expect("wait any fanout");
    assert!(
        out.status.success(),
        "any wait failed: {}\nstdout={}\nstderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "@claude#feature-any-second:\nwinner\n"
    );
    assert_sent_receipts(&out.stderr, 2);
}

#[test]
fn message_wait_json_classifies_every_unfinished_fanout_leg_on_deadline() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let _agents = ReplyAgentFixture::pair(&env, "timeout");

    let out = traced_rimz(&env, "zellij-wait-timeout-fanout.log")
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
    assert_sent_receipts(&out.stderr, 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("rimz: wait timed out for "));
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
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let [_sent, queued] = ReplyAgentFixture::pair_named(&env, "mixed", ["sent", "queued"]);
    push_pending_agent_ask(&env, &queued.session_id);

    let out = traced_rimz(&env, "zellij-wait-mixed-timeout.log")
        .args(["message", "@all", "--wait=0s", "--json", "status?"])
        .output()
        .expect("mixed-state fanout wait deadline");

    assert_eq!(out.status.code(), Some(124));
    let replies: serde_json::Value = serde_json::from_slice(&out.stdout).expect("reply JSON");
    let sent_label = "@claude#feature-mixed-sent";
    let queued_label = "@claude#feature-mixed-queued";
    assert_eq!(replies[sent_label]["status"], "timed_out");
    assert_eq!(replies[queued_label]["status"], "timed_out");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let sent_id = replies[sent_label]["message_id"].as_str().unwrap();
    let queued_id = replies[queued_label]["message_id"].as_str().unwrap();
    assert!(
        stderr.contains(&format!("sent to {sent_label} ({sent_id})")),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("queued for {queued_label} ({queued_id})")),
        "{stderr}"
    );
    assert!(stderr.contains("rimz: wait timed out for "), "{stderr}");
    assert!(stderr.contains(&format!("  {queued_label}: {queued_id} is still queued and will deliver. withdraw it: rimz message cancel {queued_id}   read the reply later: rimz agents logs {queued_label}")), "{stderr}");
    assert!(stderr.contains(&format!("  {sent_label}: {sent_id} was typed into the pane and not acknowledged; do not resend. check: rimz agents logs {sent_label}")), "{stderr}");

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
fn message_wait_requires_live_hooked_agent_target() {
    let env = Env::new();
    env.record(&env.project_root);
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

#[test]
fn steer_sends_a_file_with_cr_newlines_inside_the_paste() {
    let env = Env::new();
    env.record(&env.project_root);
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
    run_success(
        traced_rimz(&env, "zellij-file-trace.log").args([
            "message",
            "--steer",
            "@claude",
            "--file",
            prompt_file.to_str().expect("utf-8 path"),
        ]),
        "steer --file",
    );

    // Header and body newlines are logical text: the independent trace matcher
    // below requires each one to arrive as CR inside the bracketed paste.
    assert_text_then_enter(
        &trace_log,
        &user_message("keep \\n literal\nand a real break"),
    );
}

#[test]
fn steer_combines_inline_text_with_piped_stdin() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(
        &env,
        "sess-stdin",
        "feature-stdin",
        &[("ZELLIJ_PANE_ID", "3")],
    );

    let trace_log = env.project_root.join("zellij-stdin-trace.log");
    let mut cmd = traced_rimz(&env, "zellij-stdin-trace.log");
    cmd.args(["message", "--steer", "@claude", "--stdin", "review this"])
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
        &user_message("review this\n\n<stdin>\ndiff --git a/file b/file\n-old\n+new\n</stdin>"),
    );
}

#[test]
fn message_ignores_an_open_empty_stdin_without_the_flag() {
    let env = Env::new();
    env.record(&env.project_root);
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
    env.record(&env.project_root);
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
fn message_me_resolves_launch_and_process_ancestry() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let [caller, peer] = ReplyAgentFixture::pair(&env, "me");
    caller.stamp_launch_identity(&env, "launch-me", "planner");
    peer.stamp_launch_identity(&env, "launch-me-peer", "reviewer");
    caller.start(&env, "work");
    run_success(
        env.rimz()
            .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
            .env(rimz::harness::launch::ENV_AGENT_ID, "launch-me")
            .env(rimz::harness::launch::ENV_AGENT_NAME, "reviewer")
            .env("ZELLIJ_PANE_ID", peer.pane_id)
            .args(["message", "@me", "launch identity"]),
        "message self by authoritative launch id",
    );
    let messages = env.store().list_messages().expect("messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].agent_id, caller.session_id);
    assert_eq!(messages[0].kind.as_str(), "claude");

    let mut provider = std::process::Command::new("sh");
    provider
        .args([
            "-c",
            "read _; \"$0\" message @me 'ancestry identity'; exit $?",
        ])
        .arg(env.rimz_bin());
    let command = env.rimz();
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            provider.env(key, value);
        } else {
            provider.env_remove(key);
        }
    }
    if let Some(dir) = command.get_current_dir() {
        provider.current_dir(dir);
    }
    scrub_bare_agent_identity(&mut provider);
    provider.env("ZELLIJ_PANE_ID", "terminal_5");
    let mut provider = provider
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stand-in provider");
    register_running_agent_owned_by(
        &env,
        "claude",
        "sess-me-ancestor",
        "project",
        &[("ZELLIJ_PANE_ID", "terminal_5")],
        provider.id(),
    );
    provider
        .stdin
        .take()
        .expect("provider stdin")
        .write_all(b"\n")
        .expect("signal provider");
    let output = wait_with_output_bounded(provider, Duration::from_secs(2));
    assert!(
        output.status.success(),
        "ancestry self-send failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let messages = env.store().list_messages().expect("messages");
    let message = messages
        .iter()
        .find(|message| message.text == "ancestry identity")
        .expect("ancestry self message");
    assert_eq!(message.agent_id, "sess-me-ancestor");
    assert!(
        matches!(&message.sender, MessageSender::Agent { kind, .. } if kind.as_str() == "claude")
    );
}

#[test]
fn message_me_rejects_unidentified_shell_and_snapshot_only_owner() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let caller = ReplyAgentFixture::single(&env, "me-shell");
    caller.stamp_launch_identity(&env, "launch-me-shell", "planner");
    caller.start(&env, "work");
    let store = env.store();
    let mut snapshot = store.snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter_mut()
        .find(|agent| agent.agent_id == caller.session_id)
        .expect("caller row");
    agent.runtime_owner = Some(rimz::pane::RuntimeOwner::new(
        rimz::pane::RuntimeOwnerKind::Agent,
        caller.session_id.clone(),
        std::process::id(),
        rimz::proc::process_start_token(std::process::id()),
    ));
    for snapshot_only_owner in [false, true] {
        if snapshot_only_owner {
            rimz::disk::atomic::write_temp_then_rename_cache(
                &store.paths().latest_snapshot,
                &snapshot,
            )
            .expect("seed snapshot-only owner");
            assert!(
                store
                    .snapshot_cached()
                    .expect("poisoned snapshot")
                    .agents
                    .iter()
                    .any(|agent| {
                        agent
                            .runtime_owner
                            .as_ref()
                            .is_some_and(|owner| owner.pid == std::process::id())
                    })
            );
            assert!(
                store
                    .runtime_projection(rimz::RuntimeScope::Audit)
                    .expect("durable owners")
                    .agents
                    .iter()
                    .all(|agent| {
                        agent
                            .runtime_owner
                            .as_ref()
                            .is_none_or(|owner| owner.pid != std::process::id())
                    })
            );
        }
        let mut command = env.rimz();
        scrub_bare_agent_identity(&mut command);
        let output = command
            .env("ZELLIJ_PANE_ID", caller.pane_id)
            .args(["message", "@me", "must not queue"])
            .output()
            .expect("shell self-send");
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(
                "@me requires an agent RimZ can identify; run this command from an agent pane"
            ),
            "snapshot-only owner {snapshot_only_owner}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(store.list_messages().expect("messages").is_empty());
    }
}

#[test]
fn message_me_rejects_stale_identity_and_unregistered_session() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let caller = ReplyAgentFixture::single(&env, "me-stale");
    caller.stamp_launch_identity(&env, "launch-me-stale", "planner");
    caller.start(&env, "work");
    for (kind, launch_id) in [("claude", "missing-launch"), ("codex", "launch-me-stale")] {
        let output = env
            .rimz()
            .env(rimz::harness::launch::ENV_AGENT_KIND, kind)
            .env(rimz::harness::launch::ENV_AGENT_ID, launch_id)
            .env("ZELLIJ_PANE_ID", caller.pane_id)
            .args(["message", "@me", "must not queue"])
            .output()
            .expect("stale identity self-send");
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("@me requires an agent RimZ can identify")
        );
    }
    run_hook(
        &env,
        json!({"hook_event_name": "SessionEnd", "session_id": caller.session_id}),
        &[("ZELLIJ_PANE_ID", caller.pane_id)],
    );
    let output = env
        .rimz()
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .env(rimz::harness::launch::ENV_AGENT_ID, "launch-me-stale")
        .args(["agents", "show", "@me"])
        .output()
        .expect("ended caller resolution");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("the calling agent has ended"));

    seed_provisional_codex_launch(&env, "launch_me", "starting", None, "terminal_5", None);
    let output = env
        .rimz()
        .env(rimz::harness::launch::ENV_AGENT_KIND, "codex")
        .env(rimz::harness::launch::ENV_AGENT_ID, "launch_me")
        .args(["message", "@me", "must not queue"])
        .output()
        .expect("provisional caller resolution");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("the calling agent is still starting")
    );
    assert!(env.store().list_messages().expect("messages").is_empty());
}

#[test]
fn bare_resumed_agent_message_is_attributed_by_process_ancestry() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    let receiver_owner = dummy_agent_process();
    let receiver_owner_pid = receiver_owner.id();
    reap_later(receiver_owner);
    register_running_agent_owned_by(
        &env,
        "codex",
        "sess-codex-bare-receiver",
        "project",
        &[("ZELLIJ_PANE_ID", TRACE_PANE)],
        receiver_owner_pid,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "codex")]);

    let trace_name = "zellij-bare-resumed-agent-trace.log";
    let mut traced = traced_rimz(&env, trace_name);
    traced.env("RIMZ_TEST_PANE_LIST", pane_fixture);
    let mut provider = std::process::Command::new("sh");
    provider
        .args([
            "-c",
            "read _; \"$0\" message --steer @sess-codex-bare-receiver -- ping; exit $?",
        ])
        .arg(env.rimz_bin());
    for (key, value) in traced.get_envs() {
        if let Some(value) = value {
            provider.env(key, value);
        } else {
            provider.env_remove(key);
        }
    }
    if let Some(dir) = traced.get_current_dir() {
        provider.current_dir(dir);
    }
    scrub_bare_agent_identity(&mut provider);
    let mut provider = provider
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stand-in provider");

    register_running_agent_owned_by(
        &env,
        "claude",
        "sess-claude-bare",
        "project",
        &[("ZELLIJ_PANE_ID", "terminal_4")],
        provider.id(),
    );
    provider
        .stdin
        .take()
        .expect("provider stdin")
        .write_all(b"\n")
        .expect("signal provider");
    let output = wait_with_output_bounded(provider, Duration::from_secs(2));
    assert!(
        output.status.success(),
        "stand-in provider failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let sent = env
        .read_events()
        .into_iter()
        .rev()
        .find(|event| event.method == "message.sent")
        .expect("sent event");
    let params = sent.params_value();
    assert_eq!(params["sender"]["origin"], "agent");
    assert_eq!(params["sender"]["kind"], "claude");
    let sender_name = params["sender"]["name"].as_str().expect("sender petname");
    let sender_handle = format!("@{sender_name}");
    let delivered = format!("Type: AGENT_MESSAGE\nFrom: {sender_handle} (claude)\nContent:\nping");
    let trace_log = env.project_root.join(trace_name);
    assert_text_then_enter(&trace_log, &delivered);

    run_hook_for_owner(
        &env,
        "codex",
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-codex-bare-receiver",
            "prompt": delivered,
            "worktree_branch": "project",
        }),
        &[("ZELLIJ_PANE_ID", TRACE_PANE)],
        receiver_owner_pid,
    );
    let entries = rimz::transcript::read_all(env.store().paths()).expect("read transcript");
    let message = entries
        .iter()
        .find(|entry| {
            entry.agent_id.as_str() == "sess-codex-bare-receiver"
                && entry.entry == rimz::transcript::TranscriptKind::Message
        })
        .expect("receiver message entry");
    assert_eq!(message.from.as_deref(), Some(sender_handle.as_str()));
    assert_eq!(message.text, "ping");

    let rendered = run_success(
        env.rimz().args(["transcript", "#project"]),
        "receiver transcript",
    );
    let rendered = String::from_utf8_lossy(&rendered.stdout);
    assert!(
        rendered.contains(&format!("{sender_handle} → @codex")),
        "{rendered}"
    );
}

#[test]
fn steer_formats_human_and_agent_senders_and_no_from_stays_verbatim() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(
        &env,
        "sess-from-steer",
        "feature-from-steer",
        &[("ZELLIJ_PANE_ID", "3")],
    );

    let trace_log = env.project_root.join("zellij-from-steer-user-trace.log");
    run_success(
        traced_rimz(&env, "zellij-from-steer-user-trace.log").args([
            "message",
            "--steer",
            "@claude",
            "--",
            "from human",
        ]),
        "steer from human",
    );
    assert_text_then_enter(&trace_log, &user_message("from human"));

    let trace_log = env.project_root.join("zellij-from-steer-agent-trace.log");
    let out = run_success(
        traced_rimz(&env, "zellij-from-steer-agent-trace.log")
            .env("RIMZ_AGENT_KIND", "codex")
            .env("RIMZ_AGENT_NAME", "swift-otter")
            .args(["message", "--steer", "@claude", "--", "ping"]),
        "steer from agent",
    );
    assert_single_sigil_sent(&out.stdout);
    assert_text_then_enter(
        &trace_log,
        "Type: AGENT_MESSAGE\nFrom: @swift-otter (codex)\nContent:\nping",
    );
    let sent = env
        .read_events()
        .into_iter()
        .rev()
        .find(|event| event.method == "message.sent")
        .expect("sent event");
    let params = sent.params_value();
    assert_eq!(params["sender"]["origin"], "agent");
    assert_eq!(params["sender"]["kind"], "codex");
    assert_eq!(params["sender"]["name"], "swift-otter");
    assert_eq!(params["text_len"], "ping".len());
    assert_eq!(params["status"], "sent");

    let trace_log = env.project_root.join("zellij-from-steer-no-from-trace.log");
    run_success(
        traced_rimz(&env, "zellij-from-steer-no-from-trace.log")
            .env("RIMZ_AGENT_KIND", "codex")
            .env("RIMZ_AGENT_NAME", "swift-otter")
            .args(["message", "--steer", "@claude", "--no-from", "--", "exact"]),
        "steer --no-from from agent",
    );
    assert_text_then_enter(&trace_log, "exact");
    let sent = env
        .read_events()
        .into_iter()
        .rev()
        .find(|event| event.method == "message.sent")
        .expect("no-from sent event");
    assert_eq!(sent.params_value()["sender"]["origin"], "system");
}

#[test]
fn agent_broadcast_steer_with_no_from_writes_only_to_the_peer() {
    let env = Env::new();
    env.record(&env.project_root);
    let [caller, _peer] = ReplyAgentFixture::pair_in_channel(&env, "agent-steer");
    caller.stamp_launch_identity(&env, "launch-agent-steer", "planner");
    let trace_log = env.project_root.join("zellij-agent-steer-trace.log");

    let out = run_success(
        traced_rimz(&env, "zellij-agent-steer-trace.log")
            .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
            .env(rimz::harness::launch::ENV_AGENT_ID, "launch-agent-steer")
            .env(rimz::harness::launch::ENV_AGENT_NAME, "planner")
            .args(["message", "--steer", "@all", "--no-from", "--", "peer only"]),
        "agent broadcast steer",
    );

    assert!(String::from_utf8_lossy(&out.stdout).contains("sent to"));
    let lines = trace_lines(&trace_log);
    assert!(
        lines.iter().any(
            |line| line.contains("\taction\twrite\t--pane-id\tterminal_4\t")
                && is_paste_to_any_pane(line, "@all, peer only")
        ),
        "the peer receives the broadcast prefix: {lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("\t--pane-id\tterminal_3\t")),
        "the caller pane is untouched: {lines:?}"
    );
}

#[test]
fn solo_agent_broadcast_errors_but_an_exact_self_handle_still_delivers() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let caller = ReplyAgentFixture::single(&env, "agent-solo");
    caller.stamp_launch_identity(&env, "launch-agent-solo", "planner");

    let broadcast = traced_rimz(&env, "zellij-agent-solo-broadcast-trace.log")
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .env(rimz::harness::launch::ENV_AGENT_ID, "launch-agent-solo")
        .env(rimz::harness::launch::ENV_AGENT_NAME, "planner")
        .args(["message", "@all", "anyone?"])
        .output()
        .expect("solo broadcast");
    assert_eq!(broadcast.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&broadcast.stderr)
            .contains("no other agents in the current channel"),
        "solo error: {}",
        String::from_utf8_lossy(&broadcast.stderr)
    );

    run_success(
        env.rimz()
            .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
            .env(rimz::harness::launch::ENV_AGENT_ID, "launch-agent-solo")
            .env(rimz::harness::launch::ENV_AGENT_NAME, "planner")
            .args(["message", "@planner", "deliberate self-send"]),
        "exact self send",
    );
    let message = env
        .store()
        .list_messages()
        .expect("messages")
        .into_iter()
        .next_back()
        .expect("exact self message");
    assert_eq!(message.agent_id, caller.session_id);
    assert_eq!(message.text, "deliberate self-send");
}

#[test]
fn steer_sender_header_ignores_shadowed_co_resident_session() {
    let env = Env::new();
    env.record(&env.project_root);
    register_role_agent(
        &env,
        "claude",
        "sess-prefix-target",
        "reviewer",
        true,
        Some(TRACE_PANE),
    );
    register_role_agent(
        &env,
        "codex",
        "sess-prefix-owner",
        "coder",
        true,
        Some("terminal_4"),
    );
    register_role_agent(
        &env,
        "codex",
        "sess-prefix-shadow",
        "coder",
        true,
        Some("terminal_4"),
    );
    let target_pane = agent_pane(&env, "claude");
    let mut sender_pane = agent_pane(&env, "codex");
    sender_pane.pane_id = PaneId::from_parts(MuxName::Zellij, "terminal_4");
    let pane_fixture = env.write_pane_fixture(&[target_pane, sender_pane]);

    let trace_log = env.project_root.join("zellij-shadowed-sender-trace.log");
    run_success(
        traced_rimz(&env, "zellij-shadowed-sender-trace.log")
            .env("RIMZ_TEST_PANE_LIST", pane_fixture)
            .env("RIMZ_AGENT_KIND", "codex")
            .env("RIMZ_AGENT_NAME", "coder-agent")
            .env("RIMZ_AGENT_ROLE", "coder")
            .args(["message", "--steer", "@reviewer", "--", "re-review"]),
        "steer from reborn agent",
    );
    assert_text_then_enter(
        &trace_log,
        "Type: AGENT_MESSAGE\nFrom: @coder (codex)\nContent:\nre-review",
    );
}

#[test]
fn boundary_dispatch_sends_when_idle_then_parks_and_delivers_when_running() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-boundary", "feature-boundary", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-boundary",
            "worktree_branch": "feature-boundary",
        }),
        pane_env,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);

    let trace_log = env.project_root.join("zellij-queue-trace.log");
    let sent = run_success(
        traced_rimz(&env, "zellij-queue-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "@claude", "--", "go"]),
        "send at open gate",
    );
    assert_single_sigil_sent(&sent.stdout);
    let sent_id = sent_id_from_stdout(&sent.stdout);
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    assert_text_then_enter(&trace_log, &user_message("go"));
    let fresh = message_by_id(&env, &MessageId::parse(&sent_id).expect("message id"));
    assert_eq!(fresh.status, MessageStatus::Sent);
    assert_eq!(fresh.attempts, 1, "fresh live send holds a claim");

    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-boundary",
            "prompt": user_message("go"),
            "worktree_branch": "feature-boundary",
        }),
        pane_env,
    );
    assert!(
        env.store()
            .list_message_history()
            .expect("history")
            .iter()
            .any(|message| message.message_id.as_str() == sent_id
                && message.status == MessageStatus::Delivered)
    );

    let queued = run_success(
        env.rimz()
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "@claude", "--", "later"]),
        "queue while running",
    );
    let queued_id = queued_id_from_stdout(&queued.stdout);
    assert_eq!(
        String::from_utf8_lossy(&queued.stdout).trim(),
        format!(
            "queued for @claude#feature-boundary ({queued_id}) — @claude#feature-boundary is running; send now: rimz message steer {queued_id}"
        )
    );
    assert_eq!(
        message_by_id(&env, &MessageId::parse(&queued_id).expect("message id")).status,
        MessageStatus::Queued
    );
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-boundary",
            "worktree_branch": "feature-boundary",
        }),
        pane_env,
    );

    let trace_log = env.project_root.join("zellij-deferred-deliver-trace.log");
    run_success(
        traced_rimz(&env, "zellij-deferred-deliver-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "deliver", "--message-id", &queued_id]),
        "deliver at boundary",
    );
    assert_text_then_enter(&trace_log, &user_message("later"));
    assert_eq!(
        message_by_id(&env, &MessageId::parse(&queued_id).expect("message id")).status,
        MessageStatus::Sent
    );

    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-boundary",
            "prompt": user_message("later"),
            "worktree_branch": "feature-boundary",
        }),
        pane_env,
    );
    assert!(
        env.store()
            .list_message_history()
            .expect("history")
            .iter()
            .any(|message| message.message_id.as_str() == queued_id
                && message.status == MessageStatus::Delivered)
    );
    assert!(env.store().list_messages().expect("messages").is_empty());
    let methods: Vec<_> = env
        .read_events()
        .into_iter()
        .map(|event| event.method)
        .collect();
    for method in ["message.queued", "message.sent", "message.delivered"] {
        assert!(methods.iter().any(|actual| actual == method));
    }
}

#[test]
fn boundary_slash_prompt_parks_behind_sent_prompt_until_turn_ends() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-sent-hold", "feature-sent-hold", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-sent-hold",
            "worktree_branch": "feature-sent-hold",
        }),
        pane_env,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let card_status = || {
        env.store()
            .snapshot_cached()
            .expect("snapshot")
            .agents
            .iter()
            .find(|agent| agent.kind.as_str() == "claude")
            .expect("claude card")
            .status
    };
    assert_eq!(card_status(), rimz::agents::AgentStatus::Success);

    let trace_log = env.project_root.join("zellij-sent-hold-trace.log");
    let sent = run_success(
        traced_rimz(&env, "zellij-sent-hold-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "@claude", "--", "go"]),
        "send prompt at open gate",
    );
    let prompt_id = MessageId::parse(&sent_id_from_stdout(&sent.stdout)).expect("message id");
    assert_text_then_enter(&trace_log, &user_message("go"));
    assert_eq!(message_by_id(&env, &prompt_id).status, MessageStatus::Sent);
    let writes_after_prompt = trace_lines(&trace_log).len();

    let parked = run_success(
        traced_rimz(&env, "zellij-sent-hold-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "@claude", "--", "/compact"]),
        "park slash prompt behind unacknowledged prompt",
    );
    let command_id = queued_id_from_stdout(&parked.stdout);
    assert_eq!(
        String::from_utf8_lossy(&parked.stdout).trim(),
        format!("queued for @claude#feature-sent-hold ({command_id}) — behind {prompt_id}")
    );
    let command_id = MessageId::parse(&command_id).expect("message id");
    assert_eq!(
        trace_lines(&trace_log)[writes_after_prompt..]
            .iter()
            .filter(|line| line.contains("\taction\twrite"))
            .collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "a parked slash prompt must not reach the pane"
    );
    assert_eq!(
        message_by_id(&env, &command_id).status,
        MessageStatus::Queued
    );
    assert_eq!(message_by_id(&env, &prompt_id).status, MessageStatus::Sent);

    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-sent-hold",
            "prompt": user_message("go"),
            "worktree_branch": "feature-sent-hold",
        }),
        pane_env,
    );
    assert!(
        env.store()
            .list_message_history()
            .expect("history")
            .iter()
            .any(|message| message.message_id == prompt_id
                && message.status == MessageStatus::Delivered)
    );
    assert_eq!(card_status(), rimz::agents::AgentStatus::Running);
    assert_eq!(
        message_by_id(&env, &command_id).status,
        MessageStatus::Queued,
        "the acknowledgement must not deliver into the running turn"
    );

    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-sent-hold",
            "worktree_branch": "feature-sent-hold",
        }),
        pane_env,
    );
    run_success(
        traced_rimz(&env, "zellij-sent-hold-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "deliver", "--message-id", command_id.as_str()]),
        "deliver slash prompt at boundary",
    );
    assert_eq!(message_by_id(&env, &command_id).status, MessageStatus::Sent);
    let lines = trace_lines(&trace_log);
    let prompt_at = lines
        .iter()
        .position(|line| is_paste(line, &user_message("go")))
        .expect("prompt paste");
    let command_at = lines
        .iter()
        .position(|line| is_paste(line, &user_message("/compact")))
        .unwrap_or_else(|| panic!("expected the slash prompt in the pane; trace: {lines:?}"));
    assert!(
        writes_after_prompt <= command_at && prompt_at < command_at,
        "the slash prompt reaches the pane after the prompt; trace: {lines:?}"
    );
}

#[test]
fn busy_queue_confirmation_points_to_the_record_steer_command() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-queue-hint", "feature-queue-hint", pane_env);
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);

    let queued = run_success(
        env.rimz().env("RIMZ_TEST_PANE_LIST", &pane_fixture).args([
            "message",
            "@claude",
            "--",
            "send this exact record",
        ]),
        "queue while running",
    );
    let message_id = queued_id_from_stdout(&queued.stdout);
    assert_eq!(
        String::from_utf8_lossy(&queued.stdout).trim(),
        format!(
            "queued for @claude#feature-queue-hint ({message_id}) — @claude#feature-queue-hint is running; send now: rimz message steer {message_id}"
        )
    );

    let trace_log = env.project_root.join("zellij-queue-hint-trace.log");
    let steered = run_success(
        traced_rimz(&env, "zellij-queue-hint-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "steer", &message_id]),
        "steer hinted record",
    );
    assert_eq!(
        String::from_utf8_lossy(&steered.stdout).trim(),
        format!("sent to @claude ({message_id})")
    );
    assert_text_then_enter(&trace_log, &user_message("send this exact record"));
}

#[test]
fn sweep_cancels_joined_subagent_report_without_pane_write() {
    assert_joined_subagent_report_canceled(false);
}

#[test]
fn deliver_cancels_joined_subagent_report_without_pane_write() {
    assert_joined_subagent_report_canceled(true);
}

#[test]
fn sweep_delivers_unjoined_subagent_report() {
    assert_subagent_report_delivered(&[false]);
}

#[test]
fn delivery_rechecks_digest_joined_between_scan_and_claim() {
    assert_digest_joined_during_claim(false);
}

#[test]
fn delivery_does_not_revive_digest_canceled_after_claim() {
    assert_digest_joined_during_claim(true);
}

fn assert_digest_joined_during_claim(cancel: bool) {
    for command in ["sweep", "deliver", "steer"] {
        let (env, digest, pane_fixture) = subagent_report_fixture(&[false]);
        queue_messages(&env, &[&digest]);
        let before = DeliveryRendezvous::new(&env, "before");
        let after = DeliveryRendezvous::new(&env, "after");
        let trace_log = env.project_root.join("digest-race-trace.log");
        let mut delivery = traced_rimz(&env, &trace_log);
        delivery
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .env("RIMZ_TEST_DELIVERY_BEFORE_CLAIM", &before.path)
            .env("RIMZ_TEST_DELIVERY_AFTER_CLAIM", &after.path)
            .args(["message", command]);
        if command == "deliver" {
            delivery.arg("--message-id");
        }
        if command != "sweep" {
            delivery.arg(digest.message_id.as_str());
        }
        let child = delivery
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut before_release = before.arrive();
        assert_eq!(message_by_id(&env, &digest.message_id).attempts, 0);
        let store = env.store();
        let mut run = rimz::harness::run::list(store.paths()).unwrap().remove(0);
        // Persist the final join under the real workspace lock, leaving its
        // separate queue cleanup pending until the consumer has claimed.
        run.joined_at = Some(jiff::Timestamp::from_second(1_001).unwrap());
        rimz::harness::run::create(store.paths(), &run).unwrap();
        before_release.write_all(&[1]).unwrap();
        let mut after_release = after.arrive();
        assert_eq!(
            message_by_id(&env, &digest.message_id).status,
            MessageStatus::Claimed
        );
        if cancel {
            rimz::harness::run::report::join_and_settle_digest(
                &store,
                "session",
                &run.run_id,
                Some(run.follow_ups + 1),
                "joined before delivery",
            )
            .unwrap();
            assert!(store.list_messages().unwrap().is_empty());
        }
        assert_no_report_pane_write(&trace_log);
        after_release.write_all(&[1]).unwrap();
        let output = child.wait_with_output().unwrap();
        if command == "steer" {
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stderr).contains("is no longer queued"));
        } else {
            assert!(output.status.success(), "{output:?}");
        }
        assert_no_report_pane_write(&trace_log);
        assert!(
            store.list_messages().unwrap().is_empty(),
            "canceled digest must not revive"
        );
        let history = store.list_message_history().unwrap();
        let row = history
            .iter()
            .find(|row| row.message_id == digest.message_id)
            .unwrap();
        assert_eq!(row.status, MessageStatus::Canceled);
        assert_eq!(row.attempts, 1);
        assert_eq!(row.last_sent_at, None);
        assert!(
            env.read_events()
                .iter()
                .all(|event| event.method != "message.sent")
        );
    }
}

#[test]
fn delivery_releases_digest_claim_when_post_claim_run_scan_fails() {
    let (env, digest, pane_fixture) = subagent_report_fixture(&[false]);
    queue_messages(&env, &[&digest]);
    let after = DeliveryRendezvous::new(&env, "after");
    let trace_log = env.project_root.join("digest-scan-race-trace.log");
    let child = traced_rimz(&env, &trace_log)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_MESSAGE_SETTLE_MS", "0")
        .env("RIMZ_TEST_DELIVERY_AFTER_CLAIM", &after.path)
        .args([
            "message",
            "deliver",
            "--message-id",
            digest.message_id.as_str(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = after.arrive();
    assert_eq!(
        message_by_id(&env, &digest.message_id).status,
        MessageStatus::Claimed
    );
    let store = env.store();
    let run = rimz::harness::run::list(store.paths()).unwrap().remove(0);
    let path = store.paths().runs_dir.join(format!("{}.json", run.run_id));
    std::fs::write(&path, b"{not a run}\n").unwrap();
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_no_report_pane_write(&trace_log);
    let queued = message_by_id(&env, &digest.message_id);
    assert_eq!(queued.status, MessageStatus::Queued);
    assert_eq!(queued.attempts, 0);
    assert_eq!(queued.last_attempt_at, None);
    assert_eq!(queued.last_sent_at, None);
    assert!(
        queued
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("cannot check or settle joined runs"))
    );

    rimz::harness::run::create(store.paths(), &run).unwrap();
    run_success(
        traced_rimz(&env, &trace_log)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args([
                "message",
                "deliver",
                "--message-id",
                digest.message_id.as_str(),
            ]),
        "retry digest after run repair",
    );
    assert_text_then_enter(
        &trace_log,
        &format!(
            "Type: SUBAGENT_REPORT\nFrom: @rimz\nContent:\n{}",
            digest.text
        ),
    );
    assert_eq!(
        message_by_id(&env, &digest.message_id).status,
        MessageStatus::Sent
    );
}

struct DeliveryRendezvous {
    path: PathBuf,
    listener: std::os::unix::net::UnixListener,
}

impl DeliveryRendezvous {
    fn new(env: &Env, name: &str) -> Self {
        let path = env.project_root.join(format!("{name}.sock"));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        Self { path, listener }
    }

    fn arrive(&self) -> std::os::unix::net::UnixStream {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => return stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "delivery never reached {:?}",
                        self.path
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("delivery rendezvous: {error}"),
            }
        }
    }
}

#[test]
fn sweep_delivers_partially_joined_subagent_report() {
    assert_subagent_report_delivered(&[true, false]);
}

#[test]
fn sweep_delivers_unlinked_subagent_report() {
    assert_subagent_report_delivered(&[]);
}

#[test]
fn sweep_does_not_batch_subagent_report_behind_compatible_message() {
    let (env, digest, pane_fixture) = subagent_report_fixture(&[true]);
    let mut human = digest.clone();
    human.message_id = fixed_message_id(1);
    human.enqueued_at = digest.enqueued_at - jiff::SignedDuration::from_secs(1);
    human.sender = MessageSender::Human;
    human.text = "ordinary prompt".to_owned();
    human.channel = None;
    queue_messages(&env, &[&human, &digest]);

    let trace_log = env.project_root.join("zellij-report-human-trace.log");
    run_success(
        traced_rimz(&env, &trace_log)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "sweep"]),
        "sweep human ahead of joined digest",
    );
    assert_text_then_enter(&trace_log, &user_message(&human.text));
    let lines = trace_lines(&trace_log);
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.contains("\taction\twrite\t"))
            .count(),
        2,
        "only the human paste and Enter: {lines:?}"
    );
    assert_eq!(
        message_by_id(&env, &human.message_id).status,
        MessageStatus::Sent
    );
    let queued = message_by_id(&env, &digest.message_id);
    assert_eq!(queued.status, MessageStatus::Queued);
    assert_eq!(queued.attempts, 0);
    assert_eq!(queued.batch_id, None);
    submit_subagent_report_prompt(&env, &user_message(&human.text));
    assert!(env.store().list_message_history().expect("history").iter().any(|row|
        row.message_id == human.message_id && row.status == MessageStatus::Delivered
    ));
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-report",
            "worktree_branch": "feature-report",
        }),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let trace_log = env.project_root.join("zellij-report-after-human-trace.log");
    run_success(
        traced_rimz(&env, &trace_log)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "sweep"]),
        "sweep joined digest after human acknowledgement",
    );
    assert_subagent_report_canceled(&env, &digest, &trace_log);
}

#[test]
fn sweep_does_not_send_subagent_report_when_run_scan_fails() {
    let (env, digest, pane_fixture) = subagent_report_fixture(&[true]);
    queue_messages(&env, &[&digest]);
    let store = env.store();
    let runs = rimz::harness::run::list(store.paths()).expect("seeded runs");
    let path = store
        .paths()
        .runs_dir
        .join(format!("{}.json", runs[0].run_id));
    std::fs::write(path, b"{not a run}\n").expect("corrupt run record");
    let trace_log = env.project_root.join("zellij-report-scan-error-trace.log");
    run_success(
        traced_rimz(&env, &trace_log)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "sweep"]),
        "sweep with unreadable run",
    );
    assert_no_report_pane_write(&trace_log);
    let queued = message_by_id(&env, &digest.message_id);
    assert_eq!(queued.status, MessageStatus::Queued);
    assert_eq!(queued.attempts, 0);
    assert_eq!(queued.last_sent_at, None);
}

fn subagent_report_fixture(joined: &[bool]) -> (Env, MessageRecord, PathBuf) {
    use rimz::harness::run;
    use rimz::store::run::{RunRecord, RunStatus};

    let env = Env::new();
    env.record(&env.project_root);
    env.write_config(&env.project_root, "");
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-report", "feature-report", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-report",
            "worktree_branch": "feature-report",
        }),
        pane_env,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let store = env.store();
    let snapshot = store.snapshot_cached().expect("snapshot");
    let parent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == "sess-report")
        .expect("parent");
    let mut digest = MessageRecord::new(
        env.workspace_id.clone(),
        parent,
        "@first completed: first result\n@second completed: second result".to_owned(),
        DeliveryGate::Done,
    )
    .with_channel(parent.channel())
    .with_sender(MessageSender::Harness {
        notice: HarnessNotice::SubagentReport,
    })
    .with_pane_id(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
    digest.message_id = fixed_message_id(2);
    let at = jiff::Timestamp::from_second(1_000).expect("fixed timestamp");
    for (index, joined) in joined.iter().enumerate() {
        let mut record = RunRecord::new(
            env.workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            rimz::agents::PermissionMode::Auto,
            "child task".to_owned(),
            env.project_root.clone(),
        );
        record.agent_id = Some(AgentSessionId::from(format!("child-{index}")));
        record.subagent = true;
        record.status = RunStatus::Completed;
        record.started_at = at;
        record.updated_at = at;
        record.completed_at = Some(at);
        record.joined_at = joined.then_some(at);
        record.report_message_id = Some(digest.message_id.clone());
        run::create(store.paths(), &record).expect("seed completed run");
    }
    (env, digest, pane_fixture)
}

fn assert_joined_subagent_report_canceled(deliver: bool) {
    for notice in [HarnessNotice::SubagentReport, HarnessNotice::AgentReport] {
        let (env, mut digest, pane_fixture) = subagent_report_fixture(&[true]);
        digest.sender = MessageSender::Harness { notice };
        queue_messages(&env, &[&digest]);
        let trace_log = env.project_root.join("zellij-report-cancel-trace.log");
        let mut command = traced_rimz(&env, &trace_log);
        command
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0");
        if deliver {
            command.args([
                "message",
                "deliver",
                "--message-id",
                digest.message_id.as_str(),
            ]);
        } else {
            command.args(["message", "sweep"]);
        }
        run_success(&mut command, "consume joined digest");
        assert_subagent_report_canceled(&env, &digest, &trace_log);
    }
}

fn assert_no_report_pane_write(trace_log: &Path) {
    let lines = trace_lines(trace_log);
    assert!(
        lines
            .iter()
            .all(|line| !line.contains("\taction\twrite\t")
                && !line.contains("\taction\twrite-chars\t")),
        "no paste or Enter: {lines:?}"
    );
}

fn assert_subagent_report_canceled(env: &Env, digest: &MessageRecord, trace_log: &Path) {
    assert_no_report_pane_write(trace_log);
    assert!(
        env.store()
            .list_messages()
            .expect("live queue")
            .iter()
            .all(|row| row.message_id != digest.message_id)
    );
    let history = env.store().list_message_history().expect("history");
    let row = history
        .iter()
        .find(|row| row.message_id == digest.message_id)
        .expect("canceled digest in history");
    assert_eq!(row.status, MessageStatus::Canceled);
    assert_eq!(row.attempts, 0);
    assert_eq!(row.last_sent_at, None);
    assert!(
        env.read_events()
            .iter()
            .any(|event| event.method == "message.canceled"
                && event.params_value()["message_id"] == digest.message_id.as_str()
                && event.params_value()["reason"] == "joined before delivery")
    );
}

fn assert_subagent_report_delivered(joined: &[bool]) {
    let (env, digest, pane_fixture) = subagent_report_fixture(joined);
    queue_messages(&env, &[&digest]);
    let trace_log = env.project_root.join("zellij-report-deliver-trace.log");
    run_success(
        traced_rimz(&env, &trace_log)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "sweep"]),
        "sweep unconsumed digest",
    );
    let prompt = format!(
        "Type: SUBAGENT_REPORT\nFrom: @rimz\nContent:\n{}",
        digest.text
    );
    assert_text_then_enter(&trace_log, &prompt);
    assert_eq!(
        message_by_id(&env, &digest.message_id).status,
        MessageStatus::Sent
    );
    submit_subagent_report_prompt(&env, &prompt);
    assert!(env.store().list_messages().expect("live queue").is_empty());
    assert!(
        env.store()
            .list_message_history()
            .expect("history")
            .iter()
            .any(
                |row| row.message_id == digest.message_id && row.status == MessageStatus::Delivered
            )
    );
}

fn submit_subagent_report_prompt(env: &Env, prompt: &str) {
    run_hook(
        env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-report",
            "prompt": prompt,
            "worktree_branch": "feature-report",
        }),
        &[("ZELLIJ_PANE_ID", "3")],
    );
}

#[test]
fn sweep_does_not_claim_command_acknowledged_after_idle_snapshot() {
    let env = Env::new();
    env.record(&env.project_root);
    append_lifecycle(
        &env,
        "claude",
        "SessionStart",
        "sess-sweep-ack",
        LifecycleSignal::Registered,
        |observation| {
            observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
        },
    );
    let store = env.store();
    let snapshot = store.snapshot_cached().unwrap();
    let agent = &snapshot.agents[0];
    assert_eq!(agent.status, rimz::agents::AgentStatus::Idle);
    let mut prompt = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "start a turn".to_owned(),
        DeliveryGate::Done,
    );
    prompt.message_id = fixed_message_id(1);
    let mut command = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "/compact".to_owned(),
        DeliveryGate::Done,
    )
    .with_body(MessageBody::Command);
    command.message_id = fixed_message_id(2);
    queue_messages(&env, &[&prompt, &command]);
    store
        .record_sent_batch(std::slice::from_ref(&prompt), "rimz-test")
        .unwrap();
    let panes = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let after_snapshot = DeliveryRendezvous::new(&env, "sweep-snapshot");
    let trace = env.project_root.join("sweep-ack-trace.log");
    let child = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_PANE_LIST", &panes)
        .env("RIMZ_MESSAGE_DELIVERY_WINDOW_MS", "600000")
        .env("RIMZ_TEST_SWEEP_AFTER_SNAPSHOT", &after_snapshot.path)
        .args(["message", "sweep"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = after_snapshot.arrive();
    append_lifecycle(
        &env,
        "claude",
        "UserPromptSubmit",
        "sess-sweep-ack",
        LifecycleSignal::TurnStarted { turn_id: None },
        |_| {},
    );
    assert_eq!(
        store.snapshot_cached().unwrap().agents[0].status,
        rimz::agents::AgentStatus::Running
    );
    assert_eq!(
        store
            .confirm_delivered_for_card(
                &prompt.kind,
                &prompt.agent_id,
                prompt.agent_name.as_deref(),
                rimz::store::writer::DeliveryAck::TurnStarted { prompt: None },
                "rimz-test",
            )
            .unwrap()
            .len(),
        1
    );
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let queued = message_by_id(&env, &command.message_id);
    assert_eq!(queued.status, MessageStatus::Queued);
    assert_eq!(queued.attempts, 0, "the sweep must not claim mid-turn");
    assert_eq!(queued.last_attempt_at, None);
    assert_no_report_pane_write(&trace);
}

#[test]
fn sweep_requeues_unconfirmed_send_now_message_and_redelivers() {
    let env = Env::new();
    env.record(&env.project_root);
    env.write_config(&env.project_root, "");
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
    let out = run_success(
        traced_rimz(&env, "zellij-reconcile-first-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "@claude", "--", "recover me"]),
        "send-now message",
    );
    let message_id = sent_id_from_stdout(&out.stdout);
    assert_text_then_enter(&first_trace, &user_message("recover me"));
    let first_last_sent_at =
        message_by_id(&env, &MessageId::parse(&message_id).expect("message id"))
            .last_sent_at
            .expect("first send timestamp");

    let second_trace = env.project_root.join("zellij-reconcile-second-trace.log");
    run_success(
        traced_rimz(&env, "zellij-reconcile-second-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_DELIVERY_WINDOW_MS", "0")
            .args(["message", "sweep"]),
        "message sweep",
    );
    assert_text_then_enter(&second_trace, &user_message("recover me"));

    let sent = env
        .store()
        .list_messages()
        .expect("messages")
        .into_iter()
        .find(|message| message.message_id.as_str() == message_id)
        .expect("redelivered message");
    assert_eq!(sent.status, MessageStatus::Sent);
    assert_eq!(
        sent.attempts, 2,
        "fresh and redelivery claims count attempts"
    );
    assert_eq!(sent.unconfirmed_sends, 1);
    assert!(
        sent.last_sent_at.is_some_and(|at| at >= first_last_sent_at),
        "redelivery stamps the latest pane write"
    );

    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-reconcile",
            "prompt": user_message("recover me"),
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
fn sweep_holds_unconfirmed_prompt_while_compaction_bracket_is_open() {
    let env = Env::new();
    env.record(&env.project_root);
    env.write_config(&env.project_root, "");
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(
        &env,
        "sess-compact-reconcile",
        "feature-compact-reconcile",
        pane_env,
    );
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-compact-reconcile",
            "worktree_branch": "feature-compact-reconcile",
        }),
        pane_env,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);

    let first_trace = env
        .project_root
        .join("zellij-compact-reconcile-first-trace.log");
    let out = run_success(
        traced_rimz(&env, "zellij-compact-reconcile-first-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "@claude", "--", "hold me"]),
        "send-now message",
    );
    let message_id = MessageId::parse(&sent_id_from_stdout(&out.stdout)).expect("message id");
    assert_text_then_enter(&first_trace, &user_message("hold me"));

    let workspace = env.resolve_workspace(&env.project_root);
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-compact-reconcile")),
        LifecycleSignal::Compacting,
    );
    observation.worktree_branch = Some("feature-compact-reconcile".to_owned());
    observation.worktree_path = Some(env.project_root.display().to_string());
    observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, TRACE_PANE));
    let mut event = EventEnvelope::agent_lifecycle(
        workspace.workspace_id,
        workspace.session_name,
        "claude",
        "PreCompact",
        &observation,
    );
    event.timestamp =
        jiff::Timestamp::now() - jiff::SignedDuration::from_secs(COMPACTING_WINDOW_SECS + 60);
    env.store()
        .append_event(&event)
        .expect("append old compaction start");

    let second_trace = env
        .project_root
        .join("zellij-compact-reconcile-second-trace.log");
    run_success(
        traced_rimz(&env, "zellij-compact-reconcile-second-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_DELIVERY_WINDOW_MS", "0")
            .args(["message", "sweep"]),
        "message sweep",
    );
    assert_eq!(
        trace_lines(&second_trace)
            .iter()
            .filter(|line| is_paste(line, &user_message("hold me")))
            .count(),
        0,
        "sweep must not paste another copy during compaction"
    );
    let held = message_by_id(&env, &message_id);
    assert_eq!(held.status, MessageStatus::Sent);
    assert_eq!(held.unconfirmed_sends, 0);
    assert_eq!(
        env.read_events()
            .iter()
            .filter(|event| {
                event.method == "message.queued"
                    && event.params_value()["reason"].as_str() == Some("reconcile")
            })
            .count(),
        0
    );

    run_success(
        env.rimz()
            .env("RIMZ_MESSAGE_DELIVERY_WINDOW_MS", "0")
            .args(["gc", "--older-than", "24h"]),
        "gc during compaction",
    );
    let held = message_by_id(&env, &message_id);
    assert_eq!(held.status, MessageStatus::Sent);
    assert_eq!(held.unconfirmed_sends, 0);
    assert_eq!(
        env.read_events()
            .iter()
            .filter(|event| {
                event.method == "message.queued"
                    && event.params_value()["reason"].as_str() == Some("reconcile")
            })
            .count(),
        0
    );

    run_hook(
        &env,
        json!({
            "hook_event_name": "PostCompact",
            "session_id": "sess-compact-reconcile",
            "trigger": "manual",
            "worktree_branch": "feature-compact-reconcile",
        }),
        pane_env,
    );
    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-compact-reconcile",
            "prompt": user_message("hold me"),
            "worktree_branch": "feature-compact-reconcile",
        }),
        pane_env,
    );
    assert!(env.store().list_messages().expect("messages").is_empty());
    assert_eq!(
        env.read_events()
            .iter()
            .filter(|event| event.method == "message.delivered")
            .count(),
        1
    );
    assert_eq!(
        [first_trace, second_trace]
            .iter()
            .flat_map(|trace| trace_lines(trace))
            .filter(|line| is_paste(line, &user_message("hold me")))
            .count(),
        1
    );
}

#[test]
fn mixed_submit_confirms_record_and_never_resends() {
    let env = Env::new();
    env.record(&env.project_root);
    env.write_config(&env.project_root, "");
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-mixed", "feature-mixed", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "PreToolUse",
            "session_id": "sess-mixed",
            "tool_name": "AskUserQuestion",
            "tool_input": { "questions": [{ "question": "Continue?" }] },
            "worktree_branch": "feature-mixed",
        }),
        pane_env,
    );
    assert_eq!(env.snapshot_json()["agents"][0]["status"], "waiting");
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);

    let queued_trace = env.project_root.join("zellij-mixed-queued-trace.log");
    let queued = run_success(
        traced_rimz(&env, "zellij-mixed-queued-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "@claude", "--", "report body"]),
        "queue while question is open",
    );
    let message_id = MessageId::parse(&queued_id_from_stdout(&queued.stdout)).expect("message id");
    assert!(trace_lines(&queued_trace).is_empty());
    assert_eq!(
        message_by_id(&env, &message_id).status,
        MessageStatus::Queued
    );

    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-mixed",
            "worktree_branch": "feature-mixed",
        }),
        pane_env,
    );
    let sent_trace = env.project_root.join("zellij-mixed-sent-trace.log");
    run_success(
        traced_rimz(&env, "zellij-mixed-sent-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "deliver", "--message-id", message_id.as_str()]),
        "deliver after question closes",
    );
    assert_text_then_enter(&sent_trace, &user_message("report body"));
    let sent = message_by_id(&env, &message_id);
    assert_eq!(sent.status, MessageStatus::Sent);
    assert_eq!(sent.attempts, 1);

    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-mixed",
            "prompt": format!("{}do you still", user_message("report body")),
            "worktree_branch": "feature-mixed",
        }),
        pane_env,
    );
    assert!(
        env.store()
            .list_messages()
            .expect("messages")
            .iter()
            .all(|message| message.message_id != message_id)
    );
    let delivered = env
        .store()
        .list_message_history()
        .expect("history")
        .into_iter()
        .find(|message| message.message_id == message_id)
        .expect("delivered prompt");
    assert_eq!(delivered.status, MessageStatus::Delivered);
    assert_eq!(delivered.unconfirmed_sends, 0);
    let delivered_event = env
        .read_events()
        .into_iter()
        .find(|event| {
            event.method == "message.delivered"
                && event.params_value()["message_id"] == message_id.as_str()
        })
        .expect("delivered event");
    assert!(
        delivered_event.params_value()["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("12 stray bytes after"))
    );
    let entries = rimz::transcript::read_all(env.store().paths()).expect("transcript");
    let attributed = entries
        .iter()
        .find(|entry| entry.message_id.as_ref() == Some(&message_id))
        .expect("attributed message");
    assert_eq!(attributed.text, "report body");
    let direct = entries
        .iter()
        .find(|entry| entry.text == "do you still")
        .expect("direct composer input");
    assert_eq!(direct.message_id, None);

    let sweep_trace = env.project_root.join("zellij-mixed-sweep-trace.log");
    run_success(
        traced_rimz(&env, "zellij-mixed-sweep-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_DELIVERY_WINDOW_MS", "0")
            .args(["message", "sweep"]),
        "sweep after mixed acknowledgement",
    );
    assert!(trace_lines(&sweep_trace).is_empty());
}

#[test]
fn late_ack_after_reconcile_window_still_settles_without_resend() {
    let env = Env::new();
    env.record(&env.project_root);
    env.write_config(&env.project_root, "");
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(
        &env,
        "sess-unbounded-ack",
        "feature-unbounded-ack",
        pane_env,
    );
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-unbounded-ack",
            "worktree_branch": "feature-unbounded-ack",
        }),
        pane_env,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let sent_trace = env.project_root.join("zellij-unbounded-ack-sent-trace.log");
    let sent = run_success(
        traced_rimz(&env, "zellij-unbounded-ack-sent-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "@claude", "--", "arrived once"]),
        "send prompt",
    );
    let message_id = MessageId::parse(&sent_id_from_stdout(&sent.stdout)).expect("message id");
    assert_text_then_enter(&sent_trace, &user_message("arrived once"));
    let last_sent_at = message_by_id(&env, &message_id)
        .last_sent_at
        .expect("send timestamp");

    let empty_panes = env.write_pane_fixture(&[]);
    let requeue_trace = env
        .project_root
        .join("zellij-unbounded-ack-requeue-trace.log");
    run_success(
        traced_rimz(&env, "zellij-unbounded-ack-requeue-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &empty_panes)
            .env("RIMZ_MESSAGE_DELIVERY_WINDOW_MS", "0")
            .args(["message", "sweep"]),
        "requeue unconfirmed prompt",
    );
    assert!(trace_lines(&requeue_trace).is_empty());
    let queued = message_by_id(&env, &message_id);
    assert_eq!(queued.status, MessageStatus::Queued);
    assert_eq!(queued.unconfirmed_sends, 1);
    assert_eq!(queued.last_sent_at, Some(last_sent_at));

    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-unbounded-ack",
            "prompt": user_message("arrived once"),
            "worktree_branch": "feature-unbounded-ack",
        }),
        &[
            ("ZELLIJ_PANE_ID", "3"),
            ("RIMZ_MESSAGE_DELIVERY_WINDOW_MS", "0"),
        ],
    );
    assert!(
        env.store()
            .list_messages()
            .expect("messages")
            .iter()
            .all(|message| message.message_id != message_id)
    );
    assert!(
        env.store()
            .list_message_history()
            .expect("history")
            .iter()
            .any(|message| message.message_id == message_id
                && message.status == MessageStatus::Delivered)
    );

    let sweep_trace = env
        .project_root
        .join("zellij-unbounded-ack-sweep-trace.log");
    run_success(
        traced_rimz(&env, "zellij-unbounded-ack-sweep-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "sweep"]),
        "sweep after late acknowledgement",
    );
    assert!(trace_lines(&sweep_trace).is_empty());
}

#[test]
fn shortened_reconcile_window_preserves_prompt_for_late_correlated_ack() {
    let env = Env::new();
    env.record(&env.project_root);
    env.write_config(&env.project_root, "");
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[
        ("ZELLIJ_PANE_ID", "3"),
        ("RIMZ_MESSAGE_DELIVERY_WINDOW_MS", "5000"),
    ];
    register_running_agent(&env, "sess-late-ack", "feature-late-ack", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-late-ack",
            "worktree_branch": "feature-late-ack",
        }),
        pane_env,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let first_trace = env.project_root.join("zellij-late-ack-first-trace.log");
    let sent = run_success(
        traced_rimz(&env, "zellij-late-ack-first-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "@claude", "--", "arrived once"]),
        "send prompt",
    );
    let message_id = MessageId::parse(&sent_id_from_stdout(&sent.stdout)).expect("message id");
    let first_live = env.store().list_messages().expect("messages");
    let first_record = first_live
        .iter()
        .find(|message| message.message_id == message_id)
        .expect("sent prompt");
    let last_sent_at = first_record.last_sent_at.expect("send timestamp");
    assert_text_then_enter(&first_trace, &user_message("arrived once"));

    std::thread::sleep(Duration::from_millis(5_100));
    let empty_panes = env.write_pane_fixture(&[]);
    let retry_trace = env.project_root.join("zellij-late-ack-retry-trace.log");
    run_success(
        traced_rimz(&env, "zellij-late-ack-retry-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &empty_panes)
            .env("RIMZ_MESSAGE_DELIVERY_WINDOW_MS", "5000")
            .args(["message", "sweep"]),
        "requeue unconfirmed prompt",
    );
    assert!(trace_lines(&retry_trace).is_empty());
    let live = env.store().list_messages().expect("messages");
    let queued = live
        .iter()
        .find(|message| message.message_id == message_id)
        .expect("requeued prompt");
    assert_eq!(queued.status, MessageStatus::Queued);
    assert_eq!(queued.unconfirmed_sends, 1);
    assert_eq!(queued.last_sent_at, Some(last_sent_at));

    run_hook(
        &env,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "sess-late-ack",
            "prompt": user_message("arrived once"),
            "worktree_branch": "feature-late-ack",
        }),
        pane_env,
    );

    assert!(
        env.store()
            .list_messages()
            .expect("messages")
            .iter()
            .all(|message| message.message_id != message_id)
    );
    let delivered = env
        .store()
        .list_message_history()
        .expect("history")
        .into_iter()
        .find(|message| message.message_id == message_id)
        .expect("delivered prompt");
    assert_eq!(delivered.status, MessageStatus::Delivered);
}

#[test]
fn send_now_write_failure_leaves_queued_record_for_sweep_retry() {
    let env = Env::new();
    env.record(&env.project_root);
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

    let out = traced_rimz(&env, "zellij-send-fail-trace.log")
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
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
    assert_eq!(pending[0].attempts, 1);
    assert_eq!(pending[0].last_attempt_at, None);
    assert!(pending[0].last_error.is_some(), "send error is recorded");
    assert_eq!(pending[0].pane_id, None, "retry re-resolves a fresh pane");

    let retry_trace = env.project_root.join("zellij-send-retry-trace.log");
    run_success(
        traced_rimz(&env, "zellij-send-retry-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_DELIVERY_WINDOW_MS", "0")
            .args(["message", "sweep"]),
        "message sweep",
    );
    assert_text_then_enter(&retry_trace, &user_message("retry me"));
    let sent = env
        .store()
        .list_messages()
        .expect("messages")
        .into_iter()
        .find(|message| message.message_id.as_str() == message_id)
        .expect("retried message");
    assert_eq!(sent.status, MessageStatus::Sent);
    assert_eq!(sent.attempts, 2);
}

#[test]
fn fresh_claim_is_sent_when_its_recovery_wake_cannot_be_registered() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(
        &env,
        "sess-broken-wake",
        "broken-wake",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    run_hook(
        &env,
        json!({"hook_event_name": "Stop", "session_id": "sess-broken-wake", "worktree_branch": "broken-wake"}),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    std::fs::create_dir(wake_stamp_path(&env)).unwrap();
    let trace = env.project_root.join("broken-wake.log");
    let output = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .args(["message", "@claude", "--", "retry after repair"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(WAKE_REFRESH_WARNING),
        "{output:?}"
    );
    let message = env.store().list_messages().unwrap().remove(0);
    assert_eq!(message.status, MessageStatus::Sent, "{output:?}");
    assert_eq!(message.attempts, 1);
    assert_eq!(message.last_error, None);
    let lines = trace_lines(&trace);
    assert_eq!(
        lines
            .iter()
            .filter(|line| is_paste(line, &user_message("retry after repair")))
            .count(),
        1
    );
    assert_eq!(lines.iter().filter(|line| is_enter_key(line)).count(), 1);
}

#[test]
fn hook_delivery_is_sent_when_its_wake_cannot_be_refreshed() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(
        &env,
        "sess-hook-broken-wake",
        "hook-broken-wake",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let message_id = queue_add(&env, "@claude", "after the turn");
    run_hook(
        &env,
        json!({"hook_event_name": "Stop", "session_id": "sess-hook-broken-wake", "worktree_branch": "hook-broken-wake"}),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    std::fs::remove_file(wake_stamp_path(&env)).unwrap();
    std::fs::create_dir(wake_stamp_path(&env)).unwrap();
    let trace = env.project_root.join("hook-broken-wake.log");
    let output = run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "deliver", "--message-id", &message_id]),
        "hook delivery with a broken wake stamp",
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(WAKE_REFRESH_WARNING),
        "{output:?}"
    );
    let message = env.store().list_messages().unwrap().remove(0);
    assert_eq!(message.status, MessageStatus::Sent, "{output:?}");
    let lines = trace_lines(&trace);
    assert_eq!(
        lines
            .iter()
            .filter(|line| is_paste(line, &user_message("after the turn")))
            .count(),
        1
    );
    assert_eq!(lines.iter().filter(|line| is_enter_key(line)).count(), 1);
}

#[test]
fn fresh_boundary_send_parks_when_another_sender_claims_after_its_read() {
    assert_fresh_boundary_send_race(false);
}

#[test]
fn fresh_boundary_send_parks_when_another_sender_sends_after_its_read() {
    assert_fresh_boundary_send_race(true);
}

fn assert_fresh_boundary_send_race(winner_sent: bool) {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-send-race", "send-race", pane_env);
    run_hook(
        &env,
        json!({"hook_event_name": "Stop", "session_id": "sess-send-race", "worktree_branch": "send-race"}),
        pane_env,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let before_claim = DeliveryRendezvous::new(&env, "fresh-before-claim");
    let before_write = DeliveryRendezvous::new(&env, "winner-before-write");
    let trace = env.project_root.join("fresh-send-race.log");
    let loser = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_TEST_FRESH_SEND_BEFORE_CLAIM", &before_claim.path)
        .args(["message", "@claude", "--", "loser"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release_loser = before_claim.arrive();
    assert!(env.store().list_messages().unwrap().is_empty());
    let winner = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_TEST_PANE_WRITE_BEFORE_LOCK", &before_write.path)
        .args(["message", "@claude", "--", "winner"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release_winner = before_write.arrive();
    let mut winner = Some(winner);
    let held = env.store().list_messages().unwrap().remove(0);
    assert_eq!(held.status, MessageStatus::Claimed);
    assert_eq!(held.attempts, 1);
    let winner_output = if winner_sent {
        release_winner.write_all(&[1]).unwrap();
        let output = winner.take().unwrap().wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            message_by_id(&env, &held.message_id).status,
            MessageStatus::Sent
        );
        Some(output)
    } else {
        None
    };
    release_loser.write_all(&[1]).unwrap();
    let loser_output = loser.wait_with_output().unwrap();
    assert!(loser_output.status.success(), "{loser_output:?}");
    run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "sweep"]),
        "sweep while the winner is in flight",
    );
    let winner_output = winner_output.unwrap_or_else(|| {
        release_winner.write_all(&[1]).unwrap();
        let output = winner.take().unwrap().wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        output
    });
    let lines = trace_lines(&trace);
    assert_eq!(
        lines
            .iter()
            .filter(|line| is_paste(line, &user_message("winner"))
                || is_paste(line, &user_message("loser")))
            .count(),
        1,
        "only the commit winner may paste"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| is_paste(line, &user_message("winner")))
            .count(),
        1
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| is_paste(line, &user_message("loser")))
            .count(),
        0
    );
    assert_eq!(lines.iter().filter(|line| is_enter_key(line)).count(), 1);
    let receipt = String::from_utf8_lossy(&loser_output.stdout);
    assert!(
        receipt.contains(&format!("behind {}", held.message_id)),
        "{receipt}"
    );
    assert_eq!(
        String::from_utf8_lossy(&winner_output.stdout).trim(),
        format!("sent to @claude#send-race ({})", held.message_id)
    );
    let loser_id = MessageId::parse(&queued_id_from_stdout(&loser_output.stdout)).unwrap();
    assert!(loser_id.as_str() > held.message_id.as_str());
    let parked = message_by_id(&env, &loser_id);
    assert_eq!(parked.status, MessageStatus::Queued);
    assert_eq!(parked.attempts, 0);
    assert_eq!(parked.last_attempt_at, None);
    assert_eq!(
        message_by_id(&env, &held.message_id).status,
        MessageStatus::Sent
    );
    let events = env.read_events();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.method == "message.sent")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.method == "message.queued")
            .count(),
        2
    );
    let wake: Option<jiff::Timestamp> =
        serde_json::from_slice(&std::fs::read(wake_stamp_path(&env)).unwrap()).unwrap();
    assert!(
        wake.is_some(),
        "the parked loser must retain a delivery wake"
    );
}

#[test]
fn fresh_sends_exclude_hook_delivery_before_the_pane_write() {
    for mode in [None, Some("--steer"), Some("--interrupt")] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
        register_running_agent(&env, "sess-fresh-claim", "fresh-claim", pane_env);
        run_hook(
            &env,
            json!({
                "hook_event_name": "Stop", "session_id": "sess-fresh-claim", "worktree_branch": "fresh-claim",
            }),
            pane_env,
        );
        let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
        let before = DeliveryRendezvous::new(&env, "fresh-before-lock");
        let trace = env.project_root.join("fresh-claim.log");
        let mut command = traced_rimz(&env, &trace);
        command
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_TEST_PANE_WRITE_BEFORE_LOCK", &before.path)
            .args(["message"]);
        if let Some(mode) = mode {
            command.arg(mode);
        }
        let child = command
            .args(["@claude", "--", "once"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut release = before.arrive();
        let held = env.store().list_messages().unwrap().remove(0);
        let wake: Option<jiff::Timestamp> = std::fs::read(wake_stamp_path(&env))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).unwrap());
        run_success(
            traced_rimz(&env, &trace)
                .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
                .env("RIMZ_MESSAGE_SETTLE_MS", "0")
                .args([
                    "message",
                    "deliver",
                    "--message-id",
                    held.message_id.as_str(),
                ]),
            "concurrent hook delivery",
        );
        release.write_all(&[1]).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            wake,
            Some(held.last_attempt_at.unwrap() + Duration::from_secs(15)),
            "fresh claim must arm recovery before waiting for the pane lock"
        );
        let lines = trace_lines(&trace);
        assert_eq!(
            lines
                .iter()
                .filter(|line| is_paste(line, &user_message("once")))
                .count(),
            1,
            "mode {mode:?}: {lines:?}"
        );
        assert_eq!(lines.iter().filter(|line| is_enter_key(line)).count(), 1);
        assert_eq!(held.status, MessageStatus::Claimed);
        assert_eq!(held.attempts, 1);
        assert_text_then_enter(&trace, &user_message("once"));
        assert_eq!(
            env.read_events()
                .iter()
                .filter(|event| event.method == "message.sent")
                .count(),
            1
        );
    }
}

#[test]
fn expired_fresh_sender_stops_after_sweep_redelivers_its_claim() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-expired-sender", "expired-sender", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop", "session_id": "sess-expired-sender", "worktree_branch": "expired-sender",
        }),
        pane_env,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let before = DeliveryRendezvous::new(&env, "expired-before-lock");
    let trace = env.project_root.join("expired-sender.log");
    let child = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_TEST_PANE_WRITE_BEFORE_LOCK", &before.path)
        .args(["message", "@claude", "--", "once"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = before.arrive();
    let mut expired = env.store().list_messages().unwrap().remove(0);
    expired.last_attempt_at = Some(jiff::Timestamp::now() - Duration::from_secs(60));
    env.store().queue_message(&expired, "rimz-test").unwrap();
    run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "sweep"]),
        "recover waiting sender's claim",
    );
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let lines = trace_lines(&trace);
    assert_eq!(
        lines
            .iter()
            .filter(|line| is_paste(line, &user_message("once")))
            .count(),
        1,
        "stale sender pasted again: {lines:?}"
    );
    assert_eq!(lines.iter().filter(|line| is_enter_key(line)).count(), 1);
    assert_eq!(
        env.read_events()
            .iter()
            .filter(|event| event.method == "message.sent")
            .count(),
        1
    );
    let sent = message_by_id(&env, &expired.message_id);
    assert_eq!(sent.status, MessageStatus::Sent);
    assert_eq!(sent.attempts, 2);
}

#[test]
fn delivery_releases_remaining_claims_when_a_batch_member_is_canceled_before_write() {
    let env = Env::new();
    env.record(&env.project_root);
    run_hook(
        &env,
        json!({"hook_event_name": "SessionStart", "session_id": "sess-lost-batch"}),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let agent = env.store().snapshot_cached().unwrap().agents.remove(0);
    let first = MessageRecord::new(
        env.workspace_id.clone(),
        &agent,
        "first".to_owned(),
        DeliveryGate::Done,
    );
    let second = MessageRecord::new(
        env.workspace_id.clone(),
        &agent,
        "second".to_owned(),
        DeliveryGate::Done,
    );
    queue_messages(&env, &[&first, &second]);
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let before = DeliveryRendezvous::new(&env, "lost-batch-before-lock");
    let trace = env.project_root.join("lost-batch.log");
    let child = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_TEST_PANE_WRITE_BEFORE_LOCK", &before.path)
        .args([
            "message",
            "deliver",
            "--message-id",
            first.message_id.as_str(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = before.arrive();
    assert_eq!(
        message_by_id(&env, &second.message_id).status,
        MessageStatus::Claimed
    );
    assert!(
        env.store()
            .cancel_message(&second.message_id, "rimz-test", "cancel")
            .unwrap()
    );
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_no_report_pane_write(&trace);
    let released = message_by_id(&env, &first.message_id);
    assert_eq!(released.status, MessageStatus::Queued);
    assert_eq!(released.attempts, 0, "a stopped batch spends no attempt");
    assert_eq!(released.last_attempt_at, None);
    assert!(
        !env.read_events()
            .iter()
            .any(|event| event.method == "message.sent" || event.method == "message.errored")
    );
}

#[test]
fn sweep_recovers_a_lane_containing_only_an_expired_claim() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-expired-claim", "expired-claim", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop", "session_id": "sess-expired-claim", "worktree_branch": "expired-claim",
        }),
        pane_env,
    );
    let agent = env.store().snapshot_cached().unwrap().agents.remove(0);
    let queued = MessageRecord::new(
        env.workspace_id.clone(),
        &agent,
        "recover claim".to_owned(),
        DeliveryGate::Done,
    );
    env.store().queue_message(&queued, "rimz-test").unwrap();
    env.store()
        .claim_delivery_batch(
            &queued.message_id,
            rimz::agents::AgentStatus::Idle,
            jiff::Timestamp::now() - Duration::from_secs(60),
        )
        .unwrap()
        .unwrap();
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let trace = env.project_root.join("expired-claim.log");
    run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "sweep"]),
        "sweep expired claim",
    );
    assert_text_then_enter(&trace, &user_message("recover claim"));
    let sent = message_by_id(&env, &queued.message_id);
    assert_eq!(sent.status, MessageStatus::Sent);
    assert_eq!(sent.attempts, 2);
}

#[test]
fn send_now_submit_failure_leaves_sent_record() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let pane_env: &[(&str, &str)] = &[("ZELLIJ_PANE_ID", "3")];
    register_running_agent(&env, "sess-submit-fail", "feature-submit-fail", pane_env);
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-submit-fail",
            "worktree_branch": "feature-submit-fail",
        }),
        pane_env,
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let trace_log = env.project_root.join("zellij-submit-fail-trace.log");

    let out = traced_rimz(&env, "zellij-submit-fail-trace.log")
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_TEST_ZELLIJ_MODE", "fail-enter")
        .args(["message", "@claude", "--", "submit once"])
        .output()
        .expect("send-now message");

    assert!(
        out.status.success(),
        "submit failure after the durable Sent barrier stays sent\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let message_id = MessageId::parse(&sent_id_from_stdout(&out.stdout)).expect("message id");
    assert!(
        trace_lines(&trace_log)
            .iter()
            .any(|line| is_paste(line, &user_message("submit once")))
    );
    assert!(
        trace_lines(&trace_log)
            .iter()
            .any(|line| is_enter_key(line))
    );
    let sent = message_by_id(&env, &message_id);
    assert_eq!(sent.status, MessageStatus::Sent);
    assert_eq!(sent.attempts, 1);
}

#[test]
fn queue_deliver_folds_provisional_message_to_registered_card_name() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
    seed_provisional_codex_launch(
        &env,
        "launch_deferred_fold",
        "swift-otter",
        Some("coder"),
        "terminal_8",
        Some("work"),
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "codex")]);

    let add = run_success(
        env.rimz().env("RIMZ_TEST_PANE_LIST", &pane_fixture).args([
            "message",
            "@coder",
            "--",
            "read plan",
        ]),
        "message",
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
    run_success(
        traced_rimz(&env, "zellij-provisional-deliver-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "deliver", "--message-id", &message_id]),
        "message deliver",
    );

    assert_text_then_enter(&trace_log, &user_message("read plan"));
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
fn queued_delivery_labels_an_ended_root_sender_main() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "receiver", "docs", &[("ZELLIJ_PANE_ID", "3")]);
    append_lifecycle(
        &env,
        "claude",
        "Stop",
        "receiver",
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
        |observation| observation.launch.channel = Some("docs".to_owned()),
    );
    append_lifecycle(
        &env,
        "codex",
        "SessionStart",
        "sender",
        LifecycleSignal::Registered,
        |observation| observation.agent_name = Some("lucid-atlas".to_owned()),
    );
    let snapshot = env.store().snapshot_cached().unwrap();
    let sender = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id == "sender")
        .unwrap();
    assert!(sender.root_lane, "{sender:?}");
    let receiver = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id == "receiver")
        .unwrap();
    assert_eq!(receiver.channel.as_deref(), Some("docs"));
    let message = MessageRecord::new(
        env.workspace_id.clone(),
        receiver,
        "handoff".to_owned(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Agent {
        agent_id: Some(sender.agent_id.clone()),
        kind: sender.kind.clone(),
        name: sender.name.clone(),
        profile: None,
        role: None,
        channel: sender.channel(),
    });
    queue_messages(&env, &[&message]);
    append_lifecycle(
        &env,
        "codex",
        "SessionEnd",
        "sender",
        LifecycleSignal::Ended,
        |_| {},
    );
    let audit = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap();
    assert!(
        audit
            .agents
            .iter()
            .any(|agent| agent.agent_id == "sender" && agent.ended_at.is_some())
    );
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let trace = env.project_root.join("ended-root-sender.log");
    run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args([
                "message",
                "deliver",
                "--message-id",
                message.message_id.as_str(),
            ]),
        "deliver ended root sender",
    );
    let sent = message_by_id(&env, &message.message_id);
    assert_eq!(sent.status, MessageStatus::Sent, "{sent:?}");
    assert_text_then_enter(
        &trace,
        "Type: AGENT_MESSAGE\nFrom: @lucid-atlas#main (codex)\nContent:\nhandoff",
    );
}

#[test]
fn queued_delivery_batches_compatible_prompts() {
    let env = Env::new();
    env.record(&env.project_root);
    env.write_config(&env.project_root, "");
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
            DeliveryGate::Done,
        )
        .with_channel(Some("feature-batch".to_owned()))
        .with_sender(MessageSender::Agent {
            agent_id: None,
            kind: AgentKind::new_unchecked("codex"),
            name: None,
            profile: None,
            role: Some(role.to_owned()),
            channel: Some(sender_channel.to_owned()),
        });
        record.message_id = fixed_message_id(id);
        record
    };
    let mut first = message(1, "first\n", "planner", "feature-batch");
    first.sender = MessageSender::Human;
    let mut second = message(2, "second", "coder", "feature-batch");
    second.sender = MessageSender::Human;
    let third = message(3, "third", "reviewer", "docs");
    let first_id = first.message_id.clone();
    let second_id = second.message_id.clone();
    let third_id = third.message_id.clone();
    queue_messages(&env, &[&first, &second, &third]);

    let trace_log = env.project_root.join("zellij-batch-trace.log");
    run_success(
        traced_rimz(&env, "zellij-batch-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "deliver", "--message-id", first_id.as_str()]),
        "batch delivery",
    );

    let payload = format!("{}\n\n{}", user_message("first\n"), user_message("second"));
    assert_text_then_enter(&trace_log, &payload);
    let lines = trace_lines(&trace_log);
    assert_eq!(
        lines.iter().filter(|line| is_paste(line, &payload)).count(),
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
    assert_eq!(queued_third.attempts, 0, "barrier remains unclaimed");
    let sent_event_ids = env
        .read_events()
        .into_iter()
        .filter(|event| event.method == "message.sent")
        .map(|event| {
            event.params_value()["message_id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sent_event_ids,
        vec![first_id.to_string(), second_id.to_string()]
    );

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
fn compact_first_sends_once_when_its_recovery_wake_cannot_be_registered() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(
        &env,
        "sess-compact-wake-fail",
        "compact-wake-fail",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_context_fill(&env, "sess-compact-wake-fail", 80);
    let trace = env.project_root.join("compact-wake-fail.log");
    let before = DeliveryRendezvous::new(&env, "compact-wake-fail-before-lock");
    let child = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_PANE_WRITE_BEFORE_LOCK", &before.path)
        .args([
            "message",
            "--steer",
            "@claude",
            "--smart-compact",
            "70%",
            "--",
            "go",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = before.arrive();
    std::fs::remove_file(wake_stamp_path(&env)).unwrap();
    std::fs::create_dir(wake_stamp_path(&env)).unwrap();
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(env.store().list_message_history().unwrap().is_empty());
    let live = env.store().list_messages().unwrap();
    assert_eq!(live.len(), 2, "{output:?}");
    for body in [MessageBody::Command, MessageBody::Prompt] {
        let message = live.iter().find(|message| message.body == body).unwrap();
        assert_eq!(message.status, MessageStatus::Sent, "{output:?}");
        assert_eq!(message.attempts, 1);
        assert_eq!(message.last_error, None);
    }
    let lines = trace_lines(&trace);
    assert_eq!(
        lines.iter().filter(|line| is_compact_command(line)).count(),
        1
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| is_paste(line, &user_message("go")))
            .count(),
        1
    );
    assert_eq!(lines.iter().filter(|line| is_enter_key(line)).count(), 2);
    assert_compact_segments_then_enter(
        &lines,
        rimz::config::HarnessConfig::default().compact_instruction(rimz::config::CompactSeat::Solo),
    );
}

#[test]
fn compact_first_claim_arms_recovery_before_the_command_finishes() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(
        &env,
        "sess-compact-wake",
        "compact-wake",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_context_fill(&env, "sess-compact-wake", 80);
    let trace = env.project_root.join("compact-wake.log");
    let before = DeliveryRendezvous::new(&env, "compact-wake-before-lock");
    let token = DeliveryRendezvous::new(&env, "compact-wake-token");
    let child = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_PANE_WRITE_BEFORE_LOCK", &before.path)
        .env("RIMZ_TEST_COMMAND_TOKEN_WRITTEN", &token.path)
        .env("RIMZ_MESSAGE_COMMAND_SUBMIT_DELAY_MS", "0")
        .args([
            "message",
            "--steer",
            "@claude",
            "--smart-compact",
            "70%",
            "--",
            "go",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = before.arrive();
    if wake_stamp_path(&env).exists() {
        std::fs::remove_file(wake_stamp_path(&env)).unwrap();
    }
    release.write_all(&[1]).unwrap();
    let mut release = token.arrive();
    let claimed = env.store().list_messages().unwrap();
    let wake: Option<jiff::Timestamp> = std::fs::read(wake_stamp_path(&env))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).unwrap());
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(claimed.len(), 2);
    assert!(
        claimed
            .iter()
            .all(|message| message.status == MessageStatus::Claimed)
    );
    assert_eq!(
        wake,
        claimed
            .iter()
            .map(|message| message.last_attempt_at.unwrap() + Duration::from_secs(15))
            .min(),
        "compact claim must refresh recovery before writing its command"
    );
}

#[test]
fn steer_auto_compact_runs_before_a_full_window() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(&env, "sess-ac", "feature-ac", &[("ZELLIJ_PANE_ID", "3")]);
    seed_context_fill(&env, "sess-ac", 80);

    let trace_log = env.project_root.join("zellij-ac-trace.log");
    let out = run_traced_smart_compact(&env, &trace_log, "go");

    let lines = trace_lines(&trace_log);
    let compact_at = lines.iter().position(|line| is_compact_command(line));
    let paste_at = lines
        .iter()
        .position(|line| is_paste(line, &user_message("go")));
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
    assert_compact_segments_then_enter(
        &lines,
        rimz::config::HarnessConfig::default().compact_instruction(rimz::config::CompactSeat::Solo),
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
    assert_eq!(
        command.text,
        format!(
            "/compact {}",
            rimz::config::HarnessConfig::default()
                .compact_instruction(rimz::config::CompactSeat::Solo)
        )
    );
    assert_eq!(command.sender, MessageSender::System);
    assert!(command.automated);
    assert_eq!(command.status, MessageStatus::Sent);
    assert_eq!(prompt.text, "go");
    assert_eq!(prompt.status, MessageStatus::Sent);
    let compact_assist = rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None)
        .into_iter()
        .find(|record| {
            matches!(
                &record.assist,
                rimz::harness::assist_log::Assist::AutoCompact { message_id, .. }
                    if message_id == command.message_id.as_str()
            )
        })
        .expect("auto-compact assist");
    assert!(matches!(
        compact_assist.assist,
        rimz::harness::assist_log::Assist::AutoCompact {
            kind,
            agent_id,
            label: Some(label),
            threshold: rimz::store::message::AutoCompact::Percent(70),
            occupied_tokens: None,
            ..
        } if kind.as_str() == "claude" && agent_id == "sess-ac" && label == "@claude"
    ));
}

#[test]
fn sweep_times_out_unconfirmed_compact_without_writing_it_twice() {
    let env = Env::new();
    env.record(&env.project_root);
    env.write_config(&env.project_root, "");
    register_running_agent(
        &env,
        "sess-compact-once",
        "feature-compact-once",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_context_fill(&env, "sess-compact-once", 80);

    let first_trace = env.project_root.join("zellij-compact-once-first-trace.log");
    run_traced_smart_compact(&env, &first_trace, "after compact");
    let command = env
        .store()
        .list_messages()
        .expect("messages")
        .into_iter()
        .find(|message| message.body == MessageBody::Command)
        .expect("compact command");
    assert_eq!(
        trace_lines(&first_trace)
            .iter()
            .filter(|line| is_compact_command(line))
            .count(),
        1
    );

    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let retry_trace = env.project_root.join("zellij-compact-once-retry-trace.log");
    run_success(
        traced_rimz(&env, "zellij-compact-once-retry-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_MESSAGE_COMMAND_DELIVERY_WINDOW_MS", "0")
            .args(["message", "sweep"]),
        "compact reconciliation sweep",
    );

    assert!(
        trace_lines(&retry_trace)
            .iter()
            .all(|line| !is_compact_command(line)),
        "an unconfirmed compact command must not be resent"
    );
    assert!(
        env.store()
            .list_messages()
            .expect("messages")
            .iter()
            .all(|message| message.message_id != command.message_id)
    );
    let timed_out = env
        .store()
        .list_message_history()
        .expect("history")
        .into_iter()
        .find(|message| message.message_id == command.message_id)
        .expect("timed-out command");
    assert_eq!(timed_out.status, MessageStatus::TimedOut);
    assert_eq!(
        timed_out.last_error.as_deref(),
        Some("delivery unconfirmed; command not resent")
    );
}

#[test]
fn message_inherits_smart_compact_default() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(&env, "sess-ac-default", "feature-ac-default", &[]);
    run_success(
        env.rimz()
            .args(["config", "set", "harness.smart_compact", "70%"]),
        "set smart compact default",
    );

    queue_add(&env, "@claude", "inherit compact threshold");

    let messages = env
        .store()
        .list_pending_messages()
        .expect("pending messages");
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert_eq!(messages[0].auto_compact, Some(AutoCompact::Percent(70)));
}

#[test]
fn agents_compact_reports_sibling_delivery_and_terminal_reasons() {
    for delivered in [true, false] {
        let env = Env::new();
        env.record(&env.project_root);
        run_hook(
            &env,
            json!({"hook_event_name": "SessionStart", "session_id": "sess-compact-race"}),
            &[("ZELLIJ_PANE_ID", "3")],
        );
        let before = DeliveryRendezvous::new(&env, "compact-before");
        let trace = env.project_root.join("compact-race.log");
        let child = traced_rimz(&env, &trace)
            .env("RIMZ_TEST_DELIVERY_BEFORE_CLAIM", &before.path)
            .args(["agents", "compact", "@claude"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut release = before.arrive();
        let store = env.store();
        let command = store.list_messages().unwrap().remove(0);
        if !delivered {
            let observation = AgentLifecycleObservation::new(
                Some("sess-compact-race".into()),
                LifecycleSignal::CompactionEnded {
                    auto: Some(false),
                    failed: false,
                },
            );
            store
                .append_event(&EventEnvelope::agent_lifecycle(
                    env.workspace_id.clone(),
                    "rimz-test",
                    "claude",
                    "PostCompact",
                    &observation,
                ))
                .unwrap();
        }
        run_success(
            traced_rimz(&env, &trace).args([
                "message",
                "deliver",
                "--message-id",
                command.message_id.as_str(),
            ]),
            "sibling compaction delivery",
        );
        if delivered {
            store
                .confirm_delivered_for_card(
                    &command.kind,
                    &command.agent_id,
                    command.agent_name.as_deref(),
                    rimz::store::writer::DeliveryAck::Compaction,
                    "rimz-test",
                )
                .unwrap();
        }
        release.write_all(&[1]).unwrap();
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if delivered {
            assert!(output.status.success(), "{stderr}");
            assert!(String::from_utf8_lossy(&output.stdout).starts_with("compacting @claude"));
            assert_eq!(
                trace_lines(&trace)
                    .iter()
                    .filter(|line| is_compact_command(line))
                    .count(),
                1
            );
        } else {
            assert_eq!(output.status.code(), Some(1));
            assert!(
                stderr.contains("a compaction never follows a compaction"),
                "{stderr}"
            );
            assert_no_report_pane_write(&trace);
        }
    }
}

#[test]
fn command_delivery_parks_without_spending_an_attempt_when_compaction_starts_after_claim() {
    let env = Env::new();
    env.record(&env.project_root);
    run_hook(
        &env,
        json!({"hook_event_name": "SessionStart", "session_id": "sess-compact-park"}),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let store = env.store();
    let agent = store.snapshot_cached().unwrap().agents.remove(0);
    let mut command = MessageRecord::new(
        env.workspace_id.clone(),
        &agent,
        "/compact".to_owned(),
        DeliveryGate::Done,
    )
    .with_body(MessageBody::Command);
    command.attempts = rimz::store::message::MAX_DELIVERY_ATTEMPTS - 1;
    queue_messages(&env, &[&command]);
    let after = DeliveryRendezvous::new(&env, "compact-after");
    let trace = env.project_root.join("compact-park.log");
    let child = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_DELIVERY_AFTER_CLAIM", &after.path)
        .args([
            "message",
            "deliver",
            "--message-id",
            command.message_id.as_str(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = after.arrive();
    let held = message_by_id(&env, &command.message_id);
    let wake: Option<jiff::Timestamp> = std::fs::read(wake_stamp_path(&env))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).unwrap());
    let observation = AgentLifecycleObservation::new(
        Some("sess-compact-park".into()),
        LifecycleSignal::Compacting,
    );
    store
        .append_event(&EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "rimz-test",
            "claude",
            "PreCompact",
            &observation,
        ))
        .unwrap();
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let parked = message_by_id(&env, &command.message_id);
    assert_eq!(parked.status, MessageStatus::Queued);
    assert_eq!(parked.attempts, command.attempts);
    assert_eq!(
        wake,
        Some(held.last_attempt_at.unwrap() + Duration::from_secs(15)),
        "delivery claim must arm recovery before the sender continues"
    );
    assert_no_report_pane_write(&trace);
}

#[test]
fn pane_writer_lock_is_shared_across_workspaces_and_released_on_drop() {
    use rimz::disk::lock::WorkspaceLock;
    use rimz::disk::paths::RuntimePaths;
    use rimz::ids::WorkspaceId;
    use rimz::mux::PaneWriter;

    let runtime_root = tempfile::tempdir().unwrap();
    let first = RuntimePaths::under(
        WorkspaceId::from_project_root(Path::new("/first")),
        runtime_root.path(),
    )
    .unwrap();
    let second = RuntimePaths::under(
        WorkspaceId::from_project_root(Path::new("/second")),
        runtime_root.path(),
    )
    .unwrap();
    let pane = PaneId::parse("tmux:%3").unwrap();
    let other = PaneId::parse("tmux:%4").unwrap();
    assert_eq!(first.pane_write_lock(&pane), second.pane_write_lock(&pane));
    let writer = PaneWriter::open(&first, &pane, "room").unwrap();
    assert!(
        WorkspaceLock::try_acquire(&second.pane_write_lock(&pane))
            .unwrap()
            .is_none()
    );
    let other_writer = PaneWriter::open(&second, &other, "room").unwrap();
    drop(writer);
    assert!(
        WorkspaceLock::try_acquire(&second.pane_write_lock(&pane))
            .unwrap()
            .is_some()
    );
    drop(other_writer);
    let unusual = PaneId::parse("tmux:../../outside").unwrap();
    assert_eq!(
        first.pane_write_lock(&unusual).parent(),
        first.pane_write_lock(&pane).parent()
    );
}

#[test]
fn boundary_dispatch_parks_behind_a_claimed_record() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    run_hook(
        &env,
        json!({"hook_event_name": "SessionStart", "session_id": "sess-claim-blocker"}),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let store = env.store();
    let snapshot = store.snapshot_cached().unwrap();
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.kind.as_str() == "claude")
        .unwrap();
    let mut command = MessageRecord::new(
        env.workspace_id.clone(),
        agent,
        "/compact".to_owned(),
        DeliveryGate::Done,
    );
    command.body = MessageBody::Command;
    queue_messages(&env, &[&command]);
    assert!(
        store
            .claim_delivery_batch(
                &command.message_id,
                rimz::agents::AgentStatus::Idle,
                jiff::Timestamp::now(),
            )
            .unwrap()
            .is_some()
    );
    let trace = env.project_root.join("claimed-boundary.log");
    let output = run_success(
        traced_rimz(&env, &trace).args(["message", "@claude", "--", "after the command"]),
        "park behind claim",
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("queued for @claude"));
    let queued = queued_id_from_stdout(&output.stdout);
    assert_eq!(
        message_by_id(&env, &MessageId::parse(&queued).unwrap()).status,
        MessageStatus::Queued
    );
    assert_no_report_pane_write(&trace);
}

#[test]
fn pane_write_lock_holds_a_steer_behind_an_in_flight_command() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    run_hook(
        &env,
        json!({"hook_event_name": "SessionStart", "session_id": "sess-pane-lock"}),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let trace = env.project_root.join("pane-lock.log");
    let token = DeliveryRendezvous::new(&env, "token-written");
    let command = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_COMMAND_TOKEN_WRITTEN", &token.path)
        .env("RIMZ_MESSAGE_COMMAND_SUBMIT_DELAY_MS", "0")
        .args(["agents", "compact", "@claude"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = token.arrive();
    let before_lock = DeliveryRendezvous::new(&env, "before-lock");
    let mut steer = traced_rimz(&env, &trace)
        .env("RIMZ_TEST_PANE_WRITE_BEFORE_LOCK", &before_lock.path)
        .args(["message", "--steer", "@claude", "--", "hello from coder"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    before_lock.arrive().write_all(&[1]).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let waiting = steer.try_wait().unwrap().is_none();
    let pasted = trace_lines(&trace)
        .iter()
        .any(|line| is_paste(line, &user_message("hello from coder")));
    release.write_all(&[1]).unwrap();
    assert!(command.wait_with_output().unwrap().status.success());
    assert!(steer.wait_with_output().unwrap().status.success());
    assert!(waiting && !pasted, "steer wrote inside the compact command");
    let lines = trace_lines(&trace);
    assert_compact_segments_then_enter(
        &lines,
        rimz::config::HarnessConfig::default().compact_instruction(rimz::config::CompactSeat::Solo),
    );
    let paste_at = lines
        .iter()
        .position(|line| is_paste(line, &user_message("hello from coder")))
        .unwrap();
    let enters: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| is_enter_key(line).then_some(index))
        .collect();
    assert_eq!(enters.len(), 2);
    assert!(enters[0] < paste_at && paste_at < enters[1], "{lines:?}");
}

#[test]
fn agents_compact_types_the_native_command_and_refuses_a_repeat() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    run_hook(
        &env,
        json!({"hook_event_name": "SessionStart", "session_id": "sess-manual-compact"}),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let trace = env.project_root.join("manual-compact.log");
    let output = run_success(
        traced_rimz(&env, &trace).args(["agents", "compact", "@claude"]),
        "manual compact",
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("compacting @claude (msg_"));
    assert_compact_segments_then_enter(
        &trace_lines(&trace),
        rimz::config::HarnessConfig::default().compact_instruction(rimz::config::CompactSeat::Solo),
    );
    let messages = env.store().list_messages().expect("messages");
    assert_eq!(messages.len(), 1);
    let command = &messages[0];
    assert_eq!(command.body, MessageBody::Command);
    assert_eq!(command.status, MessageStatus::Sent);
    assert_eq!(command.sender, MessageSender::Human);
    assert!(!command.automated);

    for hook in [
        None,
        Some("PreCompact"),
        Some("PostCompact"),
        Some("SessionStart"),
    ] {
        if let Some(hook) = hook {
            run_hook(
                &env,
                json!({
                    "hook_event_name": hook,
                    "session_id": "sess-manual-compact",
                    "trigger": "manual",
                    "source": "compact",
                }),
                &[("ZELLIJ_PANE_ID", "3")],
            );
        }
        let refused = traced_rimz(&env, &trace)
            .args(["agents", "compact", "@claude"])
            .output()
            .expect("repeat compact");
        assert_eq!(refused.status.code(), Some(1), "after {hook:?}");
        assert!(refused.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(stderr.starts_with("error: @claude: "), "{stderr}");
        assert!(stderr.contains("compaction"), "after {hook:?}: {stderr}");
        if matches!(hook, Some("PostCompact" | "SessionStart")) {
            assert!(stderr.contains("never follows a compaction"), "{stderr}");
            assert!(stderr.contains(" ago "), "{stderr}");
        }
        assert_eq!(
            trace_lines(&trace)
                .iter()
                .filter(|line| is_compact_command(line))
                .count(),
            1,
            "repeat must not write after {hook:?}"
        );
    }
    let delivered = env.store().list_message_history().expect("history");
    assert!(delivered.iter().any(|message| {
        message.message_id == command.message_id && message.status == MessageStatus::Delivered
    }));

    run_hook(
        &env,
        json!({"hook_event_name": "SessionStart", "session_id": "sess-manual-compact", "source": "resume"}),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let resumed = traced_rimz(&env, &trace)
        .args(["agents", "compact", "@claude"])
        .output()
        .unwrap();
    assert_eq!(resumed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&resumed.stderr).contains("never follows a compaction"));

    for hook in ["UserPromptSubmit", "Stop"] {
        run_hook(
            &env,
            json!({
                "hook_event_name": hook,
                "session_id": "sess-manual-compact",
                "prompt": "continue the actual task",
            }),
            &[("ZELLIJ_PANE_ID", "3")],
        );
    }
    let next_trace = env.project_root.join("manual-compact-after-prompt.log");
    let output = run_success(
        traced_rimz(&env, &next_trace).args(["agents", "compact", "@claude"]),
        "compact after a real prompt",
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("compacting @claude (msg_"));
    assert_compact_segments_then_enter(
        &trace_lines(&next_trace),
        rimz::config::HarnessConfig::default().compact_instruction(rimz::config::CompactSeat::Solo),
    );
}

#[test]
fn agents_compact_uses_configured_custom_and_bare_instructions() {
    for (configured, instruction, expected) in [
        (
            "keep the configured context",
            None,
            "keep the configured context",
        ),
        (
            "keep the configured context",
            Some("keep the open questions"),
            "keep the open questions",
        ),
        ("keep the configured context", Some(""), ""),
        ("", None, ""),
    ] {
        let env = Env::new();
        env.record(&env.project_root);
        run_hook(
            &env,
            json!({"hook_event_name": "SessionStart", "session_id": "sess-manual-instruction"}),
            &[("ZELLIJ_PANE_ID", "3")],
        );
        run_success(
            env.rimz()
                .args(["config", "set", "harness.compact_instruction", configured]),
            "set compact instruction",
        );
        let trace = env.project_root.join("manual-instruction.log");
        let mut cmd = traced_rimz(&env, &trace);
        cmd.args(["agents", "compact", "@claude"]);
        if let Some(instruction) = instruction {
            cmd.arg(instruction);
        }
        run_success(&mut cmd, "compact with instruction");
        let messages = env.store().list_messages().expect("messages");
        assert_eq!(messages.len(), 1);
        if !expected.is_empty() {
            assert_eq!(messages[0].text, format!("/compact {expected}"));
            assert_compact_segments_then_enter(&trace_lines(&trace), expected);
            continue;
        }
        assert_eq!(messages[0].text, "/compact");
        let lines = trace_lines(&trace);
        let writes: Vec<_> = lines
            .iter()
            .filter(|line| line.contains("\taction\twrite-chars\t"))
            .collect();
        assert_eq!(writes.len(), 1, "{lines:?}");
        assert!(writes[0].ends_with("\t--\t/compact"), "{lines:?}");
        assert_eq!(lines.iter().filter(|line| is_enter_key(line)).count(), 1);
    }
}

#[test]
fn agents_compact_uses_native_commands_and_refuses_unsupported_instructions() {
    for (kind, native) in [
        ("codex", Some("/compact")),
        ("cursor", Some("/summarize")),
        ("qwen", Some("/compress")),
        ("amp", None),
    ] {
        let env = Env::new();
        env.record(&env.project_root);
        register_role_agent(
            &env,
            kind,
            "sess-native-compact",
            "coder",
            false,
            Some(TRACE_PANE),
        );
        let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, kind)]);
        let trace = env.project_root.join("native-compact.log");
        let refused = traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args([
                "agents",
                "compact",
                "@coder-agent",
                "keep the open questions",
            ])
            .output()
            .expect("unsupported compact instruction");
        assert_eq!(refused.status.code(), Some(1));
        assert!(refused.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(stderr.contains(kind), "{stderr}");
        assert!(
            stderr.contains(if native.is_some() {
                "does not accept a compaction instruction"
            } else {
                "has no native compaction command"
            }),
            "{stderr}"
        );
        assert!(env.store().list_messages().expect("messages").is_empty());
        assert!(
            trace_lines(&trace)
                .iter()
                .all(|line| !line.contains("\taction\twrite"))
        );

        let output = traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["agents", "compact", "@coder-agent"])
            .output()
            .expect("native compact");
        let Some(native) = native else {
            assert_eq!(output.status.code(), Some(1));
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("has no native compaction command")
            );
            assert!(env.store().list_messages().expect("messages").is_empty());
            assert!(
                trace_lines(&trace)
                    .iter()
                    .all(|line| !line.contains("\taction\twrite"))
            );
            continue;
        };
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let messages = env.store().list_messages().expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, native);
        assert_eq!(messages[0].body, MessageBody::Command);
        assert_eq!(messages[0].status, MessageStatus::Sent);
        let lines = trace_lines(&trace);
        let writes: Vec<_> = lines
            .iter()
            .filter(|line| line.contains("\taction\twrite-chars\t"))
            .collect();
        assert_eq!(writes.len(), 1, "{lines:?}");
        assert!(writes[0].ends_with(&format!("\t--\t{native}")), "{lines:?}");
        assert_eq!(lines.iter().filter(|line| is_enter_key(line)).count(), 1);
    }
}

#[test]
fn compaction_left_queued_arms_the_elder_wake() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(
        &env,
        "sess-compact-wake",
        "feature-compact-wake",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    assert!(!wake_stamp_path(&env).exists());
    let trace = env.project_root.join("queued-compact-wake.log");
    let output = run_success(
        traced_rimz(&env, &trace).args(["agents", "compact", "@claude"]),
        "queue running compact",
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).starts_with("queued compaction for @claude (msg_")
    );
    let messages = env.store().list_messages().expect("messages");
    assert_eq!(messages.len(), 1);
    let command = &messages[0];
    assert_eq!(command.status, MessageStatus::Queued);
    assert_eq!(command.body, MessageBody::Command);
    assert!(
        command.expires_at().is_none(),
        "operator commands do not expire"
    );
    let wake: Option<jiff::Timestamp> = serde_json::from_slice(
        &std::fs::read(wake_stamp_path(&env)).expect("queued compaction must arm the elder wake"),
    )
    .expect("wake stamp json");
    let wake = wake.expect("queued compaction wake deadline");
    assert!(
        wake <= jiff::Timestamp::now(),
        "queued compaction must be due"
    );
    assert!(
        trace_lines(&trace)
            .iter()
            .all(|line| !line.contains("\taction\twrite"))
    );
}

#[test]
fn expired_automatic_command_releases_fifo_without_a_sweep_and_is_audited() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_role_agent(
        &env,
        "claude",
        "sess-expiry",
        "expiry",
        false,
        Some(TRACE_PANE),
    );
    let panes = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let trace = env.project_root.join("command-expiry.log");
    let store = env.store();
    let receiver = store.snapshot_cached().unwrap().agents.remove(0);
    let mut command = MessageRecord::new(
        env.workspace_id.clone(),
        &receiver,
        "/compact".to_owned(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::System)
    .with_body(MessageBody::Command)
    .with_automated(true);
    command.message_id = fixed_message_id(1);
    // Backdate rather than race a timer: only the short child-process override
    // expires this record, and the queued state stays open until the explicit sweep.
    command.enqueued_at = jiff::Timestamp::now() - Duration::from_secs(2);
    queue_messages(&env, &[&command]);
    let mut send = traced_rimz(&env, &trace);
    let output = run_success(
        send.env("RIMZ_TEST_PANE_LIST", &panes)
            .env("RIMZ_MESSAGE_COMMAND_VALIDITY_MS", "1000")
            .args(["message", "@expiry-agent", "stage notice"]),
        "send past expired command",
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).starts_with("sent to @expiry"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        message_by_id(&env, &command.message_id).status,
        MessageStatus::Queued
    );
    assert!(store.list_message_history().unwrap().is_empty());
    assert_text_then_enter(&trace, &user_message("stage notice"));
    let shown = run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &panes)
            .env("RIMZ_MESSAGE_COMMAND_VALIDITY_MS", "1000")
            .args(["message", "show", command.message_id.as_str(), "--json"]),
        "show uncleared expiry",
    );
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["delivery"]["check"]["expiry"]["expired"], true);
    let shown = run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &panes)
            .env("RIMZ_MESSAGE_COMMAND_VALIDITY_MS", "1000")
            .args(["message", "show", command.message_id.as_str()]),
        "show expiry verdict",
    );
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.contains("expired: automatic command valid for 1s, queued"),
        "{shown}"
    );
    assert!(shown.contains("the next sweep records it expired"));
    assert!(!shown.contains("force now:"));
    run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &panes)
            .env("RIMZ_MESSAGE_COMMAND_VALIDITY_MS", "1000")
            .args(["message", "sweep"]),
        "clean expired command",
    );
    let expired = store.list_message_history().unwrap().remove(0);
    assert_eq!(expired.status, MessageStatus::Expired);
    assert_eq!(
        expired.last_error.as_deref(),
        Some("expired: automatic command not delivered within 1s of queueing")
    );
    let shown = run_success(
        env.rimz()
            .args(["message", "show", command.message_id.as_str()]),
        "show cleaned expiry",
    );
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.contains("expired") && shown.contains(expired.last_error.as_deref().unwrap()),
        "{shown}"
    );
    assert_eq!(
        list_message_ids(
            &env,
            &[
                "message", "list", "--system", "--all", "--status", "expired", "--json"
            ],
            None
        ),
        vec![command.message_id.to_string()]
    );
    let event = env
        .read_events()
        .into_iter()
        .find(|event| event.method == "message.expired")
        .unwrap();
    assert_eq!(event.params_value()["reason"], expired.last_error.unwrap());
    assert!(
        !trace_lines(&trace)
            .iter()
            .any(|line| is_compact_command(line))
    );
    let confirmed = store
        .confirm_delivered_for_card(
            &receiver.kind,
            &receiver.agent_id,
            receiver.name.as_deref(),
            rimz::store::writer::DeliveryAck::TurnStarted {
                prompt: Some(&user_message("stage notice")),
            },
            "rimz-test",
        )
        .unwrap();
    assert_eq!(confirmed.len(), 1);
    let mut old_prompt = MessageRecord::new(
        env.workspace_id.clone(),
        &receiver,
        "old prompt".to_owned(),
        DeliveryGate::Done,
    );
    old_prompt.message_id = fixed_message_id(2);
    old_prompt.enqueued_at = jiff::Timestamp::UNIX_EPOCH;
    queue_messages(&env, &[&old_prompt]);
    let output = run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &panes)
            .env("RIMZ_MESSAGE_COMMAND_VALIDITY_MS", "1000")
            .args(["message", "@expiry-agent", "later prompt"]),
        "send behind old prompt",
    );
    let output = String::from_utf8_lossy(&output.stdout);
    assert!(output.starts_with("queued for @expiry"), "{output}");
    let later = store
        .list_messages()
        .unwrap()
        .into_iter()
        .find(|record| record.text == "later prompt")
        .unwrap();
    let shown = run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &panes)
            .env("RIMZ_MESSAGE_COMMAND_VALIDITY_MS", "1000")
            .args(["message", "show", later.message_id.as_str(), "--json"]),
        "show old prompt blocker",
    );
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(
        shown["delivery"]["check"]["fifo"]["blocker"],
        old_prompt.message_id.as_str()
    );
}

#[test]
fn expired_automatic_command_cannot_be_forced_into_the_pane() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_role_agent(
        &env,
        "claude",
        "sess-expiry",
        "expiry",
        false,
        Some(TRACE_PANE),
    );
    let panes = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
    let trace = env.project_root.join("forced-expiry.log");
    let receiver = env.store().snapshot_cached().unwrap().agents.remove(0);
    let mut command = MessageRecord::new(
        env.workspace_id.clone(),
        &receiver,
        "/compact".to_owned(),
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::System)
    .with_body(MessageBody::Command)
    .with_automated(true);
    command.enqueued_at = jiff::Timestamp::now() - Duration::from_secs(600);
    queue_messages(&env, &[&command]);
    for verb in ["steer", "interrupt"] {
        let output = traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &panes)
            .args(["message", verb, command.message_id.as_str(), "--force"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "{verb}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("expired: automatic command valid for 10m")
        );
    }
    run_success(
        traced_rimz(&env, &trace)
            .env("RIMZ_TEST_PANE_LIST", &panes)
            .args([
                "message",
                "deliver",
                "--message-id",
                command.message_id.as_str(),
            ]),
        "deliver expired command",
    );
    assert_eq!(
        message_by_id(&env, &command.message_id).status,
        MessageStatus::Queued
    );
    assert!(
        !trace_lines(&trace)
            .iter()
            .any(|line| line.contains("\taction\twrite")),
        "{:?}",
        trace_lines(&trace)
    );
}

#[test]
fn agents_compact_queues_for_a_running_agent() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(
        &env,
        "sess-queued-compact",
        "feature-queued-compact",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let trace = env.project_root.join("queued-compact.log");
    let output = run_success(
        traced_rimz(&env, &trace).args(["agents", "compact", "@claude"]),
        "queue running compact",
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).starts_with("queued compaction for @claude (msg_")
    );
    let messages = env.store().list_messages().expect("messages");
    assert_eq!(messages.len(), 1);
    let command = &messages[0];
    assert_eq!(command.status, MessageStatus::Queued);
    assert_eq!(command.body, MessageBody::Command);
    assert_eq!(command.gate, DeliveryGate::Done);
    assert_eq!(
        list_message_ids(
            &env,
            &[
                "message", "list", "--all", "--system", "--status", "queued", "--json"
            ],
            None
        ),
        vec![command.message_id.to_string()]
    );
    let refused = traced_rimz(&env, &trace)
        .args(["agents", "compact", "@claude"])
        .output()
        .expect("repeat queued compact");
    assert_eq!(refused.status.code(), Some(1));
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8_lossy(&refused.stderr).contains(command.message_id.as_str()));
    assert!(
        trace_lines(&trace)
            .iter()
            .all(|line| !line.contains("\taction\twrite"))
    );

    let shim = zellij_trace_shim();
    run_hook(
        &env,
        json!({
            "hook_event_name": "Stop",
            "session_id": "sess-queued-compact",
            "worktree_branch": "feature-queued-compact",
        }),
        &[
            ("ZELLIJ_PANE_ID", "3"),
            ("RIMZ_ZELLIJ_BIN", shim.to_str().expect("shim path")),
            ("RIMZ_TEST_ZELLIJ_LOG", trace.to_str().expect("trace path")),
            ("RIMZ_MESSAGE_SETTLE_MS", "0"),
            ("RIMZ_MESSAGE_INTERVAL_MS", "0"),
            ("RIMZ_MESSAGE_COMMAND_SUBMIT_DELAY_MS", "0"),
        ],
    );
    wait_for_message_event(&env, "message.sent", Duration::from_secs(5));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !trace_lines(&trace).iter().any(|line| is_enter_key(line)) {
        assert!(
            Instant::now() < deadline,
            "queued compact never pressed Enter"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_compact_segments_then_enter(
        &trace_lines(&trace),
        rimz::config::HarnessConfig::default().compact_instruction(rimz::config::CompactSeat::Solo),
    );
    assert_eq!(
        message_by_id(&env, &command.message_id).status,
        MessageStatus::Sent
    );
}

#[test]
fn queued_compaction_is_rejected_after_a_native_manual_compaction() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(
        &env,
        "sess-native-repeat",
        "feature-native-repeat",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let trace = env.project_root.join("native-repeat.log");
    run_success(
        traced_rimz(&env, &trace).args(["agents", "compact", "@claude"]),
        "queue compact before native compaction",
    );
    let command = env.store().list_messages().unwrap().remove(0);
    let shim = zellij_trace_shim();
    for hook in ["PreCompact", "PostCompact", "SessionStart"] {
        run_hook(
            &env,
            json!({"hook_event_name": hook, "session_id": "sess-native-repeat", "trigger": "manual", "source": "compact"}),
            &[
                ("ZELLIJ_PANE_ID", "3"),
                ("RIMZ_ZELLIJ_BIN", shim.to_str().unwrap()),
                ("RIMZ_TEST_ZELLIJ_LOG", trace.to_str().unwrap()),
                ("RIMZ_MESSAGE_SETTLE_MS", "0"),
                ("RIMZ_MESSAGE_INTERVAL_MS", "0"),
                ("RIMZ_MESSAGE_COMMAND_SUBMIT_DELAY_MS", "0"),
            ],
        );
    }
    run_success(
        traced_rimz(&env, &trace).args(["message", "sweep"]),
        "sweep obsolete compaction",
    );
    wait_for_message_event(&env, "message.errored", Duration::from_secs(5));
    let settled = env
        .store()
        .list_message_history()
        .unwrap()
        .into_iter()
        .find(|record| record.message_id == command.message_id)
        .expect("settled command");
    assert_eq!(settled.status, MessageStatus::Errored);
    assert!(
        settled
            .last_error
            .as_deref()
            .unwrap()
            .contains("a compaction never follows a compaction")
    );
    assert!(
        trace_lines(&trace)
            .iter()
            .all(|line| !line.contains("\taction\twrite"))
    );
}

#[test]
fn smart_compact_sends_the_configured_instruction() {
    for (configured, expected_instruction) in [
        ("keep the open questions", Some("keep the open questions")),
        ("", None),
    ] {
        let env = Env::new();
        env.record(&env.project_root);
        register_running_agent(
            &env,
            "sess-ac-instruction",
            "feature-ac-instruction",
            &[("ZELLIJ_PANE_ID", "3")],
        );
        seed_context_fill(&env, "sess-ac-instruction", 80);
        run_success(
            env.rimz()
                .args(["config", "set", "harness.compact_instruction", configured]),
            "set compact instruction",
        );

        let trace_log = env.project_root.join("zellij-ac-instruction-trace.log");
        run_traced_smart_compact(&env, &trace_log, "go");
        let lines = trace_lines(&trace_log);
        if let Some(instruction) = expected_instruction {
            assert_compact_segments_then_enter(&lines, instruction);
            continue;
        }
        let compact_at = lines
            .iter()
            .position(|line| {
                line.ends_with("\taction\twrite-chars\t--pane-id\tterminal_3\t--\t/compact")
            })
            .expect("bare compact command trace");
        let enter_at = lines[compact_at + 1..]
            .iter()
            .position(|line| is_enter_key(line))
            .map(|at| compact_at + 1 + at)
            .expect("compact Enter trace");
        assert!(
            lines[compact_at + 1..enter_at]
                .iter()
                .all(|line| !line.contains("\taction\twrite-chars\t--pane-id\t")),
            "a bare command must remain one raw write; trace: {lines:?}"
        );
    }
}

#[test]
fn smart_compact_types_the_slash_token_apart_from_its_instruction() {
    let harness = rimz::config::HarnessConfig::default();
    let instruction = harness.compact_instruction(rimz::config::CompactSeat::Solo);
    assert!(
        instruction.len() > 800,
        "the regression instruction must exceed Claude's paste threshold"
    );
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(
        &env,
        "sess-ac-segments",
        "feature-ac-segments",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_context_fill(&env, "sess-ac-segments", 80);

    let trace_log = env.project_root.join("zellij-ac-segments-trace.log");
    run_traced_smart_compact(&env, &trace_log, "go");

    assert_compact_segments_then_enter(&trace_lines(&trace_log), instruction);
}

#[test]
fn boundary_auto_compact_defers_prompt_until_compaction_ends() {
    let env = Env::new();
    env.record(&env.project_root);
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
        traced_rimz(&env, "zellij-ac-boundary-trace.log")
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
            .filter(|line| is_paste(line, &user_message("go after compact")))
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
            .filter(|line| is_paste(line, &user_message("go after compact")))
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
            "prompt": user_message("go after compact"),
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
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_running_agent(
        &env,
        "sess-ac-fail",
        "feature-ac-fail",
        &[("ZELLIJ_PANE_ID", "3")],
    );
    seed_context_fill(&env, "sess-ac-fail", 80);

    let out = traced_rimz(&env, "zellij-ac-fail-trace.log")
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
    env.record(&env.project_root);
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
    let paste_at = first_lines
        .iter()
        .position(|line| is_paste(line, &user_message("go1")));
    assert!(compact_at.is_some() && compact_at < paste_at);

    let second_trace = env.project_root.join("zellij-ac-dupe-second-trace.log");
    run_traced_smart_compact(&env, &second_trace, "go2");
    let second_lines = trace_lines(&second_trace);
    assert!(!second_lines.iter().any(|line| is_compact_command(line)));

    seed_context_tokens(&env, "sess-ac-dupe", 160_000, 200_000);
    let changed_trace = env.project_root.join("zellij-ac-dupe-changed-trace.log");
    run_traced_smart_compact(&env, &changed_trace, "still no prompt");
    assert!(
        !trace_lines(&changed_trace)
            .iter()
            .any(|line| is_compact_command(line))
    );

    run_hook(
        &env,
        json!({"hook_event_name": "UserPromptSubmit", "session_id": "sess-ac-dupe", "prompt": "a real prompt"}),
        &[("ZELLIJ_PANE_ID", "3")],
    );
    let third_trace = env.project_root.join("zellij-ac-dupe-third-trace.log");
    run_traced_smart_compact(&env, &third_trace, "go3");
    let third_lines = trace_lines(&third_trace);
    assert!(third_lines.iter().any(|line| is_compact_command(line)));

    let messages = env.store().list_messages().expect("messages");
    let commands: Vec<_> = messages
        .iter()
        .filter(|message| message.body == MessageBody::Command)
        .collect();
    assert_eq!(commands.len(), 2, "command records: {commands:?}");
    let mut baselines: Vec<_> = commands
        .iter()
        .filter_map(|message| message.compacted_context_tokens)
        .collect();
    baselines.sort_unstable();
    assert_eq!(baselines, vec![150_000, 160_000]);
}

#[test]
fn queue_waiting_agent_defers_unforced_and_force_delivers() {
    {
        let env = Env::new();
        env.record(&env.project_root);
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
        run_success(
            traced_rimz(&env, "zellij-qd-trace.log")
                .env("RIMZ_MESSAGE_SETTLE_MS", "0")
                .args(["message", "@claude", "--", "go"]),
            "deferred message",
        );

        assert_eq!(
            env.store().list_pending_messages().unwrap().len(),
            1,
            "a waiting agent defers delivery; the message stays queued"
        );
        assert!(
            trace_lines(&trace_log)
                .iter()
                .all(|line| !is_paste(line, &user_message("go"))),
            "nothing is pasted while the ask reserves input"
        );
    }

    {
        let env = Env::new();
        env.record(&env.project_root);
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
        run_success(
            traced_rimz(&env, "zellij-qf-trace.log")
                .env("RIMZ_MESSAGE_SETTLE_MS", "0")
                .args(["message", "@claude", "--force", "--", "go"]),
            "forced message",
        );

        assert!(
            env.store().list_pending_messages().unwrap().is_empty(),
            "--force delivers past the waiting agent inline"
        );
        assert_text_then_enter(&trace_log, &user_message("go"));
    }
}

#[test]
fn message_miss_lists_available_agents() {
    let env = Env::new();
    env.record(&env.project_root);
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
        stderr.starts_with("error: no agent matches target `@ghost`"),
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

    let listed = run_success(
        env.rimz().args(["message", "list", "--all", "--json"]),
        "message list",
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

#[test]
fn steer_fanout_requires_opt_in_then_reports_all_targets() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(&env, "sess-amb-a", "feature-aa", &[("ZELLIJ_PANE_ID", "3")]);
    register_running_agent(&env, "sess-amb-b", "feature-ab", &[("ZELLIJ_PANE_ID", "4")]);

    let trace_log = env.project_root.join("zellij-fanout-trace.log");
    let out = traced_rimz(&env, "zellij-fanout-trace.log")
        .args(["message", "--steer", "@claude", "--", "hello"])
        .output()
        .expect("steer ambiguous");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--all"), "stderr: {stderr}");
    assert!(trace_lines(&trace_log).is_empty());

    let out = run_success(
        traced_rimz(&env, "zellij-fanout-trace.log")
            .args(["message", "--steer", "@claude", "--all", "--", "hello"]),
        "steer fanout",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("sent 2 agent(s)"), "stdout: {stdout}");
    let pasted = trace_lines(&trace_log)
        .into_iter()
        .filter(|line| is_paste_to_any_pane(line, &user_message("@claude, hello")))
        .count();
    assert_eq!(pasted, 2);
}

#[test]
fn steer_fanout_skips_blocked_and_steers_the_rest() {
    let env = Env::new();
    env.record(&env.project_root);
    register_running_agent(
        &env,
        "sess-skip-a",
        "feature-ska",
        &[("ZELLIJ_PANE_ID", "7")],
    );
    register_running_agent(
        &env,
        "sess-skip-b",
        "feature-skb",
        &[("ZELLIJ_PANE_ID", "9")],
    );
    push_pending_agent_ask(&env, "sess-skip-b");

    let out = run_success(
        traced_rimz(&env, "zellij-skip-trace.log")
            .args(["message", "--steer", "@claude", "--all", "--", "go"]),
        "steer partial skip",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("sent 1 agent(s)") && stdout.contains("waiting in pane"),
        "summary names the sent and skipped agents: {stdout}"
    );
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    let skipped = env
        .store()
        .list_message_history()
        .unwrap()
        .into_iter()
        .find(|message| message.agent_id.as_str() == "sess-skip-b")
        .expect("skipped target terminal record");
    assert_eq!(skipped.status, MessageStatus::Errored);
    assert_eq!(
        skipped.last_error.as_deref(),
        Some("agent is waiting on input in its pane")
    );
}

#[test]
fn boundary_fanout_preserves_target_order_on_hook_failure() {
    fn scenario(live_first: bool) -> (std::process::Output, Vec<String>, Vec<MessageRecord>) {
        let env = Env::new();
        env.record(&env.project_root);
        let (first_role, first_pane, second_role, second_pane) = if live_first {
            ("live", Some(TRACE_PANE), "parked", None)
        } else {
            ("parked", None, "live", Some(TRACE_PANE))
        };
        register_role_agent(
            &env,
            "claude",
            "sess-a-first",
            first_role,
            false,
            first_pane,
        );
        register_role_agent(
            &env,
            "claude",
            "sess-z-second",
            second_role,
            false,
            second_pane,
        );
        let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);
        let log_name = if live_first {
            "zellij-ordered-fanout.log"
        } else {
            "zellij-reversed-fanout.log"
        };
        let output = traced_rimz(&env, log_name)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "@all", "--", "ordered effect"])
            .output()
            .expect("ordered fanout");
        (
            output,
            trace_lines(&env.project_root.join(log_name)),
            env.store().list_messages().unwrap(),
        )
    }

    for live_first in [true, false] {
        let (output, trace, records) = scenario(live_first);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("hooks"));
        assert_eq!(
            trace
                .iter()
                .any(|line| { is_paste_to_any_pane(line, &user_message("@all, ordered effect")) }),
            live_first
        );
        assert_eq!(records.len(), usize::from(live_first));
        if let Some(record) = records.first() {
            assert_eq!(record.status, MessageStatus::Sent);
            assert_eq!(record.attempts, 1);
        }
    }
}

fn traced_rimz(env: &Env, log_name: impl AsRef<Path>) -> std::process::Command {
    let mut cmd = env.rimz();
    cmd.env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join(log_name));
    cmd
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
        traced_rimz(env, trace_log).args([
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
            .any(|line| is_paste(line, &user_message(text)))
    );
    output
}

fn trace_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|raw| raw.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

const TRACE_PANE: &str = "terminal_3";

fn is_paste(line: &str, text: &str) -> bool {
    let payload = text
        .bytes()
        .map(|byte| if byte == b'\n' { b'\r' } else { byte })
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join("\t");
    line.ends_with(&format!(
        "\taction\twrite\t--pane-id\t{TRACE_PANE}\t27\t91\t50\t48\t48\t126\t{payload}\t27\t91\t50\t48\t49\t126"
    ))
}

fn user_message(text: &str) -> String {
    format!("Type: USER_MESSAGE\nFrom: @user\nContent:\n{text}")
}

fn is_paste_to_any_pane(line: &str, text: &str) -> bool {
    let payload = text
        .bytes()
        .map(|byte| if byte == b'\n' { b'\r' } else { byte })
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join("\t");
    line.contains("\taction\twrite\t--pane-id\t")
        && line.ends_with(&format!(
            "\t27\t91\t50\t48\t48\t126\t{payload}\t27\t91\t50\t48\t49\t126"
        ))
}

fn is_enter_key(line: &str) -> bool {
    line.ends_with(&format!("\taction\twrite\t--pane-id\t{TRACE_PANE}\t13"))
}

fn is_compact_command(line: &str) -> bool {
    line.contains(&format!(
        "\taction\twrite-chars\t--pane-id\t{TRACE_PANE}\t--\t/compact"
    ))
}

fn assert_compact_segments_then_enter(lines: &[String], instruction: &str) {
    let write_prefix = format!("\taction\twrite-chars\t--pane-id\t{TRACE_PANE}\t--\t");
    let head_at = lines
        .iter()
        .position(|line| line.ends_with(&format!("{write_prefix}/compact ")))
        .unwrap_or_else(|| {
            panic!(
                "the slash token must reach the composer in its own write; a >800-char chunk is a paste to Claude; trace: {lines:?}"
            )
        });
    let instruction_at = head_at + 1;
    assert!(
        lines
            .get(instruction_at)
            .is_some_and(|line| line.ends_with(&format!("{write_prefix}{instruction}"))),
        "the compact instruction must follow the slash token in its own write; trace: {lines:?}"
    );
    assert!(
        lines.iter().all(|line| {
            line.find(&write_prefix).is_none_or(|at| {
                !line[at + write_prefix.len()..].starts_with("/compact ")
                    || line.ends_with("/compact ")
            })
        }),
        "the slash token must reach the composer in its own write; a >800-char chunk is a paste to Claude; trace: {lines:?}"
    );
    assert!(
        "/compact ".len() < instruction.len(),
        "the slash-token write must be shorter than the instruction write"
    );
    let paste_prefix = format!("\taction\twrite\t--pane-id\t{TRACE_PANE}\t27\t91\t50\t48\t48\t126");
    let next_paste_at = lines[instruction_at + 1..]
        .iter()
        .position(|line| line.contains(&paste_prefix))
        .map_or(lines.len(), |at| instruction_at + 1 + at);
    assert_eq!(
        lines[instruction_at + 1..next_paste_at]
            .iter()
            .filter(|line| is_enter_key(line))
            .count(),
        1,
        "the split command must have exactly one Enter before the following paste; trace: {lines:?}"
    );
}

fn assert_text_then_enter(trace_log: &Path, text: &str) {
    let raw = std::fs::read_to_string(trace_log).unwrap_or_default();
    // Paste bytes are decimal-encoded in this trace. A literal CR here would
    // therefore come from the byte-faithful raw `write-chars` path instead.
    assert!(
        !raw.contains('\r'),
        "no carriage return should be folded into raw typed text; trace: {raw:?}"
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
    let text_at = text_at.expect("checked above");
    let enter_at = enter_at.expect("checked above");
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

fn register_running_agent_owned_by(
    env: &Env,
    kind: &str,
    session_id: &str,
    branch: &str,
    pane_env: &[(&str, &str)],
    owner_pid: u32,
) {
    let worktree_path = env.home_root.join(branch).display().to_string();
    run_hook_for_owner(
        env,
        kind,
        json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "worktree_branch": branch,
            "worktree_path": worktree_path.as_str(),
        }),
        pane_env,
        owner_pid,
    );
    run_hook_for_owner(
        env,
        kind,
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session_id,
            "prompt": "work",
            "worktree_branch": branch,
            "worktree_path": worktree_path.as_str(),
        }),
        pane_env,
        owner_pid,
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
            LifecycleSignal::TurnStarted { turn_id: None },
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
    let workspace = env.resolve_workspace(&env.project_root);
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

struct ReplyAgentFixture {
    session_id: String,
    branch: String,
    transcript_path: PathBuf,
    pane_id: &'static str,
}

impl ReplyAgentFixture {
    fn single(env: &Env, scenario: &str) -> Self {
        Self::register(
            env,
            format!("sess-wait-{scenario}"),
            format!("feature-wait-{scenario}"),
            env.runtime_root
                .join(format!("message-wait-{scenario}.jsonl")),
            "3",
        )
    }

    fn pair(env: &Env, scenario: &str) -> [Self; 2] {
        Self::pair_named(env, scenario, ["first", "second"])
    }

    fn pair_in_channel(env: &Env, scenario: &str) -> [Self; 2] {
        [("first", "3"), ("second", "4")].map(|(side, pane)| {
            Self::register(
                env,
                format!("sess-wait-{scenario}-{side}"),
                format!("feature-{scenario}"),
                env.runtime_root
                    .join(format!("message-wait-{scenario}-{side}.jsonl")),
                pane,
            )
        })
    }

    fn pair_named(env: &Env, scenario: &str, sides: [&str; 2]) -> [Self; 2] {
        [
            Self::register(
                env,
                format!("sess-wait-{scenario}-{}", sides[0]),
                format!("feature-{scenario}-{}", sides[0]),
                env.runtime_root
                    .join(format!("message-wait-{scenario}-{}.jsonl", sides[0])),
                "3",
            ),
            Self::register(
                env,
                format!("sess-wait-{scenario}-{}", sides[1]),
                format!("feature-{scenario}-{}", sides[1]),
                env.runtime_root
                    .join(format!("message-wait-{scenario}-{}.jsonl", sides[1])),
                "4",
            ),
        ]
    }

    fn register(
        env: &Env,
        session_id: String,
        branch: String,
        transcript_path: PathBuf,
        pane_id: &'static str,
    ) -> Self {
        std::fs::write(&transcript_path, "").expect("seed transcript");
        let fixture = Self {
            session_id,
            branch,
            transcript_path,
            pane_id,
        };
        run_hook(
            env,
            json!({
                "hook_event_name": "SessionStart",
                "session_id": fixture.session_id,
                "worktree_branch": fixture.branch,
                "worktree_path": env.home_root.join(&fixture.branch),
                "transcript_path": fixture.transcript_path,
            }),
            &[("ZELLIJ_PANE_ID", fixture.pane_id)],
        );
        fixture
    }

    fn start(&self, env: &Env, prompt: &str) {
        self.start_reported(env, &user_message(prompt));
    }

    fn start_reported(&self, env: &Env, prompt: &str) {
        run_hook(
            env,
            json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": self.session_id,
                "prompt": prompt,
                "worktree_branch": self.branch,
                "transcript_path": self.transcript_path,
            }),
            &[("ZELLIJ_PANE_ID", self.pane_id)],
        );
    }

    fn send_wake(&self, env: &Env) -> MessageRecord {
        let store = env.store();
        let agent = store
            .snapshot()
            .expect("snapshot")
            .agents
            .into_iter()
            .find(|agent| agent.agent_id.as_str() == self.session_id)
            .expect("reply card");
        let workspace = env.resolve_workspace(&env.project_root);
        let message = MessageRecord::new(
            workspace.workspace_id,
            &agent,
            "wake now".to_owned(),
            DeliveryGate::Done,
        )
        .with_sender(MessageSender::Harness {
            notice: HarnessNotice::Wait,
        });
        store
            .queue_message(&message, "rimz-test")
            .expect("publish wake");
        store
            .record_sent_batch(std::slice::from_ref(&message), "rimz-test")
            .expect("send wake");
        message
    }

    fn stamp_launch_identity(&self, env: &Env, launch_id: &str, name: &str) {
        let workspace = env.resolve_workspace(&env.project_root);
        env.store()
            .append_event(&EventEnvelope::agent_launched(
                workspace.workspace_id,
                workspace.session_name,
                &AgentKind::new_unchecked("claude"),
                AgentLaunchPayload {
                    agent_id: AgentSessionId::from(self.session_id.as_str()),
                    launch_id: Some(AgentSessionId::from(launch_id)),
                    agent_name: name.to_owned(),
                    agent_name_explicit: true,
                    launch: LaunchParams::default(),
                    state: AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: Some(PaneId::from_parts(MuxName::Zellij, self.pane_id)),
                    runtime_owner: None,
                    worktree_path: Some(env.home_root.join(&self.branch).display().to_string()),
                    worktree_branch: Some(self.branch.clone()),
                    prompt: Some("work".to_owned()),
                    description: None,
                },
            ))
            .expect("stamp launch identity");
    }

    fn finish(&self, env: &Env, reply: &str, failed: bool) {
        append_claude_assistant(&self.transcript_path, reply);
        let mut payload = json!({
            "hook_event_name": "Stop",
            "session_id": self.session_id,
            "last_assistant_message": reply,
            "worktree_branch": self.branch,
            "transcript_path": self.transcript_path,
        });
        if failed {
            payload["is_error"] = json!(true);
        }
        run_hook(env, payload, &[("ZELLIJ_PANE_ID", self.pane_id)]);
    }
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

fn seed_context_fill(env: &Env, agent_id: &str, used_pct: u8) {
    let mut context = rimz::agents::AgentContext::new("claude", jiff::Timestamp::now());
    context.tokens = Some(rimz::agents::AgentTokenUsage {
        used_percentage: Some(used_pct),
        ..Default::default()
    });
    let record =
        rimz::agents::context::record::AgentContextRecord::new("claude", agent_id, context);
    rimz::store::agent_context::write_record(&env.runtime_paths(), &record)
        .expect("seed context sidecar");
}

fn seed_context_tokens(env: &Env, agent_id: &str, used: u64, window: u64) {
    let mut context = rimz::agents::AgentContext::new("claude", jiff::Timestamp::now());
    context.tokens = Some(rimz::agents::AgentTokenUsage {
        context_window_size: Some(window),
        current_usage: Some(rimz::agents::AgentCurrentUsage {
            input_tokens: Some(used),
            ..Default::default()
        }),
        ..Default::default()
    });
    let record =
        rimz::agents::context::record::AgentContextRecord::new("claude", agent_id, context);
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
    let mut context = rimz::agents::AgentContext::new("claude", at);
    context.turn_error = Some(AgentTurnError {
        class,
        at,
        label: Some("provider parked".to_owned()),
    });
    let record =
        rimz::agents::context::record::AgentContextRecord::new("claude", agent_id, context);
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
            rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("claude")),
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
    rimz::disk::atomic::write_temp_then_rename_cache(
        &env.runtime_paths().shared_rate_limits_path(),
        &cache,
    )
    .expect("seed rate-limit cache");
}

fn run_hook(env: &Env, payload: serde_json::Value, pane_env: &[(&str, &str)]) {
    let owner = dummy_agent_process();
    let owner_pid = owner.id();
    reap_later(owner);
    run_hook_for_owner(env, "claude", payload, pane_env, owner_pid);
}

fn run_hook_for_owner(
    env: &Env,
    source: &str,
    payload: serde_json::Value,
    pane_env: &[(&str, &str)],
    owner_pid: u32,
) {
    let mut payload = payload;
    stamp_worktree_path(env, &mut payload);
    let payload = serde_json::to_string(&payload).expect("payload");
    let mut cmd = env.hook_command(source);
    scrub_launch_identity(&mut cmd);
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
    // ponytail: bounded sleeper keeps hook-owned agents live; add per-test owner guard if tests outlast this window.
    cmd.arg("30").spawn().expect("spawn dummy agent process")
}

fn reap_later(mut child: std::process::Child) {
    let _ = std::thread::spawn(move || {
        let _ = child.wait();
    });
}

fn scrub_launch_identity(cmd: &mut std::process::Command) {
    for key in [
        rimz::harness::launch::ENV_AGENT_NAME,
        rimz::harness::launch::ENV_AGENT_PROFILE,
        rimz::harness::launch::ENV_AGENT_ROLE,
        rimz::harness::launch::ENV_TEAM,
        rimz::harness::launch::ENV_LAUNCH_GROUP,
        rimz::harness::launch::ENV_LAUNCH_ORDINAL,
        rimz::workspace::ENV_CHANNEL,
        rimz::harness::launch::ENV_AGENT_MODEL,
        rimz::harness::launch::ENV_AGENT_EFFORT,
    ] {
        cmd.env(key, "");
    }
}

fn scrub_bare_agent_identity(cmd: &mut std::process::Command) {
    scrub_launch_identity(cmd);
    for key in [
        rimz::harness::launch::ENV_AGENT_ID,
        rimz::harness::launch::ENV_AGENT_KIND,
        "RIMZ_AGENT_PID",
    ] {
        cmd.env_remove(key);
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
    let workspace = env.resolve_workspace(&env.project_root);
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

fn queue_add(env: &Env, target: &str, text: &str) -> String {
    let out = run_success(env.rimz().args(["message", target, "--", text]), "message");
    queued_id_from_stdout(&out.stdout)
}

fn queue_add_in_channel(env: &Env, channel: &str, target: &str, text: &str) -> String {
    let out = run_success(
        env.rimz()
            .args(["message", "--channel", channel, target, "--", text]),
        "channel message",
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
        cmd.env(rimz::workspace::ENV_CHANNEL, channel);
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
        DeliveryGate::Done,
    )
    .with_channel(Some(channel.to_owned()));
    let message_id = message.message_id.to_string();
    env.store()
        .queue_message(&message, "rimz-test")
        .expect("queue message");
    message_id
}

fn deliver_direct_channel_message(env: &Env, channel: &str, text: &str) -> String {
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
        DeliveryGate::Done,
    )
    .with_channel(Some(channel.to_owned()));
    env.store()
        .queue_message(&message, "rimz-test")
        .expect("queue message");
    env.store()
        .record_sent_batch(std::slice::from_ref(&message), "rimz-test")
        .expect("record sent");
    env.store()
        .confirm_delivered_for_card(
            &agent.kind,
            &agent.agent_id,
            agent.name.as_deref(),
            rimz::store::writer::DeliveryAck::TurnStarted { prompt: None },
            "rimz-test",
        )
        .expect("confirm delivered");
    message.message_id.to_string()
}

const WAKE_REFRESH_WARNING: &str = "cannot refresh the message wake stamp";

fn wake_stamp_path(env: &Env) -> PathBuf {
    env.runtime_paths().lane_path("message-wake.json")
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
        .and_then(|rest| {
            rest.split_whitespace().find_map(|token| {
                token
                    .strip_prefix('(')
                    .and_then(|token| token.strip_suffix(')'))
                    .filter(|token| token.starts_with("msg_"))
            })
        })
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
            native_key: None,
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
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(env.project_root.display().to_string()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        hosted_agent_lineage: Vec::new(),
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
    let workspace = env.resolve_workspace(&env.project_root);
    let kind = AgentKind::new_unchecked("codex");
    let event = EventEnvelope::agent_launched(
        workspace.workspace_id,
        workspace.session_name,
        &kind,
        AgentLaunchPayload {
            agent_id: AgentSessionId::from(launch_id),
            launch_id: None,
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
                ..LaunchParams::default()
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

#[test]
fn steer_reaches_unbound_codex_pane() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("codex");
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "codex")]);

    let trace_log = env.project_root.join("zellij-unbound-trace.log");
    run_success(
        traced_rimz(&env, "zellij-unbound-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "--steer", "@codex", "--", "continue"]),
        "steer to unbound codex pane",
    );
    assert_text_then_enter(&trace_log, &user_message("continue"));
}

#[test]
fn queue_to_provisional_codex_sends_to_live_pane_not_stale_rollup_pane() {
    let env = Env::new();
    env.record(&env.project_root);
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
    run_success(
        traced_rimz(&env, "zellij-provisional-queue-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .args(["message", "@coder", "--", "read plan"]),
        "queue to provisional codex",
    );
    assert_text_then_enter(&trace_log, &user_message("read plan"));
    let messages = env.store().list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].status, MessageStatus::Sent);
    let methods: Vec<String> = env
        .read_events()
        .into_iter()
        .map(|event| event.method)
        .collect();
    assert!(methods.iter().any(|method| method == "message.sent"));
    assert!(methods.iter().any(|method| method == "message.queued"));
}

#[test]
fn provisional_without_live_frame_parks_queue_and_steer() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("codex");
    trust_codex_preflight_hooks(&env);
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
    run_success(
        traced_rimz(&env, "zellij-provisional-no-frame-trace.log").args([
            "message",
            "@coder",
            "--",
            "read plan",
        ]),
        "park provisional queue",
    );
    let messages = env.store().list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].status, MessageStatus::Queued);
    assert_eq!(messages[0].agent_id.as_str(), "launch_no_frame");
    let methods: Vec<String> = env
        .read_events()
        .into_iter()
        .map(|event| event.method)
        .collect();
    assert!(methods.iter().any(|method| method == "message.queued"));
    assert!(methods.iter().all(|method| method != "message.sent"));
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
    let receipt = run_success(
        traced_rimz(&env, "zellij-provisional-no-frame-steer-trace.log").args([
            "message",
            "--steer",
            "@coder",
            "--",
            "read plan",
        ]),
        "park provisional steer",
    );
    assert!(String::from_utf8_lossy(&receipt.stdout).contains(" — no live pane"));
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

#[test]
fn a_bare_word_send_names_the_missing_sigil() {
    let env = Env::new();

    let out = env
        .rimz()
        .args(["message", "codex", "hi"])
        .output()
        .expect("bare-word send");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("must start with `@`") && stderr.contains("try `@codex`"),
        "a send whose sigil is missing gets the one-word fix: {stderr}"
    );

    // With nothing to deliver, the same word is a mistyped subcommand.
    let out = env
        .rimz()
        .args(["message", "lst"])
        .output()
        .expect("mistyped subcommand");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown subcommand `lst`"),
        "stderr: {stderr}"
    );
}
