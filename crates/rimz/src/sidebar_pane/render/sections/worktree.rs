//! Worktree group composition: the bold pod header with its linked-PR identity
//! and right-pinned git story, the dim `external` divider, and the row roster
//! with its parallel hit-test map entries. Finished pods collapse hidden agents
//! into a status-and-soft-brand-name receipt with the accepted work's cost
//! pinned right.

use std::collections::HashSet;

use crate::config::GlyphRole;
use crate::{
    SidebarStatusCount, SidebarWorktreeGroup, SidebarWorktreeKind, WorktreePrState,
    WorktreeTrunkSync,
};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::sidebar_pane::pixel::meter::MeterPixels;
use crate::sidebar_pane::render::fmt::dollars2;
use crate::sidebar_pane::render::labels::{
    branch_delta_spans, diff_spans, status_glyph, status_rest_style, trunk_glyph_spans,
};
use crate::sidebar_pane::render::layout::{ellipsize, spans_width, text_width};
use crate::sidebar_pane::render::theme::{Component, Theme};
use crate::sidebar_pane::render::{HitRegion, HitTarget};
use crate::sidebar_pane::view::{VisibleGroup, VisibleRoster};

use super::agent_card::{brand_tone, row_lines, session_cost_usd};
use super::{Gutter, RowCtx, content_width, with_gutter};

/// Compose one worktree group's lines, appending to `lines`, and tag each
/// content line in the parallel `map` with the visible row index it belongs to
/// (or `None` for the group header and more/less toggle line). `map` stays
/// exactly as long as `lines`, so the hit-test can look a screen line up to a
/// row with no separate geometry. The row index captured for a row's lines is
/// the value *before* `row_index` advances, matching `app::visible_rows()`:
/// both consume one [`VisibleRoster`], so ordinals stay 1:1 under capping,
/// expansion, and make-up filters. The caller skips a group the filter empties;
/// a finished pod the collapse empties still renders its header and toggle. The
/// more/less line is filter-suppressed because a narrowed body is already uncapped.
pub(in crate::sidebar_pane::render) fn worktree_group_lines_projected(
    ctx: &RowCtx<'_>,
    roster: &VisibleRoster<'_>,
    visible_group: &VisibleGroup<'_>,
    mut meter_pixels: Option<&mut MeterPixels>,
    lines: &mut Vec<Line<'static>>,
    map: &mut Vec<Option<usize>>,
    more_hits: &mut Vec<HitRegion>,
) {
    let group = visible_group.source();
    // Does the selection live in this worktree? If so the whole group reads as
    // one bracketed lane: the resting `▎` spine on the header and every row,
    // with the selected card itself lit bold `▌`. The `external` catch-all is
    // never a lane.
    let range = visible_group.range();
    let first_row = range.start;
    let passing = range.len();
    let group_selected = group.kind != SidebarWorktreeKind::External
        && (first_row..first_row + passing).contains(&ctx.selected_index);
    let lane = if group_selected {
        Gutter::Lane
    } else {
        Gutter::Blank
    };

    // The header carries the lane gutter when its worktree is selected (blank
    // otherwise), and its dotted `┄` seal shows only then, so an unselected
    // worktree is just its bold label. The `external` divider is full-bleed
    // chrome with a blank gutter.
    let header = group_header(ctx.theme, group, ctx.width, group_selected);
    if group.finished {
        more_hits.push(HitRegion::whole_line(
            lines.len(),
            HitTarget::ToggleGroup(group.key.clone()),
        ));
    }
    lines.push(with_gutter(ctx.theme, header, lane, None, ctx.width));
    // A finished worktree's name toggles its collapsed roster. A live worktree's
    // name lands on the first row — the adjacent agent — while the `external`
    // divider stays inert chrome.
    let header_target =
        (!group.finished && group.kind != SidebarWorktreeKind::External && passing > 0)
            .then_some(first_row);
    map.push(header_target);
    for (this_row, row) in range.zip(visible_group.rows(roster).iter().copied()) {
        let selected = this_row == ctx.selected_index;
        let gutter = if selected { Gutter::Selected } else { lane };
        let row_lines = row_lines(ctx, row, selected, gutter, meter_pixels.as_deref_mut());
        map.extend(std::iter::repeat_n(Some(this_row), row_lines.len()));
        lines.extend(row_lines);
    }
    let natural_hidden = visible_group.natural_hidden_count();
    let hidden = visible_group.hidden_count();
    if hidden > 0 && !visible_group.expanded() {
        more_hits.push(HitRegion::whole_line(
            lines.len(),
            HitTarget::ToggleGroup(group.key.clone()),
        ));
        let toggle = if group.finished {
            finished_roster_line(ctx, visible_group, roster)
        } else {
            Line::styled(format!("  +{hidden} more"), ctx.theme.muted())
        };
        lines.push(with_gutter(ctx.theme, toggle, lane, None, ctx.width));
        map.push(None);
    } else if natural_hidden > 0 && visible_group.expanded() {
        more_hits.push(HitRegion::whole_line(
            lines.len(),
            HitTarget::ToggleGroup(group.key.clone()),
        ));
        lines.push(with_gutter(
            ctx.theme,
            Line::styled("  − less", ctx.theme.muted()),
            lane,
            None,
            ctx.width,
        ));
        map.push(None);
    }
}

fn finished_roster_line(
    ctx: &RowCtx<'_>,
    visible_group: &VisibleGroup<'_>,
    roster: &VisibleRoster<'_>,
) -> Line<'static> {
    let group = visible_group.source();
    let visible_ids = visible_group
        .rows(roster)
        .iter()
        .map(|row| row.id.as_str())
        .collect::<HashSet<_>>();
    let hidden_rows = group
        .rows
        .iter()
        .filter(|row| !visible_ids.contains(row.id.as_str()))
        .collect::<Vec<_>>();
    let members = hidden_rows
        .iter()
        .filter_map(|row| row.status().map(|status| (*row, status)))
        .collect::<Vec<_>>();
    let process_count = hidden_rows.len().saturating_sub(members.len());

    let cost = group.rows.iter().filter_map(session_cost_usd).sum::<f64>();
    let cost_span = (cost >= 0.005)
        .then(|| Span::styled(dollars2(cost), ctx.theme.money_style(Modifier::empty())));
    let cost_width = cost_span.as_ref().map(Span::width).unwrap_or_default();
    let budget = content_width(ctx.width)
        .saturating_sub(2)
        .saturating_sub(cost_width + usize::from(cost_span.is_some()));

    let mut spans = vec![Span::raw("  ")];
    let mut roster_width = 0;
    let mut placed = 0;
    for (row, status) in &members {
        let glyph = status_glyph(ctx.theme, *status);
        let chip_width = text_width(&glyph) + 1 + text_width(row.display_name());
        let separator_width = 2 * usize::from(placed > 0);
        let remaining = members.len() - placed - 1 + process_count;
        let remainder_width = if remaining > 0 {
            text_width(&format!("  +{remaining}"))
        } else {
            0
        };
        if roster_width + separator_width + chip_width + remainder_width > budget {
            break;
        }
        if separator_width > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(glyph, status_rest_style(ctx.theme, *status)));
        spans.push(Span::styled(
            format!(" {}", row.display_name()),
            ctx.theme
                .body_brand(brand_tone(ctx.theme, ctx.providers, &row.name)),
        ));
        roster_width += separator_width + chip_width;
        placed += 1;
    }

    if placed == 0 {
        spans.push(Span::styled(
            format!("+{} done", hidden_rows.len()),
            ctx.theme.muted(),
        ));
    } else {
        let remaining = members.len() - placed + process_count;
        if remaining > 0 {
            spans.push(Span::styled(format!("  +{remaining}"), ctx.theme.muted()));
        }
    }

    if let Some(cost_span) = cost_span {
        let padding = content_width(ctx.width)
            .saturating_sub(spans_width(&spans) + cost_width)
            .max(1);
        spans.push(Span::raw(" ".repeat(padding)));
        spans.push(cost_span);
    }
    Line::from(spans)
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(in crate::sidebar_pane::render) fn worktree_group_lines(
    ctx: &RowCtx<'_>,
    group: &SidebarWorktreeGroup,
    expanded: bool,
    row_index: &mut usize,
    meter_pixels: Option<&mut MeterPixels>,
    lines: &mut Vec<Line<'static>>,
    map: &mut Vec<Option<usize>>,
    more_hits: &mut Vec<HitRegion>,
) {
    let roster = VisibleRoster::single(group, None, expanded, None);
    let base = *row_index;
    let map_start = map.len();
    worktree_group_lines_projected(
        ctx,
        &roster,
        &roster.groups()[0],
        meter_pixels,
        lines,
        map,
        more_hits,
    );
    if base > 0 {
        for ordinal in map[map_start..].iter_mut().flatten() {
            *ordinal += base;
        }
    }
    *row_index += roster.len();
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
    // here as a bold neutral heading — no inline `▌`, the spine carries the lane.
    // The header builds to the content width left after the gutter cell.
    let cw = content_width(width);
    // The worktree's PR identity follows its name in a steady link tone. Its git
    // story pins right: live local reconciling leads, then a PR verdict, then
    // the local trunk verdict; diverged/reconciling keeps the `⇡/⇣` commit delta
    // and `+/-` churn before the marker.
    // A marker-backed channel leads with the same fork/merge glyph as a
    // worktree pod and carries this same right-pinned story; only plain lanes
    // keep the `#` label.
    // The per-worktree status tally is gone: the cockpit owns the fleet
    // make-up and each row carries its own status glyph, so repeating it here
    // was noise. The label clips to whatever's left after the stats claim
    // their width, always leaving a cell so the header never shrinks to zero on
    // extreme narrowness.
    //
    // A non-repo room's root pod is name-only: a plain directory has no fork
    // and no git story, so it drops the `⑂` prefix and pins nothing right.
    let right = match group.kind {
        SidebarWorktreeKind::Root => Vec::new(),
        _ => group_git_spans(theme, group),
    };
    let right_width = spans_width(&right);
    let badge = group
        .pr_number
        .map(|number| format!(" #{number}"))
        .filter(|badge| cw.saturating_sub(right_width.saturating_add(1)) > text_width(badge));
    let badge_width = badge.as_deref().map(text_width).unwrap_or_default();
    let label_width = cw
        .saturating_sub(right_width.saturating_add(1).saturating_add(badge_width))
        .max(1);
    let label_with_prefix = match group.kind {
        SidebarWorktreeKind::Root => group.label.clone(),
        SidebarWorktreeKind::Channel if !group.worktree_backed => {
            format!("{} {}", theme.glyph(GlyphRole::ChannelHash), group.label)
        }
        _ => {
            let role = if group.trunk_sync == Some(WorktreeTrunkSync::Merged)
                || group.pr_state == Some(WorktreePrState::Merged)
            {
                GlyphRole::WorktreeMerge
            } else {
                GlyphRole::WorktreeBranch
            };
            format!("{} {}", theme.glyph(role), group.label)
        }
    };
    let left = ellipsize(&label_with_prefix, label_width);
    // The dotted `┄` seal caps only the *selected* worktree's header, so the lane
    // reads as one bracketed block; every other header is just its bold label and
    // right-pinned stats, with plain space filling the gap. Sized to land the line
    // exactly on the content width — a space frames the dotted run from the text
    // on each side it touches.
    let middle = cw.saturating_sub(text_width(&left) + badge_width + right_width);
    let fill = if sealed {
        match (right.is_empty(), middle) {
            (false, m) if m >= 2 => {
                format!(" {} ", theme.glyph(GlyphRole::WorktreeDotted).repeat(m - 2))
            }
            (true, m) if m >= 1 => {
                format!(" {}", theme.glyph(GlyphRole::WorktreeDotted).repeat(m - 1))
            }
            (_, m) => " ".repeat(m),
        }
    } else {
        " ".repeat(middle)
    };

    // The selected worktree's dotted `┄` seal wears the dim selection tone, so it
    // matches the lane spine and reads as the top of the same bracket; an
    // unselected header's gap is plain faint spaces.
    let fill_style = if sealed {
        theme.styled(Component::LaneSpine, Modifier::DIM)
    } else {
        theme.faint()
    };
    let mut spans = vec![Span::styled(
        left,
        theme.styled(Component::WorktreeHeader, Modifier::BOLD),
    )];
    if let Some(badge) = badge {
        spans.push(Span::styled(
            badge,
            theme.styled(Component::WorktreePrBadge, Modifier::empty()),
        ));
    }
    spans.push(Span::styled(fill, fill_style));
    spans.extend(right);
    Line::from(spans)
}

/// The header's right-pinned git cluster. A known PR verdict (merged/closed/open)
/// outranks the local trunk relationship, so a pristine or locally-landed
/// worktree still shows its forge state; a live local rebase/merge (`⟳`) stays
/// on top as the one actionable working-tree state. Diverged and reconciling
/// worktrees keep the numeric `⇡/⇣ +/-` stats before the marker; every other
/// state collapses to the marker alone. Worktree-backed channels share this
/// cluster and lead with the same fork/merge glyph as a worktree pod. Empty when
/// no git facts reached this group or the group is the trunk worktree itself.
fn group_git_spans(theme: &Theme, group: &SidebarWorktreeGroup) -> Vec<Span<'static>> {
    let Some(trunk) = group.trunk.as_deref() else {
        return plain_git_spans(theme, group);
    };
    let Some((role, component)) = trunk_marker(group) else {
        return plain_git_spans(theme, group);
    };
    // Diverged and reconciling worktrees carry the numeric work stats before the
    // marker; pristine, merged, and PR-clean worktrees collapse to the marker.
    if matches!(
        group.trunk_sync,
        Some(WorktreeTrunkSync::Diverged | WorktreeTrunkSync::Reconciling)
    ) {
        let mut spans = plain_git_spans(theme, group);
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.extend(trunk_glyph_spans(theme, role, trunk, component));
        return spans;
    }
    trunk_glyph_spans(theme, role, trunk, component)
}

fn plain_git_spans(theme: &Theme, group: &SidebarWorktreeGroup) -> Vec<Span<'static>> {
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

fn pr_state_marker(state: WorktreePrState) -> (GlyphRole, Component) {
    match state {
        WorktreePrState::Merged => (GlyphRole::WorktreeTrunkMerge, Component::WorktreeMerged),
        WorktreePrState::Closed => (GlyphRole::WorktreePrClosed, Component::WorktreePrClosed),
        WorktreePrState::Open => (GlyphRole::WorktreePrOpen, Component::WorktreePrOpen),
    }
}

/// The trunk marker glyph and tone, by descending priority: a live local
/// rebase/merge (`⟳`), then the forge PR verdict (merged `✓` / closed `✕` /
/// open `⊙`), then the local trunk relationship (merged `✓` / pristine `≡` /
/// diverged branch `⑂`). `None` for the trunk worktree itself (`trunk_sync`
/// `None`), whose header keeps the plain cluster.
fn trunk_marker(group: &SidebarWorktreeGroup) -> Option<(GlyphRole, Component)> {
    let sync = group.trunk_sync?;
    if sync == WorktreeTrunkSync::Reconciling {
        return Some((
            GlyphRole::WorktreeReconciling,
            Component::WorktreeReconciling,
        ));
    }
    if let Some(state) = group.pr_state {
        return Some(pr_state_marker(state));
    }
    Some(match sync {
        WorktreeTrunkSync::Merged => (GlyphRole::WorktreeTrunkMerge, Component::WorktreeMerged),
        WorktreeTrunkSync::Pristine => (GlyphRole::WorktreeTrunkEqual, Component::WorktreePristine),
        WorktreeTrunkSync::Diverged => (GlyphRole::WorktreeTrunkBranch, Component::BranchDelta),
        WorktreeTrunkSync::Reconciling => unreachable!("reconciling handled above"),
    })
}

/// The `external` catch-all (untethered scripts/CI and out-of-project shells)
/// renders as a dim `┄ external ┄┄┄` divider rather than a bold `▌` pod header.
/// It keeps an *attention-only* tally (`? n` / `! n`) so a waiting script ask
/// still surfaces; the calm counts stay with the cockpit.
fn external_divider(theme: &Theme, group: &SidebarWorktreeGroup, width: usize) -> Line<'static> {
    let cw = content_width(width);
    let tally = attention_tally(theme, &group.status_counts);
    let dotted = theme.glyph(GlyphRole::WorktreeDotted);
    let head = format!("{dotted} {} ", group.label);
    let tail = if tally.is_empty() {
        String::new()
    } else {
        format!(" {tally}")
    };
    let fill = cw
        .saturating_sub(text_width(&head) + text_width(&tail))
        .max(1);
    let mut spans = vec![
        Span::styled(head, theme.faint()),
        Span::styled(dotted.repeat(fill), theme.faint()),
    ];
    if !tally.is_empty() {
        spans.push(Span::styled(tail, theme.muted()));
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
