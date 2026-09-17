//! Wait entries share the subagent grammar: the lead shows liveness, line 1 is
//! the wait itself with the elapsed clock pinned right, and line 2 appears only
//! for a described background shell, carrying its command. A live watch (a
//! command, pid, or check wait, or a background shell) wears the working animation in
//! the subordinate wait tone while it runs; a timer or a signal holds its static
//! kind glyph, since nothing runs until it fires. The parent's own status head
//! carries sleeping. Timers come first, then live watches (command, pid, and
//! check waits, then background shells), then signals. The words come from
//! `PendingWaitTrigger::summary`; this module adds leads and layout.

use jiff::Timestamp;

use crate::agents::{
    BackgroundShell, PendingWait, PendingWaitTrigger, single_line_description, usable_description,
};
use crate::proc::command::command_program_basename;

use super::*;

enum WaitLead {
    Kind(GlyphRole),
    Working,
}

struct WaitEntry {
    lead: WaitLead,
    text: String,
    detail: Option<String>,
    since: Option<Timestamp>,
}

/// A wait its watcher is actively working on: it animates while armed.
pub(super) fn is_live_watch(trigger: &PendingWaitTrigger) -> bool {
    matches!(
        trigger,
        PendingWaitTrigger::Pid { .. }
            | PendingWaitTrigger::Command { .. }
            | PendingWaitTrigger::Check { .. }
    )
}

pub(super) fn wait_entry_lines(
    ctx: &RowCtx<'_>,
    waits: &[PendingWait],
    shells: &[BackgroundShell],
) -> Vec<Line<'static>> {
    let wait_entry = |wait: &PendingWait| WaitEntry {
        lead: match wait.trigger {
            PendingWaitTrigger::Timer { .. } => WaitLead::Kind(GlyphRole::CardWaitTimer),
            PendingWaitTrigger::Pid { .. }
            | PendingWaitTrigger::Command { .. }
            | PendingWaitTrigger::Check { .. } => WaitLead::Working,
            PendingWaitTrigger::Signal { .. } => WaitLead::Kind(GlyphRole::CardWaitSignal),
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
    let command = shell
        .command
        .as_deref()
        .and_then(|command| single_line_description(&command_program_basename(command)));
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
        lead: WaitLead::Working,
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
            match entry.lead {
                WaitLead::Kind(role) => theme.glyph(role).to_owned(),
                WaitLead::Working => role_glyph(theme, AnimationRole::Working, ctx.animation_phase),
            },
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
