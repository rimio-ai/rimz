use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{DisplayConfig, ThemeAnimationsConfig, ThemeColor, ThemeGlyphsConfig, ThemeMode};

/// `[theme] style`: a headline preset bundling color depth and glyph set so one
/// switch picks the whole look. `modern` forces truecolor and the Nerd Font
/// glyphs; `default` keeps auto color depth and the Unicode glyphs.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeStyle {
    /// Auto color depth with the default Unicode glyph vocabulary.
    Default,
    /// Truecolor depth with the Nerd Font glyph preset.
    Modern,
}

/// `[theme]`: per-machine appearance. It owns palette depth, scheme selection,
/// semantic slot overrides, sidebar render preferences, glyphs, provider
/// styling, and status-head animations. Display-only — it tunes what the
/// renderer paints, never ledger correctness.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ThemeConfig {
    /// Headline display preset bundling color depth and glyph set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<ThemeStyle>,
    /// Palette depth: `auto` follows the terminal's truecolor advertisement,
    /// `truecolor` forces RGB, and `256` quantizes RGB tones to xterm indexes.
    #[serde(skip_serializing_if = "is_default_theme_mode")]
    pub mode: ThemeMode,
    /// Palette scheme: unset uses the bundled `TokyoNight Night` theme. Bundled
    /// Alacritty theme names (`rimz list-themes`) and paths to Alacritty TOML
    /// theme files are accepted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    /// Inline Alacritty palette lifted from root `[colors.*]` in theme.toml.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colors: Option<InlinePalette>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub good: Option<ThemeColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warn: Option<ThemeColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caution: Option<ThemeColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alarm: Option<ThemeColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<ThemeColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cool: Option<ThemeColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<ThemeColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<ThemeColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub muted: Option<ThemeColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faint: Option<ThemeColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<ThemeColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<ThemeColor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection_bg: Option<ThemeColor>,
    /// Sidebar render preferences: cadence, sizing, dashboard layout, and
    /// display-only meter bands.
    #[serde(default, skip_serializing_if = "DisplayConfig::is_unset")]
    pub display: DisplayConfig,
    #[serde(skip_serializing_if = "ThemeGlyphsConfig::is_unset")]
    pub glyphs: ThemeGlyphsConfig,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, ThemeProviderStyle>,
    #[serde(skip_serializing_if = "ThemeAnimationsConfig::is_unset")]
    pub animations: ThemeAnimationsConfig,
}

impl ThemeConfig {
    /// Whether every theme knob is unset — the serialized config omits the
    /// section when the machine uses shipped defaults.
    pub fn is_unset(&self) -> bool {
        *self == Self::default()
    }

    /// The palette depth mode after folding in the [`style`](Self::style)
    /// preset: an explicit `mode` wins; otherwise `modern` forces truecolor and
    /// every other case keeps auto detection.
    pub fn effective_theme_mode(&self) -> ThemeMode {
        match self.mode {
            ThemeMode::Auto => match self.style {
                Some(ThemeStyle::Modern) => ThemeMode::Truecolor,
                _ => ThemeMode::Auto,
            },
            explicit => explicit,
        }
    }

    /// The glyph-set source after folding in the [`style`](Self::style) preset.
    /// An explicit `theme.glyphs.set` wins; otherwise `modern` selects
    /// `nerd_font` and every other case keeps the Unicode default.
    pub fn glyph_set_source(&self) -> Option<String> {
        self.glyphs.set.clone().or_else(|| match self.style {
            Some(ThemeStyle::Modern) => Some("nerd_font".to_owned()),
            _ => None,
        })
    }
}

/// Root `[colors]` in theme.toml. The shape follows Alacritty's palette tables
/// so a theme can be pasted directly into Rimz; missing or extra Alacritty keys
/// are tolerated at load, and the renderer validates the keys it needs when it
/// derives tones.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct InlinePalette {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary: Option<InlinePrimaryColors>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub normal: Option<InlineAnsiColors>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bright: Option<InlineAnsiColors>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<InlineSelectionColors>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct InlinePrimaryColors {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct InlineSelectionColors {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct InlineAnsiColors {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub black: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub red: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub green: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yellow: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub magenta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cyan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub white: Option<String>,
}

/// Per-provider styling: the ASCII emblem and brand color for the bottom
/// dashboard. Every field is optional; an omitted field uses the built-in
/// default for the provider kind, so a user overrides just the art or just the
/// color without restating both.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ThemeProviderStyle {
    /// Display name for the panel header (`Claude`, `Codex`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_name: Option<String>,
    /// Multi-line ASCII emblem painted at the left of the provider block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ascii_art: Option<String>,
    /// Brand color for the emblem. Accepts a palette role, 256-color index, or
    /// `#rrggbb`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<ThemeColor>,
}

fn is_default_theme_mode(mode: &ThemeMode) -> bool {
    *mode == ThemeMode::default()
}
