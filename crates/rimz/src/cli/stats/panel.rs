use super::fmt::*;
use super::*;
use rimz::theme::theme_glyphs;

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
    let glyph = theme_glyphs(theme);
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
    pub(super) rows: Option<usize>,
}

impl PanelGeometry {
    pub(super) fn current() -> Self {
        let cols = render::terminal_columns(80);
        let weeks = weeks_for_terminal(cols);
        let panel_width = GUTTER + weeks * 2;
        let outer = cols.saturating_sub(panel_width) / 2;
        let rows = std::io::stdout()
            .is_terminal()
            .then(|| render::terminal_rows(24));
        PanelGeometry {
            weeks,
            panel_width,
            outer,
            rows,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PanelPlan {
    pub(super) header: bool,
    pub(super) model_rows: usize,
    pub(super) agent_rows: usize,
}

pub(super) const PANEL_FIXED_ROWS: usize = 19;
pub(super) const PANEL_HEADER_ROWS: usize = 9;
pub(super) const SECTION_CHROME_ROWS: usize = 2;
const SECTION_FLOOR_ROWS: usize = 3;

pub(super) fn fit(
    rows: Option<usize>,
    want_header: bool,
    models: usize,
    agents: usize,
    assist_rows: usize,
) -> PanelPlan {
    let model_cap = models.min(MAX_MODELS);
    let agent_cap = agents.min(MAX_AGENTS);
    if !want_header {
        return PanelPlan {
            header: false,
            model_rows: model_cap,
            agent_rows: agent_cap,
        };
    }
    let Some(rows) = rows else {
        return PanelPlan {
            header: true,
            model_rows: model_cap,
            agent_rows: agent_cap,
        };
    };

    let budget = rows.saturating_sub(1);
    let assists = usize::from(assist_rows > 0) * (2 + assist_rows);
    let section_chrome = SECTION_CHROME_ROWS * (usize::from(models > 0) + usize::from(agents > 0));
    let fixed = PANEL_FIXED_ROWS + assists + section_chrome;
    let model_floor = model_cap.min(SECTION_FLOOR_ROWS);
    let agent_floor = agent_cap.min(SECTION_FLOOR_ROWS);
    let floor_rows = model_floor + agent_floor;
    let with_header = budget.saturating_sub(fixed + PANEL_HEADER_ROWS);

    if budget >= fixed + PANEL_HEADER_ROWS && with_header >= floor_rows {
        let (model_rows, agent_rows) = allocate_breakdown_rows(
            with_header,
            [models, agents],
            [model_cap, agent_cap],
            [model_floor, agent_floor],
        );
        return PanelPlan {
            header: true,
            model_rows,
            agent_rows,
        };
    }

    let without_header = budget.saturating_sub(fixed);
    let (model_rows, agent_rows) = allocate_breakdown_rows(
        without_header,
        [models, agents],
        [model_floor, agent_floor],
        [usize::from(models > 0), usize::from(agents > 0)],
    );
    PanelPlan {
        header: false,
        model_rows,
        agent_rows,
    }
}

fn allocate_breakdown_rows(
    available: usize,
    weights: [usize; 2],
    caps: [usize; 2],
    floors: [usize; 2],
) -> (usize, usize) {
    let [model_weight, agent_weight] = weights;
    let [model_cap, agent_cap] = caps;
    let [model_floor, agent_floor] = floors;
    let floor = model_floor + agent_floor;
    let usable = available.min(model_cap + agent_cap).max(floor);
    let weight = model_weight + agent_weight;
    if weight == 0 {
        return (0, 0);
    }

    let mut model_rows = (usable * model_weight / weight).clamp(model_floor, model_cap);
    let mut agent_rows = (usable * agent_weight / weight).clamp(agent_floor, agent_cap);
    while model_rows + agent_rows > usable {
        if model_rows > model_floor {
            model_rows -= 1;
        } else if agent_rows > agent_floor {
            agent_rows -= 1;
        }
    }
    while model_rows + agent_rows < usable {
        let model_open = model_rows < model_cap;
        let agent_open = agent_rows < agent_cap;
        if model_open && (!agent_open || model_weight >= agent_weight) {
            model_rows += 1;
        } else if agent_open {
            agent_rows += 1;
        } else {
            break;
        }
    }
    (model_rows, agent_rows)
}

pub(super) struct PanelStats<'a> {
    pub(super) usage: &'a Stats,
    pub(super) assists: &'a AssistStats,
    pub(super) active: Option<Window>,
}

pub(super) fn render_panel(
    w: &mut impl Write,
    stats: PanelStats<'_>,
    today_day: i64,
    dollars: bool,
    glyphs: &PanelGlyphs,
    include_header: bool,
    nl: &str,
) -> Result<()> {
    let PanelStats {
        usage: stats,
        assists,
        active,
    } = stats;
    let geometry = PanelGeometry::current();
    let assist_categories = assists::category_rows(&assists.rollup);
    let mut lines: Vec<String> = Vec::new();

    if stats.by_day.is_empty() {
        if include_header {
            lines.extend(header_lines(geometry.panel_width));
        }
        let message = "No token usage recorded yet - run an agent and check back.";
        lines.push(center(
            &render::paint(render::palette::muted(), message),
            message.chars().count(),
            geometry.panel_width,
        ));
        if !assist_categories.is_empty() {
            lines.push(String::new());
            assists::panel_lines(&mut lines, assists, geometry.panel_width);
        }
        return emit(w, &lines, geometry.outer, nl);
    }

    let selected = active.unwrap_or(Window::AllTime);
    let natural_models = model_breakdown_size(stats, selected);
    let natural_agents = agent_breakdown_size(stats, selected);
    let plan = fit(
        geometry.rows,
        include_header,
        natural_models,
        natural_agents,
        assist_categories.len().div_ceil(2),
    );
    if plan.header && include_header {
        lines.extend(header_lines(geometry.panel_width));
    }

    heatmap_lines(&mut lines, stats, today_day, geometry.weeks, dollars);
    let models = model_breakdown(stats, selected, plan.model_rows);
    let agents = agent_breakdown(stats, selected, Some(plan.agent_rows));
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
    if !assist_categories.is_empty() {
        lines.push(String::new());
        assists::panel_lines(&mut lines, assists, geometry.panel_width);
    }

    emit(w, &lines, geometry.outer, nl)
}

pub(super) fn render_unavailable(w: &mut impl Write, error: &str, nl: &str) -> Result<()> {
    let geometry = PanelGeometry::current();
    let mut lines = header_lines(geometry.panel_width);
    let message = ellipsize(
        &format!("Spending refresh unavailable - retrying. {error}"),
        geometry.panel_width,
    );
    lines.push(center(
        &render::paint(render::palette::muted(), &message),
        display_width(&message),
        geometry.panel_width,
    ));
    emit(w, &lines, geometry.outer, nl)
}

pub(super) fn ellipsize(text: &str, max_cells: usize) -> String {
    if display_width(text) <= max_cells {
        return text.to_owned();
    }
    if max_cells == 0 {
        return String::new();
    }

    let mut clipped = String::new();
    let mut used = 0;
    for character in text.chars() {
        let width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + width > max_cells - 1 {
            break;
        }
        used += width;
        clipped.push(character);
    }
    clipped.push('…');
    clipped
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
            render::paint(render::palette::accent().bold(), line)
        ));
    }
    lines.push(center(
        &render::paint(render::palette::muted(), TAGLINE),
        TAGLINE.chars().count(),
        panel_width,
    ));
    lines.push(String::new());
    lines
}

/// Print the assembled panel, each line indented to centre the block on screen.
pub(super) fn emit(w: &mut impl Write, lines: &[String], outer: usize, nl: &str) -> Result<()> {
    let pad = " ".repeat(outer);
    for line in lines {
        if line.is_empty() {
            write!(w, "{nl}")?;
        } else {
            write!(w, "{pad}{line}{nl}")?;
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
        render::paint(render::palette::meta(), header)
    ));
    lines.push(String::new());
    lines.push(render::paint(render::palette::muted(), &month_row(&grid)));

    let styles = ramp_styles();
    for row in 0..7 {
        let label = match row {
            0 => "Mon",
            2 => "Wed",
            4 => "Fri",
            _ => "",
        };
        let mut line = render::paint(render::palette::muted(), &format!("  {label:<4}"));
        for week in &grid.cells {
            match week[row] {
                Some(value) => {
                    let t = shade(value, grid.ceiling);
                    let lvl = level(t);
                    let style = if lvl == 0 {
                        render::palette::faint()
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
    lines.push(String::new());
    lines.push(format!("  {}", ramp_key(&styles)));
}

/// The compact `Less · ░ ▒ ▓ █ More` key in the cool ramp.
pub(super) fn ramp_key(styles: &[anstyle::Style; 5]) -> String {
    let mut s = format!("{} ", render::paint(render::palette::muted(), "Less"));
    for (lvl, glyph) in RAMP.iter().enumerate() {
        s.push_str(&render::paint(styles[lvl], &glyph.to_string()));
        s.push(' ');
    }
    s.push_str(&render::paint(render::palette::muted(), "More"));
    s
}

/// The windows row: a static totals row in reports, a tab bar in held dashboards.
pub(super) fn windows_lines(lines: &mut Vec<String>, stats: &Stats, active: Option<Window>) {
    let cells = Window::TABS.map(|window| {
        let tokens = stats_tokens(&window.select(&stats.total));
        (window, window.label(), fmt_tokens(tokens))
    });
    let sep = render::paint(render::palette::muted(), "  ·  ");
    let Some(active) = active else {
        let row = cells
            .into_iter()
            .map(|(_, label, tokens)| {
                format!(
                    "{} {}",
                    render::paint(render::palette::muted(), label),
                    render::paint(render::palette::cool(), &tokens)
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
                    render::paint(render::palette::muted(), label),
                    render::paint(render::palette::cool(), &tokens)
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
        cache_hit: Option<(u8, String)>,
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
                cache_hit: spend
                    .cache_hit_percent()
                    .map(|percent| (percent, format!("{percent}%"))),
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
    let cache_hit_w = rows
        .iter()
        .filter_map(|row| {
            row.cache_hit
                .as_ref()
                .map(|(_, label)| display_width(label))
        })
        .max()
        .unwrap_or(0);
    let sep = render::paint(render::palette::muted(), "·");

    rows.iter()
        .map(|row| {
            let name = pad_to(&render::paint(render::palette::muted(), &row.name), name_w);
            let mut left_full = format!(
                "{} {name} {} {sep} {} {} {sep} {} {} {sep} {} {}",
                render::paint(render::palette::cool(), "●"),
                render::paint(render::palette::money(), &pad_left(&row.usd, usd_w)),
                render::paint(render::palette::muted(), &glyphs.input),
                pad_left(&row.input, input_w),
                render::paint(render::palette::muted(), &glyphs.output),
                pad_left(&row.output, output_w),
                render::paint(render::palette::muted(), &glyphs.cache_read),
                pad_left(&row.cache_read, cache_w),
            );
            if let Some((percent, cache_hit)) = row.cache_hit.as_ref() {
                let style = match rimz::agents::CacheHealth::classify(*percent) {
                    rimz::agents::CacheHealth::Good => render::palette::good(),
                    rimz::agents::CacheHealth::Caution => render::palette::warn(),
                    rimz::agents::CacheHealth::Alarm => render::palette::alarm(),
                };
                left_full.push_str(&format!(
                    " {sep} {}",
                    render::paint(style, &pad_left(cache_hit, cache_hit_w))
                ));
            }
            let left_compact = format!(
                "{} {name} {}",
                render::paint(render::palette::cool(), "●"),
                render::paint(render::palette::money(), &pad_left(&row.usd, usd_w)),
            );
            StatCell {
                left_full,
                left_compact,
                share_pct: row.share_pct,
            }
        })
        .collect()
}

pub(super) fn model_breakdown_size(stats: &Stats, active: Window) -> usize {
    let total_usd: f64 = stats
        .by_model
        .values()
        .map(|tally| active.select(tally))
        .filter(|spend| spend.tokens > 0)
        .map(|spend| spend.usd)
        .sum();
    let mut named = 0;
    let mut other = false;
    for (id, tally) in &stats.by_model {
        let spend = active.select(tally);
        if spend.tokens == 0 {
            continue;
        }
        if id.is_empty() || below_minimum_share(spend.usd, total_usd) {
            other = true;
        } else {
            named += 1;
        }
    }
    named + usize::from(other)
}

pub(super) fn model_breakdown(
    stats: &Stats,
    active: Window,
    cap: usize,
) -> Vec<(String, SpendWindow)> {
    let total: u64 = stats
        .by_model
        .values()
        .map(|tally| active.select(tally).tokens)
        .sum();
    if total == 0 {
        return Vec::new();
    }
    let total_usd: f64 = stats
        .by_model
        .values()
        .map(|tally| active.select(tally))
        .filter(|spend| spend.tokens > 0)
        .map(|spend| spend.usd)
        .sum();

    let mut named: Vec<(String, SpendWindow)> = Vec::new();
    let mut other = SpendWindow::default();
    for (id, tally) in &stats.by_model {
        let spend = active.select(tally);
        if spend.tokens == 0 {
            continue;
        }
        if id.is_empty() || below_minimum_share(spend.usd, total_usd) {
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
    let cap = cap.min(MAX_MODELS);
    if cap == 0 {
        return Vec::new();
    }
    let natural_rows = named.len() + usize::from(other.tokens > 0);
    if natural_rows > cap {
        for (_, spend) in named.split_off(cap - 1) {
            fold_window(&mut other, &spend);
        }
    }
    if other.tokens > 0 {
        named.push(("Other".to_string(), other));
    }
    named
}

fn below_minimum_share(value: f64, total: f64) -> bool {
    total > 0.0 && value < total * MIN_BREAKDOWN_SHARE
}

pub(super) fn fold_window(acc: &mut SpendWindow, add: &SpendWindow) {
    acc.usd += add.usd;
    acc.tokens += add.tokens;
    acc.input += add.input;
    acc.output += add.output;
    acc.cache_write += add.cache_write;
    acc.cache_read += add.cache_read;
    acc.tool_calls = acc.tool_calls.saturating_add(add.tool_calls);
    for (name, count) in &add.tools {
        let total = acc.tools.entry(name.clone()).or_default();
        *total = total.saturating_add(*count);
    }
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
    pub(super) folded: bool,
}

pub(super) fn agent_breakdown_size(stats: &Stats, active: Window) -> usize {
    let total_sessions: u32 = stats
        .by_agent
        .values()
        .map(|tally| active.select(tally).sessions)
        .sum();
    let mut named = 0;
    let mut other = false;
    for tally in stats.by_agent.values() {
        let window = active.select(tally);
        if window.tokens == 0 {
            continue;
        }
        if below_minimum_share(window.sessions.into(), total_sessions.into()) {
            other = true;
        } else {
            named += 1;
        }
    }
    named + usize::from(other)
}

pub(super) fn agent_breakdown(
    stats: &Stats,
    active: Window,
    cap: Option<usize>,
) -> Vec<AgentBreakdown<'_>> {
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
            let share = if total_sessions > 0 {
                window.sessions as f64 / total_sessions as f64
            } else {
                0.0
            };
            (window.tokens > 0).then(|| AgentBreakdown {
                kind: kind.as_str(),
                name: agent_display_name(kind),
                window,
                share,
                folded: false,
            })
        })
        .collect();
    agents.sort_by(|a, b| {
        b.window
            .sessions
            .cmp(&a.window.sessions)
            .then_with(|| b.window.tokens.cmp(&a.window.tokens))
    });
    let Some(cap) = cap.map(|cap| cap.min(MAX_AGENTS)) else {
        return agents;
    };
    if cap == 0 {
        return Vec::new();
    }
    let mut other_window = SpendWindow::default();
    let mut other_share = 0.0;
    let mut has_other = false;
    agents.retain(|agent| {
        if below_minimum_share(agent.window.sessions.into(), total_sessions.into()) {
            fold_window(&mut other_window, &agent.window);
            other_share += agent.share;
            has_other = true;
            false
        } else {
            true
        }
    });
    let natural_rows = agents.len() + usize::from(has_other);
    if natural_rows > cap {
        for agent in agents.split_off(cap - 1) {
            fold_window(&mut other_window, &agent.window);
            other_share += agent.share;
            has_other = true;
        }
    }
    if has_other {
        agents.push(AgentBreakdown {
            kind: "",
            name: "Other".to_owned(),
            window: other_window,
            share: other_share,
            folded: true,
        });
    }
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
        kind: String,
        name: String,
        sessions: String,
        tokens: String,
        usd: String,
        cache_hit: Option<(u8, String)>,
        share_pct: f64,
        folded: bool,
    }

    let rows = agents
        .iter()
        .map(|agent| AgentRow {
            kind: agent.kind.to_owned(),
            name: agent.name.clone(),
            sessions: agent.window.sessions.to_string(),
            tokens: fmt_tokens(stats_tokens(&agent.window)),
            usd: fmt_usd(agent.window.usd),
            cache_hit: agent
                .window
                .cache_hit_percent()
                .map(|percent| (percent, format!("{percent}%"))),
            share_pct: agent.share * 100.0,
            folded: agent.folded,
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
    let cache_hit_w = rows
        .iter()
        .filter_map(|row| {
            row.cache_hit
                .as_ref()
                .map(|(_, label)| display_width(label))
        })
        .max()
        .unwrap_or(0);
    let sep = render::paint(render::palette::muted(), "·");

    rows.iter()
        .map(|row| {
            let identity = if row.folded {
                render::palette::muted()
            } else {
                render::palette::identity(&row.kind)
            };
            let name = pad_to(&render::paint(identity, &row.name), name_w);
            let left_compact = format!(
                "{} {name} {} {sep} {} {} {sep} {} {}",
                render::paint(identity, "●"),
                render::paint(render::palette::money(), &pad_left(&row.usd, usd_w)),
                render::paint(render::palette::muted(), &glyphs.sessions),
                pad_left(&row.sessions, sess_w),
                render::paint(render::palette::muted(), &glyphs.total),
                pad_left(&row.tokens, tok_w),
            );
            let mut left_full = left_compact.clone();
            if let Some((percent, cache_hit)) = row.cache_hit.as_ref() {
                let style = match rimz::agents::CacheHealth::classify(*percent) {
                    rimz::agents::CacheHealth::Good => render::palette::good(),
                    rimz::agents::CacheHealth::Caution => render::palette::warn(),
                    rimz::agents::CacheHealth::Alarm => render::palette::alarm(),
                };
                left_full.push_str(&format!(
                    " {sep} {}",
                    render::paint(style, &pad_left(cache_hit, cache_hit_w))
                ));
            }
            StatCell {
                left_full,
                left_compact,
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
        render::paint(render::palette::cool(), &glyphs.bar_filled.repeat(filled)),
        render::paint(
            render::palette::faint(),
            &glyphs.bar_track.repeat(width - filled)
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
        render::paint(render::palette::header(), header)
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
    let cost_per_session = if selected.sessions > 0 {
        rimz::theme::fmt::dollars2(selected.usd / f64::from(selected.sessions))
    } else {
        "—".to_owned()
    };
    let average_span = match active {
        Window::AllTime => Window::Year.span_days(),
        _ => active.span_days(),
    };
    let average_cutoff = today_day - i64::from(average_span - 1);
    let average_active_days = stats
        .by_day
        .iter()
        .filter(|(day, spend)| **day >= average_cutoff && **day <= today_day && spend.tokens > 0)
        .count();
    let daily_average = if average_active_days > 0 {
        rimz::theme::fmt::dollars2(selected.usd / average_active_days as f64)
    } else {
        "—".to_owned()
    };
    let left = [
        kv("Sessions:", &group_thousands(selected.sessions as u64)),
        kv(
            "Active days:",
            &format!("{}/{}", activity.active_count, activity.window_days),
        ),
        kv("Most active day:", &most),
        kv("Cost/session:", &cost_per_session),
    ];
    let trend = spend_trend(&stats.by_day, today_day, active)
        .map(|trend| {
            let rounded = trend.round() as i64;
            let (arrow, magnitude) = if rounded > 0 {
                ('↑', rounded)
            } else if rounded < 0 {
                ('↓', -rounded)
            } else {
                ('→', 0)
            };
            format!(
                " ({arrow}{magnitude}% vs prior {})",
                match active {
                    Window::Week => "week",
                    Window::Month => "month",
                    Window::AllTime | Window::Year => "window",
                }
            )
        })
        .unwrap_or_default();
    let right = [
        format!(
            "{} {}{}",
            render::paint(render::palette::muted(), "Spend:"),
            render::paint(
                render::palette::money(),
                &rimz::theme::fmt::dollars2(selected.usd)
            ),
            render::paint(render::palette::muted(), &trend),
        ),
        kv("Longest streak:", &plural_days(activity.longest_streak)),
        kv("Current streak:", &plural_days(activity.current_streak)),
        kv("Daily avg:", &daily_average),
    ];

    two_column(lines, &left, &right, panel_width);
}

pub(super) fn spend_trend(
    by_day: &BTreeMap<i64, DaySpend>,
    today_day: i64,
    window: Window,
) -> Option<f64> {
    let span = match window {
        Window::Week => 7,
        Window::Month => 30,
        Window::AllTime | Window::Year => return None,
    };
    let current_start = today_day - span + 1;
    let prior_start = current_start - span;
    let prior_end = current_start - 1;
    let sum = |start, end| {
        by_day
            .range(start..=end)
            .map(|(_, spend)| spend.usd)
            .sum::<f64>()
    };
    let prior = sum(prior_start, prior_end);
    (prior > 0.0).then(|| (sum(current_start, today_day) - prior) / prior * 100.0)
}

pub(super) fn two_column(
    lines: &mut Vec<String>,
    left: &[String],
    right: &[String],
    panel_width: usize,
) {
    let split = left
        .iter()
        .map(|line| display_width(line))
        .max()
        .unwrap_or(0)
        + 6;
    let right_w = right
        .iter()
        .map(|line| display_width(line))
        .max()
        .unwrap_or(0);
    if split + right_w <= panel_width {
        for index in 0..left.len().max(right.len()) {
            let left = left.get(index).map(String::as_str).unwrap_or_default();
            let right = right.get(index).map(String::as_str).unwrap_or_default();
            lines.push(
                format!("  {}{}", pad_to(left, split), right)
                    .trim_end()
                    .to_string(),
            );
        }
    } else {
        for line in left.iter().chain(right) {
            lines.push(format!("  {line}"));
        }
    }
}

/// A muted `label` followed by its value — the insight line shape.
pub(super) fn kv(label: &str, value: &str) -> String {
    format!("{} {value}", render::paint(render::palette::muted(), label))
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
    render::palette::human_chip().bold()
}

/// A continuous cool ramp, held distinct from the status reds and greens so a
/// busy day reads as volume, not as good or wrong. Density carries the reading
/// under `NO_COLOR`; this only reinforces it.
pub(super) fn heat_color(t: f64) -> anstyle::Style {
    if t < 0.34 {
        render::palette::faint()
    } else if t < 0.67 {
        render::palette::muted()
    } else {
        render::palette::cool()
    }
}

/// The compact key samples the continuous ramp at four even stops.
pub(super) fn ramp_styles() -> [anstyle::Style; 5] {
    [
        render::palette::faint(),
        heat_color(0.25),
        heat_color(0.5),
        heat_color(0.75),
        heat_color(1.0),
    ]
}
