//! The per-agent card: identity line, description, the context meter and its
//! token line, and the expanded subagent list. The card anatomy is drawn in
//! docs/interface/sidebar.md; the density and selection invariants live in
//! docs/internals/sidebar/sidebar.md.

use crate::agents::{AgentContext, TurnPhase};
use crate::config::{CardDensityMode, ContextSeverityConfig};
use crate::feed::{AgentStatus, ContextSeverity};
use crate::{AgentCard, SidebarProviderPanel, SidebarRow, SidebarSubAgent};
use jiff::Timestamp;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::sidebar_pane::render::CostRolls;
use crate::sidebar_pane::render::fmt::{
    activity_short, age_secs, clip, dollars2, elapsed_label, model_label, pct_label,
    time_remaining, tokens_int, window_short,
};
use crate::sidebar_pane::render::labels::{
    SEGMENT_CACHE_READ, TOKENS_TOTAL, activity_age_style, agent_glyph, agent_lead_style,
    agent_role_style_at, compacting_glyph, compacting_head_style, context_breakdown_spans,
    context_compaction_spans, context_gauge_spans, context_total_spans, elapsed_glyph,
    loading_dots, resolver_glyph, resolver_style, severity_heat_amount, severity_heat_color,
    subagent_glyph, subagent_head_style, todo_spans, window_style,
};
use crate::sidebar_pane::render::theme::Theme;

mod description;
mod gauge;
mod identity;

use self::{description::*, gauge::*, identity::*};

use super::process::{composed_row, process_detail_line, process_row_line};
use super::{
    Gutter, Tier, VALUE_FLASH, content_width, pin_right, trim_spans_to_width, with_gutter,
};

/// The context-meter label — a framed square reading as "the window", replacing
/// the `ctx` word now that it is the row's one bar (the account-scoped budget
/// bars moved to the provider dashboard). A fresh, unfilled window reads as the
/// hollow [`CONTEXT_EMPTY_GLYPH`].
const CONTEXT_GLYPH: &str = "▣";

/// The context-meter label for an empty (0%) window: a hollow square, the
/// unfilled sibling of `▣`, so a just-started window reads "nothing in it yet".
const CONTEXT_EMPTY_GLYPH: &str = "▢";

/// The expanded card's subagent-section glyph: stacked panes for the children an
/// agent spawned this turn.
const SUBAGENTS_GLYPH: &str = "⧉";

/// Width budget for the agent name on line 1: short agent kinds (`claude`,
/// `codex`) fit comfortably, and a longer name clips with `…` rather than
/// pushing the model/effort tokens off the line.
const NAME_MAX: usize = 12;

/// A just-started agent: idle, sitting on the `Some(0)` baseline context gauge
/// with no real usage or spend history behind it yet. Its 0% bar and zeroed stat
/// lines are noise, so the card collapses to identity + description (+ the
/// last-activity age).
fn idle_unstarted(row: &SidebarRow) -> bool {
    matches!(row.status().unwrap_or(AgentStatus::Idle), AgentStatus::Idle)
        && gauge_percent(row).unwrap_or(0) == 0
        && !row.as_agent().is_some_and(AgentCard::has_session_history)
}

fn agent(row: &SidebarRow) -> Option<&AgentCard> {
    row.as_agent()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn row_lines(
    theme: &Theme,
    row: &SidebarRow,
    providers: &[SidebarProviderPanel],
    now: Timestamp,
    width: usize,
    tier: Tier,
    selected: bool,
    card_density: CardDensityMode,
    animation_phase: u64,
    cost_rolls: &CostRolls,
    bands: &ContextSeverityConfig,
    gutter: Gutter,
) -> Vec<Line<'static>> {
    let cw = content_width(width);
    // Auto/expanded modes keep the stable card shape: selection only appends
    // subagents (expanded appends them on every card). Compact is deliberately
    // different: resting cards trim by status, and the selected card opens back
    // to the full shape.
    let identity = IdentityLineContext {
        theme,
        providers,
        now,
        tier,
        width: cw,
        animation_phase,
        cost_rolls,
    };
    let mut inner = vec![identity_line(identity, row)];
    // An active process row carries its full command on a dim second line under
    // the shell anchor — the build or `sudo` install reads in full while line 1
    // stays the stable shell label. Idle process rows have no detail to add.
    if row.is_process()
        && let Some(line) = process_detail_line(theme, row, cw)
    {
        inner.push(line);
    }
    if let Some(agent) = agent(row) {
        let compact_resting = card_density == CardDensityMode::Compact && !selected;
        if compact_resting {
            match row.status().unwrap_or(AgentStatus::Idle) {
                AgentStatus::Idle => {}
                AgentStatus::Running | AgentStatus::Waiting => {
                    inner.push(description_line(
                        theme,
                        row,
                        tier,
                        cw,
                        selected,
                        animation_phase,
                    ));
                    if let Some(line) = gauge_line(theme, row, bands, cw) {
                        inner.push(line);
                    }
                }
                AgentStatus::Paused | AgentStatus::Success | AgentStatus::Failed => {
                    inner.push(description_line(
                        theme,
                        row,
                        tier,
                        cw,
                        selected,
                        animation_phase,
                    ));
                }
            }
        } else {
            inner.push(description_line(
                theme,
                row,
                tier,
                cw,
                selected,
                animation_phase,
            ));
            // A just-started idle agent sits on the 0% baseline gauge with no
            // history behind it, so it rests at identity + description alone.
            // Once an agent has real context, spend, or compaction history, the
            // bar and the context line — the per-card `▤ · ◌ ◍ ↘ ↗` breakdown
            // with the clock-fill last-activity age — join the resting card.
            if !idle_unstarted(row) {
                if let Some(line) = gauge_line(theme, row, bands, cw) {
                    inner.push(line);
                }
                if let Some(line) = context_tokens_line(theme, row, bands, now, cw) {
                    inner.push(line);
                }
            }
            // The subagents this agent spawned this turn, appended after the
            // stats. Auto and compact show them on the selected card; expanded
            // shows them on every card.
            if (selected || card_density == CardDensityMode::Expanded)
                && !agent.sub_agents.is_empty()
            {
                inner.extend(sub_agent_lines(
                    theme,
                    &agent.sub_agents,
                    cw,
                    animation_phase,
                ));
            }
        }
    }
    inner
        .into_iter()
        .map(|line| with_gutter(theme, line, gutter))
        .collect()
}

/// The expanded card's subagent list: a `⧉ subagents (N)` header — the marker
/// in the delegation violet, the label dim — then up to two indented lines per
/// child. Line 1 leads with the same live cell an agent row wears — the
/// thinking head while the child reasons, the working fill while it acts,
/// the static `✓`/`!` verdict once it finishes — then the type and the
/// description of what the parent asked it to do; line 2 (deeper indent) is
/// its token spend `◇` (the card's whole-unit figure, never a decimal), model,
/// and reasoning effort — one per-card column grid, each slot sized to its
/// widest sibling so the figures, models, and efforts stack — with elapsed
/// work (the clock-fill glyph over a fixed `<1m`/`9m`/`2h` label in the
/// parent's age tone ramp) pinned right under the parent's own stats. Children
/// are
/// subordinate to the parent card, so their text stays at the soft middle
/// weight — the model/effort metadata a step deeper at the dim chrome, like
/// the parent's capability tokens — and indented past the parent's stat
/// lines. The description, tokens,
/// and elapsed ride in from
/// Claude's `subagentStatusLine`; the model, effort, and phase from the
/// child's own lifecycle events. A child with none of them degrades to the
/// bare type line, with line 2 dropped.
fn sub_agent_lines(
    theme: &Theme,
    sub_agents: &[SidebarSubAgent],
    width: usize,
    animation_phase: u64,
) -> Vec<Line<'static>> {
    // The `⧉` marker wears the violet of the delegation/meta family (the
    // compacting head, the `⇅ rc` flag); the label text reads at the soft
    // middle weight like the children below it.
    let mut lines = vec![Line::from(trim_spans_to_width(
        vec![
            Span::styled(
                format!("  {SUBAGENTS_GLYPH}"),
                theme.style(Color::Magenta, Modifier::empty()),
            ),
            Span::styled(format!(" subagents ({})", sub_agents.len()), theme.soft()),
        ],
        width,
    ))];
    // The metadata lines below form one per-card grid: the token figure
    // right-aligns to the widest sibling and the model pads to the widest
    // sibling, so the `·` seams, the models, and the efforts stack into
    // columns across children (the elapsed cluster already stacks via its
    // fixed right-pinned slot). A column exists only while some child carries
    // the field; a child missing a carried field blank-fills the slot.
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
            Span::styled(sub.name.clone(), theme.soft()),
        ];
        // Prefer the `subagentStatusLine` description; fall back to the task
        // descriptor, shown only when it differs from the name (the name already
        // is the type for most children) so the line never reads `Explore —
        // Explore`.
        let detail = sub
            .description
            .as_deref()
            .or(sub.task.as_deref().filter(|task| *task != sub.name));
        if let Some(detail) = detail {
            spans.push(Span::styled(format!(" — {detail}"), theme.soft()));
        }
        lines.push(Line::from(trim_spans_to_width(spans, width)));

        if let Some(line) = sub_agent_metadata_line(theme, sub, token_col, model_col, width) {
            lines.push(line);
        }
    }
    lines
}

fn sub_agent_tokens(sub: &SidebarSubAgent) -> Option<u64> {
    sub.total_tokens.filter(|total| *total > 0)
}

fn sub_agent_metadata_line(
    theme: &Theme,
    sub: &SidebarSubAgent,
    token_col: usize,
    model_col: usize,
    width: usize,
) -> Option<Line<'static>> {
    let tokens = sub_agent_tokens(sub);
    let elapsed = sub.elapsed_secs;
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
                TOKENS_TOTAL,
                theme.style(Color::Blue, Modifier::empty()),
            ));
            left.push(Span::styled(
                format!(" {:>token_col$}", tokens_int(total)),
                theme.soft(),
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
                left.push(Span::styled(" · ", theme.dim()));
            } else {
                left.push(Span::raw(" ".repeat(seam)));
            }
            left.push(Span::styled(
                format!("{:<model_col$}", model_label(model)),
                theme.dim(),
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
        left.push(Span::styled(" · ", theme.dim()));
    } else if token_col > 0 || model_col > 0 {
        left.push(Span::raw("   "));
    }
    left.push(Span::styled(effort.to_owned(), theme.dim()));
}

fn sub_agent_elapsed(theme: &Theme, elapsed: Option<i64>) -> Vec<Span<'static>> {
    elapsed
        .map(|secs| {
            vec![Span::styled(
                format!("{} {:>3}", elapsed_glyph(secs), elapsed_label(secs)),
                activity_age_style(theme, secs),
            )]
        })
        .unwrap_or_default()
}
