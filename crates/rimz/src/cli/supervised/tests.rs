use super::*;
use rimz::agents::PermissionMode;
use rimz::agents::{AgentState, AgentStatus, LaunchParams};
use rimz::harness::run::{RunCancellation, RunStatus, SupervisedRunRequest};
use rimz::harness::run_wake::{self, ExpectedRunFrame, WakeupFrame};
use rimz::harness::spec::Cell;
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
fn supervised_run_placement_matrix() {
    use super::run::{RunPlacement, run_placement};

    for (force_new_tab, has_ambient_pane, loop_zone, subagent, expected) in [
        (false, true, false, false, RunPlacement::Split),
        (false, true, false, true, RunPlacement::SubagentZone),
        (true, true, false, true, RunPlacement::Tab),
        (false, false, false, true, RunPlacement::Tab),
        (false, true, true, false, RunPlacement::LoopZone),
        (false, false, true, false, RunPlacement::LoopZone),
        (false, true, true, true, RunPlacement::LoopZone),
        (true, true, true, true, RunPlacement::Tab),
    ] {
        assert_eq!(
            run_placement(force_new_tab, has_ambient_pane, loop_zone, subagent),
            expected,
            "force_new_tab={force_new_tab}, has_ambient_pane={has_ambient_pane}, loop_zone={loop_zone}, subagent={subagent}"
        );
    }
}

#[test]
fn subagent_zone_strategy_uses_solo_column_and_team_companion_tab() {
    use super::pane::{SubagentSplitFallback, SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = rimz::config::ThemeConfig::default();
    let mut solo = agent_state("codex", "solo", AgentStatus::Running);
    solo.pane = Some(pane_ref("%1", "work"));
    let live = vec![solo.pane.clone().unwrap()];
    assert_eq!(
        select_subagent_zone_strategy(std::slice::from_ref(&solo), &live, &solo, "room", &theme),
        Some(SubagentZoneStrategy::Split {
            session_name: "room".to_owned(),
            pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
            placement: rimz::mux::SplitPlacement::Directional(rimz::mux::SplitDirection::Right,),
            on_failure: SubagentSplitFallback::CompanionTab,
        })
    );

    let mut planner = agent_state("claude", "planner", AgentStatus::Running);
    planner.team = Some("forge".to_owned());
    planner.channel = Some("design".to_owned());
    let glyph = rimz::theme::theme_glyphs(&theme)(rimz::config::GlyphRole::StatusWorking);
    planner.pane = Some(pane_ref("%2", &format!("design {glyph}")));
    assert_eq!(
        select_subagent_zone_strategy(
            std::slice::from_ref(&planner),
            std::slice::from_ref(planner.pane.as_ref().unwrap()),
            &planner,
            "room",
            &theme,
        ),
        Some(SubagentZoneStrategy::CompanionTab {
            title: "design subagents".to_owned(),
        })
    );
}

#[test]
fn subagent_zone_strategy_anchors_to_newest_child_across_team() {
    use super::pane::{SubagentSplitFallback, SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = rimz::config::ThemeConfig::default();
    let mut planner = agent_state("claude", "planner", AgentStatus::Running);
    planner.team = Some("forge".to_owned());
    planner.channel = Some("design".to_owned());
    planner.pane = Some(pane_ref("%1", "design"));
    let mut coder = agent_state("codex", "coder", AgentStatus::Running);
    coder.team = Some("forge".to_owned());
    coder.channel = Some("design".to_owned());
    coder.pane = Some(pane_ref("%2", "design"));
    let mut older = launched_child("child-old", &planner, "%3", 10);
    older.pane.as_mut().unwrap().session_name = "room".to_owned();
    let newer = launched_child("child-new", &coder, "%4", 20);
    let live = vec![
        planner.pane.clone().unwrap(),
        older.pane.clone().unwrap(),
        newer.pane.clone().unwrap(),
    ];
    let agents = vec![planner.clone(), coder, older, newer];

    assert_eq!(
        select_subagent_zone_strategy(&agents, &live, &planner, "room", &theme),
        Some(SubagentZoneStrategy::Split {
            session_name: "room".to_owned(),
            pane_id: PaneId::from_parts(MuxName::Tmux, "%4"),
            placement: rimz::mux::SplitPlacement::Stacked,
            on_failure: SubagentSplitFallback::RunTab,
        })
    );
}

#[test]
fn subagent_zone_strategy_uses_live_ended_child_and_skips_dead_newer_child() {
    use super::pane::{SubagentSplitFallback, SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = rimz::config::ThemeConfig::default();
    let mut parent = agent_state("codex", "parent", AgentStatus::Running);
    parent.pane = Some(pane_ref("%1", "work"));
    let mut kept = launched_child("kept", &parent, "%2", 10);
    kept.ended_at = Some(jiff::Timestamp::from_second(30).unwrap());
    let dead = launched_child("dead", &parent, "%3", 20);
    let live = vec![parent.pane.clone().unwrap(), kept.pane.clone().unwrap()];
    let agents = vec![parent.clone(), kept, dead];

    assert_eq!(
        select_subagent_zone_strategy(&agents, &live, &parent, "room", &theme),
        Some(SubagentZoneStrategy::Split {
            session_name: "room".to_owned(),
            pane_id: PaneId::from_parts(MuxName::Tmux, "%2"),
            placement: rimz::mux::SplitPlacement::Stacked,
            on_failure: SubagentSplitFallback::CompanionTab,
        })
    );
}

#[test]
fn subagent_zone_strategy_reuses_unbound_team_companion_view() {
    use super::pane::{SubagentSplitFallback, SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = rimz::config::ThemeConfig::default();
    let glyph = rimz::theme::theme_glyphs(&theme)(rimz::config::GlyphRole::StatusWorking);
    let mut planner = agent_state("claude", "planner", AgentStatus::Running);
    planner.team = Some("forge".to_owned());
    planner.pane = Some(pane_ref("%1", "design"));
    let mut unbound_child = pane_ref("%2", &format!("design subagents {glyph}"));
    unbound_child.session_name = "room".to_owned();
    let mut sidebar = pane_ref("%3", "design subagents");
    sidebar.session_name = "room".to_owned();
    sidebar.command = Some(rimz::pane::SIDEBAR_CHROME_TITLE.to_owned());

    assert_eq!(
        select_subagent_zone_strategy(
            std::slice::from_ref(&planner),
            &[planner.pane.clone().unwrap(), unbound_child],
            &planner,
            "room",
            &theme,
        ),
        Some(SubagentZoneStrategy::Split {
            session_name: "room".to_owned(),
            pane_id: PaneId::from_parts(MuxName::Tmux, "%2"),
            placement: rimz::mux::SplitPlacement::Stacked,
            on_failure: SubagentSplitFallback::RunTab,
        })
    );
    assert_eq!(
        select_subagent_zone_strategy(
            std::slice::from_ref(&planner),
            &[planner.pane.clone().unwrap(), sidebar],
            &planner,
            "room",
            &theme,
        ),
        Some(SubagentZoneStrategy::Split {
            session_name: "room".to_owned(),
            pane_id: PaneId::from_parts(MuxName::Tmux, "%3"),
            placement: rimz::mux::SplitPlacement::Directional(rimz::mux::SplitDirection::Right,),
            on_failure: SubagentSplitFallback::RunTab,
        })
    );
}

#[test]
fn subagent_launch_waits_for_wrapper_pane_bind_but_caps_the_wait() {
    let mut probes = 0;
    assert!(super::pane::wait_for_subagent_pane_bind_with(
        || {
            probes += 1;
            probes == 3
        },
        Duration::from_secs(1),
        Duration::ZERO,
    ));
    assert_eq!(probes, 3);
    assert!(!super::pane::wait_for_subagent_pane_bind_with(
        || false,
        Duration::ZERO,
        Duration::ZERO,
    ));
}

fn supervised_request(prompt: &str, subagent: bool) -> SupervisedRunRequest {
    SupervisedRunRequest {
        spec: "codex".to_owned(),
        prompt: prompt.to_owned(),
        description: None,
        worktree: None,
        from_pr: None,
        channel: None,
        name: None,
        background: false,
        self_cleanup_on_completion: false,
        subagent,
        force_new_tab: false,
        permission_mode: PermissionMode::Auto,
        agent: None,
        model: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        effort: None,
        budget: None,
        max_turns: None,
        timeout: None,
        keep: false,
        retries: 0,
        verify: None,
        max_attempts: None,
        loop_zone: false,
        loop_task: None,
        passthrough: Vec::new(),
        managed_launch: rimz::agents::ManagedLaunchState::PendingResolution,
    }
}

#[test]
fn supervised_launch_normalizes_model_and_effort_overrides() {
    let mut request = supervised_request("fix-it", false);
    request.model = Some(" gpt-5 ".to_owned());
    request.effort = Some(" low ".to_owned());
    let dir = tempfile::tempdir().expect("temp dir");
    let workspace =
        rimz::workspace::WorkspaceResolver::resolve(dir.path(), None).expect("resolve workspace");

    let prepared = super::run::prepare_supervised_launch_layout(
        &request,
        &request.spec,
        &workspace,
        &rimz::config::MachineConfig::default(),
        rimz::config::effective::ProfileScope::Agents,
        None,
    )
    .expect("prepare supervised launch")
    .layout;
    let [
        Cell::Agent(rimz::harness::spec::AgentCell {
            launch: LaunchParams { model, effort, .. },
            ..
        }),
    ] = prepared.columns[0].rows.as_slice()
    else {
        panic!("one agent")
    };
    assert_eq!(model.as_deref(), Some("gpt-5"));
    assert_eq!(effort.as_deref(), Some("low"));
}

#[test]
fn general_inheritance_is_a_preset_below_explicit_overrides() {
    let mut request = supervised_request("fix-it", true);
    request.effort = Some("low".to_owned());
    let dir = tempfile::tempdir().expect("temp dir");
    let workspace =
        rimz::workspace::WorkspaceResolver::resolve(dir.path(), None).expect("resolve workspace");
    let inherited = rimz::agents::LaunchPreset {
        model: Some("opus".to_owned()),
        effort: Some("high".to_owned()),
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
    };

    let prepared = super::run::prepare_supervised_launch_layout(
        &request,
        "claude",
        &workspace,
        &rimz::config::MachineConfig::default(),
        rimz::config::effective::ProfileScope::Subagents,
        Some(&inherited),
    )
    .expect("prepare inherited launch")
    .layout;
    let cell = prepared.agent_cells().next().expect("agent cell");

    assert_eq!(cell.launch.model.as_deref(), Some("opus"));
    assert_eq!(cell.launch.effort.as_deref(), Some("low"));
}

#[test]
fn unsupported_adapter_keeps_subagent_reminder_in_user_prompt() {
    let request = supervised_request("amp", true);
    let dir = tempfile::tempdir().expect("temp dir");
    let workspace =
        rimz::workspace::WorkspaceResolver::resolve(dir.path(), None).expect("resolve workspace");

    let err = super::run::prepare_supervised_launch_layout(
        &request,
        "claude",
        &workspace,
        &rimz::config::MachineConfig::default(),
        rimz::config::effective::ProfileScope::Subagents,
        None,
    )
    .expect_err("spec-like prompt");
    assert!(
        err.to_string()
            .contains("prompt `amp` looks like another spec cell"),
        "{err:#}"
    );

    let adapter = rimz::agents::find_definition("amp").unwrap();
    let prompt = super::run::supervised_prompt(&request, adapter);
    assert!(prompt.starts_with("amp\n\n<system_reminder>\n"));
    assert!(prompt.contains("must not spawn agents or subagents"));
    assert!(prompt.ends_with("\n</system_reminder>"));

    let ordinary = supervised_request("amp", false);
    assert_eq!(super::run::supervised_prompt(&ordinary, adapter), "amp");
}

#[test]
fn native_system_text_adapters_keep_subagent_reminder_out_of_user_prompt() {
    for kind in ["claude", "codex"] {
        let request = supervised_request(kind, true);
        let adapter = rimz::agents::find_definition(kind).unwrap();

        assert_eq!(super::run::supervised_prompt(&request, adapter), kind);
    }
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
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
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

#[test]
fn subagent_run_holds_its_pane_after_terminal_completion() {
    assert_eq!(run_exit_policy(true, true), (false, true));
    assert_eq!(run_exit_policy(true, false), (true, true));
    assert_eq!(run_exit_policy(false, true), (false, false));

    let run_id = rimz::RunId::new();
    let launch = rimz::agents::LaunchParams::default();
    let launch_id = rimz::ids::AgentSessionId::from("child-id");
    let pane = run_pane_cmd(RunPaneCmdArgs {
        adapter: rimz::agents::definition_by_kind("codex").unwrap(),
        run_id: &run_id,
        agent_name: Some("child"),
        agent_name_explicit: true,
        launch: &launch,
        launch_id: Some(&launch_id),
        cwd: Path::new("/tmp"),
        prompt: "work",
        cleanup_worktree: false,
        permission_args: &[],
        system_prompt_file: None,
        append_system_prompt_files: &[],
        self_cleanup_on_completion: true,
        subagent: true,
        provider_account_binding: None,
    })
    .unwrap();
    let request = rimz::harness::launch::decode_exec_request(
        "codex",
        None,
        pane.argv.last().expect("exec payload"),
    )
    .unwrap();
    assert!(!request.close_pane_on_exit);
    assert!(request.exit_on_run_completion);
    assert!(request.subagent);
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
        rimz::harness::run::create(&self.paths, &record).unwrap();
    }
}

#[test]
fn subagent_zone_lock_serializes_workspace_launches() {
    let fixture = RunFixture::new(RunStatus::Running);
    let lock_path = fixture.paths.locks_dir.join("subagent-zone.lock");

    let held = super::pane::lock_subagent_zone(&fixture.store).unwrap();
    assert!(
        rimz::store::lock::WorkspaceLock::try_acquire(&lock_path)
            .unwrap()
            .is_none()
    );
    drop(held);
    assert!(
        rimz::store::lock::WorkspaceLock::try_acquire(&lock_path)
            .unwrap()
            .is_some()
    );
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
        rimz::agents::definition_by_kind("codex").unwrap(),
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
        rimz::agents::definition_by_kind("codex").unwrap(),
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
        rimz::agents::definition_by_kind("codex").unwrap(),
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
        rimz::agents::definition_by_kind("codex").unwrap(),
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
        rimz::agents::definition_by_kind("codex").unwrap(),
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

fn pane_ref(id: &str, view_name: &str) -> PaneRef {
    let mut pane = PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, id));
    pane.view_name = Some(view_name.to_owned());
    pane
}

fn launched_child(id: &str, parent: &AgentState, pane_id: &str, registered_at: i64) -> AgentState {
    let mut child = agent_state("codex", id, AgentStatus::Running);
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.launch_depth = Some(1);
    child.pane = Some(pane_ref(pane_id, "subagents"));
    child.registered_at = Some(jiff::Timestamp::from_second(registered_at).unwrap());
    child
}
