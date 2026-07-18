use super::fmt::*;
use super::*;

pub(super) struct PanelGlyphs {
    pub(super) sessions: String,
    pub(super) total: String,
    pub(super) input: String,
    pub(super) output: String,
    pub(super) cache_read: String,
    pub(super) bar_filled: String,
    pub(super) bar_track: String,
}

pub(super) fn resolve_panel_glyphs(theme: &ThemeConfig) -> PanelGlyphs {
    let glyph = rimz::sidebar_pane::render::theme_glyphs(theme);
    PanelGlyphs {
        sessions: glyph(GlyphRole::CockpitSessions),
        total: glyph(GlyphRole::TokensTotal),
        input: glyph(GlyphRole::TokensInput),
        output: glyph(GlyphRole::TokensOutput),
        cache_read: glyph(GlyphRole::TokensCacheRead),
        bar_filled: glyph(GlyphRole::MeterBarFilled),
        bar_track: glyph(GlyphRole::MeterBarTrack),
    }
}

pub(super) fn metric(day: &DaySpend, dollars: bool) -> f64 {
    if dollars { day.usd } else { day.tokens as f64 }
}

/// Day of week with Monday = 0. Epoch day 0 (1970-01-01) is a Thursday (= 3).
pub(super) fn dow_mon0(day: i64) -> i64 {
    ((day % 7) + 3).rem_euclid(7)
}

/// The Monday that opens the week containing `day`.
pub(super) fn week_start(day: i64) -> i64 {
    day - dow_mon0(day)
}

pub(super) struct Grid {
    pub(super) weeks: usize,
    pub(super) today_day: i64,
    /// `cells[col][row]` metric for an in-range day; `None` for a future day in
    /// the current week, drawn blank like GitHub.
    pub(super) cells: Vec<[Option<f64>; 7]>,
    pub(super) ceiling: f64,
}

/// Robust ceiling so one outlier day does not compress the ramp for every
/// other active day.
const HEAT_CEILING_PERCENTILE: f64 = 0.90;
/// Daily usage is heavy-tailed; a square-root response spreads the mid-range
/// perceptually evenly across the ramp.
const HEAT_GAMMA: f64 = 0.5;
/// Any active day clearly clears empty.
const HEAT_TRACE_FLOOR: f64 = 0.15;

impl Grid {
    pub(super) fn build(
        by_day: &BTreeMap<i64, DaySpend>,
        today_day: i64,
        weeks: usize,
        dollars: bool,
    ) -> Self {
        let last_monday = week_start(today_day);
        let mut cells = Vec::with_capacity(weeks);
        let mut active = Vec::new();
        for col in 0..weeks {
            let col_monday = last_monday - ((weeks - 1 - col) as i64) * 7;
            let mut week = [None; 7];
            for (row, slot) in week.iter_mut().enumerate() {
                let day = col_monday + row as i64;
                if day > today_day {
                    continue;
                }
                let value = by_day.get(&day).map(|d| metric(d, dollars)).unwrap_or(0.0);
                *slot = Some(value);
                if value > 0.0 {
                    active.push(value);
                }
            }
            cells.push(week);
        }
        active.sort_by(f64::total_cmp);
        let ceiling = if active.is_empty() {
            0.0
        } else {
            let rank = (HEAT_CEILING_PERCENTILE * active.len() as f64).ceil() as usize;
            active[rank - 1]
        };
        Self {
            weeks,
            today_day,
            cells,
            ceiling,
        }
    }

    pub(super) fn col_monday(&self, col: usize) -> i64 {
        week_start(self.today_day) - ((self.weeks - 1 - col) as i64) * 7
    }
}

/// Shape a cell value against the robust heat ceiling, spreading heavy-tailed
/// daily usage across the perceptual ramp while keeping trace activity visible.
pub(super) fn shade(value: f64, ceiling: f64) -> f64 {
    if value <= 0.0 || ceiling <= 0.0 {
        return 0.0;
    }
    (value / ceiling)
        .clamp(0.0, 1.0)
        .powf(HEAT_GAMMA)
        .max(HEAT_TRACE_FLOOR)
}

/// Map a shaped intensity onto a ramp index `0..=4`. `·` (0) marks a day with
/// no usage; any active day reads at least `░`, so an active run renders as
/// activity rather than a gap.
pub(super) fn level(t: f64) -> usize {
    if t <= 0.0 {
        return 0;
    }
    (1 + (t * 3.0).round() as usize).min(4)
}

// ── Rendering ────────────────────────────────────────────────────────────────

pub(super) struct PanelGeometry {
    pub(super) weeks: usize,
    pub(super) panel_width: usize,
    pub(super) outer: usize,
}

impl PanelGeometry {
    pub(super) fn current() -> Self {
        let cols = render::terminal_columns(80);
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

pub(super) struct PanelStats<'a> {
    pub(super) usage: &'a Stats,
    pub(super) assists: &'a AssistStats,
}

pub(super) fn render_panel(
    stats: PanelStats<'_>,
    today_day: i64,
    dollars: bool,
    glyphs: &PanelGlyphs,
    include_header: bool,
    nl: &str,
    active: Option<Window>,
) -> Result<()> {
    let PanelStats {
        usage: stats,
        assists,
    } = stats;
    let geometry = PanelGeometry::current();
    let mut lines: Vec<String> = Vec::new();
    if include_header {
        lines.extend(header_lines(geometry.panel_width));
    }

    if stats.by_day.is_empty() {
        let message = "No token usage recorded yet - run an agent and check back.";
        lines.push(center(
            &render::paint(render::palette::MUTED, message),
            message.chars().count(),
            geometry.panel_width,
        ));
        if !assists.is_empty() {
            lines.push(String::new());
            assists::panel_lines(&mut lines, assists, geometry.panel_width, 5);
        }
        return emit(&lines, geometry.outer, nl);
    }

    heatmap_lines(&mut lines, stats, today_day, geometry.weeks, dollars);
    let selected = active.unwrap_or(Window::AllTime);
    let models = model_breakdown(stats, selected);
    let agents = agent_breakdown(stats, selected);
    let name_w = models
        .iter()
        .map(|(name, _)| display_width(name))
        .chain(agents.iter().map(|agent| display_width(&agent.name)))
        .max()
        .unwrap_or(0);

    lines.push(String::new());
    windows_lines(&mut lines, stats, active);
    let model_rows = model_cells(&models, name_w, glyphs);
    let agent_rows = agent_cells(&agents, name_w, glyphs);
    let pct_w = stat_pct_width(&model_rows, &agent_rows);
    let layout = stat_section_layout(&model_rows, &agent_rows, pct_w, geometry.panel_width);
    if !model_rows.is_empty() {
        lines.push(String::new());
        emit_stat_section(&mut lines, "Models", &model_rows, layout, glyphs);
    }
    if !agent_rows.is_empty() {
        lines.push(String::new());
        emit_stat_section(&mut lines, "Agents", &agent_rows, layout, glyphs);
    }
    lines.push(String::new());
    insights_lines(&mut lines, stats, today_day, geometry.panel_width, selected);
    if !assists.is_empty() {
        lines.push(String::new());
        assists::panel_lines(&mut lines, assists, geometry.panel_width, 5);
    }

    emit(&lines, geometry.outer, nl)
}

pub(super) fn header_lines(panel_width: usize) -> Vec<String> {
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
        lines.push(format!(
            "{wm_indent}{}",
            render::paint(render::palette::ACCENT.bold(), line)
        ));
    }
    lines.push(center(
        &render::paint(render::palette::MUTED, TAGLINE),
        TAGLINE.chars().count(),
        panel_width,
    ));
    lines.push(String::new());
    lines
}

/// Print the assembled panel, each line indented to centre the block on screen.
pub(super) fn emit(lines: &[String], outer: usize, nl: &str) -> Result<()> {
    let pad = " ".repeat(outer);
    let mut out = render::out();
    for line in lines {
        if line.is_empty() {
            write!(out, "{nl}")?;
        } else {
            write!(out, "{pad}{line}{nl}")?;
        }
    }
    Ok(())
}

/// The heatmap: a header, the month row, seven weekday rows, and the ramp key.
pub(super) fn heatmap_lines(
    lines: &mut Vec<String>,
    stats: &Stats,
    today_day: i64,
    weeks: usize,
    dollars: bool,
) {
    let grid = Grid::build(&stats.by_day, today_day, weeks, dollars);
    let header = if dollars {
        "Spend activity"
    } else {
        "Token activity"
    };
    lines.push(format!(
        "  {}",
        render::paint(render::palette::META, header)
    ));
    lines.push(String::new());
    lines.push(render::paint(render::palette::MUTED, &month_row(&grid)));

    let styles = ramp_styles();
    for row in 0..7 {
        let label = match row {
            0 => "Mon",
            2 => "Wed",
            4 => "Fri",
            _ => "",
        };
        let mut line = render::paint(render::palette::MUTED, &format!("  {label:<4}"));
        for week in &grid.cells {
            match week[row] {
                Some(value) => {
                    let t = shade(value, grid.ceiling);
                    let lvl = level(t);
                    let style = if lvl == 0 {
                        render::palette::FAINT
                    } else {
                        heat_color(t)
                    };
                    line.push_str(&render::paint(style, &RAMP[lvl].to_string()));
                    line.push(' ');
                }
                None => line.push_str("  "),
            }
        }
        lines.push(line.trim_end().to_string());
    }
    lines.push(format!("  {}", ramp_key(&styles)));
}

/// The compact `Less · ░ ▒ ▓ █ More` key in the cool ramp.
pub(super) fn ramp_key(styles: &[anstyle::Style; 5]) -> String {
    let mut s = format!("{} ", render::paint(render::palette::MUTED, "Less"));
    for (lvl, glyph) in RAMP.iter().enumerate() {
        s.push_str(&render::paint(styles[lvl], &glyph.to_string()));
        s.push(' ');
    }
    s.push_str(&render::paint(render::palette::MUTED, "More"));
    s
}

/// The windows row: a static totals row in reports, a tab bar in held dashboards.
pub(super) fn windows_lines(lines: &mut Vec<String>, stats: &Stats, active: Option<Window>) {
    let cells = Window::TABS.map(|window| {
        let tokens = stats_tokens(&window.select(&stats.total));
        (window, window.label(), fmt_tokens(tokens))
    });
    let sep = render::paint(render::palette::MUTED, "  ·  ");
    let Some(active) = active else {
        let row = cells
            .into_iter()
            .map(|(_, label, tokens)| {
                format!(
                    "{} {}",
                    render::paint(render::palette::MUTED, label),
                    render::paint(render::palette::COOL, &tokens)
                )
            })
            .collect::<Vec<_>>()
            .join(&sep);
        lines.push(format!("  {row}"));
        return;
    };

    let inner_w = cells
        .iter()
        .map(|(_, label, tokens)| label.chars().count() + 1 + tokens.chars().count())
        .max()
        .unwrap_or(0);
    let row = cells
        .into_iter()
        .map(|(window, label, tokens)| {
            let pad = " "
                .repeat(inner_w.saturating_sub(label.chars().count() + 1 + tokens.chars().count()));
            if window == active {
                render::paint(active_tab(), &format!(" {label} {tokens}{pad} "))
            } else {
                format!(
                    " {} {}{pad} ",
                    render::paint(render::palette::MUTED, label),
                    render::paint(render::palette::COOL, &tokens)
                )
            }
        })
        .collect::<Vec<_>>()
        .join(&sep);
    lines.push(format!("  {row}"));
}

pub(super) struct StatCell {
    left_full: String,
    left_compact: String,
    share_pct: f64,
}

impl StatCell {
    fn left(&self, compact: bool) -> &str {
        if compact {
            &self.left_compact
        } else {
            &self.left_full
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct StatSectionLayout {
    pub(super) compact: bool,
    pub(super) left_w: usize,
    pub(super) pct_w: usize,
    pub(super) bar_w: usize,
}

/// The per-model token breakdown, before the shared share column is appended.
pub(super) fn model_cells(
    models: &[(String, SpendWindow)],
    name_w: usize,
    glyphs: &PanelGlyphs,
) -> Vec<StatCell> {
    let total_usd: f64 = models.iter().map(|(_, model)| model.usd).sum();

    struct ModelRow {
        name: String,
        usd: String,
        input: String,
        output: String,
        cache_read: String,
        share_pct: f64,
    }

    let rows = models
        .iter()
        .map(|(name, spend)| {
            let pct = if total_usd > 0.0 {
                spend.usd / total_usd * 100.0
            } else {
                0.0
            };
            ModelRow {
                name: name.clone(),
                usd: fmt_usd(spend.usd),
                input: fmt_tokens_lower(spend.input),
                output: fmt_tokens_lower(spend.output),
                cache_read: fmt_tokens_lower(spend.cache_read),
                share_pct: pct,
            }
        })
        .collect::<Vec<_>>();
    let usd_w = rows
        .iter()
        .map(|row| display_width(&row.usd))
        .max()
        .unwrap_or(0);
    let input_w = rows
        .iter()
        .map(|row| display_width(&row.input))
        .max()
        .unwrap_or(0);
    let output_w = rows
        .iter()
        .map(|row| display_width(&row.output))
        .max()
        .unwrap_or(0);
    let cache_w = rows
        .iter()
        .map(|row| display_width(&row.cache_read))
        .max()
        .unwrap_or(0);
    let sep = render::paint(render::palette::MUTED, "·");

    rows.iter()
        .map(|row| {
            let name = pad_to(&render::paint(render::palette::COOL, &row.name), name_w);
            let left_full = format!(
                "{} {name} {} {sep} {} {} {sep} {} {} {sep} {} {}",
                render::paint(render::palette::COOL, "●"),
                pad_left(&row.usd, usd_w),
                render::paint(render::palette::MUTED, &glyphs.input),
                pad_left(&row.input, input_w),
                render::paint(render::palette::MUTED, &glyphs.output),
                pad_left(&row.output, output_w),
                render::paint(render::palette::MUTED, &glyphs.cache_read),
                pad_left(&row.cache_read, cache_w),
            );
            let left_compact = format!(
                "{} {name} {}",
                render::paint(render::palette::COOL, "●"),
                pad_left(&row.usd, usd_w),
            );
            StatCell {
                left_full,
                left_compact,
                share_pct: row.share_pct,
            }
        })
        .collect()
}

pub(super) fn model_breakdown(stats: &Stats, active: Window) -> Vec<(String, SpendWindow)> {
    let total: u64 = stats
        .by_model
        .values()
        .map(|tally| active.select(tally).tokens)
        .sum();
    if total == 0 {
        return Vec::new();
    }

    let mut named: Vec<(String, SpendWindow)> = Vec::new();
    let mut other = SpendWindow::default();
    for (id, tally) in &stats.by_model {
        let spend = active.select(tally);
        if spend.tokens == 0 {
            continue;
        }
        if id.is_empty() {
            fold_window(&mut other, &spend);
        } else {
            named.push((rimz::agents::model_display::display_model(id), spend));
        }
    }
    named.sort_by(|a, b| {
        b.1.usd
            .partial_cmp(&a.1.usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.1.tokens.cmp(&a.1.tokens))
    });
    if named.len() > MAX_MODELS {
        for (_, spend) in named.split_off(MAX_MODELS) {
            fold_window(&mut other, &spend);
        }
    }
    if other.tokens > 0 {
        named.push(("Other".to_string(), other));
    }
    named
}

pub(super) fn fold_window(acc: &mut SpendWindow, add: &SpendWindow) {
    acc.usd += add.usd;
    acc.tokens += add.tokens;
    acc.input += add.input;
    acc.output += add.output;
    acc.cache_write += add.cache_write;
    acc.cache_read += add.cache_read;
    acc.sessions += add.sessions;
}

pub(super) fn stats_tokens(window: &SpendWindow) -> u64 {
    window.tokens + window.cache_read
}

pub(super) struct AgentBreakdown<'a> {
    pub(super) kind: &'a str,
    pub(super) name: String,
    pub(super) window: SpendWindow,
    pub(super) share: f64,
}

pub(super) fn agent_breakdown(stats: &Stats, active: Window) -> Vec<AgentBreakdown<'_>> {
    let total_sessions: u32 = stats
        .by_agent
        .values()
        .map(|tally| active.select(tally).sessions)
        .sum();

    let mut agents: Vec<_> = stats
        .by_agent
        .iter()
        .filter_map(|(kind, tally)| {
            let window = active.select(tally);
            (window.tokens > 0).then(|| AgentBreakdown {
                kind: kind.as_str(),
                name: agent_display_name(kind),
                window,
                share: if total_sessions > 0 {
                    window.sessions as f64 / total_sessions as f64
                } else {
                    0.0
                },
            })
        })
        .collect();
    agents.sort_by(|a, b| {
        b.window
            .sessions
            .cmp(&a.window.sessions)
            .then_with(|| b.window.tokens.cmp(&a.window.tokens))
    });
    agents
}

pub(super) fn agent_cells(
    agents: &[AgentBreakdown<'_>],
    name_w: usize,
    glyphs: &PanelGlyphs,
) -> Vec<StatCell> {
    if agents.is_empty() {
        return Vec::new();
    }

    struct AgentRow {
        name: String,
        sessions: String,
        tokens: String,
        usd: String,
        share_pct: f64,
    }

    let rows = agents
        .iter()
        .map(|agent| AgentRow {
            name: agent.name.clone(),
            sessions: agent.window.sessions.to_string(),
            tokens: fmt_tokens(stats_tokens(&agent.window)),
            usd: fmt_usd(agent.window.usd),
            share_pct: agent.share * 100.0,
        })
        .collect::<Vec<_>>();
    let sess_w = rows
        .iter()
        .map(|row| display_width(&row.sessions))
        .max()
        .unwrap_or(0);
    let tok_w = rows
        .iter()
        .map(|row| display_width(&row.tokens))
        .max()
        .unwrap_or(0);
    let usd_w = rows
        .iter()
        .map(|row| display_width(&row.usd))
        .max()
        .unwrap_or(0);
    let sep = render::paint(render::palette::MUTED, "·");

    rows.iter()
        .map(|row| {
            let name = pad_to(&render::paint(render::palette::COOL, &row.name), name_w);
            let left = format!(
                "{} {name} {} {} {sep} {} {} {sep} {}",
                render::paint(render::palette::COOL, "●"),
                render::paint(render::palette::MUTED, &glyphs.sessions),
                pad_left(&row.sessions, sess_w),
                render::paint(render::palette::MUTED, &glyphs.total),
                pad_left(&row.tokens, tok_w),
                pad_left(&row.usd, usd_w),
            );
            StatCell {
                left_full: left.clone(),
                left_compact: left,
                share_pct: row.share_pct,
            }
        })
        .collect()
}

pub(super) fn stat_pct_width(model_cells: &[StatCell], agent_cells: &[StatCell]) -> usize {
    model_cells
        .iter()
        .chain(agent_cells)
        .map(|cell| display_width(&format!("{:.1}", cell.share_pct)))
        .max()
        .unwrap_or(0)
}

pub(super) fn stat_section_layout(
    model_cells: &[StatCell],
    agent_cells: &[StatCell],
    pct_w: usize,
    panel_width: usize,
) -> StatSectionLayout {
    let full_left_w = stat_left_width(model_cells, agent_cells, false);
    let prefix_w = stat_prefix_width(full_left_w, pct_w);
    if prefix_w + 1 + MIN_SHARE_BAR_WIDTH <= panel_width {
        StatSectionLayout {
            compact: false,
            left_w: full_left_w,
            pct_w,
            bar_w: panel_width - prefix_w - 1,
        }
    } else if prefix_w <= panel_width {
        StatSectionLayout {
            compact: false,
            left_w: full_left_w,
            pct_w,
            bar_w: 0,
        }
    } else {
        let compact_left_w = stat_left_width(model_cells, agent_cells, true);
        StatSectionLayout {
            compact: true,
            left_w: compact_left_w,
            pct_w,
            bar_w: 0,
        }
    }
}

pub(super) fn stat_left_width(
    model_cells: &[StatCell],
    agent_cells: &[StatCell],
    compact: bool,
) -> usize {
    model_cells
        .iter()
        .chain(agent_cells)
        .map(|cell| display_width(cell.left(compact)))
        .max()
        .unwrap_or(0)
}

/// The stat row up to and including the `%`, before the share bar.
pub(super) fn stat_prefix_width(left_w: usize, pct_w: usize) -> usize {
    2 + left_w + STAT_GUTTER + pct_w + 1
}

pub(super) fn share_bar(share_pct: f64, width: usize, glyphs: &PanelGlyphs) -> String {
    let filled = ((share_pct / 100.0) * width as f64)
        .round()
        .clamp(0.0, width as f64) as usize;
    format!(
        "{}{}",
        render::paint(render::palette::COOL, &glyphs.bar_filled.repeat(filled)),
        render::paint(
            render::palette::rgb(Semantic::DEFAULT.faint),
            &glyphs.bar_track.repeat(width - filled),
        ),
    )
}

pub(super) fn emit_stat_section(
    lines: &mut Vec<String>,
    header: &str,
    cells: &[StatCell],
    layout: StatSectionLayout,
    glyphs: &PanelGlyphs,
) {
    if cells.is_empty() {
        return;
    }

    lines.push(format!(
        "  {}",
        render::paint(render::palette::META, header)
    ));
    let gutter = " ".repeat(STAT_GUTTER);
    for cell in cells {
        let pct = format!("{:.1}", cell.share_pct);
        let mut line = format!(
            "  {}{gutter}{}%",
            pad_to(cell.left(layout.compact), layout.left_w),
            pad_left(&pct, layout.pct_w),
        );
        if layout.bar_w > 0 {
            line.push(' ');
            line.push_str(&share_bar(cell.share_pct, layout.bar_w, glyphs));
        }
        lines.push(line);
    }
}

pub(super) fn insights_lines(
    lines: &mut Vec<String>,
    stats: &Stats,
    today_day: i64,
    panel_width: usize,
    active: Window,
) {
    let activity = Activity::of(&stats.by_day, today_day, active);
    let selected = active.select(&stats.total);

    let most = activity
        .most_active
        .map(fmt_day)
        .unwrap_or_else(|| "—".to_string());
    let left = [
        kv("Sessions:", &group_thousands(selected.sessions as u64)),
        kv(
            "Active days:",
            &format!("{}/{}", activity.active_count, activity.window_days),
        ),
        kv("Most active day:", &most),
    ];
    let right = [
        kv("Spend:", &fmt_usd(selected.usd)),
        kv("Longest streak:", &plural_days(activity.longest_streak)),
        kv("Current streak:", &plural_days(activity.current_streak)),
    ];

    let insight_gutter = 6;
    let split = left
        .iter()
        .map(|line| display_width(line))
        .max()
        .unwrap_or(0)
        + insight_gutter;
    let right_w = right
        .iter()
        .map(|line| display_width(line))
        .max()
        .unwrap_or(0);
    if split + right_w <= panel_width {
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
pub(super) fn kv(label: &str, value: &str) -> String {
    format!("{} {value}", render::paint(render::palette::MUTED, label))
}

/// A day count with a pluralized unit: `1 day`, `27 days`.
pub(super) fn plural_days(n: u32) -> String {
    format!("{n} day{}", if n == 1 { "" } else { "s" })
}

/// The month-abbrev header: a label sits over the column where each new month
/// begins, like the GitHub graph.
pub(super) fn month_row(grid: &Grid) -> String {
    let mut buf = vec![' '; grid.weeks * 2];
    let months = (0..grid.weeks)
        .map(|col| {
            let date = utc_date(grid.col_monday(col).max(0) as u64 * DAY_SECS as u64);
            date.get(5..7)
                .and_then(|m| m.parse::<usize>().ok())
                .filter(|m| (1..=12).contains(m))
        })
        .collect::<Vec<_>>();

    let mut col = 0;
    while col < months.len() {
        let month = months[col];
        let mut end = col + 1;
        while end < months.len() && months[end] == month {
            end += 1;
        }
        if end - col >= 2
            && let Some(idx) = month
        {
            let start = col * 2;
            for (i, ch) in MONTHS[idx - 1].chars().enumerate() {
                if let Some(slot) = buf.get_mut(start + i) {
                    *slot = ch;
                }
            }
        }
        col = end;
    }
    let cells: String = buf.into_iter().collect();
    format!("{:<GUTTER$}{}", "", cells)
}

/// Centre `text` (of `visible` printable columns) within `width`, padding left.
pub(super) fn center(text: &str, visible: usize, width: usize) -> String {
    let pad = width.saturating_sub(visible) / 2;
    format!("{}{text}", " ".repeat(pad))
}

/// `s` right-padded to `width` printable columns (ANSI-aware), for column layout.
pub(super) fn pad_to(s: &str, width: usize) -> String {
    format!("{s}{}", " ".repeat(width.saturating_sub(display_width(s))))
}

/// `s` left-padded to `width` printable columns (ANSI-aware).
pub(super) fn pad_left(s: &str, width: usize) -> String {
    format!("{}{s}", " ".repeat(width.saturating_sub(display_width(s))))
}

/// Display width of a string, skipping ANSI SGR escapes.
pub(super) fn display_width(s: &str) -> usize {
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
            count += UnicodeWidthChar::width(c).unwrap_or(0);
        }
    }
    count
}

// ── Trailing windows and activity ──────────────────────────────────────────────

/// Cadence read from the per-day history: the selected-window active ratio, the
/// heaviest single day, and the longest and current active-day streaks.
pub(super) struct Activity {
    pub(super) active_count: u32,
    pub(super) window_days: u32,
    pub(super) most_active: Option<i64>,
    pub(super) longest_streak: u32,
    pub(super) current_streak: u32,
}

impl Activity {
    pub(super) fn of(by_day: &BTreeMap<i64, DaySpend>, today_day: i64, window: Window) -> Self {
        let active: BTreeSet<i64> = by_day
            .iter()
            .filter(|(_, d)| d.tokens > 0)
            .map(|(&day, _)| day)
            .collect();

        if window == Window::AllTime {
            let active_count = (today_day - 27..=today_day)
                .filter(|day| active.contains(day))
                .count() as u32;
            let most_active = by_day
                .iter()
                .filter(|(_, d)| d.tokens > 0)
                .max_by_key(|(_, d)| d.tokens)
                .map(|(&day, _)| day);
            let (longest_streak, current_streak) = streaks(&active, today_day);
            return Activity {
                active_count,
                window_days: window.span_days(),
                most_active,
                longest_streak,
                current_streak,
            };
        }

        let span = window.span_days();
        let cutoff = today_day - (span as i64 - 1);
        let scoped_active: BTreeSet<i64> = active
            .into_iter()
            .filter(|day| *day >= cutoff && *day <= today_day)
            .collect();
        let active_count = scoped_active.len() as u32;

        let most_active = by_day
            .iter()
            .filter(|(day, d)| **day >= cutoff && **day <= today_day && d.tokens > 0)
            .max_by_key(|(_, d)| d.tokens)
            .map(|(&day, _)| day);
        let (longest_streak, current_streak) = streaks(&scoped_active, today_day);

        Self {
            active_count,
            window_days: span,
            most_active,
            longest_streak,
            current_streak,
        }
    }
}

pub(super) fn streaks(active: &BTreeSet<i64>, today_day: i64) -> (u32, u32) {
    // Longest run of consecutive active days in the selected set.
    let mut longest_streak = 0;
    let mut run = 0;
    let mut prev: Option<i64> = None;
    for &day in active {
        run = if prev == Some(day - 1) { run + 1 } else { 1 };
        longest_streak = longest_streak.max(run);
        prev = Some(day);
    }

    // Current run ending at today; a today with no activity yet does not break
    // a streak that ran through yesterday.
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

    (longest_streak, current_streak)
}

// ── Styling ────────────────────────────────────────────────────────────────────

pub(super) fn active_tab() -> anstyle::Style {
    anstyle::Style::new()
        .fg_color(Some(render::palette::rgb_color(
            Semantic::DEFAULT.selection_bg,
        )))
        .bg_color(Some(render::palette::rgb_color(Semantic::DEFAULT.cool)))
        .bold()
}

/// A continuous cool ramp, held distinct from the status reds and greens so a
/// busy day reads as volume, not as good or wrong. Density carries the reading
/// under `NO_COLOR`; this only reinforces it.
pub(super) fn heat_color(t: f64) -> anstyle::Style {
    let cool = Semantic::DEFAULT.cool;
    let low = rimz::sidebar_pane::render::blend(Semantic::DEFAULT.faint, cool, 0.35);
    render::palette::rgb(rimz::sidebar_pane::render::blend(low, cool, t as f32))
}

/// The compact key samples the continuous ramp at four even stops.
pub(super) fn ramp_styles() -> [anstyle::Style; 5] {
    [
        render::palette::rgb(Semantic::DEFAULT.faint),
        heat_color(0.25),
        heat_color(0.5),
        heat_color(0.75),
        heat_color(1.0),
    ]
}
