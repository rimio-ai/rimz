use super::*;

#[test]
fn auto_scroll_nudges_the_selection_minimally_into_view() {
    // A hand-built scroll-zone map: a leading gap, row 0 on lines 1-2, row 1
    // on lines 3-4, row 2 an expanded card on lines 5-8.
    let map = vec![
        None,
        Some(0),
        Some(0),
        Some(1),
        Some(1),
        Some(2),
        Some(2),
        Some(2),
        Some(2),
    ];
    // Fully visible: the window doesn't move.
    assert_eq!(auto_scroll_to_selection(&map, 1, 0, 5), 0);
    // Above the window: scroll up to the card's first line.
    assert_eq!(auto_scroll_to_selection(&map, 0, 4, 5), 1);
    // Below the window: scroll down just enough for its last line.
    assert_eq!(auto_scroll_to_selection(&map, 2, 0, 5), 4);
    // Taller than the viewport: pin the card's first line to the top.
    assert_eq!(auto_scroll_to_selection(&map, 2, 0, 3), 5);
    // Absent from the zone: hold the clamped offset.
    assert_eq!(auto_scroll_to_selection(&map, 9, 2, 5), 2);
    // Degenerate zero-height viewport: hold.
    assert_eq!(auto_scroll_to_selection(&map, 1, 2, 0), 2);
}
#[test]
fn scroll_thumb_reads_top_and_bottom_true() {
    // 10 zone lines through a 5-row viewport: the thumb spans half the track.
    assert_eq!(scroll_thumb(0, 10, 5), (0, 2));
    // At the bottom (offset == max) the thumb pins to the track's last rows.
    assert_eq!(scroll_thumb(5, 10, 5), (3, 2));
    // Midway it sits between, flush at neither end.
    assert_eq!(scroll_thumb(2, 10, 5), (1, 2));
    // A huge zone never shrinks the thumb below one row.
    assert_eq!(scroll_thumb(0, 1_000, 4), (0, 1));
}
#[test]
fn render_scroll_overflow_shows_bar() {
    // More cards than the frame holds, mid-scroll: the cockpit stays pinned at
    // the top, the footer at the bottom, and the cards scroll between them
    // behind a right-margin scrollbar — the thumb at the top of the track,
    // since the selection (row 0) holds the window at the zone's start. The
    // freshly-stamped fade is what shows the bar: `auto` mode paints it only
    // while the viewport moves.
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &overflowing_fleet(),
        None,
        &UiState {
            scrollbar: scrolled_fade(0, 0),
            ..Default::default()
        },
        38,
        18,
    );
    let lines: Vec<&str> = rendered.lines().collect();
    assert!(
        lines[0].contains("⌘ query-engine"),
        "cockpit pinned:\n{rendered}"
    );
    assert!(lines[5].contains("⢿ 6"), "make-up pinned:\n{rendered}");
    assert!(
        lines.last().unwrap().contains("? for help"),
        "footer pinned:\n{rendered}"
    );
    assert!(rendered.contains('▐'), "the thumb renders:\n{rendered}");
    assert!(rendered.contains('▕'), "the track renders:\n{rendered}");
    assert_snapshot("scroll_overflow_shows_bar", rendered);
}
#[test]
fn render_scroll_offset_follows_selection_to_bottom() {
    // Selecting the last row auto-scrolls its card fully into view. The first
    // draw establishes the fade's baseline rather than reading as a move, so
    // this frame doubles as the settled witness: no scroll activity, no bar.
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &overflowing_fleet(),
        None,
        &UiState {
            selected_index: 5,
            ..Default::default()
        },
        38,
        18,
    );
    assert!(
        rendered.contains("task-5"),
        "the selected last card is in view:\n{rendered}"
    );
    assert!(
        !rendered.contains("task-0"),
        "the zone's head scrolled off:\n{rendered}"
    );
    assert!(
        !rendered.contains('▐') && !rendered.contains('▕'),
        "a settled viewport carries no bar:\n{rendered}"
    );
    assert_snapshot("scroll_offset_follows_selection_to_bottom", rendered);
}
#[test]
fn render_scroll_pins_tall_expanded_card_top() {
    // A selected card whose expanded subagent list outgrows the viewport pins
    // its first line — the group header — to the top of the scroll zone.
    let now = fixed_now();
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    parent.last_activity = now - Duration::from_secs(8);
    let mut agents = vec![parent];
    for i in 0..4_u64 {
        let mut child = agent(
            &format!("child-{i}"),
            "claude",
            AgentStatus::Running,
            None,
            None,
            Some("Explore"),
        );
        child.parent_agent_id = Some("claude-1".into());
        child.subagent_description = Some(format!("survey area {i}"));
        child.subagent_started_at = Some(now - Duration::from_secs(240 - i * 30));
        child.last_activity = now;
        child.total_tokens = Some(1_000 * (i + 1));
        agents.push(child);
    }
    let snapshot = snapshot_with(Vec::new(), agents);

    let rendered =
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &UiState::default(), 54, 13);
    // The viewport opens below the pinned cockpit separator, so the card
    // block's first line holds one row lower while the subagent list fills down.
    let lines: Vec<&str> = rendered.lines().collect();
    assert!(
        lines[7].contains("⑂ main"),
        "the tall card's first line pins below the cockpit separator:\n{rendered}"
    );
    assert!(
        rendered.contains("⧉ subagents (4)"),
        "the expanded list is what overflows:\n{rendered}"
    );
    assert_snapshot("scroll_pins_tall_expanded_card_top", rendered);
}
#[test]
fn render_scroll_manual_offset_holds() {
    // A wheel pin holds the user's window even though the selection (row 0)
    // sits above the fold — the peek wins until the selection changes. The
    // bar follows movement, not the pin: a held window past the settle delay
    // is bar-less even while pinned.
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &overflowing_fleet(),
        None,
        &UiState {
            scroll_offset: 6,
            manual_scroll: Some(ManualScroll {
                selection_at_start: None,
            }),
            ..Default::default()
        },
        38,
        18,
    );
    assert!(
        !rendered.contains("task-0"),
        "the selected card stays beyond the fold while pinned:\n{rendered}"
    );
    assert_snapshot("scroll_manual_offset_holds", rendered);
}
#[test]
fn render_scrollbar_hides_after_settle() {
    // The same mid-scroll fade as `render_scroll_overflow_shows_bar`, read a
    // settle window later: the stamp has aged out, so the bar is gone while
    // the cards still overflow.
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &overflowing_fleet(),
        None,
        &UiState {
            scrollbar: scrolled_fade(0, 0),
            animation_phase: 11,
            ..Default::default()
        },
        38,
        18,
    );
    assert!(
        !rendered.contains('▐') && !rendered.contains('▕'),
        "the settle window has passed — no bar:\n{rendered}"
    );
    assert_snapshot("scrollbar_hides_after_settle", rendered);
}
#[test]
fn render_scrollbar_always_mode() {
    // `[sidebar] scrollbar = "always"`: the bar is up whenever the cards
    // overflow, no scroll activity required.
    let mut snapshot = overflowing_fleet();
    snapshot.sidebar.scrollbar = ScrollbarMode::Always;
    let rendered =
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &UiState::default(), 38, 18);
    assert!(
        rendered.contains('▐') && rendered.contains('▕'),
        "always mode pins the bar with no activity:\n{rendered}"
    );
    assert_snapshot("scrollbar_always_mode", rendered);
}
#[test]
fn render_scrollbar_never_mode() {
    // `[sidebar] scrollbar = "never"` wins over live scroll activity — a
    // freshly-stamped fade and a held wheel pin paint no bar, and the cards
    // keep their geometry (the right-margin column is reserved either way).
    let mut snapshot = overflowing_fleet();
    snapshot.sidebar.scrollbar = ScrollbarMode::Never;
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            scroll_offset: 6,
            manual_scroll: Some(ManualScroll {
                selection_at_start: None,
            }),
            scrollbar: scrolled_fade(6, 0),
            ..Default::default()
        },
        38,
        18,
    );
    assert!(
        !rendered.contains('▐') && !rendered.contains('▕'),
        "never mode paints no bar even mid-scroll:\n{rendered}"
    );
    assert!(
        rendered.contains("task-3") && !rendered.contains("task-0"),
        "the pinned window still renders its cards:\n{rendered}"
    );
    assert_snapshot("scrollbar_never_mode", rendered);
}
