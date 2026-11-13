//! Integration coverage for channel-scoped addressing: `#name` message routing
//! and bare-role spawn inside a team lane.

use assert_cmd::prelude::*;
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
fn unknown_launch_word_suggests_profiles_and_teams_with_scene_exit_codes() {
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::write_definition(
        &env,
        "agents",
        "perf",
        "description: Performance reviewer\nagent: claude\nmodel: opus\ntools: []",
        "",
    );
    for (args, code) in [
        (vec!["agents", "per"], 2),
        (vec!["agents", "launch", "per"], 1),
    ] {
        let output = env
            .rimz()
            .args(&args)
            .output()
            .expect("unknown launch word");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{stderr}");
        let binary = env.rimz_bin();
        let command =
            shlex::try_join(std::iter::once(binary.to_str().unwrap()).chain(args.iter().copied()))
                .unwrap();
        let expected = format!(
            "error: no team, profile, or command named `per`\n  did you mean:\n    {command}f     profile · claude · opus\n"
        );
        // The built-in peer follows the profile at the same distance.
        assert!(
            stderr.starts_with(expected.trim_end_matches('\n')),
            "{stderr}"
        );
        assert!(stderr.contains("team · claude,codex"), "{stderr}");
    }
}

#[test]
fn unknown_launch_word_uses_the_configured_agents_home() {
    let env = Env::new();
    env.record(&env.project_root);
    let agents_home = env.home_root.join("definitions");
    std::fs::create_dir(&agents_home).unwrap();
    let output = env
        .rimz()
        .args(["agents", "xyz"])
        .env("RIMZ_AGENTS_HOME", &agents_home)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("new team:     ~/definitions/teams/xyz.md"),
        "{stderr}"
    );
    assert!(
        stderr.contains("new profile:  ~/definitions/agents/xyz.md"),
        "{stderr}"
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
        stderr.contains("no team, profile, or command named `planner`"),
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
        !stderr.contains("no team, profile, or command named")
            && stderr.contains("no live RimZ room"),
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
    crate::common::wait::seed_launch_lane(
        env,
        session_id,
        session_id,
        crate::common::wait::stamped_lane(env, &env.project_root, channel),
        team,
    );
    let mut cmd = env.hook_command("claude");
    cmd.env("ZELLIJ_PANE_ID", "3");
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

/// Fires one Claude hook for `session` in pane `pane`, under a stale lane name
/// in the hook environment, as a pane opened by an older launch would carry.
fn stale_env_hook(env: &Env, pane: &str, payload: serde_json::Value) {
    let mut cmd = env.hook_command("claude");
    cmd.env("ZELLIJ_PANE_ID", pane)
        .env(rimz::workspace::ENV_CHANNEL, "stale");
    let output = env
        .spawn_payload(cmd, &payload.to_string())
        .wait_with_output()
        .expect("wait hook");
    env.drain_hooks();
    assert!(
        output.status.success(),
        "hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn row_lane(env: &Env, session: &str) -> Option<rimz::ids::Lane> {
    env.store()
        .snapshot()
        .expect("snapshot")
        .agents
        .iter()
        .find(|agent| agent.agent_id.as_str() == session)
        .and_then(|agent| agent.lane.clone())
}

#[test]
fn hook_only_registration_takes_and_keeps_its_checkouts_own_lane() {
    if crate::common::git::git_missing() {
        return;
    }
    let env = Env::new();
    crate::common::git::init_repo(&env.project_root);
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    env.rimz()
        .args(["worktree", "new", "demo"])
        .assert()
        .success();
    let worktree = rimz::utils::path::normalize_path_lexical(
        &rimz::worktree::worktree_path(&env.project_root, &Default::default(), "demo").unwrap(),
    );
    let foreign = env.home_root.join("elsewhere");
    std::fs::create_dir_all(&foreign).unwrap();
    crate::common::git::init_repo(&foreign);
    let own = |dir: &std::path::Path| rimz::worktree::own_lane(&env.project_root, dir);

    for (pane, session, dir) in [
        ("3", "trunk-hook", &env.project_root),
        ("4", "worktree-hook", &worktree),
        ("5", "foreign-hook", &foreign),
    ] {
        stale_env_hook(
            &env,
            pane,
            json!({"hook_event_name": "SessionStart", "session_id": session, "cwd": dir}),
        );
        assert_eq!(row_lane(&env, session), Some(own(dir)), "{session}");
    }
    assert!(own(&env.project_root).is_trunk());
    assert_eq!(own(&worktree).path(), worktree.to_str().unwrap());
    assert_eq!(own(&foreign).path(), foreign.to_str().unwrap());

    stale_env_hook(
        &env,
        "3",
        json!({"hook_event_name": "SessionStart", "session_id": "trunk-hook", "cwd": &worktree}),
    );
    assert_eq!(
        row_lane(&env, "trunk-hook"),
        Some(own(&env.project_root)),
        "a registration from another directory keeps the first lane"
    );
}

#[test]
fn a_named_lane_agents_transcript_entries_carry_its_lane_name() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let design = crate::common::wait::stamped_lane(&env, &env.project_root, "design");
    crate::common::wait::seed_launch_lane(&env, "named-hook", "named-hook", design.clone(), None);
    for payload in [
        json!({"hook_event_name": "SessionStart", "session_id": "named-hook"}),
        json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "named-hook",
            "prompt": "plan the change",
        }),
        json!({
            "hook_event_name": "Stop",
            "session_id": "named-hook",
            "last_assistant_message": "planned",
        }),
    ] {
        stale_env_hook(&env, "3", payload);
    }

    assert_eq!(row_lane(&env, "named-hook"), Some(design));
    let entries = rimz::transcript::read_all(env.store().paths()).expect("transcript");
    let ours = entries
        .iter()
        .filter(|entry| entry.agent_id.as_str() == "named-hook")
        .collect::<Vec<_>>();
    assert!(ours.len() >= 2, "{entries:#?}");
    for entry in ours {
        assert_eq!(
            entry.channel.as_ref().map(rimz::ids::LaneName::as_str),
            Some("design"),
            "{entry:#?}"
        );
    }
}

#[test]
fn a_sessionless_wired_pane_is_addressed_in_its_checkouts_own_lane() {
    if crate::common::git::git_missing() {
        return;
    }
    let env = Env::new();
    crate::common::git::init_repo(&env.project_root);
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    env.rimz()
        .args(["worktree", "new", "auth"])
        .assert()
        .success();
    let worktree = rimz::utils::path::normalize_path_lexical(
        &rimz::worktree::worktree_path(&env.project_root, &Default::default(), "auth").unwrap(),
    );

    for (dir, lane) in [(&worktree, "auth"), (&env.project_root, "main")] {
        let mut pane = agent_pane(&env, "claude");
        pane.cwd = Some(dir.display().to_string());
        let own = rimz::worktree::own_lane(&env.project_root, dir);
        let snapshot = env.snapshot_json_with_panes(std::slice::from_ref(&pane));
        assert_eq!(
            snapshot["agent_panes"][0]["lane"],
            serde_json::to_value(&own).unwrap(),
            "#{lane}: {:#}",
            snapshot["agent_panes"]
        );

        let pane_fixture = env.write_pane_fixture(&[pane]);
        let sent = env
            .rimz()
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
            .env(
                "RIMZ_TEST_ZELLIJ_LOG",
                env.project_root.join(format!("zellij-{lane}-trace.log")),
            )
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .args(["message", "--steer", &format!("@claude#{lane}"), "--", "hi"])
            .output()
            .expect("message the sessionless pane");
        assert!(
            sent.status.success(),
            "#{lane}: {}",
            String::from_utf8_lossy(&sent.stderr)
        );
    }
}
