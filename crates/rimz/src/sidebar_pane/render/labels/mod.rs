//! Semantic sidebar vocabulary: the canonical status glyphs and the
//! gauge / spinner / pulse glyph helpers.
//!
//! Every meter in the sidebar — context-window %, todo progress, diff stats —
//! renders through the same vocabulary so they read as siblings, not as
//! one-off widgets (see [the sidebar grammar](../../../docs/internals/sidebar/sidebar.md)).

use crate::agents::TurnPhase;
use crate::config::{BudgetBarConfig, BudgetBurnRateConfig};
use crate::feed::ContextSeverity;
use crate::feed::{ATTENTION_AGE_CEILING_SECS, AgentStatus};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::animation::{
    AnimationRole, BREATH_DEEP_AMPLITUDE, BreathSample, UnreadAnim, UnreadEffect, effect_style,
    effect_weight, frame_at, shimmer_lift,
};
use super::theme::Theme;

mod glyphs;
mod meters;

pub(super) use self::{glyphs::*, meters::*};

/// The shared idle-age signal: one continuous tone ramp for every age reader —
/// the clock cluster, the breathing `?`/`!`, and the cockpit attention buckets.
/// Color slides from warn through caution to alarm once the age leaves the
/// first quarter hour; breath tempo follows a continuous clamped curve in
/// [`super::animation::breath_tempo`].
fn heat_fraction(age_secs: i64) -> Option<f32> {
    let first_quarter = ATTENTION_AGE_CEILING_SECS / 4;
    let heat_span = ATTENTION_AGE_CEILING_SECS - first_quarter;
    (age_secs > first_quarter)
        .then(|| ((age_secs - first_quarter) as f32 / heat_span as f32).min(1.0))
}

pub(super) fn age_heat_color(theme: &Theme, age_secs: i64) -> Option<Color> {
    heat_fraction(age_secs).map(|amount| theme.warm_heat_tone(amount))
}

/// Tone for the card's elapsed-age cluster at `age_secs` of inactivity: the
/// continuous age heat over the dim resting weight — metadata a step under the
/// card's soft text — so a fresh age stays quiet and a red one reads as the
/// cost warning it is. The figure itself still carries the magnitude under
/// `NO_COLOR`.
pub(super) fn activity_age_style(theme: &Theme, age_secs: i64) -> Style {
    age_heat_color(theme, age_secs)
        .map_or(theme.muted(), |color| theme.style(color, Modifier::empty()))
}

#[cfg(test)]
mod tests;
