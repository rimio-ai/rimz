//! Type-led wait entries share the subagent layout and pin armed age right. Command and check waits carry their command on line 2; shells do so when a command is known, and signals carry a deadline there when present. Timer, pid, and file waits take one line. Live watches retain the working animation; timers and signals retain their static leads. Waits precede background shells, with signals last.

use jiff::Timestamp;

use crate::agents::{
    BackgroundShell, PendingWait, PendingWaitTrigger, single_line_description, usable_description,
};
use crate::proc::command::{command_program_basename, program_label};

use super::*;

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
    let (signals, others): (Vec<_>, Vec<_>) = waits
        .iter()
        .partition(|wait| matches!(wait.trigger, PendingWaitTrigger::Signal { .. }));
    let mut lines = Vec::new();
    for entry in others
        .into_iter()
        .map(|wait| wait_entry(ctx, wait))
        .chain(shells.iter().map(|shell| shell_entry(ctx, shell)))
        .chain(signals.into_iter().map(|wait| wait_entry(ctx, wait)))
    {
        push_entry(ctx, &mut lines, entry);
    }
    lines
}

fn wait_entry(ctx: &RowCtx<'_>, wait: &PendingWait) -> Entry {
    let theme = ctx.theme;
    let lead = match wait.trigger {
        PendingWaitTrigger::Timer { .. } => theme.glyph(GlyphRole::CardWaitTimer).to_owned(),
        PendingWaitTrigger::Signal { .. } => theme.glyph(GlyphRole::CardWaitSignal).to_owned(),
        _ => role_glyph(theme, AnimationRole::Working, ctx.animation_phase),
    };
    entry(
        ctx,
        lead,
        wait.trigger.kind_word(),
        Some(wait.trigger.headline(ctx.now)),
        wait.trigger.detail(ctx.now),
        wait.armed_at,
    )
}

fn shell_entry(ctx: &RowCtx<'_>, shell: &BackgroundShell) -> Entry {
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
    entry(
        ctx,
        role_glyph(ctx.theme, AnimationRole::Working, ctx.animation_phase),
        "shell",
        headline,
        detail,
        Some(shell.started_at),
    )
}

/// A wait's words in the shared entry shape: the lead in the wait tone, the
/// armed age pinned right, and the detail muted on line 2.
fn entry(
    ctx: &RowCtx<'_>,
    lead: String,
    kind: &str,
    headline: Option<String>,
    detail: Option<String>,
    since: Option<Timestamp>,
) -> Entry {
    let theme = ctx.theme;
    Entry {
        lead: Span::styled(lead, theme.styled(Component::WaitHeader, Modifier::empty())),
        kind: kind.to_owned(),
        headline,
        right: since
            .map(|at| {
                vec![Span::styled(
                    elapsed_cluster(theme, age_secs(at, ctx.now)),
                    theme.muted(),
                )]
            })
            .unwrap_or_default(),
        detail: detail.map(|detail| {
            Line::from(trim_spans_to_width(
                vec![Span::raw("      "), Span::styled(detail, theme.muted())],
                content_width(ctx.width),
            ))
        }),
    }
}
