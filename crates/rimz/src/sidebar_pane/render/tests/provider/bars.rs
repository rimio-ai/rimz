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
/// Each `5h`/`7d` label mirrors its own bar's severity color, so a green and a
/// yellow window read as two differently-toned rows, not one dim slab.
#[test]
fn provider_label_mirrors_its_bar_color() {
    let theme = Theme::fixed(false);
    // 5h: 25% used → 75% left → green. 7d: 70% used → 30% left → yellow.
    let panel = provider_panel("claude", "Claude", 173, true, false, Some((25, 70)));
    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 2, "a metered panel draws a 5h and a 7d row");
    let (five_label, five_glyph, _) = bar_row_facts(&rows[0]);
    let (seven_label, seven_glyph, _) = bar_row_facts(&rows[1]);
    assert_eq!(five_label, five_glyph, "5h label mirrors its bar");
    assert_eq!(seven_label, seven_glyph, "7d label mirrors its bar");
    assert_ne!(
        five_label, seven_label,
        "a green 5h and a yellow 7d label differ in tone"
    );
}
/// The reset countdown speaks burn pace, not remaining budget: half a 5-hour
/// budget spent in the first hour is red-hot, while the remaining-budget bar
/// and label are still green at the default zones.
#[test]
fn reset_countdown_reddens_when_pace_outruns_green_bar() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
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
        reset_span_fg(&rows[0]),
        theme.style(Color::Red, Modifier::empty()).fg,
        "2.5x pace colors only the reset countdown"
    );
}

/// Under NO_COLOR the reset countdown keeps the old soft weight: color is the
/// pace signal, so when color is unavailable it should not become louder than
/// the previous soft countdown.
#[test]
fn paced_reset_countdown_under_no_color_stays_soft() {
    let theme = Theme::fixed(true);
    let now = fixed_now();
    let mut panel = provider_panel("claude", "Claude", 173, true, false, None);
    panel.windows = vec![RateLimitWindow {
        used_percentage: Some(50),
        resets_at: Some(now + Duration::from_secs(4 * 3_600)),
        duration_mins: Some(5 * 60),
    }];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 1);
    assert_eq!(reset_span_style(&rows[0]), Some(theme.soft()));
}

/// Each window gets its own pace tone: a fresh short window can rest blue while
/// the weekly budget's reset reads red because its burn rate cannot last.
#[test]
fn reset_countdowns_tone_each_window_independently() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
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
        reset_span_fg(&rows[0]),
        theme.style(Color::Blue, Modifier::empty()).fg,
        "unused started 5h window rests blue"
    );
    assert_eq!(
        reset_span_fg(&rows[1]),
        theme.style(Color::Red, Modifier::empty()).fg,
        "half the 7d budget after one day burns at 3.5x pace"
    );
}
/// A countdown with no duration has no pace denominator, so it keeps the old
/// soft tone instead of claiming a sustainable or hot burn rate.
#[test]
fn reset_countdown_with_unknown_duration_stays_soft() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
    let mut panel = provider_panel("claude", "Claude", 173, true, false, None);
    panel.windows = vec![RateLimitWindow {
        used_percentage: Some(50),
        resets_at: Some(now + Duration::from_secs(4 * 3_600)),
        duration_mins: None,
    }];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 1);
    assert_eq!(reset_span_fg(&rows[0]), theme.soft().fg);
}
/// A spent window still owns its reset countdown unless a longer spent window
/// gates it; the red empty track says exhausted, while the reset countdown
/// reports how fast it got there.
#[test]
fn spent_window_keeps_its_pace_toned_countdown() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
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
        reset_span_fg(&rows[0]),
        theme.style(theme::ORANGE, Modifier::empty()).fg,
        "spent exactly halfway through the window reads as 2x amber pace"
    );
}
/// A spent weekly cap gates the short window: with 7d exhausted the 5h row is
/// painted exhausted — red, a full empty track, and no reset countdown —
/// regardless of the 5h window's own (here untouched) reading.
#[test]
fn seven_day_exhaustion_reddens_and_silences_the_five_hour_row() {
    let theme = Theme::fixed(false);
    // 5h is untouched (would be green with a countdown); 7d is fully spent.
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
}
/// A provider that reports a single window draws exactly one bar, labeled by
/// the window's own length — the model isn't pinned to a fixed set. (A
/// transient Codex server bug once widened its window to ~30 days; this is what
/// rendered, instead of mislabeling it `7d`.)
#[test]
fn single_window_panel_draws_one_bar_labeled_by_length() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
    let mut codex = provider_panel("codex", "Codex", 33, true, false, None);
    codex.windows = vec![RateLimitWindow {
        used_percentage: Some(7),
        resets_at: Some(now + Duration::from_secs(28 * 86_400 + 4 * 3_600)),
        duration_mins: Some(43_800),
    }];
    let rows = metered_bar_rows(&theme, &codex);
    assert_eq!(rows.len(), 1, "one window → one bar");
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
}
/// A not-started window drops its countdown — these budgets begin counting
/// only on the first token, so until then the provider keeps `resets_at` slid a
/// full window-length ahead. It's detected by the reset distance, not a 0%
/// reading: the real Claude shape is `usedPercent: 1` with the reset still ~a
/// full 5h out (`4h59m`). Its bar shows full with no countdown.
#[test]
fn not_started_window_drops_its_countdown() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
    let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
    // The real not-started shape: ~1% used, reset slid a full 5h ahead (a hair
    // under, here 4h59m30s, the way a live reading reads).
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
}
/// Codex reports `usedPercent: 99` with no `resetsAt` before the first token —
/// the bar should be full (not 1% remaining) and the countdown absent.
#[test]
fn codex_not_started_shows_full_bar() {
    let theme = Theme::fixed(false);
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
}
/// An expired long-window cache is an unknown budget reading, not a full budget
/// and not a spent one. It keeps the duration label but paints a plain dim empty
/// track with no countdown.
#[test]
fn unknown_provider_window_draws_dim_empty_track() {
    let theme = Theme::fixed(false);
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
    // The today line reads the coarse integer form (`◇ 486k`), with the split.
    assert!(stats.contains("486k"), "today's JSONL tokens: {stats:?}");
    assert!(stats.contains("↗ 64k"), "the output split: {stats:?}");
}
/// A started window — its reset has ticked well below the full window — keeps
/// its countdown, even at the same low 1% usage as a not-started one. Usage
/// alone can't tell them apart; the reset distance does.
#[test]
fn started_window_keeps_its_countdown() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
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
}
/// Usage above the ~1% not-started floor means the window has started — keep its
/// countdown even when the reset still reads a near-full window. The reset-distance
/// grace only applies to a window at or below the floor (0–1% used); any real
/// usage short-circuits to "started".
#[test]
fn used_window_keeps_countdown_despite_near_full_reset() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
    let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
    // 5% used with the reset slid a full 5h out: usage above the floor wins, so
    // this counts as started despite the near-full reset.
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
