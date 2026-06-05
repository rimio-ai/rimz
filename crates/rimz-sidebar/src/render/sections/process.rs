//! Bare process rows: the dim shell/build line, its right-pinned resource
//! stats, the full-command detail line, and the resolver's composed row.

use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};
use rimz::SidebarRow;
use rimz::feed::AgentStatus;

use crate::render::fmt::{age_short, clip, fmt_cpu, fmt_io, fmt_rss};
use crate::render::labels::{status_glyph, working_glyph};
use crate::render::theme::{ORANGE, Theme};

use super::{Tier, pin_right, trim_spans_to_width};

pub(super) fn process_row_line(
    theme: &Theme,
    row: &SidebarRow,
    width: usize,
    animation_phase: u64,
) -> Line<'static> {
    let dim = theme.dim();
    // An active pane (a build, a test, a script) gets the running braille spinner
    // so live work reads at a glance; an idle shell or a TUI the user just sits in
    // rests on the same hollow `○` an idle agent shows, so the lead column reads
    // and aligns alike across the two. Both stay in the dim chrome tone, never the
    // agent's clay, so a process stays secondary to an agent.
    let lead = if row.process_active {
        working_glyph(animation_phase)
    } else {
        status_glyph(AgentStatus::Idle)
    };
    let left = vec![
        Span::styled(lead, dim),
        Span::raw(" "),
        Span::styled(row.name.clone(), dim),
    ];
    // At L2 width, resource stats pin right: `C 11%  M 1.1G  ⇅ 3M/s`.
    // Any token whose data is absent is omitted; all three drop at L0/L1.
    if Tier::for_width(width) == Tier::L2 {
        let right = proc_stats_spans(theme, row);
        if !right.is_empty() {
            return pin_right(left, right, width);
        }
    }
    Line::from(trim_spans_to_width(left, width))
}

/// Build the right-pinned resource stats spans for a process row. Returns an
/// empty vec when no metrics are available. Tokens are joined with a two-space
/// gap. Each marker wears its own tone — `C` in the live-work clay (CPU is how
/// hard the pane works), `M` in the capacity violet, `⇅` in the flow teal —
/// while the figures stay dim so the row keeps its secondary process tone.
pub(in crate::render) fn proc_stats_spans(theme: &Theme, row: &SidebarRow) -> Vec<Span<'static>> {
    let dim = theme.dim();
    let mut tokens: Vec<(&str, Color, String)> = Vec::new();
    if let Some(pct) = row.cpu_pct {
        tokens.push(("C", ORANGE, fmt_cpu(pct)));
    }
    if let Some(rss) = row.rss_kb {
        tokens.push(("M", Color::Magenta, fmt_rss(rss)));
    }
    if let Some(bps) = row.io_bps {
        tokens.push(("⇅", Color::Cyan, fmt_io(bps)));
    }
    let mut spans = Vec::with_capacity(tokens.len() * 3);
    for (i, (glyph, color, figure)) in tokens.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ".to_owned(), dim));
        }
        spans.push(Span::styled(
            glyph.to_owned(),
            theme.style(color, Modifier::empty()),
        ));
        spans.push(Span::styled(format!(" {figure}"), dim));
    }
    spans
}

/// Line 2 for an *active* process row: the full foreground command, dim and
/// indented under the shell anchor, so a build or a `sudo` install reads in full
/// while the primary line keeps the stable shell label. `None` when the producer
/// left no detail (an idle pane, or a command already shown whole on line 1).
pub(super) fn process_detail_line(
    theme: &Theme,
    row: &SidebarRow,
    width: usize,
) -> Option<Line<'static>> {
    let detail = row.command_detail.as_deref()?;
    let left = vec![
        Span::raw("  "),
        Span::styled(detail.to_owned(), theme.dim()),
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
        Span::styled(age, theme.dim()),
    ])
}
