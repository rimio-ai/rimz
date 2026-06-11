use super::*;

/// Every provider bar — `5h`, `7d` across blocks, and the unmetered `∞` —
/// shares one front (bar-start) column and one end (bar-end) column, so the
/// whole dashboard reads as one aligned grid. The structural payoff of the
/// shared bar grammar, now that the budgets live in the panel.
#[test]
fn provider_bars_share_one_front_and_end_column() {
    let theme = Theme::fixed(true);
    let panels = vec![
        provider_panel("claude", "Claude", 173, true, true, Some((25, 40))),
        provider_panel("codex", "Codex", 33, true, false, Some((55, 8))),
        provider_panel("pi", "Pi", 28, false, false, None),
    ];
    // Rendered narrow so the art column is dropped and the bar lines carry no
    // stray block glyphs from the emblem — the bar grid is what we measure.
    // The tabbed dashboard paints one panel at a time, so each panel renders
    // as its own active tab and the grid is asserted across those frames.
    let lines: Vec<String> = panels
        .iter()
        .flat_map(|panel| {
            provider_panel_lines(
                &theme,
                &panels,
                Some(panel.kind.as_str()),
                true,
                30,
                &crate::config::BudgetZonesConfig::default(),
                fixed_now(),
            )
            .0
        })
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .filter(|line| line.contains('▰') || line.contains('▱') || line.contains('▒'))
        .collect();
    assert!(lines.len() >= 5, "two metered providers + one ∞: {lines:?}");
    // Bar start: the first bar cell (tick or shade), by char column.
    let start = |line: &str| {
        line.chars()
            .position(|c| matches!(c, '▰' | '▱' | '▒'))
            .unwrap()
    };
    let starts: Vec<usize> = lines.iter().map(|line| start(line)).collect();
    assert!(
        starts.iter().all(|&s| s == starts[0]),
        "provider bars share a front column: {starts:?}"
    );
    // Bar end: the last bar cell column.
    let end = |line: &str| {
        line.char_indices()
            .filter(|(_, c)| matches!(c, '▰' | '▱' | '▒'))
            .count()
            + start(line)
    };
    let ends: Vec<usize> = lines.iter().map(|line| end(line)).collect();
    assert!(
        ends.iter().all(|&e| e == ends[0]),
        "provider bars share an end column: {ends:?}"
    );
}
#[test]
fn provider_bar_tones_labels_and_reset_countdowns() {
    let theme = Theme::fixed(false);
    let now = fixed_now();

    let panel = provider_panel("claude", "Claude", 173, true, false, Some((25, 70)));
    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 2, "a metered panel draws a 5h and a 7d row");
    let (five_label, five_glyph, _) = bar_row_facts(&rows[0]);
    let (seven_label, seven_glyph, _) = bar_row_facts(&rows[1]);
    assert_eq!(five_label, five_glyph, "5h label mirrors its bar");
    assert_eq!(seven_label, seven_glyph, "7d label mirrors its bar");
    assert_ne!(five_label, seven_label);

    let mut panel = provider_panel("claude", "Claude", 173, true, false, None);
    panel.windows = vec![RateLimitWindow {
        used_percentage: Some(50),
        resets_at: Some(now + Duration::from_secs(4 * 3_600)),
        duration_mins: Some(5 * 60),
    }];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 1);
    let (label_fg, glyph_fg, has_reset) = bar_row_facts(&rows[0]);
    assert!(has_reset, "started windows keep their reset countdown");
    assert_eq!(
        label_fg,
        theme.style(Color::Green, Modifier::empty()).fg,
        "50% remaining stays in the green bar zone"
    );
    assert_eq!(glyph_fg, label_fg, "the label still mirrors the bar");
    assert_eq!(
        reset_marker_fg(&rows[0]),
        theme.style(Color::Red, Modifier::empty()).fg,
        "2.5x pace colors only the reset marker"
    );
    assert_eq!(
        reset_time_style(&rows[0]),
        Some(theme.soft()),
        "the reset time stays neutral"
    );

    let mut panel = provider_panel("claude", "Claude", 173, true, false, None);
    panel.windows = vec![RateLimitWindow {
        used_percentage: Some(50),
        resets_at: Some(now + Duration::from_secs(4 * 3_600)),
        duration_mins: Some(5 * 60),
    }];

    let plain = Theme::fixed(true);
    let rows = metered_bar_rows(&plain, &panel);
    assert_eq!(rows.len(), 1);
    assert_eq!(reset_marker_style(&rows[0]), Some(plain.soft()));
    assert_eq!(reset_time_style(&rows[0]), Some(plain.soft()));

    let mut panel = provider_panel("claude", "Claude", 173, true, false, None);
    panel.windows = vec![
        RateLimitWindow {
            used_percentage: Some(0),
            resets_at: Some(now + Duration::from_secs(4 * 3_600)),
            duration_mins: Some(5 * 60),
        },
        RateLimitWindow {
            used_percentage: Some(50),
            resets_at: Some(now + Duration::from_secs(6 * 86_400)),
            duration_mins: Some(7 * 24 * 60),
        },
    ];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        reset_marker_style(&rows[0]),
        Some(theme.soft()),
        "unused started 5h window keeps the marker soft like its countdown"
    );
    assert_eq!(
        reset_marker_fg(&rows[1]),
        theme.style(Color::Red, Modifier::empty()).fg,
        "half the 7d budget after one day burns at 3.5x pace"
    );

    let mut panel = provider_panel("claude", "Claude", 173, true, false, None);
    panel.windows = vec![RateLimitWindow {
        used_percentage: Some(50),
        resets_at: Some(now + Duration::from_secs(4 * 3_600)),
        duration_mins: None,
    }];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 1);
    assert_eq!(reset_marker_fg(&rows[0]), theme.soft().fg);
    assert_eq!(reset_time_style(&rows[0]), Some(theme.soft()));

    let mut panel = provider_panel("claude", "Claude", 173, true, false, None);
    panel.windows = vec![RateLimitWindow {
        used_percentage: Some(100),
        resets_at: Some(now + Duration::from_secs(150 * 60)),
        duration_mins: Some(5 * 60),
    }];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 1);
    let (label_fg, _, has_reset) = bar_row_facts(&rows[0]);
    assert!(has_reset, "the spent window keeps its own countdown");
    assert_eq!(label_fg, theme.style(Color::Red, Modifier::empty()).fg);
    assert_eq!(
        reset_marker_fg(&rows[0]),
        theme.style(theme.clay(), Modifier::empty()).fg,
        "spent exactly halfway through the window reads as 2x amber pace"
    );
    assert_eq!(reset_time_style(&rows[0]), Some(theme.soft()));
}

#[test]
fn provider_window_states_control_countdowns_and_empty_tracks() {
    let theme = Theme::fixed(false);
    let now = fixed_now();

    let panel = provider_panel("claude", "Claude", 173, true, false, Some((0, 100)));
    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 2);
    let (five_label, _, five_has_reset) = bar_row_facts(&rows[0]);
    let (seven_label, _, _) = bar_row_facts(&rows[1]);
    assert!(!five_has_reset, "the cascaded 5h row drops its countdown");
    assert!(
        !rows[0].spans.iter().any(|span| span.content.contains('▰')),
        "the cascaded 5h bar is a full empty track, no fill"
    );
    assert_eq!(
        five_label, seven_label,
        "the cascaded 5h label reddens to match the exhausted 7d"
    );

    let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
    claude.windows = vec![RateLimitWindow {
        used_percentage: Some(1),
        resets_at: Some(now + Duration::from_secs(5 * 3_600 - 30)),
        duration_mins: Some(5 * 60),
    }];
    let rows = metered_bar_rows(&theme, &claude);
    assert_eq!(rows.len(), 1);
    let (_, _, has_reset) = bar_row_facts(&rows[0]);
    assert!(
        !has_reset,
        "a not-started window (reset ~ full 5h) shows no countdown"
    );
    assert!(
        rows[0].spans.iter().any(|span| span.content.contains('▰')),
        "the not-started window shows a full bar, not an empty/exhausted track"
    );

    let mut codex = provider_panel("codex", "Codex", 33, true, false, None);
    codex.windows = vec![RateLimitWindow {
        used_percentage: Some(99),
        resets_at: None,
        duration_mins: Some(5 * 60),
    }];
    let rows = metered_bar_rows(&theme, &codex);
    assert_eq!(rows.len(), 1);
    let (_, _, has_reset) = bar_row_facts(&rows[0]);
    assert!(!has_reset, "Codex not-started: no reset countdown");
    assert!(
        !rows[0].spans.iter().any(|span| span.content.contains('▱')),
        "Codex not-started: bar is full, no empty track cells"
    );

    let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
    claude.windows = vec![RateLimitWindow {
        used_percentage: None,
        resets_at: None,
        duration_mins: Some(7 * 24 * 60),
    }];
    let rows = metered_bar_rows(&theme, &claude);
    assert_eq!(rows.len(), 1);
    let label = rows[0]
        .spans
        .first()
        .expect("a label span")
        .content
        .trim()
        .to_owned();
    assert_eq!(label, "7d");
    let (label_fg, glyph_fg, has_reset) = bar_row_facts(&rows[0]);
    assert_eq!(label_fg, glyph_fg, "unknown label mirrors its dim track");
    assert_ne!(glyph_fg, Some(Color::Red), "unknown is not exhausted");
    assert!(!has_reset, "unknown windows have no reset countdown");
    assert!(
        !rows[0].spans.iter().any(|span| span.content.contains('▰')),
        "unknown windows have no filled budget cells"
    );
    assert!(
        rows[0].spans.iter().any(|span| span.content.contains('▱')),
        "unknown windows keep an empty track"
    );

    let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
    claude.windows = vec![RateLimitWindow {
        used_percentage: Some(1),
        resets_at: Some(now + Duration::from_secs(4 * 3_600)),
        duration_mins: Some(5 * 60),
    }];
    let rows = metered_bar_rows(&theme, &claude);
    assert_eq!(rows.len(), 1);
    let (_, _, has_reset) = bar_row_facts(&rows[0]);
    assert!(
        has_reset,
        "a started window (reset well below full) shows its countdown"
    );

    let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
    claude.windows = vec![RateLimitWindow {
        used_percentage: Some(5),
        resets_at: Some(now + Duration::from_secs(5 * 3_600 - 30)),
        duration_mins: Some(5 * 60),
    }];
    let rows = metered_bar_rows(&theme, &claude);
    assert_eq!(rows.len(), 1);
    let (_, _, has_reset) = bar_row_facts(&rows[0]);
    assert!(
        has_reset,
        "usage above ~1% shows the countdown even with a near-full reset"
    );
}

#[test]
fn provider_window_layout_handles_single_windows_and_wide_hour_countdowns() {
    let theme = Theme::fixed(false);
    let now = fixed_now();

    let mut codex = provider_panel("codex", "Codex", 33, true, false, None);
    codex.windows = vec![RateLimitWindow {
        used_percentage: Some(7),
        resets_at: Some(now + Duration::from_secs(28 * 86_400 + 4 * 3_600)),
        duration_mins: Some(43_800),
    }];
    let rows = metered_bar_rows(&theme, &codex);
    assert_eq!(rows.len(), 1, "one window -> one bar");
    let label = rows[0]
        .spans
        .first()
        .expect("a label span")
        .content
        .trim()
        .to_owned();
    assert_eq!(label, "30d", "the ~30-day window is labeled 30d");
    let (_, _, has_reset) = bar_row_facts(&rows[0]);
    assert!(has_reset, "the bar carries its reset countdown");

    let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
    claude.windows = vec![RateLimitWindow {
        used_percentage: Some(50),
        resets_at: Some(now + Duration::from_secs(20 * 3_600 + 20 * 60)),
        duration_mins: Some(7 * 24 * 60),
    }];

    let rows = metered_bar_rows(&theme, &claude);
    assert_eq!(rows.len(), 1);
    let text = rows[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert_eq!(text.chars().count(), 30, "row keeps the fixed region");
    assert!(
        text.ends_with(' '),
        "the value column keeps a one-cell right gutter: {text:?}"
    );
    assert_eq!(
        text.chars().position(|ch| ch == '↻'),
        Some(21),
        "the reset marker moves one cell left for the wide hour form: {text:?}"
    );
    assert!(text.contains("↻ 20h20m"), "{text:?}");
}

/// The provider stats line reads today's transcript-history spend *and* token
/// burn from the JSONL `spending`, never the live active-session sum — the one
/// figure that also holds for a token-only provider (Codex) with no live cost.
#[test]
fn provider_stats_read_todays_jsonl_spend_and_tokens() {
    let theme = Theme::fixed(false);
    let mut codex = provider_panel("codex", "Codex", 33, false, false, None);
    codex.spending = Some(crate::SpendTally {
        today: crate::SpendWindow {
            usd: 4.20,
            tokens: 486_000,
            input: 422_000,
            output: 64_000,
            cache_write: 0,
            cache_read: 68_000,
            sessions: 5,
        },
        ..Default::default()
    });
    let stats = stats_line(&theme, &codex);
    assert!(stats.contains("$4.20"), "today's JSONL spend: {stats:?}");
    assert!(stats.contains("486k"), "today's JSONL tokens: {stats:?}");
    assert!(stats.contains("↗ 64k"), "the output split: {stats:?}");
}
