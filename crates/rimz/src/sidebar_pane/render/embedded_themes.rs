//! Build-time Alacritty theme catalog embedded into the renderer.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::OnceLock;

use flate2::read::GzDecoder;

/// The Alacritty catalog, embedded at build time by `build.rs`.
const BUILD_TIME_ALACRITTY_THEMES_JSON_GZ: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/alacritty-themes.json.gz"));

pub(super) fn theme_toml(name: &str) -> Option<&'static str> {
    catalog().get(name).map(String::as_str)
}

pub(super) fn theme_count() -> usize {
    catalog().len()
}

fn catalog() -> &'static BTreeMap<String, String> {
    static CATALOG: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    CATALOG.get_or_init(load)
}

fn load() -> BTreeMap<String, String> {
    let mut json = String::new();
    if GzDecoder::new(BUILD_TIME_ALACRITTY_THEMES_JSON_GZ)
        .read_to_string(&mut json)
        .is_err()
    {
        return BTreeMap::new();
    }
    serde_json::from_str(&json).unwrap_or_default()
}

#[cfg(test)]
fn theme_names() -> impl Iterator<Item = &'static str> {
    catalog().keys().map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidebar_pane::render::scheme;

    #[test]
    fn catalog_decompresses() {
        assert_eq!(theme_names().count(), theme_count());
    }

    #[test]
    fn every_bundled_theme_parses_and_derives() {
        for name in theme_names() {
            let text = theme_toml(name).expect("catalog name resolves");
            scheme::parse_palette_tones(text)
                .unwrap_or_else(|err| panic!("bundled theme `{name}` is invalid: {err}"));
        }
    }
}
