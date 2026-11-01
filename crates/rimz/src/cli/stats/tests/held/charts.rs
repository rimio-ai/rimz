use super::*;

#[test]
fn dashboard_round_ticks_use_the_nearest_row_without_collisions() {
    let (mut stats, assists) = fixture();
    for (ceiling, expected) in [
        (1.5, vec![(4, "$1")]),
        (12.0, vec![(5, "$10"), (2, "$5")]),
        (1_500.0, vec![(6, "$1.5k"), (3, "$750")]),
        (2_500.0, vec![(5, "$2k"), (2, "$1k")]),
        (10_000.0, vec![(6, "$10k"), (3, "$5k")]),
    ] {
        stats.chart.hours =
            BTreeMap::from([(TODAY * 24, BTreeMap::from([("opus-5.5".into(), ceiling)]))]);
        let rows = plot(&stats, &assists, Window::Today, 119);
        let labels = rows
            .iter()
            .rev()
            .enumerate()
            .filter_map(|(row, line)| {
                let text = strip_ansi(line);
                let label = text.split_once('┤').unwrap().0.trim();
                assert!(label.len() <= 5, "{text}");
                (!label.is_empty()).then(|| (row, label.to_owned()))
            })
            .rev()
            .collect::<Vec<_>>();
        let expected = expected
            .into_iter()
            .map(|(row, label)| (row, label.to_owned()))
            .collect::<Vec<_>>();
        assert_eq!(labels, expected, "ceiling {ceiling}");
    }
    for height in [4, 2] {
        let output = (10..50)
            .map(|rows| frame(&stats, &assists, Window::Today, 119, Some(rows)))
            .find(|output| {
                output
                    .text
                    .lines()
                    .filter(|line| line.contains('┤'))
                    .count()
                    == height
            })
            .expect("short fitted chart");
        let labels = strip_ansi(&output.text)
            .lines()
            .filter_map(|line| line.split_once('┤').map(|(label, _)| label.trim()))
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(labels, ["$10k", "$5k"], "height {height}");
    }
}

#[test]
fn dashboard_averages_replace_ticks_on_the_axis_even_without_color() {
    let (mut stats, assists) = fixture();
    for (tab, count, peak, mean, label, row) in [
        (Window::Week, 7, 1_400.0, 852.0, "$852", 3),
        (Window::Month, 30, 2_500.0, 876.0, "$876", 5),
        (Window::Year, 52, 10_000.0, 1_899.0, "$1.9k", 6),
    ] {
        let cells = (0..count)
            .map(|ago| {
                let usd = if ago == 0 {
                    peak
                } else {
                    (mean * count as f64 - peak) / (count - 1) as f64
                };
                (
                    TODAY - ago * if tab == Window::Year { 7 } else { 1 },
                    BTreeMap::from([("opus-5.5".into(), usd)]),
                )
            })
            .collect();
        if tab == Window::Year {
            stats.chart.weeks = cells;
        } else {
            stats.chart.days = cells;
        }
        let output = strip_ansi(
            &Dashboard {
                stats: &stats,
                assists: &assists,
                today: TODAY,
                dollars: false,
                glyphs: &panel_glyphs(),
                hold: false,
                color: false,
            }
            .frame(tab, 119, None)
            .text,
        );
        let rows = output
            .lines()
            .filter(|line| line.contains(['┤', '┼']))
            .collect::<Vec<_>>();
        assert_eq!(
            rows.iter().filter(|line| line.contains('┼')).count(),
            1,
            "{output}"
        );
        assert_eq!(
            rows[row].split_once('┼').unwrap().0.trim(),
            label,
            "{output}"
        );
        assert!(!output.contains("avg"), "{output}");
        if tab == Window::Week {
            assert!(!rows.iter().any(|line| line.contains("$750")), "{output}");
        }
    }
}

#[test]
fn dashboard_chart_and_heatmap_titles_have_one_empty_line() {
    let (stats, assists) = fixture();
    for tab in Window::TABS {
        let output = strip_ansi(&frame(&stats, &assists, tab, 119, None).text);
        let lines = output.lines().collect::<Vec<_>>();
        for title in ["Spend per", "Token activity"] {
            let start = lines.iter().position(|line| line.contains(title)).unwrap();
            assert_eq!(lines[start + 1], "", "{tab:?}: {title}");
            assert!(!lines[start + 2].is_empty(), "{tab:?}: {title}");
        }
        assert!(
            lines
                .iter()
                .all(|line| line.is_empty() || !line.trim().is_empty()),
            "{output}"
        );
    }
}

fn plot(stats: &Stats, assists: &TabAssists, tab: Window, width: usize) -> Vec<String> {
    frame(stats, assists, tab, width, None)
        .text
        .lines()
        .filter(|line| strip_ansi(line).contains(['┤', '┼']))
        .map(str::to_owned)
        .collect()
}

fn forecast_fixture(spent: f64, forecast: f64, peak: f64) -> (Stats, TabAssists) {
    let (mut stats, assists) = fixture();
    stats.by_day = BTreeMap::from([(TODAY, day(1, spent))]);
    stats.total.day.usd = spent;
    let cells = |usd| BTreeMap::from([("opus-5.5".into(), usd)]);
    stats.chart.days = BTreeMap::from([(TODAY - 1, cells(peak)), (TODAY, cells(spent))]);
    stats.chart.weeks = BTreeMap::from([(TODAY - 7, cells(peak)), (TODAY, cells(spent))]);
    stats.chart.hours = BTreeMap::from([
        (TODAY * 24, cells(spent)),
        ((TODAY - 1) * 24 + 23, cells((forecast - spent) * 7.0)),
    ]);
    (stats, assists)
}

#[test]
fn dashboard_chart_titles_keep_the_comparison_figures_at_narrow_widths() {
    let (mut stats, assists) = fixture();
    for width in [60, 85, 119] {
        for (tab, comparison) in [
            (Window::Today, "~$1,265 by midnight · █ yesterday $1,293"),
            (Window::Week, "↓11% vs prior 7 days"),
            (Window::Month, "↑186% vs prior 30 days"),
        ] {
            let frame = frame(&stats, &assists, tab, width, None);
            let output = strip_ansi(&frame.text);
            let title = output
                .lines()
                .find(|line| line.contains("Spend per"))
                .unwrap();
            assert!(title.ends_with(comparison), "{width} {tab:?}: {title}");
            if tab == Window::Today {
                assert_eq!(title.contains("today $1,024 so far · "), width == 119);
                let raw_title = frame
                    .text
                    .lines()
                    .find(|line| line.contains("Spend per"))
                    .unwrap();
                assert!(display_width(raw_title) <= width, "{width}: {raw_title:?}");
                let mut actual = vt100::Parser::new(
                    1.try_into().unwrap(),
                    (width as u16).try_into().unwrap(),
                    0,
                );
                actual.process(raw_title.as_bytes());
                let comparison = render::palette::model_styles(&[], |_, _| false).1.forecast;
                let mut expected =
                    vt100::Parser::new(1.try_into().unwrap(), 2.try_into().unwrap(), 0);
                expected.process(
                    format!(
                        "{}{}",
                        render::paint(comparison, "█"),
                        render::paint(render::palette::muted(), "x")
                    )
                    .as_bytes(),
                );
                let start = title.chars().position(|c| c == '~').unwrap();
                for (column, glyph) in title.chars().enumerate().skip(start) {
                    assert_eq!(
                        actual.screen().cell(0, column as u16).unwrap().fgcolor(),
                        expected
                            .screen()
                            .cell(0, u16::from(glyph != '█'))
                            .unwrap()
                            .fgcolor(),
                        "{width}: {title} at {column}"
                    );
                }
            }
        }
    }
    stats.chart.weeks = BTreeMap::from([(
        TODAY,
        BTreeMap::from([("opus-5.5".into(), 52_000_000_000_000_000.0)]),
    )]);
    let output = strip_ansi(&frame(&stats, &assists, Window::Year, 60, None).text);
    let title = output
        .lines()
        .find(|line| line.contains("Spend per"))
        .unwrap();
    assert_eq!(title.trim_end(), "Spend per week · last 52 weeks");
}

#[test]
fn dashboard_plot_has_seven_rows_and_each_ranges_bar_count() {
    let (mut stats, assists) = fixture();
    stats.chart.weeks = (0..52)
        .map(|ago| {
            (
                TODAY - ago * 7,
                BTreeMap::from([("opus-5.5".into(), 100.0)]),
            )
        })
        .collect();
    let sizes = [62, 119].map(|width| {
        Window::TABS.map(|tab| {
            let rows = plot(&stats, &assists, tab, width);
            let bottom = strip_ansi(rows.last().unwrap());
            let bars: Vec<_> = bottom
                .split(['┤', '┼'])
                .nth(1)
                .unwrap()
                .split_whitespace()
                .collect();
            let count = if bars.len() == 1 {
                bars[0].chars().count()
            } else {
                bars.len()
            };
            (rows.len(), count)
        })
    });
    assert_eq!(sizes, [[(7, 24), (7, 7), (7, 30), (7, 52)]; 2]);
}

#[test]
fn dashboard_bar_tops_use_all_eight_blocks_and_keep_trace_spend() {
    let (mut stats, assists) = fixture();
    stats.chart.hours.clear();
    for hour in 0..8 {
        stats.chart.hours.insert(
            TODAY * 24 + hour,
            BTreeMap::from([("opus-5.5".into(), (hour + 1) as f64 / 56.0)]),
        );
    }
    stats
        .chart
        .hours
        .insert(TODAY * 24 + 8, BTreeMap::from([("opus-5.5".into(), 1.0)]));
    stats.chart.hours.insert(
        TODAY * 24 + 9,
        BTreeMap::from([("opus-5.5".into(), 0.0001)]),
    );
    let rows = plot(&stats, &assists, Window::Today, 119);
    let bottom = strip_ansi(rows.last().unwrap());
    let tops: Vec<_> = bottom
        .split('┤')
        .nth(1)
        .unwrap()
        .split_whitespace()
        .map(|bar| bar.chars().next().unwrap())
        .collect();
    assert_eq!(tops, ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█', '█', '▁']);
}

#[test]
fn dashboard_ceiling_keeps_the_fine_ladder_for_bar_heights() {
    let (mut stats, assists) = fixture();
    stats.chart.hours.clear();
    let tops: Vec<_> = [1.0, 1.1, 1.3, 1.7, 2.1, 2.7, 3.2, 4.2, 5.2, 6.2, 8.1]
        .into_iter()
        .map(|usd| {
            stats
                .chart
                .hours
                .insert(TODAY * 24, BTreeMap::from([("opus-5.5".into(), usd)]));
            let rows = plot(&stats, &assists, Window::Today, 119);
            rows.iter()
                .rev()
                .enumerate()
                .filter_map(|(row, line)| {
                    strip_ansi(line)
                        .split_once('┤')
                        .unwrap()
                        .1
                        .trim()
                        .chars()
                        .next()
                        .map(|glyph| (row, glyph))
                })
                .next_back()
                .unwrap()
        })
        .collect();
    assert_eq!(
        tops,
        [
            (6, '█'),
            (6, '▃'),
            (6, '▁'),
            (5, '█'),
            (5, '▇'),
            (6, '▂'),
            (5, '▅'),
            (5, '▇'),
            (6, '▁'),
            (5, '▃'),
            (5, '▅')
        ]
    );
}

#[test]
fn dashboard_forecast_uses_the_publication_hour_and_zero_fills_missing_days() {
    let (mut stats, assists) = fixture();
    stats.total.day.usd = 10.0;
    stats.refreshed_at_ms = (TODAY as u64 * 86_400 + 23 * 3_600) * 1_000;
    stats.chart.hours.clear();
    for ago in 1..=7 {
        for (hour, usd) in [
            (11, 700.0),
            (15, 70.0),
            (16, ago as f64),
            (23, ago as f64 * 2.0),
        ] {
            stats.chart.hours.insert(
                (TODAY - ago) * 24 + hour,
                BTreeMap::from([("opus-5.5".into(), usd)]),
            );
        }
    }
    let headers: Vec<_> = [("UTC", 15, "~$22"), ("America/New_York", 11, "~$92")]
        .into_iter()
        .map(|(zone, hour, expected)| {
            stats.timezone = zone.into();
            stats.chart.hour = hour;
            let output = strip_ansi(&frame(&stats, &assists, Window::Today, 119, None).text);
            output.contains(&format!("today $10 so far · {expected} by midnight"))
        })
        .collect();
    assert_eq!(headers, [true, true]);
    stats.chart.hours.retain(|key, _| *key >= (TODAY - 1) * 24);
    stats.chart.hours.insert(
        (TODAY - 1) * 24 + 23,
        BTreeMap::from([("opus-5.5".into(), 20.0)]),
    );
    stats.timezone = "UTC".into();
    stats.chart.hour = 15;
    let output = strip_ansi(&frame(&stats, &assists, Window::Today, 119, None).text);
    assert!(output.contains("~$13 by midnight"), "{output}");
    stats.chart.hours.clear();
    let output = strip_ansi(&frame(&stats, &assists, Window::Today, 119, None).text);
    assert!(output.contains("~$10 by midnight"), "{output}");
}

#[test]
fn dashboard_forecast_extends_only_the_bar_holding_today_in_its_model_tone() {
    let (mut stats, assists) = forecast_fixture(1.0, 4.0, 0.0);
    let forecast_style = render::palette::model_styles(&["claude"], |_, _| true).0[0].forecast;
    let projected = Window::TABS.map(|tab| {
        let rows = plot(&stats, &assists, tab, 119);
        rows.iter()
            .any(|row| row.contains(&forecast_style.to_string()))
    });
    assert_eq!(projected, [false, true, true, true]);
    let rounded_away = [0.0001, 0.0].map(|remaining| {
        stats.chart.hours.insert(
            (TODAY - 1) * 24 + 23,
            BTreeMap::from([("opus-5.5".into(), remaining)]),
        );
        Window::TABS.map(|tab| {
            plot(&stats, &assists, tab, 119)
                .iter()
                .any(|row| row.contains(&forecast_style.to_string()))
        })
    });
    assert_eq!(rounded_away, [[false; 4]; 2]);
}

#[test]
fn dashboard_forecast_preserves_spent_eighths_over_its_background() {
    let model = render::palette::model_styles(&["claude"], |_, _| true).0[0].mark;
    let forecast = render::palette::model_styles(&["claude"], |_, _| true).0[0].forecast;
    let cases = [
        (2.0, 200.0, 200.0, 0, '▁', true),
        (12.0, 14.0, 100.0, 0, '▇', true),
        (10.0, 10.4, 20.0, 3, '▄', true),
        (10.0, 10.0001, 20.0, 3, '▄', false),
    ];
    let cells = cases.map(|(spent, projected, peak, cell, glyph, background)| {
        let (stats, assists) = forecast_fixture(spent, projected, peak);
        let style = if background {
            model.bg_color(forecast.get_fg_color())
        } else {
            model
        };
        [Window::Week, Window::Month, Window::Year].map(|tab| {
            let rows = plot(&stats, &assists, tab, 119);
            let row = &rows[6 - cell];
            (row.contains(&format!("{style}{glyph}")), row.clone())
        })
    });
    assert_eq!(
        cells
            .each_ref()
            .map(|tabs| tabs.each_ref().map(|(matches, _)| *matches)),
        [[true; 3]; 4],
        "{cells:?}"
    );
}

#[test]
fn dashboard_without_color_keeps_the_ghost_but_omits_the_forecast_extension() {
    let (stats, assists) = forecast_fixture(1.0, 200.0, 0.0);
    let frames = [Window::Today, Window::Week].map(|tab| {
        strip_ansi(
            &Dashboard {
                stats: &stats,
                assists: &assists,
                today: TODAY,
                dollars: false,
                glyphs: &panel_glyphs(),
                hold: false,
                color: false,
            }
            .frame(tab, 119, None)
            .text,
        )
    });
    let plots = frames.each_ref().map(|frame| {
        let rows: Vec<_> = frame
            .lines()
            .filter(|line| line.contains(['┤', '┼']))
            .collect();
        (
            rows[0].split_once(['┤', '┼']).unwrap().0.trim().to_owned(),
            rows.iter()
                .map(|row| row.split_once(['┤', '┼']).unwrap().1.trim().to_owned())
                .collect::<Vec<_>>(),
        )
    });
    assert_eq!(plots[0].0, "$1.5k", "{frames:?}");
    assert!(
        plots[0].1.iter().all(|row| row.ends_with("░░░")),
        "{frames:?}"
    );
    assert_eq!(plots[1].0, "$1", "{frames:?}");
    assert!(
        plots[1]
            .1
            .iter()
            .all(|row| row.contains('█') && !row.contains('░')),
        "{frames:?}"
    );
    assert!(
        frames[0].contains("~$200 by midnight · ░ yesterday"),
        "{}",
        frames[0]
    );
}

#[test]
fn dashboard_yesterday_ghost_without_a_column_above_keeps_the_spend_background_clear() {
    let (mut stats, assists) = fixture();
    stats.chart.hours = BTreeMap::from([
        (TODAY * 24, BTreeMap::from([("opus-5.5".into(), 1.0)])),
        (TODAY * 24 + 1, BTreeMap::from([("opus-5.5".into(), 100.0)])),
        (
            (TODAY - 1) * 24,
            BTreeMap::from([("opus-5.5".into(), 12.0)]),
        ),
    ]);
    let style = render::palette::model_styles(&["claude"], |_, _| true).0[0].mark;
    for yesterday in [12.0, 0.5] {
        stats.chart.hours.insert(
            (TODAY - 1) * 24,
            BTreeMap::from([("opus-5.5".into(), yesterday)]),
        );
        let rows = plot(&stats, &assists, Window::Today, 119);
        assert!(rows[6].contains(&format!("{style}▁")), "{}", rows[6]);
        assert!(!rows.iter().any(|row| row.contains('░')), "{rows:?}");
    }
}

#[test]
fn dashboard_yesterday_ghost_fills_only_the_top_spend_cells_background() {
    let (mut stats, assists) = fixture();
    let (models, other) = render::palette::model_styles(&["claude"], |_, _| true);
    let model = models[0].mark;
    let backed = model.bg_color(other.forecast.get_fg_color());
    for (spent, row, glyph) in [(0.0001, 6, '▁'), (5.0, 6, '▃'), (34.0, 4, '▃')] {
        stats.chart.hours = BTreeMap::from([
            (TODAY * 24, BTreeMap::from([("opus-5.5".into(), spent)])),
            (TODAY * 24 + 1, BTreeMap::from([("opus-5.5".into(), 100.0)])),
            (
                (TODAY - 1) * 24,
                BTreeMap::from([("opus-5.5".into(), 80.0)]),
            ),
        ]);
        let rows = plot(&stats, &assists, Window::Today, 119);
        assert!(
            rows[row].contains(&render::paint(backed, &glyph.to_string().repeat(3))),
            "spent {spent}: {rows:?}"
        );
        assert!(
            rows[row - 1].contains(&render::paint(other.forecast, "███")),
            "spent {spent}: {rows:?}"
        );
        for lower in &rows[row + 1..] {
            assert!(lower.contains(&render::paint(model, "███")), "{lower:?}");
            assert!(!lower.contains(&backed.to_string()), "{lower:?}");
        }
    }
}

#[test]
fn dashboard_yesterday_ghost_uses_solid_comparison_cells_above_spend_and_in_future_hours() {
    let (mut stats, assists) = fixture();
    stats.chart.hours.clear();
    for (ago, hour, usd) in [
        (0, 0, 0.5),
        (1, 0, 1.0),
        (0, 1, 1.0),
        (1, 1, 0.5),
        (1, 23, 1.0),
    ] {
        stats.chart.hours.insert(
            (TODAY - ago) * 24 + hour,
            BTreeMap::from([("opus-5.5".into(), usd)]),
        );
    }
    let rows = plot(&stats, &assists, Window::Today, 119);
    let comparison = render::palette::model_styles(&[], |_, _| false).1.forecast;
    assert_ne!(comparison, render::palette::muted());
    let ghost = render::paint(comparison, "███");
    let model = render::paint(
        render::palette::model_styles(&["claude"], |_, _| true).0[0].mark,
        "███",
    );
    let gap = render::paint(render::palette::faint(), " ");
    assert!(
        rows[0].contains(&format!("{ghost}{gap}{model}")),
        "{:?}",
        rows[0]
    );
    assert!(rows[0].ends_with(&ghost), "{:?}", rows[0]);
    assert_eq!(rows.last().unwrap().matches(&ghost).count(), 1);
}
