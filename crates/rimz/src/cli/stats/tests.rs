use super::*;
use std::path::Path;

fn day(tokens: u64, usd: f64) -> DaySpend {
    DaySpend { tokens, usd }
}

fn tally(tokens: u64, usd: f64, sessions: u32) -> SpendTally {
    let mut tally = SpendTally::default();
    tally.year.tokens = tokens;
    tally.year.usd = usd;
    tally.year.sessions = sessions;
    tally
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
fn sunday_is_column_zero() {
    // 1970-01-04 is a Sunday (epoch day 3).
    assert_eq!(dow_sun0(3), 0);
    assert_eq!(dow_sun0(4), 1); // Monday
    assert_eq!(dow_sun0(0), 4); // 1970-01-01 is a Thursday
    assert_eq!(week_start(10), 10 - dow_sun0(10));
}

#[test]
fn level_scales_to_the_busiest_day() {
    assert_eq!(level(0.0, 0.0), 0, "empty graph is all calm");
    assert_eq!(level(0.0, 100.0), 0);
    assert_eq!(level(100.0, 100.0), 4, "the busiest day is full");
    assert_eq!(level(50.0, 100.0), 2);
    assert_eq!(level(12.0, 100.0), 0, "a near-calm day rounds down");
}

#[test]
fn grid_places_today_in_the_last_column_and_blanks_the_future() {
    let today = 20_000; // arbitrary epoch day
    let mut by_day = BTreeMap::new();
    by_day.insert(today, day(100, 1.0));
    by_day.insert(today - 7, day(50, 0.5));
    let grid = Grid::build(&by_day, today, 4, false);

    assert_eq!(grid.cells.len(), 4);
    assert!((grid.max - 100.0).abs() < f64::EPSILON);
    // Today sits in the final column at its weekday row.
    let row = dow_sun0(today) as usize;
    assert_eq!(grid.cells[3][row], Some(100.0));
    // Days after today in the current week are blank, not zero.
    if row < 6 {
        assert_eq!(grid.cells[3][row + 1], None);
    }
}

#[test]
fn published_stats_reads_rollups_and_windows() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(rimz::WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let today = 20_000;
    let by_day = BTreeMap::from([(today - 10, day(40, 4.0))]);
    let by_model = BTreeMap::from([(
        "gpt-5-codex".to_owned(),
        ModelSpend {
            usd: 7.0,
            input: 70,
            output: 30,
            cache_read: 5,
            tokens: 100,
        },
    )]);
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
    rimz::agents::spending::write_provider_spending_cache_with_rollups(
        &runtime.shared_provider_spending_path(),
        123,
        &spending,
        &by_day,
        &by_model,
    );

    let stats = load_published_stats(&runtime).expect("current aggregate is readable");

    assert_eq!(stats.by_day, by_day);
    assert_eq!(stats.by_model, by_model);
    assert_eq!(stats.total.week.tokens, 7);
    assert_eq!(stats.total.month.usd, 3.0);
    assert_eq!(stats.total.year.sessions, 9);
    assert_eq!(stats.by_agent["claude"].year.tokens, 120);
    assert_eq!(stats.by_agent["claude"].year.sessions, 3);
}

#[test]
fn cold_refresh_publishes_sidebar_provider_rollups() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(rimz::WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    ensure_shared_runtime(&runtime).unwrap();
    let transcript = write_jsonl(
        dir.path(),
        "claude.jsonl",
        &[&claude_line_today(1.25, "msg-1", "req-1")],
    );
    let files = vec![(
        &rimz::agents::ClaudeAdapter as &'static dyn rimz::agents::AgentAdapter,
        transcript.clone(),
    )];

    let stats = compute_stats_from_files(&runtime, files, true, None);
    let published = read_provider_spending_cache(&runtime.shared_provider_spending_path());
    let fresh = load_published_stats(&runtime)
        .expect("published stats are current after a stats-owned refresh");
    let cursor = read_spending_cache(&runtime.shared_spending_cursor_path());

    assert!(published.is_fresh(unix_millis_now()));
    assert!((published.spending.total.month.usd - stats.total.month.usd).abs() < 1e-9);
    assert!((fresh.total.month.usd - stats.total.month.usd).abs() < 1e-9);
    assert_eq!(
        published.spending.total.month.tokens,
        stats.total.month.tokens
    );
    assert_eq!(published.spending.by_provider, stats.by_agent);
    assert!(
        cursor
            .files
            .contains_key(&transcript.to_string_lossy().into_owned()),
        "stats publishes the cursor cache that makes the next run history-independent"
    );
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

    let a = Activity::of(&by_day, today);
    assert_eq!(a.current_streak, 5);
    assert_eq!(a.longest_streak, 5);
    assert_eq!(a.active_28, 7, "all seven active days fall inside 28");
    assert_eq!(a.most_active, Some(today - 10));
}

#[test]
fn current_streak_survives_an_inactive_today() {
    let today = 20_000;
    let mut by_day = BTreeMap::new();
    by_day.insert(today - 1, day(10, 1.0));
    by_day.insert(today - 2, day(10, 1.0));
    // Nothing logged today yet.
    let a = Activity::of(&by_day, today);
    assert_eq!(
        a.current_streak, 2,
        "a pending today does not break the streak"
    );
}

#[test]
fn cold_spinner_requires_human_stdout_and_stderr_ttys() {
    assert!(should_animate_cold_stats(true, true, true));
    assert!(!should_animate_cold_stats(false, true, true));
    assert!(!should_animate_cold_stats(true, false, true));
    assert!(!should_animate_cold_stats(true, true, false));
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
    let today = 20_008; // Saturday, so the current week has no future blanks.
    let first_day = week_start(today) - 7;
    let mut by_day = BTreeMap::new();
    for offset in 0..14 {
        by_day.insert(first_day + offset, day((offset + 1) as u64, 0.0));
    }
    let stats = Stats {
        by_day,
        by_model: BTreeMap::new(),
        by_agent: BTreeMap::new(),
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
}

#[test]
fn month_row_skips_one_column_leading_partial_month() {
    let grid = Grid::build(&BTreeMap::new(), 20_282, 3, false); // 2025-07-13.

    let row = month_row(&grid);

    assert!(row.contains("Jul"));
    assert!(!row.contains("Jun"));
    assert!(!row.contains("JuJul"));
}

#[test]
fn windows_lines_emit_one_cached_all_time_token_row() {
    let stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::from([
            (
                "a".to_owned(),
                ModelSpend {
                    usd: 1.0,
                    input: 40,
                    output: 60,
                    cache_read: 75,
                    tokens: 100,
                },
            ),
            (
                "b".to_owned(),
                ModelSpend {
                    usd: 2.0,
                    input: 5,
                    output: 20,
                    cache_read: 50,
                    tokens: 25,
                },
            ),
        ]),
        by_agent: BTreeMap::new(),
        total: SpendTally {
            week: rimz::agents::spending::SpendWindow {
                tokens: 7,
                ..Default::default()
            },
            month: rimz::agents::spending::SpendWindow {
                tokens: 30,
                ..Default::default()
            },
            year: rimz::agents::spending::SpendWindow {
                tokens: 365,
                ..Default::default()
            },
            ..Default::default()
        },
    };
    let mut lines = Vec::new();

    windows_lines(&mut lines, &stats);

    assert_eq!(lines.len(), 1);
    let row = strip_ansi(&lines[0]);
    assert!(row.contains("All time 250"));
    assert!(row.contains("Week 7"));
    assert!(row.contains("Month 30"));
    assert!(row.contains("Year 365"));
    assert!(!row.contains('$'));
    assert!(!row.contains('◇'));
}

#[test]
fn model_cells_show_usd_and_cache_read_detail() {
    let glyphs = panel_glyphs();
    let spend = ModelSpend {
        usd: 12.4,
        input: 1_200_000,
        output: 500_000,
        cache_read: 2_500_000,
        tokens: 1_700_000,
    };
    let stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::from([
            ("claude-opus-4-8".to_owned(), spend),
            (
                "gpt-5".to_owned(),
                ModelSpend {
                    tokens: 1_950_000,
                    ..Default::default()
                },
            ),
        ]),
        by_agent: BTreeMap::new(),
        total: SpendTally::default(),
    };
    let models = model_breakdown(&stats);
    let name_w = models
        .iter()
        .map(|(name, _)| display_width(name))
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();

    let cells = model_cells(&models, name_w, &glyphs);
    let pct_w = stat_pct_width(&cells, &[]);
    let (compact, left_w, show_bar) = stat_section_layout(&cells, &[], pct_w, 120);
    emit_stat_section(
        &mut lines, "Models", &cells, compact, left_w, pct_w, show_bar, &glyphs,
    );
    let row = strip_ansi(
        lines
            .iter()
            .find(|line| line.contains("Opus 4.8"))
            .expect("opus row"),
    );

    assert!(row.contains("Opus 4.8"));
    assert!(row.contains("46.6%"));
    assert!(row.contains("$12"));
    assert!(row.contains("↘ 1.2m"));
    assert!(row.contains("↗ 500.0k"));
    assert!(row.contains("◌ 2.5m"));
}

#[test]
fn modern_theme_flips_stats_token_glyphs_to_nerd_font() {
    let theme = ThemeConfig {
        style: Some(rimz::config::ThemeStyle::Modern),
        ..Default::default()
    };

    assert_ne!(
        rimz::sidebar_pane::render::theme_glyph(&theme, GlyphRole::TokensTotal),
        "◇"
    );
}

#[test]
fn agent_display_name_uses_descriptor_and_kind_fallback() {
    assert_eq!(agent_display_name("claude"), "Claude");
    assert_eq!(agent_display_name("mystery"), "Mystery");
}

#[test]
fn agent_cells_rank_by_year_tokens_and_skip_empty_agents() {
    let stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::new(),
        by_agent: BTreeMap::from([
            ("claude".to_owned(), tally(100, 3.0, 2)),
            ("codex".to_owned(), tally(300, 9.0, 4)),
        ]),
        total: SpendTally::default(),
    };
    let mut lines = Vec::new();
    let glyphs = panel_glyphs();
    let agents = agent_year_breakdown(&stats);
    let name_w = agents
        .iter()
        .map(|agent| display_width(&agent.name))
        .max()
        .unwrap_or(0);

    let cells = agent_cells(&agents, name_w, &glyphs);
    let pct_w = stat_pct_width(&[], &cells);
    let (compact, left_w, show_bar) = stat_section_layout(&[], &cells, pct_w, 120);
    emit_stat_section(
        &mut lines, "Agents", &cells, compact, left_w, pct_w, show_bar, &glyphs,
    );

    assert!(lines[0].contains("Agents"));
    let codex = lines
        .iter()
        .position(|line| line.contains("Codex"))
        .expect("codex row");
    let claude = lines
        .iter()
        .position(|line| line.contains("Claude"))
        .expect("claude row");
    assert!(codex < claude, "larger trailing-year token count leads");
    let codex = strip_ansi(&lines[codex]);
    let claude = strip_ansi(&lines[claude]);
    assert!(codex.contains("◎ 4"));
    assert!(codex.contains("◇ 300"));
    assert!(codex.contains("$9"));
    assert!(codex.contains("75.0%"));
    assert!(!codex.contains("sess"));
    assert!(!codex.contains("(75.0%)"));
    assert!(claude.contains("◎ 2"));
    assert!(claude.contains("25.0%"));

    let empty_cells = agent_cells(&[], 0, &glyphs);
    let mut empty_lines = Vec::new();
    emit_stat_section(
        &mut empty_lines,
        "Agents",
        &empty_cells,
        false,
        0,
        0,
        true,
        &glyphs,
    );
    assert!(empty_lines.is_empty());
}

#[test]
fn share_bar_fills_proportionally() {
    let glyphs = panel_glyphs();
    let counts = |share| {
        let bar = strip_ansi(&share_bar(share, &glyphs));
        (
            bar.matches(glyphs.bar_filled.as_str()).count(),
            bar.matches(glyphs.bar_track.as_str()).count(),
        )
    };

    assert_eq!(counts(0.0), (0, SHARE_BAR_WIDTH));
    assert_eq!(counts(100.0), (SHARE_BAR_WIDTH, 0));
    assert_eq!(counts(46.6), (5, 5));
}

#[test]
fn stat_sections_share_a_percent_column() {
    let glyphs = panel_glyphs();
    let stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::from([(
            "claude-opus-4-8".to_owned(),
            ModelSpend {
                tokens: 1_000,
                usd: 12.0,
                input: 200,
                output: 800,
                cache_read: 50,
            },
        )]),
        by_agent: BTreeMap::from([("claude".to_owned(), tally(1_000, 12.0, 4))]),
        total: SpendTally::default(),
    };
    let models = model_breakdown(&stats);
    let agents = agent_year_breakdown(&stats);
    let name_w = models
        .iter()
        .map(|(name, _)| display_width(name))
        .chain(agents.iter().map(|agent| display_width(&agent.name)))
        .max()
        .unwrap_or(0);
    let model_rows = model_cells(&models, name_w, &glyphs);
    let agent_rows = agent_cells(&agents, name_w, &glyphs);
    let pct_w = stat_pct_width(&model_rows, &agent_rows);
    let (compact, left_w, show_bar) = stat_section_layout(&model_rows, &agent_rows, pct_w, 120);
    let mut lines = Vec::new();

    emit_stat_section(
        &mut lines,
        "Models",
        &model_rows,
        compact,
        left_w,
        pct_w,
        show_bar,
        &glyphs,
    );
    emit_stat_section(
        &mut lines,
        "Agents",
        &agent_rows,
        compact,
        left_w,
        pct_w,
        show_bar,
        &glyphs,
    );
    let model = strip_ansi(
        lines
            .iter()
            .find(|line| line.contains("Opus 4.8"))
            .expect("model row"),
    );
    let agent = strip_ansi(
        lines
            .iter()
            .find(|line| line.contains("Claude"))
            .expect("agent row"),
    );
    let pct_col = |line: &str| display_width(line.split_once('%').expect("pct").0);

    assert_eq!(pct_col(&model), pct_col(&agent));
}

#[test]
fn insights_sessions_line_has_no_glyph() {
    let mut stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::new(),
        by_agent: BTreeMap::new(),
        total: SpendTally::default(),
    };
    stats.total.year.sessions = 7;
    let mut lines = Vec::new();

    insights_lines(&mut lines, &stats, 0, 80);

    let row = strip_ansi(&lines[0]);
    let modern_glyphs = resolve_panel_glyphs(&ThemeConfig {
        style: Some(rimz::config::ThemeStyle::Modern),
        ..Default::default()
    });
    assert!(row.trim_start().starts_with("Sessions: 7"));
    assert!(!row.contains('◎'));
    assert!(!row.contains(modern_glyphs.sessions.as_str()));
}

#[test]
fn friendly_model_names() {
    assert_eq!(friendly_model("claude-opus-4-8"), "Opus 4.8");
    assert_eq!(friendly_model("claude-haiku-4-5"), "Haiku 4.5");
    assert_eq!(friendly_model("claude-fable-5"), "Fable 5");
    assert_eq!(friendly_model("claude-opus-4-7-20260101"), "Opus 4.7");
    assert_eq!(friendly_model("gpt-5"), "GPT-5");
    assert_eq!(friendly_model("gpt-5-codex"), "GPT-5 Codex");
    assert_eq!(friendly_model("gpt-5.1-codex-max"), "GPT-5.1 Codex Max");
    assert_eq!(friendly_model("mystery-model"), "mystery-model");
}

#[test]
fn token_and_dollar_formatting() {
    assert_eq!(fmt_tokens(412_000_000), "412M");
    assert_eq!(fmt_tokens(5_200_000_000), "5.2B");
    assert_eq!(fmt_tokens(950_000), "950K");
    assert_eq!(fmt_tokens_lower(61_000_000), "61.0m");
    assert_eq!(fmt_tokens_lower(1_200_000_000), "1.2b");
    assert_eq!(fmt_usd(8_666.0), "$8,666");
    assert_eq!(fmt_usd(1_000_000.0), "$1,000,000");
    assert_eq!(group_thousands(1_741), "1,741");
}

#[test]
fn fmt_day_reads_month_and_day() {
    // Epoch day 0 is 1970-01-01; day 31 is 1970-02-01 (January has 31 days).
    assert_eq!(utc_date(0), "1970-01-01");
    assert_eq!(fmt_day(0), "Jan 1");
    assert_eq!(fmt_day(31), "Feb 1");
}
