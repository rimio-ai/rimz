//! Type-led wait entries share the subagent layout and pin armed age right. Command and check waits carry their command on line 2; shells do so when a command is known, and signals carry a deadline there when present. Timer, pid, and file waits take one line. Live watches retain the working animation; timers and signals retain their static leads. Waits precede background shells, with signals last.

use jiff::Timestamp;

use crate::agents::{
    BackgroundShell, PendingWait, PendingWaitTrigger, single_line_description, usable_description,
};
use crate::proc::command::{command_program_basename, program_label};

use super::*;

struct WaitEntry {
    lead: String,
    kind: &'static str,
    headline: Option<String>,
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
            | PendingWaitTrigger::File { .. }
    )
}

pub(super) fn wait_entry_lines(
    ctx: &RowCtx<'_>,
    waits: &[PendingWait],
    shells: &[BackgroundShell],
) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let working = || role_glyph(theme, AnimationRole::Working, ctx.animation_phase);
    let wait_entry = |wait: &PendingWait| WaitEntry {
        lead: match wait.trigger {
            PendingWaitTrigger::Timer { .. } => theme.glyph(GlyphRole::CardWaitTimer).to_owned(),
            PendingWaitTrigger::Signal { .. } => theme.glyph(GlyphRole::CardWaitSignal).to_owned(),
            _ => working(),
        },
        kind: wait.trigger.kind_word(),
        headline: Some(wait.trigger.headline(ctx.now)),
        detail: wait.trigger.detail(ctx.now),
        since: wait.armed_at,
    };
    let (signals, others): (Vec<_>, Vec<_>) = waits
        .iter()
        .partition(|wait| matches!(wait.trigger, PendingWaitTrigger::Signal { .. }));
    let mut lines = Vec::new();
    for entry in others
        .into_iter()
        .map(wait_entry)
        .chain(shells.iter().map(|shell| shell_entry(shell, working())))
        .chain(signals.into_iter().map(wait_entry))
    {
        let right = entry
            .since
            .map(|at| {
                vec![Span::styled(
                    elapsed_cluster(theme, age_secs(at, ctx.now)),
                    theme.muted(),
                )]
            })
            .unwrap_or_default();
        let detail = entry.detail.map(|detail| {
            Line::from(trim_spans_to_width(
                vec![Span::raw("      "), Span::styled(detail, theme.muted())],
                content_width(ctx.width),
            ))
        });
        push_entry(
            ctx,
            &mut lines,
            Entry {
                lead: Span::styled(
                    entry.lead,
                    theme.styled(Component::WaitHeader, Modifier::empty()),
                ),
                kind: entry.kind.to_owned(),
                headline: entry.headline,
                right,
                detail,
            },
        );
    }
    lines
}

fn shell_entry(shell: &BackgroundShell, lead: String) -> WaitEntry {
    let detail = shell
        .command
        .as_deref()
        .and_then(|command| single_line_description(&command_program_basename(command)));
    let headline = shell
        .description
        .as_deref()
        .filter(|value| usable_description(value))
        .and_then(single_line_description)
        .or_else(|| shell.command.as_deref().map(program_label));
    WaitEntry {
        lead,
        kind: "shell",
        headline,
        detail,
        since: Some(shell.started_at),
    }
}
