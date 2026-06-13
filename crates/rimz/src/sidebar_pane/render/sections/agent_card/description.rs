use super::*;

/// Line 2 for an agent: the description on its own full-width line. Rich
/// context metadata wins first; for Codex that is app-server `preview`, then
/// thread `name`. Named sessions and live task/prompt fallbacks keep the row
/// labelled when richer metadata is absent. An idle agent with nothing to show
/// yet paints the static loading-dots cue instead; any other empty description
/// falls to an em dash. A turn that died on a provider API error takes the line
/// over the fall-through — the soft upstream error text (`turn_error_label`,
/// quoted verbatim) is the row's most important fact while the `!` escalation
/// holds, and the fall-through returns once it clears. An unread descriptor
/// renders bold while the row waits for a look. At L2 the todo progress
/// (`●●●○○ 3/5`) pins to a right column, aligning under the cost/age above so
/// the dots read as a tidy gutter instead of floating after the text.
pub(super) fn description_line(
    theme: &Theme,
    row: &SidebarRow,
    now: Timestamp,
    tier: Tier,
    width: usize,
    selected: bool,
    animation_phase: u64,
) -> Line<'static> {
    // The shared unread pulse, on a concrete body tone so the description lifts
    // and dims in unison with the lead glyph and the name — and joins the glow
    // pass, which the terminal-default fg would skip.
    let pulse = unread_pulse(theme, row, now, animation_phase);
    let unread_body = || match pulse {
        Some(sample) => theme.pulse(theme.soft_tone(), sample),
        None => theme.style(theme.soft_tone(), Modifier::BOLD),
    };
    let body = if let Some(label) = agent(row)
        .and_then(|agent| agent.turn_error_label.as_deref())
        .and_then(single_line_description)
    {
        let style = if row.unread {
            unread_body().add_modifier(Modifier::ITALIC)
        } else {
            theme.soft().add_modifier(Modifier::ITALIC)
        };
        Span::styled(label, style)
    } else {
        match descriptor(row).and_then(single_line_description) {
            Some(text) if row.unread => Span::styled(text, unread_body()),
            Some(text) if selected => Span::raw(text),
            Some(text) => Span::styled(text, theme.soft()),
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

/// Whether an agent row paints the idle loading-dots cue in place of a
/// description — an idle agent with nothing to show yet (no preview, session
/// name, task, or prompt), the "waiting for your first prompt" state.
pub(super) fn shows_loading_dots(row: &SidebarRow) -> bool {
    row.is_agent()
        && matches!(row.status().unwrap_or(AgentStatus::Idle), AgentStatus::Idle)
        && descriptor(row).is_none()
}

/// Whether a description candidate is a harness-injected control turn rather
/// than human-authored text — it leads with one of the synthetic-turn tags. A
/// renderer backstop only; the real guard is `sanitize_user_prompt` in the
/// producer.
pub(super) fn looks_like_control_text(value: &str) -> bool {
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
