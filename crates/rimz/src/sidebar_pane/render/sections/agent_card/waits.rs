//! Type-led wait entries share the subagent layout and pin armed age right. Command and check waits carry their command on line 2; shells do so when a command is known, and signals carry a deadline there when present. Timer, pid, file, and team waits take one line. Live watches retain the working animation; timers, signals, and teams use static leads. Waits precede background shells, with signals last and a pending idle stop after them. Subagent waits use the child's own row instead.

use jiff::Timestamp;

use crate::agents::{
    ATTENTION_AGE_CEILING_SECS, BackgroundShell, PendingIdleStop, PendingWait, PendingWaitTrigger,
    single_line_description, usable_description,
};
use crate::proc::command::{command_program_basename, program_label};

use super::*;

pub(super) fn visible_waits(waits: &[PendingWait]) -> impl Iterator<Item = &PendingWait> {
    waits
        .iter()
        .filter(|wait| !matches!(wait.trigger, PendingWaitTrigger::Subagent { .. }))
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
    idle_stop: Option<&PendingIdleStop>,
) -> Vec<Line<'static>> {
    let (signals, others): (Vec<_>, Vec<_>) = visible_waits(waits).partition(|wait| {
        matches!(
            wait.trigger,
            PendingWaitTrigger::Signal { .. } | PendingWaitTrigger::Condition { .. }
        )
    });
    let mut lines = Vec::new();
    for entry in others
        .into_iter()
        .map(|wait| wait_entry(ctx, wait))
        .chain(shells.iter().map(|shell| shell_entry(ctx, shell)))
        .chain(signals.into_iter().map(|wait| wait_entry(ctx, wait)))
        .chain(idle_stop.map(|stop| {
            entry(
                ctx,
                ctx.theme.glyph(GlyphRole::CardWaitTimer).to_owned(),
                "stop",
                Some(stop.label(ctx.now)),
                None,
                Some(stop.stop.requested_at),
            )
        }))
    {
        push_entry(ctx, &mut lines, entry);
    }
    lines
}

fn wait_entry(ctx: &RowCtx<'_>, wait: &PendingWait) -> Entry {
    let theme = ctx.theme;
    let lead = match wait.trigger {
        PendingWaitTrigger::Timer { .. } => theme.glyph(GlyphRole::CardWaitTimer).to_owned(),
        PendingWaitTrigger::Signal { .. }
        | PendingWaitTrigger::Team { .. }
        | PendingWaitTrigger::Condition { .. } => theme.glyph(GlyphRole::CardWaitSignal).to_owned(),
        _ => role_glyph(theme, AnimationRole::Working, ctx.animation_phase),
    };
    entry(
        ctx,
        lead,
        wait.trigger.kind_word(),
        Some(wait.trigger.headline(ctx.now)),
        wait.trigger.detail(),
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
        focused: false,
        lead: Span::styled(lead, theme.styled(Component::WaitHeader, Modifier::empty())),
        kind: kind.to_owned(),
        headline: headline.map(|headline| Span::styled(headline, theme.body())),
        right: since
            .map(|at| {
                vec![Span::styled(
                    elapsed_cluster(theme, age_secs(at, ctx.now), ATTENTION_AGE_CEILING_SECS),
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
