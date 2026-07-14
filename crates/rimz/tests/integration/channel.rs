//! Integration coverage for `rimz channel`.

use assert_cmd::assert::OutputAssertExt;
use predicates::str::contains;
use rimz::message::MessageStatus;
use serde_json::{Value, json};
use std::path::PathBuf;

use crate::common::Env;

#[test]
fn channel_new_list_and_remove_round_trip() {
    let env = Env::new();

    env.rimz()
        .args(["channel", "new", "design"])
        .assert()
        .success()
        .stdout(contains("created design"));

    let out = env
        .rimz()
        .args(["channel", "list", "--json"])
        .output()
        .expect("spawn list");
    assert!(out.status.success(), "channel list succeeds");
    let parsed: Value = serde_json::from_slice(&out.stdout).expect("json");
    let entries = parsed.as_array().expect("array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["channel"], "design");
    assert_eq!(entries[0]["backing"], "named");
    assert_eq!(entries[0]["agents"].as_array().expect("agents").len(), 0);

    env.rimz()
        .args(["channel", "rm", "design"])
        .assert()
        .success()
        .stdout(contains("removed design"));

    let out = env
        .rimz()
        .args(["channel", "list", "--json"])
        .output()
        .expect("spawn list");
    assert!(out.status.success(), "channel list succeeds");
    let parsed: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert!(parsed.as_array().expect("array").is_empty());
}

#[test]
fn channel_new_validates_bare_names() {
    let env = Env::new();

    env.rimz()
        .args(["channel", "new", "bad/name"])
        .assert()
        .failure()
        .stderr(contains("invalid channel name"));
}

#[test]
fn message_routes_to_named_channel_targets() {
    let env = Env::new();
    env.install_agent_hooks("claude");
    register_idle_channel_agent(&env, "sess-channel-message", "design");
    let pane_fixture = env.write_pane_fixture(&[agent_pane(&env, "claude")]);

    let inline_trace = env.project_root.join("zellij-channel-inline-trace.log");
    let inline = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &inline_trace)
        .env("RIMZ_MESSAGE_SETTLE_MS", "0")
        .args(["message", "@claude#design", "--", "inline channel"])
        .output()
        .expect("message inline channel");
    assert!(
        inline.status.success(),
        "inline channel message failed: {}",
        String::from_utf8_lossy(&inline.stderr)
    );
    assert!(
        String::from_utf8_lossy(&inline.stdout).contains("sent "),
        "inline channel message delivered: {}",
        String::from_utf8_lossy(&inline.stdout)
    );

    let flag_trace = env.project_root.join("zellij-channel-flag-trace.log");
    let flag = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &flag_trace)
        .env("RIMZ_MESSAGE_SETTLE_MS", "0")
        .args([
            "message",
            "--channel",
            "design",
            "@claude",
            "--",
            "flag channel",
        ])
        .output()
        .expect("message --channel");
    assert!(
        flag.status.success(),
        "--channel message failed: {}",
        String::from_utf8_lossy(&flag.stderr)
    );

    let miss = env
        .rimz()
        .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env(
            "RIMZ_TEST_ZELLIJ_LOG",
            env.project_root.join("zellij-channel-miss-trace.log"),
        )
        .args(["message", "@claude#ops", "--", "wrong channel"])
        .output()
        .expect("message wrong channel");
    assert!(!miss.status.success(), "wrong channel must fail");
    let stderr = String::from_utf8_lossy(&miss.stderr);
    assert!(
        stderr.contains("channel `#ops`") && stderr.contains("`design`"),
        "channel miss names target and real channel: {stderr}"
    );

    let messages = env.store().list_messages().expect("messages");
    assert_eq!(messages.len(), 2, "only successful sends are recorded");
    assert!(messages.iter().all(|message| {
        message.agent_id.as_str() == "sess-channel-message" && message.status == MessageStatus::Sent
    }));
}

const TRACE_PANE: &str = "terminal_3";

fn register_idle_channel_agent(env: &Env, session_id: &str, channel: &str) {
    let payload = json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
        "worktree_path": env.project_root.display().to_string(),
    })
    .to_string();
    let mut cmd = env.hook_command("claude");
    cmd.env(rimz::harness::run::ENV_CHANNEL, channel)
        .env("ZELLIJ_PANE_ID", "3");
    let output = env
        .spawn_payload(cmd, &payload)
        .wait_with_output()
        .expect("wait channel hook");
    assert!(
        output.status.success(),
        "channel hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot = env.store().snapshot_cached().expect("snapshot");
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == session_id)
        .expect("channel agent");
    assert_eq!(agent.channel.as_deref(), Some(channel));
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

fn zellij_trace_shim() -> PathBuf {
    crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace"))
}
