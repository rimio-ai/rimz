use super::*;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::sidebar::notify::{LinkAlert, Notification, NotificationAgent, NotificationKind};
use crate::sidebar_pane::app::fixtures::{pane, workspace};
use crate::sidebar_pane::app::loop_state::LoopState;
use crate::{MuxName, SidebarInstanceId, WorkspaceId};
use std::path::PathBuf;

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
    for pane in &mut panes {
        pane.first_seen_at_ms = Some(frame.produced_at_ms - 60_000);
    }
    let frame =
        crate::sidebar::frame::assemble_frame(panes, frame.produced_at_ms, frame.session_name);
    let mut cycles =
        IdleCycleFixture::new(fixture.state.clone(), fixture.runtime.clone(), frame, 57);
    cycles.enable_renderer_apply();
    cycles.run_cycle();
    cycles.run_cycle();
    assert!(
        cycles
            .renderers
            .iter()
            .all(|(renderer, _)| renderer.content_applications == 1),
        "fixture is idle before the nudge"
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
        cycles.deliver_event(event);
        cycles.run_cycle();
        assert!(
            cycles
                .renderers
                .iter()
                .all(|(renderer, _)| renderer.content_applications == 1),
            "identity-free nudges must not force content onto idle consumers"
        );
    }
}

#[test]
fn fetch_window_records_consumed_deliveries_once_per_window() {
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
    cycles.worker.diag = diag.clone();
    assert_eq!(cycles.run_cycle(), 0);
    assert_eq!(cycles.run_cycle(), 2);
    assert!(
        !diag.log_path().unwrap().exists(),
        "ordinary ticks write no record"
    );
    cycles.worker.window.started -= Duration::from_secs(60);
    assert_eq!(cycles.run_cycle(), 2);
    assert!(
        diag.log_path().unwrap().exists(),
        "the worker must emit its elapsed delivery window"
    );
    let records = diagnostic_events(&diag);
    assert_eq!(records.len(), 1);
    assert!(
        matches!(
            records[0],
            crate::diag::record::DiagEvent::FetchDeliveryStats {
                cycles: 3,
                content_deliveries: 2,
                unchanged_deliveries: 2,
                ..
            }
        ),
        "counts describe consumed deliveries, not sends still waiting in mailboxes"
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
            .last_fold
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
        for (renderer, dispatcher) in &mut renderers {
            let before = renderer.current.now;
            renderer.on_snapshot(dispatcher);
            assert!(
                renderer.current.now > before,
                "clock advances on cycle {cycle}"
            );
            assert!(matches!(
                renderer.current.presence,
                Some(crate::store::snapshot::SidebarPresence::Idle { .. })
            ));
            assert_eq!(
                renderer.content_applications, 1,
                "only the first idle delivery applies content (cycle {cycle})"
            );
        }
    }
}

fn consumer(
    config: ServeConfig,
    runtime: RuntimePaths,
    rx: ResultReceiver,
) -> (LoopState, FetchDispatcher) {
    (
        LoopState::new(
            config,
            None,
            runtime,
            PathBuf::new(),
            crate::diag::DiagSink::disabled(),
            rx,
            None,
            std::sync::mpsc::sync_channel(1).0,
            Default::default(),
            None,
        ),
        FetchDispatcher::new(std::sync::mpsc::channel().0),
    )
}

fn consumers(cycles: &mut IdleCycleFixture) -> Vec<(LoopState, FetchDispatcher)> {
    cycles.sink.begin_cycle();
    cycles
        .receivers
        .drain(..)
        .map(|rx| {
            let subscriber = cycles
                .sink
                .cycle
                .iter()
                .find(|subscriber| subscriber.tx.0.ptr_eq(&Arc::downgrade(&rx.0)))
                .unwrap();
            let mut config = cycles.worker.config.clone();
            config.instance_id = subscriber.instance_id.clone();
            config.own_pane = subscriber.own_pane.clone();
            consumer(config, cycles.worker.runtime.clone(), rx)
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
    )
    .unwrap();
    let mut cycles =
        IdleCycleFixture::new(fixture.state.clone(), fixture.runtime.clone(), frame, 100);
    let mut renderers = consumers(&mut cycles);
    for _ in 0..2 {
        cycles.run_cycle();
        for (renderer, dispatcher) in &mut renderers {
            renderer.on_snapshot(dispatcher);
            assert_eq!(renderer.content_applications, 1);
        }
    }

    let (new_subscriber, rx) = subscriber("99", "sidebar_new", 1, wake_channel().0);
    let mut config = cycles.worker.config.clone();
    config.instance_id = new_subscriber.instance_id.clone();
    config.own_pane = new_subscriber.own_pane.clone();
    subscribe(&cycles.sink.subscribers, new_subscriber);
    renderers.push(consumer(config, fixture.runtime.clone(), rx));
    cycles.run_cycle();
    for (renderer, dispatcher) in &mut renderers {
        renderer.on_snapshot(dispatcher);
        assert_eq!(
            renderer.content_applications, 1,
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
            })).unwrap()).unwrap(),
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
        for (renderer, dispatcher) in &mut renderers {
            renderer.on_snapshot(dispatcher);
            if change == "context" {
                let agent = renderer
                    .current
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
                renderer.content_applications,
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
            let projected = cycles.worker.project_fold(
                WorkspaceFold {
                    workspace: WorkspaceSnapshot(snapshot),
                    frame: Some(frame.clone()),
                },
                &mut cycles.sink,
            );
            cycles.sink.publish(FetchUpdate::Snapshot {
                snapshot: Box::new(projected),
                phase: FetchPhase::Final,
                source: SnapshotSource::Cached,
            });
            for (renderer, dispatcher) in &mut renderers {
                renderer.on_snapshot(dispatcher);
                assert_eq!(renderer.content_applications, step as u64 + 1);
                if presence {
                    assert_eq!(
                        matches!(
                            renderer.current.presence,
                            Some(crate::store::snapshot::SidebarPresence::Idle { .. })
                        ),
                        step == 1
                    );
                } else {
                    assert_eq!(
                        renderer.current.rows().next().unwrap().status(),
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
        crate::sidebar::fuse::fuse_owned(projected, &Default::default(), None, 1);
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
                &format!("{index:02}"),
                &format!("sidebar_{index}"),
                2,
                wake_channel().0,
            );
            subscribe(&subscribers, sub);
            receivers.push(rx);
        }
        let mut worker = fixture.worker();
        let mut sink =
            ResultSink::shared(subscribers, Covered::default(), fixture.host_data.clone());

        let projected = worker.project_fold(
            WorkspaceFold {
                workspace: WorkspaceSnapshot(shared),
                frame: Some(Arc::new(crate::sidebar::frame::assemble_frame(
                    Vec::new(),
                    1,
                    "rimz-test",
                ))),
            },
            &mut sink,
        );
        sink.publish(FetchUpdate::Snapshot {
            snapshot: Box::new(projected),
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
    let (elder, elder_rx) = subscriber("01", "sidebar_1", 2, wake_channel().0);
    let (younger, younger_rx) = subscriber("02", "sidebar_2", 2, wake_channel().0);
    subscribe(&subscribers, elder);
    subscribe(&subscribers, younger);
    let mut sink = ResultSink::shared(subscribers, Covered::default(), fixture.host_data.clone());
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
    let mut projected = worker.project_fold(
        WorkspaceFold {
            workspace: WorkspaceSnapshot(shared),
            frame: None,
        },
        &mut sink,
    );
    for row in Arc::make_mut(&mut Arc::make_mut(&mut projected.worktree_groups)[0].rows) {
        row.unread = false;
    }

    worker.publish_snapshot(
        &fixture.state,
        SnapshotPublication {
            snapshot: projected,
            phase: FetchPhase::Final,
            source: SnapshotSource::Cached,
        },
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
    let (tx, rx) = result_channel();
    let mut sink = ResultSink::new(tx, wake_channel().0, None);
    sink.host_data = host_data;
    worker.run_cycle(state, request, &mut sink);
    drop(sink);
    rx.try_iter().map(|update| update.into_parts().0).collect()
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
    let workspace_id = workspace();
    let mut folded = SidebarSnapshot::build_with_agents(
        workspace_id.clone(),
        Vec::new(),
        jiff::Timestamp::UNIX_EPOCH,
    );
    folded.theme.display.refresh_ms = 250;
    let (tx, rx) = result_channel();
    let mut sink = ResultSink::new(tx, wake_channel().0, Some(50));

    sink.publish(FetchUpdate::Snapshot {
        snapshot: Box::new(folded),
        phase: FetchPhase::Final,
        source: SnapshotSource::Cached,
    });

    let update = rx.recv().expect("published snapshot");
    let snapshot = snapshot(&update);
    assert_eq!(snapshot.theme.display.refresh_ms, 50);
    assert_eq!(snapshot.theme.display.resolved_refresh_ms(), 50);
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
    let (elder, elder_rx) = subscriber("01", "terminal_7", 2, wake_channel().0);
    let (wake, notices) = wake_channel();
    let (younger, younger_rx) = subscriber("02", "terminal_8", 2, wake);
    subscribe(&subscribers, elder);
    subscribe(&subscribers, younger);
    let mut sink = ResultSink::shared(subscribers, Covered::default(), fixture.host_data.clone());
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
    let projected = worker.project_fold(
        WorkspaceFold {
            workspace: WorkspaceSnapshot(shared),
            frame: Some(frame),
        },
        &mut sink,
    );
    worker.publish_snapshot(
        &fixture.state,
        SnapshotPublication {
            snapshot: projected,
            phase: FetchPhase::Final,
            source: SnapshotSource::Cached,
        },
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
            .last_fold
            .as_ref()
            .unwrap()
            .workspace
            .snapshot()
            .rows()
            .any(|row| row.unread),
        "a new attachment gets the post-notification unread fold too"
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
        let (sub, rx) = subscriber(id, own, 2, wake);
        receivers.push((inbox, rx));
        subscribe(&subscribers, sub);
    }
    let mut sink = ResultSink::shared(subscribers, Covered::default(), fixture.host_data.clone());
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
    let projected = worker.project_fold(
        WorkspaceFold {
            workspace: WorkspaceSnapshot(shared),
            frame: Some(frame),
        },
        &mut sink,
    );
    worker.publish_snapshot(
        &fixture.state,
        SnapshotPublication {
            snapshot: projected,
            phase: FetchPhase::Final,
            source: SnapshotSource::Cached,
        },
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

    let config = test_config(workspace_id, SidebarInstanceId::new());
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

fn test_config(workspace_id: WorkspaceId, instance_id: SidebarInstanceId) -> ServeConfig {
    ServeConfig {
        workspace_id,
        mux: MuxName::Zellij,
        session_name: "rimz-test".to_owned(),
        instance_id,
        tick_seconds: 2,
        refresh_ms_override: None,
        timezone: jiff::tz::TimeZone::UTC,
        notification_prefs: NotificationsPrefs::default(),
        // No own pane: the fold must admit every published fixture pane even
        // when the test process itself runs inside a live mux pane.
        own_pane: None,
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
    instance_id: SidebarInstanceId,
    host_data: Arc<Mutex<HostData>>,
}

#[test]
fn renderer_gets_the_snapshot_before_a_narrowing_probe() {
    thread_local! {
        static UPDATES: std::cell::RefCell<Option<ResultReceiver>> =
            const { std::cell::RefCell::new(None) };
        static SENT_BEFORE_PROBE: std::cell::Cell<Option<bool>> =
            const { std::cell::Cell::new(None) };
    }
    let fixture = FetchFixture::new();
    let mut worker = fixture.worker();
    let lost = [(AgentKind::new_unchecked("claude"), "lost".into())].into();
    crate::store::live_roster::publish(&fixture.state.live_roster, lost).unwrap();
    let (tx, rx) = result_channel();
    UPDATES.set(Some(rx));
    let mut sink = ResultSink::new(tx, wake_channel().0, None);
    sink.host_data = fixture.host_data.clone();
    // The listing can take its whole timeout on a wedged server.
    worker.session_listed = |_, _| {
        let sent = UPDATES.with_borrow(|updates| {
            matches!(
                updates.as_ref().unwrap().try_recv(),
                Ok(FetchUpdate::Snapshot { .. })
            )
        });
        SENT_BEFORE_PROBE.set(Some(sent));
        false
    };
    worker.publish_snapshot(
        &fixture.state,
        SnapshotPublication {
            snapshot: SidebarSnapshot::build(
                fixture.workspace_id.clone(),
                Vec::new(),
                jiff::Timestamp::UNIX_EPOCH,
            ),
            phase: FetchPhase::Final,
            source: SnapshotSource::Produced,
        },
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
        SnapshotPublication {
            snapshot: snapshot.with_live_panes(panes, None),
            phase: FetchPhase::Final,
            source: SnapshotSource::Produced,
        }
    };
    let on_disk = || {
        crate::store::live_roster::read(&fixture.state.live_roster)
            .unwrap()
            .agents
    };
    let (tx, _rx) = result_channel();
    let mut sink = ResultSink::new(tx, wake_channel().0, None);
    sink.host_data = fixture.host_data.clone();
    crate::store::live_roster::publish(&fixture.state.live_roster, roster(&["a", "b"])).unwrap();
    let before = std::fs::read(&fixture.state.live_roster).unwrap();

    // A producer that outlived its session sees no agents, every cycle.
    worker.session_listed = |_, _| false;
    worker.publish_snapshot(&fixture.state, produced(&[]), &mut sink);
    worker.publish_snapshot(&fixture.state, produced(&["a"]), &mut sink);
    worker.publish_snapshot(&fixture.state, produced(&[]), &mut sink);
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
    worker.publish_snapshot(&fixture.state, produced(&["a", "b", "c"]), &mut sink);
    assert_eq!(on_disk(), roster(&["a", "b", "c"]));

    // An agent that exits in a living room leaves the roster.
    worker.session_listed = |_, _| true;
    worker.publish_snapshot(&fixture.state, produced(&["a"]), &mut sink);
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
            instance_id: SidebarInstanceId::new(),
            host_data: Arc::default(),
        }
    }

    fn worker(&self) -> FetchWorker {
        let config = test_config(self.workspace_id.clone(), self.instance_id.clone());
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
        test_config(fixture.workspace_id.clone(), fixture.instance_id.clone()),
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
            .last_fold
            .as_ref()
            .unwrap()
            .frame
            .as_deref(),
        Some(&frame)
    );
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
    let (tx, rx) = result_channel();
    let mut sink = ResultSink::new(tx, wake_channel().0, None);
    sink.host_data = fixture.host_data.clone();
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
            .last_fold
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
            .last_fold
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
    sink.publish(FetchUpdate::Snapshot {
        snapshot: Box::new(rollup),
        phase: FetchPhase::Final,
        source: SnapshotSource::Cached,
    });
    assert_eq!(
        sink.host_data
            .lock()
            .unwrap()
            .last_fold
            .as_ref()
            .unwrap()
            .workspace
            .snapshot()
            .display_name,
        "second-and-longer",
        "outcomes and test snapshots without a staged fold preserve the seed"
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
fn fetch_dispatcher_sends_idle_and_coalesces_strongest_pending_request() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    let request = FetchRequest::fresh_panes();

    dispatcher.request(request, true);

    assert!(dispatcher.in_flight);
    assert_eq!(rx.try_recv().unwrap().mode, FetchMode::FreshPanes);
    assert!(dispatcher.pending_refetch.is_none());

    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    dispatcher.request(FetchRequest::default(), false);
    dispatcher.request(FetchRequest::default(), true);
    let request = FetchRequest::fresh_panes();
    let min_pane_cache_ms = request.min_pane_cache_ms;

    dispatcher.request(request, true);

    rx.try_recv().expect("initial request");
    dispatcher.complete(true);
    let pending = rx.try_recv().expect("pending refetch");
    assert_eq!(pending.mode, FetchMode::FreshPanes);
    assert_eq!(pending.min_pane_cache_ms, min_pane_cache_ms);

    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    dispatcher.request(FetchRequest::default(), false);
    dispatcher.request(FetchRequest::fresh_panes(), true);
    let request = FetchRequest::hard_refresh();

    dispatcher.request(request, true);

    assert!(dispatcher.in_flight);
    rx.try_recv().expect("initial request");
    dispatcher.complete(true);
    let pending = rx.try_recv().expect("pending refetch");
    assert_eq!(pending.mode, FetchMode::HardRefresh);
    assert!(pending.min_pane_cache_ms.is_some());

    let request = FetchRequest::force_fold();
    assert_eq!(request.mode, FetchMode::Normal);
    assert!(request.force_fold);
    assert!(request.min_pane_cache_ms.is_none());
}

#[test]
fn fetch_dispatcher_merges_deferred_deadlines_and_absorbs_work() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    let later = Instant::now() + Duration::from_secs(10);
    let earlier = later - Duration::from_secs(3);

    dispatcher.defer_until(FetchRequest::default(), later);
    dispatcher.defer_until(FetchRequest::fresh_panes(), earlier);

    assert_eq!(dispatcher.next_deadline(), Some(earlier));
    dispatcher.request(FetchRequest::default(), false);
    let request = rx
        .try_recv()
        .expect("immediate request absorbs deferred work");
    assert!(request.is_fresh_panes());
    assert!(dispatcher.next_deadline().is_none());
}

#[test]
fn fetch_dispatcher_fires_one_strongest_follow_up_after_in_flight_work() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    dispatcher.request(FetchRequest::default(), false);
    rx.try_recv().expect("initial request");
    let due = Instant::now() + Duration::from_secs(3);
    dispatcher.defer_until(FetchRequest::fresh_panes(), due);

    dispatcher.fire_due(due);
    assert!(
        rx.try_recv().is_err(),
        "follow-up remains coalesced in flight"
    );
    dispatcher.complete(true);
    let follow_up = rx
        .try_recv()
        .expect("one follow-up dispatches on completion");
    assert!(follow_up.is_fresh_panes());
    assert!(rx.try_recv().is_err(), "only one follow-up dispatches");
}

#[test]
fn fetch_dispatcher_completion_absorbs_deferred_into_pending_follow_up() {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut dispatcher = FetchDispatcher::new(tx);
    dispatcher.request(FetchRequest::default(), false);
    rx.try_recv().expect("initial request");
    dispatcher.request(FetchRequest::default(), true);
    dispatcher.defer_until(
        FetchRequest::fresh_panes(),
        Instant::now() + Duration::from_secs(3),
    );

    dispatcher.complete(true);

    let follow_up = rx
        .try_recv()
        .expect("pending follow-up absorbs deferred work");
    assert!(follow_up.is_fresh_panes());
    assert!(dispatcher.next_deadline().is_none());
    assert!(rx.try_recv().is_err(), "only one follow-up dispatches");
}

#[test]
fn one_cycle_projects_only_the_eldest_on_the_worker() {
    let fixture = FetchFixture::new();
    let panes = ["terminal_7", "terminal_8", "terminal_9"];
    let frame = crate::sidebar::frame::assemble_frame(
        panes.iter().map(|raw| pane(raw, "tab_1", false)).collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    fixture.host_data.lock().unwrap().inputs = Some(HostInputs::from_frame(frame));

    // Two panes of one host, eldest first; each excludes only itself.
    let subscribers = Subscribers::default();
    let mut updates = Vec::new();
    for (id, own) in [("01", "terminal_7"), ("02", "terminal_8")] {
        let (tx, rx) = result_channel();
        let instance_id = SidebarInstanceId::parse(&format!("sb_{id:0>32}")).unwrap();
        subscribers.lock().unwrap().insert(
            instance_id.as_str().to_owned(),
            Subscriber {
                instance_id,
                own_pane: Some(PaneId::from_parts(MuxName::Zellij, own)),
                tick_seconds: 2,
                refresh_override: None,
                tx,
                wake: wake_channel().0,
            },
        );
        updates.push((own, rx));
    }
    let mut worker = fixture.worker();
    let mut sink = ResultSink::shared(subscribers, Covered::default(), fixture.host_data.clone());
    worker.run_cycle(&fixture.state, FetchRequest::force_fold(), &mut sink);

    for (index, (own, rx)) in updates.into_iter().enumerate() {
        let mut pending = rx.0.pending.lock().unwrap();
        let update = pending.snapshot.as_mut().expect("one publication per pane");
        assert_eq!(
            update.snapshot_mut().is_some(),
            index == 0,
            "only the eldest gets a worker-built projection"
        );
        drop(pending);
        let mut config = worker.config.clone();
        config.own_pane = Some(PaneId::from_parts(MuxName::Zellij, own));
        let (mut renderer, mut dispatcher) = consumer(config, fixture.runtime.clone(), rx);
        renderer.on_snapshot(&mut dispatcher);
        assert_eq!(renderer.projections, u64::from(index != 0));
        assert_eq!(renderer.current.own_view.as_ref().unwrap().sibling_count, 2);
        let shown: Vec<String> = renderer
            .current
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
    frame.viewed_panes = vec![PaneId::from_parts(MuxName::Zellij, "terminal_0")];
    let mut cycles =
        IdleCycleFixture::new(fixture.state.clone(), fixture.runtime.clone(), frame, 3);
    cycles.enable_renderer_apply();
    cycles.run_cycle();
    let hidden = cycles
        .renderers
        .iter()
        .enumerate()
        .find_map(|(index, (r, _))| {
            (cycles.sink.cycle[0].own_pane
                != Some(PaneId::from_parts(
                    MuxName::Zellij,
                    format!("sidebar_{index}"),
                ))
                && r.current.own_view.as_ref().is_some_and(|view| {
                    !view
                        .working_pane_ids
                        .iter()
                        .any(|pane| r.current.viewed_panes.contains(pane))
                }))
            .then_some(index)
        })
        .expect("a hidden pane");
    assert_eq!(
        cycles.renderers[hidden].0.projections, 1,
        "a younger hidden pane projects at its first take"
    );
    let before = {
        let counts = cycles.sink.deliveries.lock().unwrap();
        (counts.content, counts.unchanged)
    };
    for _ in 0..3 {
        cycles.run_cycle();
    }
    assert_eq!(
        cycles.renderers[hidden].0.projections, 1,
        "hidden equal folds never project"
    );
    let counts = cycles.sink.deliveries.lock().unwrap();
    assert_eq!(
        (counts.content - before.0) + (counts.unchanged - before.1),
        3,
        "only the watched renderer consumes each cycle"
    );
    drop(counts);
    let own = cycles.renderers[hidden]
        .0
        .current
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
        cycles.renderers[hidden].0.projections, 2,
        "reveal projects immediately"
    );
    assert!(
        cycles.renderers[hidden]
            .0
            .current
            .viewed_panes
            .contains(&own)
    );
}

#[test]
fn equal_content_is_compared_at_take_not_send_and_invalidation_reconciles() {
    let (tx, rx) = result_channel();
    let update = FetchUpdate::Snapshot {
        snapshot: Box::new(crate::sidebar_pane::app::fixtures::snapshot(&workspace())),
        phase: FetchPhase::Final,
        source: SnapshotSource::Produced,
    };
    tx.send(update.clone()).unwrap();
    rx.acknowledge(rx.take().pop().unwrap(), true);
    tx.send(update.clone()).unwrap();
    assert!(
        !rx.0
            .pending
            .lock()
            .unwrap()
            .snapshot
            .as_ref()
            .unwrap()
            .is_unchanged(),
        "send retains content until the consumer decides"
    );
    assert!(
        rx.take().pop().unwrap().is_unchanged(),
        "equal take applies clocks"
    );
    rx.allow_unchanged(true);
    tx.send(update).unwrap();
    rx.invalidate();
    assert!(
        !rx.take().pop().unwrap().is_unchanged(),
        "local changes demand content"
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
    let (first, first_rx) = subscriber("01", "terminal_7", 60, wake_channel().0);
    let (second, second_rx) = subscriber("02", "terminal_8", 60, wake_channel().0);
    subscribe(&subscribers, first);
    subscribe(&subscribers, second);
    let mut worker = fixture.worker();
    let mut sink = ResultSink::shared(subscribers, Covered::default(), fixture.host_data.clone());
    worker.run_cycle(&fixture.state, FetchRequest::force_fold(), &mut sink);
    let first = first_rx.try_recv().unwrap();
    let second = second_rx.try_recv().unwrap();
    let host = fixture.host_data.lock().unwrap();
    let remembered = host.last_fold.as_ref().unwrap().workspace.snapshot();
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
    let (stalled, stalled_rx) = subscriber("01", "terminal_7", 60, wake_channel().0);
    let (advancing, advancing_rx) = subscriber("02", "terminal_8", 60, wake_channel().0);
    subscribe(&subscribers, stalled);
    subscribe(&subscribers, advancing);
    let mut worker = fixture.worker();
    let mut sink = ResultSink::shared(subscribers, Covered::default(), fixture.host_data.clone());
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
        worker.run_cycle(&fixture.state, FetchRequest::force_fold(), &mut sink);
        let update = advancing_rx
            .try_recv()
            .expect("unrelated subscriber keeps advancing");
        assert_eq!(shown_panes(&update), ["terminal_7", &newest]);
        assert!(advancing_rx.try_recv().is_err());
    }
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
    let mut sink = ResultSink::shared(subscribers, Covered::default(), fixture.host_data.clone());
    observe::take_extractions();
    worker.run_cycle(&fixture.state, FetchRequest::force_fold(), &mut sink);
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
        let mut config = worker.config.clone();
        config.instance_id = sub.instance_id;
        config.own_pane = sub.own_pane;
        let own = config.own_pane.clone().unwrap();
        let mut state = super::super::loop_state::LoopState::new(
            config,
            None,
            fixture.runtime.clone(),
            PathBuf::from("missing.sock"),
            crate::diag::DiagSink::disabled(),
            result_channel().1,
            None,
            tx.clone(),
            crate::sidebar_pane::pixel::PixelRenderCaps::default(),
            None,
        );
        state.apply_latest_snapshot(update);
        assert!(
            !state
                .current
                .rows()
                .any(|row| row.pane.as_ref().is_some_and(|pane| pane.pane_id == own))
        );
        assert_eq!(
            state.current.own_view.as_ref().unwrap().working_pane_ids,
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
            &format!("{n:02}"),
            &format!("terminal_{n}"),
            2,
            wake_channel().0,
        );
        let mut config = worker.config.clone();
        config.instance_id = sub.instance_id.clone();
        config.own_pane = sub.own_pane.clone();
        subscribe(&subscribers, sub);
        let runtime = fixture.runtime.clone();
        let ready = ready_tx.clone();
        panes.push(std::thread::spawn(move || {
            let mut state = super::super::loop_state::LoopState::new(
                config,
                None,
                runtime,
                PathBuf::from("missing.sock"),
                crate::diag::DiagSink::disabled(),
                result_channel().1,
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
    let mut sink = ResultSink::shared(subscribers, Covered::default(), fixture.host_data.clone());
    worker.run_cycle(&fixture.state, FetchRequest::force_fold(), &mut sink);
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
    id: &str,
    own: &str,
    tick_seconds: u64,
    wake: SyncSender<Wakeup>,
) -> (Subscriber, ResultReceiver) {
    let (tx, rx) = result_channel();
    let subscriber = Subscriber {
        instance_id: SidebarInstanceId::parse(&format!("sb_{id:0>32}")).unwrap(),
        own_pane: Some(PaneId::from_parts(MuxName::Zellij, own)),
        tick_seconds,
        refresh_override: None,
        tx,
        wake,
    };
    (subscriber, rx)
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

/// Let the monotonic clock move past the last reading, so "observed before
/// the cycle started" is never a tie.
fn tick_clock() {
    std::thread::sleep(Duration::from_millis(2));
}

#[test]
fn fetch_floor_merges_requests_after_the_previous_cycle_returns() {
    let floor = Duration::from_millis(150);
    let covered = Covered::default();
    let (tx, rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    tx.send(FetchRequest::default()).unwrap();
    let queued = FetchRequest {
        min_pane_cache_ms: Some(20),
        ..FetchRequest::fresh_panes()
    };
    let worker_covered = covered.clone();
    std::thread::scope(|scope| {
        let mut gap_tx = Some(tx.clone());
        let worker = scope.spawn(move || {
            let mut cycles = Vec::new();
            drive(&rx, &worker_covered, floor, |cycle| {
                cycles.push(cycle);
                worker_covered.record(cycle);
                if let Some(gap_tx) = gap_tx.take() {
                    // A start-to-start floor would already have expired.
                    std::thread::sleep(floor + Duration::from_millis(10));
                    // Queued before the return, so they merge however late
                    // either thread is scheduled.
                    tick_clock();
                    gap_tx.send(FetchRequest::default()).unwrap();
                    gap_tx.send(queued.observed_now()).unwrap();
                    finished_tx.send(Instant::now()).unwrap();
                }
            });
            cycles
        });
        let returned_at = finished_rx.recv().unwrap();
        // Lands during the rest or, on a stalled runner, after it: either way
        // the cycle that answers it started after it was observed.
        let during_wait = FetchRequest::default().observed_now();
        tx.send(during_wait).unwrap();
        drop(tx);

        let cycles = worker.join().unwrap();
        assert!(cycles[1].started.duration_since(returned_at) >= floor);
        assert!(
            (2..=3).contains(&cycles.len()),
            "queued gap requests merge into one fold"
        );
        assert_eq!(cycles[1].request.mode, FetchMode::FreshPanes);
        assert_eq!(cycles[1].request.min_pane_cache_ms, Some(20));
        assert!(cycles[1].answers(&queued));
        assert!(!cycles[0].answers(&during_wait));
        assert!(covered.answers(&during_wait));
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
            drive(&rx, &Covered::default(), floor, |cycle| {
                cycles.push(cycle);
                if cycles.len() == 1 {
                    finished_tx.send(()).unwrap();
                }
            });
            cycles
        });
        finished_rx.recv().unwrap();
        // Past the rest; a wait counted from receipt would still cost a whole
        // floor, far more than scheduling jitter.
        std::thread::sleep(floor + Duration::from_millis(50));
        let request = FetchRequest::default();
        tx.send(request).unwrap();
        drop(tx);
        let cycles = worker.join().unwrap();
        assert_eq!(cycles.len(), 2);
        assert!(cycles[1].started.duration_since(request.observed_at) < floor);
    });
}

#[test]
fn fetch_floor_has_no_refresh_or_forced_fold_bypass() {
    let floor = Duration::from_millis(40);
    for request in [
        FetchRequest::default(),
        FetchRequest::fresh_panes(),
        FetchRequest::hard_refresh(),
        FetchRequest::force_fold(),
    ] {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(FetchRequest::default()).unwrap();
        let mut sender = Some(tx);
        let mut returned_at = None;
        let mut cycles = Vec::new();
        drive(&rx, &Covered::default(), floor, |cycle| {
            cycles.push(cycle);
            if let Some(tx) = sender.take() {
                tx.send(request.observed_now()).unwrap();
                returned_at = Some(Instant::now());
            }
        });
        assert_eq!(cycles.len(), 2);
        assert!(
            cycles[1].started.duration_since(returned_at.unwrap()) >= floor,
            "no bypass for {request:?}"
        );
        assert_eq!(cycles[1].request.mode, request.mode);
        assert_eq!(cycles[1].request.force_fold, request.force_fold);
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
            drive(&rx, &Covered::default(), floor, |cycle| {
                cycles.push(cycle);
                if cycles.len() == 1 {
                    finished_tx.send(Instant::now()).unwrap();
                }
            });
            cycles
        });
        tx.send(FetchRequest::default()).unwrap();
        let returned_at = finished_rx.recv().unwrap();
        // The rendezvous proves the worker holds this batch before disconnect.
        tx.send(FetchRequest::force_fold()).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        drop(tx);
        let cycles = worker.join().unwrap();
        assert_eq!(
            cycles.len(),
            2,
            "disconnect must not discard the held batch"
        );
        assert!(cycles[1].started.duration_since(returned_at) >= floor);
        assert!(cycles[1].request.force_fold);
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
        let (subscriber, rx) = subscriber(&(index + 1).to_string(), own, 60, wake_channel().0);
        subscribe(&subscribers, subscriber);
        receivers.push(rx);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(FetchRequest::force_fold()).unwrap();
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = reads.clone();
    let receivers = Arc::new(receivers);
    let draining = receivers.clone();
    let first_publications = Arc::new(Mutex::new(Vec::new()));
    let captured = first_publications.clone();
    let sink = ResultSink::shared(subscribers, Covered::default(), fixture.host_data.clone());
    let mut worker = fixture.worker();
    let mut sender = Some(tx);
    let mut mid_first = None;
    worker.on_read = Some(Box::new(move || {
        let read = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        tick_clock();
        match read {
            1 => {
                mid_first = Some(Instant::now());
                for _ in 0..PANES {
                    sender
                        .as_ref()
                        .unwrap()
                        .send(FetchRequest::default())
                        .unwrap();
                }
            }
            2 => {
                *captured.lock().unwrap() = draining
                    .iter()
                    .map(|rx| {
                        let update = rx.try_recv().unwrap();
                        if idle {
                            rx.acknowledge(update.clone(), true);
                        }
                        update
                    })
                    .collect();
                for _ in 0..PANES {
                    sender
                        .as_ref()
                        .unwrap()
                        .send(FetchRequest {
                            observed_at: mid_first.unwrap(),
                            ..FetchRequest::default()
                        })
                        .unwrap();
                }
                sender = None;
            }
            _ => panic!("a publication failed to cover the staggered burst"),
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
                    "covered idle requests deliver only clocks"
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
fn requests_a_finished_fold_cannot_answer_still_run() {
    requests_beyond_finished_fold(false);
}

#[test]
fn requests_a_finished_idle_fold_cannot_answer_still_run() {
    requests_beyond_finished_fold(true);
}

fn requests_beyond_finished_fold(idle: bool) {
    let fixture = FetchFixture::new();
    let frame = crate::sidebar::frame::assemble_frame(
        Vec::new(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    let mut delivery =
        IdleCycleFixture::new(fixture.state.clone(), fixture.runtime.clone(), frame, 1);
    let (tx, rx) = std::sync::mpsc::channel();
    let covered = Covered::default();
    delivery.sink.covered = covered.clone();
    tx.send(FetchRequest::default()).unwrap();
    let mut sender = Some(tx);
    let mut cycles = Vec::new();
    let mut mid_first = None;
    drive(&rx, &covered, Duration::ZERO, |cycle| {
        cycles.push(cycle.request);
        if idle {
            delivery.sink.begin_cycle();
            delivery.sink.answering = Some(cycle);
            delivery.sink.publish(FetchUpdate::Snapshot {
                snapshot: Box::new(SidebarSnapshot::build_with_agents(
                    fixture.workspace_id.clone(),
                    Vec::new(),
                    jiff::Timestamp::now(),
                )),
                phase: FetchPhase::Final,
                source: SnapshotSource::Cached,
            });
            let update = delivery.receivers[0].take().pop().unwrap();
            assert_eq!(update.is_unchanged(), cycles.len() > 1);
            delivery.receivers[0].acknowledge(update, true);
        } else {
            covered.record(cycle);
        }
        let Some(tx) = sender.as_ref() else {
            return;
        };
        tick_clock();
        match cycles.len() {
            1 => {
                mid_first = Some(Instant::now());
                tx.send(FetchRequest::default()).unwrap();
            }
            // Observed before the second fold started, but asking more than
            // its ordinary request did.
            2 => {
                let observed_at = mid_first.unwrap();
                for request in [
                    FetchRequest::hard_refresh(),
                    FetchRequest::force_fold(),
                    FetchRequest::fresh_panes(),
                ] {
                    tx.send(FetchRequest {
                        observed_at,
                        ..request
                    })
                    .unwrap();
                }
                sender = None;
            }
            _ => {}
        }
    });
    assert_eq!(cycles.len(), 3, "every stronger request runs");
    let last = cycles[2];
    assert_eq!(last.mode, FetchMode::HardRefresh);
    assert!(last.force_fold);
}

#[test]
fn a_fold_that_failed_answers_nothing() {
    let (tx, rx) = std::sync::mpsc::channel();
    let covered = Covered::default();
    tx.send(FetchRequest::default()).unwrap();
    let mut sender = Some(tx);
    let mut cycles = 0;
    let observed_at = Instant::now();
    tick_clock();
    // Nothing went out, so nothing is recorded.
    drive(&rx, &covered, Duration::ZERO, |_cycle| {
        cycles += 1;
        if let Some(tx) = sender.take() {
            tx.send(FetchRequest {
                observed_at,
                ..FetchRequest::default()
            })
            .unwrap();
        }
    });
    assert_eq!(cycles, 2);
}

#[test]
fn a_follow_up_a_finished_fold_answered_is_not_sent() {
    let (tx, rx) = std::sync::mpsc::channel();
    let covered = Covered::default();
    let mut dispatcher = FetchDispatcher::for_plane(tx, covered.clone());
    dispatcher.request(FetchRequest::default(), false);
    rx.try_recv().expect("first request");
    dispatcher.request(FetchRequest::default(), true);
    tick_clock();
    covered.record(Coverage {
        started: Instant::now(),
        request: FetchRequest::default(),
    });

    dispatcher.complete(true);
    assert!(
        rx.try_recv().is_err(),
        "the plane's fold that started after it answers the follow-up"
    );
    assert!(
        !dispatcher.in_flight,
        "nothing is left waiting for an answer"
    );

    dispatcher.request(FetchRequest::default(), false);
    rx.try_recv().expect("next request");
    dispatcher.request(FetchRequest::default(), true);
    dispatcher.complete(true);
    rx.try_recv()
        .expect("a follow-up observed after the last fold started goes out");
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
    let (eldest, eldest_rx) = subscriber("01", "terminal_7", 2, wake_channel().0);
    let (younger, younger_rx) = subscriber("02", "terminal_8", 2, wake_channel().0);
    subscribe(&subscribers, eldest);
    subscribe(&subscribers, younger);
    let mut worker = fixture.worker();
    let mut sink = ResultSink::shared(
        subscribers.clone(),
        Covered::default(),
        fixture.host_data.clone(),
    );
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
    let folded = worker.project_fold(WorkspaceFold { workspace, frame }, &mut sink);

    // The eldest leaves and a new pane arrives between fold and publication.
    unsubscribe(&subscribers, "01");
    let (newcomer, newcomer_rx) = subscriber("03", "terminal_9", 2, wake_channel().0);
    subscribe(&subscribers, newcomer);
    sink.publish(FetchUpdate::Snapshot {
        snapshot: Box::new(folded),
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
    assert!(
        newcomer_rx.try_iter().next().is_none(),
        "a pane that attached mid-cycle waits for its own first fetch"
    );
}

#[test]
fn the_worker_keeps_the_tick_of_its_eldest_renderer() {
    let fixture = FetchFixture::new();
    let subscribers = Subscribers::default();
    let (eldest, _eldest_rx) = subscriber("01", "terminal_7", 60, wake_channel().0);
    let (younger, _younger_rx) = subscriber("02", "terminal_8", 1, wake_channel().0);
    subscribe(&subscribers, eldest);
    subscribe(&subscribers, younger);
    let worker = fixture.worker();
    let mut sink = ResultSink::shared(
        subscribers.clone(),
        Covered::default(),
        fixture.host_data.clone(),
    );

    assert_eq!(worker.stand(&sink).tick, Duration::from_secs(60));
    unsubscribe(&subscribers, "01");
    sink.begin_cycle();
    assert_eq!(
        worker.stand(&sink).tick,
        Duration::from_secs(1),
        "the next eldest's tick takes over"
    );
}

#[test]
fn a_full_renderer_inbox_holds_neither_the_worker_nor_another_renderer() {
    let (full, _stalled) = std::sync::mpsc::sync_channel(1);
    full.send(Wakeup::Tick).unwrap();
    let subscribers = Subscribers::default();
    let (eldest, _eldest_rx) = subscriber("01", "terminal_7", 2, wake_channel().0);
    let (stalled, _stalled_rx) = subscriber("02", "terminal_8", 2, full);
    let (other, other_rx) = subscriber("03", "terminal_9", 2, wake_channel().0);
    for subscriber in [eldest, stalled, other] {
        subscribe(&subscribers, subscriber);
    }
    let mut sink = ResultSink::shared(subscribers, Covered::default(), Arc::default());
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
    assert!(matches!(
        other_rx.try_recv(),
        Ok(FetchUpdate::Failed { .. })
    ));
}
