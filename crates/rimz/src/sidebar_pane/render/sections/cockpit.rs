//! The cockpit's two summary lines: headline sessions + token breakdown, and
//! the live-agent count with the animated count-up spend.

use crate::SpendWindow;
use crate::store::snapshot::DailyBudgetView;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::config::GlyphRole;
use crate::sidebar_pane::render::TallyAnim;
use crate::sidebar_pane::render::fmt::{compact_count, dollars_cap, dollars2};
use crate::sidebar_pane::render::labels::{TokenColumns, TokenDetail, token_breakdown_spans};
use crate::sidebar_pane::render::theme::{Component, Theme};

use super::{metric_spans, pin_right, spans_width};

/// The cockpit's first summary line, directly beneath the repo identity:
/// `◎ {sessions}` — the threads that have run in the configured headline
/// window, the glyph in the teal it shares with the W/M store rows — on the
/// left, with the headline token breakdown `◇ ↘ ↗ ◌` (integer magnitudes, the
/// live coarse form) pinned to the right edge, narrowed to the `◇` total and
/// `◌` cache read when the full breakdown does not fit beside the sessions
/// count. The breakdown reads the workspace-scoped JSONL tally's headline
/// window and paints zeroes before any spend arrives. The live-agent count
/// and the spend ride the second line ([`cockpit_spend_line`]).
pub(in crate::sidebar_pane::render) fn cockpit_summary_line(
    theme: &Theme,
    sessions: u32,
    headline: Option<&SpendWindow>,
    width: usize,
) -> Line<'static> {
    let left = metric_spans(
        theme,
        theme.glyph(GlyphRole::CockpitSessions),
        theme.component(Component::Sessions),
        &sessions.to_string(),
    );
    let zero = SpendWindow::default();
    let window = headline.unwrap_or(&zero);
    let breakdown = |detail| {
        token_breakdown_spans(
            theme,
            window.tokens,
            window.input,
            window.output,
            window.cache_read,
            compact_count,
            detail,
            &TokenColumns::default(),
        )
    };
    let full = breakdown(TokenDetail::Full);
    let right = if spans_width(&left) + 1 + spans_width(&full) <= width {
        full
    } else {
        breakdown(TokenDetail::Summary)
    };
    pin_right(left, right, width)
}

pub(in crate::sidebar_pane::render) struct CockpitBadges {
    pub open_prs: usize,
    /// Worst open-PR tone: failed/dequeued, conflicting, pending, all passing, unknown.
    pub open_pr_attention: Component,
    pub pr_picked: bool,
}

/// The cockpit's second summary line: `¤ {live} {⑃ open-PRs}` — the
/// agents in the room right now, the glyph in the agents' own working clay —
/// on the left, with headline fleet spend pinned to the right edge, counting up
/// as a turn lands. The open-PR count is a click-to-filter target and paints as
/// a picked chip while its lens is active. Its worst open-PR tone is red for
/// failed CI or a dequeue, caution for a current-head conflict, yellow for
/// pending CI, green only when every open PR passes CI, and blue otherwise.
/// It appears only when a group's branch has an open PR. The figure ticks toward the
/// workspace tally's headline total via the shared [`TallyAnim`] roll — big decaying steps, then
/// penny by penny onto the exact figure — and brightens for a beat the instant
/// it settles (the W/M store rows below stay static). Always present — an empty
/// room reads `¤ 0` with `$0.00` on the right edge. A tripped room cap switches
/// the right edge to alarm-red local-day spend plus `of $CAP/day`, independent
/// of the headline window.
pub(in crate::sidebar_pane::render) fn cockpit_spend_line(
    theme: &Theme,
    live_agents: usize,
    badges: CockpitBadges,
    spend: (f64, Option<&DailyBudgetView>),
    anim: &TallyAnim,
    phase: u64,
    width: usize,
) -> (Line<'static>, Option<(u16, u16)>) {
    let (today_usd, daily_budget) = spend;
    let usd = anim.today_usd.display(today_usd, phase);
    let (label, style) = match daily_budget {
        Some(budget) => (
            format!("{} of {}/day", dollars2(usd), dollars_cap(budget.cap_usd)),
            theme.alarm(Modifier::BOLD),
        ),
        None => (
            dollars2(usd),
            if anim.today_usd.flashing(phase) {
                theme.value_flash()
            } else {
                theme.money_style(Modifier::BOLD)
            },
        ),
    };
    let right = vec![Span::styled(label, style)];
    let right_width = spans_width(&right);
    let mut left = metric_spans(
        theme,
        theme.glyph(GlyphRole::CockpitAgents),
        theme.clay(),
        &live_agents.to_string(),
    );
    let CockpitBadges {
        open_prs,
        open_pr_attention,
        pr_picked,
    } = badges;
    let mut open_pr_range = None;
    if open_prs > 0 {
        let tone = theme.component(open_pr_attention);
        left.push(Span::styled(" ".to_owned(), theme.body()));
        let start = spans_width(&left);
        let mut pr_spans = metric_spans(
            theme,
            theme.glyph(GlyphRole::CockpitPrOpen),
            tone,
            &open_prs.to_string(),
        );
        if pr_picked {
            let style = theme.picked_chip(tone, Modifier::BOLD);
            for span in &mut pr_spans {
                span.style = style;
            }
        }
        left.extend(pr_spans);
        let end = spans_width(&left);
        let left_budget = width.saturating_sub(right_width + 1);
        open_pr_range = (end <= left_budget).then_some((start as u16, end as u16));
    }
    (pin_right(left, right, width), open_pr_range)
}
