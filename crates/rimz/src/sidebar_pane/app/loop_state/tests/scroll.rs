use super::*;
use crate::sidebar_pane::app::inbox::WakeupInbox;

fn scroll_rig() -> (Rig, FrameOutput) {
    let mut rig = Rig::with_own_pane(pane("terminal_99", "tab_0", false).pane_id);
    let mut snapshot = agent_snapshot(&rig.ws);
    let first = snapshot.worktree_groups[0].rows[0].clone();
    let rows = Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows);
    *rows = (0..20)
        .map(|index| {
            let mut row = first.clone();
            row.id = format!("agent-{index}");
            row.name = format!("scroll-card-{index}");
            row.pane = Some(pane(&format!("terminal_{index}"), "tab_0", false));
            row
        })
        .collect();
    rig.state
        .ui
        .expanded_groups
        .insert(snapshot.worktree_groups[0].key.clone());
    rig.state.current = snapshot;
    let output = FrameOutput::default();
    rig.terminal = Terminal::with_options(
        PaneBackend::headless(output.clone()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 24)),
        },
    )
    .unwrap();
    rig.state
        .on_input(Wakeup::Resize, &mut rig.terminal, &mut rig.fetch)
        .unwrap();
    assert!(rig.state.ui.drawn_scroll_max.unwrap() > 12);
    output.0.lock().unwrap().clear();
    output.1.store(0, std::sync::atomic::Ordering::SeqCst);
    (rig, output)
}

fn queued(wakeups: impl IntoIterator<Item = Wakeup>) -> WakeupInbox {
    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    for wakeup in wakeups {
        tx.send(wakeup).unwrap();
    }
    WakeupInbox::new(rx)
}

fn move_cards_without_input(rig: &mut Rig, first: usize, second: usize, now: Instant) {
    rig.state.ui.freeze = None;
    Arc::make_mut(&mut Arc::make_mut(&mut rig.state.current.worktree_groups)[0].rows)
        .swap(first, second);
    rig.state.dirty = true;
    rig.state.paint_now(&mut rig.terminal, now).unwrap();
}

#[test]
fn automatic_moves_guard_the_first_frame_until_400ms_and_input_paints_clear_it() {
    for (click_ms, input_paint, second_move, target) in [
        (399, false, false, 1),
        (400, false, false, 2),
        (100, true, false, 2),
        (399, false, true, 1),
        (400, false, true, 3),
    ] {
        let (mut rig, _) = scroll_rig();
        let row = rig.state.ui.interactions.line_for_row(1).unwrap() as u16;
        let now = Instant::now();
        move_cards_without_input(&mut rig, 1, 2, now);
        if input_paint {
            rig.state
                .apply_input_at(
                    [Wakeup::Scroll { down: false }],
                    &mut rig.terminal,
                    now + Duration::from_millis(50),
                )
                .unwrap();
        }
        if second_move {
            move_cards_without_input(&mut rig, 1, 3, now + Duration::from_millis(200));
        }
        let outcome = rig
            .state
            .apply_input_at(
                [Wakeup::MouseClick { column: 1, row }],
                &mut rig.terminal,
                now + Duration::from_millis(click_ms),
            )
            .unwrap();
        assert_eq!(
            outcome.effects,
            vec![InputEffect::Focus(
                pane(&format!("terminal_{target}"), "tab_0", false).pane_id
            )],
            "click={click_ms}, input={input_paint}, second move={second_move}",
        );
    }
}

#[test]
fn a_guarded_card_that_left_is_swallowed_without_selection_paint_or_rearm() {
    let (mut rig, output) = scroll_rig();
    let row = rig.state.ui.interactions.line_for_row(1).unwrap() as u16;
    let now = Instant::now();
    Arc::make_mut(&mut Arc::make_mut(&mut rig.state.current.worktree_groups)[0].rows).remove(1);
    rig.state.dirty = true;
    rig.state.paint_now(&mut rig.terminal, now).unwrap();
    layout_freeze::arm_freeze(&mut rig.state.ui, 1);
    let selection = rig.state.ui.selected_pane.clone();
    let before = paints(&output);
    let outcome = rig
        .state
        .apply_input_at(
            [Wakeup::MouseClick { column: 1, row }],
            &mut rig.terminal,
            now + Duration::from_millis(399),
        )
        .unwrap();
    assert_eq!(
        outcome,
        InputOutcome::default(),
        "do not click the replacement card"
    );
    assert_eq!(rig.state.ui.selected_pane, selection);
    assert_eq!(paints(&output), before);
    assert_eq!(rig.state.ui.freeze.as_ref().unwrap().armed_ms, 1);
}

#[test]
fn resize_delivery_is_not_a_user_input_paint_for_the_click_guard() {
    let (mut rig, _) = scroll_rig();
    let row = rig.state.ui.interactions.line_for_row(1).unwrap() as u16;
    let now = Instant::now();
    move_cards_without_input(&mut rig, 1, 2, now);
    rig.state
        .apply_input_at(
            [Wakeup::Resize],
            &mut rig.terminal,
            now + Duration::from_millis(100),
        )
        .unwrap();
    let outcome = rig
        .state
        .apply_input_at(
            [Wakeup::MouseClick { column: 1, row }],
            &mut rig.terminal,
            now + Duration::from_millis(399),
        )
        .unwrap();
    assert_eq!(
        outcome.effects,
        vec![InputEffect::Focus(
            pane("terminal_1", "tab_0", false).pane_id
        )]
    );
}

#[test]
fn row_click_resolves_painted_identity_through_an_unpainted_fold() {
    let (mut rig, _) = scroll_rig();
    let row = rig.state.ui.interactions.line_for_row(1).unwrap() as u16;
    Arc::make_mut(&mut Arc::make_mut(&mut rig.state.current.worktree_groups)[0].rows).swap(1, 2);
    let outcome = rig
        .state
        .apply_input_at(
            [Wakeup::MouseClick { column: 1, row }],
            &mut rig.terminal,
            Instant::now(),
        )
        .unwrap();
    assert_eq!(
        outcome.effects,
        vec![InputEffect::Focus(
            pane("terminal_1", "tab_0", false).pane_id
        )]
    );
}

fn dispatch(rig: &mut Rig, inbox: &mut WakeupInbox) {
    inbox
        .dispatch_next(
            &mut rig.state,
            &mut rig.fetch,
            &mut rig.terminal,
            Duration::ZERO,
        )
        .unwrap();
}

fn paints(output: &FrameOutput) -> usize {
    // Cell-only frames have no sync bracket. The backend flushes its output
    // once at frame completion, even when the ratatui diff is empty.
    output.1.load(std::sync::atomic::Ordering::SeqCst)
}

fn visible(rig: &Rig) -> usize {
    rig.state
        .ui
        .scroll_ease
        .visible(rig.state.ui.scroll_offset, rig.state.ui.frame_ms)
}

fn paint_at(rig: &mut Rig, elapsed_ms: u64) {
    rig.state.anim_start = Instant::now() - Duration::from_millis(elapsed_ms);
    rig.state.next_frame = Instant::now();
    rig.paint(true);
}

fn hidden_destination() -> (Rig, FrameOutput, crate::mux::focus_anchor::FocusAnchor) {
    use crate::mux::focus_anchor::{FocusAnchor, FocusIntentState, FocusPlacement};
    let (mut rig, output) = scroll_rig();
    let target = rig.state.current.worktree_groups[0].rows[4].clone();
    let pane_id = target.pane.as_ref().unwrap().pane_id.clone();
    let mut pulled = rig.state.current.clone();
    pulled.own_view = Some(SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![pane_id.clone()],
        own_view_is_daemon: false,
    });
    pulled.focused_pane = Some(pane("terminal_0", "tab_0", false).pane_id);
    pulled.viewed_panes.clear();
    pulled.pane_session_name = Some("rimz-test".to_owned());
    rig.state.optimistic_watch_until = None;
    rig.state.ui.scroll_offset = 30;
    rig.fold(pulled, SnapshotSource::Produced);
    assert!(!rig.state.watched());
    let anchor = FocusAnchor {
        nonce: crate::ids::FocusNonce::new(),
        session_name: "rimz-test".to_owned(),
        pane_id,
        origin: FocusOrigin::User,
        repair_generation: None,
        issued_at_ms: crate::utils::time::unix_now_ms(),
        applied_at_ms: None,
        state: FocusIntentState::Requested,
        pre_action: Vec::new(),
        placement: Some(FocusPlacement {
            row_id: target.id,
            viewport_row: 2,
            held: std::collections::BTreeSet::new(),
        }),
        order: Some(rig.state.ui.last_order.clone()),
    };
    (rig, output, anchor)
}

#[test]
fn shared_placement_captures_the_painted_opening_set() {
    let (mut rig, _) = scroll_rig();
    let target = rig.state.current.worktree_groups[0].rows[1]
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    rig.state.ui.selected_index = Some(0);
    rig.state.ui.selected_pane = rig.state.current.worktree_groups[0].rows[0]
        .pane
        .as_ref()
        .map(|pane| pane.pane_id.clone());
    rig.state.dirty = true;
    paint_at(&mut rig, 0);
    let row = rig.state.ui.interactions.line_for_row(1).unwrap() as u16;
    rig.state
        .apply_input([Wakeup::MouseClick { column: 1, row }], &mut rig.terminal)
        .unwrap();
    assert_eq!(rig.state.ui.drawn_selection.as_deref(), Some("agent-1"));
    assert!(
        rig.state.ui.held_expansions.contains("agent-0"),
        "the click paint already retains A under its freeze"
    );
    let placement = rig.state.focus_placement(&target).expect("visible target");
    let mut expected = rig.state.ui.held_expansions.clone();
    expected.insert("agent-1".into());
    assert_eq!(placement.held, expected);
}

fn assert_shared_placement_from_history(previous: usize) {
    let (mut source, _) = scroll_rig();
    let first = source.state.current.worktree_groups[0].rows[0]
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    source.state.ui.selected_index = Some(0);
    source.state.ui.selected_pane = Some(first.clone());
    source.state.dirty = true;
    paint_at(&mut source, 0);
    let target = source.state.current.worktree_groups[0].rows[1].clone();
    let target_pane = target.pane.as_ref().unwrap().pane_id.clone();
    let row = source.state.ui.interactions.line_for_row(1).unwrap() as u16;
    source
        .state
        .apply_input(
            [Wakeup::MouseClick { column: 1, row }],
            &mut source.terminal,
        )
        .unwrap();
    assert_eq!(source.state.ui.drawn_selection.as_deref(), Some("agent-1"));
    let placement = source
        .state
        .focus_placement(&target_pane)
        .expect("source painted target");
    let source_y = source
        .state
        .ui
        .painted_rows
        .iter()
        .find(|(id, _)| id == &target.id)
        .unwrap()
        .1;

    let (mut destination, _, mut anchor) = hidden_destination();
    destination.state.current = source.state.current.clone();
    destination.state.ui.selected_index = Some(previous);
    destination.state.ui.selected_pane = Some(
        destination.state.current.worktree_groups[0].rows[previous]
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .clone(),
    );
    destination.state.dirty = true;
    destination
        .state
        .paint_now(&mut destination.terminal, Instant::now())
        .unwrap();
    destination.state.current.own_view = Some(SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: vec![target_pane.clone()],
        own_view_is_daemon: false,
    });
    destination.state.current.focused_pane = Some(first);
    destination.state.current.viewed_panes.clear();
    destination.state.optimistic_watch_until = None;
    anchor.pane_id = target_pane.clone();
    anchor.placement = Some(placement);
    anchor.order = Some(source.state.ui.last_order.clone());
    let (push, ack) = focus_push(anchor, destination.state.config.instance_id.clone());
    destination
        .state
        .on_wakeup(&mut destination.fetch, &mut destination.terminal, push)
        .unwrap();

    assert_eq!(destination.state.ui.selected_pane, Some(target_pane));
    assert!(destination.state.ui.focus_placement.is_none());
    assert_eq!(
        destination
            .state
            .ui
            .painted_rows
            .iter()
            .find(|(id, _)| id == &target.id)
            .unwrap()
            .1,
        source_y
    );
    assert_push_report(&ack, &destination, PushVerdict::Painted, true);
    for (id, y) in source
        .state
        .ui
        .painted_rows
        .iter()
        .filter(|(_, y)| *y <= source_y)
    {
        assert_eq!(
            destination
                .state
                .ui
                .painted_rows
                .iter()
                .find(|(painted, _)| painted == id)
                .map(|(_, row)| row),
            Some(y),
            "landing preserves the post-click rows above and including B"
        );
    }
}

#[test]
fn shared_placement_lands_after_the_destination_previously_selected_the_target() {
    assert_shared_placement_from_history(1);
}

#[test]
fn shared_placement_lands_after_the_destination_previously_selected_the_source() {
    assert_shared_placement_from_history(0);
}

#[test]
fn pushed_hidden_destination_paints_placement_without_disk_and_keeps_it_after_switch() {
    let (mut rig, output, anchor) = hidden_destination();
    let target = anchor.pane_id.clone();
    let placement = anchor.placement.clone().unwrap();
    let (push, ack) = focus_push(anchor.clone(), rig.state.config.instance_id.clone());
    assert!(!rig.runtime.focus_anchor_path().exists());
    rig.state
        .on_wakeup(&mut rig.fetch, &mut rig.terminal, push)
        .unwrap();
    assert_eq!(rig.state.ui.selected_pane, Some(target.clone()));
    assert!(
        rig.state.watched(),
        "the overlay must open the existing paint gate"
    );
    assert_eq!(
        paints(&output),
        1,
        "the push paints inside on_wakeup, before maintenance"
    );
    assert_push_report(&ack, &rig, PushVerdict::Painted, true);
    rig.paint(true);
    assert_eq!(
        paints(&output),
        1,
        "the later frame phase must not paint twice"
    );
    let y = |rig: &Rig| {
        rig.state
            .ui
            .painted_rows
            .iter()
            .find(|(id, _)| id == &placement.row_id)
            .unwrap()
            .1
    };
    assert_eq!(
        y(&rig),
        rig.state.ui.scroll_zone_top as isize + placement.viewport_row
    );
    assert_eq!(rig.state.ui.last_focus_anchor_ms, anchor.issued_at_ms);
    let old_y = y(&rig);
    let mut switched = rig.state.current.clone();
    switched.focused_pane = Some(target.clone());
    switched.viewed_panes = vec![target];
    rig.fold(switched, SnapshotSource::Produced);
    assert!(!rig.state.ui.focus_group_reveal);
    assert!(rig.state.ui.focus_placement.is_none());
    paint_at(&mut rig, 100);
    assert_eq!(
        y(&rig),
        old_y,
        "the post-switch fold must not reveal the card again"
    );
}

#[test]
fn destination_ack_waits_through_a_paint_hold_and_releases_on_exit() {
    for exit in [false, true] {
        let (mut rig, output, anchor) = hidden_destination();
        let (push, ack) = focus_push(anchor, rig.state.config.instance_id.clone());
        rig.state
            .paint_hold
            .engage(Instant::now(), crate::utils::time::unix_now_ms());
        rig.state
            .on_wakeup(&mut rig.fetch, &mut rig.terminal, push)
            .unwrap();
        rig.paint(true);
        assert_eq!(paints(&output), 0);
        assert_eq!(
            ack.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty),
            "a blocked destination keeps its acknowledgement"
        );
        rig.state.paint_hold.release();
        rig.state.should_exit = exit;
        rig.paint(true);
        if exit {
            assert_eq!(paints(&output), 0);
            assert_push_report(&ack, &rig, PushVerdict::Exiting, true);
        } else {
            assert_eq!(paints(&output), 1);
            let report = assert_push_report(&ack, &rig, PushVerdict::Painted, true);
            assert_eq!(report.blocked_by, Some(PushBlock::PaintHold));
        }
    }
}

#[test]
fn a_late_duplicate_push_uses_the_painted_frame_unless_the_destination_is_exiting() {
    for exit in [false, true] {
        let (mut rig, output, anchor) = hidden_destination();
        let (push, ack) = focus_push(anchor.clone(), rig.state.config.instance_id.clone());
        rig.state
            .on_wakeup(&mut rig.fetch, &mut rig.terminal, push)
            .unwrap();
        rig.paint(true);
        assert_push_report(&ack, &rig, PushVerdict::Painted, true);
        rig.state.should_exit = exit;
        let (push, ack) = focus_push(anchor, rig.state.config.instance_id.clone());
        rig.state
            .on_wakeup(&mut rig.fetch, &mut rig.terminal, push)
            .unwrap();
        if exit {
            assert_push_report(&ack, &rig, PushVerdict::Exiting, false);
        } else {
            assert_push_report(&ack, &rig, PushVerdict::Painted, false);
        }
        assert_eq!(paints(&output), 1, "a duplicate push must not paint again");
    }
}

#[test]
fn cleared_anchor_without_push_still_reveals_on_the_first_target_fold() {
    let (mut rig, _, anchor) = hidden_destination();
    crate::mux::focus_anchor::store(&rig.runtime, &anchor).unwrap();
    assert!(crate::mux::focus_anchor::clear_matching(
        &rig.runtime,
        anchor.nonce
    ));
    let mut switched = rig.state.current.clone();
    switched.focused_pane = Some(anchor.pane_id.clone());
    switched.viewed_panes = vec![anchor.pane_id.clone()];
    rig.fold(switched, SnapshotSource::Produced);
    assert_eq!(rig.state.ui.selected_pane, Some(anchor.pane_id));
    assert!(
        rig.state.ui.focus_group_reveal,
        "a missed intent still uses the external-focus fallback"
    );
    assert!(rig.state.ui.focus_placement.is_none());
    assert_eq!(rig.state.ui.last_focus_anchor_ms, 0);
}

fn fitted_grow_hold(resize_first: bool) {
    for (rows, cols, own_pane, holds) in [
        (30, None, true, true),
        (30, Some(79), true, true),
        (31, Some(80), true, true),
        (30, Some(80), false, true),
        (30, Some(80), true, false),
    ] {
        let (mut rig, _, anchor) = hidden_destination();
        let mut pty = crate::sidebar_pane::app::backend::tests::Pty::open(40, 24);
        rig.terminal = Terminal::new(PaneBackend::for_fd(pty.slave()).unwrap()).unwrap();
        rig.state.prev_width = Some(40);
        rig.state.self_close.seen_sibling = true;
        let (mut wake, ack) = focus_push(anchor, rig.state.config.instance_id.clone());
        let Wakeup::FocusPush(ref mut push) = wake else {
            unreachable!()
        };
        let pane = if own_pane {
            rig.state.config.own_pane.clone().unwrap()
        } else {
            pane("terminal_100", "tab_0", false).pane_id
        };
        push.expected_panes = Some(Arc::new(std::collections::HashMap::from([(
            pane,
            crate::mux::FittedPane { rows, cols },
        )])));
        if resize_first {
            pty.resize(80, 30);
            rig.state
                .on_resize(&mut rig.fetch, &mut rig.terminal, Some(80))
                .unwrap();
            assert!(
                rig.state.paint_hold.is_engaged(),
                "without a push the stale legit bound holds the grow"
            );
        }
        rig.state
            .on_wakeup(&mut rig.fetch, &mut rig.terminal, wake)
            .unwrap();
        let painted_before_resize = !resize_first && !own_pane;
        if painted_before_resize {
            assert_push_report(&ack, &rig, PushVerdict::Painted, true);
        }
        if !resize_first {
            pty.resize(80, 30);
            rig.state
                .on_resize(&mut rig.fetch, &mut rig.terminal, Some(80))
                .unwrap();
        }
        assert_eq!(
            rig.state.paint_hold.is_engaged(),
            holds,
            "resize_first={resize_first}, rows={rows}, cols={cols:?}, own_pane={own_pane}: only the exact fitted own-pane size bypasses the hold"
        );
        if holds && !painted_before_resize {
            assert_eq!(ack.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
        }
        if holds {
            rig.state.paint_hold.release();
        }
        rig.state.dirty = true;
        rig.state.next_frame = Instant::now();
        pty.drain();
        rig.paint(true);
        if rows == 30 && !painted_before_resize {
            assert_push_report(&ack, &rig, PushVerdict::Painted, true);
        }
    }
}

#[test]
fn fitted_push_before_resize_skips_only_the_exact_size_hold() {
    fitted_grow_hold(false);
}

#[test]
fn fitted_push_after_resize_releases_only_the_exact_size_hold() {
    fitted_grow_hold(true);
}

#[test]
fn displaced_placement_reports_the_landing_in_both_ack_paths() {
    for cached in [false, true] {
        let (mut rig, _, mut anchor) = hidden_destination();
        anchor.placement.as_mut().unwrap().viewport_row = 1_000;
        if cached {
            rig.state.fold_focus_intent(anchor.clone(), Instant::now());
            rig.state
                .paint_now(&mut rig.terminal, Instant::now())
                .unwrap();
        }
        let (push, ack) = focus_push(anchor.clone(), rig.state.config.instance_id.clone());
        rig.state
            .on_wakeup(&mut rig.fetch, &mut rig.terminal, push)
            .unwrap();

        assert_eq!(rig.state.ui.selected_pane, Some(anchor.pane_id));
        assert!(rig.state.ui.focus_placement.is_none());
        assert!(rig.state.ui.anchor_clamped);
        let report = assert_push_report(&ack, &rig, PushVerdict::Displaced, !cached);
        assert_eq!(report.misplaced_paints, 0);
        assert_eq!(report.unmatched_paints, 0);
    }
}

#[test]
fn cached_focus_ack_requires_a_frame_at_the_fitted_rows_not_the_current_viewport() {
    let (mut rig, _, anchor) = hidden_destination();
    let (push, ack) = focus_push(anchor.clone(), rig.state.config.instance_id.clone());
    rig.state
        .on_wakeup(&mut rig.fetch, &mut rig.terminal, push)
        .unwrap();
    rig.paint(true);
    assert_push_report(&ack, &rig, PushVerdict::Painted, true);
    rig.terminal = Terminal::with_options(
        PaneBackend::headless(FrameOutput::default()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 30)),
        },
    )
    .unwrap();
    let (mut wake, ack) = focus_push(anchor, rig.state.config.instance_id.clone());
    let Wakeup::FocusPush(ref mut push) = wake else {
        unreachable!()
    };
    push.expected_panes = Some(Arc::new(std::collections::HashMap::from([(
        rig.state.config.own_pane.clone().unwrap(),
        crate::mux::FittedPane {
            rows: 30,
            cols: None,
        },
    )])));
    rig.state
        .on_wakeup(&mut rig.fetch, &mut rig.terminal, wake)
        .unwrap();
    assert_eq!(
        ack.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "the cached frame was painted at 24 rows, not 30"
    );
    rig.state.dirty = true;
    rig.state.next_frame = Instant::now();
    rig.paint(true);
    assert_push_report(&ack, &rig, PushVerdict::Painted, false);
}

#[test]
fn parked_focus_ack_waits_for_resize_but_other_panes_rows_do_not_gate_it() {
    for own_rows in [true, false] {
        let (mut rig, _, anchor) = hidden_destination();
        let (mut wake, ack) = focus_push(anchor, rig.state.config.instance_id.clone());
        let Wakeup::FocusPush(ref mut push) = wake else {
            unreachable!()
        };
        let pane = if own_rows {
            rig.state.config.own_pane.clone().unwrap()
        } else {
            pane("terminal_100", "tab_0", false).pane_id
        };
        push.expected_panes = Some(Arc::new(std::collections::HashMap::from([(
            pane,
            crate::mux::FittedPane {
                rows: 30,
                cols: None,
            },
        )])));
        rig.state
            .on_wakeup(&mut rig.fetch, &mut rig.terminal, wake)
            .unwrap();
        rig.paint(true);
        if own_rows {
            assert_eq!(
                ack.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty),
                "an old-height paint must not acknowledge a parked push"
            );
            rig.terminal = Terminal::with_options(
                PaneBackend::headless(FrameOutput::default()),
                ratatui::TerminalOptions {
                    viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 80, 30)),
                },
            )
            .unwrap();
            rig.state
                .on_input(Wakeup::Resize, &mut rig.terminal, &mut rig.fetch)
                .unwrap();
        }
        assert_push_report(&ack, &rig, PushVerdict::Painted, true);
    }
}

#[test]
fn parked_push_reports_first_block_expiry_and_supersession() {
    let (mut rig, output, anchor) = hidden_destination();
    rig.state
        .paint_hold
        .engage(Instant::now(), crate::utils::time::unix_now_ms());
    let (first, first_ack) = focus_push(anchor.clone(), rig.state.config.instance_id.clone());
    rig.state
        .on_wakeup(&mut rig.fetch, &mut rig.terminal, first)
        .unwrap();
    rig.paint(true);
    let mut newer = anchor;
    newer.nonce = crate::ids::FocusNonce::new();
    newer.issued_at_ms += 1;
    let (second, second_ack) = focus_push(newer, rig.state.config.instance_id.clone());
    rig.state
        .on_wakeup(&mut rig.fetch, &mut rig.terminal, second)
        .unwrap();
    let first = assert_push_report(&first_ack, &rig, PushVerdict::Superseded, true);
    assert_eq!(first.blocked_by, Some(PushBlock::PaintHold));
    rig.paint(true);
    rig.state.pending_focus_push.as_mut().unwrap().until = Instant::now();
    rig.paint(true);
    let second = assert_push_report(&second_ack, &rig, PushVerdict::Expired, true);
    assert_eq!(second.blocked_by, Some(PushBlock::PaintHold));
    assert_eq!(paints(&output), 0);
}

#[test]
fn parked_push_counts_unmatched_and_misplaced_frames_at_the_paint_consumer() {
    let (mut rig, _, anchor) = hidden_destination();
    rig.state
        .paint_hold
        .engage(Instant::now(), crate::utils::time::unix_now_ms());
    let row_id = anchor.placement.as_ref().unwrap().row_id.clone();
    let (push, ack) = focus_push(anchor, rig.state.config.instance_id.clone());
    rig.state
        .on_wakeup(&mut rig.fetch, &mut rig.terminal, push)
        .unwrap();
    rig.state.ui.drawn_selection = None;
    rig.state.observe_paint(Instant::now());
    rig.state.ui.drawn_selection = Some(row_id);
    assert!(
        rig.state.ui.focus_placement.is_some(),
        "placement has not been consumed by a paint"
    );
    rig.state.observe_paint(Instant::now());
    assert_eq!(ack.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty));
    rig.state.should_exit = true;
    rig.paint(true);
    let report = assert_push_report(&ack, &rig, PushVerdict::Exiting, true);
    assert_eq!(report.unmatched_paints, 2);
    assert_eq!(report.misplaced_paints, 1);
}

#[test]
fn focus_click_is_watched_at_the_painted_row_until_the_next_paint() {
    let (mut rig, _) = scroll_rig();
    let (row_id, from_y) = rig.state.ui.painted_rows[1].clone();
    let target = rig
        .state
        .current
        .rows()
        .find(|row| row.id == row_id)
        .unwrap()
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    rig.state.observe_tx = tx;
    let effect = rig
        .state
        .apply_input(
            [Wakeup::MouseClick {
                column: 2,
                row: from_y as u16,
            }],
            &mut rig.terminal,
        )
        .unwrap();
    assert!(effect.effects.contains(&InputEffect::Focus(target)));
    // Deliberately bypass anchoring to exercise the detector at an actual paint.
    rig.state.ui.offset_authoritative = true;
    rig.state.ui.scroll_offset += 1;
    rig.state.dirty = true;
    paint_at(&mut rig, 0);
    let message = rx.try_recv();
    assert!(
        message.is_ok(),
        "the paint consumer must detect the clicked card moving: {message:?}"
    );
    let ObserveMsg::OwnAnomaly { draft, .. } = message.unwrap() else {
        panic!("expected own anomaly")
    };
    let wire = serde_json::to_value(draft.kind).unwrap();
    assert_eq!(wire["row_id"], row_id);
    assert_eq!(wire["from_y"], from_y);
    assert_eq!(wire["to_y"], from_y - 1);
}

#[test]
fn shared_placement_uses_the_destination_zone_instead_of_its_offset() {
    use crate::mux::focus_anchor::{FocusAnchor, FocusIntentState, FocusPlacement};
    for (viewport_row, banner) in [(-1, false), (2, false), (2, true)] {
        let (mut rig, _) = scroll_rig();
        if banner {
            let lead = &mut Arc::make_mut(
                &mut Arc::make_mut(&mut rig.state.current.worktree_groups)[0].rows,
            )[0];
            lead.unread = true;
            lead.as_agent_mut().unwrap().status = crate::agents::AgentStatus::Waiting;
        }
        let target = rig.state.current.worktree_groups[0].rows[4].clone();
        rig.state.ui.selected_index = Some(4);
        rig.state.ui.selected_pane = Some(target.pane.as_ref().unwrap().pane_id.clone());
        rig.state.ui.scroll_offset = 30;
        let stamp = crate::utils::time::unix_now_ms();
        crate::mux::focus_anchor::store(
            &rig.runtime,
            &FocusAnchor {
                nonce: crate::ids::FocusNonce::new(),
                session_name: "rimz-test".to_owned(),
                pane_id: rig.state.ui.selected_pane.clone().unwrap(),
                origin: FocusOrigin::User,
                repair_generation: None,
                issued_at_ms: stamp,
                applied_at_ms: Some(stamp),
                state: FocusIntentState::Applied,
                pre_action: Vec::new(),
                placement: Some(FocusPlacement {
                    row_id: target.id.clone(),
                    viewport_row,
                    held: std::collections::BTreeSet::new(),
                }),
                order: None,
            },
        )
        .unwrap();
        rig.state.apply_focus_anchor();
        assert!(rig.state.ui.focus_placement.is_some());
        rig.state.dirty = true;
        paint_at(&mut rig, 0);
        let theme = rig.state.ui.theme(&rig.state.current.theme);
        let frame = render::compose_lines(&rig.state.current, None, &rig.state.ui, &theme, 80, 24);
        let (_, first_line) = rig
            .state
            .ui
            .painted_rows
            .iter()
            .find(|(id, _)| id == &target.id)
            .unwrap();
        assert_eq!(*first_line, frame.top_height as isize + viewport_row);
        assert_eq!(
            rig.state
                .focus_placement(&target.pane.as_ref().unwrap().pane_id),
            Some(FocusPlacement {
                row_id: target.id.clone(),
                viewport_row,
                held: [target.id.clone()].into(),
            })
        );
        let offscreen = rig.state.current.worktree_groups[0].rows.last().unwrap();
        assert!(
            rig.state
                .focus_placement(&offscreen.pane.as_ref().unwrap().pane_id)
                .is_none()
        );
        assert_eq!(
            frame
                .interactions
                .line_for_target(&render::HitTarget::UnreadBanner)
                .is_some(),
            banner
        );
    }
}

#[test]
fn shared_placement_never_leaves_the_selected_card_off_screen() {
    use crate::mux::focus_anchor::FocusPlacement;
    // A placement below a shorter destination zone, and a placement for a row the
    // selection has since left: either way the selected card must end up visible.
    for (placed, selected, viewport_row) in [(19, 19, 30), (4, 18, 2)] {
        let (mut rig, _) = scroll_rig();
        let rows = &rig.state.current.worktree_groups[0].rows;
        let placed_id = rows[placed].id.clone();
        let target = rows[selected].clone();
        rig.state.ui.selected_index = Some(selected);
        rig.state.ui.selected_pane = Some(target.pane.as_ref().unwrap().pane_id.clone());
        rig.state.ui.scroll_offset = 0;
        rig.state.ui.focus_placement = Some(FocusPlacement {
            row_id: placed_id,
            viewport_row,
            held: std::collections::BTreeSet::new(),
        });
        rig.state.dirty = true;
        paint_at(&mut rig, 0);
        assert!(
            rig.state
                .ui
                .painted_rows
                .iter()
                .any(|(id, _)| id == &target.id),
            "selected card {selected} must be painted after placing row {placed} at {viewport_row}"
        );
    }
}

#[test]
fn focus_click_mid_glide_does_not_report_the_glide_as_displacement() {
    let (mut rig, _) = scroll_rig();
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    dispatch(&mut rig, &mut inbox);
    let started = rig.state.ui.frame_ms;
    paint_at(&mut rig, started + 40);
    assert!(rig.state.ui.scroll_ease.in_flight());
    let (tx, rx) = std::sync::mpsc::sync_channel(8);
    rig.state.observe_tx = tx;
    let (_, y) = rig.state.ui.painted_rows.last().unwrap().clone();
    rig.state
        .apply_input(
            [Wakeup::MouseClick {
                column: 2,
                row: y as u16,
            }],
            &mut rig.terminal,
        )
        .unwrap();
    assert!(rig.state.clicked_row.is_some());
    for elapsed in [60, 80, 100, 140] {
        paint_at(&mut rig, started + elapsed);
    }
    assert!(!rig.state.ui.scroll_ease.in_flight());
    assert!(rx.try_recv().is_err(), "wheel glide is not a displacement");
    assert!(
        rig.state.clicked_row.is_some(),
        "the watch survives the glide"
    );
}

#[test]
fn wheel_run_eases_then_releases_the_animation_grid() {
    let (mut rig, output) = scroll_rig();
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    dispatch(&mut rig, &mut inbox);
    assert_eq!(rig.state.ui.scroll_offset, 9);
    assert_eq!(visible(&rig), 0);
    assert_eq!(paints(&output), 1);
    assert!(rig.frame_active());
    let started = rig.state.ui.frame_ms;
    paint_at(&mut rig, started + 100);
    assert_eq!(visible(&rig), 9);
    assert!(!rig.state.ui.scroll_ease.in_flight());
    assert_eq!(
        render::animation_interval(
            &rig.state.current,
            &rig.state.ui,
            rig.state.ui.animation_phase,
            false
        ),
        Some(animation_frame(&rig.state.current))
    );
    paint_at(&mut rig, started + 2100);
    assert!(!rig.frame_active());
}

#[test]
fn click_after_wheel_ease_hits_the_landed_frame() {
    let (mut rig, _) = scroll_rig();
    let now = Instant::now();
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    dispatch(&mut rig, &mut inbox);
    let started = rig.state.ui.frame_ms;
    for elapsed in [16, 48, 100] {
        paint_at(&mut rig, started + elapsed);
    }
    assert!(!rig.state.ui.scroll_ease.in_flight());
    let index = rig.state.ui.interactions.visible_row_span().unwrap().0 + 1;
    let row = rig.state.ui.interactions.line_for_row(index).unwrap() as u16;
    let id = &rig.state.ui.painted_roster[index];
    let expected = rig
        .state
        .current
        .rows()
        .find(|row| &row.id == id)
        .unwrap()
        .pane
        .as_ref()
        .unwrap()
        .pane_id
        .clone();
    let outcome = rig
        .state
        .apply_input_at(
            [Wakeup::MouseClick { column: 1, row }],
            &mut rig.terminal,
            now + Duration::from_millis(200),
        )
        .unwrap();
    assert_eq!(
        outcome.effects,
        vec![InputEffect::Focus(expected)],
        "the glide continues the wheel input, not an automatic layout move"
    );
}

#[test]
fn late_wheel_frame_matches_regular_playback() {
    let (mut regular, _) = scroll_rig();
    let (mut late, _) = scroll_rig();
    let mut starts = Vec::new();
    for rig in [&mut regular, &mut late] {
        let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
        dispatch(rig, &mut inbox);
        starts.push(rig.state.ui.frame_ms);
    }
    for elapsed in [16, 32, 48, 64] {
        paint_at(&mut regular, starts[0] + elapsed);
    }
    paint_at(&mut late, starts[1] + 64);
    assert!((1..9).contains(&visible(&late)));
    assert_eq!(visible(&regular), visible(&late));
}

#[test]
fn selection_key_snaps_an_in_flight_wheel_run() {
    for count in [1, 3] {
        let (mut rig, _) = scroll_rig();
        rig.input(KeyAction::Down);
        let mut inbox = queued((0..count).map(|_| Wakeup::Scroll { down: true }));
        dispatch(&mut rig, &mut inbox);
        assert!(rig.state.ui.scroll_ease.in_flight());
        rig.input(KeyAction::Down);
        assert!(!rig.state.ui.scroll_ease.in_flight());
        assert_eq!(visible(&rig), rig.state.ui.scroll_offset);
        let (first, last) = rig.state.ui.interactions.visible_row_span().unwrap();
        assert!((first..=last).contains(&rig.state.ui.selected_index.unwrap()));
    }
}

#[test]
fn wheel_input_pulls_the_next_grid_frame_in() {
    let (mut rig, _) = scroll_rig();
    rig.state.next_frame = Instant::now() + Duration::from_secs(1);
    assert!(!rig.frame_active(), "the run starts from an idle loop");
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    rig.step(&mut inbox);
    assert!(
        rig.state
            .next_frame
            .saturating_duration_since(Instant::now())
            <= render::SCROLL_EASE_FRAME
    );
}

#[test]
fn queued_wheel_ticks_paint_once_at_the_final_offset() {
    for count in [1, 4, 30] {
        let (mut rig, output) = scroll_rig();
        let selected = rig.state.ui.selected_pane.clone();
        let max = rig.state.ui.drawn_scroll_max.unwrap();
        let mut inbox = queued((0..count).map(|_| Wakeup::Scroll { down: true }));
        dispatch(&mut rig, &mut inbox);
        assert_eq!(rig.state.ui.scroll_offset, (count * 3).min(max));
        assert_eq!(paints(&output), 1);
        assert_eq!(rig.state.ui.selected_pane, selected);
        assert!(
            !rig.state.ui.offset_authoritative,
            "paint consumed the wheel intent"
        );
        assert!(rig.state.ui.freeze.is_some());
        assert!(!rig.state.dirty);
    }
}

#[test]
fn wheel_run_matches_per_tick_paints_at_both_edges() {
    for (bottom, directions) in [
        (false, vec![false, false, true, true]),
        (true, vec![true, true, true, true, false, false]),
    ] {
        let (mut batched, _) = scroll_rig();
        let (mut single, _) = scroll_rig();
        for rig in [&mut batched, &mut single] {
            if bottom {
                rig.state.ui.scroll_offset = rig.state.ui.drawn_scroll_max.unwrap();
            }
        }
        let mut inbox = queued(directions.iter().map(|&down| Wakeup::Scroll { down }));
        dispatch(&mut batched, &mut inbox);
        for down in directions {
            single
                .state
                .on_input(
                    Wakeup::Scroll { down },
                    &mut single.terminal,
                    &mut single.fetch,
                )
                .unwrap();
        }
        assert_eq!(
            batched.state.ui.scroll_offset,
            single.state.ui.scroll_offset
        );
    }
}

#[test]
fn key_between_runs_acts_on_the_first_painted_frame() {
    let (mut rig, output) = scroll_rig();
    let mut inbox = queued([
        Wakeup::Scroll { down: true },
        Wakeup::Scroll { down: true },
        Wakeup::Key(KeyAction::ScreenTop),
        Wakeup::Scroll { down: true },
    ]);
    dispatch(&mut rig, &mut inbox);
    assert_eq!(rig.state.ui.scroll_offset, 6);
    assert_eq!(paints(&output), 1);
    // ScreenTop resolves the row from the frame the preceding run painted.
    let expected = rig.state.ui.interactions.visible_row_span().unwrap().0;
    dispatch(&mut rig, &mut inbox);
    assert_eq!(rig.state.ui.selected_index, Some(expected));
    assert_eq!(paints(&output), 2);
    let offset = rig.state.ui.scroll_offset;
    dispatch(&mut rig, &mut inbox);
    assert_eq!(rig.state.ui.scroll_offset, offset + 3);
    assert_eq!(paints(&output), 3);
}

#[test]
fn help_dismissal_consumes_only_the_first_tick_of_a_run() {
    let (mut rig, output) = scroll_rig();
    rig.input(KeyAction::Help);
    output.0.lock().unwrap().clear();
    output.1.store(0, std::sync::atomic::Ordering::SeqCst);
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    dispatch(&mut rig, &mut inbox);
    assert!(!rig.state.ui.help_visible);
    assert_eq!(rig.state.ui.scroll_offset, 6);
    assert_eq!(paints(&output), 1);
}

#[test]
fn hidden_run_stays_dirty_until_watched() {
    let (mut rig, output) = scroll_rig();
    let cards = rig.state.current.worktree_groups.clone();
    rig.hide();
    rig.state.current.worktree_groups = cards;
    let mut inbox = queued((0..3).map(|_| Wakeup::Scroll { down: true }));
    dispatch(&mut rig, &mut inbox);
    assert_eq!(rig.state.ui.scroll_offset, 9);
    assert!(rig.state.dirty);
    assert!(output.0.lock().unwrap().is_empty());
    assert_eq!(paints(&output), 0);
    rig.watch();
    rig.state.next_frame = Instant::now();
    rig.paint(true);
    assert_eq!(rig.state.ui.scroll_offset, 9);
    assert_eq!(paints(&output), 1);
    assert!(!rig.state.dirty);
}
