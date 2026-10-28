use super::*;

impl Rig {
    fn lazy_fold(&mut self, snapshot: SidebarSnapshot, source: SnapshotSource) {
        self.deliver(self.lazy_update(snapshot, source));
    }

    pub(super) fn lazy_update(
        &self,
        snapshot: SidebarSnapshot,
        source: SnapshotSource,
    ) -> FetchUpdate {
        let own = self.state.pane.config.own_pane.clone();
        let view = snapshot.own_view.as_ref().unwrap();
        let mut panes = vec![pane(own.raw(), "tab_0", false)];
        panes.extend(
            view.working_pane_ids
                .iter()
                .map(|id| pane(id.raw(), "tab_0", false)),
        );
        let frame = crate::sidebar::frame::assemble_frame(panes, 1, "rimz-test");
        FetchUpdate::Shared {
            update: Box::new(FetchUpdate::Fold {
                fold: Arc::new(WorkspaceFold {
                    workspace: crate::sidebar::enrich::WorkspaceSnapshot(snapshot),
                    frame: Some(Arc::new(frame)),
                }),
                phase: FetchPhase::Final,
                source,
            }),
            context: Arc::new(crate::sidebar_pane::app::fetch::FoldShared {
                inputs: Arc::new(crate::sidebar_pane::app::fetch::FoldInputs {
                    sampled_at: self.state.pane.clock.now(),
                    anchor: crate::mux::focus_anchor::load(&self.runtime),
                    filter: crate::sidebar::body_filter::load(&self.runtime),
                }),
                ..Default::default()
            }),
        }
    }
}

#[test]
fn equal_hidden_ticks_never_project() {
    let mut rig = hidden_rig();
    let before = rig.state.pane.projections;
    let clock = LoopClock::play(Timestamp::now());
    rig.state.pane.clock = clock.clone();
    for _ in 0..3 {
        clock.advance(Duration::from_secs(60));
        rig.lazy_fold(rig.state.views.mirror().clone(), SnapshotSource::Produced);
        assert_eq!(
            rig.state.pane.projections, before,
            "hidden ticks never project"
        );
        assert!(
            rig.state.frame_timing().1
                >= SELF_CLOSE_WATCHDOG.min(tick_for(rig.state.pane.config.tick_seconds))
        );
    }
}

#[test]
fn hidden_projection_waits_for_reveal_and_completes_skipped_final() {
    let mut rig = hidden_rig();
    assert_eq!(
        rig.state.pane.projections, 1,
        "the initial fold projects on take"
    );
    rig.request_if_idle(FetchRequest::default());
    assert!(rig.next_request().is_some());
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(rig.next_request().is_none());
    for name in ["first", "latest"] {
        defer_name(&mut rig, name);
    }
    assert_eq!(
        rig.state.pane.projections, 1,
        "deferred folds never project"
    );
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(
        rig.next_request().unwrap().is_fresh_panes(),
        "deferred final clears the latch"
    );
    rig.event(SidebarEvent::FocusChanged {
        focused: rig
            .state
            .views
            .mirror()
            .own_view
            .as_ref()
            .unwrap()
            .working_pane_ids
            .clone(),
        unfocused: vec![],
    });
    assert_eq!(
        rig.state.pane.projections, 2,
        "reveal projects only the latest"
    );
    assert_eq!(rig.view().ctx().current.display_name, "latest");
}

fn hidden_rig() -> Rig {
    let mut rig = Rig::with_own_pane(pane("terminal_1", "tab_0", false).pane_id);
    let mut snapshot = agent_snapshot(&rig.ws);
    snapshot.own_view = Some(own_view());
    snapshot.viewed_panes.clear();
    snapshot.focused_pane = None;
    rig.lazy_fold(snapshot, SnapshotSource::Produced);
    settle_width(&mut rig);
    assert!(!rig.state.watched());
    assert!(rig.state.admits_clock_only(false));
    rig
}

#[test]
fn hidden_drain_counters_measure_retained_outcomes_and_preview_work() {
    let mut rig = hidden_rig();
    let counts = host_pass::thread_counters();
    rig.state.pane.own.revision = rig.state.views.registry.lock().unwrap().revision;
    counts.reset();
    rig.state.on_snapshot();
    assert_eq!(counts.get(Counter::DrainClone), 0);
    assert_eq!(counts.get(Counter::DrainPreview), 0);
    rig.lazy_fold(rig.state.views.mirror().clone(), SnapshotSource::Produced);
    assert_eq!(counts.get(Counter::DrainClone), 1);
    assert_eq!(counts.get(Counter::DrainPreview), 1);
    rig.state.on_snapshot();
    assert_eq!(counts.get(Counter::DrainClone), 2);
    assert_eq!(counts.get(Counter::DrainPreview), 2);
}

#[test]
fn timeout_source_tracks_the_winning_deadline_without_counting_a_wake() {
    let mut rig = hidden_rig();
    let clock = LoopClock::play(rig.state.views.mirror().now);
    rig.state.pane.clock = clock.clone();
    rig.state.pane.last_self_close_check = clock.now();
    rig.state.pane.config.tick_seconds = 1;
    rig.state.pane.caps_refresh_deadline = None;
    rig.state.frame_timing();
    assert_eq!(host_pass::timeout_source(), Counter::TimeoutIdle);
    rig.state.pane.config.tick_seconds = 60;
    rig.state.frame_timing();
    assert_eq!(host_pass::timeout_source(), Counter::TimeoutWatchdog);
    rig.state.pane.caps_refresh_deadline = Some(clock.now() + Duration::from_millis(300));
    rig.state.frame_timing();
    assert_eq!(host_pass::timeout_source(), Counter::TimeoutCaps);
    rig.state.pane.caps_refresh_deadline = Some(clock.now() + Duration::from_secs(90));
    rig.state.frame_timing();
    assert_eq!(host_pass::timeout_source(), Counter::TimeoutWatchdog);
}

#[test]
fn first_focus_reveal_reads_siblings_after_dwell() {
    let mut rig = hidden_rig();
    let clock = LoopClock::play(rig.state.views.mirror().now);
    rig.state.pane.clock = clock.clone();
    let mut snapshot = rig.state.views.mirror().clone();
    let rows = Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows);
    rows[0].unread = true;
    let mut sibling = rows[0].clone();
    sibling.id = "sibling".into();
    sibling.pane = Some(pane("terminal_10", "tab_0", false));
    rows.push(sibling);
    snapshot
        .own_view
        .as_mut()
        .unwrap()
        .working_pane_ids
        .push(pane("terminal_10", "tab_0", false).pane_id);
    snapshot.own_view.as_mut().unwrap().sibling_count = 2;
    rig.lazy_fold(snapshot, SnapshotSource::Produced);
    defer_name(&mut rig, "reveal");
    rig.event(SidebarEvent::FocusChanged {
        focused: vec![own_view().working_pane_ids[0].clone()],
        unfocused: vec![],
    });
    assert!(
        rig.state
            .inspect_view(|_, view| view.tab_read_dwell_until.is_some())
    );
    assert!(rig.state.inspect_view(|ui, _| ui.freeze.is_some()));
    assert!(
        rig.state
            .views
            .mirror()
            .rows()
            .find(|row| row.id == "sibling")
            .unwrap()
            .unread
    );

    clock.advance(TAB_READ_DWELL + Duration::from_millis(1));
    let mut viewed = rig.state.views.mirror().clone();
    viewed.viewed_panes = viewed.own_view.as_ref().unwrap().working_pane_ids.clone();
    rig.lazy_fold(viewed, SnapshotSource::Produced);
    assert!(rig.state.views.mirror().rows().all(|row| !row.unread));
    assert!(
        ReadMarks::load(&rig.runtime)
            .cleared_at_ms("sibling")
            .is_some()
    );
}

#[test]
fn first_input_reveal_arms_dwell_only_once() {
    let mut rig = hidden_rig();
    let clock = LoopClock::play(rig.state.views.mirror().now);
    rig.state.pane.clock = clock.clone();
    rig.state
        .on_input(Wakeup::Key(KeyAction::Dismiss), &mut rig.terminal)
        .unwrap();
    let until = rig.state.inspect_view(|_, view| view.tab_read_dwell_until);
    assert_eq!(until, Some(clock.now() + TAB_READ_DWELL));
    assert!(rig.state.inspect_view(|ui, _| ui.freeze.is_some()));

    clock.advance(Duration::from_millis(100));
    let mut viewed = rig.state.views.mirror().clone();
    viewed.viewed_panes = viewed.own_view.as_ref().unwrap().working_pane_ids.clone();
    rig.lazy_fold(viewed, SnapshotSource::Produced);
    assert_eq!(
        rig.state.inspect_view(|_, view| view.tab_read_dwell_until),
        until
    );
}

fn defer_name(rig: &mut Rig, name: &str) {
    let mut snapshot = rig.state.views.mirror().clone();
    snapshot.display_name = name.into();
    let before = rig.state.pane.content_applications;
    rig.lazy_fold(snapshot, SnapshotSource::Produced);
    assert_eq!(
        rig.state.pane.content_applications, before,
        "hidden content stays unread in the views"
    );
}

#[test]
fn one_watched_of_fifty_seven_folds_across_data_ticks() {
    let mut rigs: Vec<_> = (0..57)
        .map(|index| {
            let own = pane(&format!("sidebar_{index}"), &format!("tab_{index}"), false).pane_id;
            let mut rig = Rig::with_own_pane(own.clone());
            let mut snapshot = agent_snapshot(&rig.ws);
            snapshot.own_view = Some(crate::store::snapshot::SidebarOwnView {
                sibling_count: 1,
                working_pane_ids: vec![
                    pane(&format!("working_{index}"), &format!("tab_{index}"), false).pane_id,
                ],
                own_view_is_daemon: false,
            });
            snapshot.viewed_panes = vec![pane("sidebar_0", "tab_0", false).pane_id];
            snapshot.focused_pane = None;
            rig.lazy_fold(snapshot, SnapshotSource::Produced);
            settle_width(&mut rig);
            // A viewed tab's read dwell is a deliberate blocker, not hidden work.
            assert_eq!(rig.state.watched(), index == 0);
            rig
        })
        .collect();
    for cycle in 1..=3 {
        for (index, rig) in rigs.iter_mut().enumerate() {
            let clock = LoopClock::play(Timestamp::now());
            rig.state.pane.clock = clock.clone();
            clock.advance(Duration::from_secs(60 * cycle));
            let mut snapshot = rig.state.views.mirror().clone();
            snapshot.display_name = format!("cycle-{cycle}");
            rig.lazy_fold(snapshot, SnapshotSource::Produced);
            assert_eq!(
                rig.state.pane.content_applications,
                if index == 0 { 1 + cycle } else { 1 }
            );
            if index != 0 {
                assert_eq!(rig.state.pane.projections, 1);
                assert!(
                    rig.state.frame_timing().1
                        >= SELF_CLOSE_WATCHDOG.min(tick_for(rig.state.pane.config.tick_seconds))
                );
            }
        }
    }
}

#[test]
fn skipped_final_completes_requests_and_refreshes_backstops() {
    let mut rig = hidden_rig();
    rig.request_if_idle(FetchRequest::default());
    assert!(rig.next_request().is_some());
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(rig.next_request().is_none());
    let before = Instant::now();
    defer_name(&mut rig, "deferred");
    assert!(rig.state.pane.last_self_close_check >= before);
    rig.request_if_idle(FetchRequest::fresh_panes());
    assert!(
        rig.next_request().unwrap().is_fresh_panes(),
        "deferred final clears the latch"
    );
    assert!(rig.state.frame_timing().1 > FRAME_MIN_TIMEOUT);
    rig.event(SidebarEvent::FocusChanged {
        focused: rig
            .state
            .views
            .mirror()
            .own_view
            .as_ref()
            .unwrap()
            .working_pane_ids
            .clone(),
        unfocused: vec![],
    });
    assert_eq!(rig.view().ctx().current.display_name, "deferred");
}

#[test]
fn retained_hidden_content_does_not_postpone_the_watchdog() {
    let mut rig = hidden_rig();
    let clock = LoopClock::play(Timestamp::now());
    rig.state.pane.clock = clock.clone();
    rig.state.pane.config.tick_seconds = 60;
    defer_name(&mut rig, "pending");
    let projections = rig.state.pane.projections;
    clock.advance(SELF_CLOSE_WATCHDOG / 2);
    rig.maintenance();
    assert!(rig.next_request().is_none(), "watchdog is not due yet");
    clock.advance(SELF_CLOSE_WATCHDOG / 2);
    rig.maintenance();
    assert!(
        rig.next_request().is_some(),
        "retained content must not suppress the watchdog"
    );
    assert_eq!(
        rig.state.pane.projections, projections,
        "content stays deferred"
    );
}

#[test]
fn reveal_never_skips() {
    assert_reveal_or_changed_own_view(true);
}

#[test]
fn changed_own_view_never_skips() {
    assert_reveal_or_changed_own_view(false);
}

fn assert_reveal_or_changed_own_view(reveal: bool) {
    let mut rig = hidden_rig();
    let mut snapshot = rig.state.views.mirror().clone();
    snapshot.display_name = "immediate".into();
    if reveal {
        snapshot.viewed_panes = snapshot.own_view.as_ref().unwrap().working_pane_ids.clone();
    } else {
        snapshot.own_view = Some(empty_own_view());
    }
    rig.lazy_fold(snapshot, SnapshotSource::Produced);
    assert_eq!(rig.view().ctx().current.display_name, "immediate");
    assert_eq!(
        rig.state.pane.should_exit, !reveal,
        "an observed sibling disappearing closes immediately"
    );
}

#[test]
fn hidden_failure_always_applies() {
    let mut rig = hidden_rig();
    defer_name(&mut rig, "before failure");
    rig.deliver(FetchUpdate::Failed {
        error: "offline".into(),
    });
    assert_eq!(rig.view().ctx().current.display_name, "before failure");
    assert_eq!(rig.view().ctx().health.failure_streak, 1);
}

#[test]
fn hidden_health_obligation_applies() {
    let mut rig = hidden_rig();
    rig.view().ctx().health.failure_streak = 1;
    assert_hidden_obligation(rig);
}

#[test]
fn hidden_gate_obligation_applies() {
    let mut rig = hidden_rig();
    rig.view().ctx().gate.reject_streak = 1;
    assert_hidden_obligation(rig);
}

#[test]
fn hidden_empty_confirmation_applies() {
    let mut rig = hidden_rig();
    rig.state.pane.self_close.seen_sibling = false;
    rig.state
        .pane
        .self_close
        .should_close(Some(0), Instant::now());
    assert_hidden_obligation(rig);
}

#[test]
fn input_flushes_hidden_content() {
    for input in [
        Wakeup::Key(KeyAction::Dismiss),
        Wakeup::Scroll { down: true },
        Wakeup::MouseClick { column: 0, row: 0 },
    ] {
        let mut rig = hidden_rig();
        defer_name(&mut rig, "before input");
        rig.state.on_input(input, &mut rig.terminal).unwrap();
        assert_eq!(rig.view().ctx().current.display_name, "before input");
    }
}

#[test]
fn hidden_resize_keeps_deferred_content_unprojected() {
    let mut rig = hidden_rig();
    defer_name(&mut rig, "before resize");
    let projections = rig.state.pane.projections;
    let reductions = rig.state.pane.content_applications;
    rig.state.on_resize(&mut rig.terminal, None).unwrap();
    assert_ne!(rig.view().ctx().current.display_name, "before resize");
    assert_eq!(rig.state.pane.projections, projections);
    assert_eq!(rig.state.pane.content_applications, reductions);
}

#[test]
fn hidden_filter_and_width_events_keep_content_pending() {
    for event in [
        SidebarEvent::BodyFilterChanged,
        SidebarEvent::WidthTargetChanged,
    ] {
        let mut rig = hidden_rig();
        defer_name(&mut rig, "pending");
        rig.event(event);
        assert_ne!(rig.view().ctx().current.display_name, "pending");
        assert_eq!(rig.state.pane.projections, 1);
    }
}

#[test]
fn hidden_overlay_only_folds_for_its_reveal() {
    let mut rig = hidden_rig();
    defer_name(&mut rig, "pending");
    assert!(!rig.state.observed_watched());
    rig.event(SidebarEvent::FocusChanged {
        focused: vec![PaneId::from_parts(MuxName::Zellij, "terminal_500")],
        unfocused: vec![],
    });
    assert_ne!(rig.view().ctx().current.display_name, "pending");
    assert_eq!(rig.state.pane.projections, 1);
    rig.event(SidebarEvent::FocusChanged {
        focused: rig
            .state
            .views
            .mirror()
            .own_view
            .as_ref()
            .unwrap()
            .working_pane_ids
            .clone(),
        unfocused: vec![],
    });
    assert_eq!(rig.view().ctx().current.display_name, "pending");
    assert!(rig.state.watched());
    assert!(
        rig.next_request()
            .expect("reveal verifies panes")
            .is_fresh_panes()
    );
}

#[test]
fn hidden_foreign_notifications_do_not_fold() {
    let mut rig = hidden_rig();
    defer_name(&mut rig, "pending");
    rig.event(SidebarEvent::Notify {
        title: "other tab".into(),
        body: "waiting".into(),
        panes: vec![PaneId::from_parts(MuxName::Zellij, "terminal_500")],
        recheck_unread: true,
        notification_kind: None,
    });
    assert_eq!(rig.state.pane.projections, 1);
    assert_ne!(rig.view().ctx().current.display_name, "pending");
}

#[test]
fn hidden_expired_freeze_neither_folds_nor_spins() {
    let mut rig = hidden_rig();
    let clock = LoopClock::play(Timestamp::now());
    rig.state.pane.clock = clock.clone();
    layout_freeze::arm_freeze(rig.view().ui(), clock.now_ms() as i64);
    {
        let mut guard = rig.view();
        let (_, context) = guard.pair();
        context.overlay_baseline = Some(context.current.clone());
    }
    clock.advance(Duration::from_secs(60));
    defer_name(&mut rig, "pending");
    assert_eq!(rig.state.pane.projections, 1);
    assert!(rig.state.frame_timing().1 > FRAME_MIN_TIMEOUT);
    rig.maintenance();
    assert!(
        rig.next_request().is_none(),
        "expired hidden freeze requests no fold"
    );
}

#[test]
fn hidden_strand_only_drains_its_owner() {
    for own in [false, true] {
        let mut rig = hidden_rig();
        defer_name(&mut rig, "pending");
        rig.event(SidebarEvent::FocusStranded {
            pane_id: if own {
                rig.state.pane.config.own_pane.clone()
            } else {
                PaneId::from_parts(MuxName::Zellij, "terminal_500")
            },
            generation: 1,
            clients: vec![],
        });
        assert_eq!(rig.view().ctx().current.display_name == "pending", own);
    }
}

#[test]
fn hidden_intent_only_drains_its_owner() {
    for own in [false, true] {
        let mut rig = hidden_rig();
        // The pending fold observes the target's session, so the jump stays in flight.
        rig.view().ctx().current.pane_session_name = Some("rimz-test".to_owned());
        defer_name(&mut rig, "pending");
        let target = if own {
            own_view().working_pane_ids[0].clone()
        } else {
            PaneId::from_parts(MuxName::Zellij, "terminal_500")
        };
        let now_ms = crate::utils::time::unix_now_ms();
        let anchor = crate::mux::focus_anchor::FocusAnchor {
            nonce: crate::ids::FocusNonce::new(),
            session_name: "rimz-test".to_owned(),
            placement: None,
            pane_id: target.clone(),
            origin: crate::mux::focus_anchor::FocusOrigin::User,
            repair_generation: None,
            issued_at_ms: now_ms,
            applied_at_ms: None,
            state: crate::mux::focus_anchor::FocusIntentState::Requested,
            pre_action: Vec::new(),
            order: None,
        };
        crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
        rig.event(SidebarEvent::FocusIntent {
            pane_id: target.clone(),
            nonce: anchor.nonce,
        });
        assert_eq!(rig.view().ctx().current.display_name == "pending", own);
        assert_eq!(
            rig.view().ctx().last_folded_focus_intent == Some(anchor.nonce),
            own
        );
        assert_eq!(rig.view().ui().selected_pane.as_ref() == Some(&target), own);
    }
}

#[test]
fn hidden_command_overlay_is_retained_until_reveal() {
    let mut rig = hidden_rig();
    defer_name(&mut rig, "pending");
    let target = rig
        .view()
        .ctx()
        .current
        .own_view
        .as_ref()
        .unwrap()
        .working_pane_ids[0]
        .clone();
    rig.event(SidebarEvent::CommandChanged {
        pane_id: target.clone(),
        command: "changed-command".into(),
    });
    assert_eq!(rig.state.pane.projections, 1);
    assert_ne!(*rig.state.events(), EventStore::default());
    rig.event(SidebarEvent::FocusChanged {
        focused: vec![rig.state.pane.config.own_pane.clone()],
        unfocused: vec![],
    });
    assert_eq!(rig.view().ctx().current.display_name, "pending");
    assert_eq!(
        rig.view()
            .ctx()
            .current
            .row_owning_pane(&target)
            .unwrap()
            .pane
            .as_ref()
            .unwrap()
            .command
            .as_deref(),
        Some("changed-command")
    );
}

#[test]
fn hidden_peer_does_not_clear_a_watched_manual_unread_guard() {
    let mut hidden = hidden_rig();
    let mut watched = hidden_rig();
    hidden.state.pane.runtime = watched.runtime.clone();
    hidden.runtime = watched.runtime.clone();
    let mut snapshot = unread_snapshot(&watched, true);
    let focused = snapshot
        .rows()
        .next()
        .unwrap()
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    snapshot.focused_pane = Some(focused.clone());
    snapshot.viewed_panes = vec![focused];
    // A guard lives in a painted view, so the watched view folds once before arming it.
    let mut unfocused = snapshot.clone();
    unfocused.focused_pane = None;
    watched.fold(unfocused, SnapshotSource::Produced);
    watched.view().ui().unread_guard = Some("agent-1".into());
    watched.fold(snapshot.clone(), SnapshotSource::Produced);
    snapshot.own_view.as_mut().unwrap().working_pane_ids =
        vec![PaneId::from_parts(MuxName::Zellij, "terminal_500")];
    // Establish the other tab before introducing the focus receipt.
    let mut initial = snapshot.clone();
    initial.focused_pane = None;
    initial.viewed_panes.clear();
    hidden.fold(initial, SnapshotSource::Produced);
    let clock = LoopClock::play(Timestamp::now());
    hidden.state.pane.clock = clock.clone();
    for _ in 0..3 {
        clock.advance(Duration::from_secs(60));
        hidden.lazy_fold(snapshot.clone(), SnapshotSource::Produced);
        assert_eq!(
            ReadMarks::load(&watched.runtime).cleared_at_ms("agent-1"),
            None
        );
    }
}

fn assert_hidden_obligation(mut rig: Rig) {
    let mut snapshot = rig.state.views.mirror().clone();
    snapshot.display_name = "obligation".into();
    rig.lazy_fold(snapshot, SnapshotSource::Produced);
    assert_eq!(rig.view().ctx().current.display_name, "obligation");
}

#[test]
fn hidden_exit_obligation_applies() {
    let mut rig = hidden_rig();
    rig.state.pane.should_exit = true;
    assert_hidden_obligation(rig);
}

#[test]
fn hidden_dwell_obligation_applies() {
    let mut rig = hidden_rig();
    rig.view().ctx().tab_read_dwell_until = Some(rig.state.pane.clock.now());
    assert_hidden_obligation(rig);
}

#[test]
fn hidden_input_focus_obligation_applies() {
    let mut rig = hidden_rig();
    rig.view().ctx().pending_input_focus = Some(rig.state.pane.config.own_pane.clone());
    assert_hidden_obligation(rig);
}

#[test]
fn hidden_repair_obligation_applies() {
    let mut rig = hidden_rig();
    rig.state.pane.pending_focus_repair = Some(PendingFocusRepair {
        pane_id: rig.state.pane.config.own_pane.clone(),
        generation: 1,
        clients: vec![],
        sent_at_ms: rig.state.pane.clock.now_ms(),
    });
    assert_hidden_obligation(rig);
}

fn unread_snapshot(rig: &Rig, unread: bool) -> SidebarSnapshot {
    let mut snapshot = rig.state.views.mirror().clone();
    let row = &mut Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)[0];
    row.unread = unread;
    if let crate::store::snapshot::RowCard::Agent(card) = &mut row.card {
        Arc::make_mut(card).status = crate::agents::AgentStatus::Waiting;
    }
    snapshot
}

fn capture_bells(rig: &mut Rig) -> FrameOutput {
    rig.state.pane.config.room.notification_prefs.desktop =
        crate::config::DesktopNotificationMode::Off;
    let output = FrameOutput::default();
    rig.terminal = Terminal::with_options(
        PaneBackend::headless(output.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 54, 24)),
        },
    )
    .unwrap();
    output
}

#[test]
fn notification_rings_against_the_deferred_unread_fold() {
    for drain in [false, true] {
        let mut rig = hidden_rig();
        let output = capture_bells(&mut rig);
        let snapshot = unread_snapshot(&rig, true);
        let panes = snapshot.own_view.as_ref().unwrap().working_pane_ids.clone();
        rig.send(rig.lazy_update(snapshot, SnapshotSource::Produced));
        if drain {
            rig.state.on_snapshot();
        }
        assert!(
            !rig.view().ctx().current.rows().next().unwrap().unread,
            "unread fold deferred"
        );
        rig.event(SidebarEvent::Notify {
            title: "waiting".into(),
            body: "waiting".into(),
            panes,
            recheck_unread: true,
            notification_kind: Some("waiting".into()),
        });
        assert_eq!(*output.0.lock().unwrap(), b"\x07");
    }
}

#[test]
fn zellij_room_reminder_banner_comes_from_the_watched_pane_only() {
    let notify = |kind: &str, recheck_unread| SidebarEvent::Notify {
        title: "RimZ: 1 unread row needs you".into(),
        body: "reminder".into(),
        panes: own_view().working_pane_ids,
        recheck_unread,
        notification_kind: Some(kind.into()),
    };
    let mut outputs = Vec::new();
    for watched in [true, false] {
        let mut rig = hidden_rig();
        rig.state.pane.config.room.mux = MuxName::Zellij;
        let output = capture_bells(&mut rig);
        rig.state.pane.config.room.notification_prefs.desktop =
            crate::config::DesktopNotificationMode::Osc;
        if watched {
            let mut snapshot = rig.state.views.mirror().clone();
            snapshot.viewed_panes = own_view().working_pane_ids;
            rig.lazy_fold(snapshot, SnapshotSource::Produced);
            output.0.lock().unwrap().clear();
        }
        rig.event(notify("reminder", false));
        let reminder = std::mem::take(&mut *output.0.lock().unwrap());
        rig.event(notify("waiting", false));
        let agent = std::mem::take(&mut *output.0.lock().unwrap());
        outputs.push((watched, reminder, agent));
    }
    let banner = |bytes: &[u8]| bytes.windows(4).any(|window| window == b"]777");
    for (watched, reminder, agent) in outputs {
        assert_eq!(
            banner(&reminder),
            watched,
            "reminder banner, watched={watched}"
        );
        assert!(
            reminder.ends_with(b"\x07"),
            "every owning pane rings its tab bell"
        );
        assert!(
            banner(&agent),
            "an agent notification keeps its Zellij emitters"
        );
    }
}

#[test]
fn arrival_settles_spend_accrued_while_hidden() {
    let mut pair = super::scroll::SharedPanes::new(MuxName::Zellij);
    set_cost(&mut pair.snapshot, 1.0);
    pair.sample(&[(1, 0)]);
    pair.paint(0);
    pair.sample(&[]);
    assert!(!pair.panes[0].state.observed_watched());
    let applications = pair.panes[0].state.pane.content_applications;
    pair.clock.advance(Duration::from_secs(1));
    set_cost(&mut pair.snapshot, 5.0);
    pair.sample(&[]);
    assert_eq!(pair.panes[0].state.pane.content_applications, applications);
    pair.sample(&[(1, 1)]);
    pair.paint(1);
    let arrived = pair.view(1);
    assert_eq!(arrived.viewing_own_tab, Some(true));
    let phase = arrived.animation_phase;
    assert!(
        !arrived.cost_rolls.any_rolling(phase),
        "arrival snaps card cost"
    );
    assert!(
        !arrived.tally.any_rolling(phase),
        "arrival snaps the headline"
    );
    set_cost(&mut pair.snapshot, 9.0);
    pair.sample(&[(1, 1)]);
    pair.paint(1);
    let watched = pair.view(1);
    assert!(
        watched.cost_rolls.any_rolling(watched.animation_phase),
        "a climb seen while watched still rolls"
    );
}

fn set_cost(snapshot: &mut SidebarSnapshot, usd: f64) {
    let row = &mut Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)[0];
    row.as_agent_mut().unwrap().delegated_cost_usd = Some(usd);
}

#[test]
fn continuously_painted_view_keeps_its_spend_roll_when_moving_tabs() {
    let mut pair = super::scroll::SharedPanes::new(MuxName::Zellij);
    set_cost(&mut pair.snapshot, 1.0);
    pair.sample(&[(1, 0)]);
    pair.paint(0);
    set_cost(&mut pair.snapshot, 5.0);
    pair.sample(&[(1, 0)]);
    pair.paint(0);
    let before = pair.view(0);
    let phase = before.animation_phase + 1;
    let row = pair.snapshot.worktree_groups[0].rows[0].id.clone();
    let display = before.cost_rolls.display(&row, 5.0, phase);
    assert!(before.cost_rolls.any_rolling(phase));
    pair.sample(&[(1, 1)]);
    pair.paint(1);
    let after = pair.view(1);
    assert!(after.cost_rolls.any_rolling(phase));
    assert_eq!(
        after.cost_rolls.display(&row, 5.0, phase),
        display,
        "moving neither settles nor restarts an ongoing roll"
    );
}
