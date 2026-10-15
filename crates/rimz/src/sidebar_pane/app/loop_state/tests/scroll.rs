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
                viewport_row
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
        assert!(rig.state.ui.order_hold.is_some());
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
