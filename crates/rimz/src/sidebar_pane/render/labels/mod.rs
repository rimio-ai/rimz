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
use super::theme::Theme;

mod glyphs;
mod meters;

pub(super) use self::{glyphs::*, meters::*};

/// The shared idle-age signal: one continuous tone ramp for every age reader —
/// the clock cluster, the breathing `?`/`!`, and the cockpit attention buckets
/// — plus discrete cadence tiers for the attention breath. Color slides from
/// warn through caution to alarm once the age leaves the first quarter hour;
/// cadence stays slow through the half hour, double-time until the hour, and
/// hard-blinks beyond it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HeatCadence {
    Amber,
    Red,
}

pub(super) fn heat_cadence(age_secs: i64) -> Option<HeatCadence> {
    match age_secs {
        i64::MIN..=1800 => None,
        1801..=3600 => Some(HeatCadence::Amber),
        _ => Some(HeatCadence::Red),
    }
}

fn heat_fraction(age_secs: i64) -> Option<f32> {
    (age_secs > 900).then(|| ((age_secs - 900) as f32 / 2700.0).min(1.0))
}

pub(super) fn age_heat_color(theme: &Theme, age_secs: i64) -> Option<Color> {
    heat_fraction(age_secs).map(|amount| theme.heat_tone(amount))
}

/// Tone for the card's elapsed-age cluster at `age_secs` of inactivity: the
/// continuous age heat over the dim resting weight — metadata a step under the
/// card's soft text — so a fresh age stays quiet and a red one reads as the
/// cost warning it is. The figure itself still carries the magnitude under
/// `NO_COLOR`.
pub(super) fn activity_age_style(theme: &Theme, age_secs: i64) -> Style {
    age_heat_color(theme, age_secs)
        .map_or(theme.dim(), |color| theme.style(color, Modifier::empty()))
}

#[cfg(test)]
mod tests;
