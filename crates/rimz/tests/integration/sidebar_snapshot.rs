//! `rimz sidebar snapshot` integration tests for provider dashboard facts.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use rimz::agents::lifecycle::LifecycleSignal;
use rimz::agents::{AgentLifecycleObservation, LaunchParams};
use rimz::store::event::EventEnvelope;
use sha2::{Digest, Sha256};

use crate::common::Env;

fn inject_lifecycle(env: &Env, agent_kind: &str, agent_id: &str) {
    let obs = AgentLifecycleObservation {
        agent_id: Some(agent_id.into()),
        agent_name: None,
        launch: LaunchParams::default(),
        signal: LifecycleSignal::Registered,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        worktree_path: Some(env.project_root.display().to_string()),
        worktree_branch: Some("main".to_owned()),
        task: None,
        prompt: None,
        transcript_path: None,
        origin: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        turn_error: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        pane_id: None,
        pane_stamp: None,
        parent_agent_id: None,
    };
    let envelope = EventEnvelope::agent_lifecycle(
        env.workspace_id.clone(),
        "test-session",
        agent_kind,
        "SessionStart",
        &obs,
    );
    env.store().append_event(&envelope).expect("append");
}

fn write_claude_settings(env: &Env, text: &str) -> PathBuf {
    let path = env.home_root.join("claude-settings.json");
    std::fs::write(&path, text).expect("write claude settings");
    path
}

fn stub_claude_version(env: &Env, version: &str) -> PathBuf {
    let dir = env.home_root.join("bin");
    std::fs::create_dir_all(&dir).expect("mkdir bin");
    let path = dir.join("claude");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo '{version}'; exit 0; fi\nexit 1\n"
        ),
    )
    .expect("write claude stub");
    let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod claude stub");
    dir
}

fn path_with_stub_first(stub_dir: &Path) -> String {
    let current = std::env::var_os("PATH").unwrap_or_default();
    format!("{}:{}", stub_dir.display(), current.to_string_lossy())
}

fn claude_remote_control_value(
    env: &Env,
    settings: &Path,
    path: Option<String>,
) -> serde_json::Value {
    inject_lifecycle(env, "claude", "claude-session-rc");

    let mut command = env.rimz();
    command
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--json",
        ])
        .env("RIMZ_CLAUDE_SETTINGS", settings)
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN");
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let output = command.output().expect("spawn sidebar snapshot");
    assert!(
        output.status.success(),
        "snapshot failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("snapshot json");
    json["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .find(|provider| provider["kind"] == "claude")
        .expect("claude provider panel")["remote_control"]
        .clone()
}

#[test]
fn snapshot_exits_cleanly_when_the_consumer_closes_the_pipe() {
    use std::io::Read;
    use std::process::Stdio;

    let env = Env::new();
    inject_lifecycle(&env, "claude", "claude-session-epipe");

    let (reader, writer) = std::io::pipe().expect("pipe");
    drop(reader);

    let mut child = env
        .rimz()
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--json",
        ])
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sidebar snapshot");

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr piped")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    let status = child.wait().expect("wait");

    assert!(status.success(), "expected clean exit; stderr: {stderr}");
    assert!(
        !stderr.contains("panicked"),
        "broken pipe must not panic; stderr: {stderr}"
    );
}

#[test]
fn sidebar_lights_claude_rc_badge_from_claude_startup_setting() {
    let env = Env::new();
    let settings = write_claude_settings(&env, r#"{ "remoteControlAtStartup": true }"#);

    assert_eq!(claude_remote_control_value(&env, &settings, None), true);
}

#[test]
fn sidebar_suppresses_claude_rc_badge_when_auth_blocks_remote_control() {
    let env = Env::new();
    let settings = write_claude_settings(
        &env,
        r#"{ "remoteControlAtStartup": true, "apiKeyHelper": "op read key" }"#,
    );
    let stub_dir = stub_claude_version(&env, "2.1.173 (Claude Code)");

    assert_eq!(
        claude_remote_control_value(&env, &settings, Some(path_with_stub_first(&stub_dir))),
        false
    );
}

#[test]
fn kiro_local_store_bootstraps_live_state_and_history_without_events() {
    let env = Env::new();
    let session_id = "sess_11111111-1111-4111-8111-111111111111";
    let bucket = &hex::encode(Sha256::digest(
        env.project_root.as_os_str().as_encoded_bytes(),
    ))[..16];
    let session_dir = env
        .home_root
        .join(".kiro/sessions")
        .join(bucket)
        .join(session_id);
    std::fs::create_dir_all(&session_dir).expect("create Kiro session dir");
    std::fs::write(
        session_dir.join("session.json"),
        serde_json::json!({
            "id": session_id,
            "schemaVersion": "1.0.0",
            "dataModelVersion": 1,
            "workspacePaths": [env.project_root],
            "createdAt": "2025-01-01T00:00:00Z",
            "lastModifiedAt": "2025-01-01T00:00:08Z",
            "status": "idle"
        })
        .to_string(),
    )
    .expect("write Kiro metadata");
    let records = [
        r#"{"id":"user","timestamp":"2025-01-01T00:00:01Z","payload":{"type":"user","content":"ping"}}"#,
        r#"{"id":"start","timestamp":"2025-01-01T00:00:02Z","payload":{"type":"turn_start","executionId":"turn"}}"#,
        r#"{"id":"pending","timestamp":"2025-01-01T00:00:03Z","payload":{"type":"pending_interaction","interactionType":"tool_approval","toolCallId":"tool","question":"Write File"}}"#,
        r#"{"id":"resolved","timestamp":"2025-01-01T00:00:04Z","payload":{"type":"interaction_resolved","toolCallId":"tool"}}"#,
        r#"{"id":"call","timestamp":"2025-01-01T00:00:05Z","payload":{"type":"tool_call","toolCallId":"tool","toolName":"fs_write","status":"approved"}}"#,
        r#"{"id":"result","timestamp":"2025-01-01T00:00:06Z","payload":{"type":"tool_result","toolCallId":"tool","success":true}}"#,
        r#"{"id":"assistant","timestamp":"2025-01-01T00:00:07Z","payload":{"type":"assistant","operationType":"Say","content":"pong"}}"#,
        r#"{"id":"context","timestamp":"2025-01-01T00:00:07.100Z","payload":{"type":"session_metadata","key":"contextUsage","value":{"usagePercentage":42.4}}}"#,
        r#"{"id":"pause","timestamp":"2025-01-01T00:00:08Z","payload":{"type":"session_event","category":"session_pause","context":{"status":"success"}}}"#,
        r#"{"id":"end","timestamp":"2025-01-01T00:00:08.100Z","payload":{"type":"turn_end","executionId":"turn","stopReason":"end_turn"}}"#,
    ];
    let transcript = session_dir.join("messages.jsonl");
    let pane = rimz::pane::PaneRef {
        pane_id: rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Tmux, "%kiro"),
        session_name: "rimz-test".to_owned(),
        view_id: Some("@0".to_owned()),
        view_kind: Some(rimz::ids::ViewKind::Window),
        view_name: None,
        is_focused: false,
        is_floating: false,
        command: Some("kiro-cli chat --v3".to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(env.project_root.to_string_lossy().into_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    };
    let event_log = env.store().paths().events_log.clone();
    let before = std::fs::read(&event_log).unwrap_or_default();
    let pane_fixture = env.write_pane_fixture(std::slice::from_ref(&pane));
    let session_name = rimz::workspace::WorkspaceResolver::resolve(&env.project_root, None)
        .expect("resolve workspace")
        .session_name;

    for (end, expected, phase) in [
        (2, "running", "reasoning"),
        (3, "waiting", "idle"),
        (5, "running", "acting"),
        (10, "success", "idle"),
    ] {
        std::fs::write(&transcript, format!("{}\n", records[..end].join("\n")))
            .expect("grow Kiro transcript");
        let output = env
            .rimz()
            .args([
                "sidebar",
                "snapshot",
                "--workspace-id",
                env.workspace_id.as_str(),
                "--mux",
                "tmux",
                "--session-name",
                &session_name,
                "--json",
            ])
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
            .output()
            .expect("run Kiro snapshot");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let snapshot: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let agent = snapshot["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|agent| agent["agent_id"] == session_id)
            .expect("Kiro agent row");
        assert_eq!(agent["status"], expected);
        assert_eq!(agent["phase"], phase);
        if end == records.len() {
            assert_eq!(agent["context_pct"], 42);
        }
    }

    let frame = rimz::sidebar::frame::assemble_frame(
        vec![pane.clone()],
        rimz::sidebar::timing::unix_now_ms(),
        &session_name,
    );
    rimz::store::atomic::write_temp_then_rename_cache(
        &env.runtime_paths().pane_frame_path(),
        &frame,
    )
    .expect("publish Kiro pane frame");

    let history = env
        .rimz()
        .args(["agents", "history", session_id, "--json"])
        .output()
        .expect("run Kiro history");
    assert!(
        history.status.success(),
        "{}",
        String::from_utf8_lossy(&history.stderr)
    );
    let history: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(history[0]["prompt"], "ping");
    assert_eq!(history[0]["outcome"], "done");
    assert_eq!(history[0]["fresh_input"], 0);
    assert_eq!(history[0]["output"], 0);
    assert!(history[0]["cost_usd"].is_null());
    assert_eq!(std::fs::read(&event_log).unwrap_or_default(), before);
}
