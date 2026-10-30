use super::*;

fn lines(active: Window) -> Vec<String> {
    lines_at(active, None)
}

fn lines_at(active: Window, rows: Option<usize>) -> Vec<String> {
    let (stats, assists) = fixture();
    strip_ansi(&frame(&stats, &assists, active, 119, rows).text)
        .lines()
        .map(|line| line.strip_prefix("   ").unwrap_or(line).to_owned())
        .collect()
}

#[test]
fn spend_columns_match_the_mock() {
    let (stats, assists) = fixture();
    let dollar_end = |line: &str| {
        let start = line.rfind('$').unwrap();
        let amount = line[start..].split_whitespace().next().unwrap();
        display_width(&line[..start]) + display_width(amount)
    };
    for cols in [60, 64, 70, 74, 75, 80, 100, 111, 112, 113, 119, 160] {
        for tab in Window::TABS {
            let output = frame(&stats, &assists, tab, cols, None).text;
            let text = strip_ansi(&output);
            let lines = text.lines().collect::<Vec<_>>();
            let title = lines
                .iter()
                .position(|line| line.contains("Subagents"))
                .unwrap();
            let row = lines[title + 1..]
                .iter()
                .find(|line| line.contains("implementer"))
                .unwrap();
            assert_eq!(
                dollar_end(lines[title]),
                dollar_end(row),
                "{cols} {tab:?}:\n{}\n{row}",
                lines[title]
            );
            assert!(
                output.contains(&render::paint(
                    render::palette::muted(),
                    "(already counted)"
                )),
                "{cols} {tab:?}: {text}"
            );
        }
    }
    let month = lines(Window::Month);
    for expected in [
        "Models                                                                                  HIT       TOK/S  TOKENS",
        "█ Opus 5.5       $9,371  ━━━━━━━━━━━━━━━━━━──────────────────────────────  38%          96%          85   15.4B",
        "Agents             $904                                   Subagents        $1,681  (already counted)",
        "● brainstormer     $266  ━━━━━━━─────────────────  29%    ● implementer    $1,060  ━━━━━━━━━━━━━━━─────────  63%",
        "Teams            $3,148                                                          AGENT-TIME    MESSAGES  TOKENS",
        "● recon          $2,621  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━────────  83%       18h10m          34    4.5B",
    ] {
        assert!(
            month.iter().any(|line| line == expected),
            "missing mock row: {expected}\n{}",
            month.join("\n")
        );
    }
    let today = lines(Window::Today);
    assert!(today.iter().any(|line| line == "█ Opus 5.5         $688  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━────────────────  67%          97%          88    1.3B"));
}

#[test]
fn provider_and_role_columns_match_the_mock() {
    let month = lines(Window::Month);
    assert!(month.iter().any(|line| line == "█ Claude     $13,232   54%   96% hit     877 sessions     █ Teams     $3,148  13%    ▒ Agents      $904   4%"), "{}", month.join("\n"));
    assert!(
        month.iter().any(|line| line
            == "█ Codex      $11,340   46%   97% hit   5,995 sessions     ░ No owner $20,520  84%"),
        "{}",
        month.join("\n")
    );
    assert!(
        month
            .iter()
            .any(|line| line == "█ Pi           $0.04   <1%                 1 session")
    );
    let today = lines(Window::Today);
    assert!(
        today
            .iter()
            .any(|line| line.starts_with("█ Claude        $691   67%   97% hit      42 sessions"))
    );
    assert!(
        today
            .iter()
            .any(|line| line.starts_with("█ Codex         $334   33%   97% hit     348 sessions"))
    );
}

#[test]
fn heatmap_columns_match_the_mock() {
    let month = lines(Window::Month);
    assert!(
        month.iter().any(|line| line.starts_with("   Mon ")),
        "weekday label and cells must retain the mock gutter"
    );
    let key = month
        .iter()
        .find(|line| line.contains("active days since"))
        .unwrap();
    assert_eq!(display_width(&key[..key.find("66 active").unwrap()]), 40);
    assert!(
        key.ends_with("66 active days since Oct 27 2025 · streak 65 days · busiest Oct 22"),
        "{key}"
    );
    let months = month
        .iter()
        .find(|line| line.contains("Nov") && line.contains("Dec"))
        .unwrap();
    assert_eq!(display_width(&months[..months.find("Nov").unwrap()]), 9);
}

#[test]
fn typical_line_matches_the_mock_row() {
    for (rows, height) in [(None, 7), (Some(40), 7)] {
        let month = lines_at(Window::Month, rows);
        let start = month
            .iter()
            .position(|line| line.starts_with("Spend per day"))
            .unwrap();
        let plot = &month[start + 1..start + 1 + height];
        assert!(plot[0].contains("$1,500"), "{}", month.join("\n"));
        let half = plot.iter().position(|line| line.contains("$750")).unwrap();
        let typical = plot
            .iter()
            .position(|line| line.contains("typical $785"))
            .unwrap();
        assert_eq!(
            typical, half,
            "typical shares the middle row at height {height}"
        );
    }
}

#[test]
fn week_typical_caption_keeps_its_label() {
    let captions = [None, Some(40)].map(|rows| {
        lines_at(Window::Week, rows)
            .iter()
            .find(|line| line.contains("$785"))
            .unwrap()
            .rsplit('┄')
            .next()
            .unwrap()
            .trim()
            .to_owned()
    });
    assert_eq!(captions, ["typical $785"; 2]);
}

#[test]
fn dashboard_order_footer_and_vertical_center_are_tab_independent() {
    let (stats, assists) = fixture();
    let full = Window::TABS.map(|tab| strip_ansi(&frame(&stats, &assists, tab, 119, None).text));
    let tallest = full.iter().map(|text| text.lines().count()).max().unwrap();
    let padded = Window::TABS
        .map(|tab| strip_ansi(&frame(&stats, &assists, tab, 119, Some(tallest + 12)).text));
    assert_eq!(
        padded
            .each_ref()
            .map(|text| text.lines().take_while(|line| line.is_empty()).count()),
        [6; 4]
    );
    let summaries = full
        .each_ref()
        .map(|text| text.lines().find(|line| line.contains("Less")).unwrap());
    assert!(summaries.iter().all(|line| *line == summaries[0]));
    for text in full {
        assert!(!text.starts_with('\n'), "unknown rows must not pad: {text}");
        assert!(
            text.find("Token activity").unwrap() < text.find("[ ").unwrap(),
            "{text}"
        );
        assert!(
            text.find("[ ").unwrap() < text.find("Spend per").unwrap(),
            "{text}"
        );
    }
}

#[test]
fn dashboard_heatmap_footer_counts_only_the_drawn_span_on_narrow_frames() {
    let (mut stats, assists) = fixture();
    let january = "2026-01-17T00:00:00Z"
        .parse::<Timestamp>()
        .unwrap()
        .as_second()
        / DAY_SECS;
    let summaries = [january, TODAY].map(|today| {
        let start = panel::week_start(today) - (weeks_for_terminal(62) as i64 - 1) * 7;
        stats.by_day = BTreeMap::from([
            (start - 1, day(100, 1.0)),
            (start, day(2, 1.0)),
            (today, day(1, 1.0)),
        ]);
        Window::TABS.map(|tab| {
            let output = strip_ansi(
                &Dashboard {
                    stats: &stats,
                    assists: &assists,
                    today,
                    dollars: false,
                    glyphs: &panel_glyphs(),
                    hold: false,
                    color: true,
                }
                .frame(tab, 62, None)
                .text,
            );
            output
                .lines()
                .skip_while(|line| !line.contains("active days since"))
                .take_while(|line| !line.trim().is_empty())
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" ")
        })
    });
    assert_eq!(
        summaries,
        [
            Window::TABS.map(|_| {
                "2 active days since Jul 7 2025 · streak 1 day · busiest Jul 7 2025".to_owned()
            }),
            Window::TABS
                .map(|_| "2 active days since Apr 13 · streak 1 day · busiest Apr 13".to_owned()),
        ]
    );
}

#[test]
fn dashboard_hint_survives_body_truncation_and_respects_hold() {
    let (stats, assists) = fixture();
    let outputs = [false, true].map(|hold| {
        (1..=12)
            .map(|rows| {
                let output = Dashboard {
                    stats: &stats,
                    assists: &assists,
                    today: TODAY,
                    dollars: false,
                    glyphs: &panel_glyphs(),
                    hold,
                    color: true,
                }
                .frame(Window::Today, 119, Some(rows))
                .text;
                strip_ansi(&output)
                    .lines()
                    .last()
                    .unwrap_or("")
                    .trim()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    });
    for (hold, lines) in [false, true].into_iter().zip(outputs) {
        let expected = if hold {
            "←/→ range · t/w/m/y jump · r reload"
        } else {
            "←/→ range · t/w/m/y jump · r reload · q quit"
        };
        assert!(
            lines.iter().all(|line| line == expected),
            "{hold}: {lines:?}"
        );
    }
}

#[test]
fn dashboard_shrinks_sections_before_the_heatmap_or_plot() {
    let (stats, assists) = fixture();
    let mut previous = [true; 6];
    let mut lost = Vec::new();
    for rows in (10..=90).rev() {
        let output = strip_ansi(&frame(&stats, &assists, Window::Month, 119, Some(rows)).text);
        let current = [
            output.contains(TAGLINE),
            output.contains("Stall notice:"),
            output
                .lines()
                .any(|line| line.trim_start().starts_with("Agents ")),
            output
                .lines()
                .any(|line| line.trim_start().starts_with("█ Fable 5.1")),
            output.contains("AGENT-TIME"),
            output.contains("Token activity"),
        ];
        for index in 0..6 {
            if previous[index] && !current[index] {
                lost.push(index);
            }
        }
        if current[5] {
            assert_eq!(
                output.lines().filter(|line| line.contains('┤')).count(),
                7,
                "{rows}: {output}"
            );
        }
        previous = current;
    }
    assert_eq!(lost, [0, 1, 2, 3, 4, 5]);
}

#[test]
fn chart_labels_match_the_mock_columns() {
    let month = lines(Window::Month);
    let axis = month.iter().find(|line| line.contains("└─")).unwrap();
    assert_eq!(display_width(&axis[..axis.find("today").unwrap()]), 94);
    let typical = month
        .iter()
        .find(|line| line.contains("typical $785"))
        .unwrap();
    assert_eq!(
        display_width(&typical[..typical.find("typical").unwrap()]),
        101
    );
    assert!(typical.ends_with("██┄ typical $785"), "{typical}");
    assert_eq!(display_width(typical), 113);
    let week = lines(Window::Week);
    let axis_row = week.iter().position(|line| line.contains("└─")).unwrap();
    let bottom = &week[axis_row - 1];
    let last_bar = display_width(&bottom[..bottom.rfind(" █").unwrap() + 1]);
    let axis = &week[axis_row];
    assert_eq!(
        display_width(&axis[..axis.find("today").unwrap()]),
        last_bar,
        "a label narrower than its bar sits under that bar: {axis}"
    );
    assert!(axis.contains("Oct 22"), "{axis}");
}

#[test]
fn narrow_week_axis_separates_dates_and_keeps_today_under_its_bar() {
    let (stats, assists) = fixture();
    let axes = [62, 70].map(|width| {
        let output = strip_ansi(&frame(&stats, &assists, Window::Week, width, None).text);
        let lines: Vec<_> = output.lines().collect();
        let row = lines.iter().position(|line| line.contains("└─")).unwrap();
        let bottom = lines[row - 1];
        let axis = lines[row];
        let today = display_width(&axis[..axis.find("today").unwrap()]);
        let bar_start = display_width(&bottom[..bottom.rfind(" █").unwrap() + 1]);
        let bar_end = display_width(bottom);
        (
            axis.split_once("└─").unwrap().1.to_owned(),
            today < bar_end && today + "today".len() > bar_start,
        )
    });
    assert_eq!(
        axes,
        [
            ("Oct 17    Oct 19    Oct 21   today".to_owned(), true),
            ("Oct 17      Oct 19      Oct 21      today".to_owned(), true),
        ]
    );
}
