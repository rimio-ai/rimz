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
fn auto_scroll_reveals_the_selected_group_header_minimally() {
    // Header lines carry the ordinal of the group's first row. Row 1 is a
    // two-line card in group 0; row 3 is a three-line card in group 1.
    let map = vec![
        None,
        Some(0),
        Some(0),
        Some(1),
        Some(1),
        None,
        Some(2),
        Some(2),
        Some(3),
        Some(3),
        Some(3),
    ];
    // Group above the window: pin the header to the top.
    assert_eq!(auto_scroll_reveal_group(&map, 0, 1, 4, 4), 1);
    // Group below the window: card lands at the bottom, header visible above.
    assert_eq!(auto_scroll_reveal_group(&map, 2, 3, 0, 5), 6);
    // Fully visible: the window doesn't move.
    assert_eq!(auto_scroll_reveal_group(&map, 0, 1, 1, 4), 1);
    // Taller than the viewport: fall back to card-follow.
    assert_eq!(
        auto_scroll_reveal_group(&map, 2, 3, 0, 4),
        auto_scroll_to_selection(&map, 3, 0, 4)
    );
    // Missing header target, as with external catch-all chrome: fall back.
    assert_eq!(
        auto_scroll_reveal_group(&map, 4, 1, 4, 4),
        auto_scroll_to_selection(&map, 1, 4, 4)
    );
    // Degenerate zero-height viewport: hold.
    assert_eq!(auto_scroll_reveal_group(&map, 0, 1, 4, 0), 4);
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
    // behind a right-rail scrollbar — the thumb at the top of the track, since
    // the selection (row 0) holds the window at the zone's start. The
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
        21,
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
    assert!(
        line_containing(&rendered, "⑂ alpha").ends_with('▐'),
        "the thumb replaces the selected header rail:\n{rendered}"
    );
    assert!(rendered.contains('▕'), "the track renders:\n{rendered}");
    assert_snapshot("scroll_overflow_shows_bar", rendered);
}
#[test]
fn help_overlay_floats_over_cards_with_scrollbar() {
    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.scrollbar = ScrollbarMode::Always;
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            help_visible: true,
            ..Default::default()
        },
        54,
        28,
    );

    let lines = rendered.lines().collect::<Vec<_>>();
    assert!(
        lines[0].contains("⌘ query-engine"),
        "cockpit stays pinned while help is open:\n{rendered}"
    );
    assert!(
        lines.last().unwrap().contains("? for help"),
        "footer stays pinned while help is open:\n{rendered}"
    );
    assert!(
        rendered.contains("keys & legend") && rendered.contains("╭") && rendered.contains("╰"),
        "the floating help box renders:\n{rendered}"
    );
    assert!(
        rendered.contains("r reload"),
        "help chrome survives narrow framing:\n{rendered}"
    );
    assert!(
        !rendered.contains("task-0"),
        "the card body clears behind the floating help box:\n{rendered}"
    );
}
#[test]
fn render_scroll_offset_follows_selection_to_bottom() {
    // Selecting the last row auto-scrolls its card fully into view. The first
    // draw establishes the fade's baseline rather than reading as a move, so
    // this frame doubles as the settled witness: no scroll activity, no bar.
    let ui = UiState {
        selected_index: 5,
        ..Default::default()
    };
    let rendered = snapshot_to_screen_with_alert_and_ui(&overflowing_fleet(), None, &ui, 38, 21);
    assert!(
        rendered.contains("task-5"),
        "the selected last card is in view:\n{rendered}"
    );
    assert!(
        !rendered.contains("task-0"),
        "the zone's head scrolled off:\n{rendered}"
    );
    let mut no_bar = overflowing_fleet();
    no_bar.theme.display.scrollbar = ScrollbarMode::Never;
    let expected = snapshot_to_screen_with_alert_and_ui(&no_bar, None, &ui, 38, 21);
    assert_eq!(
        rendered, expected,
        "a settled viewport carries no scrollbar overlay"
    );
    assert_snapshot("scroll_offset_follows_selection_to_bottom", rendered);
}

#[test]
fn focus_group_reveal_brings_selected_worktree_header_on_screen() {
    // Card-follow alone can reveal a non-first row while leaving its worktree
    // header just above the window. The focus reveal widens the target span to
    // include the header on an external focus switch.
    let following = snapshot_to_screen_with_alert_and_ui(
        &overflowing_fleet(),
        None,
        &UiState {
            selected_index: 1,
            scroll_offset: 99,
            ..Default::default()
        },
        38,
        21,
    );
    assert!(
        following.contains("task-1"),
        "card-follow still reaches the focused card:\n{following}"
    );
    assert!(
        !following.contains("⑂ alpha"),
        "card-follow leaves the worktree header above the window:\n{following}"
    );

    let revealed = snapshot_to_screen_with_alert_and_ui(
        &overflowing_fleet(),
        None,
        &UiState {
            selected_index: 1,
            scroll_offset: 99,
            focus_group_reveal: true,
            ..Default::default()
        },
        38,
        21,
    );
    assert!(
        revealed.contains("task-1"),
        "the focused card remains visible:\n{revealed}"
    );
    assert!(
        revealed.contains("⑂ alpha"),
        "the focus reveal brings the worktree header into view:\n{revealed}"
    );
}

#[test]
fn focus_group_reveal_falls_back_to_card_follow_for_external_group() {
    // The external divider maps to chrome (`None`), not a header target. A
    // focus reveal there should behave exactly like ordinary card-follow rather
    // than treating the group's first card as a header surrogate.
    let snapshot = external_overflowing_fleet();
    let mut following_ui = UiState {
        selected_index: 1,
        scroll_offset: 99,
        ..Default::default()
    };
    let theme = Theme::for_sidebar(&snapshot.theme);
    let following = compose_lines(&snapshot, None, &following_ui, &theme, 38, 21).scroll_offset;

    following_ui.focus_group_reveal = true;
    let revealed = compose_lines(&snapshot, None, &following_ui, &theme, 38, 21).scroll_offset;

    assert_eq!(
        revealed, following,
        "external groups have no worktree header to reveal, so card-follow wins"
    );
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
        snapshot_to_screen_with_alert_and_ui(&snapshot, None, &UiState::default(), 54, 22);
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
        24,
    );
    assert!(
        !rendered.contains("task-0"),
        "the selected card stays beyond the fold while pinned:\n{rendered}"
    );
    assert_snapshot("scroll_manual_offset_holds", rendered);
}
#[test]
fn scrollbar_modes_control_visibility_without_moving_the_window() {
    let settled_ui = UiState {
        scrollbar: scrolled_fade(0, 0),
        animation_phase: 11,
        ..Default::default()
    };
    let settled =
        snapshot_to_screen_with_alert_and_ui(&overflowing_fleet(), None, &settled_ui, 38, 21);
    let mut no_bar = overflowing_fleet();
    no_bar.theme.display.scrollbar = ScrollbarMode::Never;
    let expected = snapshot_to_screen_with_alert_and_ui(&no_bar, None, &settled_ui, 38, 21);
    assert_eq!(
        settled, expected,
        "the settle window has passed — no scrollbar overlay"
    );

    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.scrollbar = ScrollbarMode::Always;
    let always = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &UiState::default(), 38, 21);
    assert!(
        line_containing(&always, "⑂ alpha").ends_with('▐'),
        "always mode pins the bar with no activity:\n{always}"
    );

    let mut snapshot = overflowing_fleet();
    snapshot.theme.display.scrollbar = ScrollbarMode::Never;
    let never = snapshot_to_screen_with_alert_and_ui(
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
        !ends_with_rail(line_containing(&never, "⑂ beta")),
        "never mode leaves a non-selected rail blank even mid-scroll:\n{never}"
    );
    assert!(
        never.contains("task-2") && !never.contains("task-0"),
        "the pinned window still renders its cards:\n{never}"
    );
}

fn line_containing<'a>(rendered: &'a str, needle: &str) -> &'a str {
    rendered
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("missing {needle:?} in:\n{rendered}"))
}

fn ends_with_rail(line: &str) -> bool {
    matches!(line.chars().last(), Some('▐' | '▕'))
}

fn external_overflowing_fleet() -> SidebarSnapshot {
    let mut snapshot = overflowing_fleet();
    let mut group = snapshot.worktree_groups.remove(0);
    let beta = snapshot.worktree_groups.remove(0);
    group.key = "external".to_owned();
    group.label = "external".to_owned();
    group.kind = crate::SidebarWorktreeKind::External;
    group.status_counts = vec![crate::SidebarStatusCount {
        status: AgentStatus::Running,
        count: group.rows.len() + beta.rows.len(),
    }];
    group.rows.extend(beta.rows);
    group.diff_added = None;
    group.diff_removed = None;
    group.commits_ahead = None;
    group.commits_behind = None;
    group.trunk = None;
    group.clean = None;
    group.landed = None;
    group.trunk_sync = None;
    group.pr_state = None;
    snapshot.worktree_groups = vec![group];
    snapshot
}
