use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::{InlinePalette, Semantic, ThemeConfig, parse_hex};

use super::embedded_themes;
use super::oklab::Rgb;
use super::theme::RawPalette;

#[derive(Debug, Deserialize)]
struct AlacrittyTheme {
    colors: Option<InlinePalette>,
}

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
    load_explicit_raw_palette(name_or_path).ok()
}

pub fn scheme_swatch(name_or_path: &str) -> Option<SchemeSwatch> {
    explicit_raw_palette(name_or_path).map(|raw| SchemeSwatch {
        background: raw.background,
        foreground: raw.foreground,
        red: raw.red,
        green: raw.green,
        yellow: raw.yellow,
        blue: raw.blue,
        magenta: raw.magenta,
        cyan: raw.cyan,
    })
}

#[cfg(test)]
pub(crate) fn explicit_palette_tones(name_or_path: &str) -> Option<Semantic> {
    load_explicit_palette_tones(name_or_path).ok()
}

pub(crate) fn default_raw_palette() -> RawPalette {
    explicit_raw_palette(super::theme::DEFAULT_SCHEME).unwrap_or(RawPalette::DEFAULT)
}

/// Resolve the active scheme as a full Alacritty palette: inline `[colors]`
/// wins, then a named/path scheme, then the bundled default. Bad scheme names
/// fall back to the default, matching the renderer's lenient behavior.
pub fn resolve_inline_palette(theme: &ThemeConfig) -> InlinePalette {
    if let Some(colors) = &theme.colors {
        return colors.clone();
    }
    theme
        .scheme
        .as_deref()
        .and_then(|name_or_path| load_explicit_inline_palette(name_or_path).ok())
        .unwrap_or_else(default_inline_palette)
}

pub fn validate_explicit_scheme(name_or_path: &str) -> Result<(), String> {
    load_explicit_palette_tones(name_or_path).map(|_| ())
}

/// Every scheme name a user can select: the bundled Alacritty catalog, sorted.
pub fn available_scheme_names() -> Vec<String> {
    embedded_themes::theme_names().map(str::to_owned).collect()
}

fn load_explicit_palette_tones(name_or_path: &str) -> Result<Semantic, String> {
    load_explicit_raw_palette(name_or_path).map(|raw| raw.derive_tones())
}

fn load_explicit_raw_palette(name_or_path: &str) -> Result<RawPalette, String> {
    if let Some(text) = embedded_themes::theme_toml(name_or_path) {
        return parse_raw_palette(text).map_err(|err| {
            format!("invalid bundled sidebar theme scheme `{name_or_path}`: {err}")
        });
    }
    load_external_raw_palette(name_or_path)
}

fn load_explicit_inline_palette(name_or_path: &str) -> Result<InlinePalette, String> {
    if let Some(text) = embedded_themes::theme_toml(name_or_path) {
        return parse_inline_palette(text).map_err(|err| {
            format!("invalid bundled sidebar theme scheme `{name_or_path}`: {err}")
        });
    }
    load_external_inline_palette(name_or_path)
}

fn load_external_raw_palette(name_or_path: &str) -> Result<RawPalette, String> {
    let path = resolve_external_scheme_path(name_or_path).ok_or_else(|| {
        format!(
            "unknown sidebar theme scheme `{name_or_path}`; {}",
            theme_lookup_hint()
        )
    })?;
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("reading sidebar theme scheme `{}`: {err}", path.display()))?;
    parse_raw_palette(&text)
        .map_err(|err| format!("invalid sidebar theme scheme `{}`: {err}", path.display()))
}

fn load_external_inline_palette(name_or_path: &str) -> Result<InlinePalette, String> {
    let path = resolve_external_scheme_path(name_or_path).ok_or_else(|| {
        format!(
            "unknown sidebar theme scheme `{name_or_path}`; {}",
            theme_lookup_hint()
        )
    })?;
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("reading sidebar theme scheme `{}`: {err}", path.display()))?;
    parse_inline_palette(&text)
        .map_err(|err| format!("invalid sidebar theme scheme `{}`: {err}", path.display()))
}

pub fn theme_lookup_hint() -> String {
    format!(
        "{} bundled Alacritty themes in crates/rimz/themes/alacritty (see `rimz list-themes`); or a path to an Alacritty .toml",
        embedded_themes::theme_count()
    )
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

#[cfg(test)]
pub(crate) fn parse_palette_tones(text: &str) -> Result<Semantic, String> {
    Ok(parse_raw_palette(text)?.derive_tones())
}

pub(crate) fn inline_raw_palette(colors: &InlinePalette) -> Result<RawPalette, String> {
    raw_palette_from_colors(colors)
}

/// Parse an Alacritty theme into the [`RawPalette`] the semantic layer derives
/// from. Background, foreground, and the six normal ANSI hues are required;
/// `colors.bright.blue` is the selection accent, falling back to `normal.blue`.
fn parse_raw_palette(text: &str) -> Result<RawPalette, String> {
    let theme: AlacrittyTheme =
        toml::from_str(text).map_err(|err| format!("parsing Alacritty theme TOML: {err}"))?;
    raw_palette_from_colors(&theme.colors.unwrap_or_default())
}

fn parse_inline_palette(text: &str) -> Result<InlinePalette, String> {
    let theme: AlacrittyTheme =
        toml::from_str(text).map_err(|err| format!("parsing Alacritty theme TOML: {err}"))?;
    Ok(theme.colors.unwrap_or_default())
}

fn default_inline_palette() -> InlinePalette {
    load_explicit_inline_palette(super::theme::DEFAULT_SCHEME).unwrap_or_default()
}

fn raw_palette_from_colors(colors: &InlinePalette) -> Result<RawPalette, String> {
    let primary = colors.primary.clone().unwrap_or_default();
    let normal = colors.normal.clone().unwrap_or_default();

    // Required keys are validated in a stable order — primary first, then the
    // normal hues — so a malformed scheme reports the earliest missing key.
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
    // The selected-card band. Optional: a scheme without it derives a tint at
    // [`RawPalette::derive_tones`] time, so every theme still gets a band.
    let selection_background = match colors
        .selection
        .as_ref()
        .and_then(|selection| selection.background.as_deref())
    {
        Some(value) => Some(parse_color("colors.selection.background", value)?),
        None => None,
    };

    Ok(RawPalette {
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

fn parse_required_color(key: &str, value: Option<&str>) -> Result<Rgb, String> {
    let value = value.ok_or_else(|| format!("{key} is missing"))?;
    parse_color(key, value)
}

fn parse_color(key: &str, value: &str) -> Result<Rgb, String> {
    parse_hex(value).map_err(|err| format!("{key}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AFTERGLOW: &str = r#"
[colors.bright]
blue = '#6c99bb'

[colors.normal]
red = '#ac4142'
green = '#7e8e50'
yellow = '#e5b567'
blue = '#6c99bb'
magenta = '#9f4e85'
cyan = '#7dd6cf'

[colors.primary]
background = '#212121'
foreground = '#d0d0d0'
"#;

    const LIGHT_SCHEME: &str = r#"
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
"#;

    #[test]
    fn alacritty_scheme_parses_and_derives() {
        let tones = parse_palette_tones(AFTERGLOW).expect("parse scheme");
        assert_eq!(tones.good, (0x7e, 0x8e, 0x50));
        assert_eq!(tones.warn, (0xe5, 0xb5, 0x67));
        assert_eq!(tones.alarm, (0xac, 0x41, 0x42));
        assert_eq!(tones.accent, (0x7d, 0xd6, 0xcf));
        assert_eq!(tones.cool, (0x6c, 0x99, 0xbb));
        assert_eq!(tones.meta, (0x9f, 0x4e, 0x85));
        // Selection is its own bright cool tone, lifted off the data-cool slot —
        // never a raw copy of bright.blue — so the selected card never borrows a
        // token color.
        assert_ne!(tones.selection, tones.cool);
        // Caution warms the yellow into an amber distinct from both neighbors.
        assert_ne!(tones.caution, tones.warn);
        assert_ne!(tones.caution, tones.alarm);
        // With no `colors.selection`, the band derives a deep tint of the
        // background rather than copying the raw background.
        assert_ne!(
            tones.selection_bg,
            (0x21, 0x21, 0x21),
            "the band is a derived tint, not the raw background"
        );
    }

    #[test]
    fn scheme_swatch_exposes_raw_palette() {
        let swatch = scheme_swatch("TokyoNight Night").expect("bundled scheme");
        assert_eq!(swatch.background, (0x1a, 0x1b, 0x26));
        assert_eq!(swatch.green, (0x9e, 0xce, 0x6a));
    }

    #[test]
    fn resolve_inline_palette_keeps_bundled_full_palette() {
        let palette = resolve_inline_palette(&ThemeConfig {
            scheme: Some("TokyoNight Night".to_owned()),
            ..ThemeConfig::default()
        });
        let normal = palette.normal.expect("normal colors");
        let bright = palette.bright.expect("bright colors");
        let cursor = palette.cursor.expect("cursor colors");
        let selection = palette.selection.expect("selection colors");
        assert_eq!(normal.black.as_deref(), Some("#15161e"));
        assert_eq!(normal.white.as_deref(), Some("#a9b1d6"));
        assert_eq!(bright.red.as_deref(), Some("#f7768e"));
        assert_eq!(bright.white.as_deref(), Some("#c0caf5"));
        assert_eq!(cursor.cursor.as_deref(), Some("#c0caf5"));
        assert_eq!(cursor.text.as_deref(), Some("#1a1b26"));
        assert_eq!(selection.text.as_deref(), Some("#c0caf5"));
    }

    #[test]
    fn resolve_inline_palette_honors_inline_colors() {
        let inline = InlinePalette {
            primary: Some(crate::config::InlinePrimaryColors {
                background: Some("#010203".to_owned()),
                foreground: Some("#040506".to_owned()),
            }),
            ..InlinePalette::default()
        };
        let palette = resolve_inline_palette(&ThemeConfig {
            scheme: Some("TokyoNight Night".to_owned()),
            colors: Some(inline),
            ..ThemeConfig::default()
        });
        assert_eq!(
            palette
                .primary
                .and_then(|primary| primary.background)
                .as_deref(),
            Some("#010203")
        );
        assert!(palette.normal.is_none());
    }

    #[test]
    fn resolve_inline_palette_falls_back_to_default_on_unknown_scheme() {
        let default = resolve_inline_palette(&ThemeConfig::default());
        let unknown = resolve_inline_palette(&ThemeConfig {
            scheme: Some("missing scheme".to_owned()),
            ..ThemeConfig::default()
        });
        assert_eq!(unknown, default);
    }

    #[test]
    fn alacritty_selection_background_feeds_the_band_when_present() {
        const WITH_SELECTION: &str = r#"
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
"#;
        let tones = parse_palette_tones(WITH_SELECTION).expect("parse scheme");
        // The band is the scheme's text-selection background pulled most of the
        // way back toward the background — subdued for a full-card fill, so it
        // equals neither the raw selection color nor the background, and its blue
        // sits between the two.
        assert_ne!(tones.selection_bg, (0x28, 0x34, 0x57));
        assert_ne!(tones.selection_bg, (0x1a, 0x1b, 0x26));
        assert!(
            tones.selection_bg.2 > 0x26 && tones.selection_bg.2 < 0x57,
            "the band's blue sits between background and the raw selection: {:?}",
            tones.selection_bg
        );
    }

    #[test]
    fn alacritty_missing_required_normal_entry_is_named_error() {
        let err = parse_palette_tones(
            r#"
[colors.normal]
red = '#ac4142'
yellow = '#e5b567'
blue = '#6c99bb'
magenta = '#9f4e85'
cyan = '#7dd6cf'

[colors.primary]
background = '#212121'
foreground = '#d0d0d0'
"#,
        )
        .expect_err("missing green should fail");
        assert_eq!(err, "colors.normal.green is missing");
    }

    #[test]
    fn light_alacritty_theme_ladder_darkens_toward_foreground() {
        let tones = parse_palette_tones(LIGHT_SCHEME).expect("parse scheme");
        assert!(
            tones.body.0 < tones.muted.0 && tones.muted.0 < tones.faint.0,
            "light scheme ladder darkens toward foreground"
        );
    }

    #[test]
    fn embedded_name_resolves_to_palette() {
        assert!(
            explicit_palette_tones("Afterglow").is_some(),
            "the vendored Alacritty catalog should include Afterglow"
        );
    }

    #[test]
    fn available_scheme_names_list_the_bundled_catalog() {
        let names = available_scheme_names();
        assert!(names.iter().any(|name| name == "TokyoNight Night"));
        assert!(names.iter().any(|name| name == "Catppuccin Mocha"));
        assert!(names.windows(2).all(|pair| pair[0] <= pair[1]), "sorted");
    }

    #[test]
    fn removed_builtin_names_no_longer_resolve() {
        for removed in ["clay", "slate", "classic"] {
            assert!(
                explicit_palette_tones(removed).is_none(),
                "`{removed}` was retired in favour of the bundled catalog"
            );
            assert!(validate_explicit_scheme(removed).is_err());
        }
    }
}
