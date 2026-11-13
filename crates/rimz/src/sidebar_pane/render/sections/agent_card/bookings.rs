//! Booked launches on an agent card: the `▷ booked (N)` half of the section line, the
//! booked line a resting card keeps while its plan is unfinished, and one entry per
//! booking. The rows arrive in draw order with their bands, so this module adds no
//! ordering or health rule: `Later` entries count in `+N later` and `Older` ones in
//! `K more ended`, both static rows, and an entry takes `⚠` exactly when the loop gave
//! it a reason. Only the `⚠` segment of the section line takes the warning tone. A
//! fired entry with a live pane targets that pane on both lines; every other entry
//! selects the card.

use crate::store::snapshot::{
    BookingBand, BookingBlocker, BookingFire, BookingPr, BookingStarts, BookingState, SiblingState,
    SidebarBooking,
};

use super::*;
use crate::sidebar_pane::render::labels::value_seam;

/// The card holds a booking whose stage is not finished.
pub(super) fn open_plan(agent: &AgentCard) -> bool {
    agent.bookings.iter().any(|booking| !booking.is_finished())
}

pub(super) struct BookingBands<'a> {
    /// Every entry needing a fix, fired, or next, in draw order.
    pub(super) listed: Vec<&'a SidebarBooking>,
    /// `Later` entries, counted in `+N later`.
    pub(super) later: usize,
    /// `Older` entries, counted in `K more ended`.
    pub(super) older: usize,
}

pub(super) fn booking_bands(bookings: &[SidebarBooking]) -> BookingBands<'_> {
    let count = |band: BookingBand| bookings.iter().filter(|entry| entry.band == band).count();
    BookingBands {
        listed: bookings
            .iter()
            .filter(|entry| !matches!(entry.band, BookingBand::Later | BookingBand::Older))
            .collect(),
        later: count(BookingBand::Later),
        older: count(BookingBand::Older),
    }
}

/// The pane a click or the entry cursor focuses: a fired entry's live launch.
pub(super) fn booking_pane(booking: &SidebarBooking) -> Option<&crate::ids::PaneId> {
    fire(booking)
        .and_then(|fire| fire.live.as_ref())
        .and_then(|live| live.pane.as_ref())
}

/// A fired entry whose live launch is working, for the fast animation grid.
pub(super) fn booking_in_motion(booking: &SidebarBooking) -> bool {
    fire(booking)
        .and_then(|fire| fire.live.as_ref())
        .is_some_and(|live| live.status == AgentStatus::Running)
}

fn fire(booking: &SidebarBooking) -> Option<&BookingFire> {
    match &booking.state {
        BookingState::Fired(fire) => Some(fire),
        BookingState::Waiting(_) => None,
    }
}

/// The width the `▷ booked (N)` half takes with its long label, plus the least of its `⚠`
/// segment that still lands (`least_fix`) after the seam.
pub(super) fn booked_width(ctx: &RowCtx<'_>, agent: &AgentCard) -> usize {
    let theme = ctx.theme;
    let fix = fix_segment(ctx, agent).map_or(0, |fix| {
        text_width(&value_seam(theme)) + least_fix(theme, &fix)
    });
    text_width(theme.glyph(GlyphRole::CardBooked)) + text_width(&long_label(agent)) + fix
}

/// The least of a `⚠` segment that still lands: the glyph, its space, a cell of the words
/// and the ellipsis, or the whole segment when it is shorter.
fn least_fix(theme: &Theme, fix: &str) -> usize {
    text_width(fix).min(text_width(theme.glyph(GlyphRole::ChromeAlert)) + 3)
}

fn long_label(agent: &AgentCard) -> String {
    format!(" booked ({})", agent.bookings.len())
}

/// Finish a section line whose earlier halves are `left` and whose cost is `cost`:
/// the `▷ booked (N)` half, long when `long`, its `⚠` segment, and the right-side
/// summary. The summary gives way first (`N running`, then the next entry's reading,
/// then `K ended, no PR` shortens), then the cost, and the `⚠` segment clips its reason
/// but keeps its glyph and a cell of its words.
pub(super) fn finish_line(
    ctx: &RowCtx<'_>,
    agent: &AgentCard,
    mut left: Vec<Span<'static>>,
    cost: Vec<Span<'static>>,
    long: bool,
) -> Line<'static> {
    let theme = ctx.theme;
    let width = content_width(ctx.width);
    let label = if long {
        long_label(agent)
    } else {
        format!(" {}", agent.bookings.len())
    };
    left.extend([
        Span::styled(
            theme.glyph(GlyphRole::CardBooked).to_owned(),
            theme.styled(Component::BookedHeader, Modifier::empty()),
        ),
        Span::styled(label, theme.muted()),
    ]);
    let fix = fix_segment(ctx, agent);
    let fix_seam = if fix.is_some() {
        text_width(&value_seam(theme))
    } else {
        0
    };
    let fixed = spans_width(&left) + fix_seam;
    let fits = |right: &[Span<'static>], fix: usize| fixed + fix + 1 + spans_width(right) <= width;
    let fix_width = fix.as_deref().map_or(0, text_width);
    let right = summaries(ctx, agent)
        .into_iter()
        .map(|summary| join_right(theme, summary, &cost))
        .chain([cost.clone()])
        .find(|right| fits(right, fix_width))
        .unwrap_or_else(|| match fix.as_deref() {
            Some(fix) if !fits(&cost, least_fix(theme, fix)) => Vec::new(),
            _ => cost,
        });
    if let Some(fix) = fix {
        let room = width
            .saturating_sub(fixed + usize::from(!right.is_empty()) + spans_width(&right))
            .max(1);
        left.push(Span::styled(value_seam(theme), theme.muted()));
        left.push(Span::styled(
            ellipsize(&fix, room),
            theme.styled(Component::BookingFix, Modifier::empty()),
        ));
    }
    pin_right(left, right, width)
}

fn join_right(theme: &Theme, summary: String, cost: &[Span<'static>]) -> Vec<Span<'static>> {
    let mut right = vec![Span::styled(summary, theme.muted())];
    if !cost.is_empty() {
        right.push(Span::raw(" "));
        right.extend(cost.iter().cloned());
    }
    right
}

/// `⚠ <task>: <reason>` for one entry needing a fix, `⚠ N need a fix` for more; the
/// reason and the words go below 32 columns.
fn fix_segment(ctx: &RowCtx<'_>, agent: &AgentCard) -> Option<String> {
    let alert = ctx.theme.glyph(GlyphRole::ChromeAlert);
    let mut needs = agent
        .bookings
        .iter()
        .filter_map(|booking| Some((booking, booking.needs_fix.as_deref()?)));
    let (first, reason) = needs.next()?;
    let more = needs.count();
    Some(match (more, ctx.tier) {
        (0, Tier::L0) => format!("{alert} {}", first.task),
        (0, _) => format!("{alert} {}: {reason}", first.task),
        (more, Tier::L0) => format!("{alert} {}", more + 1),
        (more, _) => format!("{alert} {} need a fix", more + 1),
    })
}

/// The right-side summaries, widest first, each the next to try when the one before
/// does not fit.
fn summaries(ctx: &RowCtx<'_>, agent: &AgentCard) -> Vec<String> {
    let running = agent
        .bookings
        .iter()
        .filter(|booking| booking.is_running())
        .count();
    let ended = agent
        .bookings
        .iter()
        .filter(|booking| booking.ended_without_pr() && !booking.is_finished())
        .count();
    let running = (running > 0).then(|| {
        if ctx.tier == Tier::L0 {
            format!(
                "{running} {}",
                role_glyph(ctx.theme, AnimationRole::Working, ctx.head_step)
            )
        } else {
            format!("{running} running")
        }
    });
    let ended_short = (ended > 0).then(|| format!("{ended} no PR"));
    let ended_long = if ctx.tier == Tier::L0 {
        ended_short.clone()
    } else {
        (ended > 0).then(|| format!("{ended} ended, no PR"))
    };
    let reading = (running.is_none() && ended == 0)
        .then(|| {
            agent
                .bookings
                .iter()
                .find(|booking| booking.band == BookingBand::Next)
                .and_then(waiting_reading)
        })
        .flatten();
    let mut summaries = Vec::new();
    match (running, &ended_long) {
        (Some(running), Some(ended)) => {
            summaries.push(format!("{running}{}{ended}", value_seam(ctx.theme)));
        }
        (Some(running), None) => summaries.push(running),
        (None, _) => {}
    }
    summaries.extend(reading);
    summaries.extend(ended_long);
    summaries.extend(ended_short);
    summaries.dedup();
    summaries
}

/// What a waiting entry is waiting for, in the summary's words: its blocker's reading
/// or its trigger's timing.
fn waiting_reading(booking: &SidebarBooking) -> Option<String> {
    let BookingState::Waiting(wait) = &booking.state else {
        return None;
    };
    wait.blocker
        .as_ref()
        .and_then(blocker_reading)
        .or_else(|| wait.timing.clone())
}

/// A forge term's reading in the card's words (`no PR`, `#1640 open`, `PR closed`);
/// any other term's reading as the loop wrote it. `None` while the term is unread.
fn blocker_reading(blocker: &BookingBlocker) -> Option<String> {
    let reading = blocker.reading.as_deref()?;
    Some(if blocker.key.starts_with("pr[") {
        pr_words(reading, blocker.pr)
    } else {
        reading.to_owned()
    })
}

fn pr_words(reading: &str, number: Option<u64>) -> String {
    match (reading, number) {
        ("none", _) => "no PR".to_owned(),
        (state, Some(number)) => format!("#{number} {state}"),
        (state, None) => format!("PR {state}"),
    }
}

fn own_pr_words(pr: BookingPr) -> Option<String> {
    match pr {
        BookingPr::Unread => None,
        BookingPr::None => Some(pr_words("none", None)),
        BookingPr::Open { number } => Some(pr_words("open", number)),
        BookingPr::Merged { number } => Some(pr_words("merged", number)),
        BookingPr::Closed { number } => Some(pr_words("closed", number)),
    }
}

/// Two lines per entry, named for what it starts (`forge · lane-readers`, the type
/// word gone below 46 columns), the second line saying what blocks it, what it
/// launched, or when it ended.
pub(super) fn booking_entry_lines(
    ctx: &RowCtx<'_>,
    rows: &[&SidebarBooking],
    selected: bool,
) -> Vec<CardLine> {
    let mut lines = Vec::new();
    for booking in rows {
        let pane = booking_pane(booking);
        let focused = selected
            && pane
                .is_some_and(|pane| ctx.entry_cursor == Some(&EntryCursor::SubAgent(pane.clone())));
        let mut entry_lines = Vec::new();
        push_entry(ctx, &mut entry_lines, booking_entry(ctx, booking, focused));
        lines.extend(entry_lines.into_iter().map(|line| CardLine {
            line,
            target: pane.cloned().map(HitTarget::SubAgentPane),
        }));
    }
    lines
}

fn booking_entry(ctx: &RowCtx<'_>, booking: &SidebarBooking, focused: bool) -> Entry {
    let theme = ctx.theme;
    let fired = fire(booking);
    let live = fired.and_then(|fire| fire.live.as_ref());
    let lead = match (&booking.needs_fix, fired, live) {
        (Some(_), _, _) => Span::styled(
            theme.glyph(GlyphRole::ChromeAlert).to_owned(),
            theme.styled(Component::BookingFix, Modifier::empty()),
        ),
        (None, None, _) | (None, Some(BookingFire { starting: true, .. }), None) => Span::styled(
            theme.glyph(GlyphRole::CardBooked).to_owned(),
            theme.styled(Component::BookedHeader, Modifier::empty()),
        ),
        (None, Some(_), Some(live)) => Span::styled(
            agent_glyph(theme, live.status, live.phase, ctx.head_step),
            agent_role_style_at(theme, live.status, live.phase, ctx.animation_phase),
        ),
        (None, Some(_), None) => Span::styled(
            theme.glyph(GlyphRole::CardBookingEnded).to_owned(),
            theme.styled(Component::BookingEnded, Modifier::empty()),
        ),
    };
    let bright = booking.needs_fix.is_some()
        || live.is_some_and(|live| live.pane.is_some())
        || fired.is_some_and(|fire| fire.live.is_none() && fire.depended_on);
    let headline_style = if bright { theme.body() } else { theme.muted() };
    let starts = match &booking.starts {
        BookingStarts::Team(name) | BookingStarts::Profile(name) => name.clone(),
    };
    let (kind, headline) = if ctx.tier == Tier::L2 {
        (
            Span::styled(starts, theme.body()),
            Some(Span::styled(booking.task.clone(), headline_style)),
        )
    } else {
        (Span::styled(booking.task.clone(), headline_style), None)
    };
    Entry {
        focused,
        lead,
        kind,
        headline,
        right: Vec::new(),
        detail: detail_line(ctx, booking, focused),
    }
}

fn detail_line(ctx: &RowCtx<'_>, booking: &SidebarBooking, focused: bool) -> Option<Line<'static>> {
    let theme = ctx.theme;
    let clock = fire(booking).and_then(|fire| runtime(ctx, fire));
    let mut left = entry_indent(theme, focused);
    left.push(Span::raw("  "));
    let budget = content_width(ctx.width)
        .saturating_sub(spans_width(&left) + clock.as_ref().map_or(0, |clock| clock.width() + 1));
    let (text, style) = detail_text(ctx, booking, budget)?;
    left.push(Span::styled(text, style));
    Some(pin_right(
        left,
        clock.into_iter().collect(),
        content_width(ctx.width),
    ))
}

/// Plain muted runtime since the fire, frozen at the end stamp once nothing is alive.
fn runtime(ctx: &RowCtx<'_>, fire: &BookingFire) -> Option<Span<'static>> {
    let until = match (&fire.live, fire.ended_at) {
        (Some(_), _) => ctx.now,
        (None, Some(ended_at)) => ended_at,
        (None, None) => return None,
    };
    let secs = until.as_second().saturating_sub(fire.at.as_second()).max(0);
    Some(Span::styled(
        format!("{:>5}", elapsed_label(secs)),
        ctx.theme.muted(),
    ))
}

fn detail_text(
    ctx: &RowCtx<'_>,
    booking: &SidebarBooking,
    budget: usize,
) -> Option<(String, Style)> {
    let theme = ctx.theme;
    if let Some(reason) = &booking.needs_fix {
        return Some((ellipsize(reason, budget), theme.body()));
    }
    match &booking.state {
        BookingState::Waiting(wait) => {
            if let Some(blocker) = &wait.blocker {
                return Some(blocker_text(ctx, blocker, budget));
            }
            wait.timing
                .as_deref()
                .map(|timing| (ellipsize(timing, budget), theme.muted()))
        }
        BookingState::Fired(fire) => Some(fired_text(ctx, booking, fire, budget)),
    }
}

/// `after <sibling> · <reading>`, `<sibling> ended · <reading>`, `<sibling> starting ·
/// <reading>`, or the raw
/// `<key>: <reading>`, then ` +N`; the name gives way so the reading never clips.
fn blocker_text(ctx: &RowCtx<'_>, blocker: &BookingBlocker, budget: usize) -> (String, Style) {
    let theme = ctx.theme;
    let seam = value_seam(theme);
    let (name, joiner, style) = match &blocker.after {
        Some(sibling) if sibling.state == SiblingState::Ended => (
            format!("{} ended", sibling.task),
            seam.as_str(),
            theme.body(),
        ),
        Some(sibling) if sibling.state == SiblingState::Starting => (
            format!("{} starting", sibling.task),
            seam.as_str(),
            theme.muted(),
        ),
        Some(sibling) if ctx.tier == Tier::L0 => {
            (sibling.task.clone(), seam.as_str(), theme.muted())
        }
        Some(sibling) => (
            format!("after {}", sibling.task),
            seam.as_str(),
            theme.muted(),
        ),
        None => (blocker.key.clone(), ": ", theme.muted()),
    };
    let mut tail = blocker_reading(blocker)
        .map(|reading| format!("{joiner}{reading}"))
        .unwrap_or_default();
    if blocker.more > 0 {
        tail.push_str(&format!(" +{}", blocker.more));
    }
    let name = ellipsize(&name, budget.saturating_sub(text_width(&tail)).max(1));
    (format!("{name}{tail}"), style)
}

fn fired_text(
    ctx: &RowCtx<'_>,
    booking: &SidebarBooking,
    fire: &BookingFire,
    budget: usize,
) -> (String, Style) {
    let theme = ctx.theme;
    let handle = |name: &str| format!("@{}", name.trim_start_matches('@'));
    if let Some(live) = &fire.live {
        let text = match (&booking.starts, &live.stage) {
            (_, Some(stage)) => match stage.owner.as_deref() {
                Some(owner) if ctx.tier != Tier::L0 => {
                    format!("{}{}{}", stage.stage, value_seam(theme), handle(owner))
                }
                _ => stage.stage.clone(),
            },
            (BookingStarts::Team(_), None) => "stage unknown".to_owned(),
            (BookingStarts::Profile(_), None) => {
                live.handle.as_deref().map(handle).unwrap_or_default()
            }
        };
        return (ellipsize(&text, budget), theme.muted());
    }
    if fire.starting {
        return (ellipsize("starting", budget), theme.muted());
    }
    // The age gives way before the reading, which never clips.
    let pr = own_pr_words(fire.pr)
        .map(|pr| format!("{}{pr}", value_seam(theme)))
        .unwrap_or_default();
    let text = fire
        .ended_at
        .map(|at| format!("ended {} ago{pr}", elapsed_label(age_secs(at, ctx.now))))
        .filter(|text| text_width(text) <= budget)
        .unwrap_or_else(|| format!("ended{pr}"));
    let style = if fire.depended_on {
        theme.body()
    } else {
        theme.muted()
    };
    (ellipsize(&text, budget), style)
}
