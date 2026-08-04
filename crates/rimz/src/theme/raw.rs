//! Layer 1 — the raw terminal colors imported from a scheme, before any
//! semantic meaning is assigned. This is the sole entry point for
//! scheme-supplied color; every downstream tone derives from these.

use super::oklab::{self, Rgb};
use crate::config::{PaletteRole, ParsedScheme, Semantic};

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
    pub(crate) const DEFAULT: Self = Self {
        background: (0x1a, 0x1b, 0x26),
        foreground: (0xc0, 0xca, 0xf5),
        red: (0xf7, 0x76, 0x8e),
        green: (0x9e, 0xce, 0x6a),
        yellow: (0xe0, 0xaf, 0x68),
        blue: (0x7a, 0xa2, 0xf7),
        magenta: (0xbb, 0x9a, 0xf7),
        cyan: (0x7d, 0xcf, 0xff),
        bright_blue: (0x7a, 0xa2, 0xf7),
        selection_background: Some((0x28, 0x34, 0x57)),
    };

    pub(crate) fn role_rgb(&self, role: PaletteRole) -> Rgb {
        match role {
            PaletteRole::Background => self.background,
            PaletteRole::Foreground => self.foreground,
            PaletteRole::Red => self.red,
            PaletteRole::Green => self.green,
            PaletteRole::Yellow => self.yellow,
            PaletteRole::Blue => self.blue,
            PaletteRole::Magenta => self.magenta,
            PaletteRole::Cyan => self.cyan,
            PaletteRole::BrightBlue => self.bright_blue,
        }
    }

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

impl From<ParsedScheme> for RawPalette {
    fn from(parsed: ParsedScheme) -> Self {
        Self {
            background: parsed.background,
            foreground: parsed.foreground,
            red: parsed.red,
            green: parsed.green,
            yellow: parsed.yellow,
            blue: parsed.blue,
            magenta: parsed.magenta,
            cyan: parsed.cyan,
            bright_blue: parsed.bright_blue,
            selection_background: parsed.selection_background,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_scheme_text;

    #[test]
    fn bundled_scheme_derives_semantic_tones() {
        let tones =
            RawPalette::from(crate::config::explicit_scheme("Afterglow").expect("bundled scheme"))
                .derive_tones();
        assert_eq!(tones.good, (0x7e, 0x8e, 0x50));
        assert_eq!(tones.warn, (0xe5, 0xb5, 0x67));
        assert_eq!(tones.alarm, (0xac, 0x41, 0x42));
        assert_eq!(tones.accent, (0x7d, 0xd6, 0xcf));
        assert_eq!(tones.cool, (0x6c, 0x99, 0xbb));
        assert_eq!(tones.meta, (0x9f, 0x4e, 0x85));
        assert_ne!(tones.selection, tones.cool);
        assert_ne!(tones.caution, tones.warn);
        assert_ne!(tones.caution, tones.alarm);
    }

    fn parse_palette_tones(text: &str) -> Result<Semantic, String> {
        Ok(RawPalette::from(parse_scheme_text(text)?).derive_tones())
    }

    #[test]
    fn selection_background_feeds_the_selected_band() {
        let tones = parse_palette_tones(
            r#"
[colors.selection]
background = '#283457'

[colors.normal]
red = '#f7768e'
green = '#9ece6a'
yellow = '#e0af68'
blue = '#7aa2f7'
magenta = '#bb9af7'
cyan = '#7dcfff'

[colors.primary]
background = '#1a1b26'
foreground = '#c0caf5'
"#,
        )
        .expect("parse scheme");
        assert_ne!(tones.selection_bg, (0x28, 0x34, 0x57));
        assert_ne!(tones.selection_bg, (0x1a, 0x1b, 0x26));
        assert!(tones.selection_bg.2 > 0x26 && tones.selection_bg.2 < 0x57);
    }

    #[test]
    fn light_scheme_ladder_darkens_toward_foreground() {
        let tones = parse_palette_tones(
            r#"
[colors.normal]
red = '#dc322f'
green = '#859900'
yellow = '#b58900'
blue = '#268bd2'
magenta = '#d33682'
cyan = '#2aa198'

[colors.primary]
background = '#fdf6e3'
foreground = '#657b83'
"#,
        )
        .expect("parse scheme");
        assert!(tones.body.0 < tones.muted.0 && tones.muted.0 < tones.faint.0);
    }
}
