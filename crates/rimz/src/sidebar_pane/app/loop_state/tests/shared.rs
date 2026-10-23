use super::super::super::views::Views;
use super::*;
use crate::ids::{FocusNonce, MuxClientId};
use crate::pane::ClientPaneView;
use std::io::Write;

fn shared_rig(views: Arc<Mutex<Views>>, index: usize, rows: usize) -> Rig {
    let own = PaneId::from_parts(MuxName::Zellij, format!("sidebar_{index}"));
    let mut rig = Rig::with_own_pane(own.clone());
    let mut snapshot = agent_snapshot(&rig.ws);
    snapshot.pane_session_name = Some(rig.state.config.session_name.clone());
    let first = snapshot.worktree_groups[0].rows[0].clone();
    Arc::make_mut(&mut snapshot.worktree_groups)[0].rows = (0..rows)
        .map(|slot| {
            let index = slot % 2;
            let slot = slot / 2;
            let mut row = first.clone();
            row.id = format!("agent-{index}-{slot}");
            row.name = format!("shared-card-{index}-{slot}");
            row.pane = Some(pane(
                &format!("terminal_{index}_{slot}"),
                &format!("tab_{index}"),
                false,
            ));
            row
        })
        .collect::<Vec<_>>()
        .into();
    snapshot.own_view = Some(SidebarOwnView {
        sibling_count: rows / 2,
        working_pane_ids: snapshot
            .rows()
            .filter(|row| {
                row.pane.as_ref().unwrap().view_id.as_deref() == Some(&format!("tab_{index}"))
            })
            .map(|row| row.pane.as_ref().unwrap().pane_id.clone())
            .collect(),
        own_view_is_daemon: false,
    });
    snapshot.viewed_panes = vec![own];
    rig.share_views(views);
    rig.fold(snapshot, SnapshotSource::Produced);
    rig.state.with_projected_view(false, false, |state| {
        let group = state.current.worktree_groups[0].key.clone();
        state.ui.expanded_groups.insert(group);
    });
    rig
}

fn views() -> Arc<Mutex<Views>> {
    let mut views = Views::default();
    views.reconcile(
        &(0..2)
            .map(|index| ClientPaneView {
                client_id: MuxClientId::Zellij(index as u32),
                pane_id: PaneId::from_parts(MuxName::Zellij, format!("sidebar_{index}")),
            })
            .collect::<Vec<_>>(),
        true,
        Instant::now(),
        Duration::from_secs(60),
    );
    Arc::new(Mutex::new(views))
}

fn publish(rig: &mut Rig, update: FetchUpdate) {
    let mut views = rig.views.lock().unwrap();
    let completed = update.is_final();
    views.publish(update);
    if completed {
        rig.state.completion.store(true, Ordering::Relaxed);
    }
}

#[test]
fn a_shared_borrow_resets_scratch_presentation_and_mirrors_current() {
    let own = PaneId::from_parts(MuxName::Zellij, "sidebar_0");
    let mut rig = Rig::with_own_pane(own.clone());
    rig.view().current.own_view = Some(own_view());
    rig.view().current.viewed_panes.clear();
    rig.state.optimistic_watch_until = None;
    let dwell = rig.state.clock.now() + Duration::from_secs(60);
    rig.state.with_projected_view(false, false, |state| {
        state.ui.scroll_offset = 17;
        state.ui.selected_pane = Some(PaneId::from_parts(MuxName::Zellij, "terminal_2"));
        state.view.tab_read_dwell_until = Some(dwell);
    });
    rig.views.lock().unwrap().reconcile(
        &[ClientPaneView {
            client_id: MuxClientId::Zellij(0),
            pane_id: own,
        }],
        true,
        rig.state.clock.now(),
        Duration::from_secs(60),
    );
    let current = rig.state.with_projected_view(false, false, |state| {
        state.view.current.display_name = "shared current".into();
        state.view.current.now += Duration::from_secs(1);
        state.view.current.clone()
    });
    rig.views
        .lock()
        .unwrap()
        .reconcile(&[], true, rig.state.clock.now(), Duration::from_secs(60));
    rig.state.with_projected_view(false, false, |_| {});
    rig.state.inspect_view(|ui, view| {
        assert_eq!(ui.scroll_offset, 0);
        assert_eq!(ui.selected_pane, None);
        assert_eq!(view.current, current);
        assert_eq!(view.tab_read_dwell_until, Some(dwell));
    });
}

#[test]
fn outside_decisions_read_the_routed_view_not_scratch() {
    let views = views();
    let own = PaneId::from_parts(MuxName::Zellij, "sidebar_0");
    let other_pane = PaneId::from_parts(MuxName::Zellij, "sidebar_1");
    let clock = LoopClock::play(Timestamp::now());
    let mut rig = Rig::with_clock(Some(own.clone()), BodyLens::default(), None, clock.clone());
    rig.share_views(views.clone());
    rig.state.with_projected_view(false, false, |_| {});
    let mut other = Rig::with_own_pane(other_pane.clone());
    other.share_views(views.clone());
    other.state.with_projected_view(false, false, |_| {});
    settle_width(&mut rig);
    rig.state.dirty = false;
    let dwell = clock.now() + Duration::from_millis(200);
    {
        let mut view = rig.view();
        let state = &mut *view;
        state.current.own_view = Some(own_view());
        state.ui.theme(&state.view.current.theme);
        layout_freeze::arm_freeze(&mut state.ui, clock.now_ms() as i64);
        state.health.failure_streak = 1;
        state.tab_read_dwell_until = Some(dwell);
    }
    rig.state.with_projected_view(false, false, |_| {});
    assert_eq!(
        rig.state.inspect_view(|ui, view| (
            ui.freeze.is_some(),
            view.health.failure_streak,
            view.tab_read_dwell_until,
        )),
        (true, 1, Some(dwell))
    );
    assert!(!rig.state.admits_clock_only(false));
    assert_eq!(
        rig.state.frame_timing(),
        (false, Duration::from_millis(200))
    );
    views.lock().unwrap().reconcile(
        &[
            ClientPaneView {
                client_id: MuxClientId::Zellij(0),
                pane_id: other_pane,
            },
            ClientPaneView {
                client_id: MuxClientId::Zellij(1),
                pane_id: own,
            },
        ],
        true,
        clock.now(),
        Duration::from_secs(60),
    );
    assert_eq!(
        rig.state.inspect_view(|ui, view| (
            ui.freeze.is_some(),
            view.health.failure_streak,
            view.tab_read_dwell_until,
        )),
        (false, 0, None)
    );
    assert!(rig.state.admits_clock_only(false));
    assert!(rig.state.frame_timing().1 > Duration::from_millis(200));
}

#[test]
fn lagging_view_keeps_success_before_failure() {
    for newborn in [false, true] {
        let views = views();
        let mut rig = shared_rig(views.clone(), 0, 2);
        let mut changed = rig.state.current.clone();
        changed.display_name = "successful content".into();
        let success = rig.lazy_update(changed, SnapshotSource::Produced);
        publish(&mut rig, success);
        publish(
            &mut rig,
            FetchUpdate::Failed {
                error: "failed pull".into(),
            },
        );
        if newborn {
            let own = rig.state.config.own_pane.clone().unwrap();
            rig = Rig::with_own_pane(own);
            rig.share_views(views);
        }
        rig.state.on_snapshot(&mut rig.fetch);
        rig.state
            .draw_frame(&mut rig.terminal, Instant::now(), false)
            .unwrap();
        let (name, failures) = rig.state.inspect_view(|_, view| {
            (
                view.current.display_name.clone(),
                view.health.failure_streak,
            )
        });
        assert_eq!(
            (name.as_str(), failures),
            ("successful content", 1),
            "newborn={newborn}"
        );
        publish(
            &mut rig,
            FetchUpdate::Failed {
                error: "failed again".into(),
            },
        );
        rig.state.on_snapshot(&mut rig.fetch);
        assert!(rig.state.inspect_view(|_, view| {
            view.health
                .alert
                .as_ref()
                .is_some_and(|alert| alert.is_active())
        }));
    }
}

#[test]
fn lagging_view_keeps_pending_final_over_interim() {
    let mut rig = shared_rig(views(), 0, 2);
    let mut changed = rig.state.current.clone();
    changed.display_name = "completed content".into();
    let final_update = rig.lazy_update(changed.clone(), SnapshotSource::Produced);
    publish(&mut rig, final_update);
    changed.display_name = "unfinished content".into();
    let mut interim = rig.lazy_update(changed, SnapshotSource::Cached);
    if let FetchUpdate::Shared { update, .. } = &mut interim
        && let FetchUpdate::Fold { phase, .. } = update.as_mut()
    {
        *phase = FetchPhase::Interim;
    }
    publish(&mut rig, interim);
    rig.state.on_snapshot(&mut rig.fetch);
    assert_eq!(
        rig.state
            .inspect_view(|_, view| view.current.display_name.clone()),
        "completed content"
    );
}

#[test]
fn composition_uses_the_arriving_views_gate_notice() {
    let views = views();
    let mut rig = shared_rig(views.clone(), 0, 2);
    let mut other = shared_rig(views.clone(), 1, 2);
    let mut healthy = rig.state.current.clone();
    healthy.panes_produced_at_ms = Some(rig.state.clock.now_ms());
    rig.fold(healthy, SnapshotSource::Produced);
    let mut regressed = rig.state.current.clone();
    regressed.panes_produced_at_ms = None;
    // Equal content would apply clock-only and never reach the gate.
    regressed.display_name = "frameless content".into();
    rig.fold(regressed, SnapshotSource::Produced);
    assert!(rig.state.ui.gate_notice.is_some());
    other
        .state
        .draw_frame(&mut other.terminal, Instant::now(), false)
        .unwrap();
    // Every view folds the same frameless update; the arriving one has since released its hold.
    assert!(other.state.inspect_view(|_, view| view.gate.rule.is_some()));
    other.view().gate = GateState::default();
    views.lock().unwrap().reconcile(
        &[
            ClientPaneView {
                client_id: MuxClientId::Zellij(0),
                pane_id: PaneId::from_parts(MuxName::Zellij, "sidebar_1"),
            },
            ClientPaneView {
                client_id: MuxClientId::Zellij(1),
                pane_id: PaneId::from_parts(MuxName::Zellij, "sidebar_0"),
            },
        ],
        true,
        rig.state.clock.now(),
        Duration::from_secs(60),
    );
    rig.state
        .draw_frame(&mut rig.terminal, Instant::now(), false)
        .unwrap();
    assert!(rig.state.inspect_view(|_, view| view.gate.rule.is_none()));
    assert!(
        rig.state.ui.gate_notice.is_none(),
        "a healthy arriving view must not paint the prior view's notice"
    );
}

struct BlockedOutput {
    entered: Option<std::sync::mpsc::Sender<()>>,
    release: Receiver<()>,
}

#[test]
fn painting_an_up_to_date_view_does_not_prepare_a_fold() {
    let mut rig = shared_rig(views(), 0, 2);
    rig.state
        .draw_frame(&mut rig.terminal, Instant::now(), false)
        .unwrap();
    let preparations = rig.state.fold_preparations;
    rig.state
        .draw_frame(&mut rig.terminal, Instant::now(), false)
        .unwrap();
    assert_eq!(rig.state.fold_preparations, preparations);
}

impl Write for BlockedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(entered) = self.entered.take() {
            entered.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn blocked_terminal_output_leaves_other_views_free_to_paint() {
    let views = views();
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let heavy_views = views.clone();
        let heavy = scope.spawn(move || {
            let mut rig = shared_rig(heavy_views, 0, 200);
            rig.terminal.backend_mut().play_output(BlockedOutput {
                entered: Some(entered),
                release: blocked,
            });
            rig.state
                .draw_frame(&mut rig.terminal, Instant::now(), false)
                .unwrap();
        });
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        let available = views.try_lock().is_ok();
        if !available {
            release.send(()).unwrap();
        }
        assert!(
            available,
            "terminal output must not hold the shared view lock"
        );
        let mut target = shared_rig(views, 1, 2);
        let output = FrameOutput::default();
        target.terminal.backend_mut().play_output(output.clone());
        target
            .state
            .draw_frame(&mut target.terminal, Instant::now(), false)
            .unwrap();
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(&output.0.lock().unwrap());
        assert!(parser.screen().contents().contains("shared-card-1-0"));
        release.send(()).unwrap();
        heavy.join().unwrap();
    });
}

struct TimedOutput {
    output: FrameOutput,
    painted: Arc<Mutex<Option<Instant>>>,
}

impl Write for TimedOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.output.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        *self.painted.lock().unwrap() = Some(Instant::now());
        self.output.flush()
    }
}

#[test]
fn a_jump_contending_with_a_heavy_view_reaches_target_output_within_one_click_budget() {
    let views = views();
    let mut target = shared_rig(views.clone(), 1, 2);
    let (entered, waiting) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let output = FrameOutput::default();
    let painted = Arc::new(Mutex::new(None));
    target.terminal.backend_mut().play_output(TimedOutput {
        output: output.clone(),
        painted: painted.clone(),
    });
    std::thread::scope(|scope| {
        let heavy = scope.spawn(move || {
            let mut rig = shared_rig(views, 0, 200);
            rig.state.fold_transition(|state| {
                entered.send(()).unwrap();
                blocked.recv_timeout(Duration::from_secs(10)).unwrap();
                state
                    .compose_frame(
                        &mut rig.terminal,
                        Instant::now(),
                        false,
                        &EventStore::default(),
                    )
                    .0
                    .unwrap();
            });
        });
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        let clicked = Instant::now();
        let anchor = crate::mux::focus_anchor::FocusAnchor {
            nonce: FocusNonce::new(),
            session_name: target.state.config.session_name.clone(),
            pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1_0"),
            origin: FocusOrigin::User,
            repair_generation: None,
            issued_at_ms: target.state.clock.now_ms(),
            applied_at_ms: None,
            state: crate::mux::focus_anchor::FocusIntentState::Requested,
            pre_action: Vec::new(),
            placement: None,
            order: None,
        };
        crate::mux::focus_anchor::store(&target.runtime, &anchor).unwrap();
        let (push, ack) = focus_push(&mut target, anchor);
        release.send(()).unwrap();
        target
            .state
            .on_focus_push(push, &mut target.fetch, &mut target.terminal)
            .unwrap();
        let elapsed = painted
            .lock()
            .unwrap()
            .expect("the landing must reach terminal output")
            - clicked;
        assert!(
            elapsed < Duration::from_millis(500),
            "contended landing took {elapsed:?}"
        );
        assert!(
            target.state.pending_focus_push.is_none(),
            "the landing must acknowledge its paint: {:?}, {:?}",
            target.state.inspect_view(|ui, view| (
                view.current.focused_pane.clone(),
                ui.selected_pane.clone(),
                ui.drawn_selection.clone(),
                ui.last_focus_anchor_ms
            )),
            target
                .state
                .pending_focus_push
                .as_ref()
                .map(|push| &push.peer)
        );
        assert_push_report(&ack, &target, PushVerdict::Painted, true);
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(&output.0.lock().unwrap());
        assert!(
            parser
                .screen()
                .contents()
                .lines()
                .any(|line| line.contains("shared-card-1-0") && line.contains('▌'))
        );
        heavy.join().unwrap();
    });
}

#[test]
fn a_jump_does_not_change_an_unrelated_clients_view() {
    let views = views();
    let mut other = shared_rig(views.clone(), 0, 2);
    let target = shared_rig(views, 1, 2);
    other.state.with_projected_view(false, false, |state| {
        state.ui.baseline_pane = Some(PaneId::from_parts(MuxName::Zellij, "terminal_0_0"));
    });
    other.event(SidebarEvent::FocusChanged {
        focused: vec![PaneId::from_parts(MuxName::Zellij, "terminal_1_0")],
        unfocused: Vec::new(),
    });
    assert_eq!(
        other.state.inspect_view(|ui, _| ui.baseline_pane.clone()),
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_0_0")),
        "another client's focus event cannot select this view"
    );
    let anchor = crate::mux::focus_anchor::FocusAnchor {
        nonce: FocusNonce::new(),
        session_name: target.state.config.session_name.clone(),
        pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1_0"),
        origin: FocusOrigin::User,
        repair_generation: None,
        issued_at_ms: target.state.clock.now_ms(),
        applied_at_ms: None,
        state: crate::mux::focus_anchor::FocusIntentState::Requested,
        pre_action: Vec::new(),
        placement: None,
        order: None,
    };
    crate::mux::focus_anchor::store(&other.runtime, &anchor).unwrap();
    other.state.fold_focus_intent(anchor);
    assert_eq!(
        other.state.inspect_view(|ui, _| ui.baseline_pane.clone()),
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_0_0"))
    );
}
