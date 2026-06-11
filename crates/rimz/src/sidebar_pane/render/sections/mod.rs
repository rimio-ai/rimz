//! Worktree-grouped sidebar composition. The snapshot owns grouping and
//! ordering; these modules only map the view-model to terminal lines.
//!
//! The renderer expresses one *design grammar* for every meter — context-%,
//! todo progress, diff stats — so the rows read as one polished card per
//! agent, not a stack of one-off widgets. See the
//! [grammar in docs/internals/sidebar/sidebar.md](../../../../docs/internals/sidebar/sidebar.md).
//!
//! One module per section: [`cockpit`] (the summary + spend lines), [`fleet`]
//! (the make-up line), [`worktree`] (group headers and the
//! row roster), [`agent_card`] (the per-agent card), [`process`] (bare process
//! rows), and [`provider`] (the provider dashboard and the W/M fleet ledger).
//! This file owns only the shared layout primitives — the width tiers, the
//! gutter, and the span-packing helpers every section composes with.

use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};

use super::theme::Theme;

mod agent_card;
mod cockpit;
mod fleet;
mod process;
mod provider;
mod worktree;

pub(super) use cockpit::{cockpit_spend_line, cockpit_summary_line};
pub(crate) use fleet::{MakeUpHit, status_total};
pub(super) use fleet::{fleet_header_lines, fleet_size};
#[cfg(test)]
pub(super) use process::proc_stats_spans;
pub(crate) use provider::ProviderTabHit;
pub(super) use provider::{fleet_ledger_lines, provider_panel_lines};
pub(super) use worktree::worktree_group_lines;

/// The cockpit/ledger session-count glyph: `◎` for the sessions (threads) that
/// have run today. Shared by the cockpit summary and the W/M ledger rows, so a
/// session count reads the same in both places.
const SESSIONS_GLYPH: &str = "◎";

/// A brighter sage than the resting money-green, held for a couple of frames as
/// a figure lands — the quiet "ka-chunk" of a money count-up. Shared by the
/// cockpit headline and the agent cards' `$cost`, so every settle brightens the
/// same; drops to plain bold under `NO_COLOR` like every other tone.
const VALUE_FLASH: Color = Color::Indexed(150);

/// An active provider-tab pick's `NO_COLOR` caps — when a chip fill drops with
/// the colors, these notch the pick by shape inside the rail's reserved cells.
const TAB_CAP_LEFT: char = '┤';
const TAB_CAP_RIGHT: char = '├';

/// The active chip's ink: near-black, crisp on every mid-brightness fill — the
/// provider tab rail's brand fill and the make-up bucket fills alike. A fixed
/// ink tone, not a semantic palette slot.
const TAB_INK: Color = Color::Indexed(16);

/// The selected card's left accent: a bold half-block `▌` running the card's
/// full height — the one loud lane marker on screen.
const SELECTED_SPINE: &str = "▌";

/// The selected *worktree's* resting lane spine: a thin `▏` (lighter than the
/// selected card's `▌`) down the whole selected group — header and every row —
/// so the worktree holding the selection reads as one bracketed lane.
/// Non-selected worktrees carry no spine at all.
const LANE_SPINE: &str = "▏";

/// Inner content width: the sidebar width less the one-cell left gutter and the
/// one-cell right margin. Every line is built to this width and then opened with
/// a gutter cell (blank, lane `▏`, or selected `▌`), leaving the trailing column
/// as the matching right margin — so the whole sidebar reads inside a one-cell
/// frame and selecting a row only swaps the gutter glyph, never shifts a column.
pub(super) fn content_width(width: usize) -> usize {
    width.saturating_sub(2).max(1)
}

/// Width band that drives the ambient row density.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tier {
    /// Identity + a bare gauge, no labels (~24 columns).
    L0,
    /// Default: line 1 cue + capability + context gauge (~30 columns).
    L1,
    /// Wide: line 2 also inlines todo / extra meters (~44+).
    L2,
}

impl Tier {
    pub(super) fn for_width(width: usize) -> Self {
        if width >= 44 {
            Tier::L2
        } else if width >= 30 {
            Tier::L1
        } else {
            Tier::L0
        }
    }
}

/// The one-cell left gutter that opens every line — blank for chrome and resting
/// worktrees, the resting lane `▏` for the selected worktree, the bold `▌` accent
/// for the selected card itself.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Gutter {
    /// No marker — chrome and non-selected worktrees.
    Blank,
    /// The selected worktree's resting lane spine (`▏`, dim teal).
    Lane,
    /// The selected card's bold accent spine (`▌`).
    Selected,
}

/// Open a line with its one-cell gutter (see [`Gutter`]). The cell is always one
/// column, so changing it never shifts content; the trailing column the content
/// leaves free is the matching right margin. Applied to every line of a worktree
/// group so the lane spans the whole selected worktree as one block, with the
/// selected card lit `▌` inside it. Under `NO_COLOR` the `▏`/`▌` shapes carry the
/// lane and the selection without color. Rebuilding the line would drop a
/// line-level style (`Line::styled` chrome like the dim `+K more`), so the
/// incoming style is patched onto each content span — the gutter cell keeps its
/// own tone untouched.
fn with_gutter(theme: &Theme, line: Line<'static>, gutter: Gutter) -> Line<'static> {
    let cell = match gutter {
        Gutter::Blank => Span::raw(" "),
        Gutter::Lane => Span::styled(LANE_SPINE, theme.style(Color::Cyan, Modifier::DIM)),
        Gutter::Selected => Span::styled(SELECTED_SPINE, theme.selection()),
    };
    let base = line.style;
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(cell);
    spans.extend(
        line.spans
            .into_iter()
            .map(|span| Span::styled(span.content, base.patch(span.style))),
    );
    Line::from(spans)
}

/// Pack `left` from the start and pin `right` flush to the trailing edge: trim
/// the left to leave room for the right plus a one-cell gap, then pad the gap so
/// the right cluster ends at `width`. Shared by the identity line and the meter
/// rows so every right-anchored column lands on one edge.
fn pin_right(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    if right.is_empty() {
        return Line::from(trim_spans_to_width(left, width));
    }
    let right_width = spans_width(&right);
    let mut spans = trim_spans_to_width(left, width.saturating_sub(right_width + 1));
    let padding = width
        .saturating_sub(spans_width(&spans) + right_width)
        .max(1);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.extend(right);
    Line::from(spans)
}

/// Total display width of a span run, in terminal cells.
fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(Span::width).sum()
}

fn trim_spans_to_width(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut remaining = width;
    let mut trimmed = Vec::new();
    for span in spans {
        if remaining == 0 {
            break;
        }
        let span_width = span.width();
        if span_width <= remaining {
            remaining -= span_width;
            trimmed.push(span);
            continue;
        }
        let content = take_cells(span.content.as_ref(), remaining);
        if !content.is_empty() {
            trimmed.push(Span::styled(content, span.style));
        }
        break;
    }
    trimmed
}

fn take_cells(content: &str, width: usize) -> String {
    let mut taken = String::new();
    for ch in content.chars() {
        let mut candidate = taken.clone();
        candidate.push(ch);
        if Span::raw(candidate.as_str()).width() > width {
            break;
        }
        taken.push(ch);
    }
    taken
}

/// A stats metric as a colored icon glyph + value (`◷ 2h34m`, `¤ 5`): the
/// glyph carries a semantic accent (time teal, the live-agent `¤` clay) while
/// the number reads at the soft tier like every stat figure — so the stats
/// read as a tidy icon column instead of a wall of one tone.
fn metric_spans(theme: &Theme, glyph: &str, color: Color, value: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(glyph.to_owned(), theme.style(color, Modifier::empty())),
        Span::styled(format!(" {value}"), theme.soft()),
    ]
}
