//! The per-agent card: identity line, description, the context meter and its
//! token and cache-expiry lines on selected or held cards (every engaged card at expanded density),
//! and expanded subagent, wait, and booked-launch entries. Read idle and done history can shrink to one line. A state-to-template table fixes
//! the ordered line slots from lifecycle facts; late or absent data only fills
//! that skeleton. The card anatomy is drawn in docs/interface/sidebar.md; the
//! density and expansion invariants live in docs/internals/sidebar/sidebar.md.

use crate::agents::model_display::{
    ParsedModel, RenderedModel, home_series, home_series_owner, window_short,
};
use crate::agents::{AgentContext, CacheHealth, TurnPhase};
use crate::agents::{AgentStatus, ContextSeverity};
use crate::config::{AnimationRole, CardDensityMode, ContextMeterConfig, GlyphRole};
use crate::store::snapshot::{
    AgentCard, SidebarRow, SidebarSubAgent, SidebarWorktreeGroup, SubAgentTokens,
};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::sidebar_pane::pixel::meter::MeterPixels;
use crate::sidebar_pane::render::fmt::{
    activity_short, age_secs, compact_count, dollars2, elapsed_label, pct_label,
};
use crate::sidebar_pane::render::labels::{
    CardAttention, CardEmphasis, TokenColumns, TokenDetail, activity_age_style, agent_glyph,
    agent_lead_style_with_attention, agent_role, agent_role_style_at, context_breakdown_spans,
    context_compaction_spans, context_gauge_spans, context_tool_repeat_spans, context_total_spans,
    elapsed_glyph, emphasize, role_glyph, role_style, severity_heat_amount, severity_heat_color,
    token_breakdown_spans, token_total_glyph, unread_run_spans, value_seam, window_style,
};
use crate::sidebar_pane::render::layout::{ellipsize, spans_width, text_width};
use crate::sidebar_pane::render::theme::{Component, Theme};
use crate::sidebar_pane::render::{DelegationPages, EntryCursor, HitTarget};

mod bands;
mod bookings;
mod description;
mod entry;
mod gauge;
mod identity;
mod launched;
mod template;
mod waits;

use self::{description::*, gauge::*};
use bands::Finished;
use entry::{Entry, agent_who, push_entry, stage_spans, team_who};
use identity::{identity_line, stale_line};
use template::{CardSlot, CardStage, template};
pub(in crate::sidebar_pane::render) use template::{engaged_base_lines, is_fresh};

use super::process::{not_tracked_lines, process_detail_line, process_row_line};
use super::{
    CardExpansion, Gutter, RowCtx, Tier, content_width, pin_right, trim_spans_to_width, with_gutter,
};

pub(super) struct CardLine {
    pub(super) line: Line<'static>,
    pub(super) target: Option<HitTarget>,
}

impl From<Line<'static>> for CardLine {
    fn from(line: Line<'static>) -> Self {
        Self { line, target: None }
    }
}

/// Width budget for the agent handle on line 1: kind-role handles through
/// `opencode-docsmith` fit, and longer profiles clip with `…` rather than
/// pushing the model/effort tokens off the line.
const NAME_MAX: usize = 18;

fn card_cost_usd(row: &SidebarRow) -> Option<f64> {
    row.as_agent().and_then(AgentCard::cost_usd)
}

pub(in crate::sidebar_pane) fn agent_card_cost_usd(
    group: &SidebarWorktreeGroup,
    row: &SidebarRow,
) -> Option<f64> {
    let session = card_cost_usd(row);
    if group.collapses() {
        group
            .cohort_effort
            .as_ref()
            .and_then(|effort| effort.seats.get(row.id.as_str()))
            .and_then(|seat| seat.cost_usd)
            .or(session)
    } else {
        session
    }
}

/// Whether this card's delegation entries are on screen with something in
/// motion on the fast grid: a running or compacting child's head or a running
/// shell job.
pub(in crate::sidebar_pane::render) fn delegation_motion(
    row: &SidebarRow,
    density: CardDensityMode,
    expansion: CardExpansion,
    stale_after_secs: u64,
    now: jiff::Timestamp,
) -> bool {
    let Some(agent) = row.as_agent() else {
        return false;
    };
    delegation_entries_drawn(row, density, expansion, stale_after_secs, now)
        && (agent.current_sub_agents().any(|child| {
            child.status == AgentStatus::Running
                || (child.compacting && !child.status.is_attention())
        }) || !agent.background_shells.is_empty()
            || agent.shown_scripts(now).next().is_some()
            || waits::visible_waits(&agent.pending_waits, &agent.launched)
                .any(|wait| waits::is_live_watch(&wait.trigger))
            || agent.awaited.iter().any(waits::awaited_in_motion)
            || agent.launched.iter().any(launched::launched_in_motion)
            || agent.bookings.iter().any(bookings::booking_in_motion))
}

fn delegation_entries_drawn(
    row: &SidebarRow,
    density: CardDensityMode,
    expansion: CardExpansion,
    stale_after_secs: u64,
    now: jiff::Timestamp,
) -> bool {
    let status = row.status().unwrap_or(AgentStatus::Idle);
    let stage = CardStage::resolve(row, density, expansion, stale_after_secs, now);
    let plan = row.as_agent().is_some_and(bookings::open_plan);
    template(stage, status, density, expansion, plan).contains(&CardSlot::DelegationEntries)
}

fn wait_count(agent: &AgentCard, now: jiff::Timestamp) -> usize {
    agent.awaited.len()
        + waits::visible_waits(&agent.pending_waits, &agent.launched).count()
        + agent.background_shells.len()
        + agent.shown_scripts(now).count()
        + usize::from(agent.idle_stop.is_some())
}

/// Whether the card has a child of any turn, a wait, a launched entry, or a booking for
/// its delegation section to hold.
pub(super) fn has_delegation(agent: &AgentCard, now: jiff::Timestamp) -> bool {
    !agent.sub_agents.is_empty()
        || wait_count(agent, now) > 0
        || !agent.launched.is_empty()
        || !agent.bookings.is_empty()
}

/// Whether the card holds a finished entry for its delegation section to page.
pub(super) fn has_finished(agent: &AgentCard) -> bool {
    bands::has_finished(&agent.sub_agents, &agent.launched, &agent.bookings)
}

/// Whether the card holds an entry its section line counts: an in-flight child, launched
/// entry or booking, or a wait.
pub(super) fn has_in_flight(agent: &AgentCard, now: jiff::Timestamp) -> bool {
    wait_count(agent, now) > 0
        || bands::has_in_flight(&agent.sub_agents, &agent.launched, &agent.bookings)
}

fn sub_agent_cursor(child: &SidebarSubAgent) -> Option<EntryCursor> {
    if let Some(pane) = &child.pane {
        Some(EntryCursor::SubAgent(pane.clone()))
    } else if !child.provider_native && child.reopenable {
        Some(EntryCursor::ReopenSubAgent(child.id.clone()))
    } else {
        None
    }
}

fn sub_agent_target(child: &SidebarSubAgent) -> Option<HitTarget> {
    if let Some(pane) = &child.pane {
        Some(HitTarget::SubAgentPane(pane.clone()))
    } else if !child.provider_native && child.reopenable {
        Some(HitTarget::ReopenSubAgent(child.id.clone()))
    } else {
        None
    }
}

pub(in crate::sidebar_pane::render) fn delegation_entry_stops(
    row: &SidebarRow,
    expanded: CardExpansion,
    display: &crate::config::DisplayConfig,
    now: jiff::Timestamp,
) -> Vec<EntryCursor> {
    let Some(agent) = row.as_agent() else {
        return Vec::new();
    };
    let status = row.status().unwrap_or(AgentStatus::Idle);
    let stage = CardStage::resolve(
        row,
        display.card_density,
        expanded,
        display.stale_after_secs,
        now,
    );
    let slots = template(
        stage,
        status,
        display.card_density,
        expanded,
        bookings::open_plan(agent),
    );
    let mut stops = Vec::new();
    if slots.contains(&CardSlot::DelegationEntries) {
        let entries = bands::delegation_entries(
            &agent.sub_agents,
            &agent.launched,
            &agent.bookings,
            expanded.pages,
        );
        stops.extend(
            entries
                .children
                .iter()
                .filter_map(|child| sub_agent_cursor(child)),
        );
        stops.extend(
            agent
                .awaited
                .iter()
                .filter_map(|awaited| awaited.pane.clone())
                .map(EntryCursor::SubAgent),
        );
        stops.extend(
            entries
                .launched
                .iter()
                .filter_map(|launched| launched.live_pane().cloned())
                .map(EntryCursor::SubAgent),
        );
        stops.extend(
            entries
                .bookings
                .iter()
                .filter_map(|booking| bookings::booking_pane(booking).cloned())
                .map(EntryCursor::SubAgent),
        );
        stops.extend(entries.shown.iter().filter_map(|entry| {
            match entry {
                Finished::SubAgent(child) => sub_agent_cursor(child),
                Finished::Launched(launched) => {
                    launched.live_pane().cloned().map(EntryCursor::SubAgent)
                }
                Finished::Booking(booking) => bookings::booking_pane(booking)
                    .cloned()
                    .map(EntryCursor::SubAgent),
            }
        }));
        if entries.hidden > 0 {
            stops.push(EntryCursor::DelegationPage);
        }
    }
    stops
}

pub(super) fn row_lines(
    ctx: &RowCtx<'_>,
    row: &SidebarRow,
    selected: bool,
    expanded: CardExpansion,
    gutter: Gutter,
    cost_usd: Option<f64>,
    mut meter_pixels: Option<&mut MeterPixels>,
) -> Vec<CardLine> {
    let cw = content_width(ctx.width);
    let status = row.status().unwrap_or(AgentStatus::Idle);
    // The single lead unread row — the oldest one that needs an answer — keeps the
    // configured continuous signal; every other unread row settles to the steady
    // bright crest, so one pane is the only thing in motion.
    let is_lead = ctx.lead_unread == Some(row.id.as_str());
    let attention = CardAttention::new(
        ctx.theme,
        status,
        age_secs(row.last_activity, ctx.now),
        ctx.animation_phase,
        row.unread,
        selected,
        is_lead,
    );
    // An unread card that is not the selection grounds on a soft, uniform unread
    // wash — a whole-card surface, a lighter tint of the selection blue, that reads
    // at a scanning glance where the one-cell glyph is too small to, the status
    // carried by the glyph. Gated on the same `Blink` emphasis the rest of the
    // unread treatment reads from, so the wash and the glyph/name/buckets turn on
    // together; the selection band wins when both apply.
    let wash = (!selected && attention.emphasis == CardEmphasis::Blink)
        .then(|| ctx.theme.unread_wash())
        .flatten();
    let mut inner = Vec::new();
    // An active process row carries its full command on a dim second line under
    // the shell anchor — the build or `sudo` install reads in full while line 1
    // stays the stable shell label. Idle process rows have no detail to add.
    if let Some(note) = row
        .as_process()
        .and_then(|process| process.not_tracked.as_ref())
    {
        inner.extend(not_tracked_lines(ctx, row, &note.fix).map(CardLine::from));
    } else if row.is_process() {
        inner.push(CardLine::from(identity_line(
            ctx, row, attention, cost_usd, false,
        )));
        if let Some(line) = process_detail_line(ctx.theme, row, cw) {
            inner.push(line.into());
        }
    } else if let Some(agent) = row.as_agent() {
        let stage = CardStage::resolve(
            row,
            ctx.card_density,
            expanded,
            ctx.stale_after_secs,
            ctx.now,
        );
        let slots = template(
            stage,
            status,
            ctx.card_density,
            expanded,
            bookings::open_plan(agent),
        );
        for slot in slots {
            match slot {
                CardSlot::Stale => inner.push(stale_line(ctx, row, attention, cost_usd).into()),
                CardSlot::Identity => {
                    inner.push(
                        identity_line(
                            ctx,
                            row,
                            attention,
                            cost_usd,
                            !slots.contains(&CardSlot::Description),
                        )
                        .into(),
                    );
                }
                CardSlot::Description => inner.extend(
                    description_lines(ctx, row, attention)
                        .into_iter()
                        .map(CardLine::from),
                ),
                CardSlot::Question => {
                    if let Some(ask) = agent.queued_asks.first().filter(|_| {
                        matches!(agent.status, AgentStatus::Running | AgentStatus::Waiting)
                    }) {
                        let suffix = if agent.queued_asks.len() > 1 {
                            format!(" +{}", agent.queued_asks.len() - 1)
                        } else {
                            String::new()
                        };
                        let prefix = format!("{} ", ctx.theme.glyph(GlyphRole::StatusWaiting));
                        let detail = ellipsize(
                            &ask.detail,
                            cw.saturating_sub(text_width(&prefix) + text_width(&suffix)),
                        );
                        let text = ellipsize(&format!("{prefix}{detail}{suffix}"), cw);
                        inner.push(
                            Line::styled(
                                text,
                                crate::sidebar_pane::render::labels::status_style(
                                    ctx.theme,
                                    AgentStatus::Waiting,
                                ),
                            )
                            .into(),
                        );
                    }
                }
                CardSlot::Gauge => {
                    inner.push(gauge_line(ctx, row, meter_pixels.as_deref_mut()).into());
                }
                CardSlot::Tokens => {
                    if !agent.compacted_unmeasured {
                        inner.push(context_tokens_line(ctx, row).into());
                    }
                }
                CardSlot::Cache => inner.extend(cache_line(ctx, row).map(CardLine::from)),
                CardSlot::Delegation => {
                    // With nothing in flight, a card its selection or density opens keeps the
                    // done title while a pin closes the section, so the title can reopen it.
                    let titled = expanded.delegation
                        || expanded.reach.opens_card()
                        || ctx.card_density == CardDensityMode::Expanded;
                    let line = delegation_line(ctx, agent)
                        .or_else(|| titled.then(|| done_title(ctx, agent)).flatten());
                    inner.extend(line.map(|line| CardLine {
                        line,
                        target: Some(HitTarget::ToggleDelegation {
                            row: row.id.clone(),
                            open: !expanded.delegation,
                        }),
                    }));
                }
                CardSlot::Booked => inner.push(CardLine {
                    line: bookings::finish_line(
                        ctx,
                        agent,
                        vec![Span::raw("  ")],
                        Vec::new(),
                        ctx.tier == Tier::L2,
                    ),
                    target: Some(HitTarget::ToggleDelegation {
                        row: row.id.clone(),
                        open: !expanded.delegation,
                    }),
                }),
                CardSlot::DelegationEntries => {
                    let entries = bands::delegation_entries(
                        &agent.sub_agents,
                        &agent.launched,
                        &agent.bookings,
                        expanded.pages,
                    );
                    // Every current child and each paged prior-turn one share one grid, so
                    // the columns hold while current children finish out of the list.
                    let grid: Vec<_> = agent
                        .current_sub_agents()
                        .chain(entries.shown.iter().filter_map(|entry| match entry {
                            Finished::SubAgent(child) if child.prior_turn => Some(*child),
                            _ => None,
                        }))
                        .collect();
                    let grid = SubAgentGrid::measure(ctx.theme, &grid);
                    inner.extend(sub_agent_entry_lines(
                        ctx,
                        grid,
                        &entries.children,
                        selected,
                    ));
                    inner.extend(waits::awaited_entry_lines(ctx, &agent.awaited, selected));
                    inner.extend(
                        waits::wait_entry_lines(
                            ctx,
                            &agent.pending_waits,
                            &agent.launched,
                            &agent.background_shells,
                            agent.shown_scripts(ctx.now),
                            agent.idle_stop.as_ref(),
                        )
                        .into_iter()
                        .map(CardLine::from),
                    );
                    inner.extend(launched::launched_entry_lines(
                        ctx,
                        &entries.launched,
                        selected,
                    ));
                    inner.extend(bookings::booking_entry_lines(
                        ctx,
                        &entries.bookings,
                        selected,
                    ));
                    for entry in &entries.shown {
                        inner.extend(match entry {
                            Finished::SubAgent(child) => {
                                sub_agent_entry_lines(ctx, grid, &[*child], selected)
                            }
                            Finished::Launched(entry) => {
                                launched::launched_entry_lines(ctx, &[*entry], selected)
                            }
                            Finished::Booking(booking) => {
                                bookings::booking_entry_lines(ctx, &[*booking], selected)
                            }
                        });
                    }
                    if entries.hidden > 0 {
                        inner.push(pager_line(
                            ctx,
                            row,
                            selected,
                            expanded.pages,
                            entries.hidden,
                        ));
                    }
                }
            }
        }
    }
    // A card whose close began repaints its own lines in the closing tone, a launched
    // child's close only that child's entry, and a reopening child only its own entry,
    // so none of these marks moves any line or changes any height.
    let closing_card = row
        .pane
        .as_ref()
        .is_some_and(|pane| ctx.closing.contains_key(&pane.pane_id));
    let tone = ctx.theme.component(Component::CardClosing);
    let reopening_style = ctx
        .theme
        .styled(Component::SubagentReopening, Modifier::empty());
    inner
        .into_iter()
        .map(|mut card| {
            let closing_entry = matches!(
                &card.target,
                Some(HitTarget::SubAgentPane(pane)) if ctx.closing.contains_key(pane)
            );
            let reopening_entry = matches!(
                &card.target,
                Some(HitTarget::ReopenSubAgent(id)) if ctx.reopening.contains_key(id)
            );
            if closing_card || closing_entry {
                // A kitty placeholder's foreground is the id of its image, not a color.
                for span in &mut card.line.spans {
                    if !span.content.contains(crate::pixel_wire::PLACEHOLDER) {
                        span.style.fg = Some(tone);
                    }
                }
            } else if reopening_entry {
                for span in &mut card.line.spans {
                    if !span.content.contains(crate::pixel_wire::PLACEHOLDER) {
                        span.style.fg = reopening_style.fg;
                        if reopening_style.fg.is_none() {
                            span.style = span.style.add_modifier(Modifier::DIM);
                        }
                    }
                }
            }
            CardLine {
                line: with_gutter(ctx.theme, card.line, gutter, wash, ctx.width),
                target: card.target,
            }
        })
        .collect()
}

/// The last line of an open delegation section while finished entries stay hidden:
/// `▸ +N done` before its first page and `▸ +M more` after. Its target carries the record
/// an activation stores, one page past `pages` in the parent's turn this paint drew.
fn pager_line(
    ctx: &RowCtx<'_>,
    row: &SidebarRow,
    selected: bool,
    pages: usize,
    hidden: usize,
) -> CardLine {
    let focused = selected && matches!(ctx.entry_cursor, Some(EntryCursor::DelegationPage));
    let word = if pages == 0 { "done" } else { "more" };
    let mut spans = entry_indent(ctx.theme, focused);
    spans.push(Span::styled(
        format!(
            "{} +{hidden} {word}",
            ctx.theme.glyph(GlyphRole::WorktreeInactiveClosed)
        ),
        ctx.theme
            .styled(Component::DelegationPager, Modifier::empty()),
    ));
    CardLine {
        line: Line::from(spans),
        target: Some(HitTarget::PageDelegation {
            row: row.id.clone(),
            pages: DelegationPages {
                pages: std::num::NonZeroUsize::MIN.saturating_add(pages),
                turn: row.as_agent().and_then(|agent| agent.user_turn_started_at),
            },
        }),
    }
}

/// The marks, padded to end at the meter's percentage column. `trailing_width` is
/// what the line pins after them, gap included; a cluster wider than the padding
/// pushes the marks left of that column. The padding shrinks before a mark drops,
/// so a full line slides the marks right instead.
fn message_marks(
    ctx: &RowCtx<'_>,
    row: &SidebarRow,
    width: usize,
    trailing_width: usize,
) -> Vec<Span<'static>> {
    let mut marks = message_mark_spans(ctx, row, width);
    let padding = gauge_age_width(ctx, row)
        .saturating_sub(trailing_width)
        .min(width.saturating_sub(spans_width(&marks)));
    if !marks.is_empty() && padding > 0 {
        marks.push(Span::raw(" ".repeat(padding)));
    }
    marks
}

fn message_mark_spans(ctx: &RowCtx<'_>, row: &SidebarRow, mut width: usize) -> Vec<Span<'static>> {
    let Some(agent) = row.as_agent() else {
        return Vec::new();
    };
    let typing = row.pane.as_ref().is_some_and(|pane| {
        ctx.typing_until
            .get(&pane.pane_id)
            .is_some_and(|until| *until > ctx.now)
    });
    let mut marks = Vec::new();
    let pen = ctx.theme.glyph(GlyphRole::CardTyping);
    if typing && text_width(pen) <= width {
        marks.push(Span::styled(
            pen.to_owned(),
            ctx.theme.styled(Component::Typing, Modifier::empty()),
        ));
        width -= text_width(pen);
    }
    if agent.due_messages == 0 {
        return marks;
    }
    let queued = ctx.theme.glyph(GlyphRole::CardMessageQueued);
    let gap = usize::from(!marks.is_empty());
    if text_width(queued) + gap > width {
        return marks;
    }
    if gap > 0 {
        marks.push(Span::raw(" "));
    }
    marks.push(Span::styled(
        queued.to_owned(),
        ctx.theme
            .styled(Component::MessageQueued, Modifier::empty()),
    ));
    width -= text_width(queued) + gap;
    let count = format!(" {}", agent.due_messages);
    if ctx.tier != Tier::L0 && text_width(&count) <= width {
        marks.push(Span::styled(count, ctx.theme.body()));
    }
    marks
}

/// The known cost of every subagent on the card, of any turn, running or finished, as the
/// section line and the done title pin it right; empty below half a cent.
fn subagents_cost(theme: &Theme, agent: &AgentCard) -> Vec<Span<'static>> {
    agent
        .sub_agents
        .iter()
        .fold(None, |total, child| {
            crate::agents::spending::sum_optional_cost(total, child.cost_usd)
        })
        .filter(|usd| *usd >= 0.005)
        .map(|usd| {
            vec![Span::styled(
                dollars2(usd),
                theme.money_style(Modifier::empty()),
            )]
        })
        .unwrap_or_default()
}

/// The standing delegation line names each kind with something in flight, with its count:
/// the current turn's children without a verdict, armed waits with background shells, the
/// launched entries still alive, and the booked half last over the unfinished bookings,
/// with the cost of every subagent pinned right. It reads no selection or section state, so
/// opening the card only appends entries. `None` while nothing is in flight.
fn delegation_line(ctx: &RowCtx<'_>, agent: &AgentCard) -> Option<Line<'static>> {
    let children = bands::in_flight_children(&agent.sub_agents).count();
    let wait_count = wait_count(agent, ctx.now);
    let launched = agent
        .launched
        .iter()
        .filter(|entry| !entry.is_finished())
        .count();
    let booked = bookings::open_plan(agent);
    if children == 0 && wait_count == 0 && launched == 0 && !booked {
        return None;
    }
    let width = content_width(ctx.width);
    let theme = ctx.theme;
    let right = subagents_cost(theme, agent);
    let halves = |long: bool| delegation_halves(ctx, children, wait_count, launched, long);
    let leading = children > 0 || wait_count > 0 || launched > 0;
    // With launched entries or bookings on the line, the labels take their short forms
    // before the line trims, so the `⚠` segment always lands beside the cost.
    let long = ctx.tier == Tier::L2
        && ((!booked && launched == 0) || {
            let booked_width = if !booked {
                0
            } else if leading {
                text_width(&value_seam(ctx.theme)) + bookings::booked_width(ctx, agent)
            } else {
                bookings::booked_width(ctx, agent)
            };
            let cost = if right.is_empty() {
                0
            } else {
                1 + spans_width(&right)
            };
            spans_width(&halves(true)) + booked_width + cost <= width
        });
    let mut left = halves(long);
    if !booked {
        return Some(pin_right(left, right, width));
    }
    if leading {
        left.push(Span::styled(value_seam(theme), theme.muted()));
    }
    Some(bookings::finish_line(ctx, agent, left, right, long))
}

/// The section's title while nothing is in flight: the kinds its finished entries hold, in
/// the line's order and uncounted, long at the long tier where the title fits, and the
/// dimmed `✓ done (N)` pinned right, followed by the subagents' cost when one is known.
/// The kinds shorten to their glyphs, then drop, then the cost drops, before the done count
/// loses a cell. `None` without a finished entry.
fn done_title(ctx: &RowCtx<'_>, agent: &AgentCard) -> Option<Line<'static>> {
    let done = bands::finished_count(&agent.sub_agents, &agent.launched, &agent.bookings);
    if done == 0 {
        return None;
    }
    let theme = ctx.theme;
    let width = content_width(ctx.width);
    let present = bands::finished_kinds(&agent.sub_agents, &agent.launched, &agent.bookings);
    let kinds = |long: bool| {
        let mut spans = vec![Span::raw("  ")];
        let headers = [
            (
                GlyphRole::CardSubagents,
                Component::SubagentHeader,
                " subagents",
            ),
            (
                GlyphRole::CardLaunched,
                Component::LaunchedHeader,
                " launched",
            ),
            (GlyphRole::CardBooked, Component::BookedHeader, " booked"),
        ];
        for (_, (glyph, component, label)) in present
            .into_iter()
            .zip(headers)
            .filter(|(present, _)| *present)
        {
            if spans.len() > 1 {
                spans.push(if long {
                    Span::styled(value_seam(theme), theme.muted())
                } else {
                    Span::raw(" ")
                });
            }
            spans.push(Span::styled(
                theme.glyph(glyph).to_owned(),
                theme.styled(component, Modifier::empty()),
            ));
            if long {
                spans.push(Span::styled(label, theme.body()));
            }
        }
        spans
    };
    let mark = vec![Span::styled(
        format!("{} done ({done})", theme.glyph(GlyphRole::StatusDone)),
        theme.styled(Component::DelegationDone, Modifier::empty()),
    )];
    let cost = subagents_cost(theme, agent);
    let priced = if cost.is_empty() {
        mark.clone()
    } else {
        [mark.clone(), vec![Span::raw("  ")], cost].concat()
    };
    let fits = |(left, right): &(Vec<Span<'static>>, Vec<Span<'static>>)| {
        spans_width(left) + usize::from(!left.is_empty()) + spans_width(right) <= width
    };
    let (left, right) = [true, false]
        .into_iter()
        .filter(|long| !long || ctx.tier == Tier::L2)
        .map(|long| (kinds(long), priced.clone()))
        .chain([(Vec::new(), priced.clone()), (Vec::new(), mark.clone())])
        .find(fits)
        .unwrap_or((Vec::new(), mark));
    Some(pin_right(left, right, width))
}

/// The subagents, waits and launched halves of the section line, with long labels when
/// `long`. A count of 0 draws no half.
fn delegation_halves(
    ctx: &RowCtx<'_>,
    children: usize,
    wait_count: usize,
    launched: usize,
    long: bool,
) -> Vec<Span<'static>> {
    let theme = ctx.theme;
    let subagents_label = if long {
        format!(" subagents ({children})")
    } else {
        format!(" {children}")
    };
    let waits_label = if long {
        format!(" waits ({wait_count})")
    } else {
        format!(" {wait_count}")
    };
    let mut left = vec![Span::raw("  ")];
    // The `⧉` marker wears the violet of the delegation/meta family (the
    // compacting head, the `⇅ rc` flag); the label text reads at the soft
    // middle weight like the children below it.
    if children > 0 {
        left.extend([
            Span::styled(
                theme.glyph(GlyphRole::CardSubagents).to_owned(),
                theme.styled(Component::SubagentHeader, Modifier::empty()),
            ),
            Span::styled(subagents_label, theme.body()),
        ]);
    }
    if wait_count > 0 {
        if children > 0 {
            left.push(Span::styled(value_seam(theme), theme.muted()));
        }
        left.extend([
            Span::styled(
                theme.glyph(GlyphRole::CardWaits).to_owned(),
                theme.styled(Component::WaitHeader, Modifier::empty()),
            ),
            Span::styled(waits_label, theme.body()),
        ]);
    }
    if launched > 0 {
        if children > 0 || wait_count > 0 {
            left.push(Span::styled(value_seam(theme), theme.muted()));
        }
        left.extend(launched::launched_half(theme, launched, long));
    }
    left
}

/// The metadata lines of a card's children form one grid: the token figure
/// right-aligns to the widest sibling and the model label pads to the widest
/// sibling label, excluding the fast bolt, so the `·` seams, models, and
/// efforts stack into columns across children (the clock already stacks via
/// its fixed right-pinned slot). A column exists only while some child carries
/// the field; a child missing a carried field blank-fills the slot. Finished
/// metadata-free children render no second row.
#[derive(Clone, Copy)]
struct SubAgentGrid {
    token_col: usize,
    model_col: usize,
}

impl SubAgentGrid {
    /// The grid every child in `children` fits.
    fn measure(theme: &Theme, children: &[&SidebarSubAgent]) -> Self {
        Self {
            token_col: children
                .iter()
                .copied()
                .filter_map(sub_agent_tokens)
                .map(|total| compact_count(total).chars().count())
                .max()
                .unwrap_or(0),
            model_col: children
                .iter()
                .filter_map(|sub| sub_agent_model_spans(theme, sub))
                .map(|spans| spans_width(&spans))
                .max()
                .unwrap_or(0),
        }
    }
}

/// Up to two indented lines for each visible child. Line 1 leads with the same
/// live cell an agent row wears — the thinking head while the child reasons,
/// the working fill while it acts, or the static `✓`/`!` verdict once it
/// finishes — then its type, description, and an optional `⋯ bg` marker when
/// its turn parked on background work, with its known cost pinned right.
/// Line 2 carries the typed token figure, model, reasoning effort, then the
/// optional still fast-mode bolt and compactions on a per-card column grid.
/// Without effort, the bolt follows the model. A five-cell clock pins right:
/// plain muted runtime, frozen on finish. A running child quiet for five
/// minutes instead shows a face and quiet age heated over the configured stall
/// window. A stalled child keeps that quiet clock in alarm. Children stay at
/// the soft middle weight and indent past the parent's stats. The token glyph
/// distinguishes current window occupancy from a whole-run total; unknown
/// figures leave a blank slot. A metadata-free finished child degrades to its
/// bare type line, its frozen runtime pinned ahead of its cost. Entry lines for
/// `rows`, laid on one card's metadata `grid`, so the columns hold while finished
/// rows cut in and out of the list.
fn sub_agent_entry_lines(
    ctx: &RowCtx<'_>,
    grid: SubAgentGrid,
    rows: &[&SidebarSubAgent],
    selected: bool,
) -> Vec<CardLine> {
    let theme = ctx.theme;
    let animation_phase = ctx.animation_phase;
    let mut lines = Vec::new();
    let SubAgentGrid {
        token_col,
        model_col,
    } = grid;
    for sub in rows {
        let target = sub_agent_target(sub);
        let inert = target.is_none();
        let focused = selected
            && sub_agent_cursor(sub)
                .as_ref()
                .is_some_and(|stop| ctx.entry_cursor == Some(stop));
        // The leading cell is the agent-row vocabulary verbatim: a running
        // child thinks (reasoning) or fills (acting) in the live clay, a
        // finished one holds its static `✓`/`!` verdict, and a compacting one
        // wears the compacting head unless a `?`/`!`/pause wins — one head grammar
        // for the parent's cell and its children's.
        let lead = if sub.compacting && !sub.status.is_attention() {
            Span::styled(
                role_glyph(theme, AnimationRole::Compacting, ctx.head_step),
                role_style(theme, AnimationRole::Compacting, animation_phase),
            )
        } else {
            Span::styled(
                agent_glyph(theme, sub.status, sub.phase, ctx.head_step),
                agent_role_style_at(theme, sub.status, sub.phase, animation_phase),
            )
        };
        let detail = sub
            .description
            .as_deref()
            .filter(|detail| *detail != sub.name)
            .or(sub.task.as_deref().filter(|detail| *detail != sub.name))
            .or(sub
                .first_prompt
                .as_deref()
                .filter(|detail| *detail != sub.name));
        // The clock rides line 2; a metadata-free landed child has no line 2,
        // so it pins its frozen runtime ahead of its cost instead.
        let detail_line = sub_agent_metadata_line(ctx, sub, token_col, model_col, focused);
        let mut right = Vec::new();
        if detail_line.is_none() && sub_agent_finished(sub) {
            right.extend(sub_agent_clock(ctx, sub));
        }
        if let Some(usd) = sub.cost_usd.filter(|usd| *usd >= 0.005) {
            if !right.is_empty() {
                right.push(Span::raw(" "));
            }
            right.push(Span::styled(
                dollars2(usd),
                theme.money_style(Modifier::empty()),
            ));
        }
        let mut entry_lines = Vec::new();
        let headline_style = if sub.pane.is_some() {
            theme.body()
        } else {
            theme.muted()
        };
        let marker = (sub.phase == TurnPhase::Parked)
            .then(|| {
                Span::styled(
                    format!("  {} bg", theme.glyph(GlyphRole::CardParkedBg)),
                    theme.styled(Component::EntryParkedBg, Modifier::empty()),
                )
            })
            .into_iter()
            .collect();
        push_entry(
            ctx,
            &mut entry_lines,
            Entry {
                focused,
                lead,
                who: agent_who(
                    theme,
                    Span::styled(
                        sub.name.clone(),
                        theme.body_brand(theme.provider_brand_tone(sub.kind.as_str())),
                    ),
                    sub.lane.as_deref(),
                ),
                headline: sub
                    .turn_error_label
                    .as_deref()
                    .and_then(crate::agents::single_line_description)
                    .map(|label| Span::styled(label, theme.body().add_modifier(Modifier::ITALIC)))
                    .or_else(|| {
                        detail.map(|detail| Span::styled(detail.to_owned(), headline_style))
                    })
                    .into_iter()
                    .collect(),
                marker,
                clip_who: false,
                right,
                detail: detail_line,
            },
        );
        if inert {
            let inert_style = theme.styled(Component::SubagentInert, Modifier::DIM);
            for line in &mut entry_lines {
                for span in &mut line.spans {
                    if !span.content.contains(crate::pixel_wire::PLACEHOLDER) {
                        span.style.fg = inert_style.fg;
                        span.style = span.style.add_modifier(Modifier::DIM);
                    }
                }
            }
        }
        lines.extend(entry_lines.into_iter().map(|line| CardLine {
            line,
            target: target.clone(),
        }));
    }
    lines
}

fn entry_indent(theme: &Theme, focused: bool) -> Vec<Span<'static>> {
    if !focused {
        return vec![Span::raw("    ")];
    }
    vec![
        Span::raw("  "),
        Span::styled(
            theme.glyph(GlyphRole::ChromeSpineEntry).to_owned(),
            theme.styled(Component::EntrySpine, Modifier::empty()),
        ),
        Span::raw(" "),
    ]
}

fn sub_agent_tokens(sub: &SidebarSubAgent) -> Option<u64> {
    sub.tokens.map(|tokens| match tokens {
        SubAgentTokens::Window(value) | SubAgentTokens::Total(value) => value,
    })
}

/// A finished subagent has a verdict, is not stalled, and is not parked on
/// pending background work.
fn sub_agent_finished(sub: &SidebarSubAgent) -> bool {
    matches!(sub.status, AgentStatus::Success | AgentStatus::Failed)
        && !sub.stalled
        && sub.phase != TurnPhase::Parked
}

fn sub_agent_metadata_line(
    ctx: &RowCtx<'_>,
    sub: &SidebarSubAgent,
    token_col: usize,
    model_col: usize,
    focused: bool,
) -> Option<Line<'static>> {
    let theme = ctx.theme;
    let tokens = sub_agent_tokens(sub);
    let finished = sub_agent_finished(sub);
    let clock = sub_agent_clock(ctx, sub);
    let left_width = content_width(ctx.width)
        .saturating_sub(clock.as_ref().map_or(0, |clock| clock.width() + 1));
    let model = sub.model.as_deref();
    let effort = sub.effort.as_deref();
    if tokens.is_none()
        && model.is_none()
        && effort.is_none()
        && sub.compaction_count == 0
        && (finished || clock.is_none())
    {
        return None;
    }
    let mut left = entry_indent(theme, focused);
    left.push(Span::raw("  "));
    let mut prev_rendered = append_sub_agent_tokens(ctx, &mut left, sub, token_col);
    append_sub_agent_model(
        theme,
        &mut left,
        sub,
        token_col,
        model_col,
        left_width,
        &mut prev_rendered,
    );
    append_sub_agent_effort(
        theme,
        &mut left,
        effort,
        token_col,
        model_col,
        prev_rendered,
    );
    if effort.is_some() {
        append_sub_agent_bolt(theme, &mut left, sub, left_width);
    }
    let mut compactions = context_compaction_spans(theme, sub.compaction_count);
    if !prev_rendered && effort.is_none() && !compactions.is_empty() {
        // The leading seam has no figure before it to separate.
        compactions.remove(0);
    }
    if spans_width(&left) + spans_width(&compactions) <= left_width {
        left.extend(compactions);
    }
    Some(pin_right(
        left,
        clock.into_iter().collect(),
        content_width(ctx.width),
    ))
}

fn append_sub_agent_tokens(
    ctx: &RowCtx<'_>,
    left: &mut Vec<Span<'static>>,
    sub: &SidebarSubAgent,
    token_col: usize,
) -> bool {
    if token_col == 0 {
        return false;
    }
    let theme = ctx.theme;
    match sub.tokens {
        Some(tokens) => {
            let (total, glyph, style) = match tokens {
                // Match the parent's heat ramp; without a reported window,
                // only the token axis contributes.
                SubAgentTokens::Window(total) => (
                    total,
                    theme.glyph(GlyphRole::TokensFilled).to_owned(),
                    theme.style(
                        severity_heat_color(
                            theme,
                            ContextSeverity::Calm,
                            sub.context_gauge_percent().unwrap_or(0),
                            Some(total),
                            ctx.bands,
                        ),
                        Modifier::empty(),
                    ),
                ),
                SubAgentTokens::Total(total) => (
                    total,
                    token_total_glyph(theme),
                    theme.styled(Component::TokenTotal, Modifier::empty()),
                ),
            };
            left.push(Span::styled(glyph, style));
            left.push(Span::styled(
                format!(" {:>token_col$}", compact_count(total)),
                theme.body(),
            ));
            true
        }
        None => {
            left.push(Span::raw(" ".repeat(2 + token_col)));
            false
        }
    }
}

/// One shared subagent model render for both column measurement and drawing.
fn sub_agent_model_spans(theme: &Theme, sub: &SidebarSubAgent) -> Option<Vec<Span<'static>>> {
    let model = sub.model.as_deref()?;
    let rendered = ParsedModel::parse(model).render(home_series(sub.kind.as_str()));
    let mut spans = Vec::with_capacity(2);
    if let Some(series) = rendered.series.as_deref() {
        spans.push(Span::styled(
            series.to_owned(),
            identity::series_style(theme, series),
        ));
    }
    let name = identity::model_name_text(&rendered, rendered.children.len());
    if !name.is_empty() {
        spans.push(Span::styled(name, theme.muted()));
    }
    Some(spans)
}

fn append_sub_agent_model(
    theme: &Theme,
    left: &mut Vec<Span<'static>>,
    sub: &SidebarSubAgent,
    token_col: usize,
    model_col: usize,
    left_width: usize,
    prev_rendered: &mut bool,
) {
    if model_col == 0 {
        return;
    }
    let seam = if token_col > 0 {
        text_width(&value_seam(theme))
    } else {
        0
    };
    match sub_agent_model_spans(theme, sub) {
        Some(spans) => {
            if *prev_rendered {
                left.push(Span::styled(value_seam(theme), theme.muted()));
            } else {
                left.push(Span::raw(" ".repeat(seam)));
            }
            // Pad by painted width, not chars: a provider's model name may
            // hold wide characters, and `{:<N}` pads by char count.
            let pad = model_col.saturating_sub(spans_width(&spans));
            left.extend(spans);
            let bolt_width = if sub.effort.is_none() {
                append_sub_agent_bolt(theme, left, sub, left_width)
            } else {
                0
            };
            left.push(Span::raw(" ".repeat(pad.saturating_sub(bolt_width))));
            *prev_rendered = true;
        }
        None => left.push(Span::raw(" ".repeat(seam + model_col))),
    }
}

fn append_sub_agent_effort(
    theme: &Theme,
    left: &mut Vec<Span<'static>>,
    effort: Option<&str>,
    token_col: usize,
    model_col: usize,
    prev_rendered: bool,
) {
    let Some(effort) = effort else {
        return;
    };
    if prev_rendered {
        left.push(Span::styled(value_seam(theme), theme.muted()));
    } else if token_col > 0 || model_col > 0 {
        left.push(Span::raw(" ".repeat(text_width(&value_seam(theme)))));
    }
    left.push(Span::styled(effort.to_owned(), theme.muted()));
}

fn append_sub_agent_bolt(
    theme: &Theme,
    left: &mut Vec<Span<'static>>,
    sub: &SidebarSubAgent,
    left_width: usize,
) -> usize {
    if !sub.fast || sub.model.is_none() {
        return 0;
    }
    let bolt = theme.glyph(GlyphRole::CardFast);
    let width = 1 + text_width(bolt);
    if spans_width(left) + width > left_width {
        return 0;
    }
    left.push(Span::raw(" "));
    left.push(Span::styled(
        bolt.to_owned(),
        theme.styled(Component::FastMode, Modifier::BOLD),
    ));
    width
}

fn elapsed_cluster(theme: &Theme, secs: i64, ceiling: i64) -> String {
    format!(
        "{} {:>3}",
        elapsed_glyph(theme, secs, ceiling),
        elapsed_label(secs)
    )
}

fn sub_agent_clock(ctx: &RowCtx<'_>, sub: &SidebarSubAgent) -> Option<Span<'static>> {
    let quiet = age_secs(sub.last_activity, ctx.now);
    if sub.stalled
        || (sub.status == AgentStatus::Running
            && activity_short(sub.last_activity, ctx.now).is_some())
    {
        let ceiling = i64::from(ctx.stalled_after_secs);
        return Some(Span::styled(
            elapsed_cluster(ctx.theme, quiet, ceiling),
            activity_age_style(ctx.theme, quiet, ceiling),
        ));
    }
    sub.elapsed_secs
        .map(|secs| Span::styled(format!("{:>5}", elapsed_label(secs)), ctx.theme.muted()))
}
