//! Stage notices consumed by a registered provider shim on a private tmux server.

use super::support::*;
use rimz::agents::LaunchParams;
use rimz::ids::AgentSessionId;
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};
use rimz::store::message::{DeliveryGate, MessageStatus};
use std::process::Stdio;

#[test]
fn flip_and_registration_rewake_reach_receiver_after_done() {
    require_tmux!();
    let env = Env::new();
    env.install_agent_hooks("claude");
    let config = env.config_root().join("rimz/agents.toml");
    std::fs::create_dir_all(config.parent().expect("config parent")).unwrap();
    std::fs::write(
        config,
        r#"
[agents]
isolation = "host"
[agents.teams.forge]
stages = ["Build", "Review"]
[[agents.teams.forge.roles]]
role = "coder"
profile = "claude"
owns = ["Build"]
[[agents.teams.forge.roles]]
role = "reviewer"
profile = "claude"
owns = ["Review"]
"#,
    )
    .unwrap();
    let received = env.home_root.join("received");
    let ready = env.home_root.join("receiver-pid");
    let shim = env.home_root.join("claude");
    std::fs::write(
        &shim,
        r#"#!/bin/bash
set -eu
stty -echo
: > "$1"
printf '%s' "$$" > "$2"
while IFS= read -r line; do
    printf '%s\n' "$line" >> "$1"
done
"#,
    )
    .unwrap();
    chmod_executable(&shim);
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).unwrap();
    let server = TmuxServer::in_runtime_root(&env.runtime_root);
    let mut options = session_opts(
        &workspace.session_name,
        workspace.workspace_id.clone(),
        &workspace.project_root,
        &workspace.worktree_root,
        Some((160, 40)),
    );
    options
        .extra_env
        .extend(env.rimz().get_envs().filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
        }));
    server.backend.ensure_session(&options).unwrap();
    let pane = PaneId::from_parts(
        MuxName::Tmux,
        server.display(&workspace.session_name, "#{pane_id}"),
    );
    let mut argv = vec!["env".to_owned()];
    for (key, value) in env.rimz().get_envs() {
        if value.is_none() {
            argv.extend(["-u".to_owned(), key.to_str().unwrap().to_owned()]);
        }
    }
    argv.extend([
        "RIMZ_AGENT_KIND=claude".to_owned(),
        "RIMZ_AGENT_NAME=reviewer".to_owned(),
        "RIMZ_AGENT_ROLE=reviewer".to_owned(),
        "RIMZ_AGENT_ID=launch_reviewer".to_owned(),
        "RIMZ_TEAM=forge".to_owned(),
        shim.to_str().unwrap().to_owned(),
        received.to_str().unwrap().to_owned(),
        ready.to_str().unwrap().to_owned(),
    ]);
    let command = shlex::try_join(argv.iter().map(String::as_str)).unwrap();
    server.output(&["respawn-pane", "-k", "-t", pane.raw(), &command]);
    wait_for_text(&ready, "", &server, &pane);
    let pid = std::fs::read_to_string(&ready).unwrap();
    let store = env.store();
    store
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            AgentLaunchPayload {
                agent_id: AgentSessionId::from("launch_reviewer"),
                launch_id: Some(AgentSessionId::from("launch_reviewer")),
                agent_name: "reviewer".to_owned(),
                agent_name_explicit: true,
                launch: LaunchParams {
                    team: Some("forge".to_owned()),
                    role: Some("reviewer".to_owned()),
                    channel: Some("stage-test".to_owned()),
                    ..Default::default()
                },
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: Some(pane.clone()),
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    let command = || {
        let mut command = env.rimz();
        command
            .env(rimz::workspace::ENV_CHANNEL, "stage-test")
            .args(["--mux", "tmux"]);
        command
    };
    let hook = |event: &str| {
        let mut hook = command();
        hook.args(["hooks", "feed", "--source", "claude"])
            .env(rimz::harness::launch::ENV_AGENT_NAME, "reviewer")
            .env("RIMZ_AGENT_PID", &pid)
            .env("TMUX_PANE", pane.raw())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = env
            .spawn_payload(
                hook,
                &serde_json::json!({
                    "hook_event_name": event,
                    "session_id": "stage-reviewer",
                    "cwd": env.project_root,
                    "prompt": "review the change"
                })
                .to_string(),
            )
            .wait_with_output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    hook("SessionStart");
    hook("UserPromptSubmit");
    let board = env.project_root.join("blackboard.md");
    std::fs::write(
        &board,
        "# Work\nStage: Build (@coder)\n\n## Evidence\nKeep this.\n",
    )
    .unwrap();
    let flipped = command()
        .args([
            "teams",
            "flip",
            "Review",
            "--team",
            "forge",
            "-m",
            "Inspect the receiver.",
        ])
        .bounded_output()
        .unwrap();
    assert!(
        flipped.status.success(),
        "{}",
        String::from_utf8_lossy(&flipped.stderr)
    );
    let flipped_board = std::fs::read(&board).unwrap();
    assert!(String::from_utf8_lossy(&flipped_board).contains("Stage: Review (@reviewer)\n"));

    let before = std::fs::read(&received).unwrap();
    let pending = store.list_pending_messages().unwrap();
    assert_eq!(
        pending.len(),
        1,
        "{}",
        String::from_utf8_lossy(&flipped.stdout),
    );
    assert_eq!(pending[0].gate, DeliveryGate::Done);
    assert!(pending[0].text.contains("stage Review is yours"));
    let message_id = pending[0].message_id.clone();
    command()
        .args(["message", "sweep"])
        .assert_success_within_timeout("sweep while the receiver is running");
    assert_eq!(
        store.list_pending_messages().unwrap()[0].message_id,
        message_id
    );
    assert_eq!(std::fs::read(&received).unwrap(), before);
    hook("Stop");
    command()
        .args(["message", "sweep"])
        .assert_success_within_timeout("deliver after the receiver's Done boundary");
    let text = wait_for_text(&received, &pending[0].text, &server, &pane);
    let delivered = &text[before.len()..];
    assert!(delivered.contains("Type: SIGNAL"), "{delivered}");
    assert!(delivered.contains("From: @rimz"), "{delivered}");
    assert!(store.list_messages().unwrap().iter().any(|message| {
        message.message_id == message_id && message.status == MessageStatus::Sent
    }));
    assert!(store.list_pending_messages().unwrap().is_empty());
    assert_eq!(std::fs::read(&board).unwrap(), flipped_board);
    let before = std::fs::read(&received).unwrap();
    hook("SessionStart");
    command()
        .args(["message", "sweep"])
        .assert_success_within_timeout("deliver to the registered idle receiver");
    let messages = store.list_messages().unwrap();
    let rewake = messages
        .iter()
        .find(|message| message.text.contains("stage Review is still yours"))
        .expect("registration creates a still-yours notice");
    assert_eq!(rewake.gate, DeliveryGate::Done);
    assert_eq!(rewake.status, MessageStatus::Sent);
    let text = wait_for_text(&received, &rewake.text, &server, &pane);
    let delivered = &text[before.len()..];
    assert!(delivered.contains("Type: SIGNAL"), "{delivered}");
    assert!(delivered.contains("From: @rimz"), "{delivered}");
    let payload = serde_json::Deserializer::from_str(
        delivered
            .lines()
            .find(|line| line.starts_with('{'))
            .expect("signal payload"),
    )
    .into_iter::<serde_json::Value>()
    .next()
    .unwrap()
    .unwrap();
    assert_eq!(payload["signal"], "team.stage");
    assert_eq!(payload["from"], "Review");
    assert_eq!(payload["to"], "Review");
    assert_eq!(payload["by"], "rimz");
    assert!(store.list_pending_messages().unwrap().is_empty());
    assert_eq!(std::fs::read(&board).unwrap(), flipped_board);
}

fn wait_for_text(path: &Path, needle: &str, server: &TmuxServer, pane: &PaneId) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
            && text.contains(needle)
        {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "receiver did not record {needle:?}: {:?}",
            server.backend.capture_pane(pane, Some(30), false)
        );
        thread::sleep(Duration::from_millis(25));
    }
}
