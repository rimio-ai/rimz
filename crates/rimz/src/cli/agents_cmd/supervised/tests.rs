use super::*;
use rimz::agents::{AgentState, AgentStatus};
use rimz::harness::run::{PermissionMode, RunStatus};
use rimz::harness::run_wake::{self, ExpectedRunFrame, WakeupFrame};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
use rimz::pane::PaneRef;
use rimz::store::{RuntimePaths, StatePaths};
use std::path::PathBuf;
use tokio::net::UnixDatagram;

#[test]
fn stream_json_prompt_concatenates_user_message_text() {
    // String content and text-block content both contribute; non-user
    // envelopes (assistant, system) are ignored.
    let input = "\
{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"first line\"}}
{\"type\":\"assistant\",\"message\":{\"content\":\"ignored\"}}
{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"second line\"},{\"type\":\"image\"}]}}
";
    let prompt = read_stream_json_prompt(std::io::Cursor::new(input)).expect("parse stream-json");
    assert_eq!(prompt, "first line\nsecond line");
}

#[test]
fn stream_json_prompt_rejects_malformed_lines() {
    let err = read_stream_json_prompt(std::io::Cursor::new("not json\n"))
        .expect_err("malformed stream-json line fails");
    assert!(err.to_string().contains("stream-json line"), "{err:#}");
}

#[test]
fn text_prompt_combines_instruction_before_piped_content() {
    let prompt = combine_text_prompt(Some("explain"), Some("boom")).expect("combine prompt");
    assert_eq!(prompt, "explain\n\n<stdin>\nboom\n</stdin>");
}

#[test]
fn text_prompt_accepts_positional_only() {
    let prompt = combine_text_prompt(Some("explain"), None).expect("combine prompt");
    assert_eq!(prompt, "explain");
}

#[test]
fn text_prompt_accepts_piped_only() {
    let prompt = combine_text_prompt(None, Some("boom")).expect("combine prompt");
    assert_eq!(prompt, "boom");
}

#[test]
fn text_prompt_rejects_empty_inputs() {
    let err = combine_text_prompt(Some("  "), Some("\n\t"))
        .expect_err("whitespace-only prompt should fail");
    assert!(err.to_string().contains("positional PROMPT or piped stdin"));
}

#[test]
fn text_prompt_ignores_empty_piped_input() {
    let prompt = combine_text_prompt(Some("ping"), Some("")).expect("combine prompt");
    assert_eq!(prompt, "ping");

    let prompt = combine_text_prompt(Some("ping"), Some("\n\t")).expect("combine prompt");
    assert_eq!(prompt, "ping");
}

#[test]
fn text_prompt_trims_surrounding_whitespace() {
    let prompt =
        combine_text_prompt(Some("  explain  "), Some("\nboom\t")).expect("combine prompt");
    assert_eq!(prompt, "explain\n\n<stdin>\nboom\n</stdin>");
}

#[test]
fn terminal_run_is_not_sendable() {
    let mut record = run_record("codex");
    record.status = RunStatus::Canceled;

    let err = ensure_sendable(&record).expect_err("terminal run rejects sends");
    assert!(err.to_string().contains("nothing to send"));
}

#[test]
fn pane_resolution_uses_snapshot_when_record_has_no_pane() {
    let mut record = run_record("claude");
    let workspace_id = record.workspace_id.clone();
    record.agent_id = Some(AgentSessionId::from("sess-1"));
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%9");
    let mut pane = PaneRef::from_id(pane_id.clone());
    pane.session_name = "live-session".to_owned();
    let mut agent = agent_state("claude", "sess-1", AgentStatus::Running);
    agent.pane = Some(pane);
    let snapshot = rimz::SidebarSnapshot::build_with_agents(
        workspace_id,
        vec![agent],
        jiff::Timestamp::UNIX_EPOCH,
    );

    let resolved = resolve_run_pane_in_snapshot(&snapshot, "fallback-session", &record).unwrap();
    assert_eq!(resolved.pane_id, pane_id);
    assert_eq!(resolved.session_name, "live-session");
}

#[test]
fn stop_backstop_uses_late_recorded_pane_id() {
    let fixture = RunFixture::new(RunStatus::Canceled);
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%8");
    rimz::harness::run::record_pane(
        fixture.store.paths(),
        &fixture.record.run_id,
        pane_id.clone(),
    )
    .unwrap();

    let (latest, resolved) =
        latest_resolved_run_pane(&fixture.store, "rimz-test", &fixture.record).unwrap();
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

struct RunFixture {
    _dir: tempfile::TempDir,
    workspace_id: WorkspaceId,
    paths: StatePaths,
    runtime: RuntimePaths,
    store: rimz::Store,
    record: RunRecord,
}

impl RunFixture {
    fn new(status: RunStatus) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("rs")
            .tempdir_in("/tmp")
            .unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        let store = rimz::Store::open(paths.clone(), runtime.clone()).unwrap();
        let mut record = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.status = status;
        rimz::harness::run::create(&paths, &record).unwrap();
        Self {
            _dir: dir,
            workspace_id,
            paths,
            runtime,
            store,
            record,
        }
    }

    fn run_id(&self) -> rimz::RunId {
        self.record.run_id.clone()
    }

    fn expected(&self) -> ExpectedRunFrame {
        ExpectedRunFrame {
            workspace_id: self.workspace_id.clone(),
            run_id: self.run_id(),
        }
    }

    fn bind(&self) -> (std::os::unix::net::UnixDatagram, PathBuf) {
        run_wake::bind_run(&self.runtime, &self.record.run_id).unwrap()
    }

    fn complete(&mut self, message: &str) {
        self.record.status = RunStatus::Completed;
        self.record.last_message = Some(message.to_owned());
        rimz::store::run_store::write(&self.paths.runs_dir, &self.record).unwrap();
    }
}

#[test]
fn blocking_stream_wakeup_reloads_terminal_record() {
    let mut fixture = RunFixture::new(RunStatus::Running);
    let run_id = fixture.run_id();
    let (sock, sock_path) = fixture.bind();

    fixture.complete("done");
    send_run_frame(
        &sock_path,
        &WakeupFrame::RunCompleted {
            workspace_id: fixture.workspace_id.clone(),
            run_id: run_id.clone(),
            status: RunStatus::Completed,
        },
    );

    let loaded = stream_blocking_run(
        sock,
        fixture.expected(),
        &fixture.store,
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
    let fixture = RunFixture::new(RunStatus::Running);
    let run_id = fixture.run_id();
    let (sock, _sock_path) = fixture.bind();

    let timed_out = stream_blocking_run(
        sock,
        fixture.expected(),
        &fixture.store,
        &run_id,
        &rimz::agents::CodexAdapter,
        Some(Duration::ZERO),
    )
    .unwrap();

    assert_eq!(timed_out.status, RunStatus::TimedOut);
    assert_eq!(
        rimz::harness::run::load(&fixture.paths, &run_id)
            .unwrap()
            .status,
        RunStatus::TimedOut
    );
}

#[test]
fn attached_stream_timeout_does_not_mark_run_timed_out() {
    let fixture = RunFixture::new(RunStatus::Running);
    let run_id = fixture.run_id();

    let outcome = stream_attached_run(
        &fixture.store,
        &run_id,
        &rimz::agents::CodexAdapter,
        false,
        Some(Duration::ZERO),
    )
    .unwrap();

    assert_eq!(outcome, None);
    assert_eq!(
        rimz::harness::run::load(&fixture.paths, &run_id)
            .unwrap()
            .status,
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
    let mut record = run_record("codex");
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
    let mut fixture = RunFixture::new(RunStatus::Running);
    let run_id = fixture.run_id();
    let (sock, sock_path) = fixture.bind();

    fixture.complete("done");
    let frame = WakeupFrame::RunCompleted {
        workspace_id: fixture.workspace_id.clone(),
        run_id: run_id.clone(),
        status: RunStatus::Completed,
    };
    send_run_frame(&sock_path, &frame);

    let outcome = wait_for_run(sock, fixture.expected(), Some(Duration::from_secs(1))).unwrap();
    let loaded = terminal_record_after_wait(&fixture.paths, &run_id, outcome).unwrap();

    assert_eq!(loaded.status, RunStatus::Completed);
    assert_eq!(loaded.last_message.as_deref(), Some("done"));
    assert_eq!(loaded.status.exit_code(), 0);
}

#[test]
fn neutral_run_wait_marks_timeout() {
    let fixture = RunFixture::new(RunStatus::Running);
    let run_id = fixture.run_id();
    let (sock, _sock_path) = fixture.bind();

    let outcome = wait_for_run(sock, fixture.expected(), Some(Duration::from_millis(10))).unwrap();
    let timed_out = terminal_record_after_wait(&fixture.paths, &run_id, outcome).unwrap();

    assert_eq!(timed_out.status, RunStatus::TimedOut);
    assert_eq!(timed_out.status.exit_code(), 124);
}

fn run_record(kind: &str) -> RunRecord {
    RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
        AgentKind::new_unchecked(kind),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    )
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
        status,
        ..rimz::testkit::agent_state(kind, id, jiff::Timestamp::UNIX_EPOCH)
    }
}
