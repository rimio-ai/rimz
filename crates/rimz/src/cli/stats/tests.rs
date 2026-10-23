use super::*;
use rimz::agents::spending::{EffortTally, EffortWindow};
use rimz::theme::theme_glyphs;
use std::path::{Path, PathBuf};

mod held;

#[test]
fn window_flag_accepts_each_mode_and_rejects_assists() {
    let command = || StatsArgs::augment_args(clap::Command::new("stats"));
    for mode in [
        vec![],
        vec!["--json"],
        vec!["--agents"],
        vec!["--agents", "--json"],
        vec!["--teams"],
        vec!["--teams", "--json"],
        vec!["--subagents"],
        vec!["--subagents", "--json"],
        vec!["--refresh"],
    ] {
        for window in ["today", "week", "month", "year"] {
            let mut argv = vec!["stats", "--window", window];
            argv.extend(&mode);
            assert!(command().try_get_matches_from(argv).is_ok());
        }
    }
    assert!(
        command()
            .try_get_matches_from(["stats", "--window", "today", "--assists"])
            .is_err()
    );
}

#[test]
fn agents_flag_conflicts_with_other_explicit_views() {
    for flag in ["--teams", "--subagents", "--refresh", "--assists"] {
        let error = StatsArgs::augment_args(clap::Command::new("stats"))
            .try_get_matches_from(["stats", "--agents", flag])
            .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
    for flag in ["--agents", "--subagents", "--refresh", "--assists"] {
        let error = StatsArgs::augment_args(clap::Command::new("stats"))
            .try_get_matches_from(["stats", "--teams", flag])
            .unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }
}

fn role_tally(tally: SpendTally) -> rimz::agents::spending::RoleTally {
    rimz::agents::spending::RoleTally {
        tally,
        ..Default::default()
    }
}

fn role_stats(
    agents: BTreeMap<String, SpendTally>,
    subagents: BTreeMap<String, SpendTally>,
) -> Stats {
    let mut cache = ProviderSpendingCache {
        timezone: "UTC".into(),
        refreshed_at_ms: (20_000 * 86_400 + 15 * 3_600) * 1_000,
        ..Default::default()
    };
    cache.days.insert(20_000, day(1_000, 100.0));
    cache.spending.total = every_window(tally(1_000, 100.0, 10));
    cache.spending.by_provider.insert(
        "removed-plugin".into(),
        every_window(tally(1_000, 100.0, 10)),
    );
    cache.roles.agents = agents
        .into_iter()
        .map(|(profile, tally)| (profile, role_tally(tally)))
        .collect();
    cache.roles.subagents = subagents
        .into_iter()
        .map(|(profile, tally)| (profile, role_tally(tally)))
        .collect();
    cache.roles.recorded_since_secs = Some(20_000 * 86_400 + 12 * 3_600);
    Stats::from_provider(cache)
}

fn full_panel_text(stats: &Stats, held: bool) -> String {
    if held {
        return strip_ansi(&held_frame(
            stats,
            &AssistStats::default(),
            Window::Today,
            20_000,
        ));
    }
    let mut out = Vec::new();
    panel::render_spend(&mut out, stats, 20_000, false, None, Window::Today, "\n").unwrap();
    strip_ansi(std::str::from_utf8(&out).unwrap())
}

fn spend_text(stats: &Stats, role: Option<RoleView>, window: Window) -> String {
    let mut out = Vec::new();
    panel::render_spend(&mut out, stats, 20_000, false, role, window, "\n").unwrap();
    strip_ansi(std::str::from_utf8(&out).unwrap())
}

#[test]
fn one_shot_averages_spend_over_active_days() {
    let mut stats = Stats::from_provider(team_cache());
    stats.by_day = [0, 1, 7, 29, 30, 364, 365]
        .map(|ago| (20_000 - ago, day(1, 10.0)))
        .into();
    stats.by_day.insert(19_994, day(0, 1.0));
    stats.total.week.usd = 28.0;
    stats.total.month.usd = 100.0;
    stats.total.year.usd = 180.0;
    let text = spend_text(&stats, None, Window::Today);
    let row = text
        .lines()
        .find(|line| line.contains("per active day"))
        .expect(&text);
    assert_eq!(
        row.split_whitespace().collect::<Vec<_>>(),
        ["per", "active", "day", "–", "$11", "$10", "$10"]
    );
}

#[test]
fn one_shot_average_excludes_the_oldest_partial_day_from_both_inputs() {
    let mut stats = Stats::from_provider(team_cache());
    stats.by_day = BTreeMap::from([(19_993, day(1, 10.0)), (20_000, day(1, 4.0))]);
    stats.total.week.usd = 14.0;
    let text = spend_text(&stats, None, Window::Week);
    let row = text
        .lines()
        .find(|line| line.contains("per active day"))
        .unwrap();
    assert_eq!(row.split_whitespace().nth(4), Some("$4.00"), "{row}");
}

#[test]
fn one_shot_extreme_trends_do_not_move_the_month_figure() {
    let mut stats = Stats::from_provider(team_cache());
    stats.by_day = BTreeMap::from([(20_000, day(1, 1_000.0)), (19_993, day(1, 0.0005))]);
    stats.total.week.usd = 1_000.0;
    stats.total.month.usd = 24_571.0;
    let text = spend_text(&stats, None, Window::Week);
    let row = text.lines().find(|line| line.contains("dollars")).unwrap();
    assert_eq!(
        display_width(&row[..row.find("$24,571").unwrap() + "$24,571".len()]),
        69,
        "{row}"
    );
    assert!(row.contains("↑999%+"), "{row}");
}

#[test]
fn one_shot_single_day_streak_is_singular() {
    let mut stats = Stats::from_provider(team_cache());
    stats.by_day = BTreeMap::from([(20_000, day(1, 1.0))]);
    let text = spend_text(&stats, None, Window::Today);
    assert!(text.contains("streak 1 day ·"), "{text}");
}

#[test]
fn one_shot_trends_preserve_right_aligned_dollar_columns() {
    let mut stats = Stats::from_provider(team_cache());
    stats.by_day = (0..60)
        .map(|ago| {
            (
                20_000 - ago,
                day(
                    1,
                    if ago < 7 {
                        5.0
                    } else if ago < 30 {
                        10.0
                    } else {
                        4.0
                    },
                ),
            )
        })
        .collect();
    stats.total.week.usd = 35.0;
    stats.total.month.usd = 265.0;
    let with_trends = spend_text(&stats, None, Window::Month);
    let dollars = with_trends
        .lines()
        .find(|line| line.contains("dollars"))
        .unwrap();
    assert!(dollars.contains("$35 ↓50%"), "{dollars}");
    assert!(dollars.contains("$265 ↑121%"), "{dollars}");
    stats.by_day.retain(|day, _| *day > 19_993);
    let without_trends = spend_text(&stats, None, Window::Month);
    let plain = without_trends
        .lines()
        .find(|line| line.contains("dollars"))
        .unwrap();
    assert!(!plain.contains(['↑', '↓']), "{plain}");
    let header = with_trends.lines().next().unwrap();
    let end_column = |line: &str, needle: &str| {
        display_width(&line[..line.find(needle).unwrap() + needle.len()])
    };
    for (label, amount, edge) in [("Week", "$35", 52), ("Month", "$265", 69)] {
        let end = end_column(dollars, amount);
        assert_eq!(
            end,
            end_column(plain, amount),
            "{with_trends}\n{without_trends}"
        );
        assert_eq!(end, end_column(header, label));
        assert_eq!(end, edge, "the mock's column edge: {with_trends}");
    }
    assert_eq!(end_column(header, "Today"), 35);
    assert_eq!(end_column(header, "Year"), 86);
}

#[test]
fn one_shot_combines_independent_coverage_dates_above_the_first_role() {
    let since = 20_000 * 86_400 + 12 * 3_600;
    let midnight = 20_000 * 86_400;
    for (roles, effort, expected) in [
        (
            Some(since),
            Some(midnight),
            "roles recorded since Oct 04, earlier spend counts as No role",
        ),
        (
            Some(midnight),
            Some(since),
            "agent-time and messages recorded since Oct 04",
        ),
        (
            Some(since),
            Some(since),
            "roles recorded since Oct 04, earlier spend counts as No role · agent-time and messages since Oct 04",
        ),
        (None, None, ""),
    ] {
        let mut stats = Stats::from_provider(team_cache());
        stats.recorded_since_secs = roles;
        stats.effort_since_secs = effort;
        for role in [
            None,
            Some(RoleView::Agents),
            Some(RoleView::Teams),
            Some(RoleView::Subagents),
        ] {
            let text = spend_text(&stats, role, Window::Today);
            let notes: Vec<_> = text
                .lines()
                .filter(|line| line.contains("recorded since"))
                .collect();
            if expected.is_empty() {
                assert!(notes.is_empty(), "{text}");
                continue;
            }
            assert_eq!(notes.len(), 1, "{text}");
            assert_eq!(notes[0].trim(), expected);
            let section = format!("{} (today)", role.unwrap_or(RoleView::Agents).label());
            assert!(
                text.find(expected).unwrap() < text.find(&section).unwrap(),
                "{text}"
            );
        }
    }
}

#[test]
fn one_shot_reports_only_a_positive_cent_of_unassigned_spend() {
    let mut stats = Stats::from_provider(team_cache());
    for (total, expected) in [
        (150.0, true),
        (100.0, false),
        (50.0, false),
        (100.009, false),
    ] {
        stats.total.day.usd = total;
        let text = spend_text(&stats, None, Window::Today);
        assert_eq!(text.contains("No role recorded"), expected, "{text}");
        if expected {
            let row = text
                .lines()
                .find(|line| line.contains("No role recorded"))
                .unwrap();
            assert_eq!(
                row.split_whitespace().collect::<Vec<_>>(),
                ["No", "role", "recorded", "$50", "33.3%"]
            );
            assert!(text.find("Subagents (today)").unwrap() < text.find(row).unwrap());
            assert!(text.find(row).unwrap() < text.find("Token activity").unwrap());
        }
        for role in [RoleView::Agents, RoleView::Teams, RoleView::Subagents] {
            assert!(!spend_text(&stats, Some(role), Window::Today).contains("No role recorded"));
        }
    }
}

#[test]
fn one_shot_footer_scopes_sessions_and_omits_the_clause_without_them() {
    let mut stats = Stats::from_provider(team_cache());
    stats.total = SpendTally {
        day: spend_window(100, 3_333.0, 0, 0, 0, 1_111),
        week: spend_window(100, 8_888.0, 0, 0, 0, 2_222),
        month: spend_window(100, 16_665.0, 0, 0, 0, 3_333),
        year: spend_window(100, 26_664.0, 0, 0, 0, 4_444),
        ..Default::default()
    };
    for (window, label, sessions, cost) in [
        (Window::Today, "today", "1,111", "$3.00"),
        (Window::Week, "this week", "2,222", "$4.00"),
        (Window::Month, "this month", "3,333", "$5.00"),
        (Window::Year, "this year", "4,444", "$6.00"),
    ] {
        let text = spend_text(&stats, None, window);
        assert_eq!(
            text.lines().last().unwrap(),
            format!("  as of 15:00 · {sessions} sessions {label} at {cost} each")
        );
        for role in [RoleView::Agents, RoleView::Teams, RoleView::Subagents] {
            let text = spend_text(&stats, Some(role), window);
            assert_eq!(text.lines().last().unwrap(), "  as of 15:00");
        }
    }
    stats.total = SpendTally::default();
    let text = spend_text(&stats, None, Window::Today);
    assert_eq!(text.lines().last().unwrap(), "  as of 15:00");
}

#[test]
fn one_shot_fresh_machine_replaces_zero_tables_and_heatmap() {
    let stats = Stats::from_provider(ProviderSpendingCache {
        timezone: "UTC".into(),
        refreshed_at_ms: (20_000 * 86_400 + 15 * 3_600) * 1_000,
        ..Default::default()
    });
    assert_eq!(
        spend_text(&stats, None, Window::Today),
        "  No agent sessions recorded on this machine yet.\n  Spend appears here after your first agent session; rimz stats reads it from each agent's own session files.\n\n  as of 15:00\n"
    );
}

#[test]
fn one_shot_groups_session_and_run_counts_in_compact_and_explicit_views() {
    let mut cache = team_cache();
    let tally = every_window(tally(100, 10.0, 1_111));
    cache
        .spending
        .by_provider
        .insert("codex".into(), tally.clone());
    cache.roles.agents.get_mut("solo").unwrap().tally = tally.clone();
    cache.roles.subagents.get_mut("child").unwrap().tally = tally.clone();
    cache
        .roles
        .teams
        .get_mut("recon")
        .unwrap()
        .retain(|role, _| role == "coder");
    cache
        .roles
        .teams
        .get_mut("recon")
        .unwrap()
        .get_mut("coder")
        .unwrap()
        .tally = tally;
    let stats = Stats::from_provider(cache);
    let text = spend_text(&stats, None, Window::Today);
    assert_eq!(text.matches("1,111").count(), 4, "{text}");
    let headers: Vec<_> = text
        .lines()
        .filter_map(|line| {
            line.find("AGENT-TIME")
                .map(|at| &line[..at + "AGENT-TIME".len()])
        })
        .collect();
    assert_eq!(headers.len(), 3, "{text}");
    for header in headers {
        assert_eq!(display_width(header), 63, "the mock's column edge: {text}");
    }
    for role in [RoleView::Agents, RoleView::Teams, RoleView::Subagents] {
        let text = spend_text(&stats, Some(role), Window::Today);
        assert!(text.contains("1,111"), "{text}");
        assert!(text.contains("AGENT-TIME"), "{text}");
    }
}

#[test]
fn one_shot_heatmap_key_combines_window_activity_with_the_full_streak() {
    let mut stats = Stats::from_provider(team_cache());
    stats.by_day = (0..40).map(|ago| (20_000 - ago, day(1, 1.0))).collect();
    stats.by_day.insert(19_999, day(10, 1.0));
    stats.by_day.insert(19_940, day(100, 1.0));
    for (window, days, busiest) in [
        (Window::Today, 28, 19_940),
        (Window::Week, 7, 19_999),
        (Window::Month, 30, 19_999),
        (Window::Year, 28, 19_940),
    ] {
        let text = spend_text(&stats, None, window);
        let key = text.lines().find(|line| line.contains("Less")).unwrap();
        let summary = format!(
            "{days} of last {days} days active · streak 40 days · busiest {}",
            fmt_day(busiest)
        );
        let at = key.find(&summary).expect(&text);
        assert_eq!(
            display_width(&key[..at]),
            40,
            "the mock's summary column: {key}"
        );
    }
}

fn held_frame(stats: &Stats, assists: &AssistStats, active: Window, today: i64) -> String {
    dashboard::Dashboard {
        stats,
        assists: &dashboard::TabAssists {
            tabs: std::array::from_fn(|_| assists.clone()),
            earliest: None,
            ..Default::default()
        },
        today,
        dollars: false,
        glyphs: &panel_glyphs(),
    }
    .frame(active, 119, None)
    .text
}

fn effort_role(tally: SpendTally, effort: EffortWindow) -> rimz::agents::spending::RoleTally {
    rimz::agents::spending::RoleTally {
        tally,
        effort: EffortTally {
            day: effort,
            week: effort,
            month: effort,
            year: effort,
        },
    }
}

fn team_cache() -> ProviderSpendingCache {
    let mut cache = ProviderSpendingCache {
        timezone: "UTC".into(),
        refreshed_at_ms: (20_000 * 86_400 + 15 * 3_600) * 1_000,
        ..Default::default()
    };
    cache.days.insert(20_000, day(1_000, 100.0));
    cache.spending.total = every_window(tally(1_000, 100.0, 10));
    cache.roles.recorded_since_secs = Some(20_000 * 86_400 + 12 * 3_600);
    cache.roles.effort_since_secs = Some(20_000 * 86_400 + 13 * 3_600);
    cache.roles.agents.insert(
        "solo".into(),
        effort_role(
            every_window(tally(100, 20.0, 1)),
            EffortWindow {
                active_ms: 45_000,
                ..Default::default()
            },
        ),
    );
    cache.roles.subagents.insert(
        "child".into(),
        effort_role(
            every_window(tally(100, 20.0, 3)),
            EffortWindow {
                active_ms: 2_460_000,
                ..Default::default()
            },
        ),
    );
    cache.roles.teams.insert(
        "recon".into(),
        [
            ("coder", 30.0, 3, 10_800_000, 0, 5, 2),
            ("planner", 20.0, 2, 7_200_000, 1, 2, 3),
            ("reviewer", 10.0, 1, 4_320_000, 0, 2, 4),
        ]
        .into_iter()
        .map(
            |(role, usd, sessions, active_ms, from_user, from_teammates, to_teammates)| {
                (
                    role.into(),
                    effort_role(
                        every_window(tally(100, usd, sessions)),
                        EffortWindow {
                            active_ms,
                            from_user,
                            from_teammates,
                            to_teammates,
                        },
                    ),
                )
            },
        )
        .collect(),
    );
    cache
}

#[test]
fn teams_sum_seats_and_render_effort_without_role_rows() {
    for (from_user, from_teammates, messages) in [
        (1, 9, "10 (1 you)"),
        (56, 1_178, "1234 (56 you)"),
        (0, 10, "10"),
        (0, 0, "-"),
    ] {
        let mut cache = team_cache();
        for seat in cache.roles.teams.get_mut("recon").unwrap().values_mut() {
            seat.effort.day.from_user = 0;
            seat.effort.day.from_teammates = 0;
        }
        let seat = cache
            .roles
            .teams
            .get_mut("recon")
            .unwrap()
            .get_mut("coder")
            .unwrap();
        seat.effort.day.from_user = from_user;
        seat.effort.day.from_teammates = from_teammates;
        let stats = Stats::from_provider(cache);
        for held in [false, true] {
            let text = full_panel_text(&stats, held);
            if held {
                let row = text
                    .lines()
                    .find(|line| line.contains("recon"))
                    .expect(&text);
                assert!(text.contains("AGENT-TIME") && text.contains("MESSAGES"));
                assert!(
                    row.contains("$60") && row.contains("100%") && row.contains("6h12m"),
                    "{row}"
                );
                assert!(row.contains(messages), "{row}");
                for role in ["coder", "planner", "reviewer"] {
                    assert!(!text.contains(role), "{text}");
                }
                continue;
            }
            let agents = text.find("Agents (").expect(&text);
            let teams = text.find("Teams (").expect(&text);
            let children = text.find("Subagents").expect(&text);
            assert!(agents < teams && teams < children, "{text}");
            let row = text
                .lines()
                .find(|line| line.contains("recon"))
                .expect(&text);
            assert!(row.contains("$60") && row.contains("60.0%"), "{row}");
            for role in ["coder", "planner", "reviewer"] {
                assert!(
                    !text.contains(role),
                    "panels never unfold team roles: {text}"
                );
            }
            {
                assert!(
                    text.contains("AGENT-TIME") && text.contains("MESSAGES"),
                    "{text}"
                );
                let cells: Vec<_> = row.split_whitespace().collect();
                assert_eq!(&cells[..5], ["recon", "$60", "60.0%", "6", "6h12m"]);
                assert_eq!(
                    row.split("6h12m")
                        .nth(1)
                        .unwrap()
                        .trim_start()
                        .split("300")
                        .next()
                        .unwrap()
                        .trim(),
                    messages
                );
                let header = text
                    .lines()
                    .find(|line| line.contains("MESSAGES"))
                    .expect(&text);
                assert_eq!(
                    row.find(" 300").unwrap() + " 300".len(),
                    header.find("TOKENS").unwrap() + "TOKENS".len(),
                    "TOKENS stays aligned past a wide MESSAGES cell:\n{header}\n{row}"
                );
            }
        }
    }
}

#[test]
fn teams_json_preserves_role_effort_in_every_window() {
    let stats = Stats::from_provider(team_cache());
    for window in Window::TABS {
        let value = json::stats_json_value(&stats, &AssistStats::default(), 20_000, false, window);
        assert_eq!(value["agents"].as_array().unwrap().len(), 1);
        assert_eq!(value["agents"][0]["profile"], "solo");
        assert_eq!(value["agents"][0]["active_secs"], 45);
        assert_eq!(value["subagents"][0]["active_secs"], 2460);
        let team = &value["teams"][0];
        assert_eq!(
            keys(team),
            [
                "active_secs",
                "cache_hit_pct",
                "cached",
                "input",
                "messages",
                "output",
                "roles",
                "sessions",
                "share",
                "team",
                "tokens",
                "usd"
            ]
        );
        assert_eq!(team["team"], "recon");
        assert_eq!(team["sessions"], 6);
        assert_eq!(team["active_secs"], 22_320);
        assert_eq!(team["usd"], 60.0);
        assert_eq!(team["tokens"], 300);
        assert_eq!(team["share"], 0.6);
        assert_eq!(
            team["messages"],
            serde_json::json!({"total": 10, "from_user": 1})
        );
        let roles = team["roles"].as_array().unwrap();
        assert_eq!(roles.len(), 3);
        assert_eq!(roles[0]["role"], "coder");
        assert_eq!(
            keys(&roles[0]),
            [
                "active_secs",
                "cache_hit_pct",
                "cached",
                "from_teammates",
                "from_user",
                "input",
                "output",
                "role",
                "sessions",
                "share",
                "to_teammates",
                "tokens",
                "usd"
            ]
        );
        assert_eq!(roles[0]["from_teammates"], 5);
        assert_eq!(roles[0]["to_teammates"], 2);
        assert_eq!(roles[0]["share"], 0.3);
        assert_eq!(value["effort_since"], "2024-10-04");
    }
}

#[test]
fn effort_only_rows_survive_panels_and_json() {
    for effort in [
        EffortWindow {
            active_ms: 500,
            ..Default::default()
        },
        EffortWindow {
            from_user: 1,
            ..Default::default()
        },
        EffortWindow {
            to_teammates: 1,
            ..Default::default()
        },
    ] {
        let mut cache = team_cache();
        cache.days.clear();
        cache.spending.total = SpendTally::default();
        for seat in cache
            .roles
            .agents
            .values_mut()
            .chain(cache.roles.subagents.values_mut())
            .chain(
                cache
                    .roles
                    .teams
                    .values_mut()
                    .flat_map(|roles| roles.values_mut()),
            )
        {
            *seat = effort_role(SpendTally::default(), effort);
        }
        let stats = Stats::from_provider(cache);
        for held in [false, true] {
            let text = full_panel_text(&stats, held);
            let sections = if held {
                &["Agents", "Subagents"][..]
            } else {
                &["Agents", "Teams", "Subagents"][..]
            };
            for section in sections {
                assert!(
                    text.contains(section),
                    "effort alone admits {section}: {text}"
                );
            }
        }
        let value = json::stats_json_value(
            &stats,
            &AssistStats::default(),
            20_000,
            false,
            Window::Today,
        );
        for section in ["agents", "teams", "subagents"] {
            assert_eq!(value[section].as_array().unwrap().len(), 1, "{value}");
        }
    }
}

#[test]
fn teams_cap_compact_rows_and_keep_coverage_above_the_first_section() {
    let mut cache = team_cache();
    let roles = cache.roles.teams["recon"].clone();
    cache.roles.teams = (0..5)
        .map(|i| (format!("team-{i}"), roles.clone()))
        .collect();
    cache.roles.agents.clear();
    cache.roles.subagents.clear();
    let stats = Stats::from_provider(cache.clone());
    let text = full_panel_text(&stats, false);
    assert!(text.contains("2 other teams"), "{text}");
    for held in [false, true] {
        let text = full_panel_text(&stats, held);
        if held {
            assert!(!text.contains("agent-time and messages"));
            assert!(text.contains("Where it went · roles recorded since Oct 04"));
            continue;
        }
        let note = text
            .find("agent-time and messages since Oct 04")
            .expect(&text);
        assert!(note < text.find("Teams (today)").unwrap(), "{text}");
        assert!(text.find("roles recorded since").unwrap() < note, "{text}");
    }
    cache.roles.recorded_since_secs = None;
    cache.roles.effort_since_secs = Some(20_000 * 86_400);
    assert!(!full_panel_text(&Stats::from_provider(cache), false).contains("recorded since"));
}

#[test]
fn agent_sections_show_only_real_profiles_in_the_selected_window() {
    let bare = every_window(tally(100, 10.0, 1));
    for (profile, show_one_shot, show_held_profile) in [
        (tally(100, 20.0, 1), false, false),
        (every_window(tally(100, 20.0, 1)), true, true),
        (every_window(tally(100, 0.0, 1)), false, true),
        (SpendTally::default(), false, false),
    ] {
        let stats = role_stats(
            BTreeMap::from([
                ("removed-plugin".into(), bare.clone()),
                ("builder".into(), profile),
            ]),
            BTreeMap::from([("removed-plugin".into(), bare.clone())]),
        );
        for held in [false, true] {
            let text = full_panel_text(&stats, held);
            if held {
                assert!(text.contains("Agents"));
                assert_eq!(text.contains("builder"), show_held_profile, "{text}");
                continue;
            }
            let agents = "Agents (";
            let show = show_one_shot;
            assert_eq!(text.contains(agents), show, "{text}");
            assert!(text.contains("Providers"), "{text}");
            assert!(
                text.contains("Subagents"),
                "CLI-named subagents stay visible: {text}"
            );
            if show {
                let section = text
                    .split(agents)
                    .nth(1)
                    .unwrap()
                    .split("Subagents")
                    .next()
                    .unwrap();
                assert!(section.contains("removed-plugin"), "{section}");
                assert!(section.contains("builder"), "{section}");
                for column in ["SESSIONS", "TOKENS", "HIT", "TOK/S"] {
                    assert!(section.contains(column), "{section}");
                }
            }
        }
    }
}

#[test]
fn role_note_precedes_first_visible_profile_section() {
    for (agents, subagents, last) in [
        (true, true, Some("Subagents")),
        (true, false, Some("Agents")),
        (false, false, None),
    ] {
        let profiles = BTreeMap::from([("builder".into(), every_window(tally(100, 20.0, 1)))]);
        let stats = role_stats(
            if agents {
                profiles.clone()
            } else {
                BTreeMap::new()
            },
            if subagents { profiles } else { BTreeMap::new() },
        );
        for held in [false, true] {
            let text = full_panel_text(&stats, held);
            if held {
                let note = text
                    .find("Where it went · roles recorded since Oct 04")
                    .expect(&text);
                if let Some(last) = last {
                    assert!(note < text.rfind(last).unwrap(), "{text}");
                }
                assert!(!text.contains("earlier spend counts as No role"));
                continue;
            }
            if let Some(last) = last {
                let note = text.find("roles recorded since Oct 04").expect(&text);
                assert!(note < text.find(last).unwrap(), "{text}");
                assert!(text.contains("earlier spend counts as No role"), "{text}");
                assert!(!text.contains("subagent roles recorded"), "{text}");
            } else {
                assert!(!text.contains("recorded since"), "{text}");
            }
        }
    }
}

#[test]
fn empty_history_held_frame_omits_role_note() {
    let mut stats = role_stats(BTreeMap::new(), BTreeMap::new());
    stats.by_day.clear();
    let text = full_panel_text(&stats, true);
    assert!(text.contains("No agent sessions recorded on this machine yet."));
    assert!(!text.contains("recorded since"), "{text}");
}

#[test]
fn providers_keep_compact_columns_and_counted_tail() {
    let mut stats = role_stats(BTreeMap::new(), BTreeMap::new());
    stats.by_provider = (0..5)
        .map(|i| (format!("provider-{i}"), every_window(tally(100, 20.0, 1))))
        .collect();
    let text = full_panel_text(&stats, false);
    assert!(text.contains("Providers (today)"), "{text}");
    let section = text.split("Providers (today)").nth(1).expect(&text);
    assert!(
        section.contains("SESSIONS")
            && section.contains("TOKENS")
            && section.contains("HIT")
            && section.contains("TOK/S"),
        "{section}"
    );
    assert!(section.contains("2 other providers"), "{section}");
}

#[test]
fn profile_agents_json_is_unfolded_and_counts_sessions() {
    let mut profile = model_tally(100, 20.0, 60, 40, 100);
    profile.year.sessions = 1;
    let profile = every_window(profile);
    let profiles = BTreeMap::from([
        ("builder".into(), profile.clone()),
        ("alpha".into(), profile),
        ("removed-plugin".into(), every_window(tally(100, 10.0, 1))),
        ("zero".into(), SpendTally::default()),
        ("old".into(), tally(100, 50.0, 1)),
    ]);
    let stats = role_stats(profiles, BTreeMap::new());
    let value = json::stats_json_value(
        &stats,
        &AssistStats::default(),
        20_000,
        false,
        Window::Today,
    );
    assert_eq!(value["agents"].as_array().unwrap().len(), 3);
    assert_eq!(value["agents"][0]["profile"], "alpha");
    assert_eq!(value["agents"][1]["profile"], "builder");
    assert_eq!(value["agents"][2]["profile"], "removed-plugin");
    assert_eq!(
        keys(&value["agents"][0]),
        [
            "active_secs",
            "cache_hit_pct",
            "cached",
            "input",
            "output",
            "profile",
            "sessions",
            "share",
            "tokens",
            "usd"
        ]
    );
    assert_eq!(value["agents"][0]["sessions"], 1);
    assert_eq!(value["agents"][0]["share"], 0.2);
    assert_eq!(value["agents"][0]["tokens"], 200);
    assert_eq!(value["roles_since"], "2024-10-04");
    assert!(value.get("subagents_since").is_none());
}

#[test]
fn explicit_role_views_are_unfolded_and_keep_provider_named_rows() {
    let profiles = BTreeMap::from([
        ("removed-plugin".into(), every_window(tally(100, 10.0, 2))),
        ("alpha".into(), every_window(tally(100, 20.0, 3))),
        ("beta".into(), every_window(tally(100, 15.0, 1))),
        ("gamma".into(), every_window(tally(100, 5.0, 1))),
        ("zero".into(), SpendTally::default()),
    ]);
    let stats = role_stats(profiles.clone(), profiles);
    let panel = full_panel_text(&stats, false);
    assert!(panel.contains("1 other agent "), "{panel}");
    assert!(panel.contains("1 other subagent "), "{panel}");
    for (role, key, count) in [
        (RoleView::Agents, "agents", "sessions"),
        (RoleView::Subagents, "subagents", "runs"),
    ] {
        let value = json::roles_value(&stats, Window::Today, role);
        let mut expected = ["as_of", key, "roles_since", "window"];
        expected.sort_unstable();
        assert_eq!(keys(&value), expected);
        assert_eq!(value["roles_since"], "2024-10-04");
        assert_eq!(value["window"], "today");
        assert_eq!(value[key].as_array().unwrap().len(), 4);
        assert_eq!(value[key][0]["profile"], "alpha");
        assert_eq!(value[key][0][count], 3);
        assert_eq!(value[key][2]["profile"], "removed-plugin");
        let mut out = Vec::new();
        panel::render_spend(
            &mut out,
            &stats,
            20_000,
            false,
            Some(role),
            Window::Today,
            "\n",
        )
        .unwrap();
        let text = strip_ansi(std::str::from_utf8(&out).unwrap());
        for name in ["alpha", "beta", "gamma", "removed-plugin"] {
            assert!(text.contains(name), "{text}");
        }
        for column in [
            "INPUT",
            "OUTPUT",
            "CACHED",
            "HIT",
            &count.to_ascii_uppercase(),
            "AGENT-TIME",
        ] {
            assert!(text.contains(column), "{text}");
        }
        let end_column = |needle: &str| {
            let line = text.lines().find(|line| line.contains(needle)).unwrap();
            line.trim_end().chars().count()
        };
        assert_eq!(
            end_column("AGENT-TIME"),
            end_column("alpha"),
            "the agent-time header ends where its values end: {text}"
        );
        assert!(!text.contains("other "), "{text}");
        assert!(!text.contains("Spend activity"), "{text}");
        assert!(text.contains("roles recorded since Oct 04"), "{text}");
    }

    let bare = BTreeMap::from([("removed-plugin".into(), every_window(tally(100, 10.0, 1)))]);
    let stats = role_stats(bare, BTreeMap::new());
    for held in [false, true] {
        let heading = if held { "Agents\n" } else { "Agents (" };
        assert!(!full_panel_text(&stats, held).contains(heading));
    }
    let value = json::stats_json_value(
        &stats,
        &AssistStats::default(),
        20_000,
        false,
        Window::Today,
    );
    assert_eq!(value["agents"][0]["profile"], "removed-plugin");
    assert_eq!(
        json::roles_value(&stats, Window::Today, RoleView::Agents)["agents"],
        value["agents"]
    );
    let mut out = Vec::new();
    panel::render_spend(
        &mut out,
        &stats,
        20_000,
        false,
        Some(RoleView::Agents),
        Window::Today,
        "\n",
    )
    .unwrap();
    let text = strip_ansi(std::str::from_utf8(&out).unwrap());
    assert!(text.contains("removed-plugin"), "{text}");
    assert!(text.contains("SESSIONS"), "{text}");
    assert!(
        !text.contains("Providers ("),
        "explicit views contain only profiles: {text}"
    );
}

#[test]
fn empty_explicit_role_views_name_the_window_and_keep_the_recording_date() {
    let mut stats = role_stats(BTreeMap::new(), BTreeMap::new());
    for (role, noun, key) in [
        (RoleView::Agents, "agent", "agents"),
        (RoleView::Subagents, "subagent", "subagents"),
    ] {
        let mut out = Vec::new();
        panel::render_spend(
            &mut out,
            &stats,
            20_000,
            false,
            Some(role),
            Window::Week,
            "\n",
        )
        .unwrap();
        let text = strip_ansi(std::str::from_utf8(&out).unwrap());
        assert!(
            text.contains(&format!("No {noun} spend recorded (week).")),
            "{text}"
        );
        assert!(text.contains("roles recorded since Oct 04"), "{text}");
        assert!(text.contains("as of"), "{text}");
        assert_eq!(
            json::roles_value(&stats, Window::Week, role)[key],
            serde_json::json!([])
        );
    }
    stats.recorded_since_secs = None;
    for (role, key) in [
        (RoleView::Agents, "agents"),
        (RoleView::Subagents, "subagents"),
    ] {
        let mut expected = ["as_of", key, "window"];
        expected.sort_unstable();
        assert_eq!(
            keys(&json::roles_value(&stats, Window::Today, role)),
            expected
        );
    }
}

#[test]
fn models_use_the_producing_kind_identity_on_both_surfaces() {
    let mut cache = ProviderSpendingCache {
        timezone: "UTC".into(),
        ..Default::default()
    };
    let mut spend = model_tally(100, 20.0, 60, 40, 0);
    spend.day = spend.year.clone();
    cache.models.insert("gpt-5".into(), produced(spend.clone()));
    cache.spending.total = spend;
    cache.days.insert(20_000, day(100, 20.0));
    let stats = Stats::from_provider(cache);
    let expected = render::paint(render::palette::identity("codex"), "GPT 5");
    let mut plain = Vec::new();
    panel::render_spend(&mut plain, &stats, 20_000, false, None, Window::Today, "\n").unwrap();
    assert!(String::from_utf8(plain).unwrap().contains(&expected));
    assert!(held_frame(&stats, &AssistStats::default(), Window::Today, 20_000).contains(&expected));
}

#[test]
fn one_shot_refreshes_previous_local_day_but_serves_today() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(rimz::WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    ensure_shared_runtime(&runtime).unwrap();
    let _discovery = rimz::agents::spending::override_discovered_spending_files_for_test(vec![]);
    let mut cache = ProviderSpendingCache {
        timezone: "Asia/Shanghai".into(),
        refreshed_at_ms: Timestamp::now().as_millisecond() as u64,
        day_cutoff_secs: rimz::agents::spending::local_day_start_secs(
            unix_secs_now(),
            Some("Asia/Shanghai"),
        )
        .unwrap(),
        ..Default::default()
    };
    cache.spending.total = tally(100, 99.0, 1);
    rimz::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        &cache,
    );
    let today = load_stats_from_paths(&runtime, false).unwrap();
    assert_eq!(today.total.year.usd, 99.0);
    cache.day_cutoff_secs -= 86_400;
    rimz::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        &cache,
    );
    assert!(
        load_today_stats(&runtime).is_none(),
        "a walk ending today must not make yesterday's windows current"
    );
    assert!(load_stats_from_paths(&runtime, false).is_err());
    cache.refreshed_at_ms -= 86_400_000;
    rimz::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        &cache,
    );
    assert!(
        load_stats_from_paths(&runtime, false).is_err(),
        "a failed discovery must not return yesterday's publication"
    );
    let transcript = dir.path().join("empty.jsonl");
    std::fs::write(&transcript, b"").unwrap();
    let adapter = rimz::agents::definition_by_kind("claude").unwrap();
    let _discovery = rimz::agents::spending::override_discovered_spending_files_for_test(vec![
        rimz::agents::spending::SpendingFile {
            adapter,
            login: rimz::ids::LoginKey::default_for(adapter.spec().kind_id()),
            path: transcript,
        },
    ]);
    let refreshed = load_stats_from_paths(&runtime, false).unwrap();
    assert_eq!(refreshed.total.year.usd, 0.0);
    assert_eq!(
        refreshed.today_day(refreshed.refreshed_at_ms / 1000),
        refreshed.today_day(unix_secs_now())
    );
}

#[test]
fn roles_note_includes_the_partial_oldest_day_of_a_trailing_window() {
    let published = "2026-10-17T15:00:00Z".parse::<Timestamp>().unwrap();
    let since = "2026-10-11T12:00:00Z".parse::<Timestamp>().unwrap();
    let mut cache = ProviderSpendingCache {
        timezone: "UTC".into(),
        refreshed_at_ms: published.as_millisecond() as u64,
        ..Default::default()
    };
    cache.roles.recorded_since_secs = Some(since.as_second() as u64);
    let mut stats = Stats::from_provider(cache);
    let today = stats.today_day(published.as_second() as u64);
    let mut out = Vec::new();
    panel::render_spend(
        &mut out,
        &stats,
        today,
        false,
        Some(RoleView::Subagents),
        Window::Week,
        "\n",
    )
    .unwrap();
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("recorded since Oct 11"),
        "the week also includes Oct 10 afternoon, before roles were recorded"
    );
    stats.recorded_since_secs =
        Some((published - Duration::from_secs(7 * 86_400)).as_second() as u64);
    let mut out = Vec::new();
    panel::render_spend(
        &mut out,
        &stats,
        today,
        false,
        Some(RoleView::Subagents),
        Window::Week,
        "\n",
    )
    .unwrap();
    assert!(!String::from_utf8(out).unwrap().contains("recorded since"));
}

#[test]
fn provider_breakdowns_default_to_today() {
    let mut cache = ProviderSpendingCache {
        timezone: "UTC".into(),
        ..Default::default()
    };
    let mut provider = tally(500, 50.0, 5);
    provider.day = spend_window(100, 10.0, 60, 40, 0, 1);
    provider.day.timed_ms = 2_000;
    provider.day.timed_output = 40;
    cache.spending.total = provider.clone();
    cache.spending.by_provider.insert("codex".into(), provider);
    cache.days.insert(20_000, day(100, 10.0));
    let stats = Stats::from_provider(cache);
    assert_eq!(stats.by_provider["codex"].day.usd, 10.0);
    let value = json::stats_json_value(
        &stats,
        &AssistStats::default(),
        20_000,
        false,
        Window::Today,
    );
    assert_eq!(value["window"], "today");
    assert_eq!(value["providers"][0]["usd"], 10.0);
    assert_eq!(value["providers"][0]["tokens_per_sec"], 20);
    assert_eq!(value["sessions"], 1);
    let mut out = Vec::new();
    panel::render_spend(&mut out, &stats, 20_000, false, None, Window::Today, "\n").unwrap();
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("Providers (today)")
    );
}

#[test]
fn spend_first_publication_has_compact_json_and_panel() {
    let mut cache = ProviderSpendingCache {
        timezone: "Asia/Shanghai".into(),
        refreshed_at_ms: 1_728_921_600_000,
        ..Default::default()
    };
    cache.spending.total = every_window(tally(1_000_000, 100.0, 10));
    cache.days.insert(20_011, day(1_000_000, 100.0));
    cache
        .spending
        .by_provider
        .insert("codex".into(), every_window(tally(100, 60.0, 9)));
    cache.roles.subagents.insert(
        "fast".into(),
        role_tally(every_window(model_tally(
            2_000_000, 40.0, 1_500_000, 500_000, 3_000_000,
        ))),
    );
    for index in 0..5 {
        cache.models.insert(
            format!("model-{index}"),
            produced(every_window(tally(100, 20.0, 1))),
        );
    }
    cache
        .models
        .insert("zero".into(), produced(tally(100, 0.0, 1)));
    let mut publication = serde_json::to_value(cache).unwrap();
    publication["roles"]["recorded_since_secs"] = serde_json::json!(
        "2024-10-14T00:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .as_second()
    );
    let mut stats = Stats::from_provider(serde_json::from_value(publication).unwrap());
    let assists = AssistStats::default();
    let value = json::stats_json_value(&stats, &assists, 20_011, false, Window::Today);
    assert!(value["assists"].get("events").is_none());
    assert_eq!(value["timezone"], "Asia/Shanghai");
    assert!(value["as_of"].is_string());
    assert!(value["windows"]["today"].is_object());
    assert_eq!(value["window"], "today");
    assert_eq!(value["roles_since"], "2024-10-14");
    assert_eq!(value["providers"][0]["usd"], 60.0);
    assert_eq!(value["providers"][0]["share"], 0.6);
    assert!(value["providers"][0].get("tokens_per_sec").is_none());
    assert_eq!(value["agents"], serde_json::json!([]));
    assert_eq!(value["models"].as_array().unwrap().len(), 5);
    assert_eq!(value["subagents"][0]["profile"], "fast");
    assert_eq!(value["subagents"][0]["share"], 0.4);
    assert_eq!(
        keys(&value),
        [
            "active_days_28",
            "agents",
            "as_of",
            "assists",
            "current_streak",
            "days",
            "longest_streak",
            "models",
            "most_active_day",
            "providers",
            "roles_since",
            "sessions",
            "subagents",
            "teams",
            "timezone",
            "unit",
            "window",
            "windows"
        ]
    );
    assert_eq!(
        keys(&value["subagents"][0]),
        [
            "active_secs",
            "cache_hit_pct",
            "cached",
            "input",
            "output",
            "profile",
            "runs",
            "share",
            "tokens",
            "usd"
        ]
    );
    let envelope = json::roles_value(&stats, Window::Today, RoleView::Subagents);
    assert_eq!(
        keys(&envelope),
        ["as_of", "roles_since", "subagents", "window"]
    );
    assert_eq!(envelope["as_of"], "2024-10-15T00:00:00+08:00");
    assert_eq!(envelope["subagents"], value["subagents"]);
    let mut out = Vec::new();
    panel::render_spend(&mut out, &stats, 20_012, false, None, Window::Today, "\n").unwrap();
    let panel = strip_ansi(std::str::from_utf8(&out).unwrap());
    for label in [
        "Spend",
        "Today",
        "Week",
        "Month",
        "Year",
        "Models (today)",
        "Providers (today)",
        "Subagents (today)",
        "SHARE",
        "2 other models",
        "TOK/S",
        "RUNS",
        "as of",
    ] {
        assert!(panel.contains(label), "missing {label}: {panel}");
    }
    assert!(!panel.contains(WORDMARK.lines().next().unwrap()));
    assert!(!panel.contains("zero"));
    assert!(
        panel
            .lines()
            .any(|row| row.contains("Codex") && row.contains("$60"))
    );
    assert!(!panel.contains("MODEL"));
    assert!(!panel.contains("recorded since"));
    let mut year = Vec::new();
    panel::render_spend(&mut year, &stats, 20_012, false, None, Window::Year, "\n").unwrap();
    assert!(strip_ansi(std::str::from_utf8(&year).unwrap()).contains("recorded since Oct 14"));
    assert!(panel.contains("as of Oct"));
    let mut today = Vec::new();
    panel::render_spend(
        &mut today,
        &stats,
        stats.today_day(stats.refreshed_at_ms / 1000),
        false,
        Some(RoleView::Subagents),
        Window::Today,
        "\n",
    )
    .unwrap();
    let today = String::from_utf8(today).unwrap();
    assert!(today.contains("1.5M"));
    assert!(!today.contains("as of Oct"));
    assert!(!today.contains("Models (year)"));
    assert!(today.contains("RUNS"));
    assert!(!today.contains("MODEL"));
    let held = held_frame(&stats, &assists, Window::Year, 20_011);
    assert!(held.contains(TAGLINE));
    assert!(held.contains("Subagents"));
    assert!(strip_ansi(&held).contains("recorded since Oct 14"));
    stats.subagents.clear();
    let mut empty_profiles = Vec::new();
    panel::render_spend(
        &mut empty_profiles,
        &stats,
        20_100,
        false,
        None,
        Window::Year,
        "\n",
    )
    .unwrap();
    assert!(!strip_ansi(std::str::from_utf8(&empty_profiles).unwrap()).contains("recorded since"));
    let mut explicit = Vec::new();
    panel::render_spend(
        &mut explicit,
        &stats,
        20_100,
        false,
        Some(RoleView::Subagents),
        Window::Year,
        "\n",
    )
    .unwrap();
    assert!(
        strip_ansi(std::str::from_utf8(&explicit).unwrap())
            .contains("No subagent spend recorded (year).")
    );
    assert!(
        strip_ansi(std::str::from_utf8(&explicit).unwrap()).contains("recorded since Oct 14 2024")
    );
}

fn every_window(mut tally: SpendTally) -> SpendTally {
    tally.day = tally.year.clone();
    tally.week = tally.year.clone();
    tally.month = tally.year.clone();
    tally
}

fn produced(tally: SpendTally) -> ModelTally {
    ModelTally {
        tally,
        kind: "codex".into(),
    }
}

fn model_map<const N: usize>(models: [(String, SpendTally); N]) -> BTreeMap<String, ModelTally> {
    models
        .into_iter()
        .map(|(id, tally)| (id, produced(tally)))
        .collect()
}

#[test]
fn week_scopes_plain_and_json_breakdowns_and_shares() {
    let mut cache = ProviderSpendingCache {
        timezone: "UTC".into(),
        ..Default::default()
    };
    cache.spending.total = every_window(tally(1_000, 100.0, 10));
    cache.spending.total.week = spend_window(200, 20.0, 100, 100, 0, 2);
    let mut row = every_window(tally(500, 50.0, 5));
    row.week = spend_window(50, 5.0, 30, 20, 0, 1);
    cache.models.insert("gpt-5".into(), produced(row.clone()));
    cache
        .roles
        .subagents
        .insert("fast".into(), role_tally(row.clone()));
    cache
        .models
        .insert("old".into(), produced(tally(500, 50.0, 5)));
    cache
        .roles
        .subagents
        .insert("old".into(), role_tally(tally(500, 50.0, 5)));
    cache.days = BTreeMap::from([
        (20_000, day(100, 10.0)),
        (19_990, day(100, 10.0)),
        (19_900, day(900, 90.0)),
    ]);
    let mut stats = Stats::from_provider(cache);
    stats.by_provider.insert("codex".into(), row);
    let mut year_only = tally(500, 50.0, 5);
    year_only.week = SpendWindow::default();
    stats.by_provider.insert("old".into(), year_only);
    let mut out = Vec::new();
    panel::render_spend(&mut out, &stats, 20_000, false, None, Window::Week, "\n").unwrap();
    let text = strip_ansi(std::str::from_utf8(&out).unwrap());
    for label in ["Models (week)", "Providers (week)", "Subagents (week)"] {
        assert!(text.contains(label), "{text}");
    }
    for name in ["GPT 5", "Codex", "fast"] {
        assert!(
            text.lines()
                .any(|line| line.contains(name) && line.contains("$5") && line.contains("25.0%")),
            "{text}"
        );
    }
    let value =
        json::stats_json_value(&stats, &AssistStats::default(), 20_000, false, Window::Week);
    assert_eq!(value["window"], "week");
    assert_eq!(value["sessions"], 2);
    assert_eq!(value["active_days_28"], 2);
    assert_eq!(value["most_active_day"], utc_date(19_900 * DAY_SECS as u64));
    for section in ["models", "providers", "subagents"] {
        assert_eq!(value[section].as_array().unwrap().len(), 1);
        assert_eq!(value[section][0]["usd"], 5.0);
        assert_eq!(value[section][0]["share"], 0.25);
    }
    let envelope = json::roles_value(&stats, Window::Week, RoleView::Subagents);
    assert_eq!(keys(&envelope), ["as_of", "subagents", "window"]);
    assert_eq!(envelope["window"], "week");
    assert_eq!(envelope["subagents"], value["subagents"]);
}

#[test]
fn refresh_q_respects_hold() {
    let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    assert_eq!(key_outcome(key, false), KeyOutcome::Quit);
    assert_eq!(key_outcome(key, true), KeyOutcome::Ignore);
}

#[test]
fn queued_stall_notices_count_and_failed_ones_are_held() {
    let records = [
        serde_json::json!({"delivered": true}),
        serde_json::json!({"delivered": false}),
        serde_json::json!({"delivered": false, "error": "queue write failed"}),
    ]
    .into_iter()
    .map(|outcome| {
        let mut record = serde_json::json!({
            "at": "2026-01-01T00:00:00Z", "assist": "stall_notice",
            "kind": "codex", "agent_id": "session", "parent": "parent",
            "silent_secs": 60, "message_id": "message"
        });
        record
            .as_object_mut()
            .unwrap()
            .extend(outcome.as_object().unwrap().clone());
        serde_json::from_value(record).unwrap()
    })
    .collect();
    let stats = AssistStats::from_records("all", records);
    assert_eq!(stats.rollup.stall_notices, 2, "a queued notice took effect");
    assert_eq!(stats.rollup.held.get("stall_notices"), Some(&1));
    assert_eq!(
        stats.timeline(50, false).0.len(),
        stats.rollup.stall_notices
    );
    assert_eq!(stats.timeline(0, true).0.len(), 3);
    assert_eq!(stats.timeline(1, true).1, 2);
    assert!(
        assists::category_rows(&stats.rollup, assists::CategoryMode::Timeline)
            .iter()
            .any(|row| strip_ansi(row) == "Stall notice: 2 (1 held)")
    );
}

fn day(tokens: u64, usd: f64) -> DaySpend {
    DaySpend { tokens, usd }
}

#[test]
fn judge_declines_include_legacy_assist_records() {
    let records = ["judge_decline", "check_decline"].map(|tag| {
        let record =
            serde_json::from_value::<rimz::harness::assist_log::AssistRecord>(serde_json::json!({
                "at": "2026-06-01T00:00:00Z", "assist": tag, "task": "guard",
                "checkout": "/repo", "profile": "haiku", "kind": "claude", "reason": "no work"
            }));
        assert!(record.is_ok(), "both assist tags must read: {record:?}");
        record.unwrap()
    });
    let stats = assists::AssistStats::from_records("all", records.into());
    assert_eq!(
        serde_json::to_value(&stats.rollup).unwrap()["judge_declines"],
        2
    );
    assert!(
        assists::category_rows(&stats.rollup, assists::CategoryMode::Timeline)
            .join("\n")
            .contains("Judge declines:")
    );
    for event in stats.events {
        assert_eq!(
            serde_json::to_value(event).unwrap()["assist"],
            "judge_decline"
        );
    }
}

fn spend_window(
    tokens: u64,
    usd: f64,
    input: u64,
    output: u64,
    cache_read: u64,
    sessions: u32,
) -> SpendWindow {
    SpendWindow {
        usd,
        tokens,
        input,
        output,
        cache_read,
        sessions,
        ..Default::default()
    }
}

fn tally(tokens: u64, usd: f64, sessions: u32) -> SpendTally {
    SpendTally {
        year: spend_window(tokens, usd, 0, 0, 0, sessions),
        ..Default::default()
    }
}

fn model_tally(tokens: u64, usd: f64, input: u64, output: u64, cache_read: u64) -> SpendTally {
    SpendTally {
        year: spend_window(tokens, usd, input, output, cache_read, 0),
        ..Default::default()
    }
}

fn panel_glyphs() -> PanelGlyphs {
    resolve_panel_glyphs(&ThemeConfig::default())
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for e in chars.by_ref() {
                if e == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn write_jsonl(dir: &Path, filename: &str, lines: &[&str]) -> PathBuf {
    let path = dir.join(filename);
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    path
}

fn claude_line_today(cost: f64, msg_id: &str, req_id: &str) -> String {
    let today = utc_date(unix_secs_now());
    format!(
        r#"{{"timestamp":"{today}T10:00:00.000Z","costUSD":{cost},"requestId":"{req_id}","message":{{"id":"{msg_id}","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
    )
}

#[test]
fn monday_is_row_zero() {
    // 1970-01-05 is a Monday (epoch day 4).
    assert_eq!(dow_mon0(4), 0);
    assert_eq!(dow_mon0(5), 1); // Tuesday
    assert_eq!(dow_mon0(0), 3); // 1970-01-01 is a Thursday
    assert_eq!(week_start(10), 10 - dow_mon0(10));
}

#[test]
fn shade_spreads_active_values_against_the_ceiling() {
    assert_eq!(shade(0.0, 0.0), 0.0, "empty graph stays empty");
    assert_eq!(shade(0.0, 100.0), 0.0, "zero usage stays empty");
    assert_eq!(shade(10.0, 0.0), 0.0, "empty graph has no ceiling");
    assert_eq!(shade(100.0, 100.0), 1.0, "the ceiling is full");
    assert!(shade(50.0, 100.0) > shade(25.0, 100.0));
    assert_eq!(shade(1.0, 1000.0), 0.15);
    assert_eq!(shade(1000.0, 100.0), 1.0, "values above the ceiling clamp");
}

#[test]
fn level_maps_shaped_intensity_onto_every_active_glyph() {
    assert_eq!(level(0.0), 0);
    assert_eq!(level(0.15), 1, "trace activity clears empty");
    assert_eq!(level(0.2), 2);
    assert_eq!(level(0.5), 3);
    assert_eq!(level(0.9), 4);
    assert_eq!(level(1.0), 4);
}

#[test]
fn grid_places_today_in_the_last_column_and_blanks_the_future() {
    let today = 20_000; // arbitrary epoch day
    let mut by_day = BTreeMap::new();
    by_day.insert(today, day(100, 1.0));
    by_day.insert(today - 7, day(50, 0.5));
    let grid = Grid::build(&by_day, today, 4, false);

    assert_eq!(grid.cells.len(), 4);
    assert!((grid.ceiling - 100.0).abs() < f64::EPSILON);
    // Today sits in the final column at its weekday row.
    let row = dow_mon0(today) as usize;
    assert_eq!(grid.cells[3][row], Some(100.0));
    // Days after today in the current week are blank, not zero.
    if row < 6 {
        assert_eq!(grid.cells[3][row + 1], None);
    }
}

#[test]
fn grid_ceiling_ignores_one_spike_and_handles_one_active_day() {
    let today = 20_009; // Sunday, so both weeks are complete.
    let first_day = week_start(today) - 7;
    let mut by_day = BTreeMap::new();
    for offset in 0..9 {
        by_day.insert(first_day + offset, day(10, 0.0));
    }
    by_day.insert(first_day + 9, day(1000, 0.0));

    let grid = Grid::build(&by_day, today, 2, false);
    assert_eq!(grid.ceiling, 10.0, "p90 excludes the single spike");

    let one_day = BTreeMap::from([(today, day(42, 0.0))]);
    let grid = Grid::build(&one_day, today, 2, false);
    assert_eq!(grid.ceiling, 42.0);
}

#[test]
fn published_stats_reads_rollups_and_windows() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(rimz::WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let today = 20_000;
    let by_day = BTreeMap::from([(today - 10, day(40, 4.0))]);
    let by_model = model_map([("gpt-5-codex".to_owned(), model_tally(100, 7.0, 70, 30, 5))]);
    let mut spending = rimz::agents::spending::Spending::default();
    spending.total.week.tokens = 7;
    spending.total.week.usd = 0.7;
    spending.total.month.tokens = 30;
    spending.total.month.usd = 3.0;
    spending.total.year.tokens = 365;
    spending.total.year.usd = 36.5;
    spending.total.year.sessions = 9;
    spending
        .by_provider
        .insert("claude".to_owned(), tally(120, 12.0, 3));
    let expected_agents = spending.by_provider.clone();
    rimz::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        &rimz::agents::spending::ProviderSpendingCache {
            refreshed_at_ms: 123,
            spending,
            days: by_day.clone(),
            models: by_model.clone(),
            ..Default::default()
        },
    );

    let stats = load_published_stats(&runtime).expect("current aggregate is readable");

    assert_eq!(stats.by_day, by_day);
    assert_eq!(stats.by_model, by_model);
    assert_eq!(stats.by_provider, expected_agents);
    assert_eq!(stats.total.week.tokens, 7);
    assert_eq!(stats.total.month.usd, 3.0);
    assert_eq!(stats.total.year.sessions, 9);
    assert_eq!(stats.by_provider["claude"].year.tokens, 120);
    assert_eq!(stats.by_provider["claude"].year.sessions, 3);
}

#[test]
fn stats_serves_published_cache_without_walking() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(rimz::WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    ensure_shared_runtime(&runtime).unwrap();
    let by_day = BTreeMap::from([(20_000, day(40, 4.0))]);
    let by_model = model_map([(
        "claude-opus-4-8".to_owned(),
        model_tally(100, 7.0, 70, 30, 5),
    )]);
    let mut spending = rimz::agents::spending::Spending::default();
    spending.total.week.tokens = 7;
    spending.total.week.usd = 0.7;
    spending.total.month.tokens = 30;
    spending.total.month.usd = 3.0;
    spending.total.year.tokens = 365;
    spending.total.year.usd = 36.5;
    spending
        .by_provider
        .insert("claude".to_owned(), tally(120, 12.0, 3));
    rimz::agents::spending::write_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
        &rimz::agents::spending::ProviderSpendingCache {
            refreshed_at_ms: Timestamp::now().as_millisecond() as u64,
            day_cutoff_secs: rimz::agents::spending::local_day_start_secs(unix_secs_now(), None)
                .unwrap(),
            spending,
            days: by_day.clone(),
            models: by_model.clone(),
            ..Default::default()
        },
    );
    let cursor_path = runtime.shared_spending_cursor_path();
    assert!(!cursor_path.exists());
    let stats = load_or_refresh_stats(&runtime, None).unwrap();

    assert_eq!(stats.by_day, by_day);
    assert_eq!(stats.by_model, by_model);
    assert_eq!(stats.total.year.tokens, 365);
    assert_eq!(stats.by_provider["claude"].year.sessions, 3);
    assert!(
        !cursor_path.exists(),
        "published stats are served before transcript discovery or cursor writes"
    );
}

#[test]
fn cold_refresh_publishes_sidebar_provider_rollups() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(rimz::WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    ensure_shared_runtime(&runtime).unwrap();
    let first = write_jsonl(
        dir.path(),
        "claude-1.jsonl",
        &[&claude_line_today(1.25, "msg-1", "req-1")],
    );
    let second = write_jsonl(
        dir.path(),
        "claude-2.jsonl",
        &[&claude_line_today(2.5, "msg-2", "req-2")],
    );
    let claude = rimz::agents::definition_by_kind("claude").unwrap();
    let _discovery = rimz::agents::spending::override_discovered_spending_files_for_test(vec![
        rimz::agents::spending::SpendingFile {
            adapter: claude,
            login: rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("claude")),
            path: first.clone(),
        },
        rimz::agents::spending::SpendingFile {
            adapter: claude,
            login: rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("claude")),
            path: second.clone(),
        },
    ]);
    let mut progress = Vec::new();
    let mut record_progress = |reading| progress.push(reading);

    let stats = load_or_refresh_stats(&runtime, Some(&mut record_progress)).unwrap();
    let published = read_provider_spending_cache(&runtime.shared_provider_spending_path());
    let fresh = load_published_stats(&runtime)
        .expect("published stats are current after a stats-owned refresh");
    let cursor =
        rimz::agents::spending::read_spending_cache(&runtime.shared_spending_cursor_path());

    assert!(published.refreshed_at_ms > 0);
    assert_eq!(
        progress.first(),
        Some(&SpendProgress {
            finished_files: 0,
            total_files: 2,
        })
    );
    assert_eq!(
        progress.last(),
        Some(&SpendProgress {
            finished_files: 2,
            total_files: 2,
        })
    );
    assert!((published.spending.total.month.usd - stats.total.month.usd).abs() < 1e-9);
    assert!((fresh.total.month.usd - stats.total.month.usd).abs() < 1e-9);
    assert_eq!(
        published.spending.total.month.tokens,
        stats.total.month.tokens
    );
    assert_eq!(published.spending.by_provider, stats.by_provider);
    assert_eq!(published.roles.subagents, stats.subagents);
    for transcript in [first, second] {
        assert!(
            cursor
                .files
                .contains_key(&transcript.to_string_lossy().into_owned()),
            "stats publishes the cursor cache that makes the next run history-independent"
        );
    }
}

#[test]
fn activity_reads_streaks_active_ratio_and_busiest_day() {
    let today = 20_000;
    let mut by_day = BTreeMap::new();
    // A 5-day run ending today, then a gap, then an older 2-day run.
    for back in 0..5 {
        by_day.insert(today - back, day(10 + back as u64, 1.0));
    }
    by_day.insert(today - 10, day(99, 1.0)); // the heaviest day
    by_day.insert(today - 11, day(5, 1.0));

    let a = Activity::of(&by_day, today, Window::Year);
    assert_eq!(a.current_streak, 5);
    assert_eq!(a.longest_streak, 5);
    assert_eq!(
        (a.active_count, a.window_days),
        (7, 28),
        "all seven active days fall inside 28"
    );
    assert_eq!(a.most_active, Some(today - 10));
}

#[test]
fn current_streak_survives_an_inactive_today() {
    let today = 20_000;
    let mut by_day = BTreeMap::new();
    by_day.insert(today - 1, day(10, 1.0));
    by_day.insert(today - 2, day(10, 1.0));
    // Nothing logged today yet.
    let a = Activity::of(&by_day, today, Window::Year);
    assert_eq!(
        a.current_streak, 2,
        "a pending today does not break the streak"
    );
}

#[test]
fn activity_week_scopes_active_count_and_streaks() {
    let today = 20_000;
    let mut by_day = BTreeMap::new();
    for back in 0..5 {
        by_day.insert(today - back, day(10 + back as u64, 1.0));
    }
    by_day.insert(today - 6, day(99, 1.0));
    by_day.insert(today - 10, day(500, 1.0));

    let a = Activity::of(&by_day, today, Window::Week);

    assert_eq!((a.active_count, a.window_days), (6, 7));
    assert_eq!(a.longest_streak, 5);
    assert_eq!(a.current_streak, 5);
    assert_eq!(a.most_active, Some(today - 6));
}

#[test]
fn spend_trend_compares_only_equal_adjacent_windows() {
    let today = 20_000;
    let mut by_day = BTreeMap::new();
    for back in 0..7 {
        by_day.insert(today - back, day(1, 2.0));
        by_day.insert(today - 7 - back, day(1, 1.0));
    }
    by_day.insert(today - 14, day(1, 1_000.0));

    assert_eq!(spend_trend(&by_day, today, Window::Week), Some(100.0));

    let mut month = BTreeMap::new();
    for back in 0..30 {
        month.insert(today - back, day(1, 3.0));
        month.insert(today - 30 - back, day(1, 2.0));
    }
    assert_eq!(spend_trend(&month, today, Window::Month), Some(50.0));
}

#[test]
fn spend_trend_needs_a_supported_window_and_prior_spend() {
    let by_day = BTreeMap::from([(20_000, day(1, 10.0))]);

    assert_eq!(spend_trend(&by_day, 20_000, Window::Week), None);
    assert_eq!(spend_trend(&by_day, 20_000, Window::Year), None);
    assert_eq!(spend_trend(&by_day, 20_000, Window::Year), None);
}

#[test]
fn cold_spinner_requires_human_stdout_and_stderr_ttys() {
    assert!(should_animate_cold_stats(true, true, true));
    assert!(!should_animate_cold_stats(false, true, true));
    assert!(!should_animate_cold_stats(true, false, true));
    assert!(!should_animate_cold_stats(true, true, false));
}

#[test]
fn windows_wrap_forward_and_back() {
    assert_eq!(
        Window::TABS.map(Window::label).as_slice(),
        ["Today", "Week", "Month", "Year"]
    );
    for (index, window) in Window::TABS.into_iter().enumerate() {
        assert_eq!(window.next(), Window::TABS[(index + 1) % 4]);
        assert_eq!(window.prev(), Window::TABS[(index + 3) % 4]);
    }
}

#[test]
fn reload_flag_consumes_once() {
    let flag = AtomicBool::new(false);
    assert!(!consume_reload_flag(&flag));
    flag.store(true, Ordering::SeqCst);
    assert!(
        consume_reload_flag(&flag),
        "a set flag reads as a reload request"
    );
    assert!(
        !consume_reload_flag(&flag),
        "consuming clears it so it never latches"
    );
}

#[test]
fn refresh_key_outcome_reloads_on_r() {
    for hold in [false, true] {
        assert_eq!(
            key_outcome(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE), hold),
            KeyOutcome::Reload
        );
        assert_eq!(
            key_outcome(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT), hold),
            KeyOutcome::Reload
        );
    }
}

#[test]
fn refresh_key_outcome_switches_windows_on_tab() {
    assert_eq!(
        key_outcome(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), false),
        KeyOutcome::NextWindow
    );
    assert_eq!(
        key_outcome(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT), false),
        KeyOutcome::PrevWindow
    );
}

#[test]
fn refresh_key_outcome_ctrl_c_respects_hold() {
    assert_eq!(
        key_outcome(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            false
        ),
        KeyOutcome::Quit
    );
    assert_eq!(
        key_outcome(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            true
        ),
        KeyOutcome::Ignore
    );
    assert_eq!(
        key_outcome(
            KeyEvent::new(KeyCode::Char('C'), KeyModifiers::CONTROL),
            false
        ),
        KeyOutcome::Quit
    );
}

#[test]
fn refresh_key_outcome_ignores_other_keys() {
    assert_eq!(
        key_outcome(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE), false),
        KeyOutcome::Ignore
    );
    assert_eq!(
        key_outcome(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE), false),
        KeyOutcome::Ignore
    );
}

#[test]
fn held_stats_applies_window_keys_without_a_current_frame() {
    let mut state = HeldStats::new(false, panel_glyphs(), false, Window::Today);
    let mut out = Vec::new();

    assert_eq!(state.active(), Window::Today);
    for expected in ["Week", "Month", "Year", "Today", "Week", "Month"] {
        assert_eq!(
            state
                .apply_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut out,)
                .unwrap(),
            KeyOutcome::NextWindow
        );
        assert_eq!(state.active().label(), expected);
    }
    assert!(
        out.is_empty(),
        "window changes do not paint before first stats"
    );

    assert_eq!(
        state
            .apply_key(
                KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
                &mut out,
            )
            .unwrap(),
        KeyOutcome::PrevWindow
    );
    assert_eq!(state.active().label(), "Week");
    assert!(out.is_empty());
    assert_eq!(
        HeldStats::new(false, panel_glyphs(), false, Window::Month).active(),
        Window::Month
    );
}

#[test]
fn held_stats_preserves_reload_quit_and_hold_outcomes() {
    let mut out = Vec::new();
    let mut state = HeldStats::new(false, panel_glyphs(), false, Window::Today);

    assert_eq!(
        state
            .apply_key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
                &mut out,
            )
            .unwrap(),
        KeyOutcome::Reload
    );
    assert_eq!(
        state
            .apply_key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &mut out,
            )
            .unwrap(),
        KeyOutcome::Quit
    );

    let mut held = HeldStats::new(false, panel_glyphs(), true, Window::Today);
    assert_eq!(
        held.apply_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut out,
        )
        .unwrap(),
        KeyOutcome::Ignore
    );
    assert!(out.is_empty());
}

#[test]
fn held_stats_keeps_the_last_frame_after_a_refresh_failure() {
    let mut state = HeldStats::new(false, panel_glyphs(), false, Window::Today);
    let mut out = Vec::new();
    let today = unix_secs_now() as i64 / DAY_SECS;
    let mut stats = Stats {
        timezone: "UTC".into(),
        refreshed_at_ms: 0,
        day_cutoff_secs: 0,
        agents: BTreeMap::new(),
        subagents: BTreeMap::new(),
        recorded_since_secs: None,
        effort_since_secs: None,
        teams: BTreeMap::new(),
        by_day: BTreeMap::from([(today, day(42, 0.0))]),
        chart: Default::default(),
        by_model: BTreeMap::new(),
        by_provider: BTreeMap::new(),
        total: SpendTally::default(),
    };
    stats.total.year.usd = 42.0;

    state.accept_refresh(Ok(stats), &mut out).unwrap();
    assert!(state.has_frame());
    out.clear();

    state
        .accept_refresh(Err(anyhow!("refresh failed")), &mut out)
        .unwrap();
    assert!(state.has_frame());
    assert!(state.has_refresh_failure());
    let frame = String::from_utf8(out.clone()).unwrap();
    assert!(frame.contains("Year $42"), "frame was {frame:?}");
    assert!(
        !frame.contains("Spending refresh unavailable"),
        "held frame must outrank the failure: {frame:?}"
    );
    out.clear();

    state
        .apply_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut out)
        .unwrap();
    let frame = String::from_utf8(out).unwrap();
    assert!(frame.contains("Year $42"), "frame was {frame:?}");
}

#[test]
fn held_stats_refresh_failure_streak_resets_after_success() {
    let mut state = HeldStats::new(false, panel_glyphs(), false, Window::Today);
    let mut out = Vec::new();

    state
        .accept_refresh(Err(anyhow!("first failure")), &mut out)
        .unwrap();
    state
        .accept_refresh(Err(anyhow!("repeated failure")), &mut out)
        .unwrap();
    assert!(state.has_refresh_failure());

    state
        .accept_refresh(
            Ok(Stats {
                timezone: "UTC".into(),
                refreshed_at_ms: 0,
                day_cutoff_secs: 0,
                agents: BTreeMap::new(),
                subagents: BTreeMap::new(),
                recorded_since_secs: None,
                effort_since_secs: None,
                teams: BTreeMap::new(),
                by_day: BTreeMap::new(),
                chart: Default::default(),
                by_model: BTreeMap::new(),
                by_provider: BTreeMap::new(),
                total: SpendTally::default(),
            }),
            &mut out,
        )
        .unwrap();
    assert!(!state.has_refresh_failure());
}

#[test]
fn hold_cycle_ends_when_the_refresh_worker_exits() {
    let mut state = HeldStats::new(false, panel_glyphs(), true, Window::Today);
    let (tx, rx) = mpsc::sync_channel::<Result<Stats>>(1);
    drop(tx);

    let error = match hold_cycle(&mut state, &rx) {
        Err(error) => error,
        Ok(_) => panic!("a disconnected worker must end the hold, not keep cycling"),
    };
    assert_eq!(error.to_string(), "stats refresh worker exited");
}

#[test]
fn empty_dashboard_refreshes_early_until_a_frame_exists() {
    let now = Instant::now();
    let current = now + REFRESH_INTERVAL;

    assert_eq!(
        deadline_after_refresh(current, now, false),
        now + EMPTY_REFRESH_RETRY
    );
    assert_eq!(deadline_after_refresh(current, now, true), current);
}

#[test]
fn held_stats_paints_an_unavailable_first_refresh_then_replaces_it() {
    let mut state = HeldStats::new(false, panel_glyphs(), false, Window::Today);
    let mut out = Vec::new();

    state
        .accept_refresh(Err(anyhow!("refresh failed")), &mut out)
        .unwrap();

    assert!(!state.has_frame());
    assert!(state.has_refresh_failure());
    let unavailable = String::from_utf8(out.clone()).unwrap();
    assert!(
        unavailable.contains("Spending refresh unavailable - retrying. refresh failed"),
        "frame was {unavailable:?}"
    );
    out.clear();
    state
        .apply_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut out)
        .unwrap();
    let repainted = String::from_utf8(out.clone()).unwrap();
    assert!(
        repainted.contains("Spending refresh unavailable - retrying. refresh failed"),
        "repainted frame was {repainted:?}"
    );

    let today = unix_secs_now() as i64 / DAY_SECS;
    let mut stats = Stats {
        timezone: "UTC".into(),
        refreshed_at_ms: 0,
        day_cutoff_secs: 0,
        agents: BTreeMap::new(),
        subagents: BTreeMap::new(),
        recorded_since_secs: None,
        effort_since_secs: None,
        teams: BTreeMap::new(),
        by_day: BTreeMap::from([(today, day(42, 0.0))]),
        chart: Default::default(),
        by_model: BTreeMap::new(),
        by_provider: BTreeMap::new(),
        total: SpendTally::default(),
    };
    stats.total.year.usd = 42.0;
    out.clear();
    state.accept_refresh(Ok(stats), &mut out).unwrap();

    assert!(state.has_frame());
    assert!(!state.has_refresh_failure());
    let frame = String::from_utf8(out).unwrap();
    assert!(frame.contains("Year $42"), "frame was {frame:?}");
    assert!(!frame.contains("Spending refresh unavailable"));
}

#[test]
fn unavailable_message_ellipsizes_to_the_panel_width() {
    let message = ellipsize(
        "Spending refresh unavailable - retrying. spending service refresh already in progress",
        80,
    );

    assert_eq!(display_width(&message), 80);
    assert!(message.ends_with('…'));
    assert_eq!(ellipsize("short", 80), "short");
}

#[test]
fn refresh_result_carries_success_error_and_panic() {
    let success = refresh_result(|| {
        let mut stats = Stats {
            timezone: "UTC".into(),
            refreshed_at_ms: 0,
            day_cutoff_secs: 0,
            agents: BTreeMap::new(),
            subagents: BTreeMap::new(),
            recorded_since_secs: None,
            effort_since_secs: None,
            teams: BTreeMap::new(),
            by_day: BTreeMap::new(),
            chart: Default::default(),
            by_model: BTreeMap::new(),
            by_provider: BTreeMap::new(),
            total: SpendTally::default(),
        };
        stats.total.year.tokens = 42;
        Ok(stats)
    });
    assert_eq!(success.unwrap().total.year.tokens, 42);

    let error = refresh_result(|| Err(anyhow!("refresh failed")))
        .err()
        .expect("ordinary refresh error");
    assert_eq!(error.to_string(), "refresh failed");

    let panic = refresh_result(|| -> Result<Stats> { panic!("refresh panic") })
        .err()
        .expect("panic refresh error");
    assert!(
        panic.to_string().contains("refresh panic"),
        "panic payload was {panic}"
    );
}

#[test]
fn refresh_worker_reuses_one_background_thread() {
    let (threads, observed) = mpsc::channel();
    let worker = hold::RefreshWorker::new(move || {
        threads.send(thread::current().id()).unwrap();
        Err(anyhow!("injected load failure"))
    });
    assert!(worker.is_ok(), "refresh worker must start");
    let worker = worker.unwrap();
    let mut ids = Vec::new();
    for _ in 0..3 {
        worker.requests.send(()).unwrap();
        let result = worker.results.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(result.err().unwrap().to_string(), "injected load failure");
        ids.push(observed.recv_timeout(Duration::from_secs(5)).unwrap());
    }
    assert!(ids.iter().all(|id| *id == ids[0]));
    assert_ne!(ids[0], thread::current().id());
}

#[test]
fn progress_bar_tracks_file_count() {
    assert_eq!(progress_bar(0, 10), "░".repeat(PROGRESS_BAR_WIDTH));
    assert_eq!(
        progress_bar(5, 10),
        format!("{}{}", "█".repeat(10), "░".repeat(10))
    );
    assert_eq!(progress_bar(10, 10), "█".repeat(PROGRESS_BAR_WIDTH));
}

#[test]
fn ramp_key_keeps_less_and_more_together() {
    let key = ramp_key(&ramp_styles());
    assert_eq!(display_width(&key), "Less · ░ ▒ ▓ █ More".chars().count());
}

#[test]
fn heatmap_cells_render_spaced() {
    let today = 20_009; // Sunday, so the current week has no future blanks.
    let first_day = week_start(today) - 7;
    let mut by_day = BTreeMap::new();
    for offset in 0..14 {
        by_day.insert(first_day + offset, day((offset + 1) as u64, 0.0));
    }
    let stats = Stats {
        timezone: "UTC".into(),
        refreshed_at_ms: 0,
        day_cutoff_secs: 0,
        agents: BTreeMap::new(),
        subagents: BTreeMap::new(),
        recorded_since_secs: None,
        effort_since_secs: None,
        teams: BTreeMap::new(),
        by_day,
        chart: Default::default(),
        by_model: BTreeMap::new(),
        by_provider: BTreeMap::new(),
        total: SpendTally::default(),
    };
    let mut lines = Vec::new();

    heatmap_lines(&mut lines, &stats, today, 2, false);

    let monday = strip_ansi(lines.iter().find(|line| line.contains("Mon")).unwrap());
    let cells = monday.chars().skip(GUTTER).collect::<Vec<_>>();
    assert_eq!(
        cells.len(),
        3,
        "the trailing day-space is trimmed at line end"
    );
    for (idx, ch) in cells.into_iter().enumerate() {
        if idx % 2 == 0 {
            assert!(RAMP.contains(&ch), "day cell starts with a ramp glyph");
        } else {
            assert_eq!(ch, ' ', "day cell ends with a space");
        }
    }
    assert_eq!(lines.len(), 12);
    assert!(
        !lines[lines.len() - 3].is_empty(),
        "the last day row precedes the legend gap"
    );
    assert_eq!(lines[lines.len() - 2], "");
    assert_eq!(
        strip_ansi(lines.last().expect("heatmap legend")),
        "  Less · ░ ▒ ▓ █ More"
    );
}

#[test]
fn month_row_skips_one_column_leading_partial_month() {
    let grid = Grid::build(&BTreeMap::new(), 20_283, 3, false); // 2025-07-14 (Monday).

    let row = month_row(&grid);

    assert!(row.contains("Jul"));
    assert!(!row.contains("Jun"));
    assert!(!row.contains("JuJul"));
}

#[test]
fn stats_json_includes_cache_hit_percent_only_with_input_side_data() {
    let mut claude = tally(100, 1.0, 1);
    claude.year.input = 10;
    claude.year.cache_read = 90;
    claude.year.tool_calls = 3;
    claude.year.tools = BTreeMap::from([("Read".to_owned(), 3)]);
    let mut claude_model = model_tally(10, 1.0, 10, 0, 90);
    claude_model.year.tool_calls = 3;
    claude_model.year.tools = BTreeMap::from([("Read".to_owned(), 3)]);
    claude_model.year.timed_ms = 6_000;
    claude_model.year.timed_output = 400;
    let mut total = SpendTally::default();
    total.week.tool_calls = 1;
    total.week.tools = BTreeMap::from([("Read".to_owned(), 1)]);
    total.month.tool_calls = 2;
    total.month.tools = BTreeMap::from([("Read".to_owned(), 2)]);
    total.year.tool_calls = 3;
    total.year.tools = BTreeMap::from([("Read".to_owned(), 3)]);
    let stats = Stats {
        timezone: "UTC".into(),
        refreshed_at_ms: 0,
        day_cutoff_secs: 0,
        agents: BTreeMap::new(),
        subagents: BTreeMap::new(),
        recorded_since_secs: None,
        effort_since_secs: None,
        teams: BTreeMap::new(),
        by_day: BTreeMap::new(),
        chart: Default::default(),
        by_model: model_map([
            ("claude-opus-4-8".to_owned(), claude_model),
            ("output-only".to_owned(), model_tally(10, 1.0, 0, 10, 0)),
        ]),
        by_provider: BTreeMap::from([("claude".to_owned(), claude)]),
        total,
    };

    let value = json::stats_json_value(&stats, &AssistStats::default(), 0, false, Window::Year);
    let models = value["models"].as_array().unwrap();
    let claude_model = models
        .iter()
        .find(|model| model["model"] == "claude-opus-4-8")
        .unwrap();
    let output_only = models
        .iter()
        .find(|model| model["model"] == "output-only")
        .unwrap();
    assert_eq!(claude_model["cache_hit_pct"], 90);
    assert_eq!(claude_model["tokens_per_sec"], 67);
    assert!(output_only.get("tokens_per_sec").is_none());
    assert!(output_only.get("cache_hit_pct").is_none());
    assert!(output_only.get("tool_calls").is_none());
    assert_eq!(claude_model["tool_calls"], 3);
    assert_eq!(claude_model["tools"]["Read"], 3);
    assert_eq!(value["providers"][0]["cache_hit_pct"], 90);
    assert_eq!(value["providers"][0]["tools"]["Read"], 3);
    assert_eq!(value["windows"]["week"]["tool_calls"], 1);
    assert_eq!(value["windows"]["month"]["tools"]["Read"], 2);
    assert_eq!(value["windows"]["year"]["tool_calls"], 3);
}

#[test]
fn modern_theme_flips_stats_token_glyphs_to_nerd_font() {
    let theme = ThemeConfig {
        style: Some(rimz::config::ThemeStyle::Modern),
        ..Default::default()
    };

    let glyph = theme_glyphs(&theme);
    assert_ne!(glyph(GlyphRole::TokensTotal), "◇");
}

#[test]
fn agent_display_name_uses_descriptor_and_kind_fallback() {
    assert_eq!(agent_display_name("claude"), "Claude");
    assert_eq!(agent_display_name("mystery"), "Mystery");
}

#[test]
fn stats_json_lists_unpriced_usage_without_folding_or_empty_windows() {
    let rows: BTreeMap<String, SpendTally> = BTreeMap::from([
        (
            "priced".into(),
            every_window(model_tally(10_000, 10.0, 6_000, 4_000, 0)),
        ),
        (
            "dollar-only".into(),
            every_window(model_tally(0, 2.0, 0, 0, 0)),
        ),
        (
            "unpriced".into(),
            every_window(model_tally(100, 0.0, 60, 40, 40)),
        ),
        ("tiny".into(), every_window(model_tally(1, 0.0, 1, 0, 0))),
        ("empty".into(), model_tally(100, 1.0, 60, 40, 0)),
        (
            "cache-only".into(),
            every_window(model_tally(0, 0.0, 0, 0, 1_000_000)),
        ),
    ]);
    let mut stats = role_stats(BTreeMap::new(), BTreeMap::new());
    stats.by_model = rows
        .iter()
        .map(|(id, tally)| (id.clone(), produced(tally.clone())))
        .collect();
    stats.by_provider = rows.clone();
    stats.agents = rows
        .iter()
        .map(|(key, tally)| (key.clone(), role_tally(tally.clone())))
        .collect();
    stats.subagents = rows
        .into_iter()
        .map(|(key, tally)| (key, role_tally(tally)))
        .collect();
    stats.total = every_window(tally(10_101, 12.0, 4));
    let value = json::stats_json_value(&stats, &AssistStats::default(), 0, false, Window::Week);

    for (section, key) in [
        ("models", "model"),
        ("providers", "kind"),
        ("agents", "profile"),
        ("subagents", "profile"),
    ] {
        let rows = value[section].as_array().unwrap();
        let expected = if key == "profile" {
            ["priced", "dollar-only", "tiny", "unpriced"]
        } else {
            ["priced", "dollar-only", "unpriced", "tiny"]
        };
        assert_eq!(
            rows.iter()
                .map(|row| row[key].as_str().unwrap())
                .collect::<Vec<_>>(),
            expected,
            "{section}"
        );
        for (id, tokens) in [("unpriced", 140), ("tiny", 1)] {
            let row = rows.iter().find(|row| row[key] == id).unwrap();
            assert_eq!(row["usd"], 0.0);
            assert_eq!(row["share"], 0.0);
            assert_eq!(row["tokens"], tokens);
        }
    }
    for role in [RoleView::Agents, RoleView::Subagents] {
        assert_eq!(
            json::roles_value(&stats, Window::Week, role)[role.key()],
            value[role.key()]
        );
        let mut out = Vec::new();
        panel::render_spend(&mut out, &stats, 0, false, Some(role), Window::Week, "\n").unwrap();
        let text = std::str::from_utf8(&out).unwrap();
        assert!(
            text.contains("priced") && text.contains("dollar-only"),
            "{text}"
        );
        for omitted in ["unpriced", "tiny", "empty", "cache-only"] {
            assert!(!text.contains(omitted), "{text}");
        }
    }
}

#[test]
fn today_dashboard_uses_day_breakdowns_and_year_activity_without_a_trend() {
    let today = 20_000;
    let mut current = tally(900, 60.0, 10);
    current.day = spend_window(30, 12.0, 20, 10, 5, 2);
    current.headline = spend_window(800, 50.0, 600, 200, 0, 9);
    let old = tally(1_000, 100.0, 1);
    let stats = Stats {
        timezone: "UTC".into(),
        refreshed_at_ms: 0,
        day_cutoff_secs: 0,
        agents: BTreeMap::new(),
        subagents: BTreeMap::new(),
        recorded_since_secs: None,
        effort_since_secs: None,
        teams: BTreeMap::new(),
        by_day: BTreeMap::from([
            (today, day(35, 12.0)),
            (today - 1, day(50, 18.0)),
            (today - 100, day(900, 30.0)),
        ]),
        chart: Default::default(),
        by_model: model_map([
            ("current-model".into(), current.clone()),
            ("old-model".into(), old.clone()),
        ]),
        by_provider: BTreeMap::from([("codex".into(), current.clone()), ("claude".into(), old)]),
        total: current,
    };
    let active = Window::Week.prev();
    let agents = provider_breakdown(&stats, active);
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].window, stats.total.day);
    let rendered = strip_ansi(&held_frame(&stats, &AssistStats::default(), active, today));
    for expected in [
        "Today $12",
        "Current Mod",
        "Codex",
        "today $12 so far · yesterday $18",
        "2 of last 28 days active",
        "streak 2 days",
        "busiest",
    ] {
        assert!(
            rendered.contains(expected),
            "missing {expected}: {rendered}"
        );
    }
    assert!(rendered.contains(&fmt_day(today - 100)), "{rendered}");
    for absent in ["Old Model", "Claude", "vs prior", "$50"] {
        assert!(
            !rendered.contains(absent),
            "unexpected {absent}: {rendered}"
        );
    }
}

#[test]
fn model_rate_column_is_windowed_and_other_is_untimed_on_dashboard() {
    let (stats, assists) = held::fixture();
    let year = strip_ansi(&held::frame(&stats, &assists, Window::Year, 119, None).text);
    let week = strip_ansi(&held::frame(&stats, &assists, Window::Week, 119, None).text);
    assert!(
        year.lines()
            .any(|line| line.contains("Fable 5.1")
                && line.split_whitespace().any(|cell| cell == "50"))
    );
    let week_model = week
        .lines()
        .find(|line| line.contains("Opus 5.5") && !line.contains("Other"))
        .expect(&week);
    assert!(week_model.contains('–'), "{week_model}");
    assert!(
        year.lines()
            .any(|line| line.contains("Other") && line.contains('–'))
    );
}

#[test]
fn tier_fallback_has_its_own_assist_category_and_forensic_line() {
    let record = serde_json::from_value(serde_json::json!({
        "at": "2026-06-02T12:00:00Z", "assist": "tier_fallback",
        "kind": "codex", "agent_id": "session-1", "profile": "worker",
        "tier": "senior", "model": "gpt-6-astra",
        "skipped": [{"model": "opus", "reason": "logged_out"}]
    }));
    assert!(record.is_ok(), "tier fallback must decode: {record:?}");
    let stats = assists::AssistStats::from_records("all", vec![record.unwrap()]);
    assert_eq!(
        serde_json::to_value(&stats.rollup).unwrap()["tier_fallbacks"],
        1
    );
    let rows = assists::category_rows(&stats.rollup, assists::CategoryMode::Timeline).join("\n");
    assert!(rows.contains("Tier fallback:"), "{rows}");
    let line = assists::benefit_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
    assert!(
        line.contains("worker: opus → gpt-6-astra (logged out)"),
        "{line}"
    );
    let record = serde_json::from_value(serde_json::json!({
        "at": "2026-06-02T12:00:00Z", "assist": "tier_fallback",
        "kind": "codex", "agent_id": "session-1", "profile": "worker",
        "tier": "senior", "model": "gpt-6-astra", "skipped": [],
        "unbuildable": [{"model": "opus", "error": "drop the user-only skill"}]
    }))
    .unwrap();
    let stats = assists::AssistStats::from_records("all", vec![record]);
    assert_eq!(stats.rollup.tier_fallbacks, 1);
    let line = assists::benefit_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
    assert!(
        line.contains("worker: opus → gpt-6-astra (not buildable)"),
        "{line}"
    );
}

#[test]
fn stall_notice_names_child_parent_and_silence_in_stats() {
    let record = serde_json::from_value(serde_json::json!({
        "at": "2026-06-02T12:00:00Z", "assist": "stall_notice",
        "kind": "codex", "agent_id": "session-1", "label": "@still-silver",
        "parent": "@planner", "silent_secs": 1920, "message_id": "msg_1",
        "delivered": false, "error": "pane send failed\ncaused by"
    }));
    assert!(record.is_ok(), "stall notice must decode: {record:?}");
    let stats = assists::AssistStats::from_records("all", vec![record.unwrap()]);
    assert_eq!(
        serde_json::to_value(&stats.rollup).unwrap()["stall_notices"],
        0
    );
    let rows = assists::category_rows(&stats.rollup, assists::CategoryMode::Timeline).join("\n");
    assert!(
        strip_ansi(&rows).contains("Stall notice: 0 (1 held)"),
        "{rows}"
    );
    let line = assists::benefit_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
    for fact in ["@still-silver", "@planner", "32m", "pane send failed"] {
        assert!(line.contains(fact), "{line}");
    }
    assert!(!line.contains("caused by"), "{line}");
    let wire = serde_json::to_value(&stats).unwrap();
    assert_eq!(wire["events"][0]["assist"], "stall_notice");
    assert_eq!(wire["events"][0]["message_id"], "msg_1");
}

#[test]
fn model_display_names() {
    use rimz::agents::model_display::display_model;

    assert_eq!(display_model("claude-opus-4-8"), "Opus 4.8");
    assert_eq!(display_model("claude-haiku-4-5"), "Haiku 4.5");
    assert_eq!(display_model("claude-fable-5"), "Fable 5");
    assert_eq!(display_model("claude-opus-4-7-20260101"), "Opus 4.7");
    assert_eq!(display_model("gpt-5"), "GPT 5");
    assert_eq!(display_model("gpt-5-codex"), "GPT 5 Codex");
    assert_eq!(display_model("gpt-5.1-codex-max"), "GPT 5.1 Codex Max");
    assert_eq!(display_model("mystery-model"), "Mystery Model");
}

#[test]
fn token_and_dollar_formatting() {
    assert_eq!(fmt_tokens(412_000_000), "412M");
    assert_eq!(fmt_tokens(5_200_000_000), "5.2B");
    assert_eq!(fmt_tokens(950_000), "950K");
    assert_eq!(fmt_tokens_precise(61_000_000), "61.0M");
    assert_eq!(fmt_tokens_precise(1_200_000_000), "1.2B");
    assert_eq!(fmt_usd(8_666.0), "$8,666");
    assert_eq!(fmt_usd(1_000_000.0), "$1,000,000");
    assert_eq!(fmt_usd(0.12), "$0.12");
    assert_eq!(fmt_usd(9.5), "$9.50");
    assert_eq!(fmt_usd(10.4), "$10");
    assert_eq!(rimz::theme::fmt::group_thousands(1_741), "1,741");
}

#[test]
fn fmt_day_reads_month_and_day() {
    // Epoch day 0 is 1970-01-01; day 31 is 1970-02-01 (January has 31 days).
    assert_eq!(utc_date(0), "1970-01-01");
    assert_eq!(fmt_day(0), "Jan 1");
    assert_eq!(fmt_day(31), "Feb 1");
}

#[test]
fn assists_fold_rolls_up_benefit_and_keeps_failed_attempts_forensics() {
    use rimz::harness::assist_log::{Assist, AssistRecord};
    use rimz::harness::auto_redeem::RedeemReason;
    use rimz::ids::{AgentKind, AgentSessionId};

    let ts = |second| jiff::Timestamp::from_second(second).unwrap();
    let records = vec![
        AssistRecord {
            at: ts(4_000),
            assist: Assist::AutoRedeem {
                kind: "codex".to_owned(),
                login: Some("spare".parse().unwrap()),
                reason: RedeemReason::ExpiryRescue,
                request_id: "request-1".to_owned(),
                credits: 1,
                soonest_expiry: None,
                natural_reset: None,
                outcome: Some("reset".to_owned()),
                windows_reset: true,
                window_resets: Vec::new(),
                error: None,
            },
        },
        AssistRecord {
            at: ts(3_600),
            assist: Assist::AutoContinue {
                kind: AgentKind::new_unchecked("codex"),
                agent_id: AgentSessionId::from("session-1"),
                label: Some("@coder".to_owned()),
                park: "rate_limit_window_reset".to_owned(),
                parked_since: Some(ts(0)),
                delivered: true,
                message_id: "msg_1".to_owned(),
            },
        },
        AssistRecord {
            at: ts(3_700),
            assist: Assist::AutoContinue {
                kind: AgentKind::new_unchecked("codex"),
                agent_id: AgentSessionId::from("session-1"),
                label: Some("@coder".to_owned()),
                park: "overloaded_backoff_retry".to_owned(),
                parked_since: Some(ts(3_600)),
                delivered: false,
                message_id: "msg_2".to_owned(),
            },
        },
        AssistRecord {
            at: ts(4_100),
            assist: Assist::AutoCompact {
                kind: AgentKind::new_unchecked("codex"),
                agent_id: AgentSessionId::from("session-1"),
                label: Some("@coder".to_owned()),
                threshold: rimz::store::message::AutoCompact::Tokens(200_000),
                occupied_tokens: Some(210_000),
                message_id: "msg_3".to_owned(),
            },
        },
        AssistRecord {
            at: ts(4_150),
            assist: Assist::IdleCompact {
                kind: AgentKind::new_unchecked("claude"),
                agent_id: AgentSessionId::from("session-2"),
                label: Some("@planner".to_owned()),
                idle_secs: 3_540,
                idle_after_secs: None,
                occupied_tokens: 180_000,
                message_id: "msg_4".to_owned(),
                delivered: true,
                error: None,
            },
        },
        AssistRecord {
            at: ts(4_200),
            assist: Assist::AutoResume {
                workspace_id: rimz::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
                session_name: "rimz-test".to_owned(),
                cause: Some(rimz::store::event::SessionDeathCause::Crash),
                recovered: 2,
                labels: vec!["@coder".to_owned(), "@reviewer".to_owned()],
            },
        },
        AssistRecord {
            at: ts(4_300),
            assist: Assist::AutoGc {
                workspace_id: rimz::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
                scope: rimz::harness::auto_gc::GcScope::Machine,
                older_than_secs: 7 * 86_400,
                reclaimed_bytes: 2_048,
                class_bytes: Default::default(),
                worktrees_removed: 2,
                worktree_paths: Vec::new(),
                workspaces_pruned: 1,
                files_removed: 0,
                messages_archived: 0,
                problems: 0,
                error: None,
            },
        },
        AssistRecord {
            at: ts(4_400),
            assist: Assist::AutoGc {
                workspace_id: rimz::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
                scope: rimz::harness::auto_gc::GcScope::Machine,
                older_than_secs: 7 * 86_400,
                reclaimed_bytes: 0,
                class_bytes: Default::default(),
                worktrees_removed: 0,
                worktree_paths: Vec::new(),
                workspaces_pruned: 0,
                files_removed: 0,
                messages_archived: 0,
                problems: 0,
                error: Some("pruning dead workspaces\ncaused by".to_owned()),
            },
        },
    ];
    let stats = AssistStats::from_records("7d", records);

    assert_eq!(
        stats.rollup,
        AssistRollup {
            workflow_starts: 0,
            workflow_steps: 0,
            workflow_routes: 0,
            workflow_parks: 0,
            workflow_ends: 0,
            stall_notices: 0,
            resident_launches: 0,
            judge_declines: 0,
            model_aliases: 0,
            tier_fallbacks: 0,
            redeems: 1,
            resets: 1,
            manual_redeems: 0,
            manual_resets: 0,
            resumes: 1,
            recovered_secs: 3_600,
            compacts: 2,
            keepalives: 0,
            idle_stops: 0,
            idle_stop_withdrawals: 0,
            molts: 0,
            hibernations: 0,
            hibernate_reclaimed_bytes: 0,
            hibernate_holds: Default::default(),
            launch_retries: 0,
            restores: 1,
            restored_sessions: 2,
            sweeps: 1,
            tmp_evictions: 0,
            reclaimed_bytes: 2_048,
            held: [("resumes", 1), ("sweeps", 1)].into(),
        }
    );
    assert_eq!(stats.events.len(), 8, "every assist outcome stays forensic");
    assert_eq!(stats.timeline(0, false).0.len(), 6);
    assert_eq!(stats.timeline(0, true).0.len(), 8);
    let categories = category_rows(&stats.rollup, assists::CategoryMode::Timeline)
        .into_iter()
        .map(|row| strip_ansi(&row))
        .collect::<Vec<_>>();
    assert_eq!(
        categories[categories.len() - 2],
        "Auto-resume: 1 (2 agents)"
    );
    assert_eq!(categories.last().unwrap(), "Auto-gc: 1 (2 KB, 1 held)");
    let zone = jiff::tz::TimeZone::UTC;
    let lines = stats
        .events
        .iter()
        .map(|event| benefit_line(event, &zone))
        .collect::<Vec<_>>();
    assert!(
        lines
            .iter()
            .any(|line| line.contains("codex@spare credit — expiry rescue → budget reset ✓"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("@coder resumed — limit park"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("resume held — overload park"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("@coder auto-compact — 210k ctx cleared before delivery"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("@planner idle compacted after 1.0h — 180k ctx"))
    );
    assert!(lines.iter().any(|line| {
        line.contains("rebirth recovery — 2 agents restored after crash (@coder, @reviewer)")
    }));
    let forensics = stats
        .events
        .iter()
        .map(|event| forensic_line(event, &zone))
        .collect::<Vec<_>>();
    assert!(
        forensics
            .iter()
            .any(|line| line.contains("agent session-1 · message msg_3 · threshold 200k"))
    );
    assert!(
        forensics
            .iter()
            .any(|line| line.contains("agent session-2 · message msg_4 · delivered true"))
    );
    assert!(forensics.iter().any(|line| {
        line.contains("workspace ws_0123456789abcdef01234567 · session rimz-test")
    }));
    assert!(forensics.iter().any(|line| {
        line.contains("♻ machine gc swept — 2 KB reclaimed, 2 worktrees, 1 workspace · workspace ws_0123456789abcdef01234567 · cutoff 7d")
    }));
    assert!(
        forensics
            .iter()
            .any(|line| line.contains("♻ gc failed — pruning dead workspaces · workspace"))
    );

    let json = serde_json::to_value(&stats).unwrap();
    assert_eq!(json["rollup"]["resumes"], 1);
    assert_eq!(json["rollup"]["sweeps"], 1);
    assert_eq!(json["events"][0]["assist"], "auto_gc");
    let envelope = json::assists_value(&stats, 1, false, &shanghai(1_728_921_600));
    assert_eq!(keys(&envelope), ["as_of", "events", "older", "rollup"]);
    assert_eq!(envelope["as_of"], "2024-10-15T00:00:00+08:00");
    assert_eq!(envelope["events"].as_array().unwrap().len(), 1);
}

fn shanghai(second: i64) -> jiff::Zoned {
    jiff::Timestamp::from_second(second)
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::get("Asia/Shanghai").unwrap())
}

fn keys(value: &serde_json::Value) -> Vec<&str> {
    let mut keys = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys
}

#[test]
fn gc_assist_scope_defaults_to_machine_and_preserves_room() {
    use rimz::harness::assist_log::AssistRecord;

    let legacy = serde_json::json!({
        "at": "2026-01-01T00:00:00Z", "assist": "auto_gc",
        "workspace_id": "ws_0123456789abcdef01234567", "older_than_secs": 604800,
        "reclaimed_bytes": 2048, "worktrees_removed": 0, "workspaces_pruned": 0,
        "files_removed": 1, "messages_archived": 0, "problems": 0
    });
    for (scope, label) in [(None, "machine gc swept"), (Some("room"), "♻ gc swept")] {
        let mut value = legacy.clone();
        if let Some(scope) = scope {
            value["scope"] = scope.into();
        }
        let record: AssistRecord = serde_json::from_value(value).unwrap();
        assert_eq!(
            serde_json::to_value(&record).unwrap()["scope"],
            scope.unwrap_or("machine")
        );
        let stats = AssistStats::from_records("test", vec![record]);
        assert_eq!(
            serde_json::to_value(&stats).unwrap()["events"][0]["scope"],
            scope.unwrap_or("machine")
        );
        assert!(benefit_line(&stats.events[0], &jiff::tz::TimeZone::UTC).contains(label));
    }
}

#[test]
fn idle_compact_assist_renders_threshold_and_accepts_legacy_record() {
    let mut value = serde_json::json!({
        "at": "2026-01-01T00:00:00Z", "assist": "idle_compact",
        "kind": "claude", "agent_id": "session-2", "label": "@planner",
        "idle_secs": 3480, "occupied_tokens": 180000, "message_id": "msg_4", "delivered": true
    });
    for threshold in [None, Some(3420)] {
        if let Some(threshold) = threshold {
            value["idle_after_secs"] = threshold.into();
        }
        let record = serde_json::from_value(value.clone()).unwrap();
        let stats = AssistStats::from_records("7d", vec![record]);
        let line = benefit_line(&stats.events[0], &jiff::tz::TimeZone::UTC);
        if threshold.is_some() {
            assert!(
                line.contains("idle compacted after 58m (threshold 57m)"),
                "{line}"
            );
        } else {
            assert!(line.contains("idle compacted after"));
            assert!(!line.contains("threshold"));
        }
    }
}

#[test]
fn assists_panel_omits_empty_chrome_and_formats_the_rollup() {
    let mut lines = Vec::new();
    panel_lines(&mut lines, &AssistStats::default(), 80, "Assists");
    assert!(lines.is_empty());

    let stats = AssistStats {
        window: "7d".to_owned(),
        rollup: AssistRollup {
            redeems: 2,
            resets: 1,
            resumes: 5,
            recovered_secs: 22_320,
            compacts: 4,
            restores: 2,
            restored_sessions: 7,
            ..Default::default()
        },
        events: Vec::new(),
    };
    let rows = category_rows(&stats.rollup, assists::CategoryMode::Timeline)
        .into_iter()
        .map(|row| strip_ansi(&row))
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            "Auto-continue: 5 (after 6.2h parked)",
            "Auto-compact: 4",
            "Auto-redeem: 2 (1 reset)",
            "Auto-resume: 2 (7 agents)",
        ]
    );
    panel_lines(&mut lines, &stats, 80, "Assists");
    assert_eq!(lines.len(), 3, "header plus two paired count rows");
    let panel = strip_ansi(&lines.join("\n"));
    assert!(panel.contains("Assists"));
    assert!(panel.contains("Auto-resume: 2 (7 agents)"));

    let bare_counts = AssistRollup {
        redeems: 1,
        resumes: 1,
        compacts: 1,
        restores: 1,
        restored_sessions: 1,
        ..Default::default()
    };
    let bare = category_rows(&bare_counts, assists::CategoryMode::Timeline)
        .into_iter()
        .map(|row| strip_ansi(&row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!bare.contains('$'));
    assert!(!bare.contains("reset"));
    assert!(!bare.contains('h'));

    let failed_only = AssistStats {
        window: "7d".to_owned(),
        rollup: AssistRollup::default(),
        events: vec![AssistEvent::Continue {
            at: jiff::Timestamp::from_second(1).unwrap(),
            kind: rimz::ids::AgentKind::new_unchecked("codex"),
            agent_id: rimz::ids::AgentSessionId::from("session-1"),
            label: Some("@coder".to_owned()),
            park: "overloaded_backoff_retry".to_owned(),
            parked_since: None,
            delivered: false,
            message_id: "msg_1".to_owned(),
        }],
    };
    lines.clear();
    panel_lines(&mut lines, &failed_only, 80, "Assists");
    assert!(lines.is_empty());
}

#[test]
fn manual_redeems_stay_in_the_ledger_but_not_the_automation_row() {
    use rimz::harness::assist_log::{Assist, AssistRecord};
    use rimz::harness::auto_redeem::RedeemReason;
    for manual_outcome in ["reset", "nothing_to_reset"] {
        let records = [RedeemReason::BlockedGain, RedeemReason::Manual]
            .into_iter()
            .map(|reason| AssistRecord {
                at: Timestamp::from_second(1_700_000_000).unwrap(),
                assist: Assist::AutoRedeem {
                    kind: "codex".to_owned(),
                    login: Some("spare".parse().unwrap()),
                    reason,
                    request_id: "request".to_owned(),
                    credits: 1,
                    soonest_expiry: None,
                    natural_reset: None,
                    outcome: Some(
                        if reason == RedeemReason::Manual {
                            manual_outcome
                        } else {
                            "reset"
                        }
                        .to_owned(),
                    ),
                    windows_reset: true,
                    window_resets: Vec::new(),
                    error: None,
                },
            })
            .collect();
        let stats = AssistStats::from_records("7d", records);
        assert_eq!(stats.rollup.redeems, 2);
        assert_eq!(
            stats.rollup.resets,
            1 + usize::from(manual_outcome == "reset")
        );
        assert_eq!(stats.rollup.manual_redeems, 1);
        assert_eq!(
            stats.rollup.manual_resets,
            usize::from(manual_outcome == "reset")
        );
        let rows = category_rows(&stats.rollup, assists::CategoryMode::Timeline)
            .into_iter()
            .map(|row| strip_ansi(&row))
            .collect::<Vec<_>>();
        assert_eq!(rows, ["Auto-redeem: 1 (1 reset)"]);
        assert!(
            stats
                .events
                .iter()
                .any(|event| benefit_line(event, &jiff::tz::TimeZone::UTC).contains("manual"))
        );
        let mut manual = stats.rollup;
        manual.manual_redeems = manual.redeems;
        manual.manual_resets = manual.resets;
        assert!(category_rows(&manual, assists::CategoryMode::Timeline).is_empty());
    }
}

#[test]
fn today_assists_start_at_midnight_in_the_publication_zone() {
    use rimz::harness::assist_log;

    for (zone, now, midnight) in [
        (
            "Asia/Shanghai",
            "2026-01-12T17:00:00Z",
            "2026-01-12T16:00:00Z",
        ),
        (
            "America/New_York",
            "2026-03-08T18:00:00Z",
            "2026-03-08T05:00:00Z",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let midnight: Timestamp = midnight.parse().unwrap();
        let records = [
            midnight - Duration::from_secs(1),
            midnight,
            now.parse().unwrap(),
        ]
        .map(|at| {
            serde_json::json!({
                "at": at, "assist": "auto_compact", "kind": "codex", "agent_id": "session",
                "threshold": {"percent": 70}, "message_id": "msg"
            })
            .to_string()
        });
        let path = assist_log::log_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, records.join("\n") + "\n").unwrap();
        let assists =
            AssistStats::load(dir.path(), Window::Week.prev(), now.parse().unwrap(), zone);
        assert_eq!(assists.rollup.compacts, 2, "{zone}");
        assert_eq!(assists.window, "today");
    }
}

#[test]
fn assists_load_scopes_the_log_to_the_selected_window() {
    use rimz::harness::assist_log::{self, Assist, AssistRecord};
    use rimz::harness::auto_redeem::RedeemReason;

    let dir = tempfile::tempdir().unwrap();
    let now = jiff::Timestamp::from_second(10 * DAY_SECS).unwrap();
    let old = AssistRecord {
        at: jiff::Timestamp::from_second(DAY_SECS).unwrap(),
        assist: Assist::AutoRedeem {
            kind: "codex".to_owned(),
            login: None,
            reason: RedeemReason::ExpiryRescue,
            request_id: "old".to_owned(),
            credits: 1,
            soonest_expiry: None,
            natural_reset: None,
            outcome: Some("reset".to_owned()),
            windows_reset: true,
            window_resets: Vec::new(),
            error: None,
        },
    };
    let recent = AssistRecord {
        at: now,
        assist: Assist::AutoCompact {
            kind: rimz::ids::AgentKind::new_unchecked("codex"),
            agent_id: rimz::ids::AgentSessionId::from("session-1"),
            label: None,
            threshold: rimz::store::message::AutoCompact::Percent(70),
            occupied_tokens: None,
            message_id: "msg_1".to_owned(),
        },
    };
    let assist_path = assist_log::log_path(dir.path());
    std::fs::create_dir_all(assist_path.parent().unwrap()).unwrap();
    std::fs::write(
        assist_path,
        format!(
            "{}\n{}\n",
            serde_json::to_string(&old).unwrap(),
            serde_json::to_string(&recent).unwrap()
        ),
    )
    .unwrap();

    let stats = AssistStats::load(dir.path(), Window::Week, now, "UTC");
    assert_eq!(stats.rollup.redeems, 0);
    assert_eq!(stats.rollup.compacts, 1);
    assert_eq!(stats.events.len(), 1);
}
#[test]
fn today_index_uses_the_publications_zone() {
    let stats = Stats {
        timezone: "Asia/Shanghai".into(),
        refreshed_at_ms: 0,
        day_cutoff_secs: 0,
        agents: BTreeMap::new(),
        subagents: BTreeMap::new(),
        recorded_since_secs: None,
        effort_since_secs: None,
        teams: BTreeMap::new(),
        by_day: BTreeMap::new(),
        chart: Default::default(),
        by_model: BTreeMap::new(),
        by_provider: BTreeMap::new(),
        total: SpendTally::default(),
    };
    let now: Timestamp = "2026-01-12T16:30:00Z".parse().unwrap();
    assert_eq!(
        stats.today_day(now.as_second() as u64),
        now.as_second() / DAY_SECS + 1
    );
}
