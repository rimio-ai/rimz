//! The per-agent card: identity line, description, the context meter and its
//! token line, and the expanded subagent list. The card anatomy is drawn in
//! docs/interface/sidebar.md; the invariants (selection only appends, never
//! reshapes) live in docs/internals/sidebar.md.

use crate::agents::{AgentContext, TurnPhase};
use crate::config::ContextSeverityConfig;
use crate::feed::{AgentStatus, ContextSeverity};
use crate::{AgentCard, SidebarProviderPanel, SidebarRow, SidebarSubAgent};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::sidebar_renderer::render::CostRolls;
use crate::sidebar_renderer::render::fmt::{
    activity_short, age_secs, clip, dollars2, elapsed_label, model_label, pct_label,
    time_remaining, tokens_int, window_short,
};
use crate::sidebar_renderer::render::labels::{
    SEGMENT_CACHE_READ, SEGMENT_CACHE_WRITE, SEGMENT_INPUT, TOKENS_TOTAL, activity_age_style,
    agent_glyph, agent_style, attention_glyph_style, compacting_glyph, compacting_style,
    context_breakdown_spans, context_total_spans, elapsed_glyph, gauge_spans, loading_dots,
    resolver_glyph, segmented_gauge_spans, severity_color, status_style, subagent_glyph,
    subagent_style, todo_spans, window_style,
};
use crate::sidebar_renderer::render::theme::Theme;

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
/// with no real usage behind it yet. Its 0% bar and zeroed stat lines are noise,
/// so the card collapses to identity + description (+ the last-activity age).
fn idle_unstarted(row: &SidebarRow) -> bool {
    matches!(row.status().unwrap_or(AgentStatus::Idle), AgentStatus::Idle)
        && gauge_percent(row).unwrap_or(0) == 0
}

fn agent(row: &SidebarRow) -> Option<&AgentCard> {
    row.as_agent()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn row_lines(
    theme: &Theme,
    row: &SidebarRow,
    providers: &[SidebarProviderPanel],
    width: usize,
    tier: Tier,
    selected: bool,
    animation_phase: u64,
    cost_rolls: &CostRolls,
    bands: &ContextSeverityConfig,
    gutter: Gutter,
) -> Vec<Line<'static>> {
    let cw = content_width(width);
    // The resting (unselected) card is line 1 (identity), line 2 (description),
    // the ctx bar, and the token line. Selection only *appends* the subagent
    // list; it never reshapes a line already on screen, so the card never reflows
    // on expand. The budgets are account-scoped, so they live in the pinned
    // provider dashboard, never on a row.
    let mut inner = vec![identity_line(
        theme,
        row,
        providers,
        tier,
        cw,
        animation_phase,
        cost_rolls,
    )];
    // An active process row carries its full command on a dim second line under
    // the shell anchor — the build or `sudo` install reads in full while line 1
    // stays the stable shell label. Idle process rows have no detail to add.
    if row.is_process()
        && let Some(line) = process_detail_line(theme, row, cw)
    {
        inner.push(line);
    }
    if let Some(agent) = agent(row) {
        inner.push(description_line(theme, row, tier, cw, animation_phase));
        // A just-started idle agent sits on the 0% baseline gauge with nothing
        // behind it, so it rests at identity + description alone. Once an agent
        // has real context, the bar and the context line — the per-card
        // `▤ · ◌ ◍ ↘ ↗` breakdown with the clock-fill last-activity age — join
        // the resting card.
        if !idle_unstarted(row) {
            if let Some(line) = gauge_line(theme, row, bands, cw) {
                inner.push(line);
            }
            if let Some(line) = context_tokens_line(theme, row, bands, cw) {
                inner.push(line);
            }
        }
        // The subagents this agent spawned this turn, listed only in the expanded
        // card — appended after the stats so the resting card never reflows
        // (selection only ever adds lines).
        if selected && !agent.sub_agents.is_empty() {
            inner.extend(sub_agent_lines(
                theme,
                &agent.sub_agents,
                cw,
                animation_phase,
            ));
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
/// thinking sparkle while the child reasons, the working fill while it acts,
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
    let child_tokens = |sub: &SidebarSubAgent| sub.total_tokens.filter(|total| *total > 0);
    let token_col = sub_agents
        .iter()
        .filter_map(child_tokens)
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
        // child sparkles (reasoning) or fills (acting) in the live clay, a
        // finished one holds its static `✓`/`!` verdict — one head grammar
        // for the parent's cell and its children's.
        let mut spans = vec![
            Span::raw("    "),
            Span::styled(
                agent_glyph(sub.status, sub.phase, animation_phase),
                agent_style(theme, sub.status),
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

        // Line 2: token spend, model, and effort (left) and elapsed work
        // (right-pinned). A deeper indent sets it below the type line; the
        // clock-fill glyph lands under the parent's age and fills with the
        // child's worked span.
        let tokens = child_tokens(sub);
        let elapsed = sub.elapsed_secs;
        let model = sub.model.as_deref();
        let effort = sub.effort.as_deref();
        if tokens.is_some() || elapsed.is_some() || model.is_some() || effort.is_some() {
            let mut left = vec![Span::raw("      ")];
            // Walks token → model → effort; a `·` seam paints only between two
            // fields this child actually renders, and blank-fills its three
            // cells otherwise, so the indent never carries an orphan separator
            // and the columns hold across siblings.
            let mut prev_rendered = false;
            if token_col > 0 {
                match tokens {
                    Some(total) => {
                        // Children stay subordinate to the parent card, so the
                        // figure keeps the soft weight the rest of the subagent
                        // list wears, the `◇` its violet.
                        left.push(Span::styled(
                            TOKENS_TOTAL,
                            theme.style(Color::Magenta, Modifier::empty()),
                        ));
                        left.push(Span::styled(
                            format!(" {:>token_col$}", tokens_int(total)),
                            theme.soft(),
                        ));
                        prev_rendered = true;
                    }
                    // Marker cell + space + figure slot, all blank.
                    None => left.push(Span::raw(" ".repeat(2 + token_col))),
                }
            }
            if model_col > 0 {
                // The model rides after the spend at the parent's dim
                // capability weight, left-padded to the widest sibling so the
                // effort column stacks.
                let seam = if token_col > 0 { 3 } else { 0 };
                match model {
                    Some(model) => {
                        if prev_rendered {
                            left.push(Span::styled(" · ", theme.dim()));
                        } else {
                            left.push(Span::raw(" ".repeat(seam)));
                        }
                        left.push(Span::styled(
                            format!("{:<model_col$}", model_label(model)),
                            theme.dim(),
                        ));
                        prev_rendered = true;
                    }
                    None => left.push(Span::raw(" ".repeat(seam + model_col))),
                }
            }
            if let Some(effort) = effort {
                // Effort keeps the parent line's `model · effort` adjacency and
                // weight. The last left field pads nothing — nothing aligns
                // after it — but still blanks its seam when this child renders
                // no field before it, so sibling efforts stay stacked.
                if prev_rendered {
                    left.push(Span::styled(" · ", theme.dim()));
                } else if token_col > 0 || model_col > 0 {
                    left.push(Span::raw("   "));
                }
                left.push(Span::styled(effort.to_owned(), theme.dim()));
            }
            // Elapsed work in the parent's age vocabulary: the clock-fill glyph
            // and a fixed three-cell m/h label (`<1m`, ` 9m`, ` 2h`, `>1d`,
            // never seconds), toned by the same quarter-hour heat ramp the
            // parent's age wears — so the right-pinned clusters stack into one
            // column across children and a long-running child visibly heats up.
            let right = elapsed
                .map(|secs| {
                    vec![Span::styled(
                        format!("{} {:>3}", elapsed_glyph(secs), elapsed_label(secs)),
                        activity_age_style(theme, secs),
                    )]
                })
                .unwrap_or_default();
            lines.push(pin_right(left, right, width));
        }
    }
    lines
}

/// The agent name's style: its provider's brand color (Claude clay, Codex blue,
/// Provider match: the brand color at full weight so the name ties to the
/// provider dashboard. Falls back to mid-gray chrome (no DIM modifier) when no
/// provider matches the kind.
fn agent_name_style(theme: &Theme, providers: &[SidebarProviderPanel], kind: &str) -> Style {
    providers
        .iter()
        .find(|panel| panel.kind == kind)
        .map(|panel| theme.style(Color::Indexed(panel.color), Modifier::empty()))
        .unwrap_or_else(|| theme.style(Color::DarkGray, Modifier::empty()))
}

fn identity_line(
    theme: &Theme,
    row: &SidebarRow,
    providers: &[SidebarProviderPanel],
    tier: Tier,
    width: usize,
    animation_phase: u64,
    cost_rolls: &CostRolls,
) -> Line<'static> {
    if row.is_process() {
        return process_row_line(theme, row, width, animation_phase);
    }

    if let Some(resolver) = row.resolver() {
        let resolver_name = resolver
            .display_name
            .as_deref()
            .unwrap_or_else(|| resolver.resolver_id.as_str());
        let remaining = resolver
            .budget_until
            .map(time_remaining)
            .unwrap_or_else(|| "?".to_owned());
        // A resolver mid-flight is the one "waiting for an answer" motion: a
        // braille spinner while the resolver composes the decision, bounded by
        // its budget. The resolver + budget fill the slot a task would.
        return composed_row(
            theme,
            Span::styled(
                resolver_glyph(animation_phase),
                status_style(theme, AgentStatus::Waiting),
            ),
            &row.name,
            &format!("{resolver_name} {remaining}"),
            row.last_activity,
            width,
        );
    }

    let status = row.status().unwrap_or(AgentStatus::Idle);
    agent_identity_line(
        theme,
        row,
        providers,
        status,
        tier,
        width,
        animation_phase,
        cost_rolls,
    )
}

/// The leading status cell for an agent row, applying the two transient render
/// overlays before the base status glyph: a **compacting** head (a violet bar
/// pulsing as the context window condenses) and a **waiting-on-subagents** head
/// (a quiet clay wave while a live child runs). Both are short-lived and stay
/// out of the cockpit tally — they ride over the row's base status here. A
/// human-blocked `?`/`!` always wins, so the overlays defer to those; otherwise
/// the cell is the animated working/thinking fill or the static status glyph.
fn agent_lead_cell(
    theme: &Theme,
    row: &SidebarRow,
    status: AgentStatus,
    animation_phase: u64,
) -> Span<'static> {
    let actionable = status.is_actionable();
    if !actionable && agent(row).is_some_and(|agent| agent.compacting) {
        return Span::styled(compacting_glyph(animation_phase), compacting_style(theme));
    }
    if status == AgentStatus::Running
        && agent(row).is_some_and(|agent| {
            agent
                .sub_agents
                .iter()
                .any(|child| child.status == AgentStatus::Running)
        })
    {
        return Span::styled(subagent_glyph(animation_phase), subagent_style(theme));
    }
    // A blocked `?`/`!` breathes — a slow brightness pulse via
    // `attention_glyph_style` — to pull the eye back to an unanswered row. It
    // never blanks, so the one-cell column never shifts as it swells and fades.
    Span::styled(
        agent_glyph(status, row.phase(), animation_phase),
        attention_glyph_style(theme, status, age_secs(row.last_activity), animation_phase),
    )
}

/// Line 1 for an agent: the leading cell (the working fill or thinking sparkle
/// while active; a blocked `?`/`!` breathes a slow brightness pulse), the agent
/// name, then the dim capability tokens (`· model · effort · window`) with the
/// bold `$cost` (money-green) pinned right — counting up through the row's
/// stepped [`CostRolls`] roll as a turn lands, with the shared settle brighten.
/// The window token is the model's context window (`258k`, `1M`) — the
/// context-sidecar reading first, the hook-derived fallback second,
/// omitted when neither has named it. Capability tokens degrade by width tier:
/// L2 carries model + effort + window, L1 drops effort, L0 keeps just the name
/// — cost always pins right. A blocked `?`/`!` glyph heats through amber to
/// red on the age clock's quarter-hour ramp, so a long-ignored ask escalates
/// without a timestamp.
#[allow(clippy::too_many_arguments)]
fn agent_identity_line(
    theme: &Theme,
    row: &SidebarRow,
    providers: &[SidebarProviderPanel],
    status: AgentStatus,
    tier: Tier,
    width: usize,
    animation_phase: u64,
    cost_rolls: &CostRolls,
) -> Line<'static> {
    // Right cluster, built first so the left trims to whatever's left: the
    // session cost, bold in money-green, read through the row's stepped roll so
    // an increase ticks up rather than jumps. A cost that rounds to $0.00 — an
    // idle agent that has spent nothing yet — is omitted, not printed as zero
    // (the filter reads the authoritative target, never a mid-climb value).
    let mut right: Vec<Span<'static>> = Vec::new();
    if let Some(target) = ctx(row)
        .and_then(|context| context.cost.as_ref())
        .and_then(|cost| cost.total_cost_usd)
        .filter(|usd| *usd >= 0.005)
    {
        let usd = cost_rolls.display(&row.id, target, animation_phase);
        let style = if cost_rolls.flashing(&row.id, animation_phase) {
            theme.style(VALUE_FLASH, Modifier::BOLD)
        } else {
            theme.style(Color::Green, Modifier::BOLD)
        };
        right.push(Span::styled(dollars2(usd), style));
    }

    // Left cluster: glyph + name + the capability tokens at the dim chrome —
    // metadata, a step under the soft stat figures — over `·` seams of the
    // same weight. The glyph heats with the age clock once a
    // `waiting`/`failed` row sits unanswered. The kind name reads at normal
    // weight in the provider's brand color (or mid-gray chrome for unknown
    // kinds); the bright slot is saved for the task below.
    let mut left: Vec<Span<'static>> = vec![
        agent_lead_cell(theme, row, status, animation_phase),
        Span::raw(" "),
        Span::styled(
            clip(&row.name, NAME_MAX),
            agent_name_style(theme, providers, &row.name),
        ),
    ];
    if tier != Tier::L0 {
        if let Some(model) = display_model(row) {
            left.push(Span::styled(" · ", theme.dim()));
            left.push(Span::styled(model, theme.dim()));
        }
        if tier == Tier::L2
            && let Some(effort) = display_effort(row)
        {
            left.push(Span::styled(" · ", theme.dim()));
            left.push(Span::styled(effort.to_owned(), theme.dim()));
        }
        // The window token keeps the capability tokens' DIM weight — metadata,
        // not a status signal — but tints by size class (`window_style`) so
        // the magnitude reads at a glance; the context-meter severity ramp
        // keeps the loud color slot.
        if let Some(window) = display_context_window(row) {
            left.push(Span::styled(" · ", theme.dim()));
            left.push(Span::styled(
                window_short(window),
                window_style(theme, window),
            ));
        }
    }
    pin_right(left, right, width)
}

/// The model's context window for the identity line (`258k`, `1m`). Prefers the
/// out-of-band runtime reading, falls back to the hook-derived scalar, and
/// omits when neither source has named it.
fn display_context_window(row: &SidebarRow) -> Option<u64> {
    ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.context_window_size)
        .or_else(|| agent(row).and_then(|agent| agent.context_window))
        .filter(|window| *window > 0)
}

/// Line 2 for an agent: the description on its own full-width line. Rich
/// context metadata wins first; for Codex that is app-server `preview`, then
/// thread `name`. Named sessions and live task/prompt fallbacks keep the row
/// labelled when richer metadata is absent. An idle agent with nothing to show
/// yet paints the static loading-dots cue instead; any other empty description
/// falls to an em dash. A turn that died on a provider API error takes the line
/// over the fall-through — the soft upstream error text (`turn_error_label`,
/// quoted verbatim) is the row's most important fact while the `!` escalation
/// holds, and the fall-through returns once it clears. At L2 the todo progress
/// (`●●●○○ 3/5`) pins to a right column, aligning under the cost/age above so
/// the dots read as a tidy gutter instead of floating after the text.
fn description_line(
    theme: &Theme,
    row: &SidebarRow,
    tier: Tier,
    width: usize,
    animation_phase: u64,
) -> Line<'static> {
    let body = if let Some(label) = agent(row).and_then(|agent| agent.turn_error_label.as_deref()) {
        Span::styled(label.to_owned(), theme.soft())
    } else {
        match descriptor(row) {
            Some(text) => Span::raw(text.to_owned()),
            None if shows_loading_dots(row) => {
                Span::styled(loading_dots(animation_phase).to_owned(), theme.dim())
            }
            None => Span::raw("—".to_owned()),
        }
    };
    let mut left = vec![Span::raw("  "), body];
    // The agent parked its turn on still-in-flight background work: keep the
    // real activity above and add a distinct, faint secondary marker rather than
    // overwriting the description with a synthetic "N background tasks" count.
    if row.phase() == TurnPhase::Parked {
        left.push(Span::styled("  ⋯ bg", theme.faint()));
    }
    let todo_total = agent(row).and_then(|agent| agent.todo_total).unwrap_or(0);
    if tier == Tier::L2 && todo_total > 0 {
        let done = agent(row).and_then(|agent| agent.todo_done).unwrap_or(0);
        let total = todo_total;
        return pin_right(left, todo_spans(theme, done, total), width);
    }
    Line::from(trim_spans_to_width(left, width))
}

/// The line-2 description: rich preview, then rich session/thread name, then
/// task, then latest prompt. Codex maps app-server `preview` to the rich preview
/// and app-server `name` to the rich name, so its concrete order is thread
/// preview → thread name. The activity-bound `task` clears on idle, so the
/// persisted prompt keeps an unnamed session labelled past its turn until it
/// earns richer metadata. `None` when the session has nothing to show — the
/// caller paints the idle loading-dots or an em dash.
fn descriptor(row: &SidebarRow) -> Option<&str> {
    // The producer sanitizes prompt/task before they reach the row; this is a
    // last-ditch backstop so a harness control turn (`<task-notification>…`)
    // can never paint the description even if a future producer regressed.
    ctx(row)
        .and_then(|context| context.session_preview.as_deref())
        .filter(|preview| usable_description(preview))
        .or_else(|| {
            ctx(row)
                .and_then(|context| context.session_name.as_deref())
                .filter(|name| usable_description(name))
        })
        .or_else(|| {
            agent(row)
                .and_then(|agent| agent.task.as_deref())
                .filter(|task| usable_description(task))
        })
        .or_else(|| {
            agent(row)
                .and_then(|agent| agent.prompt.as_deref())
                .filter(|prompt| usable_description(prompt))
        })
}

fn usable_description(value: &str) -> bool {
    !value.is_empty() && !looks_like_control_text(value)
}

/// Whether an agent row paints the idle loading-dots cue in place of a
/// description — an idle agent with nothing to show yet (no preview, session
/// name, task, or prompt), the "waiting for your first prompt" state.
fn shows_loading_dots(row: &SidebarRow) -> bool {
    row.is_agent()
        && matches!(row.status().unwrap_or(AgentStatus::Idle), AgentStatus::Idle)
        && descriptor(row).is_none()
}

/// Whether a description candidate is a harness-injected control turn rather
/// than human-authored text — it leads with one of the synthetic-turn tags. A
/// renderer backstop only; the real guard is `sanitize_user_prompt` in the
/// producer.
fn looks_like_control_text(value: &str) -> bool {
    const CONTROL_TAG_PREFIXES: &[&str] = &[
        "<task-notification>",
        "<system-reminder>",
        "<command-message>",
        "<command-name>",
        "<local-command-stdout>",
    ];
    let trimmed = value.trim_start();
    CONTROL_TAG_PREFIXES
        .iter()
        .any(|tag| trimmed.starts_with(tag))
}

/// The session's statusline enrichment, when it published any.
fn ctx(row: &SidebarRow) -> Option<&AgentContext> {
    agent(row).and_then(|agent| agent.context.as_ref())
}

/// Model name preferred from the statusline (`Opus 4.8 (1M context)`) over the
/// coarser transcript scalar (`Opus`), then shortened for the row
/// (`Opus 4.8 (1M)`); never synthesized.
fn display_model(row: &SidebarRow) -> Option<String> {
    ctx(row)
        .and_then(|context| context.model_display_name.as_deref())
        .or_else(|| agent(row).and_then(|agent| agent.model.as_deref()))
        .filter(|model| !model.is_empty())
        .map(model_label)
}

/// Reasoning effort: the hook/ledger value (what the user configured) is
/// preferred; context falls back for sessions whose lifecycle observation has
/// not named it. This means a configured `xhigh` wins over provider/catalog
/// defaults such as `medium` or `high`.
fn display_effort(row: &SidebarRow) -> Option<&str> {
    agent(row)
        .and_then(|agent| agent.effort.as_deref())
        .or_else(|| ctx(row).and_then(|context| context.effort.as_deref()))
        .filter(|effort| !effort.is_empty())
}

/// Column widths for the per-row context meter: a one-cell lead-glyph label
/// (`▣`, sharing the column with the `◇`/`◷` glyphs on the lines below it) and a
/// fixed 5-cell right value, with the bar filling the middle. The value
/// (`78.2%`) fits five cells. The provider dashboard's budget bars carry their
/// own label/value widths but the same shape.
const BAR_LABEL_WIDTH: usize = 1;
const BAR_VALUE_WIDTH: usize = 5;

/// One aligned meter row: `<indent><label:3> <bar> <value:5>`. The caller's
/// `make_bar` builds the colored bar spans to the supplied width and supplies the
/// `label_style` for the lead glyph (the context meter tints its `▣` with the
/// bar's severity); this helper owns the indent, the fixed label and value
/// columns, and the gaps — so every row built through it shares one bar-start
/// column and one value-end column by construction, with no per-call alignment
/// math. The value column reads at the dim chrome weight, matching the token
/// figures below it — the bar's fill carries the urgency.
fn bar_row(
    theme: &Theme,
    label: &str,
    label_style: Style,
    value: &str,
    make_bar: impl FnOnce(usize) -> Vec<Span<'static>>,
    width: usize,
) -> Line<'static> {
    // "  "(2) + label(3) + " "(1) + bar + " "(1) + value(5)
    let bar_width = width
        .saturating_sub(2 + BAR_LABEL_WIDTH + 1 + 1 + BAR_VALUE_WIDTH)
        .max(1);
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(format!("{label:<BAR_LABEL_WIDTH$}"), label_style),
        Span::raw(" "),
    ];
    spans.extend(make_bar(bar_width));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        format!("{value:>BAR_VALUE_WIDTH$}"),
        theme.dim(),
    ));
    Line::from(trim_spans_to_width(spans, width))
}

/// The context meter — the resting card's one bar. `ctx` on the left, the
/// **percent used** on the right (always — the window *size* moves to the
/// expanded token line), the bar between. The fill amount and its calm-blue →
/// yellow → amber → red severity ([`row_severity`], bands from
/// `[sidebar.context]`) come from the used percentage and the absolute tokens;
/// when the statusline reports the per-message token breakdown a *calm* fill is
/// split into colored segments (cache writes / cache reads / fresh input) that
/// add up to exactly that percentage, and a warmed bar goes one solid severity
/// run. The `▣` glyph wears the same severity, so glyph, bar, and the `▤` line
/// below speak one urgency. The value prefers a one-decimal precise fraction
/// (`78.2%`) over the integer gauge. An empty (0%) window reads the hollow
/// `▢`; any usage fills it to `▣`.
fn gauge_line(
    theme: &Theme,
    row: &SidebarRow,
    bands: &ContextSeverityConfig,
    width: usize,
) -> Option<Line<'static>> {
    let percent = gauge_percent(row)?;
    let value = pct_label(precise_context_pct(row), percent);
    let severity = row_severity(row, bands);
    let color = severity_color(severity);
    // The severity decides composition-vs-solid: the segments (where the window
    // went) paint only while the meter rests calm; once it warms the bar goes
    // solid severity.
    let segments = (severity == ContextSeverity::Calm)
        .then(|| gauge_segments(row))
        .flatten();
    let glyph = if percent == 0 {
        CONTEXT_EMPTY_GLYPH
    } else {
        CONTEXT_GLYPH
    };
    Some(bar_row(
        theme,
        glyph,
        theme.style(color, Modifier::empty()),
        &value,
        |bar_width| match &segments {
            Some(segments) => segmented_gauge_spans(theme, segments, color, percent, bar_width),
            None => gauge_spans(theme, color, percent, bar_width),
        },
        width,
    ))
}

/// The row's severity verdict: the tier the producer classified and stamped
/// ([`SidebarRow::context_severity`]) when present, else classified locally
/// from the same inputs and bands — the fallback for a snapshot produced
/// before the stamp (an older producer mid-upgrade). Either way it is
/// [`ContextSeverity::classify`]'s verdict, never a renderer-private ramp.
fn row_severity(row: &SidebarRow, bands: &ContextSeverityConfig) -> ContextSeverity {
    agent(row)
        .and_then(|agent| agent.context_severity)
        .unwrap_or_else(|| {
            ContextSeverity::classify(
                gauge_percent(row).unwrap_or(0),
                row.context_used_tokens(),
                bands,
            )
        })
}

/// A precise context-used fraction (0..=100) from the current-message token
/// composition over the window size, so the `ctx` value can read a decimal
/// (`78.2%`). The composition (`input + cache_creation + cache_read`) is exactly
/// what `used_percentage` measures, so the decimal refines the same number.
/// `None` (no breakdown, or no window size) falls the value back to the integer
/// gauge percent.
fn precise_context_pct(row: &SidebarRow) -> Option<f64> {
    let window = ctx(row)?.tokens.as_ref()?.context_window_size? as f64;
    if window <= 0.0 {
        return None;
    }
    let used = row.context_used_tokens()? as f64;
    Some((used / window * 100.0).clamp(0.0, 100.0))
}

/// The context bar's value — [`SidebarRow::context_gauge_percent`], the same
/// input the producer classified the stamped severity from.
fn gauge_percent(row: &SidebarRow) -> Option<u8> {
    row.context_gauge_percent()
}

/// The context bar's color segments, when the per-message breakdown is known,
/// left to right: cache writes (violet), cache reads (blue), fresh `input`
/// (red) — the shared `SEGMENT_*` tones the context line's markers also wear,
/// so the line legends the bar by construction. The rich statusline blob is
/// preferred; the row-level [`SidebarRow::call_split`] (Codex's rollout
/// `last_token_usage`, which reports no cache-write) stands in when the blob
/// carries no split. `None` when neither source reported one (a fresh
/// session, or a statusline blob cleared by `/compact` — a rollout-fed split
/// refreshes with the next call instead), so the bar falls back to a
/// single-color ramp.
fn gauge_segments(row: &SidebarRow) -> Option<[(u64, Color); 3]> {
    if let Some(usage) = ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.current_usage.as_ref())
    {
        let input = usage.input_tokens.unwrap_or(0);
        let writes = usage.cache_creation_input_tokens.unwrap_or(0);
        let reads = usage.cache_read_input_tokens.unwrap_or(0);
        return (input + writes + reads > 0).then_some([
            (writes, SEGMENT_CACHE_WRITE),
            (reads, SEGMENT_CACHE_READ),
            (input, SEGMENT_INPUT),
        ]);
    }
    let split = row.call_split()?;
    (split.filled() > 0).then_some([
        (0, SEGMENT_CACHE_WRITE),
        (split.cache_read, SEGMENT_CACHE_READ),
        (split.fresh_input, SEGMENT_INPUT),
    ])
}

/// The card's context line — `▤` the filled part of the window (integer
/// magnitudes) with the last-activity age pinned right. `▤` is
/// `input + cache_write + cache_read` of the latest API call — exactly the
/// numerator the `▣` meter scales — so the bar's percent and this absolute
/// figure read as one measurement, and the `▤` head wears the bar's severity
/// tone to seal that pairing. A `·` seam separates the headline from the
/// latest call's composition, ordered by how the window filled: `◌` read back
/// from cache, `◍` newly written to it, `↘` fresh input, `↗` output generated
/// (which joins the window next turn) — each marker in its bar-segment color,
/// so the line doubles as the bar's legend. The `◇` totals stay the cockpit /
/// fleet-ledger / subagent vocabulary — this line answers "what is in the
/// window", not "what did today burn". The rich statusline blob is preferred;
/// the row-level [`SidebarRow::call_split`] (Codex's rollout
/// `last_token_usage`) stands in when the blob carries no split — its
/// cache-write column is unreported there, so it drops from the line. Falls
/// back to the bare `▤` rollup total when neither source has a split (Claude
/// before the first API call and right after `/compact`), so the line shows
/// *something* for every agent. The age rides the right edge only once it
/// crosses a full minute
/// — a just-active agent shows the breakdown alone, left-aligned, rather than
/// a misleading `1m` — as the clock-fill glyph ([`elapsed_glyph`]) over the
/// quarter-stepping age tone ([`activity_age_style`]): dim while warm, yellow
/// from the second quarter, amber past the half hour, red from the hour, when
/// resuming would likely re-read the whole context uncached.
fn context_tokens_line(
    theme: &Theme,
    row: &SidebarRow,
    bands: &ContextSeverityConfig,
    width: usize,
) -> Option<Line<'static>> {
    // The age clock is the line's one right pin — resource stats are
    // process-row vocabulary and never ride an agent card.
    let age = activity_short(row.last_activity)
        .map(|label| {
            let secs = age_secs(row.last_activity);
            vec![Span::styled(
                format!("{} {label}", elapsed_glyph(secs)),
                activity_age_style(theme, secs),
            )]
        })
        .unwrap_or_default();
    // The `▤` head mirrors the bar's severity — the same stamped verdict — so
    // the absolute figure and the meter above it read at one urgency. A row
    // with no gauge percent folds to 0 and lets the token overlay alone speak.
    let severity = severity_color(row_severity(row, bands));
    if let Some(usage) = ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.current_usage.as_ref())
    {
        let input = usage.input_tokens.unwrap_or(0);
        let output = usage.output_tokens.unwrap_or(0);
        let cache_write = usage.cache_creation_input_tokens.unwrap_or(0);
        let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
        let mut left = vec![Span::raw("  ")];
        left.extend(context_breakdown_spans(
            theme,
            severity,
            input + cache_write + cache_read,
            cache_read,
            cache_write,
            input,
            output,
            tokens_int,
        ));
        return Some(pin_right(left, age, width));
    }
    // The row-level split — the lifecycle rail's per-call composition (Codex's
    // rollout `last_token_usage`). Its protocol reports no per-call
    // cache-write, so that column passes 0 and drops from the line.
    if let Some(split) = row.call_split() {
        let mut left = vec![Span::raw("  ")];
        left.extend(context_breakdown_spans(
            theme,
            severity,
            split.filled(),
            split.cache_read,
            0,
            split.fresh_input,
            split.output,
            tokens_int,
        ));
        return Some(pin_right(left, age, width));
    }
    let total = agent(row).and_then(|agent| agent.total_tokens)?;
    let mut left = vec![Span::raw("  ")];
    left.extend(context_total_spans(theme, severity, total, tokens_int));
    Some(pin_right(left, age, width))
}
