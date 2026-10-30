//! Provider display identity resolved once for every human renderer.

use std::collections::BTreeMap;

use crate::agents::{EmblemTint, emblem_for, spec_by_kind};
use crate::config::{ColorDepth, PaletteRole, ThemeColor, ThemeProviderStyle, nearest_xterm_index};

use super::{Palette, Tone};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProviderIdentity {
    pub product_name: String,
    pub art: Vec<String>,
    pub art_tints: Vec<EmblemTint>,
    pub brand: BrandColor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrandColor {
    Role(PaletteRole),
    Indexed(u8),
    Rgb(u8, u8, u8),
    /// A registered definition's hand-tuned xterm index and truecolor tone.
    Brand {
        index: u8,
        rgb: (u8, u8, u8),
    },
}

fn builtin_provider_brand(kind: &str) -> BrandColor {
    spec_by_kind(kind).map_or(BrandColor::Indexed(244), |definition| BrandColor::Brand {
        index: definition.brand.color,
        rgb: definition.brand.color_rgb,
    })
}

fn configured_brand(color: ThemeColor) -> BrandColor {
    match color {
        ThemeColor::Role(role) => BrandColor::Role(role),
        ThemeColor::Indexed(index) => BrandColor::Indexed(index),
        ThemeColor::Rgb(red, green, blue) => BrandColor::Rgb(red, green, blue),
    }
}

/// Resolve only provider color, without allocating its name or emblem.
pub fn resolve_provider_brand(
    kind: &str,
    styles: &BTreeMap<String, ThemeProviderStyle>,
) -> BrandColor {
    styles
        .get(kind)
        .and_then(|style| style.color)
        .map_or_else(|| builtin_provider_brand(kind), configured_brand)
}

impl BrandColor {
    pub fn tone(self, palette: &Palette) -> Tone {
        match self {
            Self::Role(role) => palette.role_tone(role),
            Self::Indexed(index) => Tone::Indexed(index),
            Self::Rgb(red, green, blue) => palette.rgb_tone((red, green, blue)),
            Self::Brand { index, rgb } => match palette.depth {
                ColorDepth::Indexed => Tone::Indexed(index),
                ColorDepth::Truecolor => Tone::Rgb(rgb.0, rgb.1, rgb.2),
            },
        }
    }

    pub(crate) fn indexed(self) -> u8 {
        match self {
            Self::Role(_) => 7,
            Self::Indexed(index) => index,
            Self::Rgb(red, green, blue) => nearest_xterm_index(red, green, blue),
            Self::Brand { index, .. } => index,
        }
    }
}

pub fn resolve_provider_identity(
    kind: &str,
    styles: &BTreeMap<String, ThemeProviderStyle>,
) -> ResolvedProviderIdentity {
    let emblem = emblem_for(kind);
    let brand = resolve_provider_brand(kind, styles);
    let mut resolved = spec_by_kind(kind).map_or_else(
        || ResolvedProviderIdentity {
            product_name: provider_title_case(kind),
            art: emblem.lines.clone(),
            art_tints: emblem.tints.clone(),
            brand,
        },
        |definition| ResolvedProviderIdentity {
            product_name: definition.display_name.to_owned(),
            art: emblem.lines.clone(),
            art_tints: emblem.tints.clone(),
            brand,
        },
    );
    let Some(style) = styles.get(kind) else {
        return resolved;
    };
    if let Some(name) = style
        .product_name
        .as_deref()
        .filter(|name| !name.is_empty())
    {
        resolved.product_name = name.to_owned();
    }
    if let Some(art) = style.ascii_art.as_deref().filter(|art| !art.is_empty()) {
        resolved.art = art.lines().map(ToOwned::to_owned).collect();
        resolved.art_tints.clear();
    }
    resolved
}

/// Title-case a `-`/`_`/space-delimited token.
pub(crate) fn provider_title_case(value: &str) -> String {
    value
        .split(['-', '_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ColorDepth, ThemeConfig};

    #[test]
    fn provider_override_wins_over_descriptor() {
        let mut styles = BTreeMap::new();
        styles.insert(
            "claude".to_owned(),
            ThemeProviderStyle {
                product_name: Some("Anthropic".to_owned()),
                ascii_art: Some("A\nA".to_owned()),
                color: Some(ThemeColor::Indexed(42)),
            },
        );
        let identity = resolve_provider_identity("claude", &styles);
        assert_eq!(identity.product_name, "Anthropic");
        assert_eq!(identity.art, ["A", "A"]);
        assert!(identity.art_tints.is_empty());
        assert_eq!(identity.brand, BrandColor::Indexed(42));
    }

    #[test]
    fn descriptor_and_unknown_fallbacks_are_stable() {
        let claude = resolve_provider_identity("claude", &BTreeMap::new());
        assert_eq!(claude.product_name, "Claude");
        assert!(matches!(claude.brand, BrandColor::Brand { .. }));
        let unknown = resolve_provider_identity("new_agent", &BTreeMap::new());
        assert_eq!(unknown.product_name, "New Agent");
        assert_eq!(unknown.brand, BrandColor::Indexed(244));
    }

    #[test]
    fn brand_only_resolution_matches_full_identity() {
        for (kind, color) in [
            ("claude", None),
            ("new_agent", None),
            ("claude", Some(ThemeColor::Role(PaletteRole::Green))),
            ("claude", Some(ThemeColor::Indexed(42))),
            ("claude", Some(ThemeColor::Rgb(1, 2, 3))),
        ] {
            let styles = color.map_or_else(BTreeMap::new, |color| {
                BTreeMap::from([(
                    kind.to_owned(),
                    ThemeProviderStyle {
                        color: Some(color),
                        ..ThemeProviderStyle::default()
                    },
                )])
            });
            assert_eq!(
                resolve_provider_brand(kind, &styles),
                resolve_provider_identity(kind, &styles).brand,
                "brand-only and full identity differ for {kind} with {color:?}"
            );
        }
    }

    #[test]
    fn brand_tone_respects_palette_depth_and_roles() {
        let indexed = Palette::resolve(&ThemeConfig::default(), ColorDepth::Indexed);
        let rgb = Palette::resolve(&ThemeConfig::default(), ColorDepth::Truecolor);
        assert!(matches!(
            BrandColor::Rgb(1, 2, 3).tone(&indexed),
            Tone::Indexed(_)
        ));
        assert_eq!(BrandColor::Rgb(1, 2, 3).tone(&rgb), Tone::Rgb(1, 2, 3));
        assert_eq!(
            BrandColor::Role(PaletteRole::Green).tone(&rgb),
            Tone::Rgb(0x9e, 0xce, 0x6a)
        );

        let grok = resolve_provider_identity("grok", &BTreeMap::new()).brand;
        assert_eq!(grok.indexed(), 15);
        assert_ne!(nearest_xterm_index(0xff, 0xff, 0xff), 15);
        assert_eq!(grok.tone(&indexed), Tone::Indexed(15));
        assert_eq!(grok.tone(&rgb), Tone::Rgb(0xff, 0xff, 0xff));
    }
}
