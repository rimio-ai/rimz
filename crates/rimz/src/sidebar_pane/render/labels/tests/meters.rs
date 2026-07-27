use super::*;
use crate::config::GlyphRole;
use crate::sidebar_pane::render::theme::Component;
use jiff::SignedDuration;

fn text(spans: &[Span<'_>]) -> String {
    spans.iter().map(|s| s.content.as_ref()).collect()
}

fn assert_no_fg(spans: &[Span<'_>]) {
    assert!(spans.iter().all(|span| span.style.fg.is_none()));
}

fn rgb(color: Color) -> (u8, u8, u8) {
    crate::sidebar_pane::render::theme::color_to_rgb(color).expect("color rgb")
}

fn indexed_from_truecolor(color: Color) -> Color {
    let (red, green, blue) = rgb(color);
    Color::Indexed(crate::config::nearest_xterm_index(red, green, blue))
}

fn assert_cost_tones_are_hot_and_distinct(theme: &Theme) {
    let write = theme.component(Component::CacheWrite);
    let input = theme.component(Component::Input);
    let read = theme.component(Component::CacheRead);
    assert_ne!(
        input,
        theme.heat_tone(2.0 / 3.0),
        "fresh input warms past the caution amber — costlier than the hot/costly tier"
    );
    assert_ne!(
        input,
        theme.heat_tone(1.0),
        "fresh input deepens past the 100% alarm red into its own redder cell — the reddest marker on screen (redder-than-alarm proven in theme tests)"
    );
    assert_ne!(
        read, input,
        "cache-read green stays distinct from the input vermilion"
    );
    assert_eq!(
        write,
        theme.component(Component::SubagentHeader),
        "cache-write shares the compaction/delegation violet (meta) slot"
    );
    assert_ne!(
        write, input,
        "cache-write stays distinct from the input vermilion"
    );
}

#[test]
fn branch_delta_omits_zero_components() {
    let theme = Theme::fixed(true);
    assert_eq!(text(&branch_delta_spans(&theme, 3, 1)), "⇡3 ⇣1");
    assert_eq!(text(&branch_delta_spans(&theme, 3, 0)), "⇡3");
    assert_eq!(text(&branch_delta_spans(&theme, 0, 5)), "⇣5");
    assert_eq!(text(&branch_delta_spans(&theme, 0, 0)), "");
    assert_eq!(
        text(&trunk_glyph_spans(
            &theme,
            GlyphRole::WorktreeTrunkEqual,
            "main",
            Component::WorktreePristine
        )),
        "≡ main"
    );
    assert_eq!(
        text(&trunk_glyph_spans(
            &theme,
            GlyphRole::WorktreeTrunkMerge,
            "main",
            Component::WorktreeMerged
        )),
        "✓ main"
    );
}

#[test]
fn gauge_bars_map_severity_and_quantize_segments() {
    let theme = Theme::fixed(false);
    let bands = crate::config::ContextMeterConfig::default();
    assert_eq!(
        severity_heat_color(&theme, ContextSeverity::Calm, 0, None, &bands),
        theme.heat_tone(0.0),
        "calm rests at the healthy green start of the ramp"
    );
    assert_eq!(
        severity_heat_color(&theme, ContextSeverity::Yellow, 50, None, &bands),
        theme.heat_tone(0.0)
    );
    assert_eq!(
        severity_heat_color(&theme, ContextSeverity::Yellow, 70, None, &bands),
        theme.heat_tone(1.0 / 3.0)
    );
    assert_eq!(
        severity_heat_color(&theme, ContextSeverity::Amber, 80, None, &bands),
        theme.heat_tone(2.0 / 3.0)
    );
    assert_eq!(
        severity_heat_color(&theme, ContextSeverity::Red, 90, None, &bands),
        theme.heat_tone(1.0)
    );

    let truecolor = Theme::fixed_for_theme(
        false,
        &crate::config::ThemeConfig {
            mode: crate::config::ThemeMode::Truecolor,
            ..crate::config::ThemeConfig::default()
        },
    );
    for (severity, percent) in [
        (ContextSeverity::Yellow, 50),
        (ContextSeverity::Amber, 80),
        (ContextSeverity::Red, 90),
    ] {
        assert!(
            matches!(
                severity_heat_color(&truecolor, severity, percent, None, &bands),
                Color::Rgb(..)
            ),
            "{severity:?} should emit an RGB heat-ramp tone in truecolor"
        );
    }
    let warming = severity_heat_color(&truecolor, ContextSeverity::Yellow, 60, None, &bands);
    assert_eq!(warming, truecolor.heat_tone(1.0 / 6.0));
    assert_ne!(warming, truecolor.heat_tone(0.0));
    assert_ne!(warming, truecolor.heat_tone(1.0 / 3.0));
    let token_warming = severity_heat_color(
        &truecolor,
        ContextSeverity::Yellow,
        10,
        Some(160_000),
        &bands,
    );
    assert_eq!(token_warming, truecolor.heat_tone(1.0 / 6.0));
    let token_red =
        severity_heat_color(&truecolor, ContextSeverity::Red, 10, Some(384_000), &bands);
    assert_eq!(
        token_red,
        truecolor.heat_tone(1.0),
        "the token axis can drive the same heat scale"
    );
    let plain = Theme::fixed(true);
    for (percent, width, expected) in [
        (60, 5, "━━━──"),
        (38, 10, "━━━━──────"),
        (0, 5, "─────"),
        (100, 5, "━━━━━"),
    ] {
        let spans =
            context_gauge_spans(&plain, plain.heat_tone(0.5), &[], f64::from(percent), width);
        assert_eq!(text(&spans), expected, "{percent}% over width {width}");
        assert_no_fg(&spans);
    }

    assert_eq!(filled_half_cells(0.0, 4), (0, false));
    assert_eq!(filled_half_cells(f64::EPSILON, 4), (0, true));
    assert_eq!(filled_half_cells(87.5, 4), (3, true));
    assert_eq!(filled_half_cells(100.0, 4), (4, false));
    assert_eq!(
        text(&context_gauge_spans(
            &plain,
            plain.heat_tone(0.5),
            &[],
            f64::EPSILON,
            4,
        )),
        "╸───",
        "a nonzero fill keeps a half-cell floor"
    );
    assert_eq!(
        text(&context_gauge_spans(
            &plain,
            plain.heat_tone(0.5),
            &[],
            87.5,
            4,
        )),
        "━━━╸",
        "a fractional tail uses the themable heavy left half"
    );

    // Composition rides the bar at every severity. Each accent owns a leading
    // half-rule cell, and all shapes together still end exactly at the fill.
    let segments = [
        (8_000_u64, plain.component(Component::CacheRead)),
        (5_000, plain.component(Component::CacheWrite)),
        (2_000, plain.component(Component::Input)),
    ];
    let rendered = text(&context_gauge_spans(
        &plain,
        plain.heat_tone(0.6),
        &segments,
        60.0,
        10,
    ));
    assert_eq!(rendered, "━━━╺━╺────");
    assert_eq!(
        rendered.chars().count(),
        10,
        "the bar fills its width exactly"
    );
    assert_eq!(
        rendered.matches('╺').count(),
        2,
        "a gap-fronted cap starts each accent run"
    );
    assert_eq!(
        rendered.chars().filter(|c| *c == '━').count() + rendered.matches('╺').count(),
        6,
        "lead plus accent shapes occupy the 60% run"
    );
    assert_eq!(
        rendered.matches('─').count(),
        4,
        "the track fills the remainder"
    );
    assert_no_fg(&context_gauge_spans(
        &plain,
        plain.heat_tone(0.6),
        &segments,
        60.0,
        10,
    ));

    let lit = Theme::fixed(false);
    let regression = [
        (6, 10, "╸─────────", false),
        (6, 32, "━╺──────────────────────────────", true),
        (30, 10, "━━╺───────", true),
        (30, 32, "━━━━━━━━╺━──────────────────────", true),
    ];
    for (percent, width, expected, write_visible) in regression {
        let segments = [
            (50_000, lit.component(Component::CacheRead)),
            (11_000, lit.component(Component::CacheWrite)),
            (0, lit.component(Component::Input)),
        ];
        let spans = context_gauge_spans(
            &lit,
            lit.heat_tone(0.0),
            &segments,
            f64::from(percent),
            width,
        );
        assert_eq!(text(&spans), expected, "{percent}% over width {width}");
        assert_eq!(
            spans.iter().any(|span| {
                span.content == "╺" && span.style.fg == Some(lit.component(Component::CacheWrite))
            }),
            write_visible,
            "cache-write visibility at {percent}% over width {width}"
        );
    }

    let half_tail = context_gauge_spans(&lit, lit.heat_tone(0.0), &segments, 55.0, 10);
    let half_tail_text = text(&half_tail);
    assert_eq!(half_tail_text, "━━━╺━╺────");
    assert!(
        !half_tail_text.contains('╸'),
        "a segmented bar ends on a whole cell"
    );
    let track_start = half_tail_text.find('─').expect("segmented bar has track");
    assert!(
        matches!(
            half_tail_text[..track_start].chars().next_back(),
            Some('━' | '╺')
        ),
        "the last segment sits flush against the track"
    );

    let narrow = text(&context_gauge_spans(
        &plain,
        plain.heat_tone(0.0),
        &segments,
        20.0,
        10,
    ));
    assert_eq!(
        narrow, "━╺────────",
        "the smallest of three segments drops when only two cells fill"
    );

    let ordered = [
        (10_u64, lit.component(Component::CacheRead)),
        (9, lit.component(Component::CacheWrite)),
        (1, lit.component(Component::Input)),
    ];
    let ordered_spans = context_gauge_spans(&lit, lit.heat_tone(0.0), &ordered, 50.0, 20);
    assert_eq!(text(&ordered_spans), "━━━━━╺━━━╺──────────");
    let ink = |color| {
        ordered_spans
            .iter()
            .filter(|span| span.style.fg == Some(color))
            .map(|span| {
                span.content
                    .chars()
                    .map(|glyph| {
                        if glyph == '━' {
                            2
                        } else {
                            usize::from(glyph == '╺')
                        }
                    })
                    .sum::<usize>()
            })
            .sum::<usize>()
    };
    assert!(
        ink(lit.heat_tone(0.0)) >= ink(lit.component(Component::CacheWrite))
            && ink(lit.component(Component::CacheWrite)) >= ink(lit.component(Component::Input)),
        "representable weights do not invert their rendered ink"
    );

    let forced = [
        (5_u64, plain.component(Component::CacheRead)),
        (6, plain.component(Component::CacheWrite)),
        (6, plain.component(Component::Input)),
    ];
    assert_eq!(
        text(&context_gauge_spans(
            &plain,
            plain.heat_tone(0.0),
            &forced,
            30.0,
            10,
        )),
        "━╺╺───────",
        "minimum shapes win when fixed semantic order makes inversion unavoidable"
    );

    for (segments, percent, width) in [
        (&segments[..], 60, 10),
        (&ordered[..], 50, 20),
        (&forced[..], 30, 10),
    ] {
        let rendered = text(&context_gauge_spans(
            &plain,
            plain.heat_tone(0.0),
            segments,
            percent as f64,
            width,
        ));
        let filled = ((f64::from(percent) / 100.0) * width as f64).round() as usize;
        assert_eq!(rendered.chars().count(), width);
        assert!(matches!(rendered.chars().nth(filled - 1), Some('━' | '╺')));
        assert_eq!(rendered.chars().nth(filled), Some('─'));
    }

    // A weightless split falls back to a single flat health run.
    let spans = context_gauge_spans(
        &plain,
        plain.heat_tone(0.5),
        &[
            (0, plain.component(Component::CacheRead)),
            (0, plain.component(Component::Input)),
        ],
        50.0,
        4,
    );
    assert_eq!(text(&spans), "━━──");
    assert_no_fg(&spans);

    // In truecolor the cache-read run uses the flat severity tone, while
    // cache-write and fresh input stay flat in their accents.
    let amount = 0.6;
    let health = truecolor.heat_tone(amount);
    let segments = [
        (9_000_u64, health),
        (3_000, truecolor.component(Component::CacheWrite)),
        (1_500, truecolor.component(Component::Input)),
    ];
    let mut runs: Vec<Vec<Option<Color>>> = vec![Vec::new()];
    for span in &context_gauge_spans(&truecolor, health, &segments, 90.0, 16) {
        let content = span.content.as_ref();
        if content == "╺" {
            runs.push(Vec::new());
        } else if !content.is_empty() && content.chars().all(|glyph| glyph == '━') {
            runs.last_mut()
                .unwrap()
                .extend(content.chars().map(|_| span.style.fg));
        }
    }
    let read = &runs[0];
    assert!(
        read.len() >= 2 && read.iter().all(|fg| matches!(fg, Some(Color::Rgb(..)))),
        "the cache-read run is truecolor: {read:?}"
    );
    assert!(
        read.iter()
            .all(|fg| *fg == Some(truecolor.heat_tone(amount))),
        "the cache-read run is the flat severity tone: {read:?}"
    );
    let write_fg = Some(truecolor.component(Component::CacheWrite));
    let input_fg = Some(truecolor.component(Component::Input));
    assert!(
        !runs[1].is_empty() && runs[1].iter().all(|fg| *fg == write_fg),
        "cache-write is a flat accent"
    );
    assert!(
        !runs[2].is_empty() && runs[2].iter().all(|fg| *fg == input_fg),
        "fresh input is a flat accent"
    );
    let indexed = Theme::fixed(false);
    assert_cost_tones_are_hot_and_distinct(&indexed);
    assert_cost_tones_are_hot_and_distinct(&truecolor);
    assert_eq!(
        indexed.component(Component::Input),
        indexed_from_truecolor(truecolor.component(Component::Input)),
        "indexed input is the truecolor vermilion quantized to xterm"
    );
    assert_eq!(
        indexed.component(Component::CacheWrite),
        indexed_from_truecolor(truecolor.component(Component::CacheWrite)),
        "indexed cache-write is the truecolor violet quantized to xterm"
    );
}

#[test]
fn context_gauge_drops_subscale_accents_and_seals_segmented_tail() {
    let theme = Theme::fixed(false);
    let read = theme.component(Component::CacheRead);
    let write = theme.component(Component::CacheWrite);
    let input = theme.component(Component::Input);

    let noise = context_gauge_spans(
        &theme,
        theme.heat_tone(0.0),
        &[(87_000, read), (3_000, write), (2, input)],
        45.0,
        30,
    );
    let noise_text = text(&noise);
    assert_eq!(noise_text, format!("{}╺{}", "━".repeat(13), "─".repeat(16)));
    assert_eq!(noise_text.matches('╺').count(), 1);
    assert!(!noise_text.contains('╸'));
    assert!(
        noise.iter().all(|span| span.style.fg != Some(input)),
        "a sub-0.5% input accent folds into the lead run"
    );

    let threshold = context_gauge_spans(
        &theme,
        theme.heat_tone(0.0),
        &[(199, read), (0, write), (1, input)],
        100.0,
        10,
    );
    assert_eq!(text(&threshold), format!("{}╺", "━".repeat(9)));
    assert_eq!(threshold.last().and_then(|span| span.style.fg), Some(input));

    let sealed = text(&context_gauge_spans(
        &theme,
        theme.heat_tone(0.0),
        &[(183_000, read), (0, write), (1_000, input)],
        92.0,
        30,
    ));
    assert_eq!(sealed, format!("{}╺{}", "━".repeat(27), "─".repeat(2)));
    assert!(!sealed.contains('╸'));

    let flat = context_gauge_spans(
        &theme,
        theme.heat_tone(2.0 / 3.0),
        &[(90_000, theme.heat_tone(2.0 / 3.0)), (0, write), (2, input)],
        f64::EPSILON,
        30,
    );
    assert_eq!(text(&flat), format!("╸{}", "─".repeat(29)));
    assert_eq!(flat[0].style.fg, Some(theme.heat_tone(2.0 / 3.0)));
    assert!(
        flat.iter().all(|span| span.style.fg != Some(input)),
        "the flat tail ignores a dropped segment's color"
    );
}

#[test]
fn context_gauge_collapsed_split_keeps_half_tail_in_health_tone() {
    let theme = Theme::fixed(false);
    let amount = 2.0 / 3.0;
    let health = theme.heat_tone(amount);
    let read = theme.component(Component::CacheRead);
    let input = theme.component(Component::Input);
    assert_ne!(health, read, "the fixture must expose a cache-read tail");
    assert_ne!(health, input, "the fixture must expose an input tail");

    let spans = context_gauge_spans(
        &theme,
        health,
        &[
            (202_000, health),
            (0, theme.component(Component::CacheWrite)),
            (994, input),
        ],
        78.56,
        30,
    );

    assert_eq!(
        text(&spans),
        format!("{}╸{}", "━".repeat(23), "─".repeat(6))
    );
    assert_eq!(spans[0].style.fg, Some(health));
    assert_eq!(spans[1].style.fg, Some(health));
    assert!(
        spans
            .iter()
            .all(|span| span.style.fg != Some(read) && span.style.fg != Some(input)),
        "collapsed component colors fold into the flat health run"
    );
}

#[test]
fn context_gauge_uses_the_first_surviving_segments_tone() {
    let theme = Theme::fixed(false);
    let health = theme.heat_tone(0.0);
    let write = theme.component(Component::CacheWrite);
    let input = theme.component(Component::Input);

    let write_only = context_gauge_spans(
        &theme,
        health,
        &[(0, health), (26_000, write), (0, input)],
        50.0,
        10,
    );
    assert_eq!(text(&write_only), "━━━━━─────");
    assert!(
        write_only
            .iter()
            .filter(|span| span.content.contains('━'))
            .all(|span| span.style.fg == Some(write)),
        "a cache-write-only fill stays violet instead of inheriting health"
    );

    let composed = context_gauge_spans(
        &theme,
        health,
        &[(0, health), (20_000, write), (6_000, input)],
        50.0,
        10,
    );
    let write_at = composed
        .iter()
        .position(|span| span.style.fg == Some(write))
        .expect("cache-write lead");
    let input_at = composed
        .iter()
        .position(|span| span.style.fg == Some(input))
        .expect("fresh-input accent");
    assert!(
        write_at < input_at,
        "cache-write leads fresh input in composition order"
    );
    assert!(
        composed
            .iter()
            .filter(|span| span.content.contains('━') || span.content == "╺")
            .all(|span| span.style.fg != Some(health)),
        "an absent cache-read segment contributes no health-colored fill"
    );
}

#[test]
fn mana_bar_drains_ramps_and_keeps_edge_shapes() {
    let zones = BudgetBarConfig::default();
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
    // A brimming window rests green; the bar warms continuously to red as it
    // drains, the zones (yellow 50, amber 25, red 10) landing on the ramp's warm
    // stops and the spans between them interpolating rather than snapping.
    assert_eq!(fg(100), lit.heat_tone(0.0), "full window is healthy green");
    assert_eq!(fg(50), lit.heat_tone(1.0 / 3.0), "yellow zone reaches warn");
    assert_eq!(
        fg(25),
        lit.heat_tone(2.0 / 3.0),
        "amber zone reaches caution"
    );
    assert_eq!(fg(10), lit.heat_tone(1.0), "red zone reaches alarm");
    assert_eq!(
        fg(1),
        lit.heat_tone(1.0),
        "a near-spent window stays alarm red"
    );
    assert_ne!(
        fg(75),
        lit.heat_tone(0.0),
        "a three-quarter window has left pure green"
    );
    let track = &mana_bar_spans(&lit, 70, 10, &zones)[1];
    assert_eq!(track.style, lit.muted());
    let spent = mana_bar_spans(&lit, 0, 10, &zones);
    assert_eq!(
        spent[0].style.fg,
        mana_style(&lit, 0, &zones).fg,
        "the spent track wears the label's own tone so the two still mirror"
    );
    assert_eq!(
        spent[0].style.fg,
        Some(lit.heat_tone(1.0)),
        "at the ramp's alarm-red endpoint"
    );
    assert_ne!(spent[0].style.fg, lit.muted().fg);
}

#[test]
fn spent_budget_track_mirrors_its_label_under_ansi_alarm_override() {
    // An ANSI alarm override (index < 16) flows to the flat `alarm()` slot but
    // not the ramp, which keeps the scheme RGB for that stop. The spent track
    // wears the bar's own `mana_style` tone, so it must follow the label down the
    // ramp rather than diverging to the terminal ANSI red.
    let theme_config = crate::config::ThemeConfig {
        alarm: Some(crate::config::ThemeColor::Indexed(1)),
        ..crate::config::ThemeConfig::default()
    };
    let theme = Theme::fixed_for_theme(false, &theme_config);
    let zones = BudgetBarConfig::default();
    let track = mana_bar_spans(&theme, 0, 10, &zones)[0].style.fg;
    assert_eq!(
        track,
        mana_style(&theme, 0, &zones).fg,
        "track mirrors its label"
    );
    assert_eq!(
        track,
        Some(theme.heat_tone(1.0)),
        "both wear the ramp's alarm endpoint"
    );
    assert_ne!(track, Some(Color::Indexed(1)), "not the terminal ANSI red");
}

#[test]
fn mana_style_honours_custom_and_misordered_zones() {
    let lit = Theme::fixed(false);
    let tone = |remaining, zones: &BudgetBarConfig| mana_style(&lit, remaining, zones).fg;
    let tuned = BudgetBarConfig {
        yellow: 80,
        amber: 40,
        red: 20,
        ..BudgetBarConfig::default()
    };
    // Custom zones move the warm stops: brimming green, warn at yellow, caution
    // at amber, alarm at red (and below).
    assert_eq!(
        tone(100, &tuned),
        Some(lit.heat_tone(0.0)),
        "healthy rests green"
    );
    assert_eq!(tone(80, &tuned), Some(lit.heat_tone(1.0 / 3.0)));
    assert_eq!(tone(40, &tuned), Some(lit.heat_tone(2.0 / 3.0)));
    assert_eq!(tone(19, &tuned), Some(lit.heat_tone(1.0)));
    assert_eq!(
        mana_bar_spans(&lit, 80, 10, &tuned)[0].style.fg,
        tone(80, &tuned)
    );

    // A misordered config degrades worst-first: a value under the (largest) red
    // threshold reds out, while a value clear of every threshold stays green-side.
    let misordered = BudgetBarConfig {
        yellow: 25,
        amber: 10,
        red: 50,
        ..BudgetBarConfig::default()
    };
    assert_eq!(
        tone(30, &misordered),
        Some(lit.heat_tone(1.0)),
        "below red reds out"
    );
    let clear = tone(50, &misordered).expect("tone");
    assert_ne!(clear, lit.heat_tone(1.0), "a clear window is not alarm");
    assert_ne!(clear, lit.heat_tone(2.0 / 3.0), "nor is it caution");
}

#[test]
fn pace_reading_reads_burn_and_raw_elapsed_window_edges() {
    let secs = SignedDuration::from_secs;
    let reading = |used, duration, until_reset| {
        pace_reading(used, secs(duration), secs(until_reset)).expect("pace reading")
    };
    let assert_close = |actual: f64, expected: f64| {
        assert!(
            (actual - expected).abs() < 0.000_1,
            "expected {expected}, got {actual}"
        );
    };

    let five_hour = reading(50, 5 * 3_600, 4 * 3_600);
    assert_close(five_hour.ratio, 2.5);
    assert_close(five_hour.elapsed_share, 0.2);
    let seven_day = reading(50, 7 * 86_400, 6 * 86_400);
    assert_close(seven_day.ratio, 3.5);
    assert_close(seven_day.elapsed_share, 1.0 / 7.0);
    assert_close(reading(20, 5 * 3_600, 4 * 3_600).ratio, 1.0);
    assert_close(reading(0, 5 * 3_600, 4 * 3_600).ratio, 0.0);
    let floored = reading(10, 5 * 3_600, 5 * 3_600 - 60);
    assert_close(floored.ratio, 2.0);
    assert_close(floored.elapsed_share, 1.0 / 300.0);

    assert_eq!(pace_reading(50, secs(0), secs(0)), None);
    assert_eq!(pace_reading(50, secs(5 * 3_600), secs(5 * 3_600)), None);
    assert_eq!(
        pace_reading(50, secs(5 * 3_600), secs(5 * 3_600 + 60)),
        None
    );
    let overdue = pace_reading(40, secs(5 * 3_600), secs(-3_600)).expect("overdue pace");
    assert_close(overdue.ratio, 0.4);
    assert_close(overdue.elapsed_share, 1.0);
}

#[test]
fn pace_style_floors_then_climbs_the_warm_tail() {
    let lit = Theme::fixed(false);
    let defaults = BudgetBurnRateConfig::default();
    let reading = |ratio| PaceReading {
        ratio,
        elapsed_share: 1.0,
    };
    let fg = |ratio| pace_style(&lit, reading(ratio), &defaults).fg;
    // Sustainable pace rests at the soft tier; the warm tail starts only past the
    // yellow threshold, reaching caution at amber and alarm at red (and beyond).
    assert_eq!(
        pace_style(&lit, reading(1.0), &defaults),
        lit.body(),
        "even pace rests soft"
    );
    assert!(fg(1.01).is_some(), "any overburn leaves the floor");
    assert_eq!(
        fg(1.5),
        Some(lit.warm_heat_tone(0.5)),
        "amber stop is caution"
    );
    assert_eq!(fg(2.0), Some(lit.warm_heat_tone(1.0)), "red stop is alarm");
    assert_eq!(
        fg(2.01),
        Some(lit.warm_heat_tone(1.0)),
        "beyond red stays alarm"
    );

    let tuned = BudgetBurnRateConfig {
        yellow: 80,
        amber: 120,
        red: 160,
        ..defaults
    };
    let fg_tuned = |ratio| pace_style(&lit, reading(ratio), &tuned).fg;
    assert_eq!(
        pace_style(&lit, reading(0.8), &tuned),
        lit.body(),
        "yellow stop still rests"
    );
    assert_eq!(fg_tuned(1.2), Some(lit.warm_heat_tone(0.5)));
    assert_eq!(fg_tuned(1.6), Some(lit.warm_heat_tone(1.0)));

    // A misordered config degrades worst-first: a value past the (smallest) red
    // threshold reds out, while a calm value still rests.
    let misordered = BudgetBurnRateConfig {
        yellow: 200,
        amber: 150,
        red: 100,
        ..defaults
    };
    assert_eq!(
        pace_style(&lit, reading(1.2), &misordered).fg,
        Some(lit.warm_heat_tone(1.0))
    );
    assert_eq!(pace_style(&lit, reading(0.9), &misordered), lit.body());

    // NO_COLOR drops the hue; the marker recedes to the soft tier like its
    // countdown, and the shape carries the pace.
    let plain = Theme::fixed(true);
    assert_eq!(pace_style(&plain, reading(2.5), &defaults), plain.body());
}

#[test]
fn pace_style_greens_deep_underspend_after_the_early_window_gate() {
    let lit = Theme::fixed(false);
    let defaults = BudgetBurnRateConfig::default();
    let reading = |ratio, elapsed_share| PaceReading {
        ratio,
        elapsed_share,
    };
    let fg = |ratio| pace_style(&lit, reading(ratio, 0.4), &defaults).fg;

    assert_eq!(pace_style(&lit, reading(1.0, 1.0), &defaults), lit.body());
    assert_eq!(pace_style(&lit, reading(0.67, 1.0), &defaults), lit.body());
    assert_eq!(
        fg(0.66),
        Some(lit.calm_tone(1.0 / 34.0)),
        "the cool tail starts just below green"
    );
    assert_eq!(fg(0.33), Some(lit.calm_tone(1.0)));
    assert_eq!(fg(0.0), Some(lit.calm_tone(1.0)));

    for ratio in [0.0, 0.2, 0.33, 0.66] {
        assert_eq!(
            pace_style(&lit, reading(ratio, 0.399), &defaults),
            lit.body(),
            "fresh window suppresses a {ratio}x cool signal"
        );
    }

    let misordered = BudgetBurnRateConfig {
        green: 20,
        deep_green: 80,
        ..defaults
    };
    assert_eq!(
        pace_style(&lit, reading(0.5, 1.0), &misordered).fg,
        Some(lit.calm_tone(1.0)),
        "misordered cool stops degrade greenest-first"
    );

    let plain = Theme::fixed(true);
    assert_eq!(
        pace_style(&plain, reading(0.2, 1.0), &defaults),
        plain.body()
    );
}

#[test]
fn no_color_shape_contracts_keep_budget_and_diff_readable() {
    let plain = Theme::fixed(true);
    let spans = infinite_bar_spans(&plain, Color::Indexed(208), 8);
    assert_eq!(text(&spans), "▱▱▱▱▱▱▱▱");
    assert_no_fg(&spans);

    let lit = Theme::fixed(false);
    let spans = infinite_bar_spans(&lit, Color::Indexed(208), 8);
    assert_eq!(spans[0].style.fg, Some(Color::Indexed(208)));

    let spans = diff_spans(&plain, 127, 43);
    assert_eq!(text(&spans), "+127 -43");
    assert_no_fg(&spans);
}

#[test]
fn token_breakdown_keeps_shape_and_marker_styles() {
    let plain = Theme::fixed(true);
    let spans = token_breakdown_spans(
        &plain,
        76_000,
        12_000,
        64_000,
        68_000,
        fmt::tokens_int,
        TokenDetail::Full,
        &TokenColumns::default(),
    );
    assert_eq!(text(&spans), "◇ 76k ↘ 12k ↗ 64k ◌ 68k");

    let lit = Theme::fixed(false);
    let spans = token_breakdown_spans(
        &lit,
        76_000,
        12_000,
        64_000,
        68_000,
        fmt::tokens_int,
        TokenDetail::Full,
        &TokenColumns::default(),
    );
    let marker = |glyph: &str| {
        spans
            .iter()
            .find(|span| span.content.contains(glyph))
            .unwrap_or_else(|| panic!("missing {glyph}"))
            .style
    };
    assert_eq!(
        marker(lit.glyph(GlyphRole::TokensTotal)).fg,
        Some(lit.component(Component::TokenTotal))
    );
    assert_eq!(
        marker(lit.glyph(GlyphRole::TokensInput)).fg,
        Some(lit.component(Component::Input))
    );
    assert_eq!(
        marker(lit.glyph(GlyphRole::TokensOutput)).fg,
        Some(lit.component(Component::Output))
    );
    assert_eq!(
        marker(lit.glyph(GlyphRole::TokensCacheRead)).fg,
        Some(lit.component(Component::CacheRead))
    );
    for span in spans.iter().filter(|span| {
        span.content
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c.is_ascii_whitespace())
    }) {
        assert_eq!(span.style, lit.body(), "figure {:?}", span.content);
    }
}

#[test]
fn nerd_font_glyph_set_reaches_token_and_meter_labels() {
    let theme = Theme::fixed_for_theme(
        true,
        &crate::config::ThemeConfig {
            glyphs: crate::config::ThemeGlyphsConfig {
                set: Some("nerd_font".to_owned()),
                ..crate::config::ThemeGlyphsConfig::default()
            },
            ..crate::config::ThemeConfig::default()
        },
    );

    let spans = token_breakdown_spans(
        &theme,
        76_000,
        12_000,
        64_000,
        68_000,
        fmt::tokens_int,
        TokenDetail::Full,
        &TokenColumns::default(),
    );
    assert_eq!(
        text(&spans),
        "\u{ed58} 76k \u{f103} 12k \u{f102} 64k \u{f1978} 68k"
    );

    let spans = context_gauge_spans(&theme, theme.heat_tone(0.5), &[], 50.0, 4);
    assert_eq!(text(&spans), "━━──", "drawn meter bars stay Unicode");

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
    assert_eq!(
        text(&spans),
        "\u{f0fe6} 76k · \u{f1978} 68k \u{f1c0} 6k \u{f103} 1k \u{f102} 2k"
    );
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
        theme.heat_tone(0.5),
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
    // The `▤` head wears the bar's severity tip; each composition marker legends
    // the bar in its own segment tone — cache-read green, cache-write
    // compaction violet, fresh input the expense vermilion, output accent.
    assert_eq!(
        tone(theme.glyph(GlyphRole::TokensFilled)),
        Some(theme.heat_tone(0.5)),
        "severity"
    );
    assert_eq!(
        tone(theme.glyph(GlyphRole::TokensCacheRead)),
        Some(theme.component(Component::CacheRead)),
        "cache read"
    );
    assert_eq!(
        tone(theme.glyph(GlyphRole::TokensCacheWrite)),
        Some(theme.component(Component::CacheWrite)),
        "cache write"
    );
    assert_eq!(
        tone(theme.glyph(GlyphRole::TokensInput)),
        Some(theme.component(Component::Input)),
        "fresh input"
    );
    assert_eq!(
        tone(theme.glyph(GlyphRole::TokensOutput)),
        Some(theme.component(Component::Output)),
        "output"
    );
    // Every figure reads dim — only the markers carry tones — and the `·`
    // seam shares the same dim gray chrome.
    for span in spans.iter().filter(|s| s.content.starts_with(' ')) {
        if span.content.trim().is_empty() || span.content.trim() == "·" {
            continue;
        }
        assert_eq!(span.style, theme.muted(), "figure {:?}", span.content);
    }
    let seam = spans
        .iter()
        .find(|s| s.content.trim() == "·")
        .expect("no seam span");
    assert_eq!(seam.style, theme.muted(), "the seam stays dim chrome");

    let spans = context_compaction_spans(&theme, 2);
    assert_eq!(text(&spans), " · ↻ 2");
    assert_eq!(spans[0].style, theme.muted(), "seam");
    assert_eq!(spans[1].style, compacting_style(&theme), "marker");
    assert_eq!(spans[2].style, theme.muted(), "count");
    assert!(context_compaction_spans(&theme, 0).is_empty());
}

#[test]
fn tool_repeat_marker_starts_at_the_warning_threshold() {
    let theme = Theme::fixed(false);
    let repeat = crate::agent_activity::ToolRepeat {
        digest: "digest".to_owned(),
        tool: "Bash".to_owned(),
        count: 3,
        since: jiff::Timestamp::from_second(1_700_000_000).unwrap(),
    };

    assert!(context_tool_repeat_spans(&theme, AgentStatus::Running, None, 3).is_empty());
    assert!(
        context_tool_repeat_spans(
            &theme,
            AgentStatus::Running,
            Some(&crate::agent_activity::ToolRepeat {
                count: 2,
                ..repeat.clone()
            }),
            3,
        )
        .is_empty()
    );
    assert!(
        context_tool_repeat_spans(&theme, AgentStatus::Failed, Some(&repeat), 3).is_empty(),
        "a resting card does not keep the running-loop annotation"
    );
    let spans = context_tool_repeat_spans(&theme, AgentStatus::Running, Some(&repeat), 3);
    assert_eq!(text(&spans), " · ⟲ 3");
    assert_eq!(spans[0].style, theme.muted(), "seam");
    assert_eq!(spans[1].style, theme.warn(Modifier::empty()), "marker");
    assert_eq!(spans[2].style, theme.muted(), "count");
}
