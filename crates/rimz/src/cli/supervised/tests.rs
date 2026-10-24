use super::*;
use rimz as rimz_api;
use rimz::agents::PermissionMode;
use rimz::agents::{AgentState, AgentStatus, LaunchParams};
use rimz::disk::paths::{RuntimePaths, StatePaths};
use rimz::harness::run::RunCancellation;
use rimz::harness::run_wake::{self, ExpectedRunFrame};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
use rimz::pane::PaneRef;
use rimz::store::run::{ReportTo, RunStatus, WakeupFrame};
use std::path::Path;
use tokio::net::UnixDatagram;
#[path = "../../launcher/placement/test_backend.rs"]
mod zone_backend;

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
fn failure_tail_keeps_scrollback_for_collapsed_members() {
    let backend = zone_backend::Backend::default();
    let pane = PaneId::parse("tmux:%1").unwrap();
    assert_eq!(
        super::pane::capture_failure_tail(&backend, &pane, "room").as_deref(),
        Some("earlier-line\nlast-line"),
    );
    assert_eq!(
        backend.captures.lock().unwrap().as_slice(),
        &[(Some(60), false)]
    );
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
fn stop_backstop_does_not_close_a_pre_rebirth_pane() {
    let mut fixture = RunFixture::new(RunStatus::Canceled);
    fixture.record.workflow = Some(rimz::agents::WorkflowAttemptRef {
        run: rimz::ids::WorkflowRunId::new(),
        seq: 1,
        role: rimz::agents::WorkflowAttemptRole::Step,
    });
    rimz::harness::run::create(&fixture.paths, &fixture.record).unwrap();
    rimz::harness::run::record_pane(
        fixture.store.paths(),
        &fixture.record.run_id,
        PaneId::from_parts(MuxName::Zellij, "terminal_8"),
    )
    .unwrap();
    fixture
        .store
        .append_event(&rimz::EventEnvelope::session_rebirth(
            fixture.record.workspace_id.clone(),
            "rimz-test",
        ))
        .unwrap();
    super::pane::settle_stopped_run_pane(
        &fixture.store,
        "rimz-test",
        &fixture.record,
        std::time::Duration::ZERO,
        |_| Ok(true),
        |_| panic!("a reused pane in the new session must not be closed"),
    )
    .unwrap();
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

/// The backstop's verdict is a listing or a close: each scripted listing
/// answers in turn (the last answer repeats).
#[test]
fn stop_backstop_reports_a_pane_it_could_not_confirm_closed() {
    use super::pane::settle_stopped_run_pane;
    #[derive(Clone, Copy)]
    enum Listing {
        Present,
        Absent,
        Unreadable,
        SessionGone,
    }
    use Listing::{Absent, Present, SessionGone, Unreadable};
    let settle_within = |grace: Duration, listings: &[Listing], close_ok: bool, recorded: bool| {
        let fixture = RunFixture::new(RunStatus::Canceled);
        if recorded {
            rimz::harness::run::record_pane(
                fixture.store.paths(),
                &fixture.record.run_id,
                PaneId::from_parts(MuxName::Tmux, "%8"),
            )
            .unwrap();
        }
        let mut listings = listings.iter().copied();
        let mut last = None;
        let mut closes = 0;
        let mut listed = 0;
        let verdict = settle_stopped_run_pane(
            &fixture.store,
            "rimz-test",
            &fixture.record,
            grace,
            |pane| {
                listed += 1;
                last = listings.next().or(last);
                match last.expect("a scripted listing") {
                    Present => Ok(true),
                    Absent => Ok(false),
                    Unreadable => Err(rimz::mux::MuxErr::Output {
                        program: "tmux".to_owned(),
                        reason: "unreadable".to_owned(),
                    }),
                    SessionGone => Err(rimz::mux::MuxErr::SessionNotFound {
                        session: pane.session_name.clone(),
                    }),
                }
            },
            |_| {
                closes += 1;
                if close_ok {
                    Ok(())
                } else {
                    Err(rimz::mux::MuxErr::Output {
                        program: "tmux".to_owned(),
                        reason: "close refused".to_owned(),
                    })
                }
            },
        );
        let verdict = verdict.map_err(|open| StopRunErr::PaneOpen(open).to_string());
        (verdict, closes, listed)
    };
    let settle = |listings: &[Listing], close_ok: bool, recorded: bool| {
        let (verdict, closes, _) = settle_within(Duration::ZERO, listings, close_ok, recorded);
        (verdict, closes)
    };
    let open = Err(
        "pane tmux:%8 is still open: could not parse mux output from `tmux`: close refused; rerun the stop to close it"
            .to_owned(),
    );

    assert_eq!(settle(&[Absent], false, true), (Ok(()), 0));
    assert_eq!(settle(&[Present], false, true), (open.clone(), 1));
    assert_eq!(settle(&[Present, Absent], false, true), (Ok(()), 1));
    assert_eq!(settle(&[Unreadable], true, true), (Ok(()), 1));
    assert_eq!(settle(&[Unreadable], false, true), (open, 1));
    assert_eq!(
        settle(&[SessionGone], false, true),
        (Ok(()), 0),
        "a session that is gone holds no pane"
    );
    assert_eq!(
        settle(&[Present, SessionGone], false, true),
        (Ok(()), 1),
        "a session gone by the re-list took the pane with it"
    );
    assert_eq!(
        settle(&[], false, false),
        (Ok(()), 0),
        "a run with no pane rimz can name has nothing to close"
    );
    assert_eq!(
        settle_within(Duration::from_secs(1), &[Unreadable, Absent], false, true),
        (Ok(()), 0, 2),
        "a listing error before the deadline is retried, not returned"
    );

    let unresolved = super::pane::PaneOpen {
        pane: None,
        reason: "no multiplexer found".to_owned(),
    };
    assert_eq!(
        StopRunErr::PaneOpen(unresolved).to_string(),
        "the run's pane was not closed: no multiplexer found; rerun the stop to close it"
    );
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

#[test]
fn stopped_subagent_end_is_recorded_once() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};

    let mut fixture = RunFixture::new(RunStatus::Canceled);
    fixture.record.subagent = true;
    fixture.record.agent_id = Some("launch_child".into());
    fixture.record.agent_name = Some("otter".to_owned());
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%3");
    fixture
        .store
        .append_event(&rimz::EventEnvelope::agent_launched(
            fixture.workspace_id.clone(),
            "room",
            &fixture.record.kind,
            rimz::store::event::AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: "launch_child".into(),
                launch_id: None,
                agent_name: "otter".to_owned(),
                agent_name_explicit: false,
                launch: LaunchParams {
                    parent_agent_id: Some("parent".into()),
                    parent_agent_kind: Some(AgentKind::new_unchecked("codex")),
                    launch_depth: Some(1),
                    ..Default::default()
                },
                state: rimz::store::event::AgentLaunchState::Starting,
                run_id: Some(fixture.run_id()),
                pane_id: Some(pane_id.clone()),
                runtime_owner: None,
                worktree_path: None,
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    let mut observation =
        AgentLifecycleObservation::new(Some("child".into()), LifecycleSignal::Registered);
    observation.pane_id = Some(pane_id);
    fixture
        .store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            fixture.workspace_id.clone(),
            "room",
            "codex",
            "SessionStart",
            &observation,
        ))
        .unwrap();
    let registered = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert_eq!(registered.len(), 1, "{registered:?}");
    assert_eq!(registered[0].agent_id, "child");
    assert_eq!(registered[0].name.as_deref(), Some("otter"));
    assert!(fixture.record.matches_agent(&registered[0]));
    fixture.record.completed_at = registered[0].registered_at;

    stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();
    stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();

    let agents = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert!(agents[0].ended_at.is_some(), "{agents:?}");
    let events = fixture.store.read_events().unwrap();
    let ends = events
        .iter()
        .filter_map(|event| match event.kind() {
            rimz::store::event::EventKind::AgentLifecycle(payload)
                if payload.observation.signal == LifecycleSignal::Ended =>
            {
                Some(payload)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ends.len(), 1, "{events:?}");
    assert_eq!(ends[0].event_name.as_deref(), Some("rimz.subagent-stopped"));
    assert_eq!(ends[0].observation.agent_id.as_ref().unwrap(), "child");
}

#[test]
fn stopped_subagent_end_skips_root_ended_and_unmatched_rows() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};

    for case in ["root", "ended", "unmatched"] {
        let mut fixture = RunFixture::new(RunStatus::Canceled);
        fixture.record.subagent = case != "root";
        fixture.record.agent_id = Some(
            if case == "unmatched" {
                "missing"
            } else {
                "child"
            }
            .into(),
        );
        fixture
            .store
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                fixture.workspace_id.clone(),
                "room",
                "codex",
                "SessionStart",
                &AgentLifecycleObservation::new(
                    Some("child".into()),
                    if case == "ended" {
                        LifecycleSignal::Ended
                    } else {
                        LifecycleSignal::Registered
                    },
                ),
            ))
            .unwrap();
        let before = fixture.store.read_events().unwrap();
        stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();
        assert_eq!(fixture.store.read_events().unwrap(), before, "{case}");
    }
}

#[test]
fn stopped_subagent_end_prefers_the_run_id_row_over_a_newer_namesake() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};

    let mut fixture = RunFixture::new(RunStatus::Completed);
    fixture.record.subagent = true;
    fixture.record.agent_id = Some("older".into());
    fixture.record.agent_name = Some("otter".to_owned());
    let registered_at = jiff::Timestamp::now();
    for (id, name, age) in [("older", "previous", 2), ("newer", "otter", 1)] {
        let mut observation =
            AgentLifecycleObservation::new(Some(id.into()), LifecycleSignal::Registered);
        observation.agent_name = Some(name.to_owned());
        let mut event = rimz::EventEnvelope::agent_lifecycle(
            fixture.workspace_id.clone(),
            "room",
            "codex",
            "SessionStart",
            &observation,
        );
        event.timestamp = registered_at - jiff::SignedDuration::from_secs(age);
        fixture.store.append_event(&event).unwrap();
    }
    let before = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert!(
        before.iter().all(|agent| agent.ended_at.is_none()),
        "{before:?}"
    );

    stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();
    let agents = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert!(
        agents
            .iter()
            .find(|agent| agent.agent_id == "older")
            .unwrap()
            .ended_at
            .is_some(),
        "{agents:?}"
    );
    assert!(
        agents
            .iter()
            .find(|agent| agent.agent_id == "newer")
            .unwrap()
            .ended_at
            .is_none(),
        "{agents:?}"
    );
}

fn register_stopped_subagent_card(
    fixture: &RunFixture,
    id: &str,
    signal: rimz::agents::LifecycleSignal,
    timestamp: jiff::Timestamp,
) {
    let mut observation = rimz::agents::AgentLifecycleObservation::new(Some(id.into()), signal);
    observation.agent_name = Some("otter".to_owned());
    observation.parent_agent_id = Some("parent".into());
    observation.launch.launch_depth = Some(1);
    let mut event = rimz::EventEnvelope::agent_lifecycle(
        fixture.workspace_id.clone(),
        "room",
        "codex",
        "SessionStart",
        &observation,
    );
    event.timestamp = timestamp;
    fixture.store.append_event(&event).unwrap();
}

#[test]
fn stopped_subagent_end_skips_later_namesake_when_own_card_is_absent() {
    let mut fixture = RunFixture::new(RunStatus::Completed);
    fixture.record.subagent = true;
    fixture.record.agent_id = Some("old".into());
    fixture.record.agent_name = Some("otter".to_owned());
    register_stopped_subagent_card(
        &fixture,
        "new",
        rimz::agents::LifecycleSignal::Registered,
        fixture.record.completed_at.unwrap() + jiff::SignedDuration::from_secs(1),
    );
    let before = fixture.store.read_events().unwrap();

    stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();

    let agents = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    let namesake = agents.iter().find(|agent| agent.agent_id == "new").unwrap();
    assert!(
        namesake.ended_at.is_none(),
        "live namesake was ended: {agents:?}"
    );
    assert_eq!(fixture.store.read_events().unwrap(), before);
}

#[test]
fn stopped_subagent_end_skips_later_namesake_when_own_card_is_ended() {
    use rimz::agents::LifecycleSignal;

    let mut fixture = RunFixture::new(RunStatus::Completed);
    fixture.record.subagent = true;
    fixture.record.agent_id = Some("old".into());
    fixture.record.agent_name = Some("otter".to_owned());
    let completed_at = fixture.record.completed_at.unwrap();
    for (id, signal, offset) in [
        ("old", LifecycleSignal::Registered, -2),
        ("old", LifecycleSignal::Ended, -1),
        ("new", LifecycleSignal::Registered, 1),
    ] {
        register_stopped_subagent_card(
            &fixture,
            id,
            signal,
            completed_at + jiff::SignedDuration::from_secs(offset),
        );
    }
    let before = fixture.store.read_events().unwrap();
    let registered = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert_eq!(registered.len(), 2, "{registered:?}");
    let old = registered
        .iter()
        .find(|agent| agent.agent_id == "old")
        .unwrap();
    assert!(old.ended_at.is_some(), "{registered:?}");
    let new = registered
        .iter()
        .find(|agent| agent.agent_id == "new")
        .unwrap();
    assert_eq!(
        new.registered_at,
        Some(completed_at + jiff::SignedDuration::from_secs(1)),
        "{registered:?}"
    );

    stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();

    let agents = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    let namesake = agents.iter().find(|agent| agent.agent_id == "new").unwrap();
    assert!(
        namesake.ended_at.is_none(),
        "live namesake was ended: {agents:?}"
    );
    assert_eq!(fixture.store.read_events().unwrap(), before);
}

#[test]
fn stopped_subagent_end_prefers_the_newest_name_match_before_completion() {
    let mut fixture = RunFixture::new(RunStatus::Completed);
    fixture.record.subagent = true;
    fixture.record.agent_name = Some("otter".to_owned());
    for (id, age) in [("older", 2), ("newest", 0), ("middle", 1)] {
        register_stopped_subagent_card(
            &fixture,
            id,
            rimz::agents::LifecycleSignal::Registered,
            fixture.record.completed_at.unwrap() - jiff::SignedDuration::from_secs(age),
        );
    }

    stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();

    let agents = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert_eq!(agents.len(), 3, "{agents:?}");
    for agent in &agents {
        assert_eq!(
            agent.ended_at.is_some(),
            agent.agent_id == "newest",
            "{agents:?}"
        );
    }
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
        record.completed_at = status.is_terminal().then_some(record.started_at);
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
fn background_receipt_names_the_report_or_the_wait_command() {
    let receipt_for = |names: &[&str], path: Option<&str>, subagent: bool, report_to| {
        let mut err = Vec::new();
        super::output::write_background_receipt(
            &mut err,
            names,
            path.map(Path::new),
            subagent,
            report_to,
        )
        .unwrap();
        String::from_utf8(err).unwrap()
    };
    let path = Some("/state/out/planner/calm-fox.output");
    let one = ["calm-fox"].as_slice();
    let several = ["calm-fox", "bold-owl", "shy-elk"].as_slice();

    for (subagent, report, settled, noun, wait) in [
        (
            true,
            "SUBAGENT_REPORT",
            "every subagent you launched",
            "subagent",
            "rimz subagents wait",
        ),
        (
            false,
            "AGENT_REPORT",
            "every agent you launched",
            "agent",
            "rimz agents wait",
        ),
    ] {
        assert_eq!(
            receipt_for(one, path, subagent, ReportTo::Launcher),
            format!(
                "Running in the background. Keep working or end your turn: one {report} reaches you once {settled} has settled.\nIts response lands at /state/out/planner/calm-fox.output when it settles. To block instead: {wait} calm-fox\n"
            )
        );
        assert_eq!(
            receipt_for(several, path, subagent, ReportTo::Launcher),
            format!(
                "Running in the background. Keep working or end your turn: one {report} reaches you once {settled} has settled.\nEach response lands at /state/out/planner/<name>.output when its {noun} settles. To block instead: {wait} calm-fox bold-owl shy-elk\n"
            )
        );
        assert_eq!(
            receipt_for(one, path, subagent, ReportTo::Nobody),
            format!(
                "Running detached: no {report} will reach you.\nIts response lands at /state/out/planner/calm-fox.output when it settles. To collect it: {wait} calm-fox\n"
            )
        );
        assert_eq!(
            receipt_for(several, path, subagent, ReportTo::Nobody),
            format!(
                "Running detached: no {report} will reach you.\nEach response lands at /state/out/planner/<name>.output when its {noun} settles. To collect them: {wait} calm-fox bold-owl shy-elk\n"
            )
        );
    }

    assert_eq!(
        receipt_for(one, None, false, ReportTo::Launcher),
        "Running in the background. To print its final response: rimz agents wait calm-fox\n"
    );
    assert_eq!(
        receipt_for(several, None, false, ReportTo::Launcher),
        "Running in the background. To print their final responses: rimz agents wait calm-fox bold-owl shy-elk\n"
    );
    for names in [one, several] {
        for (subagent, report_to) in [
            (true, ReportTo::Launcher),
            (false, ReportTo::Nobody),
            (true, ReportTo::Nobody),
        ] {
            assert_eq!(
                receipt_for(names, None, subagent, report_to),
                receipt_for(names, None, false, ReportTo::Launcher),
                "a shell is told the same thing in either flavour, attached or detached"
            );
        }
    }
}

#[test]
fn presented_blocking_attempt_is_joined_only_once_terminal() {
    let fixture = RunFixture::new(RunStatus::Running);
    super::run::join_presented_attempt(&fixture.store, "rimz-test", &fixture.record);
    let load = || rimz::harness::run::load(&fixture.paths, &fixture.run_id()).unwrap();
    assert_eq!(load().joined_at, None);

    fixture.complete("done");
    super::run::join_presented_attempt(&fixture.store, "rimz-test", &load());
    assert!(load().joined_at.is_some());
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
    let mut sink = output::StreamSink::text(
        &mut out,
        &mut err,
        crate::cli::render::prose::Prose::Raw,
        100,
    );

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
    let mut sink = output::StreamSink::text(
        &mut out,
        &mut err,
        crate::cli::render::prose::Prose::Raw,
        100,
    );

    let outcome = stream_attached_run(
        &fixture.store,
        &run_id,
        rimz::agents::definition_by_kind("codex").unwrap(),
        false,
        Some(Duration::ZERO),
        &mut sink,
        |_| Ok(()),
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
