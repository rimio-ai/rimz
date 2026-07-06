//! Bare process rows: the shell/build line, its right-pinned resource stats,
//! and the full-command detail line.

use crate::agents::AgentStatus;
use crate::config::{AnimationRole, GlyphRole};
use crate::{ProcessState, SidebarRow};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::sidebar_pane::render::fmt::{fmt_cpu, fmt_io, fmt_rss};
use crate::sidebar_pane::render::labels::{role_glyph, status_glyph, status_style, working_style};
use crate::sidebar_pane::render::theme::{Component, Theme};

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
    // on the neutral hollow `○` of an idle agent — the lead DIM-weighted, the
    // name at the soft tier. That slight step — not a seam line — is what
    // sets the group's command tail apart from the agent cards above it, and
    // under `NO_COLOR` it survives as the soft tier's DIM weight.
    let state = row.process_state().unwrap_or(ProcessState::Idle);
    let foreign_user = row
        .as_process()
        .and_then(|process| process.foreign_user.as_deref());
    let (lead, lead_style) = if foreign_user.is_some() && state.is_idle() {
        (
            status_glyph(theme, AgentStatus::Running),
            working_style(theme, animation_phase).add_modifier(Modifier::DIM),
        )
    } else {
        match state {
            ProcessState::Busy => (
                role_glyph(theme, AnimationRole::Working, animation_phase),
                working_style(theme, animation_phase).add_modifier(Modifier::DIM),
            ),
            ProcessState::Stuck => (
                status_glyph(theme, AgentStatus::Failed),
                status_style(theme, AgentStatus::Failed).add_modifier(Modifier::BOLD),
            ),
            ProcessState::Idle => (
                status_glyph(theme, AgentStatus::Idle),
                status_style(theme, AgentStatus::Idle).add_modifier(Modifier::DIM),
            ),
        }
    };
    let mut left = vec![
        Span::styled(lead, lead_style),
        Span::raw(" "),
        Span::styled(row.name.clone(), theme.body()),
    ];
    if let Some(user) = foreign_user {
        left.push(Span::raw(" "));
        left.push(Span::styled(format!("({user})"), theme.muted()));
    }
    // At L2 width, active process rows pin resource stats right:
    // `C  11%  M 1.1G  ⇅   3M/s`. Idle shells and editors stay bare; the
    // whole cluster also drops at L0/L1, or until every metric has reported.
    if Tier::for_width(width) == Tier::L2 && !state.is_idle() {
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
/// right-aligned figure), and the whole cluster appears only after CPU,
/// memory, and IO have all reported. Each marker wears its own tone (`C` sky,
/// `M` sage, `⇅` violet, DIM-weighted like the clay lead so the row stays a
/// step below the agent cards). Figures and seams stay dim — the stats are
/// secondary chrome, and the lead glyph carries the row's liveness. Empty
/// before the second rate sample, on partial reads, and on non-Linux.
pub(in crate::sidebar_pane::render) fn proc_stats_spans(
    theme: &Theme,
    row: &SidebarRow,
) -> Vec<Span<'static>> {
    let Some(process) = row.as_process() else {
        return Vec::new();
    };
    let (Some(cpu_pct), Some(rss_kb), Some(io_bps)) =
        (process.cpu_pct, process.rss_kb, process.io_bps)
    else {
        return Vec::new();
    };
    let slots: [(GlyphRole, Component, usize, String); 3] = [
        (
            GlyphRole::ProcessCpu,
            Component::ProcCpu,
            CPU_SLOT,
            fmt_cpu(cpu_pct),
        ),
        (
            GlyphRole::ProcessMem,
            Component::ProcMem,
            RSS_SLOT,
            fmt_rss(rss_kb),
        ),
        (
            GlyphRole::ProcessIo,
            Component::ProcIo,
            IO_SLOT,
            fmt_io(io_bps),
        ),
    ];
    let mut spans = Vec::with_capacity(3 * slots.len());
    for (i, (role, tone, width, figure)) in slots.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", theme.muted()));
        }
        spans.push(Span::styled(
            format!("{} ", theme.glyph(role)),
            theme.styled(tone, Modifier::DIM),
        ));
        spans.push(Span::styled(format!("{figure:>width$}"), theme.muted()));
    }
    spans
}

/// Line 2 for an *active* process row: the foreground command with its program
/// path trimmed and arguments verbatim in the row's [`Theme::soft`] tone,
/// indented under the shell anchor, so a build or a `sudo` install reads in
/// full while the primary line keeps the stable shell label. `None` when the
/// producer left no detail (an idle pane, or a command already shown whole on
/// line 1).
pub(super) fn process_detail_line(
    theme: &Theme,
    row: &SidebarRow,
    width: usize,
) -> Option<Line<'static>> {
    let detail = row.as_process()?.command_detail.as_deref()?;
    let left = vec![
        Span::raw("  "),
        Span::styled(detail.to_owned(), theme.body()),
    ];
    Some(Line::from(trim_spans_to_width(left, width)))
}
