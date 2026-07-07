//! Worktree-grouped sidebar composition. The snapshot owns grouping and
//! ordering; these modules only map the view-model to terminal lines.
//!
//! The renderer expresses one *design grammar* for every meter — context-%,
//! diff stats — so the rows read as one polished card per
//! agent, not a stack of one-off widgets. See the
//! [grammar in docs/internals/sidebar/sidebar.md](../../../../docs/internals/sidebar/sidebar.md).
//!
//! One module per section: [`cockpit`] (the summary + spend lines), [`fleet`]
//! (the make-up line), [`worktree`] (group headers and the
//! row roster), [`agent_card`] (the per-agent card), [`process`] (bare process
//! rows), and [`provider`] (the provider dashboard and the W/M fleet store).
//! This file owns only the shared section primitives — the width tiers and the
//! gutter every section composes with.

use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};

use crate::config::GlyphRole;

pub(super) use super::layout::{pin_right, spans_width, trim_spans_to_width};
use super::theme::{Component, Theme};

mod agent_card;
mod cockpit;
mod fleet;
mod pets;
mod process;
mod provider;
mod worktree;

pub(in crate::sidebar_pane::render) use agent_card::awaiting_first_prompt_affordance;
pub(super) use cockpit::{cockpit_spend_line, cockpit_summary_line};
pub(crate) use fleet::{MakeUpHit, status_total, unread_total};
pub(super) use fleet::{fleet_header_lines, fleet_size};
#[cfg(test)]
pub(super) use process::proc_stats_spans;
pub(crate) use provider::ProviderTabHit;
pub(super) use provider::dashboard_panel_lines_with_footer;
#[cfg(test)]
pub(in crate::sidebar_pane::render) use provider::reset_expiry_heat_amount;
pub(super) use provider::{fleet_store_lines, fleet_total_lines};
pub(super) use worktree::worktree_group_lines;

/// Inner content width: the sidebar width less the one-cell left gutter and the
/// one-cell right rail. Card and worktree lines build to this width before
/// [`with_gutter`] frames them with both edge cells. Chrome lines use the same
/// inner width and reserve the right rail as blank space, so selecting a row
/// never shifts a content column.
pub(super) fn content_width(width: usize) -> usize {
    width.saturating_sub(2)
}

/// Width band that drives the ambient row density.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tier {
    /// Identity + a bare gauge, no labels (~24 columns).
    L0,
    /// Default: line 1 cue + capability + context gauge (~30 columns).
    L1,
    /// Wide: line 2 also inlines extra meters (~44+).
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

/// The gutter state that frames a line with one left cell and one right rail
/// cell — blank for chrome and resting worktrees, the resting lane `▎`/`🮇` for
/// the selected worktree, the bold `▌`/`▐` accent for the selected card itself.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Gutter {
    /// No marker — chrome and non-selected worktrees.
    Blank,
    /// The selected worktree's resting lane spine (`▎`/`🮇`, the dim selection tone).
    Lane,
    /// The selected card's bold accent spine (`▌`/`▐`).
    Selected,
}

/// Frame a line with its left gutter and right rail (see [`Gutter`]). Both cells
/// are always one column, so changing them never shifts content; the right rail
/// is active space, owned by the spine, scrollbar, or blank frame. Applied to
/// every line of a worktree group so the lane spans the whole selected worktree
/// as one block, with the selected card lit `▌`/`▐` inside it. Under `NO_COLOR`
/// the shapes carry the lane and the selection without color. Rebuilding the
/// line would drop a line-level style (`Line::styled` chrome like the dim `+K
/// more`), so the incoming style is patched onto each content span — the gutter
/// cells keep their own tone untouched.
///
/// `wash` is the resolved unread-card background ([`Theme::unread_wash`]) for an
/// unread card that is not itself the selection; it grounds the card in a soft,
/// uniform panel — a lighter tint of the selection blue — so the whole row reads as
/// unseen at a glance. The selection band always wins when both apply, so a
/// selected card never doubles the cue, and chrome (header, `+K more`) passes
/// `None`.
fn with_gutter(
    theme: &Theme,
    line: Line<'static>,
    gutter: Gutter,
    wash: Option<Color>,
    width: usize,
) -> Line<'static> {
    // The selected card rests on a background band: a dark fill behind every one
    // of its lines, padding included, so the whole card reads as one recessed
    // block. At truecolor depth the band recesses a flat step below `selection_bg`
    // ([`Theme::selection_band`]), giving the panel depth. An unread non-selected
    // card grounds in its uniform `wash` here instead — the same panel surface, a
    // lighter tint of the selection blue. The lane bracket and chrome carry no
    // band; `NO_COLOR` drops it and the bright spine plus bold weight carry the
    // selection alone.
    let band = match gutter {
        Gutter::Selected => theme.selection_band(),
        Gutter::Blank | Gutter::Lane => wash,
    };
    let (left_cell, right_cell) = match gutter {
        Gutter::Blank => (Span::raw(" "), Span::raw(" ")),
        Gutter::Lane => (
            Span::styled(
                theme.glyph(GlyphRole::ChromeSpineLaneLeft).to_owned(),
                theme.styled(Component::LaneSpine, Modifier::DIM),
            ),
            Span::styled(
                theme.glyph(GlyphRole::ChromeSpineLaneRight).to_owned(),
                theme.styled(Component::LaneSpine, Modifier::DIM),
            ),
        ),
        Gutter::Selected => (
            Span::styled(
                theme.glyph(GlyphRole::ChromeSpineCardLeft).to_owned(),
                theme.selection(),
            ),
            Span::styled(
                theme.glyph(GlyphRole::ChromeSpineCardRight).to_owned(),
                theme.selection(),
            ),
        ),
    };
    let left_cell = banded(left_cell, band);
    let right_cell = banded(right_cell, band);
    if width == 0 {
        return Line::from(Vec::<Span<'static>>::new());
    }
    if width == 1 {
        return Line::from(vec![left_cell]);
    }
    let base = line.style;
    let cw = content_width(width);
    let mut content = trim_spans_to_width(
        line.spans
            .into_iter()
            .map(|span| banded(Span::styled(span.content, base.patch(span.style)), band))
            .collect(),
        cw,
    );
    let used = spans_width(&content);
    if used < cw {
        content.push(banded(Span::raw(" ".repeat(cw - used)), band));
    }

    let mut spans = Vec::with_capacity(content.len() + 2);
    spans.push(left_cell);
    spans.extend(content);
    spans.push(right_cell);
    Line::from(spans)
}

/// Lay the selection band behind a span when one is active, holding its
/// foreground tone and weight. A `None` band (chrome, the lane, or `NO_COLOR`)
/// returns the span untouched, so the band paints only the selected card.
fn banded(span: Span<'static>, band: Option<Color>) -> Span<'static> {
    match band {
        Some(bg) => Span::styled(span.content, span.style.bg(bg)),
        None => span,
    }
}

/// A stats metric as a colored icon glyph + value (`◷ 2h34m`, `¤ 5`): the
/// glyph carries a semantic accent (time teal, the live-agent `¤` clay) while
/// the number reads at the soft tier like every stat figure — so the stats
/// read as a tidy icon column instead of a wall of one tone.
fn metric_spans(theme: &Theme, glyph: &str, color: Color, value: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(glyph.to_owned(), theme.style(color, Modifier::empty())),
        Span::styled(format!(" {value}"), theme.body()),
    ]
}
