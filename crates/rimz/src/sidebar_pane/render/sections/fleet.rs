//! The fleet make-up line — the cockpit's status buckets.

use crate::SidebarWorktreeGroup;
use crate::feed::AgentStatus;
use jiff::Timestamp;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::sidebar_pane::render::fmt::age_secs;
use crate::sidebar_pane::render::labels::{
    age_heat, agent_style_at, attention_floor_color, hard_blink, status_chip_color, status_glyph,
    status_rest_style, status_style_at, status_style_with_modifier,
};
use crate::sidebar_pane::render::theme::Theme;

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
/// live-capacity tail — working `⢿` (every running agent; the thinking head
/// is a per-row animation head, not a bucket), then a free `idle` `○`. Every
/// bucket renders; colored statuses use their semantic tone, idle rests
/// neutral by default, and a zero count sits at the soft gray beside it.
/// Counts span the capped agents (`status_counts`). The
/// fleet's live time / token / commit totals are gone — the summary line's
/// today-accumulated breakdown carries the fleet's resource read.
///
/// Every non-zero bucket is also a click-to-filter target, so the line returns
/// its [`MakeUpHit`]s alongside — emitted in lockstep with the spans, columns
/// relative to the unpadded content. The `filter` is the active pick: that
/// bucket paints the same `glyph count` cells as rest (ink on a colored fill
/// where the status has one, plus the bucket's current weight), so moving the
/// pick changes style without moving text. Under `NO_COLOR`, or for neutral
/// idle, reverse-video marks the same fixed cells.
pub(in crate::sidebar_pane::render) fn fleet_header_lines(
    theme: &Theme,
    groups: &[SidebarWorktreeGroup],
    now: Timestamp,
    filter: Option<AgentStatus>,
    animation_phase: u64,
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
        status_chip_color(theme, AgentStatus::Waiting),
        waiting,
        unread_bucket_style(
            theme,
            groups,
            AgentStatus::Waiting,
            now,
            animation_phase,
            attention_bucket_style(theme, groups, AgentStatus::Waiting, now),
        ),
    );
    left.push_count(
        AgentStatus::Failed,
        status_chip_color(theme, AgentStatus::Failed),
        failed,
        unread_bucket_style(
            theme,
            groups,
            AgentStatus::Failed,
            now,
            animation_phase,
            attention_bucket_style(theme, groups, AgentStatus::Failed, now),
        ),
    );
    // Paused stays with the attention-class cluster, after `?` / `!`: parked,
    // but still a row worth spotting. It renders like every other bucket — the
    // amber glyph with a faint `0` when empty — so the make-up stays a fixed
    // dashboard, scannable by position. It takes the held-amber resting tone
    // (`status_style`), never the heating `attention_bucket_style`, since there
    // is nothing to do until the provider recovers or the window resets.
    left.push_count(
        AgentStatus::Paused,
        status_chip_color(theme, AgentStatus::Paused),
        paused,
        unread_bucket_style(
            theme,
            groups,
            AgentStatus::Paused,
            now,
            animation_phase,
            status_style_at(theme, AgentStatus::Paused, animation_phase),
        ),
    );
    left.push_count(
        AgentStatus::Success,
        status_chip_color(theme, AgentStatus::Success),
        success,
        unread_bucket_style(
            theme,
            groups,
            AgentStatus::Success,
            now,
            animation_phase,
            status_style_at(theme, AgentStatus::Success, animation_phase),
        ),
    );
    let mut right = Cluster::new(theme, filter);
    right.push_count(
        AgentStatus::Running,
        status_chip_color(theme, AgentStatus::Running),
        working,
        agent_style_at(theme, AgentStatus::Running, animation_phase),
    );
    right.push_count(
        AgentStatus::Idle,
        status_chip_color(theme, AgentStatus::Idle),
        idle,
        status_style_at(theme, AgentStatus::Idle, animation_phase),
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
    /// position. Colored statuses wear their semantic tone; idle rests
    /// neutral unless the user configured a tone. A zero bucket rests the glyph
    /// (no bold, no heat), reads its count at the soft stat tier, and emits no
    /// hit — inert, as if not a tab. The active filter's bucket paints the
    /// fixed `glyph count` footprint as a chip for colored statuses; neutral
    /// idle uses reverse video and weight only, preserving the no-color idle
    /// head.
    fn push_count(
        &mut self,
        status: AgentStatus,
        glyph_color: Option<Color>,
        count: usize,
        style: Style,
    ) {
        if !self.spans.is_empty() {
            self.spans.push(Span::raw("   "));
            self.col += 3;
        }
        let glyph = status_glyph(self.theme, status);
        let start = self.col;
        if self.filter == Some(status) && count > 0 {
            let weight = if style.add_modifier.is_empty() {
                Modifier::BOLD
            } else {
                style.add_modifier
            };
            let pick = if let Some(glyph_color) = glyph_color {
                let chip = self.theme.chip(TAB_INK, glyph_color, Modifier::empty());
                if chip.bg.is_none() {
                    chip.add_modifier(Modifier::REVERSED)
                } else {
                    chip
                }
            } else {
                Style::default().add_modifier(Modifier::REVERSED)
            };
            self.push_span(Span::styled(
                format!("{glyph} {count}"),
                pick.add_modifier(weight),
            ));
        } else if count == 0 {
            self.push_span(Span::styled(
                glyph.to_owned(),
                status_rest_style(self.theme, status),
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
        self.col += span.width();
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
/// [`agent_lead_style`](crate::sidebar_pane::render::labels::agent_lead_style)'s
/// escalation. Reads the rendered rows (capped-away agents are excluded — the
/// bucket count still spans them, but a hidden agent never drives the visible
/// heat).
fn attention_bucket_style(
    theme: &Theme,
    groups: &[SidebarWorktreeGroup],
    status: AgentStatus,
    now: Timestamp,
) -> Style {
    let oldest = groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.status() == Some(status))
        .map(|row| age_secs(row.last_activity, now))
        .max()
        .unwrap_or(0);
    theme.style(
        age_heat(oldest).unwrap_or_else(|| attention_floor_color(theme, status)),
        Modifier::BOLD,
    )
}

fn unread_bucket_style(
    theme: &Theme,
    groups: &[SidebarWorktreeGroup],
    status: AgentStatus,
    now: Timestamp,
    animation_phase: u64,
    base: Style,
) -> Style {
    let Some(oldest) = oldest_unread_age(groups, status, now) else {
        return base;
    };
    match status {
        AgentStatus::Waiting | AgentStatus::Failed => theme.style(
            age_heat(oldest).unwrap_or_else(|| attention_floor_color(theme, status)),
            hard_blink(animation_phase),
        ),
        AgentStatus::Paused | AgentStatus::Success | AgentStatus::Running | AgentStatus::Idle => {
            status_style_with_modifier(theme, status, hard_blink(animation_phase))
        }
    }
}

fn oldest_unread_age(
    groups: &[SidebarWorktreeGroup],
    status: AgentStatus,
    now: Timestamp,
) -> Option<i64> {
    groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.unread && row.status() == Some(status))
        .map(|row| age_secs(row.last_activity, now))
        .max()
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
