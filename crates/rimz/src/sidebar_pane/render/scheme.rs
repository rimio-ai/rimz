//! Renderer derivation for config-owned selectable palettes.

use crate::config::{InlinePalette, ParsedScheme, explicit_scheme, parsed_inline_palette};
#[cfg(test)]
use crate::config::{Semantic, parse_scheme_text};

use super::theme::RawPalette;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchemeSwatch {
    pub background: (u8, u8, u8),
    pub foreground: (u8, u8, u8),
    pub red: (u8, u8, u8),
    pub green: (u8, u8, u8),
    pub yellow: (u8, u8, u8),
    pub blue: (u8, u8, u8),
    pub magenta: (u8, u8, u8),
    pub cyan: (u8, u8, u8),
}

pub(crate) fn explicit_raw_palette(name_or_path: &str) -> Option<RawPalette> {
    explicit_scheme(name_or_path).map(raw_palette)
}

pub fn scheme_swatch(name_or_path: &str) -> Option<SchemeSwatch> {
    explicit_scheme(name_or_path).map(|parsed| SchemeSwatch {
        background: parsed.background,
        foreground: parsed.foreground,
        red: parsed.red,
        green: parsed.green,
        yellow: parsed.yellow,
        blue: parsed.blue,
        magenta: parsed.magenta,
        cyan: parsed.cyan,
    })
}

#[cfg(test)]
pub(crate) fn explicit_palette_tones(name_or_path: &str) -> Option<Semantic> {
    explicit_raw_palette(name_or_path).map(|raw| raw.derive_tones())
}

pub(crate) fn default_raw_palette() -> RawPalette {
    explicit_raw_palette(crate::config::DEFAULT_SCHEME).unwrap_or(RawPalette::DEFAULT)
}

pub(crate) fn inline_raw_palette(colors: &InlinePalette) -> Result<RawPalette, String> {
    parsed_inline_palette(colors).map(raw_palette)
}

fn raw_palette(parsed: ParsedScheme) -> RawPalette {
    RawPalette {
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

#[cfg(test)]
fn parse_palette_tones(text: &str) -> Result<Semantic, String> {
    Ok(raw_palette(parse_scheme_text(text)?).derive_tones())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_scheme_derives_semantic_tones() {
        let tones = explicit_palette_tones("Afterglow").expect("bundled scheme");
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

    #[test]
    fn scheme_swatch_exposes_raw_palette() {
        let swatch = scheme_swatch("TokyoNight Night").expect("bundled scheme");
        assert_eq!(swatch.background, (0x1a, 0x1b, 0x26));
        assert_eq!(swatch.green, (0x9e, 0xce, 0x6a));
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
