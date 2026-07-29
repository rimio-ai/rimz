//! The maintenance sweep's force-folds and watchdogs, and the idle
//! `frame_timing` verdict that decides how long the loop may sleep.

use super::*;

#[test]
fn maintenance_drains_ready_snapshot_outcomes_without_snapshot_wakeup() {
    let mut rig = Rig::new();
    let snapshot = agent_snapshot(&rig.ws);

    rig.maintenance_draining(Some(FetchUpdate::Snapshot {
        snapshot: Box::new(snapshot),
        role: FetchRole::Producer,
        phase: FetchPhase::Final,
        pane_frame: PaneFrame::Held,
        source: SnapshotSource::Published,
    }));

    assert_eq!(rig.state.current.worktree_groups.len(), 1);
    assert_eq!(rig.state.current.worktree_groups[0].rows[0].name, "claude");
    assert!(rig.state.dirty, "the folded snapshot is paint-pending");
}

#[test]
fn maintenance_requests_releasing_fetch_when_order_hold_expires() {
    let mut rig = Rig::new();
    rig.state.ui.order_hold = Some(crate::sidebar_pane::render::OrderHold {
        frozen: crate::sidebar_pane::render::FrozenOrder::default(),
        expires_ms: jiff::Timestamp::now().as_millisecond() - 1,
    });

    rig.maintenance();

    assert!(
        rig.next_request().is_some(),
        "expired order hold schedules a fold to release the frozen order"
    );
}

#[test]
fn maintenance_requests_force_fold_when_tab_read_dwell_expires() {
    let mut rig = Rig::new();
    rig.state.current.own_view = Some(own_view());
    rig.state.tab_read_dwell_until = Some(Instant::now() - Duration::from_secs(1));

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
    rig.state.current.own_view = None;
    rig.state.tab_read_dwell_until = Some(Instant::now() - Duration::from_secs(1));

    rig.maintenance();

    assert!(
        rig.next_request().is_none(),
        "a frameless snapshot cannot prove the tab is still viewed"
    );
}

#[test]
fn maintenance_requests_force_fold_when_gate_deadline_is_due() {
    let mut rig = Rig::new();
    rig.state.gate = armed_gate(2, 1_001);

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
    rig.state.current = agent_snapshot(&rig.ws);

    rig.fold(
        snapshot(&rig.ws),
        PaneFrame::Held,
        SnapshotSource::Published,
    );

    assert_eq!(
        rig.state.gate.rule,
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

    rig.fold(
        snapshot(&rig.ws),
        PaneFrame::Held,
        SnapshotSource::Published,
    );
    assert!(
        rig.next_request().is_none(),
        "repeated rejected finals remain one deferred fetch"
    );
    // Each rejection derives the same absolute deadline from two clocks: a
    // nanosecond `Instant` plus a gate remainder the wall clock quantizes to
    // whole milliseconds. Re-arming therefore lands within one quantum of the
    // first deadline rather than exactly on it; a second reevaluation of its
    // own would sit seconds out.
    let rearmed = rig
        .fetch
        .next_deadline()
        .expect("the deferred reevaluation survives");
    let drift = rearmed
        .saturating_duration_since(deadline)
        .max(deadline.saturating_duration_since(rearmed));
    assert!(
        drift <= Duration::from_millis(1),
        "the first gate deadline remains authoritative; moved by {drift:?}"
    );
}

#[test]
fn self_close_watchdog_bypasses_unchanged_skip_while_empty_confirming() {
    let mut rig = Rig::new();
    let mut empty = agent_snapshot(&rig.ws);
    empty.own_view = Some(empty_own_view());
    // Birth/resurrection path: no sibling observed yet, so zero enters the
    // confirm window and the watchdog forces a fresh producer fold.
    rig.fold(empty, PaneFrame::Fresh, SnapshotSource::Produced);
    assert!(rig.state.self_close.confirming_empty());

    rig.state.last_self_close_check = Instant::now() - SELF_CLOSE_WATCHDOG;
    rig.maintenance();

    assert!(
        rig.next_request()
            .expect("self-close watchdog fetch")
            .is_producer_fresh_panes(),
        "pending empty confirmation must bypass the unchanged consumer memo"
    );
}

#[test]
fn frame_timing_suspends_unwatched_animation() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane.clone());
    rig.state.current = animating_agent_snapshot(&rig.ws);
    rig.state.current.own_view = Some(own_view());
    rig.state.current.viewed_panes.clear();
    rig.state.dirty = false;

    assert!(!rig.frame_active(), "hidden animation suspends");

    rig.watch();
    assert!(
        rig.frame_active(),
        "a viewed sibling pane resumes animation"
    );

    rig.state.current.viewed_panes = vec![own_pane];
    assert!(
        rig.frame_active(),
        "the own pane on screen resumes animation"
    );
}

#[test]
fn frame_timing_keeps_unknown_or_detached_dirty_hot_but_holds_hidden_dirty() {
    let mut rig = Rig::new();
    rig.state.current = animating_agent_snapshot(&rig.ws);
    rig.state.dirty = false;
    assert!(rig.frame_active(), "an unknown own pane stays hot");

    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane.clone());
    rig.state.current = animating_agent_snapshot(&rig.ws);
    rig.state.current.own_view = None;
    rig.state.dirty = false;
    assert!(rig.frame_active(), "an unknown own view stays hot");

    rig.state.current.own_view = Some(own_view());
    rig.state.current.viewed_panes.clear();
    rig.state.dirty = true;
    rig.state.current.presence = Some(crate::SidebarPresence::Active);
    assert!(
        !rig.frame_active(),
        "hidden and attached holds the dirty frame"
    );

    rig.state.current.presence = Some(crate::SidebarPresence::Detached);
    assert!(
        rig.frame_active(),
        "detached cannot prove hidden, so it stays hot"
    );

    rig.state.current.presence = Some(crate::SidebarPresence::Active);
    rig.state.current.viewed_panes = vec![own_pane];
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
        rig.hide_consumer();
        rig.state.dirty = false;

        rig.event(SidebarEvent::FocusChanged {
            focused: vec![focused],
            unfocused: Vec::new(),
        });
        rig.state.dirty = false;

        assert_eq!(rig.state.optimistic_watch_until.is_some(), resumes);
        assert_eq!(rig.frame_active(), resumes);
        assert_eq!(
            rig.next_request().map(|req| req.is_producer_fresh_panes()),
            resumes.then_some(true)
        );
    }
}

#[test]
fn frame_timing_wakes_for_elapsed_tab_read_dwell() {
    let own_pane = pane("terminal_1", "tab_0", false).pane_id;
    let mut rig = Rig::with_own_pane(own_pane);
    rig.state.current = animating_agent_snapshot(&rig.ws);
    rig.state.current.own_view = Some(own_view());
    rig.state.current.viewed_panes.clear();
    rig.state.dirty = false;
    rig.state.tab_read_dwell_until = Some(Instant::now() - Duration::from_secs(1));

    let (active, timeout) = rig
        .state
        .frame_timing(Duration::from_secs(60), Instant::now());

    assert!(!active);
    assert_eq!(timeout, FRAME_MIN_TIMEOUT);
}

#[test]
fn frame_timing_caps_long_tick_at_gate_deadline() {
    let mut rig = Rig::new();
    rig.state.dirty = false;
    rig.state.gate = armed_gate(1, 800);

    let (_active, timeout) = rig
        .state
        .frame_timing(Duration::from_secs(60), Instant::now());

    assert!(
        timeout <= Duration::from_millis(200),
        "idle wait wakes at the armed gate deadline: {timeout:?}"
    );
}

#[test]
fn frame_timing_caps_idle_timeout_at_order_hold_expiry() {
    let mut rig = Rig::new();
    rig.state.dirty = false;
    let theme = rig.state.current.theme.clone();
    rig.state.ui.theme(&theme);
    rig.state.ui.order_hold = Some(crate::sidebar_pane::render::OrderHold {
        frozen: crate::sidebar_pane::render::FrozenOrder::default(),
        expires_ms: jiff::Timestamp::now().as_millisecond() + 200,
    });

    let (_active, timeout) = rig
        .state
        .frame_timing(Duration::from_secs(10), Instant::now());

    assert!(
        timeout <= Duration::from_millis(200),
        "idle wait wakes for the order-hold release fold"
    );
}
