//! The maintenance sweep's force-folds and watchdogs, and the idle
//! `frame_timing` verdict that decides how long the loop may sleep.

use super::*;

#[test]
fn resize_does_not_arm_or_retime_a_layout_freeze() {
    let mut rig = Rig::new();
    rig.fold(agent_snapshot(&rig.ws), SnapshotSource::Produced);
    rig.state
        .apply_input([Wakeup::Resize], &mut rig.terminal)
        .unwrap();
    assert!(rig.view().ui().freeze.is_none(), "resize is not user input");
    layout_freeze::arm_freeze(rig.view().ui(), 1);
    rig.state
        .apply_input([Wakeup::Resize], &mut rig.terminal)
        .unwrap();
    assert_eq!(rig.view().ui().freeze.as_ref().unwrap().armed_ms, 1);
}

#[test]
fn parked_freeze_waits_past_five_seconds_and_unfocus_releases_in_the_fold() {
    let (mut snapshot, own, _, _) = super::super::super::fixtures::focus_fixture();
    snapshot.viewed_panes = vec![own.clone()];
    let mut rig = Rig::new();
    rig.state.pane.config.own_pane = Some(own.clone());
    rig.fold(snapshot, SnapshotSource::Produced);
    settle_width(&mut rig);
    rig.state.pane.dirty = false;
    {
        let pane = &mut rig.state.pane;
        let mut guard = rig
            .state
            .views
            .rig(pane.config.own_pane.as_ref(), pane.clock.now());
        let (_, context) = guard.pair();
        pane.ui.theme(&context.current.theme);
    }
    let now = crate::utils::time::unix_now_ms();
    rig.state.events().append(
        SidebarEvent::FocusChanged {
            focused: vec![own.clone()],
            unfocused: vec![],
        },
        now,
        now,
    );
    layout_freeze::arm_freeze(rig.view().ui(), now as i64 - 6_000);
    assert!(rig.state.frame_timing().1 > FRAME_MIN_TIMEOUT);
    rig.maintenance();
    assert!(
        rig.next_request().is_none(),
        "parked freeze is not due at six seconds"
    );
    rig.event(SidebarEvent::FocusChanged {
        focused: vec![],
        unfocused: vec![own],
    });
    assert!(
        rig.view().ui().freeze.is_none(),
        "unfocus releases the old freeze in its own fold"
    );
}

#[test]
fn frozen_parked_timing_reads_live_overlays_outside_a_borrow() {
    let own = PaneId::from_parts(MuxName::Tmux, "%99");
    let clock = LoopClock::play(Timestamp::now());
    let mut rig = Rig::with_clock(Some(own.clone()), BodyLens::default(), None, clock.clone());
    settle_width(&mut rig);
    rig.state.pane.dirty = false;
    rig.views.lock().unwrap().reconcile(
        &[crate::pane::ClientPaneView {
            client_id: crate::ids::MuxClientId::Tmux("client-1".into()),
            pane_id: own.clone(),
        }],
        true,
        clock.now(),
        Duration::from_secs(60),
    );
    let now = clock.now_ms();
    // Attribute the live overlay as the room recorder does, without pulling or folding it.
    rig.views
        .lock()
        .unwrap()
        .note_focus_event(std::slice::from_ref(&own), &[], now);
    {
        let pane = &mut rig.state.pane;
        let mut view = rig
            .state
            .views
            .rig(pane.config.own_pane.as_ref(), pane.clock.now());
        let (ui, context) = view.pair();
        pane.ui.theme(&context.current.theme);
        layout_freeze::arm_freeze(ui, now as i64 - 6_000);
    }
    rig.state.events().append(
        SidebarEvent::FocusChanged {
            focused: vec![own.clone()],
            unfocused: vec![],
        },
        now,
        now,
    );
    assert!(rig.state.frame_timing().1 > FRAME_MIN_TIMEOUT);
    clock.advance(Duration::from_millis(1));
    rig.state.events().append(
        SidebarEvent::FocusChanged {
            focused: vec![],
            unfocused: vec![own],
        },
        now + 1,
        now + 1,
    );
    assert_eq!(rig.state.frame_timing().1, FRAME_MIN_TIMEOUT);
}

#[test]
fn frame_timing_caps_idle_wait_at_the_parked_deadline() {
    let (mut snapshot, own, _, _) = super::super::super::fixtures::focus_fixture();
    snapshot.viewed_panes = vec![own.clone()];
    let mut rig = Rig::new();
    rig.state.pane.config.own_pane = Some(own);
    rig.fold(snapshot, SnapshotSource::Produced);
    settle_width(&mut rig);
    rig.state.pane.dirty = false;
    {
        let pane = &mut rig.state.pane;
        let mut guard = rig
            .state
            .views
            .rig(pane.config.own_pane.as_ref(), pane.clock.now());
        let (_, context) = guard.pair();
        pane.ui.theme(&context.current.theme);
    }
    layout_freeze::arm_freeze(rig.view().ui(), Timestamp::now().as_millisecond() - 29_800);
    let timeout = rig.state.frame_timing().1;
    assert!(
        timeout > FRAME_MIN_TIMEOUT,
        "parked deadline is not the expired five-second deadline"
    );
    assert!(
        timeout <= Duration::from_millis(200),
        "parked deadline caps the idle wait"
    );
}

#[test]
fn parked_focus_uses_live_events_then_pulled_clients() {
    let own = PaneId::from_parts(MuxName::Zellij, "terminal_99");
    let mut rig = Rig::with_own_pane(own.clone());
    let now = crate::utils::time::unix_now_ms();
    rig.state.events().append(
        SidebarEvent::FocusChanged {
            focused: vec![own.clone()],
            unfocused: vec![],
        },
        now,
        now,
    );
    assert!(rig.state.parked(now));
    rig.state.events().append(
        SidebarEvent::FocusChanged {
            focused: vec![],
            unfocused: vec![own.clone()],
        },
        now + 1,
        now + 1,
    );
    assert!(!rig.state.parked(now + 1));
    rig.view().ctx().current.panes_observed_at_ms = Some(now + 1);
    rig.view().ctx().current.client_views = vec![crate::pane::ClientPaneView {
        pane_id: own,
        client_id: crate::ids::MuxClientId::Zellij(1),
    }];
    assert!(rig.state.parked(now + 1));
    rig.view().ctx().current.client_views.clear();
    assert!(!rig.state.parked(now + 1));
}

#[test]
fn maintenance_drains_ready_snapshot_outcomes_without_snapshot_wakeup() {
    let mut rig = Rig::new();
    let snapshot = agent_snapshot(&rig.ws);

    rig.maintenance_draining(Some(FetchUpdate::Snapshot {
        snapshot: Box::new(snapshot),
        phase: FetchPhase::Final,
        source: SnapshotSource::Cached,
    }));

    assert_eq!(rig.view().ctx().current.worktree_groups.len(), 1);
    assert_eq!(
        rig.view().ctx().current.worktree_groups[0].rows[0].name,
        "claude"
    );
    assert!(rig.state.pane.dirty, "the folded snapshot is paint-pending");
}

#[test]
fn maintenance_requests_releasing_fetch_when_freeze_expires() {
    let mut rig = Rig::new();
    rig.view().ui().freeze = Some(crate::sidebar_pane::render::LayoutFreeze {
        frozen: crate::sidebar_pane::render::FrozenOrder::default(),
        armed_ms: jiff::Timestamp::now().as_millisecond() - 5_001,
    });

    rig.maintenance();

    assert!(
        rig.next_request().is_some(),
        "expired layout freeze schedules a fold to release the frozen order"
    );
}

#[test]
fn maintenance_requests_force_fold_when_tab_read_dwell_expires() {
    let mut rig = Rig::new();
    rig.view().ctx().current.own_view = Some(own_view());
    // The user stays on the tab past the dwell.
    rig.view().ctx().current.viewed_panes = own_view().working_pane_ids;
    rig.view().ctx().tab_read_dwell_until = Some(Instant::now() - Duration::from_secs(1));

    rig.maintenance();

    assert!(
        rig.next_request()
            .expect("tab dwell fold request")
            .forces_fold(),
        "expired tab dwell must bypass the unchanged-input skip"
    );
}

#[test]
fn maintenance_waits_for_own_view_before_tab_read_dwell_fetch() {
    let mut rig = Rig::new();
    rig.view().ctx().current.own_view = None;
    rig.view().ctx().tab_read_dwell_until = Some(Instant::now() - Duration::from_secs(1));

    rig.maintenance();

    assert!(
        rig.next_request().is_none(),
        "a frameless snapshot cannot prove the tab is still viewed"
    );
}

#[test]
fn maintenance_requests_force_fold_when_gate_deadline_is_due() {
    let mut rig = Rig::new();
    rig.view().ctx().gate = armed_gate(2, 1_001);

    rig.maintenance();

    assert!(
        rig.next_request()
            .expect("gate deadline fold request")
            .forces_fold(),
        "due gate reevaluation must bypass the unchanged-input skip"
    );
}

#[test]
fn rejected_final_defers_one_gate_deadline_reevaluation() {
    let mut rig = Rig::new();
    rig.view().ctx().current = agent_snapshot(&rig.ws);

    let fold_started = Instant::now();
    rig.fold(snapshot(&rig.ws), SnapshotSource::Cached);
    let first_fold = fold_started.elapsed();

    assert_eq!(
        rig.view().ctx().gate.rule,
        Some(crate::diag::record::GateRule::FramelessOverFrame)
    );
    assert!(
        rig.next_request().is_none(),
        "a rejected final must not immediately feed another fetch"
    );
    let deadline = rig
        .fetch
        .next_deadline()
        .expect("one deferred reevaluation");
    assert!(
        deadline > Instant::now(),
        "the reevaluation waits for the gate escape hatch"
    );

    rig.fold(snapshot(&rig.ws), SnapshotSource::Cached);
    assert!(
        rig.next_request().is_none(),
        "repeated rejected finals remain one deferred fetch"
    );
    // Each rejection derives the same absolute deadline from two clocks read
    // one after the other: an `Instant` plus a gate remainder the wall clock
    // quantizes to whole milliseconds. A stall between those reads in the
    // first fold lands its deadline late by the stall, so re-arming, which
    // keeps the earlier deadline, may pull it forward by up to the first
    // fold's duration plus one quantum. It never moves later: a second
    // reevaluation of its own would sit seconds out.
    let rearmed = rig
        .fetch
        .next_deadline()
        .expect("the deferred reevaluation survives");
    assert!(
        rearmed <= deadline,
        "re-arming never pushes the first gate deadline out; moved by {:?}",
        rearmed.saturating_duration_since(deadline)
    );
    let pulled_forward = deadline.saturating_duration_since(rearmed);
    assert!(
        pulled_forward <= first_fold + Duration::from_millis(1),
        "the first gate deadline remains authoritative; pulled forward by \
         {pulled_forward:?} after a {first_fold:?} first fold"
    );
}

#[test]
fn self_close_watchdog_verifies_panes_while_empty_confirming() {
    let mut rig = Rig::new();
    let mut empty = agent_snapshot(&rig.ws);
    empty.own_view = Some(empty_own_view());
    // Birth/resurrection path: no sibling observed yet, so zero enters the
    // confirm window and the watchdog forces a fresh producer fold.
    rig.fold(empty, SnapshotSource::Produced);
    assert!(rig.state.pane.self_close.confirming_empty());

    rig.state.pane.last_self_close_check = Instant::now() - SELF_CLOSE_WATCHDOG;
    rig.maintenance();

    assert!(
        rig.next_request()
            .expect("self-close watchdog fetch")
            .is_fresh_panes(),
        "pending empty confirmation requires fresh pane truth"
    );
}

#[test]
fn frame_timing_suspends_unwatched_animation() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane.clone());
    rig.view().ctx().current = animating_agent_snapshot(&rig.ws);
    rig.view().ctx().current.own_view = Some(own_view());
    rig.view().ctx().current.viewed_panes.clear();
    rig.state.pane.dirty = false;

    assert!(!rig.frame_active(), "hidden animation suspends");

    rig.watch();
    assert!(
        rig.frame_active(),
        "a viewed sibling pane resumes animation"
    );

    rig.view().ctx().current.viewed_panes = vec![own_pane];
    assert!(
        rig.frame_active(),
        "the own pane on screen resumes animation"
    );
}

#[test]
fn frame_timing_keeps_unknown_hot_but_holds_hidden_and_detached_dirty() {
    let mut rig = Rig::without_own_pane();
    assert!(rig.state.pane.config.own_pane.is_none());
    rig.view().ctx().current = animating_agent_snapshot(&rig.ws);
    rig.state.pane.dirty = false;
    assert!(rig.frame_active(), "an unknown own pane stays hot");

    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane.clone());
    rig.view().ctx().current = animating_agent_snapshot(&rig.ws);
    rig.view().ctx().current.own_view = None;
    rig.state.pane.dirty = false;
    assert!(rig.frame_active(), "an unknown own view stays hot");

    rig.view().ctx().current.own_view = Some(own_view());
    rig.view().ctx().current.viewed_panes.clear();
    rig.state.pane.dirty = true;
    rig.view().ctx().current.presence = Some(crate::store::snapshot::SidebarPresence::Active);
    assert!(
        !rig.frame_active(),
        "hidden and attached holds the dirty frame"
    );

    rig.view().ctx().current.presence = Some(crate::store::snapshot::SidebarPresence::Detached);
    assert!(
        !rig.frame_active(),
        "detached retains its dirty frame without painting"
    );

    rig.view().ctx().current.presence = Some(crate::store::snapshot::SidebarPresence::Active);
    rig.view().ctx().current.viewed_panes = vec![own_pane];
    assert!(
        rig.frame_active(),
        "the own pane on screen paints the dirty frame"
    );
}

#[test]
fn frame_timing_resumes_on_own_pane_focus() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let foreign_pane = pane("terminal_2", "tab_0", false).pane_id;

    for (focused, resumes) in [(own_pane.clone(), true), (foreign_pane, false)] {
        let mut rig = Rig::with_own_pane(own_pane.clone());
        rig.hide();
        rig.state.pane.dirty = false;

        rig.event(SidebarEvent::FocusChanged {
            focused: vec![focused],
            unfocused: Vec::new(),
        });
        rig.state.pane.dirty = false;

        assert_eq!(rig.state.pane.optimistic_watch_until.is_some(), resumes);
        assert_eq!(rig.frame_active(), resumes);
        assert_eq!(
            rig.next_request().map(|req| req.is_fresh_panes()),
            resumes.then_some(true)
        );
    }
}

#[test]
fn frame_timing_caps_idle_wait_at_tab_read_dwell() {
    let clock = LoopClock::play("2026-06-09T12:00:00Z".parse().unwrap());
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_clock(
        Some(own_pane.clone()),
        BodyLens::default(),
        None,
        clock.clone(),
    );
    rig.view().ctx().current.own_view = Some(own_view());
    rig.view().ctx().current.viewed_panes = vec![own_pane];
    settle_width(&mut rig);
    let theme = rig.view().ctx().current.theme.clone();
    rig.state.pane.ui.theme(&theme);
    rig.state.pane.dirty = false;
    let until = clock.now() + Duration::from_millis(300);
    rig.view().ctx().tab_read_dwell_until = Some(until);

    assert!(rig.state.pane.width_control.feedback_deadline().is_none());
    let (active, timeout) = rig.state.frame_timing();

    assert!(!active);
    assert_eq!(rig.view().ctx().tab_read_dwell_until, Some(until));
    assert!(
        timeout <= until.saturating_duration_since(clock.now()),
        "idle wait wakes at the pending tab-read dwell: {timeout:?}"
    );
}

#[test]
fn frame_timing_cancels_hidden_tab_read_dwell() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane);
    let view = rig
        .state
        .with_view_for_test(Route::READ, |_, v| v.target.shared_id().unwrap());
    rig.view().ctx().current.own_view = Some(own_view());
    rig.view().ctx().current.viewed_panes.clear();
    rig.view().ctx().tab_read_dwell_until = Some(Instant::now() - Duration::from_secs(1));
    settle_width(&mut rig);

    assert!(rig.state.pane.width_control.feedback_deadline().is_none());
    rig.state.frame_timing();

    assert!(
        rig.views
            .lock()
            .unwrap()
            .retained(view)
            .unwrap()
            .tab_read_dwell_until
            .is_none()
    );
}

#[test]
fn frame_timing_caps_long_tick_at_gate_deadline() {
    let mut rig = Rig::new();
    settle_width(&mut rig);
    let theme = rig.view().ctx().current.theme.clone();
    rig.state.pane.ui.theme(&theme);
    rig.state.pane.dirty = false;
    rig.view().ctx().gate = armed_gate(1, 800);

    assert!(rig.state.pane.width_control.feedback_deadline().is_none());
    let (_active, timeout) = rig.state.frame_timing();

    assert!(
        timeout <= Duration::from_millis(200),
        "idle wait wakes at the armed gate deadline: {timeout:?}"
    );
}

#[test]
fn frame_timing_caps_idle_timeout_at_freeze_expiry() {
    let mut rig = Rig::new();
    settle_width(&mut rig);
    rig.state.pane.dirty = false;
    let theme = rig.view().ctx().current.theme.clone();
    rig.state.pane.ui.theme(&theme);
    rig.view().ui().freeze = Some(crate::sidebar_pane::render::LayoutFreeze {
        frozen: crate::sidebar_pane::render::FrozenOrder::default(),
        armed_ms: jiff::Timestamp::now().as_millisecond() - 4_800,
    });

    rig.state.pane.config.tick_seconds = 10;
    assert!(rig.state.pane.width_control.feedback_deadline().is_none());
    let (_active, timeout) = rig.state.frame_timing();

    assert!(
        timeout <= Duration::from_millis(200),
        "idle wait wakes for the layout-freeze release fold"
    );
}
