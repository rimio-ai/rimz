use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Terminal cell height divided by width, quantized in 1/120 steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CellAspect(u16);

impl CellAspect {
    /// The cell ratio where a 36x27 sextant grid preserves a 192x208 frame,
    /// reproducing the historical full-footprint render exactly.
    pub const NEUTRAL: Self = Self(260);

    pub fn from_ratio(ratio: f32) -> Option<Self> {
        (ratio.is_finite() && (1.0..=4.0).contains(&ratio))
            .then(|| Self((ratio * 120.0).round() as u16))
    }

    pub fn ratio(self) -> f32 {
        f32::from(self.0) / 120.0
    }
}

impl Serialize for CellAspect {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f32(self.ratio())
    }
}

impl<'de> Deserialize<'de> for CellAspect {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let ratio = f32::deserialize(deserializer)?;
        Self::from_ratio(ratio).ok_or_else(|| {
            D::Error::custom(format!(
                "cell aspect ratio must be between 1.0 and 4.0, got {ratio}"
            ))
        })
    }
}

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
    /// Manual cell height/width ratio for terminals whose pty reports no pixel
    /// size. Overrides the runtime probe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cell_aspect: Option<CellAspect>,
    /// Show canned captions on fleet-status changes.
    pub voice: bool,
}

impl Default for PetsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            pet: "rocky".to_owned(),
            glyphs: PetsGlyphMode::default(),
            cell_aspect: None,
            voice: true,
        }
    }
}

impl PetsConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_aspect_quantizes_and_checks_bounds() {
        let aspect = CellAspect::from_ratio(2.404).expect("valid ratio");
        assert!((aspect.ratio() - 2.4).abs() < f32::EPSILON);
        assert_eq!(
            CellAspect::from_ratio(1.0).map(CellAspect::ratio),
            Some(1.0)
        );
        assert_eq!(
            CellAspect::from_ratio(4.0).map(CellAspect::ratio),
            Some(4.0)
        );
        assert_eq!(CellAspect::from_ratio(0.5), None);
        assert_eq!(CellAspect::from_ratio(9.0), None);
        assert_eq!(CellAspect::from_ratio(f32::NAN), None);
    }

    #[test]
    fn cell_aspect_serializes_as_a_toml_float() {
        let parsed: PetsConfig = toml::from_str("cell_aspect = 2.4").expect("parse aspect");
        assert_eq!(parsed.cell_aspect, CellAspect::from_ratio(2.4));

        let encoded = toml::to_string(&parsed).expect("serialize aspect");
        assert!(encoded.contains("cell_aspect = 2.4"));
        let round_tripped: PetsConfig = toml::from_str(&encoded).expect("round trip aspect");
        assert_eq!(round_tripped.cell_aspect, parsed.cell_aspect);
    }

    #[test]
    fn cell_aspect_rejects_out_of_range_toml_values() {
        for ratio in [0.5, 9.0] {
            let err = toml::from_str::<PetsConfig>(&format!("cell_aspect = {ratio}"))
                .expect_err("reject out-of-range aspect");
            assert!(err.to_string().contains("between 1.0 and 4.0"));
        }
    }
}
