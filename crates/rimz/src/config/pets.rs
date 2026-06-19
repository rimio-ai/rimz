use serde::{Deserialize, Serialize};

/// `[agents.pets] glyphs`: which Unicode block tier the pet renderer uses.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum PetsGlyphMode {
    /// Use the default renderer tier: sextants with the half-block floor.
    #[default]
    Auto,
    /// Use half-block cells (`▀`), the broadest terminal-font floor.
    Half,
    /// Use Unicode sextants, the default quality/coverage tier.
    Sextant,
    /// Use Unicode 16 octants. Sharpest tier, intended for explicit opt-in.
    Octant,
}

/// `[agents.pets] size`: how much space the provider-dashboard pet occupies.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum PetsSize {
    /// Match the original dashboard pet footprint.
    #[default]
    Medium,
    /// Fit the pet to the active provider block height.
    Small,
}

/// `[agents.pets]`: opt-in animated companion in the provider dashboard.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct PetsConfig {
    /// Enable the pet dashboard tab and the best-effort CDN/cache asset load.
    pub enabled: bool,
    /// Which pet to show. A built-in catalog id (`codex`, `dewey`, …) wins; an
    /// `http(s)://` selector is your own WebP sheet fetched and cached like a
    /// built-in; a path-like selector (`/`, `.`, or leading `~`) is a local
    /// sheet or a petdex pet directory; and a bare slug (`wall-e`) is a petdex
    /// pet installed under `~/.codex/pets/<slug>/`.
    pub pet: String,
    /// Dashboard pet footprint.
    pub size: PetsSize,
    /// Unicode block-glyph tier.
    pub glyphs: PetsGlyphMode,
    /// Show canned captions on fleet-status changes.
    pub voice: bool,
}

impl Default for PetsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            pet: "codex".to_owned(),
            size: PetsSize::default(),
            glyphs: PetsGlyphMode::default(),
            voice: true,
        }
    }
}
