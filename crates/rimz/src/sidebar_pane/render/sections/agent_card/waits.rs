//! Pending waits name their kind: timers by due, pid/shell watches, then signals. Only shell waits carry a second command line.

use crate::agents::{PendingWake, PendingWakeTrigger};
use crate::proc::command::{command_program_basename, program_label};

use super::*;

pub(super) fn wait_entry_lines(ctx: &RowCtx<'_>, wakes: &[PendingWake]) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let width = content_width(ctx.width);
    let mut lines = Vec::new();
    for wake in wakes {
        let summary = match &wake.trigger {
            PendingWakeTrigger::Command { command } => format!("shell {}", program_label(command)),
            _ => wake.trigger.summary(ctx.now),
        };
        let (lead, lead_style) = match &wake.trigger {
            PendingWakeTrigger::Command { .. } | PendingWakeTrigger::Pid { .. } => (
                role_glyph(theme, AnimationRole::Working, ctx.animation_phase),
                working_style(theme, ctx.animation_phase).add_modifier(Modifier::DIM),
            ),
            PendingWakeTrigger::Timer { .. } => (
                theme.glyph(GlyphRole::CardWaitTimer).to_owned(),
                theme.styled(Component::WakeHeader, Modifier::empty()),
            ),
            PendingWakeTrigger::Signal { .. } => (
                theme.glyph(GlyphRole::CardWaitSignal).to_owned(),
                theme.styled(Component::WakeHeader, Modifier::empty()),
            ),
        };
        let left = vec![
            Span::raw("    "),
            Span::styled(lead, lead_style),
            Span::raw(" "),
            Span::styled(summary, theme.body()),
        ];
        let elapsed = wake
            .armed_at
            .map(|at| {
                let secs = age_secs(at, ctx.now);
                vec![Span::styled(elapsed_cluster(theme, secs), theme.muted())]
            })
            .unwrap_or_default();
        lines.push(pin_right(left, elapsed, width));

        if let PendingWakeTrigger::Command { command } = &wake.trigger {
            let detail = vec![
                Span::raw("      "),
                Span::styled(command_program_basename(command), theme.muted()),
            ];
            lines.push(Line::from(trim_spans_to_width(detail, width)));
        }
    }
    lines
}
