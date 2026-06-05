//! The cockpit's two summary lines: today's sessions + token breakdown, and the
//! live-agent count with the animated count-up spend.

use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};
use rimz::SpendWindow;

use crate::render::TallyAnim;
use crate::render::fmt::{dollars2, tokens_int};
use crate::render::labels::token_breakdown_spans;
use crate::render::theme::{ORANGE, Theme};

use super::{SESSIONS_GLYPH, metric_spans, pin_right};

/// The cockpit's live-agent count glyph: `¤` for the agents in the room right
/// now (the sessions-today `◎` lives in the shared [`SESSIONS_GLYPH`]).
const ACTIVE_AGENTS_GLYPH: &str = "¤";

/// A brighter sage than the resting money-green, held for a couple of frames as a
/// figure lands — the quiet "ka-chunk" of the climb. Drops to plain bold under
/// `NO_COLOR` like every other tone.
const VALUE_FLASH: Color = Color::Indexed(150);

/// The cockpit's first summary line, directly beneath the repo identity:
/// `◎ {sessions}` — the threads that have run today, the glyph in the teal it
/// shares with the W/M ledger rows — on the left, with today's accumulated
/// token breakdown `◇ ↘ ↗ ◍ ◌` (integer magnitudes, the live coarse form)
/// pinned to the right edge: both halves read today's window, so the line is
/// the day at a glance. The breakdown reads the JSONL `value_tally`'s today
/// window and drops when today recorded no tokens, leaving `◎ {sessions}`
/// alone. The live-agent count and the spend ride the second line
/// ([`cockpit_spend_line`]).
pub(in crate::render) fn cockpit_summary_line(
    theme: &Theme,
    sessions: u32,
    today: Option<&SpendWindow>,
    width: usize,
) -> Line<'static> {
    let left = metric_spans(
        theme,
        SESSIONS_GLYPH,
        Color::Cyan,
        &sessions.to_string(),
        theme.value(),
    );
    let right = today
        .filter(|w| w.tokens > 0 || w.cache_write > 0 || w.cache_read > 0)
        .map(|window| {
            token_breakdown_spans(
                theme,
                window.tokens,
                window.input,
                window.output,
                window.cache_write,
                window.cache_read,
                tokens_int,
                true,
            )
        })
        .unwrap_or_default();
    pin_right(left, right, width)
}

/// The cockpit's second summary line: `¤ {live}` — the agents in the room right
/// now, the glyph in the agents' own working clay — on the left, with today's
/// fleet spend pinned to the right edge, climbing in a smooth count-up as a
/// turn lands. The figure eases toward the `value_tally` today total via the
/// shared [`TallyAnim`] roll and brightens for a beat the instant it settles —
/// the cockpit's one animated number (the W/M ledger rows below stay static).
/// Always present — an empty room reads `¤ 0`; the bold money-green `$` joins
/// the right edge once today records spend.
pub(in crate::render) fn cockpit_spend_line(
    theme: &Theme,
    live_agents: usize,
    today_usd: f64,
    anim: &TallyAnim,
    phase: u64,
    width: usize,
) -> Line<'static> {
    let left = metric_spans(
        theme,
        ACTIVE_AGENTS_GLYPH,
        ORANGE,
        &live_agents.to_string(),
        theme.value(),
    );
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
