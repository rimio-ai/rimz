use super::*;
use crate::sidebar_pane::render::labels::mana_style;

/// Every provider bar — `5h`, `7d` across blocks, and the API spend row —
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
                &crate::config::BudgetBarConfig::default(),
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
    assert!(
        lines.len() >= 5,
        "two metered providers + one api row: {lines:?}"
    );
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
        ..Default::default()
    }];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 1);
    let text = rows[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(
        text.contains("↻  4h00m"),
        "five-cell timers sit in the six-cell reset slot without a trailing gutter: {text:?}"
    );
    let (label_fg, glyph_fg, has_reset) = bar_row_facts(&rows[0]);
    assert!(has_reset, "started windows keep their reset countdown");
    assert_eq!(
        label_fg,
        Some(theme.heat_tone(1.0 / 3.0)),
        "50% remaining sits on the yellow zone, where the draining bar reaches warn"
    );
    assert_eq!(glyph_fg, label_fg, "the label still mirrors the bar");
    assert_eq!(
        reset_marker_fg(&rows[0]),
        theme.alarm(Modifier::empty()).fg,
        "2.5x pace colors only the reset marker"
    );
    assert_eq!(
        reset_time_style(&rows[0]),
        Some(theme.body()),
        "the reset time stays neutral"
    );

    let mut panel = provider_panel("claude", "Claude", 173, true, false, None);
    panel.windows = vec![RateLimitWindow {
        used_percentage: Some(50),
        resets_at: Some(now + Duration::from_secs(4 * 3_600)),
        duration_mins: Some(5 * 60),
        ..Default::default()
    }];

    let plain = Theme::fixed(true);
    let rows = metered_bar_rows(&plain, &panel);
    assert_eq!(rows.len(), 1);
    assert_eq!(reset_marker_style(&rows[0]), Some(plain.body()));
    assert_eq!(reset_time_style(&rows[0]), Some(plain.body()));

    let mut panel = provider_panel("claude", "Claude", 173, true, false, None);
    panel.windows = vec![
        RateLimitWindow {
            used_percentage: Some(0),
            resets_at: Some(now + Duration::from_secs(4 * 3_600)),
            duration_mins: Some(5 * 60),
            ..Default::default()
        },
        RateLimitWindow {
            used_percentage: Some(50),
            resets_at: Some(now + Duration::from_secs(6 * 86_400)),
            duration_mins: Some(7 * 24 * 60),
            ..Default::default()
        },
    ];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        reset_marker_style(&rows[0]),
        Some(theme.body()),
        "unused started 5h window keeps the marker soft like its countdown"
    );
    assert_eq!(
        reset_marker_fg(&rows[1]),
        theme.alarm(Modifier::empty()).fg,
        "half the 7d budget after one day burns at 3.5x pace"
    );

    let mut panel = provider_panel("claude", "Claude", 173, true, false, None);
    panel.windows = vec![RateLimitWindow {
        used_percentage: Some(50),
        resets_at: Some(now + Duration::from_secs(4 * 3_600)),
        duration_mins: None,
        ..Default::default()
    }];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 1);
    assert_eq!(reset_marker_fg(&rows[0]), theme.body().fg);
    assert_eq!(reset_time_style(&rows[0]), Some(theme.body()));

    let mut panel = provider_panel("claude", "Claude", 173, true, false, None);
    panel.extra_credits = Some(crate::agents::ExtraCredits::Disabled);
    panel.windows = vec![RateLimitWindow {
        used_percentage: Some(100),
        resets_at: Some(now + Duration::from_secs(150 * 60)),
        duration_mins: Some(5 * 60),
        ..Default::default()
    }];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 1);
    let (label_fg, _, has_reset) = bar_row_facts(&rows[0]);
    assert!(has_reset, "the spent window keeps its own countdown");
    assert_eq!(label_fg, theme.alarm(Modifier::empty()).fg);
    assert_eq!(
        reset_marker_fg(&rows[0]),
        theme.alarm(Modifier::empty()).fg,
        "spent exactly halfway through the window burns 2x — the red pace stop"
    );
    assert_eq!(reset_time_style(&rows[0]), Some(theme.body()));
}

#[test]
fn lifted_window_is_a_full_unlimited_row_aligned_with_live_windows() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
    let mut panel = provider_panel("codex", "Codex", 33, true, false, None);
    panel.windows = vec![
        RateLimitWindow {
            duration_mins: Some(5 * 60),
            lifted: true,
            ..Default::default()
        },
        RateLimitWindow {
            used_percentage: Some(40),
            resets_at: Some(now + Duration::from_secs(6 * 86_400)),
            duration_mins: Some(7 * 24 * 60),
            ..Default::default()
        },
    ];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 2);
    let texts: Vec<String> = rows
        .iter()
        .map(|row| row.spans.iter().map(|span| span.content.as_ref()).collect())
        .collect();
    assert!(texts[0].starts_with("5h "), "{texts:?}");
    assert_eq!(texts[0].chars().filter(|ch| *ch == '▰').count(), 17);
    assert_eq!(texts[0].chars().position(|ch| ch == '∞'), Some(22));
    assert!(texts[1].starts_with("7d "), "{texts:?}");
    assert!(
        !texts[1].contains('∞'),
        "the reported weekly window stays metered"
    );
    assert_eq!(texts[1].chars().position(|ch| ch == '↻'), Some(22));
    assert_eq!(
        bar_row_facts(&rows[0]).0,
        mana_style(&theme, 100, &Default::default()).fg,
        "the lifted label uses the full-mana tone"
    );
}

#[test]
fn lifted_window_infinity_stays_unicode_and_neutral_with_nerd_font() {
    let theme = Theme::fixed_for_theme(
        false,
        &crate::config::ThemeConfig {
            glyphs: crate::config::ThemeGlyphsConfig {
                set: Some("nerd_font".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let mut panel = provider_panel("codex", "Codex", 33, true, false, None);
    panel.windows = vec![RateLimitWindow {
        duration_mins: Some(5 * 60),
        lifted: true,
        ..Default::default()
    }];

    let rows = metered_bar_rows(&theme, &panel);
    let infinity = rows[0]
        .spans
        .iter()
        .find(|span| span.content == "∞")
        .expect("lifted window uses the Unicode infinity");
    assert_eq!(infinity.style, theme.body());
    assert_eq!(theme.glyph(GlyphRole::MeterUnlimited), "∞");
    assert_ne!(theme.glyph(GlyphRole::ChromeInfinity), "∞");
}

#[test]
fn spent_longer_window_gates_a_lifted_row_as_exhausted() {
    let theme = Theme::fixed(false);
    let mut panel = provider_panel("codex", "Codex", 33, true, false, None);
    panel.extra_credits = Some(crate::agents::ExtraCredits::Disabled);
    panel.windows = vec![
        RateLimitWindow {
            duration_mins: Some(5 * 60),
            lifted: true,
            ..Default::default()
        },
        RateLimitWindow {
            used_percentage: Some(100),
            duration_mins: Some(7 * 24 * 60),
            ..Default::default()
        },
    ];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 2);
    let text = rows[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(!text.contains('∞'), "a binding longer cap wins: {text:?}");
    assert!(!text.contains('▰'), "the gated bar has no remaining fill");
    let (label, track, has_reset) = bar_row_facts(&rows[0]);
    assert_eq!(label, theme.alarm(Modifier::empty()).fg);
    assert_eq!(track, label);
    assert!(!has_reset);
}

#[test]
fn provider_reset_marker_greens_only_mature_underspend() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
    let mut panel = provider_panel("claude", "Claude", 173, true, false, None);
    panel.windows = vec![
        RateLimitWindow {
            used_percentage: Some(10),
            resets_at: Some(now + Duration::from_secs(3 * 3_600)),
            duration_mins: Some(5 * 60),
            ..Default::default()
        },
        RateLimitWindow {
            used_percentage: Some(10),
            resets_at: Some(now + Duration::from_secs(4 * 3_600)),
            duration_mins: Some(5 * 60),
            ..Default::default()
        },
    ];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 2);
    let (label_fg, glyph_fg, has_reset) = bar_row_facts(&rows[0]);
    assert!(has_reset);
    assert_eq!(label_fg, mana_style(&theme, 90, &Default::default()).fg);
    assert_eq!(
        glyph_fg, label_fg,
        "the bar keeps its remaining-budget tone"
    );
    assert_eq!(
        reset_marker_fg(&rows[0]),
        Some(theme.calm_tone(1.0)),
        "0.25x pace after two hours reaches full green"
    );
    assert_eq!(reset_time_style(&rows[0]), Some(theme.body()));
    assert_eq!(
        reset_marker_style(&rows[1]),
        Some(theme.body()),
        "0.5x pace after one hour stays soft before the elapsed-share gate"
    );
    assert_eq!(reset_time_style(&rows[1]), Some(theme.body()));
}

#[test]
fn provider_bar_selection_surfaces_extra_usage_when_included_windows_are_spent() {
    let theme = Theme::fixed(false);
    let labels = |panel: &crate::SidebarProviderPanel| -> Vec<String> {
        metered_bar_rows(&theme, panel)
            .into_iter()
            .map(|line| {
                line.spans
                    .first()
                    .expect("label span")
                    .content
                    .trim()
                    .to_owned()
            })
            .collect()
    };

    let mut panel = provider_panel("claude", "Claude", 173, true, false, Some((25, 40)));
    assert_eq!(labels(&panel), vec!["5h", "7d"]);

    panel.windows[0].used_percentage = Some(100);
    panel.extra_credits = Some(crate::agents::ExtraCredits::known(
        Some(7.0),
        None,
        Some(50.0),
    ));
    assert_eq!(labels(&panel), vec!["5h", "ex"]);
    let row_text = metered_bar_rows(&theme, &panel)[1]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(row_text.contains("$7/$50"), "{row_text:?}");

    panel.extra_credits = Some(crate::agents::ExtraCredits::Disabled);
    assert_eq!(
        labels(&panel),
        vec!["5h", "7d"],
        "disabled extra usage falls back to the included-window pair"
    );

    panel.windows[0].used_percentage = Some(5);
    panel.windows[1].used_percentage = Some(100);
    panel.extra_credits = None;
    assert_eq!(
        labels(&panel),
        vec!["7d", "ex"],
        "a spent long cap makes the extra track relevant even when unknown"
    );

    panel.windows = vec![
        RateLimitWindow {
            used_percentage: Some(5),
            resets_at: None,
            duration_mins: Some(5 * 60),
            ..Default::default()
        },
        RateLimitWindow {
            used_percentage: Some(100),
            resets_at: None,
            duration_mins: Some(7 * 24 * 60),
            ..Default::default()
        },
        RateLimitWindow {
            used_percentage: Some(10),
            resets_at: None,
            duration_mins: Some(30 * 24 * 60),
            ..Default::default()
        },
    ];
    assert_eq!(
        labels(&panel),
        vec!["7d", "ex"],
        "a spent middle window stays visible even when a longer window exists"
    );
    panel.extra_credits = Some(crate::agents::ExtraCredits::Disabled);
    assert_eq!(
        labels(&panel),
        vec!["5h", "7d"],
        "disabled extra usage pairs the short window with the binding spent one"
    );
}

#[test]
fn named_quotas_remain_independent_and_bypass_temporal_substitution() {
    let theme = Theme::fixed(false);
    let now = fixed_now();
    let scope = |id: &str, label: &str| crate::agents::RateLimitWindowScope {
        id: id.to_owned(),
        label: label.to_owned(),
    };
    let mut panel = provider_panel("plugin", "Plugin", 140, true, false, None);
    panel.extra_credits = Some(crate::agents::ExtraCredits::known(
        None,
        Some(10.0),
        Some(10.0),
    ));
    panel.windows = vec![
        RateLimitWindow {
            scope: Some(scope("build_minutes", "bld")),
            used_percentage: Some(100),
            resets_at: Some(now + Duration::from_secs(4 * 3_600)),
            ..Default::default()
        },
        RateLimitWindow {
            scope: Some(scope("deployments", "dep")),
            used_percentage: Some(20),
            resets_at: Some(now + Duration::from_secs(4 * 3_600)),
            ..Default::default()
        },
    ];

    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 2, "both provider-defined quotas stay visible");
    let texts = rows
        .iter()
        .map(|row| {
            row.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(texts[0].trim_start().starts_with("bld"), "{:?}", texts);
    assert!(texts[1].trim_start().starts_with("dep"), "{:?}", texts);
    assert!(
        !texts[0].contains('▰'),
        "spent build quota has no fill: {:?}",
        texts
    );
    assert!(
        texts[1].contains('▰'),
        "spent build quota cannot paint deployments exhausted: {:?}",
        texts
    );
    assert!(texts.iter().all(|text| text.contains("↻  4h00m")));

    panel.windows[0].lifted = true;
    panel.windows[0].used_percentage = Some(0);
    panel.windows[0].resets_at = None;
    let rows = metered_bar_rows(&theme, &panel);
    let unlimited = rows[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(unlimited.starts_with("bld"));
    assert!(unlimited.contains('∞'));
}

#[test]
fn api_key_provider_shows_budget_left_or_unlimited_full_bar() {
    let theme = Theme::fixed(false);
    let mut panel = provider_panel("codex", "Codex", 33, false, false, None);
    panel.extra_credits = Some(crate::agents::ExtraCredits::known(
        Some(12.0),
        None,
        Some(25.0),
    ));
    let rows = metered_bar_rows(&theme, &panel);
    assert_eq!(rows.len(), 1);
    let text = rows[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.trim_start().starts_with("api"), "{text:?}");
    assert!(text.contains("$13"), "{text:?}");
    assert!(!text.contains("$12"), "{text:?}");
    assert!(!text.contains('/'), "{text:?}");
    assert!(
        text.contains('▰'),
        "known spend against a ceiling gets a filled remaining bar: {text:?}"
    );

    panel.extra_credits = Some(crate::agents::ExtraCredits::known(Some(12.0), None, None));
    let text = metered_bar_rows(&theme, &panel)[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.contains('∞'), "{text:?}");
    assert!(!text.contains('$'), "{text:?}");
    assert!(
        text.contains('▰') && !text.contains('▱'),
        "uncapped API usage paints a full unlimited bar: {text:?}"
    );
}

#[test]
fn antigravity_metered_unknown_and_quota_rows_never_render_as_api() {
    let theme = Theme::fixed(true);
    let mut panel = provider_panel("antigravity", "Antigravity", 33, true, false, None);
    let initial = metered_bar_rows(&theme, &panel);
    assert_eq!(initial.len(), 1);
    let initial_text = initial[0]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(
        !initial_text.trim_start().starts_with("api"),
        "{initial_text:?}"
    );

    panel.windows = vec![
        RateLimitWindow {
            used_percentage: Some(70),
            duration_mins: Some(300),
            ..Default::default()
        },
        RateLimitWindow {
            used_percentage: Some(60),
            duration_mins: Some(10_080),
            ..Default::default()
        },
    ];
    let rows = metered_bar_rows(&theme, &panel);
    let labels = rows
        .iter()
        .map(|row| row.spans.first().unwrap().content.trim().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["5h", "7d"]);
    assert!(rows.iter().all(|row| {
        !row.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .trim_start()
            .starts_with("api")
    }));
}

#[test]
fn provider_window_states_control_countdowns_and_empty_tracks() {
    let theme = Theme::fixed(false);
    let now = fixed_now();

    let mut panel = provider_panel("claude", "Claude", 173, true, false, Some((0, 100)));
    panel.extra_credits = Some(crate::agents::ExtraCredits::Disabled);
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
    assert_ne!(
        glyph_fg,
        theme.alarm(Modifier::empty()).fg,
        "unknown is not exhausted"
    );
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
    claude.windows.clear();
    let rows = metered_bar_rows(&theme, &claude);
    assert_eq!(
        rows.len(),
        1,
        "a metered account without reported windows still paints one placeholder row"
    );
    let label = rows[0]
        .spans
        .first()
        .expect("a label span")
        .content
        .trim()
        .to_owned();
    assert_eq!(label, "", "the no-window placeholder has no fake label");
    let (label_fg, glyph_fg, has_reset) = bar_row_facts(&rows[0]);
    assert_eq!(
        label_fg, glyph_fg,
        "placeholder label slot mirrors the track"
    );
    assert!(!has_reset, "placeholder rows have no reset countdown");
    assert!(
        !rows[0].spans.iter().any(|span| span.content.contains('▰')),
        "placeholder rows have no filled budget cells"
    );
    assert!(
        rows[0].spans.iter().any(|span| span.content.contains('▱')),
        "placeholder rows keep the unknown empty track"
    );

    let mut claude = provider_panel("claude", "Claude", 173, true, false, None);
    claude.windows = vec![RateLimitWindow {
        used_percentage: Some(1),
        resets_at: Some(now + Duration::from_secs(4 * 3_600)),
        duration_mins: Some(5 * 60),
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        !text.ends_with(' '),
        "reset countdowns end at the right edge without an extra gutter: {text:?}"
    );
    assert_eq!(
        text.chars().position(|ch| ch == '↻'),
        Some(22),
        "the reset marker stays at the left edge of the fixed reset slot: {text:?}"
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
        headline: crate::SpendWindow {
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
