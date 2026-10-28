use super::fmt::*;
use super::panel::PanelGlyphs;
use super::panel::{Activity, display_width, ellipsize, header_lines, pad_left, pad_to};
use super::*;
use rimz::harness::assist_log::AssistRecord;
use rimz::theme::fmt::{active_duration, group_thousands};

#[derive(Default)]
pub(super) struct TabAssists {
    pub(super) tabs: [AssistStats; 4],
    pub(super) earliest: Option<Timestamp>,
    pub(super) starts: [Option<Timestamp>; 4],
}

impl TabAssists {
    pub(super) fn from_records(records: Vec<AssistRecord>, now: Timestamp, timezone: &str) -> Self {
        let starts = Window::TABS.map(|window| window.assist_since(now, timezone));
        Self {
            earliest: records.first().map(|record| record.at),
            starts,
            tabs: std::array::from_fn(|index| {
                let start = starts[index];
                let record_start =
                    records.partition_point(|record| start.is_some_and(|start| record.at < start));
                AssistStats::from_records(
                    Window::TABS[index].label(),
                    records[record_start..].to_vec(),
                )
            }),
        }
    }
}

pub(super) struct Dashboard<'a> {
    pub(super) stats: &'a Stats,
    pub(super) assists: &'a TabAssists,
    pub(super) today: i64,
    pub(super) dollars: bool,
    pub(super) glyphs: &'a PanelGlyphs,
    pub(super) hold: bool,
    pub(super) color: bool,
}

pub(super) struct Frame {
    pub(super) text: String,
    #[cfg(test)]
    pub(super) step: usize,
}

impl Dashboard<'_> {
    pub(super) fn frame(&self, active: Window, cols: usize, rows: Option<usize>) -> Frame {
        let width = cols.min(113);
        let (step, tallest) = rows.map_or((0, 0), |rows| {
            (0..=8)
                .map(|step| {
                    let tallest = Window::TABS
                        .into_iter()
                        .map(|tab| self.lines(tab, width, step).len())
                        .max()
                        .unwrap_or(0);
                    (step, tallest)
                })
                .find(|&(_, tallest)| tallest <= rows)
                .unwrap_or((8, rows))
        });
        let margin = " ".repeat(cols.saturating_sub(width) / 2);
        let mut lines = self.lines(active, width, step);
        if let Some(rows) = rows {
            if lines.len() > rows {
                let hint = lines.pop();
                lines.truncate(rows.saturating_sub(1));
                while lines.last().is_some_and(String::is_empty) {
                    lines.pop();
                }
                if rows > 0 {
                    lines.extend(hint);
                }
            }
            lines.splice(
                0..0,
                std::iter::repeat_n(String::new(), rows.saturating_sub(tallest) / 2),
            );
        }
        let text = lines
            .into_iter()
            .map(|line| {
                if line.is_empty() {
                    return line;
                }
                format!("{margin}{}", clip(&line, width))
            })
            .collect::<Vec<_>>()
            .join("\n");
        Frame {
            text,
            #[cfg(test)]
            step,
        }
    }

    fn lines(&self, active: Window, width: usize, step: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let hint = render::paint(
            render::palette::muted(),
            if self.hold {
                "←/→ range · t/w/m/y jump · r reload"
            } else {
                "←/→ range · t/w/m/y jump · r reload · q quit"
            },
        );
        if step == 0 {
            lines.extend(header_lines(width).into_iter().skip(1));
        }
        let role_rows = [RoleView::Agents, RoleView::Subagents, RoleView::Teams]
            .map(|view| view.rows(self.stats, active));
        if self.stats.by_day.is_empty()
            && role_rows
                .iter()
                .flat_map(|rows| rows.values())
                .all(|row| !row.has_usage())
        {
            for copy in [
                "No agent sessions recorded on this machine yet.",
                "Spend appears here after your first agent session; rimz stats reads it from each agent's own session files.",
            ] {
                lines.extend(render::wrap_words(copy, width));
            }
            section(&mut lines, self.assist_lines(active, width, step >= 2));
            section(&mut lines, vec![hint]);
            return lines;
        }
        if step < 6 {
            section(&mut lines, self.heatmap(width));
        }
        section(
            &mut lines,
            vec![
                Window::TABS
                    .into_iter()
                    .map(|tab| {
                        let label = format!(
                            "{} {}",
                            tab.label(),
                            fmt_usd(tab.select(&self.stats.total).usd)
                        );
                        if tab == active {
                            render::paint(render::palette::accent().bold(), &format!("[ {label} ]"))
                        } else {
                            render::paint(render::palette::body(), &label)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(&render::paint(render::palette::muted(), "  ·  ")),
            ],
        );
        let chart_height = match step {
            0..=6 => 7,
            7 => 4,
            _ => 2,
        };
        section(&mut lines, self.chart_lines(active, width, chart_height));
        let limit = if step >= 4 { 2 } else { 4 };
        let models = fold(
            self.stats
                .by_model
                .iter()
                .map(|(id, model)| {
                    (
                        id.clone(),
                        RoleWindow {
                            spend: active.select(&model.tally),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            limit,
            true,
        );
        section(&mut lines, self.model_lines(&models, active, width));
        let providers = fold(
            self.stats
                .by_provider
                .iter()
                .map(|(kind, tally)| {
                    (
                        kind.clone(),
                        RoleWindow {
                            spend: active.select(tally),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            limit,
            false,
        );
        let half = if width >= 112 { 54 } else { width };
        let totals = role_rows
            .each_ref()
            .map(|rows| rows.values().map(|row| row.spend.usd).sum::<f64>());
        section(
            &mut lines,
            pair(
                self.provider_lines(&providers, active, half),
                self.where_lines(totals, active, half),
                width,
            ),
        );
        let [agents, subagents, teams] =
            role_rows.map(|rows| fold(rows.into_iter().collect(), limit, true));
        if step < 3 {
            section(
                &mut lines,
                pair(
                    self.profile_lines("Agents", &agents, half),
                    self.profile_lines("Subagents", &subagents, half),
                    width,
                ),
            );
        }
        if step < 5 && totals[2] > 0.0 {
            section(&mut lines, self.team_lines(&teams, width));
        }
        section(&mut lines, self.assist_lines(active, width, step >= 2));
        section(&mut lines, vec![hint]);
        lines
    }

    fn model_rank(&self, id: &str) -> Option<(&str, usize)> {
        let model = self.stats.by_model.get(id)?;
        let rank = self
            .stats
            .by_model
            .iter()
            .filter(|(other_id, other)| {
                other.kind == model.kind
                    && other
                        .tally
                        .year
                        .usd
                        .total_cmp(&model.tally.year.usd)
                        .then_with(|| other.tally.year.tokens.cmp(&model.tally.year.tokens))
                        .then_with(|| id.cmp(other_id.as_str()))
                        .is_gt()
            })
            .count();
        Some((&model.kind, rank))
    }

    fn model_style(&self, id: &str) -> anstyle::Style {
        self.model_rank(id)
            .map_or_else(render::palette::muted, |(kind, rank)| {
                render::palette::model_shade(kind, rank)
            })
    }

    fn model_lines(&self, rows: &[Row], active: Window, width: usize) -> Vec<String> {
        if rows.is_empty() {
            return Vec::new();
        }
        let show_rate = rows
            .iter()
            .any(|row| row.key.is_some() && row.value.spend.tokens_per_sec().is_some());
        let columns = SpendColumns::new(
            rows,
            width
                .saturating_sub(if show_rate { 63 } else { 51 })
                .min(48),
        );
        let mut lines = vec![columns_header(
            "Models",
            columns,
            ["HIT", if show_rate { "TOK/S" } else { "" }, "TOKENS"],
        )];
        let total = active.select(&self.stats.total).usd;
        for row in rows {
            let style = row
                .key
                .as_deref()
                .map_or_else(render::palette::muted, |id| self.model_style(id));
            let name = row
                .key
                .as_deref()
                .map_or_else(|| "Other".into(), model_name);
            let rate = if row.key.is_none() {
                "–".into()
            } else {
                row.value
                    .spend
                    .tokens_per_sec()
                    .map_or_else(|| "–".into(), |rate| rate.to_string())
            };
            lines.push(format!(
                "{}{}{}{}",
                self.spend_row(&name, row.value.spend.usd, total, columns, style, "█"),
                pad_left(&hit(&row.value.spend), 13),
                if show_rate {
                    pad_left(&rate, 12)
                } else {
                    String::new()
                },
                pad_left(&fmt_tokens(row.value.spend.display_tokens()), 8)
            ));
        }
        lines
    }

    fn profile_lines(&self, label: &str, rows: &[Row], width: usize) -> Vec<String> {
        if rows.is_empty() {
            return Vec::new();
        }
        let total = rows.iter().map(|row| row.value.spend.usd).sum();
        let columns = SpendColumns::new(rows, width.saturating_sub(30).min(24));
        let mut lines = vec![total_header(label, total, columns.dollars)];
        lines.extend(rows.iter().map(|row| {
            self.spend_row(
                row.name(),
                row.value.spend.usd,
                total,
                columns,
                render::palette::muted(),
                "●",
            )
        }));
        lines
    }

    fn team_lines(&self, rows: &[Row], width: usize) -> Vec<String> {
        let total = rows.iter().map(|row| row.value.spend.usd).sum();
        let columns = SpendColumns::new(rows, width.saturating_sub(63).min(48));
        let mut lines = vec![columns_header(
            &total_header("Teams", total, columns.dollars),
            columns,
            ["AGENT-TIME", "MESSAGES", "TOKENS"],
        )];
        for row in rows {
            lines.push(format!(
                "{}{}{}{}",
                self.spend_row(
                    row.name(),
                    row.value.spend.usd,
                    total,
                    columns,
                    render::palette::muted(),
                    "●"
                ),
                pad_left(&active_duration(row.value.effort.active_ms / 1_000), 13),
                pad_left(&panel::messages_label(&row.value), 12),
                pad_left(&fmt_tokens(row.value.spend.display_tokens()), 8)
            ));
        }
        lines
    }

    fn spend_row(
        &self,
        name: &str,
        usd: f64,
        total: f64,
        columns: SpendColumns,
        style: anstyle::Style,
        marker: &str,
    ) -> String {
        let SpendColumns { dollars, bar } = columns;
        let share = ratio(usd, total);
        let filled = (share * bar as f64).round().clamp(0.0, bar as f64) as usize;
        format!(
            "{} {}{}  {}{} {}",
            render::paint(style, marker),
            pad_to(&render::paint(style, &ellipsize(name, 14)), 14),
            pad_left(
                &render::paint(render::palette::money(), &fmt_usd(usd)),
                dollars
            ),
            render::paint(style, &self.glyphs.bar_filled.repeat(filled)),
            render::paint(
                render::palette::faint(),
                &self.glyphs.bar_track.repeat(bar - filled)
            ),
            pad_left(&percent(share), 4)
        )
    }

    fn provider_lines(&self, rows: &[Row], active: Window, width: usize) -> Vec<String> {
        let total = active.select(&self.stats.total).usd;
        let mut lines = vec![render::paint(render::palette::header(), "Providers")];
        let parts: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.value.spend.usd,
                    "█",
                    row.key
                        .as_deref()
                        .map_or_else(render::palette::muted, render::palette::identity),
                )
            })
            .collect();
        lines.push(stacked_bar(&parts, width.min(54)));
        for row in rows {
            let kind = row.key.as_deref();
            let style = kind.map_or_else(render::palette::muted, render::palette::identity);
            let name = kind
                .map(agent_display_name)
                .unwrap_or_else(|| "Other".into());
            let share = ratio(row.value.spend.usd, total);
            let sessions = row.value.spend.sessions;
            let noun = if sessions == 1 { "session" } else { "sessions" };
            let share_label = if share < 0.01 {
                "<1%".to_owned()
            } else {
                percent(share)
            };
            let cache_hit = if share >= 0.01 {
                format!("{} hit", hit(&row.value.spend))
            } else {
                String::new()
            };
            lines.push(format!(
                "{} {}{}{}{}{} {noun}",
                render::paint(style, "█"),
                pad_to(&render::paint(style, &ellipsize(&name, 8)), 8),
                pad_left(&fmt_usd(row.value.spend.usd), 10),
                pad_left(&share_label, 6),
                pad_left(&cache_hit, 10),
                pad_left(&group_thousands(u64::from(sessions)), 8)
            ));
        }
        lines
    }

    fn where_lines(&self, totals: [f64; 3], active: Window, width: usize) -> Vec<String> {
        let fleet = active.select(&self.stats.total).usd;
        let [agents, subagents, teams] = totals;
        let values = [
            teams,
            subagents,
            agents,
            (fleet - teams - subagents - agents).max(0.0),
        ];
        let labels = ["Teams", "Subagents", "Agents", "No role"];
        let glyphs = ["█", "▓", "▒", "░"];
        let styles = [
            render::palette::accent(),
            render::palette::cool(),
            render::palette::muted(),
            render::palette::faint(),
        ];
        let mut title = "Where it went".to_owned();
        if let Some(since) = self.stats.recorded_since()
            && active
                .assist_since(self.stats.as_of().timestamp(), &self.stats.timezone)
                .is_some_and(|start| start < since.timestamp())
        {
            title.push_str(&format!(
                " · roles recorded since {}",
                since.strftime("%b %d")
            ));
        }
        let mut lines = vec![render::paint(render::palette::header(), &title)];
        let parts: Vec<_> = (0..4).map(|i| (values[i], glyphs[i], styles[i])).collect();
        lines.push(stacked_bar(&parts, width.min(54)));
        let total = values.iter().sum();
        let dollars = values
            .iter()
            .map(|&usd| display_width(&fmt_usd(usd)))
            .max()
            .unwrap_or(0)
            .max(7);
        let entries = (0..4)
            .map(|i| {
                format!(
                    "{} {}{}{}",
                    render::paint(styles[i], glyphs[i]),
                    pad_to(labels[i], 9),
                    pad_left(&fmt_usd(values[i]), dollars),
                    pad_left(&percent(ratio(values[i], total)), 5)
                )
            })
            .collect::<Vec<_>>();
        for pair in entries.chunks(2) {
            lines.push(format!("{}  {}", pad_to(&pair[0], 25), pair[1]));
        }
        lines
    }

    fn heatmap(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        panel::heatmap_lines(
            &mut lines,
            self.stats,
            self.today,
            weeks_for_terminal(width),
            self.dollars,
        );
        lines.retain(|line| !line.is_empty());
        if let Some(header) = lines.first_mut() {
            *header = header.trim_start().to_owned();
        }
        if let [_, grid @ .., _] = lines.as_mut_slice() {
            for line in grid {
                line.insert(0, ' ');
            }
        }
        let grid = panel::Grid::build(
            &self.stats.by_day,
            self.today,
            weeks_for_terminal(width),
            self.dollars,
        );
        let start = grid.col_monday(0);
        let today_year = Timestamp::from_second(self.today * DAY_SECS)
            .ok()
            .map(|today| jiff::tz::TimeZone::UTC.to_datetime(today).year());
        let date_label = |day| {
            if let Ok(timestamp) = Timestamp::from_second(day * DAY_SECS) {
                let date = jiff::tz::TimeZone::UTC.to_datetime(timestamp);
                if today_year.is_some_and(|year| date.year() < year) {
                    return format!("{} {}", fmt_day(day), date.strftime("%Y"));
                }
            }
            fmt_day(day)
        };
        let active_days = self
            .stats
            .by_day
            .range(start..=self.today)
            .filter(|(_, day)| day.tokens > 0)
            .collect::<Vec<_>>();
        let streak = Activity::of(&self.stats.by_day, self.today, Window::Year).current_streak;
        let busiest = active_days
            .iter()
            .max_by_key(|(_, day)| day.tokens)
            .map(|(day, _)| date_label(**day))
            .unwrap_or_else(|| "–".into());
        let summary = format!(
            "{} active days since {} · streak {} · busiest {busiest}",
            active_days.len(),
            date_label(start),
            panel::plural_days(streak)
        );
        if 40 + display_width(&summary) <= width {
            if let Some(key) = lines.last_mut() {
                *key = format!(
                    "{}{}",
                    pad_to(key, 40),
                    render::paint(render::palette::muted(), &summary)
                );
            }
        } else {
            lines.extend(
                render::wrap_words(&summary, width)
                    .into_iter()
                    .map(|line| render::paint(render::palette::muted(), &line)),
            );
        }
        lines
    }

    fn assist_lines(&self, active: Window, width: usize, compact: bool) -> Vec<String> {
        let index = Window::TABS
            .iter()
            .position(|tab| *tab == active)
            .unwrap_or(0);
        let entries = assists::category_rows(
            &self.assists.tabs[index].rollup,
            assists::CategoryMode::Dashboard,
        );
        if entries.is_empty() {
            return Vec::new();
        }
        if compact {
            return vec![format!(
                "{}  {} kind{} · rimz stats --assists for the timeline",
                render::paint(render::palette::header(), "Assists"),
                entries.len(),
                if entries.len() == 1 { "" } else { "s" }
            )];
        }
        let mut title = "Assists".to_owned();
        if let Some(since) = self.assists.earliest
            && self.assists.starts[index].is_some_and(|start| since > start)
        {
            let zone =
                jiff::tz::TimeZone::get(&self.stats.timezone).unwrap_or(jiff::tz::TimeZone::UTC);
            title.push_str(&format!(
                " (records since {})",
                since.to_zoned(zone).strftime("%b %d")
            ));
        }
        let mut lines = Vec::new();
        assists::panel_lines(&mut lines, &self.assists.tabs[index], width, &title);
        lines
    }

    fn chart_title(&self, active: Window) -> String {
        let selected = active.select(&self.stats.total).usd;
        if active == Window::Today {
            return format!(
                "today {} so far · ~{} by midnight · yesterday {}",
                fmt_usd(selected),
                fmt_usd(self.forecast()),
                fmt_usd(
                    self.stats
                        .by_day
                        .get(&(self.stats.chart.today - 1))
                        .map_or(0.0, |day| day.usd)
                )
            );
        }
        let average = panel::per_active_day(&self.stats.by_day, self.today, active)
            .map(fmt_usd)
            .unwrap_or_default();
        if active == Window::Year {
            return format!("{average} per active day");
        }
        let trend = panel::spend_trend(&self.stats.by_day, self.today, active)
            .map(|trend| {
                format!(
                    "{} vs prior {} days · ",
                    panel::trend_label(trend),
                    active.span_days()
                )
            })
            .unwrap_or_default();
        format!("{trend}{average} a day")
    }

    fn forecast(&self) -> f64 {
        let today = self.stats.chart.today;
        let hour = i64::from(self.stats.chart.hour);
        let remaining = (1..=7)
            .flat_map(|ago| {
                self.stats
                    .chart
                    .hours
                    .range(((today - ago) * 24 + hour + 1)..((today - ago + 1) * 24))
                    .map(|(_, cells)| cells.values().sum::<f64>())
            })
            .sum::<f64>()
            / 7.0;
        let so_far = self.stats.total.day.usd;
        (so_far + remaining).max(so_far)
    }

    fn chart_lines(&self, active: Window, width: usize, height: usize) -> Vec<String> {
        let weekly = active == Window::Year;
        let hourly = active == Window::Today;
        let (count, span, data) = match active {
            Window::Today => (24, 1, &self.stats.chart.hours),
            Window::Week => (7, 1, &self.stats.chart.days),
            Window::Month => (30, 1, &self.stats.chart.days),
            Window::Year => (
                width.saturating_sub(10).clamp(1, 52),
                7,
                &self.stats.chart.weeks,
            ),
        };
        let last_day = if hourly {
            self.stats.chart.today * 24 + 23
        } else {
            self.stats.chart.today
        };
        let dates = (0..count)
            .map(|index| last_day - (count - 1 - index) as i64 * span)
            .collect::<Vec<_>>();
        let mut sums = BTreeMap::<&str, f64>::new();
        for cells in dates.iter().filter_map(|day| data.get(day)) {
            for (id, usd) in cells {
                *sums.entry(id).or_default() += usd;
            }
        }
        let mut top: Vec<_> = sums.into_iter().collect();
        top.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let has_other = top.len() > 4;
        top.truncate(4);
        let styles: BTreeMap<_, _> = top
            .iter()
            .map(|(id, _)| (*id, self.model_style(id)))
            .collect();
        let totals = dates
            .iter()
            .map(|day| {
                data.get(day)
                    .map_or(0.0, |cells| cells.values().sum::<f64>())
            })
            .collect::<Vec<_>>();
        let yesterday: Vec<_> = dates
            .iter()
            .map(|hour| {
                if hourly && self.color {
                    data.get(&(hour - 24))
                        .map_or(0.0, |cells| cells.values().sum::<f64>())
                } else {
                    0.0
                }
            })
            .collect();
        let extension = if hourly || !self.color {
            0.0
        } else {
            (self.forecast() - self.stats.total.day.usd).max(0.0)
        };
        let max = totals
            .iter()
            .copied()
            .chain(yesterday.iter().copied())
            .chain(std::iter::once(
                totals.last().copied().unwrap_or(0.0) + extension,
            ))
            .fold(0.0, f64::max);
        let typical = if weekly || hourly {
            0.0
        } else {
            self.stats
                .by_day
                .range((self.today - 30)..self.today)
                .map(|(_, day)| day.usd)
                .sum::<f64>()
                / 30.0
        };
        let ceiling = round_ceiling(max.max(typical));
        // The y labels' convention: plot row k from the bottom (k counted from 1) stands for
        // k/height of the ceiling, so the line takes the row nearest the mean, never below row 1.
        let typical_row = (typical > 0.0)
            .then(|| ((typical / ceiling * height as f64).round() as usize).clamp(1, height) - 1);
        // Keep the typical label while daily bars can remain at least two columns wide.
        let figure = typical_row.map(|_| fmt_usd(typical));
        let reserve = figure.as_deref().map_or(0, |figure| {
            let short = 2 + display_width(figure);
            let worded = short + "typical ".len();
            if (width.saturating_sub(10 + worded) + 1) / count >= 3 {
                worded
            } else {
                short
            }
        });
        let stride = ((width.saturating_sub(10 + reserve) + 1) / count)
            .clamp(1, if weekly { 2 } else { width.max(1) });
        let bar_width = if weekly {
            1
        } else {
            stride.saturating_sub(1).max(1)
        };
        let gap = stride - bar_width;
        let plot_width = count * stride - gap;
        let caption = figure.map(|figure| {
            let worded = format!("typical {figure}");
            if 10 + plot_width + 2 + display_width(&worded) <= width {
                worded
            } else {
                figure
            }
        });
        let left = match active {
            Window::Today => "Spend per hour · today".into(),
            Window::Week => "Spend per day · last 7 days".into(),
            Window::Month => "Spend per day · last 30 days".into(),
            Window::Year => format!("Spend per week · last {count} weeks"),
        };
        let right = self.chart_title(active);
        let mut lines = vec![format!(
            "{}{}",
            pad_to(
                &render::paint(render::palette::header(), &left),
                width
                    .saturating_sub(display_width(&right))
                    .max(display_width(&left) + 2)
            ),
            render::paint(render::palette::muted(), &right)
        )];
        let bars = dates
            .iter()
            .zip(&totals)
            .zip(&yesterday)
            .map(|((day, total), yesterday)| {
                let extra = if *day == last_day { extension } else { 0.0 };
                let projected = *total + extra;
                if projected <= 0.0 {
                    return Vec::new();
                }
                let cells = data.get(day);
                let segments = top
                    .iter()
                    .map(|(id, _)| {
                        (
                            Some(*id),
                            cells
                                .and_then(|cells| cells.get(*id))
                                .copied()
                                .unwrap_or(0.0),
                        )
                    })
                    .chain(std::iter::once((
                        None,
                        (*total
                            - top
                                .iter()
                                .map(|(id, _)| {
                                    cells
                                        .and_then(|cells| cells.get(*id))
                                        .copied()
                                        .unwrap_or(0.0)
                                })
                                .sum::<f64>())
                        .max(0.0),
                    )))
                    .collect::<Vec<_>>();
                let units = eighths(projected, ceiling, height);
                let spent_units = eighths(*total, ceiling, height);
                let top_id = if spent_units == 0 {
                    None
                } else {
                    let top_fraction = (spent_units - 1) % 8 + 1;
                    segment_at(
                        &segments,
                        (spent_units as f64 - top_fraction as f64 / 2.0) / spent_units as f64
                            * total,
                    )
                };
                let forecast_style =
                    render::palette::model_forecast(top_id.and_then(|id| self.model_rank(id)));
                let overlay_units = eighths(*yesterday, ceiling, height);
                (0..units.div_ceil(8))
                    .map(|cell| {
                        let projected = (units - cell * 8).min(8);
                        let spent = spent_units.saturating_sub(cell * 8).min(8);
                        if spent == 0 {
                            return (BLOCKS[projected - 1], forecast_style);
                        }
                        let at =
                            (cell as f64 * 8.0 + spent as f64 / 2.0) / spent_units as f64 * total;
                        let style = segment_at(&segments, at)
                            .map_or_else(render::palette::muted, |id| styles[id]);
                        let background = if projected > spent {
                            forecast_style.get_fg_color()
                        } else if overlay_units.saturating_sub(cell * 8).min(8) > spent {
                            render::palette::faint().get_fg_color()
                        } else {
                            None
                        };
                        (BLOCKS[spent - 1], style.bg_color(background))
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for row in (0..height).rev() {
            let label = if row == height - 1 {
                fmt_usd(ceiling)
            } else if row == (height - 1) / 2 {
                fmt_usd(ceiling / 2.0)
            } else {
                String::new()
            };
            let mut line = render::paint(render::palette::muted(), &format!("{label:>7} ┤ "));
            let baseline = if typical_row == Some(row) { "┄" } else { " " };
            for (index, bar) in bars.iter().enumerate() {
                if let Some(&(glyph, style)) = bar.get(row) {
                    line.push_str(&render::paint(style, &glyph.to_string().repeat(bar_width)));
                } else {
                    let units = eighths(yesterday[index], ceiling, height)
                        .saturating_sub(row * 8)
                        .min(8);
                    let glyph = if units > 0 {
                        BLOCKS[units - 1].to_string()
                    } else {
                        baseline.into()
                    };
                    line.push_str(&render::paint(
                        render::palette::faint(),
                        &glyph.repeat(bar_width),
                    ));
                }
                if index + 1 < count {
                    line.push_str(&render::paint(
                        render::palette::faint(),
                        &baseline.repeat(gap),
                    ));
                }
            }
            if let Some(caption) = caption.as_deref().filter(|_| typical_row == Some(row)) {
                line.push_str(&render::paint(render::palette::faint(), "┄"));
                line.push(' ');
                line.push_str(&render::paint(render::palette::muted(), caption));
            }
            lines.push(line.trim_end().to_owned());
        }
        let final_label = if hourly {
            "23"
        } else if weekly {
            "now"
        } else {
            "today"
        };
        let final_start = if bar_width >= final_label.len() {
            (count - 1) * stride
        } else {
            plot_width.saturating_sub(final_label.len())
        };
        let mut axis = String::new();
        let every = match active {
            Window::Today => 6,
            Window::Week => 1,
            Window::Month => 7,
            Window::Year => 13,
        };
        for index in (0..count - 1).step_by(every) {
            let label = if hourly {
                format!("{:02}", dates[index].rem_euclid(24))
            } else {
                fmt_day(dates[index])
            };
            let start = index * stride;
            if (!axis.is_empty() && start < display_width(&axis) + 1)
                || start + label.len() + 1 >= final_start
            {
                continue;
            }
            axis = pad_to(&axis, start);
            axis.push_str(&render::paint(render::palette::muted(), &label));
        }
        lines.push(format!(
            "{}{}{}",
            render::paint(render::palette::muted(), "     $0 └─"),
            pad_to(&axis, final_start),
            render::paint(render::palette::accent(), final_label)
        ));
        let mut legend = top
            .iter()
            .map(|(id, _)| format!("{} {}", render::paint(styles[id], "█"), model_name(id)))
            .collect::<Vec<_>>();
        if has_other {
            legend.push(render::paint(render::palette::muted(), "█ Other"));
        }
        lines.push(format!("          {}", legend.join("    ")));
        lines
    }
}

struct Row {
    key: Option<String>,
    value: RoleWindow,
}

#[derive(Clone, Copy)]
struct SpendColumns {
    dollars: usize,
    bar: usize,
}

impl SpendColumns {
    fn new(rows: &[Row], bar: usize) -> Self {
        let dollars = rows
            .iter()
            .map(|row| display_width(&fmt_usd(row.value.spend.usd)))
            .max()
            .unwrap_or(0)
            .max(7);
        Self {
            dollars,
            bar: bar.saturating_sub(dollars - 7),
        }
    }
}

impl Row {
    fn name(&self) -> &str {
        self.key.as_deref().unwrap_or("Other")
    }
}

fn fold(mut rows: Vec<(String, RoleWindow)>, limit: usize, threshold: bool) -> Vec<Row> {
    rows.retain(|(_, row)| row.has_usage());
    rows.sort_by(|a, b| {
        b.1.spend
            .usd
            .total_cmp(&a.1.spend.usd)
            .then_with(|| b.1.spend.tokens.cmp(&a.1.spend.tokens))
            .then_with(|| a.0.cmp(&b.0))
    });
    let total = rows.iter().map(|(_, row)| row.spend.usd).sum();
    let (mut named, mut tail) = (Vec::new(), Vec::new());
    for (key, value) in rows {
        let keep = named.len() < limit && (!threshold || ratio(value.spend.usd, total) >= 0.01);
        if keep {
            named.push(Row {
                key: Some(key),
                value,
            });
        } else {
            tail.push(Row {
                key: Some(key),
                value,
            });
        }
    }
    if tail.len() == 1 {
        named.extend(tail);
    } else if !tail.is_empty() {
        let mut value = RoleWindow::default();
        for row in tail {
            value.merge(&row.value);
        }
        named.push(Row { key: None, value });
    }
    named
}

fn model_name(id: &str) -> String {
    if id.is_empty() {
        "Unknown model".into()
    } else {
        rimz::agents::model_display::display_model(id)
            .chars()
            .filter(|ch| !ch.is_control())
            .collect()
    }
}

fn ratio(value: f64, total: f64) -> f64 {
    if total > 0.0 { value / total } else { 0.0 }
}

fn percent(share: f64) -> String {
    if share <= 0.0 {
        "0%".into()
    } else if share < 0.01 {
        "<1%".into()
    } else {
        format!("{:.0}%", share * 100.0)
    }
}

fn hit(spend: &SpendWindow) -> String {
    use rimz::agents::CacheHealth;
    let Some(hit) = spend.cache_hit_percent() else {
        return "–".into();
    };
    let style = match CacheHealth::classify(hit) {
        CacheHealth::Good => render::palette::good(),
        CacheHealth::Caution => render::palette::warn(),
        CacheHealth::Alarm => render::palette::alarm(),
    };
    render::paint(style, &format!("{hit}%"))
}

fn total_header(label: &str, usd: f64, dollars: usize) -> String {
    format!(
        "{}{}",
        pad_to(&render::paint(render::palette::header(), label), 15),
        pad_left(&fmt_usd(usd), dollars + 1)
    )
}

fn columns_header(label: &str, spend: SpendColumns, columns: [&str; 3]) -> String {
    format!(
        "{}{}{}{}",
        pad_to(
            &render::paint(render::palette::header(), label),
            18 + spend.dollars + spend.bar + 5
        ),
        pad_left(columns[0], 13),
        if columns[1].is_empty() {
            String::new()
        } else {
            pad_left(columns[1], 12)
        },
        pad_left(columns[2], 8)
    )
}

fn stacked_bar(parts: &[(f64, &str, anstyle::Style)], width: usize) -> String {
    let total = parts.iter().map(|part| part.0).sum();
    let mut sum = 0.0;
    let mut used = 0;
    let mut bar = String::new();
    for &(usd, glyph, style) in parts {
        sum += usd;
        let end = (ratio(sum, total) * width as f64)
            .round()
            .clamp(used as f64, width as f64) as usize;
        bar.push_str(&render::paint(style, &glyph.repeat(end - used)));
        used = end;
    }
    bar
}

fn section(lines: &mut Vec<String>, section: Vec<String>) {
    if section.is_empty() {
        return;
    }
    if !lines.is_empty() && lines.last().is_some_and(|line| !line.is_empty()) {
        lines.push(String::new());
    }
    lines.extend(section);
}

fn pair(left: Vec<String>, right: Vec<String>, width: usize) -> Vec<String> {
    if width < 112 {
        let mut lines = left;
        section(&mut lines, right);
        return lines;
    }
    (0..left.len().max(right.len()))
        .map(|index| {
            let left = left.get(index).map_or("", String::as_str);
            let right = right.get(index).map_or("", String::as_str);
            let left = clip(left, 54);
            let right = clip(right, width.saturating_sub(58));
            format!("{}{right}", pad_to(&left, 58))
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn round_ceiling(max: f64) -> f64 {
    if max <= 0.0 {
        return 1.0;
    }
    let magnitude = 10_f64.powf(max.log10().floor());
    [1.0, 1.2, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0]
        .into_iter()
        .map(|step| step * magnitude)
        .find(|ceiling| *ceiling >= max)
        .unwrap_or(magnitude * 10.0)
}

const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

fn segment_at<'a>(segments: &[(Option<&'a str>, f64)], at: f64) -> Option<&'a str> {
    let mut sum = 0.0;
    segments
        .iter()
        .find_map(|(id, usd)| {
            sum += usd;
            (sum > at).then_some(*id)
        })
        .flatten()
}

fn eighths(usd: f64, ceiling: f64, height: usize) -> usize {
    if usd <= 0.0 {
        return 0;
    }
    (usd / ceiling * height as f64 * 8.0)
        .round()
        .clamp(1.0, (height * 8) as f64) as usize
}

fn clip(line: &str, width: usize) -> String {
    let mut output = String::new();
    let mut chars = line.chars();
    let mut used = 0;
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            output.push(ch);
            for ch in chars.by_ref() {
                output.push(ch);
                if ch == 'm' {
                    break;
                }
            }
            continue;
        }
        if ch.is_control() {
            continue;
        }
        let cells = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cells > width {
            output.push_str("\x1b[0m");
            break;
        }
        output.push(ch);
        used += cells;
    }
    output.trim_end().to_owned()
}
