//! Integration coverage for channel-scoped addressing: `#name` message routing
//! and bare-role spawn inside a team lane.

use rimz::store::message::MessageStatus;
use serde_json::json;

use crate::common::{Env, zellij_trace_shim};

#[test]
fn message_routes_to_named_channel_targets() {
    let env = Env::new();
    env.record(&env.project_root);
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
        String::from_utf8_lossy(&inline.stdout).contains("sent to "),
        "inline channel message sent: {}",
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
        stderr.contains("in #ops")
            && stderr.contains("message '@claude#design' -- 'wrong channel'"),
        "channel miss names target and real channel: {stderr}"
    );

    let mut messages = env.store().list_messages().expect("messages");
    messages.sort_by(|a, b| a.message_id.as_str().cmp(b.message_id.as_str()));
    assert_eq!(messages.len(), 2, "only successful sends are recorded");
    assert!(
        messages
            .iter()
            .all(|message| message.agent_id.as_str() == "sess-channel-message")
    );
    assert_eq!(
        messages
            .iter()
            .map(|message| message.status)
            .collect::<Vec<_>>(),
        [MessageStatus::Sent, MessageStatus::Queued],
        "the second send parks behind the first, still unacknowledged, prompt"
    );
}

#[test]
fn bare_role_spawn_resolves_against_the_lane_team() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    write_forge_team_config(&env);
    register_idle_lane_agent(&env, "sess-forge-planner", "forge", Some("forge"));

    // Outside any lane, a bare role is not a spec RimZ knows.
    let outside = env
        .rimz()
        .args(["agents", "planner"])
        .output()
        .expect("spawn agents");
    let stderr = String::from_utf8_lossy(&outside.stderr).into_owned();
    assert!(
        stderr.contains("unknown team `planner`"),
        "bare role outside a lane stays unknown: {stderr}"
    );

    // Inside the forge lane it resolves to `forge.planner`, so resolution is no
    // longer what stops the launch.
    let inside = env
        .rimz()
        .args(["agents", "planner"])
        .env(rimz::workspace::ENV_CHANNEL, "forge")
        .output()
        .expect("spawn agents");
    let stderr = String::from_utf8_lossy(&inside.stderr).into_owned();
    assert!(
        !stderr.contains("unknown team") && stderr.contains("no live RimZ room"),
        "bare role in its team lane resolves and reaches the room check: {stderr}"
    );
}

#[test]
fn bare_role_colliding_with_a_cell_word_refuses() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    write_forge_team_config(&env);
    register_idle_lane_agent(&env, "sess-forge-planner", "forge", Some("forge"));

    // The team binds role `reviewer` to claude while the global `reviewer`
    // profile is codex, so the bare spec would mean two different agents.
    let out = env
        .rimz()
        .args(["agents", "reviewer"])
        .env(rimz::workspace::ENV_CHANNEL, "forge")
        .output()
        .expect("spawn agents");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "ambiguous spawn refuses: {stderr}");
    assert!(
        stderr.contains("is ambiguous in `#forge`") && stderr.contains("forge.reviewer"),
        "refusal names the lane and the qualified form: {stderr}"
    );
}

/// A `forge` team whose roles cover both spawn cases: `planner` is a plain role,
/// and `reviewer` shares its name with a global profile that resolves to a
/// different agent.
fn write_forge_team_config(env: &Env) {
    crate::common::write_definition(
        env,
        "agents",
        "claude",
        "description: Claude base",
        "Follow instructions.",
    );
    crate::common::write_definition(
        env,
        "agents",
        "worker",
        "description: Team worker\nagent: claude\ntools: []",
        "",
    );
    crate::common::write_definition(
        env,
        "agents",
        "reviewer",
        "description: Global reviewer\nagent: codex\ntools: []",
        "",
    );
    crate::common::write_definition(
        env,
        "teams",
        "forge",
        "leader: planner\nstages: [Build]\nroles:\n  - {role: planner, agent: worker, owns: [Build]}\n  - {role: reviewer, agent: worker}",
        "Complete the work.",
    );
}

const TRACE_PANE: &str = "terminal_3";

fn register_idle_channel_agent(env: &Env, session_id: &str, channel: &str) {
    register_idle_lane_agent(env, session_id, channel, None);
}

/// Seed one idle agent stamped into `channel`, optionally carrying the team it
/// launched under — the stamp channel-aware spawn reads the lane's team from.
pub(crate) fn register_idle_lane_agent(
    env: &Env,
    session_id: &str,
    channel: &str,
    team: Option<&str>,
) {
    let payload = json!({
        "hook_event_name": "SessionStart",
        "session_id": session_id,
        "worktree_path": env.project_root.display().to_string(),
    })
    .to_string();
    let mut cmd = env.hook_command("claude");
    cmd.env(rimz::workspace::ENV_CHANNEL, channel)
        .env("ZELLIJ_PANE_ID", "3");
    if let Some(team) = team {
        cmd.env(rimz::harness::launch::ENV_TEAM, team);
    }
    let output = env
        .spawn_payload(cmd, &payload)
        .wait_with_output()
        .expect("wait channel hook");
    env.drain_hooks();
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
