//! `rimz stats` — the wordmark, the token-usage heatmap, and the account-global
//! activity insights as a standalone command. The lobby embeds the same panel;
//! here it stands alone so the figures read from inside a room, where the lobby
//! never appears.
//!
//! Account-global and read-only: it reads the producer's published provider
//! aggregate when available. A standalone first launch walks transcripts once in
//! memory and never writes back, so the command adds no producer race.
//!
//! Windows: the heatmap and the model breakdown read the full available history
//! (the cache spans the trailing year); "Active days" reports the trailing four
//! weeks; the Week/Month/Year totals are the trailing 7/30/365 days.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use clap::Args;
use serde::Serialize;

use super::GlobalFlags;
use crate::cli::render;
use rimz::RuntimePaths;
use rimz::agents::AgentAdapter;
use rimz::agents::pricing::PriceBook;
use rimz::agents::spending::{
    DaySpend, ModelSpend, ProviderSpendingCache, SpendTally, compute_daily_spend,
    compute_model_breakdown, compute_spending, discover_spending_files,
    read_provider_spending_cache, read_spending_cache, unix_secs_now, utc_date,
};
use rimz::config::Semantic;

const DAY_SECS: i64 = 86_400;
/// The five-step density ramp: a calm day through your heaviest.
const RAMP: [char; 5] = ['·', '░', '▒', '▓', '█'];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
/// Widest heatmap span — about a year, GitHub's contribution window.
const MAX_WEEKS: usize = 52;
const MIN_WEEKS: usize = 4;
/// Left weekday-label column width (`"  Wed "`).
const GUTTER: usize = 6;
/// Named models shown before the rest fold into one "Other" row.
const MAX_MODELS: usize = 6;
/// A two-column body (models, insights) needs at least this much panel width;
/// narrower terminals stack to one column.
const TWO_COL_MIN: usize = 56;
const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SPINNER_TICK: Duration = Duration::from_millis(80);
const SPINNER_MIN_AGE: Duration = Duration::from_millis(150);
const SPINNER_CLEAR_COLS: usize = 96;

/// The wordmark, spaced for a monospace terminal (the README carries a variant
/// retuned for proportional HTML rendering).
const WORDMARK: &str = r#"██████╗ ██╗███╗   ███╗  ███████╗
██╔══██╗██║████╗ ████║  ╚══███╔╝
██████╔╝██║██╔████╔██║    ███╔╝
██╔══██╗██║██║╚██╔╝██║   ███╔╝
██║  ██║██║██║ ╚═╝ ██║  ███████╗
╚═╝  ╚═╝╚═╝╚═╝     ╚═╝  ╚══════╝"#;
const TAGLINE: &str = "The control room for your coding agents";

#[derive(Debug, Args)]
pub struct StatsArgs {
    /// Scale the heatmap by dollars spent instead of tokens used.
    #[arg(long)]
    pub dollars: bool,
    /// Emit the stats as JSON instead of the panel.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: StatsArgs, _globals: &GlobalFlags) -> Result<()> {
    let loaded = load_stats(!args.json)?;
    let today_day = unix_secs_now() as i64 / DAY_SECS;
    if args.json {
        return emit_json(&loaded.stats, today_day, args.dollars);
    }
    render_panel(
        &loaded.stats,
        today_day,
        args.dollars,
        !loaded.header_printed,
    )
}

struct LoadedStats {
    stats: Stats,
    header_printed: bool,
}

/// The deduplicated account-global history `rimz stats` renders: per-day buckets,
/// the per-model breakdown, and producer-priced trailing windows.
struct Stats {
    by_day: BTreeMap<i64, DaySpend>,
    by_model: BTreeMap<String, ModelSpend>,
    total: SpendTally,
}

impl Stats {
    fn from_provider(cache: ProviderSpendingCache) -> Self {
        let ProviderSpendingCache {
            days,
            models,
            spending,
            ..
        } = cache;
        Stats {
            by_day: days,
            by_model: models,
            total: spending.total,
        }
    }
}

fn load_stats(human: bool) -> Result<LoadedStats> {
    let paths = RuntimePaths::shared();
    load_stats_from_paths(&paths, human)
}

fn load_stats_from_paths(paths: &RuntimePaths, human: bool) -> Result<LoadedStats> {
    if let Some(stats) = load_published_stats(paths) {
        return Ok(LoadedStats {
            stats,
            header_printed: false,
        });
    }

    if should_animate_cold_stats(
        human,
        std::io::stdout().is_terminal(),
        std::io::stderr().is_terminal(),
    ) {
        return load_cold_stats_with_spinner(paths);
    }

    let stats = compute_cold_stats(paths);
    if human {
        write_cold_note()?;
    }
    Ok(LoadedStats {
        stats,
        header_printed: false,
    })
}

fn load_published_stats(paths: &RuntimePaths) -> Option<Stats> {
    let cache = read_provider_spending_cache(&paths.shared_provider_spending_path());
    cache
        .is_current_version()
        .then(|| Stats::from_provider(cache))
}

fn compute_cold_stats(paths: &RuntimePaths) -> Stats {
    compute_cold_stats_from_files(paths, discover_spending_files())
}

fn compute_cold_stats_from_files(
    paths: &RuntimePaths,
    files: Vec<(&'static dyn AgentAdapter, PathBuf)>,
) -> Stats {
    let mut cache = read_spending_cache(&paths.shared_spending_cursor_path());
    let total = compute_spending(&files, &mut cache, &PriceBook::embedded(), unix_secs_now()).total;
    Stats {
        by_day: compute_daily_spend(&files, &cache),
        by_model: compute_model_breakdown(&files, &cache),
        total,
    }
}

fn load_cold_stats_with_spinner(paths: &RuntimePaths) -> Result<LoadedStats> {
    let geometry = PanelGeometry::current();
    emit(&header_lines(geometry.panel_width), geometry.outer)?;

    let files = discover_spending_files();
    let file_count = files.len();
    let paths = paths.clone();
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let stats = compute_cold_stats_from_files(&paths, files);
        let _ = tx.send(stats);
    });

    let stats = wait_for_cold_stats(rx, file_count);
    if worker.join().is_err() {
        return Err(anyhow!("stats worker panicked"));
    }
    let stats = stats?;
    write_cold_note()?;
    Ok(LoadedStats {
        stats,
        header_printed: true,
    })
}

fn wait_for_cold_stats(rx: mpsc::Receiver<Stats>, file_count: usize) -> Result<Stats> {
    let start = Instant::now();
    let mut frame = 0;
    let mut shown = false;
    loop {
        match rx.recv_timeout(SPINNER_TICK) {
            Ok(stats) => {
                if shown {
                    clear_spinner_line()?;
                }
                return Ok(stats);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if start.elapsed() < SPINNER_MIN_AGE {
                    continue;
                }
                write_spinner_line(SPINNER_FRAMES[frame % SPINNER_FRAMES.len()], file_count)?;
                frame += 1;
                shown = true;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if shown {
                    clear_spinner_line()?;
                }
                return Err(anyhow!("stats worker exited before sending a result"));
            }
        }
    }
}

fn should_animate_cold_stats(human: bool, stdout_tty: bool, stderr_tty: bool) -> bool {
    human && stdout_tty && stderr_tty
}

fn write_spinner_line(frame: char, file_count: usize) -> Result<()> {
    let plural = if file_count == 1 { "" } else { "s" };
    let mut stderr = std::io::stderr().lock();
    write!(
        stderr,
        "\r{frame} Reading your history... {file_count} transcript file{plural}"
    )?;
    stderr.flush()?;
    Ok(())
}

fn clear_spinner_line() -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    write!(stderr, "\r{:<width$}\r", "", width = SPINNER_CLEAR_COLS)?;
    stderr.flush()?;
    Ok(())
}

fn write_cold_note() -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "rimz stats: computed from transcripts; start a Rimz sidebar to make this instant."
    )?;
    Ok(())
}

// ── The grid ───────────────────────────────────────────────────────────────────

/// The metric a cell scales by, per mode.
fn metric(day: &DaySpend, dollars: bool) -> f64 {
    if dollars { day.usd } else { day.tokens as f64 }
}

/// Day of week with Sunday = 0, GitHub's column start. Epoch day 0 is a
/// Thursday (= 4).
fn dow_sun0(day: i64) -> i64 {
    ((day % 7) + 4).rem_euclid(7)
}

/// The Sunday that opens the week containing `day`.
fn week_start(day: i64) -> i64 {
    day - dow_sun0(day)
}

struct Grid {
    weeks: usize,
    today_day: i64,
    /// `cells[col][row]` metric for an in-range day; `None` for a future day in
    /// the current week, drawn blank like GitHub.
    cells: Vec<[Option<f64>; 7]>,
    max: f64,
}

impl Grid {
    fn build(
        by_day: &BTreeMap<i64, DaySpend>,
        today_day: i64,
        weeks: usize,
        dollars: bool,
    ) -> Self {
        let last_sunday = week_start(today_day);
        let mut cells = Vec::with_capacity(weeks);
        let mut max = 0.0_f64;
        for col in 0..weeks {
            let col_sunday = last_sunday - ((weeks - 1 - col) as i64) * 7;
            let mut week = [None; 7];
            for (row, slot) in week.iter_mut().enumerate() {
                let day = col_sunday + row as i64;
                if day > today_day {
                    continue;
                }
                let value = by_day.get(&day).map(|d| metric(d, dollars)).unwrap_or(0.0);
                *slot = Some(value);
                max = max.max(value);
            }
            cells.push(week);
        }
        Self {
            weeks,
            today_day,
            cells,
            max,
        }
    }

    fn col_sunday(&self, col: usize) -> i64 {
        week_start(self.today_day) - ((self.weeks - 1 - col) as i64) * 7
    }
}

/// Map a cell value onto a ramp index `0..=4`, scaled to the busiest day in
/// view, so the texture reads against your own rhythm.
fn level(value: f64, max: f64) -> usize {
    if max <= 0.0 {
        return 0;
    }
    ((value / max) * 4.0).round().clamp(0.0, 4.0) as usize
}

// ── Rendering ────────────────────────────────────────────────────────────────

struct PanelGeometry {
    weeks: usize,
    panel_width: usize,
    outer: usize,
}

impl PanelGeometry {
    fn current() -> Self {
        let cols = term_cols();
        let weeks = weeks_for_terminal(cols);
        let panel_width = GUTTER + weeks * 2;
        let outer = cols.saturating_sub(panel_width) / 2;
        PanelGeometry {
            weeks,
            panel_width,
            outer,
        }
    }
}

fn render_panel(stats: &Stats, today_day: i64, dollars: bool, include_header: bool) -> Result<()> {
    let geometry = PanelGeometry::current();
    let mut lines: Vec<String> = Vec::new();
    if include_header {
        lines.extend(header_lines(geometry.panel_width));
    }

    if stats.by_day.is_empty() {
        let message = "No token usage recorded yet - run an agent and check back.";
        lines.push(center(
            &render::paint(muted(), message),
            message.chars().count(),
            geometry.panel_width,
        ));
        return emit(&lines, geometry.outer);
    }

    heatmap_lines(
        &mut lines,
        stats,
        today_day,
        geometry.weeks,
        geometry.panel_width,
        dollars,
    );
    lines.push(String::new());
    windows_lines(&mut lines, stats);
    if !stats.by_model.is_empty() {
        lines.push(String::new());
        models_lines(&mut lines, stats, geometry.panel_width);
    }
    lines.push(String::new());
    insights_lines(&mut lines, stats, today_day, geometry.panel_width);

    emit(&lines, geometry.outer)
}

fn header_lines(panel_width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    lines.push(String::new());
    // Indent the wordmark as one block (a single shared pad), so its lines stay
    // internally aligned rather than each centring to its own width.
    let art_width = WORDMARK
        .lines()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0);
    let wm_indent = " ".repeat(panel_width.saturating_sub(art_width) / 2);
    for line in WORDMARK.lines() {
        lines.push(format!("{wm_indent}{}", render::paint(brand(), line)));
    }
    lines.push(center(
        &render::paint(muted(), TAGLINE),
        TAGLINE.chars().count(),
        panel_width,
    ));
    lines.push(String::new());
    lines
}

/// Print the assembled panel, each line indented to centre the block on screen.
fn emit(lines: &[String], outer: usize) -> Result<()> {
    let pad = " ".repeat(outer);
    let mut out = render::out();
    for line in lines {
        if line.is_empty() {
            writeln!(out)?;
        } else {
            writeln!(out, "{pad}{line}")?;
        }
    }
    Ok(())
}

/// The heatmap: a header, the month row, seven weekday rows, and the ramp key.
fn heatmap_lines(
    lines: &mut Vec<String>,
    stats: &Stats,
    today_day: i64,
    weeks: usize,
    panel_width: usize,
    dollars: bool,
) {
    let grid = Grid::build(&stats.by_day, today_day, weeks, dollars);
    let header = if dollars {
        "Spend activity"
    } else {
        "Token activity"
    };
    lines.push(format!("  {}", render::paint(meta(), header)));
    lines.push(render::paint(muted(), &month_row(&grid)));

    let styles = ramp_styles();
    for row in 0..7 {
        let label = match row {
            1 => "Mon",
            3 => "Wed",
            5 => "Fri",
            _ => "",
        };
        let mut line = render::paint(muted(), &format!("  {label:<4}"));
        for week in &grid.cells {
            match week[row] {
                Some(value) => {
                    let lvl = level(value, grid.max);
                    line.push_str(&render::paint(styles[lvl], &RAMP[lvl].to_string()));
                    line.push(' ');
                }
                None => line.push_str("  "),
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines.push(justify(
        &render::paint(muted(), "Less"),
        &ramp_key(&styles),
        panel_width,
    ));
}

/// The `░ ▒ ▓ █ More` key tail, in the cool ramp (the leading `Less` is the
/// justify line's left edge).
fn ramp_key(styles: &[anstyle::Style; 5]) -> String {
    let mut s = String::new();
    for (lvl, glyph) in RAMP.iter().enumerate() {
        s.push_str(&render::paint(styles[lvl], &glyph.to_string()));
        s.push(' ');
    }
    s.push_str(&render::paint(muted(), "More"));
    s
}

/// The trailing Week / Month / Year totals, tokens and dollars, columns aligned.
fn windows_lines(lines: &mut Vec<String>, stats: &Stats) {
    for (label, win) in [
        ("Week", &stats.total.week),
        ("Month", &stats.total.month),
        ("Year", &stats.total.year),
    ] {
        let tokens = format!("◇ {}", fmt_tokens(win.tokens));
        lines.push(format!(
            "  {}{}  {}  {}",
            render::paint(muted(), &format!("{label:<6}")),
            render::paint(cool(), &format!("{tokens:<8}")),
            render::paint(muted(), "·"),
            fmt_usd(win.usd),
        ));
    }
}

/// The per-model token breakdown: bullet rows of `name (share%)` over an
/// `In / Out` line, two models per row where the panel is wide enough.
fn models_lines(lines: &mut Vec<String>, stats: &Stats, panel_width: usize) {
    let total: u64 = stats.by_model.values().map(|m| m.tokens).sum();
    if total == 0 {
        return;
    }
    let mut named: Vec<(String, ModelSpend)> = Vec::new();
    let mut other = ModelSpend::default();
    for (id, spend) in &stats.by_model {
        if id.is_empty() {
            fold_model(&mut other, spend);
        } else {
            named.push((friendly_model(id), *spend));
        }
    }
    named.sort_by_key(|model| std::cmp::Reverse(model.1.tokens));
    if named.len() > MAX_MODELS {
        for (_, spend) in named.split_off(MAX_MODELS) {
            fold_model(&mut other, &spend);
        }
    }
    if other.tokens > 0 {
        named.push(("Other".to_string(), other));
    }

    let two_col = panel_width >= TWO_COL_MIN;
    let col_w = if two_col {
        panel_width / 2
    } else {
        panel_width
    };
    lines.push(format!("  {}", render::paint(meta(), "Models")));
    for pair in named.chunks(if two_col { 2 } else { 1 }) {
        let (n0, io0) = model_cell(&pair[0].0, &pair[0].1, total);
        match pair.get(1) {
            Some((name, spend)) => {
                let (n1, io1) = model_cell(name, spend, total);
                lines.push(
                    format!("  {}{}", pad_to(&n0, col_w), n1)
                        .trim_end()
                        .to_string(),
                );
                lines.push(
                    format!("  {}{}", pad_to(&io0, col_w), io1)
                        .trim_end()
                        .to_string(),
                );
            }
            None => {
                lines.push(format!("  {n0}"));
                lines.push(format!("  {io0}"));
            }
        }
    }
}

fn fold_model(acc: &mut ModelSpend, add: &ModelSpend) {
    acc.usd += add.usd;
    acc.input += add.input;
    acc.output += add.output;
    acc.tokens += add.tokens;
}

/// `(name line, in/out line)` for one model, styled and ready to lay out.
fn model_cell(name: &str, spend: &ModelSpend, total: u64) -> (String, String) {
    let pct = spend.tokens as f64 / total as f64 * 100.0;
    let name_line = format!(
        "{} {name} {}",
        render::paint(cool(), "●"),
        render::paint(muted(), &format!("({pct:.1}%)")),
    );
    let io_line = render::paint(
        muted(),
        &format!(
            "  In: {} · Out: {}",
            fmt_tokens_lower(spend.input),
            fmt_tokens_lower(spend.output)
        ),
    );
    (name_line, io_line)
}

/// Sessions, active-day ratio, most active day, and longest / current streak.
fn insights_lines(lines: &mut Vec<String>, stats: &Stats, today_day: i64, panel_width: usize) {
    let activity = Activity::of(&stats.by_day, today_day);
    lines.push(format!(
        "  {} {}",
        render::paint(muted(), "Sessions:"),
        stats.total.year.sessions
    ));

    let most = activity
        .most_active
        .map(fmt_day)
        .unwrap_or_else(|| "—".to_string());
    let left = [
        kv("Active days:", &format!("{}/28", activity.active_28)),
        kv("Most active day:", &most),
    ];
    let right = [
        kv("Longest streak:", &plural_days(activity.longest_streak)),
        kv("Current streak:", &plural_days(activity.current_streak)),
    ];

    if panel_width >= TWO_COL_MIN {
        let split = (panel_width * 2 / 5).clamp(28, 44);
        for (l, r) in left.iter().zip(right.iter()) {
            lines.push(
                format!("  {}{}", pad_to(l, split), r)
                    .trim_end()
                    .to_string(),
            );
        }
    } else {
        for line in left.into_iter().chain(right) {
            lines.push(format!("  {line}"));
        }
    }
}

/// A muted `label` followed by its value — the insight line shape.
fn kv(label: &str, value: &str) -> String {
    format!("{} {value}", render::paint(muted(), label))
}

/// A day count with a pluralized unit: `1 day`, `27 days`.
fn plural_days(n: u32) -> String {
    format!("{n} day{}", if n == 1 { "" } else { "s" })
}

/// The month-abbrev header: a label sits over the column where each new month
/// begins, like the GitHub graph.
fn month_row(grid: &Grid) -> String {
    let mut buf = vec![' '; grid.weeks * 2];
    let mut prev = String::new();
    for col in 0..grid.weeks {
        let date = utc_date(grid.col_sunday(col).max(0) as u64 * DAY_SECS as u64);
        let month = date.get(5..7).unwrap_or("").to_owned();
        if month != prev {
            prev.clone_from(&month);
            if let Some(idx) = month.parse::<usize>().ok().filter(|m| (1..=12).contains(m)) {
                let start = col * 2;
                for (i, ch) in MONTHS[idx - 1].chars().enumerate() {
                    if let Some(slot) = buf.get_mut(start + i) {
                        *slot = ch;
                    }
                }
            }
        }
    }
    let cells: String = buf.into_iter().collect();
    format!("{:<GUTTER$}{}", "", cells)
}

/// Centre `text` (of `visible` printable columns) within `width`, padding left.
fn center(text: &str, visible: usize, width: usize) -> String {
    let pad = width.saturating_sub(visible) / 2;
    format!("{}{text}", " ".repeat(pad))
}

/// Left text, then right text pushed to `width`. ANSI-aware: padding counts
/// printable columns, not escape bytes.
fn justify(left: &str, right: &str, width: usize) -> String {
    let used = visible_len(left) + visible_len(right);
    let pad = width.saturating_sub(used).max(2);
    format!("{left}{}{right}", " ".repeat(pad))
}

/// `s` right-padded to `width` printable columns (ANSI-aware), for column layout.
fn pad_to(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(visible_len(s))))
}

/// Printable width of a string, skipping ANSI SGR escapes.
fn visible_len(s: &str) -> usize {
    let mut count = 0;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for e in chars.by_ref() {
                if e == 'm' {
                    break;
                }
            }
        } else {
            count += 1;
        }
    }
    count
}

// ── Trailing windows and activity ──────────────────────────────────────────────

/// Cadence read from the per-day history: the trailing-four-week active ratio,
/// the heaviest single day, and the longest and current active-day streaks.
struct Activity {
    active_28: u32,
    most_active: Option<i64>,
    longest_streak: u32,
    current_streak: u32,
}

impl Activity {
    fn of(by_day: &BTreeMap<i64, DaySpend>, today_day: i64) -> Self {
        let active: BTreeSet<i64> = by_day
            .iter()
            .filter(|(_, d)| d.tokens > 0)
            .map(|(&day, _)| day)
            .collect();

        let active_28 = (today_day - 27..=today_day)
            .filter(|day| active.contains(day))
            .count() as u32;

        let most_active = by_day
            .iter()
            .filter(|(_, d)| d.tokens > 0)
            .max_by_key(|(_, d)| d.tokens)
            .map(|(&day, _)| day);

        // Longest run of consecutive active days anywhere in the history.
        let mut longest_streak = 0;
        let mut run = 0;
        let mut prev: Option<i64> = None;
        for &day in &active {
            run = if prev == Some(day - 1) { run + 1 } else { 1 };
            longest_streak = longest_streak.max(run);
            prev = Some(day);
        }

        // Current run ending at today; a today with no activity yet does not
        // break a streak that ran through yesterday.
        let mut cursor = if active.contains(&today_day) {
            today_day
        } else {
            today_day - 1
        };
        let mut current_streak = 0;
        while active.contains(&cursor) {
            current_streak += 1;
            cursor -= 1;
        }

        Activity {
            active_28,
            most_active,
            longest_streak,
            current_streak,
        }
    }
}

// ── Styling ────────────────────────────────────────────────────────────────────

fn rgb(rgb: (u8, u8, u8)) -> anstyle::Style {
    anstyle::Style::new().fg_color(Some(anstyle::Color::Rgb(anstyle::RgbColor(
        rgb.0, rgb.1, rgb.2,
    ))))
}

fn brand() -> anstyle::Style {
    rgb(Semantic::DEFAULT.accent).bold()
}

fn muted() -> anstyle::Style {
    rgb(Semantic::DEFAULT.muted)
}

fn meta() -> anstyle::Style {
    rgb(Semantic::DEFAULT.meta)
}

fn cool() -> anstyle::Style {
    rgb(Semantic::DEFAULT.cool)
}

/// One cool ramp, lightness-varying, held distinct from the status reds and
/// greens so a busy day reads as volume, not as good or wrong. Density carries
/// the reading under `NO_COLOR`; this only reinforces it.
fn ramp_styles() -> [anstyle::Style; 5] {
    let scale = |(r, g, b): (u8, u8, u8), f: f32| {
        (
            (r as f32 * f) as u8,
            (g as f32 * f) as u8,
            (b as f32 * f) as u8,
        )
    };
    let cool = Semantic::DEFAULT.cool;
    [
        rgb(Semantic::DEFAULT.faint),
        rgb(scale(cool, 0.50)),
        rgb(scale(cool, 0.66)),
        rgb(scale(cool, 0.82)),
        rgb(cool),
    ]
}

// ── Model names ────────────────────────────────────────────────────────────────

/// A transcript model id rendered for people: `claude-opus-4-8` → `Opus 4.8`,
/// `gpt-5-codex` → `GPT-5 Codex`. An 8-digit date suffix is dropped; an
/// unrecognised id falls back to itself.
fn friendly_model(id: &str) -> String {
    let id = strip_date_suffix(id.trim());
    if let Some(rest) = id.strip_prefix("claude-") {
        let mut parts = rest.splitn(2, '-');
        let family = capitalize(parts.next().unwrap_or(rest));
        return match parts.next() {
            Some(ver) if !ver.is_empty() => format!("{family} {}", ver.replace('-', ".")),
            _ => family,
        };
    }
    let parts: Vec<&str> = id.split('-').collect();
    if parts.first() == Some(&"gpt") && parts.len() >= 2 {
        let mut name = format!("GPT-{}", parts[1]);
        for token in &parts[2..] {
            name.push(' ');
            name.push_str(&capitalize(token));
        }
        return name;
    }
    id.to_string()
}

/// Drop a trailing `-YYYYMMDD` 8-digit date stamp, leaving the base model id.
fn strip_date_suffix(id: &str) -> &str {
    match id.rsplit_once('-') {
        Some((base, tail)) if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => id,
    }
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

// ── Number formatting ──────────────────────────────────────────────────────────

/// Tokens as `412M`, `5.2B`, `950K`. Billions keep one decimal; smaller units
/// round.
fn fmt_tokens(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.1}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.0}M", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.0}K", n / 1e3)
    } else {
        format!("{n:.0}")
    }
}

/// Tokens at one-decimal precision in lowercase units (`61.0m`, `1.2b`), the
/// finer register the per-model In/Out lines read in.
fn fmt_tokens_lower(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.1}b", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1}m", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.1}k", n / 1e3)
    } else {
        format!("{n:.0}")
    }
}

/// Dollars as `$8,666` — rounded, thousands grouped.
fn fmt_usd(v: f64) -> String {
    let whole = v.round() as i64;
    let digits = whole.abs().to_string();
    let mut grouped = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let sign = if whole < 0 { "-" } else { "" };
    format!("{sign}${grouped}")
}

/// A day key (days since the epoch) as `May 29`.
fn fmt_day(day: i64) -> String {
    let date = utc_date(day.max(0) as u64 * DAY_SECS as u64);
    let month = date
        .get(5..7)
        .and_then(|m| m.parse::<usize>().ok())
        .filter(|m| (1..=12).contains(m));
    let dom = date.get(8..10).and_then(|d| d.parse::<u32>().ok());
    match (month, dom) {
        (Some(m), Some(d)) => format!("{} {d}", MONTHS[m - 1]),
        _ => date,
    }
}

/// Terminal width in columns; a non-TTY (a pipe) falls back to 80.
fn term_cols() -> usize {
    ratatui::crossterm::terminal::size()
        .map(|(c, _)| c as usize)
        .unwrap_or(80)
}

/// Heatmap span from the terminal width: wider screens show more weeks.
fn weeks_for_terminal(cols: usize) -> usize {
    (cols.saturating_sub(GUTTER) / 2).clamp(MIN_WEEKS, MAX_WEEKS)
}

// ── JSON ─────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct StatsJson {
    unit: &'static str,
    sessions: u32,
    active_days_28: u32,
    longest_streak: u32,
    current_streak: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    most_active_day: Option<String>,
    windows: WindowsJson,
    models: Vec<ModelJson>,
    days: Vec<DayJson>,
}

#[derive(Serialize)]
struct WindowsJson {
    week: WindowJson,
    month: WindowJson,
    year: WindowJson,
}

#[derive(Serialize)]
struct WindowJson {
    tokens: u64,
    usd: f64,
}

#[derive(Serialize)]
struct ModelJson {
    model: String,
    name: String,
    tokens: u64,
    input: u64,
    output: u64,
    usd: f64,
    share: f64,
}

#[derive(Serialize)]
struct DayJson {
    date: String,
    tokens: u64,
    usd: f64,
}

fn emit_json(stats: &Stats, today_day: i64, dollars: bool) -> Result<()> {
    let activity = Activity::of(&stats.by_day, today_day);
    let total: u64 = stats.by_model.values().map(|m| m.tokens).sum();

    let mut models: Vec<ModelJson> = stats
        .by_model
        .iter()
        .map(|(id, spend)| ModelJson {
            model: id.clone(),
            name: if id.is_empty() {
                "Other".to_string()
            } else {
                friendly_model(id)
            },
            tokens: spend.tokens,
            input: spend.input,
            output: spend.output,
            usd: spend.usd,
            share: if total > 0 {
                spend.tokens as f64 / total as f64
            } else {
                0.0
            },
        })
        .collect();
    models.sort_by_key(|model| std::cmp::Reverse(model.tokens));

    let days = stats
        .by_day
        .iter()
        .map(|(&day, spend)| DayJson {
            date: utc_date(day.max(0) as u64 * DAY_SECS as u64),
            tokens: spend.tokens,
            usd: spend.usd,
        })
        .collect();

    let doc = StatsJson {
        unit: if dollars { "usd" } else { "tokens" },
        sessions: stats.total.year.sessions,
        active_days_28: activity.active_28,
        longest_streak: activity.longest_streak,
        current_streak: activity.current_streak,
        most_active_day: activity
            .most_active
            .map(|day| utc_date(day.max(0) as u64 * DAY_SECS as u64)),
        windows: WindowsJson {
            week: WindowJson {
                tokens: stats.total.week.tokens,
                usd: stats.total.week.usd,
            },
            month: WindowJson {
                tokens: stats.total.month.tokens,
                usd: stats.total.month.usd,
            },
            year: WindowJson {
                tokens: stats.total.year.tokens,
                usd: stats.total.year.usd,
            },
        },
        models,
        days,
    };
    let rendered = serde_json::to_string_pretty(&doc).expect("StatsJson serializes");
    #[expect(clippy::print_stdout, reason = "json emitter")]
    {
        println!("{rendered}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(tokens: u64, usd: f64) -> DaySpend {
        DaySpend { tokens, usd }
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
            RuntimePaths::under(rimz::WorkspaceId::from_project_root(dir.path()), dir.path())
                .unwrap();
        let today = 20_000;
        let by_day = BTreeMap::from([(today - 10, day(40, 4.0))]);
        let by_model = BTreeMap::from([(
            "gpt-5-codex".to_owned(),
            ModelSpend {
                usd: 7.0,
                input: 70,
                output: 30,
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
        rimz::agents::spending::write_provider_spending_cache_with_rollups(
            &runtime.shared_provider_spending_path(),
            123,
            &spending,
            &by_day,
            &by_model,
        );

        let stats = load_published_stats(&runtime).expect("v3 aggregate is readable");

        assert_eq!(stats.by_day, by_day);
        assert_eq!(stats.by_model, by_model);
        assert_eq!(stats.total.week.tokens, 7);
        assert_eq!(stats.total.month.usd, 3.0);
        assert_eq!(stats.total.year.sessions, 9);
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
    }

    #[test]
    fn fmt_day_reads_month_and_day() {
        // Epoch day 0 is 1970-01-01; day 31 is 1970-02-01 (January has 31 days).
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(fmt_day(0), "Jan 1");
        assert_eq!(fmt_day(31), "Feb 1");
    }

    #[test]
    fn justify_ignores_ansi_when_padding() {
        let left = render::paint(meta(), "left");
        let right = render::paint(meta(), "right");
        let line = justify(&left, &right, 20);
        // 20 columns of printable content, escapes excluded.
        assert_eq!(visible_len(&line), 20);
    }
}
