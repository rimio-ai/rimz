use crate::common::Env;
use jiff::Timestamp;
use rimz::ids::AgentKind;
use rimz::run::{PermissionMode, RunRecord, RunStatus};
use serde_json::json;
use std::io::Write as _;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

#[test]
fn hooks_bind_and_complete_supervised_run() {
    let env = Env::new();
    let ledger = env.ledger();
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
    rimz::run::create(ledger.paths(), &record).expect("create run");

    let prompt_payload = json!({
        "hook_event_name": "UserPromptSubmit",
        "session_id": "sess-run",
        "prompt": "summarize"
    })
    .to_string();
    let mut prompt_cmd = env.hook_command("codex");
    prompt_cmd.env(rimz::run::ENV_RUN_ID, run_id.as_str());
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
    let running = rimz::run::load(ledger.paths(), &run_id).expect("load running run");
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
    stop_cmd.env(rimz::run::ENV_RUN_ID, run_id.as_str());
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
    let completed = rimz::run::load(ledger.paths(), &run_id).expect("load completed run");
    assert_eq!(completed.status, RunStatus::Completed);
    assert_eq!(completed.last_message.as_deref(), Some("done"));
    assert_eq!(
        completed.transcript_path.as_deref(),
        Some(transcript.to_str().unwrap())
    );
}

#[test]
fn run_stop_marks_canceled_and_wakes_waiter() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let ledger = env.ledger();
    let mut record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "summarize".to_owned(),
        env.project_root.clone(),
    );
    record.status = RunStatus::Running;
    let run_id = record.run_id.clone();
    rimz::run::create(ledger.paths(), &record).expect("create run");
    let (sock, _sock_path) =
        rimz::bridge::bind_run(ledger.runtime_paths(), &run_id).expect("bind run socket");

    let out = env
        .rimz()
        .args(["run", "stop", run_id.as_str()])
        .output()
        .expect("spawn run stop");
    assert!(
        out.status.success(),
        "run stop failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let canceled = rimz::run::load(ledger.paths(), &run_id).expect("load canceled run");
    assert_eq!(canceled.status, RunStatus::Canceled);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .expect("runtime");
    let outcome = runtime
        .block_on(rimz::bridge::wait_for_run_completion_owning(
            sock,
            rimz::bridge::ExpectedRunFrame {
                workspace_id: env.workspace_id.clone(),
                run_id,
            },
            Some(Duration::from_secs(1)),
        ))
        .expect("wait for wake");
    assert_eq!(
        outcome,
        rimz::bridge::RunWakeOutcome::Completed(RunStatus::Canceled)
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

    let ledger = env.ledger();
    let record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "summarize".to_owned(),
        nested.clone(),
    );
    let run_id = record.run_id.clone();
    rimz::run::create(ledger.paths(), &record).expect("create run");

    let out = env
        .rimz()
        .current_dir(&nested)
        .env(rimz::workspace::ENV_WORKSPACE_ID, env.workspace_id.as_str())
        .env(rimz::workspace::ENV_PROJECT_ROOT, &env.project_root)
        .args(["run", "status", run_id.as_str(), "--json"])
        .output()
        .expect("spawn run status");
    assert!(
        out.status.success(),
        "run status should read the pinned room ledger\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("status json");
    assert_eq!(parsed["run_id"], run_id.as_str());
    assert_eq!(parsed["workspace_id"], env.workspace_id.as_str());
}

#[test]
fn run_stream_polls_transcript_until_terminal_record() {
    let env = Env::new();
    let ledger = env.ledger();
    let transcript = env.runtime_root.join("run-stream.jsonl");
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
    rimz::run::create(ledger.paths(), &record).expect("create run");

    let child = env
        .rimz()
        .args(["run", "stream", run_id.as_str(), "--from-start"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn run stream");
    std::thread::sleep(Duration::from_millis(100));
    std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .expect("open transcript")
        .write_all(
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"hello\"}}\n",
        )
        .expect("append transcript");
    let mut terminal = rimz::run::load(ledger.paths(), &run_id).expect("load run");
    terminal.status = RunStatus::Completed;
    terminal.last_message = Some("hello".to_owned());
    terminal.updated_at = Timestamp::now();
    terminal.completed_at = Some(terminal.updated_at);
    rimz::ledger::run_store::write(&ledger.paths().runs_dir, &terminal).expect("write terminal");

    let out = child.wait_with_output().expect("wait run stream");
    assert!(
        out.status.success(),
        "run stream failed\nstdout:\n{}\nstderr:\n{}",
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
    let ledger = env.ledger();
    let mut record = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "summarize".to_owned(),
        env.project_root.clone(),
    );
    record.status = RunStatus::Running;
    let run_id = record.run_id.clone();
    rimz::run::create(ledger.paths(), &record).expect("create run");

    let out = env
        .rimz()
        .args(["run", "stream", run_id.as_str(), "--timeout", "0s"])
        .output()
        .expect("spawn run stream timeout");
    assert_eq!(
        out.status.code(),
        Some(124),
        "run stream timeout should exit 124 without marking the run terminal\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let loaded = rimz::run::load(ledger.paths(), &run_id).expect("load run");
    assert_eq!(loaded.status, RunStatus::Running);
}
