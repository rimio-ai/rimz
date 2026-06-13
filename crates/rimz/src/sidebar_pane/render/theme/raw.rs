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
}

impl RawPalette {
    /// Derive the twelve semantic tones ([`Semantic`], the shared palette
    /// the CLI and sidebar both read): ANSI hues map straight through, `caution`
    /// blends yellow toward red, and the neutral ladder steps from background
    /// toward foreground — all in OKLab so each step reads evenly.
    pub(crate) fn derive_tones(&self) -> Semantic {
        Semantic {
            good: self.green,
            warn: self.yellow,
            caution: oklab::blend(self.yellow, self.red, 0.5),
            alarm: self.red,
            accent: self.cyan,
            cool: self.blue,
            meta: self.magenta,
            body: oklab::blend(self.background, self.foreground, 0.65),
            muted: oklab::blend(self.background, self.foreground, 0.45),
            faint: oklab::blend(self.background, self.foreground, 0.25),
            rule: oklab::blend(self.background, self.foreground, 0.18),
            selection: self.bright_blue,
        }
    }
}
