//! Worktree-grouped sidebar composition. The snapshot owns grouping and
//! ordering; this module only maps the view-model to terminal lines.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rimz::feed::{AgentMode, AgentStatus};
use rimz::{SidebarRow, SidebarRowKind, SidebarStatusCount, SidebarWorktreeGroup};

use super::fmt::{age_short, clip, time_remaining};
use super::labels::{mode_pill, mode_style, status_glyph, status_style};

pub(super) fn attention_line(groups: &[SidebarWorktreeGroup]) -> Option<Line<'static>> {
    let waiting = status_total(groups, AgentStatus::Waiting);
    let failed = status_total(groups, AgentStatus::Failed);
    if waiting == 0 && failed == 0 {
        return None;
    }

    let mut spans = Vec::new();
    if waiting > 0 {
        spans.push(Span::styled(
            format!("{}{}", status_glyph(AgentStatus::Waiting), waiting),
            status_style(AgentStatus::Waiting),
        ));
    }
    if failed > 0 {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            format!("{}{}", status_glyph(AgentStatus::Failed), failed),
            status_style(AgentStatus::Failed),
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
pub(super) fn first_run_hint_lines(hooks_ready: bool) -> Vec<Line<'static>> {
    let dim = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
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
    group: &SidebarWorktreeGroup,
    width: usize,
    row_index: &mut usize,
    selected_index: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(group_header(group, width));
    for row in &group.rows {
        let selected = *row_index == selected_index;
        *row_index += 1;
        lines.push(row_line(row, width, selected));
        if row.row_kind == SidebarRowKind::Agent
            && let Some(capability) = capability_line(row)
        {
            lines.push(capability);
        }
    }
    if group.hidden_count > 0 {
        lines.push(Line::styled(
            format!("  +{} more", group.hidden_count),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
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

fn group_header(group: &SidebarWorktreeGroup, width: usize) -> Line<'static> {
    let label = format!("▌{}", group.label);
    let tally = tally_text(&group.status_counts);
    let available = width.saturating_sub(tally.chars().count() + 1);
    let left = clip(&label, available.max(1));
    let padding = width
        .saturating_sub(left.chars().count() + tally.chars().count())
        .max(1);
    Line::from(vec![
        Span::styled(
            left,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(padding)),
        Span::styled(
            tally,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ])
}

fn tally_text(counts: &[SidebarStatusCount]) -> String {
    counts
        .iter()
        .map(|count| format!("{}{}", count.count, status_glyph(count.status)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn row_line(row: &SidebarRow, width: usize, selected: bool) -> Line<'static> {
    if row.row_kind == SidebarRowKind::Process {
        return selected_line(process_row_line(row, width), selected);
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
        return selected_line(
            composed_row(
                Span::styled("⟳", status_style(AgentStatus::Waiting)),
                &row.name,
                &format!("{resolver_name} {remaining}"),
                row.last_activity,
                width,
            ),
            selected,
        );
    }

    selected_line(
        composed_row(
            Span::styled(
                status_glyph(row.status.unwrap_or(AgentStatus::Idle)),
                status_style(row.status.unwrap_or(AgentStatus::Idle)),
            ),
            &row.name,
            row.task.as_deref().unwrap_or("—"),
            row.last_activity,
            width,
        ),
        selected,
    )
}

fn selected_line(line: Line<'static>, selected: bool) -> Line<'static> {
    if selected {
        return line.patch_style(Style::default().add_modifier(Modifier::REVERSED));
    }
    line
}

fn process_row_line(row: &SidebarRow, width: usize) -> Line<'static> {
    let dim = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::DIM);
    let label = clip(&row.name, width.saturating_sub(2).max(1));
    Line::from(vec![
        Span::styled("·", dim),
        Span::raw(" "),
        Span::styled(label, dim),
    ])
}

fn composed_row(
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
        Span::styled(
            age,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ),
    ])
}

fn capability_line(row: &SidebarRow) -> Option<Line<'static>> {
    let mut tokens = Vec::new();
    if let Some(model) = row.model.as_deref().filter(|model| !model.is_empty()) {
        tokens.push(CapabilityToken::Dim(model.to_owned()));
    }
    if let Some(effort) = row.effort.as_deref().filter(|effort| !effort.is_empty()) {
        tokens.push(CapabilityToken::Dim(effort.to_owned()));
    }
    if let Some(mode) = row.mode.and_then(mode_pill) {
        let token = if row.mode == Some(AgentMode::Bypass) {
            CapabilityToken::Mode(AgentMode::Bypass, mode.to_owned())
        } else {
            CapabilityToken::Dim(mode.to_owned())
        };
        tokens.push(token);
    }
    if tokens.is_empty() {
        return None;
    }

    let mut spans = vec![Span::raw("  ")];
    for (index, token) in tokens.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                " · ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ));
        }
        match token {
            CapabilityToken::Dim(value) => spans.push(Span::styled(
                value,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )),
            CapabilityToken::Mode(mode, value) => {
                spans.push(Span::styled(value, mode_style(mode)));
            }
        }
    }
    Some(Line::from(spans))
}

enum CapabilityToken {
    Dim(String),
    Mode(AgentMode, String),
}
