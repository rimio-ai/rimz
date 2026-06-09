use super::*;

/// The commit delta spells only what's there: zero components drop rather
/// than printing `⇡0`, and a fully-zero delta is no spans at all — the
/// header's landed markers own that state.
#[test]
fn branch_delta_omits_zero_components() {
    let theme = Theme::fixed(true);
    let text = |spans: Vec<Span<'static>>| -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    };
    assert_eq!(text(branch_delta_spans(&theme, 3, 1)), "⇡3 ⇣1");
    assert_eq!(text(branch_delta_spans(&theme, 3, 0)), "⇡3");
    assert_eq!(text(branch_delta_spans(&theme, 0, 5)), "⇣5");
    assert_eq!(text(branch_delta_spans(&theme, 0, 0)), "");
    assert_eq!(text(trunk_equal_spans(&theme, "main")), "≡ main");
    assert_eq!(text(trunk_clear_spans(&theme, "main")), "✓ main");
}
/// `NO_COLOR` strips the green→amber→red ramp, but the heavy/light weight
/// split still spells the meter — the `━`/`─` shape carries the reading by
/// itself, without any label.
#[test]
fn gauge_under_no_color_reads_by_shape() {
    let theme = Theme::fixed(true);
    let spans = gauge_spans(&theme, Color::Green, 60, 5);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "━━━──");
    for span in &spans {
        assert!(
            span.style.fg.is_none(),
            "NO_COLOR theme must not emit fg color: {span:?}"
        );
    }
}
/// Fill rounds to whole cells and keeps the edge states explicit: 0% is a
/// full track, 100% is a full fill, and fractional percentages round to the
/// nearest cell.
#[test]
fn gauge_fill_math_handles_rounding_and_edges() {
    let theme = Theme::fixed(true);
    for (color, percent, width, expected) in [
        (Color::Green, 38, 10, "━━━━──────"),
        (Color::Green, 0, 5, "─────"),
        (Color::Red, 100, 5, "━━━━━"),
    ] {
        let spans = gauge_spans(&theme, color, percent, width);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, expected, "{percent}% over width {width}");
    }
}
/// The segmented bar fills the same run as the plain gauge, split into
/// colored sub-runs whose cell counts sum to the filled total. Under
/// `NO_COLOR` the segments merge into one heavy run — the shape still reads.
#[test]
fn segmented_gauge_sums_to_filled_and_merges_under_no_color() {
    let theme = Theme::fixed(true);
    let segments = [
        (8_000_u64, Color::Green),
        (5_000, Color::Cyan),
        (2_000, Color::Blue),
    ];
    let spans = segmented_gauge_spans(&theme, &segments, Color::Green, 60, 10);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    // 60% of 10 = 6 filled; segments apportion 6 → 3/2/1; then a 4-cell track.
    assert_eq!(text, "━━━━━━────");
    let filled = text.chars().filter(|c| *c == '━').count();
    assert_eq!(filled, 6);
    for span in &spans {
        assert!(span.style.fg.is_none());
    }
}
/// With nothing to break down (all-zero weights) the segmented bar is just
/// the plain single-color gauge.
#[test]
fn segmented_gauge_falls_back_with_zero_weights() {
    let theme = Theme::fixed(true);
    let spans = segmented_gauge_spans(
        &theme,
        &[(0, Color::Green), (0, Color::Cyan)],
        Color::Green,
        50,
        4,
    );
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "━━──");
}
/// The renderer's one job on severity: map the domain's tier to its tone —
/// calm blue, yellow, amber clay, red. The classification itself (the
/// percent/token bands, the worst-first ordering) lives in
/// `crate::feed::ContextSeverity` and is tested beside it.
#[test]
fn severity_color_maps_the_four_tiers() {
    assert_eq!(severity_color(ContextSeverity::Calm), Color::Blue);
    assert_eq!(severity_color(ContextSeverity::Yellow), Color::Yellow);
    assert_eq!(severity_color(ContextSeverity::Amber), ORANGE);
    assert_eq!(severity_color(ContextSeverity::Red), Color::Red);
}
/// Largest-remainder apportionment always sums to the requested total.
#[test]
fn apportion_sums_to_total() {
    assert_eq!(apportion([3, 1, 1], 5), vec![3, 1, 1]);
    assert_eq!(apportion([1, 1, 1], 4).iter().sum::<usize>(), 4);
    assert_eq!(apportion([0, 0], 3), vec![0, 0]);
}
/// The mana bar drains (filled = remaining) in the segmented `▰`/`▱` style
/// and reads by that fill/hollow shape under `NO_COLOR`; the fill ramps
/// green → yellow → amber → red by how much budget is left on the
/// `[sidebar.budget]` zones — one ramp for both the 5-hour and weekly
/// windows, speaking the same gold → clay-amber escalation as the age and
/// context ramps — over a dim `▱` track, a step up from the faint
/// context-gauge track.
#[test]
fn mana_bar_drains_and_ramps() {
    let zones = BudgetZonesConfig::default();
    let plain = Theme::fixed(true);
    let spans = mana_bar_spans(&plain, 70, 10, &zones);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "▰▰▰▰▰▰▰▱▱▱");
    for span in &spans {
        assert!(span.style.fg.is_none());
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
    // The drained share rides the dim chrome, legible against the fill.
    let track = &mana_bar_spans(&lit, 70, 10, &zones)[1];
    assert_eq!(track.style, lit.dim());
}
/// The zones are config: a tuned `[sidebar.budget]` moves the band edges, so
/// the same reading reclassifies — the ramp is driven by the snapshot-carried
/// config, not a built-in table. Checked worst-first, so a misordered config
/// degrades to the worse tier; a reading above every zone rests green.
#[test]
fn mana_style_honours_custom_and_misordered_zones() {
    let lit = Theme::fixed(false);
    let tuned = BudgetZonesConfig {
        yellow: 80,
        amber: 40,
        red: 20,
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

    // The bar fill delegates to the same tuned zones: 70% — resting green
    // under the defaults — paints its first cell yellow here.
    let bar = mana_bar_spans(&lit, 70, 10, &tuned);
    assert_eq!(bar[0].style.fg, Some(Color::Indexed(179)));

    // Misordered (red bound above amber/yellow): the worst tier wins.
    let misordered = BudgetZonesConfig {
        yellow: 25,
        amber: 10,
        red: 50,
    };
    assert_eq!(mana_style(&lit, 30, &misordered), tone(Color::Red));
    assert_eq!(mana_style(&lit, 50, &misordered), tone(Color::Green));
}
/// The drain edge states stay distinct: fully spent is a red empty track, while
/// any nonzero remaining budget keeps at least one filled cell.
#[test]
fn mana_bar_edge_shapes_distinguish_spent_from_nearly_spent() {
    let plain = Theme::fixed(true);
    let zones = BudgetZonesConfig::default();
    for (remaining, expected) in [(0, "▱▱▱▱▱▱▱▱▱▱"), (1, "▰▱▱▱▱▱▱▱▱▱")]
    {
        let spans = mana_bar_spans(&plain, remaining, 10, &zones);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, expected, "{remaining}% remaining");
        if remaining == 0 {
            assert_eq!(spans.len(), 1, "spent budget is one empty track span");
        }
        assert!(spans[0].style.fg.is_none());
    }

    let lit = Theme::fixed(false);
    let spent = mana_bar_spans(&lit, 0, 10, &zones);
    assert_eq!(spent[0].style.fg, Some(Color::Indexed(167)));
    assert_ne!(spent[0].style.fg, lit.dim().fg);
}
/// The infinite bar is a full-width empty `▱` track wearing the provider's
/// brand color — the same tone its `∞` icon carries, so the two read as one
/// branded unmetered bar. Under `NO_COLOR` the unbroken `▱` run reads as an
/// empty track by shape.
#[test]
fn infinite_bar_is_an_empty_brand_colored_track() {
    let plain = Theme::fixed(true);
    let spans = infinite_bar_spans(&plain, 208, 8);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "▱▱▱▱▱▱▱▱");
    for span in &spans {
        assert!(span.style.fg.is_none());
    }

    // With color on the track shares the `∞` icon's brand color.
    let lit = Theme::fixed(false);
    let spans = infinite_bar_spans(&lit, 208, 8);
    assert_eq!(spans[0].style.fg, Some(Color::Indexed(208)));
}
/// Todo dots use the same fill/empty grammar as the gauge — the dot
/// count plus the `n/m` label survive `NO_COLOR`.
#[test]
fn todo_under_no_color_reads_by_shape_and_label() {
    let theme = Theme::fixed(true);
    let spans = todo_spans(&theme, 3, 5);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "●●●○○ 3/5");
    for span in &spans {
        assert!(span.style.fg.is_none());
    }
}
/// Diff stats fall back to the numbers when color is stripped; the
/// `+`/`-` prefixes still distinguish the two counts.
#[test]
fn diff_under_no_color_keeps_signed_numbers() {
    let theme = Theme::fixed(true);
    let spans = diff_spans(&theme, 127, 43);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "+127 -43");
    for span in &spans {
        assert!(span.style.fg.is_none());
    }
}
/// The token breakdown reads `◇ ↘ ↗ ◌`, each marker in its one color
/// (`◇` violet, the rest their bar-segment tones) with soft-tier figures. Under
/// `NO_COLOR` the glyph shapes still spell the split.
#[test]
fn token_breakdown_shape_is_lean() {
    let theme = Theme::fixed(true);
    let spans = token_breakdown_spans(&theme, 76_000, 12_000, 64_000, 68_000, fmt::tokens_int);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "◇ 76k ↘ 12k ↗ 64k ◌ 68k");
}
/// With color on, every breakdown marker wears its one tone — the same
/// segment colors the card's context line legends — and the figures read
/// at the soft tier like every stat figure across the sidebar.
#[test]
fn token_breakdown_markers_wear_their_segment_colors() {
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
/// The card's context line reads `▤ · ◌ ◍ ↘ ↗` — the filled window, a dot
/// seam, then the composition ordered by how the window filled. Under
/// `NO_COLOR` the glyph shapes still spell the split.
#[test]
fn context_breakdown_shape_leads_with_the_filled_window() {
    let theme = Theme::fixed(true);
    let spans = context_breakdown_spans(
        &theme,
        Color::Blue,
        76_500,
        68_200,
        6_600,
        1_700,
        2_300,
        fmt::tokens_int,
    );
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k");
    for span in &spans {
        assert!(span.style.fg.is_none());
    }
}
/// With color on, the context line is the bar's legend: the `▤` head wears
/// the caller's severity, each composition marker its bar-segment tone
/// (`◌` blue, `◍` violet, `↘` red, `↗` green), and every figure reads at the
/// dim chrome weight — a step under the name line's soft tokens.
#[test]
fn context_breakdown_markers_wear_their_segment_colors() {
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
}

/// The compaction count uses the same visual grammar as the context-token
/// stats: a colored marker, then a dim figure.
#[test]
fn context_compaction_styles_marker_only() {
    let theme = Theme::fixed(false);
    let spans = context_compaction_spans(&theme, 2);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, " · ↻ 2");

    assert_eq!(spans[0].style, theme.dim(), "seam");
    assert_eq!(spans[1].style, compacting_style(&theme), "marker");
    assert_eq!(spans[2].style, theme.dim(), "count");
    assert!(context_compaction_spans(&theme, 0).is_empty());
}
