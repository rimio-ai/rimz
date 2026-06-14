//! Layer 1 — the raw terminal colors imported from a scheme, before any
//! semantic meaning is assigned. This is the sole entry point for
//! scheme-supplied color; every downstream tone derives from these.

use super::super::oklab::{self, Rgb};
use crate::config::Semantic;

/// The imported terminal colors, verbatim: background, foreground, the six
/// ANSI normal hues the renderer maps to meaning, and the selection accent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawPalette {
    pub(crate) background: Rgb,
    pub(crate) foreground: Rgb,
    pub(crate) red: Rgb,
    pub(crate) green: Rgb,
    pub(crate) yellow: Rgb,
    pub(crate) blue: Rgb,
    pub(crate) magenta: Rgb,
    pub(crate) cyan: Rgb,
    /// The selection accent: `colors.bright.blue`, falling back to `normal.blue`.
    pub(crate) bright_blue: Rgb,
    /// The selected card's background band: `colors.selection.background` when the
    /// scheme supplies one, else `None` to derive a tint from `background`/`blue`.
    pub(crate) selection_background: Option<Rgb>,
}

impl RawPalette {
    /// Derive the thirteen semantic tones ([`Semantic`], the shared palette
    /// the CLI and sidebar both read): the chromatic ANSI hues map through,
    /// `caution` blends yellow toward red to land a true amber (not a coral
    /// half-step), the neutral ladder steps from background toward foreground,
    /// and selection is its own bright cool tone with a dark band behind it — all
    /// in OKLab so each step reads evenly.
    pub(crate) fn derive_tones(&self) -> Semantic {
        // Selection owns one bright cool tone, lifted off the data blues so the
        // selected card never borrows a token color, over a dark band — the
        // scheme's own selection background when it ships one, else a deep tint of
        // its blue.
        let selection =
            oklab::lift_lightness(oklab::blend(self.bright_blue, self.foreground, 0.42), 0.05);
        // A subtle full-card band: pull the scheme's text-selection background (or
        // a blue tint when it ships none) most of the way back toward the
        // background, since a whole-card fill wants far less contrast than a
        // few-character text-selection highlight would.
        let selection_bg = match self.selection_background {
            Some(scheme_band) => oklab::blend(self.background, scheme_band, 0.22),
            None => oklab::blend(self.background, self.blue, 0.12),
        };
        Semantic {
            good: self.green,
            warn: self.yellow,
            // Warm the gold warn-yellow toward the alarm red and enrich it rather
            // than blending into it, so the caution rung lands a vivid amber-orange
            // (not a desaturated coral) on every scheme — the warm "hot/costly"
            // tier the gauge mid-band and age heat share. Fresh input warms one
            // step further past the alarm (`Palette` expense tone).
            caution: oklab::warm_toward(self.yellow, self.red, 0.22, 1.35),
            alarm: self.red,
            accent: self.cyan,
            cool: self.blue,
            meta: self.magenta,
            body: oklab::blend(self.background, self.foreground, 0.82),
            muted: oklab::blend(self.background, self.foreground, 0.6),
            faint: oklab::blend(self.background, self.foreground, 0.38),
            rule: oklab::blend(self.background, self.foreground, 0.28),
            selection,
            selection_bg,
        }
    }
}
