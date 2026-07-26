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
        description: None,
        transcript_path: None,
        origin: None,
        compacted_from: None,
        usage: rimz::agents::AgentUsageSummary::default(),
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

fn claude_remote_control_badge(
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
fn frame_emits_plain_text_over_a_pipe() {
    let env = Env::new();
    let session = "test-session";
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    let published = rimz::sidebar::frame::assemble_frame(
        Vec::new(),
        rimz::sidebar::timing::unix_now_ms(),
        session,
    );
    rimz::store::atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &published)
        .expect("publish pane frame");

    let output = env
        .rimz()
        .args([
            "sidebar",
            "frame",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--session-name",
            session,
            "--width",
            "40",
            "--height",
            "12",
        ])
        .output()
        .expect("spawn sidebar frame");

    assert!(
        output.status.success(),
        "frame failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.is_empty(), "frame stdout is empty");
    assert!(
        !output.stdout.contains(&b'\x1b'),
        "piped frame contains ANSI escapes: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn sidebar_lights_claude_rc_badge_from_claude_startup_setting() {
    let env = Env::new();
    let settings = write_claude_settings(&env, r#"{ "remoteControlAtStartup": true }"#);

    assert_eq!(
        claude_remote_control_badge(&env, &settings, None),
        "healthy"
    );
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
        claude_remote_control_badge(&env, &settings, Some(path_with_stub_first(&stub_dir))),
        "hidden"
    );
}

#[test]
fn codex_identity_enrichment_preserves_hook_owned_question() {
    let env = Env::new();
    let session_id = "11111111-1111-4111-8111-111111111111";
    let now = jiff::Timestamp::now();
    let date = now.to_zoned(jiff::tz::TimeZone::UTC).date();
    let sessions = env.home_root.join(format!(
        ".codex/sessions/{:04}/{:02}/{:02}",
        date.year(),
        date.month(),
        date.day()
    ));
    std::fs::create_dir_all(&sessions).expect("create Codex sessions dir");
    let rollout = sessions.join(format!(
        "rollout-{:04}-{:02}-{:02}T00-00-00-{session_id}.jsonl",
        date.year(),
        date.month(),
        date.day()
    ));
    std::fs::write(
        &rollout,
        format!(
            "{}\n",
            serde_json::json!({
                "timestamp": now.to_string(),
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "cwd": env.project_root,
                }
            })
        ),
    )
    .expect("write Codex rollout");

    let hook = env.run_installed_hook_in_pane(
        "codex",
        &serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": session_id,
            "tool_name": "request_user_input",
            "tool_input": {
                "questions": [{
                    "id": "shape",
                    "question": "Which fix shape?",
                    "options": [{ "label": "minimal" }, { "label": "broad" }]
                }]
            }
        })
        .to_string(),
        &[("TMUX_PANE", "%7")],
    );
    assert!(
        hook.status.success(),
        "{}",
        String::from_utf8_lossy(&hook.stderr)
    );

    let mut pane = crate::common::tmux_pane("%7", "codex", &env.project_root);
    pane.resumed_session_id = Some(rimz::ids::AgentSessionId::from(session_id));
    let snapshot = env.snapshot_json_with_panes(&[pane]);
    let agent = snapshot["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find(|agent| agent["agent_id"] == session_id)
        .expect("Codex agent");
    assert_eq!(agent["status"], "waiting");
    assert!(agent["waiting_since"].is_string());
    assert!(agent["open_ask"]["id"].as_str().is_some());

    let row = snapshot["worktree_groups"]
        .as_array()
        .expect("worktree groups")
        .iter()
        .flat_map(|group| group["rows"].as_array().expect("rows"))
        .find(|row| row["id"] == session_id)
        .expect("Codex worktree row");
    assert_eq!(row["status"], "waiting");
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
            "lastModifiedAt": "2025-01-01T00:00:00Z"
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
    std::fs::write(&transcript, b"").expect("write empty Kiro transcript");
    let pane = rimz::pane::PaneRef {
        pane_id: rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Tmux, "%kiro"),
        session_name: "rimz-test".to_owned(),
        view_id: Some("@0".to_owned()),
        view_kind: Some(rimz::ids::ViewKind::Window),
        view_name: None,
        title: None,
        is_floating: false,
        command: Some("kiro-cli-chat".to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(env.project_root.to_string_lossy().into_owned()),
        pane_pid: None,
        pane_process_start: Some("2024-12-31T23:59:58Z".parse().unwrap()),
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
    let run_snapshot = || {
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
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };

    let snapshot = run_snapshot();
    let agent = snapshot["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["agent_id"] == session_id)
        .expect("newborn Kiro agent");
    assert_eq!(agent["status"], "idle");
    assert_eq!(agent["phase"], "idle");
    assert!(agent.get("prompt").is_none());
    assert!(agent["context_pct"].is_null());
    assert!(agent["context_window"].is_null());
    assert!(agent["total_tokens"].is_null());
    assert!(agent["context"].get("model_id").is_none());
    assert!(agent["context"].get("tokens").is_none());
    assert!(agent["context"].get("cost").is_none());
    assert!(agent["model"].is_null());
    assert!(agent["effort"].is_null());
    let row = snapshot["worktree_groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["rows"].as_array().unwrap())
        .find(|row| row["id"] == session_id)
        .expect("newborn Kiro row");
    assert_eq!(row["row_kind"], "agent");
    assert_eq!(row["status"], "idle");
    assert!(row.get("prompt").is_none());
    assert!(row["context_pct"].is_null());
    assert!(row.get("total_tokens").is_none());
    assert!(row["context"].get("model_id").is_none());
    assert!(row["context"].get("tokens").is_none());
    assert!(row["context"].get("cost").is_none());
    assert_eq!(std::fs::read(&event_log).unwrap_or_default(), before);

    for (end, expected, phase) in [
        (2, "running", "reasoning"),
        (3, "waiting", "idle"),
        (5, "running", "acting"),
        (10, "success", "idle"),
    ] {
        std::fs::write(&transcript, format!("{}\n", records[..end].join("\n")))
            .expect("grow Kiro transcript");
        let snapshot = run_snapshot();
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
