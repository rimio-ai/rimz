use super::*;
use crate::cli::stats::dashboard::{Dashboard, TabAssists};
use rimz::harness::assist_log::{Assist, AssistRecord};

mod charts;
mod columns;

const TODAY: i64 = 20_749;

fn mock_window(usd: f64, tokens: u64, hit: u64, rate: u64, sessions: u32) -> SpendWindow {
    let output = tokens / 10;
    let input = (tokens - output) * (100 - hit) / 100;
    let cache_read = tokens - output - input;
    SpendWindow {
        usd,
        tokens: input + output,
        input,
        output,
        cache_read,
        timed_ms: if rate == 0 { 0 } else { 1_000 },
        timed_output: rate,
        sessions,
        ..Default::default()
    }
}

fn mock_tally(month: SpendWindow, day: SpendWindow) -> SpendTally {
    let mut week = month.clone();
    week.usd = (month.usd * 5_239.0 / 24_571.4).max(day.usd);
    week.tokens = (month.tokens / 4).max(day.tokens);
    week.input = (month.input / 4).max(day.input);
    week.output = (month.output / 4).max(day.output);
    week.cache_read = (month.cache_read / 4).max(day.cache_read);
    week.sessions = (month.sessions / 4).max(day.sessions);
    week.timed_ms = 0;
    week.timed_output = 0;
    let mut year = month.clone();
    year.usd *= 56_300.0 / 24_571.4;
    year.tokens *= 3;
    year.input *= 3;
    year.output *= 3;
    year.cache_read *= 3;
    year.sessions *= 3;
    year.timed_ms = 4_000;
    year.timed_output = 200;
    SpendTally {
        headline: month.clone(),
        day,
        week,
        month,
        year,
    }
}

pub(super) fn fixture() -> (Stats, TabAssists) {
    let mut cache = ProviderSpendingCache {
        timezone: "UTC".into(),
        refreshed_at_ms: (TODAY as u64 * 86_400 + 15 * 3_600) * 1_000,
        day_cutoff_secs: TODAY as u64 * 86_400 + 15 * 3_600,
        ..Default::default()
    };
    cache.chart.today = TODAY;
    cache.chart.hour = 15;
    cache.spending.total = mock_tally(
        mock_window(24_571.4, 39_800_000_000, 96, 0, 6_873),
        mock_window(1_024.42, 3_010_000_000, 97, 0, 390),
    );
    cache.spending.total.week.usd = 5_239.0;
    cache.spending.total.year.usd = 56_300.0;
    for (id, kind, month_usd, day_usd, month_tokens, day_tokens, hit, day_hit, rate, day_rate) in [
        (
            "opus-5.5",
            "claude",
            9_370.88,
            687.51,
            15_400_000_000,
            1_300_000_000,
            96,
            97,
            85,
            88,
        ),
        (
            "gpt-6-astra",
            "codex",
            7_958.88,
            0.0,
            6_100_000_000,
            0,
            98,
            98,
            27,
            0,
        ),
        (
            "fable-5.1",
            "claude",
            3_640.88,
            0.0,
            1_500_000_000,
            0,
            93,
            93,
            71,
            0,
        ),
        (
            "gpt-6.1-sol",
            "codex",
            3_255.88,
            332.51,
            15_500_000_000,
            1_700_000_000,
            97,
            98,
            35,
            45,
        ),
        (
            "small",
            "claude",
            300.0,
            2.2,
            600_000_000,
            5_000_000,
            94,
            92,
            20,
            0,
        ),
        (
            "",
            "codex",
            44.88,
            2.2,
            700_000_000,
            5_000_000,
            94,
            92,
            10,
            0,
        ),
    ] {
        let mut month = mock_window(month_usd, month_tokens, hit, rate, 0);
        month.timed_ms *= 2;
        month.timed_output *= 2;
        if id == "gpt-6-astra" {
            month.output = 1_000_000;
            month.input = (month_tokens - month.output) / 50;
            month.cache_read = month_tokens - month.output - month.input;
            month.tokens = month.input + month.output;
        }
        cache.models.insert(
            id.into(),
            ModelTally {
                kind: kind.into(),
                tally: mock_tally(
                    month,
                    mock_window(day_usd, day_tokens, day_hit, day_rate, 0),
                ),
            },
        );
    }
    for (kind, month, today, sessions, day_sessions, hit, day_hit) in [
        ("claude", 13_231.6, 690.51, 877, 42, 96, 97),
        ("codex", 11_339.76, 333.91, 5_995, 348, 97, 97),
        ("pi", 0.04, 0.0, 1, 0, 0, 0),
    ] {
        cache.spending.by_provider.insert(
            kind.into(),
            mock_tally(
                mock_window(month, 1_000_000, hit, 0, sessions),
                mock_window(
                    today,
                    if today == 0.0 { 0 } else { 1_000 },
                    day_hit,
                    0,
                    day_sessions,
                ),
            ),
        );
    }
    for offset in 0..65 {
        let usd = match offset {
            0 => 1_024.42,
            1 => 1_293.0,
            2..=6 => (5_239.0 - 1_024.42 - 1_293.0) / 5.0,
            7..=29 => (24_571.4 - 5_239.0) / 23.0,
            30 => 1_024.42 + 23_550.0 - 24_571.4,
            _ => (24_571.4 / 2.86 - (1_024.42 + 23_550.0 - 24_571.4)) / 29.0,
        };
        cache.days.insert(
            TODAY - offset,
            day(if offset == 1 { 2_000 } else { 1_000 }, usd),
        );
    }
    for offset in 0..30 {
        let usd = cache.days[&(TODAY - offset)].usd;
        cache.chart.days.insert(
            TODAY - offset,
            cache
                .models
                .iter()
                .map(|(id, model)| {
                    let cell = if offset == 0 {
                        model.tally.day.usd
                    } else {
                        (model.tally.month.usd - model.tally.day.usd) * usd / (24_571.4 - 1_024.42)
                    };
                    (id.clone(), cell)
                })
                .collect(),
        );
    }
    for ago in 0..=7 {
        let hours = if ago == 0 { 16 } else { 24 };
        for hour in 0..hours {
            cache.chart.hours.insert(
                (TODAY - ago) * 24 + hour,
                cache.chart.days[&(TODAY - ago)]
                    .iter()
                    .map(|(id, usd)| (id.clone(), usd / hours as f64))
                    .collect(),
            );
        }
    }
    let retained_usd = cache.days.values().map(|day| day.usd).sum::<f64>();
    cache
        .days
        .insert(TODAY - 100, day(500, 56_300.0 - retained_usd));
    for (id, model) in &mut cache.models {
        model.tally.week.usd = cache
            .chart
            .days
            .range((TODAY - 6)..)
            .map(|(_, cells)| cells[id])
            .sum();
    }
    for (&day, total) in &cache.days {
        let end = TODAY - (TODAY - day) / 7 * 7;
        let cells = cache.chart.weeks.entry(end).or_default();
        for (id, model) in &cache.models {
            let usd = cache.chart.days.get(&day).map_or_else(
                || {
                    (model.tally.year.usd - model.tally.month.usd) * total.usd
                        / (56_300.0 - 24_571.4)
                },
                |cells| cells[id],
            );
            *cells.entry(id.clone()).or_default() += usd;
        }
    }
    for (view, entries) in [
        (
            RoleView::Agents,
            vec![
                ("brainstormer", 266.0, 13.71),
                ("yagni", 160.0, 26.9),
                ("sweeper", 156.0, 19.4),
                ("prompter", 74.0, 2.42),
                ("tail-0", 61.95, 0.865),
                ("tail-1", 61.95, 0.865),
                ("tail-2", 61.95, 0.865),
                ("tail-3", 61.95, 0.865),
            ],
        ),
        (
            RoleView::Subagents,
            vec![
                ("implementer", 1_060.0, 141.6),
                ("explorer", 190.0, 18.9),
                ("gatekeeper", 134.0, 19.1),
                ("general", 127.0, 27.4),
                ("tail-0", 85.0, 12.8),
                ("tail-1", 85.0, 12.8),
            ],
        ),
        (
            RoleView::Teams,
            vec![
                ("recon", 2_620.6, 525.91),
                ("forge", 414.2, 51.0),
                ("spot", 68.8, 11.6),
                ("mill", 44.0, 21.0),
            ],
        ),
    ] {
        for (i, (name, month_usd, day_usd)) in entries.into_iter().enumerate() {
            let effort = if view == RoleView::Teams {
                EffortWindow {
                    active_ms: [65_400, 9_600, 1_740, 4_260][i] * 1_000,
                    from_user: u64::from(i == 3),
                    from_teammates: [34, 10, 3, 3][i],
                    ..Default::default()
                }
            } else {
                EffortWindow {
                    active_ms: 60_000,
                    from_teammates: 1,
                    ..Default::default()
                }
            };
            let month_tokens = if view == RoleView::Teams {
                [4_500_000_000, 609_000_000, 112_000_000, 75_000_000][i]
            } else {
                1_000_000
            };
            let day_tokens = if view == RoleView::Teams {
                [1_000_000_000, 97_000_000, 18_000_000, 38_000_000][i]
            } else {
                100_000
            };
            let role = effort_role(
                mock_tally(
                    mock_window(month_usd, month_tokens, 0, 0, 1),
                    mock_window(day_usd, day_tokens, 0, 0, 1),
                ),
                effort,
            );
            match view {
                RoleView::Agents => {
                    cache.roles.agents.insert(name.into(), role);
                }
                RoleView::Subagents => {
                    cache.roles.subagents.insert(name.into(), role);
                }
                RoleView::Teams => {
                    cache
                        .roles
                        .teams
                        .insert(name.into(), BTreeMap::from([("coder".into(), role)]));
                }
            }
        }
    }
    cache.roles.recorded_since_secs = Some((TODAY as u64 - 8) * 86_400);
    cache.roles.effort_since_secs = cache.roles.recorded_since_secs;
    let today = AssistRollup {
        resident_launches: 53,
        resumes: 16,
        recovered_secs: 18_360,
        compacts: 60,
        keepalives: 39,
        hibernations: 374,
        hibernate_reclaimed_bytes: 25 * 1_024 * 1_024 * 1_024,
        launch_retries: 17,
        sweeps: 2,
        reclaimed_bytes: 5 * 1_024 * 1_024 / 2,
        held: BTreeMap::from([("compacts", 6)]),
        ..Default::default()
    };
    let month = AssistRollup {
        stall_notices: 16,
        workflow_starts: 30,
        resident_launches: 957,
        judge_declines: 344,
        model_aliases: 4,
        empty_dir_grants: 0,
        tier_fallbacks: 200,
        resumes: 680,
        recovered_secs: 161_280,
        compacts: 717,
        keepalives: 735,
        idle_stops: 19,
        idle_stop_withdrawals: 1,
        molts: 19,
        hibernations: 3_646,
        hibernate_reclaimed_bytes: 272 * 1_024 * 1_024 * 1_024,
        launch_retries: 186,
        redeems: 5,
        resets: 5,
        restores: 8,
        restored_sessions: 240,
        sweeps: 117,
        reclaimed_bytes: 575 * 1_024 * 1_024 * 1_024,
        ..Default::default()
    };
    let assists = TabAssists {
        tabs: [today.clone(), today, month.clone(), month].map(|rollup| AssistStats {
            rollup,
            ..Default::default()
        }),
        earliest: None,
        ..Default::default()
    };
    (Stats::from_provider(cache), assists)
}

pub(super) fn frame(
    stats: &Stats,
    assists: &TabAssists,
    tab: Window,
    cols: usize,
    rows: Option<usize>,
) -> super::super::dashboard::Frame {
    Dashboard {
        stats,
        assists,
        today: TODAY,
        dollars: false,
        glyphs: &panel_glyphs(),
        hold: false,
        color: true,
    }
    .frame(tab, cols, rows)
}

#[test]
fn dashboard_fits_every_tab_without_wrapping_or_a_final_newline() {
    let (stats, assists) = fixture();
    assert!(
        !frame(&stats, &assists, Window::Month, 119, None)
            .text
            .is_empty()
    );
    for cols in [60, 80, 100, 111, 112, 113, 119, 160] {
        for rows in 10..=90 {
            let frames = Window::TABS.map(|tab| frame(&stats, &assists, tab, cols, Some(rows)));
            assert!(frames.iter().all(|frame| frame.step == frames[0].step));
            for frame in frames {
                assert!(frame.text.lines().count() <= rows, "{cols}x{rows}");
                assert!(
                    frame.text.lines().all(|line| display_width(line) <= cols),
                    "{cols}x{rows}"
                );
                assert!(!frame.text.ends_with('\n'));
            }
        }
    }
    let output = frame(&stats, &assists, Window::Month, 119, Some(40));
    assert_eq!(output.text.lines().count(), 39);
    let mut terminal = Vec::new();
    rimz::tui::replace_frame(&mut terminal, output.text.as_bytes()).unwrap();
    let mut parser = vt100::Parser::new(40.try_into().unwrap(), 119.try_into().unwrap(), 0);
    parser.process(&terminal);
    let screen = parser.screen().contents();
    assert!(
        screen
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap()
            .contains("Token activity"),
        "{screen}"
    );
    assert!(
        screen.lines().last().unwrap().contains("r reload · q quit"),
        "{screen}"
    );
}

#[test]
fn dashboard_mock_heights_and_snapshots() {
    let (stats, assists) = fixture();
    for (rows, today_height, month_height) in [(76, 65, 73), (40, 38, 39)] {
        for tab in Window::TABS {
            let output = frame(&stats, &assists, tab, 119, Some(rows));
            let expected = if tab == Window::Today {
                today_height
            } else {
                month_height
            };
            if matches!(tab, Window::Today | Window::Month) {
                assert_eq!(output.text.lines().count(), expected, "{tab:?} at {rows}");
            }
            let text = strip_ansi(&output.text)
                .lines()
                .map(str::trim_end)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                text.lines()
                    .find(|line| !line.is_empty())
                    .unwrap()
                    .starts_with("   ")
            );
            insta::assert_snapshot!(
                format!("held_{}_{}", tab.label().to_lowercase(), rows),
                text
            );
        }
    }
}

#[test]
fn dashboard_empty_keeps_the_wordmark_and_centres_at_both_widths() {
    let stats = Stats::from_provider(ProviderSpendingCache {
        timezone: "UTC".into(),
        ..Default::default()
    });
    let assists = TabAssists::default();
    for (cols, minimum, maximum) in [(200, 60, 100), (80, 0, 30)] {
        let output = strip_ansi(&frame(&stats, &assists, Window::Today, cols, Some(40)).text);
        assert!(output.contains("No agent sessions recorded on this machine yet."));
        assert!(output.contains("Spend appears here after your first agent session;"));
        let copy = output
            .lines()
            .skip_while(|line| !line.contains("No agent sessions"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(copy.contains("Spend appears here after your first agent session; rimz stats reads it from each agent's own session files."), "{copy}");
        let line = output
            .lines()
            .find(|line| line.contains(TAGLINE))
            .expect("tagline");
        let start = line.find(TAGLINE).unwrap();
        assert!(start > minimum && start < maximum, "{line}");
    }
    insta::assert_snapshot!(
        "held_empty",
        strip_ansi(&frame(&stats, &assists, Window::Today, 119, Some(76)).text)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn dashboard_folds_dollars_and_keeps_team_effort_and_small_provider_identity() {
    let (mut stats, assists) = fixture();
    let output = strip_ansi(&frame(&stats, &assists, Window::Month, 119, None).text);
    assert!(output.contains("GPT 6 Astra"), "{output}");
    let models = output
        .split("Models")
        .nth(1)
        .unwrap()
        .split("Providers")
        .next()
        .unwrap();
    assert_eq!(models.lines().filter(|line| line.contains('$')).count(), 5);
    assert!(
        models
            .lines()
            .any(|line| line.contains("Other") && line.contains('–'))
    );
    assert!(
        output
            .lines()
            .any(|line| line.contains("Pi") && line.contains("<1%") && line.contains("1 session"))
    );
    assert!(output.contains("5,995 sessions"));
    assert!(!output.contains("agent-time and messages"));
    let short = strip_ansi(&frame(&stats, &assists, Window::Month, 119, Some(45)).text);
    let other = short
        .lines()
        .find(|line| line.contains("Other") && line.contains("1h40m"))
        .expect(&short);
    assert!(other.contains("7 (1 you)"), "{other}");
    assert!(short.contains("Assists  17 kinds"));
    stats.by_provider.get_mut("pi").unwrap().month.usd = 0.0;
    let unpriced = strip_ansi(&frame(&stats, &assists, Window::Month, 119, None).text);
    assert!(
        unpriced
            .lines()
            .any(|line| line.contains("Pi") && line.contains("<1%") && line.contains("1 session"))
    );
}

#[test]
fn dashboard_single_tail_keeps_its_name_and_unknown_model_is_named() {
    let (mut stats, assists) = fixture();
    stats.by_model.remove("small");
    let output = strip_ansi(&frame(&stats, &assists, Window::Month, 119, None).text);
    assert!(output.contains("Unknown model"), "{output}");
    let teams = output.split("Teams ").last().unwrap();
    assert!(!teams.contains("coder"));
    stats.by_model = model_map([("unsafe-\x1b[2J".into(), every_window(tally(1, 1.0, 1)))]);
    assert!(
        !frame(&stats, &assists, Window::Month, 119, None)
            .text
            .contains("\x1b[2J")
    );
}

#[test]
fn dashboard_charts_use_per_range_publication_cells() {
    let (stats, assists) = fixture();
    assert_eq!(
        stats.chart.days.len(),
        30,
        "chart survives the publication projection"
    );
    let counts = Window::TABS.map(|tab| {
        let text = strip_ansi(&frame(&stats, &assists, tab, 119, None).text);
        let bottom = text.lines().rfind(|line| line.contains('┤')).unwrap();
        bottom.split('┤').nth(1).unwrap().split_whitespace().count()
    });
    // The fixture's Year is sparse; use a complete publication for its bar count below.
    assert_eq!(&counts[..3], &[24, 7, 30]);
    let month = frame(&stats, &assists, Window::Month, 119, None).text;
    let plain = strip_ansi(&month);
    let legend = plain.lines().find(|line| line.contains("Other")).unwrap();
    for name in [
        "Opus 5.5",
        "GPT 6 Astra",
        "Fable 5.1",
        "GPT 6.1 Sol",
        "Other",
    ] {
        assert!(legend.contains(name), "{legend}");
    }
    assert!(month.contains(&render::paint(render::palette::accent(), "today")));
}

#[test]
fn dashboard_typical_counts_missing_days_and_titles_follow_the_active_tab() {
    let (mut stats, assists) = fixture();
    stats.by_day = BTreeMap::from([(TODAY, day(1, 24.0)), (TODAY - 1, day(1, 300.0))]);
    for (tab, expected) in [
        (Window::Today, "today $1,024 so far · ~"),
        (Window::Week, "$162 a day"),
        (Window::Month, "$162 a day"),
        (Window::Year, "$162 per active day"),
    ] {
        let output = strip_ansi(&frame(&stats, &assists, tab, 119, None).text);
        assert!(output.contains(expected), "{tab:?}: {output}");
        assert!(!output.contains("vs prior"));
        if matches!(tab, Window::Week | Window::Month) {
            let lines: Vec<_> = output.lines().collect();
            let typical = lines
                .iter()
                .position(|line| line.contains('┤') && line.ends_with("$10"))
                .expect(&output);
            assert!(
                lines[typical + 1].contains("└─"),
                "a mean under half a row sits on the bottom row: {output}"
            );
        } else if tab == Window::Year {
            assert!(output.contains("Spend per week · last 52 weeks"));
            assert!(!output.contains("typical"));
        } else {
            assert!(output.contains("by midnight · yesterday $300"));
            assert!(!output.contains("typical"));
        }
    }
    stats.by_day.insert(TODAY - 8, day(1, 100.0));
    let output = strip_ansi(&frame(&stats, &assists, Window::Week, 119, None).text);
    assert!(output.contains("↑224% vs prior 7 days"), "{output}");
}

#[test]
fn dashboard_assist_suffixes_and_retention_header_use_the_window_start() {
    let now = Timestamp::from_second(TODAY * DAY_SECS + 15 * 3_600).unwrap();
    for (window, days) in Window::TABS.into_iter().zip([0, 6, 29, 363]) {
        assert_eq!(
            window.assist_since(now, "UTC").unwrap().as_second(),
            (TODAY - days) * DAY_SECS
        );
    }
    assert_eq!(Window::Year.span_days(), 364);
    let records = [now - Duration::from_secs(8 * 86_400), now].map(|at| AssistRecord {
        at,
        assist: Assist::CacheKeepalive {
            kind: rimz::ids::AgentKind::new_unchecked("claude"),
            agent_id: rimz::ids::AgentSessionId::from("test"),
            label: Some("fox".into()),
            idle_secs: 60,
            waits: 1,
            horizon_secs: None,
            message_id: "msg_test".into(),
            delivered: true,
            error: None,
            capped: false,
        },
    });
    let mut assists = TabAssists::from_records(records.into(), now, "UTC");
    assert_eq!(
        assists
            .tabs
            .iter()
            .map(|tab| tab.rollup.keepalives)
            .collect::<Vec<_>>(),
        [1, 1, 2, 2]
    );
    let (mut stats, _) = fixture();
    for (tab, expected) in [
        (Window::Today, false),
        (Window::Week, false),
        (Window::Month, true),
        (Window::Year, true),
    ] {
        let output = strip_ansi(&frame(&stats, &assists, tab, 119, None).text);
        assert_eq!(output.contains("(records since"), expected, "{output}");
    }
    assists.earliest = Window::Month.assist_since(now, "UTC");
    stats.refreshed_at_ms -= 60_000;
    assert!(
        !frame(&stats, &assists, Window::Month, 119, None)
            .text
            .contains("(records since")
    );
}

#[test]
fn dashboard_weekly_chart_survives_a_later_local_day() {
    let (stats, assists) = fixture();
    let output = Dashboard {
        stats: &stats,
        assists: &assists,
        today: TODAY + 1,
        dollars: false,
        glyphs: &panel_glyphs(),
        hold: false,
        color: true,
    }
    .frame(Window::Year, 119, None)
    .text;
    let chart = strip_ansi(&output)
        .split("Spend per week")
        .nth(1)
        .unwrap()
        .split("Token activity")
        .next()
        .unwrap()
        .to_owned();
    assert!(
        chart
            .lines()
            .filter(|line| line.contains('┤'))
            .any(|line| line.contains('█')),
        "weekly cells must survive a midnight repaint: {chart}"
    );
}

#[test]
fn dashboard_model_shades_follow_year_rank_on_every_tab() {
    let (stats, assists) = fixture();
    assert_ne!(
        render::palette::model_shade("claude", 0),
        render::palette::model_shade("claude", 1)
    );
    let expected = render::paint(render::palette::model_shade("claude", 1), "█");
    for tab in [Window::Month, Window::Year] {
        let output = frame(&stats, &assists, tab, 119, None).text;
        let row = output
            .lines()
            .find(|line| line.contains("Fable 5.1") && !line.contains("Opus"))
            .expect("model row");
        assert!(row.contains(&expected), "{row}");
    }
}

#[test]
fn dashboard_weekly_chart_uses_the_published_history_anchor() {
    let (stats, assists) = fixture();
    let output = strip_ansi(&frame(&stats, &assists, Window::Year, 119, None).text);
    let chart = output
        .split("Spend per week")
        .nth(1)
        .unwrap()
        .split("Token activity")
        .next()
        .unwrap();
    assert!(
        chart
            .lines()
            .any(|line| line.contains('┤') && line.contains('█')),
        "{chart}"
    );
    assert!(chart.contains("Opus 5.5"), "{chart}");
}

#[test]
fn dashboard_empty_roles_have_unsigned_zero_shares() {
    let (mut stats, assists) = fixture();
    stats.agents.clear();
    stats.teams.clear();
    stats.subagents.clear();
    let output = strip_ansi(&frame(&stats, &assists, Window::Today, 119, None).text);
    assert!(!output.contains("-0%"), "{output}");
}

#[test]
fn dashboard_average_uses_the_same_calendar_days_for_dollars_and_activity() {
    let (mut stats, assists) = fixture();
    stats.by_day = BTreeMap::from([(TODAY - 7, day(1, 10.0)), (TODAY, day(1, 4.0))]);
    stats.total.week.usd = 14.0;
    let output = strip_ansi(&frame(&stats, &assists, Window::Week, 119, None).text);
    assert!(output.contains("$4.00 a day"), "{output}");
    stats.by_day.remove(&TODAY);
    let output = strip_ansi(&frame(&stats, &assists, Window::Week, 119, None).text);
    assert!(output.contains("$0.00 a day"), "{output}");
    let output = strip_ansi(&frame(&stats, &assists, Window::Month, 119, None).text);
    assert!(output.contains("$10 a day"), "{output}");
}

#[test]
fn dashboard_hit_cells_keep_cache_health_tones() {
    let (mut stats, assists) = fixture();
    for (hit, style) in [
        (68, render::palette::alarm()),
        (70, render::palette::warn()),
        (90, render::palette::good()),
    ] {
        let spend = mock_window(100.0, 1_000, hit, 0, 1);
        stats.by_model.get_mut("opus-5.5").unwrap().tally.day = spend.clone();
        stats.by_provider.get_mut("claude").unwrap().day = spend;
        let output = frame(&stats, &assists, Window::Today, 119, None).text;
        let painted = render::paint(style, &format!("{hit}%"));
        assert!(painted.contains('\x1b'));
        for name in ["Opus 5.5", "Claude"] {
            let row = output
                .lines()
                .find(|line| line.contains(name) && line.contains('$'))
                .unwrap();
            assert!(row.contains(&painted), "{row:?}");
        }
    }
}

#[test]
fn dashboard_single_day_streak_is_singular() {
    let (mut stats, assists) = fixture();
    stats.by_day = BTreeMap::from([(TODAY, day(1, 1.0))]);
    let output = strip_ansi(&frame(&stats, &assists, Window::Today, 119, None).text);
    assert!(output.contains("streak 1 day ·"), "{output}");
}

#[test]
fn dashboard_caps_extreme_trend_labels() {
    let (mut stats, assists) = fixture();
    stats.by_day = BTreeMap::from([(TODAY, day(1, 1_000.0)), (TODAY - 7, day(1, 1.0))]);
    let output = strip_ansi(&frame(&stats, &assists, Window::Week, 119, None).text);
    assert!(output.contains("↑999%+ vs prior 7 days"), "{output}");
}

#[test]
fn dashboard_typical_never_exceeds_the_chart_ceiling() {
    let (mut stats, assists) = fixture();
    stats.by_day.insert(TODAY - 30, day(1, 300_000.0));
    let output = strip_ansi(&frame(&stats, &assists, Window::Month, 119, None).text);
    let chart = output.split("Spend per day").nth(1).unwrap();
    assert!(chart.lines().nth(1).unwrap().contains("$12,000"), "{chart}");
    let typical = chart
        .lines()
        .find(|line| line.contains('┤') && line.contains("$10,785"))
        .unwrap();
    assert!(
        typical.ends_with("┄ $10,785") && !typical.contains("typical"),
        "{chart}"
    );
    let axis = chart.lines().find(|line| line.contains("└─")).unwrap();
    assert_eq!(
        display_width(&axis[..axis.find("today").unwrap()]),
        97,
        "bars keep 3-cell strides: {chart}"
    );
    let narrow = strip_ansi(&frame(&stats, &assists, Window::Month, 60, None).text);
    assert!(
        narrow
            .lines()
            .any(|line| line.contains('┤') && line.ends_with("$10,785")),
        "{narrow}"
    );
}

#[test]
fn dashboard_narrow_weekly_chart_keeps_the_newest_weeks_and_now() {
    let (mut stats, assists) = fixture();
    stats.chart.weeks = BTreeMap::from([
        (TODAY, BTreeMap::from([("newest".into(), 100.0)])),
        (TODAY - 50 * 7, BTreeMap::from([("oldest".into(), 100.0)])),
    ]);
    let output = strip_ansi(&frame(&stats, &assists, Window::Year, 60, None).text);
    let chart = output
        .split("Spend per week")
        .nth(1)
        .unwrap()
        .split("Token activity")
        .next()
        .unwrap();
    assert!(chart.contains("last 50 weeks"), "{chart}");
    let axis = chart.lines().find(|line| line.contains("└─")).unwrap();
    assert!(axis.ends_with("now"), "{axis}");
    assert!(chart.contains("█ Newest"), "{chart}");
    assert!(!chart.contains("Oldest"), "{chart}");
}

#[test]
fn dashboard_wide_dollars_keep_complete_shares() {
    let (mut stats, assists) = fixture();
    stats.total.year.usd = 123_456.0;
    stats.by_model = model_map([("large".into(), every_window(tally(1, 123_456.0, 1)))]);
    stats.agents = BTreeMap::from([(
        "large".into(),
        role_tally(every_window(tally(1, 123_456.0, 1))),
    )]);
    stats.subagents.clear();
    stats.teams.clear();
    let output = strip_ansi(&frame(&stats, &assists, Window::Year, 119, None).text);
    for row in output.lines().filter(|line| line.contains("large")) {
        assert!(row.contains("$123,456") && row.contains("100%"), "{row}");
        assert!(display_width(row) <= 119, "{row}");
    }
    stats.agents.clear();
    let output = strip_ansi(&frame(&stats, &assists, Window::Year, 119, None).text);
    let row = output
        .lines()
        .find(|line| line.contains("No owner"))
        .unwrap();
    assert!(row.contains("$123,456 100%"), "{row}");
}
