//! Worktree group composition: the bold pod header with its right-pinned git
//! story, the dim `external` divider, and the row roster with its parallel
//! hit-test map entries.

use crate::config::{CardDensityMode, ContextSeverityConfig};
use crate::feed::AgentStatus;
use crate::{SidebarProviderPanel, SidebarStatusCount, SidebarWorktreeGroup, SidebarWorktreeKind};
use jiff::Timestamp;
use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};

use crate::sidebar_pane::render::CostRolls;
use crate::sidebar_pane::render::fmt::clip;
use crate::sidebar_pane::render::labels::{
    branch_delta_spans, diff_spans, status_glyph, trunk_clear_spans, trunk_equal_spans,
};
use crate::sidebar_pane::render::row_passes_filter;
use crate::sidebar_pane::render::theme::Theme;

use super::agent_card::row_lines;
use super::{Gutter, Tier, content_width, with_gutter};

/// Compose one worktree group's lines, appending to `lines`, and tag each
/// content line in the parallel `map` with the visible row index it belongs to
/// (or `None` for the group header and the `+K more` hidden-count line). `map`
/// stays exactly as long as `lines`, so the hit-test can look a screen line up
/// to a row with no separate geometry. The row index captured for a row's lines
/// is the value *before* `row_index` advances, matching `app::visible_rows()`:
/// both walk the same [`row_passes_filter`] predicate, so the ordinals stay 1:1
/// under a make-up filter too. The caller skips a group the filter empties; the
/// `+K more` line is filter-suppressed here (it counts producer-capped calm
/// rows, not filtered ones, so it would mislead under a narrowed body).
#[allow(clippy::too_many_arguments)]
pub(in crate::sidebar_pane::render) fn worktree_group_lines(
    theme: &Theme,
    group: &SidebarWorktreeGroup,
    providers: &[SidebarProviderPanel],
    now: Timestamp,
    width: usize,
    bands: &ContextSeverityConfig,
    card_density: CardDensityMode,
    filter: Option<AgentStatus>,
    row_index: &mut usize,
    selected_index: usize,
    animation_phase: u64,
    cost_rolls: &CostRolls,
    lines: &mut Vec<Line<'static>>,
    map: &mut Vec<Option<usize>>,
) {
    // Does the selection live in this worktree? If so the whole group reads as
    // one bracketed lane: the resting `▏` spine on the header and every row,
    // with the selected card itself lit bold `▌`. The `external` catch-all is
    // never a lane.
    let first_row = *row_index;
    let passing = group
        .rows
        .iter()
        .filter(|row| row_passes_filter(row, filter))
        .count();
    let group_selected = group.kind != SidebarWorktreeKind::External
        && (first_row..first_row + passing).contains(&selected_index);
    let lane = if group_selected {
        Gutter::Lane
    } else {
        Gutter::Blank
    };

    // The header carries the lane gutter when its worktree is selected (blank
    // otherwise), and its dotted `┄` seal shows only then, so an unselected
    // worktree is just its bold label. The `external` divider is full-bleed
    // chrome with a blank gutter.
    let header = group_header(theme, group, width, group_selected);
    lines.push(with_gutter(theme, header, lane));
    // The worktree name is itself a click target: it lands on the group's first
    // row — the agent adjacent to the header — so clicking the pod name jumps
    // straight into it. The `external` divider is not a worktree name, so it
    // stays inert chrome.
    let header_target =
        (group.kind != SidebarWorktreeKind::External && passing > 0).then_some(*row_index);
    map.push(header_target);
    let tier = Tier::for_width(content_width(width));
    for row in group
        .rows
        .iter()
        .filter(|row| row_passes_filter(row, filter))
    {
        let selected = *row_index == selected_index;
        let this_row = *row_index;
        *row_index += 1;
        let gutter = if selected { Gutter::Selected } else { lane };
        let row_lines = row_lines(
            theme,
            row,
            providers,
            now,
            width,
            tier,
            selected,
            card_density,
            animation_phase,
            cost_rolls,
            bands,
            gutter,
        );
        map.extend(std::iter::repeat_n(Some(this_row), row_lines.len()));
        lines.extend(row_lines);
    }
    if filter.is_none() && group.hidden_count > 0 {
        lines.push(with_gutter(
            theme,
            Line::styled(format!("  +{} more", group.hidden_count), theme.dim()),
            lane,
        ));
        map.push(None);
    }
}

fn group_header(
    theme: &Theme,
    group: &SidebarWorktreeGroup,
    width: usize,
    sealed: bool,
) -> Line<'static> {
    // The catch-all is not a worktree — render it as a dim divider, not a bold
    // pod header, so out-of-project sessions read as "outside the project."
    if group.kind == SidebarWorktreeKind::External {
        return external_divider(theme, group, width);
    }
    // The lane spine (added by the caller) opens the header, so the label leads
    // here in bold teal — no inline `▌`, the spine carries the lane. The header
    // builds to the content width left after the gutter cell.
    let cw = content_width(width);
    // The worktree's git story pins right: a landed marker when a non-trunk
    // branch holds no work of its own (`≡ <trunk>` at the tip, `✓ <trunk>`
    // once the trunk moved on), else the `⇡/⇣` commit delta ahead of the
    // `+/-` churn, zero components omitted. The per-worktree status tally is
    // gone: the cockpit owns the fleet make-up and each row carries its own
    // status glyph, so repeating it here was noise. The label clips to
    // whatever's left after the stats claim their width, always leaving a
    // cell so the header never shrinks to zero on extreme narrowness.
    //
    // A non-repo room's root pod is name-only: a plain directory has no fork
    // and no git story, so it drops the `⑂` prefix and pins nothing right.
    let right = match group.kind {
        SidebarWorktreeKind::Root => Vec::new(),
        _ => group_git_spans(theme, group),
    };
    let right_width: usize = right.iter().map(|span| span.content.chars().count()).sum();
    let label_width = cw.saturating_sub(right_width + 1).max(1);
    let label_with_prefix = match group.kind {
        SidebarWorktreeKind::Root => group.label.clone(),
        _ => format!("⑂ {}", group.label),
    };
    let left = clip(&label_with_prefix, label_width);
    // The dotted `┄` seal caps only the *selected* worktree's header, so the lane
    // reads as one bracketed block; every other header is just its bold label and
    // right-pinned stats, with plain space filling the gap. Sized to land the line
    // exactly on the content width — a space frames the dotted run from the text
    // on each side it touches.
    let middle = cw.saturating_sub(left.chars().count() + right_width);
    let fill = if sealed {
        match (right.is_empty(), middle) {
            (false, m) if m >= 2 => format!(" {} ", "┄".repeat(m - 2)),
            (true, m) if m >= 1 => format!(" {}", "┄".repeat(m - 1)),
            (_, m) => " ".repeat(m),
        }
    } else {
        " ".repeat(middle)
    };

    let mut spans = vec![
        Span::styled(left, theme.style(Color::Cyan, Modifier::BOLD)),
        Span::styled(fill, theme.faint()),
    ];
    spans.extend(right);
    Line::from(spans)
}

/// The header's right-pinned git cluster. A worktree holding no work of its
/// own — zero commits ahead, a zero diff against the fork point, and a proven
/// clean working tree, untracked included (every read `Some`, a probe that
/// found nothing, never an unprobed `None`) — collapses the cluster to a
/// landed marker: `≡ <trunk>` when it sits exactly at the trunk tip (zero
/// behind), `✓ <trunk>` once the trunk has moved on — done, safe to remove.
/// The trunk worktree itself (live branch == trunk) is exempt — it is
/// trivially "landed on itself," so the marker would be noise there, and it
/// keeps the plain delta/churn cluster instead. Otherwise the `⇡/⇣` commit
/// delta leads the `+/-` churn (untracked line counts folded in by the
/// producer), zero components omitted. Empty when no git read reached this
/// group.
fn group_git_spans(theme: &Theme, group: &SidebarWorktreeGroup) -> Vec<Span<'static>> {
    let no_pending = group.commits_ahead == Some(0)
        && group.diff_added == Some(0)
        && group.diff_removed == Some(0)
        && group.clean == Some(true);
    if no_pending
        && let Some(trunk) = group.trunk.as_deref()
        && group.label != trunk
    {
        match group.commits_behind {
            Some(0) => return trunk_equal_spans(theme, trunk),
            Some(_) => return trunk_clear_spans(theme, trunk),
            // A degraded behind read can't pick a marker; fall through to the
            // plain cluster rather than claim equality.
            None => {}
        }
    }
    let mut spans = branch_delta_spans(
        theme,
        group.commits_ahead.unwrap_or(0),
        group.commits_behind.unwrap_or(0),
    );
    let diff = group
        .diff_added
        .zip(group.diff_removed)
        .filter(|(added, removed)| *added + *removed > 0);
    if let Some((added, removed)) = diff {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.extend(diff_spans(theme, added, removed));
    }
    spans
}

/// The `external` catch-all (untethered scripts/CI and out-of-project shells)
/// renders as a dim `┄ external ┄┄┄` divider rather than a bold `▌` pod header.
/// It keeps an *attention-only* tally (`? n` / `! n`) so a waiting script ask
/// still surfaces; the calm counts stay with the cockpit.
fn external_divider(theme: &Theme, group: &SidebarWorktreeGroup, width: usize) -> Line<'static> {
    let cw = content_width(width);
    let tally = attention_tally(theme, &group.status_counts);
    let head = format!("┄ {} ", group.label);
    let tail = if tally.is_empty() {
        String::new()
    } else {
        format!(" {tally}")
    };
    let fill = cw
        .saturating_sub(head.chars().count() + tail.chars().count())
        .max(1);
    let mut spans = vec![
        Span::styled(head, theme.faint()),
        Span::styled("┄".repeat(fill), theme.faint()),
    ];
    if !tally.is_empty() {
        spans.push(Span::styled(tail, theme.dim()));
    }
    Line::from(spans)
}

/// An attention-only status tally — a spaced `? n` / `! n` for the waiting and
/// failed counts only. The glyph and count are separated by a space (`? 1`,
/// never `1?`); the calm states are omitted (the cockpit owns the fleet
/// make-up). Empty when nothing in the group needs a human.
fn attention_tally(theme: &Theme, counts: &[SidebarStatusCount]) -> String {
    counts
        .iter()
        .filter(|count| count.status.is_actionable())
        .filter(|count| count.count > 0)
        .map(|count| format!("{} {}", status_glyph(theme, count.status), count.count))
        .collect::<Vec<_>>()
        .join("  ")
}
