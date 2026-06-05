//! Worktree group composition: the bold pod header with its right-pinned git
//! story, the dim `external` divider, and the row roster with its parallel
//! hit-test map entries.

use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};
use rimz::config::ContextSeverityConfig;
use rimz::{
    SidebarProviderPanel, SidebarRowKind, SidebarStatusCount, SidebarWorktreeGroup,
    SidebarWorktreeKind,
};

use crate::render::fmt::clip;
use crate::render::labels::{branch_delta_spans, diff_spans, status_glyph, trunk_equal_spans};
use crate::render::theme::Theme;

use super::agent_card::row_lines;
use super::{Gutter, Tier, content_width, with_gutter};

/// Compose one worktree group's lines, appending to `lines`, and tag each
/// content line in the parallel `map` with the visible row index it belongs to
/// (or `None` for the group header and the `+K more` hidden-count line). `map`
/// stays exactly as long as `lines`, so the hit-test can look a screen line up
/// to a row with no separate geometry. The row index captured for a row's lines
/// is the value *before* `row_index` advances, matching `app::visible_rows()`.
#[allow(clippy::too_many_arguments)]
pub(in crate::render) fn worktree_group_lines(
    theme: &Theme,
    group: &SidebarWorktreeGroup,
    providers: &[SidebarProviderPanel],
    width: usize,
    bands: &ContextSeverityConfig,
    row_index: &mut usize,
    selected_index: usize,
    animation_phase: u64,
    lines: &mut Vec<Line<'static>>,
    map: &mut Vec<Option<usize>>,
) {
    // Does the selection live in this worktree? If so the whole group reads as one
    // bracketed lane: the resting `▏` spine on the header, every row, and the
    // inter-card gaps, with the selected card itself lit bold `▌`. The `external`
    // catch-all is never a lane.
    let first_row = *row_index;
    let group_selected = group.kind != SidebarWorktreeKind::Workspace
        && (first_row..first_row + group.rows.len()).contains(&selected_index);
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
    let header_target = (group.kind != SidebarWorktreeKind::Workspace && !group.rows.is_empty())
        .then_some(*row_index);
    map.push(header_target);
    let tier = Tier::for_width(content_width(width));
    // A blank line separates consecutive cards; it carries the group's lane
    // gutter so a selected worktree's spine runs unbroken through the gap, and
    // maps to `None` as structural chrome (never a jump target).
    for (index, row) in group.rows.iter().enumerate() {
        if index > 0 {
            lines.push(with_gutter(theme, Line::from(""), lane));
            map.push(None);
        }
        // The first process row after the agent cards opens the group's command
        // tail under a faint `┄ commands ┄┄┄` seam, so the full-strength process
        // rows still read apart from the agents above. Rows sort agents-first,
        // so the boundary occurs at most once; the `external` catch-all already
        // leads with its own dotted divider, so it never doubles up. Structural
        // chrome like the gap line: lane gutter, `None` in the hit-test map.
        if group.kind != SidebarWorktreeKind::Workspace
            && index > 0
            && row.row_kind == SidebarRowKind::Process
            && group.rows[index - 1].row_kind == SidebarRowKind::Agent
        {
            lines.push(with_gutter(theme, commands_divider(theme, width), lane));
            map.push(None);
        }
        let selected = *row_index == selected_index;
        let this_row = *row_index;
        *row_index += 1;
        let gutter = if selected { Gutter::Selected } else { lane };
        let row_lines = row_lines(
            theme,
            row,
            providers,
            width,
            tier,
            selected,
            animation_phase,
            bands,
            gutter,
        );
        map.extend(std::iter::repeat_n(Some(this_row), row_lines.len()));
        lines.extend(row_lines);
    }
    if group.hidden_count > 0 {
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
    if group.kind == SidebarWorktreeKind::Workspace {
        return workspace_divider(theme, group, width);
    }
    // The lane spine (added by the caller) opens the header, so the label leads
    // here in bold teal — no inline `▌`, the spine carries the lane. The header
    // builds to the content width left after the gutter cell.
    let cw = content_width(width);
    // The worktree's git story pins right: `≡ <trunk>` when a non-trunk branch
    // is fully landed, else the `⇡/⇣` commit delta ahead of the `+/-` churn,
    // zero components omitted. The per-worktree status tally is gone: the
    // cockpit owns the fleet make-up and each row carries its own status
    // glyph, so repeating it here was noise. The label clips to whatever's
    // left after the stats claim their width, always leaving a cell so the
    // header never shrinks to zero on extreme narrowness.
    let right = group_git_spans(theme, group);
    let right_width: usize = right.iter().map(|span| span.content.chars().count()).sum();
    let label_width = cw.saturating_sub(right_width + 1).max(1);
    let label_with_prefix = format!("⑂ {}", group.label);
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

/// The header's right-pinned git cluster. `≡ <trunk>` when the worktree is
/// fully landed — zero commits ahead *and* a zero diff against the fork point
/// (`Some(0)`, a read that found nothing, never an unprobed `None`) — replacing
/// every other stat: behind deliberately doesn't count against it, since a
/// landed worktree is safe to remove however far the trunk has moved on. The
/// trunk worktree itself (live branch == trunk) is exempt — it is trivially
/// "landed on itself," so the marker would be noise there, and it keeps the
/// plain delta/churn cluster instead. Otherwise the `⇡/⇣` commit delta leads
/// the `+/-` churn, zero components omitted. Empty when no git read reached
/// this group.
fn group_git_spans(theme: &Theme, group: &SidebarWorktreeGroup) -> Vec<Span<'static>> {
    let landed = group.commits_ahead == Some(0)
        && group.diff_added == Some(0)
        && group.diff_removed == Some(0);
    if landed
        && let Some(trunk) = group.trunk.as_deref()
        && group.label != trunk
    {
        return trunk_equal_spans(theme, trunk);
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

/// The seam between a worktree group's agent cards and its bare process rows:
/// a faint `┄ commands ┄┄┄` divider in the `external` divider's dotted voice.
/// Process rows read at agent-card strength, so the seam — not a dim tone — is
/// what marks them as the group's command tail rather than more agents.
fn commands_divider(theme: &Theme, width: usize) -> Line<'static> {
    let cw = content_width(width);
    let head = "┄ commands ";
    let fill = cw.saturating_sub(head.chars().count()).max(1);
    Line::styled(format!("{head}{}", "┄".repeat(fill)), theme.faint())
}

/// The `workspace` catch-all (untethered scripts/CI and out-of-project shells)
/// renders as a dim `┄ external ┄┄┄` divider rather than a bold `▌` pod header.
/// It keeps an *attention-only* tally (`? n` / `! n`) so a waiting script ask
/// still surfaces; the calm counts stay with the cockpit.
fn workspace_divider(theme: &Theme, group: &SidebarWorktreeGroup, width: usize) -> Line<'static> {
    let cw = content_width(width);
    let tally = attention_tally(&group.status_counts);
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
fn attention_tally(counts: &[SidebarStatusCount]) -> String {
    counts
        .iter()
        .filter(|count| count.status.is_actionable())
        .filter(|count| count.count > 0)
        .map(|count| format!("{} {}", status_glyph(count.status), count.count))
        .collect::<Vec<_>>()
        .join("  ")
}
