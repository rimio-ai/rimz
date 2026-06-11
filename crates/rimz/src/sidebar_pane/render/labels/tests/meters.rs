use super::*;
use jiff::SignedDuration;

fn text(spans: &[Span<'_>]) -> String {
    spans.iter().map(|s| s.content.as_ref()).collect()
}

fn assert_no_fg(spans: &[Span<'_>]) {
    assert!(spans.iter().all(|span| span.style.fg.is_none()));
}

#[test]
fn branch_delta_omits_zero_components() {
    let theme = Theme::fixed(true);
    assert_eq!(text(&branch_delta_spans(&theme, 3, 1)), "⇡3 ⇣1");
    assert_eq!(text(&branch_delta_spans(&theme, 3, 0)), "⇡3");
    assert_eq!(text(&branch_delta_spans(&theme, 0, 5)), "⇣5");
    assert_eq!(text(&branch_delta_spans(&theme, 0, 0)), "");
    assert_eq!(text(&trunk_equal_spans(&theme, "main")), "≡ main");
    assert_eq!(text(&trunk_clear_spans(&theme, "main")), "✓ main");
}

#[test]
fn gauge_bars_map_severity_and_apportion_segments() {
    assert_eq!(severity_color(ContextSeverity::Calm), Color::Blue);
    assert_eq!(severity_color(ContextSeverity::Yellow), Color::Yellow);
    assert_eq!(severity_color(ContextSeverity::Amber), ORANGE);
    assert_eq!(severity_color(ContextSeverity::Red), Color::Red);
    assert_eq!(apportion([3, 1, 1], 5), vec![3, 1, 1]);
    assert_eq!(apportion([1, 1, 1], 4).iter().sum::<usize>(), 4);
    assert_eq!(apportion([0, 0], 3), vec![0, 0]);

    let theme = Theme::fixed(true);
    for (color, percent, width, expected) in [
        (Color::Green, 60, 5, "━━━──"),
        (Color::Green, 38, 10, "━━━━──────"),
        (Color::Green, 0, 5, "─────"),
        (Color::Red, 100, 5, "━━━━━"),
    ] {
        let spans = gauge_spans(&theme, color, percent, width);
        assert_eq!(text(&spans), expected, "{percent}% over width {width}");
        assert_no_fg(&spans);
    }

    let segments = [
        (8_000_u64, Color::Green),
        (5_000, Color::Cyan),
        (2_000, Color::Blue),
    ];
    let spans = segmented_gauge_spans(&theme, &segments, Color::Green, 60, 10);
    // 60% of 10 = 6 filled; segments apportion 6 → 3/2/1; then a 4-cell track.
    assert_eq!(text(&spans), "━━━━━━────");
    let filled = text(&spans).chars().filter(|c| *c == '━').count();
    assert_eq!(filled, 6);
    assert_no_fg(&spans);

    let spans = segmented_gauge_spans(
        &theme,
        &[(0, Color::Green), (0, Color::Cyan)],
        Color::Green,
        50,
        4,
    );
    assert_eq!(text(&spans), "━━──");
}

#[test]
fn mana_bar_drains_ramps_and_keeps_edge_shapes() {
    let zones = BudgetZonesConfig::default();
    let plain = Theme::fixed(true);
    let spans = mana_bar_spans(&plain, 70, 10, &zones);
    assert_eq!(text(&spans), "▰▰▰▰▰▰▰▱▱▱");
    assert_no_fg(&spans);

    for (remaining, expected) in [(0, "▱▱▱▱▱▱▱▱▱▱"), (1, "▰▱▱▱▱▱▱▱▱▱")]
    {
        let spans = mana_bar_spans(&plain, remaining, 10, &zones);
        assert_eq!(text(&spans), expected, "{remaining}% remaining");
        if remaining == 0 {
            assert_eq!(spans.len(), 1, "spent budget is one empty track span");
        }
        assert_no_fg(&spans);
    }

    let lit = Theme::fixed(false);
    let fg = |remaining| {
        mana_bar_spans(&lit, remaining, 10, &zones)[0]
            .style
            .fg
            .unwrap()
    };
    // Green at half or more left, yellow below half, amber below a quarter,
    // red below a tenth — band edges pinned on both sides.
    assert_eq!(fg(80), Color::Indexed(108));
    assert_eq!(fg(50), Color::Indexed(108));
    assert_eq!(fg(49), Color::Indexed(179));
    assert_eq!(fg(40), Color::Indexed(179));
    assert_eq!(fg(25), Color::Indexed(179));
    assert_eq!(fg(24), Color::Indexed(173));
    assert_eq!(fg(10), Color::Indexed(173));
    assert_eq!(fg(9), Color::Indexed(167));
    assert_eq!(fg(1), Color::Indexed(167));
    let track = &mana_bar_spans(&lit, 70, 10, &zones)[1];
    assert_eq!(track.style, lit.dim());
    let spent = mana_bar_spans(&lit, 0, 10, &zones);
    assert_eq!(spent[0].style.fg, Some(Color::Indexed(167)));
    assert_ne!(spent[0].style.fg, lit.dim().fg);
}

#[test]
fn mana_style_honours_custom_and_misordered_zones() {
    let lit = Theme::fixed(false);
    let tuned = BudgetZonesConfig {
        yellow: 80,
        amber: 40,
        red: 20,
        ..BudgetZonesConfig::default()
    };
    let tone = |color| lit.style(color, Modifier::empty());
    assert_eq!(mana_style(&lit, 70, &tuned), tone(Color::Yellow));
    assert_eq!(
        mana_style(&lit, 80, &tuned),
        tone(Color::Green),
        "healthy rests green"
    );
    assert_eq!(mana_style(&lit, 39, &tuned), tone(ORANGE));
    assert_eq!(mana_style(&lit, 19, &tuned), tone(Color::Red));
    let bar = mana_bar_spans(&lit, 70, 10, &tuned);
    assert_eq!(bar[0].style.fg, Some(Color::Indexed(179)));

    let misordered = BudgetZonesConfig {
        yellow: 25,
        amber: 10,
        red: 50,
        ..BudgetZonesConfig::default()
    };
    assert_eq!(mana_style(&lit, 30, &misordered), tone(Color::Red));
    assert_eq!(mana_style(&lit, 50, &misordered), tone(Color::Green));
}

#[test]
fn pace_ratio_reads_burn_against_elapsed_window_and_edges() {
    let secs = SignedDuration::from_secs;
    let ratio = |used, duration, until_reset| {
        pace_ratio(used, secs(duration), secs(until_reset)).expect("pace ratio")
    };
    let assert_close = |actual: f64, expected: f64| {
        assert!(
            (actual - expected).abs() < 0.000_1,
            "expected {expected}, got {actual}"
        );
    };

    assert_close(ratio(50, 5 * 3_600, 4 * 3_600), 2.5);
    assert_close(ratio(50, 7 * 86_400, 6 * 86_400), 3.5);
    assert_close(ratio(20, 5 * 3_600, 4 * 3_600), 1.0);
    assert_close(ratio(0, 5 * 3_600, 4 * 3_600), 0.0);
    assert_close(ratio(10, 5 * 3_600, 5 * 3_600 - 60), 2.0);

    assert_eq!(pace_ratio(50, secs(0), secs(0)), None);
    assert_eq!(pace_ratio(50, secs(5 * 3_600), secs(5 * 3_600)), None);
    assert_eq!(pace_ratio(50, secs(5 * 3_600), secs(5 * 3_600 + 60)), None);
    let overdue = pace_ratio(40, secs(5 * 3_600), secs(-3_600)).expect("overdue pace");
    assert!(
        (overdue - 0.4).abs() < 0.000_1,
        "overdue windows clamp to full elapsed: {overdue}"
    );
}

#[test]
fn pace_style_honours_boundaries_custom_zones_and_no_color() {
    let lit = Theme::fixed(false);
    let defaults = BudgetPaceConfig::default();
    let tone = |color| lit.style(color, Modifier::empty());
    assert_eq!(pace_style(&lit, 1.0, &defaults), lit.soft());
    assert_eq!(pace_style(&lit, 1.01, &defaults), tone(Color::Yellow));
    assert_eq!(pace_style(&lit, 1.5, &defaults), tone(Color::Yellow));
    assert_eq!(pace_style(&lit, 1.51, &defaults), tone(ORANGE));
    assert_eq!(pace_style(&lit, 2.0, &defaults), tone(ORANGE));
    assert_eq!(pace_style(&lit, 2.01, &defaults), tone(Color::Red));

    let tuned = BudgetPaceConfig {
        yellow: 80,
        amber: 120,
        red: 160,
    };
    assert_eq!(pace_style(&lit, 0.81, &tuned), tone(Color::Yellow));
    assert_eq!(pace_style(&lit, 1.21, &tuned), tone(ORANGE));
    assert_eq!(pace_style(&lit, 1.61, &tuned), tone(Color::Red));

    let misordered = BudgetPaceConfig {
        yellow: 200,
        amber: 150,
        red: 100,
    };
    assert_eq!(pace_style(&lit, 1.2, &misordered), tone(Color::Red));
    assert_eq!(pace_style(&lit, 0.9, &misordered), lit.soft());

    let plain = Theme::fixed(true);
    assert_eq!(pace_style(&plain, 2.5, &defaults), plain.soft());
}

#[test]
fn no_color_shape_contracts_keep_budget_todo_and_diff_readable() {
    let plain = Theme::fixed(true);
    let spans = infinite_bar_spans(&plain, 208, 8);
    assert_eq!(text(&spans), "▱▱▱▱▱▱▱▱");
    assert_no_fg(&spans);

    let lit = Theme::fixed(false);
    let spans = infinite_bar_spans(&lit, 208, 8);
    assert_eq!(spans[0].style.fg, Some(Color::Indexed(208)));

    let spans = todo_spans(&plain, 3, 5);
    assert_eq!(text(&spans), "●●●○○ 3/5");
    assert_no_fg(&spans);

    let spans = diff_spans(&plain, 127, 43);
    assert_eq!(text(&spans), "+127 -43");
    assert_no_fg(&spans);
}

#[test]
fn token_breakdown_keeps_shape_and_marker_styles() {
    let plain = Theme::fixed(true);
    let spans = token_breakdown_spans(&plain, 76_000, 12_000, 64_000, 68_000, fmt::tokens_int);
    assert_eq!(text(&spans), "◇ 76k ↘ 12k ↗ 64k ◌ 68k");

    let lit = Theme::fixed(false);
    let spans = token_breakdown_spans(&lit, 76_000, 12_000, 64_000, 68_000, fmt::tokens_int);
    let marker = |glyph: &str| {
        spans
            .iter()
            .find(|span| span.content.contains(glyph))
            .unwrap_or_else(|| panic!("missing {glyph}"))
            .style
    };
    assert_eq!(
        marker(TOKENS_TOTAL).fg,
        lit.style(Color::Magenta, Modifier::empty()).fg
    );
    assert_eq!(
        marker(TOKENS_IN).fg,
        lit.style(SEGMENT_INPUT, Modifier::empty()).fg
    );
    assert_eq!(
        marker(TOKENS_OUT).fg,
        lit.style(SEGMENT_OUTPUT, Modifier::empty()).fg
    );
    assert_eq!(
        marker(TOKENS_CACHED).fg,
        lit.style(SEGMENT_CACHE_READ, Modifier::empty()).fg
    );
    for span in spans.iter().filter(|span| {
        span.content
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c.is_ascii_whitespace())
    }) {
        assert_eq!(span.style, lit.soft(), "figure {:?}", span.content);
    }
}

#[test]
fn context_breakdown_keeps_shape_marker_styles_and_compactions() {
    let plain = Theme::fixed(true);
    let spans = context_breakdown_spans(
        &plain,
        Color::Blue,
        76_500,
        68_200,
        6_600,
        1_700,
        2_300,
        fmt::tokens_int,
    );
    assert_eq!(text(&spans), "▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k");
    assert_no_fg(&spans);

    let theme = Theme::fixed(false);
    let spans = context_breakdown_spans(
        &theme,
        ORANGE,
        76_500,
        68_200,
        6_600,
        1_700,
        2_300,
        fmt::tokens_int,
    );
    let tone = |glyph: &str| {
        spans
            .iter()
            .find(|s| s.content.as_ref() == glyph)
            .unwrap_or_else(|| panic!("no {glyph} span"))
            .style
            .fg
    };
    assert_eq!(tone(CONTEXT_FILLED), Some(Color::Indexed(173)), "severity");
    assert_eq!(tone(TOKENS_CACHED), Some(Color::Indexed(75)), "cache read");
    assert_eq!(
        tone(TOKENS_CACHE_WRITE),
        Some(Color::Indexed(141)),
        "cache write"
    );
    assert_eq!(tone(TOKENS_IN), Some(Color::Indexed(167)), "fresh input");
    assert_eq!(tone(TOKENS_OUT), Some(Color::Indexed(108)), "output");
    // Every figure reads dim — only the markers carry tones — and the `·`
    // seam shares the same dim gray chrome.
    for span in spans.iter().filter(|s| s.content.starts_with(' ')) {
        if span.content.trim().is_empty() || span.content.trim() == "·" {
            continue;
        }
        assert_eq!(span.style, theme.dim(), "figure {:?}", span.content);
    }
    let seam = spans
        .iter()
        .find(|s| s.content.trim() == "·")
        .expect("no seam span");
    assert_eq!(seam.style, theme.dim(), "the seam stays dim chrome");

    let spans = context_compaction_spans(&theme, 2);
    assert_eq!(text(&spans), " · ↻ 2");
    assert_eq!(spans[0].style, theme.dim(), "seam");
    assert_eq!(spans[1].style, compacting_style(&theme), "marker");
    assert_eq!(spans[2].style, theme.dim(), "count");
    assert!(context_compaction_spans(&theme, 0).is_empty());
}
