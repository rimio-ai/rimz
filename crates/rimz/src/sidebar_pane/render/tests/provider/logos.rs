use std::collections::BTreeMap;
use std::time::Duration;

use super::*;
use crate::config::{ThemeConfig, ThemeMode, ThemeProviderStyle};
use crate::sidebar_pane::pixel::meter::MeterPixels;
use crate::sidebar_pane::pixel::placeholder_cluster;
use crate::theme::ProviderLogo;

const BASE: u32 = 0x520000;
const LOGOS: std::ops::Range<u32> = BASE + 0x80..BASE + 0xC0;

fn theme_with(providers: BTreeMap<String, ThemeProviderStyle>) -> Theme {
    Theme::fixed_for_theme(
        false,
        &ThemeConfig {
            mode: ThemeMode::Truecolor,
            providers,
            ..Default::default()
        },
    )
}

/// A panel whose brand resolves to truecolor, as a built-in's does.
fn branded(kind: &str, windows: Option<(u8, u8)>) -> SidebarProviderPanel {
    let mut panel = provider_panel(kind, "Claude", 173, true, false, windows);
    panel.color_rgb = Some((0xd9, 0x77, 0x57));
    panel
}

/// One panel per budget shape, from no bar to many.
fn shapes() -> Vec<(&'static str, SidebarProviderPanel)> {
    let mut undecided = branded("claude", None);
    undecided.undecided = true;
    let mut lapsed = branded("claude", None);
    lapsed.entitlement = crate::agents::Entitlement::Lapsed {
        since_ms: 1_700_000_000_000,
    };
    let mut api = branded("claude", None);
    api.metered = false;
    let mut one = branded("claude", Some((30, 60)));
    one.windows.truncate(1);
    let two = branded("claude", Some((30, 60)));
    let mut many = branded("claude", None);
    many.windows = ["dep", "bld", "run", "ci"]
        .into_iter()
        .map(|label| RateLimitWindow {
            scope: Some(crate::agents::RateLimitWindowScope {
                id: label.to_owned(),
                label: label.to_owned(),
            }),
            used_percentage: Some(20),
            resets_at: Some(fixed_now() + Duration::from_secs(3_600)),
            ..Default::default()
        })
        .collect();
    vec![
        ("undecided", undecided),
        ("lapsed", lapsed),
        ("api", api),
        ("one bar", one),
        ("two bars", two),
        ("many bars", many),
    ]
}

/// A cell aspect no [`logo_frame`] uses, so its marks are sized anew. Landed
/// marks are shared across tables, and a table that must see a mark still
/// loading asks for a size no earlier frame landed.
const UNLANDED_ASPECT: f32 = 1.75;

/// The dashboard drawn twice: once to request its marks, once they land.
fn logo_frame(dashboard: &Dashboard<'_>) -> (Vec<Line<'static>>, MeterPixels) {
    let mut pixels = MeterPixels::new(BASE);
    pixels.logos.prepare(&ThemeConfig::default(), 2.0);
    pixels.begin_frame();
    dashboard.block_with(Some(&mut pixels));
    assert!(pixels.logos.settle(Duration::from_secs(10)), "marks land");
    let lines = draw(dashboard, &mut pixels);
    (lines, pixels)
}

/// The pixel frame with no logo: a table never prepared requests no mark.
fn without_logos(dashboard: &Dashboard<'_>) -> Vec<Line<'static>> {
    draw(dashboard, &mut MeterPixels::new(BASE))
}

fn draw(dashboard: &Dashboard<'_>, pixels: &mut MeterPixels) -> Vec<Line<'static>> {
    pixels.begin_frame();
    let lines = dashboard.block_with(Some(pixels)).lines;
    pixels.observe_visible(&lines);
    lines
}

/// One string per cell: a placeholder span is one cell.
fn cells(line: &Line<'_>) -> Vec<(String, Option<u32>)> {
    let mut out = Vec::new();
    for span in &line.spans {
        if span.content.contains('\u{10eeee}') {
            let Some(Color::Rgb(red, green, blue)) = span.style.fg else {
                panic!("a placeholder carries its image id");
            };
            let id = u32::from_be_bytes([0, red, green, blue]);
            out.push((span.content.to_string(), Some(id)));
            continue;
        }
        out.extend(span.content.chars().map(|ch| (ch.to_string(), None)));
    }
    out
}

fn logo_id(cell: &(String, Option<u32>)) -> Option<u32> {
    cell.1.filter(|id| LOGOS.contains(id))
}

/// The frame as text, a logo cell drawn `▒` and another image's cell `P`.
fn frame_text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| {
            let text = cells(line)
                .iter()
                .map(|cell| match cell.1 {
                    Some(id) if LOGOS.contains(&id) => "▒".to_owned(),
                    Some(_) => "P".to_owned(),
                    None => cell.0.clone(),
                })
                .collect::<String>();
            format!("{}\n", text.trim_end())
        })
        .collect()
}

/// The text right of the art column, up to `end`.
fn detail_text(line: &Line<'_>, end: usize) -> String {
    let cells = cells(line);
    cells[10.min(cells.len())..end.min(cells.len())]
        .iter()
        .map(|cell| if cell.1.is_some() { "P" } else { &cell.0 })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

struct Logo {
    id: u32,
    first: usize,
    rows: usize,
}

/// Every logo in the frame, checking that each row is nine placeholder cells
/// at columns 0..9 carrying the logo's id and its row and column diacritics.
fn logos_in(lines: &[Line<'_>]) -> Vec<Logo> {
    let mut found: Vec<Logo> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let cells = cells(line);
        let Some(id) = cells.first().and_then(logo_id) else {
            assert!(
                cells.iter().all(|cell| logo_id(cell).is_none()),
                "a logo cell sits only in the art column: {line}"
            );
            continue;
        };
        let row = match found.last_mut() {
            Some(logo) if logo.id == id && logo.first + logo.rows == index => {
                logo.rows += 1;
                logo.rows - 1
            }
            _ => {
                found.push(Logo {
                    id,
                    first: index,
                    rows: 1,
                });
                0
            }
        };
        assert!(cells.len() >= 9, "a logo row fills the column: {line}");
        for (col, cell) in cells.iter().take(9).enumerate() {
            assert_eq!(cell.1, Some(id), "row {index} col {col}: {line}");
            assert_eq!(cell.0, placeholder_cluster(row as u16, col as u16));
        }
        assert!(
            cells
                .get(9)
                .is_none_or(|cell| cell.0 == " " && cell.1.is_none()),
            "the column's gap stays blank: {line}"
        );
    }
    found
}

/// The run of detail rows beside a logo: rows whose art column is blank or the
/// logo, with text to their right.
fn detail_rows(lines: &[Line<'_>], logo: &Logo, end: usize) -> (usize, usize) {
    let is_detail = |index: usize| {
        let cells = cells(&lines[index]);
        let column_clear = cells
            .iter()
            .take(10)
            .all(|cell| cell.0.trim().is_empty() || logo_id(cell) == Some(logo.id));
        column_clear && !detail_text(&lines[index], end).is_empty()
    };
    let seed = (logo.first..logo.first + logo.rows)
        .find(|index| is_detail(*index))
        .expect("a detail row beside the logo");
    let top = (0..seed)
        .rev()
        .take_while(|index| is_detail(*index))
        .last()
        .unwrap_or(seed);
    let rows = (top..lines.len())
        .take_while(|index| is_detail(*index))
        .count();
    (top, rows)
}

fn assert_centred(name: &str, lines: &[Line<'_>], logo: &Logo, end: usize) {
    let (top, rows) = detail_rows(lines, logo, end);
    let half_row = logo.rows == 4;
    assert!(logo.rows == 3 || half_row, "{name}: {} rows", logo.rows);
    let mark_top = 2 * logo.first + usize::from(half_row);
    assert_eq!(
        mark_top + 3,
        2 * top + rows,
        "{name}: the mark's middle, in half rows, meets the middle of {rows} detail rows from \
         row {top}"
    );
}

fn non_blank_details(lines: &[Line<'_>], end: usize) -> Vec<String> {
    lines
        .iter()
        .map(|line| detail_text(line, end))
        .filter(|text| !text.is_empty())
        .collect()
}

#[test]
fn logos_centre_on_the_detail_rows_at_every_width_and_budget_shape() {
    let theme = truecolor_sidebar_theme();
    let mut frames = String::new();
    let mut sides = std::collections::BTreeSet::new();
    for (mode, width) in [
        ("stacked wide", 54),
        ("stacked normal", 44),
        ("stacked narrow", 36),
        ("tabbed wide", 54),
        ("tabbed normal", 44),
        ("tabbed narrow", 36),
    ] {
        for (shape, panel) in shapes() {
            let name = format!("{mode} / {shape}");
            let panels = [panel];
            let dashboard = if mode.starts_with("stacked") {
                Dashboard::stacked(&theme, &panels)
            } else {
                Dashboard::tabbed(&theme, &panels)
            }
            .width(width);
            let text = without_logos(&dashboard);
            let (lines, pixels) = logo_frame(&dashboard);
            let logos = logos_in(&lines);
            assert_eq!(logos.len(), 1, "{name}:\n{}", frame_text(&lines));
            assert_centred(&name, &lines, &logos[0], width);
            assert_eq!(
                non_blank_details(&lines, width),
                non_blank_details(&text, width),
                "{name}: the details are today's"
            );
            assert!(lines.len() >= text.len(), "{name}: lines are only gained");
            for line in &lines {
                let art = cells(line)
                    .into_iter()
                    .take(9)
                    .filter(|cell| cell.1.is_none())
                    .map(|cell| cell.0)
                    .collect::<String>();
                assert!(
                    art.trim().is_empty() || !art.contains(['▐', '▛', '▜', '▝', '▘']),
                    "{name}: no emblem is drawn under the logo: {line}"
                );
            }
            sides.extend(
                pixels
                    .logos
                    .visible_images()
                    .map(|(_, _, mark)| (mark.width, mark.height)),
            );
            frames.push_str(&format!("── {name}\n{}", frame_text(&lines)));
        }
    }
    assert_eq!(sides.len(), 1, "one mark size everywhere: {sides:?}");
    assert_snapshot("provider_logo_frames", frames);
}

#[test]
fn every_way_out_of_the_logo_tier_leaves_todays_text_emblem() {
    let truecolor = truecolor_sidebar_theme();
    let panels = vec![branded("claude", Some((30, 60)))];
    let text = without_logos(&Dashboard::stacked(&truecolor, &panels));
    let (drawn, _) = logo_frame(&Dashboard::stacked(&truecolor, &panels));
    assert_eq!(logos_in(&drawn).len(), 1, "the control draws a logo");

    let indexed = Theme::fixed(false);
    let no_color = Theme::fixed(true);
    let user_art = theme_with(BTreeMap::from([(
        "claude".to_owned(),
        ThemeProviderStyle {
            ascii_art: Some("A".to_owned()),
            ..ThemeProviderStyle::default()
        },
    )]));
    let mana: ThemeConfig =
        toml::from_str("mode = 'truecolor'\n[glyphs.unicode.meter]\nmana_filled = '#'\n")
            .expect("a mana glyph override");
    let mana = Theme::fixed_for_theme(false, &mana);
    let mut indexed_brand = panels.clone();
    indexed_brand[0].color_rgb = None;
    let mut plugin = panels.clone();
    plugin[0].kind = "plugin".to_owned();
    for (name, theme, panels, width) in [
        ("indexed depth", &indexed, &panels, 54),
        ("NO_COLOR", &no_color, &panels, 54),
        ("indexed brand", &truecolor, &indexed_brand, 54),
        ("mana glyph override", &mana, &panels, 54),
        ("user ascii_art", &user_art, &panels, 54),
        ("plugin kind", &truecolor, &plugin, 54),
        ("too narrow", &truecolor, &panels, 33),
    ] {
        let dashboard = Dashboard::stacked(theme, panels).width(width);
        let (lines, _) = logo_frame(&dashboard);
        assert_eq!(
            lines,
            without_logos(&dashboard),
            "{name} keeps the text emblem"
        );
    }

    let mut loading = MeterPixels::new(BASE);
    loading
        .logos
        .prepare(&ThemeConfig::default(), UNLANDED_ASPECT);
    let dashboard = Dashboard::stacked(&truecolor, &panels);
    assert_eq!(draw(&dashboard, &mut loading), text, "a mark still loading");
    assert!(loading.logos.loading());

    let mut full = MeterPixels::new(BASE);
    full.logos.prepare(&ThemeConfig::default(), 2.0);
    let logo_box = crate::sidebar_pane::pixel::logo::LogoBox::centred_on(4);
    let sources = (0..64)
        .map(|_| ProviderLogo::Shipped(crate::agents::shipped_logo("pi").unwrap()))
        .zip(0u8..)
        .collect::<Vec<_>>();
    for (source, index) in &sources {
        full.logos
            .intern(source.clone(), [*index, 0, 0], 9, logo_box);
    }
    assert!(full.logos.settle(Duration::from_secs(10)));
    for (source, index) in &sources {
        assert!(
            full.logos
                .intern(source.clone(), [*index, 0, 0], 9, logo_box)
                .is_some()
        );
    }
    full.begin_frame();
    let lines = dashboard.block_with(Some(&mut full)).lines;
    assert_eq!(lines, text, "a full logo range");
}

#[test]
fn stacked_draws_a_logo_per_block_and_tabbed_only_the_active_one() {
    let theme = truecolor_sidebar_theme();
    let panels = ["claude", "codex", "grok"]
        .map(|kind| branded(kind, Some((30, 60))))
        .to_vec();
    let (stacked, _) = logo_frame(&Dashboard::stacked(&theme, &panels));
    let ids = logos_in(&stacked)
        .iter()
        .map(|logo| logo.id)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 3, "{}", frame_text(&stacked));
    let (tabbed, _) = logo_frame(&Dashboard::tabbed(&theme, &panels).active("codex"));
    assert_eq!(logos_in(&tabbed).len(), 1, "{}", frame_text(&tabbed));

    let mut crowded = MeterPixels::new(BASE);
    crowded
        .logos
        .prepare(&ThemeConfig::default(), UNLANDED_ASPECT);
    let logo_box = crate::sidebar_pane::pixel::logo::LogoBox::centred_on(4);
    let pi = ProviderLogo::Shipped(crate::agents::shipped_logo("pi").unwrap());
    for index in 0..63u8 {
        crowded.logos.intern(pi.clone(), [index, 0, 0], 9, logo_box);
    }
    let dashboard = Dashboard::stacked(&theme, &panels);
    crowded.begin_frame();
    dashboard.block_with(Some(&mut crowded));
    assert!(crowded.logos.settle(Duration::from_secs(10)));
    for index in 0..63u8 {
        crowded.logos.intern(pi.clone(), [index, 0, 0], 9, logo_box);
    }
    crowded.begin_frame();
    let lines = dashboard.block_with(Some(&mut crowded)).lines;
    assert_eq!(
        logos_in(&lines).len(),
        1,
        "the blocks past the last free id keep their emblems:\n{}",
        frame_text(&lines)
    );
    assert_eq!(
        frame_text(&lines).matches('▐').count(),
        2,
        "two text emblems remain"
    );
}

#[test]
fn a_user_logo_outranks_the_shipped_one_and_waits_on_the_next_tier() {
    let dir = tempfile::tempdir().expect("tempdir");
    let good = dir.path().join("mine.svg");
    std::fs::write(
        &good,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><rect width="1" height="1" fill="currentColor"/></svg>"#,
    )
    .unwrap();
    let broken = dir.path().join("broken.svg");
    std::fs::write(&broken, "<svg").unwrap();
    let styled = |logo: &std::path::Path, art: Option<&str>| {
        theme_with(BTreeMap::from([(
            "claude".to_owned(),
            ThemeProviderStyle {
                logo: Some(logo.display().to_string()),
                ascii_art: art.map(ToOwned::to_owned),
                ..ThemeProviderStyle::default()
            },
        )]))
    };
    let panels = vec![branded("claude", Some((30, 60)))];
    let shipped_theme = truecolor_sidebar_theme();
    let (_, mut pixels) = logo_frame(&Dashboard::stacked(&shipped_theme, &panels));
    let source = |pixels: &MeterPixels| {
        pixels
            .logos
            .visible_images()
            .map(|(_, image, _)| image.source().clone())
            .collect::<Vec<_>>()
    };

    let mine = styled(&good, None);
    let dashboard = Dashboard::stacked(&mine, &panels);
    draw(&dashboard, &mut pixels);
    assert!(
        matches!(source(&pixels)[..], [ProviderLogo::Shipped(_)]),
        "while the user's file loads, the shipped logo shows"
    );
    assert!(pixels.logos.settle(Duration::from_secs(10)));
    draw(&dashboard, &mut pixels);
    assert_eq!(
        source(&pixels),
        [ProviderLogo::File(good.display().to_string())]
    );

    let failing = styled(&broken, None);
    let dashboard = Dashboard::stacked(&failing, &panels);
    draw(&dashboard, &mut pixels);
    assert!(pixels.logos.settle(Duration::from_secs(10)));
    draw(&dashboard, &mut pixels);
    assert!(
        matches!(source(&pixels)[..], [ProviderLogo::Shipped(_)]),
        "a failing file draws as if unset"
    );

    let with_art = styled(&good, Some("A\nB"));
    let dashboard = Dashboard::stacked(&with_art, &panels);
    let mut fresh = MeterPixels::new(BASE);
    fresh
        .logos
        .prepare(&ThemeConfig::default(), UNLANDED_ASPECT);
    assert_eq!(
        draw(&dashboard, &mut fresh),
        without_logos(&dashboard),
        "while the file loads, the user's art shows, not the shipped logo"
    );
    assert!(fresh.logos.settle(Duration::from_secs(10)));
    draw(&dashboard, &mut fresh);
    assert_eq!(
        source(&fresh),
        [ProviderLogo::File(good.display().to_string())]
    );
}

#[test]
fn a_reserved_box_keeps_tall_user_art_inside_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let good = dir.path().join("mine.svg");
    std::fs::write(
        &good,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><rect width="1" height="1" fill="currentColor"/></svg>"#,
    )
    .unwrap();
    let art = ["111", "222", "333", "444", "555", "666"];
    let theme = theme_with(BTreeMap::from([(
        "claude".to_owned(),
        ThemeProviderStyle {
            logo: Some(good.display().to_string()),
            ascii_art: Some(art.join("\n")),
            ..ThemeProviderStyle::default()
        },
    )]));
    let mut panel = branded("claude", Some((30, 60)));
    panel.art = art.map(ToOwned::to_owned).to_vec();
    let panels = vec![panel];
    let dashboard = Dashboard::tabbed(&theme, &panels).folded_footer();
    let (drawn, _) = logo_frame(&dashboard);
    let logos = logos_in(&drawn);
    assert_eq!(logos.len(), 1, "{}", frame_text(&drawn));
    let logo_box = logos[0].first..logos[0].first + logos[0].rows;

    let reserved = dashboard
        .logo_reserve(crate::sidebar_pane::pixel::logo::LogoReserve::new(2.0))
        .block()
        .lines;

    assert_eq!(
        reserved.len(),
        drawn.len(),
        "the reserve keeps the logo's rows"
    );
    let art_column = |line: &Line<'_>| cells(line).into_iter().take(9).collect::<Vec<_>>();
    for (index, (reserved, drawn)) in reserved.iter().zip(&drawn).enumerate() {
        if !logo_box.contains(&index) {
            assert_eq!(
                art_column(reserved),
                art_column(drawn),
                "line {index} outside the box keeps its first nine cells:\n{}",
                frame_text(std::slice::from_ref(reserved))
            );
        }
    }
    let kept = reserved[logo_box]
        .iter()
        .map(|line| {
            art_column(line)
                .into_iter()
                .map(|cell| cell.0)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("|");
    assert!(
        kept.contains("666") && !kept.contains("111"),
        "the bottom rows of the art are kept: {kept}"
    );
}

#[test]
fn pet_mode_places_the_logo_in_the_active_block_beside_the_pet() {
    let theme = truecolor_sidebar_theme();
    let panels = vec![branded("claude", Some((30, 60))), branded("codex", None)];
    let pet = pixel_pet();
    let dashboard = Dashboard::pets(&theme, &panels).active("claude").pet(&pet);
    let text = without_logos(&dashboard);
    let (lines, _) = logo_frame(&dashboard);
    let logos = logos_in(&lines);
    assert_eq!(logos.len(), 1, "{}", frame_text(&lines));
    let pet_column = lines
        .iter()
        .filter_map(|line| cells(line).iter().position(|cell| cell.1 == Some(0x123456)))
        .min()
        .expect("the pet is drawn");
    assert_centred("pet", &lines, &logos[0], pet_column - 1);
    let pet_cells = |lines: &[Line<'_>]| {
        lines
            .iter()
            .flat_map(|line| cells(line))
            .filter(|cell| cell.1 == Some(0x123456))
            .count()
    };
    assert_eq!(pet_cells(&lines), pet_cells(&text), "the pet is whole");
    assert_snapshot("provider_logo_pet_frame", frame_text(&lines));
}
