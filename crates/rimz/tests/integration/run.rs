use crate::common::Env;
#[cfg(unix)]
use crate::common::{
    CommandTimeoutExt, path_with_front, write_failing_agent_shim, write_fake_login_shell,
};
use jiff::Timestamp;
use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use rimz::harness::run::{PermissionMode, RunRecord, RunStatus};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId, ViewKind};
use rimz::store::event::EventEnvelope;
use serde_json::json;
use std::io::{Read as _, Write as _};
use std::process::Command;
use std::process::Stdio;
use std::time::{Duration, Instant};

#[test]
fn kiro_supervised_run_fails_before_recording_or_launching() {
    let env = Env::new();
    let out = env
        .rimz()
        .args(["agents", "kiro", "summarize", "-p"])
        .output()
        .expect("spawn Kiro print run");

    assert!(
        !out.status.success(),
        "unsupported Kiro print run should fail\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("verified executable turn-lifecycle signal"),
        "error should name the missing completion contract\nstderr:\n{stderr}"
    );
    assert!(
        rimz::harness::run::list(env.store().paths())
            .expect("list runs")
            .is_empty(),
        "unsupported run must fail before creating state"
    );
}

#[cfg(unix)]
#[test]
fn fresh_background_supervised_run_uses_shared_room_birth() {
    let env = Env::new();
    env.install_agent_hooks("codex");
    trust_codex_hooks(&env);
    let agent_bin = write_failing_agent_shim(&env, "codex", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    let heartbeat = rimz::sidebar::heartbeat::SidebarHeartbeat::new(
        env.workspace_id.clone(),
        rimz::ids::SidebarInstanceId::new(),
        MuxName::Zellij,
        &workspace.session_name,
        runtime.sock_dir.join("supervised-stale.sock"),
        None,
    );
    let heartbeat_path = runtime.heartbeat_dir.join("sidebar.supervised-stale.json");
    std::fs::write(
        &heartbeat_path,
        serde_json::to_vec(&heartbeat).expect("serialize heartbeat"),
    )
    .expect("write heartbeat");
    let trace_path = env.project_root.join("fresh-supervised.log");
    let presence = env.project_root.join("presence.wasm");
    std::fs::write(&presence, b"test-presence").expect("write presence fixture");

    let output = env
        .rimz()
        .args([
            "--mux", "zellij", "agents", "codex", "fix it", "-p", "--bg",
        ])
        .env("SHELL", shell)
        .env("PATH", path_with_front(&agent_bin))
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", &trace_path)
        .env("RIMZ_PRESENCE_PLUGIN", presence)
        .env("ZELLIJ_PANE_ID", "1")
        .env("RIMZ_TEST_ZELLIJ_LIST_SESSIONS", "")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},{"id":2,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
        )
        .bounded_output()
        .expect("run background supervised birth");
    assert!(
        output.status.success(),
        "supervised birth failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !heartbeat_path.exists(),
        "fresh supervised birth must purge stale heartbeat"
    );
    let events = env.read_events();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.kind(), rimz::store::event::EventKind::SessionRebirth))
            .count(),
        1,
        "supervised birth records one boundary"
    );
    assert!(
        events
            .iter()
            .all(|event| !matches!(event.kind(), rimz::store::event::EventKind::SessionDeath(_))),
        "supervised birth skips normal rebirth inspection"
    );
    let trace = std::fs::read_to_string(&trace_path).expect("read supervised trace");
    let create = trace
        .lines()
        .position(|line| line.contains("\tattach\t--create-background\t"))
        .expect("session/sidebar create");
    let presence = trace
        .lines()
        .position(|line| line.contains("\tpipe\t--plugin\t"))
        .expect("presence pipe");
    assert!(
        create < presence,
        "session/sidebar creation must precede presence:\n{trace}"
    );
}

#[test]
fn hooks_bind_and_complete_supervised_run() {
    let env = Env::new();
    let store = env.store();
    let sessions = env.home_root.join("codex-sessions");
    let day = sessions.join("2026").join("06").join("10");
    std::fs::create_dir_all(&day).expect("mkdir codex sessions");
    let transcript = day.join("rollout-2026-06-10T00-00-00-sess-run.jsonl");
    std::fs::write(
        &transcript,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":null}}\n",
    )
    .expect("seed rollout");
    let record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "summarize".to_owned(),
        env.project_root.clone(),
    );
    let run_id = record.run_id.clone();
    rimz::harness::run::create(store.paths(), &record).expect("create run");

    let prompt_payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "sess-run",
        "prompt": "summarize"
    })
    .to_string();
    let mut prompt_cmd = env.hook_command("codex");
    prompt_cmd.env(rimz::harness::run::ENV_RUN_ID, run_id.as_str());
    prompt_cmd.env("RIMZ_CODEX_SESSIONS", &sessions);
    let out = env
        .spawn_payload(prompt_cmd, &prompt_payload)
        .wait_with_output()
        .expect("wait prompt hook");
    assert!(
        out.status.success(),
        "prompt hook failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let running = rimz::harness::run::load(store.paths(), &run_id).expect("load running run");
    assert_eq!(running.status, RunStatus::Running);
    assert_eq!(running.agent_id.as_deref(), Some("sess-run"));
    assert_eq!(
        running.transcript_path.as_deref(),
        Some(transcript.to_str().unwrap())
    );

    let stop_payload = json!({
        "hook_event_name": "Stop",
        "session_id": "sess-run",
        "last_assistant_message": "done"
    })
    .to_string();
    let mut stop_cmd = env.hook_command("codex");
    stop_cmd.env(rimz::harness::run::ENV_RUN_ID, run_id.as_str());
    stop_cmd.env("RIMZ_CODEX_SESSIONS", &sessions);
    let out = env
        .spawn_payload(stop_cmd, &stop_payload)
        .wait_with_output()
        .expect("wait stop hook");
    assert!(
        out.status.success(),
        "stop hook failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let completed = rimz::harness::run::load(store.paths(), &run_id).expect("load completed run");
    assert_eq!(completed.status, RunStatus::Completed);
    assert_eq!(completed.last_message.as_deref(), Some("done"));
    assert_eq!(
        completed.transcript_path.as_deref(),
        Some(transcript.to_str().unwrap())
    );
}

#[test]
fn copilot_hooks_bind_transcript_and_capture_supervised_final_text() {
    let env = Env::new();
    let store = env.store();
    let session_id = "copilot-run";
    let session_dir = env
        .home_root
        .join(".copilot/session-state")
        .join(session_id);
    std::fs::create_dir_all(&session_dir).expect("mkdir Copilot session");
    let transcript = session_dir.join("events.jsonl");
    std::fs::write(
        &transcript,
        "{\"type\":\"user.message\",\"timestamp\":\"2026-07-13T15:13:21Z\",\"data\":{\"content\":\"summarize\"}}\n",
    )
    .expect("seed Copilot events");
    let record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("copilot"),
        PermissionMode::Auto,
        "summarize".to_owned(),
        env.project_root.clone(),
    );
    let run_id = record.run_id.clone();
    rimz::harness::run::create(store.paths(), &record).expect("create run");

    let mut prompt_cmd = env.copilot_hook_command("userPromptSubmitted");
    prompt_cmd.env(rimz::harness::run::ENV_RUN_ID, run_id.as_str());
    let out = env
        .spawn_payload(
            prompt_cmd,
            &json!({"sessionId":session_id,"prompt":"summarize"}).to_string(),
        )
        .wait_with_output()
        .expect("wait Copilot prompt hook");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    let running = rimz::harness::run::load(store.paths(), &run_id).expect("load running run");
    assert_eq!(running.status, RunStatus::Running);
    assert_eq!(running.agent_id.as_deref(), Some(session_id));
    assert_eq!(running.transcript_path.as_deref(), transcript.to_str());

    std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap()
        .write_all(
            b"{\"type\":\"assistant.message\",\"timestamp\":\"2026-07-13T15:13:23Z\",\"data\":{\"content\":\"copilot done\"}}\n",
        )
        .unwrap();
    let mut stop_cmd = env.copilot_hook_command("agentStop");
    stop_cmd.env(rimz::harness::run::ENV_RUN_ID, run_id.as_str());
    let out = env
        .spawn_payload(
            stop_cmd,
            &json!({"sessionId":session_id,"transcriptPath":transcript,"stopReason":"end_turn"})
                .to_string(),
        )
        .wait_with_output()
        .expect("wait Copilot stop hook");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());

    let completed = rimz::harness::run::load(store.paths(), &run_id).expect("load completed run");
    assert_eq!(completed.status, RunStatus::Completed);
    assert_eq!(completed.last_message.as_deref(), Some("copilot done"));
    assert_eq!(completed.transcript_path.as_deref(), transcript.to_str());
}

#[test]
fn cursor_response_hook_seeds_run_before_terminal_outcome() {
    let env = Env::new();
    let store = env.store();
    let create_run = || {
        let record = RunRecord::new(
            env.workspace_id.clone(),
            AgentKind::new_unchecked("cursor"),
            PermissionMode::Auto,
            "go".to_owned(),
            env.project_root.clone(),
        );
        rimz::harness::run::create(store.paths(), &record).unwrap();
        record
    };
    let hook = |run: &RunRecord, payload: serde_json::Value| {
        let mut command = env.hook_command("cursor");
        command.env(rimz::harness::run::ENV_RUN_ID, run.run_id.as_str());
        let output = env
            .spawn_payload(command, &payload.to_string())
            .wait_with_output()
            .unwrap();
        assert!(
            output.status.success(),
            "hook stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    let completed = create_run();
    hook(
        &completed,
        json!({
            "hook_event_name": "afterAgentResponse",
            "conversation_id": "conv-completed",
            "text": "safe final"
        }),
    );
    let active = rimz::harness::run::load(store.paths(), &completed.run_id).unwrap();
    assert_eq!(active.status, RunStatus::Pending);
    assert_eq!(active.last_message.as_deref(), Some("safe final"));
    assert_eq!(active.completed_at, None);
    hook(
        &completed,
        json!({
            "hook_event_name": "stop",
            "conversation_id": "conv-completed",
            "status": "completed"
        }),
    );
    let completed = rimz::harness::run::load(store.paths(), &completed.run_id).unwrap();
    assert_eq!(completed.status, RunStatus::Completed);
    assert_eq!(completed.last_message.as_deref(), Some("safe final"));

    for (status, expected) in [
        ("aborted", RunStatus::Canceled),
        ("completed", RunStatus::Completed),
    ] {
        let run = create_run();
        hook(
            &run,
            json!({
                "hook_event_name": "stop",
                "conversation_id": format!("conv-{status}"),
                "status": status,
                "message": {"content": [{"type": "text", "text": "unsafe"}]}
            }),
        );
        let terminal = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
        assert_eq!(terminal.status, expected);
        assert_eq!(terminal.last_message, None);
    }
}

#[test]
fn run_rejects_invalid_agent_env_before_recording() {
    let env = Env::new();
    env.write_config(
        &env.project_root,
        "[[agents]]\nname = \"codex\"\nenv = { \"BAD=KEY\" = \"yes\" }\n",
    );
    let trust = env
        .rimz()
        .args(["trust", "grant"])
        .output()
        .expect("spawn trust grant");
    assert!(
        trust.status.success(),
        "trust grant failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&trust.stdout),
        String::from_utf8_lossy(&trust.stderr)
    );

    let out = env
        .rimz()
        .args(["agents", "codex", "summarize", "-p"])
        .output()
        .expect("spawn agents print");
    assert!(
        !out.status.success(),
        "agents -p should reject invalid launch env\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("BAD=KEY"),
        "agents -p error should name the invalid key\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let records = rimz::harness::run::list(env.store().paths()).expect("list runs");
    assert!(
        records.is_empty(),
        "invalid launch env should fail before recording a run: {records:?}"
    );
}

#[test]
fn print_text_input_accepts_piped_prompt_without_positional() {
    let env = Env::new();
    env.write_config(
        &env.project_root,
        "[[agents]]\nname = \"codex\"\nenv = { \"BAD=KEY\" = \"yes\" }\n",
    );
    let trust = env
        .rimz()
        .args(["trust", "grant"])
        .output()
        .expect("spawn trust grant");
    assert!(
        trust.status.success(),
        "trust grant failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&trust.stdout),
        String::from_utf8_lossy(&trust.stderr)
    );

    let mut cmd = env.rimz();
    cmd.args(["agents", "codex", "-p", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn agents print");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"summarize\n")
        .expect("write prompt stdin");
    let out = child.wait_with_output().expect("wait agents print");
    assert!(
        !out.status.success(),
        "agents -p should fail after accepting piped prompt\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("BAD=KEY"),
        "piped prompt should advance to launch validation\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("expected a prompt"),
        "explicit stdin should satisfy text input\nstderr:\n{stderr}"
    );
}

#[test]
fn print_json_flag_points_at_output_format() {
    let env = Env::new();
    let out = env
        .rimz()
        .args(["agents", "codex", "summarize", "-p", "--json"])
        .output()
        .expect("spawn agents print json");
    assert!(
        !out.status.success(),
        "`-p --json` should be rejected in favor of --output-format"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--output-format json"),
        "stderr should redirect to --output-format\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn multi_cell_print_name_reports_print_cardinality() {
    let env = Env::new();
    let out = env
        .rimz()
        .args(["agents", "claude,codex", "summarize", "-p", "--name", "one"])
        .output()
        .expect("spawn named multi-cell print run");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "multi-cell print run should fail");
    assert!(
        stderr.contains("--print requires a layout with exactly one agent cell"),
        "unexpected stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("--name requires"),
        "interactive name validation must not replace print validation:\n{stderr}"
    );
}

#[test]
fn print_stream_json_input_refuses_a_positional_prompt() {
    let env = Env::new();
    let out = env
        .rimz()
        .args([
            "agents",
            "codex",
            "summarize",
            "-p",
            "--input-format",
            "stream-json",
        ])
        .output()
        .expect("spawn agents print stream-json input");
    assert!(
        !out.status.success(),
        "stream-json input plus a positional prompt should be rejected"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("drop the positional PROMPT"),
        "stderr should name the conflict\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn print_stream_json_input_refuses_stdin_flag() {
    let env = Env::new();
    let out = env
        .rimz()
        .args([
            "agents",
            "codex",
            "-p",
            "--stdin",
            "--input-format",
            "stream-json",
        ])
        .output()
        .expect("spawn agents print stream-json input with stdin flag");
    assert!(
        !out.status.success(),
        "redundant --stdin should be rejected"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("--input-format stream-json already reads stdin; drop --stdin"),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(unix)]
#[test]
fn failed_supervised_run_retries_with_failure_context() {
    let env = Env::new();
    if !init_git_repo(&env.project_root) {
        tracing::warn!("skipping: git unavailable");
        return;
    }
    let store = env.store();
    let worktree = env
        .home_root
        .join("project-worktrees")
        .join("retry-worktree");
    let mut child = spawn_retrying_print(&env, "retry-success", true);

    let mut records = wait_for_run_count(&store, &mut child, 1);
    assert!(worktree.is_dir(), "attempt 1 created the shared worktree");
    let mut failed = records.pop().expect("first run");
    assert_eq!(failed.agent_name.as_deref(), Some("fixed-name"));
    failed.status = RunStatus::Failed;
    failed.failure_tail = Some("compiler exploded\nlast diagnostic".to_owned());
    failed.transcript_path = Some("/tmp/attempt-one.jsonl".to_owned());
    finish_run(&store, &mut failed);

    let records = wait_for_run_count(&store, &mut child, 2);
    assert!(
        worktree.is_dir(),
        "the failed first attempt must not remove the shared worktree"
    );
    let retry = records
        .iter()
        .find(|record| record.retry_of.as_ref() == Some(&failed.run_id))
        .expect("retry run");
    assert!(
        retry
            .agent_name
            .as_deref()
            .is_some_and(|name| !name.is_empty())
    );
    assert!(
        retry
            .prompt
            .starts_with("fix it\n\n<previous-attempt-failure>")
    );
    assert!(retry.prompt.contains("compiler exploded\nlast diagnostic"));
    assert_eq!(
        retry.prompt.matches("<previous-attempt-failure>").count(),
        1
    );

    let mut completed = retry.clone();
    completed.status = RunStatus::Completed;
    completed.last_message = Some("fixed".to_owned());
    tombstone_retry_agents(&env, &store);
    finish_run(&store, &mut completed);

    let out = child.wait_with_output().expect("wait retrying print");
    assert!(
        out.status.success(),
        "retrying print failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "fixed\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("rimz: run failed (exit 1)"), "{stderr}");
    assert!(
        stderr.contains("compiler exploded\nlast diagnostic"),
        "{stderr}"
    );
    assert!(
        stderr.contains("transcript: /tmp/attempt-one.jsonl"),
        "{stderr}"
    );
    assert!(
        stderr.contains("rimz: retrying (attempt 2 of 2)"),
        "{stderr}"
    );
    assert!(
        !worktree.exists(),
        "the coordinator removes the clean worktree after the terminal attempt"
    );
}

#[cfg(unix)]
#[test]
fn timed_out_supervised_run_does_not_retry() {
    let env = Env::new();
    let store = env.store();
    let mut child = spawn_retrying_print(&env, "retry-timeout", false);

    let mut records = wait_for_run_count(&store, &mut child, 1);
    let mut timed_out = records.pop().expect("first run");
    timed_out.status = RunStatus::TimedOut;
    finish_run(&store, &mut timed_out);

    let out = child.wait_with_output().expect("wait timed-out print");
    assert_eq!(
        out.status.code(),
        Some(124),
        "timed-out print returned wrong status\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        rimz::harness::run::list(store.paths())
            .expect("list terminal runs")
            .len(),
        1
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains("retrying"));
}

#[cfg(unix)]
#[test]
fn completed_run_reprompts_same_session_until_verify_passes() {
    let env = Env::new();
    let store = env.store();
    let pane_fixture = env.write_pane_fixture(&[verify_agent_pane(&env)]);
    let mut child = spawn_verifying_print(&env, "verify-pass", 3, &pane_fixture, None);

    let mut records = wait_for_run_count(&store, &mut child, 1);
    let mut completed = records.pop().expect("initial run");
    register_verify_agent(&env, &store, &mut completed);
    completed.status = RunStatus::Completed;
    completed.last_message = Some("first answer".to_owned());
    finish_run(&store, &mut completed);

    let reopened = wait_for_run_status(&store, &mut child, &completed.run_id, RunStatus::Running);
    let verify = reopened.verify.as_ref().expect("red verify detail");
    assert_eq!(verify.attempts, 1);
    assert!(!verify.passed);
    assert_eq!(verify.code, Some(1));
    assert!(verify.output.contains("verify red"));
    let messages = wait_for_message_count(&store, &mut child, 1);
    assert_eq!(messages.len(), 1);
    assert!(messages[0].text.contains("Verification failed"));
    assert!(messages[0].text.contains("--- verify `"));
    assert!(messages[0].text.contains("verify red"));

    std::fs::write(env.project_root.join(".verify-green"), "green\n").expect("green marker");
    let mut second = reopened;
    second.status = RunStatus::Completed;
    second.last_message = Some("fixed answer".to_owned());
    finish_run(&store, &mut second);

    let out = child.wait_with_output().expect("wait verified print");
    assert!(
        out.status.success(),
        "verified print failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "fixed answer\n");
    let stored = rimz::harness::run::load(store.paths(), &second.run_id).expect("verified run");
    assert_eq!(stored.status, RunStatus::Completed);
    let verify = stored.verify.expect("green verify detail");
    assert!(verify.passed);
    assert_eq!(verify.attempts, 2);
    assert_eq!(verify.code, Some(0));
    assert!(verify.output.contains("verify red"));
}

#[cfg(unix)]
#[test]
fn red_verify_at_attempt_cap_exits_123_with_forensics() {
    let env = Env::new();
    let store = env.store();
    let pane_fixture = env.write_pane_fixture(&[verify_agent_pane(&env)]);
    let mut child = spawn_verifying_print(&env, "verify-cap", 1, &pane_fixture, None);

    let mut records = wait_for_run_count(&store, &mut child, 1);
    let mut completed = records.pop().expect("initial run");
    completed.status = RunStatus::Completed;
    completed.last_message = Some("claimed done".to_owned());
    finish_run(&store, &mut completed);

    let out = child.wait_with_output().expect("wait capped verify");
    assert_eq!(
        out.status.code(),
        Some(123),
        "capped verify returned wrong status\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "claimed done\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rimz: run verify_failed (exit 123)"),
        "{stderr}"
    );
    assert!(stderr.contains("verify `"), "{stderr}");
    assert!(stderr.contains("exited 1 (attempt 1)"), "{stderr}");
    assert!(stderr.contains("verify red"), "{stderr}");
    let stored = rimz::harness::run::load(store.paths(), &completed.run_id).expect("failed verify");
    assert_eq!(stored.status, RunStatus::VerifyFailed);
}

#[cfg(unix)]
#[test]
fn verify_reprompt_wait_timeout_stays_terminal() {
    let env = Env::new();
    let store = env.store();
    let pane_fixture = env.write_pane_fixture(&[verify_agent_pane(&env)]);
    let mut child = spawn_verifying_print(&env, "verify-timeout", 2, &pane_fixture, Some("1s"));

    let mut records = wait_for_run_count(&store, &mut child, 1);
    let mut completed = records.pop().expect("initial run");
    register_verify_agent(&env, &store, &mut completed);
    completed.status = RunStatus::Completed;
    finish_run(&store, &mut completed);

    let out = child
        .wait_with_output()
        .expect("wait timed verify re-prompt");
    assert_eq!(out.status.code(), Some(124));
    let stored = rimz::harness::run::load(store.paths(), &completed.run_id).expect("timed run");
    assert_eq!(stored.status, RunStatus::TimedOut);
}

#[cfg(unix)]
#[test]
fn verify_reprompt_interrupt_stays_canceled() {
    let env = Env::new();
    let store = env.store();
    let pane_fixture = env.write_pane_fixture(&[verify_agent_pane(&env)]);
    let mut child = spawn_verifying_print(&env, "verify-interrupt", 2, &pane_fixture, None);

    let mut records = wait_for_run_count(&store, &mut child, 1);
    let mut completed = records.pop().expect("initial run");
    register_verify_agent(&env, &store, &mut completed);
    completed.status = RunStatus::Completed;
    finish_run(&store, &mut completed);
    wait_for_run_status(&store, &mut child, &completed.run_id, RunStatus::Running);
    wait_for_message_count(&store, &mut child, 1);

    let signal = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("signal supervised run");
    assert!(signal.success());
    let out = child.wait_with_output().expect("wait interrupted verify");
    assert_eq!(out.status.code(), Some(130));
    let stored = rimz::harness::run::load(store.paths(), &completed.run_id).expect("canceled run");
    assert_eq!(stored.status, RunStatus::Canceled);
}

#[cfg(unix)]
fn spawn_retrying_print(env: &Env, trace_name: &str, use_worktree: bool) -> std::process::Child {
    env.install_agent_hooks("codex");
    trust_codex_hooks(env);
    let agent_bin = write_failing_agent_shim(env, "codex", 1);
    let shell = write_fake_login_shell(env, "rimz-test-sh", &[]);
    let trace_log = env.project_root.join(format!("{trace_name}.log"));
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    let runtime = env.runtime_paths();
    rimz::sidebar::cache::write_pane_topology_cache(
        &runtime,
        &rimz::mux::zellij::pane_topology::PaneTopologyCache {
            session_name: workspace.session_name.clone(),
            produced_at_ms: rimz::sidebar::timing::unix_now_ms(),
            writer: None,
            focused_pane: None,
            clients: None,
            panes: Vec::new(),
        },
    )
    .expect("write pane topology");
    let heartbeat = rimz::sidebar::heartbeat::SidebarHeartbeat::new(
        env.workspace_id.clone(),
        rimz::ids::SidebarInstanceId::new(),
        MuxName::Zellij,
        &workspace.session_name,
        runtime.sock_dir.join("sidebar.sock"),
        None,
    );
    std::fs::write(
        runtime.heartbeat_dir.join("sidebar.retry.json"),
        serde_json::to_vec(&heartbeat).expect("serialize heartbeat"),
    )
    .expect("write heartbeat");
    let mut command = env.rimz();
    command
        .args([
            "--mux",
            "zellij",
            "agents",
            "codex",
            "fix it",
            "-p",
            "--retries",
            "1",
            "-n",
            "fixed-name",
        ])
        .env("SHELL", shell)
        .env("PATH", path_with_front(&agent_bin))
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", trace_log)
        .env("RIMZ_TEST_ZELLIJ_LIST_PANES", "[]")
        .env("RIMZ_TEST_ZELLIJ_TOPOLOGY_PANES", "[]")
        .env("ZELLIJ_PANE_ID", "1")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if use_worktree {
        command.arg("--worktree=retry-worktree");
    }
    command.spawn().expect("spawn retrying print")
}

#[cfg(unix)]
fn spawn_verifying_print(
    env: &Env,
    trace_name: &str,
    max_attempts: u32,
    pane_fixture: &std::path::Path,
    timeout: Option<&str>,
) -> std::process::Child {
    env.install_agent_hooks("codex");
    trust_codex_hooks(env);
    let agent_bin = write_failing_agent_shim(env, "codex", 1);
    let shell = write_fake_login_shell(env, "rimz-test-sh", &[]);
    let trace_log = env.project_root.join(format!("{trace_name}.log"));
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    let mut command = env.rimz();
    command
        .args([
            "--mux",
            "zellij",
            "agents",
            "codex",
            "fix it",
            "-p",
            "--verify",
            "printf 'verify red\\n' >&2; test -f .verify-green",
            "--max-attempts",
            &max_attempts.to_string(),
        ])
        .env("SHELL", shell)
        .env("PATH", path_with_front(&agent_bin))
        .env("RIMZ_TEST_PANE_LIST", pane_fixture)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", trace_log)
        .env("ZELLIJ_PANE_ID", "1")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(timeout) = timeout {
        command.args(["--timeout", timeout]);
    }
    command.spawn().expect("spawn verifying print")
}

#[cfg(unix)]
fn register_verify_agent(env: &Env, store: &rimz::Store, record: &mut RunRecord) {
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    let agent_id = AgentSessionId::from("sess-verify");
    let pane_id = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let mut observation =
        AgentLifecycleObservation::new(Some(agent_id.clone()), LifecycleSignal::Registered);
    observation.agent_name = record.agent_name.clone();
    observation.worktree_path = Some(env.project_root.display().to_string());
    observation.pane_id = Some(pane_id.clone());
    store
        .append_event(&EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            workspace.session_name,
            "codex",
            "SessionStart",
            &observation,
        ))
        .expect("register verify agent");
    record.agent_id = Some(agent_id);
    record.pane_id = Some(pane_id);
}

#[cfg(unix)]
fn verify_agent_pane(env: &Env) -> rimz::pane::PaneRef {
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    rimz::pane::PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_9"),
        session_name: workspace.session_name,
        view_id: Some("tab_1".to_owned()),
        view_kind: Some(ViewKind::Tab),
        view_name: Some("project".to_owned()),
        title: None,
        is_focused: false,
        is_floating: false,
        command: Some("codex".to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(env.project_root.display().to_string()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: Some(AgentSessionId::from("sess-verify")),
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

#[cfg(unix)]
fn init_git_repo(root: &std::path::Path) -> bool {
    if !git_ok(root, &["init", "-q", "-b", "main"]) {
        return false;
    }
    let _ = git_ok(root, &["config", "user.email", "test@example.com"]);
    let _ = git_ok(root, &["config", "user.name", "Test User"]);
    std::fs::write(root.join("README.md"), "base\n").expect("write README");
    git_ok(root, &["add", "README.md"]) && git_ok(root, &["commit", "-q", "-m", "base"])
}

#[cfg(unix)]
fn git_ok(cwd: &std::path::Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn tombstone_retry_agents(env: &Env, store: &rimz::Store) {
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    // The trace backend opens no real wrapper process, so drain the provisional
    // launch cards an exiting wrapper would tombstone before cleanup.
    for _ in 0..3 {
        let agents = store.snapshot().expect("snapshot agents").agents;
        if agents.is_empty() {
            return;
        }
        for agent in agents {
            let observation = rimz::agents::AgentLifecycleObservation::new(
                Some(agent.agent_id),
                LifecycleSignal::Ended,
            );
            store
                .append_event(&rimz::EventEnvelope::agent_lifecycle(
                    env.workspace_id.clone(),
                    &workspace.session_name,
                    agent.kind.as_str(),
                    "rimz.agent-ended",
                    &observation,
                ))
                .expect("tombstone retry agent");
        }
    }
    panic!("retry launch cards did not tombstone");
}

#[cfg(unix)]
fn wait_for_run_count(
    store: &rimz::Store,
    child: &mut std::process::Child,
    expected: usize,
) -> Vec<RunRecord> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let records = rimz::harness::run::list(store.paths()).expect("list runs");
        if records.len() >= expected {
            return records;
        }
        if let Some(status) = child.try_wait().expect("poll supervised run") {
            let mut stdout = String::new();
            let mut stderr = String::new();
            child
                .stdout
                .take()
                .expect("child stdout")
                .read_to_string(&mut stdout)
                .expect("read child stdout");
            child
                .stderr
                .take()
                .expect("child stderr")
                .read_to_string(&mut stderr)
                .expect("read child stderr");
            panic!(
                "supervised run exited {status} before {expected} records\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {expected} run records; saw {records:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn wait_for_run_status(
    store: &rimz::Store,
    child: &mut std::process::Child,
    run_id: &rimz::RunId,
    expected: RunStatus,
) -> RunRecord {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let record = rimz::harness::run::load(store.paths(), run_id).expect("load run");
        if record.status == expected {
            return record;
        }
        assert!(
            child.try_wait().expect("poll supervised run").is_none(),
            "supervised run exited before reaching {expected:?}: {record:?}"
        );
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {expected:?}: {record:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn wait_for_message_count(
    store: &rimz::Store,
    child: &mut std::process::Child,
    expected: usize,
) -> Vec<rimz::message::MessageRecord> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let messages = store.list_messages().expect("list messages");
        if messages.len() >= expected
            && messages
                .iter()
                .take(expected)
                .all(|message| message.status == rimz::message::MessageStatus::Sent)
        {
            return messages;
        }
        assert!(
            child.try_wait().expect("poll supervised run").is_none(),
            "supervised run exited before queueing {expected} messages"
        );
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {expected} messages"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(unix)]
fn finish_run(store: &rimz::Store, record: &mut RunRecord) {
    record.updated_at = Timestamp::now();
    record.completed_at = Some(record.updated_at);
    rimz::store::run_store::write(&store.paths().runs_dir, record).expect("write terminal run");
    rimz::store::wakeup::wake_run(store.runtime_paths(), record).expect("wake run waiter");
}

#[cfg(unix)]
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

#[test]
fn run_stop_marks_canceled_and_wakes_waiter() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let store = env.store();
    let mut record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "summarize".to_owned(),
        env.project_root.clone(),
    );
    record.status = RunStatus::Running;
    let run_id = record.run_id.clone();
    rimz::harness::run::create(store.paths(), &record).expect("create run");
    let (sock, _sock_path) =
        rimz::harness::run_wake::bind_run(store.runtime_paths(), &run_id).expect("bind run socket");

    let out = env
        .rimz()
        .args(["agents", "stop", run_id.as_str()])
        .output()
        .expect("spawn agents stop");
    assert!(
        out.status.success(),
        "agents stop failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let canceled = rimz::harness::run::load(store.paths(), &run_id).expect("load canceled run");
    assert_eq!(canceled.status, RunStatus::Canceled);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .expect("runtime");
    let outcome = runtime
        .block_on(rimz::harness::run_wake::wait_for_run_completion_owning(
            sock,
            rimz::harness::run_wake::ExpectedRunFrame {
                workspace_id: env.workspace_id.clone(),
                run_id,
            },
            Some(Duration::from_secs(1)),
        ))
        .expect("wait for wake");
    assert_eq!(
        outcome,
        rimz::harness::run_wake::RunWakeOutcome::Completed(RunStatus::Canceled)
    );
}

#[test]
fn run_status_honors_pinned_room_inside_nested_repo() {
    let env = Env::new();
    let nested = env.project_root.join("code").join("query-engine");
    std::fs::create_dir_all(&nested).expect("mkdir nested repo");
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&nested)
        .status();
    match status {
        Ok(status) if status.success() => {}
        _ => {
            tracing::warn!("skipping: git unavailable");
            return;
        }
    }

    let store = env.store();
    let record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "summarize".to_owned(),
        nested.clone(),
    );
    let run_id = record.run_id.clone();
    rimz::harness::run::create(store.paths(), &record).expect("create run");

    let out = env
        .rimz()
        .current_dir(&nested)
        .env(rimz::workspace::ENV_WORKSPACE_ID, env.workspace_id.as_str())
        .env(rimz::workspace::ENV_PROJECT_ROOT, &env.project_root)
        .args(["agents", "show", run_id.as_str(), "--json"])
        .output()
        .expect("spawn agents show");
    assert!(
        out.status.success(),
        "agents show should read the pinned room store\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("status json");
    assert_eq!(parsed["run"]["run_id"], run_id.as_str());
    assert_eq!(parsed["run"]["workspace_id"], env.workspace_id.as_str());
}

#[test]
fn agents_show_converges_stale_pidless_audit_card_and_keeps_fresh_context() {
    let env = Env::new();
    let store = env.store();
    std::fs::write(store.paths().locks_dir.join("dead-reap.stamp"), b"")
        .expect("defer initial reap");
    let mut stale = rimz::agents::AgentLifecycleObservation::new(
        Some("sess-stale".into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    stale.agent_name = Some("lucid-atlas".to_owned());
    stale.worktree_branch = Some("pets".to_owned());
    let mut event = rimz::EventEnvelope::agent_lifecycle(
        env.workspace_id.clone(),
        "session",
        "claude",
        "SessionStart",
        &stale,
    );
    event.timestamp = jiff::Timestamp::now() - Duration::from_secs(4 * 60 * 60);
    store.append_event(&event).expect("append stale lifecycle");

    let before_reap = env
        .rimz()
        .args(["agents", "show", "lucid-atlas", "--json"])
        .output()
        .expect("spawn agents show before reap");
    assert!(
        before_reap.status.success(),
        "agents show should resolve the audit card before convergence\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&before_reap.stdout),
        String::from_utf8_lossy(&before_reap.stderr)
    );
    let before_reap: serde_json::Value =
        serde_json::from_slice(&before_reap.stdout).expect("show json before reap");
    assert_eq!(before_reap["agent"]["agent_id"], "sess-stale");
    assert_eq!(before_reap["stale"], true);

    let mut fresh = rimz::agents::AgentLifecycleObservation::new(
        Some("sess-fresh".into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    fresh.agent_name = Some("vivid-ocean".to_owned());
    fresh.worktree_branch = Some("fresh".to_owned());
    std::fs::remove_file(store.paths().locks_dir.join("dead-reap.stamp"))
        .expect("force convergence reap");
    store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "session",
            "claude",
            "SessionStart",
            &fresh,
        ))
        .expect("append fresh lifecycle and reap stale card");

    let after_reap = env
        .rimz()
        .args(["agents", "show", "lucid-atlas"])
        .output()
        .expect("spawn agents show after reap");
    assert!(
        !after_reap.status.success(),
        "tombstoned stale card should no longer resolve\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&after_reap.stdout),
        String::from_utf8_lossy(&after_reap.stderr)
    );
    let after_reap_stderr = String::from_utf8_lossy(&after_reap.stderr);
    assert!(
        after_reap_stderr.contains("no agent matches") && after_reap_stderr.contains("lucid-atlas"),
        "show should report the converged card as unknown: {after_reap_stderr}"
    );

    // A fresh session remains runtime-visible, and its rich statusline sidecar
    // still reaches the `show --json` payload.
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    let mut context = rimz::store::agent_context::empty_context("claude", jiff::Timestamp::now());
    context.tokens = Some(rimz::agents::AgentTokenUsage {
        context_window_size: Some(1_000_000),
        used_percentage: Some(30),
        current_usage: Some(rimz::agents::AgentCurrentUsage {
            input_tokens: Some(5_000),
            cache_read_input_tokens: Some(300_000),
            ..Default::default()
        }),
        ..Default::default()
    });
    let record = rimz::store::agent_context::new_record("claude", "sess-fresh", context);
    rimz::store::agent_context::write_record(&runtime, &record).expect("write context sidecar");

    let out = env
        .rimz()
        .args(["agents", "show", "vivid-ocean", "--json"])
        .output()
        .expect("spawn agents show for fresh card");

    assert!(
        out.status.success(),
        "agents show should resolve fresh card\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("show json");
    assert_eq!(parsed["agent"]["agent_id"], "sess-fresh");
    assert!(parsed.get("stale").is_none());
    assert_eq!(
        parsed["agent"]["context"]["tokens"]["context_window_size"], 1_000_000,
        "show --json folds the rich context window: {parsed}"
    );
    assert_eq!(
        parsed["agent"]["context"]["tokens"]["current_usage"]["cache_read_input_tokens"], 300_000,
        "folded usage reaches the payload: {parsed}"
    );
}

#[test]
fn agents_show_capture_errors_when_agent_has_no_bound_pane() {
    let env = Env::new();
    let store = env.store();
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some("sess-captureless".into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    observation.agent_name = Some("lucid-atlas".to_owned());
    observation.worktree_path = Some(env.project_root.display().to_string());
    store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "session",
            "claude",
            "SessionStart",
            &observation,
        ))
        .expect("append lifecycle");

    let out = env
        .rimz()
        .args(["agents", "show", "lucid-atlas", "--capture"])
        .output()
        .expect("spawn agents show --capture");

    assert!(
        !out.status.success(),
        "agents show --capture should fail without a bound pane\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("agent lucid-atlas has no bound pane"),
        "missing no-pane error: {stderr}"
    );
}

#[test]
fn agents_list_requires_live_room() {
    let env = Env::new();

    assert_agents_list_requires_live_room(&env, &["agents", "list"]);
    assert_agents_list_requires_live_room(&env, &["agents", "list", "--all"]);
}

fn assert_agents_list_requires_live_room(env: &Env, args: &[&str]) {
    let out = env.rimz().args(args).output().expect("spawn agents list");
    assert!(
        !out.status.success(),
        "agents list should require a live room\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no live RimZ room"),
        "missing live-room guidance: {stderr}"
    );
    assert!(
        stderr.contains("rimz start") && stderr.contains("rimz attach"),
        "guidance should name start and attach: {stderr}"
    );
}

#[test]
fn agents_scope_positional_lists_one_lane_and_address_hint_is_actionable() {
    let env = Env::new();
    let workspace = rimz::WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
    register_list_agent(&env, &workspace, "sess-auth", "claude", "auth", "%1");
    register_list_agent(&env, &workspace, "sess-ops", "codex", "ops", "%2");
    publish_pane_frame(
        &env,
        &workspace.session_name,
        vec![
            list_pane(
                &workspace.session_name,
                "%1",
                "claude",
                &env.home_root.join("auth"),
            ),
            list_pane(
                &workspace.session_name,
                "%2",
                "codex",
                &env.home_root.join("ops"),
            ),
        ],
    );

    let top_level = run_agents_json_list(&env, &workspace.session_name, &["agents", "#auth"]);
    assert_agent_ids(&top_level, &["sess-auth"]);

    let subcommand =
        run_agents_json_list(&env, &workspace.session_name, &["agents", "list", "#auth"]);
    assert_agent_ids(&subcommand, &["sess-auth"]);

    let out = env
        .rimz()
        .args(["agents", "@coder"])
        .output()
        .expect("agents address hint");
    assert!(!out.status.success(), "address-as-spec must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("agent address, not a launch spec")
            && stderr.contains("rimz agents show @coder")
            && stderr.contains("rimz message @coder"),
        "hint should name address verbs: {stderr}"
    );
}

fn register_list_agent(
    env: &Env,
    workspace: &rimz::ResolvedWorkspace,
    session_id: &str,
    kind: &str,
    branch: &str,
    pane_raw: &str,
) {
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some(AgentSessionId::from(session_id)),
        LifecycleSignal::Registered,
    );
    observation.worktree_path = Some(env.home_root.join(branch).display().to_string());
    observation.worktree_branch = Some(branch.to_owned());
    observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, pane_raw));
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id.clone(),
            workspace.session_name.clone(),
            kind,
            "SessionStart",
            &observation,
        ))
        .expect("append lifecycle");
}

fn list_pane(
    session_name: &str,
    raw: &str,
    command: &str,
    cwd: &std::path::Path,
) -> rimz::pane::PaneRef {
    rimz::pane::PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, raw),
        session_name: session_name.to_owned(),
        view_id: Some("@0".to_owned()),
        view_kind: Some(ViewKind::Window),
        view_name: Some("room".to_owned()),
        title: None,
        is_focused: false,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(cwd.display().to_string()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

fn publish_pane_frame(env: &Env, session_name: &str, panes: Vec<rimz::pane::PaneRef>) {
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    let frame = rimz::sidebar::frame::assemble_frame(
        panes,
        rimz::sidebar::timing::unix_now_ms(),
        session_name,
    );
    rimz::store::atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &frame)
        .expect("publish pane frame");
}

fn run_agents_json_list(env: &Env, session_name: &str, args: &[&str]) -> serde_json::Value {
    let trace_log = env.project_root.join(format!("{}.log", args.join("-")));
    let mut command = env.rimz();
    command
        .args(["--mux", "zellij"])
        .args(args)
        .arg("--json")
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", trace_log)
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{session_name} [Created 1s ago]\n"),
        );
    let out = command.output().expect("agents list json");
    assert!(
        out.status.success(),
        "agents list failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("agents json")
}

fn assert_agent_ids(json: &serde_json::Value, expected: &[&str]) {
    let actual: Vec<&str> = json
        .as_array()
        .expect("agent array")
        .iter()
        .map(|agent| agent["agent_id"].as_str().expect("agent_id"))
        .collect();
    assert_eq!(actual, expected, "scoped list returned {json:#}");
}

fn zellij_trace_shim() -> std::path::PathBuf {
    crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace"))
}

#[test]
fn run_stream_prints_codex_and_copilot_text_until_terminal_record() {
    for (kind, line) in [
        (
            "codex",
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"hello\"}}\n"
                .as_slice(),
        ),
        (
            "copilot",
            b"{\"type\":\"assistant.message\",\"data\":{\"content\":\"hello\"}}\n".as_slice(),
        ),
    ] {
        let env = Env::new();
        let store = env.store();
        let transcript = env.runtime_root.join("run-stream.jsonl");
        std::fs::write(&transcript, "").expect("seed transcript");
        let mut record = RunRecord::new(
            env.workspace_id.clone(),
            AgentKind::new_unchecked(kind),
            PermissionMode::Auto,
            "summarize".to_owned(),
            env.project_root.clone(),
        );
        record.status = RunStatus::Running;
        record.transcript_path = Some(transcript.to_string_lossy().into_owned());
        let run_id = record.run_id.clone();
        rimz::harness::run::create(store.paths(), &record).expect("create run");

        let child = env
            .rimz()
            .args([
                "agents",
                "wait",
                run_id.as_str(),
                "--stream",
                "--from-start",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn agents wait stream");
        std::thread::sleep(Duration::from_millis(100));
        std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .expect("open transcript")
            .write_all(line)
            .expect("append transcript");
        let mut terminal = rimz::harness::run::load(store.paths(), &run_id).expect("load run");
        terminal.status = RunStatus::Completed;
        terminal.last_message = Some("hello".to_owned());
        terminal.updated_at = Timestamp::now();
        terminal.completed_at = Some(terminal.updated_at);
        rimz::store::run_store::write(&store.paths().runs_dir, &terminal)
            .expect("write terminal");

        let out = child.wait_with_output().expect("wait agents stream");
        assert!(
            out.status.success(),
            "{kind} agents wait --stream failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hello\n", "{kind}");
    }
}

#[test]
fn run_stream_json_polls_transcript_until_terminal_record() {
    let env = Env::new();
    let store = env.store();
    let transcript = env.runtime_root.join("run-stream-json.jsonl");
    std::fs::write(&transcript, "").expect("seed transcript");
    let mut record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "summarize".to_owned(),
        env.project_root.clone(),
    );
    record.status = RunStatus::Running;
    record.transcript_path = Some(transcript.to_string_lossy().into_owned());
    let run_id = record.run_id.clone();
    rimz::harness::run::create(store.paths(), &record).expect("create run");

    let child = env
        .rimz()
        .args([
            "agents",
            "wait",
            run_id.as_str(),
            "--stream",
            "--json",
            "--from-start",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agents wait stream");
    std::thread::sleep(Duration::from_millis(100));
    std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .expect("open transcript")
        .write_all(
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"hello\"}}\n",
        )
        .expect("append transcript");
    let mut terminal = rimz::harness::run::load(store.paths(), &run_id).expect("load run");
    terminal.status = RunStatus::Completed;
    terminal.last_message = Some("hello".to_owned());
    terminal.updated_at = Timestamp::now();
    terminal.completed_at = Some(terminal.updated_at);
    rimz::store::run_store::write(&store.paths().runs_dir, &terminal).expect("write terminal");

    let out = child.wait_with_output().expect("wait agents stream");
    assert!(
        out.status.success(),
        "agents wait --stream --json failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("ndjson line"))
        .collect::<Vec<_>>();
    assert_eq!(lines[0], json!({ "event": "message", "text": "hello" }));
    assert_eq!(
        lines.last().unwrap(),
        &json!({ "event": "end", "status": "completed", "last_message": "hello" })
    );
}

#[test]
fn run_stream_timeout_stops_watching_without_timing_out_run() {
    let env = Env::new();
    let store = env.store();
    let mut record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "summarize".to_owned(),
        env.project_root.clone(),
    );
    record.status = RunStatus::Running;
    let run_id = record.run_id.clone();
    rimz::harness::run::create(store.paths(), &record).expect("create run");

    let out = env
        .rimz()
        .args([
            "agents",
            "wait",
            run_id.as_str(),
            "--stream",
            "--timeout",
            "0s",
        ])
        .output()
        .expect("spawn agents wait stream timeout");
    assert_eq!(
        out.status.code(),
        Some(124),
        "agents wait stream timeout should exit 124 without marking the run terminal\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let loaded = rimz::harness::run::load(store.paths(), &run_id).expect("load run");
    assert_eq!(loaded.status, RunStatus::Running);
}

fn create_running_named_run(env: &Env, store: &rimz::Store, name: &str) -> RunRecord {
    let mut record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        format!("task for {name}"),
        env.project_root.clone(),
    );
    record.agent_name = Some(name.to_owned());
    record.status = RunStatus::Running;
    rimz::harness::run::create(store.paths(), &record).expect("create running run");
    record
}

fn write_run_status(store: &rimz::Store, record: &mut RunRecord, status: RunStatus) {
    record.status = status;
    record.updated_at = Timestamp::now();
    record.completed_at = status.is_terminal().then_some(record.updated_at);
    rimz::store::run_store::write(&store.paths().runs_dir, record).expect("write run status");
}

#[test]
fn wait_multi_blocks_until_all_terminal() {
    let env = Env::new();
    let store = env.store();
    let mut otter = create_running_named_run(&env, &store, "swift-otter");
    let mut fox = create_running_named_run(&env, &store, "quiet-fox");

    let child = env
        .rimz()
        .args(["agents", "wait", "swift-otter", "quiet-fox"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn multi-target wait");
    std::thread::sleep(Duration::from_millis(100));
    write_run_status(&store, &mut otter, RunStatus::Completed);
    std::thread::sleep(Duration::from_millis(600));
    write_run_status(&store, &mut fox, RunStatus::Completed);

    let out = child
        .wait_with_output()
        .expect("wait for multi-target wait");
    assert!(
        out.status.success(),
        "multi-target wait failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "swift-otter completed\nquiet-fox completed\n"
    );
}

#[test]
fn wait_multi_exits_with_first_failed_code() {
    let env = Env::new();
    let store = env.store();
    let mut completed = create_running_named_run(&env, &store, "swift-otter");
    let mut failed = create_running_named_run(&env, &store, "quiet-fox");

    let child = env
        .rimz()
        .args(["agents", "wait", "swift-otter", "quiet-fox"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn failed multi-target wait");
    std::thread::sleep(Duration::from_millis(100));
    write_run_status(&store, &mut completed, RunStatus::Completed);
    write_run_status(&store, &mut failed, RunStatus::Failed);

    let out = child.wait_with_output().expect("wait for failed join");
    assert_eq!(
        out.status.code(),
        Some(1),
        "failed join returned wrong status\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn wait_multi_json_emits_labeled_map() {
    let env = Env::new();
    let store = env.store();
    let mut otter = create_running_named_run(&env, &store, "swift-otter");
    let mut fox = create_running_named_run(&env, &store, "quiet-fox");

    let child = env
        .rimz()
        .args(["agents", "wait", "swift-otter", "quiet-fox", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn JSON multi-target wait");
    std::thread::sleep(Duration::from_millis(100));
    write_run_status(&store, &mut otter, RunStatus::Completed);
    write_run_status(&store, &mut fox, RunStatus::Completed);

    let out = child.wait_with_output().expect("wait for JSON join");
    assert!(out.status.success());
    let result: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("labeled wait result map");
    assert_eq!(result.as_object().map(serde_json::Map::len), Some(2));
    assert_eq!(result["swift-otter"]["status"], "completed");
    assert_eq!(result["swift-otter"]["exit"], 0);
    assert_eq!(result["quiet-fox"]["status"], "completed");
    assert_eq!(result["quiet-fox"]["exit"], 0);
}

#[test]
fn wait_multi_json_reports_failed_entry_and_exit() {
    let env = Env::new();
    let store = env.store();
    let mut completed = create_running_named_run(&env, &store, "swift-otter");
    let mut failed = create_running_named_run(&env, &store, "quiet-fox");
    write_run_status(&store, &mut completed, RunStatus::Completed);
    failed.failure_tail = Some("review failed".to_owned());
    write_run_status(&store, &mut failed, RunStatus::Failed);

    let out = env
        .rimz()
        .args(["agents", "wait", "swift-otter", "quiet-fox", "--json"])
        .output()
        .expect("run failed JSON join");

    assert_eq!(out.status.code(), Some(1));
    let result: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("failed wait result map");
    assert_eq!(result["swift-otter"]["status"], "completed");
    assert_eq!(result["quiet-fox"]["status"], "failed");
    assert_eq!(result["quiet-fox"]["exit"], 1);
    assert_eq!(result["quiet-fox"]["error"], "review failed");
}

#[test]
fn wait_any_prints_first_finisher_and_leaves_rest_running() {
    let env = Env::new();
    let store = env.store();
    let otter = create_running_named_run(&env, &store, "swift-otter");
    let mut fox = create_running_named_run(&env, &store, "quiet-fox");

    let child = env
        .rimz()
        .args(["agents", "wait", "swift-otter", "quiet-fox", "--any"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn first-finisher wait");
    std::thread::sleep(Duration::from_millis(100));
    write_run_status(&store, &mut fox, RunStatus::Completed);

    let out = child.wait_with_output().expect("wait for first finisher");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "quiet-fox\n");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "quiet-fox completed\n"
    );
    let other = rimz::harness::run::load(store.paths(), &otter.run_id).expect("load other run");
    assert_eq!(other.status, RunStatus::Running);
}

#[test]
fn wait_any_json_prints_winner_map() {
    let env = Env::new();
    let store = env.store();
    let otter = create_running_named_run(&env, &store, "swift-otter");
    let mut fox = create_running_named_run(&env, &store, "quiet-fox");
    write_run_status(&store, &mut fox, RunStatus::Completed);

    let out = env
        .rimz()
        .args([
            "agents",
            "wait",
            "swift-otter",
            "quiet-fox",
            "--any",
            "--json",
        ])
        .output()
        .expect("run JSON first-finisher wait");

    assert!(out.status.success());
    let result: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("winner wait result map");
    assert_eq!(result.as_object().map(serde_json::Map::len), Some(1));
    assert_eq!(result["quiet-fox"]["status"], "completed");
    assert_eq!(result["quiet-fox"]["exit"], 0);
    let other = rimz::harness::run::load(store.paths(), &otter.run_id).expect("load other run");
    assert_eq!(other.status, RunStatus::Running);
}

#[test]
fn wait_any_first_finisher_failure_is_nonzero() {
    let env = Env::new();
    let store = env.store();
    let _otter = create_running_named_run(&env, &store, "swift-otter");
    let mut fox = create_running_named_run(&env, &store, "quiet-fox");

    let child = env
        .rimz()
        .args(["agents", "wait", "swift-otter", "quiet-fox", "--any"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn failing first-finisher wait");
    std::thread::sleep(Duration::from_millis(100));
    write_run_status(&store, &mut fox, RunStatus::Failed);

    let out = child
        .wait_with_output()
        .expect("wait for failed first finisher");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "quiet-fox\n");
}

#[test]
fn wait_multi_rejects_stream() {
    let env = Env::new();
    let out = env
        .rimz()
        .args(["agents", "wait", "swift-otter", "quiet-fox", "--stream"])
        .output()
        .expect("run invalid streamed join");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("--stream tails one target; wait on a single reference")
    );
}

#[test]
fn wait_multi_timeout_exits_124() {
    let env = Env::new();
    let store = env.store();
    let otter = create_running_named_run(&env, &store, "swift-otter");
    let fox = create_running_named_run(&env, &store, "quiet-fox");

    let out = env
        .rimz()
        .args([
            "agents",
            "wait",
            "swift-otter",
            "quiet-fox",
            "--timeout",
            "0s",
        ])
        .output()
        .expect("run timed multi-target wait");
    assert_eq!(out.status.code(), Some(124));
    for run_id in [&otter.run_id, &fox.run_id] {
        let record = rimz::harness::run::load(store.paths(), run_id).expect("load running run");
        assert_eq!(record.status, RunStatus::Running);
    }
}

#[test]
fn wait_multi_json_timeout_stamps_pending_entries() {
    let env = Env::new();
    let store = env.store();
    let mut otter = create_running_named_run(&env, &store, "swift-otter");
    let fox = create_running_named_run(&env, &store, "quiet-fox");
    write_run_status(&store, &mut otter, RunStatus::Completed);

    let out = env
        .rimz()
        .args([
            "agents",
            "wait",
            "swift-otter",
            "quiet-fox",
            "--timeout",
            "0s",
            "--json",
        ])
        .output()
        .expect("run timed JSON join");

    assert_eq!(out.status.code(), Some(124));
    let result: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("timed wait result map");
    assert_eq!(result["swift-otter"]["status"], "completed");
    assert_eq!(result["swift-otter"]["exit"], 0);
    assert_eq!(result["quiet-fox"]["status"], "timed_out");
    assert_eq!(result["quiet-fox"]["exit"], 124);
    let pending = rimz::harness::run::load(store.paths(), &fox.run_id).expect("load pending run");
    assert_eq!(pending.status, RunStatus::Running);
}
