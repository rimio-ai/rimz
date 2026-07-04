use super::*;

/// The compose-affordance frames for a selected, awaiting-first-prompt card:
/// a bracketed ellipsis that grows `[.  ]` -> `[.. ]` -> `[...]`.
const AWAITING_DOTS: [&str; 3] = ["[.  ]", "[.. ]", "[...]"];

/// Hold each dot frame across four base animation phases so the placeholder
/// reads as calm motion on the breath grid.
const AWAITING_DOT_STEP: u64 = 4;

pub(super) fn awaiting_dots_frame(animation_phase: u64) -> &'static str {
    AWAITING_DOTS[((animation_phase / AWAITING_DOT_STEP) as usize) % AWAITING_DOTS.len()]
}

/// The compose-affordance line shown where the description will land once a
/// prompt arrives.
pub(super) fn awaiting_prompt_line(
    theme: &Theme,
    animation_phase: u64,
    width: usize,
) -> Line<'static> {
    Line::from(trim_spans_to_width(
        vec![
            Span::raw("  "),
            Span::styled(
                awaiting_dots_frame(animation_phase).to_owned(),
                theme.faint(),
            ),
        ],
        width,
    ))
}

/// Line 2 for an agent: the description on its own full-width line. Rich
/// context metadata wins first; for Codex that is app-server `preview`, then
/// thread `name`. Named sessions and live task/prompt fallbacks keep the row
/// labelled when richer metadata is absent. An idle agent with nothing to show
/// yet contributes no line; any non-idle empty description falls to an em dash.
/// A turn that died on a provider API error takes the line over the
/// fall-through — the soft upstream error text (`turn_error_label`, quoted
/// verbatim) is the row's most important fact while the `!` escalation holds,
/// and the fall-through returns once it clears. The row emphasis that drives
/// the glyph and name also drives the description: unread actionable rows and
/// unread results blink, rows worth a look or selected rows read at full body
/// weight, and calm unselected rows soften.
pub(super) fn description_line(
    theme: &Theme,
    row: &SidebarRow,
    width: usize,
    attention: CardAttention,
) -> Line<'static> {
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
    let mut left = vec![Span::raw("  ")];
    if let Some(label) = agent(row)
        .and_then(|agent| agent.turn_error_label.as_deref())
        .and_then(single_line_description)
    {
        left.extend(body_spans(&label, true));
    } else {
        match descriptor(row).and_then(single_line_description) {
            Some(text) => left.extend(body_spans(&text, false)),
            None => left.push(Span::raw("—".to_owned())),
        }
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
    Line::from(trim_spans_to_width(left, width))
}

/// The line-2 description: rich preview, then rich session/thread name, then
/// launch description, task, and latest prompt. Codex maps app-server `preview`
/// to the rich preview and app-server `name` to the rich name, so its concrete
/// order is thread preview → thread name. The activity-bound `task` clears on
/// idle, so the persisted prompt keeps an unnamed session labelled past its
/// turn until it earns richer metadata. `None` when the session has nothing to
/// show — the caller skips blank idle cards or paints an em dash.
pub(super) fn descriptor(row: &SidebarRow) -> Option<&str> {
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
                .and_then(|agent| agent.description.as_deref())
                .filter(|description| usable_description(description))
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

pub(super) fn usable_description(value: &str) -> bool {
    single_line_description(value).is_some() && !looks_like_control_text(value)
}

fn single_line_description(value: &str) -> Option<String> {
    let mut out = String::new();
    let mut pending_space = false;
    for ch in value.chars() {
        if ch.is_whitespace() {
            if !out.is_empty() {
                pending_space = true;
            }
            continue;
        }
        if ch.is_control() {
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    (!out.is_empty()).then_some(out)
}

/// An idle agent with nothing to describe yet — waiting for its first prompt.
pub(in crate::sidebar_pane::render) fn awaiting_first_prompt(row: &SidebarRow) -> bool {
    row.is_agent()
        && matches!(row.status().unwrap_or(AgentStatus::Idle), AgentStatus::Idle)
        && descriptor(row).is_none()
}

/// Whether a description candidate is a harness-injected control turn rather
/// than human-authored text — it leads with one of the synthetic-turn tags. A
/// renderer backstop only; the real guard is `sanitize_user_prompt` in the
/// producer, and the producer-owned tag list is shared here so the two guards
/// cannot drift.
pub(super) fn looks_like_control_text(value: &str) -> bool {
    let trimmed = value.trim_start();
    crate::agents::CONTROL_TAG_PREFIXES
        .iter()
        .any(|tag| trimmed.starts_with(tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awaiting_dots_frame_steps_and_wraps() {
        assert_eq!(awaiting_dots_frame(0), "[.  ]");
        assert_eq!(awaiting_dots_frame(AWAITING_DOT_STEP), "[.. ]");
        assert_eq!(awaiting_dots_frame(2 * AWAITING_DOT_STEP), "[...]");
        assert_eq!(awaiting_dots_frame(3 * AWAITING_DOT_STEP), "[.  ]");
    }
}
