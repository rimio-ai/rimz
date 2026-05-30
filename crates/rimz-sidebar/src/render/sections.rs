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
use rimz::feed::{AgentState, AgentStatus, PermissionPosture};
use rimz::{
    SidebarRow, SidebarRowKind, SidebarStatusCount, SidebarWorktreeGroup, SidebarWorktreeKind,
};

use super::fmt::{
    age_label, age_secs, age_short, clip, dollars, duration_compact, duration_compact_minutes,
    duration_worked, model_label, pct_label, time_remaining, tokens_short, window_size_short,
};
use super::labels::{
    age_style, agent_glyph, agent_style, diff_spans, gauge_spans, posture_pill, posture_style,
    resolver_glyph, resource_bar_spans, segmented_gauge_spans, status_glyph, status_style,
    thinking_still, todo_spans, tokens_label,
};
use super::theme::Theme;

/// Lead glyph for the work line — a clock face for "time worked", so the line
/// reads iconographically (`◷ 12m worked`) and sets the worked span apart from a
/// row's activity age. One cell, so it never disturbs the card's alignment.
const WORKED_GLYPH: &str = "◷";

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

/// The fixed fleet header — the cockpit. Three lines when the room has agents,
/// one calm count line when it does not, so the body below never shifts
/// vertically as agents change *state* (only the empty↔populated transition
/// moves it):
///
/// ```text
/// ? 2   ! 1                6 agents     attention (loud) + total (dim, right)
/// ⢿ 3   ✽ 1   ◌ 2   ✓ 4                calm states
/// +214 -31 · 41k tok · $4.20    2h34m   totals (+/- left, time pinned right)
/// ```
///
/// L1 leads with the attention buckets (`waiting` `?`, `failed` `!`) so the
/// states that need a human read first; with none it reads a calm `✓ all
/// clear`, never an empty line. L2 is the calm tail — `running` split into
/// working/thinking by plan posture (the split reads posture off the visible
/// rows, so a capped-away `running` agent folds into working). Counts come from
/// `status_counts`, which spans capped agents; the resource totals on L3 sum the
/// full agent list, so a capped agent's spend still lands in the total.
pub(super) fn fleet_header_lines(
    theme: &Theme,
    agents: &[AgentState],
    groups: &[SidebarWorktreeGroup],
    width: usize,
) -> Vec<Line<'static>> {
    let running = status_total(groups, AgentStatus::Running);
    let thinking = groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| {
            row.status == Some(AgentStatus::Running)
                && row.permission_posture == Some(PermissionPosture::Plan)
        })
        .count();
    let working = running.saturating_sub(thinking);
    let waiting = status_total(groups, AgentStatus::Waiting);
    let failed = status_total(groups, AgentStatus::Failed);
    let idle = status_total(groups, AgentStatus::Idle);
    let success = status_total(groups, AgentStatus::Success);
    let total = working + thinking + waiting + failed + idle + success;

    let count_label = format!("{total} {}", if total == 1 { "agent" } else { "agents" });
    // An empty (or process-only) room is a single calm count line; the
    // three-line cockpit is reserved for a room that has agents to summarize.
    if total == 0 {
        return vec![Line::from(trim_spans_to_width(
            vec![Span::styled(count_label, theme.dim())],
            width,
        ))];
    }

    // L1 — the attention buckets, loud, with the agent total pinned right.
    let mut attention: Vec<Span<'static>> = Vec::new();
    push_count(
        &mut attention,
        status_glyph(AgentStatus::Waiting),
        waiting,
        status_style(theme, AgentStatus::Waiting),
    );
    push_count(
        &mut attention,
        status_glyph(AgentStatus::Failed),
        failed,
        status_style(theme, AgentStatus::Failed),
    );
    if attention.is_empty() {
        attention.push(Span::styled(
            "✓ all clear",
            theme.style(Color::Green, Modifier::DIM),
        ));
    }
    let line1 = pin_right(
        attention,
        vec![Span::styled(count_label, theme.dim())],
        width,
    );

    // L2 — the calm tail.
    let mut calm: Vec<Span<'static>> = Vec::new();
    push_count(
        &mut calm,
        status_glyph(AgentStatus::Running),
        working,
        agent_style(theme, AgentStatus::Running),
    );
    push_count(
        &mut calm,
        thinking_still(),
        thinking,
        agent_style(theme, AgentStatus::Running),
    );
    push_count(
        &mut calm,
        status_glyph(AgentStatus::Idle),
        idle,
        status_style(theme, AgentStatus::Idle),
    );
    push_count(
        &mut calm,
        status_glyph(AgentStatus::Success),
        success,
        status_style(theme, AgentStatus::Success),
    );
    let line2 = Line::from(trim_spans_to_width(calm, width));

    vec![
        line1,
        line2,
        fleet_totals_line(theme, agents, groups, width),
    ]
}

/// Append a `glyph n` bucket to a header line, spaced from the previous one. The
/// glyph and its count are always separated by a single space (`? 2`, never
/// `?2`); successive buckets are separated by three. A zero count is skipped.
fn push_count(spans: &mut Vec<Span<'static>>, glyph: &str, count: usize, style: Style) {
    if count == 0 {
        return;
    }
    if !spans.is_empty() {
        spans.push(Span::raw("   "));
    }
    spans.push(Span::styled(format!("{glyph} {count}"), style));
}

/// L3 of the cockpit: the fleet's resource totals — lines changed leftmost, then
/// tokens and spend, with total time worked pinned to the right edge. Each
/// metric is summed from the data that carries it (cost/tokens/duration from the
/// agents, the `+/-` churn from the worktree diffs) and is dropped when no agent
/// reports it, so the line shows only what is real.
fn fleet_totals_line(
    theme: &Theme,
    agents: &[AgentState],
    groups: &[SidebarWorktreeGroup],
    width: usize,
) -> Line<'static> {
    let (mut added, mut removed) = (0u64, 0u64);
    for group in groups {
        added += u64::from(group.diff_added.unwrap_or(0));
        removed += u64::from(group.diff_removed.unwrap_or(0));
    }
    let (mut tokens, mut has_tokens) = (0u64, false);
    let (mut cost, mut has_cost) = (0f64, false);
    let (mut duration_ms, mut has_duration) = (0u64, false);
    for agent in agents {
        if let Some(n) = agent_total_tokens(agent) {
            tokens += n;
            has_tokens = true;
        }
        if let Some(record) = agent.context.as_ref().and_then(|ctx| ctx.cost.as_ref()) {
            if let Some(usd) = record.total_cost_usd {
                cost += usd;
                has_cost = true;
            }
            if let Some(ms) = record.total_duration_ms {
                duration_ms += ms;
                has_duration = true;
            }
        }
    }

    let mut left: Vec<Span<'static>> = Vec::new();
    if added + removed > 0 {
        left.extend(diff_spans(theme, clamp_u32(added), clamp_u32(removed)));
    }
    if has_tokens {
        push_dot(&mut left, theme);
        left.push(tokens_label(theme, tokens));
    }
    if has_cost {
        push_dot(&mut left, theme);
        left.push(Span::styled(
            dollars(cost),
            theme.style(Color::Green, Modifier::empty()),
        ));
    }
    let right = if has_duration {
        vec![Span::styled(duration_worked(duration_ms), theme.dim())]
    } else {
        Vec::new()
    };
    pin_right(left, right, width)
}

/// The cumulative token total for an agent: the statusline split when present,
/// else the transcript rollup scalar.
fn agent_total_tokens(agent: &AgentState) -> Option<u64> {
    agent
        .context
        .as_ref()
        .and_then(|ctx| ctx.tokens.as_ref())
        .map(|tokens| {
            tokens.total_input_tokens.unwrap_or(0) + tokens.total_output_tokens.unwrap_or(0)
        })
        .filter(|total| *total > 0)
        .or(agent.total_tokens)
}

/// A faint ` · ` separator between totals fields, pushed only when something
/// already sits to its left.
fn push_dot(spans: &mut Vec<Span<'static>>, theme: &Theme) {
    if !spans.is_empty() {
        spans.push(Span::styled(" · ", theme.faint()));
    }
}

fn clamp_u32(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
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

/// Compose one worktree group's lines, appending to `lines`, and tag each
/// content line in the parallel `map` with the visible row index it belongs to
/// (or `None` for the group header and the `+K more` hidden-count line). `map`
/// stays exactly as long as `lines`, so the hit-test can look a screen line up
/// to a row with no separate geometry. The row index captured for a row's lines
/// is the value *before* `row_index` advances, matching `app::visible_rows()`.
#[allow(clippy::too_many_arguments)]
pub(super) fn worktree_group_lines(
    theme: &Theme,
    group: &SidebarWorktreeGroup,
    width: usize,
    density: SidebarDensity,
    row_index: &mut usize,
    selected_index: usize,
    animation_phase: u64,
    lines: &mut Vec<Line<'static>>,
    map: &mut Vec<Option<usize>>,
) {
    lines.push(group_header(theme, group, width));
    map.push(None);
    let tier = Tier::for_width(content_width(width));
    for row in &group.rows {
        let selected = *row_index == selected_index;
        let this_row = *row_index;
        *row_index += 1;
        let row_lines = row_lines(theme, row, width, tier, density, selected, animation_phase);
        map.extend(std::iter::repeat_n(Some(this_row), row_lines.len()));
        lines.extend(row_lines);
    }
    if group.hidden_count > 0 {
        lines.push(Line::styled(
            format!("  +{} more", group.hidden_count),
            theme.dim(),
        ));
        map.push(None);
    }
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
    // The worktree's churn (lines added/removed vs trunk) pins right. The
    // per-worktree status tally is gone: the cockpit owns the fleet make-up and
    // each row carries its own status glyph, so repeating it here was noise. The
    // label clips to whatever's left after the diff claims its width, always
    // leaving a cell so the header never shrinks to zero on extreme narrowness.
    let diff = group
        .diff_added
        .zip(group.diff_removed)
        .filter(|(added, removed)| *added + *removed > 0);
    let diff_text = diff.map(|(added, removed)| format!("+{added} -{removed}"));
    let right_width = diff_text.as_deref().map_or(0, |text| text.chars().count());
    let label_width = width.saturating_sub(right_width + 1).max(1);
    let left = clip(&label, label_width);
    let padding = width
        .saturating_sub(left.chars().count() + right_width)
        .max(1);

    let mut spans = vec![
        Span::styled(left, theme.style(Color::Cyan, Modifier::BOLD)),
        Span::raw(" ".repeat(padding)),
    ];
    if let Some((added, removed)) = diff {
        spans.extend(diff_spans(theme, added, removed));
    }
    Line::from(spans)
}

/// The `workspace` catch-all (untethered scripts/CI and out-of-project shells)
/// renders as a dim `┄ external ┄┄┄` divider rather than a bold `▌` pod header.
/// It keeps an *attention-only* tally (`? n` / `! n`) so a waiting script ask
/// still surfaces; the calm counts stay with the cockpit.
fn workspace_divider(theme: &Theme, group: &SidebarWorktreeGroup, width: usize) -> Line<'static> {
    let tally = attention_tally(&group.status_counts);
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
        .filter(|count| matches!(count.status, AgentStatus::Waiting | AgentStatus::Failed))
        .filter(|count| count.count > 0)
        .map(|count| format!("{} {}", status_glyph(count.status), count.count))
        .collect::<Vec<_>>()
        .join("  ")
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
    // session cost in money-green, then — for the resting states only — the
    // activity age.
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
    }
    // The age earns its place only when the agent is *not* actively working: a
    // working/thinking head already signals liveness and its age is always
    // "now". For waiting/failed it ramps with staleness (dim → amber → red past
    // the 10-minute window) so a long-ignored ask visibly reddens; idle/done
    // stay dim. A stalled `running` agent is projected to `failed` upstream, so
    // it lands here with its age back — in red.
    if status != AgentStatus::Running {
        let secs = age_secs(row.last_activity);
        if !right.is_empty() {
            right.push(Span::raw("  "));
        }
        right.push(Span::styled(
            age_label(secs),
            age_style(theme, status, secs),
        ));
    }

    // Left cluster: glyph + name + dim capability tokens.
    let mut left: Vec<Span<'static>> = vec![
        Span::styled(
            agent_glyph(status, row.permission_posture, animation_phase),
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
/// else an em dash) on its own full-width line. At L2 the todo progress
/// (`●●●○○ 3/5`) pins to a right column, aligning under the cost/age above so
/// the dots read as a tidy gutter instead of floating after the text.
fn description_line(theme: &Theme, row: &SidebarRow, tier: Tier, width: usize) -> Line<'static> {
    let left = vec![Span::raw("  "), Span::raw(descriptor(row).to_owned())];
    if tier == Tier::L2 && row.todo_total.unwrap_or(0) > 0 {
        let (done, total) = (row.todo_done.unwrap_or(0), row.todo_total.unwrap_or(0));
        return pin_right(left, todo_spans(theme, done, total), width);
    }
    Line::from(trim_spans_to_width(left, width))
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
/// one in the resting `compact` card. `ctx` on the left, the window *size* it
/// fills on the right (`1M` / `200k`), the bar between. The fill amount and its
/// green → amber → red ramp come from the used percentage; when the statusline
/// reports the per-message token breakdown the fill is split into colored
/// segments (cache writes / cache reads / fresh input) that add up to exactly
/// that percentage. With no window size known (e.g. Codex exposes no token
/// context) the value falls back to the used-percent label.
fn gauge_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    let percent = gauge_percent(row)?;
    let value = ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.context_window_size)
        .filter(|size| *size > 0)
        .map(window_size_short)
        .unwrap_or_else(|| pct_label(precise_context_pct(row), percent));
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

/// The session's token totals (`76.5k tok · ↓64.2k ↑12.3k`): the cumulative
/// total, then input (`↓`, read into context) and output (`↑`, generated).
/// Falls back to the bare rollup total for an agent whose context carries no
/// read-only token split (Codex's app-server exposes none), so the line shows
/// *something* for every agent. No cumulative cached figure exists — the cache
/// split is per-message, and the ctx bar's colored segments already show where
/// the live window went.
fn token_totals_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    if let Some(tokens) = ctx(row).and_then(|context| context.tokens.as_ref())
        && (tokens.total_input_tokens.is_some() || tokens.total_output_tokens.is_some())
    {
        let input = tokens.total_input_tokens.unwrap_or(0);
        let output = tokens.total_output_tokens.unwrap_or(0);
        let spans = vec![
            Span::raw("  "),
            tokens_label(theme, input + output),
            Span::styled(" · ", theme.faint()),
            Span::styled(format!("↓{}", tokens_short(input)), theme.dim()),
            Span::raw(" "),
            Span::styled(format!("↑{}", tokens_short(output)), theme.dim()),
        ];
        return Some(Line::from(trim_spans_to_width(spans, width)));
    }
    let total = row.total_tokens?;
    let spans = vec![Span::raw("  "), tokens_label(theme, total)];
    Some(Line::from(trim_spans_to_width(spans, width)))
}

/// The session's work line (`◷ 12m worked · +127 -43`): a clock-led span of time
/// worked and the lines the agent added/removed, from the statusline cost
/// record. The clock sets the worked time apart from a row's activity age; the
/// diff is the agent's own edit count, distinct from the worktree-total diff on
/// the group header. Drawn only when the cost record reports a field.
fn work_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    let cost = ctx(row)?.cost.as_ref()?;
    let mut spans = vec![Span::raw("  ")];
    let mut printed = false;
    if let Some(ms) = cost.total_duration_ms {
        spans.push(Span::styled(
            format!("{WORKED_GLYPH} {} worked", duration_worked(ms)),
            theme.dim(),
        ));
        printed = true;
    }
    if cost.total_lines_added.is_some() || cost.total_lines_removed.is_some() {
        if printed {
            spans.push(Span::styled(" · ", theme.faint()));
        }
        spans.extend(diff_spans(
            theme,
            clamp_u32(cost.total_lines_added.unwrap_or(0)),
            clamp_u32(cost.total_lines_removed.unwrap_or(0)),
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
