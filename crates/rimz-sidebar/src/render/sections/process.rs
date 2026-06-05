//! Bare process rows: the shell/build line, its right-pinned resource stats,
//! the full-command detail line, and the resolver's composed row.

use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};
use rimz::SidebarRow;
use rimz::feed::AgentStatus;

use crate::render::fmt::{age_short, clip, fmt_cpu, fmt_io, fmt_rss};
use crate::render::labels::{status_glyph, status_style, working_glyph};
use crate::render::theme::{ORANGE, Theme};

use super::{Tier, pin_right, trim_spans_to_width};

pub(super) fn process_row_line(
    theme: &Theme,
    row: &SidebarRow,
    width: usize,
    animation_phase: u64,
) -> Line<'static> {
    // The row speaks the agent cards' vocabulary one step down: an active
    // pane (a build, a test, a script) gets the running braille spinner in
    // the same work clay a running agent wears, an idle shell or a TUI rests
    // on the quiet-green hollow `○` of an idle agent — the lead DIM-weighted,
    // the name at the soft tier. That slight step — not a seam line — is what
    // sets the group's command tail apart from the agent cards above it, and
    // under `NO_COLOR` it survives as the soft tier's DIM weight.
    let (lead, lead_style) = if row.process_active {
        (
            working_glyph(animation_phase),
            theme.style(ORANGE, Modifier::DIM),
        )
    } else {
        (
            status_glyph(AgentStatus::Idle),
            status_style(theme, AgentStatus::Idle).add_modifier(Modifier::DIM),
        )
    };
    let left = vec![
        Span::styled(lead, lead_style),
        Span::raw(" "),
        Span::styled(row.name.clone(), theme.soft()),
    ];
    // At L2 width, resource stats pin right: `C  11%  M 1.1G  ⇅   3M/s`.
    // The whole cluster drops at L0/L1, or when no metric has reported yet.
    if Tier::for_width(width) == Tier::L2 {
        let right = proc_stats_spans(theme, row);
        if !right.is_empty() {
            return pin_right(left, right, width);
        }
    }
    Line::from(trim_spans_to_width(left, width))
}

/// The figure slot widths of the fixed stats grid: each figure right-aligns
/// into the slot its formatter guarantees (`100%` · `512M` / `1.1G` ·
/// `450k/s`), so a changing magnitude never walks the cluster sideways.
const CPU_SLOT: usize = 4;
const RSS_SLOT: usize = 4;
const IO_SLOT: usize = 6;

/// Build the right-pinned resource stats for a process row as one fixed grid —
/// `C  34%  M 512M  ⇅   8M/s`. Each metric owns a fixed slot (marker, space,
/// right-aligned figure); once any metric reports, all three markers paint —
/// each in its own tone (`C` sky, `M` sage, `⇅` violet, DIM-weighted like the
/// clay lead so the row stays a step below the agent cards) — and a metric not
/// yet sampled (rates on the first tick) holds a dim `--` in its figure slot,
/// so the grid reads whole from the first reading on and the columns never
/// move. Figures and seams stay dim — the stats are secondary chrome, and the
/// lead glyph carries the row's liveness. Empty when no metric has reported at
/// all (non-Linux).
pub(in crate::render) fn proc_stats_spans(theme: &Theme, row: &SidebarRow) -> Vec<Span<'static>> {
    let slots: [(&str, Color, usize, Option<String>); 3] = [
        ("C", Color::Blue, CPU_SLOT, row.cpu_pct.map(fmt_cpu)),
        ("M", Color::Green, RSS_SLOT, row.rss_kb.map(fmt_rss)),
        ("⇅", Color::Magenta, IO_SLOT, row.io_bps.map(fmt_io)),
    ];
    if slots.iter().all(|(_, _, _, figure)| figure.is_none()) {
        return Vec::new();
    }
    let mut spans = Vec::with_capacity(3 * slots.len());
    for (i, (marker, tone, width, figure)) in slots.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", theme.dim()));
        }
        spans.push(Span::styled(
            format!("{marker} "),
            theme.style(tone, Modifier::DIM),
        ));
        let figure = figure.unwrap_or_else(|| "--".to_owned());
        spans.push(Span::styled(format!("{figure:>width$}"), theme.dim()));
    }
    spans
}

/// Line 2 for an *active* process row: the full foreground command in the
/// row's [`Theme::soft`] tone, indented under the shell anchor, so a build or
/// a `sudo` install reads in full while the primary line keeps the stable
/// shell label. `None` when the producer left no detail (an idle pane, or a
/// command already shown whole on line 1).
pub(super) fn process_detail_line(
    theme: &Theme,
    row: &SidebarRow,
    width: usize,
) -> Option<Line<'static>> {
    let detail = row.command_detail.as_deref()?;
    let left = vec![
        Span::raw("  "),
        Span::styled(detail.to_owned(), theme.soft()),
    ];
    Some(Line::from(trim_spans_to_width(left, width)))
}

pub(super) fn composed_row(
    theme: &Theme,
    lead: Span<'static>,
    name: &str,
    task: &str,
    last_activity: jiff::Timestamp,
    width: usize,
) -> Line<'static> {
    let age = age_short(last_activity);
    let lead_width = 2;
    let name_width = 7;
    let age_width = age.chars().count();
    let fixed = lead_width + name_width + 2 + age_width;
    let task_width = width.saturating_sub(fixed).max(1);
    let name = format!("{:<name_width$}", clip(name, name_width));
    let task = clip(task, task_width);
    let padding = width
        .saturating_sub(lead_width + name.chars().count() + 1 + task.chars().count() + age_width)
        .max(1);

    Line::from(vec![
        lead,
        Span::raw(" "),
        Span::raw(name),
        Span::raw(" "),
        Span::raw(task),
        Span::raw(" ".repeat(padding)),
        Span::styled(age, theme.soft()),
    ])
}
