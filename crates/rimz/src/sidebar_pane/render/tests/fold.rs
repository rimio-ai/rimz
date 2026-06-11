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
    assert!(help.contains("j/k rows"));
    assert!(help.contains("J/K worktrees"));
    assert!(help.contains("l or"));
    assert!(help.contains("q waiting"));
    assert!(help.contains("!/e attention"));
    assert!(help.contains("? waiting"));
    assert!(help.contains("○ idle"));
    assert!(
        !help.contains("┄ commands"),
        "the retired seam left the legend"
    );
    assert!(!help.contains("posture"), "the posture legend is gone");
}
/// The chrome rebuilds keep a line-level style on its way to the screen:
/// `pad_chrome` patches it into the rebuilt spans and `center_line` carries it
/// on the centered line — without the carry, every `Line::styled` chrome line
/// (the hairlines, the footer hint, the help overlay) silently rendered at the
/// default foreground. The help overlay itself reads in the faint tier.
#[test]
fn chrome_rebuilds_carry_line_level_styles() {
    let theme = Theme::fixed(false);
    let padded = pad_chrome(Line::styled("keys & legend", theme.faint()));
    assert_eq!(padded.spans[0].content.as_ref(), " ", "gutter first");
    assert_eq!(padded.spans[1].style, theme.faint());

    let centered = center_line(Line::styled("? for help", theme.faint()), 30);
    assert_eq!(centered.style, theme.faint());

    for line in help_lines(&theme) {
        assert_eq!(line.style, theme.faint());
    }
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
    let rendered = snapshot_to_screen(&snapshot, 24, 12);

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
    // Row lines differ only by the leading one-cell gutter; strip it.
    let strip = |line: &String| line.chars().skip(1).collect::<String>();
    let fold: Vec<String> = unselected[1..].iter().map(strip).collect();
    let full: Vec<String> = selected[1..].iter().map(strip).collect();
    // The resting fold is identity + description + ctx bar + the context line.
    assert_eq!(
        fold.len(),
        4,
        "the fold is four card lines (incl. the context line): {fold:?}"
    );
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
            &snapshot.sidebar.context,
            snapshot.sidebar.card_density,
            None,
            &mut row_index,
            selected_index,
            0,
            &CostRolls::default(),
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
/// The resting card is four lines (identity, description, ctx bar, token
/// line); selecting it appends only the subagent list, so the deepest data is
/// one keystroke away without ever reshaping a resting line.
#[test]
fn resting_card_is_four_lines_and_selection_only_appends() {
    // Card lines, excluding the group header.
    let resting = card_lines(usize::MAX).len() - 1;
    let selected = card_lines(0).len() - 1;
    assert_eq!(resting, 4, "identity, description, ctx, token line");
    // This single-agent fixture has no subagents, so selection appends
    // nothing — the resting height already carries every per-row stat.
    assert_eq!(selected, 4);
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
fn bottom_chrome_empty_room_is_footer_only() {
    let snapshot = snapshot_with(Vec::new(), Vec::new());

    let (lines, hits) = bottom_chrome_texts(&snapshot, None);

    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("? for help"), "{}", lines.join("\n"));
    assert!(
        !lines[0].trim().is_empty() && !is_hairline(&lines[0]),
        "the empty-room footer does not float under a separator"
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
/// The agent name wears its provider's brand color (Claude's clay), tying the
/// card to the provider dashboard. Read the expected index off the snapshot's
/// own panel so the test follows config overrides.
#[test]
fn agent_name_wears_its_provider_brand_color() {
    let theme = Theme::fixed(false); // color on, so the brand tone survives
    let state = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
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

    let lines = group_lines(&snapshot, &theme, usize::MAX);
    let name = lines
        .iter()
        .flat_map(|line| &line.spans)
        .find(|span| span.content == "claude")
        .expect("the agent name span");
    assert_eq!(
        name.style.fg,
        Some(Color::Indexed(expected)),
        "the agent name wears the provider color"
    );
}
