//! Selectable Alacritty theme catalog and palette loading.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use flate2::read::GzDecoder;
use serde::Deserialize;

use super::{InlinePalette, ThemeConfig, parse_hex};

const BUILD_TIME_ALACRITTY_THEMES_JSON_GZ: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/alacritty-themes.json.gz"));
pub(crate) const DEFAULT_SCHEME: &str = "TokyoNight Night";

#[derive(Debug, Deserialize)]
struct AlacrittyTheme {
    colors: Option<InlinePalette>,
}

/// Validated colors needed to derive renderer semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParsedScheme {
    pub(crate) background: (u8, u8, u8),
    pub(crate) foreground: (u8, u8, u8),
    pub(crate) red: (u8, u8, u8),
    pub(crate) green: (u8, u8, u8),
    pub(crate) yellow: (u8, u8, u8),
    pub(crate) blue: (u8, u8, u8),
    pub(crate) magenta: (u8, u8, u8),
    pub(crate) cyan: (u8, u8, u8),
    pub(crate) bright_blue: (u8, u8, u8),
    pub(crate) selection_background: Option<(u8, u8, u8)>,
}

/// One bundled scheme's display name and terminal swatch colors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemeSwatch {
    pub name: String,
    pub background: (u8, u8, u8),
    pub foreground: (u8, u8, u8),
    pub red: (u8, u8, u8),
    pub green: (u8, u8, u8),
    pub yellow: (u8, u8, u8),
    pub blue: (u8, u8, u8),
    pub magenta: (u8, u8, u8),
    pub cyan: (u8, u8, u8),
}

#[derive(Debug)]
struct LoadedScheme {
    colors: InlinePalette,
    parsed: ParsedScheme,
}

/// Every resolvable bundled scheme and its raw swatch, sorted by name.
pub fn scheme_swatches() -> Vec<SchemeSwatch> {
    theme_names()
        .filter_map(|name| {
            let parsed = parse_scheme(theme_toml(name)?).ok()?.parsed;
            Some(SchemeSwatch {
                name: name.to_owned(),
                background: parsed.background,
                foreground: parsed.foreground,
                red: parsed.red,
                green: parsed.green,
                yellow: parsed.yellow,
                blue: parsed.blue,
                magenta: parsed.magenta,
                cyan: parsed.cyan,
            })
        })
        .collect()
}

/// Resolve the active scheme as a full Alacritty palette. Inline colors win;
/// invalid named or path schemes fall back to the bundled default.
pub fn resolve_inline_palette(theme: &ThemeConfig) -> InlinePalette {
    if let Some(colors) = &theme.colors {
        return colors.clone();
    }
    theme
        .scheme
        .as_deref()
        .and_then(|name_or_path| {
            load_explicit_scheme(name_or_path)
                .ok()
                .map(|scheme| scheme.colors)
        })
        .unwrap_or_else(default_inline_palette)
}

pub(crate) fn validate_explicit_scheme(name_or_path: &str) -> Result<(), String> {
    load_explicit_scheme(name_or_path).map(|_| ())
}

pub(crate) fn explicit_scheme(name_or_path: &str) -> Option<ParsedScheme> {
    load_explicit_scheme(name_or_path)
        .ok()
        .map(|scheme| scheme.parsed)
}

pub(crate) fn parsed_inline_palette(colors: &InlinePalette) -> Result<ParsedScheme, String> {
    parse_colors(colors)
}

#[cfg(test)]
pub(crate) fn parse_scheme_text(text: &str) -> Result<ParsedScheme, String> {
    parse_scheme(text).map(|scheme| scheme.parsed)
}

pub fn theme_lookup_hint() -> String {
    format!(
        "{} bundled Alacritty themes in crates/rimz/themes/alacritty (see `rimz list-themes`); or a path to an Alacritty .toml",
        theme_count()
    )
}

fn load_explicit_scheme(name_or_path: &str) -> Result<LoadedScheme, String> {
    if let Some(text) = theme_toml(name_or_path) {
        return parse_scheme(text).map_err(|err| {
            format!("invalid bundled sidebar theme scheme `{name_or_path}`: {err}")
        });
    }
    let (path, text) = load_external(name_or_path)?;
    parse_scheme(&text)
        .map_err(|err| format!("invalid sidebar theme scheme `{}`: {err}", path.display()))
}

fn load_external(name_or_path: &str) -> Result<(PathBuf, String), String> {
    let path = resolve_external_scheme_path(name_or_path).ok_or_else(|| {
        format!(
            "unknown sidebar theme scheme `{name_or_path}`; {}",
            theme_lookup_hint()
        )
    })?;
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("reading sidebar theme scheme `{}`: {err}", path.display()))?;
    Ok((path, text))
}

fn resolve_external_scheme_path(name_or_path: &str) -> Option<PathBuf> {
    let path = expand_home(Path::new(name_or_path));
    path.is_file().then_some(path)
}

fn expand_home(path: &Path) -> PathBuf {
    let raw = path.as_os_str().to_string_lossy();
    if raw == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(stripped) = raw.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(stripped);
    }
    path.to_path_buf()
}

fn parse_scheme(text: &str) -> Result<LoadedScheme, String> {
    let colors = parse_inline_palette(text)?;
    let parsed = parse_colors(&colors)?;
    Ok(LoadedScheme { colors, parsed })
}

fn parse_inline_palette(text: &str) -> Result<InlinePalette, String> {
    let theme: AlacrittyTheme =
        toml::from_str(text).map_err(|err| format!("parsing Alacritty theme TOML: {err}"))?;
    Ok(theme.colors.unwrap_or_default())
}

fn default_inline_palette() -> InlinePalette {
    load_explicit_scheme(DEFAULT_SCHEME)
        .map(|scheme| scheme.colors)
        .unwrap_or_default()
}

fn parse_colors(colors: &InlinePalette) -> Result<ParsedScheme, String> {
    let primary = colors.primary.clone().unwrap_or_default();
    let normal = colors.normal.clone().unwrap_or_default();
    let background =
        parse_required_color("colors.primary.background", primary.background.as_deref())?;
    let foreground =
        parse_required_color("colors.primary.foreground", primary.foreground.as_deref())?;
    let red = parse_required_color("colors.normal.red", normal.red.as_deref())?;
    let green = parse_required_color("colors.normal.green", normal.green.as_deref())?;
    let yellow = parse_required_color("colors.normal.yellow", normal.yellow.as_deref())?;
    let blue = parse_required_color("colors.normal.blue", normal.blue.as_deref())?;
    let magenta = parse_required_color("colors.normal.magenta", normal.magenta.as_deref())?;
    let cyan = parse_required_color("colors.normal.cyan", normal.cyan.as_deref())?;
    let bright_blue = match colors
        .bright
        .as_ref()
        .and_then(|bright| bright.blue.as_deref())
    {
        Some(value) => parse_color("colors.bright.blue", value)?,
        None => blue,
    };
    let selection_background = match colors
        .selection
        .as_ref()
        .and_then(|selection| selection.background.as_deref())
    {
        Some(value) => Some(parse_color("colors.selection.background", value)?),
        None => None,
    };
    Ok(ParsedScheme {
        background,
        foreground,
        red,
        green,
        yellow,
        blue,
        magenta,
        cyan,
        bright_blue,
        selection_background,
    })
}

fn parse_required_color(key: &str, value: Option<&str>) -> Result<(u8, u8, u8), String> {
    let value = value.ok_or_else(|| format!("{key} is missing"))?;
    parse_color(key, value)
}

fn parse_color(key: &str, value: &str) -> Result<(u8, u8, u8), String> {
    parse_hex(value).map_err(|err| format!("{key}: {err}"))
}

fn theme_toml(name: &str) -> Option<&'static str> {
    catalog().get(name).map(String::as_str)
}

fn theme_count() -> usize {
    catalog().len()
}

fn theme_names() -> impl Iterator<Item = &'static str> {
    catalog().keys().map(String::as_str)
}

fn catalog() -> &'static BTreeMap<String, String> {
    static CATALOG: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut json = String::new();
        if GzDecoder::new(BUILD_TIME_ALACRITTY_THEMES_JSON_GZ)
            .read_to_string(&mut json)
            .is_err()
        {
            return BTreeMap::new();
        }
        serde_json::from_str(&json).unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MISSING_GREEN: &str = r#"
[colors.normal]
red = '#ac4142'
yellow = '#e5b567'
blue = '#6c99bb'
magenta = '#9f4e85'
cyan = '#7dd6cf'

[colors.primary]
background = '#212121'
foreground = '#d0d0d0'
"#;

    #[test]
    fn catalog_decompresses_and_every_scheme_parses() {
        assert_eq!(theme_names().count(), theme_count());
        for name in theme_names() {
            let text = theme_toml(name).expect("catalog name resolves");
            parse_scheme(text)
                .unwrap_or_else(|err| panic!("bundled theme `{name}` is invalid: {err}"));
        }
    }

    #[test]
    fn catalog_swatches_are_sorted_and_include_known_schemes() {
        let swatches = scheme_swatches();
        assert!(
            swatches
                .iter()
                .any(|swatch| swatch.name == "TokyoNight Night")
        );
        assert!(
            swatches
                .iter()
                .any(|swatch| swatch.name == "Catppuccin Mocha")
        );
        assert!(swatches.windows(2).all(|pair| pair[0].name <= pair[1].name));
    }

    #[test]
    fn scheme_swatch_exposes_raw_palette() {
        let swatch = scheme_swatches()
            .into_iter()
            .find(|swatch| swatch.name == "TokyoNight Night")
            .expect("bundled scheme");
        assert_eq!(swatch.background, (0x1a, 0x1b, 0x26));
        assert_eq!(swatch.green, (0x9e, 0xce, 0x6a));
    }

    #[test]
    fn required_colors_fail_in_stable_order() {
        assert_eq!(
            parse_scheme(MISSING_GREEN).expect_err("missing green"),
            "colors.normal.green is missing"
        );
    }

    #[test]
    fn inline_colors_win_and_bad_scheme_falls_back() {
        let inline = InlinePalette {
            primary: Some(super::super::InlinePrimaryColors {
                background: Some("#010203".to_owned()),
                foreground: Some("#040506".to_owned()),
            }),
            ..InlinePalette::default()
        };
        assert_eq!(
            resolve_inline_palette(&ThemeConfig {
                scheme: Some("missing scheme".to_owned()),
                colors: Some(inline.clone()),
                ..ThemeConfig::default()
            }),
            inline
        );
        assert_eq!(
            resolve_inline_palette(&ThemeConfig {
                scheme: Some("missing scheme".to_owned()),
                ..ThemeConfig::default()
            }),
            resolve_inline_palette(&ThemeConfig::default())
        );
    }

    #[test]
    fn external_path_and_home_expansion_resolve() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("scheme.toml");
        std::fs::write(&path, theme_toml("Afterglow").expect("Afterglow")).expect("write");
        assert!(load_explicit_scheme(path.to_str().expect("utf-8")).is_ok());

        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(
                expand_home(Path::new("~/scheme.toml")),
                PathBuf::from(home).join("scheme.toml")
            );
        }
    }

    #[test]
    fn external_and_unknown_errors_keep_user_facing_context() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "[colors.primary]\nbackground = 'nothex'\n").expect("write");
        assert_eq!(
            validate_explicit_scheme(path.to_str().expect("utf-8")).expect_err("bad scheme"),
            format!(
                "invalid sidebar theme scheme `{}`: colors.primary.background: hex colors must start with #",
                path.display()
            )
        );
        assert_eq!(
            resolve_inline_palette(&ThemeConfig {
                scheme: Some(path.display().to_string()),
                ..ThemeConfig::default()
            }),
            resolve_inline_palette(&ThemeConfig::default())
        );

        assert_eq!(
            validate_explicit_scheme("does-not-exist").expect_err("unknown scheme"),
            format!(
                "unknown sidebar theme scheme `does-not-exist`; {}",
                theme_lookup_hint()
            )
        );
    }

    #[test]
    fn full_bundled_palette_is_retained() {
        let palette = resolve_inline_palette(&ThemeConfig {
            scheme: Some(DEFAULT_SCHEME.to_owned()),
            ..ThemeConfig::default()
        });
        assert_eq!(
            palette.bright.and_then(|bright| bright.white).as_deref(),
            Some("#c0caf5")
        );
        assert_eq!(
            palette.cursor.and_then(|cursor| cursor.cursor).as_deref(),
            Some("#c0caf5")
        );
    }
}
