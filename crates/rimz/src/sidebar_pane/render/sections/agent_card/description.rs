use super::*;
use crate::store::snapshot::CardDescription;

/// The compose-affordance frames for a selected, awaiting-first-prompt card:
/// a fixed-width ellipsis that grows `.  ` -> `.. ` -> `...`.
const AWAITING_DOTS: [&str; 3] = [".  ", ".. ", "..."];

/// Hold each dot frame across ten base animation phases (~1 s at the default
/// 100 ms grid, ~3 s per cycle) so the placeholder reads as a resting pulse.
const AWAITING_DOT_STEP: u64 = 10;

pub(super) fn awaiting_dots_frame(animation_phase: u64) -> &'static str {
    AWAITING_DOTS[((animation_phase / AWAITING_DOT_STEP) as usize) % AWAITING_DOTS.len()]
}

/// The compose-affordance line shown where the description will land once a
/// prompt arrives.
pub(super) fn awaiting_prompt_line(animation_phase: u64, width: usize) -> Line<'static> {
    Line::from(trim_spans_to_width(
        vec![
            Span::raw("  "),
            Span::styled(
                awaiting_dots_frame(animation_phase).to_owned(),
                Style::default(),
            ),
        ],
        width,
    ))
}

/// Line 2 for an agent: the open ask while waiting, else the description on its
/// own full-width line. Rich context metadata wins the fallback; for Codex that
/// is thread `name`, then app-server `preview`. Named sessions and live task/prompt fallbacks keep the row
/// labelled when richer metadata is absent. An idle agent with nothing to show
/// yet contributes no line; any non-idle empty description falls to an em dash.
/// A turn that failed or paused on a provider API error takes the line over the
/// fall-through — the soft upstream error text (`turn_error_label`, quoted
/// verbatim) is the row's most important fact while the failure or pause holds,
/// and the fall-through returns once it clears. The row emphasis that drives
/// the glyph and name also drives the description: unread actionable rows and
/// unread results blink, rows worth a look or selected rows read at full body
/// weight, and calm unselected rows soften.
pub(super) fn description_line(
    ctx: &RowCtx<'_>,
    row: &SidebarRow,
    attention: CardAttention,
) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    spans.extend(description_spans(ctx, row, attention));
    Line::from(trim_spans_to_width(spans, content_width(ctx.width)))
}

pub(super) fn description_spans(
    ctx: &RowCtx<'_>,
    row: &SidebarRow,
    attention: CardAttention,
) -> Vec<Span<'static>> {
    let theme = ctx.theme;
    // The shared unread treatment, on a concrete body tone so the description
    // lifts in unison with the lead glyph and the name. Shimmer flows one span
    // per character; blink and bright stay a single span; calm rows read at the
    // terminal-default (normal) or muted body (soft) tone.
    let body_spans = |text: &str, italic: bool| -> Vec<Span<'static>> {
        let spans = match attention.emphasis {
            CardEmphasis::Blink => {
                unread_run_spans(theme, Some(theme.body_tone()), attention.anim, text)
            }
            CardEmphasis::Normal => vec![Span::styled(text.to_owned(), Style::default())],
            CardEmphasis::Soft => vec![Span::styled(text.to_owned(), theme.body())],
        };
        if italic {
            spans
                .into_iter()
                .map(|mut span| {
                    span.style = span.style.add_modifier(Modifier::ITALIC);
                    span
                })
                .collect()
        } else {
            spans
        }
    };
    let mut left = Vec::new();
    match row.as_agent().map_or(CardDescription::None, |agent| {
        agent.open_ask().map_or_else(
            || agent.description(ctx.now),
            |ask| CardDescription::Activity(ask.to_owned()),
        )
    }) {
        CardDescription::Error(label) => left.extend(body_spans(&label, true)),
        CardDescription::Wait(label) => left.push(Span::styled(
            label,
            theme.body().add_modifier(Modifier::ITALIC),
        )),
        CardDescription::Activity(text) => left.extend(body_spans(&text, false)),
        CardDescription::None => left.push(Span::raw("—".to_owned())),
    }
    // The agent parked its turn on still-in-flight background work: keep the
    // real activity above and add a distinct, faint secondary marker rather than
    // overwriting the description with a synthetic "N background tasks" count.
    if row.phase() == TurnPhase::Parked {
        left.push(Span::styled(
            format!("  {} bg", theme.glyph(GlyphRole::CardParkedBg)),
            theme.faint(),
        ));
    }
    left
}

/// An idle agent with no submitted prompt or authored pre-prompt label. Rich
/// session names and previews are deliberately not evidence: providers may
/// fill those fields with presentation text before the user submits anything.
pub(super) fn awaiting_first_prompt(row: &SidebarRow) -> bool {
    let Some(agent) = row.as_agent() else {
        return false;
    };
    matches!(row.status().unwrap_or(AgentStatus::Idle), AgentStatus::Idle)
        && agent.prompt.is_none()
        && agent
            .description
            .as_deref()
            .is_none_or(|description| !crate::agents::usable_description(description))
        && agent
            .task
            .as_deref()
            .is_none_or(|task| !crate::agents::usable_description(task))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awaiting_dots_frame_steps_and_wraps() {
        assert_eq!(awaiting_dots_frame(0), ".  ");
        assert_eq!(awaiting_dots_frame(AWAITING_DOT_STEP), ".. ");
        assert_eq!(awaiting_dots_frame(2 * AWAITING_DOT_STEP), "...");
        assert_eq!(awaiting_dots_frame(3 * AWAITING_DOT_STEP), ".  ");
    }

    #[test]
    fn awaiting_prompt_line_uses_default_style_for_placeholder() {
        let line = awaiting_prompt_line(0, 8);

        assert_eq!(line.spans[1].content.as_ref(), ".  ");
        assert_eq!(line.spans[1].style, Style::default());
    }
}
