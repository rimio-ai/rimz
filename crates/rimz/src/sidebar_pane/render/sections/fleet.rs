//! The fleet make-up line — the cockpit's status buckets — and the first-run
//! hint an empty room shows in its place.

use crate::SidebarWorktreeGroup;
use crate::feed::AgentStatus;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::sidebar_pane::render::fmt::age_secs;
use crate::sidebar_pane::render::labels::{age_heat, agent_style, status_glyph, status_style};
use crate::sidebar_pane::render::theme::{ORANGE, Theme};

use super::{TAB_INK, pin_right, trim_spans_to_width};

/// One clickable status bucket in the cockpit make-up line: the line index
/// within [`fleet_header_lines`]'s returned lines (always 0 — the make-up is
/// one row), the half-open column range the bucket's footprint occupies
/// relative to the unpadded content, and the status it filters the body to.
/// `compose_lines` translates the position to absolute screen coordinates
/// (the cockpit base and the one-cell chrome gutter) before storing it on
/// `UiState::make_up_hits` for the mouse hit-test. A zero-count bucket emits
/// no hit — inert, as if not a tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MakeUpHit {
    pub(crate) line: usize,
    pub(crate) col_start: u16,
    pub(crate) col_end: u16,
    pub(crate) status: AgentStatus,
}

/// The fixed fleet header — the cockpit's make-up line, below the repo
/// dashboard's identity and `¤`/`◎`/spend lines. One line when the room has
/// agents, nothing when it does not (the `¤ 0` count lives on the summary above),
/// so the body below never shifts vertically as agents change *state*:
///
/// ```text
/// ? 2   ! 1   ⏸ 0   ✓ 4                        ⢿ 3   ○ 2   make-up: left · right
/// ```
///
/// The line splits the make-up by who might want you. The left cluster is the
/// rows worth a glance — `waiting` `?` and `failed` `!` (each wearing its
/// oldest row's age heat over a yellow floor), parked `paused` `⏸` (held
/// amber, never heating), then `success` `✓`. The right cluster is the
/// live-capacity tail — working `⢿` (every running agent; the thinking sparkle
/// is a per-row animation head, not a bucket), then a free `idle` `○`. Every
/// bucket renders — the glyph always in its semantic color, a zero
/// count at the soft gray beside it. Counts span the capped agents
/// (`status_counts`). The
/// fleet's live time / token / commit totals are gone — the summary line's
/// today-accumulated breakdown carries the fleet's resource read.
///
/// Every non-zero bucket is also a click-to-filter target, so the line returns
/// its [`MakeUpHit`]s alongside — emitted in lockstep with the spans, columns
/// relative to the unpadded content. The `filter` is the active pick: that
/// bucket paints the same `glyph count` cells as rest (ink on its semantic
/// fill, bold), so moving the pick changes style without moving text. Under
/// `NO_COLOR`, where the fill drops, reverse-video marks the same fixed cells.
pub(in crate::sidebar_pane::render) fn fleet_header_lines(
    theme: &Theme,
    groups: &[SidebarWorktreeGroup],
    filter: Option<AgentStatus>,
    width: usize,
) -> (Vec<Line<'static>>, Vec<MakeUpHit>) {
    let working = status_total(groups, AgentStatus::Running);
    let waiting = status_total(groups, AgentStatus::Waiting);
    let failed = status_total(groups, AgentStatus::Failed);
    let paused = status_total(groups, AgentStatus::Paused);
    let idle = status_total(groups, AgentStatus::Idle);
    let success = status_total(groups, AgentStatus::Success);
    let total = working + waiting + failed + paused + idle + success;

    // An empty (or process-only) room has no make-up line — the `¤ 0  ◎ 0` summary
    // lives on the dashboard above. The make-up line is reserved for a room that
    // has agents to summarize.
    if total == 0 {
        return (Vec::new(), Vec::new());
    }

    // Top line — the make-up split by who might want you. The left cluster gathers
    // the rows worth a glance: `waiting` `?` and `failed` `!` (the oldest row's
    // heat over a yellow floor), parked `paused` `⏸`, then success. The right
    // cluster is the live-capacity tail: working, then idle. Every bucket shows
    // its count.
    let mut left = Cluster::new(theme, filter);
    left.push_count(
        AgentStatus::Waiting,
        Color::Yellow,
        waiting,
        attention_bucket_style(theme, groups, AgentStatus::Waiting),
    );
    left.push_count(
        AgentStatus::Failed,
        Color::Red,
        failed,
        attention_bucket_style(theme, groups, AgentStatus::Failed),
    );
    // Paused stays with the attention-class cluster, after `?` / `!`: parked,
    // but still a row worth spotting. It renders like every other bucket — the
    // amber glyph with a faint `0` when empty — so the make-up stays a fixed
    // dashboard, scannable by position. It takes the held-amber resting tone
    // (`status_style`), never the heating `attention_bucket_style`, since there
    // is nothing to do until the provider recovers or the window resets.
    left.push_count(
        AgentStatus::Paused,
        Color::Yellow,
        paused,
        status_style(theme, AgentStatus::Paused),
    );
    left.push_count(
        AgentStatus::Success,
        Color::Green,
        success,
        status_style(theme, AgentStatus::Success),
    );
    let mut right = Cluster::new(theme, filter);
    right.push_count(
        AgentStatus::Running,
        ORANGE,
        working,
        agent_style(theme, AgentStatus::Running),
    );
    right.push_count(
        AgentStatus::Idle,
        Color::Green,
        idle,
        status_style(theme, AgentStatus::Idle),
    );

    // Split left / right when both clusters fit; on a narrow sidebar (the right
    // cluster alone can outrun the width) fall back to one left-packed line so the
    // attention buckets stay intact and the live-capacity tail clips, rather
    // than crushing `? 0  ! 0` down to a stub. The left hits are already
    // content-absolute (the cluster starts at column 0); the right hits shift to
    // wherever the layout lands their cluster — `pin_right` packs it against the
    // right edge, the fallback appends it after the left cluster and its gap.
    let mut hits = left.hits;
    let buckets = if left.col + 1 + right.col <= width {
        offset_hits(&mut hits, right.hits, width - right.col);
        pin_right(left.spans, right.spans, width)
    } else {
        let mut spans = left.spans;
        let mut gap = 0;
        if !spans.is_empty() && !right.spans.is_empty() {
            spans.push(Span::raw("   "));
            gap = 3;
        }
        offset_hits(&mut hits, right.hits, left.col + gap);
        spans.extend(right.spans);
        Line::from(trim_spans_to_width(spans, width))
    };
    // A bucket the width clipped keeps no hit — drop it whole rather than leave
    // a target pointing past the visible edge, the rail's drop-whole-tab rule.
    hits.retain(|hit| usize::from(hit.col_end) <= width);

    (vec![buckets], hits)
}

/// Append a right-cluster hit run onto the absolute hit list, shifted by the
/// column where the layout landed the cluster.
fn offset_hits(hits: &mut Vec<MakeUpHit>, cluster: Vec<MakeUpHit>, offset: usize) {
    hits.extend(cluster.into_iter().map(|hit| MakeUpHit {
        col_start: hit.col_start + offset as u16,
        col_end: hit.col_end + offset as u16,
        ..hit
    }));
}

/// One make-up cluster under construction: the spans, the running column the
/// hit geometry is read from (`col` doubles as the cluster's width — every
/// make-up glyph is single-cell), and one [`MakeUpHit`] per non-zero bucket,
/// emitted in lockstep with the spans so the click targets can never drift
/// from the paint.
struct Cluster<'a> {
    theme: &'a Theme,
    filter: Option<AgentStatus>,
    spans: Vec<Span<'static>>,
    hits: Vec<MakeUpHit>,
    col: usize,
}

impl<'a> Cluster<'a> {
    fn new(theme: &'a Theme, filter: Option<AgentStatus>) -> Self {
        Self {
            theme,
            filter,
            spans: Vec::new(),
            hits: Vec::new(),
            col: 0,
        }
    }

    /// Append a `glyph n` bucket, spaced from the previous one. The glyph and
    /// its count are always separated by a single space (`? 2`, never `?2`);
    /// successive buckets are separated by three. Every bucket renders, so a
    /// zero reads `? 0` — the cockpit is a fixed dashboard, scannable by
    /// position. The glyph always wears its semantic color, so the make-up
    /// reads as a stable colored legend; a zero bucket rests the glyph (no
    /// bold, no heat), reads its count at the soft stat tier, and emits no hit
    /// — inert, as if not a tab. The active filter's bucket paints the fixed
    /// `glyph count` footprint as a chip (`TAB_INK` on the glyph's semantic
    /// fill, bold); under `NO_COLOR` it keeps the footprint and adds reverse
    /// video because there is no fill color to carry the pick.
    fn push_count(&mut self, status: AgentStatus, glyph_color: Color, count: usize, style: Style) {
        if !self.spans.is_empty() {
            self.spans.push(Span::raw("   "));
            self.col += 3;
        }
        let glyph = status_glyph(status);
        let start = self.col;
        if self.filter == Some(status) && count > 0 {
            let chip = self.theme.chip(TAB_INK, glyph_color, Modifier::BOLD);
            let pick = if chip.bg.is_none() {
                chip.add_modifier(Modifier::REVERSED)
            } else {
                chip
            };
            self.push_span(Span::styled(format!("{glyph} {count}"), pick));
        } else if count == 0 {
            self.push_span(Span::styled(
                glyph.to_owned(),
                self.theme.style(glyph_color, Modifier::empty()),
            ));
            self.push_span(Span::styled(format!(" {count}"), self.theme.soft()));
            return;
        } else {
            self.push_span(Span::styled(format!("{glyph} {count}"), style));
        }
        self.hits.push(MakeUpHit {
            line: 0,
            col_start: start as u16,
            col_end: self.col as u16,
            status,
        });
    }

    fn push_span(&mut self, span: Span<'static>) {
        self.col += span.content.chars().count();
        self.spans.push(span);
    }
}

/// The fleet head-count read by the dashboard's L2: `(main, subs)` — the main
/// agents you launched (the sum of the capped per-worktree `status_counts`, so it
/// matches the cockpit make-up below) and the subagents they spawned this turn.
pub(in crate::sidebar_pane::render) fn fleet_size(
    groups: &[SidebarWorktreeGroup],
) -> (usize, usize) {
    let main = groups
        .iter()
        .flat_map(|group| &group.status_counts)
        .map(|count| count.count)
        .sum();
    let subs = groups
        .iter()
        .flat_map(|group| &group.rows)
        .map(|row| row.sub_agents().len())
        .sum();
    (main, subs)
}

/// The cockpit attention bucket's tone: bold, wearing the oldest contributing
/// row's [`age_heat`] over the same yellow floor as the per-row glyph — the
/// aggregate echo of
/// [`attention_glyph_style`](crate::sidebar_pane::render::labels::attention_glyph_style)'s
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
        .filter(|row| row.status() == Some(status))
        .map(|row| age_secs(row.last_activity))
        .max()
        .unwrap_or(0);
    theme.style(age_heat(oldest).unwrap_or(Color::Yellow), Modifier::BOLD)
}

/// The full-fleet count for one make-up bucket — the sum of every group's
/// `status_counts` entry for `status`, exactly the figure the make-up line
/// displays. The auto-clear in `app::reconcile_selection` reads the same sum,
/// so a filter ends in the same fold its bucket reads zero.
pub(crate) fn status_total(groups: &[SidebarWorktreeGroup], status: AgentStatus) -> usize {
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
pub(in crate::sidebar_pane::render) fn first_run_hint_lines(
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
