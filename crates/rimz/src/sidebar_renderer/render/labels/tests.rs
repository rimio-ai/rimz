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

/// Fill rounds to the nearest whole cell: 38% of ten cells is 3.8, so four
/// heavy cells then a light track. At full width the bar has cells to spare,
/// so whole-cell resolution reads smoothly without a fractional edge.
#[test]
fn gauge_rounds_fill_to_whole_cells() {
    let theme = Theme::fixed(true);
    let spans = gauge_spans(&theme, Color::Green, 38, 10);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "━━━━──────");
}

/// At 0% the bar is an unbroken light track, so a "no progress" reading is
/// the same full-width shape as a started one rather than a blank.
#[test]
fn gauge_zero_percent_is_all_track() {
    let theme = Theme::fixed(true);
    let spans = gauge_spans(&theme, Color::Green, 0, 5);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "─────");
}

/// At 100% the heavy rule fills the whole width and leaves no track.
#[test]
fn gauge_full_has_no_track() {
    let theme = Theme::fixed(true);
    let spans = gauge_spans(&theme, Color::Red, 100, 5);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "━━━━━");
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

/// The clock face fills a quarter per quarter hour and rings past the
/// hour, with each bucket's upper edge inclusive.
#[test]
fn elapsed_glyph_fills_by_the_quarter_hour() {
    assert_eq!(elapsed_glyph(0), "◔");
    assert_eq!(elapsed_glyph(900), "◔");
    assert_eq!(elapsed_glyph(901), "◑");
    assert_eq!(elapsed_glyph(1800), "◑");
    assert_eq!(elapsed_glyph(1801), "◕");
    assert_eq!(elapsed_glyph(2700), "◕");
    assert_eq!(elapsed_glyph(2701), "●");
    assert_eq!(elapsed_glyph(3600), "●");
    assert_eq!(elapsed_glyph(3601), "◉");
    assert_eq!(elapsed_glyph(48 * 3600), "◉");
}

/// The window token's tint steps by magnitude — the dim capability chrome
/// below 128k, sky at 128k, gold at 258k, clay amber at 1m+ — with the tinted
/// bands DIM-weighted so they never outshine the meter. `NO_COLOR`
/// collapses every band to the bare DIM weight.
#[test]
fn window_style_tints_by_size_class_but_stays_subordinate() {
    let theme = Theme::fixed(false);
    let banded = |window| window_style(&theme, window);
    assert_eq!(banded(32_000), theme.dim());
    assert_eq!(banded(127_999), theme.dim());
    assert_eq!(banded(128_000), theme.style(Color::Blue, Modifier::DIM));
    assert_eq!(banded(200_000), theme.style(Color::Blue, Modifier::DIM));
    assert_eq!(banded(258_000), theme.style(Color::Yellow, Modifier::DIM));
    assert_eq!(banded(999_999), theme.style(Color::Yellow, Modifier::DIM));
    assert_eq!(banded(1_000_000), theme.style(ORANGE, Modifier::DIM));
    assert_eq!(banded(1_050_000), theme.style(ORANGE, Modifier::DIM));

    let plain = Theme::fixed(true);
    for window in [32_000, 128_000, 258_000, 1_050_000] {
        assert!(window_style(&plain, window).fg.is_none());
        assert!(
            window_style(&plain, window)
                .add_modifier
                .contains(Modifier::DIM)
        );
    }
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

/// A fully spent window (0% remaining) is a full-width *empty* `▱` track —
/// never a `▰` fill — painted red, so "used up" never reads as the quiet
/// untouched track a plain absent-fill would leave. The reset-time text is a
/// separate span the row owns, so it stays unalarmed; only the bar reddens.
#[test]
fn mana_bar_spent_is_a_full_width_red_empty_track() {
    let zones = BudgetZonesConfig::default();
    let plain = Theme::fixed(true);
    let spans = mana_bar_spans(&plain, 0, 10, &zones);
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    // Still an empty track (no `▰`), spanning the full width as one run.
    assert_eq!(text, "▱▱▱▱▱▱▱▱▱▱");
    assert_eq!(spans.len(), 1);
    // Under NO_COLOR the red is suppressed; the empty-track shape still reads.
    assert!(spans[0].style.fg.is_none());

    // With color on, the spent track shares the mana ramp's red — not the
    // dim track tone a non-spent drain leaves behind.
    let lit = Theme::fixed(false);
    let spent = mana_bar_spans(&lit, 0, 10, &zones);
    assert_eq!(spent[0].style.fg, Some(Color::Indexed(167)));
    assert_ne!(spent[0].style.fg, lit.dim().fg);
}

/// Any nonzero remaining budget gets at least one filled cell, even on a
/// narrow sidebar where percentage rounding would otherwise erase it. The
/// bar still uses the red near-spent ramp, but it no longer looks fully
/// exhausted while a sliver remains.
#[test]
fn mana_bar_nonzero_remaining_keeps_one_filled_cell() {
    let plain = Theme::fixed(true);
    let spans = mana_bar_spans(&plain, 1, 10, &BudgetZonesConfig::default());
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "▰▱▱▱▱▱▱▱▱▱");
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

/// The attention glyph wears the shared age heat over a yellow floor — a
/// fresh ask reads yellow, amber past the half hour, red past the hour, the
/// same quarters as the age clock beside it — and only for the
/// `waiting`/`failed` states; every calm state keeps its resting tone,
/// however old.
#[test]
fn attention_glyph_heats_with_the_age_clock_over_a_yellow_floor() {
    let theme = Theme::fixed(false);
    let yellow = theme.style(Color::Yellow, Modifier::BOLD).fg;
    let amber = theme.style(ORANGE, Modifier::BOLD).fg;
    let red = theme.style(Color::Red, Modifier::BOLD).fg;

    // Both attention states floor at yellow while the age heat is still
    // resting — a row that needs a human never reads as dim chrome — then
    // step with the clock quarters. The glyph breathes, so its brightness
    // modifier varies by frame; only the color is asserted here.
    for status in [AgentStatus::Waiting, AgentStatus::Failed] {
        assert_eq!(attention_glyph_style(&theme, status, 5 * 60, 0).fg, yellow);
        assert_eq!(attention_glyph_style(&theme, status, 25 * 60, 0).fg, yellow);
        assert_eq!(attention_glyph_style(&theme, status, 31 * 60, 0).fg, amber);
        assert_eq!(attention_glyph_style(&theme, status, 61 * 60, 0).fg, red);
    }
    // Calm states never heat, however old — they take their plain style.
    assert_eq!(
        attention_glyph_style(&theme, AgentStatus::Idle, 2 * 60 * 60, 0).fg,
        agent_style(&theme, AgentStatus::Idle).fg
    );
    assert_eq!(
        attention_glyph_style(&theme, AgentStatus::Running, 2 * 60 * 60, 0).fg,
        agent_style(&theme, AgentStatus::Running).fg
    );
}

/// Each animation cycles through its frames and wraps, so the phase can grow
/// without bound.
#[test]
fn animations_cycle_and_wrap() {
    for (phase, expected) in WORKING_FRAMES.iter().enumerate() {
        assert_eq!(working_glyph(phase as u64), *expected);
    }
    assert_eq!(
        working_glyph(WORKING_FRAMES.len() as u64),
        WORKING_FRAMES[0]
    );
    assert_eq!(THINKING_FRAMES, ["·", "✢", "✳", "✶", "✻", "✶", "✳", "✢"]);
    for (phase, expected) in THINKING_FRAMES.iter().enumerate() {
        let held_phase = phase as u64 * THINKING_FRAME_HOLD;
        assert_eq!(thinking_glyph(held_phase), *expected);
        assert_eq!(thinking_glyph(held_phase + 1), *expected);
        assert_eq!(thinking_glyph(held_phase + 2), *expected);
    }
    assert_eq!(
        thinking_glyph(THINKING_FRAMES.len() as u64 * THINKING_FRAME_HOLD),
        THINKING_FRAMES[0]
    );
    assert_eq!(
        resolver_glyph(RESOLVER_FRAMES.len() as u64),
        RESOLVER_FRAMES[0]
    );
    // The two transient heads cycle and wrap on the same shared phase.
    for (phase, expected) in COMPACTING_FRAMES.iter().enumerate() {
        assert_eq!(compacting_glyph(phase as u64), *expected);
    }
    assert_eq!(
        compacting_glyph(COMPACTING_FRAMES.len() as u64),
        COMPACTING_FRAMES[0]
    );
    for (phase, expected) in SUBAGENT_FRAMES.iter().enumerate() {
        assert_eq!(subagent_glyph(phase as u64), *expected);
    }
    assert_eq!(
        subagent_glyph(SUBAGENT_FRAMES.len() as u64),
        SUBAGENT_FRAMES[0]
    );
    // The phase can grow without bound and still indexes a frame.
    assert_eq!(
        working_glyph(u64::MAX),
        WORKING_FRAMES[(u64::MAX % WORKING_FRAMES.len() as u64) as usize]
    );
}

/// The loading dots are static while the attention glyph breathes a slow
/// brightness pulse — `DIM` at the troughs, `BOLD` at the peak — that wraps
/// with the phase, never strobing.
#[test]
fn loading_dots_and_attention_breath_cadence() {
    assert_eq!(loading_dots(0), "...");
    assert_eq!(loading_dots(7), "...");
    assert_eq!(loading_dots(8), "...");
    assert_eq!(loading_dots(16), "...");
    assert_eq!(loading_dots(24), "...");

    // DIM at the troughs, normal between, BOLD at the half-cycle peak.
    let fresh = 5 * 60;
    assert_eq!(attention_breath(0, fresh), Modifier::DIM);
    assert_eq!(attention_breath(6, fresh), Modifier::empty());
    assert_eq!(
        attention_breath(12, fresh),
        Modifier::BOLD,
        "peak at the half-cycle"
    );
    assert_eq!(attention_breath(18, fresh), Modifier::empty());
    assert_eq!(
        attention_breath(24, fresh),
        Modifier::DIM,
        "wraps to the trough"
    );
}

/// The breath paces with the age heat: yellow keeps the resting ~2.4s
/// triangle, amber runs the same wave at double-time (~1.2s), and red
/// drops the swell for a hard `BOLD`↔`DIM` blink flipping every third
/// tick — so the cadence alone carries the urgency under `NO_COLOR`.
#[test]
fn attention_breath_quickens_with_the_age_heat() {
    // Yellow (25m): the same wave as the fresh floor — slow.
    let yellow = 25 * 60;
    assert_eq!(attention_breath(0, yellow), Modifier::DIM);
    assert_eq!(attention_breath(12, yellow), Modifier::BOLD);

    // Amber (40m): double-time — the half-cycle peak lands at tick 6.
    let amber = 40 * 60;
    assert_eq!(attention_breath(0, amber), Modifier::DIM);
    assert_eq!(
        attention_breath(6, amber),
        Modifier::BOLD,
        "peak in half the time"
    );
    assert_eq!(
        attention_breath(12, amber),
        Modifier::DIM,
        "full cycle in 1.2s"
    );

    // Red (2h): a square wave — no normal mid-level, just BOLD↔DIM.
    let red = 2 * 60 * 60;
    assert_eq!(attention_breath(0, red), Modifier::BOLD);
    assert_eq!(
        attention_breath(2, red),
        Modifier::BOLD,
        "held through the half"
    );
    assert_eq!(
        attention_breath(3, red),
        Modifier::DIM,
        "hard flip, no gradient"
    );
    assert_eq!(attention_breath(5, red), Modifier::DIM);
    assert_eq!(attention_breath(6, red), Modifier::BOLD, "wraps");
}

/// The elapsed-age tone steps with the clock-fill quarters: the dim resting
/// weight through the first quarter (a resume still hits cache), yellow to
/// the half hour, amber beyond it, red past the hour — when a resume would
/// likely re-read the whole context uncached.
#[test]
fn activity_age_style_steps_with_the_clock_quarters() {
    let theme = Theme::fixed(false);
    let yellow = theme.style(Color::Yellow, Modifier::empty());
    let amber = theme.style(ORANGE, Modifier::empty());
    let red = theme.style(Color::Red, Modifier::empty());
    assert_eq!(activity_age_style(&theme, 60), theme.dim());
    assert_eq!(activity_age_style(&theme, 900), theme.dim());
    assert_eq!(
        activity_age_style(&theme, 901),
        yellow,
        "yellow from the second quarter"
    );
    assert_eq!(activity_age_style(&theme, 1800), yellow);
    assert_eq!(
        activity_age_style(&theme, 1801),
        amber,
        "amber past the half hour"
    );
    assert_eq!(activity_age_style(&theme, 3600), amber);
    assert_eq!(
        activity_age_style(&theme, 3601),
        red,
        "red once the cache is likely invalidated"
    );
}

/// The token breakdown reads `◇ ↘ ↗ ◍ ◌`, each marker in its one color
/// (`◇` violet, the rest their bar-segment tones) with soft-tier figures;
/// the `◍` cache-write field drops when excluded (the W/M rows). Under
/// `NO_COLOR` the glyph shapes still spell the split.
#[test]
fn token_breakdown_shape_and_optional_cache_write() {
    let theme = Theme::fixed(true);
    let full = token_breakdown_spans(
        &theme,
        76_000,
        12_000,
        64_000,
        12_000,
        68_000,
        super::super::fmt::tokens_int,
        true,
    );
    let text: String = full.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "◇ 76k ↘ 12k ↗ 64k ◍ 12k ◌ 68k");

    let lean = token_breakdown_spans(
        &theme,
        76_000,
        12_000,
        64_000,
        12_000,
        68_000,
        super::super::fmt::tokens_int,
        false,
    );
    let text: String = lean.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "◇ 76k ↘ 12k ↗ 64k ◌ 68k", "no ◍ when excluded");
}

/// With color on, every breakdown marker wears its one tone — the same
/// segment colors the card's context line legends — and the figures read
/// at the soft tier like every stat figure across the sidebar.
#[test]
fn token_breakdown_markers_wear_their_segment_colors() {
    let lit = Theme::fixed(false);
    let spans = token_breakdown_spans(
        &lit,
        76_000,
        12_000,
        64_000,
        12_000,
        68_000,
        super::super::fmt::tokens_int,
        true,
    );
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
        marker(TOKENS_CACHE_WRITE).fg,
        lit.style(SEGMENT_CACHE_WRITE, Modifier::empty()).fg
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
        super::super::fmt::tokens_int,
    );
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k");
    for span in &spans {
        assert!(span.style.fg.is_none());
    }
}

/// With color on, the context line is the bar's legend: the `▤` head wears
/// the caller's severity, each composition marker its bar-segment tone
/// (`◌` blue, `◍` yellow, `↘` red, `↗` green), and every figure reads at the
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
        super::super::fmt::tokens_int,
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
        Some(Color::Indexed(179)),
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

/// The rate-limited glyph is the media `pause` mark carrying the
/// text-presentation selector (`U+FE0E`), so it renders single-cell
/// monochrome and the cockpit columns never drift when it appears.
#[test]
fn rate_limited_glyph_carries_the_text_presentation_selector() {
    assert_eq!(status_glyph(AgentStatus::RateLimited), RATE_LIMITED_GLYPH);
    let mut chars = RATE_LIMITED_GLYPH.chars();
    assert_eq!(chars.next(), Some('⏸'));
    assert_eq!(chars.next(), Some('\u{FE0E}'));
    assert_eq!(chars.next(), None);
    // Measured by ratatui's own layout width (the selector is zero-width),
    // it occupies exactly one cell like every other status glyph — so the
    // cockpit columns never drift when the `⏸` bucket appears.
    assert_eq!(Span::raw(RATE_LIMITED_GLYPH).width(), 1);
    assert_eq!(Span::raw(status_glyph(AgentStatus::Waiting)).width(), 1);
}

/// Rate-limited rests in held amber — the attention family, but *not* the
/// bold, heating weight of `?`/`!`. It is attention-class yet parked, so
/// neglect never escalates it: even hours parked it stays amber, since
/// there is nothing to do but wait for the reset.
#[test]
fn rate_limited_rests_in_held_amber_and_never_reddens() {
    let theme = Theme::fixed(false);
    let style = status_style(&theme, AgentStatus::RateLimited);
    assert_eq!(style.fg, Some(Color::Indexed(179)));
    assert!(!style.add_modifier.contains(Modifier::BOLD));
    let long_parked = attention_glyph_style(&theme, AgentStatus::RateLimited, 2 * 60 * 60, 0);
    assert_eq!(long_parked.fg, Some(Color::Indexed(179)));
    assert!(!long_parked.add_modifier.contains(Modifier::BOLD));
}

/// A running agent animates the working fill; while its turn is still in
/// the pre-edit thinking phase it sparkles; a stalled agent (folded to
/// `Failed` upstream) and every other state takes the static glyph,
/// regardless of phase.
#[test]
fn agent_glyph_animates_only_active_states() {
    assert_eq!(
        agent_glyph(AgentStatus::Running, TurnPhase::Acting, 2),
        WORKING_FRAMES[2]
    );
    assert_eq!(
        agent_glyph(AgentStatus::Running, TurnPhase::Reasoning, 4),
        THINKING_FRAMES[1]
    );
    // The sparkle is the running-state indicator — a stale thinking bit on
    // a non-running agent never sparkles.
    assert_eq!(agent_glyph(AgentStatus::Idle, TurnPhase::Idle, 2), "○");
    assert_eq!(agent_glyph(AgentStatus::Waiting, TurnPhase::Idle, 2), "?");
    assert_eq!(agent_glyph(AgentStatus::Failed, TurnPhase::Idle, 2), "!");
    assert_eq!(agent_glyph(AgentStatus::Idle, TurnPhase::Idle, 2), "○");
    assert_eq!(agent_glyph(AgentStatus::Success, TurnPhase::Idle, 2), "✓");
}
