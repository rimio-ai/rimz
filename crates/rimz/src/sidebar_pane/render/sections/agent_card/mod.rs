//! The per-agent card: identity line, description, the context meter and its
//! token line, and the expanded subagent list. A state-to-template table fixes
//! the ordered line slots from lifecycle facts; late or absent data only fills
//! that skeleton. The card anatomy is drawn in docs/interface/sidebar.md; the
//! density and selection invariants live in docs/internals/sidebar/sidebar.md.

use crate::agents::{AgentContext, AgentCurrentUsage, CacheHealth, TurnPhase};
use crate::agents::{AgentStatus, ContextSeverity};
use crate::config::{AnimationRole, ContextMeterConfig, GlyphRole};
use crate::{AgentCard, SidebarRow, SidebarSubAgent, SidebarWorktreeGroup};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::sidebar_pane::pixel::meter::MeterPixels;
use crate::sidebar_pane::render::fmt::{
    activity_short, age_secs, dollars2, elapsed_label, model_label, pct_label, tokens_int,
    window_short,
};
use crate::sidebar_pane::render::labels::{
    CardAttention, CardEmphasis, TokenColumns, TokenDetail, activity_age_style, agent_glyph,
    agent_lead_style_with_attention, agent_role_style_at, compacting_head_style,
    context_breakdown_spans, context_compaction_spans, context_gauge_spans,
    context_tool_repeat_spans, context_total_spans, elapsed_glyph, emphasize, role_glyph,
    severity_heat_amount, severity_heat_color, subagent_head_style, token_breakdown_spans,
    token_total_glyph, unread_run_spans, window_style,
};
use crate::sidebar_pane::render::layout::ellipsize;
use crate::sidebar_pane::render::theme::{Component, Theme};

mod description;
mod gauge;
mod identity;
mod template;

use self::{description::*, gauge::*};
use identity::{display_context_window, identity_line};
use template::{CardSlot, CardStage, template};

use super::process::{process_detail_line, process_row_line};
use super::{Gutter, RowCtx, Tier, content_width, pin_right, trim_spans_to_width, with_gutter};

/// Width budget for the agent handle on line 1: kind-role handles through
/// `opencode-docsmith` fit, and longer profiles clip with `…` rather than
/// pushing the model/effort tokens off the line.
const NAME_MAX: usize = 18;

fn agent(row: &SidebarRow) -> Option<&AgentCard> {
    row.as_agent()
}

fn session_cost_usd(row: &SidebarRow) -> Option<f64> {
    ctx(row)
        .and_then(|context| context.cost.as_ref())
        .and_then(|cost| cost.total_cost_usd)
}

pub(in crate::sidebar_pane) fn agent_card_cost_usd(
    group: &SidebarWorktreeGroup,
    row: &SidebarRow,
) -> Option<f64> {
    let session = session_cost_usd(row);
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

pub(in crate::sidebar_pane::render) fn awaiting_first_prompt_affordance(row: &SidebarRow) -> bool {
    matches!(CardStage::of(row), CardStage::Fresh { labeled: false })
}

pub(super) fn row_lines(
    ctx: &RowCtx<'_>,
    row: &SidebarRow,
    selected: bool,
    gutter: Gutter,
    cost_usd: Option<f64>,
    mut meter_pixels: Option<&mut MeterPixels>,
) -> Vec<Line<'static>> {
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
    if row.is_process() {
        inner.push(identity_line(ctx, row, attention, cost_usd));
        if let Some(line) = process_detail_line(ctx.theme, row, cw) {
            inner.push(line);
        }
    } else if let Some(agent) = agent(row) {
        let stage = CardStage::of(row);
        for slot in template(stage, status, ctx.card_density, selected) {
            match slot {
                CardSlot::Identity => {
                    inner.push(identity_line(ctx, row, attention, cost_usd));
                }
                CardSlot::Description => inner.push(description_line(ctx, row, attention)),
                CardSlot::AwaitingDots => {
                    inner.push(awaiting_prompt_line(ctx.animation_phase, cw));
                }
                CardSlot::Gauge => {
                    inner.push(gauge_line(ctx, row, meter_pixels.as_deref_mut()));
                }
                CardSlot::Tokens => inner.push(context_tokens_line(ctx, row)),
                CardSlot::Subagents => {
                    inner.extend(sub_agent_lines(ctx, &agent.sub_agents));
                }
            }
        }
    }
    inner
        .into_iter()
        .map(|line| with_gutter(ctx.theme, line, gutter, wash, ctx.width))
        .collect()
}

/// The expanded card's subagent list: a `⧉ subagents (N)` header — the marker
/// in the delegation violet, the label dim — then up to two indented lines per
/// child. Line 1 leads with the same live cell an agent row wears — the
/// thinking head while the child reasons, the working fill while it acts,
/// the static `✓`/`!` verdict once it finishes — then the type and the
/// description of what the parent asked it to do, with the child's exact cost
/// pinned right when a priced per-child transcript exists; line 2 (deeper
/// indent) is its reported token figure `◇` (the card's whole-unit figure,
/// never a decimal), model, and reasoning effort — one per-card column grid,
/// each slot sized to its widest sibling so the figures, models, and efforts
/// stack — with elapsed work (the clock-fill glyph over a fixed
/// `<1m`/`9m`/`2h` label in the parent's age tone ramp) pinned right under the
/// parent's own stats. Children are
/// subordinate to the parent card, so their text stays at the soft middle
/// weight — the model/effort metadata a step deeper at the dim chrome, like
/// the parent's capability tokens — and indented past the parent's stat
/// lines. Claude's description, cumulative tokens, and elapsed ride in from
/// `subagentStatusLine`; Codex reports current child context from the rollout
/// tail. Model, effort, and phase come from child lifecycle events. A child
/// with none of them degrades to the bare type line. A finished child retains
/// reported token/model/effort metadata but drops the elapsed clock because its
/// work span is over.
fn sub_agent_lines(ctx: &RowCtx<'_>, sub_agents: &[SidebarSubAgent]) -> Vec<Line<'static>> {
    if sub_agents.is_empty() {
        return Vec::new();
    }
    let theme = ctx.theme;
    let width = content_width(ctx.width);
    let animation_phase = ctx.animation_phase;
    // The `⧉` marker wears the violet of the delegation/meta family (the
    // compacting head, the `⇅ rc` flag); the label text reads at the soft
    // middle weight like the children below it.
    let mut lines = vec![Line::from(trim_spans_to_width(
        vec![
            Span::styled(
                format!("  {}", theme.glyph(GlyphRole::CardSubagents)),
                theme.styled(Component::SubagentHeader, Modifier::empty()),
            ),
            Span::styled(format!(" subagents ({})", sub_agents.len()), theme.body()),
        ],
        width,
    ))];
    // The metadata lines below form one per-card grid: the token figure
    // right-aligns to the widest sibling and the model pads to the widest
    // sibling, so the `·` seams, the models, and the efforts stack into
    // columns across children (the elapsed cluster already stacks via its
    // fixed right-pinned slot). A column exists only while some child carries
    // the field; a child missing a carried field blank-fills the slot. Finished
    // metadata-free children render no second row.
    let token_col = sub_agents
        .iter()
        .filter_map(sub_agent_tokens)
        .map(|total| tokens_int(total).chars().count())
        .max()
        .unwrap_or(0);
    let model_col = sub_agents
        .iter()
        .filter_map(|sub| sub.model.as_deref())
        .map(|model| model_label(model).chars().count())
        .max()
        .unwrap_or(0);
    for sub in sub_agents {
        // The leading cell is the agent-row vocabulary verbatim: a running
        // child thinks (reasoning) or fills (acting) in the live clay, a
        // finished one holds its static `✓`/`!` verdict — one head grammar
        // for the parent's cell and its children's.
        let mut spans = vec![
            Span::raw("    "),
            Span::styled(
                agent_glyph(theme, sub.status, sub.phase, animation_phase),
                agent_role_style_at(theme, sub.status, sub.phase, animation_phase),
            ),
            Span::raw(" "),
            Span::styled(sub.name.clone(), theme.body()),
        ];
        // Prefer the `subagentStatusLine` description; fall back to the task
        // definition, shown only when it differs from the name (the name already
        // is the type for most children) so the line never reads `Explore —
        // Explore`.
        let detail = sub
            .description
            .as_deref()
            .or(sub.task.as_deref().filter(|task| *task != sub.name));
        if let Some(detail) = detail {
            spans.push(Span::styled(format!(" — {detail}"), theme.body()));
        }
        let mut right = Vec::new();
        if let Some(usd) = sub.cost_usd.filter(|usd| *usd >= 0.005) {
            right.push(Span::styled(
                dollars2(usd),
                theme.money_style(Modifier::empty()),
            ));
        }
        lines.push(pin_right(spans, right, width));

        if let Some(line) = sub_agent_metadata_line(theme, sub, token_col, model_col, width) {
            lines.push(line);
        }
    }
    lines
}

fn sub_agent_tokens(sub: &SidebarSubAgent) -> Option<u64> {
    sub.total_tokens.filter(|total| *total > 0)
}

/// A finished subagent has a verdict and no longer needs a live elapsed clock.
fn sub_agent_finished(sub: &SidebarSubAgent) -> bool {
    matches!(sub.status, AgentStatus::Success | AgentStatus::Failed)
}

fn sub_agent_metadata_line(
    theme: &Theme,
    sub: &SidebarSubAgent,
    token_col: usize,
    model_col: usize,
    width: usize,
) -> Option<Line<'static>> {
    let tokens = sub_agent_tokens(sub);
    let elapsed = (!sub_agent_finished(sub))
        .then_some(sub.elapsed_secs)
        .flatten();
    let model = sub.model.as_deref();
    let effort = sub.effort.as_deref();
    if tokens.is_none() && elapsed.is_none() && model.is_none() && effort.is_none() {
        return None;
    }
    let mut left = vec![Span::raw("      ")];
    let mut prev_rendered = append_sub_agent_tokens(theme, &mut left, tokens, token_col);
    append_sub_agent_model(
        theme,
        &mut left,
        model,
        token_col,
        model_col,
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
    Some(pin_right(left, sub_agent_elapsed(theme, elapsed), width))
}

fn append_sub_agent_tokens(
    theme: &Theme,
    left: &mut Vec<Span<'static>>,
    tokens: Option<u64>,
    token_col: usize,
) -> bool {
    if token_col == 0 {
        return false;
    }
    match tokens {
        Some(total) => {
            left.push(Span::styled(
                token_total_glyph(theme),
                theme.styled(Component::TokenTotal, Modifier::empty()),
            ));
            left.push(Span::styled(
                format!(" {:>token_col$}", tokens_int(total)),
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

fn append_sub_agent_model(
    theme: &Theme,
    left: &mut Vec<Span<'static>>,
    model: Option<&str>,
    token_col: usize,
    model_col: usize,
    prev_rendered: &mut bool,
) {
    if model_col == 0 {
        return;
    }
    let seam = if token_col > 0 { 3 } else { 0 };
    match model {
        Some(model) => {
            if *prev_rendered {
                left.push(Span::styled(" · ", theme.muted()));
            } else {
                left.push(Span::raw(" ".repeat(seam)));
            }
            left.push(Span::styled(
                format!("{:<model_col$}", model_label(model)),
                theme.muted(),
            ));
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
        left.push(Span::styled(" · ", theme.muted()));
    } else if token_col > 0 || model_col > 0 {
        left.push(Span::raw("   "));
    }
    left.push(Span::styled(effort.to_owned(), theme.muted()));
}

fn sub_agent_elapsed(theme: &Theme, elapsed: Option<i64>) -> Vec<Span<'static>> {
    elapsed
        .map(|secs| {
            vec![Span::styled(
                format!("{} {:>3}", elapsed_glyph(theme, secs), elapsed_label(secs)),
                activity_age_style(theme, secs),
            )]
        })
        .unwrap_or_default()
}
