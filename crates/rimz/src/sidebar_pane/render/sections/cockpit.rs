//! The cockpit's two summary lines: today's sessions + token breakdown, and the
//! live-agent count with the animated count-up spend.

use crate::SpendWindow;
use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};

use crate::sidebar_pane::render::TallyAnim;
use crate::sidebar_pane::render::animation::{
    BREATH_DEEP_AMPLITUDE, BreathSample, DEFAULT_BREATH_PERIOD,
};
use crate::sidebar_pane::render::fmt::{dollars2, tokens_int};
use crate::sidebar_pane::render::labels::{attention_floor_color, token_breakdown_spans};
use crate::sidebar_pane::render::theme::Theme;

use super::{SESSIONS_GLYPH, VALUE_FLASH, metric_spans, pin_right};

/// The cockpit's live-agent count glyph: `¤` for the agents in the room right
/// now (the sessions-today `◎` lives in the shared [`SESSIONS_GLYPH`]).
const ACTIVE_AGENTS_GLYPH: &str = "¤";

/// The cockpit's first summary line, directly beneath the repo identity:
/// `◎ {sessions}` — the threads that have run today, the glyph in the teal it
/// shares with the W/M ledger rows — on the left, with today's accumulated
/// token breakdown `◇ ↘ ↗ ◌` (integer magnitudes, the live coarse form)
/// pinned to the right edge: both halves read today's window, so the line is
/// the day at a glance. The breakdown reads the workspace-scoped JSONL tally's
/// today window and drops when today recorded no tokens, leaving `◎ {sessions}`
/// alone. The live-agent count and the spend ride the second line
/// ([`cockpit_spend_line`]).
pub(in crate::sidebar_pane::render) fn cockpit_summary_line(
    theme: &Theme,
    sessions: u32,
    today: Option<&SpendWindow>,
    width: usize,
) -> Line<'static> {
    let left = metric_spans(theme, SESSIONS_GLYPH, Color::Cyan, &sessions.to_string());
    let right = today
        .filter(|w| w.tokens > 0 || w.cache_read > 0)
        .map(|window| {
            token_breakdown_spans(
                theme,
                window.tokens,
                window.input,
                window.output,
                window.cache_read,
                tokens_int,
            )
        })
        .unwrap_or_default();
    pin_right(left, right, width)
}

/// The cockpit's second summary line: `¤ {live}` — the agents in the room right
/// now, the glyph in the agents' own working clay — on the left, with today's
/// fleet spend pinned to the right edge, counting up as a turn lands. The
/// figure ticks toward the workspace tally's today total via the shared
/// [`TallyAnim`] roll — big decaying steps, then penny by penny onto the exact
/// figure — and brightens for a beat the instant it settles (the W/M ledger
/// rows below stay static). Always present — an empty room reads `¤ 0`; the
/// bold money-green `$` joins the right edge once today records spend.
pub(in crate::sidebar_pane::render) fn cockpit_spend_line(
    theme: &Theme,
    live_agents: usize,
    unread_agents: usize,
    today_usd: f64,
    anim: &TallyAnim,
    phase: u64,
    width: usize,
) -> Line<'static> {
    let mut left = metric_spans(
        theme,
        ACTIVE_AGENTS_GLYPH,
        theme.clay(),
        &live_agents.to_string(),
    );
    if unread_agents > 0 {
        left.push(Span::styled(
            format!(" ({unread_agents})"),
            theme.breathe(
                attention_floor_color(theme, crate::feed::AgentStatus::Waiting),
                BreathSample::new(phase, DEFAULT_BREATH_PERIOD, BREATH_DEEP_AMPLITUDE),
            ),
        ));
    }
    let right = if today_usd > 0.0 {
        let usd = anim.today_usd.display(today_usd, phase);
        let style = if anim.today_usd.flashing(phase) {
            theme.style(VALUE_FLASH, Modifier::BOLD)
        } else {
            theme.style(Color::Green, Modifier::BOLD)
        };
        vec![Span::styled(dollars2(usd), style)]
    } else {
        Vec::new()
    };
    pin_right(left, right, width)
}
