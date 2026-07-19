use super::*;
use rimz::theme::theme_glyphs;
use std::path::Path;

fn day(tokens: u64, usd: f64) -> DaySpend {
    DaySpend { tokens, usd }
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
    let by_model = BTreeMap::from([("gpt-5-codex".to_owned(), model_tally(100, 7.0, 70, 30, 5))]);
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
fn stats_serves_published_cache_without_walking() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        RuntimePaths::under(rimz::WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    ensure_shared_runtime(&runtime).unwrap();
    let by_day = BTreeMap::from([(20_000, day(40, 4.0))]);
    let by_model = BTreeMap::from([(
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
    rimz::agents::spending::write_provider_spending_cache_with_rollups(
        &runtime.shared_provider_spending_path(),
        unix_millis_now(),
        &spending,
        &by_day,
        &by_model,
    );
    let cursor_path = runtime.shared_spending_cursor_path();
    assert!(!cursor_path.exists());
    let mut walker = SpendingWalker::new();

    let stats = load_or_refresh_stats(&runtime, None, &mut walker).unwrap();

    assert_eq!(stats.by_day, by_day);
    assert_eq!(stats.by_model, by_model);
    assert_eq!(stats.total.year.tokens, 365);
    assert_eq!(stats.by_agent["claude"].year.sessions, 3);
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
    let transcript = write_jsonl(
        dir.path(),
        "claude.jsonl",
        &[&claude_line_today(1.25, "msg-1", "req-1")],
    );
    let files = vec![(
        rimz::agents::definition_by_kind("claude").unwrap(),
        transcript.clone(),
    )];

    let mut walker = SpendingWalker::new();
    let stats = compute_stats_from_files(&runtime, files, true, None, &mut walker);
    let published = read_provider_spending_cache(&runtime.shared_provider_spending_path());
    let fresh = load_published_stats(&runtime)
        .expect("published stats are current after a stats-owned refresh");
    let cursor =
        rimz::agents::spending::read_spending_cache(&runtime.shared_spending_cursor_path());

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

    let a = Activity::of(&by_day, today, Window::AllTime);
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
    let a = Activity::of(&by_day, today, Window::AllTime);
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
fn cold_spinner_requires_human_stdout_and_stderr_ttys() {
    assert!(should_animate_cold_stats(true, true, true));
    assert!(!should_animate_cold_stats(false, true, true));
    assert!(!should_animate_cold_stats(true, false, true));
    assert!(!should_animate_cold_stats(true, true, false));
}

#[test]
fn windows_wrap_forward_and_back() {
    assert_eq!(Window::AllTime.next(), Window::Week);
    assert_eq!(Window::Year.next(), Window::AllTime);
    assert_eq!(Window::AllTime.prev(), Window::Year);
    assert_eq!(Window::Week.prev(), Window::AllTime);
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
    let mut state = HeldStats::new(false, panel_glyphs(), false);
    let mut out = Vec::new();

    for expected in [Window::Week, Window::Month, Window::Year, Window::AllTime] {
        assert_eq!(
            state
                .apply_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut out,)
                .unwrap(),
            KeyOutcome::NextWindow
        );
        assert_eq!(state.active(), expected);
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
    assert_eq!(state.active(), Window::Year);
    assert!(out.is_empty());
}

#[test]
fn held_stats_preserves_reload_quit_and_hold_outcomes() {
    let mut out = Vec::new();
    let mut state = HeldStats::new(false, panel_glyphs(), false);

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

    let mut held = HeldStats::new(false, panel_glyphs(), true);
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
fn refresh_event_carries_success_and_error_without_unwinding() {
    let success = refresh_event(|| {
        let mut stats = Stats {
            by_day: BTreeMap::new(),
            by_model: BTreeMap::new(),
            by_agent: BTreeMap::new(),
            total: SpendTally::default(),
        };
        stats.total.year.tokens = 42;
        Ok(stats)
    });
    match success.stats {
        Some(Ok(stats)) => assert_eq!(stats.total.year.tokens, 42),
        _ => panic!("successful refresh must carry stats"),
    }

    let error = refresh_event(|| Err(anyhow!("refresh failed")));
    match error.stats {
        Some(Err(error)) => assert_eq!(error.to_string(), "refresh failed"),
        _ => panic!("ordinary refresh failure must stay in the event"),
    }

    let panic = refresh_event(|| -> Result<Stats> { panic!("refresh panic") });
    assert!(
        panic.stats.is_none(),
        "panic becomes an empty refresh event"
    );
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
    let grid = Grid::build(&BTreeMap::new(), 20_283, 3, false); // 2025-07-14 (Monday).

    let row = month_row(&grid);

    assert!(row.contains("Jul"));
    assert!(!row.contains("Jun"));
    assert!(!row.contains("JuJul"));
}

#[test]
fn windows_lines_fold_cache_read_so_all_time_equals_year() {
    let stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::new(),
        by_agent: BTreeMap::new(),
        total: SpendTally {
            week: spend_window(7, 0.0, 0, 0, 3, 0),
            month: spend_window(30, 0.0, 0, 0, 20, 0),
            year: spend_window(365, 0.0, 0, 0, 35, 0),
            ..Default::default()
        },
    };
    let mut lines = Vec::new();

    windows_lines(&mut lines, &stats, None);

    assert_eq!(lines.len(), 1);
    let row = strip_ansi(&lines[0]);
    assert!(row.contains("All time 400"));
    assert!(row.contains("Week 10"));
    assert!(row.contains("Month 50"));
    assert!(row.contains("Year 400"));
    assert!(!row.contains('$'));
    assert!(!row.contains('◇'));
}

#[test]
fn windows_lines_paint_only_the_active_tab_as_a_chip() {
    let mut stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::new(),
        by_agent: BTreeMap::new(),
        total: SpendTally::default(),
    };
    stats.total.week.tokens = 7;
    stats.total.month.tokens = 30;
    stats.total.year.tokens = 365;
    let mut lines = Vec::new();

    windows_lines(&mut lines, &stats, Some(Window::Week));

    let active_start = active_tab().render().to_string();
    assert_eq!(lines[0].matches(&active_start).count(), 1);
    assert!(lines[0].contains(&render::paint(active_tab(), " Week 7       ")));
    let row = strip_ansi(&lines[0]);
    assert!(row.contains("All time 365"));
    assert!(row.contains("Week 7"));
    assert!(row.contains("Month 30"));
    assert!(row.contains("Year 365"));
}

#[test]
fn panel_fit_preserves_data_before_header_chrome() {
    let used = |plan: PanelPlan, assist_rows: usize| {
        PANEL_FIXED_ROWS
            + usize::from(plan.header) * PANEL_HEADER_ROWS
            + usize::from(plan.model_rows > 0) * SECTION_CHROME_ROWS
            + usize::from(plan.agent_rows > 0) * SECTION_CHROME_ROWS
            + plan.model_rows
            + plan.agent_rows
            + usize::from(assist_rows > 0) * (2 + assist_rows)
    };

    let tall = fit(Some(80), true, 4, 5, 2);
    assert_eq!(
        tall,
        PanelPlan {
            header: true,
            model_rows: 4,
            agent_rows: 5,
        }
    );
    assert!(used(tall, 2) <= 79);

    let floor = fit(Some(37), true, 8, 10, 0);
    assert_eq!(
        floor,
        PanelPlan {
            header: true,
            model_rows: 3,
            agent_rows: 3,
        }
    );
    assert!(used(floor, 0) <= 36);

    let without_header = fit(Some(30), true, 8, 10, 0);
    assert_eq!(
        without_header,
        PanelPlan {
            header: false,
            model_rows: 3,
            agent_rows: 3,
        }
    );
    assert!(used(without_header, 0) <= 29);

    let very_short = fit(Some(24), true, 8, 10, 0);
    assert_eq!(
        very_short,
        PanelPlan {
            header: false,
            model_rows: 1,
            agent_rows: 1,
        }
    );
    assert!(used(very_short, 0) <= 23);

    let no_breakdowns = fit(Some(24), true, 0, 0, 0);
    assert!(!no_breakdowns.header);
    assert!(used(no_breakdowns, 0) <= 23);

    assert_eq!(
        fit(None, true, 8, 10, 2),
        PanelPlan {
            header: true,
            model_rows: MAX_MODELS,
            agent_rows: MAX_AGENTS,
        }
    );
    assert_eq!(
        fit(Some(24), false, 8, 10, 0),
        PanelPlan {
            header: false,
            model_rows: MAX_MODELS,
            agent_rows: MAX_AGENTS,
        }
    );
}

#[test]
fn model_cells_show_usd_and_cache_read_detail() {
    let glyphs = panel_glyphs();
    let spend = model_tally(1_700_000, 12.4, 1_200_000, 500_000, 2_500_000);
    let stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::from([
            ("claude-opus-4-8".to_owned(), spend),
            ("gpt-5".to_owned(), model_tally(1_950_000, 0.0, 0, 0, 0)),
        ]),
        by_agent: BTreeMap::new(),
        total: SpendTally::default(),
    };
    let models = model_breakdown(&stats, Window::AllTime, MAX_MODELS);
    let name_w = models
        .iter()
        .map(|(name, _)| display_width(name))
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();

    let cells = model_cells(&models, name_w, &glyphs);
    let pct_w = stat_pct_width(&cells, &[]);
    let layout = stat_section_layout(&cells, &[], pct_w, 120);
    emit_stat_section(&mut lines, "Models", &cells, layout, &glyphs);
    let row = strip_ansi(
        lines
            .iter()
            .find(|line| line.contains("Opus 4.8"))
            .expect("opus row"),
    );

    assert!(row.contains("Opus 4.8"));
    assert!(row.contains("100.0%"));
    assert!(row.contains("$12"));
    assert!(row.contains("↘ 1.2m"));
    assert!(row.contains("↗ 500.0k"));
    assert!(row.contains("◌ 2.5m"));
}

#[test]
fn model_breakdown_ranks_by_usd_before_tokens() {
    let stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::from([
            (
                "claude-opus-4-8".to_owned(),
                model_tally(100, 12.0, 40, 60, 0),
            ),
            ("gpt-5".to_owned(), model_tally(1_000, 1.0, 400, 600, 0)),
        ]),
        by_agent: BTreeMap::new(),
        total: SpendTally::default(),
    };

    let models = model_breakdown(&stats, Window::AllTime, MAX_MODELS);

    assert_eq!(models[0].0, "Opus 4.8");
    assert_eq!(models[1].0, "GPT 5");
}

#[test]
fn model_breakdown_drops_models_without_selected_window_activity() {
    let mut week_model = model_tally(1_000, 10.0, 400, 600, 90);
    week_model.week = spend_window(100, 1.0, 40, 60, 9, 0);
    let year_only = model_tally(2_000, 20.0, 800, 1_200, 180);
    let stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::from([
            ("claude-opus-4-8".to_owned(), week_model),
            ("gpt-5".to_owned(), year_only),
        ]),
        by_agent: BTreeMap::new(),
        total: SpendTally::default(),
    };

    let models = model_breakdown(&stats, Window::Week, MAX_MODELS);

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].0, "Opus 4.8");
    assert_eq!(models[0].1.tokens, 100);
    assert_eq!(models[0].1.cache_read, 9);
}

#[test]
fn model_breakdown_reserves_the_last_capped_row_for_other() {
    let stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::from([
            ("model-a".to_owned(), model_tally(500, 5.0, 500, 0, 0)),
            ("model-b".to_owned(), model_tally(400, 4.0, 400, 0, 0)),
            ("model-c".to_owned(), model_tally(300, 3.0, 300, 0, 0)),
            ("model-d".to_owned(), model_tally(200, 2.0, 200, 0, 0)),
        ]),
        by_agent: BTreeMap::new(),
        total: SpendTally::default(),
    };

    let models = model_breakdown(&stats, Window::AllTime, 3);

    assert_eq!(models.len(), 3);
    assert_eq!(models[0].0, "Model A");
    assert_eq!(models[1].0, "Model B");
    assert_eq!(models[2].0, "Other");
    assert_eq!(models[2].1.tokens, 500);
    assert_eq!(models[2].1.usd, 5.0);
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
fn agent_cells_rank_by_sessions_and_skip_empty_agents() {
    let mut claude = tally(300, 3.0, 2);
    claude.year.cache_read = 50;
    let stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::new(),
        by_agent: BTreeMap::from([
            ("claude".to_owned(), claude),
            ("codex".to_owned(), tally(100, 9.0, 4)),
        ]),
        total: SpendTally::default(),
    };
    let mut lines = Vec::new();
    let glyphs = panel_glyphs();
    let agents = agent_breakdown(&stats, Window::AllTime, Some(MAX_AGENTS));
    let name_w = agents
        .iter()
        .map(|agent| display_width(&agent.name))
        .max()
        .unwrap_or(0);

    let cells = agent_cells(&agents, name_w, &glyphs);
    let pct_w = stat_pct_width(&[], &cells);
    let layout = stat_section_layout(&[], &cells, pct_w, 120);
    emit_stat_section(&mut lines, "Agents", &cells, layout, &glyphs);

    assert!(lines[0].contains("Agents"));
    let codex = lines
        .iter()
        .position(|line| line.contains("Codex"))
        .expect("codex row");
    let claude = lines
        .iter()
        .position(|line| line.contains("Claude"))
        .expect("claude row");
    assert!(codex < claude, "larger trailing-year session count leads");
    let codex = strip_ansi(&lines[codex]);
    let claude = strip_ansi(&lines[claude]);
    assert!(codex.contains("◎ 4"));
    assert!(codex.contains("◇ 100"));
    assert!(codex.contains("$9"));
    assert!(codex.contains("66.7%"));
    assert!(!codex.contains("sess"));
    assert!(!codex.contains("(66.7%)"));
    assert!(claude.contains("◎ 2"));
    assert!(claude.contains("◇ 350"));
    assert!(claude.contains("33.3%"));

    let empty_cells = agent_cells(&[], 0, &glyphs);
    let mut empty_lines = Vec::new();
    emit_stat_section(
        &mut empty_lines,
        "Agents",
        &empty_cells,
        StatSectionLayout {
            compact: false,
            left_w: 0,
            pct_w: 0,
            bar_w: MIN_SHARE_BAR_WIDTH,
        },
        &glyphs,
    );
    assert!(empty_lines.is_empty());
}

#[test]
fn agent_breakdown_folds_tail_totals_into_other() {
    let stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::new(),
        by_agent: BTreeMap::from([
            ("agent-a".to_owned(), tally(500, 5.0, 5)),
            ("agent-b".to_owned(), tally(400, 4.0, 4)),
            ("agent-c".to_owned(), tally(300, 3.0, 3)),
            ("agent-d".to_owned(), tally(200, 2.0, 2)),
            ("agent-e".to_owned(), tally(100, 1.0, 1)),
        ]),
        total: SpendTally::default(),
    };

    let agents = agent_breakdown(&stats, Window::AllTime, Some(3));

    assert_eq!(agents.len(), 3);
    assert_eq!(agents[0].window.sessions, 5);
    assert_eq!(agents[1].window.sessions, 4);
    let other = &agents[2];
    assert_eq!(other.name, "Other");
    assert!(other.folded);
    assert_eq!(other.window.sessions, 6);
    assert_eq!(other.window.tokens, 600);
    assert_eq!(other.window.usd, 6.0);
    assert!((other.share - 0.4).abs() < f64::EPSILON);

    let glyphs = panel_glyphs();
    let cells = agent_cells(&agents, "Agent A".len(), &glyphs);
    let layout = stat_section_layout(&[], &cells, stat_pct_width(&[], &cells), 120);
    let mut lines = Vec::new();
    emit_stat_section(&mut lines, "Agents", &cells, layout, &glyphs);
    let other = lines
        .iter()
        .find(|line| line.contains("Other"))
        .expect("folded row");
    assert!(other.contains(&render::paint(render::palette::muted(), "●")));
    assert!(other.contains(&render::paint(render::palette::muted(), "Other")));

    let all = agent_breakdown(&stats, Window::AllTime, None);
    assert_eq!(all.len(), 5, "uncapped JSON data stays complete");
}

#[test]
fn share_bar_fills_proportionally() {
    let glyphs = panel_glyphs();
    let counts = |share| {
        let bar = strip_ansi(&share_bar(share, MIN_SHARE_BAR_WIDTH, &glyphs));
        (
            bar.matches(glyphs.bar_filled.as_str()).count(),
            bar.matches(glyphs.bar_track.as_str()).count(),
        )
    };

    assert_eq!(counts(0.0), (0, MIN_SHARE_BAR_WIDTH));
    assert_eq!(counts(100.0), (MIN_SHARE_BAR_WIDTH, 0));
    assert_eq!(counts(46.6), (5, 5));
}

#[test]
fn stat_share_bar_stretches_to_panel_width() {
    let glyphs = panel_glyphs();
    let stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::from([
            (
                "claude-opus-4-8".to_owned(),
                model_tally(1_000, 12.0, 200, 800, 50),
            ),
            ("gpt-5".to_owned(), model_tally(1_000, 0.0, 0, 0, 0)),
        ]),
        by_agent: BTreeMap::new(),
        total: SpendTally::default(),
    };
    let models = model_breakdown(&stats, Window::AllTime, MAX_MODELS);
    let name_w = models
        .iter()
        .map(|(name, _)| display_width(name))
        .max()
        .unwrap_or(0);
    let cells = model_cells(&models, name_w, &glyphs);
    let pct_w = stat_pct_width(&cells, &[]);
    let panel_width = 120;
    let layout = stat_section_layout(&cells, &[], pct_w, panel_width);
    let mut lines = Vec::new();

    assert!(layout.bar_w > 0);
    emit_stat_section(&mut lines, "Models", &cells, layout, &glyphs);
    let row = strip_ansi(
        lines
            .iter()
            .find(|line| line.contains("Opus 4.8"))
            .expect("model row"),
    );

    assert_eq!(display_width(&row), panel_width);
}

#[test]
fn stat_sections_share_a_percent_column() {
    let glyphs = panel_glyphs();
    let stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::from([(
            "claude-opus-4-8".to_owned(),
            model_tally(1_000, 12.0, 200, 800, 50),
        )]),
        by_agent: BTreeMap::from([("claude".to_owned(), tally(1_000, 12.0, 4))]),
        total: SpendTally::default(),
    };
    let models = model_breakdown(&stats, Window::AllTime, MAX_MODELS);
    let agents = agent_breakdown(&stats, Window::AllTime, Some(MAX_AGENTS));
    let name_w = models
        .iter()
        .map(|(name, _)| display_width(name))
        .chain(agents.iter().map(|agent| display_width(&agent.name)))
        .max()
        .unwrap_or(0);
    let model_rows = model_cells(&models, name_w, &glyphs);
    let agent_rows = agent_cells(&agents, name_w, &glyphs);
    let pct_w = stat_pct_width(&model_rows, &agent_rows);
    let layout = stat_section_layout(&model_rows, &agent_rows, pct_w, 120);
    let mut lines = Vec::new();

    emit_stat_section(&mut lines, "Models", &model_rows, layout, &glyphs);
    emit_stat_section(&mut lines, "Agents", &agent_rows, layout, &glyphs);
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

    insights_lines(&mut lines, &stats, 0, 80, Window::AllTime);

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
fn insights_sessions_scope_to_the_active_window() {
    let mut stats = Stats {
        by_day: BTreeMap::new(),
        by_model: BTreeMap::new(),
        by_agent: BTreeMap::new(),
        total: SpendTally::default(),
    };
    stats.total.week.sessions = 2;
    stats.total.week.usd = 12.0;
    stats.total.year.sessions = 7;
    stats.total.year.usd = 99.0;
    let mut lines = Vec::new();

    insights_lines(&mut lines, &stats, 0, 80, Window::Week);

    let row = strip_ansi(&lines[0]);
    assert!(
        row.trim_start().starts_with("Sessions: 2"),
        "row was {row:?}"
    );
    assert!(row.contains("$12"), "row was {row:?}");
    assert!(!row.contains("$99"), "row was {row:?}");
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

#[test]
fn assists_fold_rolls_up_benefit_and_keeps_failed_attempts_forensics() {
    use rimz::harness::assist_log::{Assist, AssistRecord, AssistWindowReset, FocusRepairOutcome};
    use rimz::harness::auto_redeem::RedeemReason;
    use rimz::harness::schedule::run_log::{
        LoopRunMode, LoopRunRecord, LoopRunResult, PingWindowOutcome,
    };
    use rimz::ids::{AgentKind, AgentSessionId};

    let ts = |second| jiff::Timestamp::from_second(second).unwrap();
    let records = vec![
        AssistRecord {
            at: ts(4_000),
            assist: Assist::AutoRedeem {
                kind: "codex".to_owned(),
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
            assist: Assist::FocusRepair {
                nonce: Some("nonce-1".to_owned()),
                workspace_id: rimz::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
                session_name: "rimz-test".to_owned(),
                generation: 7,
                evidence: Vec::new(),
                target: rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, "terminal_2"),
                outcome: FocusRepairOutcome::AcceptedUnconfirmed,
                error: None,
            },
        },
        AssistRecord {
            at: ts(4_200),
            assist: Assist::FocusRepair {
                nonce: Some("nonce-1".to_owned()),
                workspace_id: rimz::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
                session_name: "rimz-test".to_owned(),
                generation: 7,
                evidence: Vec::new(),
                target: rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, "terminal_2"),
                outcome: FocusRepairOutcome::Confirmed,
                error: None,
            },
        },
    ];
    let mut ping = LoopRunRecord::new(
        "autoping-codex",
        LoopRunResult::Completed,
        LoopRunMode::Scheduled,
        10,
    );
    ping.at = ts(5_000);
    ping.cost_usd = Some(0.09);
    ping.window = Some(PingWindowOutcome {
        shortest: Some(AssistWindowReset {
            duration_mins: Some(300),
            resets_at: Some(ts(23_000)),
        }),
        longest: None,
    });

    let stats = AssistStats::from_records("7d", records, vec![ping]);

    assert_eq!(
        stats.rollup,
        AssistRollup {
            pings: 1,
            ping_cost_usd: 0.09,
            redeems: 1,
            resets: 1,
            resumes: 1,
            recovered_secs: 3_600,
            focus_repairs: 1,
            focus_confirmed: 1,
            focus_failed: 0,
        }
    );
    assert_eq!(stats.events.len(), 6, "every assist outcome stays forensic");
    let categories = category_rows(&stats.rollup)
        .into_iter()
        .map(|row| strip_ansi(&row))
        .collect::<Vec<_>>();
    assert_eq!(categories.last().unwrap(), "Focus repair: 1");
    assert!(categories.iter().all(|row| !row.contains("confirmed")));
    let zone = jiff::tz::TimeZone::UTC;
    let lines = stats
        .events
        .iter()
        .map(|event| benefit_line(event, &zone))
        .collect::<Vec<_>>();
    assert!(
        lines
            .iter()
            .any(|line| line.contains("codex ping — window"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("expiry rescue → budget reset ✓"))
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
            .any(|line| line.contains("focus repair zellij:terminal_2 — accepted"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("focus repair zellij:terminal_2 — confirmed"))
    );

    let json = serde_json::to_value(&stats).unwrap();
    assert_eq!(json["rollup"]["resumes"], 1);
    assert_eq!(json["events"][0]["assist"], "ping");
}

#[test]
fn assists_panel_omits_empty_chrome_and_formats_the_rollup() {
    let mut lines = Vec::new();
    panel_lines(&mut lines, &AssistStats::default(), 80);
    assert!(lines.is_empty());

    let stats = AssistStats {
        window: "7d".to_owned(),
        rollup: AssistRollup {
            pings: 9,
            ping_cost_usd: 0.09,
            redeems: 2,
            resets: 1,
            resumes: 5,
            recovered_secs: 22_320,
            focus_repairs: 3,
            focus_confirmed: 0,
            focus_failed: 2,
        },
        events: Vec::new(),
    };
    let rows = category_rows(&stats.rollup)
        .into_iter()
        .map(|row| strip_ansi(&row))
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            "Auto-ping: 9 ($0.09)",
            "Auto-redeem: 2 (1 reset)",
            "Auto-continue: 5 (+6.2h)",
            "Focus repair: 3 (2 failed)",
        ]
    );
    panel_lines(&mut lines, &stats, 80);
    assert_eq!(lines.len(), 3, "header plus two paired count rows");
    let panel = strip_ansi(&lines.join("\n"));
    assert!(panel.contains("Assists"));
    assert!(panel.contains("Auto-ping: 9 ($0.09)"));
    assert!(panel.contains("Focus repair: 3 (2 failed)"));

    let bare_counts = AssistRollup {
        pings: 1,
        redeems: 1,
        resumes: 1,
        focus_repairs: 1,
        ..Default::default()
    };
    let bare = category_rows(&bare_counts)
        .into_iter()
        .map(|row| strip_ansi(&row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!bare.contains('$'));
    assert!(!bare.contains("reset"));
    assert!(!bare.contains('h'));
    assert!(!bare.contains("failed"));

    use rimz::harness::assist_log::FocusRepairOutcome;
    let superseded_only = AssistStats {
        window: "7d".to_owned(),
        rollup: AssistRollup::default(),
        events: vec![AssistEvent::FocusRepair {
            at: jiff::Timestamp::from_second(1).unwrap(),
            nonce: None,
            workspace_id: rimz::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            session_name: "rimz-test".to_owned(),
            generation: 1,
            evidence: Vec::new(),
            target: rimz::ids::PaneId::from_parts(rimz::ids::MuxName::Zellij, "terminal_2"),
            outcome: FocusRepairOutcome::Superseded,
            error: None,
        }],
    };
    lines.clear();
    panel_lines(&mut lines, &superseded_only, 80);
    assert!(lines.is_empty());
}

#[test]
fn assists_load_scopes_both_logs_to_the_selected_window() {
    use rimz::harness::assist_log::{self, Assist, AssistRecord};
    use rimz::harness::auto_redeem::RedeemReason;
    use rimz::harness::schedule::run_log::{self, LoopRunMode, LoopRunRecord, LoopRunResult};

    let dir = tempfile::tempdir().unwrap();
    let now = jiff::Timestamp::from_second(10 * DAY_SECS).unwrap();
    let old = AssistRecord {
        at: jiff::Timestamp::from_second(DAY_SECS).unwrap(),
        assist: Assist::AutoRedeem {
            kind: "codex".to_owned(),
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
    let assist_path = assist_log::log_path(dir.path());
    std::fs::create_dir_all(assist_path.parent().unwrap()).unwrap();
    std::fs::write(
        assist_path,
        format!("{}\n", serde_json::to_string(&old).unwrap()),
    )
    .unwrap();

    let mut recent_ping = LoopRunRecord::new(
        "autoping-codex",
        LoopRunResult::Completed,
        LoopRunMode::Scheduled,
        10,
    );
    recent_ping.at = now;
    std::fs::write(
        run_log::log_path(dir.path()),
        format!("{}\n", serde_json::to_string(&recent_ping).unwrap()),
    )
    .unwrap();

    let stats = AssistStats::load(dir.path(), Window::Week, now);
    assert_eq!(stats.rollup.pings, 1);
    assert_eq!(stats.rollup.redeems, 0);
    assert_eq!(stats.events.len(), 1);
}
