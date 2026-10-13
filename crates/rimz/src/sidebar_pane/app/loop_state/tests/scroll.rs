use super::*;
use crate::sidebar_pane::app::attachment::WakeupInbox;

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
        assert!(rig.state.ui.manual_scroll.is_some());
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
