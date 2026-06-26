use serde::{Deserialize, Serialize};

/// `[theme.pets] glyphs`: which pet render tier the dashboard uses.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum PetsGlyphMode {
    /// Use the default renderer ladder: pixels, then sextants.
    #[default]
    Auto,
    /// Use kitty graphics pixels when supported; fall back to cell art.
    Pixel,
    /// Use Unicode sextants.
    Sextant,
    /// Use Unicode 16 octants.
    Octant,
}

/// `[theme.pets]`: opt-in animated companion in the provider dashboard.
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
    /// Render tier: `auto` tries pixels, then sextants.
    pub glyphs: PetsGlyphMode,
    /// Show canned captions on fleet-status changes.
    pub voice: bool,
}

impl Default for PetsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            pet: "rocky".to_owned(),
            glyphs: PetsGlyphMode::default(),
            voice: true,
        }
    }
}

impl PetsConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
