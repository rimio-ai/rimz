use crate::common::Env;
use jiff::Timestamp;
use rimz::agents::LifecycleSignal;
use rimz::harness::run::{PermissionMode, RunRecord, RunStatus};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId, ViewKind};
use serde_json::json;
use std::io::Write as _;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

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
    cmd.args(["agents", "codex", "-p"])
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
        "piped stdin should satisfy text input\nstderr:\n{stderr}"
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

#[cfg(target_os = "linux")]
#[test]
fn agents_show_falls_back_to_audit_rollup_for_stale_card() {
    let env = Env::new();
    let store = env.store();
    let observation = rimz::agents::AgentLifecycleObservation {
        agent_id: Some("sess-stale".into()),
        agent_name: Some("lucid-atlas".to_owned()),
        launch: rimz::agents::LaunchParams::default(),
        signal: rimz::agents::LifecycleSignal::Registered,
        agent_pid: Some(u32::MAX),
        agent_process_start: None,
        runtime_owner: Some(rimz::RuntimeOwner::new(
            rimz::RuntimeOwnerKind::Agent,
            "sess-stale",
            u32::MAX,
            None,
        )),
        worktree_path: None,
        worktree_branch: Some("pets".to_owned()),
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
    store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            "session",
            "claude",
            "SessionStart",
            &observation,
        ))
        .expect("append stale lifecycle");

    // A rich statusline sidecar for the same session: `show --json` must fold it
    // onto the resolved card (even via the audit fallback) so the real token
    // window reaches consumers, not the carried-forward `context_pct`.
    let runtime = env.runtime_paths();
    runtime.ensure_dirs().expect("runtime dirs");
    // A fresh `observed_at`: `read_all` ages out a sidecar past its TTL.
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
    let record = rimz::store::agent_context::new_record("claude", "sess-stale", context);
    rimz::store::agent_context::write_record(&runtime, &record).expect("write context sidecar");

    let out = env
        .rimz()
        .args(["agents", "show", "lucid-atlas", "--json"])
        .output()
        .expect("spawn agents show");

    assert!(
        out.status.success(),
        "agents show should resolve stale card from audit rollup\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("show json");
    assert_eq!(parsed["agent"]["agent_id"], "sess-stale");
    assert_eq!(parsed["stale"], true);
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
        stderr.contains("no live Rimz room"),
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
        is_focused: false,
        is_floating: false,
        command: Some(command.to_owned()),
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
fn run_stream_polls_transcript_until_terminal_record() {
    let env = Env::new();
    let store = env.store();
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
        "agents wait --stream failed\nstdout:\n{}\nstderr:\n{}",
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
