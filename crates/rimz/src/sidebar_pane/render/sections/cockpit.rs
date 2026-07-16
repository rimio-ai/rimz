//! The cockpit's two summary lines: headline sessions + token breakdown, and
//! the live-agent count with the animated count-up spend.

use crate::agents::AgentStatus;
use crate::{DailyBudgetView, SpendWindow};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::config::GlyphRole;
use crate::sidebar_pane::render::TallyAnim;
use crate::sidebar_pane::render::fmt::{dollars_cap, dollars2, tokens_int};
use crate::sidebar_pane::render::labels::{TokenColumns, TokenDetail, token_breakdown_spans};
use crate::sidebar_pane::render::theme::{Component, Theme};

use super::{metric_spans, pin_right, spans_width};

/// The cockpit's first summary line, directly beneath the repo identity:
/// `◎ {sessions}` — the threads that have run in the configured headline
/// window, the glyph in the teal it shares with the W/M store rows — on the
/// left, with the headline token breakdown `◇ ↘ ↗ ◌` (integer magnitudes, the
/// live coarse form) pinned to the right edge. The breakdown reads the
/// workspace-scoped JSONL tally's headline window and paints zeroes before any
/// spend arrives. The live-agent count and the spend ride the second line
/// ([`cockpit_spend_line`]).
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
    let right = token_breakdown_spans(
        theme,
        window.tokens,
        window.input,
        window.output,
        window.cache_read,
        tokens_int,
        TokenDetail::Full,
        &TokenColumns::default(),
    );
    pin_right(left, right, width)
}

pub(in crate::sidebar_pane::render) struct CockpitBadges {
    pub unread_agents: usize,
    pub unread_picked: bool,
    pub open_prs: usize,
    pub pr_picked: bool,
}

pub(in crate::sidebar_pane::render) struct CockpitChipHits {
    pub unread: Option<(u16, u16)>,
    pub open_pr: Option<(u16, u16)>,
}

/// The cockpit's second summary line: `¤ {live} ({unread}) {⑃ open-PRs}` — the
/// agents in the room right now, the glyph in the agents' own working clay —
/// on the left, with headline fleet spend pinned to the right edge, counting up
/// as a turn lands. The steady unread count and open-PR count are
/// click-to-filter targets and paint as picked chips while their lenses are
/// active. The open-PR count uses the lane markers' PR-open tone and appears
/// only when an agent lane's branch has an open PR. The figure ticks toward the
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
) -> (Line<'static>, CockpitChipHits) {
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
    let mut unread_range = None;
    let CockpitBadges {
        unread_agents,
        unread_picked,
        open_prs,
        pr_picked,
    } = badges;
    if unread_agents > 0 {
        // A steady tally, not a blink — the attention blink lives on the cards
        // and the make-up buckets; the cockpit count holds its attention tone.
        let waiting = theme.animations.status(AgentStatus::Waiting).color();
        let style = if unread_picked {
            theme.picked_chip(waiting, Modifier::BOLD)
        } else {
            theme.style(waiting, Modifier::BOLD)
        };
        left.push(Span::styled(" ".to_owned(), theme.body()));
        let start = spans_width(&left);
        left.push(Span::styled(format!("({unread_agents})"), style));
        let end = spans_width(&left);
        let left_budget = width.saturating_sub(right_width + 1);
        unread_range = (end <= left_budget).then_some((start as u16, end as u16));
    }
    let mut open_pr_range = None;
    if open_prs > 0 {
        left.push(Span::styled(" ".to_owned(), theme.body()));
        let start = spans_width(&left);
        let mut pr_spans = metric_spans(
            theme,
            theme.glyph(GlyphRole::CockpitPrOpen),
            theme.component(Component::WorktreePrOpen),
            &open_prs.to_string(),
        );
        if pr_picked {
            let style =
                theme.picked_chip(theme.component(Component::WorktreePrOpen), Modifier::BOLD);
            for span in &mut pr_spans {
                span.style = style;
            }
        }
        left.extend(pr_spans);
        let end = spans_width(&left);
        let left_budget = width.saturating_sub(right_width + 1);
        open_pr_range = (end <= left_budget).then_some((start as u16, end as u16));
    }
    (
        pin_right(left, right, width),
        CockpitChipHits {
            unread: unread_range,
            open_pr: open_pr_range,
        },
    )
}
