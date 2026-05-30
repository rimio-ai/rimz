//! Worktree-grouped sidebar composition. The snapshot owns grouping and
//! ordering; this module only maps the view-model to terminal lines.
//!
//! The renderer expresses one *design grammar* for every meter — context-%,
//! todo progress, diff stats — so the rows read as one polished card per
//! agent, not a stack of one-off widgets. See the
//! [grammar in docs/internals/sidebar.md](../../../docs/internals/sidebar.md).

use jiff::Timestamp;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rimz::agents::{AgentContext, RateLimitWindow};
use rimz::config::SidebarDensity;
use rimz::feed::AgentStatus;
use rimz::{
    SidebarRow, SidebarRowKind, SidebarStatusCount, SidebarWorktreeGroup, SidebarWorktreeKind,
};

use super::fmt::{
    age_short, clip, dollars, duration_compact, duration_compact_minutes, duration_worked,
    model_label, pct_label, time_remaining, tokens_short,
};
use super::labels::{
    agent_glyph, agent_style, diff_spans, gauge_spans, posture_pill, posture_style, resolver_glyph,
    resource_bar_spans, segmented_gauge_spans, status_glyph, status_style, thinking_still,
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

/// The fixed, always-present fleet header: the agent total and a glyph+count for
/// each non-empty status bucket — `6 agents  ⢿3 ✽1 ?2`. Always exactly one line
/// (trimmed to width, never wrapped), so the body below never shifts vertically
/// as agents appear, clear, or change state. `working`/`thinking` split the
/// `running` total by plan mode; the split reads plan mode off the visible rows,
/// so a `running` agent hidden by the per-worktree cap folds into `working` (the
/// calm tail is rarely thinking). Counts come from `status_counts`, which spans
/// even capped-away agents, so the aggregate is never lost.
pub(super) fn fleet_stats_line(
    theme: &Theme,
    groups: &[SidebarWorktreeGroup],
    width: usize,
) -> Line<'static> {
    let running = status_total(groups, AgentStatus::Running);
    let thinking = groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.status == Some(AgentStatus::Running) && row.plan_mode)
        .count();
    let working = running.saturating_sub(thinking);
    // Order: the busy/attention states first, then the calm tail.
    let buckets: [(usize, &'static str, Style); 6] = [
        (
            working,
            status_glyph(AgentStatus::Running),
            agent_style(theme, AgentStatus::Running),
        ),
        (
            thinking,
            thinking_still(),
            agent_style(theme, AgentStatus::Running),
        ),
        (
            status_total(groups, AgentStatus::Waiting),
            status_glyph(AgentStatus::Waiting),
            status_style(theme, AgentStatus::Waiting),
        ),
        (
            status_total(groups, AgentStatus::Failed),
            status_glyph(AgentStatus::Failed),
            status_style(theme, AgentStatus::Failed),
        ),
        (
            status_total(groups, AgentStatus::Idle),
            status_glyph(AgentStatus::Idle),
            status_style(theme, AgentStatus::Idle),
        ),
        (
            status_total(groups, AgentStatus::Success),
            status_glyph(AgentStatus::Success),
            status_style(theme, AgentStatus::Success),
        ),
    ];
    let total: usize = buckets.iter().map(|(count, _, _)| count).sum();
    let unit = if total == 1 { "agent" } else { "agents" };
    let mut spans = vec![Span::styled(format!("{total} {unit}"), theme.dim())];
    let mut printed = false;
    for (count, glyph, style) in buckets {
        if count == 0 {
            continue;
        }
        spans.push(Span::raw(if printed { " " } else { "  " }));
        spans.push(Span::styled(format!("{glyph}{count}"), style));
        printed = true;
    }
    Line::from(trim_spans_to_width(spans, width))
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
    density: SidebarDensity,
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
            density,
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
    density: SidebarDensity,
    selected: bool,
    animation_phase: u64,
) -> Vec<Line<'static>> {
    let cw = content_width(width);
    // The resting (unselected) card is line 1 (identity), line 2 (description),
    // and the ctx bar — plus whatever `density` keeps resident. Selection only
    // *appends* the deeper lines (the 5h/7d budget bars, then the token and work
    // stats); it never reshapes a line already on screen, so the card never
    // reflows on expand, in any density.
    let mut inner = vec![identity_line(theme, row, tier, cw, animation_phase)];
    if row.row_kind == SidebarRowKind::Agent {
        inner.push(description_line(theme, row, tier, cw));
        if let Some(line) = gauge_line(theme, row, cw) {
            inner.push(line);
        }
        let show_bars = selected || density.shows_budget_bars();
        let show_stats = selected || density.shows_stats();
        if show_bars {
            inner.extend(budget_bar_lines(theme, row, cw));
        }
        if show_stats {
            if let Some(line) = token_totals_line(theme, row, cw) {
                inner.push(line);
            }
            if let Some(line) = work_line(theme, row, cw) {
                inner.push(line);
            }
        }
        // Sub-agent list: designed but deferred — the rollup carries no
        // parent→child link yet (see docs/internals/sidebar.md, "sub-agents").
    }
    inner
        .into_iter()
        .map(|line| with_gutter(theme, line, selected))
        .collect()
}

/// Width budget for the agent name on line 1: short agent kinds (`claude`,
/// `codex`) fit comfortably, and a longer name clips with `…` rather than
/// pushing the model/effort tokens off the line.
const NAME_MAX: usize = 12;

fn identity_line(
    theme: &Theme,
    row: &SidebarRow,
    tier: Tier,
    width: usize,
    animation_phase: u64,
) -> Line<'static> {
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
        // its budget. The resolver + budget fill the slot a task would.
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
    agent_identity_line(theme, row, status, tier, width, animation_phase)
}

/// Line 1 for an agent: the leading cell (animated only while the agent is
/// actively working or plan-mode thinking — attention markers stay still), the
/// agent name, then the dim capability tokens (`· model · effort`) and the
/// permission posture pill, with `$cost` (money-green) and the activity age
/// pinned right. Capability tokens degrade by width tier: L2 carries model +
/// effort + posture, L1 drops effort, L0 keeps just the name — cost and age
/// always pin right.
fn agent_identity_line(
    theme: &Theme,
    row: &SidebarRow,
    status: AgentStatus,
    tier: Tier,
    width: usize,
    animation_phase: u64,
) -> Line<'static> {
    // Right cluster, built first so the left trims to whatever's left: the
    // session cost in money-green, then the dim activity age.
    let mut right: Vec<Span<'static>> = Vec::new();
    if let Some(cost) = ctx(row)
        .and_then(|context| context.cost.as_ref())
        .and_then(|cost| cost.total_cost_usd)
        .map(dollars)
    {
        right.push(Span::styled(
            cost,
            theme.style(Color::Green, Modifier::empty()),
        ));
        right.push(Span::raw("  "));
    }
    right.push(Span::styled(age_short(row.last_activity), theme.dim()));

    // Left cluster: glyph + name + dim capability tokens.
    let mut left: Vec<Span<'static>> = vec![
        Span::styled(
            agent_glyph(status, row.plan_mode, animation_phase),
            agent_style(theme, status),
        ),
        Span::raw(" "),
        Span::raw(clip(&row.name, NAME_MAX)),
    ];
    if tier != Tier::L0 {
        if let Some(model) = display_model(row) {
            left.push(Span::styled(" · ", theme.dim()));
            left.push(Span::styled(model, theme.dim()));
        }
        if tier == Tier::L2
            && let Some(effort) = display_effort(row)
        {
            left.push(Span::styled(" · ", theme.dim()));
            left.push(Span::styled(effort.to_owned(), theme.dim()));
        }
        if let Some(posture_label) = row.permission_posture.and_then(posture_pill) {
            let posture = row
                .permission_posture
                .expect("posture is Some when its label is Some");
            left.push(Span::styled(" · ", theme.dim()));
            left.push(Span::styled(
                posture_label.to_owned(),
                posture_style(theme, posture),
            ));
        }
    }
    pin_right(left, right, width)
}

/// Line 2 for an agent: the description (the user's session name, else the task,
/// else an em dash) on its own full-width line. At L2 it also inlines todo
/// progress (`●●●○○ 3/5`), which used to ride the old capability line.
fn description_line(theme: &Theme, row: &SidebarRow, tier: Tier, width: usize) -> Line<'static> {
    let mut spans = vec![Span::raw("  "), Span::raw(descriptor(row).to_owned())];
    if tier == Tier::L2 && row.todo_total.unwrap_or(0) > 0 {
        spans.push(Span::raw("  "));
        let (done, total) = (row.todo_done.unwrap_or(0), row.todo_total.unwrap_or(0));
        spans.extend(todo_spans(theme, done, total));
    }
    Line::from(trim_spans_to_width(spans, width))
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

/// The line-2 description: the user's session name when they set one (`--name` /
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

/// Column widths for the three aligned meter bars (`ctx` / `5h↻` / `7d↻`): a
/// fixed 3-cell left label and a fixed 5-cell right value, with the bar filling
/// the middle. The values (`78.2%`, `3h20m`, `5d12h`) all fit five cells.
const BAR_LABEL_WIDTH: usize = 3;
const BAR_VALUE_WIDTH: usize = 5;

/// One aligned meter row: `<indent><label:3> <bar> <value:5>`. The caller's
/// `make_bar` builds the colored bar spans to the supplied width; this helper
/// owns the indent, the fixed label and value columns, and the gaps — so `ctx`,
/// `5h`, and `7d` share one bar-start column and one value-end column by
/// construction, with no per-call alignment math.
fn bar_row(
    theme: &Theme,
    label: &str,
    value: &str,
    make_bar: impl FnOnce(usize) -> Vec<Span<'static>>,
    width: usize,
) -> Line<'static> {
    // "  "(2) + label(3) + " "(1) + bar + " "(1) + value(5)
    let bar_width = width
        .saturating_sub(2 + BAR_LABEL_WIDTH + 1 + 1 + BAR_VALUE_WIDTH)
        .max(1);
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(format!("{label:<BAR_LABEL_WIDTH$}"), theme.dim()),
        Span::raw(" "),
    ];
    spans.extend(make_bar(bar_width));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("{value:>BAR_VALUE_WIDTH$}"),
        theme.dim(),
    ));
    Line::from(trim_spans_to_width(spans, width))
}

/// Bar #1, the context meter — the first of the three aligned bars and the only
/// one in the resting `compact` card. `ctx` on the left, the used-percent value
/// on the right, the bar between. When the statusline reports the per-message
/// token breakdown the fill is split into colored segments (cache writes / cache
/// reads / fresh input) — the three add up to exactly the used percentage —
/// otherwise it is a single green → amber → red ramp.
fn gauge_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    let percent = gauge_percent(row)?;
    let value = pct_label(precise_context_pct(row), percent);
    let segments = gauge_segments(row);
    Some(bar_row(
        theme,
        "ctx",
        &value,
        |bar_width| match &segments {
            Some(segments) => segmented_gauge_spans(theme, segments, percent, bar_width),
            None => gauge_spans(theme, percent, bar_width),
        },
        width,
    ))
}

/// A precise context-used fraction (0..=100) from the current-message token
/// composition over the window size, so the `ctx` value can read a decimal
/// (`78.2%`). The composition (`input + cache_creation + cache_read`) is exactly
/// what `used_percentage` measures, so the decimal refines the same number.
/// `None` (no breakdown, or no window size) falls the value back to the integer
/// gauge percent.
fn precise_context_pct(row: &SidebarRow) -> Option<f64> {
    let tokens = ctx(row)?.tokens.as_ref()?;
    let window = tokens.context_window_size? as f64;
    if window <= 0.0 {
        return None;
    }
    let usage = tokens.current_usage.as_ref()?;
    let used = usage.input_tokens.unwrap_or(0)
        + usage.cache_creation_input_tokens.unwrap_or(0)
        + usage.cache_read_input_tokens.unwrap_or(0);
    Some((used as f64 / window * 100.0).clamp(0.0, 100.0))
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

/// Bars #2 and #3, the draining budget windows — appended beneath the ctx bar
/// when the card expands (selection, or a `bars`/`full` density). Both ride the
/// shared bar grammar so they line up under `ctx`: a 3-cell label carrying the
/// `↻` reset mark (`5h↻` / `7d↻`), the draining bar, and the reset countdown in
/// the 5-cell value column. Empty when the agent reported no rate-limit windows.
fn budget_bar_lines(theme: &Theme, row: &SidebarRow, width: usize) -> Vec<Line<'static>> {
    let Some(limits) = ctx(row).and_then(|context| context.rate_limits.as_ref()) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    // The 5-hour reset floors to minutes (seconds are noise on it); the weekly
    // reset keeps the full d/h resolution.
    if let Some(line) = usage_window_line(
        theme,
        "5h↻",
        limits.five_hour.as_ref(),
        duration_compact_minutes,
        width,
    ) {
        lines.push(line);
    }
    if let Some(line) = usage_window_line(
        theme,
        "7d↻",
        limits.seven_day.as_ref(),
        duration_compact,
        width,
    ) {
        lines.push(line);
    }
    lines
}

/// One usage-limit window as a draining budget bar in the shared bar grammar:
/// the bar fills with budget *remaining* (`100 - used`) so a full bar means
/// headroom, and the right value is the reset countdown (`reset_fmt`). Drawn
/// only when the window reported a usage percentage.
fn usage_window_line(
    theme: &Theme,
    label: &str,
    window: Option<&RateLimitWindow>,
    reset_fmt: fn(Timestamp) -> String,
    width: usize,
) -> Option<Line<'static>> {
    let window = window?;
    let remaining = 100 - window.used_percentage?;
    let value = window.resets_at.map(reset_fmt).unwrap_or_default();
    Some(bar_row(
        theme,
        label,
        &value,
        |bar_width| resource_bar_spans(theme, remaining, bar_width),
        width,
    ))
}

/// The session's token totals (`76.5k tok · ↑64.2k ↓12.3k`): the cumulative
/// total, then input and output. Falls back to the bare rollup total for an
/// agent whose context carries no read-only token split (Codex's app-server
/// exposes none), so the line shows *something* for every agent. No cumulative
/// cached figure exists — the cache split is per-message, and the ctx bar's
/// colored segments already show where the live window went.
fn token_totals_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    if let Some(tokens) = ctx(row).and_then(|context| context.tokens.as_ref())
        && (tokens.total_input_tokens.is_some() || tokens.total_output_tokens.is_some())
    {
        let input = tokens.total_input_tokens.unwrap_or(0);
        let output = tokens.total_output_tokens.unwrap_or(0);
        let spans = vec![
            Span::raw("  "),
            tokens_label(theme, input + output),
            Span::styled(" · ", theme.dim()),
            Span::styled(format!("↑{}", tokens_short(input)), theme.dim()),
            Span::raw(" "),
            Span::styled(format!("↓{}", tokens_short(output)), theme.dim()),
        ];
        return Some(Line::from(trim_spans_to_width(spans, width)));
    }
    let total = row.total_tokens?;
    let spans = vec![Span::raw("  "), tokens_label(theme, total)];
    Some(Line::from(trim_spans_to_width(spans, width)))
}

/// The session's work line (`12m worked · +127 -43`): total time worked and the
/// lines the agent added/removed, from the statusline cost record. The diff is
/// the agent's own edit count — distinct from the worktree-total diff on the
/// group header. Drawn only when the cost record reports a field.
fn work_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    let cost = ctx(row)?.cost.as_ref()?;
    let mut spans = vec![Span::raw("  ")];
    let mut printed = false;
    if let Some(ms) = cost.total_duration_ms {
        spans.push(Span::styled(
            format!("{} worked", duration_worked(ms)),
            theme.dim(),
        ));
        printed = true;
    }
    if cost.total_lines_added.is_some() || cost.total_lines_removed.is_some() {
        if printed {
            spans.push(Span::styled(" · ", theme.dim()));
        }
        let clamp = |n: u64| u32::try_from(n).unwrap_or(u32::MAX);
        spans.extend(diff_spans(
            theme,
            clamp(cost.total_lines_added.unwrap_or(0)),
            clamp(cost.total_lines_removed.unwrap_or(0)),
        ));
        printed = true;
    }
    printed.then(|| Line::from(trim_spans_to_width(spans, width)))
}

/// Total display width of a span run, in terminal cells.
fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|span| span.content.chars().count()).sum()
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
