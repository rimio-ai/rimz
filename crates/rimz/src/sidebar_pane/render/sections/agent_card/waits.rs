//! Pending waits name their kind: timers by due, pid/shell watches, then signals. Only shell waits carry a second command line. Background shells follow the waits with the shell watch's layout.

use jiff::Timestamp;

use crate::agents::{BackgroundShell, PendingWait, PendingWaitTrigger};
use crate::proc::command::{command_program_basename, program_label};

use super::*;

pub(super) fn wait_entry_lines(ctx: &RowCtx<'_>, waits: &[PendingWait]) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let mut lines = Vec::new();
    for wait in waits {
        let summary = match &wait.trigger {
            PendingWaitTrigger::Command { command } => format!("shell {}", program_label(command)),
            _ => wait.trigger.summary(ctx.now),
        };
        let (lead, lead_style) = match &wait.trigger {
            PendingWaitTrigger::Command { .. } | PendingWaitTrigger::Pid { .. } => {
                working_lead(ctx)
            }
            PendingWaitTrigger::Timer { .. } => (
                theme.glyph(GlyphRole::CardWaitTimer).to_owned(),
                theme.styled(Component::WaitHeader, Modifier::empty()),
            ),
            PendingWaitTrigger::Signal { .. } => (
                theme.glyph(GlyphRole::CardWaitSignal).to_owned(),
                theme.styled(Component::WaitHeader, Modifier::empty()),
            ),
        };
        let detail = match &wait.trigger {
            PendingWaitTrigger::Command { command } => Some(command_program_basename(command)),
            _ => None,
        };
        push_entry(
            ctx,
            &mut lines,
            (lead, lead_style),
            summary,
            wait.armed_at,
            detail,
        );
    }
    lines
}

/// Each shell reads `bg shell {program}` under the working spinner, with its
/// description, else its command, on the dim second line.
pub(super) fn background_shell_entry_lines(
    ctx: &RowCtx<'_>,
    shells: &[BackgroundShell],
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for shell in shells {
        let summary = match &shell.command {
            Some(command) => format!("bg shell {}", program_label(command)),
            None => "bg shell".to_owned(),
        };
        let detail = shell
            .description
            .clone()
            .or_else(|| shell.command.as_deref().map(command_program_basename));
        push_entry(
            ctx,
            &mut lines,
            working_lead(ctx),
            summary,
            Some(shell.started_at),
            detail,
        );
    }
    lines
}

fn working_lead(ctx: &RowCtx<'_>) -> (String, Style) {
    (
        role_glyph(ctx.theme, AnimationRole::Working, ctx.animation_phase),
        working_style(ctx.theme, ctx.animation_phase).add_modifier(Modifier::DIM),
    )
}

fn push_entry(
    ctx: &RowCtx<'_>,
    lines: &mut Vec<Line<'static>>,
    (lead, lead_style): (String, Style),
    summary: String,
    since: Option<Timestamp>,
    detail: Option<String>,
) {
    let theme = ctx.theme;
    let width = content_width(ctx.width);
    let left = vec![
        Span::raw("    "),
        Span::styled(lead, lead_style),
        Span::raw(" "),
        Span::styled(summary, theme.body()),
    ];
    let elapsed = since
        .map(|at| {
            let secs = age_secs(at, ctx.now);
            vec![Span::styled(elapsed_cluster(theme, secs), theme.muted())]
        })
        .unwrap_or_default();
    lines.push(pin_right(left, elapsed, width));

    if let Some(detail) = detail {
        let detail = vec![Span::raw("      "), Span::styled(detail, theme.muted())];
        lines.push(Line::from(trim_spans_to_width(detail, width)));
    }
}
