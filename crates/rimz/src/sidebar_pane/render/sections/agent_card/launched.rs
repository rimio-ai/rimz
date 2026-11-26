//! Launched peers on an agent card: the `▶ launched (N)` half of the section line and one
//! entry per agent or team the card's agent launched directly, drawn as a fired booking's
//! entry is. A running entry reads on one line, an agent its card's text and a team its
//! board stage, its runtime pinned right, and targets its pane; one whose card has not
//! landed reads its own text over `starting`, and a finished one its own text over when it
//! ended and its pull request. Which entries list is the section's paging rule.

use crate::store::snapshot::{BookingStarts, SidebarLaunched};

use super::bookings::{ended_text, runtime_since};
use super::*;

/// A running entry whose lead is working, for the fast animation grid.
pub(super) fn launched_in_motion(entry: &SidebarLaunched) -> bool {
    entry
        .live
        .as_ref()
        .is_some_and(|live| live.status == AgentStatus::Running)
}

/// The `▶ launched (N)` half, `▶ N` unless `long`.
pub(super) fn launched_half(theme: &Theme, count: usize, long: bool) -> [Span<'static>; 2] {
    let label = if long {
        format!(" launched ({count})")
    } else {
        format!(" {count}")
    };
    [
        Span::styled(
            theme.glyph(GlyphRole::CardLaunched).to_owned(),
            theme.styled(Component::LaunchedHeader, Modifier::empty()),
        ),
        Span::styled(label, theme.body()),
    ]
}

/// One entry per launch in `rows`, named for what it starts (`coder#lane`,
/// `team forge#lane`).
pub(super) fn launched_entry_lines(
    ctx: &RowCtx<'_>,
    rows: &[&SidebarLaunched],
    selected: bool,
) -> Vec<CardLine> {
    let mut lines = Vec::new();
    for launched in rows {
        let pane = launched.live_pane();
        let focused = selected
            && pane
                .is_some_and(|pane| ctx.entry_cursor == Some(&EntryCursor::SubAgent(pane.clone())));
        let mut entry_lines = Vec::new();
        push_entry(
            ctx,
            &mut entry_lines,
            launched_entry(ctx, launched, focused),
        );
        lines.extend(entry_lines.into_iter().map(|line| CardLine {
            line,
            target: pane.cloned().map(HitTarget::SubAgentPane),
        }));
    }
    lines
}

fn launched_entry(ctx: &RowCtx<'_>, launched: &SidebarLaunched, focused: bool) -> Entry {
    let theme = ctx.theme;
    let live = launched.live.as_ref();
    let lead = match live {
        Some(live) => Span::styled(
            agent_glyph(theme, live.status, live.phase, ctx.head_step),
            agent_role_style_at(theme, live.status, live.phase, ctx.animation_phase),
        ),
        None if launched.starting => Span::styled(
            theme.glyph(GlyphRole::CardLaunched).to_owned(),
            theme.styled(Component::LaunchedStarting, Modifier::empty()),
        ),
        None => Span::styled(
            theme.glyph(GlyphRole::CardBookingEnded).to_owned(),
            theme.styled(Component::LaunchedEnded, Modifier::empty()),
        ),
    };
    let headline_style = if live.is_some_and(|live| live.pane.is_some()) {
        theme.body()
    } else {
        theme.muted()
    };
    let lane = launched.lane.as_deref();
    let who = match &launched.starts {
        BookingStarts::Team(name) => team_who(theme, Some(name), lane),
        BookingStarts::Profile(name) => agent_who(
            theme,
            Span::styled(
                name.clone(),
                theme.styled(Component::EntryAgentName, Modifier::empty()),
            ),
            lane,
        ),
    };
    let text = |text: Option<&str>| {
        text.map(|text| Span::styled(text.to_owned(), headline_style))
            .into_iter()
            .collect::<Vec<_>>()
    };
    let headline = match (live, &launched.starts) {
        (Some(live), BookingStarts::Team(_)) => match &live.stage {
            Some(stage) => stage_spans(theme, &stage.stage, stage.done),
            None => vec![Span::styled("stage unknown", theme.muted())],
        },
        (Some(live), BookingStarts::Profile(_)) => text(
            live.description
                .as_deref()
                .or(launched.description.as_deref()),
        ),
        (None, _) => text(launched.description.as_deref()),
    };
    let clock = runtime_since(ctx, launched.at, live.is_some(), launched.ended_at);
    Entry {
        focused,
        lead,
        who,
        headline,
        marker: Vec::new(),
        clip_who: live.is_some() && matches!(launched.starts, BookingStarts::Team(_)),
        right: clock
            .clone()
            .filter(|_| live.is_some())
            .into_iter()
            .collect(),
        detail: live
            .is_none()
            .then(|| ended_line(ctx, launched, clock, focused)),
    }
}

/// A second line under an entry with nothing alive: `starting`, else when it ended and its
/// pull request with its frozen runtime pinned right.
fn ended_line(
    ctx: &RowCtx<'_>,
    launched: &SidebarLaunched,
    clock: Option<Span<'static>>,
    focused: bool,
) -> Line<'static> {
    let theme = ctx.theme;
    let width = content_width(ctx.width);
    let mut left = entry_indent(theme, focused);
    left.push(Span::raw("  "));
    let budget = width
        .saturating_sub(spans_width(&left) + clock.as_ref().map_or(0, |clock| clock.width() + 1));
    let text = if launched.starting {
        ellipsize("starting", budget)
    } else {
        ended_text(ctx, launched.ended_at, launched.pr, budget)
    };
    left.push(Span::styled(text, theme.muted()));
    pin_right(left, clock.into_iter().collect(), width)
}
