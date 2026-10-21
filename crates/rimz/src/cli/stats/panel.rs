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
    pub(super) provider_rows: usize,
    pub(super) agent_rows: usize,
    pub(super) subagent_rows: usize,
}

pub(super) const PANEL_FIXED_ROWS: usize = 19;
pub(super) const PANEL_HEADER_ROWS: usize = 9;
pub(super) const SECTION_CHROME_ROWS: usize = 2;
const SECTION_FLOOR_ROWS: usize = 3;

pub(super) fn fit(
    rows: Option<usize>,
    want_header: bool,
    natural: [usize; 4],
    assist_rows: usize,
    role_note_rows: usize,
) -> PanelPlan {
    let [models, providers, agents, subagents] = natural;
    let caps = [
        models.min(MAX_MODELS),
        providers.min(MAX_SPEND_ROWS),
        agents.min(MAX_SPEND_ROWS),
        subagents.min(MAX_SPEND_ROWS),
    ];
    if !want_header {
        return PanelPlan {
            header: false,
            model_rows: caps[0],
            provider_rows: caps[1],
            agent_rows: caps[2],
            subagent_rows: caps[3],
        };
    }
    let Some(rows) = rows else {
        return PanelPlan {
            header: true,
            model_rows: caps[0],
            provider_rows: caps[1],
            agent_rows: caps[2],
            subagent_rows: caps[3],
        };
    };

    let budget = rows.saturating_sub(1);
    let assists = usize::from(assist_rows > 0) * (2 + assist_rows);
    let mut weights = natural;
    let mut floors = caps.map(|cap| cap.min(SECTION_FLOOR_ROWS));
    let section_chrome = SECTION_CHROME_ROWS * weights.iter().filter(|&&rows| rows > 0).count();
    let note_rows = usize::from(agents > 0 || subagents > 0) * role_note_rows;
    let mut fixed = PANEL_FIXED_ROWS + assists + section_chrome + note_rows;
    let floor_rows = floors.iter().sum();
    let with_header = budget.saturating_sub(fixed + PANEL_HEADER_ROWS);

    if budget >= fixed + PANEL_HEADER_ROWS && with_header >= floor_rows {
        let [model_rows, provider_rows, agent_rows, subagent_rows] =
            allocate_breakdown_rows(with_header, weights, caps, floors);
        return PanelPlan {
            header: true,
            model_rows,
            provider_rows,
            agent_rows,
            subagent_rows,
        };
    }

    for i in [3, 2] {
        let minimum_rows = weights.iter().filter(|&&rows| rows > 0).count();
        if budget >= fixed + minimum_rows {
            break;
        }
        if weights[i] == 0 {
            continue;
        }
        weights[i] = 0;
        floors[i] = 0;
        fixed -= SECTION_CHROME_ROWS;
        if weights[2] == 0 && weights[3] == 0 {
            fixed -= note_rows;
        }
    }
    let without_header = budget.saturating_sub(fixed);
    let [model_rows, provider_rows, agent_rows, subagent_rows] = allocate_breakdown_rows(
        without_header,
        weights,
        floors,
        weights.map(|rows| usize::from(rows > 0)),
    );
    PanelPlan {
        header: false,
        model_rows,
        provider_rows,
        agent_rows,
        subagent_rows,
    }
}

fn allocate_breakdown_rows(
    available: usize,
    weights: [usize; 4],
    caps: [usize; 4],
    floors: [usize; 4],
) -> [usize; 4] {
    let usable = available.min(caps.iter().sum()).max(floors.iter().sum());
    let weight: usize = weights.iter().sum();
    if weight == 0 {
        return [0; 4];
    }

    let mut rows =
        std::array::from_fn(|i| (usable * weights[i] / weight).clamp(floors[i], caps[i]));
    while rows.iter().sum::<usize>() > usable {
        // The sum exceeds usable, which is at least the sum of the floors.
        let i = (0..4)
            .find(|&i| rows[i] > floors[i])
            .expect("a row exceeds its floor");
        rows[i] -= 1;
    }
    while rows.iter().sum::<usize>() < usable {
        let Some(i) = (0..4)
            .filter(|&i| rows[i] < caps[i])
            .max_by_key(|&i| (weights[i], 4 - i))
        else {
            break;
        };
        rows[i] += 1;
    }
    rows
}

pub(super) struct PanelStats<'a> {
    pub(super) usage: &'a Stats,
    pub(super) assists: &'a AssistStats,
    pub(super) active: Window,
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

    let selected = active;
    let note = roles_note(stats, today_day, active);
    let natural_models = model_breakdown_size(stats, selected);
    let natural_providers = provider_breakdown_size(stats, selected);
    let natural_agents = if show_agents(stats, selected, has_usage) {
        breakdown_size(&stats.agents, selected)
    } else {
        0
    };
    let natural_subagents = breakdown_size(&stats.subagents, selected);
    let plan = fit(
        geometry.rows,
        include_header,
        [
            natural_models,
            natural_providers,
            natural_agents,
            natural_subagents,
        ],
        assist_categories.len().div_ceil(2),
        note.as_ref()
            .map_or(0, |note| display_width(note).div_ceil(geometry.panel_width)),
    );
    if plan.header && include_header {
        lines.extend(header_lines(geometry.panel_width));
    }

    heatmap_lines(&mut lines, stats, today_day, geometry.weeks, dollars);
    let total_usd = selected.select(&stats.total).usd;
    let models = model_breakdown(stats, selected, plan.model_rows);
    let providers = provider_breakdown(stats, selected, Some(plan.provider_rows));
    let agents = profile_breakdown(&stats.agents, selected, total_usd, Some(plan.agent_rows));
    let subagents = profile_breakdown(
        &stats.subagents,
        selected,
        total_usd,
        Some(plan.subagent_rows),
    );
    let name_w = models
        .iter()
        .map(|(name, _, _)| display_width(name))
        .chain(
            providers
                .iter()
                .map(|provider| display_width(&provider.name)),
        )
        .chain(agents.iter().map(|agent| display_width(&agent.name)))
        .chain(subagents.iter().map(|agent| display_width(&agent.name)))
        .max()
        .unwrap_or(0);

    lines.push(String::new());
    windows_lines(&mut lines, stats, Some(active));
    let model_rows = model_cells(&models, name_w, glyphs, total_usd);
    let provider_rows = provider_cells(&providers, name_w, glyphs);
    let agent_rows = spend_cells(&agents, name_w, glyphs, false);
    let subagent_rows = spend_cells(&subagents, name_w, glyphs, false);
    let sections = [
        &model_rows[..],
        &provider_rows[..],
        &agent_rows[..],
        &subagent_rows[..],
    ];
    let pct_w = stat_pct_width(&sections);
    let layout = stat_section_layout(&sections, pct_w, geometry.panel_width);
    for (label, rows) in ["Models", "Providers", "Agents", "Subagents"]
        .into_iter()
        .zip(sections)
    {
        if !rows.is_empty() {
            lines.push(String::new());
            emit_stat_section(&mut lines, label, rows, layout, glyphs);
        }
    }
    if (plan.agent_rows > 0 || plan.subagent_rows > 0)
        && let Some(note) = note
    {
        lines.push(note);
    }
    lines.push(String::new());
    insights_lines(&mut lines, stats, today_day, geometry.panel_width, selected);
    if !assist_categories.is_empty() {
        lines.push(String::new());
        assists::panel_lines(&mut lines, assists, geometry.panel_width);
    }

    emit(w, &lines, geometry.outer, nl)
}

#[derive(Clone, Copy)]
enum SpendSection {
    Models,
    Providers,
    Profiles(RoleView),
}

impl SpendSection {
    fn label(self) -> &'static str {
        match self {
            Self::Models => "Models",
            Self::Providers => "Providers",
            Self::Profiles(role) => role.label(),
        }
    }

    fn plural(self) -> &'static str {
        match self {
            Self::Models => "models",
            Self::Providers => "providers",
            Self::Profiles(role) => role.key(),
        }
    }

    fn count_header(self) -> Option<&'static str> {
        match self {
            Self::Models => None,
            Self::Providers => Some("SESSIONS"),
            Self::Profiles(role) => Some(role.count_header()),
        }
    }
}

pub(super) fn render_spend(
    w: &mut impl Write,
    stats: &Stats,
    today_day: i64,
    dollars: bool,
    role: Option<RoleView>,
    active: Window,
    nl: &str,
) -> Result<()> {
    let mut lines = Vec::new();
    if role.is_none() {
        let [_, week, month, year] = Window::TABS.map(|window| window.select(&stats.total));
        lines.push(format!(
            "  {:20} {:>12} {:>12} {:>12} {:>12}",
            "Spend", "Today", "Week", "Month", "Year"
        ));
        let today = stats.by_day.get(&today_day).cloned().unwrap_or_default();
        lines.push(format!(
            "  {:20} {:>12} {:>12} {:>12} {:>12}",
            "dollars",
            fmt_usd(today.usd),
            fmt_usd(week.usd),
            fmt_usd(month.usd),
            fmt_usd(year.usd)
        ));
        lines.push(format!(
            "  {:20} {:>12} {:>12} {:>12} {:>12}",
            "tokens",
            fmt_tokens(today.tokens),
            fmt_tokens(week.display_tokens()),
            fmt_tokens(month.display_tokens()),
            fmt_tokens(year.display_tokens())
        ));
        let models = stats
            .by_model
            .iter()
            .map(|(id, model)| {
                let name = if id.is_empty() {
                    "Unknown model".into()
                } else {
                    render::paint(
                        render::palette::identity(&model.kind),
                        &rimz::agents::model_display::display_model(id),
                    )
                };
                (name, active.select(&model.tally))
            })
            .collect();
        let providers = stats
            .by_provider
            .iter()
            .map(|(kind, tally)| {
                (
                    render::paint(render::palette::identity(kind), &agent_display_name(kind)),
                    active.select(tally),
                )
            })
            .collect();
        spend_section(
            &mut lines,
            SpendSection::Models,
            models,
            active.select(&stats.total).usd,
            Some(3),
            active,
        );
        spend_section(
            &mut lines,
            SpendSection::Providers,
            providers,
            active.select(&stats.total).usd,
            Some(3),
            active,
        );
    }
    let mut visible_profiles = false;
    for profile_role in [RoleView::Agents, RoleView::Subagents] {
        if role.is_some_and(|role| role != profile_role) {
            continue;
        }
        if role.is_none()
            && profile_role == RoleView::Agents
            && !show_agents(stats, active, has_spend)
        {
            continue;
        }
        let rows = profile_role
            .tallies(stats)
            .iter()
            .map(|(profile, tally)| (profile.clone(), active.select(tally)))
            .collect();
        let shown = spend_section(
            &mut lines,
            SpendSection::Profiles(profile_role),
            rows,
            active.select(&stats.total).usd,
            if role.is_some() { None } else { Some(3) },
            active,
        );
        visible_profiles |= shown;
        if !shown && role.is_some() {
            lines.push(format!(
                "  No {} spend recorded ({}).",
                profile_role.noun(),
                active.label().to_ascii_lowercase(),
            ));
        }
    }
    if (role.is_some() || visible_profiles)
        && let Some(note) = roles_note(stats, today_day, active)
    {
        lines.push(note);
    }
    if role.is_none() {
        lines.push(String::new());
        heatmap_lines(
            &mut lines,
            stats,
            today_day,
            PanelGeometry::current().weeks,
            dollars,
        );
    }
    let as_of = stats.as_of();
    let same_day = stats.today_day(stats.refreshed_at_ms / 1000) == today_day;
    lines.push(format!(
        "\n  as of {}",
        as_of.strftime(if same_day { "%H:%M" } else { "%b %d %H:%M" })
    ));
    emit(w, &lines, 0, nl)
}

fn share_of(value: f64, total: f64) -> f64 {
    if total > 0.0 { value / total } else { 0.0 }
}

/// One-shot text lists the rows with spend.
fn has_spend(window: &SpendWindow) -> bool {
    window.usd > 0.0
}

/// Held sections list the rows with dollars or work tokens.
fn has_usage(window: &SpendWindow) -> bool {
    window.usd > 0.0 || window.tokens > 0
}

/// Agents shows when the asking panel would list a profile not named for a
/// provider; `admits` is that panel's row admission.
fn show_agents(stats: &Stats, active: Window, admits: fn(&SpendWindow) -> bool) -> bool {
    stats.agents.iter().any(|(profile, tally)| {
        admits(&active.select(tally)) && !stats.by_provider.contains_key(profile)
    })
}

fn roles_note(stats: &Stats, today_day: i64, active: Window) -> Option<String> {
    let since = stats.recorded_since()?;
    let start = active.assist_since(stats.as_of().timestamp(), &stats.timezone)?;
    if start >= since.timestamp() {
        return None;
    }
    let today = Timestamp::from_second(today_day * DAY_SECS)
        .ok()?
        .to_zoned(jiff::tz::TimeZone::UTC);
    let date = since.strftime(if since.year() == today.year() {
        "%b %d"
    } else {
        "%b %d %Y"
    });
    Some(render::paint(
        render::palette::muted(),
        &format!("  roles recorded since {date}; earlier spend shows under Providers only"),
    ))
}

fn spend_section(
    lines: &mut Vec<String>,
    section: SpendSection,
    mut rows: Vec<(String, SpendWindow)>,
    total: f64,
    cap: Option<usize>,
    active: Window,
) -> bool {
    rows.retain(|(_, spend)| has_spend(spend));
    rows.sort_by(|a, b| b.1.usd.total_cmp(&a.1.usd).then_with(|| a.0.cmp(&b.0)));
    if rows.is_empty() {
        return false;
    }
    if let Some(cap) = cap.filter(|cap| rows.len() > *cap) {
        let tail = rows.split_off(cap);
        let mut spend = SpendWindow::default();
        for (_, row) in &tail {
            spend.merge(row);
        }
        rows.push((format!("{} other {}", tail.len(), section.plural()), spend));
    }
    let width = rows
        .iter()
        .map(|(name, _)| display_width(name))
        .max()
        .unwrap_or(0)
        .max(20);
    lines.push(String::new());
    let compact = match section {
        SpendSection::Models => false,
        SpendSection::Providers => true,
        SpendSection::Profiles(_) => cap.is_some(),
    };
    let detail_header = if compact {
        format!(
            "{:>8} {:>8} {:>4} {:>5}",
            section.count_header().unwrap_or_default(),
            "TOKENS",
            "HIT",
            "TOK/S"
        )
    } else {
        format!(
            "{:>8} {:>8} {:>8} {:>4}{}",
            "INPUT",
            "OUTPUT",
            "CACHED",
            "HIT",
            section
                .count_header()
                .map(|header| format!(" {header:>8}"))
                .unwrap_or_default(),
        )
    };
    lines.push(render::paint(
        render::palette::header(),
        &format!(
            "  {} {:>10} {:>7}  {detail_header}",
            pad_to(
                &format!(
                    "{} ({})",
                    section.label(),
                    active.label().to_ascii_lowercase()
                ),
                width
            ),
            "SPEND",
            "SHARE",
        ),
    ));
    for (name, spend) in rows {
        let hit = spend
            .cache_hit_percent()
            .map(|hit| format!("{hit}%"))
            .unwrap_or_else(|| "-".into());
        let detail = if compact {
            format!(
                "{:>8} {:>8} {:>4} {:>5}",
                spend.sessions,
                fmt_tokens(spend.display_tokens()),
                hit,
                spend
                    .tokens_per_sec()
                    .map(|rate| rate.to_string())
                    .unwrap_or_else(|| "-".into()),
            )
        } else {
            format!(
                "{:>8} {:>8} {:>8} {:>4}{}",
                fmt_tokens_precise(spend.input),
                fmt_tokens_precise(spend.output),
                fmt_tokens_precise(spend.cache_read),
                hit,
                if matches!(section, SpendSection::Profiles(_)) {
                    format!(" {:>8}", spend.sessions)
                } else {
                    String::new()
                }
            )
        };
        let name = if matches!(section, SpendSection::Profiles(_)) {
            render::paint(render::palette::muted(), &name)
        } else {
            name
        };
        lines.push(format!(
            "  {} {} {:>6.1}%  {detail}",
            pad_to(&name, width),
            render::paint(
                render::palette::money(),
                &format!("{:>10}", fmt_usd(spend.usd))
            ),
            share_of(spend.usd, total) * 100.0,
        ));
    }
    true
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
        let tokens = window.select(&stats.total).display_tokens();
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
    models: &[(String, SpendWindow, String)],
    name_w: usize,
    glyphs: &PanelGlyphs,
    total_usd: f64,
) -> Vec<StatCell> {
    struct ModelRow {
        name: String,
        kind: String,
        usd: String,
        input: String,
        output: String,
        cache_read: String,
        cache_hit: Option<(u8, String)>,
        rate: Option<String>,
        share_pct: f64,
    }

    let rows = models
        .iter()
        .map(|(name, spend, kind)| {
            let pct = if total_usd > 0.0 {
                spend.usd / total_usd * 100.0
            } else {
                0.0
            };
            ModelRow {
                name: name.clone(),
                kind: kind.clone(),
                usd: fmt_usd(spend.usd),
                input: fmt_tokens_precise(spend.input),
                output: fmt_tokens_precise(spend.output),
                cache_read: fmt_tokens_precise(spend.cache_read),
                cache_hit: spend
                    .cache_hit_percent()
                    .map(|percent| (percent, format!("{percent}%"))),
                rate: spend.tokens_per_sec().map(|rate| rate.to_string()),
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

    let rate_w = rows
        .iter()
        .filter_map(|row| row.rate.as_ref())
        .map(|rate| display_width(rate) + " tok/s".len())
        .max()
        .unwrap_or(0);

    rows.iter()
        .map(|row| {
            let identity = if row.kind.is_empty() {
                render::palette::muted()
            } else {
                render::palette::identity(&row.kind)
            };
            let name = pad_to(&render::paint(identity, &row.name), name_w);
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
            } else if rate_w > 0 && cache_hit_w > 0 {
                left_full.push_str(&format!(" {sep} {}", " ".repeat(cache_hit_w)));
            }
            if rate_w > 0 {
                let rate = row.rate.as_ref().map_or_else(
                    || pad_left("-", rate_w),
                    |rate| {
                        format!(
                            "{} {}",
                            pad_left(rate, rate_w - " tok/s".len()),
                            render::paint(render::palette::muted(), "tok/s")
                        )
                    },
                );
                left_full.push_str(&format!(" {sep} {rate}"));
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
    natural_size(model_rows(stats, active))
}

pub(super) fn model_breakdown(
    stats: &Stats,
    active: Window,
    cap: usize,
) -> Vec<(String, SpendWindow, String)> {
    fold_breakdown(model_rows(stats, active), cap.min(MAX_MODELS))
        .into_iter()
        .map(|row| match row.key {
            Some((id, kind)) => (
                rimz::agents::model_display::display_model(id),
                row.window,
                kind.to_owned(),
            ),
            None => ("Other".to_owned(), row.window, String::new()),
        })
        .collect()
}

/// Models keyed by id and producing kind; the empty id has no key, so it
/// always folds.
fn model_rows(stats: &Stats, active: Window) -> Vec<BreakdownRow<(&str, &str)>> {
    breakdown_rows(stats.by_model.iter().map(|(id, model)| {
        (
            (!id.is_empty()).then_some((id.as_str(), model.kind.as_str())),
            active.select(&model.tally),
        )
    }))
}

pub(super) struct SpendBreakdown<'a> {
    pub(super) kind: &'a str,
    pub(super) name: String,
    pub(super) window: SpendWindow,
    pub(super) share: f64,
    pub(super) folded: bool,
}

pub(super) fn provider_breakdown_size(stats: &Stats, active: Window) -> usize {
    breakdown_size(&stats.by_provider, active)
}

fn breakdown_size(tallies: &BTreeMap<String, SpendTally>, active: Window) -> usize {
    natural_size(tally_rows(tallies, active))
}

/// With `cap: None`, every admitted row unfolded (the JSON path).
pub(super) fn provider_breakdown(
    stats: &Stats,
    active: Window,
    cap: Option<usize>,
) -> Vec<SpendBreakdown<'_>> {
    let total_usd = active.select(&stats.total).usd;
    let mut rows = tally_rows(&stats.by_provider, active);
    if let Some(cap) = cap {
        rows = fold_breakdown(rows, cap.min(MAX_SPEND_ROWS));
    }
    rows.into_iter()
        .map(|row| SpendBreakdown {
            kind: row.key.unwrap_or(""),
            name: row
                .key
                .map(agent_display_name)
                .unwrap_or_else(|| "Other".to_owned()),
            share: share_of(row.window.usd, total_usd),
            window: row.window,
            folded: row.key.is_none(),
        })
        .collect()
}

/// Profiles measure significance against their own dollars and work tokens,
/// not the fleet totals used for displayed shares.
pub(super) fn profile_breakdown(
    tallies: &BTreeMap<String, SpendTally>,
    active: Window,
    total_usd: f64,
    cap: Option<usize>,
) -> Vec<SpendBreakdown<'_>> {
    let mut rows = tally_rows(tallies, active);
    if let Some(cap) = cap {
        rows = fold_breakdown(rows, cap.min(MAX_SPEND_ROWS));
    }
    rows.into_iter()
        .map(|row| SpendBreakdown {
            kind: "",
            name: row.key.unwrap_or("Other").to_owned(),
            share: share_of(row.window.usd, total_usd),
            window: row.window,
            folded: row.key.is_none(),
        })
        .collect()
}

struct BreakdownRow<K> {
    key: Option<K>,
    window: SpendWindow,
}

impl<K: Ord> BreakdownRow<K> {
    fn display_cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .window
            .usd
            .total_cmp(&self.window.usd)
            .then_with(|| other.window.tokens.cmp(&self.window.tokens))
            .then_with(|| self.key.cmp(&other.key))
    }
}

/// Admits the rows with dollars or work tokens, in display order.
fn breakdown_rows<K: Ord>(
    rows: impl Iterator<Item = (Option<K>, SpendWindow)>,
) -> Vec<BreakdownRow<K>> {
    let mut rows: Vec<_> = rows
        .filter(|(_, window)| has_usage(window))
        .map(|(key, window)| BreakdownRow { key, window })
        .collect();
    rows.sort_by(BreakdownRow::display_cmp);
    rows
}

fn tally_rows(tallies: &BTreeMap<String, SpendTally>, active: Window) -> Vec<BreakdownRow<&str>> {
    breakdown_rows(
        tallies
            .iter()
            .map(|(key, tally)| (Some(key.as_str()), active.select(tally))),
    )
}

/// A section's row count before any cap cut: `fit`'s proportional weight.
fn natural_size<K: Ord>(rows: Vec<BreakdownRow<K>>) -> usize {
    fold_breakdown(rows, usize::MAX).len()
}

/// Names the rows holding `MIN_BREAKDOWN_SHARE` of the section's dollars or
/// work tokens and merges the rest, keyless rows included, into a trailing
/// `Other` (key `None`). Past `cap`, the least significant named rows join it.
fn fold_breakdown<K: Ord>(rows: Vec<BreakdownRow<K>>, cap: usize) -> Vec<BreakdownRow<K>> {
    if cap == 0 {
        return Vec::new();
    }
    let total_usd: f64 = rows.iter().map(|row| row.window.usd).sum();
    let total_tokens: u64 = rows.iter().map(|row| row.window.tokens).sum();
    let significance = |row: &BreakdownRow<K>| {
        share_of(row.window.usd, total_usd)
            .max(share_of(row.window.tokens as f64, total_tokens as f64))
    };
    let (mut named, mut folded): (Vec<_>, Vec<_>) = rows
        .into_iter()
        .partition(|row| row.key.is_some() && significance(row) >= MIN_BREAKDOWN_SHARE);
    if named.len() + usize::from(!folded.is_empty()) > cap {
        named.sort_by(|a, b| significance(b).total_cmp(&significance(a)));
        folded.extend(named.split_off(cap - 1));
        named.sort_by(BreakdownRow::display_cmp);
    }
    if !folded.is_empty() {
        let mut window = SpendWindow::default();
        for row in &folded {
            window.merge(&row.window);
        }
        named.push(BreakdownRow { key: None, window });
    }
    named
}

pub(super) fn provider_cells(
    providers: &[SpendBreakdown<'_>],
    name_w: usize,
    glyphs: &PanelGlyphs,
) -> Vec<StatCell> {
    spend_cells(providers, name_w, glyphs, true)
}

fn spend_cells(
    agents: &[SpendBreakdown<'_>],
    name_w: usize,
    glyphs: &PanelGlyphs,
    show_rate: bool,
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
        rate: Option<String>,
    }

    let rows = agents
        .iter()
        .map(|agent| AgentRow {
            kind: agent.kind.to_owned(),
            name: agent.name.clone(),
            sessions: agent.window.sessions.to_string(),
            tokens: fmt_tokens(agent.window.display_tokens()),
            usd: fmt_usd(agent.window.usd),
            cache_hit: agent
                .window
                .cache_hit_percent()
                .map(|percent| (percent, format!("{percent}%"))),
            share_pct: agent.share * 100.0,
            folded: agent.folded,
            rate: if show_rate {
                agent.window.tokens_per_sec().map(|rate| rate.to_string())
            } else {
                None
            },
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
    let rate_w = rows
        .iter()
        .filter_map(|row| row.rate.as_ref())
        .map(|rate| display_width(rate) + " tok/s".len())
        .max()
        .unwrap_or(0);

    rows.iter()
        .map(|row| {
            let identity = if row.folded || row.kind.is_empty() {
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
            } else if rate_w > 0 && cache_hit_w > 0 {
                left_full.push_str(&format!(" {sep} {}", " ".repeat(cache_hit_w)));
            }
            if rate_w > 0 {
                let rate = row.rate.as_ref().map_or_else(
                    || pad_left("-", rate_w),
                    |rate| {
                        format!(
                            "{} {}",
                            pad_left(rate, rate_w - " tok/s".len()),
                            render::paint(render::palette::muted(), "tok/s")
                        )
                    },
                );
                left_full.push_str(&format!(" {sep} {rate}"));
            }
            StatCell {
                left_full,
                left_compact,
                share_pct: row.share_pct,
            }
        })
        .collect()
}

pub(super) fn stat_pct_width(sections: &[&[StatCell]]) -> usize {
    sections
        .iter()
        .flat_map(|rows| rows.iter())
        .map(|cell| display_width(&format!("{:.1}", cell.share_pct)))
        .max()
        .unwrap_or(0)
}

pub(super) fn stat_section_layout(
    sections: &[&[StatCell]],
    pct_w: usize,
    panel_width: usize,
) -> StatSectionLayout {
    let full_left_w = stat_left_width(sections, false);
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
        let compact_left_w = stat_left_width(sections, true);
        StatSectionLayout {
            compact: true,
            left_w: compact_left_w,
            pct_w,
            bar_w: 0,
        }
    }
}

pub(super) fn stat_left_width(sections: &[&[StatCell]], compact: bool) -> usize {
    sections
        .iter()
        .flat_map(|rows| rows.iter())
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
    let (average_span, average_usd) = match active {
        Window::Today => (Window::Year.span_days(), stats.total.year.usd),
        _ => (active.span_days(), selected.usd),
    };
    let average_cutoff = today_day - i64::from(average_span - 1);
    let average_active_days = stats
        .by_day
        .iter()
        .filter(|(day, spend)| **day >= average_cutoff && **day <= today_day && spend.tokens > 0)
        .count();
    let daily_average = if average_active_days > 0 {
        rimz::theme::fmt::dollars2(average_usd / average_active_days as f64)
    } else {
        "—".to_owned()
    };
    let left = [
        kv(
            "Sessions:",
            &rimz::theme::fmt::group_thousands(selected.sessions as u64),
        ),
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
                    Window::Today | Window::Year => "window",
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
        Window::Today | Window::Year => return None,
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

        if matches!(window, Window::Today | Window::Year) {
            const RECENT_ACTIVITY_DAYS: u32 = 28;
            let active_count = (today_day - i64::from(RECENT_ACTIVITY_DAYS - 1)..=today_day)
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
                window_days: RECENT_ACTIVITY_DAYS,
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
