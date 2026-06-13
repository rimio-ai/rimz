use super::*;

/// The agent name's style: the matching provider brand color at full weight, so
/// the name ties to the provider dashboard. Falls back to mid-gray chrome (no
/// DIM modifier) when no provider matches the kind.
pub(super) fn agent_name_style(
    theme: &Theme,
    providers: &[SidebarProviderPanel],
    kind: &str,
    unread: bool,
) -> Style {
    let style = providers
        .iter()
        .find(|panel| panel.kind == kind)
        .map(|panel| theme.style(theme.brand_tone(panel), Modifier::empty()))
        .unwrap_or_else(|| theme.style(Color::DarkGray, Modifier::empty()));
    if unread {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

pub(super) struct IdentityLineContext<'a> {
    pub(super) theme: &'a Theme,
    pub(super) providers: &'a [SidebarProviderPanel],
    pub(super) now: Timestamp,
    pub(super) tier: Tier,
    pub(super) width: usize,
    pub(super) animation_phase: u64,
    pub(super) cost_rolls: &'a CostRolls,
}

pub(super) fn identity_line(ctx: IdentityLineContext<'_>, row: &SidebarRow) -> Line<'static> {
    if row.is_process() {
        return process_row_line(ctx.theme, row, ctx.width, ctx.animation_phase);
    }

    if let Some(resolver) = row.resolver() {
        let resolver_name = resolver
            .display_name
            .as_deref()
            .unwrap_or_else(|| resolver.resolver_id.as_str());
        let remaining = resolver
            .budget_until
            .map(|deadline| time_remaining(deadline, ctx.now))
            .unwrap_or_else(|| "?".to_owned());
        // A resolver mid-flight is the one "waiting for an answer" motion: a
        // braille spinner while the resolver composes the decision, bounded by
        // its budget. The resolver + budget fill the slot a task would.
        return composed_row(
            ctx.theme,
            Span::styled(
                resolver_glyph(ctx.theme, ctx.animation_phase),
                resolver_style(ctx.theme, ctx.animation_phase),
            ),
            &row.name,
            &format!("{resolver_name} {remaining}"),
            row.last_activity,
            ctx.now,
            ctx.width,
        );
    }

    let status = row.status().unwrap_or(AgentStatus::Idle);
    agent_identity_line(
        ctx.theme,
        row,
        ctx.providers,
        ctx.now,
        status,
        ctx.tier,
        ctx.width,
        ctx.animation_phase,
        ctx.cost_rolls,
    )
}

/// The leading status cell for an agent row, applying the two transient render
/// overlays before the base status glyph: a **compacting** head (a violet bar
/// pulsing as the context window condenses) and a **waiting-on-subagents** head
/// (a quiet clay wave while a live child runs). Both are short-lived and stay
/// out of the cockpit tally — they ride over the row's base status here. A
/// human-blocked `?`/`!` always wins, so the overlays defer to those; otherwise
/// the cell is the animated working/thinking fill or the static status glyph.
pub(super) fn agent_lead_cell(
    theme: &Theme,
    row: &SidebarRow,
    status: AgentStatus,
    now: Timestamp,
    animation_phase: u64,
) -> Span<'static> {
    let actionable = status.is_actionable();
    if !actionable && agent(row).is_some_and(|agent| agent.compacting) {
        return Span::styled(
            compacting_glyph(theme, animation_phase),
            compacting_head_style(theme, animation_phase),
        );
    }
    if status == AgentStatus::Running
        && agent(row).is_some_and(|agent| {
            agent
                .sub_agents
                .iter()
                .any(|child| child.status == AgentStatus::Running)
        })
    {
        return Span::styled(
            subagent_glyph(theme, animation_phase),
            subagent_head_style(theme, animation_phase),
        );
    }
    // A blocked `?`/`!` breathes — a slow brightness pulse via
    // `attention_glyph_style` — to pull the eye back to an unanswered row. It
    // never blanks, so the one-cell column never shifts as it swells and fades.
    let style = agent_card_lead_style(theme, row, status, now, animation_phase);
    Span::styled(
        agent_glyph(theme, status, row.phase(), animation_phase),
        style,
    )
}

fn agent_card_lead_style(
    theme: &Theme,
    row: &SidebarRow,
    status: AgentStatus,
    now: Timestamp,
    animation_phase: u64,
) -> Style {
    if status == AgentStatus::Idle {
        return theme.soft();
    }
    agent_lead_style(
        theme,
        status,
        row.phase(),
        age_secs(row.last_activity, now),
        animation_phase,
        row.unread,
    )
}

/// Line 1 for an agent: the leading cell (the working fill or thinking head
/// while active; a blocked `?`/`!` breathes a slow brightness pulse), the agent
/// name, then the dim capability tokens (`· model · effort · window`) with the
/// bold `$cost` (dollar green) pinned right — counting up through the row's
/// stepped [`CostRolls`] roll as a turn lands, with the shared settle brighten.
/// The window token is the model's context window (`258k`, `1M`) — the
/// context-sidecar reading first, the hook-derived fallback second,
/// omitted when neither has named it. Capability tokens degrade by width tier:
/// L2 carries model + effort + window, L1 drops effort, L0 keeps just the name
/// — cost always pins right. A blocked `?`/`!` glyph slides through the age
/// heat ramp toward alarm, so a long-ignored ask escalates without a timestamp.
#[allow(clippy::too_many_arguments)]
pub(super) fn agent_identity_line(
    theme: &Theme,
    row: &SidebarRow,
    providers: &[SidebarProviderPanel],
    now: Timestamp,
    status: AgentStatus,
    tier: Tier,
    width: usize,
    animation_phase: u64,
    cost_rolls: &CostRolls,
) -> Line<'static> {
    // Right cluster, built first so the left trims to whatever's left: the
    // session cost, bold in dollar green, read through the row's stepped roll so
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
            theme.money_style(Modifier::BOLD)
        };
        right.push(Span::styled(dollars2(usd), style));
    }

    // Left cluster: glyph + name + the capability tokens at the dim chrome —
    // metadata, a step under the soft stat figures — over `·` seams of the
    // same weight. The glyph heats continuously with the age clock once a
    // `waiting`/`failed` row sits unanswered. The kind name reads at normal
    // weight in the provider's brand color (or mid-gray chrome for unknown
    // kinds); the bright slot is saved for the task below.
    let mut left: Vec<Span<'static>> = vec![
        agent_lead_cell(theme, row, status, now, animation_phase),
        Span::raw(" "),
        Span::styled(
            clip(&row.name, NAME_MAX),
            agent_name_style(theme, providers, &row.name, row.unread),
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
pub(super) fn display_context_window(row: &SidebarRow) -> Option<u64> {
    ctx(row)
        .and_then(|context| context.tokens.as_ref())
        .and_then(|tokens| tokens.context_window_size)
        .or_else(|| agent(row).and_then(|agent| agent.context_window))
        .filter(|window| *window > 0)
}
