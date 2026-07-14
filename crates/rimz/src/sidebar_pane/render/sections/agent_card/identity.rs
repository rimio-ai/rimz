use super::*;

/// The agent handle's spans under the row's card emphasis, sharing the unread
/// treatment with the lead glyph and description so the three move as one group:
/// the normal tier wears the matching provider brand color, resolved from the
/// agent kind, tying the card to the provider dashboard; the soft tier keeps
/// that brand hue muted to the body tier, so a calm unselected card still reads
/// as its provider; an unread row carries the chosen unread effect — a single
/// styled span for blink/bright, one span per character for the flowing shimmer.
/// The tone resolves from a matched provider panel, then the registered kind's
/// descriptor brand, then mid-gray chrome for a kind with no registered
/// descriptor.
pub(super) fn attention_name_spans(
    theme: &Theme,
    providers: &[SidebarProviderPanel],
    display: &str,
    kind: &str,
    attention: CardAttention,
) -> Vec<Span<'static>> {
    let brand = providers
        .iter()
        .find(|panel| panel.kind == kind)
        .map(|panel| theme.brand_tone(panel))
        .or_else(|| {
            crate::agents::descriptor_by_kind(kind).map(|descriptor| {
                theme.brand_rgb_tone(descriptor.brand.color, Some(descriptor.brand.color_rgb))
            })
        })
        .unwrap_or_else(|| theme.component(Component::UnknownBrand));
    let text = ellipsize(display, NAME_MAX);
    match attention.emphasis {
        CardEmphasis::Blink => unread_run_spans(theme, Some(brand), attention.anim, &text),
        _ => vec![Span::styled(
            text,
            emphasize(theme, Some(brand), attention.emphasis, attention.anim),
        )],
    }
}

pub(super) struct IdentityLineContext<'a> {
    pub(super) theme: &'a Theme,
    pub(super) providers: &'a [SidebarProviderPanel],
    pub(super) tier: Tier,
    pub(super) width: usize,
    pub(super) attention: CardAttention,
    pub(super) animation_phase: u64,
    pub(super) cost_rolls: &'a CostRolls,
}

pub(super) fn identity_line(ctx: IdentityLineContext<'_>, row: &SidebarRow) -> Line<'static> {
    if row.is_process() {
        return process_row_line(ctx.theme, row, ctx.width, ctx.animation_phase);
    }

    let status = row.status().unwrap_or(AgentStatus::Idle);
    agent_identity_line(
        ctx.theme,
        row,
        ctx.providers,
        status,
        ctx.tier,
        ctx.width,
        ctx.attention,
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
    attention: CardAttention,
    animation_phase: u64,
) -> Span<'static> {
    let actionable = status.is_actionable();
    if !actionable && agent(row).is_some_and(|agent| agent.compacting) {
        return Span::styled(
            role_glyph(theme, AnimationRole::Compacting, animation_phase),
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
            role_glyph(theme, AnimationRole::Delegating, animation_phase),
            subagent_head_style(theme, animation_phase),
        );
    }
    // The card emphasis carries the row's attention level; the glyph never
    // blanks, so the one-cell column never shifts as it brightens.
    let style = agent_lead_style_with_attention(theme, status, row.phase(), attention);
    Span::styled(
        agent_glyph(theme, status, row.phase(), animation_phase),
        style,
    )
}

/// Line 1 for an agent: the leading cell (the working fill or thinking head
/// while active; attention rows use the card emphasis), the agent
/// name, then the dim capability tokens (`· model · reasoning · window`) with the
/// bold `$cost` (dollar green) pinned right — counting up through the row's
/// stepped [`CostRolls`] roll as a turn lands, with the shared settle brighten.
/// The window token is the model's context window (`258k`, `1m`) — the
/// context-sidecar reading first, the row's carried/default fallback second.
/// It rides only non-idle cards; idle cards keep model and reasoning configuration but drop the
/// window until work starts.
/// The whole capability cluster rides behind a resolved model: with none named,
/// reasoning and window tokens drop too (a bare `272k` names nothing). Capability
/// tokens then degrade by width tier: L2 carries model + reasoning + window, L1
/// drops reasoning, L0 — and any model-less row — keeps just the name; cost always
/// pins right. A blocked `?`/`!`/`⏸` glyph holds its fixed status tone — yellow,
/// red, blue — with the unread attention effect, not age, drawing the eye to an
/// unanswered ask.
#[allow(clippy::too_many_arguments)]
pub(super) fn agent_identity_line(
    theme: &Theme,
    row: &SidebarRow,
    providers: &[SidebarProviderPanel],
    status: AgentStatus,
    tier: Tier,
    width: usize,
    attention: CardAttention,
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
            theme.value_flash()
        } else {
            theme.money_style(Modifier::BOLD)
        };
        right.push(Span::styled(dollars2(usd), style));
    }

    // Left cluster: glyph + name + the capability tokens at the dim chrome —
    // metadata, a step under the soft stat figures — over `·` seams of the
    // same weight. The glyph holds its fixed status tone; the unread attention
    // effect supplies any motion. The display handle reads at normal weight in
    // the provider's brand color (or mid-gray chrome for kinds with no
    // registered descriptor); the bright slot is saved for the task below.
    let mut left: Vec<Span<'static>> = vec![
        agent_lead_cell(theme, row, status, attention, animation_phase),
        Span::raw(" "),
    ];
    left.extend(attention_name_spans(
        theme,
        providers,
        row.display_name(),
        &row.name,
        attention,
    ));
    // The capability cluster is the model and its properties: effort or thinking
    // configures the model, and the window is the model's window. With no model resolved a
    // bare `xhigh`/`272k` names nothing, so the whole cluster rides behind a
    // known model — a model-less row reads like L0, just the handle.
    if tier != Tier::L0
        && let Some(model) = display_model(row)
    {
        left.push(Span::styled(" · ", theme.muted()));
        left.push(Span::styled(model, theme.muted()));
        if tier == Tier::L2
            && let Some(reasoning) = display_reasoning(row)
        {
            left.push(Span::styled(" · ", theme.muted()));
            left.push(Span::styled(reasoning.to_owned(), theme.muted()));
        }
        // The window token keeps the capability tokens' DIM weight — metadata,
        // not a status signal — but tints by size class (`window_style`) so
        // the magnitude reads at a glance; the context-meter severity ramp
        // keeps the loud color slot.
        if status != AgentStatus::Idle
            && let Some(window) = display_context_window(row)
        {
            left.push(Span::styled(" · ", theme.muted()));
            left.push(Span::styled(
                window_short(window),
                window_style(theme, window),
            ));
        }
    }
    pin_right(left, right, width)
}

/// The model's context window for the identity line (`258k`, `1m`). Prefers the
/// out-of-band runtime reading and falls back to the row's carried/default
/// window.
pub(super) fn display_context_window(row: &SidebarRow) -> Option<u64> {
    agent(row)
        .and_then(|agent| {
            agent
                .context
                .as_ref()
                .and_then(|context| context.tokens.as_ref())
                .and_then(|tokens| tokens.context_window_size)
                .or(agent.context_window)
                .or_else(|| {
                    crate::agents::descriptor_by_kind(row.name.as_str())
                        .and_then(|descriptor| descriptor.default_context_window)
                })
        })
        .filter(|window| *window > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ThemeConfig, ThemeMode};

    fn normal_attention() -> CardAttention {
        CardAttention {
            emphasis: CardEmphasis::Normal,
            anim: None,
        }
    }

    fn truecolor_theme() -> Theme {
        Theme::fixed_for_theme(
            false,
            &ThemeConfig {
                mode: ThemeMode::Truecolor,
                ..ThemeConfig::default()
            },
        )
    }

    #[test]
    fn attention_name_keeps_kind_role_handle_in_full() {
        let theme = truecolor_theme();

        let spans =
            attention_name_spans(&theme, &[], "claude-docsmith", "claude", normal_attention());

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "claude-docsmith");
    }

    #[test]
    fn attention_name_clips_long_profile_with_single_cell_ellipsis() {
        let theme = truecolor_theme();

        let spans = attention_name_spans(
            &theme,
            &[],
            "opencode-docsmithery",
            "opencode",
            normal_attention(),
        );

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "opencode-docsmith…");
        assert_eq!(spans[0].width(), NAME_MAX);
    }

    #[test]
    fn attention_name_registered_kind_uses_descriptor_brand_without_provider_panel() {
        let theme = truecolor_theme();

        let spans = attention_name_spans(&theme, &[], "claude", "claude", normal_attention());

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style.fg, Some(theme.clay()));
        assert_ne!(
            spans[0].style.fg,
            Some(theme.component(Component::UnknownBrand))
        );
    }

    #[test]
    fn attention_name_unregistered_kind_without_provider_panel_uses_unknown_brand() {
        let theme = truecolor_theme();

        let spans = attention_name_spans(&theme, &[], "nope", "nope", normal_attention());

        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].style.fg,
            Some(theme.component(Component::UnknownBrand))
        );
    }
}
