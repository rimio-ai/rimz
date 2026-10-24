use super::*;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::sidebar::enrich::project_local;
use crate::sidebar::notify::{LinkAlert, Notification, NotificationAgent, NotificationKind};
use crate::sidebar_pane::app::fixtures::{pane, workspace};
use crate::sidebar_pane::app::loop_state::LoopState;
use crate::{MuxName, SidebarInstanceId, WorkspaceId};
use std::path::PathBuf;

#[test]
fn room_reminder_delivery_fans_out_once_with_the_room_count() {
    let fixture = FetchFixture::new();
    let mut worker = fixture.worker();
    let mut snapshot = crate::sidebar_pane::app::fixtures::agent_snapshot(&fixture.workspace_id);
    let group = &mut Arc::make_mut(&mut snapshot.worktree_groups)[0];
    let mut row = group.rows[0].clone();
    row.unread = true;
    let crate::store::snapshot::RowCard::Agent(card) = &mut row.card else {
        panic!("agent")
    };
    Arc::make_mut(card).status = crate::agents::AgentStatus::Waiting;
    let mut other = row.clone();
    other.id = "agent-2".into();
    other.pane = Some(pane("terminal_10", "tab_1", false));
    group.rows = vec![row, other].into();
    snapshot.viewed_panes.clear();
    worker.config.notification_prefs.remind_secs = 1;
    let log = fixture._dir.path().join("reminder.log");
    worker.config.notification_prefs.command = Some(format!(
        "printf '%s:%s\\n' \"$RIMZ_NOTIFY_KIND\" \"$RIMZ_NOTIFY_UNREAD\" >> {}",
        shlex::try_quote(log.to_str().unwrap()).unwrap(),
    ));
    worker.clock = super::super::clock::LoopClock::play(snapshot.now);
    let (wake, events) = wake_channel();
    let (mut sink, _reader) = single_sink(fixture.host_data.clone(), wake);
    let publish = |worker: &mut FetchWorker, sink: &mut ResultSink| {
        worker.publish_fold(
            &fixture.state,
            frameless(snapshot.clone()),
            FetchPhase::Final,
            SnapshotSource::Cached,
            sink,
        );
    };
    publish(&mut worker, &mut sink);
    assert!(
        events
            .try_iter()
            .all(|event| !matches!(event, Wakeup::Event(_)))
    );
    worker.clock.advance(Duration::from_secs(1));
    publish(&mut worker, &mut sink);
    let notices: Vec<_> = events
        .try_iter()
        .filter_map(|event| match event {
            Wakeup::Event(event) => Some(event),
            _ => None,
        })
        .collect();
    assert_eq!(
        notices.len(),
        1,
        "one room reminder per interval, without an agent notification"
    );
    let event = &notices[0];
    assert!(
        matches!(event.event, SidebarEvent::Notify { recheck_unread: false, notification_kind: Some(ref kind), .. } if kind == "reminder")
    );
    publish(&mut worker, &mut sink);
    assert!(
        events
            .try_iter()
            .all(|event| !matches!(event, Wakeup::Event(_)))
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while std::fs::read_to_string(&log).unwrap_or_default() != "reminder:2\n"
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read_to_string(log).unwrap(), "reminder:2\n");
}

#[test]
fn agent_delivery_pushes_the_room_reminder_out_by_an_interval() {
    let fixture = FetchFixture::new();
    let mut worker = fixture.worker();
    let mut snapshot = crate::sidebar_pane::app::fixtures::agent_snapshot(&fixture.workspace_id);
    let set_status = |snapshot: &mut SidebarSnapshot, index: usize, status| {
        let row =
            &mut Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)[index];
        let crate::store::snapshot::RowCard::Agent(card) = &mut row.card else {
            panic!("agent")
        };
        Arc::make_mut(card).status = status;
    };
    let group = &mut Arc::make_mut(&mut snapshot.worktree_groups)[0];
    let mut row = group.rows[0].clone();
    row.unread = true;
    let mut other = row.clone();
    other.id = "agent-2".into();
    other.pane = Some(pane("terminal_10", "tab_1", false));
    group.rows = vec![row, other].into();
    set_status(&mut snapshot, 0, crate::agents::AgentStatus::Waiting);
    set_status(&mut snapshot, 1, crate::agents::AgentStatus::Running);
    snapshot.viewed_panes.clear();
    worker.config.notification_prefs.remind_secs = 2;
    worker.config.notification_prefs.coalesce_ms = 0;
    worker.clock = super::super::clock::LoopClock::play(snapshot.now);
    let (wake, events) = wake_channel();
    let (mut sink, _reader) = single_sink(fixture.host_data.clone(), wake);
    let mut publish = |worker: &mut FetchWorker, snapshot: &SidebarSnapshot| {
        worker.publish_fold(
            &fixture.state,
            frameless(snapshot.clone()),
            FetchPhase::Final,
            SnapshotSource::Cached,
            &mut sink,
        );
        events
            .try_iter()
            .filter_map(|event| match event {
                Wakeup::Event(event) => match event.event {
                    SidebarEvent::Notify {
                        notification_kind, ..
                    } => notification_kind,
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert!(publish(&mut worker, &snapshot).is_empty(), "arms at t");
    worker.clock.advance(Duration::from_secs(1));
    set_status(&mut snapshot, 1, crate::agents::AgentStatus::Waiting);
    let rung = publish(&mut worker, &snapshot);
    assert!(
        !rung.is_empty() && rung.iter().all(|kind| kind != "reminder"),
        "an agent notification rings at t+1: {rung:?}"
    );
    worker.clock.advance(Duration::from_secs(1));
    assert!(
        publish(&mut worker, &snapshot).is_empty(),
        "the ring re-armed the reminder past t+2"
    );
    worker.clock.advance(Duration::from_secs(1));
    assert_eq!(publish(&mut worker, &snapshot), ["reminder"]);
}

#[test]
fn parked_workflows_are_content_not_a_clock_only_change() {
    let left = crate::sidebar_pane::app::fixtures::snapshot(&workspace());
    let mut value = serde_json::to_value(&left).unwrap();
    value["parked_workflows"] = serde_json::json!([{"run":"wf_0190b8172c0070008000000000000001","workflow":"review","node":"triage","reason":"asked","text":"Which fix?","checkout":"/repo","since":"2026-01-01T00:00:00Z"}]);
    let right = serde_json::from_value(value).unwrap();
    assert!(
        !same_snapshot_content(&left, &right),
        "a newly parked run must reach the renderer as content"
    );
}

#[test]
fn renderer_fixture_moves_the_clients_presentation_without_reducing_hidden_views() {
    let fixture = FetchFixture::new();
    let mut panes = Vec::new();
    for index in 0..2 {
        let tab = format!("tab_{index}");
        panes.push(pane(&format!("terminal_{index}"), &tab, false));
        let mut chrome = pane(&format!("sidebar_{index}"), &tab, false);
        chrome.command = Some("rimz-sidebar".into());
        panes.push(chrome);
    }
    let mut frame = crate::sidebar::frame::assemble_frame(
        panes,
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    frame.viewed_panes = vec![PaneId::from_parts(MuxName::Zellij, "terminal_0")];
    frame.client_views = vec![crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Zellij(1),
        pane_id: frame.viewed_panes[0].clone(),
    }];
    frame.client_view_fresh = true;
    let mut cycles =
        IdleCycleFixture::new(fixture.state.clone(), fixture.runtime.clone(), frame, 2);
    cycles.enable_renderer_apply();
    cycles.run_cycle();
    let source = cycles
        .renderers
        .iter()
        .position(|renderer| {
            renderer
                .views
                .mirror()
                .own_view
                .as_ref()
                .unwrap()
                .working_pane_ids[0]
                .raw()
                == "terminal_0"
        })
        .unwrap();
    let destination = 1 - source;
    let mut terminal =
        ratatui::Terminal::new(super::super::backend::PaneBackend::captured(40, 60)).unwrap();
    let renderer = &mut cycles.renderers[source];
    renderer
        .on_input(
            super::super::input::Wakeup::Key(super::super::input::KeyAction::Help),
            &mut terminal,
        )
        .unwrap();
    let before = cycles.renderers[destination].pane.content_applications;
    for _ in 0..3 {
        cycles.run_cycle();
    }
    assert_eq!(
        cycles.renderers[destination].pane.content_applications,
        before
    );
    {
        let mut data = cycles.sink.host_data.lock().unwrap();
        let frame = Arc::make_mut(&mut data.inputs.as_mut().unwrap().frame);
        frame.viewed_panes = vec![PaneId::from_parts(MuxName::Zellij, "terminal_1")];
        frame.client_views[0].pane_id = frame.viewed_panes[0].clone();
    }
    cycles.run_cycle();
    assert!(
        cycles
            .sink
            .host_data
            .lock()
            .unwrap()
            .views
            .lock()
            .unwrap()
            .peek(&PaneId::from_parts(MuxName::Zellij, "sidebar_1"))
            .is_some_and(|(state, _)| state.help_visible),
        "the fixture must carry the client's presentation, not independent pane copies"
    );
}

#[test]
fn identity_free_nudges_keep_every_idle_consumer_clock_only() {
    let fixture = FetchFixture::new();
    let frame = crate::testkit::fleet::seed_scale_room(
        &fixture.state,
        &fixture.runtime,
        crate::testkit::fleet::ScaleRoom {
            agents: 50,
            panes: 57,
            ended_sessions: 0,
            orphan_contexts: 0,
        },
    )
    .unwrap();
    let mut panes = frame.to_pane_refs();
    let mut sidebars = Vec::new();
    for (index, pane) in panes.iter_mut().enumerate() {
        pane.first_seen_at_ms = Some(frame.produced_at_ms - 60_000);
        pane.view_id = Some(format!("tab_{index}"));
        let mut sidebar = pane.clone();
        sidebar.pane_id = PaneId::from_parts(MuxName::Zellij, format!("sidebar_{index}"));
        sidebar.command = Some("rimz-sidebar".into());
        sidebars.push(sidebar);
    }
    let viewed = sidebars[0].pane_id.clone();
    panes.extend(sidebars);
    let mut frame =
        crate::sidebar::frame::assemble_frame(panes, frame.produced_at_ms, frame.session_name);
    frame.viewed_panes = vec![viewed.clone()];
    frame.client_views = vec![crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Zellij(1),
        pane_id: viewed,
    }];
    let mut cycles =
        IdleCycleFixture::new(fixture.state.clone(), fixture.runtime.clone(), frame, 57);
    cycles.enable_renderer_apply();
    cycles.run_cycle();
    cycles.run_cycle();
    let before = cycles
        .renderers
        .iter()
        .map(|renderer| renderer.pane.content_applications)
        .collect::<Vec<_>>();
    assert!(
        before.iter().all(|count| *count > 0),
        "every consumer has its initial content"
    );
    for event in [
        SidebarEvent::StoreDelta {
            event_method: None,
            agent_signal: None,
        },
        SidebarEvent::PanesChanged,
        SidebarEvent::PaneOpened {
            pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_0"),
            command: None,
        },
    ] {
        cycles.deliver_event(event.clone());
        cycles.run_cycle();
        assert_eq!(
            cycles
                .renderers
                .iter()
                .map(|renderer| renderer.pane.content_applications)
                .collect::<Vec<_>>(),
            before,
            "{event:?}: identity-free nudges must not force content onto idle consumers"
        );
    }
}

#[test]
fn fetch_window_records_reductions_once_per_window() {
    let fixture = FetchFixture::new();
    let diag = crate::diag::DiagSink::under(
        fixture.state.root.clone(),
        fixture.workspace_id.clone(),
        "rimz-test",
        None,
    );
    let frame = crate::testkit::fleet::seed_scale_room(
        &fixture.state,
        &fixture.runtime,
        crate::testkit::fleet::ScaleRoom {
            agents: 2,
            panes: 2,
            ended_sessions: 0,
            orphan_contexts: 0,
        },
    )
    .unwrap();
    let mut cycles =
        IdleCycleFixture::new(fixture.state.clone(), fixture.runtime.clone(), frame, 2);
    cycles.renderers = consumers(&mut cycles);
    cycles.worker.diag = diag.clone();
    cycles.run_cycle();
    let subscriber = &cycles.sink.cycle[0];
    let reader = Replay::new(&cycles.sink.host_data, subscriber);
    let mut config = pane_config(cycles.worker.config.clone());
    config.own_pane = subscriber.own_pane.clone();
    let mut co_viewer = consumer(config, fixture.runtime.clone(), reader);
    co_viewer.on_snapshot();
    let views = cycles.sink.host_data.lock().unwrap().views.clone();
    assert_eq!(views.lock().unwrap().reductions.content, 2);
    assert_eq!(views.lock().unwrap().reductions.unchanged, 0);
    cycles.run_cycle();
    assert!(
        !diag.log_path().unwrap().exists(),
        "ordinary ticks write no record"
    );
    cycles.worker.window.started -= Duration::from_secs(60);
    cycles.run_cycle();
    assert!(
        diag.log_path().unwrap().exists(),
        "the worker must emit its elapsed reduction window"
    );
    let records = diagnostic_events(&diag);
    assert_eq!(records.len(), 1);
    assert!(
        matches!(
            records[0],
            crate::diag::record::DiagEvent::FetchReductionStats {
                cycles: 3,
                content_reductions: 2,
                unchanged_reductions: 2,
                ..
            }
        ),
        "counts describe successful reductions, not unread publications"
    );
    cycles.run_cycle();
    assert_eq!(diagnostic_events(&diag).len(), 1);
}

#[test]
fn idle_scale_cycles_advance_renderer_clocks_without_content_application() {
    let fixture = FetchFixture::new();
    let mut frame = crate::testkit::fleet::seed_scale_room(
        &fixture.state,
        &fixture.runtime,
        crate::testkit::fleet::ScaleRoom {
            agents: 200,
            panes: 100,
            ended_sessions: 5_000,
            orphan_contexts: 0,
        },
    )
    .unwrap();
    // A live room's plain shell: no agent, no derived process start, and the
    // producer's first-seen stamp.
    let mut panes = frame.to_pane_refs();
    let mut shell = panes[0].clone();
    shell.pane_id = PaneId::from_parts(MuxName::Zellij, "terminal_shell");
    shell.cwd = Some("/repo".to_owned());
    shell.first_seen_at_ms = Some(frame.produced_at_ms - 60_000);
    panes.push(shell);
    frame = crate::sidebar::frame::assemble_frame(
        panes,
        frame.produced_at_ms,
        frame.session_name.clone(),
    );
    let now_ms = crate::utils::time::unix_now_ms();
    frame.presence = Some(crate::store::snapshot::PresenceSample {
        human_clients: 1,
        last_input_ms: Some(now_ms - 3_600_000),
        sampled_at_ms: now_ms,
    });
    let mut cycles =
        IdleCycleFixture::new(fixture.state.clone(), fixture.runtime.clone(), frame, 100);
    let mut renderers = consumers(&mut cycles);
    assert_eq!(renderers.len(), 100);
    let mut previous: Option<Arc<WorkspaceFold>> = None;
    for cycle in 0..3 {
        cycles.run_cycle();
        let fold = cycles
            .sink
            .host_data
            .lock()
            .unwrap()
            .views
            .lock()
            .unwrap()
            .fold
            .clone()
            .unwrap();
        assert!(
            fold.workspace.0.rows().any(|row| row.is_process()),
            "the room carries a plain shell row"
        );
        if let Some(previous) = &previous {
            let current = &fold.workspace.0;
            let previous = &previous.workspace.0;
            assert!(
                Arc::ptr_eq(&current.worktree_groups, &previous.worktree_groups),
                "equal groups must be shared across idle cycles"
            );
            assert!(Arc::ptr_eq(&current.agents, &previous.agents));
            assert!(Arc::ptr_eq(&current.agent_panes, &previous.agent_panes));
            assert!(Arc::ptr_eq(
                &current.fenced_sessions,
                &previous.fenced_sessions
            ));
            assert!(Arc::ptr_eq(
                current.resume_outcomes.as_ref().unwrap(),
                previous.resume_outcomes.as_ref().unwrap()
            ));
        }
        previous = Some(fold);
        for renderer in &mut renderers {
            let before = renderer.views.mirror().now;
            renderer.on_snapshot();
            assert!(
                renderer.views.mirror().now > before,
                "clock advances on cycle {cycle}"
            );
            assert!(matches!(
                renderer.views.mirror().presence,
                Some(crate::store::snapshot::SidebarPresence::Idle { .. })
            ));
            assert_eq!(
                renderer.pane.content_applications, 1,
                "only the first idle delivery applies content (cycle {cycle})"
            );
        }
    }
}

fn consumer(config: ServeConfig, runtime: RuntimePaths, reader: Replay) -> LoopState {
    if let Some(own) = &config.own_pane {
        let mut views = reader.views.lock().unwrap();
        if views.peek(own).is_none() {
            let (_, entry) = views.presentation(own, Instant::now(), false);
            // These fixtures start independent consumers, not carried client views.
            entry.cursor = Default::default();
        }
    }
    let mut state = LoopState::new(
        crate::sidebar_pane::app::clock::LoopClock::wall(),
        config,
        None,
        runtime,
        PathBuf::new(),
        crate::diag::DiagSink::disabled(),
        FetchHandle::new(std::sync::mpsc::channel().0, reader.completion),
        None,
        std::sync::mpsc::sync_channel(1).0,
        Default::default(),
        None,
    );
    state.share_views(reader.views, reader.cursor.lock().unwrap().revision);
    state
}

fn consumers(cycles: &mut IdleCycleFixture) -> Vec<LoopState> {
    cycles.sink.begin_cycle();
    cycles.cursors.clear();
    cycles
        .sink
        .cycle
        .iter()
        .map(|subscriber| {
            let mut config = pane_config(cycles.worker.config.clone());
            config.instance_id = subscriber.instance_id.clone();
            config.own_pane = subscriber.own_pane.clone();
            let reader = Replay::new(&cycles.sink.host_data, subscriber);
            consumer(config, cycles.worker.runtime.clone(), reader)
        })
        .collect()
}

#[test]
fn event_context_and_shared_input_changes_reach_every_idle_consumer() {
    let fixture = FetchFixture::new();
    let frame = crate::testkit::fleet::seed_scale_room(
        &fixture.state,
        &fixture.runtime,
        crate::testkit::fleet::ScaleRoom {
            agents: 2,
            panes: 2,
            ended_sessions: 0,
            orphan_contexts: 0,
        },
    )
    .unwrap();
    let target = frame.to_pane_refs()[0].pane_id.clone();
    crate::store::agent_context::write(
        &fixture.runtime,
        "claude",
        "agent-0",
        &serde_json::from_value(serde_json::json!({
            "source": "claude", "model_id": "claude-opus-4-8", "observed_at": jiff::Timestamp::now()
        }))
        .unwrap(),
        None,
    )
    .unwrap();
    let mut cycles =
        IdleCycleFixture::new(fixture.state.clone(), fixture.runtime.clone(), frame, 100);
    let mut renderers = consumers(&mut cycles);
    for _ in 0..2 {
        cycles.run_cycle();
        for renderer in &mut renderers {
            renderer.on_snapshot();
            assert_eq!(renderer.pane.content_applications, 1);
        }
    }

    let (new_subscriber, mut rx) = subscriber(&fixture, "99", "sidebar_new", 1, wake_channel().0);
    rx.views = cycles.sink.host_data.lock().unwrap().views.clone();
    let mut config = pane_config(cycles.worker.config.clone());
    config.instance_id = new_subscriber.instance_id.clone();
    config.own_pane = new_subscriber.own_pane.clone();
    subscribe(&cycles.sink.subscribers, new_subscriber);
    renderers.push(consumer(config, fixture.runtime.clone(), rx));
    cycles.run_cycle();
    for renderer in &mut renderers {
        renderer.on_snapshot();
        assert_eq!(
            renderer.pane.content_applications, 1,
            "new subscriber gets content while established consumers stay idle"
        );
    }

    for (index, change) in ["event", "context", "anchor", "filter"]
        .into_iter()
        .enumerate()
    {
        match change {
            "event" => crate::store::event_log::append(&fixture.state.events_log, &crate::testkit::fleet::registered_lifecycle(&fixture.workspace_id, 2)).unwrap(),
            "context" => crate::store::agent_context::write(&fixture.runtime, "claude", "agent-0", &serde_json::from_value(serde_json::json!({
                "source": "claude", "model_id": "claude-sonnet-4-6", "observed_at": jiff::Timestamp::now()
            })).unwrap(), None).unwrap(),
            "anchor" => crate::mux::focus_anchor::store(&fixture.runtime, &crate::mux::focus_anchor::FocusAnchor {
                nonce: crate::ids::FocusNonce::new(),
                session_name: crate::testkit::fleet::SESSION_NAME.to_owned(),
                pane_id: target.clone(),
                origin: crate::mux::focus_anchor::FocusOrigin::User,
                repair_generation: None,
                issued_at_ms: crate::utils::time::unix_now_ms(),
                applied_at_ms: None,
                state: crate::mux::focus_anchor::FocusIntentState::Requested,
                    pre_action: Vec::new(), placement: None, order: None,
            }).unwrap(),
            "filter" => crate::sidebar::body_filter::write(&fixture.runtime, &crate::sidebar::body_filter::BodyLens { query: Some("claude".into()), ..Default::default() }).unwrap(),
            _ => unreachable!(),
        }
        cycles.run_cycle();
        for renderer in &mut renderers {
            renderer.on_snapshot();
            if change == "context" {
                let agent = renderer
                    .views
                    .mirror()
                    .agents
                    .iter()
                    .find(|agent| agent.agent_id.as_str() == "agent-0")
                    .expect("seeded session remains visible");
                assert_eq!(
                    agent
                        .context
                        .as_ref()
                        .and_then(|context| context.model_id.as_deref()),
                    Some("claude-sonnet-4-6")
                );
            }
            assert_eq!(
                renderer.pane.content_applications,
                index as u64 + 2,
                "{change} reaches each consumer on the next cycle"
            );
        }
    }
}

#[test]
fn clock_thresholds_reach_every_consumer_as_content() {
    let fixture = FetchFixture::new();
    let start = jiff::Timestamp::from_second(1_750_000_000).unwrap();
    for presence in [true, false] {
        let mut live_pane = pane("terminal_7", "tab_1", false);
        live_pane.command = Some("claude".into());
        let mut frame = crate::sidebar::frame::assemble_frame(
            vec![live_pane.clone()],
            start.as_millisecond() as u64,
            "rimz-test",
        );
        frame.presence = Some(crate::store::snapshot::PresenceSample {
            human_clients: 1,
            last_input_ms: Some(start.as_millisecond() as u64),
            sampled_at_ms: start.as_millisecond() as u64,
        });
        let frame = Arc::new(frame);
        let mut cycles = IdleCycleFixture::new(
            fixture.state.clone(),
            fixture.runtime.clone(),
            (*frame).clone(),
            100,
        );
        let mut renderers = consumers(&mut cycles);
        let mut agent = crate::testkit::agent_state("claude", "stall", start);
        agent.status = crate::agents::AgentStatus::Running;
        agent.pane = Some(live_pane.clone());
        let threshold = if presence {
            SidebarSnapshot::build_with_agents(fixture.workspace_id.clone(), Vec::new(), start)
                .sidebar
                .afk_after_ms()
                / 1_000
        } else {
            120
        };
        for (step, seconds) in [threshold - 1, threshold].into_iter().enumerate() {
            let now = start + jiff::SignedDuration::from_secs(seconds as i64);
            let mut snapshot = SidebarSnapshot::build_with_agents(
                fixture.workspace_id.clone(),
                if presence {
                    Vec::new()
                } else {
                    vec![agent.clone()]
                },
                now,
            );
            if !presence {
                snapshot.attention.stalled_after_secs = std::num::NonZeroU32::new(120).unwrap();
                snapshot = snapshot.with_live_panes(vec![live_pane.clone()], None);
            }
            let projected = WorkspaceFold {
                workspace: WorkspaceSnapshot(snapshot),
                frame: Some(frame.clone()),
            };
            cycles.sink.publish(FetchUpdate::Fold {
                fold: Arc::new(projected),
                phase: FetchPhase::Final,
                source: SnapshotSource::Cached,
            });
            for renderer in &mut renderers {
                renderer.on_snapshot();
                assert_eq!(renderer.pane.content_applications, step as u64 + 1);
                if presence {
                    assert_eq!(
                        matches!(
                            renderer.views.mirror().presence,
                            Some(crate::store::snapshot::SidebarPresence::Idle { .. })
                        ),
                        step == 1
                    );
                } else {
                    assert_eq!(
                        renderer.views.mirror().rows().next().unwrap().status(),
                        Some(if step == 0 {
                            crate::agents::AgentStatus::Running
                        } else {
                            crate::agents::AgentStatus::Failed
                        })
                    );
                }
            }
        }
    }
}

fn assert_shared_fields(shown: &SidebarSnapshot, original: &SidebarSnapshot) {
    assert!(Arc::ptr_eq(
        &shown.worktree_groups,
        &original.worktree_groups
    ));
    assert!(Arc::ptr_eq(&shown.agents, &original.agents));
    assert!(Arc::ptr_eq(
        &shown.fenced_sessions,
        &original.fenced_sessions
    ));
    assert!(Arc::ptr_eq(&shown.agent_panes, &original.agent_panes));
    for (group, original) in shown
        .worktree_groups
        .iter()
        .zip(original.worktree_groups.iter())
    {
        assert!(Arc::ptr_eq(&group.rows, &original.rows));
    }
    if let (Some(shown), Some(original)) = (&shown.resume_outcomes, &original.resume_outcomes) {
        assert!(Arc::ptr_eq(shown, original));
    }
}

#[test]
fn steady_state_fusion_and_gate_preserve_shared_fields() {
    use crate::sidebar_pane::app::{gate, state::RenderState};
    let fixture = FetchFixture::new();
    let shared = crate::sidebar_pane::app::fixtures::agent_snapshot(&fixture.workspace_id);
    let frame = crate::sidebar::frame::assemble_frame(Vec::new(), 1, "rimz-test");
    let own = PaneId::from_parts(MuxName::Zellij, "sidebar_absent");
    let projected = project_local(WorkspaceSnapshot(shared.clone()), Some(&frame), Some(&own));
    let (fused, baseline, _) =
        crate::sidebar::fuse::fuse_owned(projected, Default::default(), None, 1);
    assert!(baseline.is_none());
    assert_shared_fields(&fused, &shared);
    let (state, _, rejected, escaped, _) = gate::apply_gate(
        RenderState {
            snapshot: fused,
            health: Default::default(),
        },
        true,
        &shared,
        &Default::default(),
        shared.now,
    );
    assert!(!rejected && !escaped);
    assert_shared_fields(&state.snapshot, &shared);
}

#[test]
fn publication_shares_agent_scaled_fields_at_room_scale() {
    for count in [24, 100] {
        let fixture = FetchFixture::new();
        let mut shared = crate::sidebar_pane::app::fixtures::agent_snapshot(&fixture.workspace_id);
        let template = shared.worktree_groups[0].clone();
        Arc::make_mut(&mut shared.worktree_groups).clear();
        for group_index in 0..4 {
            let mut group = template.clone();
            Arc::make_mut(&mut group.rows).clear();
            for index in 0..50 {
                let mut row = template.rows[0].clone();
                row.id = format!("row-{group_index}-{index}");
                Arc::make_mut(&mut group.rows).push(row);
            }
            Arc::make_mut(&mut shared.worktree_groups).push(group);
        }
        Arc::make_mut(&mut shared.agents).clear();
        for index in 0..200 {
            Arc::make_mut(&mut shared.agents).push(crate::testkit::agent_state(
                "claude",
                &format!("agent-{index}"),
                jiff::Timestamp::UNIX_EPOCH,
            ));
        }
        for index in 0..4000 {
            Arc::make_mut(&mut shared.fenced_sessions).insert((
                AgentKind::new_unchecked("claude"),
                AgentSessionId::from(format!("ended-{index}")),
            ));
        }
        shared.value_tally = Some(crate::SpendTally::default().into());
        shared.workspace_value_tally = Some(crate::SpendTally::default().into());
        let retained = shared.clone();
        let subscribers = Subscribers::default();
        let mut receivers = Vec::new();
        for index in 0..count {
            let (sub, rx) = subscriber(
                &fixture,
                &format!("{index:02}"),
                &format!("sidebar_{index}"),
                2,
                wake_channel().0,
            );
            subscribe(&subscribers, sub);
            receivers.push(rx);
        }
        let mut sink = ResultSink::shared(subscribers, fixture.host_data.clone());

        let projected = WorkspaceFold {
            workspace: WorkspaceSnapshot(shared),
            frame: Some(Arc::new(crate::sidebar::frame::assemble_frame(
                Vec::new(),
                1,
                "rimz-test",
            ))),
        };
        sink.publish(FetchUpdate::Fold {
            fold: Arc::new(projected),
            phase: FetchPhase::Final,
            source: SnapshotSource::Cached,
        });
        let delivered: Vec<_> = receivers.iter().map(|rx| rx.try_recv().unwrap()).collect();
        for update in &delivered {
            let shown = snapshot(update);
            assert_eq!(shown.rows().count(), 200);
            assert_eq!(shown.agents.len(), 200);
            assert_eq!(shown.fenced_sessions.len(), 4000);
            assert_shared_fields(shown, &retained);
            for (shown, retained) in [
                (&shown.value_tally, &retained.value_tally),
                (
                    &shown.workspace_value_tally,
                    &retained.workspace_value_tally,
                ),
            ] {
                assert!(std::ptr::eq(
                    &shown.as_ref().unwrap().headline,
                    &retained.as_ref().unwrap().headline,
                ));
            }
        }
    }
}

#[test]
fn unchanged_pane_overlays_keep_group_vector_shared() {
    let fixture = FetchFixture::new();
    let shared = crate::sidebar_pane::app::fixtures::agent_snapshot(&fixture.workspace_id);
    let mut local = shared.clone();

    let missing = PaneId::from_parts(MuxName::Zellij, "sidebar_absent");
    local.remove_pane_rows(&missing);
    local.overlay_pane_command(&missing, "zsh");
    local.sort_groups_for_presentation();
    crate::sidebar_pane::app::state::set_rows_unread(&mut local, &["absent".into()], false);
    assert_shared_fields(&local, &shared);
}

#[test]
fn unread_write_back_copies_only_the_changed_group() {
    let fixture = FetchFixture::new();
    let subscribers = Subscribers::default();
    let (elder, elder_rx) = subscriber(&fixture, "01", "sidebar_1", 2, wake_channel().0);
    let (younger, younger_rx) = subscriber(&fixture, "02", "sidebar_2", 2, wake_channel().0);
    subscribe(&subscribers, elder);
    subscribe(&subscribers, younger);
    let mut sink = ResultSink::shared(subscribers, fixture.host_data.clone());
    let mut shared = crate::sidebar_pane::app::fixtures::agent_snapshot(&fixture.workspace_id);
    let mut second = shared.worktree_groups[0].rows[0].clone();
    second.id = "second".into();
    Arc::make_mut(&mut Arc::make_mut(&mut shared.worktree_groups)[0].rows).push(second);
    for row in Arc::make_mut(&mut Arc::make_mut(&mut shared.worktree_groups)[0].rows) {
        row.unread = true;
        row.as_agent_mut().unwrap().status = crate::agents::AgentStatus::Idle;
    }
    let mut unchanged = shared.worktree_groups[0].clone();
    Arc::make_mut(&mut unchanged.rows).truncate(1);
    Arc::make_mut(&mut unchanged.rows)[0].id = "unchanged".into();
    Arc::make_mut(&mut unchanged.rows)[0].unread = false;
    Arc::make_mut(&mut shared.worktree_groups).push(unchanged);
    let retained = shared.clone();
    let mut worker = fixture.worker();
    let projected = frameless(shared);

    worker.publish_fold(
        &fixture.state,
        projected,
        FetchPhase::Final,
        SnapshotSource::Cached,
        &mut sink,
    );
    for rx in [elder_rx, younger_rx] {
        let update = rx.try_recv().unwrap();
        let shown = snapshot(&update);
        assert!(shown.rows().all(|row| !row.unread));
        assert!(!Arc::ptr_eq(
            &shown.worktree_groups,
            &retained.worktree_groups
        ));
        assert!(Arc::ptr_eq(&shown.agents, &retained.agents));
        assert!(Arc::ptr_eq(
            &shown.fenced_sessions,
            &retained.fenced_sessions
        ));
        assert!(Arc::ptr_eq(&shown.agent_panes, &retained.agent_panes));
        for group in shown.worktree_groups.iter() {
            let unchanged = group.rows.iter().any(|row| row.id == "unchanged");
            let original = &retained.worktree_groups[usize::from(unchanged)];
            assert_eq!(Arc::ptr_eq(&group.rows, &original.rows), unchanged);
        }
    }
    assert!(
        retained.worktree_groups[0]
            .rows
            .iter()
            .all(|row| row.unread)
    );
}

struct Replay {
    views: Arc<Mutex<super::super::views::Views>>,
    cursor: Mutex<super::super::views::Cursor>,
    completion: Arc<AtomicBool>,
    own_pane: Option<PaneId>,
}

impl Replay {
    fn new(host_data: &Mutex<HostData>, subscriber: &Subscriber) -> Self {
        Self {
            views: host_data.lock().unwrap().views.clone(),
            cursor: Mutex::default(),
            completion: subscriber.completion.clone(),
            own_pane: subscriber.own_pane.clone(),
        }
    }

    fn take(&self) -> Vec<FetchUpdate> {
        let views = self.views.lock().unwrap();
        let mut cursor = self.cursor.lock().unwrap();
        let updates = views
            .latest
            .since(cursor.revision)
            .into_iter()
            .map(|update| {
                cursor.replay(
                    update,
                    |fold| LoopState::project_fold(fold, self.own_pane.as_ref(), None),
                    true,
                )
            })
            .collect();
        cursor.revision = views.revision;
        updates
    }

    fn try_iter(&self) -> std::vec::IntoIter<FetchUpdate> {
        self.take().into_iter()
    }

    fn try_recv(&self) -> Result<FetchUpdate, std::sync::mpsc::TryRecvError> {
        self.try_iter()
            .next()
            .ok_or(std::sync::mpsc::TryRecvError::Empty)
    }

    fn recv(&self) -> Result<FetchUpdate, std::sync::mpsc::RecvError> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(update) = self.try_recv() {
                return Ok(update);
            }
            if Instant::now() >= deadline {
                return Err(std::sync::mpsc::RecvError);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

fn single_sink(host_data: Arc<Mutex<HostData>>, wake: SyncSender<Wakeup>) -> (ResultSink, Replay) {
    let subscriber = Subscriber {
        instance_id: SidebarInstanceId::new(),
        own_pane: None,
        tick_seconds: 1,
        completion: Arc::default(),
        wake,
    };
    let reader = Replay::new(&host_data, &subscriber);
    let subscribers = Arc::new(Mutex::new(BTreeMap::from([(
        subscriber.instance_id.as_str().to_owned(),
        subscriber,
    )])));
    (ResultSink::shared(subscribers, host_data), reader)
}

fn frameless(snapshot: SidebarSnapshot) -> WorkspaceFold {
    WorkspaceFold {
        workspace: WorkspaceSnapshot(snapshot),
        frame: None,
    }
}

fn wake_channel() -> (SyncSender<Wakeup>, std::sync::mpsc::Receiver<Wakeup>) {
    std::sync::mpsc::sync_channel(64)
}

fn guarded_reader() -> (tempfile::TempDir, RollupCursor) {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(workspace(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, RollupCursor::new())
}

fn run_cycle(
    worker: &mut FetchWorker,
    state: &StatePaths,
    request: FetchRequest,
    host_data: Arc<Mutex<HostData>>,
) -> Vec<FetchUpdate> {
    let (mut sink, reader) = single_sink(host_data, wake_channel().0);
    reader.cursor.lock().unwrap().revision = reader.views.lock().unwrap().revision;
    worker.run_cycle(state, request, &mut sink);
    reader
        .try_iter()
        .map(|update| update.into_parts().0)
        .collect()
}

fn snapshot(update: &FetchUpdate) -> &SidebarSnapshot {
    match update {
        FetchUpdate::Shared { update, .. } => snapshot(update),
        FetchUpdate::Snapshot { snapshot, .. } => snapshot,
        FetchUpdate::Fold { .. } => panic!("expected a projected snapshot"),
        FetchUpdate::Unchanged { .. } => panic!("expected content, got clock-only delivery"),
        FetchUpdate::Failed { error, .. } => panic!("expected snapshot, got: {error}"),
    }
}

#[test]
fn produce_guard_maps_failures_and_suppresses_renderer_panic_diagnostics() {
    let (_dir, mut reader) = guarded_reader();
    let result: std::result::Result<SidebarSnapshot, String> =
        run_produce_guarded(&mut reader, |_| {
            Err(crate::sidebar::produce::ProduceErr::Fixture {
                path: PathBuf::from("/nonexistent/panes.json"),
                reason: "injected failure".to_owned(),
            })
        });
    assert!(result.unwrap_err().contains("injected failure"));

    let _hook_guard = crate::sidebar_pane::app::PANIC_HOOK_TEST_LOCK
        .lock()
        .unwrap();
    let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed_hook = observed.clone();
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |_| {
        observed_hook.store(
            super::super::produce_panic_diagnostic_suppressed(),
            std::sync::atomic::Ordering::SeqCst,
        );
    }));

    let result: std::result::Result<SidebarSnapshot, String> =
        run_produce_guarded(&mut reader, |_| panic!("boom"));
    std::panic::set_hook(previous_hook);

    assert_eq!(result.unwrap_err(), "sidebar produce panicked: boom");
    assert!(
        observed.load(std::sync::atomic::Ordering::SeqCst),
        "caught producer panics run under the diagnostic-suppression guard"
    );
}

#[test]
fn refresh_override_stamps_folded_snapshot() {
    let fixture = FetchFixture::new();
    let mut folded = SidebarSnapshot::build_with_agents(
        fixture.workspace_id.clone(),
        Vec::new(),
        jiff::Timestamp::UNIX_EPOCH,
    );
    folded.theme.display.refresh_ms = 250;
    let subscribers = Subscribers::default();
    let mut readers = Vec::new();
    for id in ["01", "02"] {
        let (subscriber, reader) =
            subscriber(&fixture, id, &format!("sidebar_{id}"), 1, wake_channel().0);
        subscribe(&subscribers, subscriber);
        readers.push(reader);
    }
    let mut sink = ResultSink::shared(subscribers, fixture.host_data.clone());
    sink.publish(FetchUpdate::Fold {
        fold: Arc::new(frameless(folded)),
        phase: FetchPhase::Final,
        source: SnapshotSource::Cached,
    });
    for reader in readers {
        let mut config = pane_config(fixture.worker().config);
        config.own_pane = reader.own_pane.clone();
        config.refresh_ms_override = Some(50);
        let mut renderer = consumer(config, fixture.runtime.clone(), reader);
        renderer.on_snapshot();
        assert_eq!(renderer.views.mirror().theme.display.refresh_ms, 50);
        assert_eq!(
            renderer.views.mirror().theme.display.resolved_refresh_ms(),
            50
        );
    }
    assert_eq!(
        fixture
            .host_data
            .lock()
            .unwrap()
            .views
            .lock()
            .unwrap()
            .fold
            .as_ref()
            .unwrap()
            .workspace
            .snapshot()
            .theme
            .display
            .refresh_ms,
        250
    );
}

#[test]
fn notification_panes_target_agent_panes() {
    let first = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let second = PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let notification = Notification {
        agents: vec![
            notification_agent("a1", Some(first.clone())),
            notification_agent("a2", None),
            notification_agent("a3", Some(second.clone())),
        ],
        notification_kind: NotificationKind::Coalesced,
        title: "RimZ: 3 agents need attention".to_owned(),
        body: "a1: waiting | a2: failed | a3: waiting".to_owned(),
        unread_count: None,
    };

    assert_eq!(notification_panes(&notification), vec![first, second]);
}

#[test]
fn notification_reconciliation_reaches_the_younger_pane_before_its_bell() {
    use super::super::notify::{BellDecision, bell_decision};

    let fixture = FetchFixture::new();
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let runtime = RuntimePaths::under(fixture.workspace_id.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    UnreadEpisodes::default().persist(&runtime).unwrap();
    let mut worker = fixture.worker();
    worker.runtime = runtime.clone();
    worker.config.notification_prefs.coalesce_ms = 0;
    let subscribers = Subscribers::default();
    let (elder, elder_rx) = subscriber(&fixture, "01", "terminal_7", 2, wake_channel().0);
    let (wake, notices) = wake_channel();
    let (younger, younger_rx) = subscriber(&fixture, "02", "terminal_8", 2, wake);
    subscribe(&subscribers, elder);
    subscribe(&subscribers, younger);
    let mut sink = ResultSink::shared(subscribers, fixture.host_data.clone());
    let mut shared = crate::sidebar_pane::app::fixtures::agent_snapshot(&fixture.workspace_id);
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut shared.worktree_groups)[0].rows)
        [0]
    .as_agent_mut()
    .unwrap()
    .status = crate::agents::AgentStatus::Waiting;
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut shared.worktree_groups)[0].rows)
        [0]
    .pane = Some(pane("terminal_9", "tab_2", false));
    let target = PaneId::from_parts(MuxName::Zellij, "terminal_9");
    let frame = Arc::new(crate::sidebar::frame::assemble_frame(
        vec![
            pane("terminal_7", "tab_1", false),
            pane("terminal_8", "tab_2", false),
            pane("terminal_9", "tab_2", false),
        ],
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    ));
    let projected = WorkspaceFold {
        workspace: WorkspaceSnapshot(shared),
        frame: Some(frame),
    };
    worker.publish_fold(
        &fixture.state,
        projected,
        FetchPhase::Final,
        SnapshotSource::Cached,
        &mut sink,
    );

    assert!(
        snapshot(&elder_rx.recv().unwrap())
            .rows()
            .any(|row| row.unread)
    );
    let younger = younger_rx.recv().unwrap();
    assert!(
        snapshot(&younger).rows().any(|row| row.unread),
        "this publication must carry the newly reconciled episode"
    );
    assert!(
        sink.host_data
            .lock()
            .unwrap()
            .views
            .lock()
            .unwrap()
            .fold
            .as_ref()
            .unwrap()
            .workspace
            .snapshot()
            .rows()
            .any(|row| row.unread),
        "a new attachment gets the post-notification unread fold too"
    );
    let mut plane = super::super::plane::DataPlane::detached();
    plane.views = fixture.host_data.lock().unwrap().views.clone();
    let mut config = pane_config(worker.config.clone());
    config.instance_id = SidebarInstanceId::new();
    let subscription = plane.subscribe(&config, wake_channel().0);
    assert!(
        subscription
            .seed
            .unwrap()
            .workspace
            .snapshot()
            .rows()
            .any(|row| row.unread)
    );
    assert_eq!(
        notices.recv_timeout(Duration::from_secs(1)).unwrap(),
        Wakeup::Snapshot,
        "the snapshot wake precedes the notice"
    );
    let Wakeup::Event(notice) = notices.recv_timeout(Duration::from_secs(1)).unwrap() else {
        panic!("expected notification");
    };
    let notice = notice.event;
    assert!(
        matches!(notice, SidebarEvent::Notify { ref panes, recheck_unread: true, .. } if panes.as_slice() == std::slice::from_ref(&target))
    );
    assert_eq!(
        bell_decision(snapshot(&younger), &[target], true),
        BellDecision::Fired
    );
}

#[test]
fn a_worker_notification_reaches_every_subscribed_pane_after_its_snapshot() {
    let fixture = FetchFixture::new();
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let runtime = RuntimePaths::under(fixture.workspace_id.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    UnreadEpisodes::default().persist(&runtime).unwrap();
    let mut worker = fixture.worker();
    worker.runtime = runtime.clone();
    worker.config.notification_prefs.coalesce_ms = 0;
    worker.config.notification_prefs.enabled = true;
    let subscribers = Subscribers::default();
    let mut receivers = Vec::new();
    for (id, own) in [("01", "terminal_7"), ("02", "terminal_8")] {
        let (wake, inbox) = wake_channel();
        let (sub, rx) = subscriber(&fixture, id, own, 2, wake);
        receivers.push((inbox, rx));
        subscribe(&subscribers, sub);
    }
    let mut sink = ResultSink::shared(subscribers, fixture.host_data.clone());
    let mut shared = crate::sidebar_pane::app::fixtures::agent_snapshot(&fixture.workspace_id);
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut shared.worktree_groups)[0].rows)
        [0]
    .as_agent_mut()
    .unwrap()
    .status = crate::agents::AgentStatus::Waiting;
    let frame = Arc::new(crate::sidebar::frame::assemble_frame(
        vec![
            pane("terminal_7", "tab_1", false),
            pane("terminal_8", "tab_2", false),
        ],
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    ));
    let projected = WorkspaceFold {
        workspace: WorkspaceSnapshot(shared),
        frame: Some(frame),
    };
    worker.publish_fold(
        &fixture.state,
        projected,
        FetchPhase::Final,
        SnapshotSource::Cached,
        &mut sink,
    );
    for (inbox, rx) in receivers {
        assert_eq!(
            inbox.recv_timeout(Duration::from_secs(1)).unwrap(),
            Wakeup::Snapshot
        );
        assert!(snapshot(&rx.recv().unwrap()).rows().any(|row| row.unread));
        let Wakeup::Event(envelope) = inbox.recv_timeout(Duration::from_secs(1)).unwrap() else {
            panic!("expected notification");
        };
        assert_eq!(envelope.workspace_id, fixture.workspace_id);
        assert_eq!(envelope.session_name.as_deref(), Some("rimz-test"));
        assert!(matches!(envelope.event, SidebarEvent::Notify { .. }));
    }
}

#[test]
fn diagnostics_name_link_alerts() {
    let dir = tempfile::tempdir().unwrap();
    let sink =
        crate::diag::DiagSink::under(dir.path().to_path_buf(), workspace(), "rimz-test", None);
    emit_link_alert(
        &sink,
        LinkAlert {
            tier: crate::ids::LinkTier::Degraded,
            rtt_ms: Some(230),
            miss_pct: 4,
            since_ms: 10,
            recovered_after_ms: None,
        },
    );

    let events = diagnostic_events(&sink);
    assert!(matches!(
        &events[0],
        crate::diag::record::DiagEvent::LinkAlert {
            tier: crate::ids::LinkTier::Degraded,
            rtt_ms: Some(230),
            miss_pct: 4,
            since_ms: 10,
            recovered_after_ms: None,
        }
    ));
}

/// One forced cycle folds retained inputs, then produces on the same warm cursor and delivers the final reconciling outcome. The pane producer is seeded in memory and heavy lanes are pre-published fresh, so the hermetic cycle forks nothing.
#[test]
fn forced_cycle_retains_the_reconciling_produce_for_a_stalled_renderer() {
    let dir = tempfile::tempdir().unwrap();
    let workspace_id = workspace();
    let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
    state.ensure_dirs().unwrap();
    runtime.ensure_dirs().unwrap();

    let now_ms = crate::utils::time::unix_now_ms();
    let frame = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_7", "tab_1", false)],
        now_ms,
        "rimz-test",
    );
    crate::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        &crate::agents::spending::ProviderSpendingCache {
            refreshed_at_ms: now_ms,
            day_cutoff_secs: crate::agents::spending::local_day_start_secs(now_ms / 1000, None)
                .unwrap(),
            timezone: "UTC".into(),
            chart: crate::agents::spending::ChartSpend {
                today: (now_ms / 86_400_000) as i64,
                ..Default::default()
            },
            spending: crate::agents::spending::Spending::default(),
            ..Default::default()
        },
    );
    let accounts = crate::agents::account::AccountsCache {
        logins: crate::agents::known_kinds()
            .map(|kind| {
                (
                    crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(kind)),
                    crate::agents::account::ProviderRecord {
                        profile: None,
                        probe_error: None,
                        login: None,
                        probed_at_ms: now_ms,
                        ok: true,
                        account: None,
                    },
                )
            })
            .collect(),
    };
    std::fs::write(
        runtime.shared_accounts_path(),
        serde_json::to_vec(&accounts).unwrap(),
    )
    .unwrap();

    let config = test_config(workspace_id);
    let request = FetchRequest {
        mode: FetchMode::HardRefresh,
        ..FetchRequest::default()
    };
    let producer = PaneProducer::seeded(&runtime, &config.session_name);
    let mut worker = FetchWorker::new(
        config,
        runtime,
        crate::diag::DiagSink::disabled(),
        producer,
        None,
    );
    let inputs = HostInputs::from_frame(frame);
    worker.producer.seed_test_frame(Some(inputs.frame.clone()));
    let outcomes = run_cycle(
        &mut worker,
        &state,
        request,
        Arc::new(Mutex::new(HostData {
            inputs: Some(inputs),
            ..Default::default()
        })),
    );

    assert_eq!(
        outcomes.len(),
        1,
        "the reconciling produce replaces the unread fast snapshot"
    );
    let produced = &outcomes[0];
    assert!(produced.is_final(), "the produce closes the cycle");
    assert!(matches!(
        produced,
        FetchUpdate::Snapshot {
            source: SnapshotSource::Produced,
            ..
        }
    ));
    let produced_snapshot = snapshot(produced);
    assert!(
        !produced_snapshot.worktree_groups.is_empty(),
        "the produce folds the same pane frame"
    );
}

#[test]
fn produce_gate_bounds_normal_attempts_and_forces_refreshes() {
    let tick = Duration::from_secs(1);
    let start = Instant::now();
    for (name, mode, frame_age_ms, expected) in [
        (
            "fresh producer frame skips produce",
            FetchMode::Normal,
            Some(100),
            false,
        ),
        (
            "stale producer frame produces",
            FetchMode::Normal,
            Some(1000),
            true,
        ),
        ("cold producer produces", FetchMode::Normal, None, true),
        (
            "producer-only freshness stays producer-only",
            FetchMode::FreshPanes,
            Some(0),
            true,
        ),
        (
            "producer hard refresh produces",
            FetchMode::HardRefresh,
            Some(0),
            true,
        ),
    ] {
        let mut cadence = ProducerCadence::default();
        assert_eq!(
            cadence.start_attempt_if_due(mode, frame_age_ms, tick, start),
            expected,
            "{name}"
        );
    }
}

#[test]
fn produce_gate_throttles_cold_stale_and_failed_attempts_at_tick_boundary() {
    let tick = Duration::from_secs(1);
    let start = Instant::now();
    let mut cadence = ProducerCadence::default();

    assert!(cadence.start_attempt_if_due(FetchMode::Normal, None, tick, start));
    assert!(
        !cadence.start_attempt_if_due(
            FetchMode::Normal,
            Some(10_000),
            tick,
            start + tick - Duration::from_nanos(1),
        ),
        "recording before the produce path throttles a failed cold attempt",
    );
    assert!(cadence.start_attempt_if_due(FetchMode::Normal, Some(10_000), tick, start + tick,));
}

#[test]
fn produce_gate_forced_attempts_bypass_and_advance_local_cadence() {
    let tick = Duration::from_secs(1);
    let start = Instant::now();
    let mut cadence = ProducerCadence::default();

    assert!(cadence.start_attempt_if_due(FetchMode::Normal, None, tick, start));
    let forced_at = start + Duration::from_millis(10);
    assert!(cadence.start_attempt_if_due(FetchMode::FreshPanes, Some(0), tick, forced_at,));
    assert!(cadence.start_attempt_if_due(
        FetchMode::HardRefresh,
        Some(0),
        tick,
        forced_at + Duration::from_millis(10),
    ));
    assert!(
        !cadence.start_attempt_if_due(FetchMode::Normal, Some(10_000), tick, forced_at + tick,)
    );
}

#[test]
fn tab_name_memo_deduplicates_within_one_pane_observation() {
    let mut frame = crate::sidebar::frame::assemble_frame(
        vec![pane("terminal_7", "tab_1", false)],
        12,
        "rimz-test",
    );
    frame.topology_stamp_ms = Some(11);
    let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_7");
    let rename = crate::sidebar::produce::tab_status::TabRename {
        anchor: anchor.clone(),
        desired_name: "#feat ?".to_owned(),
        intent: crate::mux::tab_name::TabNameIntent::Status {
            observed: "#feat".to_owned(),
        },
    };
    let mut memo = TabNameMemo::default();

    assert_eq!(
        memo.pending(&frame, vec![rename.clone()]),
        vec![rename.clone()]
    );
    assert!(
        memo.pending(&frame, vec![rename.clone()]).is_empty(),
        "the same desired name is attempted once per pane observation",
    );

    let changed = crate::sidebar::produce::tab_status::TabRename {
        desired_name: "#feat !".to_owned(),
        ..rename.clone()
    };
    assert_eq!(
        memo.pending(&frame, vec![changed.clone()]),
        vec![changed.clone()],
        "a status change within the same observation still dispatches",
    );

    frame.observed_at_ms += 1;
    assert_eq!(
        memo.pending(&frame, vec![changed.clone()]),
        vec![changed],
        "a fresh pane observation permits a retry",
    );
}

#[test]
fn released_tab_deduplicates_and_settles_on_the_next_observation() {
    let mut pane = pane("terminal_7", "tab_1", false);
    pane.title = Some("opus".to_owned());
    pane.view_name = Some("opus".to_owned());
    let mut frame = crate::sidebar::frame::assemble_frame(vec![pane], 12, "rimz-test");
    frame.tabs[0].naming.owner = Some(crate::mux::tab_name::TabOwnerRecord {
        base: "opus".to_owned(),
        founders: Vec::new(),
    });
    let snapshot = SidebarSnapshot::build_with_agents(
        crate::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").expect("workspace"),
        Vec::new(),
        jiff::Timestamp::from_second(1_700_000_000).expect("time"),
    );
    let mut memo = TabNameMemo::default();
    let renames =
        crate::sidebar::produce::tab_status::desired_tab_renames(&snapshot, &frame, "zsh");
    assert!(matches!(
        renames[0].intent,
        crate::mux::tab_name::TabNameIntent::Release { .. }
    ));
    assert_eq!(memo.pending(&frame, renames.clone()).len(), 1);
    assert!(memo.pending(&frame, renames).is_empty());
    frame.observed_at_ms += 1;
    frame.tabs[0].name = Some("zsh".to_owned());
    let renames =
        crate::sidebar::produce::tab_status::desired_tab_renames(&snapshot, &frame, "zsh");
    assert!(memo.pending(&frame, renames).is_empty());
}

fn notification_agent(id: &str, pane_id: Option<PaneId>) -> NotificationAgent {
    NotificationAgent {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from(id),
        label: format!("claude {id}"),
        handle: format!("claude {id}"),
        worktree: None,
        task: None,
        pane_id,
        root: None,
        ask_id: None,
        new_status: None,
    }
}

fn test_config(workspace_id: WorkspaceId) -> RoomConfig {
    RoomConfig {
        workspace_id,
        mux: MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        tick_seconds: 2,
        timezone: jiff::tz::TimeZone::UTC,
        notification_prefs: NotificationsPrefs::default(),
    }
}

fn pane_config(room: RoomConfig) -> ServeConfig {
    ServeConfig {
        instance_id: SidebarInstanceId::new(),
        own_pane: None,
        tick_seconds: room.tick_seconds,
        refresh_ms_override: None,
        room,
    }
}

fn diagnostic_events(sink: &crate::diag::DiagSink) -> Vec<crate::diag::record::DiagEvent> {
    std::fs::read_to_string(sink.log_path().unwrap())
        .expect("diagnostic log")
        .lines()
        .map(|line| {
            serde_json::from_str::<crate::diag::record::DiagEnvelope>(line)
                .expect("diagnostic envelope")
                .event
        })
        .collect()
}

struct FetchFixture {
    _dir: tempfile::TempDir,
    workspace_id: WorkspaceId,
    state: StatePaths,
    runtime: RuntimePaths,
    host_data: Arc<Mutex<HostData>>,
}

#[test]
fn renderer_gets_the_snapshot_before_a_narrowing_probe() {
    thread_local! {
        static UPDATES: std::cell::RefCell<Option<Replay>> =
            const { std::cell::RefCell::new(None) };
        static SENT_BEFORE_PROBE: std::cell::Cell<Option<bool>> =
            const { std::cell::Cell::new(None) };
    }
    let fixture = FetchFixture::new();
    let mut worker = fixture.worker();
    let lost = [(AgentKind::new_unchecked("claude"), "lost".into())].into();
    crate::store::live_roster::publish(&fixture.state.live_roster, lost).unwrap();
    let (mut sink, reader) = single_sink(fixture.host_data.clone(), wake_channel().0);
    UPDATES.set(Some(reader));
    worker.session_listed = |_, _| {
        let sent = UPDATES.with_borrow(|updates| {
            let reader = updates.as_ref().unwrap();
            let views = reader.views.lock().unwrap();
            views.revision == 1
                && reader.completion.load(Ordering::Relaxed)
                && views
                    .latest
                    .since(0)
                    .first()
                    .is_some_and(|update| update.is_final() && update.is_success())
        });
        SENT_BEFORE_PROBE.set(Some(sent));
        false
    };
    worker.publish_fold(
        &fixture.state,
        frameless(SidebarSnapshot::build(
            fixture.workspace_id.clone(),
            Vec::new(),
            jiff::Timestamp::UNIX_EPOCH,
        )),
        FetchPhase::Final,
        SnapshotSource::Produced,
        &mut sink,
    );
    assert_eq!(SENT_BEFORE_PROBE.get(), Some(true));
}

#[test]
fn roster_narrowing_needs_a_listed_session() {
    let fixture = FetchFixture::new();
    let diag = crate::diag::DiagSink::under(
        fixture._dir.path().to_path_buf(),
        fixture.workspace_id.clone(),
        "rimz-test",
        None,
    );
    let mut worker = fixture.worker();
    worker.diag = diag.clone();
    let roster = |ids: &[&str]| -> std::collections::BTreeSet<(AgentKind, AgentSessionId)> {
        ids.iter()
            .map(|id| (AgentKind::new_unchecked("claude"), (*id).into()))
            .collect()
    };
    let produced = |ids: &[&str]| {
        let now = jiff::Timestamp::now();
        let panes: Vec<_> = ids
            .iter()
            .map(|id| crate::sidebar::produce::test_support::pane(id, Some("claude"), None))
            .collect();
        let agents = ids
            .iter()
            .zip(&panes)
            .map(|(id, pane)| crate::agents::AgentState {
                status: crate::agents::AgentStatus::Running,
                pane: Some(pane.clone()),
                ..crate::testkit::agent_state("claude", id, now)
            })
            .collect();
        let mut snapshot =
            SidebarSnapshot::build_with_agents(fixture.workspace_id.clone(), agents, now);
        snapshot.panes_produced_at_ms = Some(1);
        frameless(snapshot.with_live_panes(panes, None))
    };
    let on_disk = || {
        crate::store::live_roster::read(&fixture.state.live_roster)
            .unwrap()
            .agents
    };
    let (mut sink, _rx) = single_sink(fixture.host_data.clone(), wake_channel().0);
    crate::store::live_roster::publish(&fixture.state.live_roster, roster(&["a", "b"])).unwrap();
    let before = std::fs::read(&fixture.state.live_roster).unwrap();

    // A producer that outlived its session sees no agents, every cycle.
    worker.session_listed = |_, _| false;
    worker.publish_fold(
        &fixture.state,
        produced(&[]),
        FetchPhase::Final,
        SnapshotSource::Produced,
        &mut sink,
    );
    worker.publish_fold(
        &fixture.state,
        produced(&["a"]),
        FetchPhase::Final,
        SnapshotSource::Produced,
        &mut sink,
    );
    worker.publish_fold(
        &fixture.state,
        produced(&[]),
        FetchPhase::Final,
        SnapshotSource::Produced,
        &mut sink,
    );
    assert_eq!(std::fs::read(&fixture.state.live_roster).unwrap(), before);
    assert_eq!(on_disk(), roster(&["a", "b"]), "a birth still parks both");
    assert_eq!(
        diagnostic_events(&diag),
        [
            crate::diag::record::DiagEvent::LiveRosterHeld {
                dropped: roster(&["a", "b"]).into_iter().collect(),
            },
            crate::diag::record::DiagEvent::LiveRosterHeld {
                dropped: roster(&["b"]).into_iter().collect(),
            },
        ],
        "a repeat of one held set is rate-limited to one record",
    );

    // Nothing removed: written without asking the mux.
    worker.session_listed = |_, _| panic!("an additive publication must not probe");
    worker.publish_fold(
        &fixture.state,
        produced(&["a", "b", "c"]),
        FetchPhase::Final,
        SnapshotSource::Produced,
        &mut sink,
    );
    assert_eq!(on_disk(), roster(&["a", "b", "c"]));

    // An agent that exits in a living room leaves the roster.
    worker.session_listed = |_, _| true;
    worker.publish_fold(
        &fixture.state,
        produced(&["a"]),
        FetchPhase::Final,
        SnapshotSource::Produced,
        &mut sink,
    );
    assert_eq!(on_disk(), roster(&["a"]));
}

impl FetchFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id = workspace();
        let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
        let runtime =
            RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
        state.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        Self {
            _dir: dir,
            workspace_id,
            state,
            runtime,
            host_data: Arc::default(),
        }
    }

    fn worker(&self) -> FetchWorker {
        let config = test_config(self.workspace_id.clone());
        let producer = PaneProducer::seeded(&self.runtime, &config.session_name);
        let mut worker = FetchWorker::new(
            config,
            self.runtime.clone(),
            crate::diag::DiagSink::disabled(),
            producer,
            None,
        );
        worker.producer.seed_test_frame(
            self.host_data
                .lock()
                .unwrap()
                .inputs
                .as_ref()
                .map(|inputs| inputs.frame.clone()),
        );
        worker.producer_cadence.last_attempt = Some(Instant::now() + Duration::from_secs(3600));
        worker.session_listed = |_, _| panic!("a unit test must not ask a multiplexer");
        worker
    }

    fn run(&self, request: FetchRequest) -> Vec<FetchUpdate> {
        self.run_with(request, &mut self.worker())
    }

    fn run_with(&self, request: FetchRequest, worker: &mut FetchWorker) -> Vec<FetchUpdate> {
        run_cycle(worker, &self.state, request, self.host_data.clone())
    }

    fn write_pane_frame(&self) {
        let mut frame = crate::sidebar::frame::assemble_frame(
            vec![pane("terminal_7", "tab_1", false)],
            crate::utils::time::unix_now_ms(),
            "rimz-test",
        );
        frame.topology_stamp_ms = Some(11);
        self.host_data.lock().unwrap().inputs = Some(HostInputs::from_frame(frame));
    }
}

#[test]
fn host_start_fast_fold_uses_publications_before_the_first_produce() {
    let fixture = FetchFixture::new();
    let session = "rimz-test";
    let producer = PaneProducer::seeded(&fixture.runtime, session);
    assert!(HostInputs::seeded(&producer, session, None).is_none());

    let frame =
        crate::sidebar::frame::assemble_frame(vec![pane("terminal_7", "tab_1", false)], 1, session);
    let projection = serde_json::from_value(serde_json::json!({
        "session_name": session,
        "wiring": { "kinds": ["codex"], "default_models": {} },
        "inputs": { "by_kind": {} },
        "observations": []
    }))
    .unwrap();
    std::fs::write(
        fixture.runtime.pane_frame_path(),
        serde_json::to_vec(&frame).unwrap(),
    )
    .unwrap();
    std::fs::write(
        fixture.runtime.agent_projection_path(),
        serde_json::to_vec(&projection).unwrap(),
    )
    .unwrap();
    let producer = PaneProducer::seeded(&fixture.runtime, session);
    let last_projection =
        crate::sidebar::agent_projection::read_publication(&fixture.runtime, session);
    fixture.host_data.lock().unwrap().inputs =
        HostInputs::seeded(&producer, session, last_projection.clone());
    assert!(
        fixture.host_data.lock().unwrap().inputs.is_some(),
        "a previous publication seeds both host folds before the first produce"
    );
    let mut worker = FetchWorker::new(
        test_config(fixture.workspace_id.clone()),
        fixture.runtime.clone(),
        crate::diag::DiagSink::disabled(),
        producer,
        last_projection,
    );
    assert_eq!(worker.last_projection, Some(projection));
    worker.producer_cadence.last_attempt = Some(Instant::now() + Duration::from_secs(3600));
    worker.session_listed = |_, _| panic!("the fast fold must not query a mux");
    std::fs::remove_file(fixture.runtime.pane_frame_path()).unwrap();
    std::fs::remove_file(fixture.runtime.agent_projection_path()).unwrap();

    let outcomes = fixture.run_with(FetchRequest::default(), &mut worker);
    assert_eq!(outcomes.len(), 1, "the first produce is deferred");
    let folded = snapshot(&outcomes[0]);
    assert_eq!(folded.panes_produced_at_ms, Some(1));
    assert!(
        folded.rows().next().is_some(),
        "the first fast fold has rows"
    );
    assert!(folded.wired_kinds.contains(&"codex".to_owned()));
    assert_eq!(
        fixture
            .host_data
            .lock()
            .unwrap()
            .views
            .lock()
            .unwrap()
            .fold
            .as_ref()
            .unwrap()
            .frame
            .as_deref(),
        Some(&frame)
    );
}

#[test]
fn a_delivered_fold_is_already_the_hosts_latest_fold() {
    let fixture = FetchFixture::new();
    let (wake, wakes) = wake_channel();
    let (mut sink, reader) = single_sink(fixture.host_data.clone(), wake);
    for (index, name) in ["first", "second"].into_iter().enumerate() {
        let mut snapshot = crate::sidebar_pane::app::fixtures::snapshot(&workspace());
        snapshot.display_name = name.into();
        sink.publish(FetchUpdate::Fold {
            fold: Arc::new(frameless(snapshot)),
            phase: FetchPhase::Final,
            source: SnapshotSource::Produced,
        });
        assert_eq!(
            wakes.recv_timeout(Duration::from_secs(1)).unwrap(),
            Wakeup::Snapshot
        );
        let views = reader.views.lock().unwrap();
        assert_eq!(views.revision, index as u64 + 1);
        let latest = views
            .latest
            .since(index as u64)
            .pop()
            .unwrap()
            .into_parts()
            .0;
        let FetchUpdate::Fold { fold, .. } = latest else {
            panic!("room fold")
        };
        assert_eq!(fold.workspace.snapshot().display_name, name);
        assert!(
            Arc::ptr_eq(views.fold.as_ref().unwrap(), &fold),
            "a woken painter must read the fold that woke it, not its predecessor"
        );
    }
}

#[test]
fn fast_fold_retains_its_fold_for_new_attachments() {
    let fixture = FetchFixture::new();
    fixture.write_pane_frame();
    let mut rollup = SidebarSnapshot::build(
        fixture.workspace_id.clone(),
        Vec::new(),
        jiff::Timestamp::now(),
    );
    rollup.display_name = "first".to_owned();
    rollup.reflects_log = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    std::fs::write(
        &fixture.state.latest_snapshot,
        serde_json::to_vec(&rollup).unwrap(),
    )
    .unwrap();
    let mut worker = fixture.worker();
    let (mut sink, rx) = single_sink(fixture.host_data.clone(), wake_channel().0);
    worker.run_cycle(&fixture.state, FetchRequest::default(), &mut sink);
    assert!(matches!(
        rx.recv().unwrap().into_parts().0,
        FetchUpdate::Snapshot { .. }
    ));
    assert_eq!(sink.cycle.len(), 1);
    assert_eq!(
        sink.host_data
            .lock()
            .unwrap()
            .views
            .lock()
            .unwrap()
            .fold
            .as_ref()
            .unwrap()
            .workspace
            .snapshot()
            .display_name,
        "first"
    );

    rollup.display_name = "second-and-longer".to_owned();
    std::fs::write(
        &fixture.state.latest_snapshot,
        serde_json::to_vec(&rollup).unwrap(),
    )
    .unwrap();
    worker.run_cycle(&fixture.state, FetchRequest::default(), &mut sink);
    assert!(matches!(
        rx.recv().unwrap().into_parts().0,
        FetchUpdate::Snapshot { .. }
    ));
    assert_eq!(
        sink.host_data
            .lock()
            .unwrap()
            .views
            .lock()
            .unwrap()
            .fold
            .as_ref()
            .unwrap()
            .workspace
            .snapshot()
            .display_name,
        "second-and-longer",
    );
    sink.publish(FetchUpdate::Failed {
        error: "no fold".into(),
    });
    assert_eq!(
        sink.host_data
            .lock()
            .unwrap()
            .views
            .lock()
            .unwrap()
            .fold
            .as_ref()
            .unwrap()
            .workspace
            .snapshot()
            .display_name,
        "second-and-longer",
        "a failure preserves the last successful seed"
    );
}

#[test]
fn fast_fold_posts_frameless_rollup_before_first_publish() {
    let fixture = FetchFixture::new();
    let mut rollup = SidebarSnapshot::build(
        fixture.workspace_id.clone(),
        Vec::new(),
        jiff::Timestamp::now(),
    );
    rollup.display_name = "cold-room".to_owned();
    rollup.reflects_log = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    std::fs::write(
        &fixture.state.latest_snapshot,
        serde_json::to_vec(&rollup).unwrap(),
    )
    .unwrap();

    let mut outcomes = fixture.run(FetchRequest::default());

    assert_eq!(outcomes.len(), 1);
    let outcome = outcomes.pop().unwrap();
    assert!(outcome.is_final());
    let folded = snapshot(&outcome);
    assert_eq!(folded.display_name, "cold-room");
    assert_eq!(folded.panes_produced_at_ms, None);
    assert!(
        folded.worktree_groups.is_empty(),
        "frameless folds carry rollup metadata but admit no pane cards"
    );
}

#[test]
fn fast_fold_miss_posts_the_rollup_error_as_the_final_outcome() {
    let fixture = FetchFixture::new();
    std::fs::create_dir_all(&fixture.state.events_log).unwrap();
    let mut outcomes = fixture.run(FetchRequest::default());

    assert_eq!(outcomes.len(), 1);
    let outcome = outcomes.pop().unwrap();
    assert!(outcome.is_final());
    let reason = match &outcome {
        FetchUpdate::Failed { error, .. } => error,
        _ => panic!("expected failed cache read"),
    };
    assert!(
        reason.contains(&fixture.state.events_log.display().to_string()),
        "the outcome names the unreadable path, got: {reason}"
    );
}

#[test]
fn fetch_handle_holds_due_deadlines_until_completion() {
    let (tx, rx) = std::sync::mpsc::channel();
    let completion: Arc<AtomicBool> = Arc::default();
    let mut handle = FetchHandle::new(tx, Arc::clone(&completion));
    handle.request_if_idle(FetchRequest::default());
    assert!(rx.try_recv().is_ok());
    assert!(!handle.completed());
    handle.request_if_idle(FetchRequest::fresh_panes());
    assert!(rx.try_recv().is_err());
    handle.request(FetchRequest::fresh_panes());
    assert!(rx.try_recv().unwrap().is_fresh_panes());

    completion.store(true, Ordering::Relaxed);
    assert!(handle.completed());
    assert!(!handle.completed());
    handle.request_if_idle(FetchRequest::fresh_panes());
    assert!(rx.try_recv().unwrap().is_fresh_panes());

    let (tx, rx) = std::sync::mpsc::channel();
    drop(rx);
    let mut closed = FetchHandle::new(tx, Arc::default());
    closed.request(FetchRequest::default());
    assert!(!closed.awaiting);
    closed.request_if_idle(FetchRequest::fresh_panes());
    assert!(!closed.awaiting);
}

#[test]
fn one_cycle_projects_each_pane_excluding_only_itself() {
    let fixture = FetchFixture::new();
    let panes = ["terminal_7", "terminal_8", "terminal_9"];
    let frame = crate::sidebar::frame::assemble_frame(
        panes.iter().map(|raw| pane(raw, "tab_1", false)).collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    fixture.host_data.lock().unwrap().inputs = Some(HostInputs::from_frame(frame));

    let subscribers = Subscribers::default();
    let mut updates = Vec::new();
    for (id, own) in [("01", "terminal_7"), ("02", "terminal_8")] {
        let (subscriber, rx) = subscriber(&fixture, id, own, 2, wake_channel().0);
        subscribe(&subscribers, subscriber);
        updates.push((own, rx));
    }
    let mut worker = fixture.worker();
    let mut sink = ResultSink::shared(subscribers, fixture.host_data.clone());
    worker.run_cycle(&fixture.state, FetchRequest::default(), &mut sink);

    for (own, rx) in updates {
        let mut config = pane_config(worker.config.clone());
        config.own_pane = Some(PaneId::from_parts(MuxName::Zellij, own));
        let mut renderer = consumer(config, fixture.runtime.clone(), rx);
        renderer.on_snapshot();
        assert_eq!(
            renderer
                .views
                .mirror()
                .own_view
                .as_ref()
                .unwrap()
                .sibling_count,
            2
        );
        let shown: Vec<String> = renderer
            .views
            .mirror()
            .rows()
            .filter_map(|row| row.pane.as_ref().map(|pane| pane.pane_id.raw().to_owned()))
            .collect();
        let expected: Vec<&str> = panes.iter().copied().filter(|raw| *raw != own).collect();
        assert_eq!(
            shown, expected,
            "the renderer of {own} excludes only itself"
        );
    }
}

#[test]
fn a_failed_final_is_not_replaced_by_its_projected_interim() {
    let fixture = FetchFixture::new();
    let frame = crate::sidebar::frame::assemble_frame(Vec::new(), 0, "rimz-test");
    let mut cycles =
        IdleCycleFixture::new(fixture.state.clone(), fixture.runtime.clone(), frame, 1);
    cycles.enable_renderer_apply();
    let snapshot = crate::sidebar_pane::app::fixtures::snapshot(&fixture.workspace_id);
    cycles.sink.context = Some(Arc::new(FoldShared::default()));
    cycles.sink.publish(FetchUpdate::Fold {
        fold: Arc::new(frameless(snapshot)),
        phase: FetchPhase::Interim,
        source: SnapshotSource::Cached,
    });
    cycles.sink.publish(FetchUpdate::Failed {
        error: "producer failed after the interim fold".into(),
    });
    let views = cycles.sink.host_data.lock().unwrap().views.clone();
    let renderer = &mut cycles.renderers[0];
    renderer.on_snapshot();
    assert_eq!(
        views
            .lock()
            .unwrap()
            .peek(&PaneId::from_parts(MuxName::Zellij, "sidebar_0"))
            .unwrap()
            .1
            .health
            .failure_streak,
        1,
        "the latest final failure must reach the view despite the shared fold context"
    );
}

#[test]
fn hidden_lazy_folds_project_only_on_reveal_and_count_only_takes() {
    let fixture = FetchFixture::new();
    let frame = crate::testkit::fleet::seed_scale_room(
        &fixture.state,
        &fixture.runtime,
        crate::testkit::fleet::ScaleRoom {
            agents: 2,
            panes: 3,
            ended_sessions: 0,
            orphan_contexts: 0,
        },
    )
    .unwrap();
    let mut panes = frame.to_pane_refs();
    let mut sidebars = Vec::new();
    for (index, pane) in panes.iter_mut().enumerate() {
        pane.view_id = Some(format!("tab_{index}"));
        let mut sidebar = pane.clone();
        sidebar.pane_id = PaneId::from_parts(MuxName::Zellij, format!("sidebar_{index}"));
        sidebar.command = Some("rimz-sidebar".into());
        sidebars.push(sidebar);
    }
    panes.extend(sidebars);
    let mut frame = crate::sidebar::frame::assemble_frame(panes, frame.produced_at_ms, "rimz-test");
    frame.viewed_panes = vec![PaneId::from_parts(MuxName::Zellij, "terminal_1")];
    let mut cycles =
        IdleCycleFixture::new(fixture.state.clone(), fixture.runtime.clone(), frame, 3);
    cycles.enable_renderer_apply();
    cycles.run_cycle();
    let hidden = cycles
        .renderers
        .iter()
        .enumerate()
        .find_map(|(index, r)| {
            r.views
                .mirror()
                .own_view
                .as_ref()
                .is_some_and(|view| {
                    !view
                        .working_pane_ids
                        .iter()
                        .any(|pane| r.views.mirror().viewed_panes.contains(pane))
                })
                .then_some(index)
        })
        .expect("a hidden pane");
    assert_eq!(hidden, 0, "the eldest pane is hidden too");
    assert_eq!(
        cycles.renderers[hidden].pane.projections, 1,
        "a hidden pane projects on its first reduction"
    );
    let views = cycles.sink.host_data.lock().unwrap().views.clone();
    let before = {
        let guard = views.lock().unwrap();
        let counts = &guard.reductions;
        (counts.content, counts.unchanged)
    };
    for _ in 0..3 {
        cycles.run_cycle();
    }
    assert_eq!(
        cycles.renderers[hidden].pane.projections, 1,
        "hidden equal folds never project"
    );
    let guard = views.lock().unwrap();
    let counts = &guard.reductions;
    assert_eq!(
        (counts.content - before.0) + (counts.unchanged - before.1),
        3,
        "only the watched renderer consumes each cycle"
    );
    drop(guard);
    let own = cycles.renderers[hidden]
        .views
        .mirror()
        .own_view
        .as_ref()
        .unwrap()
        .working_pane_ids[0]
        .clone();
    {
        let mut data = cycles.sink.host_data.lock().unwrap();
        Arc::make_mut(&mut data.inputs.as_mut().unwrap().frame).viewed_panes = vec![own.clone()];
    }
    cycles.run_cycle();
    assert_eq!(
        cycles.renderers[hidden].pane.projections, 2,
        "reveal projects the unread fold once"
    );
    assert!(
        cycles.renderers[hidden]
            .views
            .mirror()
            .viewed_panes
            .contains(&own)
    );
}

#[test]
fn one_cycle_shares_rosters_and_cards_across_renderers() {
    let fixture = FetchFixture::new();
    let mut agent_pane = pane("terminal_9", "tab_1", false);
    agent_pane.command = Some("codex".into());
    let frame = crate::sidebar::frame::assemble_frame(
        vec![
            pane("terminal_7", "tab_1", false),
            pane("terminal_8", "tab_1", false),
            agent_pane,
        ],
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    let projection = serde_json::from_value(serde_json::json!({
        "session_name": "rimz-test",
        "wiring": { "kinds": ["codex"], "default_models": {} },
        "inputs": { "by_kind": {} },
        "observations": []
    }))
    .unwrap();
    let mut producer = PaneProducer::seeded(&fixture.runtime, "rimz-test");
    producer.seed_test_frame(Some(Arc::new(frame)));
    fixture.host_data.lock().unwrap().inputs =
        HostInputs::seeded(&producer, "rimz-test", Some(projection));
    let subscribers = Subscribers::default();
    let (first, first_rx) = subscriber(&fixture, "01", "terminal_7", 60, wake_channel().0);
    let (second, second_rx) = subscriber(&fixture, "02", "terminal_8", 60, wake_channel().0);
    subscribe(&subscribers, first);
    subscribe(&subscribers, second);
    let mut worker = fixture.worker();
    let mut sink = ResultSink::shared(subscribers, fixture.host_data.clone());
    worker.run_cycle(&fixture.state, FetchRequest::default(), &mut sink);
    let first = first_rx.try_recv().unwrap();
    let second = second_rx.try_recv().unwrap();
    let host = fixture.host_data.lock().unwrap();
    let views = host.views.lock().unwrap();
    let remembered = views.fold.as_ref().unwrap().workspace.snapshot();
    assert!(remembered.rows().any(|row| row.is_agent()));
    for projected in [snapshot(&first), snapshot(&second)] {
        assert!(Arc::ptr_eq(&projected.agents, &remembered.agents));
        assert!(Arc::ptr_eq(
            &projected.fenced_sessions,
            &remembered.fenced_sessions
        ));
        for row in projected.rows().filter(|row| row.is_agent()) {
            let prior = remembered.rows().find(|prior| prior.id == row.id).unwrap();
            let (
                crate::store::snapshot::RowCard::Agent(card),
                crate::store::snapshot::RowCard::Agent(prior),
            ) = (&row.card, &prior.card)
            else {
                panic!("agent cards")
            };
            assert!(Arc::ptr_eq(card, prior));
        }
    }
}

#[test]
fn a_stalled_subscriber_retains_only_the_latest_projected_fold() {
    let fixture = FetchFixture::new();
    let subscribers = Subscribers::default();
    let (stalled, stalled_rx) = subscriber(&fixture, "01", "terminal_7", 60, wake_channel().0);
    let (advancing, advancing_rx) = subscriber(&fixture, "02", "terminal_8", 60, wake_channel().0);
    subscribe(&subscribers, stalled);
    subscribe(&subscribers, advancing);
    let mut worker = fixture.worker();
    let mut sink = ResultSink::shared(subscribers, fixture.host_data.clone());
    let now = crate::utils::time::unix_now_ms();
    for fold in 0..64 {
        tick_clock();
        let newest = format!("terminal_{}", 100 + fold);
        let frame = crate::sidebar::frame::assemble_frame(
            ["terminal_7", "terminal_8", &newest]
                .into_iter()
                .map(|id| pane(id, "tab_1", false))
                .collect(),
            now + fold,
            "rimz-test",
        );
        fixture.host_data.lock().unwrap().inputs = Some(HostInputs::from_frame(frame));
        worker.run_cycle(&fixture.state, FetchRequest::default(), &mut sink);
        let updates = advancing_rx.take();
        assert_eq!(updates.len(), 1, "unrelated subscriber keeps advancing");
        assert_eq!(shown_panes(&updates[0]), ["terminal_7", &newest]);
    }
    assert_eq!(
        stalled_rx
            .views
            .lock()
            .unwrap()
            .latest
            .since(stalled_rx.cursor.lock().unwrap().revision)
            .len(),
        1
    );
    let updates: Vec<_> = stalled_rx.try_iter().collect();
    assert_eq!(
        updates.len(),
        1,
        "a stalled consumer retains one snapshot, not one per fold"
    );
    assert_eq!(shown_panes(&updates[0]), ["terminal_8", "terminal_163"]);
    let (_, context) = updates.into_iter().next().unwrap().into_parts();
    assert!(
        Arc::ptr_eq(&context.unwrap(), sink.context.as_ref().unwrap()),
        "the newest snapshot keeps its own shared context"
    );
}

#[test]
fn shared_observation_extracts_common_once_and_keeps_each_own_view() {
    use crate::sidebar::observe;
    let fixture = FetchFixture::new();
    let frame = crate::sidebar::frame::assemble_frame(
        (0..40)
            .flat_map(|n| {
                [
                    pane(&format!("terminal_{n}"), &format!("tab_{n}"), false),
                    pane(&format!("terminal_{}", n + 100), &format!("tab_{n}"), false),
                ]
            })
            .collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    fixture.host_data.lock().unwrap().inputs = Some(HostInputs::from_frame(frame));
    let subscribers = Subscribers::default();
    let mut receivers = Vec::new();
    for n in 0..40 {
        let (sub, rx) = subscriber(
            &fixture,
            &format!("{n:02}"),
            &format!("terminal_{n}"),
            2,
            wake_channel().0,
        );
        receivers.push((sub.clone(), rx));
        subscribe(&subscribers, sub);
    }
    let (tx, _rx) = std::sync::mpsc::sync_channel(64);
    let mut worker = fixture.worker();
    worker.observer = Some(observe::RoomObserver::new(tx.clone(), Arc::default()));
    let mut sink = ResultSink::shared(subscribers, fixture.host_data.clone());
    observe::take_extractions();
    worker.run_cycle(&fixture.state, FetchRequest::default(), &mut sink);
    assert_eq!(
        observe::take_extractions(),
        (1, 0),
        "one common extraction for forty projections"
    );
    let mut common = None;
    for (n, (sub, rx)) in receivers.into_iter().enumerate() {
        let update = rx.try_recv().unwrap();
        let FetchUpdate::Shared { context, .. } = &update else {
            panic!("shared context missing")
        };
        let sig = context.observation.as_ref().unwrap();
        if let Some(first) = &common {
            assert!(Arc::ptr_eq(first, sig));
        } else {
            common = Some(sig.clone());
        }
        let mut config = pane_config(worker.config.clone());
        config.instance_id = sub.instance_id;
        config.own_pane = sub.own_pane;
        let own = config.own_pane.clone().unwrap();
        let mut state = super::super::loop_state::LoopState::new(
            crate::sidebar_pane::app::clock::LoopClock::wall(),
            config,
            None,
            fixture.runtime.clone(),
            PathBuf::from("missing.sock"),
            crate::diag::DiagSink::disabled(),
            FetchHandle::new(std::sync::mpsc::channel().0, Arc::default()),
            None,
            tx.clone(),
            crate::sidebar_pane::pixel::PixelRenderCaps::default(),
            None,
        );
        state.apply_latest_snapshot(update);
        assert!(
            !state
                .views
                .mirror()
                .rows()
                .any(|row| row.pane.as_ref().is_some_and(|pane| pane.pane_id == own))
        );
        assert_eq!(
            state
                .views
                .mirror()
                .own_view
                .as_ref()
                .unwrap()
                .working_pane_ids,
            vec![PaneId::from_parts(
                MuxName::Zellij,
                format!("terminal_{}", n + 100)
            )]
        );
    }
    assert_eq!(
        observe::take_extractions(),
        (0, 40),
        "only own parts are extracted per pane"
    );
}

#[test]
fn fold_file_reads_are_shared_across_forty_attachment_threads() {
    use crate::mux::focus_anchor;
    use crate::sidebar::body_filter;
    let fixture = FetchFixture::new();
    let frame = crate::sidebar::frame::assemble_frame(
        (0..40)
            .flat_map(|n| {
                [
                    pane(&format!("terminal_{n}"), &format!("tab_{n}"), false),
                    pane(&format!("terminal_{}", n + 100), &format!("tab_{n}"), false),
                ]
            })
            .collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    fixture.host_data.lock().unwrap().inputs = Some(HostInputs::from_frame(frame));
    let subscribers = Subscribers::default();
    let mut worker = fixture.worker();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let mut panes = Vec::new();
    for n in 0..40 {
        let (sub, rx) = subscriber(
            &fixture,
            &format!("{n:02}"),
            &format!("terminal_{n}"),
            2,
            wake_channel().0,
        );
        let mut config = pane_config(worker.config.clone());
        config.instance_id = sub.instance_id.clone();
        config.own_pane = sub.own_pane.clone();
        subscribe(&subscribers, sub);
        let runtime = fixture.runtime.clone();
        let ready = ready_tx.clone();
        panes.push(std::thread::spawn(move || {
            let mut state = super::super::loop_state::LoopState::new(
                crate::sidebar_pane::app::clock::LoopClock::wall(),
                config,
                None,
                runtime,
                PathBuf::from("missing.sock"),
                crate::diag::DiagSink::disabled(),
                FetchHandle::new(std::sync::mpsc::channel().0, Arc::default()),
                None,
                std::sync::mpsc::sync_channel(64).0,
                crate::sidebar_pane::pixel::PixelRenderCaps::default(),
                None,
            );
            focus_anchor::take_reads();
            body_filter::take_reads();
            ready.send(()).unwrap();
            drop(ready);
            state.apply_latest_snapshot(rx.recv().unwrap());
            (focus_anchor::take_reads(), body_filter::take_reads())
        }));
    }
    drop(ready_tx);
    for _ in 0..40 {
        ready_rx.recv().unwrap();
    }
    focus_anchor::take_reads();
    body_filter::take_reads();
    let mut sink = ResultSink::shared(subscribers, fixture.host_data.clone());
    worker.run_cycle(&fixture.state, FetchRequest::default(), &mut sink);
    let mut reads = (focus_anchor::take_reads(), body_filter::take_reads());
    for pane in panes {
        let own = pane.join().unwrap();
        reads.0 += own.0;
        reads.1 += own.1;
    }
    assert_eq!(
        reads.0, 1,
        "focus-anchor observation is one room file read per fold"
    );
    assert_eq!(
        reads.1, 1,
        "body-filter observation is one room file read per fold"
    );
}

fn subscriber(
    fixture: &FetchFixture,
    id: &str,
    own: &str,
    tick_seconds: u64,
    wake: SyncSender<Wakeup>,
) -> (Subscriber, Replay) {
    let subscriber = Subscriber {
        instance_id: SidebarInstanceId::parse(&format!("sb_{id:0>32}")).unwrap(),
        own_pane: Some(PaneId::from_parts(MuxName::Zellij, own)),
        tick_seconds,
        completion: Arc::default(),
        wake,
    };
    let reader = Replay::new(&fixture.host_data, &subscriber);
    (subscriber, reader)
}

fn subscribe(subscribers: &Subscribers, subscriber: Subscriber) {
    subscribers
        .lock()
        .unwrap()
        .insert(subscriber.instance_id.as_str().to_owned(), subscriber);
}

fn unsubscribe(subscribers: &Subscribers, id: &str) {
    subscribers.lock().unwrap().remove(&format!("sb_{id:0>32}"));
}

fn shown_panes(update: &FetchUpdate) -> Vec<String> {
    snapshot(update)
        .rows()
        .filter_map(|row| row.pane.as_ref().map(|pane| pane.pane_id.raw().to_owned()))
        .collect()
}

/// Let successive fixture publications observe distinct clock ticks.
fn tick_clock() {
    std::thread::sleep(Duration::from_millis(2));
}

#[test]
fn fetch_floor_merges_requests_after_the_previous_cycle_returns() {
    let floor = Duration::from_millis(150);
    let (tx, rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    tx.send(FetchRequest::default()).unwrap();
    let queued = FetchRequest {
        min_pane_cache_ms: Some(20),
        ..FetchRequest::fresh_panes()
    };
    std::thread::scope(|scope| {
        let mut gap_tx = Some(tx.clone());
        let worker = scope.spawn(move || {
            let mut cycles = Vec::new();
            drive(
                &rx,
                floor,
                || None,
                |request| {
                    cycles.push((Instant::now(), request));
                    if let Some(gap_tx) = gap_tx.take() {
                        // A start-to-start floor would already have expired.
                        std::thread::sleep(floor + Duration::from_millis(10));
                        // Queued before the return, so they merge however late
                        // either thread is scheduled.
                        gap_tx.send(FetchRequest::default()).unwrap();
                        gap_tx.send(queued).unwrap();
                        finished_tx.send(Instant::now()).unwrap();
                    }
                },
            );
            cycles
        });
        let returned_at = finished_rx.recv().unwrap();
        tx.send(FetchRequest::default()).unwrap();
        drop(tx);

        let cycles = worker.join().unwrap();
        assert!(cycles[1].0.duration_since(returned_at) >= floor);
        assert!(
            (2..=3).contains(&cycles.len()),
            "queued gap requests merge into one fold"
        );
        assert_eq!(cycles[1].1.mode, FetchMode::FreshPanes);
        assert_eq!(cycles[1].1.min_pane_cache_ms, Some(20));
    });
}

#[test]
fn fetch_floor_does_not_delay_a_request_after_a_long_idle() {
    let floor = Duration::from_millis(600);
    let (tx, rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    tx.send(FetchRequest::default()).unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let mut cycles = Vec::new();
            drive(
                &rx,
                floor,
                || None,
                |request| {
                    cycles.push((Instant::now(), request));
                    if cycles.len() == 1 {
                        finished_tx.send(()).unwrap();
                    }
                },
            );
            cycles
        });
        finished_rx.recv().unwrap();
        // Past the rest; a wait counted from receipt would still cost a whole
        // floor, far more than scheduling jitter.
        std::thread::sleep(floor + Duration::from_millis(50));
        let sent_at = Instant::now();
        tx.send(FetchRequest::default()).unwrap();
        drop(tx);
        let cycles = worker.join().unwrap();
        assert_eq!(cycles.len(), 2);
        assert!(cycles[1].0.duration_since(sent_at) < floor);
    });
}

#[test]
fn fetch_floor_has_no_mode_bypass() {
    let floor = Duration::from_millis(40);
    for request in [
        FetchRequest::default(),
        FetchRequest::fresh_panes(),
        FetchRequest::hard_refresh(),
    ] {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(FetchRequest::default()).unwrap();
        let mut sender = Some(tx);
        let mut returned_at = None;
        let mut cycles = Vec::new();
        drive(
            &rx,
            floor,
            || None,
            |cycle| {
                cycles.push((Instant::now(), cycle));
                if let Some(tx) = sender.take() {
                    tx.send(request).unwrap();
                    returned_at = Some(Instant::now());
                }
            },
        );
        assert_eq!(cycles.len(), 2);
        assert!(
            cycles[1].0.duration_since(returned_at.unwrap()) >= floor,
            "no bypass for {request:?}"
        );
        assert_eq!(cycles[1].1.mode, request.mode);
    }
}

#[test]
fn fetch_floor_finishes_the_batch_when_senders_disconnect_during_the_gap() {
    let floor = Duration::from_millis(150);
    let (tx, rx) = std::sync::mpsc::sync_channel(0);
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            let mut cycles = Vec::new();
            drive(
                &rx,
                floor,
                || None,
                |request| {
                    cycles.push((Instant::now(), request));
                    if cycles.len() == 1 {
                        finished_tx.send(Instant::now()).unwrap();
                    }
                },
            );
            cycles
        });
        tx.send(FetchRequest::default()).unwrap();
        let returned_at = finished_rx.recv().unwrap();
        // The rendezvous proves the worker holds this batch before disconnect.
        tx.send(FetchRequest::hard_refresh()).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        drop(tx);
        let cycles = worker.join().unwrap();
        assert_eq!(
            cycles.len(),
            2,
            "disconnect must not discard the held batch"
        );
        assert!(cycles[1].0.duration_since(returned_at) >= floor);
        assert_eq!(cycles[1].1.mode, FetchMode::HardRefresh);
    });
}

#[test]
fn drive_backstop_runs_an_ordinary_cycle_after_an_idle_tick() {
    let tick = Duration::from_millis(30);
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(FetchRequest::default()).unwrap();
    let mut sender = Some(tx);
    let mut returned_at = None;
    let mut cycles = Vec::new();
    drive(
        &rx,
        Duration::ZERO,
        || Some(tick),
        |request| {
            cycles.push((Instant::now(), request));
            if cycles.len() == 1 {
                returned_at = Some(Instant::now());
            } else {
                drop(sender.take());
            }
        },
    );
    assert_eq!(cycles.len(), 2);
    assert_eq!(cycles[1].1.mode, FetchMode::Normal);
    assert!(cycles[1].0.duration_since(returned_at.unwrap()) >= tick);
}

#[test]
fn drive_backstop_waits_without_subscribers() {
    let tick = Duration::from_millis(30);
    let (tx, rx) = std::sync::mpsc::channel();
    let (started, first) = std::sync::mpsc::channel();
    tx.send(FetchRequest::default()).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            first.recv().unwrap();
            std::thread::sleep(tick * 3);
            drop(tx);
        });
        let mut cycles = 0;
        drive(
            &rx,
            Duration::ZERO,
            || None,
            |_| {
                cycles += 1;
                if cycles == 1 {
                    started.send(()).unwrap();
                }
            },
        );
        assert_eq!(cycles, 1);
    });
}

#[test]
fn drive_backstop_waits_on_an_unrepresentable_tick() {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(FetchRequest::default()).unwrap();
    let mut sender = Some(tx);
    let mut cycles = 0;
    drive(
        &rx,
        Duration::ZERO,
        || Some(Duration::MAX),
        |_| {
            cycles += 1;
            drop(sender.take());
        },
    );
    assert_eq!(cycles, 1);
}

#[test]
fn drive_backstop_restarts_after_each_cycle() {
    let tick = Duration::from_millis(30);
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(FetchRequest::default()).unwrap();
    let sender = std::cell::RefCell::new(Some(tx));
    let completed = std::cell::Cell::new(0);
    let mut sent_during_idle = false;
    let mut returned_at = None;
    let mut cycles = Vec::new();
    drive(
        &rx,
        Duration::ZERO,
        || {
            if completed.get() == 1 && !sent_during_idle {
                // Queue inside the injected wait, avoiding a race with a sender thread's scheduler.
                std::thread::sleep(tick / 2);
                sender
                    .borrow()
                    .as_ref()
                    .unwrap()
                    .send(FetchRequest::fresh_panes())
                    .unwrap();
                sent_during_idle = true;
            }
            Some(tick)
        },
        |request| {
            cycles.push((Instant::now(), request));
            completed.set(cycles.len());
            if cycles.len() == 2 {
                returned_at = Some(Instant::now());
            }
            if cycles.len() == 3 {
                drop(sender.borrow_mut().take());
            }
        },
    );
    assert_eq!(cycles.len(), 3);
    assert_eq!(cycles[1].1.mode, FetchMode::FreshPanes);
    assert_eq!(cycles[2].1.mode, FetchMode::Normal);
    assert!(cycles[2].0.duration_since(returned_at.unwrap()) >= tick);
}

#[test]
fn drive_backstop_rereads_membership_after_a_timeout() {
    let tick = Duration::from_millis(30);
    let (tx, rx) = std::sync::mpsc::channel();
    let (started, first) = std::sync::mpsc::channel();
    tx.send(FetchRequest::default()).unwrap();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            first.recv().unwrap();
            std::thread::sleep(tick * 3);
            drop(tx);
        });
        let mut waits = 0;
        let mut cycles = 0;
        drive(
            &rx,
            Duration::ZERO,
            || {
                waits += 1;
                (waits == 1).then_some(tick)
            },
            |_| {
                cycles += 1;
                if cycles == 1 {
                    started.send(()).unwrap();
                }
            },
        );
        assert_eq!(cycles, 1, "a stale deadline must not fold an empty room");
        assert!(waits >= 2, "membership is read again at the timeout");
    });
}

#[test]
fn staggered_requests_behind_one_fold_cost_at_most_two_folds() {
    staggered_requests(false);
}

#[test]
fn staggered_idle_requests_behind_one_fold_cost_at_most_two_folds() {
    staggered_requests(true);
}

fn staggered_requests(idle: bool) {
    const PANES: usize = 8;
    let fixture = FetchFixture::new();
    let panes: Vec<String> = (1..=PANES).map(|id| format!("terminal_{id}")).collect();
    let frame = if idle {
        crate::testkit::fleet::seed_scale_room(
            &fixture.state,
            &fixture.runtime,
            crate::testkit::fleet::ScaleRoom {
                agents: PANES,
                panes: PANES,
                ended_sessions: 0,
                orphan_contexts: 0,
            },
        )
        .unwrap()
    } else {
        crate::sidebar::frame::assemble_frame(
            panes.iter().map(|id| pane(id, "tab_1", false)).collect(),
            crate::utils::time::unix_now_ms(),
            "rimz-test",
        )
    };
    let panes: Vec<_> = frame
        .to_pane_refs()
        .iter()
        .map(|pane| pane.pane_id.raw().to_owned())
        .collect();
    fixture.host_data.lock().unwrap().inputs = Some(HostInputs::from_frame(frame));
    let subscribers = Subscribers::default();
    let mut receivers = Vec::new();
    for (index, own) in panes.iter().enumerate() {
        let (subscriber, rx) = subscriber(
            &fixture,
            &(index + 1).to_string(),
            own,
            60,
            wake_channel().0,
        );
        subscribe(&subscribers, subscriber);
        receivers.push(rx);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(FetchRequest::default()).unwrap();
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = reads.clone();
    let receivers = Arc::new(receivers);
    let draining = receivers.clone();
    let first_publications = Arc::new(Mutex::new(Vec::new()));
    let captured = first_publications.clone();
    let sink = ResultSink::shared(subscribers, fixture.host_data.clone());
    let mut worker = fixture.worker();
    let mut sender = Some(tx);
    worker.on_read = Some(Box::new(move || {
        let read = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        match read {
            1 => {
                for _ in 0..PANES {
                    sender
                        .as_ref()
                        .unwrap()
                        .send(FetchRequest::default())
                        .unwrap();
                }
            }
            2 => {
                *captured.lock().unwrap() =
                    draining.iter().map(|rx| rx.try_recv().unwrap()).collect();
                sender = None;
            }
            _ => panic!("the staggered burst cost more than two folds"),
        }
    }));
    worker.run_resolving(rx, sink, |_| Ok(fixture.state.clone()));

    assert_eq!(
        reads.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "count only cycles crossing the store-reader boundary"
    );
    let first_publications = std::mem::take(&mut *first_publications.lock().unwrap());
    for ((own, rx), first) in panes.iter().zip(receivers.iter()).zip(first_publications) {
        let updates: Vec<_> = std::iter::once(first).chain(rx.try_iter()).collect();
        assert_eq!(
            updates.len(),
            2,
            "every subscriber receives both real publications"
        );
        for (index, update) in updates.into_iter().enumerate() {
            if idle && index == 1 {
                assert!(
                    update.is_unchanged(),
                    "an unchanged idle fold delivers only clocks"
                );
                assert!(update.is_final());
                continue;
            }
            assert_eq!(
                shown_panes(&update),
                panes
                    .iter()
                    .filter(|pane| *pane != own)
                    .cloned()
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn one_publication_serves_the_renderers_its_cycle_began_with() {
    let fixture = FetchFixture::new();
    let frame = crate::sidebar::frame::assemble_frame(
        ["terminal_7", "terminal_8", "terminal_9"]
            .iter()
            .map(|raw| pane(raw, "tab_1", false))
            .collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    fixture.host_data.lock().unwrap().inputs = Some(HostInputs::from_frame(frame));
    let subscribers = Subscribers::default();
    let (elder_wake, elder_wakes) = wake_channel();
    let (eldest, eldest_rx) = subscriber(&fixture, "01", "terminal_7", 2, elder_wake);
    let (younger_wake, younger_wakes) = wake_channel();
    let (younger, younger_rx) = subscriber(&fixture, "02", "terminal_8", 2, younger_wake);
    subscribe(&subscribers, eldest);
    subscribe(&subscribers, younger);
    let mut worker = fixture.worker();
    let mut sink = ResultSink::shared(subscribers.clone(), fixture.host_data.clone());
    sink.begin_cycle();
    let inputs = fixture.host_data.lock().unwrap().inputs.clone().unwrap();
    let workspace = HostInputs::fold(
        Some(&inputs),
        &mut worker.cursor,
        &fixture.state,
        &fixture.runtime,
    )
    .unwrap();
    let frame = Some(inputs.frame);
    let folded = WorkspaceFold { workspace, frame };

    // The eldest leaves and a new pane arrives between fold and publication.
    unsubscribe(&subscribers, "01");
    let (new_wake, new_wakes) = wake_channel();
    let (newcomer, newcomer_rx) = subscriber(&fixture, "03", "terminal_9", 2, new_wake);
    subscribe(&subscribers, newcomer);
    sink.publish(FetchUpdate::Fold {
        fold: Arc::new(folded),
        phase: FetchPhase::Final,
        source: SnapshotSource::Cached,
    });

    let younger: Vec<_> = younger_rx.try_iter().collect();
    assert_eq!(younger.len(), 1);
    assert_eq!(
        shown_panes(&younger[0]),
        ["terminal_7", "terminal_9"],
        "the younger pane gets its own view, not the departed eldest's"
    );
    let eldest: Vec<_> = eldest_rx.try_iter().collect();
    assert_eq!(shown_panes(&eldest[0]), ["terminal_8", "terminal_9"]);
    assert!(eldest_rx.completion.load(Ordering::Relaxed));
    assert!(younger_rx.completion.load(Ordering::Relaxed));
    assert!(!newcomer_rx.completion.load(Ordering::Relaxed));
    assert_eq!(elder_wakes.try_recv().unwrap(), Wakeup::Snapshot);
    assert_eq!(younger_wakes.try_recv().unwrap(), Wakeup::Snapshot);
    assert!(new_wakes.try_recv().is_err());
    assert_eq!(
        shown_panes(&newcomer_rx.recv().unwrap()),
        ["terminal_7", "terminal_8"]
    );
}

#[test]
fn the_worker_keeps_the_tick_of_its_eldest_renderer() {
    let fixture = FetchFixture::new();
    let subscribers = Subscribers::default();
    let (eldest, _eldest_rx) = subscriber(&fixture, "01", "terminal_7", 60, wake_channel().0);
    let (younger, _younger_rx) = subscriber(&fixture, "02", "terminal_8", 1, wake_channel().0);
    subscribe(&subscribers, eldest);
    subscribe(&subscribers, younger);
    let mut sink = ResultSink::shared(subscribers.clone(), fixture.host_data.clone());

    assert_eq!(data_tick(&sink.cycle[0]), Duration::from_secs(60));
    unsubscribe(&subscribers, "01");
    sink.begin_cycle();
    assert_eq!(
        data_tick(&sink.cycle[0]),
        Duration::from_secs(1),
        "the next eldest's tick takes over"
    );
}

#[test]
fn the_worker_tick_follows_age_not_magnitude() {
    let fixture = FetchFixture::new();
    let subscribers = Subscribers::default();
    let (eldest, _eldest_rx) = subscriber(&fixture, "01", "terminal_7", 1, wake_channel().0);
    let (younger, _younger_rx) = subscriber(&fixture, "02", "terminal_8", 60, wake_channel().0);
    subscribe(&subscribers, eldest);
    subscribe(&subscribers, younger);
    let mut sink = ResultSink::shared(subscribers.clone(), fixture.host_data.clone());

    assert_eq!(data_tick(&sink.cycle[0]), Duration::from_secs(1));
    unsubscribe(&subscribers, "01");
    sink.begin_cycle();
    assert_eq!(data_tick(&sink.cycle[0]), Duration::from_secs(60));
}

#[test]
fn the_worker_witness_is_its_eldest_renderers_pane() {
    let fixture = FetchFixture::new();
    let subscribers = Subscribers::default();
    let (eldest, _eldest_rx) = subscriber(&fixture, "01", "terminal_7", 2, wake_channel().0);
    let (younger, _younger_rx) = subscriber(&fixture, "02", "terminal_8", 2, wake_channel().0);
    subscribe(&subscribers, eldest);
    subscribe(&subscribers, younger);
    let mut sink = ResultSink::shared(subscribers.clone(), fixture.host_data.clone());

    assert_eq!(
        sink.witness(),
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_7"))
    );
    unsubscribe(&subscribers, "01");
    sink.begin_cycle();
    assert_eq!(
        sink.witness(),
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_8")),
        "the next eldest's pane takes over"
    );
}

#[test]
fn a_full_renderer_inbox_holds_neither_the_worker_nor_another_renderer() {
    let fixture = FetchFixture::new();
    let (full, _stalled) = std::sync::mpsc::sync_channel(1);
    full.send(Wakeup::Tick).unwrap();
    let subscribers = Subscribers::default();
    let (eldest, _eldest_rx) = subscriber(&fixture, "01", "terminal_7", 2, wake_channel().0);
    let (stalled, _stalled_rx) = subscriber(&fixture, "02", "terminal_8", 2, full);
    let (other, other_rx) = subscriber(&fixture, "03", "terminal_9", 2, wake_channel().0);
    for subscriber in [eldest, stalled, other] {
        subscribe(&subscribers, subscriber);
    }
    let mut sink = ResultSink::shared(subscribers, fixture.host_data.clone());
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        sink.publish(FetchUpdate::Failed {
            error: "failed fold".into(),
        });
        let _ = done_tx.send(());
    });

    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the publication returns with one inbox full");
    assert!(other_rx.completion.load(Ordering::Relaxed));
    assert!(matches!(
        other_rx
            .views
            .lock()
            .unwrap()
            .latest
            .since(0)
            .pop()
            .unwrap(),
        FetchUpdate::Failed { .. }
    ));
}
