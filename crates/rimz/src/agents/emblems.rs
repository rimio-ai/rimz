//! Embedded provider-emblem catalog and shared fallback resolution.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

const CATALOG_SOURCE: &str = include_str!("emblems.toml");

/// The SVG mark shipped for each built-in kind, from `crates/rimz/logos/`.
/// A mark with no colour of its own fills with `currentColor`.
const SHIPPED_LOGOS: &[(&str, &str)] = &[
    ("claude", include_str!("../../logos/claude.svg")),
    ("codex", include_str!("../../logos/codex.svg")),
    ("amp", include_str!("../../logos/amp.svg")),
    ("copilot", include_str!("../../logos/copilot.svg")),
    ("kimi", include_str!("../../logos/kimi.svg")),
    ("pi", include_str!("../../logos/pi.svg")),
    ("opencode", include_str!("../../logos/opencode.svg")),
    ("antigravity", include_str!("../../logos/antigravity.svg")),
    ("cursor", include_str!("../../logos/cursor.svg")),
    ("droid", include_str!("../../logos/droid.svg")),
    ("kiro", include_str!("../../logos/kiro.svg")),
    ("qwen", include_str!("../../logos/qwen.svg")),
    ("grok", include_str!("../../logos/grok.svg")),
    ("hermes", include_str!("../../logos/hermes.svg")),
    ("qoder", include_str!("../../logos/qoder.svg")),
    ("omp", include_str!("../../logos/omp.svg")),
    ("muse", include_str!("../../logos/muse.svg")),
    ("minimax", include_str!("../../logos/minimax.svg")),
];

/// One tinted run inside an emblem: chars `[start, start + len)` of art row
/// `row` paint in this tone instead of the provider's brand tone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmblemTint {
    pub row: usize,
    pub start: usize,
    pub len: usize,
    pub color: u8,
    pub color_rgb: (u8, u8, u8),
}

/// Resolved provider emblem art and its catalog-defined tint runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Emblem {
    pub lines: Vec<String>,
    pub tints: Vec<EmblemTint>,
}

/// The parsed catalog: curated per-kind art keyed by agent kind, plus the
/// shared `fallback` pool that covers kinds without their own emblem.
struct Catalog {
    curated: BTreeMap<String, Emblem>,
    fallback: Vec<Emblem>,
}

fn catalog() -> &'static Catalog {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let raw: BTreeMap<String, toml::Value> =
            toml::from_str(CATALOG_SOURCE).expect("embedded emblem catalog must be valid TOML");
        let mut curated = BTreeMap::new();
        let mut fallback = Vec::new();
        for (key, value) in raw {
            if key == "fallback" {
                fallback = value
                    .as_array()
                    .expect("`fallback` must be an array of emblems")
                    .iter()
                    .map(|entry| parse_emblem("fallback", entry))
                    .collect();
            } else {
                curated.insert(key.clone(), parse_emblem(&key, &value));
            }
        }
        assert!(
            !fallback.is_empty(),
            "embedded emblem catalog must carry a non-empty `fallback` pool"
        );
        Catalog { curated, fallback }
    })
}

fn parse_emblem(kind: &str, value: &toml::Value) -> Emblem {
    let (art, tint_mask, tint_colors) = match value {
        toml::Value::String(art) => (art.as_str(), None, None),
        toml::Value::Table(table) => {
            let art = table
                .get("art")
                .and_then(toml::Value::as_str)
                .expect("curated emblem table must carry string `art`");
            let tint_mask = table.get("tint").map(|value| {
                value
                    .as_str()
                    .expect("curated emblem `tint` must be a string")
            });
            let tint_colors = table.get("tints").map(|value| {
                value
                    .as_table()
                    .expect("curated emblem `tints` must be a table")
            });
            (art, tint_mask, tint_colors)
        }
        _ => panic!("curated emblem must be a string or table"),
    };
    let lines = emblem_from(art);
    assert!(
        lines.len() <= 4,
        "{kind} emblem has more than the supported four rows"
    );
    let Some(mask) = tint_mask else {
        return Emblem {
            lines,
            tints: Vec::new(),
        };
    };
    let colors = tint_colors.expect("curated emblem with `tint` must carry a `tints` table");
    let mask_lines = emblem_from(mask);
    assert!(
        mask_lines.len() <= lines.len(),
        "{kind} tint mask is taller than its emblem"
    );

    let mut tints = Vec::new();
    for (row, mask_line) in mask_lines.iter().enumerate() {
        let mask_chars: Vec<char> = mask_line.chars().collect();
        assert!(
            mask_chars.len() <= lines[row].chars().count(),
            "{kind} tint mask row {row} is wider than its emblem row"
        );
        let mut start = 0;
        while start < mask_chars.len() {
            let key = mask_chars[start];
            if key == ' ' {
                start += 1;
                continue;
            }
            let mut end = start + 1;
            while end < mask_chars.len() && mask_chars[end] == key {
                end += 1;
            }
            let color = colors
                .get(&key.to_string())
                .unwrap_or_else(|| panic!("{kind} tint mask key `{key}` has no color"));
            let color = color
                .as_table()
                .expect("curated emblem tint color must be a table");
            let indexed = color
                .get("indexed")
                .and_then(toml::Value::as_integer)
                .and_then(|value| u8::try_from(value).ok())
                .expect("curated emblem tint `indexed` must fit in a u8");
            let rgb = color
                .get("rgb")
                .and_then(toml::Value::as_str)
                .map(parse_rgb)
                .expect("curated emblem tint must carry string `rgb`");
            tints.push(EmblemTint {
                row,
                start,
                len: end - start,
                color: indexed,
                color_rgb: rgb,
            });
            start = end;
        }
    }
    Emblem { lines, tints }
}

/// Split a stored emblem block into its rendered lines.
fn emblem_from(art: &str) -> Vec<String> {
    art.trim_matches('\n')
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_rgb(value: &str) -> (u8, u8, u8) {
    let hex = value
        .strip_prefix('#')
        .expect("curated emblem tint `rgb` must start with `#`");
    assert!(
        hex.len() == 6 && hex.is_ascii(),
        "curated emblem tint `rgb` must use `#rrggbb`"
    );
    let component = |range| {
        u8::from_str_radix(&hex[range], 16)
            .expect("curated emblem tint `rgb` must use hexadecimal digits")
    };
    (component(0..2), component(2..4), component(4..6))
}

/// Resolve the emblem for `kind`: definition brand override, curated catalog
/// entry, or the shared fallback.
pub(crate) fn emblem_for(kind: &str) -> Emblem {
    if let Some(art) = super::spec_by_kind(kind).and_then(|definition| definition.brand.emblem) {
        return Emblem {
            lines: emblem_from(art),
            tints: Vec::new(),
        };
    }
    catalog()
        .curated
        .get(kind)
        .cloned()
        // `catalog()` guarantees the fallback pool is non-empty.
        .unwrap_or_else(|| catalog().fallback[0].clone())
}

/// The SVG logo shipped for a built-in `kind`; plugin kinds have none.
pub(crate) fn shipped_logo(kind: &str) -> Option<&'static str> {
    SHIPPED_LOGOS
        .iter()
        .find_map(|(shipped, svg)| (*shipped == kind).then_some(*svg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::registry::BUILTINS;

    #[test]
    fn fallback_is_present_and_non_empty() {
        assert!(!catalog().fallback[0].lines.is_empty());
    }

    #[test]
    fn every_provider_has_curated_art_distinct_from_fallback() {
        let fallback = &catalog().fallback[0];
        for adapter in BUILTINS {
            let kind = adapter.spec().kind;
            let art = catalog().curated.get(kind);
            assert!(art.is_some(), "{kind} must have a curated emblem");
            let art = art.expect("checked above");
            assert!(!art.lines.is_empty(), "{kind} emblem must not be empty");
            assert_ne!(art, fallback, "{kind} must have art of its own");
        }
    }

    #[test]
    fn every_provider_ships_a_logo_on_a_transparent_ground() {
        for adapter in BUILTINS {
            let kind = adapter.spec().kind;
            let svg = shipped_logo(kind).unwrap_or_else(|| panic!("{kind} must ship a logo"));
            let tree = crate::config::parse_logo_svg(svg.as_bytes(), [0x40, 0x80, 0xc0])
                .unwrap_or_else(|err| panic!("{kind} logo must parse: {err}"));
            let mut pixmap = resvg::tiny_skia::Pixmap::new(64, 64).expect("pixmap");
            let size = tree.size();
            let scale = (64.0 / size.width()).min(64.0 / size.height());
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );
            let alphas = pixmap.pixels().iter().map(|pixel| pixel.alpha());
            let (opaque, clear) = alphas.fold((0, 0), |(opaque, clear), alpha| {
                (
                    opaque + usize::from(alpha > 0),
                    clear + usize::from(alpha == 0),
                )
            });
            assert!(
                opaque > 64,
                "{kind} logo must draw a mark: {opaque} painted pixels"
            );
            assert!(
                clear > 64 * 64 / 8,
                "{kind} logo must keep a transparent ground: {clear} clear pixels"
            );
            for corner in [0, 63, 64 * 63, 64 * 64 - 1] {
                assert_eq!(
                    pixmap.pixels()[corner].alpha(),
                    0,
                    "{kind} logo must not sit on a background tile"
                );
            }
        }
    }

    #[test]
    fn antigravity_uses_the_gemini_gem() {
        assert_eq!(
            emblem_for("antigravity").lines,
            [" ▗▛▀▀▀▜▖", "▐█ ◆ ◆ █▌", " ▝▀▀▀▀▀▘"]
        );
    }

    #[test]
    fn copilot_uses_origin_art_and_catalog_tints() {
        let emblem = emblem_for("copilot");
        assert_eq!(emblem.lines, ["╭─╮╭─╮", "╰─╯╰─╯", "█ ▘▝ █", " ▔▔▔▔"]);
        assert_eq!(
            emblem.tints,
            [
                EmblemTint {
                    row: 0,
                    start: 0,
                    len: 6,
                    color: 33,
                    color_rgb: (0x2f, 0x94, 0xff),
                },
                EmblemTint {
                    row: 1,
                    start: 0,
                    len: 6,
                    color: 33,
                    color_rgb: (0x2f, 0x94, 0xff),
                },
                EmblemTint {
                    row: 2,
                    start: 2,
                    len: 2,
                    color: 84,
                    color_rgb: (0x60, 0xed, 0x83),
                },
            ]
        );
    }

    #[test]
    fn plain_catalog_emblems_are_untinted() {
        assert!(emblem_for("claude").tints.is_empty());
    }

    #[test]
    fn unknown_kind_uses_fallback() {
        assert_eq!(emblem_for("does-not-exist"), catalog().fallback[0]);
    }

    #[test]
    fn tint_masks_reject_unknown_keys_and_shape_overflow() {
        for source in [
            "art = 'x'\ntint = 'g'\n[tints]",
            "art = 'x'\ntint = 'gg'\n[tints]\ng = { rgb = '#000000', indexed = 0 }",
            "art = 'x'\ntint = '''g\ng'''\n[tints]\ng = { rgb = '#000000', indexed = 0 }",
        ] {
            let value = toml::from_str::<toml::Value>(source).expect("test emblem table");
            assert!(
                std::panic::catch_unwind(|| parse_emblem("test", &value)).is_err(),
                "invalid tint catalog entry was accepted: {source}"
            );
        }
    }
}
