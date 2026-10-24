use super::fmt::*;
use super::*;
use rimz::theme::theme_glyphs;

pub(super) struct PanelGlyphs {
    pub(super) bar_filled: String,
    pub(super) bar_track: String,
}

pub(super) fn resolve_panel_glyphs(theme: &ThemeConfig) -> PanelGlyphs {
    let glyph = theme_glyphs(theme);
    PanelGlyphs {
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

    fn noun(self, count: usize) -> &'static str {
        match (self, count) {
            (Self::Models, 1) => "model",
            (Self::Models, _) => "models",
            (Self::Providers, 1) => "provider",
            (Self::Providers, _) => "providers",
            (Self::Profiles(role), 1) => role.noun(),
            (Self::Profiles(role), _) => role.key(),
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
    if role.is_none()
        && stats.by_day.is_empty()
        && [RoleView::Agents, RoleView::Teams, RoleView::Subagents]
            .into_iter()
            .all(|role| {
                role.rows(stats, active)
                    .values()
                    .all(|row| !row.has_usage())
            })
    {
        lines.push("  No agent sessions recorded on this machine yet.".into());
        lines.push("  Spend appears here after your first agent session; rimz stats reads it from each agent's own session files.".into());
        lines.push(spend_footer(stats, today_day, None));
        return emit(w, &lines, 0, nl);
    }
    if role.is_none() {
        let [_, week, month, year] = Window::TABS.map(|window| window.select(&stats.total));
        let today = stats.by_day.get(&today_day).cloned().unwrap_or_default();
        let averages = Window::TABS.map(|window| {
            per_active_day(&stats.by_day, today_day, window)
                .map(fmt_usd)
                .unwrap_or_else(|| "–".into())
        });
        let trends = Window::TABS.map(|window| {
            spend_trend(&stats.by_day, today_day, window)
                .map(|trend| format!(" {}", trend_label(trend)))
                .unwrap_or_default()
        });
        // Each figure right-aligns to its column's edge, so a Week or Month trend runs into
        // the next column's padding without moving any figure.
        const COLUMN_EDGES: [usize; 4] = [35, 52, 69, 86];
        for (label, values, row_trends) in [
            (
                "Spend",
                Window::TABS.map(|window| window.label().to_owned()),
                None,
            ),
            (
                "dollars",
                [today.usd, week.usd, month.usd, year.usd].map(fmt_usd),
                Some(&trends),
            ),
            (
                "tokens",
                [
                    today.tokens,
                    week.display_tokens(),
                    month.display_tokens(),
                    year.display_tokens(),
                ]
                .map(fmt_tokens),
                None,
            ),
            ("per active day", averages, None),
        ] {
            let mut line = format!("  {label:20}");
            for (index, (value, edge)) in values.iter().zip(COLUMN_EDGES).enumerate() {
                let start = edge
                    .saturating_sub(display_width(value))
                    .max(display_width(&line) + 1);
                line = pad_to(&line, start) + value;
                line.push_str(row_trends.map_or("", |trends| trends[index].as_str()));
            }
            lines.push(line);
        }
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
                (
                    name,
                    RoleWindow {
                        spend: active.select(&model.tally),
                        ..Default::default()
                    },
                )
            })
            .collect();
        let providers = stats
            .by_provider
            .iter()
            .map(|(kind, tally)| {
                (
                    render::paint(render::palette::identity(kind), &agent_display_name(kind)),
                    RoleWindow {
                        spend: active.select(tally),
                        ..Default::default()
                    },
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
    for profile_role in [RoleView::Agents, RoleView::Teams, RoleView::Subagents] {
        if role.is_some_and(|role| role != profile_role) {
            continue;
        }
        if role.is_none()
            && profile_role == RoleView::Agents
            && !show_agents(stats, active, has_spend)
        {
            continue;
        }
        let rows = profile_role.rows(stats, active).into_iter().collect();
        let mut section_lines = Vec::new();
        let shown = spend_section(
            &mut section_lines,
            SpendSection::Profiles(profile_role),
            rows,
            active.select(&stats.total).usd,
            if role.is_some() { None } else { Some(3) },
            active,
        );
        if !visible_profiles && (shown || role.is_some()) {
            let notes = recording_notes(stats, today_day, active);
            if !notes.is_empty() {
                lines.push(String::new());
                lines.extend(notes);
            }
        }
        lines.extend(section_lines);
        visible_profiles |= shown;
        if !shown && role.is_some() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(format!(
                "  No {} spend recorded ({}).",
                profile_role.noun(),
                active.label().to_ascii_lowercase(),
            ));
        }
    }
    if role.is_none() {
        let total = active.select(&stats.total).usd;
        let assigned: f64 = [RoleView::Agents, RoleView::Teams, RoleView::Subagents]
            .into_iter()
            .flat_map(|role| role.rows(stats, active).into_values())
            .map(|row| row.spend.usd)
            .sum();
        let unassigned = (total - assigned).max(0.0);
        if unassigned >= 0.01 {
            lines.push(String::new());
            lines.push(format!(
                "  {} {} {:>6.1}%",
                pad_to(
                    &render::paint(render::palette::muted(), "No role recorded"),
                    20
                ),
                render::paint(
                    render::palette::money(),
                    &format!("{:>10}", fmt_usd(unassigned))
                ),
                share_of(unassigned, total) * 100.0,
            ));
        }
        lines.push(String::new());
        heatmap_lines(
            &mut lines,
            stats,
            today_day,
            weeks_for_terminal(render::terminal_columns(80)),
            dollars,
        );
        let activity = Activity::of(&stats.by_day, today_day, active);
        let streak = Activity::of(&stats.by_day, today_day, Window::Year).current_streak;
        let busiest = activity
            .most_active
            .map(fmt_day)
            .unwrap_or_else(|| "–".into());
        let summary = format!(
            "{} of last {} days active · streak {} · busiest {busiest}",
            activity.active_count,
            activity.window_days,
            plural_days(streak)
        );
        if let Some(key) = lines.last_mut() {
            *key = format!(
                "{}{}",
                pad_to(key, 40),
                render::paint(render::palette::muted(), &summary)
            );
        }
    }
    lines.push(spend_footer(
        stats,
        today_day,
        role.is_none().then_some(active),
    ));
    emit(w, &lines, 0, nl)
}

fn spend_footer(stats: &Stats, today_day: i64, window: Option<Window>) -> String {
    let as_of = stats.as_of();
    let same_day = stats.today_day(stats.refreshed_at_ms / 1000) == today_day;
    let mut line = format!(
        "\n  as of {}",
        as_of.strftime(if same_day { "%H:%M" } else { "%b %d %H:%M" })
    );
    let Some(window) = window else { return line };
    let spend = window.select(&stats.total);
    if spend.sessions == 0 {
        return line;
    }
    let period = match window {
        Window::Today => "today",
        Window::Week => "this week",
        Window::Month => "this month",
        Window::Year => "this year",
    };
    line.push_str(&format!(
        " · {} {} {period} at {} each",
        rimz::theme::fmt::group_thousands(u64::from(spend.sessions)),
        if spend.sessions == 1 {
            "session"
        } else {
            "sessions"
        },
        rimz::theme::fmt::dollars2(spend.usd / f64::from(spend.sessions))
    ));
    line
}

fn share_of(value: f64, total: f64) -> f64 {
    if total > 0.0 { value / total } else { 0.0 }
}

/// One-shot text lists the rows with spend.
fn has_spend(window: &SpendWindow) -> bool {
    window.usd > 0.0
}

/// JSON providers include dollars or work tokens.
fn has_usage(window: &SpendWindow) -> bool {
    window.usd > 0.0 || window.tokens > 0
}

/// Agents shows when the asking panel would list a profile not named for a
/// provider; `admits` is that panel's row admission.
fn show_agents(stats: &Stats, active: Window, admits: fn(&SpendWindow) -> bool) -> bool {
    stats.agents.iter().any(|(profile, tally)| {
        let row = active.select_role(tally);
        (admits(&row.spend) || row.has_effort()) && !stats.by_provider.contains_key(profile)
    })
}

fn recording_notes(stats: &Stats, today_day: i64, active: Window) -> Vec<String> {
    let date = |since: Option<jiff::Zoned>| {
        let since = since?;
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
        Some(date.to_string())
    };
    let note = match (date(stats.recorded_since()), date(stats.effort_since())) {
        (Some(roles), Some(effort)) => format!(
            "roles recorded since {roles}, earlier spend counts as No role · agent-time and messages since {effort}"
        ),
        (Some(roles), None) => {
            format!("roles recorded since {roles}, earlier spend counts as No role")
        }
        (None, Some(effort)) => format!("agent-time and messages recorded since {effort}"),
        (None, None) => return Vec::new(),
    };
    vec![render::paint(
        render::palette::muted(),
        &format!("  {note}"),
    )]
}

fn spend_section(
    lines: &mut Vec<String>,
    section: SpendSection,
    mut rows: Vec<(String, RoleWindow)>,
    total: f64,
    cap: Option<usize>,
    active: Window,
) -> bool {
    rows.retain(|(_, row)| has_spend(&row.spend) || row.has_effort());
    rows.sort_by(|a, b| {
        b.1.spend
            .usd
            .total_cmp(&a.1.spend.usd)
            .then_with(|| a.0.cmp(&b.0))
    });
    if rows.is_empty() {
        return false;
    }
    if let Some(cap) = cap.filter(|cap| rows.len() > *cap) {
        let tail = rows.split_off(cap);
        let mut spend = RoleWindow::default();
        for (_, row) in &tail {
            spend.merge(row);
        }
        rows.push((
            format!("{} other {}", tail.len(), section.noun(tail.len())),
            spend,
        ));
    }
    let width = rows
        .iter()
        .map(|(name, _)| display_width(name))
        .max()
        .unwrap_or(0)
        .max(20);
    let messages_width = rows
        .iter()
        .map(|(_, row)| display_width(&messages_label(row)))
        .max()
        .unwrap_or(0)
        .max(12);
    lines.push(String::new());
    let compact = match section {
        SpendSection::Models => false,
        SpendSection::Providers => true,
        SpendSection::Profiles(_) => cap.is_some(),
    };
    let effort_header = match section {
        SpendSection::Profiles(role) => format!(
            " {:>11}{}",
            "AGENT-TIME",
            if role == RoleView::Teams {
                format!(" {:>messages_width$}", "MESSAGES")
            } else {
                String::new()
            }
        ),
        _ => String::new(),
    };
    let detail_header = if compact {
        format!(
            "{:>8}{effort_header} {:>8} {:>4} {:>5}",
            section.count_header().unwrap_or_default(),
            "TOKENS",
            "HIT",
            "TOK/S"
        )
    } else {
        format!(
            "{:>8} {:>8} {:>8} {:>4}{}{effort_header}",
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
    for (name, row) in rows {
        let spend = &row.spend;
        let effort = if let SpendSection::Profiles(role) = section {
            let seconds = row.effort.active_ms / 1000;
            let active = if seconds == 0 {
                "-".into()
            } else {
                rimz::theme::fmt::active_duration(seconds)
            };
            let messages = if role != RoleView::Teams {
                String::new()
            } else {
                format!(" {:>messages_width$}", messages_label(&row))
            };
            format!(" {active:>11}{messages}")
        } else {
            String::new()
        };
        let hit = spend
            .cache_hit_percent()
            .map(|hit| format!("{hit}%"))
            .unwrap_or_else(|| "-".into());
        let detail = if compact {
            format!(
                "{:>8}{effort} {:>8} {:>4} {:>5}",
                rimz::theme::fmt::group_thousands(u64::from(spend.sessions)),
                fmt_tokens(spend.display_tokens()),
                hit,
                spend
                    .tokens_per_sec()
                    .map(|rate| rate.to_string())
                    .unwrap_or_else(|| "-".into()),
            )
        } else {
            format!(
                "{:>8} {:>8} {:>8} {:>4}{}{effort}",
                fmt_tokens_precise(spend.input),
                fmt_tokens_precise(spend.output),
                fmt_tokens_precise(spend.cache_read),
                hit,
                if matches!(section, SpendSection::Profiles(_)) {
                    format!(
                        " {:>8}",
                        rimz::theme::fmt::group_thousands(u64::from(spend.sessions))
                    )
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

pub(super) fn messages_label(row: &RoleWindow) -> String {
    match (row.messages(), row.effort.from_user) {
        (0, _) => "-".into(),
        (total, 0) => total.to_string(),
        (total, from_user) => format!("{total} ({from_user} you)"),
    }
}

pub(super) fn render_unavailable(w: &mut impl Write, error: &str, nl: &str) -> Result<()> {
    let cols = render::terminal_columns(80);
    let panel_width = GUTTER + weeks_for_terminal(cols) * 2;
    let outer = cols.saturating_sub(panel_width) / 2;
    let mut lines = header_lines(panel_width);
    let message = ellipsize(
        &format!("Spending refresh unavailable - retrying. {error}"),
        panel_width,
    );
    lines.push(center(
        &render::paint(render::palette::muted(), &message),
        display_width(&message),
        panel_width,
    ));
    emit(w, &lines, outer, nl)
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

pub(super) struct SpendBreakdown<'a> {
    pub(super) kind: &'a str,
    pub(super) name: String,
    pub(super) window: SpendWindow,
    pub(super) share: f64,
}

/// Unfolded providers for JSON, independent of held row admission and fitting.
pub(super) fn provider_breakdown(stats: &Stats, active: Window) -> Vec<SpendBreakdown<'_>> {
    let total = active.select(&stats.total).usd;
    let mut rows: Vec<_> = stats
        .by_provider
        .iter()
        .filter_map(|(kind, tally)| {
            let window = active.select(tally);
            has_usage(&window).then(|| SpendBreakdown {
                kind,
                name: agent_display_name(kind),
                share: share_of(window.usd, total),
                window,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        b.window
            .usd
            .total_cmp(&a.window.usd)
            .then_with(|| b.window.tokens.cmp(&a.window.tokens))
            .then_with(|| a.kind.cmp(b.kind))
    });
    rows
}

pub(super) fn per_active_day(
    by_day: &BTreeMap<i64, DaySpend>,
    today_day: i64,
    window: Window,
) -> Option<f64> {
    if window == Window::Today {
        return None;
    }
    let mut usd = 0.0;
    let mut count = 0;
    for day in by_day
        .range((today_day - i64::from(window.span_days()) + 1)..=today_day)
        .map(|(_, day)| day)
    {
        usd += day.usd;
        count += usize::from(day.tokens > 0);
    }
    Some(if count == 0 { 0.0 } else { usd / count as f64 })
}

pub(super) fn trend_label(trend: f64) -> String {
    let arrow = if trend >= 0.0 { '↑' } else { '↓' };
    if trend.abs() > 999.0 {
        format!("{arrow}999%+")
    } else {
        format!("{arrow}{:.0}%", trend.abs())
    }
}

pub(super) fn plural_days(days: u32) -> String {
    format!("{days} day{}", if days == 1 { "" } else { "s" })
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
        + 2;
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
                format!("{}{}", pad_to(left, split), right)
                    .trim_end()
                    .to_string(),
            );
        }
    } else {
        for line in left.iter().chain(right) {
            lines.push(line.clone());
        }
    }
}

/// A muted label followed by its value.
pub(super) fn kv(label: &str, value: &str) -> String {
    format!("{} {value}", render::paint(render::palette::muted(), label))
}

/// The month-abbrev header: a label sits over the column where each new month begins, like the
/// GitHub graph.
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
