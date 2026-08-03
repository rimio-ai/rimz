//! Worktree group composition: the bold pod header with its linked-PR identity
//! and right-pinned git story, the dim `external` divider, and the row roster
//! with its parallel hit-test map entries. Finished multi-row pods collapse
//! hidden agents into a two-line receipt: an expandable team/member roster with
//! cost pinned right, then token totals and cache health with the finished age
//! pinned right.

use std::collections::HashSet;

use crate::config::GlyphRole;
use crate::store::snapshot::{
    SidebarStatusCount, SidebarWorktreeGroup, SidebarWorktreeKind, WorktreePrCi, WorktreePrState,
    WorktreeTrunkSync,
};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::sidebar_pane::pixel::meter::MeterPixels;
use crate::sidebar_pane::render::fmt::{activity_short, age_label, age_secs, dollars2, tokens_int};
use crate::sidebar_pane::render::labels::{
    TokenColumns, TokenDetail, branch_delta_spans, diff_spans, elapsed_glyph, status_glyph,
    status_rest_style, token_breakdown_spans, token_total_glyph, trunk_glyph_spans,
};
use crate::sidebar_pane::render::layout::{ellipsize, spans_width, text_width};
use crate::sidebar_pane::render::theme::{Component, Theme};
use crate::sidebar_pane::render::{HitTarget, RenderedBlock};
use crate::sidebar_pane::view::{VisibleGroup, VisibleRoster};

use super::agent_card::row_lines;
use super::{Gutter, RowCtx, content_width, pin_right, with_gutter};

/// Inputs needed to render one projected worktree group.
pub(in crate::sidebar_pane::render) struct WorktreeRenderContext<'render, 'snapshot> {
    pub(in crate::sidebar_pane::render) row: &'render RowCtx<'snapshot>,
    pub(in crate::sidebar_pane::render) roster: &'render VisibleRoster<'snapshot>,
    pub(in crate::sidebar_pane::render) group: &'render VisibleGroup<'snapshot>,
    pub(in crate::sidebar_pane::render) meter_pixels: Option<&'render mut MeterPixels>,
}

enum WorktreeTail {
    None,
    More {
        line: Line<'static>,
        totals: Option<Line<'static>>,
    },
    Less(Line<'static>),
}

/// Compose one worktree group's lines and interaction geometry together.
/// The row index captured for a row's lines matches `app::visible_rows()`:
/// both consume one [`VisibleRoster`], so ordinals stay 1:1 under capping,
/// expansion, and make-up filters. The caller skips a group the filter empties;
/// a finished multi-agent pod the collapse empties still renders its header and
/// two-line receipt when totals exist. The live more/less line is
/// filter-suppressed because a narrowed body is already uncapped.
pub(in crate::sidebar_pane::render) fn worktree_group_lines_projected(
    render: WorktreeRenderContext<'_, '_>,
) -> RenderedBlock {
    let WorktreeRenderContext {
        row: ctx,
        roster,
        group: visible_group,
        mut meter_pixels,
    } = render;
    let mut block = RenderedBlock::default();
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
    let (header, header_link) = group_header(ctx.theme, group, ctx.width, group_selected);
    let collapses = group.collapses();
    let header_hit = collapses.then(|| (0..u16::MAX, HitTarget::ToggleGroup(group.key.clone())));
    let header_link = header_link.map(|(columns, url)| {
        (
            columns.start.saturating_add(1)..columns.end.saturating_add(1),
            HitTarget::Hyperlink(url),
        )
    });
    // A collapsing worktree's name toggles its roster. Every other worktree's
    // name lands on the first row — the adjacent agent — while the `external`
    // divider stays inert chrome.
    let header_target = (!collapses && group.kind != SidebarWorktreeKind::External && passing > 0)
        .then_some(first_row);
    block.push_with_regions(
        with_gutter(ctx.theme, header, lane, None, ctx.width),
        header_target,
        header_hit.into_iter().chain(header_link),
    );
    for (this_row, row) in range.zip(visible_group.rows(roster).iter().copied()) {
        let selected = this_row == ctx.selected_index;
        let expanded =
            super::row_expanded_by_selection(roster, visible_group, this_row, ctx.selected_index);
        let gutter = if selected { Gutter::Selected } else { lane };
        let cost_usd = super::agent_card::agent_card_cost_usd(group, row);
        for line in row_lines(
            ctx,
            row,
            selected,
            expanded,
            gutter,
            cost_usd,
            meter_pixels.as_deref_mut(),
        ) {
            block.push_row(line, this_row);
        }
    }
    let target = HitTarget::ToggleGroup(group.key.clone());
    match worktree_tail(ctx, roster, visible_group) {
        WorktreeTail::More { line, totals } => {
            block.push_target(
                with_gutter(ctx.theme, line, lane, None, ctx.width),
                target.clone(),
            );
            if let Some(totals) = totals {
                block.push_target(
                    with_gutter(ctx.theme, totals, lane, None, ctx.width),
                    target,
                );
            }
        }
        WorktreeTail::Less(line) => {
            block.push_target(with_gutter(ctx.theme, line, lane, None, ctx.width), target)
        }
        WorktreeTail::None => {}
    }
    block
}

fn worktree_tail(
    ctx: &RowCtx<'_>,
    roster: &VisibleRoster<'_>,
    visible_group: &VisibleGroup<'_>,
) -> WorktreeTail {
    let group = visible_group.source();
    let collapses = group.collapses();
    let hidden = visible_group.hidden_count();
    if hidden > 0 && !visible_group.expanded() {
        return WorktreeTail::More {
            line: if collapses {
                finished_roster_line(ctx, visible_group, roster)
            } else {
                Line::styled(format!("  +{hidden} more"), ctx.theme.muted())
            },
            totals: collapses
                .then(|| finished_totals_line(ctx, group))
                .flatten(),
        };
    }
    if visible_group.natural_hidden_count() > 0 && visible_group.expanded() && !collapses {
        WorktreeTail::Less(Line::styled("  − less", ctx.theme.muted()))
    } else {
        WorktreeTail::None
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

    let team = group.team.as_deref();
    let cost_spans = if let Some(cost) = group
        .cohort_effort
        .as_ref()
        .and_then(|effort| effort.cost_usd)
        .filter(|cost| *cost >= 0.005)
    {
        vec![Span::styled(
            dollars2(cost),
            ctx.theme.money_style(Modifier::empty()),
        )]
    } else {
        Vec::new()
    };
    let mut spans = vec![
        Span::styled(
            ctx.theme.glyph(GlyphRole::WorktreeExpand).to_owned(),
            ctx.theme.muted(),
        ),
        Span::raw(" "),
    ];
    if let Some(team) = team {
        spans.push(Span::styled(
            team.to_owned(),
            ctx.theme.muted().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
    }
    let width = content_width(ctx.width);
    let budget =
        width.saturating_sub(spans_width(&cost_spans) + usize::from(!cost_spans.is_empty()));
    let mut roster_width = spans_width(&spans);
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
                .body_brand(ctx.theme.provider_brand_tone(&row.name)),
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

    pin_right(spans, cost_spans, width)
}

fn finished_totals_line(ctx: &RowCtx<'_>, group: &SidebarWorktreeGroup) -> Option<Line<'static>> {
    let max_last_activity = group
        .rows
        .iter()
        .filter(|row| row.is_agent())
        .map(|row| row.last_activity)
        .max()?;
    let (tokens, active_secs) = group.cohort_effort.as_ref().map_or(
        (crate::agents::spending::EffortTokens::default(), None),
        |effort| (effort.tokens, effort.active_secs),
    );
    let total = tokens.display_total();
    let input = tokens.input.saturating_add(tokens.cache_write);
    let output = tokens.output;
    let cache_read = tokens.cache_read;
    let right = active_secs.map_or_else(
        || {
            activity_short(max_last_activity, ctx.now).map_or_else(Vec::new, |label| {
                let seconds = age_secs(max_last_activity, ctx.now);
                vec![Span::styled(
                    format!("{} {label}", elapsed_glyph(ctx.theme, seconds)),
                    ctx.theme.muted(),
                )]
            })
        },
        |active_secs| {
            let seconds = i64::try_from(active_secs).unwrap_or(i64::MAX);
            vec![Span::styled(
                format!(
                    "{} {}",
                    elapsed_glyph(ctx.theme, seconds),
                    age_label(seconds)
                ),
                ctx.theme.muted(),
            )]
        },
    );
    if total == 0 && right.is_empty() {
        return None;
    }

    let cache_hit = (total > 0)
        .then(|| crate::agents::context::cache_hit_percent(cache_read, input))
        .flatten()
        .map_or_else(Vec::new, |percent| {
            let style = match crate::agents::CacheHealth::classify(percent) {
                crate::agents::CacheHealth::Good => ctx.theme.good(Modifier::empty()),
                crate::agents::CacheHealth::Caution => ctx.theme.warn(Modifier::empty()),
                crate::agents::CacheHealth::Alarm => ctx.theme.alarm(Modifier::empty()),
            };
            vec![
                Span::styled(" · ", ctx.theme.muted()),
                Span::styled(format!("{percent}%"), style),
            ]
        });
    let width = content_width(ctx.width);
    let left_budget = width
        .saturating_sub(spans_width(&right) + usize::from(!right.is_empty()))
        .saturating_sub(2)
        .saturating_sub(spans_width(&cache_hit));
    let mut left = vec![Span::raw("  ")];
    if total > 0 {
        let full = token_breakdown_spans(
            ctx.theme,
            total,
            input,
            output,
            cache_read,
            tokens_int,
            TokenDetail::Full,
            &TokenColumns::default(),
        );
        let summary = token_breakdown_spans(
            ctx.theme,
            total,
            input,
            output,
            cache_read,
            tokens_int,
            TokenDetail::Summary,
            &TokenColumns::default(),
        );
        if spans_width(&full) <= left_budget {
            left.extend(full);
        } else if spans_width(&summary) <= left_budget {
            left.extend(summary);
        } else {
            left.extend([
                Span::styled(
                    token_total_glyph(ctx.theme),
                    ctx.theme.styled(Component::TokenTotal, Modifier::empty()),
                ),
                Span::styled(format!(" {}", tokens_int(total)), ctx.theme.body()),
            ]);
        }
        left.extend(cache_hit);
    }
    Some(pin_right(left, right, width))
}

fn group_header(
    theme: &Theme,
    group: &SidebarWorktreeGroup,
    width: usize,
    sealed: bool,
) -> (Line<'static>, Option<(std::ops::Range<u16>, String)>) {
    // The catch-all is not a worktree — render it as a dim divider, not a bold
    // pod header, so out-of-project sessions read as "outside the project."
    if group.kind == SidebarWorktreeKind::External {
        return (external_divider(theme, group, width), None);
    }
    // The lane spine (added by the caller) opens the header, so the label leads
    // here as a bold neutral heading — no inline `▌`, the spine carries the lane.
    // The header builds to the content width left after the gutter cell.
    let cw = content_width(width);
    // The worktree's branch/PR CI marker follows the name, then any linked PR
    // number in a steady link tone. Its git story pins right: live local
    // reconciling leads, then a PR verdict, then
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
    let ci = (group.pr_state.is_none()
        || matches!(
            group.pr_state,
            Some(WorktreePrState::Open | WorktreePrState::Merged)
        ))
    .then_some(group.pr_ci)
    .flatten()
    .map(|ci| {
        let (role, component) = pr_ci_marker(ci);
        (format!(" {}", theme.glyph(role)), component)
    });
    let badge = group.pr_number.map(|number| format!(" #{number}"));
    let identity_width = badge
        .as_ref()
        .map(|badge| text_width(badge))
        .unwrap_or_default()
        + ci.as_ref()
            .map(|(glyph, _)| text_width(glyph))
            .unwrap_or_default();
    let (ci, badge, identity_width) =
        if cw.saturating_sub(right_width.saturating_add(1)) > identity_width {
            (ci, badge, identity_width)
        } else {
            (None, None, 0)
        };
    let label_width = cw
        .saturating_sub(right_width.saturating_add(1).saturating_add(identity_width))
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
    let team_suffix = group
        .team
        .as_deref()
        .filter(|team| !group.label.ends_with(&format!("/{team}")))
        .map(|team| format!(" · {team}"));
    let qualifier_suffix = group
        .label_qualifier
        .as_deref()
        .map(|qualifier| format!(" · {qualifier}"));
    let full_label = format!(
        "{label_with_prefix}{}{}",
        qualifier_suffix.as_deref().unwrap_or_default(),
        team_suffix.as_deref().unwrap_or_default()
    );
    let left = ellipsize(&full_label, label_width);
    let left_width = text_width(&left);
    let hyperlink = badge.as_ref().and_then(|badge| {
        let url = group.pr_url.as_ref()?;
        let badge_text = badge.trim_start();
        let badge_lead = text_width(badge).saturating_sub(text_width(badge_text));
        let ci_width = ci
            .as_ref()
            .map(|(glyph, _)| text_width(glyph))
            .unwrap_or_default();
        let start = left_width
            .saturating_add(ci_width)
            .saturating_add(badge_lead);
        let end = start.saturating_add(text_width(badge_text));
        Some((
            u16::try_from(start).ok()?..u16::try_from(end).ok()?,
            url.clone(),
        ))
    });
    // The dotted `┄` seal caps only the *selected* worktree's header, so the lane
    // reads as one bracketed block; every other header is just its bold label and
    // right-pinned stats, with plain space filling the gap. Sized to land the line
    // exactly on the content width — a space frames the dotted run from the text
    // on each side it touches.
    let middle = cw.saturating_sub(left_width + identity_width + right_width);
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
    let label_style = if group.finished {
        theme.muted().add_modifier(Modifier::BOLD)
    } else {
        theme.styled(Component::WorktreeHeader, Modifier::BOLD)
    };
    let mut spans = if left == full_label {
        let mut spans = vec![Span::styled(label_with_prefix, label_style)];
        if let Some(suffix) = qualifier_suffix {
            spans.push(Span::styled(
                suffix,
                theme.styled(Component::WorktreeQualifier, Modifier::empty()),
            ));
        }
        if let Some(suffix) = team_suffix {
            spans.push(Span::styled(
                suffix,
                theme.styled(Component::TeamLabel, Modifier::empty()),
            ));
        }
        spans
    } else {
        vec![Span::styled(left, label_style)]
    };
    if let Some((glyph, component)) = ci {
        spans.push(Span::styled(
            glyph,
            theme.styled(component, Modifier::empty()),
        ));
    }
    if let Some(badge) = badge {
        spans.push(Span::styled(
            badge,
            theme.styled(Component::WorktreePrBadge, Modifier::empty()),
        ));
    }
    spans.push(Span::styled(fill, fill_style));
    spans.extend(right);
    (Line::from(spans), hyperlink)
}

/// The header's right-pinned git cluster. A known PR verdict (merged/closed/open)
/// outranks the local trunk relationship, so a pristine or locally-landed
/// worktree still shows its forge state; a live local rebase/merge (`⟳`) stays
/// on top as the one actionable working-tree state. Diverged and reconciling
/// worktrees keep the numeric `⇡/⇣ +/-` stats before the marker, except that a
/// merged PR collapses spent stats to its marker alone; every other state also
/// collapses to the marker. Worktree-backed channels share this cluster and lead
/// with the same fork/merge glyph as a worktree pod. Empty when no git facts
/// reached this group or the group is the trunk worktree itself.
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
    ) && component != Component::WorktreeMerged
    {
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

fn pr_ci_marker(ci: WorktreePrCi) -> (GlyphRole, Component) {
    match ci {
        WorktreePrCi::Pending => (GlyphRole::WorktreeCiPending, Component::PrCiPending),
        WorktreePrCi::Passing => (GlyphRole::WorktreeCiPassing, Component::PrCiPassing),
        WorktreePrCi::Failing => (GlyphRole::WorktreeCiFailing, Component::PrCiFailing),
    }
}

/// The trunk marker glyph and tone, by descending priority: a live local
/// rebase/merge (`⟳`), then the forge PR verdict (merged `✓` / closed `✕` /
/// open `⑃`), then the local trunk relationship (merged `✓` / pristine `≡` /
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
