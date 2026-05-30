//! Worktree-grouped sidebar composition. The snapshot owns grouping and
//! ordering; this module only maps the view-model to terminal lines.
//!
//! The renderer expresses one *design grammar* for every meter — context-%,
//! todo progress, diff stats — so the rows read as one polished card per
//! agent, not a stack of one-off widgets. See the
//! [grammar in docs/internals/sidebar.md](../../../docs/internals/sidebar.md).

use jiff::Timestamp;
use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};
use rimz::agents::{AgentContext, RateLimitWindow};
use rimz::feed::{AgentStatus, PermissionPosture};
use rimz::{
    SidebarRow, SidebarRowKind, SidebarStatusCount, SidebarWorktreeGroup, SidebarWorktreeKind,
};

use super::fmt::{
    age_short, clip, dollars, duration_compact, duration_compact_minutes, model_label,
    time_remaining, tokens_short,
};
use super::labels::{
    ResourceKind, agent_glyph, agent_style, diff_spans, gauge_spans, posture_pill, posture_style,
    resolver_glyph, resource_bar_spans, segmented_gauge_spans, status_glyph, status_style,
    todo_spans, tokens_label,
};
use super::theme::Theme;

/// Glyph for the selected row's left accent bar; lives in a one-cell gutter
/// reserved on every row so selecting one never shifts the columns.
const SELECTION_BAR: &str = "▎";

/// Lead glyph for the remote-control host row — a sync mark that, with the
/// violet tone, sets it apart from an agent's `◌`/spinner and a process's `·`.
/// The shape alone carries the distinction under `NO_COLOR`.
const REMOTE_CONTROL_GLYPH: &str = "⇅";

/// Width left for a row's content after the selection gutter claims its cell.
fn content_width(width: usize) -> usize {
    width.saturating_sub(1).max(1)
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
    animation_phase: u64,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(group_header(theme, group, width));
    let tier = Tier::for_width(content_width(width));
    for row in &group.rows {
        let selected = *row_index == selected_index;
        *row_index += 1;
        lines.extend(row_lines(
            theme,
            row,
            width,
            tier,
            selected,
            animation_phase,
        ));
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
    // The catch-all is not a worktree — render it as a dim divider, not a bold
    // `▌` pod header, so out-of-project sessions read as "outside the project."
    if group.kind == SidebarWorktreeKind::Workspace {
        return workspace_divider(theme, group, width);
    }
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

/// The `workspace` catch-all (untethered scripts/CI and out-of-project shells)
/// renders as a dim `┄ external ┄┄┄` divider rather than a bold `▌` pod header.
/// The right-aligned tally is kept so a waiting script ask still surfaces.
fn workspace_divider(theme: &Theme, group: &SidebarWorktreeGroup, width: usize) -> Line<'static> {
    let tally = tally_text(&group.status_counts);
    let head = format!("┄ {} ", group.label);
    let tail = if tally.is_empty() {
        String::new()
    } else {
        format!(" {tally}")
    };
    let fill = width
        .saturating_sub(head.chars().count() + tail.chars().count())
        .max(1);
    let mut spans = vec![
        Span::styled(head, theme.dim()),
        Span::styled("┄".repeat(fill), theme.dim()),
    ];
    if !tally.is_empty() {
        spans.push(Span::styled(tail, theme.dim()));
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
    animation_phase: u64,
) -> Vec<Line<'static>> {
    let cw = content_width(width);
    // Selecting a row never reshapes its ambient lines: line 1, the capability
    // line, and the context bar render the same whether selected or not.
    // Selection only *adds* a detail block beneath them — usage windows and the
    // token split — so a calm row stays exactly as tall as its unselected self.
    let mut inner = vec![row_line(theme, row, cw, animation_phase)];
    if row.row_kind == SidebarRowKind::Agent {
        // L0 is too narrow for the capability labels; it keeps just identity
        // and the bar beneath it.
        if tier != Tier::L0
            && let Some(line) = capability_line(theme, row, tier, cw)
        {
            inner.push(line);
        }
        if let Some(line) = gauge_line(theme, row, cw) {
            inner.push(line);
        }
        if selected {
            inner.extend(detail_lines(theme, row, cw));
        }
    }
    inner
        .into_iter()
        .map(|line| with_gutter(theme, line, selected))
        .collect()
}

fn row_line(theme: &Theme, row: &SidebarRow, width: usize, animation_phase: u64) -> Line<'static> {
    if row.row_kind == SidebarRowKind::Process {
        return process_row_line(theme, row, width);
    }

    if row.row_kind == SidebarRowKind::RemoteControl {
        return remote_control_row_line(theme, row, width);
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
        // A resolver mid-flight is the one "waiting for an answer" motion: a
        // braille spinner while the resolver composes the decision, bounded by
        // its budget.
        return composed_row(
            theme,
            Span::styled(
                resolver_glyph(animation_phase),
                status_style(theme, AgentStatus::Waiting),
            ),
            &row.name,
            &format!("{resolver_name} {remaining}"),
            row.last_activity,
            width,
        );
    }

    let status = row.status.unwrap_or(AgentStatus::Idle);
    // The leading cell animates only when the agent is actively doing something:
    // a running agent fills (working) or sparkles (plan-mode thinking). A
    // waiting `?`, a failed/stalled `!`, idle `◌`, and success `✓` stay still —
    // attention markers must be scannable, not jittery.
    composed_row(
        theme,
        Span::styled(
            agent_glyph(status, row.plan_mode, animation_phase),
            agent_style(theme, status),
        ),
        &row.name,
        descriptor(row),
        row.last_activity,
        width,
    )
}

/// The line-1 descriptor: the user's session name when they set one (`--name` /
/// `/rename`), else the agent's task, else an em dash. The name is what a human
/// chose to call this session, so it reads better than the first-prompt task
/// when present.
fn descriptor(row: &SidebarRow) -> &str {
    ctx(row)
        .and_then(|context| context.session_name.as_deref())
        .filter(|name| !name.is_empty())
        .or(row.task.as_deref())
        .unwrap_or("—")
}

/// The session's statusline enrichment, when it published any.
fn ctx(row: &SidebarRow) -> Option<&AgentContext> {
    row.context.as_ref()
}

/// Model name preferred from the statusline (`Opus 4.8 (1M context)`) over the
/// coarser transcript scalar (`Opus`), then shortened for the row
/// (`Opus 4.8 (1M)`); never synthesized.
fn display_model(row: &SidebarRow) -> Option<String> {
    ctx(row)
        .and_then(|context| context.model_display_name.as_deref())
        .or(row.model.as_deref())
        .filter(|model| !model.is_empty())
        .map(model_label)
}

/// Reasoning effort preferred from the statusline over the transcript scalar.
fn display_effort(row: &SidebarRow) -> Option<&str> {
    ctx(row)
        .and_then(|context| context.effort.as_deref())
        .or(row.effort.as_deref())
        .filter(|effort| !effort.is_empty())
}

/// Prefix a row line with the one-cell selection gutter: an accent `▎` on the
/// selected row, a blank cell otherwise. Applied to every line of a row so the
/// bar spans the whole (possibly multi-line) card.
fn with_gutter(theme: &Theme, line: Line<'static>, selected: bool) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    if selected {
        spans.push(Span::styled(SELECTION_BAR, theme.selection()));
    } else {
        spans.push(Span::raw(" "));
    }
    spans.extend(line.spans);
    Line::from(spans)
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

/// The remote-control host: a calm, single violet line. It is ambient
/// infrastructure (the snapshot pins it to the bottom of its group), never a
/// status-bearing agent, so it carries no motion or meters.
fn remote_control_row_line(theme: &Theme, row: &SidebarRow, width: usize) -> Line<'static> {
    let label = clip(&row.name, width.saturating_sub(2).max(1));
    Line::from(vec![
        Span::styled(
            REMOTE_CONTROL_GLYPH,
            theme.style(Color::Magenta, Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(label, theme.style(Color::Magenta, Modifier::empty())),
    ])
}

fn composed_row(
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

fn capability_line(
    theme: &Theme,
    row: &SidebarRow,
    tier: Tier,
    width: usize,
) -> Option<Line<'static>> {
    let mut tokens: Vec<CapabilityToken> = Vec::new();
    if let Some(model) = display_model(row) {
        tokens.push(CapabilityToken::Dim(model));
    }
    if let Some(effort) = display_effort(row) {
        tokens.push(CapabilityToken::Dim(effort.to_owned()));
    }
    let posture = row.permission_posture.and_then(posture_pill);
    if let Some(posture_label) = posture {
        let posture = row
            .permission_posture
            .expect("posture is Some when its label is Some");
        tokens.push(CapabilityToken::Posture(posture, posture_label.to_owned()));
    }
    let has_inline_todo = tier == Tier::L2 && row.todo_total.unwrap_or(0) > 0;
    // Session cost pins to the right edge of the line (`$1.27`); the capability
    // tokens pack from the left and yield to it when the row is narrow.
    let cost = ctx(row)
        .and_then(|context| context.cost.as_ref())
        .and_then(|cost| cost.total_cost_usd)
        .map(dollars);
    if tokens.is_empty() && !has_inline_todo && cost.is_none() {
        return None;
    }

    let mut left: Vec<Span<'static>> = vec![Span::raw("  ")];
    let mut printed_any = false;
    for token in tokens {
        if printed_any {
            left.push(Span::styled(" · ", theme.dim()));
        }
        match token {
            CapabilityToken::Dim(value) => left.push(Span::styled(value, theme.dim())),
            CapabilityToken::Posture(posture, value) => {
                left.push(Span::styled(value, posture_style(theme, posture)));
            }
        }
        printed_any = true;
    }
    if has_inline_todo {
        if printed_any {
            left.push(Span::raw("  "));
        }
        let (done, total) = (row.todo_done.unwrap_or(0), row.todo_total.unwrap_or(0));
        left.extend(todo_spans(theme, done, total));
    }

    let Some(cost) = cost else {
        return Some(Line::from(trim_spans_to_width(left, width)));
    };
    // Trim the left chunk to leave room for the cost plus a one-cell gap, then
    // pad the gap so the cost sits flush right.
    let cost_width = cost.chars().count();
    let mut spans = trim_spans_to_width(left, width.saturating_sub(cost_width + 1));
    let padding = width
        .saturating_sub(spans_width(&spans) + cost_width)
        .max(1);
    spans.push(Span::raw(" ".repeat(padding)));
    // Cost reads in money-green — the one non-dim token on the line, so spend
    // stands out from the dim model/effort chrome around it.
    spans.push(Span::styled(
        cost,
        theme.style(Color::Green, Modifier::empty()),
    ));
    Some(Line::from(spans))
}

/// Centered context bar, drawn as its own thin line beneath the capability
/// row. It starts at the same indent as the model name it underlines and leaves
/// an equal gap at the trailing edge, so every agent's bar shares one left edge
/// and the bars line up across worktrees with no alignment bookkeeping. When the
/// statusline reports the per-message token breakdown the fill is split into
/// colored segments (cache writes / cache reads / fresh input) — the three add
/// up to exactly the used percentage — otherwise it is a single-color ramp.
fn gauge_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    let percent = gauge_percent(row)?;
    // Reserve the leading two columns again on the right so the bar sits
    // centered with matching gaps on both ends.
    let bar_width = width.saturating_sub(4);
    let mut spans = vec![Span::raw("  ")];
    match gauge_segments(row) {
        Some(segments) => spans.extend(segmented_gauge_spans(theme, &segments, percent, bar_width)),
        None => spans.extend(gauge_spans(theme, percent, bar_width)),
    }
    Some(Line::from(trim_spans_to_width(spans, width)))
}

/// The context bar's value: the statusline's authoritative `used_percentage`
/// when present, else the transcript-derived gauge.
fn gauge_percent(row: &SidebarRow) -> Option<u8> {
    ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.used_percentage)
        .or(row.context_pct)
}

/// The context bar's color segments, when the per-message breakdown is known,
/// left to right: cache writes (amber), cache reads (green), fresh `input`
/// (red). `None` when no breakdown was reported (a fresh session, post-compact,
/// or a non-Claude agent), so the bar falls back to a single-color ramp.
fn gauge_segments(row: &SidebarRow) -> Option<[(u64, Color); 3]> {
    let usage = ctx(row)?.tokens.as_ref()?.current_usage.as_ref()?;
    let input = usage.input_tokens.unwrap_or(0);
    let writes = usage.cache_creation_input_tokens.unwrap_or(0);
    let reads = usage.cache_read_input_tokens.unwrap_or(0);
    (input + writes + reads > 0).then_some([
        (writes, Color::Yellow),
        (reads, Color::Green),
        (input, Color::Red),
    ])
}

/// The detail block selection reveals beneath the ambient lines: the token
/// split, then the two usage windows as draining stamina/mana bars. The token
/// totals lead — the per-session number reads first, then the budget meters it
/// draws down. Falls back to a bare token total for an agent that published no
/// statusline context.
fn detail_lines(theme: &Theme, row: &SidebarRow, width: usize) -> Vec<Line<'static>> {
    let Some(context) = ctx(row) else {
        return tokens_line(theme, row, width).into_iter().collect();
    };
    let mut lines = Vec::new();
    if let Some(line) = token_split_line(theme, context, width) {
        lines.push(line);
    } else if let Some(line) = tokens_line(theme, row, width) {
        lines.push(line);
    }
    if let Some(limits) = context.rate_limits.as_ref() {
        // The 5-hour reset floors to minutes (seconds are noise on it); the
        // weekly reset keeps the full d/h resolution.
        if let Some(line) = usage_window_line(
            theme,
            "5h",
            limits.five_hour.as_ref(),
            ResourceKind::Stamina,
            duration_compact_minutes,
            width,
        ) {
            lines.push(line);
        }
        if let Some(line) = usage_window_line(
            theme,
            "7d",
            limits.seven_day.as_ref(),
            ResourceKind::Mana,
            duration_compact,
            width,
        ) {
            lines.push(line);
        }
    }
    lines
}

/// Column width reserved for a usage-window label, so the two bars share one
/// left edge. The `5h`/`7d` labels both fit in two cells.
const WINDOW_LABEL_WIDTH: usize = 2;

/// One usage-limit window as a draining resource bar with a reset countdown:
/// `5h ▰▰▰▰▰▰▰▱▱▱ ↻ 3h12m`. The bar fills with budget *remaining*
/// (`100 - used`) so a full bar means headroom; `reset_fmt` renders the
/// countdown beside it — the 5-hour window floors to minutes, the weekly keeps
/// full resolution. Drawn only when the window reported a usage percentage.
fn usage_window_line(
    theme: &Theme,
    label: &str,
    window: Option<&RateLimitWindow>,
    kind: ResourceKind,
    reset_fmt: fn(Timestamp) -> String,
    width: usize,
) -> Option<Line<'static>> {
    let window = window?;
    let remaining = 100 - window.used_percentage?;
    let reset = window
        .resets_at
        .map(|at| format!("{RESET_GLYPH} {}", reset_fmt(at)));
    let reset_width = reset.as_ref().map_or(0, |text| text.chars().count());
    // "  " indent + label column + " " + bar + (" " + reset).
    let tail = if reset_width > 0 { reset_width + 1 } else { 0 };
    let bar_width = width
        .saturating_sub(2 + WINDOW_LABEL_WIDTH + 1 + tail)
        .max(1);
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(format!("{label:<WINDOW_LABEL_WIDTH$}"), theme.dim()),
        Span::raw(" "),
    ];
    spans.extend(resource_bar_spans(theme, remaining, kind, bar_width));
    if let Some(reset) = reset {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(reset, theme.dim()));
    }
    Some(Line::from(trim_spans_to_width(spans, width)))
}

/// The selected row's token split: `76.5k tok · ↑64.2k ↓12.3k` — current-context
/// total, then input and output. Drawn from the statusline token usage.
fn token_split_line(theme: &Theme, context: &AgentContext, width: usize) -> Option<Line<'static>> {
    let tokens = context.tokens.as_ref()?;
    let input = tokens.total_input_tokens;
    let output = tokens.total_output_tokens;
    if input.is_none() && output.is_none() {
        return None;
    }
    let (input, output) = (input.unwrap_or(0), output.unwrap_or(0));
    let spans = vec![
        Span::raw("  "),
        tokens_label(theme, input + output),
        Span::styled(" · ", theme.dim()),
        Span::styled(format!("↑{}", tokens_short(input)), theme.dim()),
        Span::raw(" "),
        Span::styled(format!("↓{}", tokens_short(output)), theme.dim()),
    ];
    Some(Line::from(trim_spans_to_width(spans, width)))
}

/// The bare token total, shown when the context carries no read-only token
/// usage (Codex, whose app-server exposes none) so selection still reveals
/// *something* for every agent.
fn tokens_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    let total = row.total_tokens?;
    let spans = vec![Span::raw("  "), tokens_label(theme, total)];
    Some(Line::from(trim_spans_to_width(spans, width)))
}

/// Reset marker beside a usage-window countdown — a single-width refresh arrow.
const RESET_GLYPH: char = '↻';

/// Total display width of a span run, in terminal cells.
fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|span| span.content.chars().count()).sum()
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
