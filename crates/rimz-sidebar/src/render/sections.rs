//! Worktree-grouped sidebar composition. The snapshot owns grouping and
//! ordering; this module only maps the view-model to terminal lines.
//!
//! The renderer expresses one *design grammar* for every meter — context-%,
//! todo progress, diff stats — so the rows read as one polished card per
//! agent, not a stack of one-off widgets. See the
//! [grammar in docs/internals/sidebar.md](../../../docs/internals/sidebar.md).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rimz::feed::{AgentStatus, PermissionPosture};
use rimz::{SidebarRow, SidebarRowKind, SidebarStatusCount, SidebarWorktreeGroup};

use super::fmt::{age_short, clip, time_remaining};
use super::labels::{
    diff_spans, gauge_spans, posture_pill, posture_style, pulse_glyph, status_glyph, status_style,
    todo_spans, tokens_label,
};
use super::theme::Theme;

/// Shared gauge width for the inline ambient bar and the selected card's
/// labeled `ctx` bar — one length, so the meter reads the same whether the
/// card is selected or not.
const GAUGE_WIDTH: usize = 10;

/// Width band that drives the ambient row density.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tier {
    /// Identity + a bare gauge, no labels (~24 columns).
    L0,
    /// Default: line 1 cue + capability + ctx gauge + pulse (~30 columns).
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

pub(super) fn attention_line(
    theme: &Theme,
    groups: &[SidebarWorktreeGroup],
) -> Option<Line<'static>> {
    let waiting = status_total(groups, AgentStatus::Waiting);
    let failed = status_total(groups, AgentStatus::Failed);
    if waiting == 0 && failed == 0 {
        return None;
    }

    let mut spans = Vec::new();
    if waiting > 0 {
        spans.push(Span::styled(
            format!("{}{}", status_glyph(AgentStatus::Waiting), waiting),
            status_style(theme, AgentStatus::Waiting),
        ));
    }
    if failed > 0 {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("{}{}", status_glyph(AgentStatus::Failed), failed),
            status_style(theme, AgentStatus::Failed),
        ));
    }
    Some(Line::from(spans))
}

/// Dim getting-started hint for a healthy room with no agent or feed rows.
/// Shell/editor process rows can still be present; the renderer suppresses
/// this cue once an agent-like process or product row appears.
///
/// The cue names the *real* next step. Until hooks are wired, running
/// claude/codex registers nothing, so an un-wired room points at `rimz hooks
/// install`; once wired (`hooks_ready`), it invites launching an agent.
pub(super) fn first_run_hint_lines(theme: &Theme, hooks_ready: bool) -> Vec<Line<'static>> {
    let dim = theme.dim();
    let lines: [&str; 3] = if hooks_ready {
        ["no agents yet", "run claude or codex", "in a pane to begin"]
    } else {
        [
            "no agents yet",
            "install hooks:",
            "rimz hooks install claude",
        ]
    };
    lines
        .into_iter()
        .map(|text| Line::styled(text, dim))
        .collect()
}

pub(super) fn worktree_group_lines(
    theme: &Theme,
    group: &SidebarWorktreeGroup,
    width: usize,
    row_index: &mut usize,
    selected_index: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(group_header(theme, group, width));
    let tier = Tier::for_width(width);
    for row in &group.rows {
        let selected = *row_index == selected_index;
        *row_index += 1;
        lines.extend(row_lines(theme, row, width, tier, selected));
    }
    if group.hidden_count > 0 {
        lines.push(Line::styled(
            format!("  +{} more", group.hidden_count),
            theme.dim(),
        ));
    }
    lines
}

fn status_total(groups: &[SidebarWorktreeGroup], status: AgentStatus) -> usize {
    groups
        .iter()
        .flat_map(|group| &group.status_counts)
        .filter(|count| count.status == status)
        .map(|count| count.count)
        .sum()
}

fn group_header(theme: &Theme, group: &SidebarWorktreeGroup, width: usize) -> Line<'static> {
    let label = format!("▌{}", group.label);
    let tally = tally_text(&group.status_counts);
    let diff_text = group
        .diff_added
        .zip(group.diff_removed)
        .filter(|(added, removed)| *added + *removed > 0)
        .map(|(added, removed)| format!("+{added} -{removed}"));

    // Right-align tally, with diff sitting just left of it. The label is
    // clipped to whatever's left after both right-hand chunks claim their
    // width; clipping always leaves at least one cell so the header never
    // shrinks to zero on extreme narrowness.
    let right_text = match diff_text.as_deref() {
        Some(diff) if !tally.is_empty() => format!("{diff}  {tally}"),
        Some(diff) => diff.to_owned(),
        None => tally.clone(),
    };
    let right_width = right_text.chars().count();
    let label_width = width.saturating_sub(right_width + 1).max(1);
    let left = clip(&label, label_width);
    let padding = width
        .saturating_sub(left.chars().count() + right_width)
        .max(1);

    let mut spans = vec![
        Span::styled(left, theme.style(Color::Cyan, Modifier::BOLD)),
        Span::raw(" ".repeat(padding)),
    ];
    if diff_text.is_some() {
        let (added, removed) = (
            group.diff_added.unwrap_or(0),
            group.diff_removed.unwrap_or(0),
        );
        spans.extend(diff_spans(theme, added, removed));
        if !tally.is_empty() {
            spans.push(Span::raw("  "));
        }
    }
    if !tally.is_empty() {
        spans.push(Span::styled(tally, theme.dim()));
    }
    Line::from(spans)
}

fn tally_text(counts: &[SidebarStatusCount]) -> String {
    counts
        .iter()
        .map(|count| format!("{}{}", count.count, status_glyph(count.status)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn row_lines(
    theme: &Theme,
    row: &SidebarRow,
    width: usize,
    tier: Tier,
    selected: bool,
) -> Vec<Line<'static>> {
    let mut lines = vec![selected_line(row_line(theme, row, width), selected)];
    if row.row_kind == SidebarRowKind::Agent {
        if let Some(line) = capability_line(theme, row, tier, width, selected) {
            lines.push(selected_line(line, selected));
        }
        if selected {
            if let Some(line) = selected_meter_line(theme, row, width) {
                lines.push(selected_line(line, selected));
            }
            if let Some(line) = selected_diff_line(theme, row, width) {
                lines.push(selected_line(line, selected));
            }
        }
    }
    lines
}

fn row_line(theme: &Theme, row: &SidebarRow, width: usize) -> Line<'static> {
    if row.row_kind == SidebarRowKind::Process {
        return process_row_line(theme, row, width);
    }

    if let Some(resolver) = &row.resolver {
        let resolver_name = resolver
            .display_name
            .as_deref()
            .unwrap_or_else(|| resolver.resolver_id.as_str());
        let remaining = resolver
            .budget_until
            .map(time_remaining)
            .unwrap_or_else(|| "?".to_owned());
        return composed_row(
            theme,
            Span::styled("⟳", status_style(theme, AgentStatus::Waiting)),
            &row.name,
            &format!("{resolver_name} {remaining}"),
            row.last_activity,
            row,
            width,
        );
    }

    composed_row(
        theme,
        Span::styled(
            status_glyph(row.status.unwrap_or(AgentStatus::Idle)),
            status_style(theme, row.status.unwrap_or(AgentStatus::Idle)),
        ),
        &row.name,
        row.task.as_deref().unwrap_or("—"),
        row.last_activity,
        row,
        width,
    )
}

fn selected_line(line: Line<'static>, selected: bool) -> Line<'static> {
    if selected {
        return line.patch_style(Style::default().add_modifier(Modifier::REVERSED));
    }
    line
}

fn process_row_line(theme: &Theme, row: &SidebarRow, width: usize) -> Line<'static> {
    let dim = theme.dim();
    let label = clip(&row.name, width.saturating_sub(2).max(1));
    Line::from(vec![
        Span::styled("·", dim),
        Span::raw(" "),
        Span::styled(label, dim),
    ])
}

fn composed_row(
    theme: &Theme,
    lead: Span<'static>,
    name: &str,
    task: &str,
    last_activity: jiff::Timestamp,
    row: &SidebarRow,
    width: usize,
) -> Line<'static> {
    let age = age_short(last_activity);
    let pulse = (row.row_kind == SidebarRowKind::Agent
        && matches!(
            row.status,
            Some(AgentStatus::Running | AgentStatus::Waiting)
        ))
    .then(|| pulse_glyph(row.last_event_pulse));
    let lead_width = 2;
    let name_width = 7;
    let age_width = age.chars().count();
    let pulse_width = pulse.map(|_| 2).unwrap_or(0);
    let fixed = lead_width + name_width + 2 + age_width + pulse_width;
    let task_width = width.saturating_sub(fixed).max(1);
    let name = format!("{:<name_width$}", clip(name, name_width));
    let task = clip(task, task_width);
    let padding = width
        .saturating_sub(
            lead_width + name.chars().count() + 1 + task.chars().count() + age_width + pulse_width,
        )
        .max(1);

    let mut spans = vec![
        lead,
        Span::raw(" "),
        Span::raw(name),
        Span::raw(" "),
        Span::raw(task),
        Span::raw(" ".repeat(padding)),
        Span::styled(age, theme.dim()),
    ];
    if let Some(glyph) = pulse {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            glyph,
            theme.style(Color::Cyan, Modifier::empty()),
        ));
    }
    Line::from(spans)
}

fn capability_line(
    theme: &Theme,
    row: &SidebarRow,
    tier: Tier,
    width: usize,
    selected: bool,
) -> Option<Line<'static>> {
    let mut tokens: Vec<CapabilityToken> = Vec::new();
    if let Some(model) = row.model.as_deref().filter(|model| !model.is_empty()) {
        tokens.push(CapabilityToken::Dim(model.to_owned()));
    }
    if let Some(effort) = row.effort.as_deref().filter(|effort| !effort.is_empty()) {
        tokens.push(CapabilityToken::Dim(effort.to_owned()));
    }
    let posture = row.permission_posture.and_then(posture_pill);
    if let Some(posture_label) = posture {
        let posture = row
            .permission_posture
            .expect("posture is Some when its label is Some");
        tokens.push(CapabilityToken::Posture(posture, posture_label.to_owned()));
    }
    // The selected card renders the labeled `ctx …` meter on its own line, so
    // the ambient inline gauge and todo dots are suppressed here to avoid
    // showing the same reading twice.
    let has_inline_gauge =
        !selected && matches!(tier, Tier::L1 | Tier::L2) && row.context_pct.is_some();
    let has_inline_todo = !selected && tier == Tier::L2 && row.todo_total.unwrap_or(0) > 0;
    if tokens.is_empty() && !has_inline_gauge && !has_inline_todo {
        return None;
    }

    let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
    let mut printed_any = false;
    for token in tokens {
        if printed_any {
            spans.push(Span::styled(" · ", theme.dim()));
        }
        match token {
            CapabilityToken::Dim(value) => spans.push(Span::styled(value, theme.dim())),
            CapabilityToken::Posture(posture, value) => {
                spans.push(Span::styled(value, posture_style(theme, posture)));
            }
        }
        printed_any = true;
    }
    if has_inline_gauge {
        if printed_any {
            spans.push(Span::raw("  "));
        }
        let percent = row.context_pct.unwrap_or(0);
        spans.extend(gauge_spans(theme, percent, GAUGE_WIDTH));
        printed_any = true;
    }
    if has_inline_todo {
        let (done, total) = (row.todo_done.unwrap_or(0), row.todo_total.unwrap_or(0));
        spans.push(Span::raw("  "));
        spans.extend(todo_spans(theme, done, total));
        printed_any = true;
    }
    if !printed_any {
        return None;
    }
    Some(Line::from(trim_spans_to_width(spans, width)))
}

fn selected_meter_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    let has_ctx = row.context_pct.is_some();
    let has_todo = row.todo_total.unwrap_or(0) > 0;
    if !has_ctx && !has_todo {
        return None;
    }
    // Same gauge length as the ambient inline bar — the selected card just
    // adds the `ctx` label so the meter and its number read together.
    let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
    if has_ctx {
        spans.push(Span::styled("ctx ", theme.dim()));
        spans.extend(gauge_spans(
            theme,
            row.context_pct.unwrap_or(0),
            GAUGE_WIDTH,
        ));
    }
    if has_ctx && has_todo {
        spans.push(Span::raw("  "));
    }
    if has_todo {
        let (done, total) = (row.todo_done.unwrap_or(0), row.todo_total.unwrap_or(0));
        spans.extend(todo_spans(theme, done, total));
    }
    Some(Line::from(trim_spans_to_width(spans, width)))
}

fn selected_diff_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    let total = row.total_tokens?;
    let mut spans = vec![Span::raw("  ")];
    spans.push(tokens_label(theme, total));
    Some(Line::from(trim_spans_to_width(spans, width)))
}

enum CapabilityToken {
    Dim(String),
    Posture(PermissionPosture, String),
}

fn trim_spans_to_width(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let mut remaining = width;
    let mut trimmed = Vec::new();
    for span in spans {
        if remaining == 0 {
            break;
        }
        let span_width = span.content.chars().count();
        if span_width <= remaining {
            remaining -= span_width;
            trimmed.push(span);
            continue;
        }
        let content = span.content.chars().take(remaining).collect::<String>();
        if !content.is_empty() {
            trimmed.push(Span::styled(content, span.style));
        }
        break;
    }
    trimmed
}
