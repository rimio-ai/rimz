//! Semantic sidebar vocabulary: the canonical status glyphs and the
//! gauge / spinner / pulse glyph helpers.
//!
//! Every meter in the sidebar — context-window %, todo progress, diff stats —
//! renders through the same vocabulary so they read as siblings, not as
//! one-off widgets (see [the sidebar grammar](../../../docs/internals/sidebar/sidebar.md)).

use crate::agents::TurnPhase;
use crate::config::{BudgetPaceConfig, BudgetZonesConfig};
use crate::feed::AgentStatus;
use crate::feed::ContextSeverity;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::animation::{AnimationRole, effect_modifier, frame_at, still_frame};
use super::theme::{ORANGE, Theme};

mod glyphs;
mod meters;

pub(super) use self::{glyphs::*, meters::*};

/// The shared age heat: one tone ramp for every idle-age reader — the clock
/// cluster, the breathing `?`/`!`, and the cockpit attention buckets — stepping
/// with the quarter-hour buckets that fill the clock face ([`elapsed_glyph`]).
/// `None` through the first quarter (callers pick the resting tone), yellow to
/// the half hour, amber beyond it, red past the hour — when resuming would
/// almost certainly re-read the whole context at uncached input rates.
pub(super) fn age_heat(age_secs: i64) -> Option<Color> {
    match age_secs {
        i64::MIN..=900 => None,
        901..=1800 => Some(Color::Yellow),
        1801..=3600 => Some(ORANGE),
        _ => Some(Color::Red),
    }
}

/// Tone for the card's elapsed-age cluster at `age_secs` of inactivity: the
/// shared [`age_heat`] over the dim resting weight — metadata a step under
/// the card's soft text — so a fresh age stays quiet and a red one reads as
/// the cost warning it is. The figure itself still carries the magnitude
/// under `NO_COLOR`.
pub(super) fn activity_age_style(theme: &Theme, age_secs: i64) -> Style {
    age_heat(age_secs).map_or(theme.dim(), |color| theme.style(color, Modifier::empty()))
}

#[cfg(test)]
mod tests;
