use super::*;

#[test]
fn render_footer_and_help_overlay() {
    let workspace = fixed_workspace();
    let mut native = FeedItem::new(
        workspace,
        Surface::NativeUi,
        FeedKind::Permission,
        "allow?",
        "codex",
        "agent-hook",
    );
    native.worktree_branch = Some("main".to_owned());
    let codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Waiting,
        Some("/repo/main"),
        Some("main"),
        Some("allow?"),
    );
    let snapshot = snapshot_with(vec![native], vec![codex]);
    let rendered = snapshot_to_screen(&snapshot, 80, 18);
    assert!(rendered.contains("? for help"), "{rendered}");
    assert!(!rendered.contains("next"), "{rendered}");

    let help = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: true,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        },
        80,
        28,
    );
    assert!(help.contains("keys & legend"));
    assert!(help.contains("╭"), "{help}");
    assert!(help.contains("│"), "{help}");
    assert!(help.contains("╰"), "{help}");
    assert!(help.contains("j/k rows"));
    assert!(help.contains("J/K worktrees"));
    assert!(help.contains("g/G ends"));
    assert!(help.contains("n/N needs-you"));
    assert!(help.contains("l focus"));
    assert!(help.contains("m/M read / unread"));
    assert!(help.contains("Alt+p sidebar (toggle)"));
    assert!(help.contains("? q waiting"));
    assert!(help.contains("! e attention"));
    assert!(help.contains("○ o idle"));
    assert!(help.contains("any key to close"));
    assert!(
        help.contains("allow?"),
        "cards render under the floating box"
    );
    assert!(
        !help.contains("? close"),
        "legend no longer names a close key"
    );
    assert!(
        !help.contains("┄ commands"),
        "the retired seam left the legend"
    );
    assert!(!help.contains("posture"), "the posture legend is gone");
    assert_snapshot("help_overlay_floating_box", help);
}
/// The chrome rebuilds keep a line-level style on its way to the screen:
/// `pad_chrome` patches it into the rebuilt spans and `center_line` carries it
/// on the centered line — without the carry, every `Line::styled` chrome line
/// (the hairlines, the footer hint, the help overlay) silently rendered at the
/// default foreground. The help overlay itself reads in the body tier.
#[test]
fn chrome_rebuilds_carry_line_level_styles() {
    let theme = Theme::fixed(false);
    let padded = pad_chrome(Line::styled("keys & legend", theme.body()));
    assert_eq!(padded.spans[0].content.as_ref(), " ", "gutter first");
    assert_eq!(padded.spans[1].style, theme.body());

    let centered = center_line(Line::styled("? for help", theme.faint()), 30);
    assert_eq!(centered.style, theme.faint());

    for line in help_lines(&theme, Some("Alt+p"), 80) {
        assert_eq!(line.style, theme.body());
    }
}
#[test]
fn help_overlay_falls_back_borderless_when_too_narrow() {
    let theme = Theme::fixed(false);
    let lines = help_lines(&theme, Some("Alt+p"), 20);
    let text_lines = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    let text = text_lines.join("\n");

    assert!(!text.contains("╭"), "narrow help drops the frame:\n{text}");
    assert!(
        text.contains("↕ j/k rows"),
        "narrow help keeps glyph-prefixed rows:\n{text}"
    );
    assert!(
        lines.iter().all(|line| line.width() == 20),
        "narrow help lines occupy the pane width so compose keeps them left-aligned:\n{text}"
    );
    assert!(
        text_lines.iter().all(|line| line.starts_with(' ')),
        "narrow help rows share one left gutter:\n{text}"
    );
    assert!(
        text_lines[5].starts_with(" filter"),
        "the filter header stays on the shared left edge:\n{text}"
    );
}
#[test]
fn render_group_cap_shows_overflow_indicator() {
    let agents = (0..9)
        .map(|i| {
            let mut agent = agent(
                &format!("codex-{i}"),
                "codex",
                AgentStatus::Idle,
                Some("/repo/main"),
                Some("main"),
                Some(&format!("task-{i}")),
            );
            agent.last_activity = fixed_now() - Duration::from_secs(i);
            agent
        })
        .collect::<Vec<_>>();
    let snapshot = snapshot_with(Vec::new(), agents);

    // Tall enough that the six capped idle rows plus the `+3 more` overflow
    // all fit, so the indicator the test is named for actually renders.
    let rendered = snapshot_to_screen(&snapshot, 36, 38);
    assert!(rendered.contains("+3 more"), "{rendered}");
    assert_snapshot("group_cap_with_overflow", rendered);
}
/// L0 density (~24 columns): line 1 still names the row by status glyph
/// and clipped name, and label-less meter chrome from line 2 is dropped
/// when capability data is absent.
#[test]
fn render_l0_density_keeps_identity_when_narrow() {
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("compile"),
    );
    codex.last_activity = fixed_now() - Duration::from_secs(3);
    let snapshot = snapshot_with(Vec::new(), vec![codex]);
    // Tall enough that the card clears the bottom-pinned footer after the
    // cockpit's pinned separator (the agent row is what we measure).
    let rendered = snapshot_to_screen(&snapshot, 24, 15);

    assert!(
        // phase 0 of the working spinner is the first frame `⣾`.
        rendered.contains("⣾ codex"),
        "L0 keeps status glyph + name:\n{rendered}"
    );
    assert!(
        rendered.contains("main"),
        "L0 keeps the worktree label:\n{rendered}"
    );
    assert!(
        !rendered.contains(" · "),
        "L0 drops the capability tokens entirely:\n{rendered}"
    );
    assert_snapshot("l0_density_minimal_row", rendered);
}
/// The load-bearing no-flicker guarantee: selecting a row only *appends*
/// lines beneath the card — the resting fold lines (identity, description,
/// ctx bar, token line) keep their exact content, differing only by the
/// selection gutter.
#[test]
fn selecting_a_row_only_appends_never_reshapes_the_fold_lines() {
    let unselected = card_lines(usize::MAX);
    let selected = card_lines(0);

    // Selecting the worktree adds the lane gutter and the dotted seal to its
    // header — chrome, not a card line — but never touches the label itself.
    assert!(unselected[0].contains("main"), "{:?}", unselected[0]);
    assert!(selected[0].contains("main"), "{:?}", selected[0]);
    assert!(
        !unselected[0].contains('┄'),
        "an unselected worktree header is unsealed: {:?}",
        unselected[0]
    );
    assert!(
        selected[0].contains('┄'),
        "the selected worktree header is sealed: {:?}",
        selected[0]
    );
    // Row lines differ only by the frame cells; strip the left gutter and right
    // rail before comparing the content.
    let strip = |line: &String| {
        let mut text = line.chars().skip(1).collect::<String>();
        text.pop();
        text
    };
    let fold: Vec<String> = unselected[1..].iter().map(strip).collect();
    let full: Vec<String> = selected[1..].iter().map(strip).collect();
    // The resting fold is identity + description + ctx bar + the context line.
    assert_eq!(
        fold.len(),
        4,
        "the fold is four card lines (incl. the context line): {fold:?}"
    );
    // Counting the card lines directly (excluding the group header) agrees: four
    // at rest and four selected, so selection never grows the resting height for
    // a subagent-less card.
    assert_eq!(unselected.len() - 1, 4, "{unselected:?}");
    assert_eq!(selected.len() - 1, 4, "{selected:?}");
    // The context line rides the resting fold, not a reveal-on-select detail.
    assert!(
        fold.iter().any(|line| line.contains("▤ ")),
        "the context line is part of the resting fold: {fold:?}"
    );
    // This card has no subagents, so selection appends nothing — it only
    // lights the gutter (already stripped), never reshaping a fold line.
    assert_eq!(
        fold, full,
        "selection only appends; it never reshapes the fold lines"
    );
}
/// The expanded card lists the agent's subagents (status glyph + type),
/// nested under the parent and shown only when the row is selected — the
/// resting card never reveals them, preserving the no-reflow invariant.
#[test]
fn expanded_card_lists_subagents_only_when_selected() {
    let parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    // A paneless child of the parent, still running — it nests onto the
    // parent's card during snapshot projection.
    let mut kid = agent(
        "kid-1",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("Explore"),
    );
    kid.parent_agent_id = Some("claude-1".into());
    let snapshot = snapshot_with(Vec::new(), vec![parent, kid]);
    let theme = Theme::fixed(true);
    let render = |selected_index: usize| {
        let mut row_index = 0;
        let mut lines = Vec::new();
        let mut map = Vec::new();
        worktree_group_lines(
            &theme,
            &snapshot.worktree_groups[0],
            &snapshot.providers,
            snapshot.now,
            54,
            &snapshot.theme.display.context_meter,
            snapshot.theme.display.card_density,
            None,
            &mut row_index,
            selected_index,
            0,
            &CostRolls::default(),
            lead_unread(&snapshot.worktree_groups).map(|(id, _)| id),
            &mut lines,
            &mut map,
        );
        lines
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let selected = render(0);
    assert!(
        selected.contains("subagents"),
        "expanded card lists subagents:\n{selected}"
    );
    assert!(
        selected.contains("Explore"),
        "the subagent type is shown:\n{selected}"
    );

    let resting = render(usize::MAX);
    assert!(
        !resting.contains("subagents"),
        "the resting card hides the subagent list:\n{resting}"
    );
}
#[test]
fn bottom_chrome_dashboard_starts_with_a_blank_separator() {
    let mut snapshot = snapshot_with(Vec::new(), Vec::new());
    snapshot.providers = vec![provider_panel(
        "claude",
        "Claude",
        173,
        true,
        true,
        Some((25, 40)),
    )];

    let (lines, hits) = bottom_chrome_texts(&snapshot, None);

    assert_eq!(lines.first().map(String::as_str), Some(""));
    assert!(
        is_hairline(&lines[1]),
        "the panel's own hairline follows the fixed separator:\n{}",
        lines.join("\n")
    );
    assert!(
        hits.is_empty(),
        "a single-provider dashboard has no tab hits"
    );
}
#[test]
fn bottom_chrome_ledger_only_opens_with_a_hairline() {
    let mut snapshot = snapshot_with(Vec::new(), Vec::new());
    snapshot.value_tally = Some(bottom_tally());

    let (lines, hits) = bottom_chrome_texts(&snapshot, None);

    assert!(
        is_hairline(&lines[0]),
        "a ledger without dashboard carries its own rule:\n{}",
        lines.join("\n")
    );
    assert!(lines[1].contains("W:"), "week row:\n{}", lines.join("\n"));
    assert!(lines[2].contains("M:"), "month row:\n{}", lines.join("\n"));
    assert_eq!(lines[3], "", "footer breathes below the ledger");
    assert!(
        lines[4].contains("? for help"),
        "footer follows the blank:\n{}",
        lines.join("\n")
    );
    assert!(hits.is_empty());
}
#[test]
fn bottom_chrome_empty_room_keeps_zero_ledger() {
    let snapshot = snapshot_with(Vec::new(), Vec::new());

    let (lines, hits) = bottom_chrome_texts(&snapshot, None);

    assert!(
        is_hairline(&lines[0]),
        "the zero ledger opens with a separator:\n{}",
        lines.join("\n")
    );
    assert!(
        lines[1].contains("W:") && lines[1].contains("$0.00"),
        "week row:\n{}",
        lines.join("\n")
    );
    assert!(
        lines[2].contains("M:") && lines[2].contains("$0.00"),
        "month row:\n{}",
        lines.join("\n")
    );
    assert_eq!(lines[3], "", "footer breathes below the zero ledger");
    assert!(
        lines[4].contains("? for help"),
        "footer follows the blank:\n{}",
        lines.join("\n")
    );
    assert!(hits.is_empty());
}
/// A just-started idle agent — idle, on the `Some(0)` baseline gauge with no
/// usage behind it — sheds the 0% context bar and the zeroed stats, resting at
/// identity + description alone with nothing to append on selection. The same
/// 0% reading while *running* still paints the bar, so the suppression is gated
/// on idle, not merely on a zero percent.
#[test]
fn just_started_idle_agent_sheds_the_gauge_and_zeroed_stats() {
    let theme = Theme::fixed(true);
    let mk = |status| {
        let state = agent(
            "claude-1",
            "claude",
            status,
            Some("/repo/main"),
            Some("main"),
            Some("warm up"),
        );
        snapshot_with(Vec::new(), vec![state])
    };

    let idle = mk(AgentStatus::Idle);
    let resting = line_texts(&group_lines(&idle, &theme, usize::MAX));
    let expanded = line_texts(&group_lines(&idle, &theme, 0));

    assert!(
        resting
            .iter()
            .all(|line| !line.contains('▣') && !line.contains('▢')),
        "fresh idle card hides the context bar:\n{}",
        resting.join("\n")
    );
    // Header + identity + description — no gauge or stats at rest.
    assert_eq!(resting.len(), 3, "{resting:?}");
    let joined = expanded.join("\n");
    assert!(
        !joined.contains('▣') && !joined.contains('▤'),
        "expanded fresh idle card hides the bar and the zeroed stats:\n{joined}"
    );
    // A fresh idle card has nothing to append on selection — no stats, no age,
    // no subagents — so expanding it adds no line.
    assert_eq!(expanded.len(), 3, "{expanded:?}");

    let running = line_texts(&group_lines(&mk(AgentStatus::Running), &theme, usize::MAX));
    assert!(
        running.iter().any(|line| line.contains('▢')),
        "a running 0% agent keeps its bar (the hollow ▢ at 0%):\n{}",
        running.join("\n")
    );
}

#[test]
fn compacted_idle_agent_keeps_the_gauge_and_context_line() {
    let theme = Theme::fixed(true);
    let mut state = agent(
        "claude-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        Some("warm up"),
    );
    state.context_pct = Some(100);
    state.total_tokens = Some(283_900);
    state.compaction_count = 1;
    let mut context = claude_context(fixed_now());
    context.tokens = Some(AgentTokenUsage {
        context_window_size: Some(1_000_000),
        used_percentage: Some(0),
        remaining_percentage: Some(100),
        current_usage: None,
    });
    state.context = Some(context);

    let snapshot = snapshot_with(Vec::new(), vec![state]);
    let rendered = line_texts(&group_lines(&snapshot, &theme, usize::MAX));

    assert_eq!(rendered.len(), 5, "{rendered:?}");
    let gauge = rendered
        .iter()
        .find(|line| line.contains('▢'))
        .expect("the compacted idle agent keeps its hollow 0% gauge");
    assert!(gauge.contains("0%"), "{gauge}");
    assert!(!gauge.contains("100%"), "{gauge}");
    let context = rendered
        .iter()
        .find(|line| line.contains('▤'))
        .expect("the compacted idle agent keeps its context line");
    assert!(context.contains("▤ 283k"), "{context}");
    assert!(context.contains("↻ 1"), "{context}");
    for marker in ['◌', '◍', '↘', '↗'] {
        assert!(
            !context.contains(marker),
            "post-compact context line falls back to the bare total:\n{context}"
        );
    }
}

#[test]
fn idle_agent_with_cost_history_keeps_the_gauge() {
    let theme = Theme::fixed(true);
    let mut state = agent(
        "claude-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        Some("warm up"),
    );
    let mut context = claude_context(fixed_now());
    context.tokens = Some(AgentTokenUsage {
        context_window_size: Some(1_000_000),
        used_percentage: Some(0),
        remaining_percentage: Some(100),
        current_usage: None,
    });
    state.context = Some(context);

    let snapshot = snapshot_with(Vec::new(), vec![state]);
    let rendered = line_texts(&group_lines(&snapshot, &theme, usize::MAX));

    assert_eq!(rendered.len(), 4, "{rendered:?}");
    let gauge = rendered
        .iter()
        .find(|line| line.contains('▢'))
        .expect("cost history keeps the 0% gauge visible");
    assert!(gauge.contains("0%"), "{gauge}");
    assert!(
        rendered.iter().all(|line| !line.contains('▤')),
        "cost-only history has no token context line:\n{}",
        rendered.join("\n")
    );
}
/// Consecutive cards in a group stack without a blank separator. Different
/// worktrees still get their group-level gap in the scroll composer.
#[test]
fn consecutive_cards_stack_without_a_blank_separator() {
    let theme = Theme::fixed(true);
    let one = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("task one"),
    );
    let two = agent(
        "claude-2",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("task two"),
    );
    let snapshot = snapshot_with(Vec::new(), vec![one, two]);
    let rendered = line_texts(&group_lines(&snapshot, &theme, usize::MAX));

    let names: Vec<usize> = rendered
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains("claude"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        names.len(),
        2,
        "two cards in the group:\n{}",
        rendered.join("\n")
    );
    let blanks: Vec<usize> = rendered
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim().is_empty())
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        blanks,
        Vec::<usize>::new(),
        "no blank line sits between cards in the same worktree:\n{}",
        rendered.join("\n")
    );
    assert!(
        !rendered[names[1] - 1].trim().is_empty(),
        "the line before the second card is still card content:\n{}",
        rendered.join("\n")
    );
}
/// The agent handle uses the provider brand color in the card's normal tier. At
/// truecolor a calm unselected card softens it a lightness step; the 256-color
/// cube is too coarse for that subtle step, so an indexed render (this test)
/// keeps the full brand and leaves the calm cue to the selection bar and
/// description. Read the expected index off the snapshot's own panel so the test
/// follows config overrides.
#[test]
fn agent_handle_follows_card_emphasis() {
    let theme = Theme::fixed(false); // indexed depth, color on, so the brand tone survives
    let mut state = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    state.role = Some("planner".to_owned());
    let mut snapshot = snapshot_with(Vec::new(), vec![state]);
    // Provider panels are producer-only (`with_provider_aggregates`), so the
    // reducer-built snapshot carries none — set one as the producer would.
    snapshot.providers = vec![provider_panel(
        "claude",
        "Claude Code",
        173,
        true,
        true,
        None,
    )];
    let expected = snapshot.providers[0].color;

    let name_style = |selected_index| {
        group_lines(&snapshot, &theme, selected_index)
            .into_iter()
            .flat_map(|line| line.spans)
            .find(|span| span.content == "planner")
            .expect("the agent handle span")
            .style
    };
    let selected = name_style(0);
    assert_eq!(
        selected.fg,
        Some(Color::Indexed(expected)),
        "the selected agent handle wears the full provider color"
    );
    let calm = name_style(usize::MAX);
    assert_eq!(
        calm,
        theme.body_brand(Color::Indexed(expected)),
        "a calm unselected agent handle takes the calm brand style"
    );
    assert_eq!(
        calm.fg, selected.fg,
        "at indexed depth the calm handle keeps the same full-brand tone — the cube \
         can't render a subtle dim, so the selection band, bar, and description \
         carry the selected/calm distinction"
    );
    assert_ne!(
        calm.fg,
        theme.body().fg,
        "the calm handle keeps its brand hue, not the flat soft gray"
    );
}
