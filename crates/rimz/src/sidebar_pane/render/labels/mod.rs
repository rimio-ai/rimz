//! Semantic sidebar vocabulary: the canonical status glyphs and the
//! gauge / spinner / pulse glyph helpers.
//!
//! Every meter in the sidebar — context-window %, diff stats —
//! renders through the same vocabulary so they read as siblings, not as
//! one-off widgets (see [the sidebar grammar](../../../docs/internals/sidebar/sidebar.md)).

use crate::agents::AgentStatus;
use crate::agents::ContextSeverity;
use crate::agents::TurnPhase;
use crate::config::{AnimationRole, BudgetBarConfig, BudgetBurnRateConfig, UnreadEffect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use super::animation::{
    BREATH_DEEP_AMPLITUDE, BreathSample, UnreadAnim, effect_style, effect_weight, frame_at,
    shimmer_lift,
};
use super::theme::Theme;

mod glyphs;
mod meters;

pub(super) use self::{glyphs::*, meters::*};

pub(super) fn value_seam(theme: &Theme) -> String {
    format!(" {} ", theme.glyph(crate::config::GlyphRole::Seam))
}

/// The child quiet-clock tone ramp: color slides from warn
/// through caution to alarm once the age leaves the first quarter of
/// `ceiling_secs`, the configured stall window. Breath tempo follows its own clamped curve in
/// [`super::animation::breath_tempo`].
fn heat_fraction(age_secs: i64, ceiling_secs: i64) -> Option<f32> {
    let first_quarter = ceiling_secs / 4;
    let heat_span = ceiling_secs - first_quarter;
    (age_secs > first_quarter)
        .then(|| ((age_secs - first_quarter) as f32 / heat_span as f32).min(1.0))
}

fn age_heat_color(theme: &Theme, age_secs: i64, ceiling_secs: i64) -> Option<Color> {
    heat_fraction(age_secs, ceiling_secs).map(|amount| theme.warm_heat_tone(amount))
}

/// Tone for the child's elapsed-age cluster at `age_secs` of inactivity: the
/// continuous age heat over the dim resting weight — metadata a step under the
/// card's soft text — so a fresh age stays quiet and a red one signals a
/// quiet child worth checking. The figure itself still carries the magnitude under
/// `NO_COLOR`.
pub(super) fn activity_age_style(theme: &Theme, age_secs: i64, ceiling_secs: i64) -> Style {
    age_heat_color(theme, age_secs, ceiling_secs)
        .map_or(theme.muted(), |color| theme.style(color, Modifier::empty()))
}

#[cfg(test)]
mod tests;
