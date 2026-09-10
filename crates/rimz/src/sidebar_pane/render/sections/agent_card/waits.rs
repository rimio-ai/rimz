//! Pending-wait entries in projection order: timers by due, commands, then signals.

use crate::agents::{PendingWake, PendingWakeTrigger};
use crate::proc::command::{command_program_basename, program_label};

use super::*;

pub(super) fn wait_entry_lines(ctx: &RowCtx<'_>, wakes: &[PendingWake]) -> Vec<Line<'static>> {
    let theme = ctx.theme;
    let width = content_width(ctx.width);
    let mut lines = Vec::new();
    for wake in wakes {
        let summary = match &wake.trigger {
            PendingWakeTrigger::Command { command } => program_label(command),
            _ => wake.trigger.summary(ctx.now),
        };
        let left = vec![
            Span::raw("    "),
            Span::styled(
                theme.glyph(GlyphRole::CardWaits).to_owned(),
                theme.styled(Component::WakeHeader, Modifier::empty()),
            ),
            Span::raw(" "),
            Span::styled(summary, theme.body()),
        ];
        let elapsed = wake.armed_at.map(|at| age_secs(at, ctx.now));
        lines.push(pin_right(left, elapsed_spans(theme, elapsed), width));

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
