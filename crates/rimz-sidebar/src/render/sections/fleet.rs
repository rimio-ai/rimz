//! The fleet make-up line — the cockpit's status buckets — and the first-run
//! hint an empty room shows in its place.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use rimz::SidebarWorktreeGroup;
use rimz::feed::AgentStatus;

use crate::render::fmt::age_secs;
use crate::render::labels::{age_heat, agent_style, status_glyph, status_style};
use crate::render::theme::{ORANGE, Theme};

use super::{pin_right, spans_width, trim_spans_to_width};

/// The fixed fleet header — the cockpit's make-up line, below the repo
/// dashboard's identity and `¤`/`◎`/spend lines. One line when the room has
/// agents, nothing when it does not (the `¤ 0` count lives on the summary above),
/// so the body below never shifts vertically as agents change *state*:
///
/// ```text
/// ? 2   ! 1   ○ 2   ⏸ 0                        ⢿ 3   ✓ 4   make-up: left · right
/// ```
///
/// The line splits the make-up by who might want you. The left cluster is the
/// rows worth a glance — `waiting` `?` and `failed` `!` (each wearing its
/// oldest row's age heat over a yellow floor), a free `idle` `○` (calm
/// green, but grouped left because a free agent wants work), then a parked
/// `rate-limited` `⏸` (held amber, never heating) closing the cluster. The right
/// cluster is the busy/done tail — working `⢿` (every running agent; the
/// thinking sparkle is a per-row animation head, not a bucket), then `success`
/// `✓`. Every bucket renders — the glyph always in its semantic color, a zero
/// count faint beside it. Counts span the capped agents (`status_counts`). The
/// fleet's live time / token / commit totals are gone — the summary line's
/// today-accumulated breakdown carries the fleet's resource read.
pub(in crate::render) fn fleet_header_lines(
    theme: &Theme,
    groups: &[SidebarWorktreeGroup],
    width: usize,
) -> Vec<Line<'static>> {
    let working = status_total(groups, AgentStatus::Running);
    let waiting = status_total(groups, AgentStatus::Waiting);
    let failed = status_total(groups, AgentStatus::Failed);
    let rate_limited = status_total(groups, AgentStatus::RateLimited);
    let idle = status_total(groups, AgentStatus::Idle);
    let success = status_total(groups, AgentStatus::Success);
    let total = working + waiting + failed + rate_limited + idle + success;

    // An empty (or process-only) room has no make-up line — the `¤ 0  ◎ 0` summary
    // lives on the dashboard above. The make-up line is reserved for a room that
    // has agents to summarize.
    if total == 0 {
        return Vec::new();
    }

    // Top line — the make-up split by who might want you. The left cluster gathers
    // the rows worth a glance: `waiting` `?` and `failed` `!` (the oldest row's
    // heat over a yellow floor), a free `idle` `○` — calm green, but grouped
    // left because a free agent wants work — then a parked `rate-limited` `⏸`
    // closing the cluster. The right cluster is the busy/done tail: working,
    // then success. Every bucket shows its count.
    let mut left: Vec<Span<'static>> = Vec::new();
    push_count(
        theme,
        &mut left,
        status_glyph(AgentStatus::Waiting),
        Color::Yellow,
        waiting,
        attention_bucket_style(theme, groups, AgentStatus::Waiting),
    );
    push_count(
        theme,
        &mut left,
        status_glyph(AgentStatus::Failed),
        Color::Red,
        failed,
        attention_bucket_style(theme, groups, AgentStatus::Failed),
    );
    push_count(
        theme,
        &mut left,
        status_glyph(AgentStatus::Idle),
        Color::Green,
        idle,
        status_style(theme, AgentStatus::Idle),
    );
    // Rate-limited closes the left cluster, after the free `○` idle agent:
    // attention-class but parked. It renders like every other bucket — the
    // amber glyph with a faint `0` when empty — so the make-up stays a fixed
    // dashboard, scannable by position. It takes the held-amber resting tone
    // (`status_style`), never the heating `attention_bucket_style`, since there
    // is nothing to do but wait for the window to reset.
    push_count(
        theme,
        &mut left,
        status_glyph(AgentStatus::RateLimited),
        Color::Yellow,
        rate_limited,
        status_style(theme, AgentStatus::RateLimited),
    );
    let mut right: Vec<Span<'static>> = Vec::new();
    push_count(
        theme,
        &mut right,
        status_glyph(AgentStatus::Running),
        ORANGE,
        working,
        agent_style(theme, AgentStatus::Running),
    );
    push_count(
        theme,
        &mut right,
        status_glyph(AgentStatus::Success),
        Color::Green,
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

    vec![buckets]
}

/// The fleet head-count read by the dashboard's L2: `(main, subs)` — the main
/// agents you launched (the sum of the capped per-worktree `status_counts`, so it
/// matches the cockpit make-up below) and the subagents they spawned this turn.
pub(in crate::render) fn fleet_size(groups: &[SidebarWorktreeGroup]) -> (usize, usize) {
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

/// The cockpit attention bucket's tone: bold, wearing the oldest contributing
/// row's [`age_heat`] over the same yellow floor as the per-row glyph — the
/// aggregate echo of
/// [`attention_glyph_style`](crate::render::labels::attention_glyph_style)'s
/// escalation. Reads the rendered rows (capped-away agents are excluded — the
/// bucket count still spans them, but a hidden agent never drives the visible
/// heat).
fn attention_bucket_style(
    theme: &Theme,
    groups: &[SidebarWorktreeGroup],
    status: AgentStatus,
) -> Style {
    let oldest = groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.status == Some(status))
        .map(|row| age_secs(row.last_activity))
        .max()
        .unwrap_or(0);
    theme.style(age_heat(oldest).unwrap_or(Color::Yellow), Modifier::BOLD)
}

/// Append a `glyph n` bucket to a header line, spaced from the previous one. The
/// glyph and its count are always separated by a single space (`? 2`, never
/// `?2`); successive buckets are separated by three. Every bucket renders, so a
/// zero reads `? 0` — the cockpit is a fixed dashboard, scannable by position.
/// The glyph always wears its semantic color, so the make-up reads as a stable
/// colored legend; a zero bucket rests the glyph (no bold, no heat) and drops
/// only its count to faint chrome, so the eye still lands on the live counts.
fn push_count(
    theme: &Theme,
    spans: &mut Vec<Span<'static>>,
    glyph: &str,
    glyph_color: Color,
    count: usize,
    style: Style,
) {
    if !spans.is_empty() {
        spans.push(Span::raw("   "));
    }
    if count == 0 {
        spans.push(Span::styled(
            glyph.to_owned(),
            theme.style(glyph_color, Modifier::empty()),
        ));
        spans.push(Span::styled(format!(" {count}"), theme.faint()));
    } else {
        spans.push(Span::styled(format!("{glyph} {count}"), style));
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

/// Dim getting-started hint for a healthy room with no agent or feed rows.
/// Shell/editor process rows can still be present; the renderer suppresses
/// this cue once an agent-like process or product row appears.
///
/// The cue names the *real* next step. Until hooks are wired, running
/// claude/codex registers nothing, so an un-wired room points at `rimz hooks
/// install`; once wired (`hooks_ready`), it invites launching an agent.
pub(in crate::render) fn first_run_hint_lines(
    theme: &Theme,
    hooks_ready: bool,
) -> Vec<Line<'static>> {
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
