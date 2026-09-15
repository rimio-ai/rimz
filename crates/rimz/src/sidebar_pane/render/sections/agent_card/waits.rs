//! Wait entries share the subagent grammar: the lead glyph is the kind (`◷`
//! timer, `❯` shell job, `⌁` signal), line 1 is the wait itself with the
//! elapsed clock pinned right, and line 2 appears only for a described
//! background shell, carrying its command. Timers come first, then shell jobs
//! (command and pid waits, then background shells), then signals. The words
//! come from `PendingWaitTrigger::summary`; this module adds glyphs and layout.

use jiff::Timestamp;

use crate::agents::{
    BackgroundShell, PendingWait, PendingWaitTrigger, single_line_description, usable_description,
};
use crate::proc::command::command_program_basename;

use super::*;

struct WaitEntry {
    lead: GlyphRole,
    text: String,
    detail: Option<String>,
    since: Option<Timestamp>,
}

pub(super) fn wait_entry_lines(
    ctx: &RowCtx<'_>,
    waits: &[PendingWait],
    shells: &[BackgroundShell],
) -> Vec<Line<'static>> {
    let wait_entry = |wait: &PendingWait| WaitEntry {
        lead: match wait.trigger {
            PendingWaitTrigger::Timer { .. } => GlyphRole::CardWaitTimer,
            PendingWaitTrigger::Pid { .. } | PendingWaitTrigger::Command { .. } => {
                GlyphRole::CardWaitShell
            }
            PendingWaitTrigger::Signal { .. } => GlyphRole::CardWaitSignal,
        },
        text: wait.trigger.summary(ctx.now),
        detail: None,
        since: wait.armed_at,
    };
    let (signals, others): (Vec<_>, Vec<_>) = waits
        .iter()
        .partition(|wait| matches!(wait.trigger, PendingWaitTrigger::Signal { .. }));
    let mut lines = Vec::new();
    for entry in others
        .into_iter()
        .map(wait_entry)
        .chain(shells.iter().map(shell_entry))
        .chain(signals.into_iter().map(wait_entry))
    {
        push_entry(ctx, &mut lines, entry);
    }
    lines
}

/// A described shell reads as its description over its command; otherwise the
/// command alone takes line 1.
fn shell_entry(shell: &BackgroundShell) -> WaitEntry {
    let command = shell.command.as_deref().map(command_program_basename);
    let description = shell
        .description
        .as_deref()
        .filter(|value| usable_description(value))
        .and_then(single_line_description);
    let (text, detail) = match (description, command) {
        (Some(description), command) => (description, command),
        (None, Some(command)) => (command, None),
        (None, None) => ("background job".to_owned(), None),
    };
    WaitEntry {
        lead: GlyphRole::CardWaitShell,
        text,
        detail,
        since: Some(shell.started_at),
    }
}

fn push_entry(ctx: &RowCtx<'_>, lines: &mut Vec<Line<'static>>, entry: WaitEntry) {
    let theme = ctx.theme;
    let width = content_width(ctx.width);
    let left = vec![
        Span::raw("    "),
        Span::styled(
            theme.glyph(entry.lead).to_owned(),
            theme.styled(Component::WaitHeader, Modifier::empty()),
        ),
        Span::raw(" "),
        Span::styled(entry.text, theme.body()),
    ];
    let elapsed = entry
        .since
        .map(|at| {
            let secs = age_secs(at, ctx.now);
            vec![Span::styled(elapsed_cluster(theme, secs), theme.muted())]
        })
        .unwrap_or_default();
    lines.push(pin_right(left, elapsed, width));

    if let Some(detail) = entry.detail {
        let detail = vec![Span::raw("      "), Span::styled(detail, theme.muted())];
        lines.push(Line::from(trim_spans_to_width(detail, width)));
    }
}
