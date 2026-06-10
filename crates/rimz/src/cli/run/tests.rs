use super::*;
use clap::Parser;
use rimz::bridge::{ExpectedRunFrame, WakeupFrame};
use rimz::feed::{AgentState, AgentStatus, PaneRef};
use rimz::ids::{AgentSessionId, MuxName, PaneId, WorkspaceId};
use rimz::ledger::{RuntimePaths, StatePaths};
use tokio::net::UnixDatagram;

#[derive(Debug, Parser)]
struct RunHarness {
    #[command(flatten)]
    args: RunArgs,
}

#[test]
fn permission_mode_rejects_conflicting_flags() {
    let args = RunArgs {
        command: None,
        prompt: Some("hi".to_owned()),
        agent: Some("claude".to_owned()),
        worktree: None,
        ask: true,
        yolo: true,
        timeout: None,
        keep: false,
        detach: false,
        json: false,
        stream: false,
    };
    assert!(permission_mode(&args).is_err());
}

#[test]
fn parse_timeout_accepts_duration_units() {
    assert_eq!(parse_timeout("30s").unwrap(), Duration::from_secs(30));
    assert_eq!(parse_timeout("5m").unwrap(), Duration::from_secs(300));
    assert_eq!(parse_timeout("1h").unwrap(), Duration::from_secs(3600));
    assert_eq!(parse_timeout("1d").unwrap(), Duration::from_secs(86_400));
}

#[test]
fn send_subcommand_requires_separator_and_parses_enter() {
    let run_id = rimz::RunId::new();
    let parsed =
        RunHarness::try_parse_from(["run", "send", run_id.as_str(), "--enter", "--", "continue"])
            .expect("parse send");
    let Some(RunSubcmd::Send {
        run_id: parsed_id,
        enter,
        text,
    }) = parsed.args.command
    else {
        panic!("expected send subcommand");
    };
    assert_eq!(parsed_id, run_id);
    assert!(enter);
    assert_eq!(text, "continue");

    assert!(
        RunHarness::try_parse_from(["run", "send", run_id.as_str(), "continue"]).is_err(),
        "the free text must live after --"
    );
}

#[test]
fn terminal_run_is_not_sendable() {
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let mut record = RunRecord::new(
        workspace_id,
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.status = RunStatus::Canceled;

    let err = ensure_sendable(&record).expect_err("terminal run rejects sends");
    assert!(err.to_string().contains("nothing to send"));
}

#[test]
fn stream_output_mode_suppresses_final_message_print() {
    assert_eq!(
        blocking_run_output(false, true),
        BlockingRunOutput::StreamAlreadyEmitted
    );
    assert_eq!(blocking_run_output(true, false), BlockingRunOutput::Json);
    assert_eq!(
        blocking_run_output(false, false),
        BlockingRunOutput::FinalMessage
    );
}

#[test]
fn pane_resolution_uses_snapshot_when_record_has_no_pane() {
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let mut record = RunRecord::new(
        workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.agent_id = Some(AgentSessionId::from("sess-1"));
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%9");
    let mut pane = PaneRef::from_id(pane_id.clone());
    pane.session_name = "live-session".to_owned();
    let mut agent = agent_state("claude", "sess-1", AgentStatus::Running);
    agent.pane = Some(pane);
    let snapshot = rimz::SidebarSnapshot::build_with_agents(
        workspace_id,
        Vec::new(),
        vec![agent],
        jiff::Timestamp::UNIX_EPOCH,
    );

    let resolved = resolve_run_pane_in_snapshot(&snapshot, "fallback-session", &record).unwrap();
    assert_eq!(resolved.pane_id, pane_id);
    assert_eq!(resolved.session_name, "live-session");
}

#[test]
fn stop_backstop_uses_late_recorded_pane_id() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let ledger = rimz::Ledger::open(paths.clone(), runtime).unwrap();
    let mut stale = RunRecord::new(
        workspace_id,
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    stale.status = RunStatus::Canceled;
    rimz::run::create(ledger.paths(), &stale).unwrap();
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%8");
    rimz::run::record_pane(ledger.paths(), &stale.run_id, pane_id.clone()).unwrap();

    let (latest, resolved) = latest_resolved_run_pane(&ledger, "rimz-test", &stale).unwrap();
    assert_eq!(latest.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(resolved.pane_id, pane_id);
    assert_eq!(resolved.session_name, "rimz-test");
}

#[test]
fn stream_event_shapes_are_ndjson_ready() {
    let value = serde_json::to_value(RunStreamEvent::End {
        status: RunStatus::Canceled,
        last_message: Some("bye".to_owned()),
    })
    .unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "event": "end",
            "status": "canceled",
            "last_message": "bye"
        })
    );
}

#[test]
fn blocking_stream_wakeup_reloads_terminal_record() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();
    let ledger = rimz::Ledger::open(paths.clone(), runtime.clone()).unwrap();
    let mut record = RunRecord::new(
        workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.status = RunStatus::Running;
    let run_id = record.run_id.clone();
    rimz::run::create(&paths, &record).unwrap();
    let (sock, sock_path) = bridge::bind_run(&runtime, &run_id).unwrap();

    record.status = RunStatus::Completed;
    record.last_message = Some("done".to_owned());
    rimz::ledger::run_store::write(&paths.runs_dir, &record).unwrap();
    send_run_frame(
        &sock_path,
        &WakeupFrame::RunCompleted {
            workspace_id: workspace_id.clone(),
            run_id: run_id.clone(),
            status: RunStatus::Completed,
        },
    );

    let loaded = stream_blocking_run(
        sock,
        ExpectedRunFrame {
            workspace_id,
            run_id: run_id.clone(),
        },
        &ledger,
        &run_id,
        &rimz::agents::CodexAdapter,
        Some(Duration::from_secs(1)),
    )
    .unwrap();

    assert_eq!(loaded.status, RunStatus::Completed);
    assert_eq!(loaded.last_message.as_deref(), Some("done"));
}

#[test]
fn blocking_stream_timeout_marks_run_timed_out() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();
    let ledger = rimz::Ledger::open(paths.clone(), runtime.clone()).unwrap();
    let mut record = RunRecord::new(
        workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.status = RunStatus::Running;
    let run_id = record.run_id.clone();
    rimz::run::create(&paths, &record).unwrap();
    let (sock, _sock_path) = bridge::bind_run(&runtime, &run_id).unwrap();

    let timed_out = stream_blocking_run(
        sock,
        ExpectedRunFrame {
            workspace_id,
            run_id: run_id.clone(),
        },
        &ledger,
        &run_id,
        &rimz::agents::CodexAdapter,
        Some(Duration::ZERO),
    )
    .unwrap();

    assert_eq!(timed_out.status, RunStatus::TimedOut);
    assert_eq!(
        rimz::run::load(&paths, &run_id).unwrap().status,
        RunStatus::TimedOut
    );
}

#[test]
fn attached_stream_timeout_does_not_mark_run_timed_out() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();
    let ledger = rimz::Ledger::open(paths.clone(), runtime).unwrap();
    let mut record = RunRecord::new(
        workspace_id,
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.status = RunStatus::Running;
    let run_id = record.run_id.clone();
    rimz::run::create(&paths, &record).unwrap();

    let outcome = stream_attached_run(
        &ledger,
        &run_id,
        &rimz::agents::CodexAdapter,
        false,
        Some(Duration::ZERO),
    )
    .unwrap();

    assert_eq!(outcome, None);
    assert_eq!(
        rimz::run::load(&paths, &run_id).unwrap().status,
        RunStatus::Running
    );
}

#[test]
fn transcript_cursor_skips_existing_attach_bytes_and_resets_on_path_change() {
    use std::io::Write as _;

    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.jsonl");
    std::fs::write(
        &first,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"old\"}}\n",
    )
    .unwrap();
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let mut record = RunRecord::new(
        workspace_id,
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.transcript_path = Some(first.to_string_lossy().into_owned());
    let mut cursor = TranscriptCursor::new(false);

    assert!(
        cursor
            .messages(&record, &rimz::agents::CodexAdapter)
            .is_empty(),
        "default attach starts at the current end"
    );

    std::fs::OpenOptions::new()
            .append(true)
            .open(&first)
            .unwrap()
            .write_all(
                b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"new\"}}\n",
            )
            .unwrap();
    assert_eq!(
        cursor.messages(&record, &rimz::agents::CodexAdapter),
        vec!["new"]
    );

    let second = dir.path().join("second.jsonl");
    std::fs::write(
        &second,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"fresh\"}}\n",
    )
    .unwrap();
    record.transcript_path = Some(second.to_string_lossy().into_owned());
    assert_eq!(
        cursor.messages(&record, &rimz::agents::CodexAdapter),
        vec!["fresh"],
        "a new transcript path starts at byte zero"
    );
}

#[test]
fn completed_run_wakeup_reloads_terminal_record() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();
    let mut record = RunRecord::new(
        workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    let run_id = record.run_id.clone();
    rimz::run::create(&paths, &record).unwrap();
    let (sock, sock_path) = bridge::bind_run(&runtime, &run_id).unwrap();

    record.status = RunStatus::Completed;
    record.last_message = Some("done".to_owned());
    rimz::ledger::run_store::write(&paths.runs_dir, &record).unwrap();
    let frame = WakeupFrame::RunCompleted {
        workspace_id: workspace_id.clone(),
        run_id: run_id.clone(),
        status: RunStatus::Completed,
    };
    send_run_frame(&sock_path, &frame);

    let outcome = wait_for_run(
        sock,
        ExpectedRunFrame {
            workspace_id,
            run_id: run_id.clone(),
        },
        Some(Duration::from_secs(1)),
    )
    .unwrap();
    let loaded = terminal_record_after_wait(&paths, &run_id, outcome).unwrap();

    assert_eq!(loaded.status, RunStatus::Completed);
    assert_eq!(loaded.last_message.as_deref(), Some("done"));
    assert_eq!(loaded.status.exit_code(), 0);
}

#[test]
fn neutral_run_wait_marks_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();
    let record = RunRecord::new(
        workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    let run_id = record.run_id.clone();
    rimz::run::create(&paths, &record).unwrap();
    let (sock, _sock_path) = bridge::bind_run(&runtime, &run_id).unwrap();

    let outcome = wait_for_run(
        sock,
        ExpectedRunFrame {
            workspace_id,
            run_id: run_id.clone(),
        },
        Some(Duration::from_millis(10)),
    )
    .unwrap();
    let timed_out = terminal_record_after_wait(&paths, &run_id, outcome).unwrap();

    assert_eq!(timed_out.status, RunStatus::TimedOut);
    assert_eq!(timed_out.status.exit_code(), 124);
}

fn send_run_frame(path: &Path, frame: &WakeupFrame) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap();
    runtime.block_on(async {
        let sender = UnixDatagram::unbound().unwrap();
        let bytes = serde_json::to_vec(frame).unwrap();
        sender.send_to(&bytes, path).await.unwrap();
    });
}

fn agent_state(kind: &str, id: &str, status: AgentStatus) -> AgentState {
    AgentState {
        agent_id: AgentSessionId::from(id),
        kind: AgentKind::new_unchecked(kind),
        status,
        phase: rimz::agents::TurnPhase::Idle,
        pane: None,
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        parent_agent_id: None,
        worktree_path: None,
        worktree_branch: None,
        task: None,
        prompt: None,
        transcript_path: None,
        recent_prompts: Vec::new(),
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        compacting_since: None,
        compaction_count: 0,
        last_seen: jiff::Timestamp::UNIX_EPOCH,
        last_activity: jiff::Timestamp::UNIX_EPOCH,
        registered_at: Some(jiff::Timestamp::UNIX_EPOCH),
    }
}
