//! The fleet make-up line — the cockpit's status buckets.

use crate::agents::AgentStatus;
use crate::store::snapshot::{SidebarWorktreeGroup, WorktreePrCi, WorktreePrState};
use jiff::Timestamp;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::sidebar_pane::render::fmt::age_secs;
use crate::sidebar_pane::render::labels::{
    attention_cell_style, status_chip_color, status_glyph, status_rest_style, unread_anim,
};
use crate::sidebar_pane::render::theme::Theme;
use crate::sidebar_pane::render::{BodyFilter, HitRegion, HitTarget};

use super::{pin_right, trim_spans_to_width};

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
/// rows worth a glance — `waiting` `?`, `failed` `!`, parked `paused` `⏸`, and
/// `success` `✓` (each blinking only while a visible matching row is unread;
/// read buckets hold their fixed status tone — `?` yellow, `!` red, `⏸` blue,
/// `✓` green — at any age). The
/// right cluster is the
/// live-capacity tail — working `⢿` (every running agent; the thinking head
/// is a per-row animation head, not a bucket), then a free `idle` `○`. Every
/// bucket renders; colored statuses use their semantic tone, idle rests at the
/// soft stat gray, and every zero count sits at the same soft gray beside its
/// glyph.
/// Counts span the full snapshot roster (`status_counts`). The
/// fleet's live time / token / commit totals are gone — the summary line's
/// today-accumulated breakdown carries the fleet's resource read.
///
/// Every non-zero bucket is also a click-to-filter target, so the line returns
/// its typed [`HitRegion`]s alongside — emitted in lockstep with the spans,
/// columns relative to the unpadded content. The `filter` is the active pick: that
/// bucket paints the same `glyph count` cells as rest (ink on a colored fill
/// where the status has one, plus the bucket's current weight), so moving the
/// pick changes style without moving text. Under `NO_COLOR`, or for the soft
/// idle bucket, reverse-video marks the same fixed cells.
pub(in crate::sidebar_pane::render) fn fleet_header_lines(
    theme: &Theme,
    groups: &[SidebarWorktreeGroup],
    now: Timestamp,
    filter: Option<BodyFilter>,
    animation_phase: u64,
    width: usize,
    lead_unread_status: Option<AgentStatus>,
) -> (Vec<Line<'static>>, Vec<HitRegion>) {
    let status_filter = match filter {
        Some(BodyFilter::Status(status)) => Some(status),
        Some(BodyFilter::Unread | BodyFilter::OpenPr) | None => None,
    };
    let buckets = [
        (AgentStatus::Waiting, BucketCluster::Left),
        (AgentStatus::Failed, BucketCluster::Left),
        (AgentStatus::Paused, BucketCluster::Left),
        (AgentStatus::Success, BucketCluster::Left),
        (AgentStatus::Running, BucketCluster::Right),
        (AgentStatus::Idle, BucketCluster::Right),
    ];
    let total = buckets
        .iter()
        .map(|(status, _)| BodyFilter::Status(*status).total(groups))
        .sum::<usize>();

    // An empty (or process-only) room has no make-up line — the `¤ 0  ◎ 0` summary
    // lives on the dashboard above. The make-up line is reserved for a room that
    // has agents to summarize.
    if total == 0 {
        return (Vec::new(), Vec::new());
    }

    // Top line — the make-up split by who might want you. The left cluster
    // gathers the rows worth a glance: `waiting` `?` and `failed` `!` (unread
    // rows blink; read rows rest on their fixed status tone), parked
    // `paused` `⏸`, then success. The right cluster is the live-capacity tail:
    // working, then idle. Every bucket shows its count.
    let mut left = Cluster::new(theme, status_filter);
    let mut right = Cluster::new(theme, status_filter);
    for (status, target) in buckets {
        let cluster = match target {
            BucketCluster::Left => &mut left,
            BucketCluster::Right => &mut right,
        };
        cluster.push_count(
            status,
            status_chip_color(theme, status),
            BodyFilter::Status(status).total(groups),
            bucket_style(
                theme,
                groups,
                status,
                bucket_tone(theme, status),
                now,
                animation_phase,
                lead_unread_status == Some(status),
            ),
        );
    }

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
    hits.retain(|hit| usize::from(hit.columns.end) <= width);

    (vec![buckets], hits)
}

/// Append a right-cluster hit run onto the absolute hit list, shifted by the
/// column where the layout landed the cluster.
fn offset_hits(hits: &mut Vec<HitRegion>, cluster: Vec<HitRegion>, offset: usize) {
    hits.extend(cluster.into_iter().map(|hit| HitRegion {
        columns: hit.columns.start + offset as u16..hit.columns.end + offset as u16,
        ..hit
    }));
}

/// One make-up cluster under construction: the spans, the running column the
/// hit geometry is read from (`col` doubles as the cluster's width — every
/// make-up glyph is single-cell), and one [`HitRegion`] per non-zero bucket,
/// emitted in lockstep with the spans so the click targets can never drift
/// from the paint.
struct Cluster<'a> {
    theme: &'a Theme,
    filter: Option<AgentStatus>,
    spans: Vec<Span<'static>>,
    hits: Vec<HitRegion>,
    col: usize,
}

#[derive(Clone, Copy)]
enum BucketCluster {
    Left,
    Right,
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
    /// position. Colored statuses wear their semantic tone; idle reads at the
    /// soft stat tier. A zero bucket rests the glyph (no bold, no heat), reads
    /// its count at the soft stat tier, and emits no hit — inert, as if not a
    /// tab. The active filter's bucket paints the fixed `glyph count` footprint
    /// as a chip for colored statuses; idle keeps the soft gray and adds reverse
    /// video plus weight, preserving the fixed idle head.
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
                self.theme.picked_chip(glyph_color, Modifier::empty())
            } else {
                style.add_modifier(Modifier::REVERSED)
            };
            self.push_span(Span::styled(
                format!("{glyph} {count}"),
                pick.add_modifier(weight),
            ));
        } else if count == 0 {
            let glyph_style = if status == AgentStatus::Idle {
                self.theme.body()
            } else {
                status_rest_style(self.theme, status)
            };
            self.push_span(Span::styled(glyph.to_owned(), glyph_style));
            self.push_span(Span::styled(format!(" {count}"), self.theme.body()));
            return;
        } else {
            self.push_span(Span::styled(format!("{glyph} {count}"), style));
        }
        self.hits.push(HitRegion::line(
            0,
            start as u16..self.col as u16,
            HitTarget::BodyFilter(BodyFilter::Status(status)),
        ));
    }

    fn push_span(&mut self, span: Span<'static>) {
        self.col += span.width();
        self.spans.push(span);
    }
}

/// The fleet head-count read by the dashboard's L2: `(main, subs)` — the main
/// agents you launched (the sum of the per-worktree `status_counts`, so it
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

fn bucket_tone(theme: &Theme, status: AgentStatus) -> (Style, Option<Color>) {
    match status {
        AgentStatus::Waiting | AgentStatus::Failed => {
            let color = theme.animations.status(status).color();
            (theme.style(color, Modifier::empty()), Some(color))
        }
        AgentStatus::Paused | AgentStatus::Success | AgentStatus::Running => (
            status_rest_style(theme, status),
            status_chip_color(theme, status),
        ),
        AgentStatus::Idle => (theme.body(), status_chip_color(theme, status)),
    }
}

/// One cockpit bucket's tone, in lockstep with its rows. A `?`/`!` bucket owns
/// the lead unread row when `is_lead_bucket`, so it carries the configured
/// continuous attention signal with that row; any unread bucket that is *not*
/// the lead settles to the steady bright crest, and a read bucket holds its
/// `rest_style`. Reads the full roster, so a hidden unread row still drives its
/// cockpit bucket while the row itself remains reachable through the unread lens
/// or group expansion.
fn bucket_style(
    theme: &Theme,
    groups: &[SidebarWorktreeGroup],
    status: AgentStatus,
    tone: (Style, Option<Color>),
    now: Timestamp,
    animation_phase: u64,
    is_lead_bucket: bool,
) -> Style {
    let (rest_style, color) = tone;
    let oldest_unread = groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.status() == Some(status) && row.unread)
        .map(|row| age_secs(row.last_activity, now))
        .max();
    if let Some(age) = oldest_unread {
        return match unread_anim(theme, status, age, animation_phase, is_lead_bucket) {
            Some(anim) => attention_cell_style(theme, color, anim, 0, 1),
            None => color.map_or_else(
                || rest_style.add_modifier(Modifier::BOLD),
                |color| theme.style(color, Modifier::BOLD),
            ),
        };
    }
    rest_style
}

pub(in crate::sidebar_pane::render) fn open_pr_total(groups: &[SidebarWorktreeGroup]) -> usize {
    groups
        .iter()
        .filter(|group| group.pr_state == Some(WorktreePrState::Open))
        .count()
}

pub(in crate::sidebar_pane::render) fn open_pr_worst_ci(
    groups: &[SidebarWorktreeGroup],
) -> Option<WorktreePrCi> {
    let mut saw_open = false;
    let mut saw_unknown = false;
    let mut saw_pending = false;
    let mut saw_passing = false;
    for group in groups
        .iter()
        .filter(|group| group.pr_state == Some(WorktreePrState::Open))
    {
        saw_open = true;
        match group.pr_ci {
            Some(WorktreePrCi::Failing) => return Some(WorktreePrCi::Failing),
            Some(WorktreePrCi::Pending) => saw_pending = true,
            Some(WorktreePrCi::Passing) => saw_passing = true,
            None => saw_unknown = true,
        }
    }
    if saw_pending {
        return Some(WorktreePrCi::Pending);
    }
    (saw_open && saw_passing && !saw_unknown).then_some(WorktreePrCi::Passing)
}
