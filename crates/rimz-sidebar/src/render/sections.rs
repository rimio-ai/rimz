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
    SidebarProviderPanel, SidebarRow, SidebarRowKind, SidebarStatusCount, SidebarSubAgent,
    SidebarWorktreeGroup, SidebarWorktreeKind,
};

use super::fmt::{
    activity_short, age_secs, age_short, clip, dollars2, duration_worked, duration_worked_coarse,
    model_label, pct_label, reset_days_hours, reset_hours_minutes, time_remaining, tokens_short,
};
use super::labels::{
    TOKENS_CACHED, TOKENS_IN, TOKENS_OUT, agent_glyph, agent_style, attention_glyph_style,
    compacting_glyph, compacting_style, context_severity_color, ctx_glyph_color, diff_spans,
    gauge_spans, infinite_bar_spans, mana_bar_spans, mana_color, posture_pill, posture_style,
    resolver_glyph, segmented_gauge_spans, status_glyph, status_style, subagent_glyph,
    subagent_style, thinking_still, todo_spans, tokens_label, working_glyph,
};
use super::theme::Theme;

/// Lead glyph for the work line — a clock face for "time worked", so the line
/// reads iconographically (`◷ 12m worked`) and sets the worked span apart from a
/// row's activity age. One cell, so it never disturbs the card's alignment.
const WORKED_GLYPH: &str = "◷";

/// The context-meter label — a framed square reading as "the window", replacing
/// the `ctx` word now that it is the row's one bar (the account-scoped 5h/7d
/// budgets moved to the provider dashboard).
const CONTEXT_GLYPH: &str = "▣";

/// Lead glyph for the fleet's committed-work total (`◆ 3`): a filled diamond,
/// the committed sibling of the `◇` token total — commits ahead of trunk, the
/// work waiting to land.
const COMMITS_GLYPH: &str = "◆";

/// The cockpit fleet-size glyphs: a filled `✦` for the main agents you launched,
/// a hollow `✧` for the subagents they spawned this turn — the same filled/hollow
/// contrast the status glyphs use, here for "yours" vs "spawned".
const FLEET_MAIN_GLYPH: &str = "✦";
const FLEET_SUB_GLYPH: &str = "✧";

/// The selected card's left accent: a bold half-block `▌` running the card's
/// full height — the one loud lane marker on screen.
const SELECTED_SPINE: &str = "▌";

/// The selected *worktree's* resting lane spine: a thin `▏` (lighter than the
/// selected card's `▌`) down the whole selected group — header, every row, and
/// the inter-card gaps — so the worktree holding the selection reads as one
/// bracketed lane. Non-selected worktrees carry no spine at all.
const LANE_SPINE: &str = "▏";

/// Inner content width: the sidebar width less the one-cell left gutter and the
/// one-cell right margin. Every line is built to this width and then opened with
/// a gutter cell (blank, lane `▏`, or selected `▌`), leaving the trailing column
/// as the matching right margin — so the whole sidebar reads inside a one-cell
/// frame and selecting a row only swaps the gutter glyph, never shifts a column.
pub(super) fn content_width(width: usize) -> usize {
    width.saturating_sub(2).max(1)
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

/// The fixed fleet header — the cockpit, below the repo dashboard's identity and
/// count/spend lines. Two lines when the room has agents, nothing when it does
/// not (the `✦ 0` head-count lives on the dashboard above), so the body below
/// never shifts vertically as agents change *state*:
///
/// ```text
/// ? 2   ! 1   ○ 2                        ✽ 1   ⢿ 3   ✓ 4   make-up: left · right
/// ◷ 2h34m · ◇ 41k · ◆ 3                                    fleet time · tokens · commits
/// ```
///
/// The top line splits the make-up by who might want you. The left cluster is the
/// rows worth a glance — `waiting` `?` and `failed` `!` (each yellow, reddening
/// once any of its rows is past the neglect window), then a free `idle` `○` at the
/// cluster's right edge (calm green, but grouped left because a free agent wants
/// work). The right cluster is the busy/done tail — thinking `✽` (plan-mode
/// reasoning, read before acting), working `⢿`, then `success` `✓`. Every bucket
/// renders, so a zero reads a faint `? 0`. The bottom line is the fleet's time,
/// token, and committed-work totals. Counts span capped agents (`status_counts`);
/// the totals sum the full agent list.
pub(super) fn fleet_header_lines(
    theme: &Theme,
    agents: &[AgentState],
    groups: &[SidebarWorktreeGroup],
    width: usize,
    redden_secs: i64,
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
    let rate_limited = status_total(groups, AgentStatus::RateLimited);
    let idle = status_total(groups, AgentStatus::Idle);
    let success = status_total(groups, AgentStatus::Success);
    let total = working + thinking + waiting + failed + rate_limited + idle + success;

    // An empty (or process-only) room has no cockpit at all — the `✦ 0` head-count
    // lives on the dashboard above. The two-line cockpit is reserved for a room
    // that has agents to summarize.
    if total == 0 {
        return Vec::new();
    }

    // Top line — the make-up split by who might want you. The left cluster gathers
    // the rows worth a glance: `waiting` `?` and `failed` `!` (yellow, reddening
    // once any of their rows is stale), then a free `idle` `○` at the cluster's
    // right edge — calm green, but grouped left because a free agent wants work.
    // The right cluster is the busy/done tail: thinking before working, then
    // success. Every bucket shows its count.
    let mut left: Vec<Span<'static>> = Vec::new();
    push_count(
        theme,
        &mut left,
        status_glyph(AgentStatus::Waiting),
        waiting,
        attention_bucket_style(
            theme,
            any_attention_stale(groups, AgentStatus::Waiting, redden_secs),
        ),
    );
    push_count(
        theme,
        &mut left,
        status_glyph(AgentStatus::Failed),
        failed,
        attention_bucket_style(
            theme,
            any_attention_stale(groups, AgentStatus::Failed, redden_secs),
        ),
    );
    // Rate-limited sits right after `!`: attention-class, but parked. Unlike the
    // always-on buckets it shows *only when populated* — it is a rare,
    // non-actionable state, and the cockpit's width is precious (a permanent
    // `⏸ 0` would push the busy tail off a narrow sidebar). When it does appear
    // it takes the held-amber resting tone — never the reddening
    // `attention_bucket_style` — since there is nothing to do but wait.
    if rate_limited > 0 {
        push_count(
            theme,
            &mut left,
            status_glyph(AgentStatus::RateLimited),
            rate_limited,
            status_style(theme, AgentStatus::RateLimited),
        );
    }
    push_count(
        theme,
        &mut left,
        status_glyph(AgentStatus::Idle),
        idle,
        status_style(theme, AgentStatus::Idle),
    );
    let mut right: Vec<Span<'static>> = Vec::new();
    push_count(
        theme,
        &mut right,
        thinking_still(),
        thinking,
        agent_style(theme, AgentStatus::Running),
    );
    push_count(
        theme,
        &mut right,
        status_glyph(AgentStatus::Running),
        working,
        agent_style(theme, AgentStatus::Running),
    );
    push_count(
        theme,
        &mut right,
        status_glyph(AgentStatus::Success),
        success,
        status_style(theme, AgentStatus::Success),
    );

    // Split left / right when both clusters fit; on a narrow sidebar (the right
    // cluster alone can outrun the width) fall back to one left-packed line so the
    // attention buckets stay intact and the busy tail clips, rather than crushing
    // `? 0  ! 0` down to a stub.
    let buckets = if spans_width(&left) + 1 + spans_width(&right) <= width {
        pin_right(left, right, width)
    } else {
        if !left.is_empty() && !right.is_empty() {
            left.push(Span::raw("   "));
        }
        left.extend(right);
        Line::from(trim_spans_to_width(left, width))
    };

    vec![
        buckets,
        fleet_totals_line(theme, &fleet_totals(agents, groups), width),
    ]
}

/// The fleet head-count read by the dashboard's L2: `(main, subs)` — the main
/// agents you launched (the sum of the capped per-worktree `status_counts`, so it
/// matches the cockpit make-up below) and the subagents they spawned this turn.
pub(super) fn fleet_size(groups: &[SidebarWorktreeGroup]) -> (usize, usize) {
    let main = groups
        .iter()
        .flat_map(|group| &group.status_counts)
        .map(|count| count.count)
        .sum();
    let subs = groups
        .iter()
        .flat_map(|group| &group.rows)
        .map(|row| row.sub_agents.len())
        .sum();
    (main, subs)
}

/// The cockpit attention bucket's tone: bold yellow while every contributing row
/// is fresh, bold red once any has sat unanswered past the neglect window — the
/// aggregate echo of the per-row glyph escalation in [`attention_glyph_style`].
fn attention_bucket_style(theme: &Theme, stale: bool) -> Style {
    let color = if stale { Color::Red } else { Color::Yellow };
    theme.style(color, Modifier::BOLD)
}

/// Whether any visible row in `status` has gone unanswered past `redden_secs`.
/// Reads the rendered rows (capped-away agents are excluded — the bucket count
/// still spans them, but a hidden agent never drives the visible heat).
fn any_attention_stale(
    groups: &[SidebarWorktreeGroup],
    status: AgentStatus,
    redden_secs: i64,
) -> bool {
    groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.status == Some(status))
        .any(|row| age_secs(row.last_activity) >= redden_secs)
}

/// Append a `glyph n` bucket to a header line, spaced from the previous one. The
/// glyph and its count are always separated by a single space (`? 2`, never
/// `?2`); successive buckets are separated by three. Every bucket renders, so a
/// zero reads `? 0` — the cockpit is a fixed dashboard, scannable by position —
/// but a zero bucket drops to faint chrome so the eye lands on the live counts,
/// not the empty ones.
fn push_count(
    theme: &Theme,
    spans: &mut Vec<Span<'static>>,
    glyph: &str,
    count: usize,
    style: Style,
) {
    if !spans.is_empty() {
        spans.push(Span::raw("   "));
    }
    let style = if count == 0 { theme.faint() } else { style };
    spans.push(Span::styled(format!("{glyph} {count}"), style));
}

/// The fleet's summed resource totals, computed once and read by both the repo
/// dashboard's L2 (the `cost`, pinned right of the head-count) and the cockpit's
/// bottom line (time, tokens, and committed work). Each `Option` stays `None`
/// until some agent reports that metric, so a consumer renders only what is real.
pub(super) struct FleetTotals {
    pub cost: Option<f64>,
    pub tokens: Option<u64>,
    pub duration_ms: Option<u64>,
    pub commits: u64,
}

pub(super) fn fleet_totals(agents: &[AgentState], groups: &[SidebarWorktreeGroup]) -> FleetTotals {
    let commits = groups
        .iter()
        .filter_map(|group| group.commits_ahead)
        .map(u64::from)
        .sum();
    let mut totals = FleetTotals {
        cost: None,
        tokens: None,
        duration_ms: None,
        commits,
    };
    for agent in agents {
        if let Some(n) = agent_total_tokens(agent) {
            *totals.tokens.get_or_insert(0) += n;
        }
        if let Some(record) = agent.context.as_ref().and_then(|ctx| ctx.cost.as_ref()) {
            if let Some(usd) = record.total_cost_usd {
                *totals.cost.get_or_insert(0.0) += usd;
            }
            if let Some(ms) = record.total_duration_ms {
                *totals.duration_ms.get_or_insert(0) += ms;
            }
        }
    }
    totals
}

/// The repo dashboard's L2: the fleet head-count on the left — `✦ {main}` main
/// agents (a filled star, teal to echo the metric icons) and, when any spawned
/// children this turn, `✧ {subs}` subagents (a hollow star, faint and
/// subordinate) — with the bold money-green spend (two decimals) pinned right.
/// Always present beneath the identity line; an empty room reads `✦ 0` with no
/// spend. Committed work moved down to the cockpit totals line.
pub(super) fn dashboard_summary_line(
    theme: &Theme,
    size: (usize, usize),
    totals: &FleetTotals,
    width: usize,
) -> Line<'static> {
    let (main, subs) = size;
    let mut left = metric_spans(theme, FLEET_MAIN_GLYPH, Color::Cyan, &main.to_string());
    if subs > 0 {
        left.push(Span::raw("   "));
        left.push(Span::styled(FLEET_SUB_GLYPH, theme.faint()));
        left.push(Span::styled(format!(" {subs}"), theme.dim()));
    }
    let right = match totals.cost {
        Some(cost) => vec![Span::styled(
            dollars2(cost),
            theme.style(Color::Green, Modifier::BOLD),
        )],
        None => Vec::new(),
    };
    pin_right(left, right, width)
}

/// The cockpit's bottom line: the fleet's time, token, and committed-work totals
/// — time worked behind a teal clock, the violet `◇` token total, then the green
/// `◆` commits ahead of trunk. Spend lives on the dashboard above; this line
/// carries the running fleet's aggregates and drops a field no agent reported.
fn fleet_totals_line(theme: &Theme, totals: &FleetTotals, width: usize) -> Line<'static> {
    let mut left: Vec<Span<'static>> = Vec::new();
    if let Some(ms) = totals.duration_ms {
        left.extend(metric_spans(
            theme,
            WORKED_GLYPH,
            Color::Cyan,
            &duration_worked_coarse(ms),
        ));
    }
    if let Some(tokens) = totals.tokens {
        push_dot(&mut left, theme);
        left.extend(tokens_label(theme, tokens));
    }
    if totals.commits > 0 {
        push_dot(&mut left, theme);
        left.extend(metric_spans(
            theme,
            COMMITS_GLYPH,
            Color::Green,
            &totals.commits.to_string(),
        ));
    }
    Line::from(trim_spans_to_width(left, width))
}

/// A stats metric as a colored icon glyph + dim value (`◷ 2h34m`, `◆ 3`): the
/// glyph carries a semantic accent (time teal, commits green; the `◇` token
/// total goes violet via [`tokens_label`]) while the number stays neutral, so
/// the stats read as a tidy icon column instead of a wall of one tone.
fn metric_spans(theme: &Theme, glyph: &str, color: Color, value: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(glyph.to_owned(), theme.style(color, Modifier::empty())),
        Span::styled(format!(" {value}"), theme.dim()),
    ]
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
    providers: &[SidebarProviderPanel],
    width: usize,
    density: SidebarDensity,
    redden_secs: i64,
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
            density,
            selected,
            animation_phase,
            redden_secs,
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

fn status_total(groups: &[SidebarWorktreeGroup], status: AgentStatus) -> usize {
    groups
        .iter()
        .flat_map(|group| &group.status_counts)
        .filter(|count| count.status == status)
        .map(|count| count.count)
        .sum()
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
    let label_width = cw.saturating_sub(right_width + 1).max(1);
    let left = clip(&group.label, label_width);
    // The dotted `┄` seal caps only the *selected* worktree's header, so the lane
    // reads as one bracketed block; every other header is just its bold label and
    // right-pinned diff, with plain space filling the gap. Sized to land the line
    // exactly on the content width — a space frames the dotted run from the text
    // on each side it touches.
    let middle = cw.saturating_sub(left.chars().count() + right_width);
    let fill = if sealed {
        match (diff.is_some(), middle) {
            (true, m) if m >= 2 => format!(" {} ", "┄".repeat(m - 2)),
            (false, m) if m >= 1 => format!(" {}", "┄".repeat(m - 1)),
            (_, m) => " ".repeat(m),
        }
    } else {
        " ".repeat(middle)
    };

    let mut spans = vec![
        Span::styled(left, theme.style(Color::Cyan, Modifier::BOLD)),
        Span::styled(fill, theme.faint()),
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
        .filter(|count| matches!(count.status, AgentStatus::Waiting | AgentStatus::Failed))
        .filter(|count| count.count > 0)
        .map(|count| format!("{} {}", status_glyph(count.status), count.count))
        .collect::<Vec<_>>()
        .join("  ")
}

/// A just-started agent: idle, sitting on the `Some(0)` baseline context gauge
/// with no real usage behind it yet. Its 0% bar and zeroed stat lines are noise,
/// so the card collapses to identity + description (+ the last-activity age).
fn idle_unstarted(row: &SidebarRow) -> bool {
    matches!(row.status.unwrap_or(AgentStatus::Idle), AgentStatus::Idle)
        && gauge_percent(row).unwrap_or(0) == 0
}

#[allow(clippy::too_many_arguments)]
fn row_lines(
    theme: &Theme,
    row: &SidebarRow,
    providers: &[SidebarProviderPanel],
    width: usize,
    tier: Tier,
    density: SidebarDensity,
    selected: bool,
    animation_phase: u64,
    redden_secs: i64,
    gutter: Gutter,
) -> Vec<Line<'static>> {
    let cw = content_width(width);
    // The resting (unselected) card is line 1 (identity), line 2 (description),
    // and the ctx bar — plus whatever `density` keeps resident. Selection only
    // *appends* the deeper lines (the token and work stats); it never reshapes a
    // line already on screen, so the card never reflows on expand. The 5h/7d
    // budgets are account-scoped, so they live in the pinned provider dashboard,
    // never on a row.
    let mut inner = vec![identity_line(
        theme,
        row,
        providers,
        tier,
        cw,
        animation_phase,
        redden_secs,
    )];
    // An active process row carries its full command on a dim second line under
    // the shell anchor — the build or `sudo` install reads in full while line 1
    // stays the stable shell label. Idle process rows have no detail to add.
    if row.row_kind == SidebarRowKind::Process
        && let Some(line) = process_detail_line(theme, row, cw)
    {
        inner.push(line);
    }
    if row.row_kind == SidebarRowKind::Agent {
        inner.push(description_line(theme, row, tier, cw));
        // A just-started idle agent sits on the 0% baseline gauge with nothing
        // behind it — suppress the bar so the fresh card reads calm.
        if !idle_unstarted(row)
            && let Some(line) = gauge_line(theme, row, cw)
        {
            inner.push(line);
        }
        if selected || density.shows_stats() {
            if idle_unstarted(row) {
                // The zeroed token and work lines are noise on a fresh card; keep
                // only the last-activity age, pinned bottom-right.
                inner.push(activity_age_line(theme, row, cw));
            } else {
                if let Some(line) = token_totals_line(theme, row, cw) {
                    inner.push(line);
                }
                if let Some(line) = work_line(theme, row, cw) {
                    inner.push(line);
                }
            }
        }
        // The subagents this agent spawned this turn, listed only in the
        // expanded card — appended after the stats so the resting card never
        // reflows (selection only ever adds lines).
        if selected && !row.sub_agents.is_empty() {
            inner.extend(sub_agent_lines(theme, &row.sub_agents, cw));
        }
    }
    inner
        .into_iter()
        .map(|line| with_gutter(theme, line, gutter))
        .collect()
}

/// The expanded card's subagent list: a dim `subagents (N)` header, then one
/// indented line per child — its status glyph, its type, and the task when that
/// adds anything. Children are subordinate to the parent card, so every line is
/// dim and indented past the parent's own stat lines.
fn sub_agent_lines(
    theme: &Theme,
    sub_agents: &[SidebarSubAgent],
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(trim_spans_to_width(
        vec![Span::styled(
            format!("  subagents ({})", sub_agents.len()),
            theme.dim(),
        )],
        width,
    ))];
    for sub in sub_agents {
        let mut spans = vec![
            Span::raw("    "),
            Span::styled(status_glyph(sub.status), status_style(theme, sub.status)),
            Span::raw(" "),
            Span::styled(sub.name.clone(), theme.dim()),
        ];
        // Show the task only when it differs from the name (the name already is
        // the type for most children) so the line doesn't read `Explore — Explore`.
        if let Some(task) = sub.task.as_deref().filter(|task| *task != sub.name) {
            spans.push(Span::styled(format!(" — {task}"), theme.dim()));
        }
        lines.push(Line::from(trim_spans_to_width(spans, width)));
    }
    lines
}

/// Width budget for the agent name on line 1: short agent kinds (`claude`,
/// `codex`) fit comfortably, and a longer name clips with `…` rather than
/// pushing the model/effort tokens off the line.
const NAME_MAX: usize = 12;

/// The agent name's style: its provider's brand color (Claude clay, Codex blue,
/// …) kept at the dim weight the name already carries, so the card ties to its
/// provider dashboard without shouting. Falls back to plain dim chrome when no
/// provider matches the kind.
fn agent_name_style(theme: &Theme, providers: &[SidebarProviderPanel], kind: &str) -> Style {
    providers
        .iter()
        .find(|panel| panel.kind == kind)
        .map(|panel| theme.style(Color::Indexed(panel.color), Modifier::DIM))
        .unwrap_or_else(|| theme.dim())
}

fn identity_line(
    theme: &Theme,
    row: &SidebarRow,
    providers: &[SidebarProviderPanel],
    tier: Tier,
    width: usize,
    animation_phase: u64,
    redden_secs: i64,
) -> Line<'static> {
    if row.row_kind == SidebarRowKind::Process {
        return process_row_line(theme, row, width, animation_phase);
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
    agent_identity_line(
        theme,
        row,
        providers,
        status,
        tier,
        width,
        animation_phase,
        redden_secs,
    )
}

/// The leading status cell for an agent row, applying the two transient render
/// overlays before the base status glyph: a **compacting** head (a violet bar
/// pulsing as the context window condenses) and a **waiting-on-subagents** head
/// (a quiet clay wave while a live child runs). Both are short-lived and stay
/// out of the cockpit tally — they ride over the row's base status here. A
/// human-blocked `?`/`!` always wins, so the overlays defer to those; otherwise
/// the cell is the animated working/thinking fill or the static status glyph.
fn agent_lead_cell(
    theme: &Theme,
    row: &SidebarRow,
    status: AgentStatus,
    animation_phase: u64,
    redden_secs: i64,
) -> Span<'static> {
    let actionable = matches!(status, AgentStatus::Waiting | AgentStatus::Failed);
    if !actionable && row.compacting {
        return Span::styled(compacting_glyph(animation_phase), compacting_style(theme));
    }
    if status == AgentStatus::Running
        && row
            .sub_agents
            .iter()
            .any(|child| child.status == AgentStatus::Running)
    {
        return Span::styled(subagent_glyph(animation_phase), subagent_style(theme));
    }
    Span::styled(
        agent_glyph(status, row.permission_posture, animation_phase),
        attention_glyph_style(theme, status, age_secs(row.last_activity), redden_secs),
    )
}

/// Line 1 for an agent: the leading cell (animated only while the agent is
/// actively working or plan-mode thinking — attention markers stay still), the
/// agent name, then the dim capability tokens (`· model · effort`) and the
/// permission posture pill, with the bold `$cost` (money-green) pinned right.
/// Capability tokens degrade by width tier: L2 carries model + effort + posture,
/// L1 drops effort, L0 keeps just the name — cost always pins right. A blocked
/// `?`/`!` glyph reddens once the row has gone unanswered past the 30-minute
/// neglect window, so a long-ignored ask escalates without a timestamp.
#[allow(clippy::too_many_arguments)]
fn agent_identity_line(
    theme: &Theme,
    row: &SidebarRow,
    providers: &[SidebarProviderPanel],
    status: AgentStatus,
    tier: Tier,
    width: usize,
    animation_phase: u64,
    redden_secs: i64,
) -> Line<'static> {
    // Right cluster, built first so the left trims to whatever's left: the
    // session cost, bold in money-green.
    let mut right: Vec<Span<'static>> = Vec::new();
    if let Some(cost) = ctx(row)
        .and_then(|context| context.cost.as_ref())
        .and_then(|cost| cost.total_cost_usd)
        .map(dollars2)
    {
        right.push(Span::styled(
            cost,
            theme.style(Color::Green, Modifier::BOLD),
        ));
    }

    // Left cluster: glyph + name + dim capability tokens. The glyph reddens once
    // a `waiting`/`failed` row has sat past the neglect window. The kind name is
    // repeated and low-information, so it dims to chrome; the leading glyph and
    // its color carry identity, and the bright slot is saved for the task below.
    let mut left: Vec<Span<'static>> = vec![
        agent_lead_cell(theme, row, status, animation_phase, redden_secs),
        Span::raw(" "),
        Span::styled(
            clip(&row.name, NAME_MAX),
            agent_name_style(theme, providers, &row.name),
        ),
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
/// `/rename`), else the agent's live task, else the latest prompt, else an em
/// dash. The name is what a human chose to call this session, so it reads better
/// than the task. The activity-bound `task` clears on idle, so the persisted
/// prompt keeps an unnamed session labelled past its turn until it earns a name.
fn descriptor(row: &SidebarRow) -> &str {
    ctx(row)
        .and_then(|context| context.session_name.as_deref())
        .filter(|name| !name.is_empty())
        .or(row.task.as_deref().filter(|task| !task.is_empty()))
        .or(row.prompt.as_deref().filter(|prompt| !prompt.is_empty()))
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

/// The one-cell left gutter that opens every line — blank for chrome and resting
/// worktrees, the resting lane `▏` for the selected worktree, the bold `▌` accent
/// for the selected card itself.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Gutter {
    /// No marker — chrome and non-selected worktrees.
    Blank,
    /// The selected worktree's resting lane spine (`▏`, dim teal).
    Lane,
    /// The selected card's bold accent spine (`▌`).
    Selected,
}

/// Open a line with its one-cell gutter (see [`Gutter`]). The cell is always one
/// column, so changing it never shifts content; the trailing column the content
/// leaves free is the matching right margin. Applied to every line of a worktree
/// group so the lane spans the whole selected worktree as one block, with the
/// selected card lit `▌` inside it. Under `NO_COLOR` the `▏`/`▌` shapes carry the
/// lane and the selection without color.
fn with_gutter(theme: &Theme, line: Line<'static>, gutter: Gutter) -> Line<'static> {
    let cell = match gutter {
        Gutter::Blank => Span::raw(" "),
        Gutter::Lane => Span::styled(LANE_SPINE, theme.style(Color::Cyan, Modifier::DIM)),
        Gutter::Selected => Span::styled(SELECTED_SPINE, theme.selection()),
    };
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(cell);
    spans.extend(line.spans);
    Line::from(spans)
}

fn process_row_line(
    theme: &Theme,
    row: &SidebarRow,
    width: usize,
    animation_phase: u64,
) -> Line<'static> {
    let dim = theme.dim();
    let label = clip(&row.name, width.saturating_sub(2).max(1));
    // An active pane (a build, a test, a script) gets the running braille spinner
    // so live work reads at a glance; an idle shell or a TUI the user just sits in
    // rests on the same hollow `○` an idle agent shows, so the lead column reads
    // and aligns alike across the two. Both stay in the dim chrome tone, never the
    // agent's clay, so a process stays secondary to an agent.
    let lead = if row.process_active {
        working_glyph(animation_phase)
    } else {
        status_glyph(AgentStatus::Idle)
    };
    Line::from(vec![
        Span::styled(lead, dim),
        Span::raw(" "),
        Span::styled(label, dim),
    ])
}

/// Line 2 for an *active* process row: the full foreground command, dim and
/// indented under the shell anchor, so a build or a `sudo` install reads in full
/// while the primary line keeps the stable shell label. `None` when the producer
/// left no detail (an idle pane, or a command already shown whole on line 1).
fn process_detail_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    let detail = row.command_detail.as_deref()?;
    let left = vec![
        Span::raw("  "),
        Span::styled(detail.to_owned(), theme.dim()),
    ];
    Some(Line::from(trim_spans_to_width(left, width)))
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

/// Column widths for the per-row context meter: a one-cell lead-glyph label
/// (`▣`, sharing the column with the `◇`/`◷` glyphs on the lines below it) and a
/// fixed 5-cell right value, with the bar filling the middle. The value
/// (`78.2%`) fits five cells. The provider dashboard's budget bars carry their
/// own label/value widths but the same shape.
const BAR_LABEL_WIDTH: usize = 1;
const BAR_VALUE_WIDTH: usize = 5;

/// One aligned meter row: `<indent><label:3> <bar> <value:5>`. The caller's
/// `make_bar` builds the colored bar spans to the supplied width and supplies the
/// `label_style` for the lead glyph (the context meter tints its `▣` with the
/// bar's severity); this helper owns the indent, the fixed label and value
/// columns, and the gaps — so every row built through it shares one bar-start
/// column and one value-end column by construction, with no per-call alignment
/// math. The value column stays dim chrome.
fn bar_row(
    theme: &Theme,
    label: &str,
    label_style: Style,
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
        Span::styled(format!("{label:<BAR_LABEL_WIDTH$}"), label_style),
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

/// The context meter — the resting card's one bar. `ctx` on the left, the
/// **percent used** on the right (always — the window *size* moves to the
/// expanded token line), the bar between. The fill amount and its green → amber
/// → red ramp come from the used percentage; when the statusline reports the
/// per-message token breakdown the fill is split into colored segments (cache
/// writes / cache reads / fresh input) that add up to exactly that percentage.
/// The value prefers a one-decimal precise fraction (`78.2%`) over the integer
/// gauge.
fn gauge_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    let percent = gauge_percent(row)?;
    let value = pct_label(precise_context_pct(row), percent);
    let used = context_used_tokens(row);
    // The bar's severity decides composition-vs-solid and the solid color: the
    // composition segments (where the window went) paint only while it is
    // calm-green; once it warns the bar goes solid severity.
    let bar_color = context_severity_color(percent, used);
    let segments = (bar_color == Color::Green)
        .then(|| gauge_segments(row))
        .flatten();
    // The `▣` glyph follows *total* usage (blue when calm), decoupled from the
    // bar's dominant segment — it reads how full the window is, not where it went.
    let glyph_color = ctx_glyph_color(percent, used);
    Some(bar_row(
        theme,
        CONTEXT_GLYPH,
        theme.style(glyph_color, Modifier::empty()),
        &value,
        |bar_width| match &segments {
            Some(segments) => segmented_gauge_spans(theme, segments, bar_color, percent, bar_width),
            None => gauge_spans(theme, bar_color, percent, bar_width),
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
    let window = ctx(row)?.tokens.as_ref()?.context_window_size? as f64;
    if window <= 0.0 {
        return None;
    }
    let used = context_used_tokens(row)? as f64;
    Some((used / window * 100.0).clamp(0.0, 100.0))
}

/// Tokens currently occupying the context window — the current message's `input
/// + cache_creation + cache_read`. The numerator behind both
/// [`precise_context_pct`] and the absolute-token severity overlay in
/// [`gauge_line`]. `None` when no per-message breakdown was reported.
fn context_used_tokens(row: &SidebarRow) -> Option<u64> {
    let usage = ctx(row)?.tokens.as_ref()?.current_usage.as_ref()?;
    Some(
        usage.input_tokens.unwrap_or(0)
            + usage.cache_creation_input_tokens.unwrap_or(0)
            + usage.cache_read_input_tokens.unwrap_or(0),
    )
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
/// left to right: cache writes (amber), cache reads (blue), fresh `input` (red).
/// `None` when no breakdown was reported (a fresh session, post-compact, or a
/// non-Claude agent), so the bar falls back to a single-color ramp.
fn gauge_segments(row: &SidebarRow) -> Option<[(u64, Color); 3]> {
    let usage = ctx(row)?.tokens.as_ref()?.current_usage.as_ref()?;
    let input = usage.input_tokens.unwrap_or(0);
    let writes = usage.cache_creation_input_tokens.unwrap_or(0);
    let reads = usage.cache_read_input_tokens.unwrap_or(0);
    (input + writes + reads > 0).then_some([
        (writes, Color::Yellow),
        (reads, Color::Blue),
        (input, Color::Red),
    ])
}

/// The session's token totals as the glyph set — `◇ 76.5k ↘ 64.2k ↗ 12.3k ◌
/// 1.6k`: the cumulative total (violet `◇`), then input read in (`↘`), output
/// generated (`↗`), and the latest message's cached reads (`◌`, dropped when
/// none). The breakdown glyphs stay dim so only the `◇` total carries a tone.
/// Falls back to the bare `◇` rollup total for an agent whose context carries no
/// read-only token split (Codex's app-server exposes none), so the line shows
/// *something* for every agent.
fn token_totals_line(theme: &Theme, row: &SidebarRow, width: usize) -> Option<Line<'static>> {
    if let Some(tokens) = ctx(row).and_then(|context| context.tokens.as_ref())
        && (tokens.total_input_tokens.is_some() || tokens.total_output_tokens.is_some())
    {
        let input = tokens.total_input_tokens.unwrap_or(0);
        let output = tokens.total_output_tokens.unwrap_or(0);
        // The cache split is per-message, so `◌` reads the latest message's
        // cache (creation + reads); there is no cumulative cached figure.
        let cached = tokens
            .current_usage
            .as_ref()
            .map(|usage| {
                usage.cache_creation_input_tokens.unwrap_or(0)
                    + usage.cache_read_input_tokens.unwrap_or(0)
            })
            .filter(|cached| *cached > 0);
        let mut spans = vec![Span::raw("  ")];
        spans.extend(tokens_label(theme, input + output));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("{TOKENS_IN} {}", tokens_short(input)),
            theme.dim(),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("{TOKENS_OUT} {}", tokens_short(output)),
            theme.dim(),
        ));
        if let Some(cached) = cached {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("{TOKENS_CACHED} {}", tokens_short(cached)),
                theme.dim(),
            ));
        }
        return Some(Line::from(trim_spans_to_width(spans, width)));
    }
    let total = row.total_tokens?;
    let mut spans = vec![Span::raw("  ")];
    spans.extend(tokens_label(theme, total));
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
        // Teal clock icon (matching the cockpit's time total), dim value + word.
        spans.push(Span::styled(
            WORKED_GLYPH,
            theme.style(Color::Cyan, Modifier::empty()),
        ));
        spans.push(Span::styled(
            format!(" {} worked", duration_worked(ms)),
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
    // Last-activity age pinned right — the one coarse "how long since this agent
    // did something" readout, kept to this detail line so the compact row stays
    // calm.
    let age = Span::styled(activity_short(row.last_activity), theme.dim());
    printed.then(|| pin_right(spans, vec![age], width))
}

/// The lone last-activity age, pinned bottom-right — the only stat a just-started
/// idle agent shows once the zeroed token and work lines are suppressed.
fn activity_age_line(theme: &Theme, row: &SidebarRow, width: usize) -> Line<'static> {
    let age = Span::styled(activity_short(row.last_activity), theme.dim());
    pin_right(vec![Span::raw("  ")], vec![age], width)
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

/// The provider dashboard's fixed art column width: the brand emblem is padded
/// to this many cells so the stats/bar column to its right starts at one shared
/// cell for every provider block — the bars align across providers by
/// construction. Dropped (bars run full-width) below [`PROVIDER_ART_MIN_WIDTH`].
const PROVIDER_ART_WIDTH: usize = 9;

/// Narrowest sidebar that still affords the art column beside a bar; below it
/// the emblem is dropped so the bar keeps a legible length.
const PROVIDER_ART_MIN_WIDTH: usize = 34;

/// The provider bar's label slot (`5h` / `7d` / `∞`) and reset-value column,
/// shared by every provider bar so they align front and back. The value holds
/// `↻ ` plus a two-unit reset countdown (`↻ 3d12h`).
const PROVIDER_LABEL_WIDTH: usize = 2;
const PROVIDER_VALUE_WIDTH: usize = 7;

/// The pinned per-provider dashboard: one block per provider (`Claude Code`,
/// `Codex`, …), each a header line then the brand emblem zipped against the
/// aggregate stats and the account-scoped budget bars. A metered account drains
/// 5h/7d "mana" bars toward their resets; an unmetered (API-key) account shows
/// the `∞` "infinite power" bar in the label slot with no countdown. The bars
/// share one start and one end column across every block, so the whole
/// dashboard reads as one aligned grid. Bottom chrome — never a jump target.
pub(super) fn provider_panel_lines(
    theme: &Theme,
    providers: &[SidebarProviderPanel],
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, panel) in providers.iter().enumerate() {
        // A blank line sets each provider block apart, so two providers read as
        // two distinct cards rather than one dense slab.
        if index > 0 {
            lines.push(Line::from(""));
        }
        lines.push(provider_header_line(theme, panel, width));
        lines.extend(provider_body_lines(theme, panel, width));
    }
    lines
}

/// `Claude Code v2.1.158 · Claude Max          ⇅ rc`: the product name in the
/// brand color and the version + plan dim on the left, with the violet `⇅ rc`
/// flag pinned to the top-right corner of the block when remote control is on for
/// the provider. Fields drop out when unknown.
fn provider_header_line(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    width: usize,
) -> Line<'static> {
    let mut left = vec![Span::styled(
        panel.product_name.clone(),
        theme.style(Color::Indexed(panel.color), Modifier::BOLD),
    )];
    if let Some(version) = panel.version.as_deref() {
        left.push(Span::styled(format!(" v{version}"), theme.dim()));
    }
    if let Some(plan) = panel.plan.as_deref() {
        left.push(Span::styled(" · ", theme.faint()));
        left.push(Span::styled(plan.to_owned(), theme.dim()));
    }
    let right = if panel.remote_control {
        vec![Span::styled(
            "⇅ rc",
            theme.style(Color::Magenta, Modifier::BOLD),
        )]
    } else {
        Vec::new()
    };
    pin_right(left, right, width)
}

/// The block beneath the header: the brand emblem in a fixed left column zipped
/// against the right column — aggregate stats on the first line, the budget bars
/// below. The art is dropped (and the bars run full width) when the sidebar is
/// too narrow to fit both.
fn provider_body_lines(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    width: usize,
) -> Vec<Line<'static>> {
    let show_art = !panel.art.is_empty() && width >= PROVIDER_ART_MIN_WIDTH;
    let art_column = if show_art { PROVIDER_ART_WIDTH + 1 } else { 0 };
    let bar_region = width.saturating_sub(art_column);

    // The right column, top to bottom: aggregate stats then the budget bars,
    // packed directly so the three rows line up against the three-line emblem and
    // the bars sit right under the numbers (no separator row).
    let mut rights: Vec<Vec<Span<'static>>> = vec![provider_stats_spans(theme, panel)];
    rights.extend(provider_bar_rows(theme, panel, bar_region));

    let rows = panel.art.len().max(rights.len());
    let mut lines = Vec::with_capacity(rows);
    for index in 0..rows {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if show_art {
            let art_line = panel.art.get(index).map(String::as_str).unwrap_or("");
            spans.push(Span::styled(
                pad_to(art_line, PROVIDER_ART_WIDTH),
                theme.style(Color::Indexed(panel.color), Modifier::empty()),
            ));
            spans.push(Span::raw(" "));
        }
        if let Some(right) = rights.get(index) {
            spans.extend(right.iter().cloned());
        }
        lines.push(Line::from(trim_spans_to_width(spans, width)));
    }
    lines
}

/// The provider's aggregate stats line: bold money-green spend (two decimals) and
/// the `◇` token total — always rendered, reading `$0.00 · ◇ 0` for an idle
/// account so the line above the budget bars is never blank. The summed `+/-`
/// churn is intentionally absent — a noisy per-account aggregate; per-worktree
/// churn lives on the group headers and per-agent churn on the work line.
fn provider_stats_spans(theme: &Theme, panel: &SidebarProviderPanel) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        dollars2(panel.total_cost_usd.unwrap_or(0.0)),
        theme.style(Color::Green, Modifier::BOLD),
    )];
    let tokens = panel.total_input_tokens.unwrap_or(0) + panel.total_output_tokens.unwrap_or(0);
    push_dot(&mut spans, theme);
    spans.extend(tokens_label(theme, tokens));
    spans
}

/// The provider's budget bars within `region`: a metered account drains its
/// 5h/7d "mana" bars; an unmetered account shows the single `∞` bar. The 5-hour
/// reset reads `{h}h{mm}m` and the weekly `{d}d{hh}h` — both fixed two-unit, so
/// the two countdowns column-align. Each row aligns front and back within
/// `region`, so they line up across providers too.
fn provider_bar_rows(
    theme: &Theme,
    panel: &SidebarProviderPanel,
    region: usize,
) -> Vec<Vec<Span<'static>>> {
    if !panel.metered {
        return vec![infinite_bar_row(theme, panel.color, region)];
    }
    // A spent weekly cap gates the 5-hour window: once 7d is exhausted the 5h
    // budget is unusable regardless of its own reading, so paint the 5h row as
    // exhausted (red, no countdown) rather than a misleading fresh bar.
    let seven_exhausted = panel
        .seven_day
        .as_ref()
        .and_then(|window| window.used_percentage)
        .is_some_and(|used| used >= 100);
    let mut rows = Vec::new();
    if let Some(spans) = metered_bar_row(
        theme,
        "5h",
        panel.five_hour.as_ref(),
        reset_hours_minutes,
        region,
        seven_exhausted,
    ) {
        rows.push(spans);
    }
    if let Some(spans) = metered_bar_row(
        theme,
        "7d",
        panel.seven_day.as_ref(),
        reset_days_hours,
        region,
        false,
    ) {
        rows.push(spans);
    }
    rows
}

/// One metered budget bar row: a `5h`/`7d` label, the draining mana bar (filled
/// = remaining), and the `↻ <reset>` countdown right-aligned in the value
/// column. The label mirrors its bar's severity color. `force_exhausted` paints
/// the row as fully spent — red, no countdown — regardless of the window's own
/// reading (the 7d→5h cascade). `None` when the window reported no usage
/// percentage and is not force-exhausted.
fn metered_bar_row(
    theme: &Theme,
    label: &str,
    window: Option<&RateLimitWindow>,
    reset_fmt: fn(Timestamp) -> String,
    region: usize,
    force_exhausted: bool,
) -> Option<Vec<Span<'static>>> {
    let window = window?;
    let remaining = if force_exhausted {
        0
    } else {
        100u8.saturating_sub(window.used_percentage?)
    };
    let value = if force_exhausted {
        String::new()
    } else {
        window
            .resets_at
            .map(|at| format!("↻ {}", reset_fmt(at)))
            .unwrap_or_default()
    };
    let bar_width = provider_bar_width(region);
    let mut spans = vec![
        Span::styled(
            format!("{label:<PROVIDER_LABEL_WIDTH$}"),
            theme.style(mana_color(remaining), Modifier::empty()),
        ),
        Span::raw(" "),
    ];
    spans.extend(mana_bar_spans(theme, remaining, bar_width));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("{value:>PROVIDER_VALUE_WIDTH$}"),
        theme.dim(),
    ));
    Some(spans)
}

/// The unmetered `∞` bar row: the infinity icon rides the label slot (aligned
/// with `5h`/`7d`), then the full brand-colored infinite bar. The value column
/// is reserved but empty — no countdown — so the bar's right edge still aligns
/// with the metered bars'.
fn infinite_bar_row(theme: &Theme, color: u8, region: usize) -> Vec<Span<'static>> {
    let bar_width = provider_bar_width(region);
    let mut spans = vec![
        Span::styled(
            format!("{:<PROVIDER_LABEL_WIDTH$}", "∞"),
            theme.style(Color::Indexed(color), Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    spans.extend(infinite_bar_spans(theme, bar_width));
    spans.push(Span::raw(" "));
    spans.push(Span::raw(" ".repeat(PROVIDER_VALUE_WIDTH)));
    spans
}

/// The bar's cell width inside a provider `region`: the region less the label,
/// the value column, and the two single-cell gaps that frame the bar. At least
/// one cell, so a narrow sidebar still paints a (short) bar.
fn provider_bar_width(region: usize) -> usize {
    region
        .saturating_sub(PROVIDER_LABEL_WIDTH + 1 + 1 + PROVIDER_VALUE_WIDTH)
        .max(1)
}

/// Pad (or clip) a string to exactly `width` terminal cells — the fixed art
/// column, so the right column starts at one shared cell for every block.
fn pad_to(value: &str, width: usize) -> String {
    let count = value.chars().count();
    if count >= width {
        value.chars().take(width).collect()
    } else {
        let mut padded = value.to_owned();
        padded.extend(std::iter::repeat_n(' ', width - count));
        padded
    }
}
