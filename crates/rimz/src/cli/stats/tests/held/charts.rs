use super::*;

fn plot(stats: &Stats, assists: &TabAssists, tab: Window, width: usize) -> Vec<String> {
    frame(stats, assists, tab, width, None)
        .text
        .lines()
        .filter(|line| strip_ansi(line).contains('┤'))
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
                .split('┤')
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
fn dashboard_ceiling_uses_the_fine_ladder_and_middle_row() {
    let (mut stats, assists) = fixture();
    stats.chart.hours.clear();
    let labels: Vec<_> = [1.0, 1.1, 1.3, 1.7, 2.1, 2.7, 3.2, 4.2, 5.2, 6.2, 8.1]
        .into_iter()
        .map(|usd| {
            stats
                .chart
                .hours
                .insert(TODAY * 24, BTreeMap::from([("opus-5.5".into(), usd)]));
            let rows = plot(&stats, &assists, Window::Today, 119);
            let label = |row: usize| {
                strip_ansi(&rows[row])
                    .split('┤')
                    .next()
                    .unwrap()
                    .trim()
                    .to_owned()
            };
            (label(0), label(3))
        })
        .collect();
    let expected: Vec<_> = [1.0, 1.2, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0]
        .into_iter()
        .map(|ceiling| (fmt_usd(ceiling), fmt_usd(ceiling / 2.0)))
        .collect();
    assert_eq!(labels, expected);
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
    let forecast_style = render::palette::model_forecast(Some(("claude", 0)));
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
    let model = render::palette::model_shade("claude", 0);
    let forecast = render::palette::model_forecast(Some(("claude", 0)));
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
fn dashboard_without_color_omits_marks_and_scales_only_drawn_spend() {
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
        let rows: Vec<_> = frame.lines().filter(|line| line.contains('┤')).collect();
        (
            rows[0].split_once('┤').unwrap().0.trim().to_owned(),
            rows.iter()
                .map(|row| row.split_once('┤').unwrap().1.trim().to_owned())
                .collect::<Vec<_>>(),
        )
    });
    assert_eq!(
        plots,
        [
            ("$1.00".to_owned(), vec!["███".to_owned(); 7]),
            ("$1.00".to_owned(), vec!["█████████████".to_owned(); 7]),
        ],
        "{frames:?}"
    );
    assert!(
        frames[0].contains("~$200 by midnight · yesterday"),
        "{}",
        frames[0]
    );
}

#[test]
fn dashboard_yesterday_overlay_shares_a_partial_spend_cell_as_background() {
    let (mut stats, assists) = fixture();
    stats.chart.hours = BTreeMap::from([
        (TODAY * 24, BTreeMap::from([("opus-5.5".into(), 1.0)])),
        (TODAY * 24 + 1, BTreeMap::from([("opus-5.5".into(), 100.0)])),
        (
            (TODAY - 1) * 24,
            BTreeMap::from([("opus-5.5".into(), 12.0)]),
        ),
    ]);
    let rows = plot(&stats, &assists, Window::Today, 119);
    let style =
        render::palette::model_shade("claude", 0).bg_color(render::palette::faint().get_fg_color());
    assert!(rows[6].contains(&format!("{style}▁")), "{}", rows[6]);
}

#[test]
fn dashboard_yesterday_overlay_is_faint_above_actual_and_in_future_hours() {
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
    let faint = render::paint(render::palette::faint(), "███");
    let model = render::paint(render::palette::model_shade("claude", 0), "███");
    let gap = render::paint(render::palette::faint(), " ");
    assert!(
        rows[0].contains(&format!("{faint}{gap}{model}")),
        "{:?}",
        rows[0]
    );
    assert!(rows[0].ends_with(&faint), "{:?}", rows[0]);
    assert_eq!(rows.last().unwrap().matches(&faint).count(), 1);
}
