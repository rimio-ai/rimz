use super::*;
use crate::sidebar_pane::pixel::meter::{MeterPixels, MeterRaster};
use crate::sidebar_pane::render::layout::{spans_width, text_width};

/// The columns of a line's text bar cells and of its image placeholders.
fn bar_cells(line: &Line<'_>) -> (Vec<usize>, Vec<(usize, u32)>) {
    let (mut glyphs, mut placeholders, mut column) = (Vec::new(), Vec::new(), 0);
    for span in &line.spans {
        if span.content.contains('\u{10eeee}') {
            let Some(Color::Rgb(red, green, blue)) = span.style.fg else {
                panic!("a placeholder carries its image id");
            };
            placeholders.push((column, u32::from_be_bytes([0, red, green, blue])));
            column += span.width();
            continue;
        }
        for ch in span.content.chars() {
            if matches!(ch, '▰' | '▱' | '╱' | '╎') {
                glyphs.push(column);
            }
            column += text_width(&ch.to_string());
        }
    }
    (glyphs, placeholders)
}

fn placeholder_count(lines: &[Line<'_>]) -> usize {
    lines.iter().map(|line| bar_cells(line).1.len()).sum()
}

/// One provider for each row kind that draws a bar.
fn row_kinds() -> Vec<(&'static str, SidebarProviderPanel)> {
    let now = fixed_now();
    let windows = provider_panel("claude", "Claude", 173, true, false, Some((30, 60)));

    let mut quota = provider_panel("plugin", "Plugin", 140, true, false, None);
    quota.windows = vec![RateLimitWindow {
        scope: Some(crate::agents::RateLimitWindowScope {
            id: "deployments".to_owned(),
            label: "dep".to_owned(),
        }),
        used_percentage: Some(3),
        resets_at: Some(now + Duration::from_secs(4 * 3_600)),
        ..Default::default()
    }];

    let mut extra = provider_panel("claude", "Claude", 173, true, false, Some((100, 40)));
    extra.extra_credits = Some(crate::agents::ExtraCredits::known(
        Some(45.0),
        None,
        Some(50.0),
    ));

    let mut api = provider_panel("codex", "Codex", 33, false, false, None);
    api.extra_credits = Some(crate::agents::ExtraCredits::known(
        Some(12.0),
        None,
        Some(25.0),
    ));

    let mut unknown = provider_panel("claude", "Claude", 173, true, false, None);
    unknown.window_placeholders = vec!["5h".to_owned(), "7d".to_owned()];

    let mut lifted = provider_panel("codex", "Codex", 33, true, false, None);
    lifted.windows = vec![RateLimitWindow {
        duration_mins: Some(5 * 60),
        lifted: true,
        ..Default::default()
    }];

    vec![
        ("time windows", windows),
        ("named quota", quota),
        ("ex", extra),
        ("api", api),
        ("unknown", unknown),
        ("lifted", lifted),
    ]
}

#[test]
fn pixel_budget_bars_take_the_text_bars_cells_on_every_row_kind() {
    let theme = truecolor_sidebar_theme();
    for (name, panel) in row_kinds() {
        let dashboard = Dashboard::stacked(&theme, std::slice::from_ref(&panel)).width(40);
        let text = dashboard.lines();
        let mut pixels = MeterPixels::new(0x120000);
        pixels.begin_frame();
        let drawn = dashboard.block_with(Some(&mut pixels)).lines;
        assert_eq!(drawn.len(), text.len(), "{name}: the same line count");
        let mut ids = std::collections::BTreeSet::new();
        let mut bars = 0;
        for (text_line, drawn_line) in text.iter().zip(&drawn) {
            let (glyphs, _) = bar_cells(text_line);
            let (left, placeholders) = bar_cells(drawn_line);
            assert!(left.is_empty(), "{name}: no text bar is left: {drawn_line}");
            assert_eq!(
                placeholders
                    .iter()
                    .map(|(column, _)| *column)
                    .collect::<Vec<_>>(),
                glyphs,
                "{name}: the pixel bar takes the text bar's cells"
            );
            assert_eq!(
                spans_width(&drawn_line.spans),
                spans_width(&text_line.spans),
                "{name}"
            );
            ids.extend(placeholders.iter().map(|(_, id)| *id));
            bars += usize::from(!glyphs.is_empty());
        }
        assert!(bars > 0, "{name} draws a bar");
        pixels.observe_visible(&drawn);
        let visible = pixels
            .visible_images()
            .map(|(id, _)| id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(visible, ids, "{name}: every bar image stays visible");
    }
}

#[test]
fn budget_bars_keep_the_text_bar_where_pixels_cannot_match_it() {
    let truecolor = truecolor_sidebar_theme();
    let panels = row_kinds()
        .into_iter()
        .map(|(_, panel)| panel)
        .collect::<Vec<_>>();
    let text = Dashboard::stacked(&truecolor, &panels).lines();
    assert_eq!(
        placeholder_count(&text),
        0,
        "a frame without pixels draws text"
    );

    let fresh = || {
        let mut pixels = MeterPixels::new(0x120000);
        pixels.begin_frame();
        pixels
    };
    let mut pixels = fresh();
    assert!(
        placeholder_count(
            &Dashboard::stacked(&truecolor, &panels)
                .block_with(Some(&mut pixels))
                .lines
        ) > 0
    );

    let indexed = Theme::fixed(false);
    let override_glyph = Theme::fixed_for_theme(
        false,
        &crate::config::ThemeConfig {
            mode: crate::config::ThemeMode::Truecolor,
            ..toml::from_str("[glyphs.unicode.meter]\nmana_ceiling = '|'")
                .expect("a mana glyph override")
        },
    );
    for (name, theme) in [("indexed", &indexed), ("override", &override_glyph)] {
        let mut pixels = fresh();
        let lines = Dashboard::stacked(theme, &panels)
            .block_with(Some(&mut pixels))
            .lines;
        assert_eq!(placeholder_count(&lines), 0, "{name} keeps the text bar");
        assert_eq!(
            line_texts(&lines),
            line_texts(&Dashboard::stacked(theme, &panels).lines()),
            "{name}"
        );
    }

    let mut full = fresh();
    for index in 0..crate::sidebar_pane::pixel::METER_ID_CAPACITY {
        full.intern(MeterRaster::new(
            2,
            0.5,
            [(index >> 8) as u8, index as u8, 1],
            Vec::new(),
            [4, 5, 6],
            None,
        ))
        .expect("a free id");
    }
    let lines = Dashboard::stacked(&truecolor, &panels)
        .block_with(Some(&mut full))
        .lines;
    assert_eq!(
        placeholder_count(&lines),
        0,
        "a full id table keeps the text bar"
    );
    assert_eq!(line_texts(&lines), line_texts(&text));
}
