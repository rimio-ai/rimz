use super::*;
use rimz::agents::{AgentState, AgentStatus};
use rimz::harness::run::{PermissionMode, RunCancellation, RunStatus};
use rimz::harness::run_wake::{self, ExpectedRunFrame, WakeupFrame};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
use rimz::pane::PaneRef;
use rimz::store::{RuntimePaths, StatePaths};
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

    fn waiter(&self, cancellation: RunCancellation) -> run_wake::RunWaiter {
        run_wake::RunWaiter::bind(&self.runtime, self.expected(), cancellation).unwrap()
    }

    fn complete(&self, message: &str) {
        let mut record = self.record.clone();
        record.status = RunStatus::Completed;
        record.last_message = Some(message.to_owned());
        rimz::store::run_store::write(&self.paths.runs_dir, &record).unwrap();
    }
}

#[test]
fn blocking_stream_wakeup_reloads_terminal_record() {
    let fixture = RunFixture::new(RunStatus::Running);
    let run_id = fixture.run_id();
    let waiter = fixture.waiter(RunCancellation::new());
    let sock_path = waiter.socket_path().to_path_buf();

    fixture.complete("done");
    send_run_frame(
        &sock_path,
        &WakeupFrame::RunCompleted {
            workspace_id: fixture.workspace_id.clone(),
            run_id: run_id.clone(),
            status: RunStatus::Completed,
        },
    );
    let mut cursor = rimz::agents::transcript::TranscriptCursor::new(true);
    let mut out = Vec::new();
    let mut sink = output::StreamSink::ndjson(&mut out);

    let loaded = stream_blocking_run(
        &waiter,
        &fixture.store,
        &rimz::agents::CodexAdapter,
        Some(Duration::from_secs(1)),
        (&mut cursor, &mut sink),
    )
    .unwrap();

    assert_eq!(loaded.status, RunStatus::Completed);
    assert_eq!(loaded.last_message.as_deref(), Some("done"));
}

#[test]
fn blocking_stream_timeout_marks_run_timed_out() {
    let fixture = RunFixture::new(RunStatus::Running);
    let run_id = fixture.run_id();
    let waiter = fixture.waiter(RunCancellation::new());
    let mut cursor = rimz::agents::transcript::TranscriptCursor::new(true);
    let mut out = Vec::new();
    let mut sink = output::StreamSink::ndjson(&mut out);

    let timed_out = stream_blocking_run(
        &waiter,
        &fixture.store,
        &rimz::agents::CodexAdapter,
        Some(Duration::ZERO),
        (&mut cursor, &mut sink),
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
fn blocking_text_stream_leaves_forensics_to_its_caller() {
    let fixture = RunFixture::new(RunStatus::Failed);
    let waiter = fixture.waiter(RunCancellation::new());
    let mut cursor = rimz::agents::transcript::TranscriptCursor::new(true);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut sink = output::StreamSink::text(&mut out, &mut err);

    let failed = stream_blocking_run(
        &waiter,
        &fixture.store,
        &rimz::agents::CodexAdapter,
        Some(Duration::from_secs(1)),
        (&mut cursor, &mut sink),
    )
    .unwrap();

    assert_eq!(failed.status, RunStatus::Failed);
    assert!(err.is_empty());
}

#[test]
fn attached_stream_timeout_does_not_mark_run_timed_out() {
    let fixture = RunFixture::new(RunStatus::Running);
    let run_id = fixture.run_id();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut sink = output::StreamSink::text(&mut out, &mut err);

    let outcome = stream_attached_run(
        &fixture.store,
        &run_id,
        &rimz::agents::CodexAdapter,
        false,
        Some(Duration::ZERO),
        &mut sink,
    )
    .unwrap();

    assert_eq!(outcome, None);
    assert_eq!(
        rimz::harness::run::load(&fixture.paths, &run_id)
            .unwrap()
            .status,
        RunStatus::Running
    );
    assert!(String::from_utf8(err).unwrap().contains("wait timed out"));
}

#[test]
fn blocking_stream_interrupt_marks_run_canceled() {
    let fixture = RunFixture::new(RunStatus::Running);
    let run_id = fixture.run_id();
    let cancellation = RunCancellation::new();
    cancellation.request();
    let waiter = fixture.waiter(cancellation);
    let mut cursor = rimz::agents::transcript::TranscriptCursor::new(true);
    let mut out = Vec::new();
    let mut sink = output::StreamSink::ndjson(&mut out);

    let canceled = stream_blocking_run(
        &waiter,
        &fixture.store,
        &rimz::agents::CodexAdapter,
        Some(Duration::from_secs(1)),
        (&mut cursor, &mut sink),
    )
    .unwrap();

    assert_eq!(canceled.status, RunStatus::Canceled);
    assert_eq!(
        rimz::harness::run::load(&fixture.paths, &run_id)
            .unwrap()
            .status,
        RunStatus::Canceled
    );
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
